//! Hostile hyperlinks (POL-01, D-05, D-06): bad addresses, floods, the budgets, wrapped and wide
//! links, the alternate screen, a resize, scrolling, the frame budget and the cost of a screen full of links.
#![cfg(feature = "ghostty")]

mod common;
mod perf_report;

use std::time::Instant;

use common::engine;
use holzwave_core::TerminalEngine;
use holzwave_core::links::{
    MAX_SPANS_PER_ROW, MAX_URI_BYTES, MAX_URI_BYTES_PER_FRAME, MAX_URI_BYTES_PER_ROW, clean_uri,
};
use holzwave_core::wire::{FrameEncoder, FrameMsg, LINK, RowMsg};
use holzwave_core::{GhosttyEngine, LinkSpan};

fn osc8(uri: &[u8], text: &str) -> Vec<u8> {
    let mut out = b"\x1b]8;;".to_vec();
    out.extend_from_slice(uri);
    out.extend_from_slice(b"\x1b\\");
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(b"\x1b]8;;\x1b\\");
    out
}

fn frame(e: &mut impl TerminalEngine, enc: &mut FrameEncoder) -> Option<FrameMsg> {
    enc.encode(e.update().expect("update"))
}

/// Every span of every changed row of an update.
fn spans(e: &mut GhosttyEngine) -> Vec<(u16, LinkSpan)> {
    let update = e.update().expect("update");
    update.changed.iter().flat_map(|r| r.links.iter().map(|l| (r.y, l.clone()))).collect()
}

fn flagged_cells(row: &RowMsg) -> usize {
    row.runs.iter().filter(|r| r.4 & LINK != 0).map(|r| usize::from(r.1)).sum()
}

fn address_bytes(f: &FrameMsg) -> usize {
    f.changed.iter().flat_map(|r| r.links.iter()).map(|l| l.2.len()).sum()
}

/// An address of exactly `n` bytes: `https://h/` and then letters.
fn address_of(n: usize, tag: usize) -> String {
    let head = format!("https://h/{tag}/");
    assert!(head.len() < n);
    format!("{head}{}", "a".repeat(n - head.len()))
}

// ---- Bad addresses ---------------------------------------------------------------------------------------------------

#[test]
fn an_address_with_a_character_that_may_not_be_in_it_never_reaches_a_span() {
    let bad: [&[u8]; 6] = [
        b"https://a\x7fb",
        b"https://a\x01b",
        b"https://a b",
        b"https://a\xc3\xa4b",
        "https://a\u{202e}b".as_bytes(),
        b"https://a\xffb",
    ];
    for uri in bad {
        let mut e = engine(40, 3);
        e.feed(&osc8(uri, "txt"));
        let found = spans(&mut e);
        // Whatever the terminal makes of the control characters it strips, a span that exists is a clean one.
        for (_, span) in &found {
            assert!(clean_uri(span.uri.as_bytes()).is_some(), "{:?}", span.uri);
            assert!(span.uri.bytes().all(|b| (0x21..=0x7e).contains(&b)), "{:?}", span.uri);
        }
        assert!(e.screen().expect("screen").to_text().contains("txt"), "the text stays on screen for {uri:?}");
    }
}

#[test]
fn a_blank_a_utf8_letter_and_a_format_character_give_no_span_at_all() {
    for uri in [&b"https://a b"[..], b"https://a\xc3\xa4b", "https://a\u{202e}b".as_bytes()] {
        let mut e = engine(40, 3);
        e.feed(&osc8(uri, "txt"));
        assert!(spans(&mut e).is_empty(), "{uri:?}");
        assert!(e.screen().expect("screen").to_text().starts_with("txt"));
    }
}

#[test]
fn the_length_limit_is_what_the_engine_keeps_and_nothing_longer_is_a_span() {
    // The engine keeps an address of at most 2046 bytes whole and drops a longer one (measured in the spike), so the
    // cap of 2048 bytes in `clean_uri` is a second line that cannot fire for OSC 8.
    let mut kept = 0;
    for n in [100, 2000, 2046, 2047, 2048, 2049, 4000] {
        let mut e = engine(40, 3);
        e.feed(&osc8(address_of(n, 0).as_bytes(), "L"));
        let found = spans(&mut e);
        assert!(found.len() <= 1, "{n}");
        if let Some((_, span)) = found.first() {
            assert_eq!(span.uri.len(), n);
            assert!(n <= MAX_URI_BYTES);
            kept = kept.max(n);
        } else {
            assert!(n >= 2047, "an address of {n} bytes must be kept");
        }
        assert!(e.screen().expect("screen").to_text().starts_with('L'));
    }
    assert_eq!(kept, 2046);
    // The cleaning itself draws the line at 2048 bytes.
    assert!(clean_uri(address_of(2048, 0).as_bytes()).is_some());
    assert!(clean_uri(address_of(2049, 0).as_bytes()).is_none());
}

#[test]
fn each_denied_scheme_in_two_letter_cases_gives_no_span_and_the_text_stays() {
    for scheme in ["javascript", "JavaScript", "data", "DATA", "vbscript", "VBScript", "blob", "Blob", "about", "ABOUT"] {
        let mut e = engine(40, 3);
        e.feed(&osc8(format!("{scheme}:payload").as_bytes(), "txt"));
        assert!(spans(&mut e).is_empty(), "{scheme}");
        assert!(e.screen().expect("screen").to_text().starts_with("txt"), "{scheme}");
    }
}

#[test]
fn a_file_address_is_delivered_and_the_policy_decides_later() {
    let mut e = engine(40, 3);
    e.feed(&osc8(b"file:///etc/passwd", "etc"));
    let found = spans(&mut e);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1.uri, "file:///etc/passwd");
}

#[test]
fn a_link_that_is_never_closed_covers_only_the_cells_the_program_wrote_with_it_open() {
    let mut e = engine(10, 4);
    e.feed(b"\x1b]8;;https://example.org/open\x1b\\ab\r\ncd");
    let found = spans(&mut e);
    assert_eq!(
        found,
        vec![
            (0, LinkSpan { x0: 0, x1: 1, uri: "https://example.org/open".to_string() }),
            (1, LinkSpan { x0: 0, x1: 1, uri: "https://example.org/open".to_string() }),
        ]
    );
}

// ---- Budgets ---------------------------------------------------------------------------------------------------------

#[test]
fn forty_links_in_one_row_give_the_first_eight_by_column() {
    let mut e = engine(100, 2);
    let mut bytes = Vec::new();
    for i in 0..40 {
        bytes.extend(osc8(format!("https://x.example/{i}").as_bytes(), "l"));
        bytes.push(b' ');
    }
    e.feed(&bytes);
    let found = spans(&mut e);
    assert_eq!(found.len(), MAX_SPANS_PER_ROW, "{found:?}");
    for (i, (y, span)) in found.iter().enumerate() {
        assert_eq!(*y, 0);
        assert_eq!((span.x0, span.x1), (i as u16 * 2, i as u16 * 2));
        assert_eq!(span.uri, format!("https://x.example/{i}"));
    }
}

#[test]
fn addresses_of_1500_bytes_give_two_spans_in_a_row_and_the_third_is_over_the_row_budget() {
    let mut e = engine(20, 2);
    let mut bytes = Vec::new();
    for i in 0..3 {
        bytes.extend(osc8(address_of(1500, i).as_bytes(), "l"));
        bytes.push(b' ');
    }
    e.feed(&bytes);
    let found = spans(&mut e);
    assert_eq!(found.len(), 2, "3000 bytes fit, 4500 do not");
    assert!(found.iter().map(|(_, s)| s.uri.len()).sum::<usize>() <= MAX_URI_BYTES_PER_ROW);
    assert_eq!(found[1].1.uri, address_of(1500, 1));
}

/// A screen of `rows` rows with three links of 1500 bytes each on every row (two of them become spans).
fn flooded(rows: u16) -> (GhosttyEngine, FrameEncoder) {
    let mut e = engine(20, rows);
    let mut bytes = Vec::new();
    for y in 0..rows {
        for i in 0..3 {
            bytes.extend(osc8(address_of(1500, usize::from(y) * 3 + i).as_bytes(), "l"));
            bytes.push(b' ');
        }
        if y + 1 < rows {
            bytes.extend_from_slice(b"\r\n");
        }
    }
    e.feed(&bytes);
    (e, FrameEncoder::new(1))
}

#[test]
fn a_flood_of_links_in_one_frame_stops_at_the_frame_budget() {
    let (mut e, mut enc) = flooded(100);
    let f = frame(&mut e, &mut enc).expect("first");
    assert!(f.full);
    let total = address_bytes(&f);
    assert!(total > 0 && total <= MAX_URI_BYTES_PER_FRAME, "{total}");
    // 3000 bytes per row: 43 rows fit in 131072 bytes, the 44th does not.
    assert_eq!(total, 43 * 3000);
    let linked_rows = f.changed.iter().filter(|r| !r.links.is_empty()).count();
    assert_eq!(linked_rows, 43);
    for row in &f.changed {
        if row.links.is_empty() {
            assert_eq!(flagged_cells(row), 0, "row {} has the flag without a span", row.y);
        } else {
            assert!(row.y < 43, "row {} is past the budget and has links", row.y);
            assert_eq!(flagged_cells(row), row.links.iter().map(|l| usize::from(l.1 - l.0) + 1).sum::<usize>());
        }
    }
    // The rows after the budget keep their text.
    assert!(f.changed[70].runs[0].0.starts_with("l l l"));
    eprintln!("Frame budget: {total} address bytes in {linked_rows} rows of the 100");
}

#[test]
fn the_same_screen_gives_the_same_frame_and_nothing_is_resent() {
    let (mut e, mut enc) = flooded(100);
    let first = frame(&mut e, &mut enc).expect("first");
    // The page asks for a full frame again: the rows come out exactly as before (no flicker between two frames).
    enc.request_full();
    e.request_full();
    let again = frame(&mut e, &mut enc).expect("full again");
    assert!(again.full);
    assert_eq!(first.changed, again.changed);
    // An update that changes nothing is no frame, and the rows that were dropped are not resent.
    assert!(frame(&mut e, &mut enc).is_none());
}

#[test]
fn a_later_frame_that_changes_one_row_after_the_budget_does_not_resend_the_others() {
    let (mut e, mut enc) = flooded(100);
    frame(&mut e, &mut enc).expect("first");
    e.feed(b"\x1b[71;1Hq");
    let f = frame(&mut e, &mut enc).expect("one row changed");
    assert!(!f.full);
    assert_eq!(f.changed.len(), 1, "{:?}", f.changed.iter().map(|r| r.y).collect::<Vec<_>>());
    assert_eq!(f.changed[0].y, 70);
    assert!(f.changed[0].links.is_empty() && flagged_cells(&f.changed[0]) == 0, "still after the budget");
}

#[test]
fn a_row_inside_the_budget_that_changes_keeps_its_links_and_alone_is_sent() {
    let (mut e, mut enc) = flooded(100);
    frame(&mut e, &mut enc).expect("first");
    e.feed(b"\x1b[6;1HQ");
    let f = frame(&mut e, &mut enc).expect("one row changed");
    assert_eq!(f.changed.len(), 1);
    assert_eq!(f.changed[0].y, 5);
    assert_eq!(f.changed[0].links.len(), 2, "the row stays inside the budget");
}

// ---- Structure -------------------------------------------------------------------------------------------------------

#[test]
fn a_link_that_wraps_is_two_spans_with_the_same_address() {
    let mut e = engine(10, 3);
    e.feed(&osc8(b"https://example.org/w", "abcdefghijklmno"));
    let found = spans(&mut e);
    assert_eq!(
        found,
        vec![
            (0, LinkSpan { x0: 0, x1: 9, uri: "https://example.org/w".to_string() }),
            (1, LinkSpan { x0: 0, x1: 4, uri: "https://example.org/w".to_string() }),
        ]
    );
}

#[test]
fn a_wide_character_inside_a_link_is_inside_its_span() {
    let mut e = engine(20, 2);
    let mut enc = FrameEncoder::new(1);
    e.feed(&osc8(b"https://example.org/j", "a\u{65e5}b"));
    let f = frame(&mut e, &mut enc).expect("first");
    let row = &f.changed[0];
    assert_eq!(row.links, vec![(0, 3, "https://example.org/j".to_string())]);
    assert_eq!(flagged_cells(row), 4);
}

#[test]
fn a_wide_character_at_the_end_of_a_link_is_covered_to_its_second_half() {
    let mut e = engine(20, 2);
    e.feed(&osc8(b"https://example.org/j", "ab\u{65e5}"));
    let found = spans(&mut e);
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].1.x0, found[0].1.x1), (0, 3));
}

#[test]
fn a_link_on_the_alternate_screen_is_gone_when_the_screen_is_left() {
    let mut e = engine(20, 3);
    let mut enc = FrameEncoder::new(1);
    frame(&mut e, &mut enc).expect("first");
    e.feed(b"\x1b[?1049h");
    e.feed(&osc8(b"https://example.org/alt", "alt"));
    let f = frame(&mut e, &mut enc).expect("alternate screen");
    assert_eq!(f.changed.iter().filter(|r| !r.links.is_empty()).count(), 1);
    e.feed(b"\x1b[?1049l");
    let f = frame(&mut e, &mut enc).expect("back on the main screen");
    assert!(f.changed.iter().all(|r| r.links.is_empty() && flagged_cells(r) == 0), "{:?}", f.changed);
}

#[test]
fn after_a_resize_to_fewer_columns_the_spans_lie_inside_the_new_grid_and_the_frame_is_full() {
    let mut e = engine(40, 4);
    let mut enc = FrameEncoder::new(1);
    e.feed(b"0123456789");
    e.feed(&osc8(b"https://example.org/r", "link link link link"));
    let before = frame(&mut e, &mut enc).expect("first");
    assert!(before.changed[0].links.iter().any(|l| l.1 >= 20));
    e.resize(15, 4, (8, 16)).expect("resize");
    let f = frame(&mut e, &mut enc).expect("resized");
    assert!(f.full);
    assert_eq!(f.cols, 15);
    for row in &f.changed {
        for (x0, x1, _) in &row.links {
            assert!(x0 <= x1 && *x1 < 15, "{x0}..{x1} in {} columns", f.cols);
        }
        assert!(row.runs.iter().map(|r| usize::from(r.1)).sum::<usize>() <= 15);
    }
}

#[test]
fn scrolling_moves_the_spans_with_their_rows() {
    let mut e = engine(20, 4);
    let mut enc = FrameEncoder::new(1);
    e.feed(b"\x1b[3;1H");
    e.feed(&osc8(b"https://example.org/s", "link"));
    let f = frame(&mut e, &mut enc).expect("first");
    assert_eq!(f.changed[2].links.len(), 1);
    // Two more lines at the bottom push the link up by two rows.
    e.feed(b"\x1b[4;1H\r\n\r\n");
    let f = frame(&mut e, &mut enc).expect("scrolled");
    assert!(!f.full);
    let linked: Vec<u16> = f.changed.iter().filter(|r| !r.links.is_empty()).map(|r| r.y).collect();
    assert_eq!(linked, vec![0], "{linked:?}");
    // The row it left is sent without links.
    assert!(f.changed.iter().filter(|r| r.y == 2).all(|r| r.links.is_empty() && flagged_cells(r) == 0));
}

#[test]
fn a_cleaned_span_survives_the_json_round_trip_as_three_values() {
    let mut e = engine(20, 2);
    let mut enc = FrameEncoder::new(1);
    e.feed(&osc8(b"mailto:holz@example.org", "mail"));
    let f = frame(&mut e, &mut enc).expect("first");
    let json = serde_json::to_value(&f).expect("json");
    assert_eq!(json["changed"][0]["links"], serde_json::json!([[0, 3, "mailto:holz@example.org"]]));
    assert!(json["changed"][1].get("links").is_none());
}

// ---- Cost ------------------------------------------------------------------------------------------------------------

/// Whole-screen update and encode of 120 by 40 cells where every cell is part of a link, and the same screen without.
/// Release only; it prints the times and asserts none (the perf recorder judges numbers, not this test).
#[test]
#[ignore = "timing, run in release: cargo test --release -p holzwave-core --test links_hostile -- --ignored --nocapture"]
fn the_cost_of_a_screen_where_every_cell_is_a_link() {
    let (cols, rows) = (120u16, 40u16);
    let mut linked = Vec::new();
    let mut plain = Vec::new();
    for y in 0..rows {
        // Eight links per row (the row budget), together covering the whole row; the rest of the cells of each link
        // are read too, which is the worst case for the cell loop.
        for i in 0..8 {
            let text = "x".repeat(usize::from(cols) / 8);
            linked.extend(osc8(format!("https://example.org/{y}/{i}").as_bytes(), &text));
        }
        plain.extend(std::iter::repeat_n(b'x', usize::from(cols)));
        if y + 1 < rows {
            linked.extend_from_slice(b"\r\n");
            plain.extend_from_slice(b"\r\n");
        }
    }
    let measure = |input: &[u8]| -> Vec<f64> {
        let mut times = Vec::new();
        for _ in 0..20 {
            let mut e = engine(cols, rows);
            let mut enc = FrameEncoder::new(1);
            e.feed(input);
            let start = Instant::now();
            let update = e.update().expect("update");
            let f = enc.encode(update).expect("frame");
            times.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(f.changed.len(), usize::from(rows));
        }
        times
    };
    let with_links = measure(&linked);
    let without = measure(&plain);
    perf_report::report("links.update_encode_all_linked_ms", "ms", &with_links);
    perf_report::report("links.update_encode_no_links_ms", "ms", &without);
    let ratio = perf_report::median(&with_links) / perf_report::median(&without);
    eprintln!("Cost: all linked {:.3} ms, no links {:.3} ms, ratio {ratio:.2}", perf_report::median(&with_links), perf_report::median(&without));
}
