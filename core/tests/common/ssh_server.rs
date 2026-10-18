//! The one in-process SSH server of the integration tests.
//!
//! It stands in for a real host so that the tests of the pane (`SshLink`), the exec primitive (`SshExec`) and every
//! other consumer of a session run in a plain `cargo test`, with no Zig and no machine of their own. Every test file
//! that needs a server starts it here; nobody writes a second one, and a feature that needs a new server behaviour (a
//! subsystem, a forward, another login method) adds it to this file.
//!
//! What it does:
//!
//! - **Login.** Password login (exactly the option's user and password) and public-key login (exactly the option's
//!   user and one of the authorized public keys). The server verifies the signature and knows the public half only,
//!   so a password cannot be what let a key test pass. Every refusal and every success is counted.
//! - **Shell.** A pty request is accepted and counted; a shell request prints `welcome N` (`N` is the zero-based index
//!   of the connection), each input line comes back as `echo:<line>` and the line `exit` ends the shell with status 3.
//!   With [`Options::shell_flood`] the line `flood` is answered with a stream of bytes, and with
//!   [`Options::cut_shell_after`] the first connection is cut a while after its shell started.
//! - **Channel limit.** With [`Options::max_channels`] a session channel beyond that many open at once is refused, as
//!   OpenSSH's `MaxSessions` does.
//! - **Exec.** The command line is recorded, `ran:<command>` is printed and the command ends with status 0.
//! - **Jump host.** With [`Options::jump`] the server answers `direct-tcpip` requests for loopback addresses by
//!   connecting to them and copying both ways; every request is recorded.
//! - **SFTP.** With [`Options::sftp`] the `sftp` subsystem serves a real directory (and refuses every path outside it);
//!   every subsystem started is counted.
//! - **Unix socket forward.** With [`Options::forward`] a `streamlocal-forward@openssh.com` request is accepted (without
//!   it, or with [`Options::refuse_forward`], it is refused) and recorded, as is its cancel; [`Server::open_forwarded`]
//!   opens a forwarded channel to the client on the newest login and hands the test a blocking [`ForwardedPeer`].
//! - **Wrapped shell.** With [`Options::wrapped_shell`] an exec request that starts with `exec sh -c 'HOLZWAVE_` behaves
//!   like a shell; [`Options::refuse_exec_once`] answers the first exec request with a failure. `Seen::events` holds
//!   the requests of the channels in order.
//! - **Scripted exec and the SFTP log.** [`Options::exec_script`] answers one exact command line with a list of replies in
//!   turn (the last one repeats), each with its own exit status; `Seen::sftp_ops` lists the operations of the SFTP
//!   subsystem in order (`open`, `mkdir`, `rename`, `remove`, `setstat`) with paths relative to the served root.
//!   [`Options::sftp_refuse_overwrite`] makes a rename onto an existing file fail, as OpenSSH's does; [`Options::sftp_fail_writes`]
//!   and [`Options::sftp_deny_upload`] fail a write and refuse to open a file for writing.
//! - **Stale sockets.** With [`Options::stale_sockets`] a forward request for a path that is still bound is refused, and a
//!   path is freed by its cancel only: a login that is cut leaves its paths bound, as a real `sshd` does until it notices.
//! - **Agent forwarding.** A request for it is refused and counted (`Seen::agent_requests`), so a test can say that a
//!   login through an agent never asked for it.
//! - **Keyboard-interactive.** With [`Options::keyboard_interactive`] the server logs a person in through rounds of questions:
//!   each round is a name, instructions, prompts with their echo flags and the answers it expects. A right answer goes to
//!   the next round (or accepts after the last one), a wrong one is a refusal; `Seen::kbd_rounds` counts the rounds
//!   asked and `Seen::kbd_refused` the answer rounds that were wrong. A round with no prompts expects no answer.
//! - **Cuts and counts.** [`Server::cut`] ends every connection that is open now; `Seen::disconnects` counts the
//!   connections that ended, whoever ended them.
//!
//! The server runs on a thread of its own with a loopback listener on a free port, until the process ends.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use holzwave_core::ssh::{HostKeyPolicy, Reconnect, SshAuth, SshTarget, learn_host_key};
use russh::keys::ssh_key::private::{KeypairData, RsaKeypair};
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId, Disconnect};
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle as SftpHandle, Name, OpenFlags, Status, StatusCode, Version};
use tokio::net::{TcpListener, TcpStream};

/// What the server accepts.
#[derive(Clone)]
pub struct Options {
    /// The one user that can log in.
    pub user: &'static str,
    /// The password of that user; `None` refuses every password.
    pub password: Option<&'static str>,
    /// The public keys of that user; none refuses every key.
    pub authorized: Vec<PublicKey>,
    /// Answer `direct-tcpip` requests (the server is a jump host).
    pub jump: bool,
    /// Serve the `sftp` subsystem over this directory (made canonical by [`Options::sftp`]).
    pub sftp_root: Option<PathBuf>,
    /// What every exec request prints instead of `ran:<command>`.
    pub exec_reply: Option<Vec<u8>>,
    /// The size of the answer to the shell line `flood`; `None` makes it an ordinary line.
    pub shell_flood: Option<usize>,
    /// The most session channels open at once; the next one is refused.
    pub max_channels: Option<usize>,
    /// Cut the first connection this many milliseconds after its shell started.
    pub cut_shell_after: Option<u64>,
    /// Accept `streamlocal-forward@openssh.com` requests.
    pub forward: bool,
    /// An exec request that starts the wrapped shell behaves like a shell.
    pub wrapped_shell: bool,
    /// The first exec request is answered with a failure.
    pub refuse_exec_once: bool,
    /// Exec requests whose command line is exactly the first member: the replies in turn (bytes and exit status), the last
    /// one for every later call.
    pub exec_script: Vec<(String, Vec<(Vec<u8>, u32)>)>,
    /// A rename onto an existing file fails (OpenSSH's SFTP server does the same).
    pub sftp_refuse_overwrite: bool,
    /// A write to an open file fails.
    pub sftp_fail_writes: bool,
    /// Opening a file for writing is refused.
    pub sftp_deny_upload: bool,
    /// A forward request for a socket path that is still bound is refused, as `sshd` does without `StreamLocalBindUnlink`:
    /// a path stays bound until its forward is cancelled, also after a login that ended without a goodbye.
    pub stale_sockets: bool,
    /// The rounds of the keyboard-interactive login; empty refuses that method.
    pub kbd: Vec<KbdRound>,
}

/// One round of a keyboard-interactive login: the name, the instructions, the prompts with their echo flag, and the answers
/// that let the login go on.
pub type KbdRound = (String, String, Vec<(String, bool)>, Vec<String>);

/// A round of a keyboard-interactive login, for [`Options::keyboard_interactive`].
pub fn round(name: &str, instructions: &str, prompts: &[(&str, bool)], answers: &[&str]) -> KbdRound {
    (
        name.to_owned(),
        instructions.to_owned(),
        prompts.iter().map(|(text, echo)| ((*text).to_owned(), *echo)).collect(),
        answers.iter().map(|a| (*a).to_owned()).collect(),
    )
}

impl Options {
    /// User `holz` with the password `pw` and nothing else.
    pub fn password() -> Self {
        Self::new(Some("pw"), Vec::new())
    }

    /// User `holz` with this public key and no password.
    pub fn key(key: PublicKey) -> Self {
        Self::new(None, vec![key])
    }

    fn new(password: Option<&'static str>, authorized: Vec<PublicKey>) -> Self {
        Self {
            user: "holz",
            password,
            authorized,
            jump: false,
            sftp_root: None,
            exec_reply: None,
            shell_flood: None,
            max_channels: None,
            cut_shell_after: None,
            forward: false,
            wrapped_shell: false,
            refuse_exec_once: false,
            exec_script: Vec::new(),
            sftp_refuse_overwrite: false,
            sftp_fail_writes: false,
            sftp_deny_upload: false,
            stale_sockets: false,
            kbd: Vec::new(),
        }
    }

    /// User `holz` who logs in only by answering these rounds (no password, no key).
    pub fn keyboard_interactive(rounds: Vec<KbdRound>) -> Self {
        let mut options = Self::new(None, Vec::new());
        options.kbd = rounds;
        options
    }

    /// The same server, and it also answers `direct-tcpip`.
    pub fn jump(mut self) -> Self {
        self.jump = true;
        self
    }

    /// The same server, and its SFTP subsystem serves `root`. Paths outside it are refused.
    pub fn sftp(mut self, root: PathBuf) -> Self {
        self.sftp_root = Some(root.canonicalize().unwrap());
        self
    }

    /// The same server, and the shell line `flood` is answered with `bytes` bytes (the letters `a` to `z` in turn, in
    /// 16 KiB chunks, one chunk a millisecond: fast enough to fill any bound, slow enough that a test can use the
    /// connection while it runs) and then the line `flood done`.
    pub fn shell_flood(mut self, bytes: usize) -> Self {
        self.shell_flood = Some(bytes);
        self
    }

    /// The same server, and a session channel beyond `n` open at once is refused.
    pub fn max_channels(mut self, n: usize) -> Self {
        self.max_channels = Some(n);
        self
    }

    /// The same server, and its first connection is cut `ms` milliseconds after its shell started.
    pub fn cut_shell_after(mut self, ms: u64) -> Self {
        self.cut_shell_after = Some(ms);
        self
    }

    /// The same server, and it accepts Unix socket forward requests.
    pub fn forward(mut self) -> Self {
        self.forward = true;
        self
    }

    /// The same server, and it refuses Unix socket forward requests (the default, said out loud).
    pub fn refuse_forward(mut self) -> Self {
        self.forward = false;
        self
    }

    /// The same server, and an exec request whose text starts with `exec sh -c 'HOLZWAVE_` behaves like a shell: it
    /// prints `welcome N` and echoes like the shell.
    pub fn wrapped_shell(mut self) -> Self {
        self.wrapped_shell = true;
        self
    }

    /// The same server, and the first exec request it gets is answered with a failure.
    pub fn refuse_exec_once(mut self) -> Self {
        self.refuse_exec_once = true;
        self
    }

    /// The same server, and the exec request whose command line is exactly `command` answers with `replies` in turn (bytes and
    /// exit status); the last reply is the answer of every later call. Other commands are answered as before.
    pub fn exec_script(mut self, command: &str, replies: &[(&str, u32)]) -> Self {
        let replies = replies.iter().map(|(bytes, status)| (bytes.as_bytes().to_vec(), *status)).collect();
        self.exec_script.push((command.to_owned(), replies));
        self
    }

    /// The same server, and a rename onto an existing file fails.
    pub fn sftp_refuse_overwrite(mut self) -> Self {
        self.sftp_refuse_overwrite = true;
        self
    }

    /// The same server, and a forward request for a socket path that is still bound is refused (a path is bound by its
    /// forward and freed by the cancel of it, not by the end of the login that made it).
    pub fn stale_sockets(mut self) -> Self {
        self.stale_sockets = true;
        self
    }

    /// The same server, and every write to an open file fails.
    pub fn sftp_fail_writes(mut self) -> Self {
        self.sftp_fail_writes = true;
        self
    }

    /// The same server, and opening a file for writing is refused.
    pub fn sftp_deny_upload(mut self) -> Self {
        self.sftp_deny_upload = true;
        self
    }

    /// The same server, and every exec request prints `bytes` (and nothing else) before it ends with status 0.
    pub fn exec_reply(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.exec_reply = Some(bytes.into());
        self
    }
}

/// What the server saw, shared with the test that started it.
#[derive(Default)]
pub struct Seen {
    /// TCP connections accepted.
    pub connections: AtomicUsize,
    /// Successful logins.
    pub logins: AtomicUsize,
    /// Refused logins.
    pub refused_logins: AtomicUsize,
    /// Shell requests.
    pub shells: AtomicUsize,
    /// Pty requests.
    pub ptys: AtomicUsize,
    /// The command line of every exec request, in order.
    pub commands: Mutex<Vec<String>>,
    /// The `(host, port)` of every `direct-tcpip` request, in order.
    pub direct_tcpip: Mutex<Vec<(String, u16)>>,
    /// Connections that ended (the server dropped the handler of the connection), whoever ended them.
    pub disconnects: AtomicUsize,
    /// `sftp` subsystems started.
    pub sftp_sessions: AtomicUsize,
    /// The path of every Unix socket forward request, in order.
    pub forwards: Mutex<Vec<String>>,
    /// The path of every cancel of a Unix socket forward, in order.
    pub cancelled_forwards: Mutex<Vec<String>>,
    /// Exec requests that started the wrapped shell.
    pub execs_as_shell: AtomicUsize,
    /// `auth-agent-req@openssh.com` requests: a client that asks for agent forwarding. The server refuses every one.
    pub agent_requests: AtomicUsize,
    /// Rounds of questions the server sent in a keyboard-interactive login.
    pub kbd_rounds: AtomicUsize,
    /// Answer rounds of a keyboard-interactive login that were wrong (each also counts as a refused login).
    pub kbd_refused: AtomicUsize,
    /// The requests of the session channels in order: `pty`, `shell`, `exec`, `agent`, `forward PATH`, `cancel PATH`.
    pub events: Mutex<Vec<String>>,
    /// How often each scripted command ran, for [`Options::exec_script`].
    script_calls: Mutex<HashMap<String, usize>>,
    /// The operations of the SFTP subsystem in order, paths relative to the served root.
    pub sftp_ops: Mutex<Vec<String>>,
    /// The socket paths that are bound now, for [`Options::stale_sockets`].
    bound_sockets: Mutex<HashSet<String>>,
    /// Exec requests that were refused by [`Options::refuse_exec_once`].
    refused_execs: AtomicUsize,
    /// The handles of the connections that logged in, for [`Server::cut`].
    handles: Mutex<Vec<server::Handle>>,
}

impl Seen {
    /// The commands run so far.
    pub fn commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }

    /// The `direct-tcpip` requests so far.
    pub fn direct_tcpip(&self) -> Vec<(String, u16)> {
        self.direct_tcpip.lock().unwrap().clone()
    }

    /// The paths of the forward requests so far.
    pub fn forwards(&self) -> Vec<String> {
        self.forwards.lock().unwrap().clone()
    }

    /// The paths of the forward cancels so far.
    pub fn cancelled_forwards(&self) -> Vec<String> {
        self.cancelled_forwards.lock().unwrap().clone()
    }

    /// The operations of the SFTP subsystem so far, in order.
    pub fn sftp_ops(&self) -> Vec<String> {
        self.sftp_ops.lock().unwrap().clone()
    }

    /// The requests of the channels so far, in order.
    pub fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    fn event(&self, text: impl Into<String>) {
        self.events.lock().unwrap().push(text.into());
    }
}

/// A running server.
pub struct Server {
    pub addr: SocketAddr,
    pub host_key: PublicKey,
    pub seen: Arc<Seen>,
}

/// Start a server on a loopback port of the system's choosing.
pub fn start(options: Options) -> Server {
    let host = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let config = Arc::new(server::Config {
        keys: vec![host.clone()],
        // A refusal is quick, so a test of a refused login does not wait.
        auth_rejection_time: Duration::from_millis(0),
        auth_rejection_time_initial: Some(Duration::from_millis(0)),
        ..Default::default()
    });
    let seen = Arc::new(Seen::default());
    let mut factory = Factory { seen: seen.clone(), options };
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let socket = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            tx.send(socket.local_addr().unwrap()).unwrap();
            let _ = factory.run_on_socket(config, &socket).await;
        });
    });
    Server { addr: rx.recv().unwrap(), host_key: host.public_key().clone(), seen }
}

impl Server {
    /// End every connection that is open now, as a network that failed would. A connection made afterwards is not
    /// touched.
    pub fn cut(&self) {
        let handles = std::mem::take(&mut *self.seen.handles.lock().unwrap());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for handle in handles {
            let _ = rt.block_on(handle.disconnect(Disconnect::ByApplication, "cut".into(), "en".into()));
        }
    }

    /// Open a forwarded Unix socket channel for `path` to the client of the newest login, as `sshd` does when something
    /// connects to the forwarded socket. `Err` with the reason when the client refuses it or there is no login.
    pub fn open_forwarded(&self, path: &str) -> Result<ForwardedPeer, String> {
        let handle = self.seen.handles.lock().unwrap().last().cloned().ok_or("no login")?;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let channel = rt
            .block_on(async { tokio::time::timeout(Duration::from_secs(10), handle.channel_open_forwarded_streamlocal(path)).await })
            .map_err(|_| "the open timed out".to_string())?
            .map_err(|e| e.to_string())?;
        Ok(ForwardedPeer { rt, channel, pending: Vec::new(), ended: false })
    }

    /// A target for a pane: host `127.0.0.1` and this port, user `holz`, three attempts 100 ms apart.
    pub fn target(&self, auth: SshAuth) -> SshTarget {
        let mut target = SshTarget::new("127.0.0.1", "holz", auth);
        target.port = self.addr.port();
        target.reconnect = Reconnect { attempts: 3, delay: Duration::from_millis(100) };
        target
    }

    /// Learn the host key of this server into a known hosts file, as if the user's own pane had connected before.
    pub fn trust(&self, known_hosts: &Path) {
        learn_host_key("127.0.0.1", self.addr.port(), &self.host_key, known_hosts).unwrap();
    }

    /// A target for the exec primitive: a known hosts file and the strict policy. It does not learn the key; call
    /// [`Server::trust`] for that. The file is made empty when it does not exist.
    pub fn exec_target(&self, auth: SshAuth, known_hosts: &Path) -> SshTarget {
        if !known_hosts.exists() {
            std::fs::write(known_hosts, "").unwrap();
        }
        let mut target = self.target(auth);
        target.known_hosts = Some(known_hosts.to_path_buf());
        target.host_key = HostKeyPolicy::Strict;
        target
    }
}

/// The server's end of a forwarded channel, with blocking calls. Every wait is bounded.
pub struct ForwardedPeer {
    rt: tokio::runtime::Runtime,
    channel: Channel<Msg>,
    pending: Vec<u8>,
    ended: bool,
}

impl ForwardedPeer {
    /// Send `bytes` to the client.
    pub fn write_all(&mut self, bytes: &[u8]) {
        self.rt.block_on(async { tokio::time::timeout(Duration::from_secs(10), self.channel.data(bytes)).await }).unwrap().unwrap();
    }

    /// Send `bytes` and say whether the client took them all within two seconds (`false` when the channel is gone, or the
    /// client no longer opens its window).
    pub fn try_write(&mut self, bytes: &[u8]) -> bool {
        matches!(self.rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), self.channel.data(bytes)).await }), Ok(Ok(())))
    }

    /// Wait for the next message of the channel for at most `wait`. `None` when nothing came; an end marks the peer ended.
    fn next(&mut self, wait: Duration) -> Option<Vec<u8>> {
        let msg = self.rt.block_on(async { tokio::time::timeout(wait, self.channel.wait()).await });
        match msg {
            Err(_) => None,
            Ok(Some(russh::ChannelMsg::Data { data })) => Some(data.to_vec()),
            Ok(Some(russh::ChannelMsg::Close | russh::ChannelMsg::Eof)) | Ok(None) => {
                self.ended = true;
                None
            }
            Ok(Some(_)) => Some(Vec::new()),
        }
    }

    /// Read up to and including the next newline, for at most 10 s. `None` when the channel ended or nothing came.
    pub fn read_line(&mut self) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(at) = self.pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=at).collect();
                return Some(String::from_utf8_lossy(&line).into_owned());
            }
            if self.ended || Instant::now() >= deadline {
                return None;
            }
            if let Some(chunk) = self.next(deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(200))) {
                self.pending.extend_from_slice(&chunk);
            }
        }
    }

    /// Whether the channel has ended (the client closed it), waiting up to `wait` for that.
    pub fn closed_within(&mut self, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        while !self.ended && Instant::now() < deadline {
            self.next(deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(50)));
        }
        self.ended
    }

    /// Whether the channel has ended, looking for a moment.
    pub fn closed(&mut self) -> bool {
        self.closed_within(Duration::from_millis(100))
    }

    /// End the channel from this side.
    pub fn close(&mut self) {
        let _ = self.rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), self.channel.close()).await });
    }
}

/// An Ed25519 key.
pub fn ed25519() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
}

/// An RSA key of `bits` bits. Generating one takes seconds in a dev build, so a test makes it once.
pub fn rsa(bits: usize) -> PrivateKey {
    let pair = RsaKeypair::random(&mut rand::rng(), bits).unwrap();
    PrivateKey::new(KeypairData::from(pair), "").unwrap()
}

/// Write `key` into `dir` as `name` in the OpenSSH format, mode 0600 on Unix, encrypted with `passphrase` when one is
/// given. The folder is the caller's private scratch folder; the path of the file is returned.
pub fn write_key(dir: &Path, name: &str, key: &PrivateKey, passphrase: Option<&str>) -> PathBuf {
    let path = dir.join(name);
    let text = match passphrase {
        Some(passphrase) => {
            let sealed = key.encrypt(&mut rand::rng(), passphrase).unwrap();
            sealed.to_openssh(LineEnding::LF).unwrap().to_string()
        }
        None => key.to_openssh(LineEnding::LF).unwrap().to_string(),
    };
    write_private(&path, text.as_bytes());
    path
}

/// Write a file that only its owner can read.
pub fn write_private(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).unwrap().write_all(bytes).unwrap();
}

/// The output of a link, read by one helper thread, so a test can wait for text as often as it likes.
pub struct Out {
    rx: mpsc::Receiver<Vec<u8>>,
    pub seen: String,
}

impl Out {
    pub fn new(mut reader: Box<dyn Read + Send>) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self { rx, seen: String::new() }
    }

    /// Take in everything that arrives until nothing has arrived for `gap`, or `max` has passed.
    pub fn quiet(&mut self, gap: Duration, max: Duration) {
        let deadline = Instant::now() + max;
        while let Ok(chunk) = self.rx.recv_timeout(gap.min(deadline.saturating_duration_since(Instant::now()))) {
            self.seen.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    /// Wait until `needle` has been seen in the output, or fail after `secs` seconds.
    pub fn until(&mut self, needle: &str, secs: u64) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while !self.seen.contains(needle) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(chunk) => self.seen.push_str(&String::from_utf8_lossy(&chunk)),
                Err(_) => panic!("no {needle:?} within {secs} s; saw {:?}", self.seen),
            }
        }
    }
}

// ---- the server itself ------------------------------------------------------------------------------------------

/// Makes one [`Handler`] per connection.
#[derive(Clone)]
struct Factory {
    seen: Arc<Seen>,
    options: Options,
}

impl server::Server for Factory {
    type Handler = Handler;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Handler {
        let index = self.seen.connections.fetch_add(1, Ordering::SeqCst);
        Handler {
            seen: self.seen.clone(),
            options: self.options.clone(),
            index,
            shells: HashSet::new(),
            channels: HashMap::new(),
            open: HashSet::new(),
            kbd_at: 0,
        }
    }
}

struct Handler {
    seen: Arc<Seen>,
    options: Options,
    /// The zero-based index of this connection.
    index: usize,
    /// The channels that run the shell. Data on any other channel (a forwarded connection) is not the shell's input.
    shells: HashSet<ChannelId>,
    /// Session channels that have not asked for anything yet: an `sftp` request needs the channel itself. A channel is
    /// let go of when it runs a shell or a command, because nobody reads it then and its data would pile up.
    channels: HashMap<ChannelId, Channel<Msg>>,
    /// The session channels that are open now, for [`Options::max_channels`].
    open: HashSet<ChannelId>,
    /// The round of the keyboard-interactive login this connection is at.
    kbd_at: usize,
}

impl Drop for Handler {
    fn drop(&mut self) {
        self.seen.disconnects.fetch_add(1, Ordering::SeqCst);
    }
}

impl Handler {
    fn judge(&self, accepted: bool) -> Auth {
        if accepted {
            self.seen.logins.fetch_add(1, Ordering::SeqCst);
            Auth::Accept
        } else {
            self.seen.refused_logins.fetch_add(1, Ordering::SeqCst);
            Auth::reject()
        }
    }
}

impl server::Handler for Handler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        let accepted = user == self.options.user && self.options.password == Some(password);
        Ok(self.judge(accepted))
    }

    // Called after the signature was verified: the client really holds the private key.
    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let accepted = user == self.options.user && self.options.authorized.iter().any(|known| known == key);
        Ok(self.judge(accepted))
    }

    // The rounds of `Options::keyboard_interactive`: the first call has no response and asks round 0, each later call
    // carries the answers to the round asked last.
    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _submethods: &str,
        response: Option<server::Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        if user != self.options.user || self.options.kbd.is_empty() {
            return Ok(self.judge(false));
        }
        match response {
            None => self.kbd_at = 0,
            Some(answers) => {
                let given: Vec<String> = answers.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).collect();
                if given != self.options.kbd[self.kbd_at].3 {
                    self.seen.kbd_refused.fetch_add(1, Ordering::SeqCst);
                    return Ok(self.judge(false));
                }
                self.kbd_at += 1;
            }
        }
        match self.options.kbd.get(self.kbd_at) {
            None => Ok(self.judge(true)),
            Some((name, instructions, prompts, _)) => {
                self.seen.kbd_rounds.fetch_add(1, Ordering::SeqCst);
                let prompts: Vec<(std::borrow::Cow<'static, str>, bool)> =
                    prompts.iter().map(|(text, echo)| (std::borrow::Cow::Owned(text.clone()), *echo)).collect();
                Ok(Auth::Partial {
                    name: std::borrow::Cow::Owned(name.clone()),
                    instructions: std::borrow::Cow::Owned(instructions.clone()),
                    prompts: std::borrow::Cow::Owned(prompts),
                })
            }
        }
    }

    // A client that forwards its agent asks here. It is refused and counted, so a test can say that nobody asked.
    async fn agent_request(&mut self, _channel: ChannelId, _session: &mut Session) -> Result<bool, Self::Error> {
        self.seen.agent_requests.fetch_add(1, Ordering::SeqCst);
        self.seen.event("agent");
        Ok(false)
    }

    async fn auth_succeeded(&mut self, session: &mut Session) -> Result<(), Self::Error> {
        self.seen.handles.lock().unwrap().push(session.handle());
        Ok(())
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.options.max_channels.is_some_and(|most| self.open.len() >= most) {
            // A dropped handle refuses the channel, as `MaxSessions` does.
            drop(reply);
            return Ok(());
        }
        self.open.insert(channel.id());
        if self.options.sftp_root.is_some() {
            self.channels.insert(channel.id(), channel);
        }
        reply.accept().await;
        Ok(())
    }

    async fn channel_close(&mut self, channel: ChannelId, _session: &mut Session) -> Result<(), Self::Error> {
        self.open.remove(&channel);
        self.channels.remove(&channel);
        Ok(())
    }

    async fn subsystem_request(&mut self, id: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
        if let (Some(root), "sftp", Some(channel)) = (&self.options.sftp_root, name, self.channels.remove(&id)) {
            self.seen.sftp_sessions.fetch_add(1, Ordering::SeqCst);
            session.channel_success(id)?;
            let files = Files { root: root.clone(), handles: HashMap::new(), next: 0, seen: self.seen.clone(), options: self.options.clone() };
            tokio::spawn(russh_sftp::server::run(channel.into_stream(), files));
            return Ok(());
        }
        session.channel_failure(id)?;
        Ok(())
    }

    // A jump request. The server only forwards to loopback addresses, and only when it was made a jump host; any other
    // request is rejected by dropping the reply.
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _originator: &str,
        _originator_port: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Ok(port) = u16::try_from(port) else { return Ok(()) };
        self.seen.direct_tcpip.lock().unwrap().push((host.to_string(), port));
        if !self.options.jump || host != "127.0.0.1" {
            return Ok(());
        }
        let Ok(mut onward) = TcpStream::connect((host, port)).await else { return Ok(()) };
        reply.accept().await;
        let mut stream = channel.into_stream();
        tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut onward).await;
        });
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _cols: u32,
        _rows: u32,
        _px_w: u32,
        _px_h: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.seen.ptys.fetch_add(1, Ordering::SeqCst);
        self.seen.event("pty");
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        self.channels.remove(&channel);
        self.seen.shells.fetch_add(1, Ordering::SeqCst);
        self.seen.event("shell");
        self.shells.insert(channel);
        session.channel_success(channel)?;
        session.data(channel, format!("welcome {}\r\n", self.index).into_bytes())?;
        if let (Some(ms), 0) = (self.options.cut_shell_after, self.index) {
            let handle = session.handle();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                let _ = handle.disconnect(Disconnect::ByApplication, "cut".into(), "en".into()).await;
            });
        }
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        if !self.shells.contains(&channel) {
            return Ok(());
        }
        let text = String::from_utf8_lossy(data).to_string();
        if let (Some(total), "flood") = (self.options.shell_flood, text.trim()) {
            // Sent from a task of its own through the session's handle, so the handler returns at once and the
            // connection keeps serving its other channels while the flood runs.
            let handle = session.handle();
            tokio::spawn(async move {
                let mut sent = 0usize;
                while sent < total {
                    let n = (total - sent).min(16 * 1024);
                    let chunk: Vec<u8> = (sent..sent + n).map(|i| b'a' + (i % 26) as u8).collect();
                    if handle.data(channel, chunk).await.is_err() {
                        return;
                    }
                    sent += n;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                let _ = handle.data(channel, b"flood done\r\n".to_vec()).await;
            });
            return Ok(());
        }
        if text.trim() == "exit" {
            session.exit_status_request(channel, 3)?;
            session.close(channel)?;
        } else {
            session.data(channel, format!("echo:{text}").into_bytes())?;
        }
        Ok(())
    }

    async fn streamlocal_forward(&mut self, socket_path: &str, _session: &mut Session) -> Result<bool, Self::Error> {
        self.seen.forwards.lock().unwrap().push(socket_path.to_string());
        self.seen.event(format!("forward {socket_path}"));
        if self.options.forward && self.options.stale_sockets && !self.seen.bound_sockets.lock().unwrap().insert(socket_path.to_string()) {
            // Address already in use: the socket of a login that was lost is still there.
            return Ok(false);
        }
        Ok(self.options.forward)
    }

    async fn cancel_streamlocal_forward(&mut self, socket_path: &str, _session: &mut Session) -> Result<bool, Self::Error> {
        self.seen.cancelled_forwards.lock().unwrap().push(socket_path.to_string());
        self.seen.event(format!("cancel {socket_path}"));
        self.seen.bound_sockets.lock().unwrap().remove(socket_path);
        Ok(true)
    }

    async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        self.channels.remove(&channel);
        let command = String::from_utf8_lossy(data).to_string();
        self.seen.commands.lock().unwrap().push(command.clone());
        self.seen.event("exec");
        if self.options.refuse_exec_once && self.seen.refused_execs.fetch_add(1, Ordering::SeqCst) == 0 {
            session.channel_failure(channel)?;
            return Ok(());
        }
        if self.options.wrapped_shell && command.starts_with("exec sh -c 'HOLZWAVE_") {
            self.seen.execs_as_shell.fetch_add(1, Ordering::SeqCst);
            self.shells.insert(channel);
            session.channel_success(channel)?;
            session.data(channel, format!("welcome {}\r\n", self.index).into_bytes())?;
            return Ok(());
        }
        if let Some((_, replies)) = self.options.exec_script.iter().find(|(text, _)| *text == command) {
            let call = {
                let mut calls = self.seen.script_calls.lock().unwrap();
                let call = calls.entry(command.clone()).or_insert(0);
                *call += 1;
                *call - 1
            };
            let (bytes, status) = replies[call.min(replies.len() - 1)].clone();
            session.channel_success(channel)?;
            if !bytes.is_empty() {
                session.data(channel, bytes)?;
            }
            session.exit_status_request(channel, status)?;
            session.eof(channel)?;
            session.close(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        match &self.options.exec_reply {
            Some(bytes) => session.data(channel, bytes.clone())?,
            None => session.data(channel, format!("ran:{command}\r\n").into_bytes())?,
        }
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

// ---- the SFTP subsystem ------------------------------------------------------------------------------------------

/// An SFTP handler that serves a real directory with `std::fs` and refuses every path outside it.
struct Files {
    root: PathBuf,
    handles: HashMap<String, Open>,
    next: u32,
    seen: Arc<Seen>,
    options: Options,
}

enum Open {
    Dir(Vec<File>),
    File(std::fs::File),
}

fn status(id: u32, code: StatusCode) -> Status {
    Status { id, status_code: code, error_message: format!("{code:?}"), language_tag: "en".into() }
}

fn io_status(e: &std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

impl Files {
    /// The path as a path below the root, or a refusal: no `..`, and the root is a prefix.
    fn guard(&self, path: &str) -> Result<PathBuf, StatusCode> {
        let path = Path::new(path);
        let climbs = path.components().any(|part| matches!(part, std::path::Component::ParentDir));
        if climbs || !path.starts_with(&self.root) {
            return Err(StatusCode::PermissionDenied);
        }
        Ok(path.to_path_buf())
    }

    /// The path relative to the root, with a leading slash; a path outside it stays as it is.
    fn relative(&self, path: &str) -> String {
        Path::new(path).strip_prefix(&self.root).map_or_else(|_| path.to_owned(), |p| format!("/{}", p.display()))
    }

    /// Record one operation, the path relative to the root.
    fn log(&self, what: &str, path: &str) {
        let line = format!("{what} {}", self.relative(path));
        self.seen.sftp_ops.lock().unwrap().push(line);
    }

    fn handle(&mut self, open: Open) -> String {
        self.next += 1;
        let handle = format!("h{}", self.next);
        self.handles.insert(handle.clone(), open);
        handle
    }
}

impl russh_sftp::server::Handler for Files {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, _v: u32, _e: HashMap<String, String>) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let real = if path == "." { self.root.clone() } else { std::fs::canonicalize(self.guard(&path)?).map_err(|e| io_status(&e))? };
        Ok(Name { id, files: vec![File::dummy(real.to_string_lossy().into_owned())] })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<SftpHandle, Self::Error> {
        let mut files = Vec::new();
        for item in std::fs::read_dir(self.guard(&path)?).map_err(|e| io_status(&e))? {
            let item = item.map_err(|e| io_status(&e))?;
            let meta = std::fs::symlink_metadata(item.path()).map_err(|e| io_status(&e))?;
            files.push(File::new(item.file_name().to_string_lossy().into_owned(), FileAttributes::from(&meta)));
        }
        Ok(SftpHandle { id, handle: self.handle(Open::Dir(files)) })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(Open::Dir(files)) if !files.is_empty() => Ok(Name { id, files: std::mem::take(files) }),
            _ => Err(StatusCode::Eof),
        }
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let meta = std::fs::metadata(self.guard(&path)?).map_err(|e| io_status(&e))?;
        Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let meta = std::fs::symlink_metadata(self.guard(&path)?).map_err(|e| io_status(&e))?;
        Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        match self.handles.get(&handle) {
            Some(Open::File(f)) => Ok(Attrs { id, attrs: FileAttributes::from(&f.metadata().map_err(|e| io_status(&e))?) }),
            _ => Err(StatusCode::Failure),
        }
    }

    async fn open(&mut self, id: u32, path: String, flags: OpenFlags, _a: FileAttributes) -> Result<SftpHandle, Self::Error> {
        if flags.contains(OpenFlags::WRITE) {
            self.log("open-write", &path);
            if self.options.sftp_deny_upload {
                return Err(StatusCode::PermissionDenied);
            }
        }
        let path = self.guard(&path)?;
        let mut options = std::fs::OpenOptions::new();
        options
            .read(flags.contains(OpenFlags::READ))
            .write(flags.contains(OpenFlags::WRITE))
            .create(flags.contains(OpenFlags::CREATE))
            .truncate(flags.contains(OpenFlags::TRUNCATE));
        let file = options.open(path).map_err(|e| io_status(&e))?;
        Ok(SftpHandle { id, handle: self.handle(Open::File(file)) })
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, Self::Error> {
        let Some(Open::File(file)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        file.seek(SeekFrom::Start(offset)).map_err(|e| io_status(&e))?;
        let mut buf = vec![0u8; len as usize];
        let n = file.read(&mut buf).map_err(|e| io_status(&e))?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, Self::Error> {
        if self.options.sftp_fail_writes {
            return Err(StatusCode::Failure);
        }
        let Some(Open::File(file)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        file.seek(SeekFrom::Start(offset)).map_err(|e| io_status(&e))?;
        file.write_all(&data).map_err(|e| io_status(&e))?;
        Ok(status(id, StatusCode::Ok))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(status(id, StatusCode::Ok))
    }

    async fn mkdir(&mut self, id: u32, path: String, attrs: FileAttributes) -> Result<Status, Self::Error> {
        self.log("mkdir", &path);
        let dir = self.guard(&path)?;
        std::fs::create_dir(&dir).map_err(|e| io_status(&e))?;
        #[cfg(unix)]
        if let Some(mode) = attrs.permissions {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode & 0o7777)).map_err(|e| io_status(&e))?;
        }
        #[cfg(not(unix))]
        let _ = attrs;
        Ok(status(id, StatusCode::Ok))
    }

    async fn setstat(&mut self, id: u32, path: String, attrs: FileAttributes) -> Result<Status, Self::Error> {
        self.log(&format!("setstat {:o}", attrs.permissions.map_or(0, |m| m & 0o7777)), &path);
        let path = self.guard(&path)?;
        #[cfg(unix)]
        if let Some(mode) = attrs.permissions {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode & 0o7777)).map_err(|e| io_status(&e))?;
        }
        #[cfg(not(unix))]
        let _ = (path, attrs);
        Ok(status(id, StatusCode::Ok))
    }

    async fn rename(&mut self, id: u32, old: String, new: String) -> Result<Status, Self::Error> {
        let line = format!("rename {} {}", self.relative(&old), self.relative(&new));
        self.seen.sftp_ops.lock().unwrap().push(line);
        if self.options.sftp_refuse_overwrite && self.guard(&new)?.exists() {
            return Err(StatusCode::Failure);
        }
        std::fs::rename(self.guard(&old)?, self.guard(&new)?).map_err(|e| io_status(&e))?;
        Ok(status(id, StatusCode::Ok))
    }

    async fn remove(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        self.log("remove", &path);
        std::fs::remove_file(self.guard(&path)?).map_err(|e| io_status(&e))?;
        Ok(status(id, StatusCode::Ok))
    }
}
