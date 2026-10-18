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
use russh::keys::agent::server::{Agent, MessageType, serve};
use russh::keys::ssh_encoding::Encode;
use russh::keys::ssh_key::{Certificate, Signature, certificate};
use russh::keys::signature::Signer;
use russh::keys::PrivateKey;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

/// An agent that lists its keys but answers every request to sign with a failure.
#[derive(Clone)]
struct NeverSigns;

impl Agent for NeverSigns {
    async fn confirm_request(&self, message: MessageType) -> bool {
        !matches!(message, MessageType::Sign)
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

/// An agent of its own that lists the identities in the order it was given, which `russh`'s agent (a hash map) does not
/// promise, and signs with Ed25519 keys. Only what the tests need: list the identities and sign.
struct OrderedAgent {
    dir: Scratch,
}

/// Append `bytes` as an SSH string.
fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend((bytes.len() as u32).to_be_bytes());
    out.extend(bytes);
}

/// Take an SSH string off the front of `rest`.
fn take_string<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
    let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
    let bytes = rest.get(4..4 + len)?;
    *rest = &rest[4 + len..];
    Some(bytes)
}

/// The agent's answer to one request: `listed` are the public key blobs it lists, `signers` the keys it can sign with.
fn answer(request: &[u8], listed: &[Vec<u8>], signers: &[PrivateKey]) -> Vec<u8> {
    const FAILURE: u8 = 5;
    match request.split_first() {
        // SSH_AGENTC_REQUEST_IDENTITIES: the list, in the order it was given.
        Some((&11, _)) => {
            let mut reply = vec![12];
            reply.extend((listed.len() as u32).to_be_bytes());
            for blob in listed {
                put_string(&mut reply, blob);
                put_string(&mut reply, b"");
            }
            reply
        }
        // SSH_AGENTC_SIGN_REQUEST: the key blob, the data and the flags.
        Some((&13, rest)) => {
            let mut rest = rest;
            let (Some(blob), Some(data)) = (take_string(&mut rest), take_string(&mut rest)) else { return vec![FAILURE] };
            let Some(key) = signers.iter().find(|k| k.public_key().to_bytes().is_ok_and(|b| b == blob)) else { return vec![FAILURE] };
            let Ok(signature) = Signer::<Signature>::try_sign(key.key_data(), data) else { return vec![FAILURE] };
            let mut encoded = Vec::new();
            if signature.encode(&mut encoded).is_err() {
                return vec![FAILURE];
            }
            let mut reply = vec![14];
            put_string(&mut reply, &encoded);
            reply
        }
        _ => vec![FAILURE],
    }
}

impl OrderedAgent {
    fn start(listed: Vec<Vec<u8>>, signers: Vec<PrivateKey>) -> Self {
        let dir = Scratch::new("ago");
        let path = dir.path().join("a.sock");
        let (ready, wait) = mpsc::channel();
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let listener = UnixListener::bind(&path).unwrap();
                ready.send(()).unwrap();
                let (listed, signers) = (Arc::new(listed), Arc::new(signers));
                while let Ok((mut stream, _)) = listener.accept().await {
                    let (listed, signers) = (listed.clone(), signers.clone());
                    tokio::spawn(async move {
                        loop {
                            let mut len = [0u8; 4];
                            if stream.read_exact(&mut len).await.is_err() {
                                return;
                            }
                            let len = u32::from_be_bytes(len) as usize;
                            if len > 64 * 1024 {
                                return;
                            }
                            let mut request = vec![0u8; len];
                            if stream.read_exact(&mut request).await.is_err() {
                                return;
                            }
                            let reply = answer(&request, &listed, &signers);
                            let mut frame = (reply.len() as u32).to_be_bytes().to_vec();
                            frame.extend(reply);
                            if stream.write_all(&frame).await.is_err() {
                                return;
                            }
                        }
                    });
                }
            });
        });
        wait.recv().unwrap();
        Self { dir }
    }

    /// An agent that lists these keys in this order and signs for each of them.
    fn keys(keys: &[PrivateKey]) -> Self {
        Self::start(keys.iter().map(|k| k.public_key().to_bytes().unwrap()).collect(), keys.to_vec())
    }

    fn auth(&self) -> SshAuth {
        SshAuth::Agent { socket: Some(self.dir.path().join("a.sock")), identity: None }
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

// ---- the edges: no agent, no keys, a refusal, the tries, the identity file, no secret ---------------------------------

/// The pane's whole failure text for `auth` against `server`: everything up to the line that waits for Enter. The pane
/// is in the failed state and is closed afterwards.
fn failure(server: &Server, auth: SshAuth) -> String {
    let (link, mut out) = pane(server, auth);
    out.until("Press Enter to try again", 10);
    assert_eq!(link.state(), ConnState::Failed, "the pane waits for Enter");
    Box::new(link).close().unwrap();
    out.seen.clone()
}

/// A server that knows `key` only.
fn server_for(key: &PrivateKey) -> Server {
    ssh_server::start(Options::key(key.public_key().clone()))
}

fn attempts(server: &Server) -> usize {
    count(server, |s| &s.logins) + count(server, |s| &s.refused_logins)
}

/// The public key of `key` as a file in `dir` (the form `ssh-keygen` writes next to a private key).
fn write_public(dir: &Path, name: &str, key: &PrivateKey) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, key.public_key().to_openssh().unwrap() + "\n").unwrap();
    path
}

fn with_identity(agent: &TestAgent, identity: &Path) -> SshAuth {
    SshAuth::Agent { socket: Some(agent.socket()), identity: Some(identity.to_path_buf()) }
}

#[test]
fn no_agent_is_told_in_words() {
    let server = ssh_server::start(Options::password());
    // Nobody listens on this socket (the environment is read by a unit test of the function that does).
    let dir = Scratch::new("agn");
    let nobody = dir.path().join("nobody.sock");
    let text = failure(&server, SshAuth::Agent { socket: Some(nobody.clone()), identity: None });
    assert!(text.contains("Connection to 127.0.0.1 failed: the ssh-agent cannot be reached. Press Enter to try again."), "{text}");
    assert!(!text.contains("nobody.sock") && !text.contains(dir.path().to_str().unwrap()), "{text}");
    assert_eq!(attempts(&server), 0, "the server was asked for nothing");
}

#[test]
fn an_agent_without_keys_is_told_so() {
    let key = ssh_server::ed25519();
    let agent = TestAgent::start(&[]);
    let server = server_for(&key);
    let text = failure(&server, agent.auth());
    assert!(text.contains("Connection to 127.0.0.1 failed: the ssh-agent holds no keys. Press Enter to try again."), "{text}");
    assert_eq!(attempts(&server), 0, "no login attempt");
}

#[test]
fn an_agent_with_the_wrong_key_is_refused_in_words() {
    let known = ssh_server::ed25519();
    let other = ssh_server::ed25519();
    let agent = TestAgent::start(&[other]);
    let server = server_for(&known);
    let text = failure(&server, agent.auth());
    assert!(text.contains("failed: the ssh-agent has no key that this host accepts. Press Enter to try again."), "{text}");
    assert_eq!(count(&server, |s| &s.refused_logins), 1, "after one attempt");
    assert_eq!(count(&server, |s| &s.logins), 0);
}

#[test]
fn at_most_five_keys_are_tried() {
    let right = ssh_server::ed25519();
    let wrong: Vec<PrivateKey> = (0..7).map(|_| ssh_server::ed25519()).collect();

    // Seven wrong keys and the right one last: five are tried and the login fails.
    let mut all = wrong.clone();
    all.push(right.clone());
    let agent = OrderedAgent::keys(&all);
    let server = server_for(&right);
    let text = failure(&server, agent.auth());
    assert!(text.contains("the ssh-agent has no key that this host accepts"), "{text}");
    assert_eq!(count(&server, |s| &s.refused_logins), 5, "five attempts, no more");
    assert_eq!(count(&server, |s| &s.logins), 0);

    // The right key in position five is the last one tried and it works.
    let mut fifth: Vec<PrivateKey> = wrong[..4].to_vec();
    fifth.push(right.clone());
    fifth.extend(wrong[4..].iter().cloned());
    let agent = OrderedAgent::keys(&fifth);
    let server = server_for(&right);
    let (link, mut out) = pane(&server, agent.auth());
    out.until("welcome 0", 10);
    assert_eq!(count(&server, |s| &s.refused_logins), 4);
    assert_eq!(count(&server, |s| &s.logins), 1);
    Box::new(link).close().unwrap();

    // In position six it is never offered.
    let mut sixth: Vec<PrivateKey> = wrong[..5].to_vec();
    sixth.push(right.clone());
    let agent = OrderedAgent::keys(&sixth);
    let server = server_for(&right);
    failure(&server, agent.auth());
    assert_eq!((count(&server, |s| &s.refused_logins), count(&server, |s| &s.logins)), (5, 0));
}

#[test]
fn the_identity_file_selects_one_key() {
    let dir = Scratch::new("agi");
    let keys = [ssh_server::ed25519(), ssh_server::ed25519(), ssh_server::ed25519()];
    let agent = TestAgent::start(&keys);

    // The second key is the one the server knows: its public key file picks it, so exactly one attempt is made.
    let server = server_for(&keys[1]);
    let second = write_public(dir.path(), "second.pub", &keys[1]);
    let (link, mut out) = pane(&server, with_identity(&agent, &second));
    out.until("welcome 0", 10);
    assert_eq!((count(&server, |s| &s.logins), count(&server, |s| &s.refused_logins)), (1, 0), "one attempt, with that key");
    Box::new(link).close().unwrap();

    // A held key that this server does not know is tried once and refused in words.
    let first = write_public(dir.path(), "first.pub", &keys[0]);
    let text = failure(&server, with_identity(&agent, &first));
    assert!(text.contains("the ssh-agent has no key that this host accepts"), "{text}");
    assert_eq!(count(&server, |s| &s.refused_logins), 1, "one more attempt, not three");

    // A file that is not a public key.
    let junk = dir.path().join("junk.pub");
    std::fs::write(&junk, "this is not a key\n").unwrap();
    let text = failure(&server, with_identity(&agent, &junk));
    assert!(text.contains("failed: the public key file cannot be used. Press Enter to try again."), "{text}");
    assert!(!text.contains("junk.pub"), "{text}");
    assert_eq!(count(&server, |s| &s.refused_logins), 1, "no attempt");

    // A file that does not exist, and a folder.
    let text = failure(&server, with_identity(&agent, &dir.path().join("gone.pub")));
    assert!(text.contains("the public key file cannot be used"), "{text}");
    let text = failure(&server, with_identity(&agent, dir.path()));
    assert!(text.contains("the public key file cannot be used"), "{text}");

    // A private key given by mistake is refused and its text is never read into a message.
    let private = ssh_server::write_key(dir.path(), "id_private", &keys[1], None);
    let secret_text = std::fs::read_to_string(&private).unwrap();
    let text = failure(&server, with_identity(&agent, &private));
    assert!(text.contains("the public key file cannot be used"), "{text}");
    for line in secret_text.lines().filter(|l| l.len() > 20 && !l.starts_with("-----")) {
        assert!(!text.contains(line), "a line of the private key is in the message");
    }

    // A public key that the agent does not hold.
    let stranger = write_public(dir.path(), "stranger.pub", &ssh_server::ed25519());
    let attempts_before = attempts(&server);
    let text = failure(&server, with_identity(&agent, &stranger));
    assert!(text.contains("failed: the public key is not in the ssh-agent. Press Enter to try again."), "{text}");
    assert_eq!(attempts(&server), attempts_before, "no attempt");
}

#[test]
fn an_agent_that_refuses_to_sign_is_told_so() {
    let key = ssh_server::ed25519();
    let agent = TestAgent::start_with(&[key.clone()], NeverSigns);
    let server = server_for(&key);
    let text = failure(&server, agent.auth());
    assert!(text.contains("Connection to 127.0.0.1 failed: the ssh-agent refused to sign. Press Enter to try again."), "{text}");
    assert_eq!(count(&server, |s| &s.logins), 0);
}

/// A certificate for `key`, signed by a throwaway authority.
fn certificate_for(key: &PrivateKey) -> Certificate {
    let authority = ssh_server::ed25519();
    let mut builder = certificate::Builder::new(vec![7u8; 16], key.public_key().key_data().clone(), 0, u64::MAX).unwrap();
    builder.valid_principal("holz").unwrap();
    builder.sign(&authority).unwrap()
}

#[test]
fn certificates_are_skipped() {
    let key = ssh_server::ed25519();
    let cert = certificate_for(&key);
    // The certificate of the right key comes first; the plain key comes after it. The server knows the plain key, and a
    // certificate that was offered would be refused (the test server knows no authority), so a refusal is a count.
    let agent = OrderedAgent::start(vec![cert.to_bytes().unwrap(), key.public_key().to_bytes().unwrap()], vec![key.clone()]);
    let server = server_for(&key);
    let (link, mut out) = pane(&server, agent.auth());
    out.until("welcome 0", 10);
    assert_eq!((count(&server, |s| &s.logins), count(&server, |s| &s.refused_logins)), (1, 0), "the certificate was not offered");
    Box::new(link).close().unwrap();

    // An agent that holds a certificate and nothing else holds no key to offer.
    let only = OrderedAgent::start(vec![cert.to_bytes().unwrap()], vec![key.clone()]);
    let text = failure(&server, only.auth());
    assert!(text.contains("the ssh-agent holds no keys"), "{text}");
}

#[test]
fn an_agent_profile_cannot_carry_a_secret() {
    use holzwave_core::connections::{AGENT_HAS_NO_SECRET, SecretChange};
    use holzwave_core::secrets::MemoryStore;
    let dir = Scratch::new("ags");
    let store = Arc::new(MemoryStore::default());
    let (connections, _) = Connections::load(dir.path(), store.clone(), None, &[]);
    let agent_profile = || {
        let mut p = Profile::new("talos", "Talos", "host.example.org");
        p.user = "holz".into();
        p.auth = Auth::Agent;
        p
    };

    // A secret is refused, and nothing is written: not the keychain and not the file.
    let refused = connections.save(agent_profile(), SecretChange::Set { value: "hunter2".into() }).unwrap_err().to_string();
    assert!(refused.contains(AGENT_HAS_NO_SECRET) && refused.contains("an agent login has no password or passphrase"), "{refused}");
    assert_eq!(store.get("ssh:talos").unwrap(), None);
    assert!(connections.list().is_empty());
    assert!(!dir.path().join("connections.json").exists());
    // Not even an empty one: that would be a removal the person did not ask for by name.
    assert!(connections.save(agent_profile(), SecretChange::Set { value: String::new() }).is_err());

    // Keep and Clear are fine, and the connection has no secret.
    assert!(!connections.save(agent_profile(), SecretChange::Keep).unwrap().has_secret);
    assert!(!connections.save(agent_profile(), SecretChange::Clear).unwrap().has_secret);
    assert!(!connections.list()[0].has_secret);

    // A saved password connection switched to the agent loses its keychain entry, with Clear and with Keep.
    for change in [SecretChange::Clear, SecretChange::Keep] {
        let mut password = agent_profile();
        password.id = "web".into();
        password.auth = Auth::Password;
        connections.save(password.clone(), SecretChange::Set { value: "hunter2".into() }).unwrap();
        assert_eq!(store.get("ssh:web").unwrap().as_deref(), Some("hunter2"));
        password.auth = Auth::Agent;
        assert!(!connections.save(password, change).unwrap().has_secret);
        assert_eq!(store.get("ssh:web").unwrap(), None, "the entry of the old login is gone");
    }
    // The file says `agent` and holds no secret.
    let file = std::fs::read_to_string(dir.path().join("connections.json")).unwrap();
    assert!(file.contains(r#""auth": "agent""#) || file.contains(r#""auth":"agent""#), "{file}");
    assert!(!file.contains("hunter2"));
}

#[test]
fn no_message_holds_a_key_a_signature_or_the_socket() {
    let dir = Scratch::new("agm");
    let right = ssh_server::ed25519();
    let wrong = ssh_server::ed25519();
    let server = server_for(&right);
    let private = ssh_server::write_key(dir.path(), "id_wrong", &wrong, None);

    // The agents live until the end of the test: their sockets are in folders that go with them.
    let nobody = dir.path().join("nobody.sock");
    let empty = TestAgent::start(&[]);
    let wrong_agent = TestAgent::start(&[wrong.clone()]);
    let refusing = TestAgent::start_with(&[right.clone()], NeverSigns);
    let held = TestAgent::start(&[right.clone()]);
    let stranger = write_public(dir.path(), "stranger.pub", &ssh_server::ed25519());
    let cases = vec![
        ("unreachable", SshAuth::Agent { socket: Some(nobody.clone()), identity: None }, nobody),
        ("no keys", empty.auth(), empty.socket()),
        ("wrong key", wrong_agent.auth(), wrong_agent.socket()),
        ("refuses to sign", refusing.auth(), refusing.socket()),
        ("private key as identity", with_identity(&held, &private), held.socket()),
        ("key not held", with_identity(&held, &stranger), held.socket()),
    ];

    for (what, auth, socket) in cases {
        let text = failure(&server, auth);
        let mut needles = vec![socket.to_str().unwrap().to_owned(), dir.path().to_str().unwrap().to_owned(), "SSH_AUTH_SOCK=".into()];
        // The public key text of every key in play, and the private key's own text.
        for key in [&right, &wrong] {
            let public = key.public_key().to_openssh().unwrap();
            needles.push(public.split_whitespace().nth(1).unwrap().to_owned());
        }
        for line in std::fs::read_to_string(&private).unwrap().lines().filter(|l| l.len() > 20 && !l.starts_with("-----")) {
            needles.push(line.to_owned());
        }
        for needle in needles {
            assert!(!text.contains(&needle), "{what}: the message holds {needle:?}: {text}");
        }
        assert!(text.contains("Connection to 127.0.0.1 failed: ") && text.contains(". Press Enter to try again."), "{what}: {text}");
    }
}
