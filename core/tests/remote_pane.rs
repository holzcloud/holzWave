//! A pane that shows a session on a server, with the real engine, against a scripted server on a loopback socket.
//!
//! - the states of the connection (reconnecting with its count, failed with a way to try again, signed out), the notices on
//!   the screen and what is typed while the connection is down;
//! - the clean re-attach: after a resync, a reconnect and a request for older lines the pane holds exactly what a new pane
//!   holds when it is attached at that moment, to the byte of its own snapshot;
//! - the end of a session, with its status, and the words of the design record;
//! - the writer view, the takeover, the grid rule (the writer's size is the session's, a watcher follows) and older lines
//!   that keep the viewport where it was;
//! - a hostile server ends the attachment with a fixed sentence and nothing else.
#![cfg(feature = "ghostty")]
#![cfg(unix)]

#[path = "support/fake_device.rs"]
mod fake_device;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fake_device::{Conn, FakeDevice, Got, PATIENCE};
use holzwave_core::GhosttyEngine;
use holzwave_core::conn::{ConnState, RemoteState};
use holzwave_core::engine::{DomKey, TerminalEngine};
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec, RemoteTarget};
use holzwave_core::remote::hub::{Hub, HubOptions};
use holzwave_core::remote::link::{
    EXIT_GAVE_UP, INPUT_DROPPED_FOR, NOTICE_ATTACHING, NOTICE_LOADING_OLDER, NOTICE_RECONNECTING, NOTICE_RESYNCING, NOTICE_UNREACHABLE, notice,
    notice_ended,
};
use holzwave_core::remote::proto::{Event, Op, Request, WriterView};
use holzwave_core::server_link::Fault;
use holzwave_core::ssh::Reconnect;
use holzwave_core::textread::TextRegion;

const BOUND: usize = 4 * 1024 * 1024;

#[derive(Clone, Default)]
struct Probe {
    events: Arc<Mutex<Vec<PaneEvent>>>,
}

impl Probe {
    fn sink(&self) -> impl FnMut(PaneEvent) + Send + 'static {
        let events = self.events.clone();
        move |event| {
            if !matches!(event, PaneEvent::Frame(_)) {
                events.lock().unwrap().push(event);
            }
        }
    }

    fn held(&self) -> Vec<Option<u32>> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| if let PaneEvent::Held { code } = e { Some(*code) } else { None })
            .collect()
    }

    /// Waits for the pane to be held, closes it and returns the codes of its `Exited` events.
    fn end(&self, pane: &PaneHandle) -> Vec<Option<u32>> {
        wait_for("the pane to be held", || !self.held().is_empty());
        assert!(self.exits().is_empty(), "a held pane has not ended yet");
        pane.close();
        assert!(pane.join(PATIENCE), "closing a held pane ends it");
        self.exits()
    }

    fn exits(&self) -> Vec<Option<u32>> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| if let PaneEvent::Exited { code } = e { Some(*code) } else { None })
            .collect()
    }
}

fn hub_with(device: &FakeDevice, tune: impl FnOnce(&mut HubOptions)) -> Hub {
    let mut options = HubOptions::default();
    tune(&mut options);
    Hub::new(device.endpoint.clone(), options)
}

fn fast() -> impl FnOnce(&mut HubOptions) {
    |o| o.reconnect = Reconnect { attempts: 3, delay: Duration::from_millis(30) }
}

fn start_pane(hub: &Hub, session: u32, size: (u16, u16), tail: Option<u32>) -> (PaneHandle, Probe) {
    let mut spec = PaneSpec::new("remote", size.0, size.1);
    spec.exit_poll = None;
    // The pane is held when its session ends, so that the last screen and the notice can be read.
    spec.linger = Some(Duration::from_secs(3600));
    spec.remote = Some(RemoteTarget { session, tail, attach: Arc::new(hub.clone()) });
    let probe = Probe::default();
    let pane = PaneHandle::spawn_ghostty(1, spec, probe.sink()).expect("the pane starts");
    (pane, probe)
}

fn screen(pane: &PaneHandle) -> String {
    pane.read_text(TextRegion::LastRows(200), 1 << 20).ok().flatten().map(|r| r.text).unwrap_or_default()
}

fn viewport(pane: &PaneHandle) -> String {
    pane.read_text(TextRegion::Screen, 1 << 20).ok().flatten().map(|r| r.text).unwrap_or_default()
}

fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !check() {
        assert!(Instant::now() < deadline, "{what} did not happen in time");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_text(pane: &PaneHandle, needle: &str) {
    wait_for(&format!("the screen showing {needle:?}"), || screen(pane).contains(needle));
}

fn wait_state(pane: &PaneHandle, want: impl Fn(&ConnState) -> bool) {
    wait_for("the state", || want(&pane.conn_state()));
}

fn count(text: &str, needle: &str) -> usize {
    text.matches(needle).count()
}

fn enter(pane: &PaneHandle) {
    pane.key(DomKey { code: "Enter".into(), key: "Enter".into(), ..DomKey::default() }).expect("a key");
}

/// The state of a session on a server, as a program there made it: a terminal of its own that the scripted server snapshots.
struct Session {
    engine: GhosttyEngine,
    /// The number the next live chunk has.
    next_seq: u32,
}

impl Session {
    fn new(cols: u16, rows: u16) -> Session {
        Session { engine: GhosttyEngine::new(cols, rows, 10_000).expect("engine"), next_seq: 0 }
    }

    fn print(&mut self, text: &str) {
        self.engine.feed(text.as_bytes());
    }

    fn lines(&mut self, from: u32, to: u32) {
        for n in from..to {
            self.print(&format!("line {n}\r\n"));
        }
    }

    /// Answers an attach the way the server does: the grid, the writer view, whether older lines exist, and the snapshot of the
    /// tail that was asked for (`default_tail` when it was not) in chunks.
    fn serve(&mut self, conn: &mut Conn, request: &Request, writer: WriterView, default_tail: u32) -> u32 {
        let Op::Attach { session, tail } = request.op else { panic!("an attach was expected, got {request:?}") };
        let wanted = tail.unwrap_or(default_tail) as usize;
        let history = self.engine.scrollback_rows();
        let rows = wanted.min(history);
        let snapshot = self.engine.snapshot_vt(rows, BOUND).expect("snapshot");
        let (cols, grid_rows) = self.engine.size().expect("size");
        conn.attached(request.id, session, cols, grid_rows, writer, history > rows, snapshot.len() as u32);
        let mut seq = 0;
        for chunk in snapshot.chunks(16 * 1024) {
            conn.snapshot(session, seq, chunk);
            seq += 1;
        }
        self.next_seq = seq;
        wanted as u32
    }

    fn live(&mut self, conn: &mut Conn, session: u32, text: &str) {
        self.print(text);
        conn.live(session, self.next_seq, text.as_bytes());
        self.next_seq += 1;
    }

    /// The snapshot a new pane would be given for `tail` rows now.
    fn fresh(&mut self, tail: usize) -> Vec<u8> {
        let rows = tail.min(self.engine.scrollback_rows());
        self.engine.snapshot_vt(rows, BOUND).expect("snapshot")
    }
}

/// What the pane's own engine would snapshot, with `tail` rows.
fn pane_snapshot(pane: &PaneHandle, tail: usize) -> (Vec<u8>, (u16, u16), usize) {
    let tap = pane.attach_tap(tail, BOUND, 1 << 20).expect("tap");
    let out = (tap.snapshot.clone(), tap.size, tap.history_rows);
    pane.detach_tap(tap.id);
    out
}

/// A pane attached to a scripted server with `session` as its content; returns when the pane shows it.
fn attached_pane(
    device: &FakeDevice,
    hub: &Hub,
    id: u32,
    session: &mut Session,
    writer: WriterView,
    tail: Option<u32>,
    marker: &str,
) -> (PaneHandle, Probe, Conn) {
    let size = session.engine.size().expect("size");
    let (pane, probe) = start_pane(hub, id, size, tail);
    let mut conn = device.accept();
    let request = conn.request();
    session.serve(&mut conn, &request, writer, 100);
    wait_text(&pane, marker);
    (pane, probe, conn)
}

// ---------------------------------------------------------------------------------------------------------------------
// The connection: states, notices, reconnect
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn a_dropped_socket_shows_reconnecting_once_with_the_count_and_heals_with_a_fresh_token() {
    let device = FakeDevice::start();
    // The defaults: five tries, three seconds apart, as an SSH pane has.
    let hub = hub_with(&device, |_| {});
    let mut session = Session::new(80, 24);
    session.print("first screen$ ");
    let (pane, probe, conn) = attached_pane(&device, &hub, 4, &mut session, WriterView::Writing, None, "first screen$");
    assert_eq!(pane.conn_state(), ConnState::Connected);

    conn.cut();
    wait_state(&pane, |s| matches!(s, ConnState::Reconnecting { .. }));
    assert_eq!(pane.conn_state(), ConnState::Reconnecting { attempt: 1, of: 5, in_secs: 3 });
    wait_text(&pane, NOTICE_RECONNECTING);
    assert_eq!(count(&screen(&pane), NOTICE_RECONNECTING), 1, "once, not once per try");

    // After the pause the hub connects again with another token and attaches the pane again.
    session.print("\r\nafter the gap$ ");
    let mut conn = device.accept();
    let request = conn.request();
    session.serve(&mut conn, &request, WriterView::Writing, 100);
    wait_text(&pane, "after the gap$");
    wait_state(&pane, |s| *s == ConnState::Connected);
    let screen_now = screen(&pane);
    assert!(!screen_now.contains(NOTICE_RECONNECTING), "the fresh attach replaced the screen: {screen_now:?}");
    let seen = device.seen();
    assert_eq!(seen.len(), 2);
    assert_ne!(seen[0].authorization, seen[1].authorization, "a fresh token for every connection");
    assert!(probe.exits().is_empty(), "the pane stayed open all the time");
}

#[test]
fn when_the_tries_are_used_the_pane_fails_waits_for_enter_drops_what_is_typed_and_a_close_ends_the_wait() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let mut session = Session::new(80, 24);
    session.print("prompt$ ");
    let (pane, probe, conn) = attached_pane(&device, &hub, 4, &mut session, WriterView::Writing, None, "prompt$");
    conn.cut();
    // The server is there and does not speak: every try fails.
    device.drop_connections_for(Duration::from_millis(500));
    wait_state(&pane, |s| *s == ConnState::Failed);
    wait_text(&pane, NOTICE_UNREACHABLE);
    let text = screen(&pane);
    assert_eq!(count(&text, NOTICE_RECONNECTING), 1, "one Reconnecting line for the whole loss: {text:?}");
    assert_eq!(count(&text, NOTICE_UNREACHABLE), 1);

    // Typed bytes are dropped: nothing connects, nothing is kept.
    pane.text("abc".into()).unwrap();
    assert!(device.accept_within(Duration::from_millis(400)).is_none(), "nothing happens by itself after the tries");
    assert_eq!(pane.conn_state(), ConnState::Failed);

    // Enter is one fresh try. The server still does not speak: failed again, and it says so again.
    enter(&pane);
    device.drop_connections_for(Duration::from_millis(600));
    wait_for("a second notice", || count(&screen(&pane), NOTICE_UNREACHABLE) == 2);
    assert_eq!(pane.conn_state(), ConnState::Failed);
    // Another Enter and this time the server answers: what was typed meanwhile never arrived.
    enter(&pane);
    let mut conn = device.accept();
    let request = conn.request();
    session.print("back$ ");
    session.serve(&mut conn, &request, WriterView::Writing, 100);
    wait_text(&pane, "back$");
    assert!(!screen(&pane).contains("abc"), "typed bytes are not kept while the connection is down");
    let got = conn.drain(Duration::from_millis(200));
    assert!(!got.iter().any(|g| matches!(g, Got::Input { .. })), "{got:?}");

    // Failed again, then a close ends the wait.
    conn.cut();
    device.drop_connections_for(Duration::from_millis(500));
    wait_state(&pane, |s| *s == ConnState::Failed);
    pane.close();
    assert!(pane.join(PATIENCE), "closing the pane ends the wait");
    assert_eq!(probe.exits().len(), 1);
}

#[test]
fn a_token_the_server_no_longer_takes_is_not_tried_again_the_pane_says_so_at_once_and_enter_attaches_after_a_new_sign_in() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let mut session = Session::new(80, 24);
    session.print("prompt$ ");
    let (pane, probe, conn) = attached_pane(&device, &hub, 4, &mut session, WriterView::Writing, None, "prompt$");
    conn.close(holzwave_core::remote::proto::CLOSE_REVOKED);
    wait_state(&pane, |s| *s == ConnState::Failed);
    wait_text(&pane, Fault::SignedOut.sentence());
    assert!(device.accept_within(Duration::from_millis(500)).is_none(), "4401 is never retried");
    assert!(probe.exits().is_empty(), "the pane is still there to read");

    // The person signs in again, then presses Enter: one attach, with a token the endpoint hands out now.
    enter(&pane);
    let mut conn = device.accept();
    let request = conn.request();
    session.print("signed in again$ ");
    session.serve(&mut conn, &request, WriterView::Writing, 100);
    wait_text(&pane, "signed in again$");
    wait_state(&pane, |s| *s == ConnState::Connected);
    assert_eq!(device.endpoint.handed().len(), 2);
}

#[test]
fn a_refused_token_at_the_first_connect_is_signed_out_and_an_unknown_session_ends_the_pane_with_its_sentence() {
    // A token the server refuses.
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let (pane, probe) = start_pane(&hub, 4, (80, 24), None);
    device.refuse(401);
    wait_state(&pane, |s| *s == ConnState::Failed);
    wait_text(&pane, Fault::SignedOut.sentence());
    assert!(probe.exits().is_empty());
    pane.close();
    assert!(pane.join(PATIENCE));

    // A session the server does not know.
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let (pane, probe) = start_pane(&hub, 4, (80, 24), None);
    let mut conn = device.accept();
    let request = conn.request();
    conn.error(request.id, holzwave_core::remote::proto::ErrorWord::NotFound);
    wait_text(&pane, "That session is not on the server any more.");
    assert_eq!(probe.end(&pane), vec![Some(EXIT_GAVE_UP)]);
    assert_eq!(probe.held(), vec![Some(EXIT_GAVE_UP)]);
    assert_eq!(pane.conn_state(), ConnState::Closed);
}

// ---------------------------------------------------------------------------------------------------------------------
// The clean re-attach
// ---------------------------------------------------------------------------------------------------------------------

/// A session with a history, styled text, a program that cleared the screen, a title and a cursor that is not at home.
fn rich_session() -> Session {
    let mut session = Session::new(80, 24);
    session.lines(0, 300);
    session.print("\x1b[2J\x1b[H\x1b[31;1mcleared and redrawn\x1b[0m\r\n");
    session.lines(300, 330);
    session.print("\x1b[?2004h\x1b[10;5H\x1b]0;a title\x07marker-end");
    session
}

#[test]
fn after_a_resync_a_reconnect_and_older_lines_the_pane_holds_what_a_new_pane_would_hold() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let mut session = rich_session();
    let (pane, _probe, mut conn) = attached_pane(&device, &hub, 4, &mut session, WriterView::Writing, Some(100), "marker-end");
    assert_eq!(pane_snapshot(&pane, 1000).0, session.fresh(100), "the first attach");

    // Output the pane has already shown before the server asks for a fresh start.
    session.live(&mut conn, 4, "\r\nsome live output");
    wait_text(&pane, "some live output");

    // 1. A resync: the server says the pane was too slow. The answer is delayed so that the state can be read.
    conn.event(Event::Resync { session: 4 });
    let request = conn.request();
    assert_eq!(request.op, Op::Attach { session: 4, tail: Some(100) }, "the tail the pane last used");
    wait_for("the resyncing state", || pane.remote_view().is_some_and(|v| v.state == RemoteState::Resyncing));
    wait_text(&pane, NOTICE_RESYNCING);
    session.serve(&mut conn, &request, WriterView::Writing, 100);
    wait_for("the state to end", || pane.remote_view().is_some_and(|v| v.state == RemoteState::None));
    assert_eq!(pane_snapshot(&pane, 1000).0, session.fresh(100), "after a resync");
    assert!(!screen(&pane).contains(NOTICE_RESYNCING));

    // 2. A reconnect, after a program wrote more.
    session.print("\r\nwritten while the socket was away");
    conn.cut();
    let mut conn = device.accept();
    let request = conn.request();
    session.serve(&mut conn, &request, WriterView::Writing, 100);
    wait_text(&pane, "written while the socket was away");
    wait_state(&pane, |s| *s == ConnState::Connected);
    assert_eq!(pane_snapshot(&pane, 1000).0, session.fresh(100), "after a reconnect");

    // 3. Older lines: a longer tail, and the pane then holds what a new pane with that tail holds.
    let before = pane_snapshot(&pane, 1000).2;
    pane.load_older().unwrap();
    let request = conn.request();
    assert_eq!(request.op, Op::Attach { session: 4, tail: Some(200) }, "the tail doubles");
    session.serve(&mut conn, &request, WriterView::Writing, 100);
    wait_for("older lines", || pane_snapshot(&pane, 1000).2 > before);
    wait_for("the state to end", || pane.remote_view().is_some_and(|v| v.state == RemoteState::None));
    assert_eq!(pane_snapshot(&pane, 1000).0, session.fresh(200), "after older lines");
}

#[test]
fn a_session_that_ends_keeps_its_last_screen_and_the_notice_and_ends_the_pane_once_with_its_status() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let mut session = Session::new(80, 24);
    session.print("running$ ");
    let (pane, probe, mut conn) = attached_pane(&device, &hub, 4, &mut session, WriterView::Writing, None, "running$");
    conn.event(Event::Exited { session: 4, code: Some(3) });
    wait_text(&pane, "Session ended · exit 3");
    let text = screen(&pane);
    assert!(text.contains("running$"), "the last screen stays: {text:?}");
    assert_eq!(probe.end(&pane), vec![Some(3)], "one Exited, with the status of the program");
    assert_eq!(probe.held(), vec![Some(3)]);
    assert_eq!(pane.conn_state(), ConnState::Closed);
    // The pane told the server it left: the place is given back.
    let request = conn.request();
    assert_eq!(request.op, Op::Detach { session: 4 });
}

#[test]
fn the_words_of_every_notice_are_the_words_of_the_design_record_and_a_notice_is_plain_text() {
    let design = include_str!("../../docs/design.md");
    // The table of the states: the line on the screen is the fourth cell of each row after the header.
    let at = design.find("| State | Dot | Header context | Line on the screen | Writer label |").expect("the table of the states of a pane");
    let table: Vec<&str> = design[at..].lines().skip(2).take(6).collect();
    let cells: Vec<String> = table
        .iter()
        .map(|row| row.split('|').nth(4).expect("a fourth cell").trim().trim_matches('`').to_owned())
        .collect();
    assert_eq!(cells[0], NOTICE_ATTACHING);
    assert_eq!(cells[1], NOTICE_LOADING_OLDER);
    assert_eq!(cells[2], NOTICE_RESYNCING);
    assert_eq!(cells[3], notice_ended(Some(7)).replace('7', "N"));
    assert_eq!(cells[4], NOTICE_RECONNECTING);
    assert_eq!(cells[5], NOTICE_UNREACHABLE);

    // Plain text: control characters become spaces, so no sequence of a server can ride in a notice.
    let line = String::from_utf8(notice("a\x1b[31mb\x07\x1b]0;x\x07c\r\nd")).unwrap();
    assert_eq!(line.matches('\x1b').count(), 2, "only the two of the colour that wraps it: {line:?}");
    assert!(!line.contains('\x07'));
    assert!(line.starts_with("\r\n\x1b[33m") && line.ends_with("\x1b[0m\r\n"));
}

// ---------------------------------------------------------------------------------------------------------------------
// The writer view, the grid rule and older lines
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn the_view_is_none_for_a_local_pane_and_follows_the_events_of_the_server_in_order_with_the_bytes() {
    let local = PaneHandle::spawn_ghostty(2, PaneSpec::new("/bin/sh", 80, 24).args(["-c", "sleep 5"]), |_| {}).expect("a local pane");
    assert!(local.remote_view().is_none());
    assert!(local.takeover().is_err() && local.load_older().is_err());
    local.close();

    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let (pane, _probe) = start_pane(&hub, 4, (80, 24), None);
    let first = pane.remote_view().expect("a remote pane has a view");
    assert_eq!((first.session, first.writer, first.state), (4, WriterView::Free, RemoteState::Attaching));
    let mut conn = device.accept();
    let request = conn.request();
    let mut session = Session::new(100, 30);
    session.lines(0, 100);
    session.serve(&mut conn, &request, WriterView::Watching, 20);
    wait_for("the attach", || pane.remote_view().is_some_and(|v| v.state == RemoteState::None));
    let view = pane.remote_view().unwrap();
    assert_eq!((view.writer, view.cols, view.rows, view.more), (WriterView::Watching, 100, 30, true));
    conn.event(Event::Writer { session: 4, state: WriterView::Free });
    wait_for("free", || pane.remote_view().is_some_and(|v| v.writer == WriterView::Free));
    conn.event(Event::Writer { session: 4, state: WriterView::Writing });
    wait_for("writing", || pane.remote_view().is_some_and(|v| v.writer == WriterView::Writing));
}

#[test]
fn input_the_server_dropped_is_said_in_the_view_for_a_few_seconds_and_never_on_the_screen() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let mut session = Session::new(80, 24);
    session.print("busy$ ");
    let (pane, _probe, mut conn) = attached_pane(&device, &hub, 4, &mut session, WriterView::Writing, None, "busy$");
    let before = screen(&pane);
    conn.event(Event::Dropped { session: 4 });
    wait_for("the dropped state", || pane.remote_view().is_some_and(|v| v.state == RemoteState::InputDropped));
    assert_eq!(pane.remote_view().unwrap().writer, WriterView::Writing, "the writer view stays");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(screen(&pane), before, "the screen stays the session's: the notice is the header's");
    // An event about another session changes nothing here.
    conn.event(Event::Dropped { session: 5 });
    // The state ends by itself a few seconds after the last word of the server.
    let deadline = Instant::now() + INPUT_DROPPED_FOR + Duration::from_secs(5);
    while pane.remote_view().is_some_and(|v| v.state == RemoteState::InputDropped) {
        assert!(Instant::now() < deadline, "the dropped state never ended");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(pane.remote_view().unwrap().state, RemoteState::None);
}

#[test]
fn a_watcher_drops_what_is_typed_before_it_is_sent_and_counts_it_and_a_writer_sends_it() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let mut session = Session::new(80, 24);
    session.print("watching$ ");
    let (pane, _probe, mut conn) = attached_pane(&device, &hub, 4, &mut session, WriterView::Watching, None, "watching$");
    pane.text("rm -rf /\r".into()).unwrap();
    wait_for("the count", || pane.remote_view().is_some_and(|v| v.dropped_input >= 9));
    assert_eq!(pane.remote_view().unwrap().dropped_input, 9);
    assert!(conn.drain(Duration::from_millis(300)).is_empty(), "nothing was sent");
    assert_eq!(hub.stats().input_frames, 0);

    conn.event(Event::Writer { session: 4, state: WriterView::Writing });
    wait_for("writing", || pane.remote_view().is_some_and(|v| v.writer == WriterView::Writing));
    pane.text("ls\r".into()).unwrap();
    let got = conn.drain(Duration::from_millis(500));
    assert_eq!(got, vec![Got::Input { session: 4, bytes: b"ls\r".to_vec() }]);
}

#[test]
fn a_remote_pane_never_answers_the_programs_queries_because_the_sessions_engine_on_the_server_does() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let mut session = Session::new(80, 24);
    session.print("asking$ ");
    let (pane, _probe, mut conn) = attached_pane(&device, &hub, 4, &mut session, WriterView::Writing, None, "asking$");
    // Cursor position, primary and secondary device attributes, the colour of the text, the kitty keyboard flags and the
    // version: every one of them a query a local pane's engine answers.
    session.live(&mut conn, 4, "\x1b[6n\x1b[c\x1b[>c\x1b]10;?\x1b\\\x1b[?u\x1b[>0q");
    session.live(&mut conn, 4, "asked$ ");
    wait_text(&pane, "asked$");
    assert!(conn.drain(Duration::from_millis(300)).is_empty(), "a pane that writes sent an answer to the program");
    assert_eq!(hub.stats().input_frames, 0);
    // What is typed is still sent.
    pane.text("ls\r".into()).unwrap();
    assert_eq!(conn.drain(Duration::from_millis(500)), vec![Got::Input { session: 4, bytes: b"ls\r".to_vec() }]);

    // A pane that watches does not count the answers it never sends as dropped typing.
    conn.event(Event::Writer { session: 4, state: WriterView::Watching });
    wait_for("watching", || pane.remote_view().is_some_and(|v| v.writer == WriterView::Watching));
    session.live(&mut conn, 4, "\x1b[6n\x1b[c watched$ ");
    wait_text(&pane, "watched$");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(pane.remote_view().unwrap().dropped_input, 0);
    assert!(conn.drain(Duration::from_millis(300)).is_empty());
}

#[test]
fn the_writers_size_is_the_sessions_a_watcher_follows_and_taking_over_sends_the_new_writers_size_once() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    // The session is 120 by 40; this pane's tile is smaller. It watches.
    let mut session = Session::new(120, 40);
    session.print("big session$ ");
    let (pane, _probe) = start_pane(&hub, 4, (50, 10), None);
    let mut conn = device.accept();
    let request = conn.request();
    session.serve(&mut conn, &request, WriterView::Watching, 100);
    wait_text(&pane, "big session$");
    // The engine follows the session's grid, not the tile.
    assert_eq!(pane_snapshot(&pane, 0).1, (120, 40));
    assert_eq!(pane.remote_view().map(|v| (v.cols, v.rows)), Some((120, 40)));

    // A watcher's own tile resize changes neither its engine nor the session.
    pane.resize(60, 12, (8, 16)).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(pane_snapshot(&pane, 0).1, (120, 40));
    assert!(conn.drain(Duration::from_millis(200)).is_empty(), "no resize was asked of the server");

    // The session changes size (the writer elsewhere resized): the watcher follows.
    conn.event(Event::Size { session: 4, cols: 90, rows: 30 });
    wait_for("the watcher to follow", || pane_snapshot(&pane, 0).1 == (90, 30));

    // Taking over: the server is asked, and when the right moves the pane sends its own tile size once.
    pane.takeover().unwrap();
    let request = conn.request();
    assert_eq!(request.op, Op::Takeover { session: 4 });
    conn.answer(request.id, holzwave_core::remote::proto::Answer::Done);
    conn.event(Event::Writer { session: 4, state: WriterView::Writing });
    let request = conn.request();
    assert_eq!(request.op, Op::Resize { session: 4, cols: 60, rows: 12 }, "the recorded tile, once");
    conn.answer(request.id, holzwave_core::remote::proto::Answer::Done);
    conn.event(Event::Size { session: 4, cols: 60, rows: 12 });
    wait_for("the engine to follow the new size", || pane_snapshot(&pane, 0).1 == (60, 12));
    assert!(conn.request_within(Duration::from_millis(300)).is_none(), "the size is sent once");

    // As the writer, a resize of the tile is sent.
    pane.resize(70, 20, (8, 16)).unwrap();
    let request = conn.request();
    assert_eq!(request.op, Op::Resize { session: 4, cols: 70, rows: 20 });
    conn.answer(request.id, holzwave_core::remote::proto::Answer::Done);
    conn.event(Event::Size { session: 4, cols: 70, rows: 20 });
    wait_for("the engine", || pane_snapshot(&pane, 0).1 == (70, 20));
    // The size the server confirms is not sent back.
    assert!(conn.request_within(Duration::from_millis(300)).is_none());
}

#[test]
fn a_size_that_changes_while_output_is_still_queued_is_applied_after_that_output_as_on_the_server() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let mut session = Session::new(120, 40);
    session.print("wide$ ");
    let (pane, _probe, mut conn) = attached_pane(&device, &hub, 4, &mut session, WriterView::Watching, None, "wide$");
    // A flood the pane is still feeding when the size changes, and at its end text placed where only the old width has room.
    for n in 0..40 {
        let chunk: String = (0..200).map(|i| format!("{n:02} filler line {i:04} of the flood at the old size\r\n")).collect();
        session.live(&mut conn, 4, &chunk);
    }
    session.live(&mut conn, 4, "\x1b[5;100Hfar-right\x1b[20;1Hthe end");
    conn.event(Event::Size { session: 4, cols: 80, rows: 40 });
    session.engine.resize(80, 40, (0, 0)).expect("the session's engine resizes");
    wait_for("the engine to follow", || pane_snapshot(&pane, 0).1 == (80, 40));
    wait_text(&pane, "the end");
    assert_eq!(pane_snapshot(&pane, 0).0, session.fresh(0), "the pane's screen is the session's");
}

#[test]
fn a_pane_that_writes_with_a_tile_that_is_not_the_sessions_size_sends_its_size_when_it_attaches() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let mut session = Session::new(80, 24);
    session.print("created small$ ");
    let (pane, _probe) = start_pane(&hub, 4, (100, 30), None);
    let mut conn = device.accept();
    let request = conn.request();
    session.serve(&mut conn, &request, WriterView::Writing, 100);
    let request = conn.request();
    assert_eq!(request.op, Op::Resize { session: 4, cols: 100, rows: 30 });
    wait_text(&pane, "created small$");
}

#[test]
fn older_lines_double_the_tail_keep_the_viewport_where_it_was_and_stop_when_the_server_has_no_more() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let mut session = Session::new(80, 24);
    session.lines(0, 600);
    session.print("end of output");
    let (pane, _probe, mut conn) = attached_pane(&device, &hub, 4, &mut session, WriterView::Watching, Some(100), "end of output");
    assert!(pane.remote_view().unwrap().more);

    // The person has scrolled up; the viewport shows other lines than the last ones.
    pane.scroll(-40).unwrap();
    wait_for("the viewport to move", || !viewport(&pane).contains("end of output"));
    let before = viewport(&pane);
    assert!(before.contains("line "), "{before:?}");

    pane.load_older().unwrap();
    // A second call while the request is on its way does nothing.
    pane.load_older().unwrap();
    let request = conn.request();
    assert_eq!(request.op, Op::Attach { session: 4, tail: Some(200) });
    wait_for("the loading state", || pane.remote_view().is_some_and(|v| v.state == RemoteState::LoadingOlder));
    wait_text(&pane, NOTICE_LOADING_OLDER);
    session.serve(&mut conn, &request, WriterView::Watching, 100);
    wait_for("more history", || pane_snapshot(&pane, 1000).2 >= 200);
    wait_for("the state to end", || pane.remote_view().is_some_and(|v| v.state == RemoteState::None));
    assert!(conn.request_within(Duration::from_millis(300)).is_none(), "only one request was made");
    // The same lines are in view as before: the distance from the bottom did not change.
    wait_for("the viewport to be put back", || viewport(&pane) == before);

    // Ask until the server says there is nothing older: the view says so, and a further call does nothing.
    let mut tail = 200;
    while pane.remote_view().unwrap().more {
        pane.load_older().unwrap();
        let request = conn.request();
        tail *= 2;
        assert_eq!(request.op, Op::Attach { session: 4, tail: Some(tail) });
        session.serve(&mut conn, &request, WriterView::Watching, 100);
        wait_for("the state to end", || pane.remote_view().is_some_and(|v| v.state == RemoteState::None));
    }
    assert!(tail >= 800, "{tail}");
    pane.load_older().unwrap();
    assert!(conn.request_within(Duration::from_millis(300)).is_none(), "nothing older: a call does nothing");
}

#[test]
fn closing_a_remote_pane_detaches_and_never_ends_the_session() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, fast());
    let mut session = Session::new(80, 24);
    session.print("watching$ ");
    let (pane, probe, mut conn) = attached_pane(&device, &hub, 4, &mut session, WriterView::Watching, None, "watching$");
    pane.notice("still a pane").unwrap();
    wait_text(&pane, "still a pane");
    pane.close();
    assert!(pane.join(PATIENCE));
    assert_eq!(probe.exits().len(), 1);
    let request = conn.request();
    assert_eq!(request.op, Op::Detach { session: 4 }, "the pane left");
    let got = conn.drain(Duration::from_millis(300));
    assert!(!got.iter().any(|g| matches!(g, Got::Request(Request { op: Op::End { .. }, .. }))), "{got:?}");
}

#[test]
fn a_hostile_server_ends_the_attachment_with_the_fixed_sentence_and_nothing_else() {
    for (cols, rows, snapshot) in [(0u16, 24u16, 0u32), (65535, 24, 0), (80, 65535, 0), (80, 24, u32::MAX)] {
        let device = FakeDevice::start();
        let hub = hub_with(&device, fast());
        let (pane, probe) = start_pane(&hub, 4, (80, 24), None);
        let mut conn = device.accept();
        let request = conn.request();
        conn.attached(request.id, 4, cols, rows, WriterView::Writing, false, snapshot);
        wait_text(&pane, Fault::Protocol.sentence());
        let text = screen(&pane);
        assert_eq!(probe.end(&pane), vec![Some(EXIT_GAVE_UP)]);
        assert!(!text.contains("hello") && text.lines().count() < 10, "{text:?}");
    }
}
