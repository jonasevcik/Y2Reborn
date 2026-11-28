use super::*;

fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[test]
fn the_tokens_turn_light_and_keep_their_alpha() {
    assert_eq!(light(color::BG), BG);
    assert_eq!(light(color::TEXT), 0x15171CFF);
    assert_eq!(light(color::SHEET) & 0xFF, color::SHEET & 0xFF);
    // Type on the accent stays white; a see-through accent fill gets more solid.
    assert_eq!(light(color::ON_ACCENT), 0xFFFFFFFF);
    assert_eq!(light(crate::theme::alpha(color::ACCENT, 120)), 0x2563D9F0);
    // Text on a light pill (ink on TEXT) becomes light on dark.
    assert_eq!(light(color::INK), BG);
    for token in [
        color::TEXT,
        color::MUTED,
        color::DIM,
        color::ACCENT,
        color::DANGER,
    ] {
        assert!(contrast(light(token), BG) >= 4.5, "{token:08X}");
    }
}

#[test]
fn glass_turns_dark_and_shadows_soften() {
    assert_eq!(light(color::GLASS), 0x0000_0019);
    assert_eq!(light(color::RULE) >> 8, 0);
    assert_eq!(light(0x0000_0090), 0x0000_0039, "a shadow, softer");
    assert_eq!(light(0x0000_00FF), 0x0000_0066);
}

#[test]
fn greys_turn_around_and_bright_colours_get_readable() {
    // A grey between text and background stays between them.
    let mid = crate::theme::mix(color::TEXT, color::BG, 0.5);
    let shown = light(mid);
    assert!(luminance(shown) > luminance(light(color::TEXT)));
    assert!(luminance(shown) < luminance(BG));
    // A bright cover accent keeps its hue and reads on the light background.
    let accent = light(0xFFB04AFF);
    assert!(contrast(accent, BG) >= 4.5);
    let (h, _, _) = hsl(accent);
    assert!((h - hsl(0xFFB04AFF).0).abs() < 4., "hue kept: {h}");
    // A dark placeholder tile becomes a pale one.
    assert!(luminance(light(0x2A3040FF)) > 0.5);
}

#[test]
fn only_flat_colours_type_and_icons_change() {
    let mut frame = vec![
        Quad::rect(0., 0., 10., 10., color::BG),
        Quad {
            tex: Tex::Thumb(7),
            ..Quad::rect(0., 0., 10., 10., color::TEXT)
        },
        Quad {
            tex: Tex::Art,
            ..Quad::rect(0., 0., 10., 10., color::TEXT)
        },
        Quad {
            tex: Tex::Font(0),
            ..Quad::rect(0., 0., 10., 10., color::TEXT)
        },
    ];
    apply(&mut frame);
    assert_eq!(frame[0].color, BG);
    assert_eq!(frame[1].color, color::TEXT, "covers keep their tint");
    assert_eq!(frame[2].color, color::TEXT);
    assert_eq!(frame[3].color, 0x15171CFF);
}

#[test]
fn each_colour_of_a_frame_is_worked_out_once_as_before() {
    // Every kind of colour: the tokens down to clear, glass, shadows, greys, bright cover
    // accents, and enough others for colours to share a slot of the memo.
    let mut colours: Vec<u32> = TOKENS
        .iter()
        .flat_map(|(from, _)| [0xFF, 0x80, 0x24, 0].map(|a| *from & !0xFF | a))
        .collect();
    colours.extend([
        color::GLASS,
        color::RULE,
        color::SCRIM,
        0x0000_00FF,
        0,
        0xFFB0_4AFF,
        0xFFB0_4A00,
        0x2A30_40FF,
        crate::theme::mix(color::TEXT, color::BG, 0.5),
    ]);
    let mut x = 0x1234_5678_u32;
    colours.extend((0..2000).map(|_| {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        x
    }));
    let mut memo = Memo::EMPTY;
    for _ in 0..2 {
        for &c in &colours {
            assert_eq!(memo.light(c), light(c), "{c:08X}");
        }
    }
    // A colour once worked out is taken from the memo, not worked out again.
    let mut memo = Memo::EMPTY;
    memo.light(0xFFB0_4AFF);
    for slot in memo.0.iter_mut().flatten() {
        slot.1 = 0x0102_03FF;
    }
    assert_eq!(memo.light(0xFFB0_4AFF), 0x0102_03FF);
    // Now Playing's wave in a cover's colour, its feathered edges clear: recoloured as before.
    let mut c = crate::canvas::Canvas::default();
    crate::components::wave_progress(
        &mut c,
        16.,
        300.,
        448.,
        0.9,
        0xFFB0_4AFF,
        0.,
        3.6,
        true,
        None,
    );
    let mut frame = c.draw;
    assert!(frame.iter().filter(|q| q.warp.is_some()).count() > 400);
    let shown = |frame: &[Quad]| -> Vec<(u32, Option<[u32; 4]>)> {
        frame
            .iter()
            .map(|q| (q.color, q.warp.map(|w| w.colors)))
            .collect()
    };
    let before: Vec<_> = shown(&frame)
        .into_iter()
        .map(|(c, w)| (light(c), w.map(|w| w.map(light))))
        .collect();
    apply(&mut frame);
    assert_eq!(shown(&frame), before);
}

#[test]
fn round_artist_pictures_have_the_focus_rows_colour_at_their_corners() {
    use crate::components::{FOCUS_FILL, FOCUS_OVER_BG};
    // The corners take the focus row's glass over the page: in the dark look…
    let over = |glass: u32, page: u32| crate::theme::mix(page, glass, (glass & 0xFF) as f32 / 255.);
    assert_eq!(over(FOCUS_FILL, color::BG), FOCUS_OVER_BG);
    // …and in the light one, with no darker wedges on the row.
    assert_eq!(light(FOCUS_OVER_BG), over(light(FOCUS_FILL), BG));
}

/// A film on Now Playing keeps the dark look, as it does in a skin (Y2Reborn #131): the picture
/// on black, white subtitles with a dark edge, and the shade under the title and time line.
#[test]
fn a_film_keeps_its_colours() {
    use reborn_core::{AppModel, PlaybackState, Screen, Track};
    let film = Track {
        title: "Holiday".into(),
        codec: "h264/aac".into(),
        duration_ms: 125_000,
        ..Default::default()
    };
    let mut m = AppModel {
        screen: Screen::NowPlaying,
        ..Default::default()
    };
    m.replace_queue(vec![film], 0).unwrap();
    // Paused: the title, the time line and the shade under them show.
    m.playback = PlaybackState::Paused;
    let mut ui = crate::Ui {
        video: crate::views_videos::VideoPicture {
            width: 480,
            height: 270,
            subtitle: "Where were you?".into(),
        },
        ..Default::default()
    };
    let colours = |ui: &crate::Ui, m: &AppModel| -> Vec<(u32, Option<[u32; 4]>)> {
        let frame = ui.draw(m, &[], false);
        frame
            .iter()
            .map(|q| (q.color, q.warp.map(|w| w.colors)))
            .collect()
    };
    let dark = colours(&ui, &m);
    assert!(dark.contains(&(0x0000_00E0, None)), "the subtitle's edge");
    ui.light = true;
    let lit = colours(&ui, &m);
    assert!(lit.contains(&(0x0000_00FF, None)), "on black");
    assert_eq!(lit, dark);
    // A screen sliding out as the film comes in is still in the light look.
    let ui = crate::Ui {
        ui_motion: true,
        light: true,
        video: ui.video.clone(),
        ..Default::default()
    };
    m.screen = Screen::Home;
    ui.draw(&m, &[], false);
    m.screen = Screen::NowPlaying;
    let sliding = colours(&ui, &m);
    assert!(sliding.contains(&(BG, None)), "the light page slides out");
    assert!(sliding.contains(&(0x0000_00FF, None)), "the film's black");
}
