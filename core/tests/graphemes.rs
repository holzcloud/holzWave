//! Grapheme clusters and character widths (DESK-27): a cluster takes the display width Unicode prescribes, the
//! cursor advances by exactly that width, and the wire format keeps one code point per cell with the tail of a
//! cluster in `extras`. `core/src/ghostty.rs` holds the companion unit test that compares the cursor advance with
//! the library's own `grapheme_width`.
#![cfg(feature = "ghostty")]

mod common;

use common::{ascii, engine};
use holzwave_core::engine::{SelectOp, Width};
use holzwave_core::wire::{FrameEncoder, RowMsg};
use holzwave_core::{GhosttyEngine, TerminalEngine};

/// One case: the text fed at the start of a row, the columns the cursor must advance and the cells at the start of
/// the row (`text`, `width` of each cell; a spacer cell has no text of its own).
struct Case {
    name: &'static str,
    text: &'static str,
    advance: u16,
    cells: &'static [(&'static str, Width)],
}

const N: Width = Width::Narrow;
const W: Width = Width::Wide;
const S: Width = Width::Spacer;

const CASES: &[Case] = &[
    Case { name: "letter and combining acute", text: "e\u{0301}", advance: 1, cells: &[("e\u{0301}", N)] },
    Case {
        name: "CJK pair",
        text: "\u{65E5}\u{672C}",
        advance: 4,
        cells: &[("\u{65E5}", W), ("", S), ("\u{672C}", W), ("", S)],
    },
    Case { name: "emoji", text: "\u{1F600}", advance: 2, cells: &[("\u{1F600}", W), ("", S)] },
    Case {
        name: "ZWJ family",
        text: "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
        advance: 2,
        cells: &[("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", W), ("", S)],
    },
    Case {
        name: "flag of Switzerland",
        text: "\u{1F1E8}\u{1F1ED}",
        advance: 2,
        cells: &[("\u{1F1E8}\u{1F1ED}", W), ("", S)],
    },
    Case { name: "heart with VS16", text: "\u{2764}\u{FE0F}", advance: 2, cells: &[("\u{2764}\u{FE0F}", W), ("", S)] },
    Case {
        name: "thumbs up with skin tone",
        text: "\u{1F44D}\u{1F3FD}",
        advance: 2,
        cells: &[("\u{1F44D}\u{1F3FD}", W), ("", S)],
    },
    Case {
        name: "Hangul jamo syllable",
        text: "\u{1112}\u{1161}\u{11AB}",
        advance: 2,
        cells: &[("\u{1112}\u{1161}\u{11AB}", W), ("", S)],
    },
    Case {
        name: "combining mark after a wide character",
        text: "\u{65E5}\u{0301}",
        advance: 2,
        cells: &[("\u{65E5}\u{0301}", W), ("", S)],
    },
];

/// A 20x3 engine that has been fed `text` at the start of the first row.
fn fed(text: &str) -> GhosttyEngine {
    let mut e = engine(20, 3);
    e.feed(text.as_bytes());
    e
}

fn cursor_x(e: &mut GhosttyEngine) -> u16 {
    e.update().expect("update").cursor.expect("cursor in the viewport").x
}

/// The text a consumer rebuilds from a wire row: one code point per cell, `\0` marks the second column of a wide
/// character, and the tail of a cluster comes from `extras` at the column of its first code point.
fn rebuild(row: &RowMsg) -> String {
    let mut out = String::new();
    let mut x: u16 = 0;
    for run in &row.runs {
        assert_eq!(run.0.chars().count(), usize::from(run.1), "a run's text has one code point per column");
        for ch in run.0.chars() {
            if ch != '\0' {
                out.push(ch);
                if let Some((_, tail)) = row.extras.iter().find(|(col, _)| *col == x) {
                    out.push_str(tail);
                }
            }
            x += 1;
        }
    }
    out
}

#[test]
fn the_engine_runs_with_grapheme_clustering_on() {
    let mut e = engine(20, 3);
    e.take_replies();
    // DECRQM: ESC [ ? 2027 $ p is answered with ESC [ ? 2027 ; Ps $ y, Ps 1 = set, 2 = reset.
    e.feed(b"\x1b[?2027$p");
    assert_eq!(ascii(&e.take_replies()), r"\x1b[?2027;1$y", "clustering is on at construction");
}

#[test]
fn decrqm_reports_the_state_the_engine_is_really_in() {
    let mut e = engine(20, 3);
    e.take_replies();
    e.feed(b"\x1b[?2027l\x1b[?2027$p");
    assert_eq!(ascii(&e.take_replies()), r"\x1b[?2027;2$y", "a program that switched it off sees it off");

    // And the layout follows the state: with clustering off a ZWJ family is not one cluster any more.
    e.feed(b"\x1b[2J\x1b[H\xf0\x9f\x91\xa8\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7");
    assert_ne!(cursor_x(&mut e), 2, "three emoji are three wide cells without clustering");

    e.feed(b"\x1b[?2027h\x1b[?2027$p");
    assert_eq!(ascii(&e.take_replies()), r"\x1b[?2027;1$y");
}

#[test]
fn every_case_advances_the_cursor_by_the_width_unicode_prescribes() {
    for case in CASES {
        let mut e = fed(case.text);
        assert_eq!(cursor_x(&mut e), case.advance, "{}: {:?}", case.name, case.text);
    }
}

#[test]
fn the_first_cell_holds_the_whole_cluster_and_a_wide_cell_is_followed_by_a_spacer() {
    for case in CASES {
        let mut e = fed(case.text);
        let screen = e.screen().expect("screen");
        for (i, (text, width)) in case.cells.iter().enumerate() {
            let cell = &screen.cells[i];
            assert_eq!(cell.width, *width, "{}: cell {i} width", case.name);
            if *width != S {
                assert_eq!(cell.text, *text, "{}: cell {i} text", case.name);
            }
        }
        // Nothing else was written on the row.
        let rest = &screen.cells[case.cells.len()..20];
        assert!(rest.iter().all(|c| c.text.is_empty()), "{}: cells after the cluster are blank", case.name);
    }
}

#[test]
fn a_wide_character_at_the_last_column_wraps_and_leaves_a_spacer_head() {
    let mut e = engine(20, 3);
    e.feed("a".repeat(19).as_bytes());
    e.feed("\u{65E5}b".as_bytes());

    let screen = e.screen().expect("screen");
    assert_eq!(screen.cells[19].width, S, "the unused last column of the first row is a spacer head");
    assert_eq!(screen.cells[20].text, "\u{65E5}");
    assert_eq!(screen.cells[20].width, W);
    assert_eq!(screen.cells[22].text, "b");

    let text = screen.to_text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "a".repeat(19), "the spacer head does not show");
    assert_eq!(lines[1], "\u{65E5}b");
    let cursor = e.update().expect("update").cursor.expect("cursor");
    assert_eq!((cursor.x, cursor.y), (3, 1));

    // Selecting the whole line joins the soft wrap: the text reads back as it was written.
    e.select(SelectOp::All).expect("select all");
    let copied = e.selection_text().expect("text").expect("something is selected");
    assert_eq!(copied, format!("{}\u{65E5}b", "a".repeat(19)));
}

#[test]
fn the_wire_row_carries_one_code_point_per_cell_and_the_tail_in_extras() {
    for case in CASES {
        let mut e = engine(20, 3);
        let mut enc = FrameEncoder::new(1);
        enc.encode(e.update().expect("update")).expect("first frame");
        e.feed(case.text.as_bytes());
        let frame = enc.encode(e.update().expect("update")).expect("a frame for the text");
        let row = frame.changed.iter().find(|r| r.y == 0).expect("row 0 changed");

        // One code point per cell, and no row is wider than the grid. A run of default-style cells keeps its padding
        // spaces (trimming is by whole run), so the written part is what is left after the trailing spaces.
        let covered: usize = row.runs.iter().map(|r| usize::from(r.1)).sum();
        let code_points: usize = row.runs.iter().map(|r| r.0.chars().count()).sum();
        assert_eq!(code_points, covered, "{}: one code point per cell", case.name);
        assert!(covered <= 20, "{}: the row is not wider than the grid", case.name);
        let all: String = row.runs.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(
            all.trim_end_matches(' ').chars().count(),
            usize::from(case.advance),
            "{}: cells written (first code points and spacers)",
            case.name
        );
        // The text comes back whole from runs and extras.
        assert_eq!(rebuild(row).trim_end(), case.text, "{}", case.name);
    }
}
