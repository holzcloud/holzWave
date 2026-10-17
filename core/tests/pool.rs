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

