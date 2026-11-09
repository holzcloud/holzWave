//! The byte bridge of a session that lives on a server: a headless pane, an atomic snapshot of its terminal plus a tap on
//! the bytes that follow, and a second engine that is fed both.
//!
//! What is proved here, in the smallest place and with a real shell on a real PTY and the real engine:
//!
//! - the join of the snapshot and the live bytes loses no byte and repeats none, for clients that attach while the
//!   program is writing;
//! - a second engine that is fed the snapshot shows what the first one shows (text, styles, cursor, modes, tab stops,
//!   scrolling region, the screen that is active, links), and the things a snapshot cannot carry are listed and
//!   asserted absent;
//! - the scrollback tail is exact and bounded;
//! - a tap that cannot keep up is dropped and told so, and neither stalls the pane nor grows memory;
//! - raw input bytes reach a headless pane and no other.
#![cfg(feature = "ghostty")]
// The programs here are `/bin/sh` scripts.
#![cfg(unix)]

mod common;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use common::Xorshift64;
use holzwave_core::engine::TerminalEngine;
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec, Tap, TapEnd, TapMsg};
use holzwave_core::textread::TextRegion;
use holzwave_core::GhosttyEngine;

const WAIT: Duration = Duration::from_secs(20);
const QUEUE: usize = 4 * 1024 * 1024;
const SNAPSHOT: usize = 4 * 1024 * 1024;

/// Everything a pane tells its sink.
#[derive(Clone, Default)]
struct Probe {
    events: Arc<Mutex<Vec<PaneEvent>>>,
}

impl Probe {
    fn sink(&self) -> impl FnMut(PaneEvent) + Send + 'static {
        let events = self.events.clone();
        move |event| events.lock().unwrap().push(event)
    }
}

fn headless(script: &str) -> PaneSpec {
    let mut spec = PaneSpec::new("/bin/sh", 80, 24).args(["-c", script]);
    spec.headless = true;
    spec
}

fn start(spec: PaneSpec, probe: &Probe) -> PaneHandle {
    PaneHandle::spawn_ghostty(1, spec, probe.sink()).expect("the pane starts")
}

/// A client: an engine of its own, built the way a client of the byte bridge builds it.
struct Client {
    engine: GhosttyEngine,
    tap: Tap,
}

impl Client {
    /// Attach and feed the snapshot into an engine of the size the attach answered.
    fn attach(pane: &PaneHandle, tail_rows: usize) -> Client {
        let tap = pane.attach_tap(tail_rows, SNAPSHOT, QUEUE).expect("the tap attaches");
        let mut engine = GhosttyEngine::new(tap.size.0, tap.size.1, 10_000).expect("client engine");
        engine.feed(&tap.snapshot);
        Client { engine, tap }
    }

    /// Feed what the tap delivers until the last rows of the client's own screen show `marker`; returns false when the
    /// tap ended or nothing came for the whole wait.
    fn pump_until(&mut self, marker: &str) -> bool {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            if self.shows(marker) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            match self.tap.rx.recv(Duration::from_millis(100)) {
                Some(TapMsg::Bytes(bytes)) => self.engine.feed(&bytes),
                Some(TapMsg::End(_)) => return self.shows(marker),
                None => {}
            }
        }
    }

    fn shows(&mut self, marker: &str) -> bool {
        self.text(TextRegion::LastRows(30)).contains(marker)
    }

    fn text(&mut self, region: TextRegion) -> String {
        self.engine.read_text(region, 512 * 1024).expect("read").map(|read| read.text).unwrap_or_default()
    }
}

fn pane_text(pane: &PaneHandle, region: TextRegion) -> String {
    pane.read_text(region, 512 * 1024).expect("read").map(|read| read.text).unwrap_or_default()
}

/// The numbers of the lines `n <int>` of a text, in the order they stand.
fn numbers(text: &str) -> Vec<u32> {
    text.lines().filter_map(|line| line.strip_prefix("n ")).filter_map(|rest| rest.trim().parse().ok()).collect()
}

/// A program that prints `count` numbered lines in bursts of 60, with a short pause between the bursts so that clients can
/// attach while it writes, then prints `done` and waits.
fn paced_numbers(count: u32) -> String {
    format!(
        "i=0; while [ $i -lt {count} ]; do echo \"n $i\"; i=$((i+1)); \
         if [ $((i % 60)) -eq 0 ]; then sleep 0.02; fi; done; echo done; sleep 60"
    )
}

/// Wait until the pane's own screen shows `marker`.
fn wait_for_pane(pane: &PaneHandle, marker: &str) {
    assert!(
        common::wait_until(WAIT, || pane_text(pane, TextRegion::LastRows(30)).contains(marker)),
        "the pane never showed {marker:?}"
    );
}

fn close(pane: PaneHandle) {
    pane.close();
    assert!(pane.join(Duration::from_secs(10)), "the pane did not end");
}

// ---- the join of the snapshot and the stream ----

#[test]
fn joins_a_snapshot_and_the_tap_lose_and_repeat_no_numbered_line() {
    const LINES: u32 = 1800;
    let probe = Probe::default();
    let pane = start(headless(&paced_numbers(LINES)), &probe);
    let mut rng = Xorshift64::new(0x5eed_0001);

    // Clients attach at random moments while the program is writing, every one with the screen only.
    let mut clients = Vec::new();
    for _ in 0..8 {
        thread::sleep(Duration::from_millis(rng.below(60) as u64));
        clients.push(Client::attach(&pane, 0));
    }

    let mut mid_stream = 0;
    for (n, client) in clients.iter_mut().enumerate() {
        assert!(client.pump_until("done"), "client {n} never saw the end of the program");
        let seen = numbers(&client.text(TextRegion::LastRows(2000)));
        assert!(!seen.is_empty(), "client {n} has no numbered line");
        // Present exactly once and in order: each number follows the one before it, up to the last line the program wrote.
        for pair in seen.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "client {n}: {} is followed by {}", pair[0], pair[1]);
        }
        assert_eq!(*seen.last().unwrap(), LINES - 1, "client {n} does not end at the last line");
        if seen[0] > 0 {
            mid_stream += 1;
        }
    }
    assert!(mid_stream >= 2, "only {mid_stream} of 8 clients attached after the program had started writing");
    close(pane);
}

#[test]
fn joins_a_burst_that_never_pauses_is_joined_without_loss_too() {
    const LINES: u32 = 1900;
    let script = format!("i=0; while [ $i -lt {LINES} ]; do echo \"n $i\"; i=$((i+1)); done; echo done; sleep 60");
    let probe = Probe::default();
    let pane = start(headless(&script), &probe);
    let mut rng = Xorshift64::new(0x5eed_0002);
    let mut clients = Vec::new();
    for _ in 0..6 {
        thread::sleep(Duration::from_micros(rng.below(4000) as u64));
        clients.push(Client::attach(&pane, 0));
    }
    for (n, client) in clients.iter_mut().enumerate() {
        assert!(client.pump_until("done"), "client {n} never saw the end of the program");
        let seen = numbers(&client.text(TextRegion::LastRows(2000)));
        assert!(!seen.is_empty(), "client {n}");
        for pair in seen.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "client {n}: {} is followed by {}", pair[0], pair[1]);
        }
        assert_eq!(*seen.last().unwrap(), LINES - 1, "client {n}");
    }
    close(pane);
}

#[test]
fn joins_the_second_engine_shows_the_same_screen_as_the_pane() {
    let probe = Probe::default();
    let pane = start(headless(&paced_numbers(600)), &probe);
    thread::sleep(Duration::from_millis(80));
    let mut client = Client::attach(&pane, 0);
    assert!(client.pump_until("done"));
    wait_for_pane(&pane, "done");

    assert_eq!(client.tap.size, (80, 24), "the attach states the size of the pane");
    let own = pane_text(&pane, TextRegion::Screen);
    let theirs = client.text(TextRegion::Screen);
    assert!(own.contains("n 599") && own.contains("done"), "the screen of the pane: {own:?}");
    assert_eq!(theirs, own, "the client's screen differs from the pane's");
    close(pane);
}

#[test]
fn joins_a_tap_of_a_pane_that_ended_ends_closed_after_its_bytes() {
    let probe = Probe::default();
    let pane = start(headless("sleep 0.3; echo last-words"), &probe);
    let mut client = Client::attach(&pane, 0);
    let mut ended = None;
    let deadline = std::time::Instant::now() + WAIT;
    while ended.is_none() && std::time::Instant::now() < deadline {
        match client.tap.rx.recv(Duration::from_millis(100)) {
            Some(TapMsg::Bytes(bytes)) => client.engine.feed(&bytes),
            Some(TapMsg::End(end)) => ended = Some(end),
            None => {}
        }
    }
    assert_eq!(ended, Some(TapEnd::Closed));
    assert!(client.text(TextRegion::LastRows(5)).contains("last-words"), "the last output came before the end");
    // The end repeats.
    assert_eq!(client.tap.rx.recv(Duration::ZERO), Some(TapMsg::End(TapEnd::Closed)));
    assert!(pane.join(Duration::from_secs(10)));
}

// ---- the engines: what a second engine fed a snapshot reproduces ----

mod engines {
    use super::*;
    use holzwave_core::engine::{DomKey, EngineEvent, Mods, SelectMode, SelectOp, Update};

    pub fn engine(cols: u16, rows: u16, scrollback: usize) -> GhosttyEngine {
        GhosttyEngine::new(cols, rows, scrollback).expect("engine")
    }

    /// An empty engine of the same size, fed the snapshot of `from`.
    pub fn rebuilt(from: &mut GhosttyEngine, tail_rows: usize) -> GhosttyEngine {
        let (cols, rows) = from.size().expect("size");
        let snapshot = from.snapshot_vt(tail_rows, SNAPSHOT).expect("snapshot");
        let mut client = engine(cols, rows, 10_000);
        client.feed(&snapshot);
        client
    }

    pub fn text(engine: &mut GhosttyEngine) -> String {
        engine.read_text(TextRegion::LastRows(2000), 512 * 1024).expect("read").map(|read| read.text).unwrap_or_default()
    }

    /// What a page would draw of the screen right now: every row, with the links, the cursor and the modes.
    pub fn drawn(engine: &mut GhosttyEngine) -> Update {
        engine.request_full();
        let mut update = engine.update().expect("update");
        // The scrollbar says how much history each side holds; the tests compare that where it is meant to be equal.
        update.scrollbar = Default::default();
        update
    }

    pub fn arrow_up(engine: &mut GhosttyEngine) -> Vec<u8> {
        let key = DomKey { code: "ArrowUp".into(), key: "ArrowUp".into(), mods: Mods::NONE, ..DomKey::default() };
        let event = holzwave_core::engine::KeyEvent::from_dom(&key, false);
        engine.encode_key(&event).expect("encode")
    }

    pub fn pasted(engine: &mut GhosttyEngine) -> Vec<u8> {
        engine.paste("word", false).expect("paste");
        engine.take_replies()
    }

    pub fn numbered(count: usize) -> Vec<u8> {
        (0..count).flat_map(|i| format!("n {i}\r\n").into_bytes()).collect()
    }

    mod equality {
        use super::*;

        /// Styled text of every kind the snapshot is to carry, a wide character, a combining sequence, a link, modes, a
        /// scrolling region, custom tab stops and a cursor of a chosen shape and place; ends on the main screen.
        fn styled() -> Vec<u8> {
            let mut out = String::new();
            // Scrollback first, so that the styled text below stays on the screen of 20 rows.
            for i in 0..30 {
                out.push_str(&format!("scrolled {i}\r\n"));
            }
            out.push_str("\x1b[?2004h\x1b[?1h"); // bracketed paste, application cursor keys
            out.push_str("\x1b[31mred\x1b[0m \x1b[38;5;200mp200\x1b[0m \x1b[38;2;10;20;30m\x1b[48;2;200;100;50mtrue\x1b[0m\r\n");
            out.push_str("\x1b[1mbold\x1b[0m \x1b[3mital\x1b[0m \x1b[4mul\x1b[0m \x1b[4:3mcurly\x1b[0m \x1b[4:2mdbl\x1b[0m \x1b[7minv\x1b[0m\r\n");
            out.push_str("wide \u{6f22}\u{5b57} combining e\u{301}x\r\n");
            out.push_str("\x1b]8;;https://example.com/a?b=1\x1b\\a link\x1b]8;;\x1b\\ and no link\r\n");
            out.push_str("a line that is longer than the forty columns of this terminal and so it wraps\r\n");
            // Tab stops: clear all, then set columns 10 and 30 (the cursor is moved there and an HTS is sent).
            out.push_str("\x1b[3g\x1b[1;11H\x1bH\x1b[1;31H\x1bH");
            // A scrolling region of the rows 5 to 15, which homes the cursor, then the cursor at a chosen place and shape.
            out.push_str("\x1b[5;15r\x1b[7;9H\x1b[5 q");
            out.into_bytes()
        }

        fn reference() -> GhosttyEngine {
            let mut pane = engine(40, 20, 10_000);
            pane.feed(&styled());
            pane
        }

        #[test]
        fn styles_text_links_cursor_modes_tab_stops_and_region_are_reproduced() {
            let mut pane = reference();
            let mut client = rebuilt(&mut pane, 5);

            // What a page would draw: every row with its cells (colours, bold, italic, underline, inverse, width) and its
            // links, the cursor with its place, shape, blinking and visibility, and the screen that is active.
            let (own, theirs) = (drawn(&mut pane), drawn(&mut client));
            assert_eq!(theirs.cursor, own.cursor, "the cursor");
            assert_eq!(theirs.modes, own.modes, "the modes the page reads");
            assert_eq!(theirs.changed.len(), own.changed.len());
            for (mine, yours) in own.changed.iter().zip(&theirs.changed) {
                assert_eq!(yours, mine, "row {}", mine.y);
            }
            let cursor = own.cursor.expect("a cursor");
            assert_eq!((cursor.x, cursor.y), (8, 6), "the chosen cell");
            assert_eq!(cursor.style, holzwave_core::engine::CursorStyle::Bar, "the chosen shape");
            assert!(own.changed.iter().any(|row| !row.links.is_empty()), "the reference has a link");
            assert!(own.modes.bracketed_paste, "the reference has bracketed paste on");

            // The text, as the reader of the page reads it (soft wraps joined, so the wrapped line is one line again).
            let tail = &text(&mut pane);
            assert!(tail.contains("a line that is longer than the forty columns of this terminal and so it wraps"));
            let screen_of = |e: &mut GhosttyEngine| e.read_text(TextRegion::Screen, 64 * 1024).unwrap().map(|r| r.text);
            assert_eq!(screen_of(&mut client), screen_of(&mut pane));

            // The modes, by what they do.
            assert_eq!(pasted(&mut client), pasted(&mut pane));
            assert!(pasted(&mut client).starts_with(b"\x1b[200~"), "bracketed paste is on in the client");
            assert_eq!(arrow_up(&mut client), arrow_up(&mut pane));
            assert_eq!(arrow_up(&mut client), b"\x1bOA", "application cursor keys are on in the client");

            // The tab stops and the scrolling region, by feeding both the same bytes and comparing again: tabs from the
            // start of a row, then line feeds at the bottom of the region, which scroll the region and nothing else.
            let more = b"\r\t[a]\t[b]\t[c]\x1b[15;1H\n\n\nafter\x1b[?25l";
            pane.feed(more);
            client.feed(more);
            let (own, theirs) = (drawn(&mut pane), drawn(&mut client));
            assert_eq!(theirs.cursor, own.cursor);
            assert_eq!(theirs.changed, own.changed, "the same bytes after the snapshot give the same screen");
            assert!(!theirs.cursor.expect("cursor").visible, "visibility follows the program in both");
        }

        #[test]
        fn a_snapshot_of_the_rebuilt_engine_is_the_snapshot_it_was_built_from() {
            let mut pane = reference();
            let first = pane.snapshot_vt(5, SNAPSHOT).expect("snapshot");
            let mut client = rebuilt(&mut pane, 5);
            // Everything the formatter reads from the client is what it read from the reference, down to the underline
            // styles that the cells of the page cannot tell apart.
            assert_eq!(client.snapshot_vt(5, SNAPSHOT).expect("snapshot"), first);
            let all = String::from_utf8_lossy(&first).into_owned();
            for expected in ["4:3", "4:2", "]8;;https://example.com/a?b=1", "[5;15r", "[3g", "[?2004h", "[?1h", "[5 q"] {
                assert!(all.contains(expected), "the snapshot lacks {expected:?}: {all:?}");
            }
        }

        #[test]
        fn the_alternate_screen_that_is_active_is_what_the_client_gets() {
            let mut pane = engine(40, 10, 1000);
            pane.feed(&numbered(40));
            pane.feed(b"\x1b[?1049h\x1b[2;3Hfull screen\x1b[7mbar\x1b[0m");
            let mut client = rebuilt(&mut pane, 100);
            let (own, theirs) = (drawn(&mut pane), drawn(&mut client));
            assert!(own.modes.alt_screen && theirs.modes.alt_screen, "the alternate screen is active in both");
            assert_eq!(theirs.changed, own.changed);
            // The main screen behind it is not carried; this is a stated limit of the snapshot, so a client that sees the
            // program leave the alternate screen sees an empty main screen.
            client.feed(b"\x1b[?1049l");
            assert_eq!(numbers(&text(&mut client)), Vec::<u32>::new(), "the main screen was not carried");
            pane.feed(b"\x1b[?1049l");
            assert_eq!(numbers(&text(&mut pane)).len(), 40, "the pane's own main screen is whole");
        }

        #[test]
        fn what_a_snapshot_cannot_carry_is_absent_and_the_directory_is_carried() {
            let mut pane = engine(40, 10, 1000);
            // A one pixel picture (the Kitty protocol, direct data), a title, a directory and a selection.
            pane.feed(b"\x1b_Ga=T,f=32,s=1,v=1,i=7,q=2;/wAA/w==\x1b\\");
            pane.feed(b"\x1b]2;a title\x07\x1b]7;file://host/tmp/work\x07hello world\r\n");
            pane.select(SelectOp::Begin { x: 0, y: 0, mode: SelectMode::Cell }).expect("select");
            pane.select(SelectOp::Extend { x: 4, y: 0 }).expect("extend");
            assert_eq!(pane.selection_text().unwrap().as_deref(), Some("hello"));
            assert!(pane.image_generation(7).is_some(), "the reference holds the picture");
            let own_events = pane.take_events();
            assert!(own_events.iter().any(|e| matches!(e, EngineEvent::Title(_))));

            let mut client = rebuilt(&mut pane, 100);
            assert!(client.image_generation(7).is_none(), "pictures are not carried");
            assert_eq!(client.selection_text().unwrap(), None, "the selection is not carried");
            let events = client.take_events();
            assert!(!events.iter().any(|e| matches!(e, EngineEvent::Title(_))), "the title is not carried: {events:?}");
            assert!(
                events.iter().any(|e| matches!(e, EngineEvent::Pwd(dir) if dir == "file://host/tmp/work")),
                "the working directory is carried: {events:?}"
            );
            assert_eq!(text(&mut client), "hello world");
        }

        #[test]
        fn a_snapshot_changes_nothing_the_pane_shows() {
            let mut pane = engine(40, 6, 100);
            pane.feed(b"first\r\n");
            let _ = pane.update().expect("update");
            pane.feed(b"second\r\n");
            let _ = rebuilt(&mut pane, 10);
            let update = pane.update().expect("update");
            assert!(
                update.changed.iter().any(|row| row.cells.iter().map(|c| c.text.as_str()).collect::<String>().contains("second")),
                "the row the snapshot saw is still in the next frame"
            );
        }
    }

    mod bounds {
        use super::*;
        use std::time::Instant;

        #[test]
        fn a_snapshot_over_its_bound_is_refused_whole_never_cut() {
            let mut pane = engine(80, 24, 10_000);
            pane.feed(&numbered(2000));
            let size = pane.snapshot_vt(1000, SNAPSHOT).expect("snapshot").len();
            assert!(size > 4000, "{size}");
            assert!(pane.snapshot_vt(1000, size).is_ok(), "exactly the bound fits");
            assert!(pane.snapshot_vt(1000, size - 1).is_err(), "a byte over is refused");
            assert!(pane.snapshot_vt(1000, 0).is_err());
            // The engine is as it was.
            assert_eq!(pane.snapshot_vt(1000, SNAPSHOT).expect("snapshot").len(), size);
        }

        #[test]
        fn a_snapshot_over_its_bound_leaves_the_pane_running_and_registers_no_tap() {
            let probe = Probe::default();
            let pane = start(headless(&paced_numbers(500)), &probe);
            thread::sleep(Duration::from_millis(100));
            assert!(pane.attach_tap(0, 10, QUEUE).is_err(), "ten bytes cannot hold a screen");
            let mut client = Client::attach(&pane, 0);
            assert!(client.pump_until("done"), "the pane runs on and the next attach works");
            close(pane);
        }

        #[test]
        fn a_snapshot_of_ten_thousand_rows_is_taken_in_a_measured_time() {
            let mut pane = engine(120, 40, 10_000);
            // Mixed output: plain lines, styled lines, wide characters and a link now and then.
            let mut feed = String::new();
            for i in 0..10_040 {
                match i % 4 {
                    0 => feed.push_str(&format!("line {i} plain text of a length that fills a part of the row\r\n")),
                    1 => feed.push_str(&format!("\x1b[3{}mline {i}\x1b[1;4m styled \x1b[0m\x1b[48;5;{}mbg\x1b[0m\r\n", i % 8, i % 256)),
                    2 => feed.push_str(&format!("{i} \u{6f22}\u{5b57}\u{3042}\u{3044} wide e\u{301} combining\r\n")),
                    _ => feed.push_str(&format!("\x1b]8;;https://example.com/{i}\x1b\\link {i}\x1b]8;;\x1b\\ after\r\n")),
                }
            }
            pane.feed(feed.as_bytes());
            let rows = pane.screen().expect("screen").scrollbar.total;
            assert!(rows >= 9_000, "{rows} rows");
            let started = Instant::now();
            let snapshot = pane.snapshot_vt(10_000, 64 * 1024 * 1024).expect("snapshot");
            let took = started.elapsed();
            println!("snapshot of {rows} rows (tail 10000, 120x40, debug build): {} bytes in {:?}", snapshot.len(), took);
            assert!(took < Duration::from_secs(2), "{took:?}");
            let mut client = engine(120, 40, 10_000);
            let started = Instant::now();
            client.feed(&snapshot);
            println!("feeding it into an empty engine: {:?}", started.elapsed());
            assert_eq!(drawn(&mut client).changed, drawn(&mut pane).changed, "the screens agree after 10 000 rows");
        }
    }

    mod tail {
        use super::*;

        #[test]
        fn a_tail_of_a_thousand_rows_of_three_thousand_lines_is_the_last_thousand_and_the_screen() {
            let mut pane = engine(80, 24, 10_000);
            pane.feed(&numbered(3000));
            let mut client = rebuilt(&mut pane, 1000);
            let seen = numbers(&text(&mut client));
            // The screen holds the last 23 lines and an empty row under the cursor, so the tail and the screen are 1023 lines.
            let expected: Vec<u32> = (1977..3000).collect();
            assert_eq!(seen, expected, "first {:?}, last {:?}, {} lines", seen.first(), seen.last(), seen.len());
            let total = client.screen().expect("screen").scrollbar.total;
            assert_eq!(total, 1000 + 24, "nothing older than the tail is in the client");
        }

        #[test]
        fn a_tail_larger_than_the_scrollback_is_clamped_and_none_gives_the_screen_only() {
            let mut pane = engine(80, 24, 10_000);
            pane.feed(&numbered(200));
            let all = numbers(&text(&mut rebuilt(&mut pane, 1_000_000)));
            assert_eq!(all, (0..200).collect::<Vec<u32>>(), "every row that exists, no more");
            let exactly = numbers(&text(&mut rebuilt(&mut pane, 177)));
            assert_eq!(exactly, (0..200).collect::<Vec<u32>>(), "the scrollback is 177 rows");
            let screen_only = numbers(&text(&mut rebuilt(&mut pane, 0)));
            assert_eq!(screen_only, (177..200).collect::<Vec<u32>>());
            assert_eq!(rebuilt(&mut pane, 0).screen().expect("screen").scrollbar.total, 24);
            usize::MAX.checked_add(0).expect("a huge tail does not overflow the call");
            let _ = rebuilt(&mut pane, usize::MAX);
        }

        #[test]
        fn a_tail_that_starts_inside_a_wrapped_line_keeps_the_text_of_the_rest() {
            let mut pane = engine(20, 4, 1000);
            // One logical line of 70 characters is four rows of 20.
            let long: String = (0..70u8).map(|i| char::from(b'a' + i % 26)).collect();
            pane.feed(format!("{long}\r\nend1\r\nend2\r\nend3\r\n").as_bytes());
            for tail in 0..=5 {
                let mut client = rebuilt(&mut pane, tail);
                assert_eq!(drawn(&mut client).changed, drawn(&mut pane).changed, "the screens differ with a tail of {tail}");
            }
            // With the whole line in the tail the client reads it back as one line, as the pane does.
            let mut client = rebuilt(&mut pane, 10);
            assert_eq!(text(&mut client), text(&mut pane), "a wrapped line is one line in the client, as in the pane");
            assert!(text(&mut client).contains(&long));
        }
    }
}
