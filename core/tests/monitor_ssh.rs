//! The system monitor of another computer, end to end against an in-process SSH server that answers the fixed monitor
//! command with the text of a Linux host. No Zig and no real host are needed.
//!
//! The server knows the user `holz` with the password `pw`. It records every command and every request that a monitor
//! must never make (a terminal, an environment variable, an agent), and answers each command from a queue of replies; the
//! last reply repeats.

use std::cell::Cell;
use std::collections::{BTreeSet, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use holzwave_core::blocks::{Block, BlockKind, Blocks};
use holzwave_core::connections::{Connections, KNOWN_HOSTS_FILE, SecretChange};
use holzwave_core::monitor::remote::{SAMPLE_TIMEOUT, ssh_monitor_with};
use holzwave_core::monitor::{Proc, Reading, RemoteMonitors, Snapshot, remote};
use holzwave_core::profiles::{Auth, Kind, Profile};
use holzwave_core::secrets::MemoryStore;
use holzwave_core::ssh::{ExecLimits, MAX_EXEC_OUTPUT, learn_host_key};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{self, Auth as ServerAuth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use tokio::net::TcpListener;

/// One piece of a reply, sent in chunks of at most 16 KiB.
#[derive(Clone)]
enum Part {
    Bytes(Vec<u8>),
    /// `bytes` sent `times` times, without ever holding the whole text.
    Repeat(Vec<u8>, usize),
}

/// What the server does with a command.
#[derive(Clone)]
enum Reply {
    /// Print the parts, then end with this exit status.
    Send(Vec<Part>, u32),
    /// Print nothing and never end.
    Hang,
}

impl Reply {
    fn text(text: &str) -> Self {
        Reply::Send(vec![Part::Bytes(text.as_bytes().to_vec())], 0)
    }
}

#[derive(Default)]
struct Seen {
    connections: AtomicUsize,
    ptys: AtomicUsize,
    envs: AtomicUsize,
    agents: AtomicUsize,
    commands: Mutex<Vec<String>>,
}

impl Seen {
    fn commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }
}

#[derive(Clone)]
struct Fake {
    seen: Arc<Seen>,
    replies: Arc<Mutex<VecDeque<Reply>>>,
}

impl Fake {
    /// The next reply; the last one stays and repeats.
    fn next_reply(&self) -> Reply {
        let mut queue = self.replies.lock().unwrap();
        if queue.len() > 1 { queue.pop_front().unwrap() } else { queue.front().cloned().unwrap_or(Reply::Hang) }
    }
}

impl server::Server for Fake {
    type Handler = Self;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Self {
        self.seen.connections.fetch_add(1, Ordering::SeqCst);
        self.clone()
    }
}

impl server::Handler for Fake {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<ServerAuth, Self::Error> {
        Ok(if user == "holz" && password == "pw" { ServerAuth::Accept } else { ServerAuth::reject() })
    }

    async fn channel_open_session(&mut self, _: Channel<Msg>, reply: server::ChannelOpenHandle, _: &mut Session) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        _: ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.seen.ptys.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn env_request(&mut self, _: ChannelId, _: &str, _: &str, _: &mut Session) -> Result<(), Self::Error> {
        self.seen.envs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn agent_request(&mut self, _: ChannelId, _: &mut Session) -> Result<bool, Self::Error> {
        self.seen.agents.fetch_add(1, Ordering::SeqCst);
        Ok(false)
    }

    async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        self.seen.commands.lock().unwrap().push(String::from_utf8_lossy(data).into_owned());
        session.channel_success(channel)?;
        match self.next_reply() {
            Reply::Hang => {}
            Reply::Send(parts, status) => {
                for part in parts {
                    let (bytes, times) = match part {
                        Part::Bytes(bytes) => (bytes, 1),
                        Part::Repeat(bytes, times) => (bytes, times),
                    };
                    for _ in 0..times {
                        for chunk in bytes.chunks(16 * 1024) {
                            session.data(channel, chunk.to_vec())?;
                        }
                    }
                }
                session.exit_status_request(channel, status)?;
                session.eof(channel)?;
                session.close(channel)?;
            }
        }
        Ok(())
    }
}

struct Server {
    addr: SocketAddr,
    key: PrivateKey,
    seen: Arc<Seen>,
}

impl Server {
    /// Start the server on a thread of its own; it runs until the test process ends. Its replies are `replies`.
    fn start(replies: Vec<Reply>) -> Self {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let config = Arc::new(server::Config {
            keys: vec![key.clone()],
            auth_rejection_time: Duration::from_millis(0),
            auth_rejection_time_initial: Some(Duration::from_millis(0)),
            ..Default::default()
        });
        let seen = Arc::new(Seen::default());
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let mut fake = Fake { seen: seen.clone(), replies: replies.clone() };
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let socket = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                tx.send(socket.local_addr().unwrap()).unwrap();
                let _ = fake.run_on_socket(config, &socket).await;
            });
        });
        Server { addr: rx.recv().unwrap(), key, seen }
    }
}

/// A scratch directory that is removed when the test ends, also when it fails.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("holzwave-monitor-ssh-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The saved connection `talos` for the server, over an in-memory keychain.
struct World {
    dir: Scratch,
    connections: Connections,
}

impl World {
    /// Save `talos` and learn the server's key, as opening the connection once would.
    fn new(name: &str, server: &Server) -> Self {
        let world = Self::without_key(name, server);
        learn_host_key("127.0.0.1", server.addr.port(), server.key.public_key(), &world.dir.path().join(KNOWN_HOSTS_FILE)).unwrap();
        world
    }

    /// The same, with an existing known hosts file that does not hold the server's key.
    fn without_key(name: &str, server: &Server) -> Self {
        let dir = Scratch::new(name);
        std::fs::write(dir.path().join(KNOWN_HOSTS_FILE), "").unwrap();
        let (connections, _) = Connections::load(dir.path(), Arc::new(MemoryStore::default()), None, &[]);
        let world = World { dir, connections };
        world.save_talos(server.addr.port());
        world
    }

    fn save_talos(&self, port: u16) {
        let profile = Profile { port, user: "holz".into(), auth: Auth::Password, ..Profile::new("talos", "Talos", "127.0.0.1") };
        self.connections.save(profile, SecretChange::Set { value: "pw".into() }).unwrap();
    }
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/monitor").join(name)).unwrap()
}

fn proc(pid: u32, name: &str, cpu: f32, mem: u64) -> Proc {
    Proc { pid, name: name.into(), cpu, mem }
}

/// What fixture A alone says: a first reading, with no rates.
fn facts_of_a() -> Snapshot {
    Snapshot {
        cpu: 0.0,
        cores: 4,
        mem_used: 6_537_216_000,
        mem_total: 16_777_216_000,
        net_rx: 0,
        net_tx: 0,
        uptime: 5400,
        procs: vec![
            proc(842, "node server", 0.0, 204_800_000),
            proc(977, "a (b) c", 0.0, 81_920_000),
            proc(1, "systemd", 0.0, 12_288_000),
            proc(7, "kworker", 0.0, 409_600),
        ],
    }
}

/// What fixture B says after fixture A, two seconds later.
fn facts_of_b() -> Snapshot {
    Snapshot {
        cpu: 37.5,
        cores: 4,
        mem_used: 7_561_216_000,
        mem_total: 16_777_216_000,
        net_rx: 2_100_000,
        net_tx: 500_000,
        uptime: 5402,
        procs: vec![
            proc(842, "node server", 200.0, 204_800_000),
            proc(1, "systemd", 10.0, 12_288_000),
            proc(977, "a (b) c", 0.0, 81_920_000),
            proc(1500, "new", 0.0, 4_096_000),
            proc(7, "kworker", 0.0, 2_048_000),
        ],
    }
}

fn live(reading: Result<Reading, String>) -> Snapshot {
    match reading {
        Ok(Reading::Live(snapshot)) => snapshot,
        other => panic!("not a live reading: {other:?}"),
    }
}

#[test]
fn a_saved_connection_gives_a_live_reading_through_the_fake_server() {
    let server = Server::start(vec![Reply::text(&fixture("linux_a.txt")), Reply::text(&fixture("linux_b.txt"))]);
    let world = World::new("tracer", &server);
    let monitors = RemoteMonitors::new();

    assert_eq!(live(monitors.sample(&world.connections, "talos")), facts_of_a());

    // The second sample, one second later by the clock the monitor is given.
    let monitor = monitors
        .get_or_build("talos", world.connections.generation(), || Err("the monitor of this generation exists".into()))
        .expect("the monitor of the first sample is kept");
    let now = std::cell::Cell::new(Instant::now());
    now.set(now.get() + Duration::from_secs(1));
    let second = live(monitor.sample_with(&|| now.get()));
    assert_eq!(second, facts_of_b());
    assert_eq!(second.procs.iter().map(|p| (p.pid, p.name.as_str())).collect::<Vec<_>>(), [(842, "node server"), (1, "systemd"), (977, "a (b) c"), (1500, "new"), (7, "kworker")]);

    assert_eq!(server.seen.connections.load(Ordering::SeqCst), 1, "one session for both samples");
    assert_eq!(server.seen.commands().len(), 2, "one command each");

    // A monitor block may name the saved connection, and the page is told its name.
    let blocks = Blocks::load(world.dir.path(), None, &[1]);
    let block = Block { kind: BlockKind::Monitor, source: "talos".into(), path: String::new(), edit: false };
    blocks.assign(1, block).unwrap();
    let views = blocks.views(&world.connections);
    assert_eq!(views.len(), 1);
    assert_eq!((views[0].pane, views[0].source_label.as_str()), (1, "Talos"));
    assert!(blocks.assign(2, Block { kind: BlockKind::Monitor, source: "talos".into(), path: "/x".into(), edit: false }).is_err());
    assert!(blocks.assign(2, Block { kind: BlockKind::Monitor, source: String::new(), path: String::new(), edit: false }).is_err());
}

/// A clock that stands still until it is moved, so the sharing of young samples does not depend on the machine's speed.
struct Clock {
    start: Instant,
    at: Cell<Duration>,
}

impl Clock {
    fn new() -> Self {
        Clock { start: Instant::now(), at: Cell::new(Duration::ZERO) }
    }

    fn after(&self, secs: u64) -> impl Fn() -> Instant + '_ {
        self.at.set(Duration::from_secs(secs));
        move || self.start + self.at.get()
    }
}

#[test]
fn the_command_is_the_fixed_script_and_asks_for_no_terminal_environment_or_agent() {
    let server = Server::start(vec![Reply::text(&fixture("linux_a.txt")), Reply::text(&fixture("linux_b.txt"))]);
    let world = World::new("fixed", &server);
    let monitors = RemoteMonitors::new();
    let clock = Clock::new();
    live(monitors.sample_with(&world.connections, "talos", &clock.after(0)));
    live(monitors.sample_with(&world.connections, "talos", &clock.after(2)));
    let commands = server.seen.commands();
    assert_eq!(commands, vec![remote::command(), remote::command()], "the recorded command is the constant, byte for byte");
    assert_eq!(commands[0], format!("exec sh -c -- '{}'", remote::SCRIPT));
    assert_eq!(server.seen.ptys.load(Ordering::SeqCst), 0, "no terminal");
    assert_eq!(server.seen.envs.load(Ordering::SeqCst), 0, "no environment");
    assert_eq!(server.seen.agents.load(Ordering::SeqCst), 0, "no agent");
    assert_eq!(server.seen.connections.load(Ordering::SeqCst), 1);
}

#[test]
fn a_host_that_is_not_linux_is_unsupported_and_asked_again_only_after_half_a_minute() {
    let server = Server::start(vec![Reply::text(&fixture("darwin.txt"))]);
    let world = World::new("darwin", &server);
    let monitors = RemoteMonitors::new();
    let clock = Clock::new();
    let expected = Reading::Unsupported { system: "Darwin".into() };
    assert_eq!(monitors.sample_with(&world.connections, "talos", &clock.after(0)), Ok(expected.clone()));
    assert_eq!(monitors.sample_with(&world.connections, "talos", &clock.after(10)), Ok(expected.clone()));
    assert_eq!(server.seen.commands().len(), 1, "10 s on: no command");
    assert_eq!(monitors.sample_with(&world.connections, "talos", &clock.after(31)), Ok(expected));
    assert_eq!(server.seen.commands().len(), 2, "31 s on: one command");
}

#[test]
fn a_host_without_a_posix_shell_gives_an_empty_answer_and_is_unsupported() {
    let server = Server::start(vec![Reply::Send(Vec::new(), 127)]);
    let world = World::new("noshell", &server);
    let monitors = RemoteMonitors::new();
    assert_eq!(monitors.sample(&world.connections, "talos"), Ok(Reading::Unsupported { system: String::new() }));
}

#[test]
fn an_unknown_host_key_is_refused_with_a_hint_and_nothing_is_learned() {
    let server = Server::start(vec![Reply::text(&fixture("linux_a.txt"))]);
    let world = World::without_key("unknown", &server);
    let known = world.dir.path().join(KNOWN_HOSTS_FILE);
    let before = std::fs::read(&known).unwrap();
    let monitors = RemoteMonitors::new();
    let error = monitors.sample(&world.connections, "talos").unwrap_err();
    assert_eq!(error, remote::HOST_KEY_HINT);
    assert_eq!(error, "The host key is not known yet. Connect to this host once, then try again.");
    assert_eq!(std::fs::read(&known).unwrap(), before, "the known hosts file is as it was");
    assert!(server.seen.commands().is_empty(), "no command reached the server");
}

/// The resident memory of this process in kilobytes, where `ps` can say it.
fn resident_kb() -> Option<u64> {
    let out = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

#[test]
fn a_flood_of_process_lines_loses_its_front_and_keeps_the_counters() {
    let b = fixture("linux_b.txt");
    let footer = b[b.find("\n@@uname").unwrap()..].to_string();
    let line = "4242 (flood) S 0 1 1 0 -1 4194560 12000 400000 30 200 1 1 0 0 20 0 1 0 10 174587904 100 18446744073709551615 1 1 0 0\n";
    let times = 1_572_864 / line.len();
    let reply = Reply::Send(vec![Part::Bytes(b"@@begin\n".to_vec()), Part::Repeat(line.as_bytes().to_vec(), times), Part::Bytes(footer.into_bytes())], 0);
    let server = Server::start(vec![reply]);
    let world = World::new("flood", &server);
    let monitors = RemoteMonitors::new();
    // The first flood warms the allocator and the connection; the memory is read around the second one, so what is
    // measured is what a flood adds, not what the first connection and the other tests of this file hold.
    let clock = Clock::new();
    live(monitors.sample_with(&world.connections, "talos", &clock.after(0)));
    let before = resident_kb();
    let snapshot = live(monitors.sample_with(&world.connections, "talos", &clock.after(2)));
    assert_eq!((snapshot.mem_used, snapshot.mem_total, snapshot.uptime), (7_561_216_000, 16_777_216_000, 5402));
    assert!(snapshot.procs.len() <= 8);
    assert!(snapshot.procs.iter().all(|p| p.pid == 4242), "only the flood is left of the process list");
    if let (Some(before), Some(after)) = (before, resident_kb()) {
        assert!(after.saturating_sub(before) < 8 * 1024, "the process grew by {} KiB", after.saturating_sub(before));
    }
}

#[test]
fn a_host_that_never_answers_is_no_answer_at_the_timeout() {
    let server = Server::start(vec![Reply::Hang]);
    let world = World::new("hang", &server);
    let target = world.connections.ssh_target("talos").unwrap();
    let monitor = ssh_monitor_with("Talos", target, ExecLimits { max_output: MAX_EXEC_OUTPUT, timeout: Duration::from_millis(300) }).unwrap();
    let started = Instant::now();
    assert_eq!(monitor.sample(), Err("the host did not answer in time".to_string()));
    assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
    assert!(SAMPLE_TIMEOUT >= Duration::from_secs(10));
}

#[test]
fn a_saved_connection_that_changed_is_sampled_on_a_new_session() {
    let server = Server::start(vec![Reply::text(&fixture("linux_a.txt"))]);
    let world = World::new("changed", &server);
    let monitors = RemoteMonitors::new();
    let clock = Clock::new();
    live(monitors.sample_with(&world.connections, "talos", &clock.after(0)));
    assert_eq!(server.seen.connections.load(Ordering::SeqCst), 1);
    // Saving the connection again, as an edit does, moves its generation.
    world.save_talos(server.addr.port());
    live(monitors.sample_with(&world.connections, "talos", &clock.after(1)));
    assert_eq!(server.seen.connections.load(Ordering::SeqCst), 2, "the next sample opens connection number 2");
    assert_eq!(monitors.len(), 1);
}

#[test]
fn leaving_the_last_pane_of_a_host_drops_its_session() {
    let server = Server::start(vec![Reply::text(&fixture("linux_a.txt"))]);
    let world = World::new("leave", &server);
    let monitors = RemoteMonitors::new();
    live(monitors.sample(&world.connections, "talos"));
    assert_eq!(monitors.len(), 1);
    monitors.keep_only(&BTreeSet::new());
    assert_eq!(monitors.len(), 0);
}

#[test]
fn a_missing_connection_and_a_non_ssh_connection_are_refused_with_their_reason() {
    let server = Server::start(vec![Reply::text(&fixture("linux_a.txt"))]);
    let world = World::new("refused", &server);
    let monitors = RemoteMonitors::new();
    let missing = monitors.sample(&world.connections, "nope").unwrap_err();
    assert_eq!(missing, "no connection nope");
    let wsl = Profile { kind: Kind::Wsl, ..Profile::new("box", "Box", "Ubuntu") };
    world.connections.save(wsl, SecretChange::Keep).unwrap();
    let not_ssh = monitors.sample(&world.connections, "box").unwrap_err();
    assert!(not_ssh.contains("is not an SSH connection"), "{not_ssh}");
    assert!(!missing.starts_with("unsupported") && !not_ssh.starts_with("unsupported"));
    assert!(server.seen.commands().is_empty());
    assert_eq!(monitors.len(), 0, "a refused connection stores nothing");
}
