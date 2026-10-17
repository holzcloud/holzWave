//! The pooled SSH connection of a saved profile, against the shared in-process SSH server: one login for the file browser
//! and the command runner, single flight, loss, cooldown, idle, the host-key rule and a caller that ends.
//!
//! Time is moved by giving the clock-taking functions (`session_at`, `reap`, `cooldown_left`) an `Instant` in the
//! future, so no test sleeps for a cooldown or an idle time.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use common::ssh_server::{self, Options, Server};
use holzwave_core::blocks::Blocks;
use holzwave_core::connections::{Connections, KNOWN_HOSTS_FILE, SecretChange};
use holzwave_core::files::{FileSource, SftpSource};
use holzwave_core::monitor::{Reading, RemoteMonitors};
use holzwave_core::pool::{COOLDOWN_FATAL, COOLDOWN_LOST, Connection, IDLE, MAX_CONNECTIONS, Open, SessionPool};
use holzwave_core::profiles::{Auth, Kind, Profile};
use holzwave_core::secrets::MemoryStore;
use holzwave_core::ssh::{ExecCancel, ExecLimits, HostKeyPolicy, SshAuth, SshExec, SshTarget, learn_host_key};

/// A scratch directory that is removed when the test ends, also when it fails.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        Scratch(common::scratch_dir(tag))
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

/// A server with an SFTP root of two files, the key of the server learned, and a connection to it.
struct Rig {
    /// Removed when the rig goes.
    _dir: Scratch,
    root: PathBuf,
    server: Server,
    conn: Arc<Connection>,
}

impl Rig {
    fn new(tag: &str) -> Self {
        Self::with(tag, SshAuth::Password("pw".into()))
    }

    fn with(tag: &str, auth: SshAuth) -> Self {
        let dir = Scratch::new(tag);
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        // The server speaks canonical paths (on a Mac the temporary folder is behind a link).
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("a.txt"), "alpha").unwrap();
        std::fs::write(root.join("b.txt"), "beta").unwrap();
        let server = ssh_server::start(Options::password().sftp(root.clone()));
        let known = dir.path().join("known_hosts");
        server.trust(&known);
        let conn = Connection::standalone("Test host", server.exec_target(auth, &known)).unwrap();
        Rig { _dir: dir, root, server, conn }
    }

    fn files(&self) -> SftpSource {
        SftpSource::on(self.conn.clone())
    }

    fn exec(&self) -> SshExec {
        SshExec::on(self.conn.clone()).unwrap()
    }

    fn root(&self) -> &str {
        self.root.to_str().unwrap()
    }

    fn seen(&self, counter: impl Fn(&ssh_server::Seen) -> usize) -> usize {
        counter(&self.server.seen)
    }

    fn connections(&self) -> usize {
        self.seen(|s| s.connections.load(Ordering::SeqCst))
    }

    fn logins(&self) -> usize {
        self.seen(|s| s.logins.load(Ordering::SeqCst))
    }

    fn refused(&self) -> usize {
        self.seen(|s| s.refused_logins.load(Ordering::SeqCst))
    }
}

fn run(exec: &SshExec, command: &str) -> String {
    let result = exec.exec(command, &ExecLimits::default(), &ExecCancel::new()).unwrap();
    assert_eq!(result.status, Some(0), "{command}");
    String::from_utf8_lossy(&result.stdout).trim().to_owned()
}

fn names(source: &dyn FileSource, root: &str) -> Vec<String> {
    source.list(root).unwrap().entries.into_iter().map(|e| e.name).collect()
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    assert!(common::wait_until(Duration::from_secs(10), &mut condition), "no {what} within 10 s");
}

/// Run a future to its end on a runtime of its own, the way a caller that is not the connection's does.
fn pollster<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(future)
}

// ---- the tracer -------------------------------------------------------------------------------------------------

#[test]
fn one_login_serves_files_and_commands() {
    let rig = Rig::new("one-login");
    let files = rig.files();
    assert_eq!(names(&files, rig.root()), ["a.txt", "b.txt"]);
    assert_eq!(files.read(&format!("{}/a.txt", rig.root()), 1024).unwrap(), b"alpha");
    let exec = rig.exec();
    assert_eq!(run(&exec, "echo ok"), "ran:echo ok");
    assert_eq!(names(&files, rig.root()).len(), 2, "the file browser still works after the command");

    assert_eq!(rig.connections(), 1, "one TCP connection for the file browser and the command runner");
    assert_eq!(rig.logins(), 1, "one successful login");
    assert_eq!(rig.refused(), 0);
    assert_eq!(rig.server.seen.sftp_sessions.load(Ordering::SeqCst), 1, "one SFTP session");
    assert_eq!(rig.server.seen.commands(), ["echo ok"], "one exec channel");
}

#[test]
fn a_session_made_on_one_runtime_outlives_that_runtime() {
    let rig = Rig::new("foreign-runtime");
    let conn = rig.conn.clone();
    // The first caller has a short-lived runtime of its own, as a pane's thread has. It makes the login through it and
    // ends.
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async { conn.session(Open::Interactive).await.map(|_| ()) }).unwrap();
        drop(runtime);
    })
    .join()
    .unwrap();
    assert_eq!(rig.logins(), 1);
    assert!(rig.conn.is_connected(), "the session did not go with the caller's runtime");

    // Another thread uses the same session: no new login.
    let files = rig.files();
    let root = rig.root().to_owned();
    let listed = thread::spawn(move || names(&files, &root)).join().unwrap();
    assert_eq!(listed, ["a.txt", "b.txt"]);
    let exec = rig.exec();
    assert_eq!(thread::spawn(move || run(&exec, "echo again")).join().unwrap(), "ran:echo again");
    assert_eq!(rig.connections(), 1);
    assert_eq!(rig.logins(), 1, "still one login");
}

// ---- the pool and the saved profile ----------------------------------------------------------------------------

/// The saved connection `talos` for the server, over an in-memory keychain.
struct World {
    dir: Scratch,
    connections: Connections,
}

impl World {
    /// Save `talos` (password `pw`); `learn` puts the server's key into the known hosts file, as a pane would have.
    fn new(tag: &str, server: &Server, learn: bool) -> Self {
        let dir = Scratch::new(tag);
        let known = dir.path().join(KNOWN_HOSTS_FILE);
        if learn {
            server.trust(&known);
        } else {
            std::fs::write(&known, "").unwrap();
        }
        let (connections, _) = Connections::load(dir.path(), Arc::new(MemoryStore::default()), None, &[]);
        let world = World { dir, connections };
        world.save("pw", server.addr.port());
        world
    }

    fn save(&self, password: &str, port: u16) {
        let profile = Profile { port, user: "holz".into(), auth: Auth::Password, ..Profile::new("talos", "Talos", "127.0.0.1") };
        self.connections.save(profile, SecretChange::Set { value: password.into() }).unwrap();
    }

    fn known(&self) -> PathBuf {
        self.dir.path().join(KNOWN_HOSTS_FILE)
    }

    /// The agent's command on the connection of the saved profile.
    fn agent(&self) -> SshExec {
        SshExec::on(self.connections.session("talos").unwrap()).unwrap()
    }
}

#[test]
fn a_saved_profile_gets_one_connection_per_generation() {
    let server = ssh_server::start(Options::password());
    let world = World::new("generation", &server, true);
    let one = world.connections.session("talos").unwrap();
    let again = world.connections.session("talos").unwrap();
    assert!(Arc::ptr_eq(&one, &again), "the same version of the list gives the same connection");
    assert_eq!(one.label(), "Talos");

    world.save("pw", server.addr.port());
    let next = world.connections.session("talos").unwrap();
    assert!(!Arc::ptr_eq(&one, &next), "a save gives a new connection");
    assert!(Arc::ptr_eq(&next, &world.connections.session("talos").unwrap()));

    world.connections.delete("talos").unwrap();
    let missing = world.connections.session("talos").unwrap_err().to_string();
    assert!(missing.contains("no connection talos"), "{missing}");

    let wsl = Profile { kind: Kind::Wsl, ..Profile::new("box", "Box", "Ubuntu") };
    world.connections.save(wsl, SecretChange::Keep).unwrap();
    let not_ssh = world.connections.session("box").unwrap_err().to_string();
    assert!(not_ssh.contains("Box is not an SSH connection"), "{not_ssh}");
    assert!(world.connections.session("nope").unwrap_err().to_string().contains("no connection nope"));
}

#[test]
fn the_file_browser_the_monitor_and_the_agent_share_one_login() {
    let dir = Scratch::new("share-root");
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::write(root.join("a.txt"), "alpha").unwrap();
    let reading = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/monitor/linux_a.txt")).unwrap();
    let server = ssh_server::start(Options::password().sftp(root.clone()).exec_reply(reading));
    let world = World::new("share", &server, true);

    // The file browser: a listing through `Blocks::source`.
    let blocks = Blocks::load(world.dir.path(), None, &[]);
    let listing = blocks.list(&world.connections, "talos", root.to_str().unwrap()).unwrap();
    assert_eq!(listing.entries.len(), 1);
    // The monitor of the host: its fixed command, answered with a Linux reading.
    let monitors = RemoteMonitors::new();
    assert!(matches!(monitors.sample(&world.connections, "talos"), Ok(Reading::Live(_))));
    // The agent's command: the same connection of the same profile.
    let agent = SshExec::on(world.connections.session("talos").unwrap()).unwrap();
    assert!(!run(&agent, "echo agent").is_empty(), "the server prints its fixed reply");

    assert_eq!(server.seen.connections.load(Ordering::SeqCst), 1, "one TCP connection");
    assert_eq!(server.seen.logins.load(Ordering::SeqCst), 1, "one login");
    assert_eq!(server.seen.sftp_sessions.load(Ordering::SeqCst), 1);
    let commands = server.seen.commands();
    assert_eq!(commands.len(), 2, "the monitor's command and the agent's");
    assert_eq!(commands[1], "echo agent");
}

#[test]
fn at_most_sixteen_connections_are_kept() {
    let pool = SessionPool::new();
    let make = |id: &str, generation: u64| {
        pool.connection(id, generation, || Ok((id.to_owned(), SshTarget::new("127.0.0.1", "holz", SshAuth::Password("pw".into())))))
    };
    // Seventeen ids, none in use: the least recently used is evicted for the newest and the pool stays at the bound.
    let mut kept = Vec::new();
    for n in 0..=MAX_CONNECTIONS {
        kept.push(make(&format!("id{n}"), n as u64).unwrap());
        assert!(pool.len() <= MAX_CONNECTIONS);
    }
    assert_eq!(pool.len(), MAX_CONNECTIONS);

    // With every one in use the next fails in words.
    let pool = SessionPool::new();
    let leased: Vec<_> = (0..MAX_CONNECTIONS)
        .map(|n| {
            let connection = make_in(&pool, &format!("id{n}"));
            let lease = connection.lease();
            (connection, lease)
        })
        .collect();
    let refused = pool
        .connection("one-more", 1, || Ok(("x".into(), SshTarget::new("127.0.0.1", "holz", SshAuth::Password("pw".into())))))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("too many connections are open"), "{refused}");
    assert_eq!(pool.len(), MAX_CONNECTIONS);
    // Letting one go makes room again.
    let mut leased = leased;
    leased.pop();
    pool.connection("one-more", 1, || Ok(("x".into(), SshTarget::new("127.0.0.1", "holz", SshAuth::Password("pw".into()))))).unwrap();
    assert_eq!(pool.len(), MAX_CONNECTIONS);
}

fn make_in(pool: &SessionPool, id: &str) -> Arc<Connection> {
    pool.connection(id, 1, || Ok((id.to_owned(), SshTarget::new("127.0.0.1", "holz", SshAuth::Password("pw".into()))))).unwrap()
}

// ---- the life of a session ---------------------------------------------------------------------------------------

#[test]
fn eight_threads_at_once_make_one_login() {
    let rig = Rig::new("eight");
    let barrier = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|n| {
            let barrier = barrier.clone();
            let files = rig.files();
            let exec = rig.exec();
            let root = rig.root().to_owned();
            thread::spawn(move || {
                barrier.wait();
                if n % 2 == 0 {
                    assert_eq!(names(&files, &root), ["a.txt", "b.txt"]);
                } else {
                    assert_eq!(run(&exec, &format!("echo {n}")), format!("ran:echo {n}"));
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(rig.connections(), 1);
    assert_eq!(rig.logins(), 1, "eight users at once, one login");
}

#[test]
fn a_closed_session_is_replaced_once_for_every_user() {
    let rig = Rig::new("replaced");
    let files = rig.files();
    let exec = rig.exec();
    assert_eq!(names(&files, rig.root()).len(), 2);
    assert_eq!(run(&exec, "echo first"), "ran:echo first");
    assert_eq!(rig.logins(), 1);

    rig.server.cut();
    wait_until("the client to see the cut", || !rig.conn.is_connected());

    // Both users find the session closed and share the one new login.
    assert_eq!(names(&files, rig.root()).len(), 2);
    assert_eq!(run(&exec, "echo second"), "ran:echo second");
    assert_eq!(rig.logins(), 2, "exactly one more login in total");
    assert_eq!(rig.connections(), 2);
}

#[test]
fn a_cut_is_recovered_from_whether_or_not_the_client_has_noticed_it() {
    // No wait for the client between the cut and the next request: the request may find the session closed, or fail on
    // it and be tried once more. Either way the user sees no error and the server sees one more login.
    for round in 1..=5 {
        let rig = Rig::new("cut-at-once");
        let files = rig.files();
        let exec = rig.exec();
        assert_eq!(names(&files, rig.root()).len(), 2);
        assert_eq!(run(&exec, "echo first"), "ran:echo first");
        rig.server.cut();
        // The server has dropped the connection; the client may or may not have seen it yet.
        wait_until("the server to drop the connection", || rig.server.seen.disconnects.load(Ordering::SeqCst) == 1);
        assert_eq!(names(&files, rig.root()).len(), 2, "round {round}: the listing after the cut");
        assert_eq!(run(&exec, "echo second"), "ran:echo second", "round {round}");
        assert_eq!(rig.logins(), 2, "round {round}");
    }
}

#[test]
fn a_background_open_waits_after_a_refused_login() {
    let rig = Rig::with("refused", SshAuth::Password("wrong".into()));
    let conn = rig.conn.clone();
    let t0 = Instant::now();
    let first = pollster(conn.session_at(Open::Pinned, t0)).unwrap_err();
    assert!(first.fatal, "{first}");
    assert!(first.why.contains("refused the login"), "{first}");
    assert_eq!(rig.refused(), 1);
    assert_eq!(conn.cooldown_left(t0), Some(COOLDOWN_FATAL));

    // A second background open inside the cooldown gets the same reason and makes no attempt.
    let second = pollster(conn.session_at(Open::Pinned, t0 + Duration::from_secs(5))).unwrap_err();
    assert_eq!(second, first);
    assert_eq!(rig.refused(), 1, "no network attempt during the cooldown");
    assert_eq!(rig.connections(), 1);

    // A person's own open in the same second makes its attempt.
    let own = pollster(conn.session_at(Open::Interactive, t0 + Duration::from_secs(5))).unwrap_err();
    assert!(own.why.contains("refused the login"));
    assert_eq!(rig.refused(), 2, "an interactive open is never made to wait");

    // After the cooldown a background open tries again. (The interactive failure at +5 s moved the record.)
    let later = t0 + Duration::from_secs(5) + COOLDOWN_FATAL;
    assert_eq!(conn.cooldown_left(later), None);
    pollster(conn.session_at(Open::Pinned, later)).unwrap_err();
    assert_eq!(rig.refused(), 3, "after the cooldown a background open tries again");
}

#[test]
fn a_lost_connection_waits_less() {
    // Nobody listens on this port: the connection is refused, which is a lost connection and not a refused login.
    let free = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    let dir = Scratch::new("lost");
    let known = dir.path().join("known_hosts");
    std::fs::write(&known, "").unwrap();
    let mut target = SshTarget::new("127.0.0.1", "holz", SshAuth::Password("pw".into()));
    target.port = port;
    target.known_hosts = Some(known);
    let conn = Connection::standalone("Nowhere", target).unwrap();
    let t0 = Instant::now();
    let error = pollster(conn.session_at(Open::Pinned, t0)).unwrap_err();
    assert!(!error.fatal, "{error}");
    assert_eq!(conn.cooldown_left(t0), Some(COOLDOWN_LOST), "3 s and not 15 s");
    assert_eq!(conn.cooldown_left(t0 + Duration::from_secs(2)), Some(Duration::from_secs(1)));
    assert_eq!(conn.cooldown_left(t0 + COOLDOWN_LOST), None);
    // Inside the cooldown the same reason comes back at once.
    assert_eq!(pollster(conn.session_at(Open::Pinned, t0 + Duration::from_secs(1))).unwrap_err(), error);
}

#[test]
fn an_idle_session_is_closed_and_a_leased_one_is_not() {
    let server = ssh_server::start(Options::password());
    let dir = Scratch::new("idle");
    let known = dir.path().join("known_hosts");
    server.trust(&known);
    let pool = SessionPool::new();
    let make = |id: &str| {
        pool.connection(id, 1, || Ok((id.to_owned(), server.exec_target(SshAuth::Password("pw".into()), &known)))).unwrap()
    };
    let (idle, busy) = (make("idle"), make("busy"));
    let run_on = |conn: &Arc<Connection>| run(&SshExec::on(conn.clone()).unwrap(), "echo hi");
    assert_eq!(run_on(&idle), "ran:echo hi");
    assert_eq!(run_on(&busy), "ran:echo hi");
    assert_eq!(server.seen.logins.load(Ordering::SeqCst), 2);
    let lease = busy.lease();

    let late = Instant::now() + IDLE + Duration::from_secs(1);
    pool.reap(late);
    wait_until("the idle session to be closed", || server.seen.disconnects.load(Ordering::SeqCst) == 1);
    assert!(!idle.is_connected());
    assert!(busy.is_connected(), "a leased session is never closed");

    drop(lease);
    // The lease moved the clock of the last use to now, so the next 121 s count from there.
    pool.reap(Instant::now() + IDLE + Duration::from_secs(1));
    wait_until("the second session to be closed", || server.seen.disconnects.load(Ordering::SeqCst) == 2);
    assert!(!busy.is_connected());

    // The next use logs in again.
    assert_eq!(run_on(&idle), "ran:echo hi");
    assert_eq!(server.seen.logins.load(Ordering::SeqCst), 3);
}

#[test]
fn a_fresh_use_resets_the_idle_clock() {
    let server = ssh_server::start(Options::password());
    let dir = Scratch::new("fresh-use");
    let known = dir.path().join("known_hosts");
    server.trust(&known);
    let pool = SessionPool::new();
    let conn = pool.connection("a", 1, || Ok(("a".into(), server.exec_target(SshAuth::Password("pw".into()), &known)))).unwrap();
    let t0 = Instant::now();
    pollster(conn.session_at(Open::Interactive, t0)).unwrap();
    // A use at 100 s keeps the session through a reap at 150 s.
    pollster(conn.session_at(Open::Interactive, t0 + Duration::from_secs(100))).unwrap();
    pool.reap(t0 + Duration::from_secs(150));
    thread::sleep(Duration::from_millis(200));
    assert!(conn.is_connected());
    assert_eq!(server.seen.disconnects.load(Ordering::SeqCst), 0);
    // 121 s after that use it goes.
    pool.reap(t0 + Duration::from_secs(100) + IDLE);
    wait_until("the session to be closed", || server.seen.disconnects.load(Ordering::SeqCst) == 1);
}

// ---- the host-key rule belongs to the opener --------------------------------------------------------------------

/// What a command on the saved profile `talos` says when it fails.
fn run_error(exec: &SshExec, command: &str) -> String {
    exec.exec(command, &ExecLimits::default(), &ExecCancel::new()).unwrap_err().to_string()
}

#[test]
fn a_command_runner_never_learns_a_host_key() {
    let server = ssh_server::start(Options::password());
    let world = World::new("runner-learns-nothing", &server, false);
    let before = std::fs::read(world.known()).unwrap();
    let error = run_error(&world.agent(), "echo hi");
    assert!(error.contains("not known"), "{error}");
    assert_eq!(std::fs::read(world.known()).unwrap(), before, "the known hosts file is as it was");
    assert_eq!(server.seen.logins.load(Ordering::SeqCst), 0, "no login was made on an unknown host");
    assert!(server.seen.commands().is_empty());
}

#[test]
fn a_file_browser_open_learns_the_key_and_the_command_runner_may_then_use_the_session() {
    let dir = Scratch::new("browser-learns-root");
    let root = dir.path().join("root").to_owned();
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::write(root.join("a.txt"), "alpha").unwrap();
    let server = ssh_server::start(Options::password().sftp(root.clone()));
    let world = World::new("browser-learns", &server, false);
    assert!(std::fs::read(world.known()).unwrap().is_empty());

    let blocks = Blocks::load(world.dir.path(), None, &[]);
    assert_eq!(blocks.list(&world.connections, "talos", root.to_str().unwrap()).unwrap().entries.len(), 1);
    assert!(!std::fs::read(world.known()).unwrap().is_empty(), "the person's own open learned the key");

    assert_eq!(run(&world.agent(), "echo hi"), "ran:echo hi");
    assert_eq!(server.seen.connections.load(Ordering::SeqCst), 1);
    assert_eq!(server.seen.logins.load(Ordering::SeqCst), 1, "the command runner used the session the browser made");
}

#[test]
fn a_changed_host_key_is_refused_for_both_openers_and_loudly() {
    let dir = Scratch::new("changed-root");
    let root = dir.path().join("root").to_owned();
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let server = ssh_server::start(Options::password().sftp(root.clone()));
    let world = World::new("changed", &server, false);
    // The file holds another key for this host: someone else answers on the address.
    let other = ssh_server::ed25519();
    learn_host_key("127.0.0.1", server.addr.port(), other.public_key(), &world.known()).unwrap();

    // The command runner first, so it makes a real attempt; then the file browser, which is never made to wait.
    let command = run_error(&world.agent(), "echo hi");
    assert!(command.contains("CHANGED"), "{command}");
    let blocks = Blocks::load(world.dir.path(), None, &[]);
    let listing = blocks.list(&world.connections, "talos", root.to_str().unwrap()).unwrap_err().to_string();
    assert!(listing.contains("CHANGED"), "{listing}");
    assert_eq!(server.seen.logins.load(Ordering::SeqCst), 0, "no login happened with a changed key");
    assert_eq!(server.seen.refused_logins.load(Ordering::SeqCst), 0);
}

#[test]
fn a_target_that_cannot_be_pinned_is_refused_when_the_runner_is_made() {
    let server = ssh_server::start(Options::password());
    let dir = Scratch::new("unpinnable");
    let known = dir.path().join("known_hosts");
    server.trust(&known);

    // No known hosts file at all.
    let bare = server.target(SshAuth::Password("pw".into()));
    let conn = Connection::standalone("Bare", bare.clone()).unwrap();
    let error = SshExec::on(conn).err().unwrap().to_string();
    assert!(error.contains("has no known_hosts file"), "{error}");

    // A file, but the host is set to accept any key.
    let mut any = server.exec_target(SshAuth::Password("pw".into()), &known);
    any.host_key = HostKeyPolicy::AcceptAny;
    let error = SshExec::on(Connection::standalone("Any", any.clone()).unwrap()).err().unwrap().to_string();
    assert!(error.contains("accept any host key"), "{error}");

    // A jump host that cannot be pinned makes the whole target unpinnable.
    let mut behind = server.exec_target(SshAuth::Password("pw".into()), &known);
    behind.jump = Some(Box::new(bare));
    let error = SshExec::on(Connection::standalone("Behind", behind).unwrap()).err().unwrap().to_string();
    assert!(error.contains("has no known_hosts file"), "{error}");

    // The standalone constructor says the same, and nothing ever connected.
    assert!(SshExec::new("New", any).is_err());
    assert_eq!(server.seen.connections.load(Ordering::SeqCst), 0);
}

#[test]
fn a_command_runner_opening_first_does_not_widen_what_a_pane_may_do_later() {
    let server = ssh_server::start(Options::password());
    let world = World::new("runner-then-pane", &server, false);
    let conn = world.connections.session("talos").unwrap();
    let t0 = Instant::now();
    let refused = pollster(conn.session_at(Open::Pinned, t0)).unwrap_err();
    assert!(refused.fatal && refused.why.contains("not known"), "{refused}");
    assert!(std::fs::read(world.known()).unwrap().is_empty());
    assert!(conn.cooldown_left(t0).is_some(), "the background opener now waits");

    // The person's own open, a second later, is not made to wait and learns the key; the refusal poisoned nothing.
    pollster(conn.session_at(Open::Interactive, t0 + Duration::from_secs(1))).unwrap();
    assert!(!std::fs::read(world.known()).unwrap().is_empty());
    assert_eq!(conn.cooldown_left(t0 + Duration::from_secs(1)), None, "a success clears the wait");
    assert_eq!(server.seen.logins.load(Ordering::SeqCst), 1);
    // And now the command runner uses that session.
    assert_eq!(run(&world.agent(), "echo hi"), "ran:echo hi");
    assert_eq!(server.seen.logins.load(Ordering::SeqCst), 1);
}

#[test]
fn the_error_texts_hold_no_secret() {
    const SECRET: &str = "s3cret-Pa55word!";
    let dir = Scratch::new("no-secret-root");
    let root = dir.path().join("root").to_owned();
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    // The server knows `pw`, so a login with the secret is refused; an unknown host key and a changed one are refused
    // before that.
    let server = ssh_server::start(Options::password().sftp(root.clone()));
    let world = World::new("no-secret", &server, false);
    world.save(SECRET, server.addr.port());
    let conn = world.connections.session("talos").unwrap();
    let mut texts = vec![format!("{conn:?}"), format!("{:?}", conn.target().auth)];
    let t0 = Instant::now();

    let unknown = pollster(conn.session_at(Open::Pinned, t0)).unwrap_err();
    texts.extend([format!("{unknown}"), format!("{unknown:?}")]);
    texts.push(run_error(&world.agent(), "echo hi"));
    let blocks = Blocks::load(world.dir.path(), None, &[]);
    // The person's own open learns the key, and the login is refused for the wrong password.
    let refused = blocks.list(&world.connections, "talos", root.to_str().unwrap()).unwrap_err();
    texts.extend([refused.to_string(), format!("{refused:?}")]);
    let again = pollster(conn.session_at(Open::Pinned, t0 + Duration::from_secs(1))).unwrap_err();
    assert!(again.why.contains("refused the login"), "{again}");
    texts.extend([format!("{again}"), format!("{again:?}")]);
    texts.push(run_error(&world.agent(), "echo hi"));

    // A changed key too.
    let other = ssh_server::ed25519();
    std::fs::write(world.known(), "").unwrap();
    learn_host_key("127.0.0.1", server.addr.port(), other.public_key(), &world.known()).unwrap();
    world.save(SECRET, server.addr.port());
    let changed = pollster(world.connections.session("talos").unwrap().session_at(Open::Interactive, t0)).unwrap_err();
    assert!(changed.why.contains("CHANGED"), "{changed}");
    texts.extend([format!("{changed}"), format!("{changed:?}")]);

    assert!(texts.len() >= 10);
    for text in &texts {
        assert!(!text.contains(SECRET), "a message holds the secret: {text}");
    }
}
