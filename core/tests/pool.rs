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

