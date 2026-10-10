//! A theme reaches the engine (DESK-12, engine half): default colours and the whole 256-colour palette change, a
//! program asking for colours (OSC 10, 11, 12, 4) gets the theme's answer, and the change is a reset for the
//! consumer.
#![cfg(feature = "ghostty")]

mod common;

use common::{ascii, engine};
use holzwave_core::engine::EngineTheme;
use holzwave_core::{Color, GhosttyEngine, TerminalEngine};

const fn c(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b }
}

/// fg #112233, bg #445566, cursor #778899 and sixteen colours that differ from each other and from the defaults.
fn theme() -> EngineTheme {
    let mut ansi = [Color::default(); 16];
    for (i, slot) in ansi.iter_mut().enumerate() {
        let i = i as u8;
        *slot = c(10 + i * 13, 220 - i * 9, 31 + i * 7);
    }
    EngineTheme { fg: c(0x11, 0x22, 0x33), bg: c(0x44, 0x55, 0x66), cursor: c(0x77, 0x88, 0x99), ansi }
}

/// The colour of the first cell after feeding `bytes` (one character with the given SGR).
fn first_cell_fg(e: &mut GhosttyEngine, bytes: &str) -> Color {
    e.feed(b"\x1b[2J\x1b[H");
    e.feed(bytes.as_bytes());
    e.screen().expect("screen").cells[0].fg
}

fn themed() -> GhosttyEngine {
    let mut e = engine(20, 3);
    assert!(e.set_theme(&theme()).is_ok(), "set_theme must be accepted");
    e
}

#[test]
fn a_theme_change_is_a_full_reset_with_the_theme_defaults() {
    let mut e = engine(20, 3);
    e.update().expect("first update");
    assert!(e.set_theme(&theme()).is_ok(), "set_theme must be accepted");

    let u = e.update().expect("update");
    assert!(u.reset, "the consumer must replace everything after a theme change");
    assert!(u.full);
    assert_eq!(u.default_fg, c(0x11, 0x22, 0x33));
    assert_eq!(u.default_bg, c(0x44, 0x55, 0x66));
    assert_eq!(u.changed.len(), 3, "every row is in the reset update");

    // And only once: the next update is quiet again.
    let quiet = e.update().expect("update");
    assert!(!quiet.reset && !quiet.full);
}

#[test]
fn sgr_colours_resolve_to_the_themes_sixteen_colours() {
    let mut e = themed();
    let t = theme();
    assert_eq!(first_cell_fg(&mut e, "\x1b[31mx"), t.ansi[1], "SGR 31");
    assert_eq!(first_cell_fg(&mut e, "\x1b[32mx"), t.ansi[2], "SGR 32");
    assert_eq!(first_cell_fg(&mut e, "\x1b[91mx"), t.ansi[9], "SGR 91");
    assert_eq!(first_cell_fg(&mut e, "\x1b[97mx"), t.ansi[15], "SGR 97");
    for n in 0..16usize {
        assert_eq!(first_cell_fg(&mut e, &format!("\x1b[38;5;{n}mx")), t.ansi[n], "38;5;{n}");
    }
}

#[test]
fn the_cube_and_the_grey_ramp_exist_and_differ_from_the_engine_defaults() {
    let mut themed = themed();
    let mut plain = engine(20, 3);
    let mut seen = Vec::new();
    for n in [16u8, 21, 46, 196, 201, 231, 232, 240, 255] {
        let sgr = format!("\x1b[38;5;{n}mx");
        let with_theme = first_cell_fg(&mut themed, &sgr);
        let without = first_cell_fg(&mut plain, &sgr);
        assert_ne!(with_theme, without, "entry {n} must come from the theme, not the engine default");
        seen.push(with_theme);
    }
    seen.sort_by_key(|c| (c.r, c.g, c.b));
    seen.dedup();
    assert!(seen.len() >= 8, "the generated entries are not all one colour: {seen:?}");
}

#[test]
fn colour_queries_are_answered_with_the_theme() {
    let mut e = themed();
    e.take_replies();

    e.feed(b"\x1b]11;?\x07");
    assert_eq!(ascii(&e.take_replies()), r"\x1b]11;rgb:4444/5555/6666\x1b\\", "OSC 11 is the background");
    e.feed(b"\x1b]10;?\x07");
    assert_eq!(ascii(&e.take_replies()), r"\x1b]10;rgb:1111/2222/3333\x1b\\", "OSC 10 is the foreground");
    e.feed(b"\x1b]12;?\x07");
    assert_eq!(ascii(&e.take_replies()), r"\x1b]12;rgb:7777/8888/9999\x1b\\", "OSC 12 is the cursor");

    let a1 = theme().ansi[1];
    e.feed(b"\x1b]4;1;?\x07");
    let expected = format!(
        r"\x1b]4;1;rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x1b\\",
        a1.r, a1.g, a1.b
    );
    assert_eq!(ascii(&e.take_replies()), expected, "OSC 4 is a palette entry");
}

#[test]
fn a_programs_own_background_wins_over_the_theme_until_osc_111() {
    // Documented behaviour (RESEARCH Pitfall 11): a colour the program set with OSC 11 beats the theme's default
    // until the program resets it with OSC 111. A theme switch does not recolour such a pane.
    let mut e = engine(20, 3);
    e.update().expect("first update");
    e.feed(b"\x1b]11;rgb:ff/00/00\x07");
    assert!(e.set_theme(&theme()).is_ok(), "set_theme must be accepted");

    let overridden = e.update().expect("update");
    assert!(overridden.reset, "the theme change is a reset either way");
    assert_eq!(overridden.default_bg, c(0xff, 0, 0), "the program's colour stays");
    e.take_replies();
    e.feed(b"\x1b]11;?\x07");
    assert_eq!(ascii(&e.take_replies()), r"\x1b]11;rgb:ffff/0000/0000\x1b\\");

    e.feed(b"\x1b]111\x07");
    let restored = e.update().expect("update");
    assert_eq!(restored.default_bg, c(0x44, 0x55, 0x66), "OSC 111 gives the theme's colour back");
}

#[test]
fn a_second_theme_replaces_the_first() {
    let mut e = themed();
    let mut other = theme();
    other.fg = c(0xaa, 0xbb, 0xcc);
    other.bg = c(0x01, 0x02, 0x03);
    other.ansi[1] = c(9, 8, 7);
    assert!(e.set_theme(&other).is_ok());

    let u = e.update().expect("update");
    assert_eq!((u.default_fg, u.default_bg), (c(0xaa, 0xbb, 0xcc), c(1, 2, 3)));
    assert_eq!(first_cell_fg(&mut e, "\x1b[31mx"), c(9, 8, 7));
}
