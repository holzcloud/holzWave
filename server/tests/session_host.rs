//! The session host of a worker, with real programs on real PTYs and no network: sessions that outlive every client, an exact
//! attach with a bounded tail, one writer with an honest clock, the end of a program, a client that cannot keep up, and the
//! pane cap. A recording attachment stands for the socket of a client.
#![cfg(unix)]

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use holzwave_core::GhosttyEngine;
use holzwave_core::engine::TerminalEngine;
use holzwave_core::pane::PaneSpec;
use holzwave_core::textread::TextRegion;
use holzwave_server::config::Remote;
use holzwave_server::worker::lease::{Input, WriterState};
use holzwave_core::pane::MAX_RAW_BACKLOG;
use holzwave_server::worker::sessions::{
    Attached, Attachment, CHUNK_BYTES, Clock, DROPPED_NOTE_MS, EXITED_GRACE_MS, HostSettings, MAX_INPUT_BYTES, MonotonicClock,
    Place, SesError, SessionHost, SessionLimits, Spawner,
};

const WAIT: Duration = Duration::from_secs(30);

// ---- the rig ----------------------------------------------------------------------------------------------------------------

/// What an attachment was handed, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Got {
    Snapshot(u32, Vec<u8>),
    Live(u32, Vec<u8>),
    Writer(WriterState),
    Size(u16, u16),
    Exited(Option<u32>),
    Resync,
}

/// A client that records everything. `hold` makes it slow (its reader blocks inside a push, as a client that does not read
/// would); `accept` makes it refuse pushes.
struct Rec {
    got: Mutex<Vec<Got>>,
    hold: AtomicBool,
    accept: AtomicBool,
}

impl Rec {
    fn new() -> Arc<Rec> {
        Arc::new(Rec { got: Mutex::new(Vec::new()), hold: AtomicBool::new(false), accept: AtomicBool::new(true) })
    }

    fn got(&self) -> Vec<Got> {
        self.got.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn push(&self, got: Got) {
        self.got.lock().unwrap_or_else(PoisonError::into_inner).push(got);
    }

    fn writers(&self) -> Vec<WriterState> {
        self.got().into_iter().filter_map(|g| if let Got::Writer(w) = g { Some(w) } else { None }).collect()
    }

    fn exits(&self) -> Vec<Option<u32>> {
        self.got().into_iter().filter_map(|g| if let Got::Exited(c) = g { Some(c) } else { None }).collect()
    }

    fn sizes(&self) -> Vec<(u16, u16)> {
        self.got().into_iter().filter_map(|g| if let Got::Size(c, r) = g { Some((c, r)) } else { None }).collect()
    }

    fn resyncs(&self) -> usize {
        self.got().iter().filter(|g| **g == Got::Resync).count()
    }

    /// Every byte the attachment was handed, snapshot first.
    fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for got in self.got() {
            if let Got::Snapshot(_, b) | Got::Live(_, b) = got {
                out.extend_from_slice(&b);
            }
        }
        out
    }

    fn snapshot_bytes(&self) -> usize {
        self.got().iter().map(|g| if let Got::Snapshot(_, b) = g { b.len() } else { 0 }).sum()
    }

    /// The sequence numbers of the chunks, which must count 0, 1, 2 ... across the snapshot and the live bytes.
    fn seqs(&self) -> Vec<u32> {
        self.got()
            .into_iter()
            .filter_map(|g| match g {
                Got::Snapshot(seq, _) | Got::Live(seq, _) => Some(seq),
                _ => None,
            })
            .collect()
    }

    fn live_text(&self) -> String {
        let mut out = Vec::new();
        for got in self.got() {
            if let Got::Live(_, b) = got {
                out.extend_from_slice(&b);
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn wait_hold(&self) {
        while self.hold.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Attachment for Rec {
    fn snapshot_chunk(&self, _session: u32, seq: u32, bytes: &[u8]) -> bool {
        assert!(bytes.len() <= CHUNK_BYTES, "a chunk is at most {CHUNK_BYTES} bytes");
        if !self.accept.load(Ordering::SeqCst) {
            return false;
        }
        self.push(Got::Snapshot(seq, bytes.to_vec()));
        true
    }

    fn live(&self, _session: u32, seq: u32, bytes: &[u8]) -> bool {
        assert!(bytes.len() <= CHUNK_BYTES, "a chunk is at most {CHUNK_BYTES} bytes");
        self.wait_hold();
        if !self.accept.load(Ordering::SeqCst) {
            return false;
        }
        self.push(Got::Live(seq, bytes.to_vec()));
        true
    }

    fn writer(&self, _session: u32, state: WriterState) {
        self.push(Got::Writer(state));
    }

    fn size(&self, _session: u32, cols: u16, rows: u16) {
        self.push(Got::Size(cols, rows));
    }

    fn exited(&self, _session: u32, code: Option<u32>) {
        self.push(Got::Exited(code));
    }

    fn resync(&self, _session: u32) {
        self.push(Got::Resync);
    }
}

/// A clock that stands still until a test moves it.
struct ManualClock(AtomicU64);

impl ManualClock {
    fn new() -> Arc<ManualClock> {
        Arc::new(ManualClock(AtomicU64::new(0)))
    }

    fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Starts `sh -c <script>` for every session. A place is free while fewer places are taken, and the panes of a window, than
/// the cap.
struct Programs {
    fallback: String,
    taken: Arc<AtomicUsize>,
    window_panes: AtomicUsize,
    cap: usize,
    scrollback: usize,
}

struct Taken(Arc<AtomicUsize>);

impl Drop for Taken {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Programs {
    fn new(fallback: &str) -> Arc<Programs> {
        Arc::new(Programs {
            fallback: fallback.to_owned(),
            taken: Arc::new(AtomicUsize::new(0)),
            window_panes: AtomicUsize::new(0),
            cap: 64,
            scrollback: 10_000,
        })
    }

    fn with(fallback: &str, cap: usize, scrollback: usize) -> Arc<Programs> {
        Arc::new(Programs {
            fallback: fallback.to_owned(),
            taken: Arc::new(AtomicUsize::new(0)),
            window_panes: AtomicUsize::new(0),
            cap,
            scrollback,
        })
    }
}

impl Spawner for Programs {
    fn spec(&self, cols: u16, rows: u16) -> PaneSpec {
        let mut spec = PaneSpec::new("/bin/sh", cols, rows).args(["-c", &self.fallback]);
        spec.scrollback = self.scrollback;
        spec
    }

    fn take_place(&self) -> Option<Place> {
        let taken = self.taken.fetch_add(1, Ordering::SeqCst);
        if taken + self.window_panes.load(Ordering::SeqCst) >= self.cap {
            self.taken.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Box::new(Taken(self.taken.clone())))
    }
}

fn until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(Instant::now() < deadline, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Attaches and waits for the answer.
fn attach(host: &SessionHost, client: u64, rec: &Arc<Rec>, session: u32, tail: Option<u32>) -> Result<Attached, SesError> {
    let (tx, rx) = mpsc::channel();
    host.attach(client, rec.clone(), session, tail, move |answer| {
        let _ = tx.send(answer);
    });
    rx.recv_timeout(WAIT).expect("the attach is always answered")
}

/// [`attach`], then waits until the whole snapshot the answer announced has been handed to the attachment, which the reader
/// thread does after the answer.
fn attach_whole(host: &SessionHost, client: u64, rec: &Arc<Rec>, session: u32, tail: Option<u32>) -> Result<Attached, SesError> {
    let attached = attach(host, client, rec, session, tail)?;
    until("the whole snapshot arrives", || rec.snapshot_bytes() == attached.snapshot_bytes as usize);
    Ok(attached)
}

/// What a client shows: an engine of the size the attach answered, fed everything the attachment was handed.
fn render(attached: &Attached, rec: &Rec) -> String {
    let mut engine = GhosttyEngine::new(attached.cols, attached.rows, 20_000).expect("client engine");
    engine.feed(&rec.bytes());
    engine.read_text(TextRegion::LastRows(20_000), 4 << 20).expect("read").map(|read| read.text).unwrap_or_default()
}

/// The numbers of the lines `n <int>` of a text, in the order they stand.
fn numbers(text: &str) -> Vec<u32> {
    text.lines().filter_map(|line| line.strip_prefix("n ")).filter_map(|rest| rest.trim().parse().ok()).collect()
}

fn consecutive(seen: &[u32]) -> bool {
    seen.windows(2).all(|pair| pair[1] == pair[0] + 1)
}

fn host_of(programs: &Arc<Programs>) -> SessionHost {
    SessionHost::new(programs.clone(), SessionLimits::DEFAULT)
}

/// A program that prints a numbered line every 50 ms.
const TICKING: &str = "i=0; while true; do echo \"n $i\"; i=$((i+1)); sleep 0.05; done";

/// A program that prints `count` numbered lines in bursts of 60 with a short pause between, then `done`, and waits.
fn paced(count: u32) -> String {
    format!(
        "i=0; while [ $i -lt {count} ]; do echo \"n $i\"; i=$((i+1)); \
         if [ $((i % 60)) -eq 0 ]; then sleep 0.02; fi; done; echo done; sleep 60"
    )
}

/// A creator (client 1) and its attachment to a new session.
fn started(host: &SessionHost, rec: &Arc<Rec>) -> u32 {
    host.create(1, rec.clone(), 80, 24).expect("the session starts")
}

// ---- sessions that outlive the clients ---------------------------------------------------------------------------------------

#[test]
fn output_made_while_nobody_is_attached_is_in_the_snapshot_of_the_next_attach_once_and_in_order() {
    let programs = Programs::new(TICKING);
    let host = host_of(&programs);
    let first = Rec::new();
    let id = started(&host, &first);
    let attached = attach(&host, 1, &first, id, None).expect("attach");
    until("the first lines", || numbers(&render(&attached, &first)).len() >= 5);
    let seen_before = *numbers(&render(&attached, &first)).last().expect("a number");
    host.detach(1, id).expect("detach");

    // Nobody is attached for two seconds; the program keeps printing.
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(host.list().len(), 1, "the session runs with no client");
    assert_eq!(host.list()[0].attached, 0);

    let second = Rec::new();
    let attached = attach_whole(&host, 2, &second, id, None).expect("the second attach");
    let seen = numbers(&render(&attached, &second));
    assert!(consecutive(&seen), "each line once and in order: {seen:?}");
    assert_eq!(seen[0], 0, "the whole history is in the tail");
    assert!(
        *seen.last().expect("lines") >= seen_before + 25,
        "the lines printed meanwhile are there: the first client saw up to {seen_before}, the snapshot ends at {:?}",
        seen.last()
    );
    // The snapshot is exactly what the answer announced, and the sequence numbers count on from it into the live bytes.
    assert_eq!(second.snapshot_bytes(), attached.snapshot_bytes as usize);
    until("some live bytes", || second.got().iter().any(|g| matches!(g, Got::Live(..))));
    let seqs = second.seqs();
    assert_eq!(seqs, (0..u32::try_from(seqs.len()).expect("small")).collect::<Vec<_>>());
    drop(host);
}

#[test]
fn a_client_that_attaches_while_the_program_writes_sees_every_number_once_across_snapshot_and_live_bytes() {
    const LINES: u32 = 2400;
    let programs = Programs::new(&paced(LINES));
    let host = host_of(&programs);
    let creator = Rec::new();
    let id = started(&host, &creator);
    let mut state: u64 = 0x5eed_0009;
    let mut next = move |bound: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % bound
    };
    let mut clients = Vec::new();
    for n in 0..8u64 {
        std::thread::sleep(Duration::from_millis(next(50)));
        let rec = Rec::new();
        let attached = attach(&host, 10 + n, &rec, id, Some(0)).expect("attach");
        clients.push((rec, attached));
    }
    let mut mid_stream = 0;
    for (n, (rec, attached)) in clients.iter().enumerate() {
        until("done reaches the client", || render(attached, rec).contains("done"));
        let seen = numbers(&render(attached, rec));
        assert!(!seen.is_empty(), "client {n} has no line");
        assert!(consecutive(&seen), "client {n}: a number is lost or repeated");
        assert_eq!(*seen.last().expect("lines"), LINES - 1, "client {n} does not end at the last line");
        if seen[0] > 0 {
            mid_stream += 1;
        }
    }
    assert!(mid_stream >= 2, "only {mid_stream} of 8 attached after the program had started writing");
}

// ---- the list and the id space ------------------------------------------------------------------------------------------------

#[test]
fn the_list_describes_a_session_and_an_id_of_another_host_is_unknown() {
    let programs = Programs::new("printf '\\033]0;a title\\007'; sleep 60");
    let host = host_of(&programs);
    let rec = Rec::new();
    let id = started(&host, &rec);
    attach(&host, 1, &rec, id, None).expect("attach");
    until("the title", || host.list().iter().any(|s| s.title == "a title"));
    let listed = host.list();
    assert_eq!(listed.len(), 1);
    let info = &listed[0];
    assert_eq!((info.id, info.cols, info.rows, info.attached, info.writer_held), (id, 80, 24, 1, true));
    assert!(info.started > 1_700_000_000, "the start is a Unix time");
    assert!(info.title.len() <= 256 && info.cwd.len() <= 4096);

    // Another person's host made ids of its own: three here are three there, and this host has made one.
    let other_programs = Programs::new("sleep 60");
    let other = host_of(&other_programs);
    let theirs: Vec<u32> = (0..3).map(|_| other.create(7, Rec::new(), 80, 24).expect("start")).collect();
    let stranger = *theirs.last().expect("an id");
    assert!(stranger > id, "an id that this host has not made");
    let probe = Rec::new();
    assert_eq!(attach(&host, 9, &probe, stranger, None), Err(SesError::NotFound));
    assert_eq!(host.input(9, stranger, b"x"), Err(SesError::NotFound));
    assert_eq!(host.resize(9, stranger, 100, 30), Err(SesError::NotFound));
    assert_eq!(host.takeover(9, stranger), Err(SesError::NotFound));
    assert_eq!(host.detach(9, stranger), Err(SesError::NotFound));
    assert_eq!(host.beat(9, stranger), Err(SesError::NotFound));
    assert_eq!(host.end(stranger), Err(SesError::NotFound));
    for odd in [0, u32::MAX] {
        assert_eq!(attach(&host, 9, &probe, odd, None), Err(SesError::NotFound));
    }
    assert!(probe.got().is_empty(), "nothing was handed to a client that asked for an id that is not here");
    assert_eq!(host.list().len(), 1, "and the other host's sessions are not in this list");
    assert_eq!(other.list().len(), 3);

    // Ids are never reused while the host lives.
    host.end(id).expect("end");
    until("the ended session leaves", || {
        host.detach(1, id).ok();
        host.list().is_empty()
    });
    let again = host.create(1, Rec::new(), 80, 24).expect("start");
    assert!(again > id, "{again} must not be {id} again");
}

// ---- the tail, the cap and the answer -----------------------------------------------------------------------------------------

#[test]
fn the_tail_is_clamped_to_the_history_cap_and_the_pane_and_the_answer_says_whether_older_lines_exist() {
    // 300 lines on a screen of 24 rows, against a default tail of 50, a cap of 100 and a pane that keeps about 150.
    let programs = Programs::with("i=0; while [ $i -lt 300 ]; do echo \"n $i\"; i=$((i+1)); done; echo done; sleep 60", 64, 150);
    let limits = SessionLimits { attach_tail_lines: 50, history_max_lines: 100, ..SessionLimits::DEFAULT };
    let host = SessionHost::new(programs.clone(), limits);
    let creator = Rec::new();
    let id = started(&host, &creator);
    until("the program is done", || {
        let rec = Rec::new();
        let Ok(attached) = attach_whole(&host, 99, &rec, id, Some(0)) else { return false };
        host.detach(99, id).ok();
        render(&attached, &rec).contains("done")
    });

    let lines = |tail: Option<u32>| {
        let rec = Rec::new();
        let attached = attach_whole(&host, 2, &rec, id, tail).expect("attach");
        let seen = numbers(&render(&attached, &rec));
        host.detach(2, id).expect("detach");
        (seen.len(), attached.more, consecutive(&seen), *seen.last().expect("lines"))
    };
    // The screen holds 24 rows; the tail comes on top of it, near enough (the engine drops whole pages).
    let (default, more, ok, last) = lines(None);
    assert!(ok && last == 299 && more, "{default} lines, more {more}");
    assert!((60..=85).contains(&default), "a default tail of 50 rows on top of the screen: {default}");
    let (zero, more, ok, _) = lines(Some(0));
    assert!(ok && more, "the screen alone leaves older lines out");
    assert!((15..=26).contains(&zero), "the screen alone: {zero}");
    let (clamped, more, ok, _) = lines(Some(1_000_000));
    assert!(ok && more, "a pane of 150 rows of history holds more than the cap of 100");
    assert!((100..=130).contains(&clamped), "a request for a million rows is cut to the cap of 100: {clamped}");
    assert!(clamped > default);
    drop(host);

    // A pane that holds less than the tail: nothing is left out, so `more` is false.
    let programs = Programs::new("i=0; while [ $i -lt 10 ]; do echo \"n $i\"; i=$((i+1)); done; sleep 60");
    let host = host_of(&programs);
    let creator = Rec::new();
    let id = started(&host, &creator);
    until("the lines", || {
        let rec = Rec::new();
        let attached = attach_whole(&host, 2, &rec, id, Some(5000)).expect("attach");
        host.detach(2, id).ok();
        assert!(!attached.more, "the whole history fits on the screen");
        numbers(&render(&attached, &rec)).len() == 10
    });
}

#[test]
fn a_snapshot_over_the_byte_cap_is_refused_and_the_session_keeps_running() {
    let limits = SessionLimits { snapshot_max_bytes: 1500, ..SessionLimits::DEFAULT };
    let programs = Programs::new("i=0; while [ $i -lt 400 ]; do echo \"n $i\"; i=$((i+1)); done; echo done; sleep 60");
    let host = SessionHost::new(programs.clone(), limits);
    let creator = Rec::new();
    let id = started(&host, &creator);
    // While the program is still writing the snapshot fits; once its output is long enough it is refused.
    let refused = Rec::new();
    until("the output is there and too large", || {
        let probe = Rec::new();
        if attach(&host, 2, &probe, id, Some(10_000)) == Err(SesError::Refused) {
            assert!(probe.got().is_empty(), "a refused snapshot hands out no byte");
            return true;
        }
        host.detach(2, id).ok();
        false
    });
    assert_eq!(attach(&host, 2, &refused, id, Some(10_000)), Err(SesError::Refused));
    assert!(refused.got().is_empty());
    // The session runs on, and a request that fits is served: the screen alone is small.
    assert_eq!(host.list().len(), 1);
    until("the screen alone fits and shows the end", || {
        let small = Rec::new();
        let Ok(attached) = attach_whole(&host, 3, &small, id, Some(0)) else { return false };
        host.detach(3, id).ok();
        assert!(attached.snapshot_bytes < 1500);
        render(&attached, &small).contains("done")
    });
}

// ---- input, resize and the writer ------------------------------------------------------------------------------------------------

#[test]
fn input_of_the_writer_reaches_the_program_and_input_of_a_watcher_does_not() {
    // No echo of the terminal: whatever comes back was written by `cat`, so it reached the program.
    let programs = Programs::new("stty -echo; cat");
    let host = host_of(&programs);
    let writer = Rec::new();
    let id = started(&host, &writer);
    let attached = attach(&host, 1, &writer, id, None).expect("attach");
    assert_eq!(attached.writer, WriterState::Writing, "the creator writes");
    let watcher = Rec::new();
    let seen = attach(&host, 2, &watcher, id, None).expect("attach");
    assert_eq!(seen.writer, WriterState::Watching);
    std::thread::sleep(Duration::from_millis(300)); // `stty` has run

    // The watcher first: a `cat` would echo it, and the writer's own line after it proves the program is reading.
    assert_eq!(host.input(2, id, b"from-the-watcher\n"), Ok(Input::Dropped));
    assert_eq!(host.input(1, id, b"from-the-writer\n"), Ok(Input::Accepted));
    until("the writer's line comes back", || writer.live_text().contains("from-the-writer"));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!writer.live_text().contains("from-the-watcher"), "the watcher's bytes never reached the program");
    assert!(!watcher.live_text().contains("from-the-watcher"));
    assert!(watcher.live_text().contains("from-the-writer"), "the watcher sees what the program writes");
    // An unknown client's input is dropped too.
    assert_eq!(host.input(77, id, b"stranger\n"), Ok(Input::Dropped));
    assert_eq!(host.input(1, id, &vec![b'x'; 70_000]), Err(SesError::BadRequest));
}

#[test]
fn only_the_writer_resizes_and_every_attachment_hears_the_new_size_after_a_takeover() {
    let programs = Programs::new("sleep 60");
    let host = host_of(&programs);
    let writer = Rec::new();
    let id = started(&host, &writer);
    attach(&host, 1, &writer, id, None).expect("attach");
    let watcher = Rec::new();
    attach(&host, 2, &watcher, id, None).expect("attach");

    assert_eq!(host.resize(2, id, 100, 30), Err(SesError::Refused), "a watcher cannot resize");
    assert_eq!(host.resize(99, id, 100, 30), Err(SesError::Refused), "a client that is not attached cannot either");
    assert_eq!((host.list()[0].cols, host.list()[0].rows), (80, 24), "nothing changed");
    assert!(writer.sizes().is_empty() && watcher.sizes().is_empty(), "and nobody was told of a change that did not happen");

    host.takeover(2, id).expect("the watcher takes over");
    assert_eq!(watcher.writers(), vec![WriterState::Writing]);
    assert_eq!(writer.writers(), vec![WriterState::Watching]);
    assert_eq!(host.resize(1, id, 100, 30), Err(SesError::Refused), "the old writer is a watcher now");
    host.resize(2, id, 100, 30).expect("the new writer resizes");
    assert_eq!((host.list()[0].cols, host.list()[0].rows), (100, 30));
    until("both hear the size", || writer.sizes() == vec![(100, 30)] && watcher.sizes() == vec![(100, 30)]);
    // A client that attaches now is told the new size by the answer.
    let late = Rec::new();
    let attached = attach(&host, 3, &late, id, None).expect("attach");
    assert_eq!((attached.cols, attached.rows), (100, 30));
    // The size is clamped to what a pane takes.
    host.resize(2, id, 5000, 0).expect("clamped");
    assert!(host.list()[0].cols <= 1000 && host.list()[0].rows >= 1);
}

#[test]
fn the_silence_of_the_writer_frees_the_right_after_the_configured_time_and_never_moves_it() {
    let clock = ManualClock::new();
    let programs = Programs::new("sleep 60");
    let host = SessionHost::start(programs.clone(), HostSettings::default(), clock.clone());
    let writer = Rec::new();
    let id = started(&host, &writer);
    attach(&host, 1, &writer, id, None).expect("attach");
    let watcher = Rec::new();
    attach(&host, 2, &watcher, id, None).expect("attach");

    clock.set(29_999);
    host.sweep();
    assert!(writer.writers().is_empty() && watcher.writers().is_empty(), "29 999 ms of silence: still held");
    assert!(host.list()[0].writer_held);
    clock.set(30_000);
    host.sweep();
    assert_eq!(writer.writers(), vec![WriterState::Free]);
    assert_eq!(watcher.writers(), vec![WriterState::Free]);
    assert!(!host.list()[0].writer_held);

    // The watcher is not made the writer by anything it does; its input is dropped, a beat changes nothing.
    clock.set(500_000);
    assert_eq!(host.input(2, id, b"x"), Ok(Input::Dropped));
    host.beat(2, id).expect("beat");
    host.sweep();
    assert_eq!(watcher.writers(), vec![WriterState::Free], "no new event: nothing moved");
    // An explicit takeover is what moves it.
    host.takeover(2, id).expect("takeover");
    assert_eq!(watcher.writers(), vec![WriterState::Free, WriterState::Writing]);
    assert_eq!(writer.writers(), vec![WriterState::Free, WriterState::Watching]);
    // The writer's own signs of life keep the right.
    clock.set(520_000);
    host.beat(2, id).expect("beat");
    clock.set(540_000);
    host.sweep();
    assert!(host.list()[0].writer_held, "20 s after the beat is inside the time");
    clock.set(550_000);
    host.sweep();
    assert!(!host.list()[0].writer_held, "30 s after the beat is not");
}

#[test]
fn the_housekeeping_thread_frees_the_right_by_itself_with_the_real_clock() {
    let limits = SessionLimits { writer_timeout_ms: 300, ..SessionLimits::DEFAULT };
    let programs = Programs::new("sleep 60");
    let host = SessionHost::start(
        programs.clone(),
        HostSettings { limits, ..HostSettings::default() },
        Arc::new(MonotonicClock::new()),
    );
    let writer = Rec::new();
    let id = started(&host, &writer);
    attach(&host, 1, &writer, id, None).expect("attach");
    let watcher = Rec::new();
    attach(&host, 2, &watcher, id, None).expect("attach");
    let begun = Instant::now();
    until("both are told free", || writer.writers() == vec![WriterState::Free] && watcher.writers() == vec![WriterState::Free]);
    assert!(begun.elapsed() >= Duration::from_millis(250), "not before the time: {:?}", begun.elapsed());
}

#[test]
fn a_writer_that_detaches_frees_the_right_and_a_watcher_that_leaves_changes_nothing() {
    let programs = Programs::new("sleep 60");
    let host = host_of(&programs);
    let writer = Rec::new();
    let id = started(&host, &writer);
    attach(&host, 1, &writer, id, None).expect("attach");
    let first = Rec::new();
    attach(&host, 2, &first, id, None).expect("attach");
    let second = Rec::new();
    attach(&host, 3, &second, id, None).expect("attach");
    host.detach(3, id).expect("a watcher leaves");
    assert!(writer.writers().is_empty() && first.writers().is_empty(), "nothing changed for the others");
    // The socket of the writer ends: the same as a detach, for every session at once.
    host.gone(1);
    assert_eq!(first.writers(), vec![WriterState::Free]);
    assert!(!host.list()[0].writer_held);
    assert_eq!(host.list()[0].attached, 1);
}

// ---- the end of a program --------------------------------------------------------------------------------------------------------------

#[test]
fn a_program_that_ends_ends_its_session_and_the_attachments_hear_it_with_the_code() {
    let programs = Programs::new("echo hi; sleep 0.5; exit 3");
    let clock = ManualClock::new();
    let host = SessionHost::start(programs.clone(), HostSettings::default(), clock.clone());
    let rec = Rec::new();
    let id = started(&host, &rec);
    let other = Rec::new();
    attach(&host, 1, &rec, id, None).expect("attach");
    attach(&host, 2, &other, id, None).expect("attach");
    until("both hear the end", || rec.exits() == vec![Some(3)] && other.exits() == vec![Some(3)]);
    assert!(rec.live_text().contains("hi"), "the output came before the end");
    // It stays listed while someone is attached, but cannot be attached again.
    assert_eq!(host.list().len(), 1);
    assert_eq!(attach(&host, 3, &Rec::new(), id, None), Err(SesError::NotFound));
    assert_eq!(host.input(1, id, b"x"), Ok(Input::Dropped), "nothing is written to a program that ended");
    host.detach(1, id).expect("detach");
    assert_eq!(host.list().len(), 1, "one client is still attached");
    host.detach(2, id).expect("detach");
    assert!(host.list().is_empty(), "it leaves with the last attachment");
    assert_eq!(host.end(id), Err(SesError::NotFound));
}

#[test]
fn an_ended_session_nobody_is_attached_to_leaves_at_once_and_one_somebody_forgot_leaves_after_the_grace() {
    let programs = Programs::new("exit 0");
    let clock = ManualClock::new();
    let host = SessionHost::start(programs.clone(), HostSettings::default(), clock.clone());
    let forgotten = Rec::new();
    let id = started(&host, &forgotten);
    attach(&host, 1, &forgotten, id, None).expect("attach");
    until("the end is heard", || !forgotten.exits().is_empty());
    assert_eq!(host.list().len(), 1);
    clock.set(EXITED_GRACE_MS - 1);
    host.sweep();
    assert_eq!(host.list().len(), 1, "inside the grace");
    clock.set(EXITED_GRACE_MS + 5);
    host.sweep();
    assert!(host.list().is_empty(), "after the grace");

    // Nobody attached when it ended.
    let loner = Rec::new();
    let quick = host.create(2, loner.clone(), 80, 24).expect("start");
    host.gone(2);
    let _ = quick;
    until("the session leaves by itself", || host.is_empty());
}

#[test]
fn ending_a_running_session_kills_its_program_once_and_an_unknown_id_is_not_found() {
    let programs = Programs::new("sleep 60");
    let host = host_of(&programs);
    let rec = Rec::new();
    let id = started(&host, &rec);
    attach(&host, 1, &rec, id, None).expect("attach");
    assert_eq!(host.end(id), Ok(()));
    until("the attachment hears the end", || rec.exits().len() == 1);
    host.detach(1, id).expect("detach");
    until("the session is gone", || host.is_empty());
    assert_eq!(host.end(id), Err(SesError::NotFound), "ending it again is an unknown id");
    assert_eq!(host.end(424_242), Err(SesError::NotFound));
    assert_eq!(rec.exits().len(), 1, "the end is told once");
}

// ---- a client that cannot keep up --------------------------------------------------------------------------------------------------

#[test]
fn an_attachment_whose_tap_overflows_is_told_to_resync_and_the_others_and_the_session_are_not_affected() {
    // 64 bursts of 64 KiB, 10 ms apart: 4 MiB against a queue of 128 KiB per attachment.
    let flood = "i=0; while [ $i -lt 64 ]; do yes abcdefghijklmnopqrstuvwxyz | head -c 65536; sleep 0.01; i=$((i+1)); done; echo finished; sleep 60";
    let programs = Programs::new(flood);
    let host = SessionHost::start(
        programs.clone(),
        HostSettings { tap_queue_bytes: 128 * 1024, ..HostSettings::default() },
        Arc::new(MonotonicClock::new()),
    );
    let slow = Rec::new();
    slow.hold.store(true, Ordering::SeqCst);
    let fast = Rec::new();
    let id = started(&host, &slow);
    // The slow client's reader blocks in its first live push, so its queue fills.
    attach(&host, 1, &slow, id, Some(0)).expect("attach");
    attach(&host, 2, &fast, id, Some(0)).expect("attach");
    until("the fast client has everything", || fast.live_text().contains("finished"));
    assert_eq!(fast.resyncs(), 0, "the fast client lost nothing");
    // Let the slow one go: its stream is over, it is told, and it is told once.
    slow.hold.store(false, Ordering::SeqCst);
    until("the slow client is told to resync", || slow.resyncs() == 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(slow.resyncs(), 1);
    assert_eq!(host.list().len(), 1, "the session runs on");
    assert_eq!(host.list()[0].attached, 2, "and the slow client is still attached: it only lost its stream");

    // A re-attach works and shows the screen as it is now.
    let again = Rec::new();
    let attached = attach_whole(&host, 1, &again, id, Some(0)).expect("the slow client attaches again");
    assert_eq!(attached.writer, WriterState::Writing, "it kept its place in the lease");
    assert_eq!(host.list()[0].attached, 2);
    assert!(render(&attached, &again).contains("finished"));
}

#[test]
fn a_push_that_cannot_be_taken_ends_that_attachments_stream_with_a_resync() {
    let programs = Programs::new(&paced(600));
    let host = host_of(&programs);
    let creator = Rec::new();
    let id = started(&host, &creator);
    let refusing = Rec::new();
    refusing.accept.store(false, Ordering::SeqCst);
    let other = Rec::new();
    attach(&host, 2, &other, id, Some(0)).expect("attach");
    attach(&host, 3, &refusing, id, Some(10_000)).expect("the answer comes before any push");
    until("the refusing client is told", || refusing.resyncs() >= 1);
    assert!(refusing.bytes().is_empty());
    until("the other client has the end", || other.live_text().contains("done"));
    assert_eq!(other.resyncs(), 0);
}

#[test]
fn input_that_a_program_leaves_unread_is_dropped_and_the_writer_is_told_once_a_second() {
    // Raw mode, no echo, and a program that never reads: everything typed waits in the pane.
    let programs = Programs::new("stty raw -echo; exec sleep 600");
    let clock = ManualClock::new();
    let host = SessionHost::start(programs.clone(), HostSettings::default(), clock.clone());
    let writer = Rec::new();
    let id = started(&host, &writer);
    std::thread::sleep(Duration::from_millis(300)); // `stty` has run
    let chunk = vec![b'x'; MAX_INPUT_BYTES];
    let mut answers = Vec::new();
    for _ in 0..2 * MAX_RAW_BACKLOG / MAX_INPUT_BYTES + 8 {
        answers.push(host.input(1, id, &chunk));
    }
    let busy = answers.iter().filter(|a| **a == Err(SesError::Busy)).count();
    assert_eq!(busy, 1, "told once while the clock stands still: {answers:?}");
    let first_busy = answers.iter().position(|a| *a == Err(SesError::Busy)).expect("told");
    assert!(answers[..first_busy].iter().all(|a| *a == Ok(Input::Accepted)), "what fits is taken first");
    assert!(answers[first_busy + 1..].iter().all(|a| *a == Ok(Input::Dropped)), "the rest is dropped quietly: {answers:?}");
    // A second later the writer is told again; a watcher's input is dropped by the lease and never told.
    clock.set(DROPPED_NOTE_MS - 1);
    assert_eq!(host.input(1, id, &chunk), Ok(Input::Dropped));
    clock.set(DROPPED_NOTE_MS);
    assert_eq!(host.input(1, id, &chunk), Err(SesError::Busy));
    let watcher = Rec::new();
    attach(&host, 2, &watcher, id, Some(0)).expect("attach");
    assert_eq!(host.input(2, id, &chunk), Ok(Input::Dropped));
}

#[test]
fn a_reattach_answers_only_after_the_old_stream_has_pushed_its_last_byte_so_nothing_old_follows_the_answer() {
    let programs = Programs::new(TICKING);
    let host = host_of(&programs);
    // One recorder for both attaches, as one socket is for a device: the order it sees is the order the client reads.
    let rec = Rec::new();
    let id = started(&host, &rec);
    attach_whole(&host, 1, &rec, id, Some(0)).expect("attach");
    until("live bytes flow", || !rec.live_text().is_empty());

    // The old reader is caught inside a push of live bytes while the client attaches again.
    rec.hold.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(300));
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            host.attach(1, rec.clone(), id, Some(0), |answer| {
                rec.push(Got::Writer(WriterState::Free)); // a marker: where the answer stands in what the client read
                let _ = tx.send(answer);
            });
        });
        std::thread::sleep(Duration::from_millis(300));
        rec.hold.store(false, Ordering::SeqCst);
    });
    let attached = rx.recv_timeout(WAIT).expect("answered").expect("attached again");
    until("the new snapshot arrives", || {
        let got = rec.got();
        let at = got.iter().position(|g| *g == Got::Writer(WriterState::Free)).expect("the marker");
        let sent: usize = got[at..].iter().map(|g| if let Got::Snapshot(_, b) = g { b.len() } else { 0 }).sum();
        sent == attached.snapshot_bytes as usize
    });
    until("the new stream carries live bytes", || {
        let got = rec.got();
        let at = got.iter().position(|g| *g == Got::Writer(WriterState::Free)).expect("the marker");
        got[at..].iter().any(|g| matches!(g, Got::Live(..)))
    });
    let got = rec.got();
    let at = got.iter().position(|g| *g == Got::Writer(WriterState::Free)).expect("the marker");
    let after: Vec<u32> = got[at..]
        .iter()
        .filter_map(|g| match g {
            Got::Snapshot(seq, _) | Got::Live(seq, _) => Some(*seq),
            _ => None,
        })
        .collect();
    assert_eq!(after.first(), Some(&0), "the first chunk after the answer is the new snapshot's first: {after:?}");
    assert!(after.windows(2).all(|w| w[1] == w[0] + 1), "no chunk of the old stream after the answer: {after:?}");
    assert!(matches!(got[at + 1], Got::Snapshot(0, _)), "the answer is followed by the snapshot: {:?}", &got[at..at + 2]);
    assert_eq!(rec.resyncs(), 0, "the old stream ended quietly");
}

#[test]
fn a_writer_whose_stream_is_dropped_keeps_its_right_to_write_and_gets_a_new_stream_when_it_attaches_again() {
    // No echo of the terminal: whatever comes back was written by `cat`, so it reached the program.
    let programs = Programs::new("stty -echo; cat");
    let host = host_of(&programs);
    let writer = Rec::new();
    let id = started(&host, &writer);
    attach_whole(&host, 1, &writer, id, None).expect("attach");
    let watcher = Rec::new();
    attach(&host, 2, &watcher, id, None).expect("attach");
    std::thread::sleep(Duration::from_millis(300)); // `stty` has run

    // The supervisor could not deliver the writer's stream: only the stream ends.
    host.drop_stream(1, id).expect("the session is there");
    assert!(watcher.writers().is_empty(), "nobody is told of a change of the writer");
    assert!(host.list()[0].writer_held && host.list()[0].attached == 2, "{:?}", host.list());
    assert_eq!(writer.resyncs(), 0, "the supervisor tells the client, the host does not tell it again");
    assert_eq!(host.input(1, id, b"still-the-writer\n"), Ok(Input::Accepted));
    until("the watcher sees the writer's line", || watcher.live_text().contains("still-the-writer"));
    std::thread::sleep(Duration::from_millis(200));
    assert!(!writer.live_text().contains("still-the-writer"), "the dropped stream carries nothing more");

    // Attaching again keeps the place in the lease and brings the screen.
    let again = Rec::new();
    let attached = attach_whole(&host, 1, &again, id, None).expect("attach again");
    assert_eq!(attached.writer, WriterState::Writing);
    assert!(render(&attached, &again).contains("still-the-writer"));
    assert_eq!(host.drop_stream(1, 999), Err(SesError::NotFound), "an unknown session");
    assert_eq!(host.drop_stream(77, id), Ok(()), "a client that is not attached changes nothing");
    assert!(host.list()[0].writer_held);
}

// ---- the pane cap -----------------------------------------------------------------------------------------------------------------

#[test]
fn sessions_and_the_panes_of_the_window_together_never_pass_the_cap() {
    let programs = Programs::with("sleep 60", 3, 1000);
    programs.window_panes.store(1, Ordering::SeqCst);
    let host = host_of(&programs);
    let one = host.create(1, Rec::new(), 80, 24).expect("the second pane");
    host.create(1, Rec::new(), 80, 24).expect("the third pane");
    assert_eq!(host.create(1, Rec::new(), 80, 24), Err(SesError::TooMany), "one pane of the window and two sessions fill the cap of 3");
    assert_eq!(host.list().len(), 2);
    // A session that ends and leaves gives its place back.
    host.end(one).expect("end");
    until("its place is free", || {
        host.gone(1);
        host.list().len() == 1
    });
    host.create(2, Rec::new(), 80, 24).expect("a place again");
}

#[test]
fn the_count_hook_tells_how_many_sessions_there_are() {
    let programs = Programs::new("sleep 60");
    let host = host_of(&programs);
    let counts = Arc::new(Mutex::new(Vec::new()));
    let seen = counts.clone();
    host.set_count_hook(move |n| seen.lock().unwrap_or_else(PoisonError::into_inner).push(n));
    let a = host.create(1, Rec::new(), 80, 24).expect("start");
    host.create(1, Rec::new(), 80, 24).expect("start");
    host.end(a).expect("end");
    host.gone(1);
    until("the count falls", || counts.lock().unwrap_or_else(PoisonError::into_inner).last() == Some(&1));
    assert_eq!(counts.lock().unwrap_or_else(PoisonError::into_inner)[..2], [1, 2]);
    host.shutdown();
    assert_eq!(counts.lock().unwrap_or_else(PoisonError::into_inner).last(), Some(&0));
}

// ---- the four keys -----------------------------------------------------------------------------------------------------------------

#[test]
fn the_defaults_of_the_four_keys_are_the_ones_the_host_runs_with() {
    assert_eq!(Remote::default().session_limits(), SessionLimits::DEFAULT);
    assert_eq!(SessionLimits::DEFAULT.attach_tail_lines, 1000);
    assert_eq!(SessionLimits::DEFAULT.history_max_lines, 10_000);
    assert_eq!(SessionLimits::DEFAULT.snapshot_max_bytes, 4 * 1024 * 1024);
    assert_eq!(SessionLimits::DEFAULT.writer_timeout_ms, 30_000);
}
