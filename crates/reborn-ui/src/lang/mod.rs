//! The language of Reborn's own text (menus, settings, messages).
//!
//! The English text is the key: `tr("Crossfade")` is the text in the chosen language and the
//! English itself where a language has no translation yet, so a screen is never blank and a
//! call site still reads as the English it shows. Each language is one table sorted by its
//! English text (`cs.rs`), found by binary search like a glyph (`text::find`).
//!
//! The chosen language is one process-wide value, set from the settings at the top of the
//! UI's entry points (`Ui::draw`, `Ui::model_action`, `Ui::observe`), so the page builders need
//! no extra argument. A language only changes text: keys, filters and saved state stay English,
//! which keeps `/data` readable by older builds.
use reborn_core::Language;
mod cs;

/// One language's text: (English, translation), sorted by English.
type Table = &'static [(&'static str, &'static str)];

/// The chosen language's place in `Language::ALL`. One value for the whole process, so text is
/// the same on whichever thread draws it; tests run in parallel threads, so there each thread
/// has its own and a test that chooses Czech cannot change another test's English.
#[cfg(not(test))]
mod chosen {
    use std::sync::atomic::{AtomicU8, Ordering};
    static INDEX: AtomicU8 = AtomicU8::new(0);
    pub(super) fn store(index: u8) {
        INDEX.store(index, Ordering::Relaxed);
    }
    pub(super) fn load() -> u8 {
        INDEX.load(Ordering::Relaxed)
    }
}
#[cfg(test)]
mod chosen {
    use std::cell::Cell;
    thread_local!(static INDEX: Cell<u8> = const { Cell::new(0) });
    pub(super) fn store(index: u8) {
        INDEX.with(|i| i.set(index));
    }
    pub(super) fn load() -> u8 {
        INDEX.with(Cell::get)
    }
}

/// Use `language` for the text drawn from now on.
pub fn set(language: Language) {
    let index = Language::ALL
        .iter()
        .position(|l| *l == language)
        .unwrap_or(0);
    chosen::store(index as u8);
}

/// The language text is drawn in.
pub fn current() -> Language {
    Language::ALL
        .get(usize::from(chosen::load()))
        .copied()
        .unwrap_or_default()
}

fn table(language: Language) -> Option<Table> {
    match language {
        Language::English => None,
        Language::Czech => Some(cs::TABLE),
    }
}

/// `text` in `language`; English where there is no translation.
pub fn tr_in(language: Language, text: &'static str) -> &'static str {
    let Some(table) = table(language) else {
        return text;
    };
    table
        .binary_search_by_key(&text, |entry| entry.0)
        .map_or(text, |i| table[i].1)
}

/// `text` in the chosen language; English where there is no translation.
pub fn tr(text: &'static str) -> &'static str {
    tr_in(current(), text)
}

/// Which of a noun's three forms goes with `n` in `language`: English has two (one, and the
/// rest, written as `few` and `many` alike); Czech has "1 skladba", "2-4 skladby", "5 skladeb".
pub fn plural_in(
    language: Language,
    n: usize,
    one: &'static str,
    few: &'static str,
    many: &'static str,
) -> &'static str {
    match language {
        Language::English => {
            if n == 1 {
                one
            } else {
                many
            }
        }
        Language::Czech => match n {
            1 => one,
            2..=4 => few,
            _ => many,
        },
    }
}

/// `plural_in` for the chosen language.
pub fn plural(n: usize, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    plural_in(current(), n, one, few, many)
}

#[cfg(test)]
mod tests;
