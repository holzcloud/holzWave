//! OSC 8 hyperlinks from bytes to a frame, with the real engine (POL-01, D-05, D-06, D-08): the span in the update, the
//! cleaned span and the LINK flag in the row of the frame, and the rules of when a row is sent again.
#![cfg(feature = "ghostty")]

mod common;

use common::engine;
use holzwave_core::LinkSpan;
use holzwave_core::TerminalEngine;
use holzwave_core::wire::{FrameEncoder, FrameMsg, LINK, RowMsg};

const URI: &str = "https://example.org/a";

/// `ESC ] 8 ; params ; uri ESC \ text ESC ] 8 ; ; ESC \`
fn link(params: &str, uri: &str, text: &str) -> Vec<u8> {
    format!("\x1b]8;{params};{uri}\x1b\\{text}\x1b]8;;\x1b\\").into_bytes()
}

fn frame(e: &mut impl TerminalEngine, enc: &mut FrameEncoder) -> Option<FrameMsg> {
    enc.encode(e.update().expect("update"))
}

/// The flag of every cell of a row, from its runs.
fn flags(row: &RowMsg, cols: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for run in &row.runs {
        out.extend(std::iter::repeat_n(run.4, usize::from(run.1)));
    }
    out.resize(cols, 0);
    out
}

#[test]
fn a_link_in_the_output_is_a_cleaned_span_in_the_update() {
    let mut e = engine(20, 3);
    let mut bytes = link("", URI, "link");
    bytes.extend_from_slice(b" plain");
    e.feed(&bytes);
    let update = e.update().expect("update");
    let row = update.changed.iter().find(|r| r.y == 0).expect("row 0");
    assert_eq!(row.links, vec![LinkSpan { x0: 0, x1: 3, uri: URI.to_string() }]);
    assert!(update.changed.iter().filter(|r| r.y != 0).all(|r| r.links.is_empty()));
}

#[test]
fn the_row_of_the_frame_has_the_span_and_the_flag_on_exactly_its_cells() {
    let mut e = engine(20, 3);
    let mut enc = FrameEncoder::new(1);
    let mut bytes = link("", URI, "link");
    bytes.extend_from_slice(b"plain");
    e.feed(&bytes);
    let f = frame(&mut e, &mut enc).expect("first frame");
    let row = &f.changed[0];
    assert_eq!(row.links, vec![(0, 3, URI.to_string())]);
    let flags = flags(row, 20);
    assert!(flags[..4].iter().all(|f| f & LINK != 0), "{flags:?}");
    assert!(flags[4..].iter().all(|f| f & LINK == 0), "{flags:?}");
    assert_eq!(LINK, 16);
}

#[test]
fn the_json_has_a_links_key_only_on_a_row_with_links() {
    let mut e = engine(20, 3);
    let mut enc = FrameEncoder::new(1);
    frame(&mut e, &mut enc).expect("first");
    let mut bytes = link("", URI, "link");
    bytes.extend_from_slice(b"\r\nplain");
    e.feed(&bytes);
    let f = frame(&mut e, &mut enc).expect("typed");
    let json = serde_json::to_value(&f).expect("json");
    let rows = json["changed"].as_array().expect("rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["links"], serde_json::json!([[0, 3, URI]]));
    assert!(rows[1].as_object().expect("row").get("links").is_none(), "{}", rows[1]);
    // The first row's first run carries flag 16 and the key sits next to runs.
    assert_eq!(rows[0]["runs"][0][4], 16);
}

#[test]
fn overwriting_the_linked_cells_removes_the_span_and_sends_the_row_again_once() {
    let mut e = engine(20, 3);
    let mut enc = FrameEncoder::new(1);
    e.feed(&link("", URI, "link"));
    frame(&mut e, &mut enc).expect("first");

    e.feed(b"\rxxxx");
    let f = frame(&mut e, &mut enc).expect("the text changed and the link is gone");
    let row = &f.changed[0];
    assert!(row.links.is_empty());
    assert!(flags(row, 20).iter().all(|f| f & LINK == 0));
    assert!(frame(&mut e, &mut enc).is_none(), "nothing changed, nothing is sent");
}

#[test]
fn erasing_the_line_removes_the_span() {
    let mut e = engine(20, 3);
    let mut enc = FrameEncoder::new(1);
    e.feed(&link("", URI, "link"));
    frame(&mut e, &mut enc).expect("first");
    e.feed(b"\r\x1b[2K");
    let f = frame(&mut e, &mut enc).expect("the row changed");
    assert!(f.changed.iter().all(|r| r.links.is_empty()));
}

#[test]
fn the_closing_sequence_alone_changes_nothing_that_is_on_the_screen() {
    let mut e = engine(20, 3);
    let mut enc = FrameEncoder::new(1);
    // Opened and never closed here, then closed on its own: the text written while open stays linked, and the close
    // links nothing that comes after.
    e.feed(format!("\x1b]8;;{URI}\x1b\\link").as_bytes());
    frame(&mut e, &mut enc).expect("first");
    e.feed(b"\x1b]8;;\x1b\\");
    assert!(frame(&mut e, &mut enc).is_none(), "a close changes no cell");
    e.feed(b" after");
    let f = frame(&mut e, &mut enc).expect("typed");
    assert_eq!(f.changed[0].links, vec![(0, 3, URI.to_string())]);
}

#[test]
fn a_frame_that_changes_one_row_does_not_resend_the_linked_row() {
    let mut e = engine(20, 3);
    let mut enc = FrameEncoder::new(1);
    e.feed(&link("", URI, "link"));
    frame(&mut e, &mut enc).expect("first");
    e.feed(b"\r\nsecond");
    let f = frame(&mut e, &mut enc).expect("second row");
    assert_eq!(f.changed.len(), 1, "{:?}", f.changed);
    assert_eq!(f.changed[0].y, 1);
}

#[test]
fn parameters_are_accepted_and_ignored() {
    let mut e = engine(40, 3);
    e.feed(&link("id=a:foo=b", "https://x.example/", "idlink"));
    let update = e.update().expect("update");
    let row = update.changed.iter().find(|r| r.y == 0).expect("row 0");
    assert_eq!(row.links, vec![LinkSpan { x0: 0, x1: 5, uri: "https://x.example/".to_string() }]);
}

#[test]
fn a_link_on_a_later_row_is_a_span_on_that_row_in_a_dirty_only_update() {
    let mut e = engine(20, 4);
    let mut enc = FrameEncoder::new(1);
    frame(&mut e, &mut enc).expect("first");
    let mut bytes = b"\r\n\r\n  ".to_vec();
    bytes.extend(link("", "mailto:holz@example.org", "mail"));
    e.feed(&bytes);
    let update = e.update().expect("update");
    assert!(!update.full);
    let row = update.changed.iter().find(|r| r.y == 2).expect("row 2");
    assert_eq!(row.links, vec![LinkSpan { x0: 2, x1: 5, uri: "mailto:holz@example.org".to_string() }]);
}

#[test]
fn two_different_links_next_to_each_other_are_two_spans() {
    let mut e = engine(30, 2);
    let mut bytes = link("", "https://a.example/", "aaa");
    bytes.extend(link("", "https://b.example/", "bbb"));
    e.feed(&bytes);
    let update = e.update().expect("update");
    assert_eq!(
        update.changed[0].links,
        vec![
            LinkSpan { x0: 0, x1: 2, uri: "https://a.example/".to_string() },
            LinkSpan { x0: 3, x1: 5, uri: "https://b.example/".to_string() },
        ]
    );
}

#[test]
fn the_same_link_after_a_gap_is_a_second_span() {
    let mut e = engine(30, 2);
    let mut bytes = link("", URI, "aa");
    bytes.extend_from_slice(b"..");
    bytes.extend(link("", URI, "bb"));
    e.feed(&bytes);
    let update = e.update().expect("update");
    assert_eq!(
        update.changed[0].links,
        vec![LinkSpan { x0: 0, x1: 1, uri: URI.to_string() }, LinkSpan { x0: 4, x1: 5, uri: URI.to_string() }]
    );
}

#[test]
fn text_without_a_link_has_no_span_and_no_flag() {
    let mut e = engine(20, 3);
    let mut enc = FrameEncoder::new(1);
    e.feed(b"just text\r\nmore");
    let f = frame(&mut e, &mut enc).expect("first");
    for row in &f.changed {
        assert!(row.links.is_empty());
        assert!(row.runs.iter().all(|r| r.4 & LINK == 0));
    }
}
