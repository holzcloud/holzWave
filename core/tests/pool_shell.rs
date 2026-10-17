//! The shell of a pane on the pooled connection of a saved profile, against the shared in-process SSH server: one login
//! for a pane, a file listing and a command; closing a pane; two panes; reconnect; a refused login; a changed host key;
//! a server that limits its channels; and a pane that cannot keep up with a flood.
//!
//! Every wait is bounded. No test needs Zig except the one pane-level test behind the `ghostty` feature.

mod common;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use common::ssh_server::{self, Options, Out, Server};
use holzwave_core::conn::{ConnState, Link};
use holzwave_core::connections::{Connections, KNOWN_HOSTS_FILE, SecretChange};
use holzwave_core::files::{FileSource, SftpSource};
use holzwave_core::pane::PaneSpec;
use holzwave_core::pool::Connection;
use holzwave_core::profiles::{Auth, Profile};
use holzwave_core::secrets::MemoryStore;
use holzwave_core::ssh::{ExecCancel, ExecLimits, MAX_BACKLOG, SshExec, SshLink, learn_host_key};

/// A scratch directory that is removed when the test ends, also when it fails.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A server with an SFTP root of two files and a saved profile `talos` that points at it (password `pw` unless a test
/// says otherwise), one reconnect delay of one second and three attempts.
struct World {
    dir: Scratch,
    root: PathBuf,
    server: Server,
    connections: Connections,
}

impl World {
    /// `tune` shapes the server; `learn` puts the server's host key into the known hosts file, as a pane would have.
    fn new(tag: &str, tune: impl FnOnce(Options) -> Options, learn: bool, password: &str) -> Self {
        let dir = Scratch(common::scratch_dir(tag));
        let root = dir.0.join("root");
        std::fs::create_dir(&root).unwrap();
        // The server speaks canonical paths (on a Mac the temporary folder is behind a link).
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("a.txt"), "alpha").unwrap();
        std::fs::write(root.join("b.txt"), "beta").unwrap();
        let server = ssh_server::start(tune(Options::password().sftp(root.clone())));
        if learn {
            server.trust(&dir.0.join(KNOWN_HOSTS_FILE));
        } else {
            std::fs::write(dir.0.join(KNOWN_HOSTS_FILE), "").unwrap();
        }
        let (connections, _) = Connections::load(&dir.0, Arc::new(MemoryStore::default()), None, &[1, 2, 3]);
        let profile = Profile {
            port: server.addr.port(),
            user: "holz".into(),
            auth: Auth::Password,
            reconnect_attempts: 3,
            reconnect_delay_secs: 1,
            ..Profile::new("talos", "Talos", "127.0.0.1")
        };
        connections.save(profile, SecretChange::Set { value: password.into() }).unwrap();
        World { dir, root, server, connections }
    }

    fn plain(tag: &str) -> Self {
        Self::new(tag, |o| o, true, "pw")
    }

    fn connection(&self) -> Arc<Connection> {
        self.connections.session("talos").unwrap()
    }

    fn root(&self) -> &str {
        self.root.to_str().unwrap()
    }

    fn known(&self) -> PathBuf {
        self.dir.0.join(KNOWN_HOSTS_FILE)
    }

    fn files(&self) -> SftpSource {
        SftpSource::on(self.connection())
    }

    fn exec(&self) -> SshExec {
        SshExec::on(self.connection()).unwrap()
    }

    fn pane(&self) -> Pane {
        Pane::new(SshLink::attach(self.connection(), 80, 24).unwrap())
    }

    fn count(&self, counter: impl Fn(&ssh_server::Seen) -> &AtomicUsize) -> usize {
        counter(&self.server.seen).load(Ordering::SeqCst)
    }

    fn logins(&self) -> usize {
        self.count(|s| &s.logins)
    }
}

/// A pane's link with its output read by a helper thread and its input at hand.
struct Pane {
    link: SshLink,
    out: Out,
    input: Box<dyn Write + Send>,
}

impl Pane {
    fn new(mut link: SshLink) -> Self {
        let out = Out::new(link.take_reader().unwrap());
        let input = link.take_writer().unwrap();
        Pane { link, out, input }
    }

    fn type_in(&mut self, text: &str) {
        self.input.write_all(text.as_bytes()).unwrap();
    }

    fn close(self) -> u32 {
        Box::new(self.link).close().unwrap()
    }
}

fn run(exec: &SshExec, command: &str) -> String {
    let result = exec.exec(command, &ExecLimits::default(), &ExecCancel::new()).unwrap();
    assert_eq!(result.status, Some(0), "{command}");
    String::from_utf8_lossy(&result.stdout).trim().to_owned()
}

fn names(files: &SftpSource, root: &str) -> Vec<String> {
    files.list(root).unwrap().entries.into_iter().map(|e| e.name).collect()
}

fn wait_until(what: &str, secs: u64, condition: impl FnMut() -> bool) {
    assert!(common::wait_until(Duration::from_secs(secs), condition), "no {what} within {secs} s");
}

// ---- the tracer -------------------------------------------------------------------------------------------------

#[test]
fn a_pane_a_listing_and_a_command_share_one_login() {
    let w = World::plain("one-login");
    let mut pane = w.pane();
    pane.out.until("welcome 0", 10);
    assert_eq!(names(&w.files(), w.root()), ["a.txt", "b.txt"]);
    assert_eq!(run(&w.exec(), "echo ok"), "ran:echo ok");

    let seen = &w.server.seen;
    assert_eq!(seen.connections.load(Ordering::SeqCst), 1, "one TCP connection");
    assert_eq!(w.logins(), 1, "one login for the pane, the files and the command");
    assert_eq!(seen.shells.load(Ordering::SeqCst), 1);
    assert_eq!(seen.ptys.load(Ordering::SeqCst), 1);
    assert_eq!(seen.sftp_sessions.load(Ordering::SeqCst), 1);
    assert_eq!(seen.commands(), ["echo ok"]);
    assert_eq!(pane.link.state(), ConnState::Connected);
    pane.close();
}

#[test]
fn closing_a_pane_leaves_the_session_to_the_others() {
    let w = World::plain("close-pane");
    let files = w.files();
    let mut pane = w.pane();
    pane.out.until("welcome 0", 10);
    assert_eq!(names(&files, w.root()).len(), 2);
    assert_eq!(pane.close(), 0, "a pane closed by the person ends with code 0");

    // The session stays: no new login and no disconnect.
    assert_eq!(names(&files, w.root()).len(), 2, "a listing after the pane closed");
    assert_eq!(run(&w.exec(), "echo after"), "ran:echo after");
    assert_eq!(w.logins(), 1, "no new login after the pane closed");
    assert_eq!(w.count(|s| &s.connections), 1);
    assert_eq!(w.count(|s| &s.disconnects), 0, "closing a pane never disconnects the session");
}

#[test]
fn two_panes_of_one_profile_share_the_connection() {
    let w = World::plain("two-panes");
    w.connections.assign(1, "talos").unwrap();
    w.connections.assign(2, "talos").unwrap();
    let (mut one, mut two) = (PaneSpec::new("sh", 80, 24), PaneSpec::new("sh", 80, 24));
    w.connections.apply(1, &mut one).unwrap();
    w.connections.apply(2, &mut two).unwrap();
    let (a, b) = (one.ssh_connection.clone().unwrap(), two.ssh_connection.clone().unwrap());
    assert!(Arc::ptr_eq(&a, &b), "every pane of one profile gets the same connection");
    assert!(Arc::ptr_eq(&a, &w.connection()));

    let mut first = Pane::new(SshLink::attach(a, 80, 24).unwrap());
    let mut second = Pane::new(SshLink::attach(b, 100, 30).unwrap());
    first.out.until("welcome 0", 10);
    second.out.until("welcome 0", 10);
    assert_eq!(w.count(|s| &s.shells), 2, "two shells");
    assert_eq!(w.logins(), 1, "one login");
    assert_eq!(w.count(|s| &s.connections), 1);
    first.type_in("one");
    first.out.until("echo:one", 10);
    second.type_in("two");
    second.out.until("echo:two", 10);
    assert!(!first.out.seen.contains("echo:two") && !second.out.seen.contains("echo:one"), "each shell has its own channel");
    // Closing one leaves the other running.
    first.close();
    second.type_in("still");
    second.out.until("echo:still", 10);
    second.close();
}

// ---- reconnect and the words of today ---------------------------------------------------------------------------

#[test]
fn a_lost_session_is_replaced_once_for_the_pane_the_files_and_the_commands() {
    let w = World::new("lost", |o| o.cut_shell_after(1000), true, "pw");
    let files = w.files();
    let exec = w.exec();
    let mut pane = w.pane();
    pane.out.until("welcome 0", 10);
    assert_eq!(names(&files, w.root()).len(), 2, "before the cut");
    assert_eq!(w.logins(), 1);

    // The server cuts the connection; the pane says so and waits its second before it tries again.
    pane.out.until("Retrying 1/3", 10);
    // The files do not wait for the pane: whoever comes first makes the new login, the other uses it.
    assert_eq!(names(&files, w.root()).len(), 2, "a listing after the cut");
    pane.out.until("welcome 1", 10);
    assert_eq!(run(&exec, "echo again"), "ran:echo again", "a command after the cut");
    assert_eq!(w.count(|s| &s.connections), 2, "exactly one new connection");
    assert_eq!(w.logins(), 2, "one login before the cut and one after it, for all three users");
    assert_eq!(pane.link.state(), ConnState::Connected);
    pane.close();
}

#[test]
fn the_typing_between_the_cut_and_the_new_shell_is_kept() {
    let w = World::new("typing", |o| o.cut_shell_after(300), true, "pw");
    let mut pane = w.pane();
    pane.out.until("welcome 0", 10);
    pane.out.until("Retrying 1/3", 10);
    // 70 KiB while the connection is down: the first 64 KiB are kept, the rest is dropped.
    for _ in 0..70 {
        pane.type_in(&"x".repeat(1024));
    }
    pane.out.until("welcome 1", 10);
    // The kept input arrives in the new shell, echoed in pieces; only the output after the new greeting counts.
    let echoed = |out: &Out| out.seen.split_once("welcome 1").map_or(0, |(_, rest)| rest.matches('x').count());
    let deadline = Instant::now() + Duration::from_secs(10);
    while echoed(&pane.out) < 64 * 1024 && Instant::now() < deadline {
        pane.out.quiet(Duration::from_millis(100), Duration::from_secs(1));
    }
    pane.out.quiet(Duration::from_millis(500), Duration::from_secs(5));
    assert_eq!(echoed(&pane.out), 64 * 1024, "what was typed while down is kept up to the bound, and no more");
    pane.type_in("later");
    pane.out.until("echo:later", 10);
    pane.close();
}

#[test]
fn a_refused_login_keeps_the_pane_open_and_enter_tries_again_without_the_cooldown() {
    let w = World::new("refused", |o| o, true, "wrong");
    let mut pane = w.pane();
    pane.out.until("refused the login. Press Enter to try again.", 10);
    assert!(pane.out.seen.contains("Connection to 127.0.0.1 failed: holz@127.0.0.1 refused the login."), "{:?}", pane.out.seen);
    assert_eq!(pane.link.state(), ConnState::Failed);
    assert_eq!(pane.link.try_wait().unwrap(), None, "the pane is not ended: the reason stays on the screen");
    assert_eq!(w.count(|s| &s.refused_logins), 1);

    // A command runner in the same second is answered from the cooldown, without a login.
    let why = w.exec().exec("echo no", &ExecLimits::default(), &ExecCancel::new()).unwrap_err().to_string();
    assert!(why.contains("refused the login"), "{why}");
    assert_eq!(w.count(|s| &s.refused_logins), 1, "a background open waits after a refused login");

    // A person's Enter is never held back by it.
    pane.type_in("\r");
    wait_until("a second attempt at once", 5, || w.count(|s| &s.refused_logins) == 2);
    pane.out.until("Press Enter to try again", 10);
    assert_eq!(pane.close(), 255);
}

#[test]
fn a_changed_host_key_is_loud_on_the_pane() {
    let w = World::new("changed-key", |o| o, false, "pw");
    // The file knows another key for this very host and port.
    learn_host_key("127.0.0.1", w.server.addr.port(), ssh_server::ed25519().public_key(), &w.known()).unwrap();
    let mut pane = w.pane();
    pane.out.until("CHANGED", 10);
    pane.out.until("Press Enter to try again", 10);
    assert!(pane.out.seen.contains("Connection to 127.0.0.1 failed:"), "{:?}", pane.out.seen);
    assert_eq!(pane.link.state(), ConnState::Failed);
    assert_eq!(w.logins(), 0, "nobody logged in to a host whose key changed");
    pane.close();
}

const REFUSED_CHANNEL: &str = "the server refuses another channel on this connection; it limits how many can be open at once";

#[test]
fn a_server_that_refuses_another_channel_is_told_in_words() {
    // The pane takes the one channel; a listing needs a second one.
    let w = World::new("max-channels", |o| o.max_channels(1), true, "pw");
    let mut pane = w.pane();
    pane.out.until("welcome 0", 10);
    let why = w.files().list(w.root()).unwrap_err().to_string();
    assert!(why.contains(REFUSED_CHANNEL), "{why}");
    assert!(!why.contains("ChannelOpenFailure"), "no raw protocol error: {why}");
    let why = w.exec().exec("echo no", &ExecLimits::default(), &ExecCancel::new()).unwrap_err().to_string();
    assert!(why.contains(REFUSED_CHANNEL), "{why}");
    // The pane keeps running.
    pane.type_in("hi");
    pane.out.until("echo:hi", 10);
    assert_eq!(pane.link.state(), ConnState::Connected);
    pane.close();

    // The other way round: a listing holds the one channel, and the pane says so on its screen and goes on retrying.
    let w = World::new("max-channels-pane", |o| o.max_channels(1), true, "pw");
    let files = w.files();
    assert_eq!(names(&files, w.root()).len(), 2);
    let mut pane = w.pane();
    pane.out.until(REFUSED_CHANNEL, 10);
    pane.out.until("Retrying 1/3", 10);
    pane.out.until("Retrying 2/3", 10);
    assert_eq!(w.count(|s| &s.shells), 0, "the shell was never opened");
    assert_eq!(w.logins(), 1, "the retries use the one login");
    pane.close();
}

// ---- the backlog ------------------------------------------------------------------------------------------------

/// The bytes of a flood of `n` bytes as the server sends it, from the welcome line to the end line.
fn flood_text(n: usize) -> Vec<u8> {
    let mut text = b"welcome 0\r\n".to_vec();
    text.extend((0..n).map(|i| b'a' + (i % 26) as u8));
    text.extend_from_slice(b"flood done\r\n");
    text
}

/// Read raw output until `needle` has been seen, in a thread, and return how many bytes were read and the last of them.
/// (The `Out` helper searches the whole text on every chunk, which is too slow for 16 MiB.)
fn read_until(mut reader: Box<dyn Read + Send>, needle: &str, secs: u64) -> (usize, String) {
    let (tx, rx) = mpsc::channel();
    let wanted = needle.to_owned();
    std::thread::spawn(move || {
        let needle = wanted;
        let (mut total, mut tail) = (0usize, Vec::new());
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    total += n;
                    tail.extend_from_slice(&buf[..n]);
                    let keep = needle.len() + 2 * buf.len();
                    if tail.len() > keep {
                        tail.drain(..tail.len() - keep);
                    }
                    if String::from_utf8_lossy(&tail).contains(&needle) {
                        break;
                    }
                }
            }
        }
        let _ = tx.send((total, String::from_utf8_lossy(&tail).into_owned()));
    });
    rx.recv_timeout(Duration::from_secs(secs)).unwrap_or_else(|_| panic!("no {needle:?} within {secs} s"))
}

#[test]
fn a_pane_that_reads_normally_is_not_cut() {
    const SIZE: usize = 4 * 1024 * 1024;
    let w = World::new("flood-normal", |o| o.shell_flood(SIZE), true, "pw");
    let mut link = SshLink::attach(w.connection(), 80, 24).unwrap();
    let mut reader = link.take_reader().unwrap();
    let mut input = link.take_writer().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut all = Vec::new();
        let mut buf = vec![0u8; 8192];
        while !all.ends_with(b"flood done\r\n") {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => all.extend_from_slice(&buf[..n]),
            }
        }
        let _ = tx.send(all);
    });
    input.write_all(b"flood\n").unwrap();
    let all = rx.recv_timeout(Duration::from_secs(60)).expect("the flood was read to its end");
    assert!(all == flood_text(SIZE), "every byte arrived, in order ({} bytes of {})", all.len(), flood_text(SIZE).len());
    assert!(!all.contains(&0x1b), "no notice: the pane was not cut");
    assert_eq!(link.state(), ConnState::Connected);
    assert_eq!(link.backlog_bytes(), 0, "everything that was read is gone from the backlog");
    Box::new(link).close().unwrap();
}

#[test]
fn the_backlog_counter_is_exact() {
    const SIZE: usize = 2 * 1024 * 1024;
    let w = World::new("flood-counter", |o| o.shell_flood(SIZE), true, "pw");
    let mut link = SshLink::attach(w.connection(), 80, 24).unwrap();
    let mut reader = link.take_reader().unwrap();
    let mut input = link.take_writer().unwrap();
    input.write_all(b"flood\n").unwrap();
    let total = flood_text(SIZE).len();
    // Nothing is read: wait until the whole flood is queued, and then the count is exactly its size.
    wait_until("the whole flood queued", 30, || link.backlog_bytes() == total);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(link.backlog_bytes(), total, "bytes queued, nothing read");

    // Read in pieces of every size: queued minus read is the count at every sample.
    let mut read = 0usize;
    let mut buf = vec![0u8; 70_000];
    for step in 0..2000usize {
        let want = 1 + (step * 7919) % buf.len();
        let n = reader.read(&mut buf[..want]).unwrap();
        read += n;
        assert_eq!(link.backlog_bytes(), total - read, "after {read} bytes read");
        if read == total {
            break;
        }
    }
    while read < total {
        read += reader.read(&mut buf).unwrap();
        assert_eq!(link.backlog_bytes(), total - read);
    }
    assert_eq!(link.backlog_bytes(), 0, "after the reader drained everything");
    Box::new(link).close().unwrap();
}

#[test]
fn a_pane_that_never_reads_cannot_stall_the_files_of_the_host() {
    let w = World::new("flood-never", |o| o.shell_flood(64 * 1024 * 1024), true, "pw");
    let files = w.files();
    let exec = w.exec();
    let mut link = SshLink::attach(w.connection(), 80, 24).unwrap();
    let reader = link.take_reader().unwrap();
    let mut input = link.take_writer().unwrap();
    wait_until("the shell", 10, || link.backlog_bytes() > 0);
    input.write_all(b"flood\n").unwrap();

    // The pane's output is never read. While the flood runs, the files and the commands of the host go on.
    let started = Instant::now();
    let (mut during, mut highest, mut slowest) = (0, 0usize, Duration::ZERO);
    while link.state() != ConnState::Failed {
        assert!(started.elapsed() < Duration::from_secs(20), "the flood never reached the cap; backlog {}", link.backlog_bytes());
        let before = link.backlog_bytes();
        let t = Instant::now();
        assert_eq!(names(&files, w.root()).len(), 2);
        assert_eq!(run(&exec, "echo busy"), "ran:echo busy");
        slowest = slowest.max(t.elapsed());
        assert!(t.elapsed() < Duration::from_secs(2), "a listing and a command took {:?} during the flood", t.elapsed());
        if before > 64 * 1024 && before < MAX_BACKLOG {
            during += 1;
        }
        highest = highest.max(link.backlog_bytes());
        // The backlog never passes the cap by more than a chunk (in fact by nothing but a notice).
        assert!(highest <= MAX_BACKLOG + 16 * 1024, "the backlog grew to {highest}");
    }
    eprintln!("flood: {during} rounds during the flood, slowest {slowest:?}, highest backlog {highest} of {MAX_BACKLOG}");
    assert!(during >= 1, "no listing completed while the flood ran");
    assert!(link.backlog_bytes() <= MAX_BACKLOG + 16 * 1024);
    // Still fine after the cut.
    assert_eq!(names(&files, w.root()).len(), 2);
    assert_eq!(w.logins(), 1);
    drop(reader);
    Box::new(link).close().unwrap();
}

#[test]
fn past_the_cap_the_shell_is_closed_and_the_pane_says_why() {
    let w = World::new("flood-cap", |o| o.shell_flood(64 * 1024 * 1024), true, "pw");
    let files = w.files();
    let mut link = SshLink::attach(w.connection(), 80, 24).unwrap();
    let reader = link.take_reader().unwrap();
    let mut input = link.take_writer().unwrap();
    wait_until("the shell", 10, || link.backlog_bytes() > 0);
    input.write_all(b"flood\n").unwrap();
    wait_until("the cap", 20, || link.state() == ConnState::Failed);
    assert_eq!(link.try_wait().unwrap(), None, "the pane stays open and waits for Enter");
    assert!(link.backlog_bytes() >= MAX_BACKLOG - 16 * 1024, "the pane was cut at the cap, not before: {}", link.backlog_bytes());
    // The session stays up for the files.
    assert_eq!(names(&files, w.root()).len(), 2);
    assert_eq!(w.logins(), 1);
    assert_eq!(w.count(|s| &s.disconnects), 0);

    // The reader resumes: the screen holds the sentence, after what was already queued.
    let sentence = "Connection to 127.0.0.1 failed: the output arrived faster than the pane could show it. Press Enter to try again.";
    let (seen, tail) = read_until(reader, sentence, 30);
    assert!(seen >= MAX_BACKLOG - 32 * 1024, "the reader got the whole backlog first: {seen} bytes");
    assert!(tail.contains(sentence), "the sentence is missing: {tail:?}");
    assert_eq!(link.backlog_bytes(), 0, "the reader drained the backlog");
    Box::new(link).close().unwrap();
}

#[test]
fn the_pane_attach_and_the_standalone_connect_both_work() {
    // `connect` makes a connection of its own and ends its login when the pane closes; `attach` leaves the login.
    let w = World::plain("standalone");
    let mut target = w.server.target(holzwave_core::ssh::SshAuth::Password("pw".into()));
    target.known_hosts = Some(w.known());
    let mut link = SshLink::connect(target, 80, 24).unwrap();
    let mut out = Out::new(link.take_reader().unwrap());
    out.until("welcome 0", 10);
    Box::new(link).close().unwrap();
    wait_until("the login of a standalone pane to end", 10, || w.count(|s| &s.disconnects) == 1);
    assert_eq!(w.logins(), 1);
}

// ---- the pane itself (needs the terminal engine, so Zig) --------------------------------------------------------

#[cfg(feature = "ghostty")]
mod pane {
    use super::*;
    use std::sync::Mutex;

    use holzwave_core::pane::{PaneEvent, PaneHandle};
    use holzwave_core::wire::{FrameMsg, RowMsg};

    fn row_text(row: &RowMsg) -> String {
        row.runs.iter().flat_map(|run| run.0.chars()).filter(|c| *c != '\0').collect()
    }

    #[test]
    fn a_pane_with_a_pooled_connection_shows_the_remote_shell() {
        let w = World::plain("pane-level");
        w.connections.assign(1, "talos").unwrap();
        let mut spec = PaneSpec::new("sh", 80, 24);
        w.connections.apply(1, &mut spec).unwrap();
        assert!(spec.ssh_connection.is_some() && spec.ssh.is_some());

        let frames: Arc<Mutex<Vec<FrameMsg>>> = Arc::default();
        let sink = {
            let frames = frames.clone();
            move |event: PaneEvent| {
                if let PaneEvent::Frame(frame) = event {
                    frames.lock().unwrap().push(frame);
                }
            }
        };
        let pane = PaneHandle::spawn_ghostty(1, spec, sink).expect("the pane starts");
        let shows = |text: &str| frames.lock().unwrap().iter().any(|f| f.changed.iter().any(|r| row_text(r).contains(text)));
        assert!(common::wait_until(Duration::from_secs(10), || shows("welcome 0")), "the first frame shows the remote shell");
        // The file browser of the same profile costs no second login.
        assert_eq!(names(&w.files(), w.root()).len(), 2);
        assert_eq!(w.logins(), 1);
        pane.close();
        assert!(pane.join(Duration::from_secs(5)));
        assert_eq!(w.count(|s| &s.disconnects), 0, "closing the pane leaves the login");
    }
}
