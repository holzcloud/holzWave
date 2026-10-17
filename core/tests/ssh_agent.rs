//! Login through the user's ssh-agent, against the in-process SSH server of `common::ssh_server` and an agent that also
//! runs in process.
//!
//! The agent is `russh`'s own agent server on a Unix socket in a private scratch folder, or, where a test needs to
//! control the order of the keys, a small agent written here that lists its keys in the order it was given. A key
//! lives only in the agent's memory: nothing here writes a private key to a file, and every SSH server of these tests
//! knows public keys only, so a password or a key file cannot be what lets a test pass.
//!
//! The agent tests are Unix only (a Unix socket); the Windows pipe is compiled and cross-checked, never run.
#![cfg(unix)]

mod common;

use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::task::{Context, Poll};
use std::thread;

use common::ssh_server::{self, Options, Out, Server};
use futures_core::Stream;
use holzwave_core::conn::{ConnState, Link};
use holzwave_core::connections::{Connections, KNOWN_HOSTS_FILE};
use holzwave_core::files::{FileSource, SftpSource};
use holzwave_core::pool::Connection;
use holzwave_core::profiles::{Auth, Profile, Profiles};
use holzwave_core::secrets::SecretStore;
use holzwave_core::ssh::{ExecCancel, ExecLimits, SshAuth, SshExec, SshLink};
use russh::keys::agent::client::AgentClient;
use russh::keys::agent::server::{Agent, serve};
use russh::keys::PrivateKey;
use tokio::net::{UnixListener, UnixStream};

/// A private folder (mode 0700) that is removed when the test ends, also when it fails.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = common::scratch_dir(tag);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(dir)
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

/// The connections `serve` takes, as a `Stream` (the one thing the core's tests need `futures-core` for).
struct Incoming(UnixListener);

impl Stream for Incoming {
    type Item = io::Result<UnixStream>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_accept(cx).map(|accepted| Some(accepted.map(|(stream, _)| stream)))
    }
}

/// A running in-process agent: a socket in a private folder of its own.
struct TestAgent {
    dir: Scratch,
}

impl TestAgent {
    /// An agent (`russh`'s server) that holds `keys`. It lists them in no promised order.
    fn start(keys: &[PrivateKey]) -> Self {
        Self::start_with(keys, ())
    }

    /// The same with a different agent behaviour.
    fn start_with<A: Agent + Send + Sync + 'static>(keys: &[PrivateKey], agent: A) -> Self {
        let dir = Scratch::new("ag");
        let socket = dir.path().join("a.sock");
        let (ready, wait) = mpsc::channel();
        let path = socket.clone();
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let listener = UnixListener::bind(&path).unwrap();
                ready.send(()).unwrap();
                let _ = serve(Incoming(listener), agent).await;
            });
        });
        wait.recv().unwrap();
        let me = Self { dir };
        // The keys go in through the agent protocol, as `ssh-add` does; the agent keeps them in memory only.
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let mut client = AgentClient::connect_uds(me.socket()).await.unwrap();
            for key in keys {
                client.add_identity(key, &[]).await.unwrap();
            }
        });
        me
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("a.sock")
    }

    fn auth(&self) -> SshAuth {
        SshAuth::Agent { socket: Some(self.socket()), identity: None }
    }
}

/// Connect a pane with `auth`; its output is read by a helper thread.
fn pane(server: &Server, auth: SshAuth) -> (SshLink, Out) {
    let mut link = SshLink::connect(server.target(auth), 80, 24).unwrap();
    let out = Out::new(link.take_reader().unwrap());
    (link, out)
}

fn count(server: &Server, counter: impl Fn(&ssh_server::Seen) -> &std::sync::atomic::AtomicUsize) -> usize {
    counter(&server.seen).load(Ordering::SeqCst)
}

#[test]
fn an_agent_key_logs_in_to_a_pane() {
    let key = ssh_server::ed25519();
    let agent = TestAgent::start(&[key.clone()]);
    let server = ssh_server::start(Options::key(key.public_key().clone()));

    // No key file at all: the private key exists in the agent's memory and nowhere else.
    let (mut link, mut out) = pane(&server, agent.auth());
    let mut writer = link.take_writer().unwrap();
    out.until("welcome 0", 10);
    assert_eq!(link.state(), ConnState::Connected);
    writer.write_all(b"hi").unwrap();
    out.until("echo:hi", 10);

    assert_eq!(count(&server, |s| &s.logins), 1, "one login");
    assert_eq!(count(&server, |s| &s.refused_logins), 0);
    assert_eq!(count(&server, |s| &s.agent_requests), 0, "agent forwarding is never asked for");
    assert!(!server.seen.events().iter().any(|e| e == "agent"));
    Box::new(link).close().unwrap();
}

/// A store that must never be read or written: an agent login has no secret. Saving an agent connection removes a stale
/// entry, so a removal is allowed (and a store that has none ignores it).
struct NoKeychain;

impl SecretStore for NoKeychain {
    fn get(&self, account: &str) -> holzwave_core::Result<Option<String>> {
        panic!("the keychain was read for {account}");
    }
    fn set(&self, account: &str, _: &str) -> holzwave_core::Result<()> {
        panic!("the keychain was written for {account}");
    }
    fn delete(&self, _: &str) -> holzwave_core::Result<()> {
        Ok(())
    }
}

#[test]
fn an_agent_login_is_the_one_login_of_the_pool() {
    let dir = Scratch::new("agp");
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    // The server speaks canonical paths (on a Mac the temporary folder is behind a link).
    let root = root.canonicalize().unwrap();
    std::fs::write(root.join("a.txt"), "alpha").unwrap();

    let key = ssh_server::ed25519();
    let agent = TestAgent::start(&[key.clone()]);
    let server = ssh_server::start(Options::key(key.public_key().clone()).sftp(root.clone()));
    server.trust(&dir.path().join(KNOWN_HOSTS_FILE));

    // A saved agent connection, in a store that panics on any access.
    let (connections, _) = Connections::load(dir.path(), Arc::new(NoKeychain), None, &[]);
    let mut profile = Profile::new("talos", "Talos", "127.0.0.1");
    profile.port = server.addr.port();
    profile.user = "holz".into();
    profile.auth = Auth::Agent;
    let saved = connections.save(profile, holzwave_core::connections::SecretChange::Keep).unwrap();
    assert!(!saved.has_secret);

    // What the saved profile becomes; only the socket is a test's: a profile reads `SSH_AUTH_SOCK` at connect time.
    let mut target = connections.ssh_target("talos").unwrap();
    assert!(matches!(target.auth, SshAuth::Agent { socket: None, identity: None }));
    target.auth = agent.auth();
    let connection = Connection::standalone("Talos", target).unwrap();

    let (mut link, mut out) = {
        let mut link = SshLink::attach(connection.clone(), 80, 24).unwrap();
        let out = Out::new(link.take_reader().unwrap());
        (link, out)
    };
    out.until("welcome 0", 10);
    let names: Vec<String> =
        SftpSource::on(connection.clone()).list(root.to_str().unwrap()).unwrap().entries.into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["a.txt"]);
    let result = SshExec::on(connection.clone()).unwrap().exec("echo ok", &ExecLimits::default(), &ExecCancel::new()).unwrap();
    assert_eq!(result.stdout, b"ran:echo ok\r\n");

    assert_eq!(count(&server, |s| &s.connections), 1, "one TCP connection");
    assert_eq!(count(&server, |s| &s.logins), 1, "one login for the pane, the files and the command");
    assert_eq!(count(&server, |s| &s.sftp_sessions), 1);
    assert_eq!(count(&server, |s| &s.agent_requests), 0);
    let _ = link.take_writer();
    Box::new(link).close().unwrap();
}

#[test]
fn a_profile_with_the_agent_login_loads_and_maps() {
    // The value is `agent` in the file; an old file without it loads as before.
    let text = serde_json::to_string(&Profiles { version: 1, items: vec![{ let mut p = Profile::new("a", "A", "h"); p.user = "u".into(); p.auth = Auth::Agent; p }] }).unwrap();
    assert!(text.contains(r#""auth":"agent""#), "{text}");
    let (loaded, status) = Profiles::from_json(&text);
    assert_eq!(loaded.items[0].auth, Auth::Agent);
    assert_eq!(status, holzwave_core::layout::LoadStatus::Clean);
    let old = r#"{"version":1,"items":[{"id":"talos","name":"Talos","host":"h","user":"u","key_path":"k"}]}"#;
    assert_eq!(Profiles::from_json(old).0.items[0].auth, Auth::Key);

    // Without a key path and with one; neither reads the keychain.
    let known = Path::new("/tmp/known");
    let home = Path::new("/home/holz");
    let mut profiles = loaded;
    let target = profiles.ssh_target("a", &NoKeychain, Some(home), known).unwrap();
    assert!(matches!(target.auth, SshAuth::Agent { socket: None, identity: None }));
    assert_eq!(profiles.items[0].problem(), None);

    profiles.items[0].key_path = "~/.ssh/id_ed25519.pub".into();
    assert_eq!(profiles.items[0].problem(), None);
    let target = profiles.ssh_target("a", &NoKeychain, Some(home), known).unwrap();
    match target.auth {
        SshAuth::Agent { socket: None, identity: Some(file) } => assert_eq!(file, home.join(".ssh/id_ed25519.pub")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_agent_debug_hides_the_socket() {
    let socket = PathBuf::from("/private/tmp/very-secret-folder/agent.sock");
    let shown = format!("{:?}", SshAuth::Agent { socket: Some(socket), identity: Some(PathBuf::from("/home/holz/.ssh/work.pub")) });
    assert!(shown.contains("work.pub"), "{shown}");
    assert!(!shown.contains("very-secret-folder") && !shown.contains("agent.sock") && !shown.contains("/home/holz"), "{shown}");
    let bare = format!("{:?}", SshAuth::Agent { socket: None, identity: None });
    assert_eq!(bare, "Agent(none)");
    // The target that holds it prints the same way.
    let target = ssh_server::start(Options::password()).target(SshAuth::Agent { socket: Some("/x/agent.sock".into()), identity: None });
    assert!(!format!("{target:?}").contains("agent.sock"));
}
