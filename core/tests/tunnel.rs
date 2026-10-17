//! The reverse forward of a Unix socket over the pooled login, against the shared in-process SSH server: the path rule,
//! what the server may open, what the bridge bounds, and the start of a shell that gets its environment from one constant
//! line. No test needs Zig. Every wait is bounded.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::ssh_server::{self, ForwardedPeer, Options, Server};
use holzwave_core::pool::{Connection, ForwardError, ForwardLimits, ForwardSink, ForwardedStream, valid_forward_path};
use holzwave_core::ssh::SshAuth;

/// The tests share one process and one counter of bridge threads: the ones that open forwarded connections take turns, so
/// a test that counts threads is not counting another test's.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

const PATH: &str = "/tmp/holzwave-0123456789abcdef0123456789abcdef.sock";

/// A sink that reads one line and writes it back in capitals.
struct Upper;

impl ForwardSink for Upper {
    fn serve(&self, stream: ForwardedStream) {
        let ForwardedStream { reader, mut writer } = stream;
        let mut line = String::new();
        if BufReader::new(reader).read_line(&mut line).is_ok() {
            let _ = writer.write_all(line.to_uppercase().as_bytes());
        }
    }
}

fn connect(server: &Server) -> Arc<Connection> {
    Connection::standalone("test", server.target(SshAuth::Password("pw".into()))).unwrap()
}

/// Ask for the forward of `PATH` the way a pane does, and wait for the answer.
fn forward(connection: &Arc<Connection>, sink: Arc<dyn ForwardSink>) -> Result<holzwave_core::pool::ForwardGuard, ForwardError> {
    let c = connection.clone();
    connection.block_on(async move { c.forward(PATH, sink).await })
}

fn forward_with(
    connection: &Arc<Connection>,
    sink: Arc<dyn ForwardSink>,
    limits: ForwardLimits,
) -> Result<holzwave_core::pool::ForwardGuard, ForwardError> {
    let c = connection.clone();
    connection.block_on(async move { c.forward_with(PATH, sink, limits).await })
}

#[test]
fn a_forwarded_channel_reaches_the_sink_and_comes_back() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward());
    let connection = connect(&server);
    let guard = forward(&connection, Arc::new(Upper)).expect("the server allows the forward");
    assert_eq!(server.seen.forwards(), [PATH], "the server was asked for exactly this path");

    let mut peer = server.open_forwarded(PATH).expect("a registered path is let in");
    peer.write_all(b"ping\n");
    assert_eq!(peer.read_line().as_deref(), Some("PING\n"));
    drop(guard);
}

#[test]
fn a_forwarded_channel_for_an_unknown_path_is_rejected() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward());
    let connection = connect(&server);
    let _guard = forward(&connection, Arc::new(Upper)).unwrap();
    let other = "/tmp/holzwave-ffffffffffffffffffffffffffffffff.sock";
    assert!(server.open_forwarded(other).is_err(), "nobody registered {other}");
    // The registered one still works.
    assert!(server.open_forwarded(PATH).is_ok());
}

#[test]
fn the_path_rule_is_exact() {
    let hex = "0123456789abcdef0123456789abcdef";
    assert!(valid_forward_path(&format!("/tmp/holzwave-{hex}.sock")));
    assert!(valid_forward_path(PATH));
    for bad in [
        String::new(),
        format!("/tmp/holzwave-{}.sock", hex.to_uppercase()),
        format!("/tmp/holzwave-{}.sock", &hex[1..]),
        format!("/tmp/holzwave-{hex}0.sock"),
        format!("/var/holzwave-{hex}.sock"),
        format!("/tmp/other-{hex}.sock"),
        format!("/tmp/holzwave-{hex}"),
        format!("/tmp/holzwave-{hex}.sock "),
        format!(" /tmp/holzwave-{hex}.sock"),
        format!("/tmp/../tmp/holzwave-{hex}.sock"),
        format!("/tmp/holzwave-{}..{}.sock", &hex[..15], &hex[17..]),
        format!("/tmp/holzwave-{} {}.sock", &hex[..15], &hex[16..]),
        format!("/tmp/holzwave-{}\0{}.sock", &hex[..15], &hex[16..]),
        format!("/tmp/holzwave-{}/{}.sock", &hex[..15], &hex[16..]),
        format!("/tmp/holzwave-{}g.sock", &hex[..31]),
        format!("/tmp/holzwave-{hex}.sock\n"),
    ] {
        assert!(!valid_forward_path(&bad), "{bad:?} must be refused");
    }
}

#[test]
fn a_path_that_is_not_the_rule_is_never_asked_of_the_server() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward());
    let connection = connect(&server);
    let c = connection.clone();
    let result = connection.block_on(async move { c.forward("/tmp/evil; rm -rf /", Arc::new(Upper)).await });
    assert!(matches!(result, Err(ForwardError::Failed(_))), "{result:?}");
    assert!(server.seen.forwards().is_empty(), "nothing was asked");
    assert_eq!(server.seen.connections.load(Ordering::SeqCst), 0, "not even a login was made");
}

#[test]
fn a_server_that_refuses_the_forward_gets_refused_and_no_sink_stays() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().refuse_forward());
    let connection = connect(&server);
    let result = forward(&connection, Arc::new(Upper));
    assert!(matches!(result, Err(ForwardError::Refused)), "{result:?}");
    assert_eq!(server.seen.forwards(), [PATH]);
    assert!(server.open_forwarded(PATH).is_err(), "the sink was unregistered");
}

/// A scratch directory that is removed when the test ends, also when it fails.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---- helpers for the bounds -------------------------------------------------------------------------------------

fn within(what: &str, secs: u64, mut condition: impl FnMut() -> bool) {
    assert!(common::wait_until(Duration::from_secs(secs), &mut condition), "no {what} within {secs} s");
}

/// A sink that counts its calls and the bytes it read, and waits for a release before it reads (or never reads).
#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    read: AtomicUsize,
    timed_out: AtomicBool,
    ended: AtomicBool,
    release: Mutex<bool>,
    wake: std::sync::Condvar,
}

impl Probe {
    fn release(&self) {
        *self.release.lock().unwrap() = true;
        self.wake.notify_all();
    }

    fn wait_release(&self) {
        let mut released = self.release.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !*released && Instant::now() < deadline {
            released = self.wake.wait_timeout(released, Duration::from_millis(50)).unwrap().0;
        }
    }
}

/// How a probe's sink behaves.
enum Mode {
    /// Read until the end or an error, counting bytes.
    Drain,
    /// Block until released, then read nothing.
    Stuck,
    /// Write this many bytes back and report how it went.
    Write(usize),
}

struct Behave(Arc<Probe>, Mode);

impl ForwardSink for Behave {
    fn serve(&self, stream: ForwardedStream) {
        let probe = &self.0;
        probe.calls.fetch_add(1, Ordering::SeqCst);
        let ForwardedStream { mut reader, mut writer } = stream;
        match &self.1 {
            Mode::Drain => {
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            probe.read.fetch_add(n, Ordering::SeqCst);
                        }
                        Err(e) => {
                            probe.timed_out.store(e.kind() == std::io::ErrorKind::TimedOut, Ordering::SeqCst);
                            break;
                        }
                    }
                }
            }
            Mode::Stuck => probe.wait_release(),
            Mode::Write(total) => {
                let chunk = vec![b'x'; 64 * 1024];
                let mut left = *total;
                while left > 0 {
                    let n = left.min(chunk.len());
                    if writer.write_all(&chunk[..n]).is_err() {
                        probe.timed_out.store(true, Ordering::SeqCst);
                        break;
                    }
                    left -= n;
                }
                probe.wait_release();
            }
        }
        probe.ended.store(true, Ordering::SeqCst);
    }
}

fn peers(server: &Server, n: usize) -> Vec<ForwardedPeer> {
    (0..n).map(|_| server.open_forwarded(PATH).expect("a place is free")).collect()
}

fn quick(silence_ms: u64, life_ms: u64) -> ForwardLimits {
    ForwardLimits { silence: Duration::from_millis(silence_ms), life: Duration::from_millis(life_ms), ..ForwardLimits::default() }
}

#[test]
fn a_fifth_concurrent_connection_is_rejected_and_a_closed_one_frees_a_place() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward());
    let connection = connect(&server);
    let probe = Arc::new(Probe::default());
    let guard = forward(&connection, Arc::new(Behave(probe.clone(), Mode::Drain))).unwrap();
    let mut open = peers(&server, 4);
    within("the four sinks to run", 10, || probe.calls.load(Ordering::SeqCst) == 4);
    assert_eq!(guard.open(), 4);
    assert!(server.open_forwarded(PATH).is_err(), "a fifth connection is rejected");

    open.remove(0).close();
    // The place is given back when the connection is over; the client has to notice the close first.
    within("a place to be free", 10, || server.open_forwarded(PATH).map(|peer| open.push(peer)).is_ok());
    assert!(server.open_forwarded(PATH).is_err(), "and it is full again");
    drop(open);
}

#[test]
fn a_connection_that_sends_too_much_is_closed() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward());
    let connection = connect(&server);
    let probe = Arc::new(Probe::default());
    let guard = forward(&connection, Arc::new(Behave(probe.clone(), Mode::Drain))).unwrap();
    let mut peer = server.open_forwarded(PATH).unwrap();
    let chunk = vec![b'a'; 32 * 1024];
    let mut sent = 0usize;
    // No newline, more than 1 MiB: the client closes the channel after the bound.
    while sent <= 3 * 1024 * 1024 && peer.try_write(&chunk) {
        sent += chunk.len();
    }
    assert!(peer.closed_within(Duration::from_secs(10)), "the channel is closed from the client side");
    within("the sink to see the end", 10, || probe.ended.load(Ordering::SeqCst));
    assert!(probe.read.load(Ordering::SeqCst) <= 1024 * 1024, "the sink read {} bytes", probe.read.load(Ordering::SeqCst));
    assert!(!probe.timed_out.load(Ordering::SeqCst));
    within("the place to be given back", 10, || guard.open() == 0);
}

#[test]
fn a_silent_connection_is_ended() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward());
    let connection = connect(&server);
    let probe = Arc::new(Probe::default());
    let guard = forward_with(&connection, Arc::new(Behave(probe.clone(), Mode::Drain)), quick(300, 60_000)).unwrap();
    let mut peer = server.open_forwarded(PATH).unwrap();
    let started = Instant::now();
    within("the silence to end the connection", 10, || probe.ended.load(Ordering::SeqCst));
    assert!(probe.timed_out.load(Ordering::SeqCst), "the reader says TimedOut");
    assert!(started.elapsed() < Duration::from_secs(8));
    assert!(peer.closed_within(Duration::from_secs(10)), "and the channel closes");
    within("the place to be given back", 10, || guard.open() == 0);
}

#[test]
fn a_connection_that_lives_too_long_is_closed() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward());
    let connection = connect(&server);
    let probe = Arc::new(Probe::default());
    // The silence bound is long: only the life bound can end it. The peer keeps talking, so it is never silent.
    let guard = forward_with(&connection, Arc::new(Behave(probe.clone(), Mode::Drain)), quick(60_000, 600)).unwrap();
    let mut peer = server.open_forwarded(PATH).unwrap();
    let started = Instant::now();
    while !peer.closed_within(Duration::from_millis(1)) && started.elapsed() < Duration::from_secs(10) {
        let _ = peer.try_write(b"x");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(peer.closed_within(Duration::from_secs(5)), "closed by the life bound");
    assert!(started.elapsed() >= Duration::from_millis(500), "not before the bound");
    within("the sink to see the end", 10, || probe.ended.load(Ordering::SeqCst));
    within("the place to be given back", 10, || guard.open() == 0);
}

#[test]
fn a_sink_that_never_reads_cannot_stall_the_session() {
    let _serial = serial();
    let dir = Scratch(common::scratch_dir("tunnel-stall"));
    std::fs::write(dir.0.join("a.txt"), "alpha").unwrap();
    let root = dir.0.canonicalize().unwrap().to_string_lossy().into_owned();
    let server = ssh_server::start(Options::password().forward().sftp(dir.0.clone()));
    let connection = connect(&server);
    let probe = Arc::new(Probe::default());
    let _guard = forward(&connection, Arc::new(Behave(probe.clone(), Mode::Stuck))).unwrap();
    let mut peer = server.open_forwarded(PATH).unwrap();
    within("the sink to run", 10, || probe.calls.load(Ordering::SeqCst) == 1);

    // Another channel of the same login while the flood runs: a file listing.
    let flood = std::thread::spawn(move || {
        let chunk = vec![b'z'; 32 * 1024];
        let mut sent = 0usize;
        while sent < 4 * 1024 * 1024 && peer.try_write(&chunk) {
            sent += chunk.len();
        }
        (peer, sent)
    });
    let started = Instant::now();
    let mut rounds = 0;
    while !flood.is_finished() && started.elapsed() < Duration::from_secs(15) {
        let begun = Instant::now();
        let c = connection.clone();
        let listing = connection.block_on(async move { c.sftp().await.map(|(_, sftp)| sftp) });
        let sftp = listing.expect("the login is still usable");
        let folder = root.clone();
        let names = connection.block_on(async move { sftp.read_dir(folder).await.map(|d| d.count()) });
        assert!(names.is_ok(), "{names:?}");
        assert!(begun.elapsed() < Duration::from_secs(2), "a listing took {:?} while a sink was stuck", begun.elapsed());
        rounds += 1;
    }
    let (mut peer, sent) = flood.join().unwrap();
    assert!(rounds >= 1, "at least one listing ran");
    assert!(sent > 0);
    assert!(peer.closed_within(Duration::from_secs(10)), "the connection of the stuck sink was closed");
    probe.release();
}

#[test]
fn a_sink_that_writes_too_much_is_cut() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward());
    let connection = connect(&server);
    let probe = Arc::new(Probe::default());
    let _guard = forward(&connection, Arc::new(Behave(probe.clone(), Mode::Write(3 * 1024 * 1024)))).unwrap();
    let mut peer = server.open_forwarded(PATH).unwrap();
    within("the writer to be refused", 10, || probe.timed_out.load(Ordering::SeqCst));
    assert!(peer.closed_within(Duration::from_secs(10)), "the channel is closed");
    probe.release();
}

#[test]
fn the_registry_is_cleaned() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward());
    let connection = connect(&server);
    within("the threads of the earlier tests to be gone", 10, || holzwave_core::pool::forward_threads() == 0);
    let before = holzwave_core::pool::forward_threads();
    let probe = Arc::new(Probe::default());
    let guard = forward(&connection, Arc::new(Behave(probe.clone(), Mode::Drain))).unwrap();
    let mut peer = server.open_forwarded(PATH).unwrap();
    within("the sink to run", 10, || probe.calls.load(Ordering::SeqCst) == 1);
    assert_eq!(holzwave_core::pool::forward_threads(), before + 1, "one bridge thread");

    // Dropped without `cancel`, as a panic path would.
    drop(guard);
    within("the server to see the cancel", 10, || server.seen.cancelled_forwards() == [PATH]);
    assert!(server.open_forwarded(PATH).is_err(), "the sink is unregistered");
    // The connection that was open ends with its peer; close it and wait for the thread.
    peer.close();
    within("no bridge thread to be left", 10, || holzwave_core::pool::forward_threads() == before);
}

#[test]
fn a_panicking_sink_leaves_no_thread_and_no_place() {
    let _serial = serial();
    struct Boom;
    impl ForwardSink for Boom {
        fn serve(&self, _stream: ForwardedStream) {
            panic!("the sink panics on purpose");
        }
    }
    let server = ssh_server::start(Options::password().forward());
    let connection = connect(&server);
    within("the threads of the earlier tests to be gone", 10, || holzwave_core::pool::forward_threads() == 0);
    let before = holzwave_core::pool::forward_threads();
    let guard = forward(&connection, Arc::new(Boom)).unwrap();
    for _ in 0..8 {
        let mut peer = server.open_forwarded(PATH).expect("a panic gives its place back");
        assert!(peer.closed_within(Duration::from_secs(10)), "the connection of a dead sink is closed");
    }
    within("the places to be given back", 10, || guard.open() == 0);
    within("no bridge thread to be left", 10, || holzwave_core::pool::forward_threads() == before);
}

#[test]
fn a_replaced_session_takes_its_forwards_with_it() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward());
    let connection = connect(&server);
    let _guard = forward(&connection, Arc::new(Upper)).unwrap();
    assert!(server.open_forwarded(PATH).is_ok());
    server.cut();
    within("the session to be seen as closed", 10, || !connection.is_connected());
    // A new login: the old path is not registered on it, so the server's open for it is rejected.
    let c = connection.clone();
    connection.block_on(async move { c.session(holzwave_core::pool::Open::Interactive).await }).unwrap();
    assert_eq!(server.seen.logins.load(Ordering::SeqCst), 2);
    assert!(server.open_forwarded(PATH).is_err(), "not registered on the new session");
    // Asked for again on the new session (the shell loop does this), it works.
    let again = forward(&connection, Arc::new(Upper)).unwrap();
    let mut peer = server.open_forwarded(PATH).unwrap();
    peer.write_all(b"again\n");
    assert_eq!(peer.read_line().as_deref(), Some("AGAIN\n"));
    drop(again);
}
