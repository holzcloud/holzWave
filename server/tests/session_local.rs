//! The worker's own window on its sessions: a pane of the browser's window attached in-process to the worker's session host,
//! with real programs on real PTYs and no network.
//!
//! A real `AppCore` with the worker's host answer (`Host::sessions`, the in-process service) makes, opens, drives and closes
//! a session as a pane of its window, and panes built on the same service the way the core builds them (a `PaneSpec` whose
//! remote target is the service's) show one writer, the grid rule, older lines, a fresh start after falling behind and the
//! end of a program. A device stands next to them through the worker's own adapter of the device socket (`handle_ses`, the
//! function the lane runs for every request of a device), so both kinds of client meet in the one session host. The host
//! runs on a clock the test moves, so the life of a window pane against the 30 s silence rule is read at the millisecond.
//! The tracer test comes first; the filter `tracer` selects exactly it.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use holzwave_core::conn::{ConnState, RemoteState};
use holzwave_core::connections::ConnView;
use holzwave_core::layout::{MAIN_WINDOW, PaneId};
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
use holzwave_core::remote::proto::WriterView;
use holzwave_core::secrets::{MemoryStore, SecretStore};
use holzwave_core::textread::TextRegion;
use holzwave_host::local_network::{Source, Unsupported};
use holzwave_host::seams::{Host, HostLimits, Platform};
use holzwave_host::sessions::SessionService;
use holzwave_host::state::{AppCore, Out, ShellSpec, Sink, Target, View};
use holzwave_host::windows::{CursorSpot, Place};
use holzwave_server::wire::{Ctl, Frame, Ses, SesAnswer, SesEvent};
use holzwave_server::worker::dispatch::{Outbound, QueueLimits};
use holzwave_server::worker::lease::WriterState;
use holzwave_server::worker::local_attach::{BrowserSockets, LOCAL_CLIENT_BASE, LocalOptions, LocalSessions, THIS_SERVER};
use holzwave_server::worker::runtime::{CoreSpawner, handle_ses};
use holzwave_server::worker::sessions::{Attached, Clock, HostSettings, SessionHost};
use serde_json::json;

/// How long a test waits for a shell or a screen.
const PATIENCE: Duration = Duration::from_secs(20);
/// The beat interval of the device client, in milliseconds of the manual clock.
const BEAT_MS: u64 = 10_000;
/// The silence that frees the right to write, in milliseconds.
const SILENCE_MS: u64 = 30_000;

// The fixed sentences, written out here so that a change of one is seen.
const NOT_FOUND: &str = "That session is not on the server any more.";

// ---------------------------------------------------------------------------------------------------------------------
// The rig
// ---------------------------------------------------------------------------------------------------------------------

/// A clock that stands still until a test moves it.
struct ManualClock(AtomicU64);

impl ManualClock {
    fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }

    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now()
    }
}

/// What the page of the window would show: the rows of every pane, built from its frames.
#[derive(Default)]
struct Screens {
    rows: Mutex<HashMap<PaneId, Vec<String>>>,
}

impl Sink for Screens {
    fn send(&self, _target: Target, out: Out) {
        if let Out::Frame(frame) = out {
            let mut all = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
            let rows = all.entry(frame.pane).or_default();
            if frame.full {
                rows.clear();
            }
            rows.resize(usize::from(frame.rows), String::new());
            for row in &frame.changed {
                if let Some(slot) = rows.get_mut(usize::from(row.y)) {
                    *slot = row.runs.iter().flat_map(|run| run.0.chars()).filter(|c| *c != '\0').collect();
                }
            }
        }
    }

    fn close_window(&self, _label: &str) {}

    fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
        Ok(())
    }

    fn cursor_spot(&self, _source: &str) -> CursorSpot {
        CursorSpot::Unknown
    }
}

impl Screens {
    fn text(&self, pane: PaneId) -> String {
        let all = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        let rows = all.get(&pane).cloned().unwrap_or_default();
        let used = rows.iter().rposition(|row| !row.trim().is_empty()).map_or(0, |at| at + 1);
        rows[..used].iter().map(|row| row.trim_end()).collect::<Vec<&str>>().join("\n")
    }
}

/// The worker's host as the tests need it: the web platform, a store in memory, the cap of the test and the in-process
/// sessions, which is the one answer this plan adds to the worker's host.
struct WorkerHost {
    store: Arc<MemoryStore>,
    max_panes: Option<usize>,
    sessions: Arc<LocalSessions>,
}

impl Host for WorkerHost {
    fn platform(&self) -> Platform {
        Platform::Web
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        self.store.clone()
    }

    fn local_network(&self) -> Arc<dyn Source> {
        Arc::new(Unsupported)
    }

    fn text_scale(&self) -> u32 {
        100
    }

    fn limits(&self) -> HostLimits {
        HostLimits { max_panes: self.max_panes, ..HostLimits::default() }
    }

    fn local_processes(&self) -> bool {
        false
    }

    fn sessions(&self) -> Option<Arc<dyn SessionService>> {
        Some(self.sessions.clone())
    }
}

/// One person's worker: the core of the window, its session host on a manual clock, the in-process service and the browser
/// sockets the supervisor announced.
struct World {
    dir: PathBuf,
    core: Arc<AppCore>,
    screens: Arc<Screens>,
    host: Arc<SessionHost>,
    local: Arc<LocalSessions>,
    browsers: Arc<BrowserSockets>,
    clock: Arc<ManualClock>,
}

impl World {
    fn start(tag: &str, max_panes: Option<usize>, options: LocalOptions) -> World {
        let dir = std::env::temp_dir().join(format!("hw-session-local-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch folder");
        let browsers = BrowserSockets::new();
        let local = LocalSessions::new(browsers.clone(), options);
        let screens = Arc::new(Screens::default());
        let host = Arc::new(WorkerHost { store: Arc::new(MemoryStore::default()), max_panes, sessions: local.clone() });
        let core = AppCore::new_with_host(dir.clone(), screens.clone(), ShellSpec::new("/bin/sh", &[]), None, None, host);
        let clock = Arc::new(ManualClock(AtomicU64::new(0)));
        let sessions = Arc::new(SessionHost::start(Arc::new(CoreSpawner::new(core.clone())), HostSettings::default(), clock.clone()));
        local.bind(&sessions);
        World { dir, core, screens, host: sessions, local, browsers, clock }
    }

    fn view(&self) -> View {
        self.core.view_in(MAIN_WINDOW).expect("the main window")
    }

    /// A new session shown in a new tab of the window, as `New session` makes it.
    fn new_session(&self) -> (PaneId, u32) {
        let view = self.core.server_session_new_in(MAIN_WINDOW).expect("a new session");
        let pane = pane_of(&view);
        let rows = self.core.server_sessions().expect("the list");
        let session = rows.last().expect("the new session is listed").id;
        (pane, session)
    }

    fn start_pane(&self, pane: PaneId, cols: u32, rows: u32) {
        self.core.start_pane(pane, cols, rows, 8, 16).expect("the pane starts");
    }

    fn typed(&self, pane: PaneId, text: &str) {
        self.core.text(pane, text.to_owned()).expect("typed");
    }

    fn screen(&self, pane: PaneId) -> String {
        self.screens.text(pane)
    }

    fn conn(&self, pane: PaneId) -> Option<ConnView> {
        self.core.pane_connections().into_iter().find(|view| view.pane == pane)
    }

    fn writer(&self, pane: PaneId) -> &'static str {
        self.conn(pane).and_then(|view| view.remote).map_or("", |remote| remote.writer)
    }

    fn until_screen(&self, pane: PaneId, what: &str, check: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let text = self.screen(pane);
            if check(&text) {
                return;
            }
            assert!(Instant::now() < deadline, "{what} did not happen in time; the screen: {text:?}; the line: {:?}", self.conn(pane));
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn until_prompt(&self, pane: PaneId) {
        until("the attach", PATIENCE, || {
            self.conn(pane).is_some_and(|v| v.state == "connected" && v.remote.is_some_and(|r| r.phase.is_empty()))
        });
        self.until_screen(pane, "a prompt", |text| prompt(text));
    }

    fn until_line(&self, pane: PaneId, line: &str) {
        self.until_screen(pane, line, |text| has_line(text, line));
    }

    /// The size of `session` in the host's list.
    fn grid(&self, session: u32) -> Option<(u16, u16)> {
        self.host.list().into_iter().find(|info| info.id == session).map(|info| (info.cols, info.rows))
    }

    /// Moves the manual clock to `ms`, beats as the service's thread would, and lets the host free what is due.
    fn at(&self, ms: u64) {
        self.clock.set(ms);
        self.local.tick();
        self.host.sweep();
    }

    /// A pane of the kind the core starts for a session of this worker: its spec names the service's target.
    fn pane_on(&self, id: PaneId, session: u32, size: (u16, u16), tail: Option<u32>, probe: &Probe) -> PaneHandle {
        let mut target = self.local.target(session).expect("a target");
        target.tail = tail;
        let mut spec = PaneSpec::new("remote", size.0, size.1);
        spec.exit_poll = None;
        // The pane is held when its session ends, so that the last screen and the notice can be read.
        spec.linger = Some(Duration::from_secs(3600));
        spec.remote = Some(target);
        PaneHandle::spawn_ghostty(id, spec, probe.sink()).expect("the pane starts")
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.host.shutdown();
        self.core.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What a pane told besides its frames, and a switch that makes its engine thread wait inside a frame, as a page that does not
/// draw would: the pane then stops reading its subscription.
#[derive(Clone, Default)]
struct Probe {
    events: Arc<Mutex<Vec<PaneEvent>>>,
    hold: Arc<(Mutex<bool>, Condvar)>,
}

impl Probe {
    fn sink(&self) -> impl FnMut(PaneEvent) + Send + 'static {
        let (events, hold) = (self.events.clone(), self.hold.clone());
        move |event| {
            if matches!(event, PaneEvent::Frame(_)) {
                let (held, wake) = &*hold;
                let mut held = held.lock().unwrap_or_else(PoisonError::into_inner);
                while *held {
                    held = wake.wait(held).unwrap_or_else(PoisonError::into_inner);
                }
            } else {
                events.lock().unwrap_or_else(PoisonError::into_inner).push(event);
            }
        }
    }

    fn set_hold(&self, on: bool) {
        let (held, wake) = &*self.hold;
        *held.lock().unwrap_or_else(PoisonError::into_inner) = on;
        wake.notify_all();
    }

    fn held(&self) -> Vec<Option<u32>> {
        let events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        events.iter().filter_map(|e| if let PaneEvent::Held { code } = e { Some(*code) } else { None }).collect()
    }

    fn exited(&self) -> Vec<Option<u32>> {
        let events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        events.iter().filter_map(|e| if let PaneEvent::Exited { code } = e { Some(*code) } else { None }).collect()
    }
}

fn pane_text(pane: &PaneHandle) -> String {
    pane.read_text(TextRegion::LastRows(200), 1 << 20).ok().flatten().map(|r| r.text).unwrap_or_default()
}

fn pane_writer(pane: &PaneHandle) -> Option<WriterView> {
    pane.remote_view().map(|view| view.writer)
}

fn until_pane(pane: &PaneHandle, what: &str, check: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let text = pane_text(pane);
        if check(&text) {
            return;
        }
        assert!(Instant::now() < deadline, "{what} did not happen in time; the screen: {text:?}; the view: {:?}", pane.remote_view());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What the pane's own engine would snapshot, with `tail` rows: the screen, its size and the rows of history it holds.
fn pane_snapshot(pane: &PaneHandle, tail: usize) -> (Vec<u8>, (u16, u16), usize) {
    let tap = pane.attach_tap(tail, 64 << 20, 1 << 20).expect("tap");
    let out = (tap.snapshot.clone(), tap.size, tap.history_rows);
    pane.detach_tap(tap.id);
    out
}

/// A device next to the window: requests go through the worker's adapter of the device socket, and the frames it would send
/// to the socket are collected here.
struct Device {
    socket: u32,
    queue: Arc<Outbound>,
    frames: Arc<Mutex<Vec<Frame>>>,
    reader: Option<JoinHandle<()>>,
    next: u64,
}

impl Device {
    fn new(socket: u32) -> Device {
        let queue = Outbound::new(QueueLimits::default());
        let frames = Arc::new(Mutex::new(Vec::new()));
        let reader = {
            let (queue, frames) = (queue.clone(), frames.clone());
            std::thread::spawn(move || {
                while let Some((frame, _)) = queue.pop_wait() {
                    frames.lock().unwrap_or_else(PoisonError::into_inner).push(frame);
                }
            })
        };
        Device { socket, queue, frames, reader: Some(reader), next: 1 }
    }

    fn frames(&self) -> Vec<Frame> {
        self.frames.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Sends a request and waits for its answer.
    fn ask(&mut self, host: &SessionHost, make: impl FnOnce(u32, u64) -> Ses) -> SesAnswer {
        let request = self.next;
        self.next += 1;
        handle_ses(host, &self.queue, make(self.socket, request));
        let deadline = Instant::now() + PATIENCE;
        loop {
            let found = self.frames().into_iter().find_map(|frame| match frame {
                Frame::Ses(Ses::Answer { request: r, answer, .. }) if r == request => Some(answer),
                _ => None,
            });
            if let Some(answer) = found {
                return answer;
            }
            assert!(Instant::now() < deadline, "no answer to request {request}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn attach(&mut self, host: &SessionHost, session: u32, tail: Option<u32>) -> Attached {
        match self.ask(host, |socket, request| Ses::Attach { socket, request, session, tail }) {
            SesAnswer::Attached(attached) => attached,
            other => panic!("not attached: {other:?}"),
        }
    }

    /// The snapshot of a fresh attach with `tail` rows: exactly what a new pane with that tail is given.
    fn fresh(&mut self, host: &SessionHost, session: u32, tail: u32) -> Vec<u8> {
        let before = self.frames().len();
        let attached = self.attach(host, session, Some(tail));
        let want = attached.snapshot_bytes as usize;
        let deadline = Instant::now() + PATIENCE;
        loop {
            let mut bytes = Vec::new();
            let mut seen_answer = false;
            for frame in self.frames().into_iter().skip(before) {
                match frame {
                    Frame::Ses(Ses::Answer { answer: SesAnswer::Attached(_), .. }) => {
                        seen_answer = true;
                        bytes.clear();
                    }
                    Frame::Ses(Ses::Snapshot { bytes: chunk, .. }) if seen_answer => bytes.extend_from_slice(&chunk),
                    _ => {}
                }
            }
            if bytes.len() >= want {
                return bytes;
            }
            assert!(Instant::now() < deadline, "the snapshot did not arrive");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn input(&self, host: &SessionHost, session: u32, text: &str) {
        handle_ses(host, &self.queue, Ses::Input { socket: self.socket, session, bytes: text.as_bytes().to_vec() });
    }

    fn takeover(&mut self, host: &SessionHost, session: u32) {
        assert_eq!(self.ask(host, |socket, request| Ses::Takeover { socket, request, session }), SesAnswer::Done);
    }

    fn writers(&self) -> Vec<WriterState> {
        self.events().into_iter().filter_map(|e| if let SesEvent::Writer(w) = e { Some(w) } else { None }).collect()
    }

    fn sizes(&self) -> Vec<(u16, u16)> {
        self.events().into_iter().filter_map(|e| if let SesEvent::Size { cols, rows } = e { Some((cols, rows)) } else { None }).collect()
    }

    fn events(&self) -> Vec<SesEvent> {
        self.frames()
            .into_iter()
            .filter_map(|frame| match frame {
                Frame::Ses(Ses::Event { event, .. }) => Some(event),
                _ => None,
            })
            .collect()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.queue.close();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn prompt(text: &str) -> bool {
    let last = text.trim_end();
    last.ends_with('$') || last.ends_with('#') || last.ends_with('%')
}

fn has_line(text: &str, line: &str) -> bool {
    text.lines().any(|l| l.trim() == line)
}

fn until(what: &str, within: Duration, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !check() {
        assert!(Instant::now() < deadline, "{what} did not happen in time");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The one pane a view of a new tab shows.
fn pane_of(view: &View) -> PaneId {
    assert_eq!(view.panes.len(), 1, "a new tab has one pane");
    view.focus
}

// ---------------------------------------------------------------------------------------------------------------------
// The tracer
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn tracer_a_window_pane_shows_a_session_of_its_own_worker_and_closing_it_only_detaches() {
    let w = World::start("tracer", None, LocalOptions::default());
    assert_eq!(w.core.server_sessions(), Ok(Vec::new()));
    let tabs = w.view().tabs.len();

    let (pane, session) = w.new_session();
    assert_eq!(w.view().tabs.len(), tabs + 1, "one more tab");
    let rows = w.core.server_sessions().expect("the list");
    assert_eq!((rows.len(), rows[0].id, rows[0].held), (1, session, true), "the pane's claim holds the right to write");

    // The page starts the pane: the shell's prompt is in its frames and what is typed reaches the program.
    w.start_pane(pane, 80, 24);
    w.until_prompt(pane);
    w.typed(pane, "echo hw-$((6*7))\r");
    w.until_line(pane, "hw-42");

    // What the header, the sidebar and the status line are told.
    let line = w.conn(pane).expect("the pane has a line");
    assert_eq!(
        serde_json::to_value(&line).unwrap(),
        json!({
            "pane": pane, "profile": "", "name": THIS_SERVER, "state": "connected", "attempt": 0, "of": 0, "secs": 0,
            "remote": { "writer": "writing", "session": session, "cols": 80, "rows": 24, "more": false, "phase": "" }
        })
    );
    let rows = w.core.server_sessions().expect("the list");
    assert_eq!((rows[0].attached, rows[0].held), (1, true));
    assert_eq!(w.local.attached_panes(), 1);

    // Closing the pane detaches: the right to write is free at once and the session stays in the list.
    w.core.close_pane_in(MAIN_WINDOW, pane).expect("the pane closes");
    until("the detach", PATIENCE, || w.host.list().first().is_some_and(|info| info.attached == 0 && !info.writer_held));
    assert_eq!(w.local.attached_panes(), 0);
    assert_eq!(w.core.server_sessions().expect("the list").iter().map(|row| row.id).collect::<Vec<_>>(), vec![session]);
}

// ---------------------------------------------------------------------------------------------------------------------
// One writer across the window and the devices
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn a_device_watches_takes_over_and_gives_back_and_the_silence_frees_the_right_for_both() {
    let w = World::start("one-writer", None, LocalOptions::default());
    let (pane, session) = w.new_session();
    w.start_pane(pane, 80, 24);
    w.until_prompt(pane);

    let mut device = Device::new(1);
    let attached = device.attach(&w.host, session, None);
    assert_eq!(attached.writer, WriterState::Watching, "the window pane writes, the device watches");

    // The watcher's input reaches nothing; the writer's does.
    device.input(&w.host, session, "echo dev-$((2*3))\r");
    w.typed(pane, "echo pane-$((1+1))\r");
    w.until_line(pane, "pane-2");
    assert!(!has_line(&w.screen(pane), "dev-6"), "the device's input was dropped");

    // The device takes over: the pane watches within a second and what it types is dropped.
    device.takeover(&w.host, session);
    until("the pane watching", Duration::from_secs(1), || w.writer(pane) == "watching");
    w.typed(pane, "echo lost-$((3*3))\r");
    device.input(&w.host, session, "echo dev-$((4*4))\r");
    w.until_line(pane, "dev-16");
    assert!(!has_line(&w.screen(pane), "lost-9"), "the pane's input was dropped while it watched");
    assert!(w.conn(pane).is_some_and(|v| v.state == "connected"));

    // The pane takes it back, and the device hears both changes in order.
    w.core.pane_takeover(pane).expect("a takeover");
    until("the pane writing", Duration::from_secs(1), || w.writer(pane) == "writing");
    until("the device told", PATIENCE, || device.writers().len() == 2);
    assert_eq!(device.writers(), vec![WriterState::Writing, WriterState::Watching]);

    // No browser socket is attached, so nobody beats for the pane: the silence frees the right for both.
    w.at(SILENCE_MS - 1);
    assert_eq!(w.writer(pane), "writing");
    w.at(SILENCE_MS);
    until("the pane free", PATIENCE, || w.writer(pane) == "free");
    until("the device free", PATIENCE, || device.writers().last() == Some(&WriterState::Free));
}

#[test]
fn the_writer_pane_sets_the_grid_a_device_follows_and_a_watching_tile_changes_nothing() {
    let w = World::start("grid", None, LocalOptions::default());
    let (pane, session) = w.new_session();
    w.start_pane(pane, 100, 30);
    w.until_prompt(pane);
    until("the session takes the writer's tile", PATIENCE, || w.grid(session) == Some((100, 30)));

    let mut device = Device::new(1);
    let attached = device.attach(&w.host, session, None);
    assert_eq!((attached.cols, attached.rows), (100, 30), "the device sets its grid to the session's");

    // The device writes and sets the size; the pane follows the size events.
    device.takeover(&w.host, session);
    let answer = device.ask(&w.host, |socket, request| Ses::Resize { socket, request, session, cols: 90, rows: 20 });
    assert_eq!(answer, SesAnswer::Done);
    until("the pane follows", PATIENCE, || w.conn(pane).and_then(|v| v.remote).is_some_and(|r| (r.cols, r.rows) == (90, 20)));

    // A watching pane's own tile changes nothing.
    w.core.resize(pane, 70, 25, 8, 16).expect("the tile changes");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(w.grid(session), Some((90, 20)));
    assert_eq!(device.sizes(), vec![(90, 20)]);

    // Taking over sends the pane's tile once, and the device follows.
    w.core.pane_takeover(pane).expect("a takeover");
    until("the session takes the new writer's tile", PATIENCE, || w.grid(session) == Some((70, 25)));
    until("the device follows", PATIENCE, || device.sizes().last() == Some(&(70, 25)));
}

// ---------------------------------------------------------------------------------------------------------------------
// Older lines, a fresh start after falling behind, the end of a program
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn older_lines_leave_the_pane_as_a_fresh_attach_would_and_a_slow_pane_starts_afresh_and_the_end_is_told_once() {
    let w = World::start("older", None, LocalOptions { queue_bytes: 64 * 1024, ..LocalOptions::default() });
    let session = w.local.create(80, 24).expect("a session");
    let probe = Probe::default();
    let pane = w.pane_on(1, session, (80, 24), None, &probe);
    until_pane(&pane, "a prompt", prompt);
    assert_eq!(pane_writer(&pane), Some(WriterView::Writing), "the pane the session was made for writes");
    pane.text("seq 1 1500\r".into()).expect("typed");
    until_pane(&pane, "the numbers", |text| text.contains("1500") && prompt(text));

    // A pane that attaches now gets the first tail of a longer history: it says there is more, and older lines are a
    // longer tail that leaves it holding exactly what a new pane with that tail holds.
    let late_probe = Probe::default();
    let late = w.pane_on(2, session, (80, 24), None, &late_probe);
    until("the late pane attached", PATIENCE, || late.remote_view().is_some_and(|v| v.state == RemoteState::None && v.more));
    assert_eq!(pane_writer(&late), Some(WriterView::Watching));
    let before = pane_snapshot(&late, 100_000).2;
    late.load_older().expect("older lines");
    until("older lines", PATIENCE, || {
        late.remote_view().is_some_and(|v| v.state == RemoteState::None) && pane_snapshot(&late, 100_000).2 > before
    });
    // The session is idle, so a fresh attach is a fixed picture; the pane is fed its snapshot after the answer, so it is read
    // until it holds the whole of it.
    let mut device = Device::new(1);
    let fresh = device.fresh(&w.host, session, 2000);
    until("the pane holds what a new pane with that tail holds", PATIENCE, || pane_snapshot(&late, 100_000).0 == fresh);

    // The late pane's page stops drawing, so the pane stops reading: it falls behind, is told to start afresh and recovers.
    late_probe.set_hold(true);
    pane.text("seq 1 300000\r".into()).expect("typed");
    until("the slow pane loses its stream", Duration::from_secs(60), || w.local.resyncs() >= 1);
    late_probe.set_hold(false);
    until_pane(&pane, "the end of the numbers", |text| text.contains("300000") && prompt(text));
    until_pane(&late, "the slow pane caught up", |text| text.contains("300000") && prompt(text));
    until("the slow pane settled", PATIENCE, || late.remote_view().is_some_and(|v| v.state == RemoteState::None));
    assert_eq!(
        late.read_text(TextRegion::Screen, 1 << 20).ok().flatten().map(|r| r.text),
        pane.read_text(TextRegion::Screen, 1 << 20).ok().flatten().map(|r| r.text),
        "both panes show the same screen"
    );

    // The program ends: each pane is told its status once.
    pane.text("exit 3\r".into()).expect("typed");
    until("the end", PATIENCE, || probe.held() == vec![Some(3)] && late_probe.held() == vec![Some(3)]);
    pane.close();
    late.close();
    until("the panes end", PATIENCE, || probe.exited().len() == 1 && late_probe.exited().len() == 1);
    assert_eq!((probe.held(), probe.exited()), (vec![Some(3)], vec![Some(3)]));
}

// ---------------------------------------------------------------------------------------------------------------------
// Persons, caps and identities
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn the_sessions_are_the_workers_own_and_sessions_count_with_the_panes_against_the_cap() {
    let alice = World::start("alice", None, LocalOptions::default());
    let bob = World::start("bob", None, LocalOptions::default());
    let (_, bobs) = bob.new_session();
    // Alice's worker has never made a session: Bob's number is not hers to name.
    let tabs = alice.view().tabs.len();
    assert_eq!(alice.core.server_session_open_in(MAIN_WINDOW, bobs).err().as_deref(), Some(NOT_FOUND));
    assert_eq!(alice.view().tabs.len(), tabs, "no tab is left behind");
    assert_eq!(alice.core.server_sessions(), Ok(Vec::new()));
    // A pane that names it anyway hears the same sentence and ends.
    let probe = Probe::default();
    let stray = alice.pane_on(9, bobs, (80, 24), None, &probe);
    until_pane(&stray, "the refusal", |text| text.contains(NOT_FOUND));
    stray.close();
    assert_eq!(bob.core.server_sessions().expect("the list").len(), 1, "Bob's session is untouched");

    // The cap counts the window's panes and the sessions together: the window's pane, the session and its pane are three.
    let capped = World::start("cap", Some(3), LocalOptions::default());
    let (_, first) = capped.new_session();
    assert_eq!(capped.core.outside_pane_count(), 1, "the session holds one place under the cap");
    let tabs = capped.view().tabs.len();
    let refused = capped.core.server_session_new_in(MAIN_WINDOW).expect_err("over the cap");
    assert_eq!(refused, "this server allows 3 panes per person; close one to open another");
    assert_eq!(capped.view().tabs.len(), tabs, "no tab is left behind");
    assert_eq!(capped.host.list().iter().map(|info| info.id).collect::<Vec<_>>(), vec![first], "and no session");

    // With room for the session but not for its pane, the session that was made is ended again and no tab is made.
    let tight = World::start("tight", Some(4), LocalOptions::default());
    let (_, kept) = tight.new_session();
    let tabs = tight.view().tabs.len();
    let refused = tight.core.server_session_new_in(MAIN_WINDOW).expect_err("no room for the pane");
    assert_eq!(refused, "this server allows 4 panes per person; close one to open another");
    assert_eq!(tight.view().tabs.len(), tabs);
    until("the extra session ends", PATIENCE, || tight.host.list().iter().map(|info| info.id).collect::<Vec<_>>() == vec![kept]);
}

#[test]
fn every_pane_and_every_device_is_an_attachment_of_its_own_and_no_pane_is_a_socket() {
    assert!(LOCAL_CLIENT_BASE > u64::from(u32::MAX), "a pane's number is above every socket number");
    let w = World::start("identities", None, LocalOptions::default());
    let session = w.local.create(80, 24).expect("a session");
    let first = Probe::default();
    let pane = w.pane_on(1, session, (80, 24), None, &first);
    until_pane(&pane, "a prompt", prompt);

    // A device with the smallest socket number is a second attachment, not the pane again.
    let mut device = Device::new(1);
    assert_eq!(device.attach(&w.host, session, None).writer, WriterState::Watching);
    // A second pane of the same window on the same session is a third one, and it watches.
    let second = Probe::default();
    let other = w.pane_on(2, session, (80, 24), None, &second);
    until("the second pane attached", PATIENCE, || pane_writer(&other) == Some(WriterView::Watching));
    let info = w.host.list().into_iter().find(|info| info.id == session).expect("listed");
    assert_eq!((info.attached, info.writer_held), (3, true));
    assert_eq!(pane_writer(&pane), Some(WriterView::Writing));
    assert_eq!(w.local.attached_panes(), 2);

    // The second pane takes over: each of the three has its own view, and only the views that changed are told.
    other.takeover().expect("a takeover");
    until("the views", PATIENCE, || {
        pane_writer(&pane) == Some(WriterView::Watching) && pane_writer(&other) == Some(WriterView::Writing)
    });
    assert_eq!(device.writers(), Vec::<WriterState>::new(), "the device watched before and watches now");
    other.close();
    pane.close();
    until("both panes left", PATIENCE, || w.local.attached_panes() == 0);
    let info = w.host.list().into_iter().find(|info| info.id == session).expect("listed");
    assert_eq!((info.attached, info.writer_held), (1, false), "the device stays, and the right left with its holder");
}

// ---------------------------------------------------------------------------------------------------------------------
// The life of a window pane against the silence rule
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn an_attached_tab_keeps_the_panes_right_and_the_silence_frees_it_after_the_last_tab_left() {
    let w = World::start("life", None, LocalOptions::default());
    let session = w.local.create(80, 24).expect("a session");
    let probe = Probe::default();
    let pane = w.pane_on(1, session, (80, 24), None, &probe);
    until_pane(&pane, "a prompt", prompt);
    let mut device = Device::new(1);
    assert_eq!(device.attach(&w.host, session, None).writer, WriterState::Watching);

    // One browser socket, announced as the supervisor announces it.
    w.browsers.control(&Ctl::Attach { socket: 1 });
    // A device frame is not a browser socket: its socket ending changes nothing of the count.
    handle_ses(&w.host, &Outbound::new(QueueLimits::default()), Ses::Gone { socket: 77 });
    assert_eq!(w.browsers.count(), 1);

    // Five minutes of nobody typing, in steps of the beat interval: the tab keeps the pane writing.
    let mut now = 0;
    for _ in 0..30 {
        now += BEAT_MS;
        w.at(now);
        assert_eq!(pane_writer(&pane), Some(WriterView::Writing), "at {now} ms");
    }
    assert_eq!(device.writers(), Vec::<WriterState>::new(), "the device still watches");

    // A second tab comes and one of the two goes: the beats go on.
    w.browsers.control(&Ctl::Attach { socket: 2 });
    w.browsers.control(&Ctl::Detach { socket: 1 });
    for _ in 0..6 {
        now += BEAT_MS;
        w.at(now);
    }
    assert_eq!(pane_writer(&pane), Some(WriterView::Writing));

    // The last tab goes: the silence counts from the last beat, exactly as for a device whose beats stopped.
    let last_beat = now;
    w.browsers.control(&Ctl::Detach { socket: 2 });
    w.at(last_beat + SILENCE_MS - 1);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(pane_writer(&pane), Some(WriterView::Writing), "at 29 999 ms of silence");
    w.at(last_beat + SILENCE_MS);
    until("the pane free", PATIENCE, || pane_writer(&pane) == Some(WriterView::Free));
    until("the device free", PATIENCE, || device.writers() == vec![WriterState::Free]);
    // A beat never makes a free pane the holder.
    w.browsers.control(&Ctl::Attach { socket: 3 });
    now = last_beat + SILENCE_MS + BEAT_MS;
    w.at(now);
    assert_eq!(pane_writer(&pane), Some(WriterView::Free));

    // A reload: the pane takes the right, the tab goes and a new one is announced inside the silence time.
    pane.takeover().expect("a takeover");
    until("the pane writing", PATIENCE, || pane_writer(&pane) == Some(WriterView::Writing));
    let taken = w.clock.now();
    w.browsers.control(&Ctl::Detach { socket: 3 });
    w.at(taken + 20_000);
    w.browsers.control(&Ctl::Attach { socket: 4 });
    w.at(taken + 20_000 + BEAT_MS);
    w.at(taken + 20_000 + 2 * BEAT_MS);
    assert_eq!(pane_writer(&pane), Some(WriterView::Writing), "the reload kept the writer");

    // A watching pane is never made the holder by a beat, and a device's silence is not covered by the tab's beats.
    device.takeover(&w.host, session);
    until("the pane watching", PATIENCE, || pane_writer(&pane) == Some(WriterView::Watching));
    let device_took = w.clock.now();
    w.at(device_took + BEAT_MS);
    assert_eq!(pane_writer(&pane), Some(WriterView::Watching));
    w.at(device_took + 2 * BEAT_MS);
    w.at(device_took + SILENCE_MS);
    until("the device's silence frees it", PATIENCE, || pane_writer(&pane) == Some(WriterView::Free));

    // With no socket attached, closing the pane that holds the right frees it at once.
    pane.takeover().expect("a takeover");
    until("the pane writing", PATIENCE, || pane_writer(&pane) == Some(WriterView::Writing));
    until("the device watching", PATIENCE, || device.writers().last() == Some(&WriterState::Watching));
    w.browsers.control(&Ctl::Detach { socket: 4 });
    assert_eq!(w.browsers.count(), 0);
    let told = device.writers().len();
    pane.close();
    until("the device told at once", PATIENCE, || device.writers().len() == told + 1);
    assert_eq!(device.writers().last(), Some(&WriterState::Free));
    assert_eq!(w.local.attached_panes(), 0, "a pane that is not attached is beaten for no more");
    w.browsers.control(&Ctl::Attach { socket: 5 });
    w.at(w.clock.now() + BEAT_MS);
    let info = w.host.list().into_iter().find(|info| info.id == session).expect("listed");
    assert_eq!((info.attached, info.writer_held), (1, false));

    // A device attached with no browser socket does not keep a pane's right alive.
    w.browsers.control(&Ctl::Detach { socket: 5 });
    let again = Probe::default();
    let pane = w.pane_on(2, session, (80, 24), None, &again);
    until("attached", PATIENCE, || {
        pane.conn_state() == ConnState::Connected && pane.remote_view().is_some_and(|v| v.state == RemoteState::None)
    });
    assert_eq!(pane_writer(&pane), Some(WriterView::Free), "nobody writes");
    pane.takeover().expect("a takeover");
    until("the pane writing", PATIENCE, || pane_writer(&pane) == Some(WriterView::Writing));
    let start = w.clock.now();
    for step in 1..=3 {
        w.at(start + step * BEAT_MS);
    }
    until("freed by the silence", PATIENCE, || pane_writer(&pane) == Some(WriterView::Free));
    pane.close();
}
