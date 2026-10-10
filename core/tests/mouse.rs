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
