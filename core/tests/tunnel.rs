//! The reverse forward of a Unix socket over the pooled login, against the shared in-process SSH server: the path rule,
//! what the server may open, what the bridge bounds, and the start of a shell that gets its environment from one constant
//! line. No test needs Zig. Every wait is bounded.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::ssh_server::{self, ForwardedPeer, Options, Server};
use holzwave_core::conn::Link;
use holzwave_core::pool::{Connection, ForwardError, ForwardLimits, ForwardSink, ForwardedStream, valid_forward_path};
use holzwave_core::ssh::{SshAuth, SshLink, Tunnel, wrapped_shell_command};

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
        // Each listing runs on a thread of its own and is waited for with a bound, so a stalled login fails the test
        // instead of hanging it.
        let (done, result) = std::sync::mpsc::channel();
        let (c, folder) = (connection.clone(), root.clone());
        let begun = Instant::now();
        std::thread::spawn(move || {
            let c2 = c.clone();
            let listing = c.block_on(async move { c2.sftp().await.map(|(_, sftp)| sftp) });
            let counted = listing.map_err(|e| e.to_string()).and_then(|sftp| {
                c.block_on(async move { sftp.read_dir(folder).await.map(|d| d.count()).map_err(|e| e.to_string()) })
            });
            let _ = done.send(counted);
        });
        let counted = result.recv_timeout(Duration::from_secs(2)).unwrap_or_else(|_| panic!("a listing took over 2 s while a sink was stuck"));
        assert_eq!(counted, Ok(1), "the login is still usable");
        assert!(begun.elapsed() < Duration::from_secs(2));
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

// ---- the wrapped shell ------------------------------------------------------------------------------------------

const TOKEN: &str = "00112233445566778899aabbccddeeff";

/// The three values of a pane's environment, in an order the template does not use.
fn env() -> Vec<(String, String)> {
    vec![
        ("HOLZWAVE_PANE".into(), "7".into()),
        ("HOLZWAVE_TOKEN".into(), TOKEN.into()),
        ("HOLZWAVE_SOCKET".into(), PATH.into()),
    ]
}

fn tunnel(sink: Arc<dyn ForwardSink>) -> Tunnel {
    Tunnel { path: PATH.into(), env: env(), sink }
}

/// A pane's link with its output read by a helper thread and its input at hand.
struct Pane {
    link: SshLink,
    out: ssh_server::Out,
    input: Box<dyn Write + Send>,
}

impl Pane {
    fn start(connection: &Arc<Connection>, tunnel: Option<Tunnel>) -> Self {
        let mut link = SshLink::attach_with(connection.clone(), 80, 24, tunnel).unwrap();
        let out = ssh_server::Out::new(link.take_reader().unwrap());
        let input = link.take_writer().unwrap();
        Pane { link, out, input }
    }

    fn close(self) {
        Box::new(self.link).close().unwrap();
    }
}

const NOT_AVAILABLE: &str = "wsh is not available on this host: ";

#[test]
fn a_tunnel_pane_starts_a_wrapped_shell() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward().wrapped_shell());
    let connection = connect(&server);
    let mut pane = Pane::start(&connection, Some(tunnel(Arc::new(Upper))));
    pane.out.until("welcome 0", 10);
    assert_eq!(server.seen.events(), ["pty".to_owned(), format!("forward {PATH}"), "exec".to_owned()], "in this order, and no shell request");
    assert_eq!(server.seen.commands(), [wrapped_shell_command(&env()).unwrap()], "one exec request, exactly the template");
    assert_eq!(server.seen.shells.load(Ordering::SeqCst), 0);
    assert_eq!(server.seen.execs_as_shell.load(Ordering::SeqCst), 1);
    assert!(!pane.out.seen.contains(NOT_AVAILABLE), "{:?}", pane.out.seen);

    let mut peer = server.open_forwarded(PATH).expect("the forwarded channel reaches the sink");
    peer.write_all(b"hello\n");
    assert_eq!(peer.read_line().as_deref(), Some("HELLO\n"));
    // The shell works like any other.
    pane.input.write_all(b"hi\n").unwrap();
    pane.out.until("echo:hi", 10);
    pane.close();
}

#[test]
fn the_wrapped_command_is_one_constant_template() {
    let make = |socket: &str, token: &str, pane: &str| {
        wrapped_shell_command(&[
            ("HOLZWAVE_TOKEN".into(), token.into()),
            ("HOLZWAVE_PANE".into(), pane.into()),
            ("HOLZWAVE_SOCKET".into(), socket.into()),
        ])
        .unwrap()
    };
    let one = make(PATH, TOKEN, "7");
    assert_eq!(
        one,
        format!(
            "exec sh -c 'HOLZWAVE_SOCKET={PATH}; HOLZWAVE_TOKEN={TOKEN}; HOLZWAVE_PANE=7; PATH=\"$PATH:$HOME/.holzwave/bin\"; \
             export HOLZWAVE_SOCKET HOLZWAVE_TOKEN HOLZWAVE_PANE PATH; exec \"${{SHELL:-/bin/sh}}\" -l'"
        ),
        "the exact template"
    );
    let sets = [one, make("/tmp/a.sock", "abc", "12"), make("/tmp/holzwave-ffffffffffffffffffffffffffffffff.sock", "0", "999999")];
    for text in &sets {
        assert!(!text.contains('\n') && !text.contains('!') && !text.contains('\\'), "{text}");
        assert_eq!(text.matches('\'').count(), 2, "only the two quotes around the script: {text}");
        for part in [
            "exec sh -c '",
            "export HOLZWAVE_SOCKET HOLZWAVE_TOKEN HOLZWAVE_PANE PATH",
            "PATH=\"$PATH:$HOME/.holzwave/bin\"",
            "exec \"${SHELL:-/bin/sh}\" -l",
        ] {
            assert!(text.contains(part), "{part} in {text}");
        }
    }
    // Replacing the three values by a placeholder leaves the same text for every set.
    let shape = |text: &str, values: [&str; 3]| values.iter().fold(text.to_owned(), |t, v| t.replacen(v, "VALUE", 1));
    let a = shape(&sets[0], [PATH, TOKEN, "7"]);
    let b = shape(&sets[1], ["/tmp/a.sock", "abc", "12"]);
    let c = shape(&sets[2], ["/tmp/holzwave-ffffffffffffffffffffffffffffffff.sock", "0", "999999"]);
    assert_eq!(a, b);
    assert_eq!(b, c);
}

#[test]
fn values_that_could_break_the_command_are_refused() {
    let names = ["HOLZWAVE_SOCKET", "HOLZWAVE_TOKEN", "HOLZWAVE_PANE"];
    let with = |index: usize, value: &str| -> Vec<(String, String)> {
        names.iter().enumerate().map(|(i, n)| ((*n).to_owned(), if i == index { value.to_owned() } else { "ok".to_owned() })).collect()
    };
    assert!(wrapped_shell_command(&with(0, "/tmp/a-b_c.d:e")).is_ok());
    for bad in [
        "", "a b", "a'b", "a\"b", "a\nb", "a;b", "a$b", "a`b", "a|b", "a&b", "a<b", "a>b", "a\\b", "a(b", "a)b", "a*b", "a?b", "~", "a~b", "caf\u{e9}",
        "a\tb", "a\0b", "a{b}", "a[b]", "a#b", "a!b", "a=b", &"x".repeat(129),
    ] {
        for i in 0..3 {
            assert!(wrapped_shell_command(&with(i, bad)).is_err(), "{bad:?} in {} must be refused", names[i]);
        }
    }
    assert!(wrapped_shell_command(&with(1, &"x".repeat(128))).is_ok(), "128 bytes are allowed");
    // Names: anything but exactly the three, once each.
    for bad in ["holzwave_pane", "HOLZWAVE_pane", "PATH", "HOLZWAVE_", "HOLZWAVE_PANE ", "HOLZWAVE_PA-NE", "HOLZWAVE_TOKEN;x", ""] {
        let mut env = with(0, "ok");
        env[2].0 = bad.to_owned();
        assert!(wrapped_shell_command(&env).is_err(), "{bad:?} is not a name");
    }
    assert!(wrapped_shell_command(&with(0, "ok")[..2]).is_err(), "two names");
    let mut four = with(0, "ok");
    four.push(("HOLZWAVE_EXTRA".into(), "ok".into()));
    assert!(wrapped_shell_command(&four).is_err(), "four names");
    let mut twice = with(0, "ok");
    twice[2].0 = "HOLZWAVE_TOKEN".into();
    assert!(wrapped_shell_command(&twice).is_err(), "a name twice");
    // The words of a refusal never hold a value.
    let secret = with(1, "secret value");
    let why = wrapped_shell_command(&secret).unwrap_err();
    assert!(!why.contains("secret"), "{why}");
}

#[test]
fn a_refused_forward_leaves_a_plain_shell_with_one_line_why() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().refuse_forward().wrapped_shell());
    let connection = connect(&server);
    let mut pane = Pane::start(&connection, Some(tunnel(Arc::new(Upper))));
    pane.out.until("welcome 0", 10);
    let line = format!("{NOT_AVAILABLE}the server does not allow Unix socket forwarding");
    assert_eq!(pane.out.seen.matches(&line).count(), 1, "said once: {:?}", pane.out.seen);
    assert_eq!(server.seen.events(), ["pty".to_owned(), format!("forward {PATH}"), "shell".to_owned()]);
    assert_eq!(server.seen.shells.load(Ordering::SeqCst), 1);
    assert_eq!(server.seen.execs_as_shell.load(Ordering::SeqCst), 0);
    assert!(server.seen.commands().is_empty(), "no exec request");
    pane.input.write_all(b"works\n").unwrap();
    pane.out.until("echo:works", 10);
    assert!(!pane.out.seen.contains(TOKEN), "the token is never shown");
    pane.close();
}

#[test]
fn a_refused_exec_after_the_forward_cancels_it() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward().wrapped_shell().refuse_exec_once());
    let connection = connect(&server);
    let mut pane = Pane::start(&connection, Some(tunnel(Arc::new(Upper))));
    pane.out.until("welcome 0", 10);
    let line = format!("{NOT_AVAILABLE}the server did not run the start line");
    assert_eq!(pane.out.seen.matches(&line).count(), 1, "{:?}", pane.out.seen);
    assert_eq!(
        server.seen.events(),
        ["pty".to_owned(), format!("forward {PATH}"), "exec".to_owned(), format!("cancel {PATH}"), "shell".to_owned()]
    );
    assert!(server.open_forwarded(PATH).is_err(), "the sink is gone");
    pane.input.write_all(b"works\n").unwrap();
    pane.out.until("echo:works", 10);
    pane.close();
}

#[test]
fn a_forward_that_cannot_be_set_up_is_said_and_the_pane_is_plain() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward().wrapped_shell());
    let connection = connect(&server);
    let mut bad = tunnel(Arc::new(Upper));
    bad.path = "/tmp/not-the-rule".into();
    let mut pane = Pane::start(&connection, Some(bad));
    pane.out.until("welcome 0", 10);
    assert!(pane.out.seen.contains(&format!("{NOT_AVAILABLE}the forward could not be set up: the socket path is not valid")), "{:?}", pane.out.seen);
    assert_eq!(server.seen.events(), ["pty".to_owned(), "shell".to_owned()]);
    pane.close();
}

#[test]
fn an_environment_that_is_refused_never_asks_the_server_for_anything() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward().wrapped_shell());
    let connection = connect(&server);
    let mut broken = tunnel(Arc::new(Upper));
    broken.env[1].1 = "a b; rm -rf /".into();
    let mut pane = Pane::start(&connection, Some(broken));
    pane.out.until("welcome 0", 10);
    assert!(pane.out.seen.contains(&format!("{NOT_AVAILABLE}the environment was refused")), "{:?}", pane.out.seen);
    assert!(!pane.out.seen.contains("rm -rf"), "the value is never shown");
    assert_eq!(server.seen.events(), ["pty".to_owned(), "shell".to_owned()], "no forward, no exec");
    pane.close();
}

/// The value of `HOLZWAVE_SOCKET` in the exec line the server saw as its `n`th.
fn socket_in_command(server: &Server, n: usize) -> String {
    let command = server.seen.commands().get(n).cloned().expect("the exec line");
    let after = command.split("HOLZWAVE_SOCKET=").nth(1).expect("the line sets the socket");
    after.split(';').next().unwrap().to_owned()
}

#[test]
fn a_lost_session_requests_the_forward_again_on_a_new_path() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward().wrapped_shell());
    let connection = connect(&server);
    let mut pane = Pane::start(&connection, Some(tunnel(Arc::new(Upper))));
    pane.out.until("welcome 0", 10);
    server.cut();
    pane.out.until("welcome 1", 15);
    let forwards = server.seen.forwards();
    assert_eq!(forwards.len(), 2, "{forwards:?}");
    assert_eq!(forwards[0], PATH, "the first start asks for the path the tunnel was made with");
    assert_ne!(forwards[1], PATH, "the start after a lost login asks for a path nobody asked for before");
    assert!(valid_forward_path(&forwards[1]), "{}", forwards[1]);
    assert_eq!(server.seen.execs_as_shell.load(Ordering::SeqCst), 2);
    // The new shell's environment names the new path, and only the path changed: the token is the same.
    assert_eq!(socket_in_command(&server, 0), PATH);
    assert_eq!(socket_in_command(&server, 1), forwards[1]);
    let (first, second) = (server.seen.commands()[0].clone(), server.seen.commands()[1].clone());
    assert_eq!(first.replace(PATH, "SOCKET"), second.replace(&forwards[1], "SOCKET"), "nothing else of the line differs");
    assert!(second.contains(TOKEN));
    let mut peer = server.open_forwarded(&forwards[1]).expect("the new session serves the forward");
    peer.write_all(b"back\n");
    assert_eq!(peer.read_line().as_deref(), Some("BACK\n"));
    pane.close();
}

#[test]
fn a_stale_socket_after_an_unclean_loss_does_not_refuse_the_new_forward() {
    let _serial = serial();
    // A server that behaves as `sshd` does without `StreamLocalBindUnlink`: a path stays bound after a login was lost.
    let server = ssh_server::start(Options::password().forward().wrapped_shell().stale_sockets());
    let connection = connect(&server);
    let mut pane = Pane::start(&connection, Some(tunnel(Arc::new(Upper))));
    pane.out.until("welcome 0", 10);
    assert!(server.seen.cancelled_forwards().is_empty());
    server.cut();
    pane.out.until("welcome 1", 15);
    assert!(!pane.out.seen.contains(NOT_AVAILABLE), "no refusal line: {:?}", pane.out.seen);
    let forwards = server.seen.forwards();
    assert_eq!(forwards.len(), 2);
    assert_eq!(server.seen.execs_as_shell.load(Ordering::SeqCst), 2, "the new shell was started through the wrapped line");
    // A second loss: a third path, still served.
    server.cut();
    pane.out.until("welcome 2", 15);
    let forwards = server.seen.forwards();
    assert_eq!(forwards.len(), 3);
    let distinct: std::collections::HashSet<_> = forwards.iter().collect();
    assert_eq!(distinct.len(), 3, "every start asks for its own path: {forwards:?}");
    assert!(forwards.iter().all(|p| valid_forward_path(p)));
    assert_eq!(server.seen.execs_as_shell.load(Ordering::SeqCst), 3);
    let mut peer = server.open_forwarded(&forwards[2]).expect("the newest path is served");
    peer.write_all(b"again\n");
    assert_eq!(peer.read_line().as_deref(), Some("AGAIN\n"));
    pane.close();
}

#[test]
fn the_model_of_a_stale_socket_refuses_the_same_path() {
    // The control of the test above: this server does refuse a path that is still bound, so a pane that asked again for
    // the same path would have been left with a plain shell.
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward().wrapped_shell().stale_sockets());
    let connection = connect(&server);
    let guard = forward(&connection, Arc::new(Upper)).expect("the first forward of the path");
    server.cut();
    // The login ended without a goodbye: the guard is never dropped, so no cancel is sent for the path.
    std::mem::forget(guard);
    // A new client after the loss (the old one has not noticed it yet) asks for the same path.
    let again = forward(&connect(&server), Arc::new(Upper)).map(|_| ());
    assert!(matches!(again, Err(ForwardError::Refused)), "the path is still bound: {again:?}");
}

#[test]
fn closing_the_pane_cancels_the_forward() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward().wrapped_shell());
    let connection = connect(&server);
    let mut pane = Pane::start(&connection, Some(tunnel(Arc::new(Upper))));
    pane.out.until("welcome 0", 10);
    assert!(server.open_forwarded(PATH).is_ok());
    pane.close();
    assert_eq!(server.seen.cancelled_forwards(), [PATH], "the cancel was asked and answered before the pane closed");
    // The connection is still held by this test, so the session is up: the sink is what is gone.
    assert!(server.open_forwarded(PATH).is_err(), "a second forwarded channel for it is rejected");
}

#[test]
fn a_pane_without_a_tunnel_is_unchanged() {
    let _serial = serial();
    let server = ssh_server::start(Options::password().forward().wrapped_shell());
    let connection = connect(&server);
    let mut link = SshLink::attach(connection.clone(), 80, 24).unwrap();
    let mut out = ssh_server::Out::new(link.take_reader().unwrap());
    out.until("welcome 0", 10);
    assert_eq!(server.seen.events(), ["pty", "shell"]);
    assert!(server.seen.forwards().is_empty(), "no forward was asked for");
    assert!(server.seen.commands().is_empty());
    assert!(!out.seen.contains(NOT_AVAILABLE));
    Box::new(link).close().unwrap();
}

#[test]
fn the_debug_text_of_a_tunnel_holds_neither_the_path_nor_the_token() {
    let text = format!("{:?}", tunnel(Arc::new(Upper)));
    assert!(!text.contains(PATH) && !text.contains("holzwave-"), "{text}");
    assert!(!text.contains(TOKEN) && !text.contains("HOLZWAVE_TOKEN"), "{text}");
}
