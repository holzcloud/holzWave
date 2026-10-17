//! The system monitor of another computer, end to end against an in-process SSH server that answers the fixed monitor
//! command with the text of a Linux host. No Zig and no real host are needed.
//!
//! The server knows the user `holz` with the password `pw`. It records every command and every request that a monitor
//! must never make (a terminal, an environment variable, an agent), and answers each command from a queue of replies; the
//! last reply repeats.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use holzwave_core::blocks::{Block, BlockKind, Blocks};
use holzwave_core::connections::{Connections, KNOWN_HOSTS_FILE, SecretChange};
use holzwave_core::monitor::{Proc, Reading, RemoteMonitors, Snapshot};
use holzwave_core::profiles::{Auth, Profile};
use holzwave_core::secrets::MemoryStore;
use holzwave_core::ssh::learn_host_key;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{self, Auth as ServerAuth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use tokio::net::TcpListener;

/// One piece of a reply, sent in chunks of at most 16 KiB.
#[derive(Clone)]
enum Part {
    Bytes(Vec<u8>),
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
                    let Part::Bytes(bytes) = part;
                    for chunk in bytes.chunks(16 * 1024) {
                        session.data(channel, chunk.to_vec())?;
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
    let block = Block { kind: BlockKind::Monitor, source: "talos".into(), path: String::new() };
    blocks.assign(1, block).unwrap();
    let views = blocks.views(&world.connections);
    assert_eq!(views.len(), 1);
    assert_eq!((views[0].pane, views[0].source_label.as_str()), (1, "Talos"));
    assert!(blocks.assign(2, Block { kind: BlockKind::Monitor, source: "talos".into(), path: "/x".into() }).is_err());
    assert!(blocks.assign(2, Block { kind: BlockKind::Monitor, source: String::new(), path: String::new() }).is_err());
}
