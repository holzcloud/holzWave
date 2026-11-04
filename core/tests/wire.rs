//! The frame protocol: engine updates become sequenced JSON frames with only the rows that changed (DESK-11, D-09,
//! D-11).
#![cfg(feature = "ghostty")]

mod common;

use common::engine;
use holzwave_core::TerminalEngine;
use holzwave_core::engine::{SearchOp, SelectMode, SelectOp};
use holzwave_core::wire::{FrameEncoder, FrameMsg};

/// Run the engine's update through the encoder.
fn next_frame(e: &mut impl TerminalEngine, enc: &mut FrameEncoder) -> Option<FrameMsg> {
    enc.encode(e.update().expect("update"))
}

#[test]
fn typing_one_character_is_one_changed_row_with_a_sequence_number() {
    let mut e = engine(20, 5);
    let mut enc = FrameEncoder::new(7);

    let first = next_frame(&mut e, &mut enc).expect("the first frame is always emitted");
    assert!(first.full);
    assert_eq!(first.seq, 1);
    assert_eq!(first.pane, 7);
    assert_eq!((first.cols, first.rows), (20, 5));

    e.feed(b"hi");
    let typed = next_frame(&mut e, &mut enc).expect("typing changes a row");
    assert!(!typed.full);
    assert_eq!(typed.seq, 2);
    assert_eq!(typed.changed.len(), 1);
    let row = &typed.changed[0];
    assert_eq!(row.y, 0);
    assert_eq!(row.runs.len(), 1, "runs: {:?}", row.runs);
    assert!(row.runs[0].0.starts_with("hi"), "run text {:?}", row.runs[0].0);
    assert!(row.extras.is_empty());
}

#[test]
fn an_unchanged_update_encodes_to_nothing() {
    let mut e = engine(20, 5);
    let mut enc = FrameEncoder::new(1);
    next_frame(&mut e, &mut enc).expect("first");
    e.feed(b"hi");
    next_frame(&mut e, &mut enc).expect("typed");
    assert!(next_frame(&mut e, &mut enc).is_none(), "nothing changed, nothing is sent");
}

#[test]
fn the_json_frame_has_exactly_the_documented_keys() {
    let mut e = engine(20, 5);
    let mut enc = FrameEncoder::new(3);
    next_frame(&mut e, &mut enc).expect("first");
    e.feed(b"hi");
    let typed = next_frame(&mut e, &mut enc).expect("typed");

    let json = serde_json::to_value(&typed).expect("json");
    let mut keys: Vec<&str> = json.as_object().expect("object").keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected = vec![
        "pane", "seq", "full", "cols", "rows", "fg", "bg", "cursor", "scrollbar", "selection", "matches", "search",
        "copy_cursor", "modes", "changed",
    ];
    expected.sort_unstable();
    assert_eq!(keys, expected);

    assert!(json["search"].is_null());
    assert!(json["copy_cursor"].is_null());
    let cursor = &json["cursor"];
    assert_eq!(cursor["x"], 2);
    assert_eq!(cursor["y"], 0);
    assert_eq!(cursor["style"], "block");
    assert!(cursor["blink"].is_boolean());
    assert_eq!(json["scrollbar"].as_array().expect("scrollbar").len(), 3);
    let modes = json["modes"].as_object().expect("modes");
    let mut mode_keys: Vec<&str> = modes.keys().map(String::as_str).collect();
    mode_keys.sort_unstable();
    assert_eq!(mode_keys, vec!["alt", "bracketed_paste", "mouse"]);

    let row = &json["changed"][0];
    assert_eq!(row["y"], 0);
    assert!(row["runs"].is_array());
    assert!(row.get("extras").is_none(), "extras is omitted when empty");
    // A run is `[text, cols, fg, bg, flags]`.
    let run = row["runs"][0].as_array().expect("run");
    assert_eq!(run.len(), 5);
    assert!(run[0].as_str().expect("text").starts_with("hi"));
}

// ---- Task 2: header-only frames, resets, scroll diffing ------------------------------------------------------------

use holzwave_core::wire::RowMsg;

/// Feed `bytes` and encode the update that follows.
fn feed_frame(e: &mut impl TerminalEngine, enc: &mut FrameEncoder, bytes: &[u8]) -> Option<FrameMsg> {
    e.feed(bytes);
    next_frame(e, enc)
}

/// Engine and encoder past their first frame, with `hi` typed.
fn typed() -> (holzwave_core::GhosttyEngine, FrameEncoder) {
    let mut e = engine(20, 5);
    let mut enc = FrameEncoder::new(1);
    next_frame(&mut e, &mut enc).expect("first");
    feed_frame(&mut e, &mut enc, b"hi").expect("typed");
    (e, enc)
}

#[test]
fn cursor_visibility_and_shape_are_header_only_frames() {
    let (mut e, mut enc) = typed();
    let seq = enc.encode(e.update().unwrap()); // nothing changed since: no frame
    assert!(seq.is_none());

    let hidden = feed_frame(&mut e, &mut enc, b"\x1b[?25l").expect("hiding the cursor is a frame");
    assert!(hidden.changed.is_empty());
    assert!(!hidden.full);
    assert!(hidden.cursor.is_none(), "a hidden cursor is not on the wire");

    let shown = feed_frame(&mut e, &mut enc, b"\x1b[?25h").expect("showing it is a frame");
    assert!(shown.changed.is_empty() && !shown.full);
    assert!(shown.cursor.is_some());
    assert_eq!(shown.seq, hidden.seq + 1);

    let bar = feed_frame(&mut e, &mut enc, b"\x1b[6 q").expect("a new shape is a frame");
    assert!(bar.changed.is_empty() && !bar.full);
    let json = serde_json::to_value(&bar).unwrap();
    assert_eq!(json["cursor"]["style"], "bar");
    assert_eq!(json["cursor"]["blink"], false);
}

#[test]
fn a_resize_is_a_full_frame_with_the_new_size_and_every_row() {
    let (mut e, mut enc) = typed();
    e.resize(30, 8, (0, 0)).unwrap();
    let f = next_frame(&mut e, &mut enc).expect("resize frame");
    assert!(f.full);
    assert_eq!((f.cols, f.rows), (30, 8));
    assert_eq!(f.changed.len(), 8);
    assert!(f.changed.iter().enumerate().all(|(i, r)| usize::from(r.y) == i));

    // Resizing to the same size is nothing at all.
    e.resize(30, 8, (0, 0)).unwrap();
    assert!(next_frame(&mut e, &mut enc).is_none());
}

#[test]
fn request_full_on_the_engine_and_on_the_encoder_both_resend_every_row() {
    let (mut e, mut enc) = typed();
    e.request_full();
    let f = next_frame(&mut e, &mut enc).expect("engine request");
    assert!(f.full);
    assert_eq!(f.changed.len(), 5);
    assert!(f.changed[0].runs[0].0.starts_with("hi"));

    // The encoder alone can resend: it keeps the rows it last sent, so a webview that reconnects needs no engine.
    enc.request_full();
    let f = next_frame(&mut e, &mut enc).expect("encoder request");
    assert!(f.full);
    assert_eq!(f.changed.len(), 5);
    assert!(f.changed[0].runs[0].0.starts_with("hi"));

    // Only once: the request is consumed.
    let after = feed_frame(&mut e, &mut enc, b"!").expect("typed");
    assert!(!after.full);
    assert_eq!(after.changed.len(), 1);
}

#[test]
fn scrolling_output_sends_only_the_rows_that_differ_from_the_last_frame() {
    let mut e = engine(20, 5);
    let mut enc = FrameEncoder::new(1);
    next_frame(&mut e, &mut enc).expect("first");
    // The screen as the webview has it, one encoded row per y.
    let mut held: Vec<RowMsg> = (0..5u16).map(|y| RowMsg { y, runs: vec![], extras: vec![], links: vec![] }).collect();

    for i in 0..60 {
        let Some(f) = feed_frame(&mut e, &mut enc, format!("line {i}\r\n").as_bytes()) else { continue };
        assert!(!f.full, "line {i}: a scroll is not a reason for a full frame");
        for row in &f.changed {
            assert_ne!(&held[usize::from(row.y)], row, "line {i}: row {} was sent unchanged", row.y);
            held[usize::from(row.y)] = row.clone();
        }
    }
    // Four lines on the screen end up shown, one per row, the last row is where the cursor waits.
    assert!(held[3].runs[0].0.starts_with("line 59"));
}

#[test]
fn identical_rows_after_a_scroll_are_not_sent_again() {
    let mut e = engine(20, 5);
    let mut enc = FrameEncoder::new(1);
    next_frame(&mut e, &mut enc).expect("first");
    // Fill the screen with the same text; from then on every scroll leaves the rows as they are.
    let mut last = None;
    for _ in 0..12 {
        last = feed_frame(&mut e, &mut enc, b"same\r\n");
    }
    let f = last.expect("the scrollbar total still grows, so there is a frame");
    assert!(f.changed.is_empty(), "rows re-sent: {:?}", f.changed.iter().map(|r| r.y).collect::<Vec<_>>());
    assert!(!f.full);
}

#[test]
fn the_alternate_screen_sets_the_mode_and_swaps_the_rows() {
    let (mut e, mut enc) = typed();
    let entered = feed_frame(&mut e, &mut enc, b"\x1b[?1049h").expect("enter");
    assert!(entered.modes.alt);
    assert!(!entered.full);
    // The row that held `hi` is blank on the alternate screen and the mirror needs to hear that.
    assert!(entered.changed.iter().any(|r| r.y == 0 && r.runs.is_empty()), "rows: {:?}", entered.changed);

    let left = feed_frame(&mut e, &mut enc, b"\x1b[?1049l").expect("leave");
    assert!(!left.modes.alt);
    assert!(left.changed.iter().any(|r| r.y == 0 && r.runs[0].0.starts_with("hi")));
}

#[test]
fn sequence_numbers_start_at_one_and_skip_nothing() {
    let mut e = engine(20, 5);
    let mut enc = FrameEncoder::new(1);
    let mut seqs = Vec::new();
    let mut none = 0;
    for chunk in [&b"a"[..], b"", b"b", b"", b"\x1b[?25l", b"", b"\r\n"] {
        match feed_frame(&mut e, &mut enc, chunk) {
            Some(f) => seqs.push(f.seq),
            None => none += 1,
        }
    }
    assert!(none >= 3, "empty updates are not emitted");
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
}

// ---- Task 3: one code point per cell, tails in extras ------------------------------------------------------------

use holzwave_core::wire::Run;

/// Turn grapheme clustering on (DECSET 2027) the way a program that wants clusters does. The engine starts with it
/// on, so these tests are explicit about the state they mean.
const CLUSTERING: &[u8] = b"\x1b[?2027h";

/// Turn grapheme clustering off (RM 2027), the way a program that does not want clusters does.
const NO_CLUSTERING: &[u8] = b"\x1b[?2027l";

/// Feed `bytes` to a fresh 20x5 engine with clustering on and return the first row of the resulting frame.
fn first_row_of(bytes: &str) -> RowMsg {
    let mut e = engine(20, 5);
    let mut enc = FrameEncoder::new(1);
    next_frame(&mut e, &mut enc).expect("first");
    e.feed(CLUSTERING);
    e.feed(bytes.as_bytes());
    let f = next_frame(&mut e, &mut enc).expect("frame");
    f.changed.into_iter().find(|r| r.y == 0).expect("row 0")
}

/// The text of all runs of a row.
fn run_text(row: &RowMsg) -> String {
    row.runs.iter().map(|r| r.0.as_str()).collect()
}

/// The invariant of the protocol: a run's text has exactly one code point per cell it covers, and a row never covers
/// more cells than the grid is wide.
fn assert_one_code_point_per_cell(row: &RowMsg, cols: u16) {
    let mut covered = 0usize;
    for Run(text, cells, ..) in &row.runs {
        assert_eq!(text.chars().count(), usize::from(*cells), "row {}: run {text:?} covers {cells} cells", row.y);
        covered += usize::from(*cells);
    }
    assert!(covered <= usize::from(cols), "row {}: {covered} cells in a grid {cols} wide", row.y);
    for (x, tail) in &row.extras {
        assert!(!tail.is_empty(), "row {}: empty tail at {x}", row.y);
        assert!(usize::from(*x) < covered, "row {}: tail at {x} outside the runs", row.y);
        let at = run_text(row).chars().nth(usize::from(*x)).expect("cell");
        assert_ne!(at, '\0', "row {}: a tail sits on a spacer at {x}", row.y);
    }
}

#[test]
fn a_combining_mark_is_the_tail_of_its_cell() {
    let row = first_row_of("e\u{0301}x");
    assert!(run_text(&row).starts_with("ex"), "text {:?}", run_text(&row));
    assert_eq!(row.extras, vec![(0, "\u{0301}".to_string())]);
    assert_one_code_point_per_cell(&row, 20);
}

#[test]
fn a_zwj_family_is_one_wide_cell_with_the_rest_as_its_tail() {
    let row = first_row_of("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}z");
    // The first code point, a spacer for the second column, then the next cell.
    assert!(run_text(&row).starts_with("\u{1F468}\0z"), "text {:?}", run_text(&row));
    assert_eq!(row.extras, vec![(0, "\u{200D}\u{1F469}\u{200D}\u{1F467}".to_string())]);
    assert_one_code_point_per_cell(&row, 20);
}

#[test]
fn a_flag_and_wide_cjk_keep_one_code_point_per_cell() {
    let flag = first_row_of("\u{1F1E9}\u{1F1EA}q");
    assert!(run_text(&flag).starts_with("\u{1F1E9}\0q"), "text {:?}", run_text(&flag));
    assert_eq!(flag.extras, vec![(0, "\u{1F1EA}".to_string())]);
    assert_one_code_point_per_cell(&flag, 20);

    let cjk = first_row_of("日本y");
    assert!(run_text(&cjk).starts_with("日\0本\0y"), "text {:?}", run_text(&cjk));
    assert!(cjk.extras.is_empty());
    assert_one_code_point_per_cell(&cjk, 20);
}

#[test]
fn a_plain_ascii_cell_has_no_extras() {
    let row = first_row_of("plain text");
    assert!(row.extras.is_empty());
    assert!(run_text(&row).starts_with("plain text"));
    assert_one_code_point_per_cell(&row, 20);
}

#[test]
fn an_empty_cell_inside_styled_text_is_a_single_space() {
    // Red background on a blank, then text: the blank is part of the row's runs as a space.
    let row = first_row_of("\x1b[41m \x1b[0mx");
    assert!(run_text(&row).starts_with(" x"), "text {:?}", run_text(&row));
    assert_eq!(row.runs[0].0, " ");
    assert_eq!(row.runs[0].1, 1);
    assert_one_code_point_per_cell(&row, 20);
}

#[test]
fn the_cell_count_invariant_holds_with_clustering_off_and_on() {
    let text = "e\u{0301}\u{1F468}\u{200D}\u{1F469}\u{1F1E9}\u{1F1EA}日本o\u{0308}\u{0323}\u{1F44D}\u{1F3FD}x";
    for clustering in [false, true] {
        let mut e = engine(40, 5);
        let mut enc = FrameEncoder::new(1);
        next_frame(&mut e, &mut enc).expect("first");
        e.feed(if clustering { CLUSTERING } else { NO_CLUSTERING });
        e.feed(text.as_bytes());
        let f = next_frame(&mut e, &mut enc).expect("frame");
        for row in &f.changed {
            assert_one_code_point_per_cell(row, 40);
        }
    }
}

#[test]
fn a_search_and_a_move_between_matches_are_header_only_frames() {
    let mut e = engine(30, 5);
    let mut enc = FrameEncoder::new(1);
    e.feed(b"hello one\r\nnothing\r\nhello two\r\nx\r\nlast");
    next_frame(&mut e, &mut enc).expect("first");

    e.search(SearchOp::Set("hello".into())).expect("set");
    let set = next_frame(&mut e, &mut enc).expect("a new search changes the header");
    assert!(!set.full && set.changed.is_empty(), "no row changed: {:?}", set.changed.len());
    assert_eq!(set.matches, vec![(0, 0, 4, false), (2, 0, 4, true)]);
    assert_eq!(set.search, Some(holzwave_core::wire::SearchMsg { total: 2, current: Some(1) }));

    e.search(SearchOp::Prev).expect("prev");
    let moved = next_frame(&mut e, &mut enc).expect("moving the current match changes the header");
    assert!(!moved.full && moved.changed.is_empty());
    assert_eq!(moved.matches, vec![(0, 0, 4, true), (2, 0, 4, false)]);
    assert_eq!(moved.search, Some(holzwave_core::wire::SearchMsg { total: 2, current: Some(0) }));
    assert!(moved.seq > set.seq);

    e.search(SearchOp::Clear).expect("clear");
    let cleared = next_frame(&mut e, &mut enc).expect("clearing changes the header");
    assert!(cleared.matches.is_empty() && cleared.search.is_none() && cleared.changed.is_empty());
}

// ---- Task 3: the golden fixture shared with the TypeScript patcher test ------------------------------------------

#[derive(serde::Serialize)]
struct Step {
    name: &'static str,
    /// The bytes fed, ASCII-escaped.
    input: String,
    /// The selection operations applied after the bytes, as text; absent when the step makes none.
    #[serde(skip_serializing_if = "Option::is_none")]
    select: Option<String>,
    /// The search operations applied after the bytes and the selection, as text; absent when the step makes none.
    #[serde(skip_serializing_if = "Option::is_none")]
    search: Option<String>,
    /// The new grid size when the step is a resize.
    resize: Option<(u16, u16)>,
    /// `null` when the step produced no frame.
    frame: Option<FrameMsg>,
    /// The expected grid after the step: one plain-text line per row, spacers dropped, tails appended, trailing
    /// blanks trimmed.
    grid: Vec<String>,
}

#[derive(serde::Serialize)]
struct Fixture {
    version: u32,
    cols: u16,
    rows: u16,
    steps: Vec<Step>,
}

fn scenario() -> Fixture {
    let (cols, rows) = (20, 5);
    let mut e = engine(cols, rows);
    // The engine learns the size of a cell through a resize; without it a picture has no place on the screen.
    e.resize(cols, rows, (8, 16)).expect("cell size");
    let mut enc = FrameEncoder::new(1);
    let mut steps = Vec::new();

    let mut step = |name: &'static str,
                    input: &[u8],
                    resize: Option<(u16, u16)>,
                    ops: &[SelectOp],
                    searches: &[SearchOp],
                    e: &mut holzwave_core::GhosttyEngine| {
        e.feed(input);
        if let Some((c, r)) = resize {
            e.resize(c, r, (8, 16)).expect("resize");
        }
        for op in ops {
            e.select(*op).expect("select");
        }
        for op in searches {
            e.search(op.clone()).expect("search");
        }
        let select = (!ops.is_empty()).then(|| format!("{ops:?}"));
        let search = (!searches.is_empty()).then(|| format!("{searches:?}"));
        let frame = enc.encode(e.update().expect("update"));
        let text = e.screen().expect("screen").to_text();
        let grid = text.strip_suffix('\n').unwrap_or(&text).split('\n').map(str::to_string).collect();
        steps.push(Step { name, input: input.escape_ascii().to_string(), select, search, resize, frame, grid });
    };

    step("initial", b"", None, &[], &[], &mut e);
    step("typing", b"hello", None, &[], &[], &mut e);
    step("newline and scroll", b"\r\nworld\r\nthree\r\nfour\r\nfive\r\nsix", None, &[], &[], &mut e);
    step("colours and styles", b"\r\n\x1b[1;31mred\x1b[0m \x1b[4;42mu\x1b[0m \x1b[3;7mit\x1b[0m", None, &[], &[], &mut e);
    step("wide cjk pair", b"\r\n\xe6\x97\xa5\xe6\x9c\xac!", None, &[], &[], &mut e);
    step("emoji with clustering", "\u{1b}[?2027h\r\n\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} ok".as_bytes(), None, &[], &[], &mut e);
    step("combining sequence", "\r\ne\u{0301}o\u{0308}x".as_bytes(), None, &[], &[], &mut e);
    step(
        "select two rows",
        b"",
        None,
        &[
            SelectOp::Begin { x: 0, y: 3, mode: SelectMode::Cell },
            SelectOp::Extend { x: 3, y: 4 },
        ],
        &[],
        &mut e,
    );
    step("selection follows a scroll", b"\r\nmore", None, &[], &[], &mut e);
    step("selection cleared", b"", None, &[SelectOp::Clear], &[], &mut e);
    step("resize to 24x6", b"", Some((24, 6)), &[], &[], &mut e);
    step("alternate screen enter", b"\x1b[?1049h\x1b[HALT", None, &[], &[], &mut e);
    step("alternate screen leave", b"\x1b[?1049l", None, &[], &[], &mut e);
    step("cursor hide", b"\x1b[?25l", None, &[], &[], &mut e);
    step("erase display", b"\x1b[2J", None, &[], &[], &mut e);
    // Search: three lines with the needle on a cleared screen; the needle is set (newest match current), the next
    // step moves one match down with wrap-around, and clearing removes matches and search from the header.
    step(
        "text to search",
        b"\x1b[?25h\x1b[H\x1b[2Jfind me\r\nother\r\nand find again\r\nFIND last",
        None,
        &[],
        &[],
        &mut e,
    );
    step("search set", b"", None, &[], &[SearchOp::Set("find".to_string())], &mut e);
    step("search next wraps to the first", b"", None, &[], &[SearchOp::Next], &mut e);
    step("search clear", b"", None, &[], &[SearchOp::Clear], &mut e);
    // Images: one pixel placed in one cell (a list arrives), text that changes no image (no list), the picture
    // deleted (an empty list).
    step("image placed", b"\x1b[H\x1b[2J\x1b_Ga=T,f=32,s=1,v=1,i=5,c=1,r=1;AAAA/w==\x1b\\", None, &[], &[], &mut e);
    step("text beside the image", b"\r\nafter", None, &[], &[], &mut e);
    step("image deleted", b"\x1b_Ga=d,d=I,i=5\x1b\\", None, &[], &[], &mut e);

    Fixture { version: 1, cols, rows, steps }
}

#[test]
fn fixtures_frames_json_is_the_golden_sequence_for_the_patcher() {
    let fixture = scenario();
    for step in &fixture.steps {
        let Some(frame) = &step.frame else { continue };
        for row in &frame.changed {
            assert_one_code_point_per_cell(row, frame.cols);
        }
    }
    let mut json = serde_json::to_string_pretty(&fixture).expect("json");
    json.push('\n');

    let path = common::fixtures_dir().join("frames.json");
    if common::update_fixtures() {
        std::fs::create_dir_all(path.parent().expect("dir")).expect("create fixtures dir");
        std::fs::write(&path, &json).expect("write frames.json");
        return;
    }
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is missing ({e}); run with UPDATE_FIXTURES=1", path.display()));
    assert!(on_disk == json, "frames.json differs from the regenerated sequence; run with UPDATE_FIXTURES=1 and review");
}

// ---- Images in frames (POL-01, D-03) -------------------------------------------------------------------------------

use holzwave_core::engine::{ImageLayer, ImagePlacement, MAX_PLACEMENTS, Update};

fn placement(id: u32, generation: u64) -> ImagePlacement {
    ImagePlacement {
        image_id: id,
        generation,
        layer: ImageLayer::AboveText,
        z: 0,
        col: 1,
        row: 2,
        cols: 3,
        rows: 4,
        offset_x: 5,
        offset_y: 6,
        width: 24,
        height: 64,
        src_x: 7,
        src_y: 8,
        src_w: 9,
        src_h: 10,
    }
}

/// The update of a 20 by 5 grid with nothing changed and the given list (`None`: the engine reports no change).
fn update_with(images: Option<Vec<ImagePlacement>>) -> Update {
    Update { cols: 20, rows: 5, images, ..Update::default() }
}

#[test]
fn a_frame_without_an_image_change_serialises_to_the_bytes_it_had_before_images_existed() {
    let mut e = engine(20, 5);
    let mut enc = FrameEncoder::new(7);
    next_frame(&mut e, &mut enc).expect("first");
    let typed = feed_frame(&mut e, &mut enc, b"hi").expect("typed");
    assert!(typed.images.is_none());
    let json = serde_json::to_string(&typed).expect("json");
    let expected = concat!(
        r#"{"pane":7,"seq":2,"full":false,"cols":20,"rows":5,"fg":16777215,"bg":0,"#,
        r#""cursor":{"x":2,"y":0,"style":"block","blink":false},"scrollbar":[0,5,5],"selection":[],"matches":[],"#,
        r#""search":null,"copy_cursor":null,"modes":{"alt":false,"bracketed_paste":false,"mouse":false},"#,
        r#""changed":[{"y":0,"runs":[["hi                  ",20,16777215,0,0]]}]}"#,
    );
    assert_eq!(json, expected);
}

#[test]
fn the_first_full_frame_without_images_has_no_images_key_and_one_with_images_has_the_list() {
    let mut enc = FrameEncoder::new(1);
    let first = enc.encode(update_with(Some(Vec::new()))).expect("first");
    assert!(first.full && first.images.is_none(), "an empty list on a full frame is the absence of the key");

    let mut enc = FrameEncoder::new(1);
    let first = enc.encode(update_with(Some(vec![placement(5, 9)]))).expect("first");
    assert!(first.full);
    let list = first.images.as_ref().expect("a full frame with images carries them");
    assert_eq!(list.len(), 1);
}

#[test]
fn the_placement_is_sixteen_numbers_in_the_documented_order() {
    let mut enc = FrameEncoder::new(1);
    let frame = enc.encode(update_with(Some(vec![placement(5, 9)]))).expect("frame");
    let json = serde_json::to_value(&frame).expect("json");
    // id, generation, layer, z, col, row, cols, rows, x offset, y offset, width, height, src x, y, w, h.
    assert_eq!(json["images"], serde_json::json!([[5, 9, 2, 0, 1, 2, 3, 4, 5, 6, 24, 64, 7, 8, 9, 10]]));
    let mut below = placement(1, 1);
    below.layer = ImageLayer::BelowBg;
    below.z = -2_000_000_000;
    let mut mid = placement(2, 1);
    mid.layer = ImageLayer::BelowText;
    mid.z = -1;
    let frame = enc.encode(update_with(Some(vec![below, mid]))).expect("frame");
    let json = serde_json::to_value(&frame).expect("json");
    assert_eq!(json["images"][0][2], 0, "below the backgrounds");
    assert_eq!(json["images"][0][3], -2_000_000_000);
    assert_eq!(json["images"][1][2], 1, "below the text");
}

#[test]
fn a_change_of_the_list_alone_makes_a_frame_and_the_same_list_twice_makes_one() {
    let mut enc = FrameEncoder::new(1);
    enc.encode(update_with(Some(Vec::new()))).expect("first");
    let placed = enc.encode(update_with(Some(vec![placement(5, 1)]))).expect("the list alone is a frame");
    assert!(!placed.full && placed.changed.is_empty());
    assert_eq!(placed.images.as_ref().map(Vec::len), Some(1));
    assert!(enc.encode(update_with(Some(vec![placement(5, 1)]))).is_none(), "the same list again says nothing");
    assert!(enc.encode(update_with(None)).is_none(), "no report says nothing either");
    let next = enc.encode(update_with(Some(vec![placement(5, 2)]))).expect("a new generation is a change");
    assert_eq!(next.images.as_ref().map(|l| l[0].1), Some(2));
    let gone = enc.encode(update_with(Some(Vec::new()))).expect("an empty list is a change");
    assert_eq!(gone.images, Some(Vec::new()), "an empty list that follows a list is sent as empty");
    let json = serde_json::to_value(&gone).expect("json");
    assert_eq!(json["images"], serde_json::json!([]));
    assert!(enc.encode(update_with(Some(Vec::new()))).is_none());
}

#[test]
fn a_frame_after_the_list_was_sent_carries_none_even_when_the_engine_reports_nothing() {
    let mut enc = FrameEncoder::new(1);
    enc.encode(update_with(Some(vec![placement(5, 1)]))).expect("first");
    let mut text = update_with(None);
    text.cursor = None;
    text.scrollbar.offset = 1;
    let frame = enc.encode(text).expect("a header change");
    assert!(frame.images.is_none(), "the page already holds the list");
}

#[test]
fn a_full_frame_requested_by_the_encoder_resends_the_list_it_holds() {
    let mut enc = FrameEncoder::new(1);
    enc.encode(update_with(Some(vec![placement(5, 1)]))).expect("first");
    enc.request_full();
    let full = enc.encode(update_with(None)).expect("full frame");
    assert!(full.full);
    assert_eq!(full.images.as_ref().map(Vec::len), Some(1));
    // And one that follows is plain again.
    assert!(enc.encode(update_with(None)).is_none());
}

#[test]
fn a_size_change_resets_the_list_with_the_engines_new_one() {
    let mut enc = FrameEncoder::new(1);
    enc.encode(update_with(Some(vec![placement(5, 1)]))).expect("first");
    let mut resized = update_with(Some(Vec::new()));
    resized.cols = 30;
    resized.reset = true;
    let frame = enc.encode(resized).expect("full");
    assert!(frame.full && frame.images.is_none(), "the page replaces everything: no list means none");
}

#[test]
fn the_encoder_cuts_the_list_at_the_engines_maximum() {
    let list: Vec<ImagePlacement> = (0..(MAX_PLACEMENTS as u32 + 40)).map(|i| placement(i + 1, 1)).collect();
    let mut enc = FrameEncoder::new(1);
    let frame = enc.encode(update_with(Some(list))).expect("frame");
    assert_eq!(frame.images.as_ref().map(Vec::len), Some(MAX_PLACEMENTS));
}
