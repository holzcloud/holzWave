//! What the engine reports as changed between two calls of `update` (DESK-11).
//!
//! libghostty marks rows dirty; `update` turns that into rows plus a header (cursor, scrollbar, colours, modes) that
//! is correct even when no row changed. Every expectation was measured on the pinned libghostty revision (RESEARCH
//! Pitfall 7).
#![cfg(feature = "ghostty")]

mod common;

use common::{Xorshift64, engine};
use holzwave_core::TerminalEngine;
use holzwave_core::engine::{CursorStyle, Update, Width};
use holzwave_core::wire::{CursorShape, FrameEncoder, FrameMsg, ModesMsg};

fn update(e: &mut impl TerminalEngine) -> Update {
    e.update().expect("update")
}

/// The text of one row of an update, cells joined, blanks kept.
fn row_text(u: &Update, y: u16) -> String {
    let row = u.changed.iter().find(|r| r.y == y).unwrap_or_else(|| panic!("row {y} is not in the update"));
    row.cells.iter().map(|c| if c.text.is_empty() { " " } else { c.text.as_str() }).collect()
}

#[test]
fn the_first_update_is_full_and_reset_and_has_every_row() {
    let mut e = engine(20, 5);
    let u = update(&mut e);
    assert!(u.full, "first update must be full");
    assert!(u.reset, "first update must reset the consumer");
    assert_eq!((u.cols, u.rows), (20, 5));
    let ys: Vec<u16> = u.changed.iter().map(|r| r.y).collect();
    assert_eq!(ys, vec![0, 1, 2, 3, 4]);
    assert!(u.changed.iter().all(|r| r.cells.len() == 20));
}

#[test]
fn typing_changes_exactly_the_cursor_row() {
    let mut e = engine(20, 5);
    update(&mut e);
    e.feed(b"hi");
    let u = update(&mut e);
    assert!(!u.full, "typing is a partial update");
    assert!(!u.reset);
    assert_eq!(u.changed.len(), 1, "changed rows: {:?}", u.changed.iter().map(|r| r.y).collect::<Vec<_>>());
    assert_eq!(u.changed[0].y, 0);
    assert!(row_text(&u, 0).starts_with("hi"), "row 0 is {:?}", row_text(&u, 0));
    let cursor = u.cursor.expect("cursor");
    assert_eq!((cursor.x, cursor.y), (2, 0));
}

#[test]
fn an_update_with_nothing_fed_has_no_rows_and_the_same_header() {
    let mut e = engine(20, 5);
    update(&mut e);
    e.feed(b"hi");
    let before = update(&mut e);
    let after = update(&mut e);
    assert!(after.changed.is_empty(), "rows: {:?}", after.changed.iter().map(|r| r.y).collect::<Vec<_>>());
    assert!(!after.full);
    assert!(!after.reset);
    assert_eq!(after.cursor, before.cursor);
    assert_eq!(after.scrollbar, before.scrollbar);
    assert_eq!((after.cols, after.rows), (before.cols, before.rows));
    assert_eq!((after.default_fg, after.default_bg), (before.default_fg, before.default_bg));
    assert_eq!(after.modes, before.modes);
}

#[test]
fn hiding_and_showing_the_cursor_and_changing_its_shape_change_the_header_not_the_rows() {
    let mut e = engine(20, 5);
    update(&mut e);
    e.feed(b"hi");
    let base = update(&mut e);
    assert!(base.cursor.expect("cursor").visible);

    e.feed(b"\x1b[?25l");
    let hidden = update(&mut e);
    assert!(hidden.changed.is_empty(), "libghostty reports Clean, so no rows");
    assert!(!hidden.cursor.expect("still in the viewport").visible);

    e.feed(b"\x1b[?25h\x1b[5 q");
    let bar = update(&mut e);
    assert!(bar.changed.is_empty());
    let cursor = bar.cursor.expect("cursor");
    assert!(cursor.visible);
    assert_eq!(cursor.style, CursorStyle::Bar);
    assert!(cursor.blinking, "DECSCUSR 5 is a blinking bar");
}

#[test]
fn a_resize_resets_the_consumer_even_when_the_terminal_is_quiet() {
    let mut e = engine(20, 5);
    update(&mut e);
    e.resize(30, 8, (0, 0)).expect("resize");
    let u = update(&mut e);
    assert!(u.reset && u.full);
    assert_eq!((u.cols, u.rows), (30, 8));
    assert_eq!(u.changed.len(), 8);
    assert!(u.changed.iter().all(|r| r.cells.len() == 30));

    let quiet = update(&mut e);
    assert!(!quiet.reset && !quiet.full && quiet.changed.is_empty());
}

#[test]
fn request_full_makes_the_next_update_a_reset_with_every_row() {
    let mut e = engine(20, 5);
    update(&mut e);
    e.feed(b"hi");
    update(&mut e);
    e.request_full();
    let u = update(&mut e);
    assert!(u.full && u.reset);
    assert_eq!(u.changed.len(), 5);
    assert!(row_text(&u, 0).starts_with("hi"));
    let after = update(&mut e);
    assert!(!after.full && !after.reset);
}

#[test]
fn output_that_scrolls_is_full_but_not_a_reset() {
    let mut e = engine(20, 5);
    update(&mut e);
    for i in 0..8 {
        e.feed(format!("line {i}\r\n").as_bytes());
    }
    let u = update(&mut e);
    assert!(u.full, "libghostty reports Full for output that scrolls");
    assert!(!u.reset, "the consumer does not have to start over for a scroll");
    assert_eq!(u.changed.len(), 5);
}

#[test]
fn the_alternate_screen_is_a_mode_and_a_full_update() {
    let mut e = engine(20, 5);
    update(&mut e);
    e.feed(b"hi");
    update(&mut e);

    e.feed(b"\x1b[?1049h");
    let entered = update(&mut e);
    assert!(entered.modes.alt_screen);
    assert!(entered.full);
    assert!(!entered.reset);

    e.feed(b"\x1b[?1049l");
    let left = update(&mut e);
    assert!(!left.modes.alt_screen);
    assert!(left.full);
    assert!(row_text(&left, 0).starts_with("hi"), "the primary screen is back");
}

#[test]
fn bracketed_paste_and_mouse_tracking_are_in_the_modes() {
    let mut e = engine(20, 5);
    assert!(!update(&mut e).modes.bracketed_paste);
    e.feed(b"\x1b[?2004h");
    assert!(update(&mut e).modes.bracketed_paste);
    e.feed(b"\x1b[?1000h");
    let u = update(&mut e);
    assert!(u.modes.mouse_tracking);
    assert!(u.modes.bracketed_paste);
    e.feed(b"\x1b[?1000l\x1b[?2004l");
    let off = update(&mut e);
    assert!(!off.modes.mouse_tracking && !off.modes.bracketed_paste);
}

// ---- the mirror: what a webview holds after applying frames in order ------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
struct MCell {
    text: String,
    fg: u32,
    bg: u32,
    flags: u8,
}

/// A consumer that knows nothing but frames. It checks sequence continuity and that only a full frame may change the
/// grid size.
struct Mirror {
    seq: u64,
    cols: u16,
    rows: u16,
    fg: u32,
    bg: u32,
    grid: Vec<Vec<MCell>>,
    cursor: Option<(u16, u16, CursorShape, bool)>,
    scrollbar: (u64, u64, u64),
    modes: Option<ModesMsg>,
}

impl Mirror {
    fn new() -> Self {
        Self { seq: 0, cols: 0, rows: 0, fg: 0, bg: 0, grid: Vec::new(), cursor: None, scrollbar: (0, 0, 0), modes: None }
    }

    fn blank(&self) -> MCell {
        MCell { text: " ".into(), fg: self.fg, bg: self.bg, flags: 0 }
    }

    fn apply(&mut self, f: &FrameMsg) {
        assert_eq!(f.seq, self.seq + 1, "sequence numbers go up by exactly one");
        self.seq = f.seq;
        let resized = (f.cols, f.rows) != (self.cols, self.rows);
        assert!(f.full || !resized, "a size change must arrive as a full frame");
        self.fg = f.fg;
        self.bg = f.bg;
        if f.full {
            self.cols = f.cols;
            self.rows = f.rows;
            let blank = self.blank();
            self.grid = vec![vec![blank; usize::from(f.cols)]; usize::from(f.rows)];
        }
        if f.full {
            let ys: Vec<u16> = f.changed.iter().map(|r| r.y).collect();
            assert_eq!(ys, (0..f.rows).collect::<Vec<_>>(), "a full frame carries every row, in order");
        }
        for row in &f.changed {
            assert!(row.y < self.rows, "row {} outside the grid", row.y);
            let mut cells = Vec::with_capacity(usize::from(self.cols));
            for run in &row.runs {
                let points: Vec<char> = run.0.chars().collect();
                assert_eq!(points.len(), usize::from(run.1), "one code point per cell in run {run:?}");
                for c in points {
                    cells.push(MCell { text: c.to_string(), fg: run.2, bg: run.3, flags: run.4 });
                }
            }
            assert!(cells.len() <= usize::from(self.cols), "row {} is wider than the grid", row.y);
            let blank = self.blank();
            cells.resize(usize::from(self.cols), blank);
            for (x, tail) in &row.extras {
                cells[usize::from(*x)].text.push_str(tail);
            }
            self.grid[usize::from(row.y)] = cells;
        }
        self.cursor = f.cursor.map(|c| (c.x, c.y, c.style, c.blink));
        self.scrollbar = f.scrollbar;
        self.modes = Some(f.modes);
    }
}

/// The engine's screen as the mirror should see it. A blank cell in the default background and without flags may
/// have been trimmed from the row end, and then its foreground is the default one: it is not compared.
fn assert_mirror_matches(e: &mut impl TerminalEngine, m: &Mirror, alt: bool, context: &str) {
    let s = e.screen().expect("screen");
    assert_eq!((m.cols, m.rows), (s.cols, s.rows), "{context}: size");
    assert_eq!(m.fg, rgb(s.default_fg), "{context}: default fg");
    assert_eq!(m.bg, rgb(s.default_bg), "{context}: default bg");
    let default_bg = rgb(s.default_bg);
    for (y, row) in s.cells.chunks(usize::from(s.cols)).enumerate() {
        for (x, cell) in row.iter().enumerate() {
            let want_text = match cell.width {
                Width::Spacer => "\0".to_string(),
                _ if cell.text.is_empty() => " ".to_string(),
                _ => cell.text.clone(),
            };
            let flags = u8::from(cell.bold) | u8::from(cell.italic) << 1 | u8::from(cell.underline) << 2 | u8::from(cell.inverse) << 3;
            let got = &m.grid[y][x];
            assert_eq!(got.text, want_text, "{context}: text at ({x},{y})");
            assert_eq!(got.bg, rgb(cell.bg), "{context}: bg at ({x},{y})");
            assert_eq!(got.flags, flags, "{context}: flags at ({x},{y})");
            let trimmable = want_text == " " && rgb(cell.bg) == default_bg && flags == 0;
            if !trimmable {
                assert_eq!(got.fg, rgb(cell.fg), "{context}: fg at ({x},{y})");
            }
        }
    }
    let want_cursor = s.cursor.filter(|c| c.visible).map(|c| {
        let shape = match c.style {
            CursorStyle::Block => CursorShape::Block,
            CursorStyle::Bar => CursorShape::Bar,
            CursorStyle::Underline => CursorShape::Underline,
            CursorStyle::Hollow => CursorShape::Hollow,
        };
        (c.x, c.y, shape, c.blinking)
    });
    assert_eq!(m.cursor, want_cursor, "{context}: cursor");
    assert_eq!(m.scrollbar, (s.scrollbar.offset, s.scrollbar.total, s.scrollbar.len), "{context}: scrollbar");
    assert_eq!(m.modes.map(|x| x.alt), Some(alt), "{context}: alternate screen");
}

fn rgb(c: holzwave_core::Color) -> u32 {
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

/// One random operation: the bytes to feed, or a resize, or a viewport scroll.
enum Op {
    Feed(String),
    Resize(u16, u16),
    Scroll(isize),
    ScrollBottom,
}

fn random_op(rng: &mut Xorshift64) -> Op {
    const WORDS: [&str; 8] = ["hello", "world", "a", "xyz", "0123456789", "ab cd", "~!@#", "wrap this line around"];
    const CLUSTERS: [&str; 8] = [
        "日本",
        "🙂",
        "e\u{0301}",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
        "🇩🇪",
        "ä",
        "o\u{0308}o",
        "\u{4E2D}x",
    ];
    let n = |rng: &mut Xorshift64, hi: usize| rng.below(hi) + 1;
    let s = match rng.below(26) {
        0..=3 => WORDS[rng.below(WORDS.len())].to_string(),
        4 => "\r\n".to_string(),
        5 => "\r".to_string(),
        6 => "\t".to_string(),
        7 => {
            const SGR: [&str; 12] = ["0", "1", "3", "4", "7", "22", "27", "31", "42", "93", "38;5;200", "48;2;10;200;30"];
            format!("\x1b[{}m", SGR[rng.below(SGR.len())])
        }
        8 => format!("\x1b[{};{}H", n(rng, 14), n(rng, 45)),
        9 => format!("\x1b[{}{}", n(rng, 6), ["A", "B", "C", "D"][rng.below(4)]),
        10 => format!("\x1b[{}K", rng.below(3)),
        11 => format!("\x1b[{}J", rng.below(4)),
        12 => format!("\x1b[{}L", n(rng, 3)),
        13 => format!("\x1b[{}M", n(rng, 3)),
        14 => format!("\x1b[{}S", n(rng, 4)),
        15 => format!("\x1b[{}T", n(rng, 4)),
        16..=18 => CLUSTERS[rng.below(CLUSTERS.len())].to_string(),
        19 => ["\x1b[?1049h", "\x1b[?1049l"][rng.below(2)].to_string(),
        20 => format!("\x1b[{} q", rng.below(7)),
        21 => ["\x1b[?25l", "\x1b[?25h"][rng.below(2)].to_string(),
        22 => format!("\x1b[{}@", n(rng, 3)),
        23 => format!("\x1b[{}P", n(rng, 3)),
        24 => return if rng.chance(70) { Op::Resize(rng.below(36) as u16 + 5, rng.below(11) as u16 + 2) } else { Op::ScrollBottom },
        _ => return Op::Scroll(rng.below(11) as isize - 5),
    };
    Op::Feed(s)
}

#[test]
fn a_mirror_built_only_from_frames_equals_the_screen_after_every_operation() {
    for seed in 1..=20u64 {
        let mut rng = Xorshift64::new(seed * 0x9E37_79B9 + 17);
        let mut e = engine(20, 5);
        let mut enc = FrameEncoder::new(seed);
        let mut mirror = Mirror::new();
        let mut alt = false;

        let first = enc.encode(update(&mut e)).expect("the first frame");
        assert!(first.full);
        mirror.apply(&first);
        assert_mirror_matches(&mut e, &mirror, alt, &format!("seed {seed} start"));

        for step in 0..400 {
            let op = random_op(&mut rng);
            let (label, resized) = match &op {
                Op::Feed(s) => {
                    if s == "\x1b[?1049h" {
                        alt = true;
                    } else if s == "\x1b[?1049l" {
                        alt = false;
                    }
                    e.feed(s.as_bytes());
                    (format!("feed {s:?}"), false)
                }
                Op::Resize(c, r) => {
                    e.resize(*c, *r, (0, 0)).expect("resize");
                    (format!("resize {c}x{r}"), true)
                }
                Op::Scroll(d) => {
                    e.scroll(*d);
                    (format!("scroll {d}"), false)
                }
                Op::ScrollBottom => {
                    e.scroll_to_bottom();
                    ("scroll_to_bottom".to_string(), false)
                }
            };
            let context = format!("seed {seed} step {step} after {label}");
            if let Some(frame) = enc.encode(update(&mut e)) {
                assert!(!frame.full || resized, "{context}: full frame without a reason");
                mirror.apply(&frame);
            }
            assert_mirror_matches(&mut e, &mirror, alt, &context);
        }
    }
}
