//! OSC 133 command output on the pinned libghostty-vt revision: which rows `select_output` calls the output of a
//! command, read back through `GhosttyEngine::output_at`.
//!
//! Verdict: PASS
//! Terminators: ST, BEL
//! Unmarked text: None
//! Revision: libghostty-vt d2036baeb605d31d09a9e2ba6d0c923e04c34d54, Zig 0.16.0
//!
//! This file is the guard for a libghostty bump: a newer revision that changes an answer fails or un-ignores a test
//! here instead of silently changing what the AI chat reads.
#![cfg(feature = "ghostty")]

mod common;

use holzwave_core::engine::{SelectMode, SelectOp};
use holzwave_core::{GhosttyEngine, TerminalEngine};

/// OSC string terminator: ESC backslash.
const ST: &str = "\x1b\\";
/// OSC string terminator: BEL.
const BEL: &str = "\x07";
const COLS: u16 = 20;
const ROWS: u16 = 6;
/// Width of the prompt text `$ `.
const PROMPT_WIDTH: usize = 2;

fn grid() -> GhosttyEngine {
    common::engine(COLS, ROWS)
}

/// One OSC 133 mark: `ESC ] 133 ; payload terminator`.
fn mark(payload: &str, term: &str) -> String {
    format!("\x1b]133;{payload}{term}")
}

/// Builds a shell-like byte stream of prompts, inputs and outputs, and tracks the screen row of every line so the
/// tests can name rows. Rows are screen rows: the first line written is row 0 and a row keeps its number when it
/// scrolls into the scrollback (the scrollback limit of 1000 is never reached).
struct Transcript {
    bytes: Vec<u8>,
    next_row: u32,
    term: &'static str,
}

impl Transcript {
    fn new(term: &'static str) -> Self {
        Self { bytes: Vec::new(), next_row: 0, term }
    }

    /// A prompt, the input, the output lines and the end mark `end` (a whole payload such as `D;0`). Returns the
    /// screen rows of the output lines (empty for no output); the input row is `start - 1`.
    fn command(&mut self, input: &str, output: &[&str], end: &str) -> std::ops::Range<u32> {
        debug_assert!(input.len() + PROMPT_WIDTH < usize::from(COLS), "input wraps");
        debug_assert!(output.iter().all(|l| l.chars().count() < usize::from(COLS)), "output line wraps");
        let t = self.term;
        let mut s = String::new();
        s += &mark("A", t);
        s += "$ ";
        s += &mark("B", t);
        s += input;
        s += "\r\n";
        s += &mark("C", t);
        for line in output {
            s += line;
            s += "\r\n";
        }
        s += &mark(end, t);
        self.bytes.extend_from_slice(s.as_bytes());
        let start = self.next_row + 1;
        let end_row = start + output.len() as u32;
        self.next_row = end_row;
        start..end_row
    }

    /// The active prompt, without a line break. Returns its row.
    fn prompt(&mut self) -> u32 {
        let t = self.term;
        let s = format!("{}$ {}", mark("A", t), mark("B", t));
        self.bytes.extend_from_slice(s.as_bytes());
        self.next_row
    }

    fn feed_into(&self, e: &mut GhosttyEngine) {
        e.feed(&self.bytes);
    }
}

/// Assert what `output_at(row, 4096)` returns, naming the row and the observed value.
fn expect(e: &GhosttyEngine, row: u32, want: Option<&str>) {
    let got = e.output_at(row, 4096).unwrap_or_else(|err| panic!("output_at({row}) failed: {err}"));
    assert_eq!(got.as_deref(), want, "output_at({row}): observed {got:?}, expected {want:?}");
}

/// Decides the verdict.
#[test]
fn visible_output_is_selected_exactly() {
    let mut t = Transcript::new(ST);
    let rows = t.command("ls", &["alpha", "beta"], "D;0");
    t.prompt();
    let mut e = grid();
    t.feed_into(&mut e);
    for row in rows {
        expect(&e, row, Some("alpha\nbeta"));
    }
}

/// Decides the verdict.
#[test]
fn scrolled_off_output_is_selected_exactly() {
    let mut t = Transcript::new(ST);
    let mut first = 0..0;
    for k in 1..=4 {
        let lines = [format!("c{k}-a"), format!("c{k}-b"), format!("c{k}-c")];
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let r = t.command(&format!("cmd{k}"), &refs, "D;0");
        if k == 1 {
            first = r;
        }
    }
    t.prompt();
    let mut e = grid();
    t.feed_into(&mut e);
    for row in first.clone() {
        expect(&e, row, Some("c1-a\nc1-b\nc1-c"));
    }
    e.scroll(-3);
    for row in first.clone() {
        expect(&e, row, Some("c1-a\nc1-b\nc1-c"));
    }
    e.scroll_to_bottom();
    for row in first {
        expect(&e, row, Some("c1-a\nc1-b\nc1-c"));
    }
}

/// Decides the verdict.
#[test]
fn unmarked_text_gives_none() {
    let mut e = grid();
    let mut s = String::new();
    for n in 0..30 {
        s += &format!("plain {n:02}\r\n");
    }
    e.feed(s.as_bytes());
    // Row 30 is the cursor row (the line after the last line break).
    for row in [0, 14, 29, 30] {
        expect(&e, row, None);
    }
}

/// Decides the verdict.
#[test]
fn output_taller_than_the_screen_is_selected_whole() {
    for count in [usize::from(ROWS), usize::from(ROWS) + 8] {
        let lines: Vec<String> = (0..count).map(|n| format!("out-{n:02}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let want = lines.join("\n");
        let mut t = Transcript::new(ST);
        let rows = t.command("big", &refs, "D;0");
        t.prompt();
        let mut e = grid();
        t.feed_into(&mut e);
        for row in [rows.start, rows.start + (count / 2) as u32, rows.end - 1] {
            let got = e.output_at(row, 4096).expect("output_at");
            assert_eq!(got.as_deref(), Some(want.as_str()), "{count} lines, output_at({row}): observed {got:?}");
        }
    }
}

/// Decides the verdict.
#[test]
fn neighbouring_commands_do_not_bleed() {
    let mut t = Transcript::new(ST);
    let one = t.command("echo 1", &["one-a", "one-b"], "D;0");
    let two = t.command("false", &["two-a"], "D;1");
    let three = t.command("ls", &["three-a", "three-b", "three-c"], "D");
    t.prompt();
    let mut e = grid();
    t.feed_into(&mut e);
    for row in one {
        expect(&e, row, Some("one-a\none-b"));
    }
    for row in two {
        expect(&e, row, Some("two-a"));
    }
    for row in three {
        expect(&e, row, Some("three-a\nthree-b\nthree-c"));
    }
}

/// Decides the verdict.
#[test]
fn the_row_above_the_active_prompt_is_the_last_output() {
    let mut t = Transcript::new(ST);
    t.command("echo 1", &["one-a", "one-b"], "D;0");
    t.command("false", &["two-a"], "D;1");
    t.command("ls", &["three-a", "three-b", "three-c"], "D");
    let p = t.prompt();
    let mut e = grid();
    t.feed_into(&mut e);
    expect(&e, p - 1, Some("three-a\nthree-b\nthree-c"));
    e.scroll(-2);
    expect(&e, p - 1, Some("three-a\nthree-b\nthree-c"));
}

#[test]
fn prompt_and_input_rows_give_none() {
    let mut t = Transcript::new(ST);
    let a = t.command("echo 1", &["one-a"], "D;0");
    let b = t.command("ls", &["two-a", "two-b"], "D;0");
    let p = t.prompt();
    let mut e = grid();
    t.feed_into(&mut e);
    for row in [a.start - 1, b.start - 1, p] {
        expect(&e, row, None);
    }
}

#[test]
fn both_osc_terminators_mark_the_same_region() {
    for (name, term) in [("ST", ST), ("BEL", BEL)] {
        let mut t = Transcript::new(term);
        let rows = t.command("ls", &["alpha", "beta"], "D;0");
        t.prompt();
        let mut e = grid();
        t.feed_into(&mut e);
        for row in rows {
            let got = e.output_at(row, 4096).expect("output_at");
            assert_eq!(got.as_deref(), Some("alpha\nbeta"), "terminator {name}, output_at({row}): observed {got:?}");
        }
    }
}

#[test]
fn mark_options_and_exit_codes_do_not_move_the_region() {
    for a in ["A", "A;cl=m;aid=7"] {
        for d in ["D", "D;0", "D;130"] {
            let mut s = String::new();
            s += &mark(a, ST);
            s += "$ ";
            s += &mark("B", ST);
            s += "ls\r\n";
            s += &mark("C", ST);
            s += "alpha\r\nbeta\r\n";
            s += &mark(d, ST);
            s += &mark("A", ST);
            s += "$ ";
            s += &mark("B", ST);
            let mut e = grid();
            e.feed(s.as_bytes());
            for row in [1, 2] {
                let got = e.output_at(row, 4096).expect("output_at");
                assert_eq!(got.as_deref(), Some("alpha\nbeta"), "marks {a} / {d}, output_at({row}): observed {got:?}");
            }
        }
    }
}

#[test]
fn a_command_without_output_gives_none() {
    let mut t = Transcript::new(ST);
    let rows = t.command("true", &[], "D;0");
    assert!(rows.is_empty());
    let p = t.prompt();
    let mut e = grid();
    t.feed_into(&mut e);
    // The row above the next prompt is the input row.
    expect(&e, p - 1, None);
}

#[test]
fn output_text_holds_no_control_characters() {
    let mut s = String::new();
    s += &mark("A", ST);
    s += "$ ";
    s += &mark("B", ST);
    s += "x\r\n";
    s += &mark("C", ST);
    s += "\x1b[31mred\x1b[0m\r\n";
    s += "\x1b]0;evil title\x07";
    s += "\x1b]52;c;ZXZpbA==\x07";
    s += "bell\x07\r\n";
    s += "ab\x08c\r\n";
    s += "a\tb\r\n";
    s += "abc\rX\r\n";
    s += &mark("D;0", ST);
    s += &mark("A", ST);
    s += "$ ";
    s += &mark("B", ST);
    let mut e = grid();
    e.feed(s.as_bytes());
    let row = 1;
    let got = e.output_at(row, 4096).expect("output_at").unwrap_or_else(|| panic!("output_at({row}): observed None"));
    assert!(got.contains("red"), "observed {got:?}");
    for c in got.chars() {
        let bad = (c < ' ' && c != '\n' && c != '\t') || c == '\x7f' || ('\u{80}'..='\u{9f}').contains(&c);
        assert!(!bad, "control character {c:?} in {got:?}");
    }
}

#[test]
fn soft_wrapped_output_lines_are_joined() {
    let long = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHI";
    assert_eq!(long.len(), 45);
    let mut s = String::new();
    s += &mark("A", ST);
    s += "$ ";
    s += &mark("B", ST);
    s += "x\r\n";
    s += &mark("C", ST);
    s += long;
    s += "\r\n";
    s += &mark("D;0", ST);
    s += &mark("A", ST);
    s += "$ ";
    s += &mark("B", ST);
    let mut e = grid();
    e.feed(s.as_bytes());
    // The long line occupies rows 1 to 3 of a 20-column grid.
    expect(&e, 2, Some(long));
}

#[test]
fn output_at_installs_no_selection_and_keeps_the_users() {
    let mut t = Transcript::new(ST);
    let rows = t.command("ls", &["alpha", "beta"], "D;0");
    t.prompt();
    let mut e = grid();
    t.feed_into(&mut e);
    assert_eq!(e.selection_text().expect("selection_text"), None);
    expect(&e, rows.start, Some("alpha\nbeta"));
    assert_eq!(e.selection_text().expect("selection_text"), None, "output_at installed a selection");

    e.select(SelectOp::Begin { x: 0, y: 0, mode: SelectMode::Line }).expect("select");
    let before = e.selection_text().expect("selection_text");
    assert!(before.is_some(), "the user selection must have text");
    expect(&e, rows.start, Some("alpha\nbeta"));
    assert_eq!(e.selection_text().expect("selection_text"), before, "output_at changed the user's selection");
}

#[test]
fn output_is_bounded_and_keeps_the_tail() {
    let lines: Vec<String> = (0..60).map(|n| format!("line-é-{n:02}")).collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut t = Transcript::new(ST);
    let rows = t.command("many", &refs, "D;0");
    t.prompt();
    let mut e = grid();
    t.feed_into(&mut e);
    let got = e.output_at(rows.start, 64).expect("output_at").expect("some output");
    assert!(got.len() <= 64, "{} bytes", got.len());
    assert!(got.ends_with("line-é-59"), "observed {got:?}");
    assert!(!got.contains('\u{FFFD}'), "observed {got:?}");
}

#[test]
fn rows_past_the_end_and_a_zero_bound_give_none() {
    let mut t = Transcript::new(ST);
    t.command("ls", &["alpha", "beta"], "D;0");
    t.prompt();
    let mut e = grid();
    t.feed_into(&mut e);
    assert_eq!(e.output_at(1, 0).expect("zero bound"), None);
    assert_eq!(e.output_at(10_000, 4096).expect("past the end"), None);
    assert_eq!(e.output_at(u32::MAX, 4096).expect("u32::MAX"), None);
    expect(&e, 1, Some("alpha\nbeta"));
}
