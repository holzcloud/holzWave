//! Mouse reporting and the wheel policy of the engine (D-24, DESK-09).
//!
//! Every expected byte string here was measured on the pinned libghostty revision. The webview sends device-pixel
//! positions and buttons; the engine decides from the live terminal modes whether and how the program hears of them.
#![cfg(feature = "ghostty")]

mod common;

use common::{ascii, engine};
use holzwave_core::GhosttyEngine;
use holzwave_core::TerminalEngine;
use holzwave_core::engine::{Mods, MouseAction, MouseButton, MouseEvent};

/// The cell size of every test grid: 8 x 16 device pixels.
const CELL: (u32, u32) = (8, 16);

/// A 40 x 10 grid with the cell size told to the engine.
fn grid() -> GhosttyEngine {
    let mut e = engine(40, 10);
    e.resize(40, 10, CELL).expect("resize");
    e
}

/// The pixel at the centre of cell (`col`, `row`), as the webview would report a pointer in the middle of it.
fn centre(col: u32, row: u32) -> (f32, f32) {
    ((col * CELL.0 + CELL.0 / 2) as f32, (row * CELL.1 + CELL.1 / 2) as f32)
}

fn event(action: MouseAction, button: MouseButton, mods: Mods, at: (f32, f32)) -> MouseEvent {
    MouseEvent { action, button, mods, x: at.0, y: at.1 }
}

/// What the engine reports for `event`, as readable ASCII.
fn report(e: &mut impl TerminalEngine, event: MouseEvent) -> String {
    ascii(&e.mouse(&event).expect("mouse"))
}

fn press(e: &mut impl TerminalEngine, button: MouseButton, at: (f32, f32)) -> String {
    report(e, event(MouseAction::Press, button, Mods::NONE, at))
}

fn release(e: &mut impl TerminalEngine, button: MouseButton, at: (f32, f32)) -> String {
    report(e, event(MouseAction::Release, button, Mods::NONE, at))
}

#[test]
fn a_press_and_a_release_are_encoded_in_sgr_when_the_program_asked_for_them() {
    let mut e = grid();
    e.feed(b"\x1b[?1000h\x1b[?1006h");

    assert_eq!(press(&mut e, MouseButton::Left, centre(4, 2)), r"\x1b[<0;5;3M");
    assert_eq!(release(&mut e, MouseButton::Left, centre(4, 2)), r"\x1b[<0;5;3m");
}

#[test]
fn without_a_tracking_mode_nothing_is_reported() {
    let mut e = grid();

    assert_eq!(press(&mut e, MouseButton::Left, centre(4, 2)), "");
    assert_eq!(release(&mut e, MouseButton::Left, centre(4, 2)), "");
    // A format without a tracking mode is still no tracking.
    e.feed(b"\x1b[?1006h");
    assert_eq!(press(&mut e, MouseButton::Left, centre(4, 2)), "");
}

#[test]
fn text_output_does_not_change_the_result() {
    let mut e = grid();
    e.feed(b"hello\r\nworld\r\n");
    assert_eq!(press(&mut e, MouseButton::Left, centre(4, 2)), "");

    e.feed(b"\x1b[?1000h\x1b[?1006h");
    e.feed(b"more text\r\n");
    assert_eq!(press(&mut e, MouseButton::Left, centre(4, 2)), r"\x1b[<0;5;3M");
    assert_eq!(release(&mut e, MouseButton::Left, centre(4, 2)), r"\x1b[<0;5;3m");
}

/// The motion of the pointer with `button` held, to the centre of a cell.
fn motion(e: &mut impl TerminalEngine, button: MouseButton, at: (f32, f32)) -> String {
    report(e, event(MouseAction::Motion, button, Mods::NONE, at))
}

#[test]
fn x10_reports_only_the_press_of_a_button_as_esc_bracket_m_and_three_bytes() {
    let mut e = grid();
    e.feed(b"\x1b[?9h");

    // X10 (DECSET 9): ESC [ M, then button, column and row each plus 32, one-based for the position.
    assert_eq!(press(&mut e, MouseButton::Left, centre(0, 0)), r"\x1b[M !!");
    // Right is button 2, which is the quote character once 32 is added; `ascii` shows it escaped.
    assert_eq!(press(&mut e, MouseButton::Right, centre(4, 2)), r#"\x1b[M\"%#"#);
    assert_eq!(release(&mut e, MouseButton::Right, centre(4, 2)), "", "X10 has no release");
    assert_eq!(motion(&mut e, MouseButton::None, centre(5, 2)), "", "X10 has no motion");
}

#[test]
fn normal_tracking_in_the_legacy_format_reports_press_and_release_but_not_motion() {
    let mut e = grid();
    e.feed(b"\x1b[?1000h");

    assert_eq!(press(&mut e, MouseButton::Left, centre(0, 0)), r"\x1b[M !!");
    // The legacy format has one release code for every button: 3 plus 32.
    assert_eq!(release(&mut e, MouseButton::Left, centre(0, 0)), r"\x1b[M#!!");
    assert_eq!(motion(&mut e, MouseButton::None, centre(5, 2)), "");
}

#[test]
fn a_column_the_legacy_format_cannot_express_never_panics() {
    let mut e = engine(300, 10);
    e.resize(300, 10, CELL).expect("resize");
    e.feed(b"\x1b[?1000h");

    // Column 250 plus 33 is 283: past the single byte of the legacy format. The pinned encoder reports nothing
    // rather than a wrong column.
    assert_eq!(press(&mut e, MouseButton::Left, centre(250, 2)), "");
    // The last column it can express: 222 plus 33 is 255.
    assert_eq!(press(&mut e, MouseButton::Left, centre(222, 2)), "\\x1b[M \\xff#");
}

#[test]
fn button_tracking_reports_motion_only_while_a_button_is_held_and_once_per_cell() {
    let mut e = grid();
    e.feed(b"\x1b[?1002h\x1b[?1006h");

    assert_eq!(motion(&mut e, MouseButton::None, centre(5, 2)), "", "no button held");

    assert_eq!(press(&mut e, MouseButton::Left, centre(4, 2)), r"\x1b[<0;5;3M");
    assert_eq!(motion(&mut e, MouseButton::Left, centre(5, 2)), r"\x1b[<32;6;3M");
    // Two pixels further, same cell: reported once.
    assert_eq!(motion(&mut e, MouseButton::Left, (centre(5, 2).0 + 2.0, centre(5, 2).1)), "");
    // The engine knows the button is held even when the caller says nothing about it.
    assert_eq!(motion(&mut e, MouseButton::None, centre(6, 2)), r"\x1b[<32;7;3M");

    assert_eq!(release(&mut e, MouseButton::Left, centre(6, 2)), r"\x1b[<0;7;3m");
    assert_eq!(motion(&mut e, MouseButton::None, centre(7, 2)), "", "the button was released");
}

#[test]
fn any_event_tracking_reports_motion_with_no_button() {
    let mut e = grid();
    e.feed(b"\x1b[?1003h\x1b[?1006h");

    assert_eq!(motion(&mut e, MouseButton::None, centre(4, 2)), r"\x1b[<35;5;3M");
    assert_eq!(motion(&mut e, MouseButton::None, centre(5, 2)), r"\x1b[<35;6;3M");
}

#[test]
fn modifiers_and_buttons_add_to_the_button_code() {
    let mut e = grid();
    e.feed(b"\x1b[?1000h\x1b[?1006h");
    let at = centre(4, 2);

    for (button, mods, expected) in [
        (MouseButton::Left, Mods::SHIFT, r"\x1b[<4;5;3M"),
        (MouseButton::Left, Mods::ALT, r"\x1b[<8;5;3M"),
        (MouseButton::Left, Mods::CTRL, r"\x1b[<16;5;3M"),
        (MouseButton::Left, Mods::SHIFT | Mods::CTRL, r"\x1b[<20;5;3M"),
        (MouseButton::Middle, Mods::NONE, r"\x1b[<1;5;3M"),
        (MouseButton::Right, Mods::NONE, r"\x1b[<2;5;3M"),
        (MouseButton::Right, Mods::ALT | Mods::CTRL, r"\x1b[<26;5;3M"),
    ] {
        assert_eq!(report(&mut e, event(MouseAction::Press, button, mods, at)), expected, "{button:?} {mods:?}");
        // Leave nothing held between rows of the table.
        e.mouse(&event(MouseAction::Release, button, mods, at)).expect("release");
    }
}

#[test]
fn the_utf8_format_encodes_large_columns_as_two_bytes() {
    let mut e = engine(200, 10);
    e.resize(200, 10, CELL).expect("resize");
    e.feed(b"\x1b[?1000h\x1b[?1005h");

    assert_eq!(press(&mut e, MouseButton::Left, centre(4, 2)), r"\x1b[M %#");
    // Column 100 is 133 once one-based and shifted by 32: U+0085 is C2 85 in UTF-8.
    assert_eq!(press(&mut e, MouseButton::Left, centre(100, 2)), r"\x1b[M \xc2\x85#");
}

#[test]
fn the_urxvt_format_reports_decimal_numbers() {
    let mut e = grid();
    e.feed(b"\x1b[?1000h\x1b[?1015h");

    assert_eq!(press(&mut e, MouseButton::Left, centre(4, 2)), r"\x1b[32;5;3M");
    assert_eq!(release(&mut e, MouseButton::Left, centre(4, 2)), r"\x1b[35;5;3M");
}

#[test]
fn the_sgr_pixels_format_reports_the_pixel_the_encoder_computes() {
    let mut e = grid();
    e.feed(b"\x1b[?1000h\x1b[?1016h");

    // Named expectation: SGR-Pixels carries the pixel position, zero-based as the webview sent it.
    assert_eq!(press(&mut e, MouseButton::Left, (36.0, 40.0)), r"\x1b[<0;36;40M");
    assert_eq!(release(&mut e, MouseButton::Left, (36.0, 40.0)), r"\x1b[<0;36;40m");
}

#[test]
fn a_reset_of_the_terminal_leaves_no_button_held() {
    let mut e = grid();
    e.feed(b"\x1b[?1002h\x1b[?1006h");
    press(&mut e, MouseButton::Left, centre(4, 2));

    // RIS drops the tracking mode; the program turns it on again later. The button of the earlier press must not
    // come back as a phantom drag.
    e.feed(b"\x1bc");
    assert_eq!(press(&mut e, MouseButton::Left, centre(1, 1)), "", "no tracking after the reset");
    e.feed(b"\x1b[?1002h\x1b[?1006h");

    assert_eq!(motion(&mut e, MouseButton::None, centre(5, 2)), "");
}

#[test]
fn a_release_the_program_never_asked_for_leaves_nothing_held() {
    let mut e = grid();
    e.feed(b"\x1b[?1002h\x1b[?1006h");
    press(&mut e, MouseButton::Left, centre(4, 2));
    // The program stops tracking while the button is down, and the release arrives with tracking off.
    e.feed(b"\x1b[?1002l");
    assert_eq!(release(&mut e, MouseButton::Left, centre(4, 2)), "");
    e.feed(b"\x1b[?1002h");

    assert_eq!(motion(&mut e, MouseButton::None, centre(6, 2)), "");
}

#[test]
fn odd_positions_never_panic_and_stay_on_the_grid() {
    let mut e = grid();
    e.feed(b"\x1b[?1003h\x1b[?1006h");

    // Not finite or negative: cell (0, 0). Far outside (but finite): the last cell (39, 9).
    for (x, y, expected) in [
        (f32::NAN, f32::NAN, r"\x1b[<35;1;1M"),
        (f32::INFINITY, f32::NEG_INFINITY, r"\x1b[<35;1;1M"),
        (f32::INFINITY, 100.0, r"\x1b[<35;1;7M"),
        (-5.0, -1.0, r"\x1b[<35;1;1M"),
        (1.0e9, 1.0e9, r"\x1b[<35;40;10M"),
        (320.0, 160.0, r"\x1b[<35;40;10M"),
        (319.9, 159.9, r"\x1b[<35;40;10M"),
    ] {
        // Different cells in turn, so that the last-cell tracking does not hide a row of the table.
        e.mouse(&event(MouseAction::Motion, MouseButton::None, Mods::NONE, centre(20, 5))).expect("motion");
        assert_eq!(motion(&mut e, MouseButton::None, (x, y)), expected, "({x}, {y})");
    }
}

#[test]
fn an_unknown_cell_size_is_cell_zero_zero_and_never_a_division_by_zero() {
    let mut e = engine(40, 10);
    // No resize: the cell size is unknown. A resize with (0, 0) says so explicitly.
    e.feed(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(press(&mut e, MouseButton::Left, (36.0, 40.0)), r"\x1b[<0;1;1M");

    e.resize(40, 10, (0, 0)).expect("resize");
    assert_eq!(press(&mut e, MouseButton::Left, (300.0, 100.0)), r"\x1b[<0;1;1M");
    assert_eq!(release(&mut e, MouseButton::Left, (300.0, 100.0)), r"\x1b[<0;1;1m");
}
