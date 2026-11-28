//! Runtime glyphs: characters the built-in Open Sans pages lack (Japanese,
//! Chinese, Korean and whatever else Noto Sans CJK carries), rasterised on
//! first draw at a face's exact pixel size into the shared 1024x1024 coverage
//! page (`reborn_graphics::glyph_page`), which the renderer uploads.
//!
//! Metrics come straight from the font (no rasterising), so `text::width`,
//! `fit` and `wrap` agree with what is drawn. The page is packed in shelves of
//! square cells per size class and evicts the least recently drawn glyph;
//! glyphs drawn in this or the previous frame are never evicted (a screen
//! sliding out redraws the previous frame's quads). A class out of shelves
//! takes over stale shelves of others, merging adjacent ones when none is
//! tall enough. At most `GLYPHS_PER_FRAME` new glyphs are rasterised per
//! frame; the rest are left blank for a frame and `pending()` asks the caller
//! to draw again. A glyph with no room is not rasterised, and asks for
//! another frame only when the next one frees a cell.
//!
//! The font is read through `reborn_graphics::cjk_font` the first time a
//! character misses the built-in pages; without it nothing here exists and
//! text keeps showing U+FFFD.
use crate::skin::atlas;
use reborn_graphics::{glyph_page, Quad, Tex, GLYPH_PAGE};
use std::{
    collections::HashMap,
    ops::ControlFlow,
    sync::{Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant},
};

/// Cell edges in texels. A glyph takes the smallest class holding its bitmap
/// and a one-texel clear border on every side.
const CLASSES: [usize; 8] = [12, 16, 20, 24, 32, 40, 48, 64];
/// New glyphs rasterised per frame (see `pending`).
pub const GLYPHS_PER_FRAME: usize = 48;
/// A frame that never calls `begin_frame` still gets a fresh budget after this.
const BUDGET_WINDOW: Duration = Duration::from_millis(250);

/// A character of the CJK font at one pixel size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Runtime {
    pub ch: char,
    pub px: u8,
    pub id: u16,
    /// Pen advance in pixels.
    pub advance: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub px: u8,
    pub ch: char,
}

/// Where a resident glyph is: its bitmap in the page (`w`/`h` 0 for a blank
/// glyph) and the bitmap's top-left relative to the pen on the baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub src: [u16; 4],
    pub left: i16,
    pub top: i16,
}

struct Shelf {
    y: usize,
    h: usize,
    cell: usize,
    cells: Vec<Option<Slot>>,
}

/// A cell's glyph and the frame it was last drawn in. The frame lives here,
/// not in `entries`, so eviction reads it without hashing a key per cell (a
/// page full of CJK at R15 has about 2,600 cells of one class).
#[derive(Clone, Copy)]
struct Slot {
    key: Key,
    used: u64,
}

struct Entry {
    placed: Placed,
    /// (shelf, cell) for a glyph with pixels; `None` for a blank glyph.
    at: Option<(usize, usize)>,
}

/// A free cell's top-left and edge; the caller writes `edge` x `edge` texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub x: usize,
    pub y: usize,
    pub edge: usize,
}

/// Slot bookkeeping for the page (no pixels; see `write_cell`).
pub struct GlyphCache {
    width: usize,
    height: usize,
    shelves: Vec<Shelf>,
    entries: HashMap<Key, Entry>,
    /// First row no shelf uses.
    top: usize,
    frame: u64,
    /// Size classes (by index in `CLASSES`) that found no room in this frame.
    /// Room only shrinks within a frame (what is drawn stays protected and
    /// nothing else is freed), so the rest of the class fails at once.
    full: [bool; CLASSES.len()],
    pub evictions: u64,
}

impl GlyphCache {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            shelves: vec![],
            entries: HashMap::new(),
            top: 0,
            frame: 2,
            full: [false; CLASSES.len()],
            evictions: 0,
        }
    }
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        self.full = [false; CLASSES.len()];
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    fn protected(&self, used: u64) -> bool {
        used + 1 >= self.frame
    }
    /// A resident glyph, marked as drawn in this frame.
    pub fn get(&mut self, key: Key) -> Option<Placed> {
        let e = self.entries.get(&key)?;
        if let Some((shelf, cell)) = e.at {
            let slot = self
                .shelves
                .get_mut(shelf)
                .and_then(|s| s.cells.get_mut(cell));
            if let Some(Some(slot)) = slot {
                slot.used = self.frame;
            }
        }
        Some(e.placed)
    }
    /// Remember a glyph without pixels (a space), so it is not rasterised again.
    pub fn insert_blank(&mut self, key: Key) -> Placed {
        let placed = Placed {
            src: [0; 4],
            left: 0,
            top: 0,
        };
        self.entries.insert(key, Entry { placed, at: None });
        placed
    }
    /// Reserve a cell for a `w` x `h` bitmap drawn `left`/`top` from the pen.
    /// `None` when it is too large or no cell can be had without taking one
    /// on screen.
    pub fn insert(&mut self, key: Key, w: usize, h: usize, left: i16, top: i16) -> Option<Cell> {
        let class = CLASSES
            .iter()
            .position(|&c| w + 2 <= c && h + 2 <= c && c <= self.width)?;
        if self.full[class] {
            return None;
        }
        let edge = CLASSES[class];
        let Some((shelf, cell)) = self.free_cell(edge) else {
            self.full[class] = true;
            return None;
        };
        let used = self.frame;
        let s = &mut self.shelves[shelf];
        s.cells[cell] = Some(Slot { key, used });
        let (x, y) = (cell * s.cell, s.y);
        self.entries.insert(
            key,
            Entry {
                placed: Placed {
                    src: [(x + 1) as u16, (y + 1) as u16, w as u16, h as u16],
                    left,
                    top,
                },
                at: Some((shelf, cell)),
            },
        );
        Some(Cell { x, y, edge })
    }
    fn free_cell(&mut self, edge: usize) -> Option<(usize, usize)> {
        // A free cell on a shelf of this class.
        for (i, s) in self.shelves.iter().enumerate() {
            if s.cell == edge {
                if let Some(c) = s.cells.iter().position(Option::is_none) {
                    return Some((i, c));
                }
            }
        }
        // A new shelf below the others.
        if self.top + edge <= self.height {
            self.shelves.push(Shelf {
                y: self.top,
                h: edge,
                cell: edge,
                cells: vec![None; self.width / edge],
            });
            self.top += edge;
            return Some((self.shelves.len() - 1, 0));
        }
        // The least recently drawn glyph of this class.
        let victim = self
            .shelves
            .iter()
            .enumerate()
            .filter(|(_, s)| s.cell == edge)
            .flat_map(|(i, s)| {
                s.cells
                    .iter()
                    .enumerate()
                    .filter_map(move |(c, slot)| slot.map(|slot| (i, c, slot)))
            })
            .filter(|(.., slot)| !self.protected(slot.used))
            .min_by_key(|(.., slot)| slot.used);
        if let Some((i, c, slot)) = victim {
            self.entries.remove(&slot.key);
            self.shelves[i].cells[c] = None;
            self.evictions += 1;
            return Some((i, c));
        }
        self.repurpose(edge)
    }
    /// Clear the stalest run of adjacent shelves with nothing on screen that
    /// is at least `edge` tall, and make it one shelf of this class: a single
    /// shelf tall enough if there is one, else shorter ones merged. Without
    /// merging, a class with no shelf yet could never get a cell once smaller
    /// classes filled the page (CJK lyrics at S25 after browsing a CJK
    /// library at R11 to R15).
    fn repurpose(&mut self, edge: usize) -> Option<(usize, usize)> {
        let stale: Vec<Option<u64>> = self.shelves.iter().map(|s| self.stale_since(s)).collect();
        let (first, last, _) = (0..self.shelves.len())
            .filter_map(|first| self.run(first, edge, &stale))
            .min_by_key(|&(first, last, newest)| (last > first, newest))?;
        let mut h = 0;
        for s in &mut self.shelves[first..=last] {
            h += s.h;
            for slot in s.cells.drain(..).flatten() {
                self.entries.remove(&slot.key);
                self.evictions += 1;
            }
        }
        self.shelves.drain(first + 1..=last);
        let s = &mut self.shelves[first];
        s.h = h;
        s.cell = edge;
        s.cells = vec![None; self.width / edge];
        // The shelves below the merged ones moved up the list.
        if last > first {
            for (shelf, _) in self.entries.values_mut().filter_map(|e| e.at.as_mut()) {
                if *shelf > last {
                    *shelf -= last - first;
                }
            }
        }
        Some((first, 0))
    }
    /// The newest frame a glyph on `s` was drawn in (0 for none), or `None`
    /// while one of them is protected.
    fn stale_since(&self, s: &Shelf) -> Option<u64> {
        s.cells.iter().flatten().try_fold(0, |newest, slot| {
            (!self.protected(slot.used)).then_some(newest.max(slot.used))
        })
    }
    /// The fewest stale shelves from `first` on that are `edge` tall together:
    /// (first, last, the newest frame any of them was drawn in).
    fn run(&self, first: usize, edge: usize, stale: &[Option<u64>]) -> Option<(usize, usize, u64)> {
        let (mut h, mut newest) = (0, 0);
        let shelves = self.shelves.iter().zip(stale).enumerate().skip(first);
        for (last, (s, since)) in shelves {
            newest = newest.max((*since)?);
            h += s.h;
            if h >= edge {
                return Some((first, last, newest));
            }
        }
        None
    }
    /// Mark the glyph a quad samples as drawn in this frame.
    pub fn touch(&mut self, src: [u16; 4]) {
        let (x, y) = (usize::from(src[0]), usize::from(src[1]));
        let Some(s) = self.shelves.iter_mut().find(|s| s.y < y && y <= s.y + s.h) else {
            return;
        };
        let Some(Some(slot)) = s.cells.get_mut(x.saturating_sub(1) / s.cell) else {
            return;
        };
        if self
            .entries
            .get(&slot.key)
            .is_some_and(|e| e.placed.src == src)
        {
            slot.used = self.frame;
        }
    }
    /// Whether a glyph found no room in this frame while a cell is held
    /// only by the previous frame's drawing, which the next frame may take.
    /// Otherwise drawing again cannot place it until the screen changes.
    pub fn waiting_helps(&self) -> bool {
        self.full.contains(&true)
            && self
                .shelves
                .iter()
                .flat_map(|s| s.cells.iter().flatten())
                .any(|slot| slot.used + 1 == self.frame)
    }
}

/// The CJK font, parsed once.
struct Font {
    face: ttf_parser::Face<'static>,
    units: f32,
}

fn font() -> Option<&'static Font> {
    static FONT: OnceLock<Option<Font>> = OnceLock::new();
    FONT.get_or_init(|| {
        let data = reborn_graphics::cjk_font()?;
        let face = ttf_parser::Face::parse(data, 0).ok()?;
        let units = f32::from(face.units_per_em().max(1));
        Some(Font { face, units })
    })
    .as_ref()
}

/// Whether the CJK font is installed and readable.
pub fn available() -> bool {
    font().is_some()
}

/// `ch` from the CJK font at `px`, when the font has it.
pub fn lookup(ch: char, px: u8) -> Option<Runtime> {
    let f = font()?;
    let id = f.face.glyph_index(ch)?;
    if id.0 == 0 {
        return None;
    }
    let advance = f32::from(f.face.glyph_hor_advance(id).unwrap_or(0)) * f32::from(px) / f.units;
    Some(Runtime {
        ch,
        px,
        id: id.0,
        advance,
    })
}

struct State {
    cache: GlyphCache,
    raster: ab_glyph_rasterizer::Rasterizer,
    /// One cell's texels, reused from glyph to glyph (see `write_cell`).
    block: Vec<u8>,
    /// New glyphs still allowed in this frame; `None` is unlimited.
    budget: Option<usize>,
    per_frame: Option<usize>,
    window: Instant,
    pending: bool,
}

impl State {
    fn new(cache: GlyphCache, per_frame: Option<usize>) -> Self {
        Self {
            cache,
            raster: ab_glyph_rasterizer::Rasterizer::new(1, 1),
            block: Vec::new(),
            budget: per_frame,
            per_frame,
            window: Instant::now(),
            pending: false,
        }
    }
    fn begin_frame(&mut self) {
        self.cache.begin_frame();
        self.budget = self.per_frame;
        self.window = Instant::now();
        self.pending = false;
    }
    fn end_frame(&mut self, quads: &[Quad]) {
        for q in quads.iter().filter(|q| q.tex == Tex::Glyphs) {
            self.cache.touch(q.src);
        }
        // A glyph left blank for want of room asks for another frame only
        // when that frame can free a cell; a page full of what is on screen
        // would otherwise keep the loop drawing.
        if self.cache.waiting_helps() {
            self.pending = true;
        }
    }
    /// What `place` decides before rasterising: a resident glyph, the budget,
    /// a blank glyph, or the cell for a glyph of `bounds()` (read on a miss
    /// only). `Continue` hands back the cell to rasterise into.
    fn reserve(
        &mut self,
        key: Key,
        bounds: impl FnOnce() -> Option<atlas::Bounds>,
    ) -> ControlFlow<Option<Placed>, (Cell, atlas::Bounds)> {
        if let Some(p) = self.cache.get(key) {
            return ControlFlow::Break(Some(p));
        }
        if self.window.elapsed() > BUDGET_WINDOW {
            self.budget = self.per_frame;
            self.window = Instant::now();
        }
        if self.budget == Some(0) {
            self.pending = true;
            return ControlFlow::Break(None);
        }
        let Some(b) = bounds() else {
            self.spend();
            return ControlFlow::Break(Some(self.cache.insert_blank(key)));
        };
        // The cell comes before the pixels: a glyph that finds no room costs
        // no rasterising and no budget, frame after frame.
        let Some(cell) = self.cache.insert(key, b.w, b.h, b.left, b.top) else {
            return ControlFlow::Break(None);
        };
        self.spend();
        ControlFlow::Continue((cell, b))
    }
    fn spend(&mut self) {
        if let Some(b) = self.budget.as_mut() {
            *b -= 1;
        }
    }
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| {
            let cache = GlyphCache::new(GLYPH_PAGE, GLYPH_PAGE);
            Mutex::new(State::new(cache, Some(GLYPHS_PER_FRAME)))
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// New glyphs rasterised per frame; `None` rasterises everything at once
/// (host previews, which draw each state exactly once).
pub fn set_budget(per_frame: Option<usize>) {
    let mut s = state();
    s.per_frame = per_frame;
    s.budget = per_frame;
}

/// Start a frame: a fresh rasterising budget and eviction protection for
/// what this frame draws.
pub fn begin_frame() {
    state().begin_frame();
}

/// End a frame: every runtime glyph in `quads` counts as drawn now (quads
/// kept from an earlier frame, such as a screen sliding out, included).
pub fn end_frame(quads: &[Quad]) {
    state().end_frame(quads);
}

/// Glyphs were left blank in the last frame for want of budget, or of room
/// the next frame frees; draw again.
pub fn pending() -> bool {
    state().pending
}

/// The glyph's place in the page, rasterising it on first use. `None` when it
/// cannot be drawn this frame (budget spent, too large, or the page is full
/// of glyphs on screen); the caller still advances the pen.
pub fn place(g: Runtime) -> Option<Placed> {
    let f = font()?;
    let id = ttf_parser::GlyphId(g.id);
    let scale = f32::from(g.px) / f.units;
    let key = Key { px: g.px, ch: g.ch };
    let mut guard = state();
    let s = &mut *guard;
    let (cell, bounds) = match s.reserve(key, || atlas::bounds(&f.face, id, scale)) {
        ControlFlow::Break(placed) => return placed,
        ControlFlow::Continue(reserved) => reserved,
    };
    let cov = atlas::coverage_within(&f.face, id, scale, bounds, &mut s.raster);
    write_cell(&mut s.block, cell, &cov);
    s.cache.get(key)
}

/// The cell's block with the bitmap at (1, 1) and a clear border, replacing
/// whatever an evicted glyph left there. `block` is the same buffer for
/// every glyph, so a glyph costs no allocation here.
fn write_cell(block: &mut Vec<u8>, cell: Cell, cov: &atlas::Coverage) {
    block.clear();
    block.resize(cell.edge * cell.edge, 0);
    for (row, line) in cov.alpha.chunks_exact(cov.w).enumerate() {
        let at = (row + 1) * cell.edge + 1;
        block[at..at + cov.w].copy_from_slice(line);
    }
    glyph_page().write(cell.x, cell.y, cell.edge, cell.edge, block);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(ch: char) -> Key {
        Key { px: 15, ch }
    }

    #[test]
    fn insert_and_lookup_pack_cells_by_size_class() {
        let mut c = GlyphCache::new(64, 64);
        assert_eq!(c.get(key('一')), None);
        let a = c.insert(key('一'), 15, 14, 1, 13).unwrap();
        assert_eq!(
            a,
            Cell {
                x: 0,
                y: 0,
                edge: 20
            }
        );
        let placed = c.get(key('一')).unwrap();
        assert_eq!(placed.src, [1, 1, 15, 14]);
        assert_eq!((placed.left, placed.top), (1, 13));
        // Same class: next cell on the shelf.
        let b = c.insert(key('二'), 18, 10, 0, 9).unwrap();
        assert_eq!((b.x, b.y, b.edge), (20, 0, 20));
        // A larger glyph opens a shelf of its own class below.
        let big = c.insert(Key { px: 28, ch: '三' }, 27, 28, 0, 25).unwrap();
        assert_eq!((big.x, big.y, big.edge), (0, 20, 32));
        // Too large for any class.
        assert_eq!(c.insert(key('四'), 70, 10, 0, 0), None);
        // Blank glyphs take no cell.
        assert_eq!(c.insert_blank(key('\u{3000}')).src, [0; 4]);
        assert_eq!(c.len(), 4);
    }

    #[test]
    fn eviction_takes_the_least_recently_drawn_and_spares_the_screen() {
        // One shelf of three 20-texel cells.
        let mut c = GlyphCache::new(60, 20);
        for ch in ['a', 'b', 'c'] {
            c.insert(key(ch), 16, 16, 0, 0).unwrap();
        }
        // All three are on screen (this frame): nothing can go.
        assert_eq!(c.insert(key('d'), 16, 16, 0, 0), None);
        c.begin_frame();
        // Still drawn by the previous frame (a sliding screen): kept.
        assert_eq!(c.insert(key('d'), 16, 16, 0, 0), None);
        c.begin_frame();
        // 'b' and 'c' are drawn again; 'a' is the stalest and makes room.
        c.get(key('b'));
        let src_c = c.get(key('c')).unwrap().src;
        let d = c.insert(key('d'), 16, 16, 0, 0).unwrap();
        assert_eq!((d.x, d.y), (0, 0));
        assert_eq!(c.get(key('a')), None);
        assert_eq!(c.evictions, 1);
        // Quads kept from an earlier frame protect their glyphs via `touch`.
        c.begin_frame();
        c.begin_frame();
        c.touch(src_c);
        c.get(key('d'));
        let e = c.insert(key('e'), 16, 16, 0, 0).unwrap();
        assert_eq!((e.x, e.y), (20, 0), "'b' (untouched) is evicted, not 'c'");
        assert!(c.get(key('c')).is_some() && c.get(key('b')).is_none());
    }

    #[test]
    fn a_stale_shelf_of_another_size_is_repurposed() {
        let mut c = GlyphCache::new(64, 32);
        c.insert(key('a'), 16, 16, 0, 0).unwrap();
        c.insert(key('b'), 16, 16, 0, 0).unwrap();
        let big = Key { px: 28, ch: '大' };
        // 32-class needs a 32-high shelf: the 20-high shelf is too short.
        assert_eq!(c.insert(big, 25, 25, 0, 0), None);
        let mut c = GlyphCache::new(64, 40);
        c.insert(Key { px: 36, ch: '大' }, 35, 35, 0, 0).unwrap();
        let small = c.insert(key('a'), 16, 16, 0, 0);
        assert_eq!(small, None, "no room and the big glyph is on screen");
        c.begin_frame();
        c.begin_frame();
        let small = c.insert(key('a'), 16, 16, 0, 0).unwrap();
        assert_eq!((small.x, small.y, small.edge), (0, 0, 20));
        assert!(c.get(Key { px: 36, ch: '大' }).is_none());
        // The repurposed shelf holds 64 / 20 = 3 small cells.
        assert!(c.insert(key('b'), 16, 16, 0, 0).is_some());
        assert!(c.insert(key('c'), 16, 16, 0, 0).is_some());
    }

    #[test]
    fn adjacent_stale_shelves_merge_for_a_taller_class() {
        // 32 texels wide: one 20-texel cell per shelf, two 16-texel ones.
        let mut c = GlyphCache::new(32, 56);
        let small = Key { px: 11, ch: 'c' };
        c.insert(key('a'), 16, 16, 0, 0).unwrap();
        c.insert(key('b'), 16, 16, 0, 0).unwrap();
        c.insert(small, 12, 12, 0, 0).unwrap();
        // The page is full and a 32-texel glyph has no shelf of its own.
        let big = Key { px: 25, ch: '大' };
        assert_eq!(c.insert(big, 25, 25, 0, 0), None, "all on screen");
        c.begin_frame();
        c.begin_frame();
        // 'b' is still drawn: the stale 20 rows above it and 16 below are
        // each too short, and not adjacent.
        c.get(key('b'));
        assert_eq!(c.insert(big, 25, 25, 0, 0), None, "'b' splits the room");
        c.begin_frame();
        c.begin_frame();
        // Nothing on screen: the two 20-row shelves at the top become one.
        let cell = c.insert(big, 25, 25, 0, 0).unwrap();
        assert_eq!((cell.x, cell.y, cell.edge), (0, 0, 32));
        assert!(c.get(key('a')).is_none() && c.get(key('b')).is_none());
        assert_eq!(c.evictions, 2);
        // The glyph below keeps its place.
        assert_eq!(c.get(small).map(|p| p.src), Some([1, 41, 12, 12]));
        assert_eq!((c.shelves.len(), c.shelves[0].h), (2, 40));
        assert_sound(&c);
    }

    /// Every glyph with pixels sits in the cell its entry names, every cell
    /// names a glyph that sits there, and no two bitmaps share a texel of
    /// themselves or their clear border.
    fn assert_sound(c: &GlyphCache) {
        for (i, s) in c.shelves.iter().enumerate() {
            for (j, slot) in s.cells.iter().enumerate() {
                if let Some(slot) = slot {
                    assert_eq!(c.entries[&slot.key].at, Some((i, j)));
                }
            }
        }
        let mut taken = vec![false; c.width * c.height];
        for (key, e) in &c.entries {
            let Some((i, j)) = e.at else {
                continue;
            };
            assert_eq!(c.shelves[i].cells[j].map(|s| s.key), Some(*key));
            let [x, y, w, h] = e.placed.src.map(usize::from);
            for row in y - 1..y + h + 1 {
                for texel in &mut taken[row * c.width + x - 1..][..w + 2] {
                    assert!(!*texel, "{key:?} overlaps another glyph");
                    *texel = true;
                }
            }
        }
    }

    /// A page filled the way browsing a large CJK library at R11 and R15
    /// fills it: 16- and 20-texel shelves to the bottom, drawn in one frame.
    fn browsed_page() -> GlyphCache {
        let mut c = GlyphCache::new(GLYPH_PAGE, GLYPH_PAGE);
        for (i, ch) in ('\u{4E00}'..='\u{9FFF}').enumerate() {
            let (px, side) = if i % 2 == 0 { (11, 12) } else { (15, 16) };
            if c.insert(Key { px, ch }, side, side, 0, 0).is_none() {
                break;
            }
        }
        c
    }

    /// One frame of `screen` (glyphs and their bitmap sides) through
    /// `State::reserve`, as `place` draws it; the glyphs it rasterised.
    fn draw(s: &mut State, screen: &[(Key, usize)]) -> usize {
        s.begin_frame();
        let mut rasterised = 0;
        for &(key, side) in screen {
            let b = atlas::Bounds {
                w: side,
                h: side,
                left: 0,
                top: 0,
            };
            if s.reserve(key, || Some(b)).is_continue() {
                rasterised += 1;
            }
        }
        s.end_frame(&[]);
        rasterised
    }

    /// `screen` drawn the way the main loop draws it, again for as long as
    /// `pending` asks (20 frames at most); the frames drawn and the glyphs
    /// rasterised.
    fn settle(s: &mut State, screen: &[(Key, usize)]) -> (usize, usize) {
        let (mut frames, mut rasterised) = (0, 0);
        loop {
            rasterised += draw(s, screen);
            frames += 1;
            if !s.pending || frames == 20 {
                return (frames, rasterised);
            }
        }
    }

    #[test]
    fn a_frame_with_more_misses_than_the_budget_settles() {
        // Lyrics at S25 (100 glyphs in 32-texel cells) opens over a page a
        // CJK library filled; the library was drawn in the frame before.
        let mut s = State::new(browsed_page(), Some(GLYPHS_PER_FRAME));
        assert!(s.cache.len() > 3000, "{} glyphs browsed", s.cache.len());
        let lyrics: Vec<(Key, usize)> = ('\u{4E00}'..='\u{9FFF}')
            .take(100)
            .map(|ch| (Key { px: 25, ch }, 25))
            .collect();
        // One frame for the library to go, then 48, 48 and 4 glyphs, each
        // rasterised once.
        assert_eq!(settle(&mut s, &lyrics), (4, 100));
        assert!(lyrics.iter().all(|&(k, _)| s.cache.get(k).is_some()));
        assert_sound(&s.cache);
        // Drawn again (the next line lights up): all hits.
        assert_eq!(settle(&mut s, &lyrics), (1, 0));
    }

    #[test]
    fn glyphs_with_no_room_are_not_rasterised_and_let_the_loop_rest() {
        // A page that holds just the screen's nine 20-texel glyphs; sixty
        // 32-texel glyphs beside them can never get a cell.
        let mut s = State::new(GlyphCache::new(64, 60), Some(GLYPHS_PER_FRAME));
        let small = ('a'..='i').map(|ch| (key(ch), 16));
        let big = ('\u{4E00}'..='\u{9FFF}')
            .take(60)
            .map(|ch| (Key { px: 25, ch }, 25));
        let screen: Vec<_> = small.chain(big).collect();
        assert_eq!(settle(&mut s, &screen), (1, 9));
        assert_eq!(settle(&mut s, &screen), (1, 0));
    }

    /// The real font, when the developer points `REBORN_CJK_FONT` at it
    /// (e.g. Y2Linux `.cache/buildroot-dl/y2-font-noto-cjk/`).
    #[test]
    fn real_font_glyphs_rasterise_into_the_shared_page() {
        let Some(g) = lookup('音', 15) else {
            eprintln!("CJK font not available; skipped");
            return;
        };
        assert!((g.advance - 15.).abs() < 0.01, "{}", g.advance);
        let placed = place(g).expect("placed");
        let [x, y, w, h] = placed.src.map(usize::from);
        assert!((10..=17).contains(&w) && (10..=17).contains(&h), "{w}x{h}");
        assert!(placed.top >= 11 && placed.top <= 14, "{}", placed.top);
        let page = glyph_page();
        let ink: u32 = (0..h)
            .flat_map(|r| page.pixels()[(y + r) * GLYPH_PAGE + x..][..w].to_vec())
            .map(u32::from)
            .sum();
        assert!(ink > 255 * 20, "the glyph has coverage");
        drop(page);
        // A second lookup is a hit at the same place.
        assert_eq!(place(g), Some(placed));
        // Hangul and kana too; a space-like glyph is blank.
        assert!(lookup('아', 13).is_some() && lookup('の', 13).is_some());
        let space = lookup('\u{3000}', 15).unwrap();
        assert_eq!(place(space).map(|p| p.src[2]), Some(0));
        assert!(space.advance > 14.);
    }

    /// Host timing: `REBORN_CJK_FONT=… cargo test --release -p reborn-ui
    /// glyph_cache::tests::timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn timing() {
        let started = Instant::now();
        let Some(f) = font() else {
            eprintln!("CJK font not available; skipped");
            return;
        };
        eprintln!(
            "map + parse: {:.2} ms",
            started.elapsed().as_secs_f64() * 1e3
        );
        // 200 distinct common characters: kana, Hangul syllables, Han.
        let text: Vec<char> = "残酷な天使のテーゼ宇多田ヒカル아이유魂のルフラン少年よ神話になれ\
            初恋花束を君に東京事変椎名林檎米津玄師夜に駆ける打上花火恋するフォーチュンクッキー"
            .chars()
            .chain((0..200).filter_map(|i| char::from_u32(0x4E00 + i * 97)))
            .chain((0..40).filter_map(|i| char::from_u32(0xAC00 + i * 211)))
            .filter(|c| !c.is_whitespace() && *c != '\\')
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .take(200)
            .collect();
        assert_eq!(text.len(), 200);
        let mut raster = ab_glyph_rasterizer::Rasterizer::new(1, 1);
        for px in [13u8, 15, 17, 20, 25] {
            let mut cache = GlyphCache::new(GLYPH_PAGE, GLYPH_PAGE);
            let mut page = reborn_graphics::GlyphPage::default();
            let t0 = Instant::now();
            let mut lookups = 0.;
            for &ch in &text {
                let l = Instant::now();
                let g = lookup(ch, px).unwrap();
                lookups += l.elapsed().as_secs_f64();
                let cov = atlas::coverage(
                    &f.face,
                    ttf_parser::GlyphId(g.id),
                    f32::from(px) / f.units,
                    &mut raster,
                    62,
                )
                .unwrap()
                .unwrap();
                let cell = cache
                    .insert(Key { px, ch }, cov.w, cov.h, cov.left, cov.top)
                    .unwrap();
                let mut block = vec![0u8; cell.edge * cell.edge];
                for (row, line) in cov.alpha.chunks_exact(cov.w).enumerate() {
                    let at = (row + 1) * cell.edge + 1;
                    block[at..at + cov.w].copy_from_slice(line);
                }
                page.write(cell.x, cell.y, cell.edge, cell.edge, &block);
            }
            let total = t0.elapsed().as_secs_f64();
            eprintln!(
                "{px} px: 200 glyphs {:.2} ms ({:.1} us each, lookup {:.2} us), dirty rows {:?}",
                total * 1e3,
                total * 1e6 / 200.,
                lookups * 1e6 / 200.,
                page.take_dirty()
            );
        }
        // Metrics only (text::width on a CJK title).
        let t0 = Instant::now();
        let mut w = 0.;
        for _ in 0..1000 {
            w += crate::text::width(crate::text::Face::R15, "残酷な天使のテーゼ 宇多田ヒカル");
        }
        eprintln!(
            "width of a 16-char CJK line: {:.2} us",
            t0.elapsed().as_secs_f64() * 1e3,
        );
        assert!(w > 0.);
    }
}
