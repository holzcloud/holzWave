//! SSH as a [`Link`]: a shell on another machine, over `russh`, with a reconnect that keeps the pane.
//!
//! The pane reads and writes plain bytes; this module runs the protocol on a thread of its own with a small
//! single-threaded tokio runtime. When the connection is lost the pane stays: a line says so on its screen, what is
//! typed meanwhile is kept (up to [`MAX_BUFFERED`] bytes) and the connection is made again, up to
//! [`Reconnect::attempts`] times. A shell that ends on its own, a refused login and a changed host key end the pane
//! at once, because trying again cannot change them.
//!
//! Secrets (a key's passphrase, a password) are given by the caller in [`SshAuth`] and are never written to disk or
//! to the screen here. Host keys are checked against a `known_hosts` file ([`HostKeyPolicy`]).

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use russh::client::{self, Handle};
use russh::keys::{PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate, known_hosts, load_secret_key};
use russh::{ChannelMsg, Disconnect};
use tokio::sync::mpsc as tmpsc;

use crate::conn::{ConnState, Link, SharedState};
use crate::{Error, Result};

/// The most input kept while the connection is down. More is dropped, so a stuck key cannot fill the memory.
pub const MAX_BUFFERED: usize = 64 * 1024;

/// How long one attempt to connect and log in may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How to log in.
#[derive(Clone)]
pub enum SshAuth {
    /// A private key file, with its passphrase if it has one.
    Key { path: PathBuf, passphrase: Option<String> },
    /// A password.
    Password(String),
}

impl std::fmt::Debug for SshAuth {
    // A secret never reaches a log line through `{:?}`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SshAuth::Key { path, passphrase } => f
                .debug_struct("Key")
                .field("path", path)
                .field("passphrase", &passphrase.as_ref().map(|_| "<set>"))
                .finish(),
            SshAuth::Password(_) => f.write_str("Password(<set>)"),
        }
    }
}

/// What to do with a host key the `known_hosts` file does not know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HostKeyPolicy {
    /// Learn it and go on (trust on first use); a key that differs from a known one is always refused.
    #[default]
    AcceptNew,
    /// Refuse an unknown key.
    Strict,
    /// Accept any key. For tests only.
    AcceptAny,
}

/// How often and how fast a lost connection is made again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reconnect {
    /// Attempts in a row before the pane gives up. 0 does not reconnect.
    pub attempts: u32,
    /// The wait before each attempt.
    pub delay: Duration,
}

impl Default for Reconnect {
    fn default() -> Self {
        Self { attempts: 5, delay: Duration::from_secs(3) }
    }
}

/// Where and how to connect.
#[derive(Clone, Debug)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    /// A host to go through first (`ProxyJump`). It has its own login.
    pub jump: Option<Box<SshTarget>>,
    /// A line typed into the shell once it runs, such as `tmux attach -t main`.
    pub startup: Option<String>,
    pub reconnect: Reconnect,
    /// The file that holds the known host keys. `None` accepts any key.
    pub known_hosts: Option<PathBuf>,
    pub host_key: HostKeyPolicy,
    /// The `TERM` the remote shell is told.
    pub term: String,
}

impl SshTarget {
    pub fn new(host: impl Into<String>, user: impl Into<String>, auth: SshAuth) -> Self {
        Self {
            host: host.into(),
            port: 22,
            user: user.into(),
            auth,
            jump: None,
            startup: None,
            reconnect: Reconnect::default(),
            known_hosts: None,
            host_key: HostKeyPolicy::default(),
            term: "xterm-256color".into(),
        }
    }
}

/// What the connection thread is told.
enum Cmd {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16, px_w: u32, px_h: u32 },
    Close,
}

/// How one connected session ended.
enum Outcome {
    /// The remote shell ended with this code.
    Exit(u32),
    /// The user closed the pane.
    Closed,
    /// The connection broke; trying again may help.
    Lost(String),
    /// Trying again cannot help (login refused, host key changed, unusable key).
    Fatal(String),
}

/// The ends the pane reads and writes.
struct ChanReader {
    rx: mpsc::Receiver<Vec<u8>>,
    pending: Vec<u8>,
    pos: usize,
}

impl Read for ChanReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.pending.len() {
            match self.rx.recv() {
                Ok(chunk) => {
                    self.pending = chunk;
                    self.pos = 0;
                }
                // The thread ended: end of output.
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.pending.len() - self.pos);
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

struct ChanWriter {
    tx: tmpsc::UnboundedSender<Cmd>,
}

impl Write for ChanWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tx
            .send(Cmd::Data(buf.to_vec()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the connection ended"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A shell on another machine.
pub struct SshLink {
    reader: Option<Box<dyn Read + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    cmd: tmpsc::UnboundedSender<Cmd>,
    state: SharedState,
    exit: Arc<Mutex<Option<u32>>>,
    thread: Option<JoinHandle<()>>,
}

impl SshLink {
    /// Start connecting to `target` with a grid of `cols` x `rows`. Returns at once; the connection is made on a
    /// thread of its own and its progress is in [`SshLink::state`] and on the pane's screen.
    pub fn connect(target: SshTarget, cols: u16, rows: u16) -> Result<Self> {
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
        let (cmd_tx, cmd_rx) = tmpsc::unbounded_channel::<Cmd>();
        let state = SharedState::new(ConnState::Connecting);
        let exit = Arc::new(Mutex::new(None));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::Pty(format!("ssh runtime: {e}")))?;
        let thread = {
            let (state, exit) = (state.clone(), exit.clone());
            thread::Builder::new().name("ssh".into()).spawn(move || {
                let code = runtime.block_on(run(target, (cols, rows), cmd_rx, out_tx, state.clone()));
                *exit.lock().unwrap_or_else(PoisonError::into_inner) = Some(code);
                state.set(ConnState::Closed);
            })?
        };
        Ok(Self {
            reader: Some(Box::new(ChanReader { rx: out_rx, pending: Vec::new(), pos: 0 })),
            writer: Some(Box::new(ChanWriter { tx: cmd_tx.clone() })),
            cmd: cmd_tx,
            state,
            exit,
            thread: Some(thread),
        })
    }

    /// The state of the connection.
    pub fn state(&self) -> ConnState {
        self.state.get()
    }

    /// A handle on the state that outlives the link, for the code that shows it.
    pub fn shared_state(&self) -> SharedState {
        self.state.clone()
    }
}

impl Link for SshLink {
    fn take_reader(&mut self) -> Option<Box<dyn Read + Send>> {
        self.reader.take()
    }

    fn take_writer(&mut self) -> Option<Box<dyn Write + Send>> {
        self.writer.take()
    }

    fn process_id(&self) -> Option<u32> {
        None
    }

    fn resize_px(&self, cols: u16, rows: u16, px_w: u32, px_h: u32) -> Result<()> {
        self.cmd
            .send(Cmd::Resize { cols, rows, px_w, px_h })
            .map_err(|_| Error::Pty("the connection ended".into()))
    }

    fn try_wait(&mut self) -> Result<Option<u32>> {
        Ok(*self.exit.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn close(mut self: Box<Self>) -> Result<u32> {
        let _ = self.cmd.send(Cmd::Close);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let code = *self.exit.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(code.unwrap_or(0))
    }

    fn state(&self) -> ConnState {
        self.state.get()
    }

    fn shared_state(&self) -> Option<SharedState> {
        Some(self.state.clone())
    }
}

/// The text a pane shows for something the connection has to say: dim, on a line of its own, never an escape
/// sequence from the network.
fn notice(text: &str) -> Vec<u8> {
    let clean: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    format!("\r\n\x1b[33m{clean}\x1b[0m\r\n").into_bytes()
}

/// The whole life of a connection: connect, run, and connect again while that makes sense. Returns the exit code.
async fn run(
    target: SshTarget,
    size: (u16, u16),
    mut cmd: tmpsc::UnboundedReceiver<Cmd>,
    out: mpsc::Sender<Vec<u8>>,
    state: SharedState,
) -> u32 {
    let mut size = (size.0, size.1, 0u32, 0u32);
    let mut buffered: Vec<u8> = Vec::new();
    let mut attempt = 0u32;
    loop {
        let outcome = session(&target, &mut size, &mut cmd, &out, &state, &mut buffered, &mut attempt).await;
        match outcome {
            Outcome::Exit(code) => return code,
            Outcome::Closed => return 0,
            Outcome::Fatal(why) => {
                let text = format!("Connection to {} failed: {why}.", target.host);
                if !wait_for_enter(&text, &mut cmd, &out, &state, &mut size, &mut buffered).await {
                    return 255;
                }
                attempt = 0;
            }
            Outcome::Lost(why) => {
                if attempt >= target.reconnect.attempts {
                    let text = format!("Connection to {} lost: {why}. Giving up.", target.host);
                    if !wait_for_enter(&text, &mut cmd, &out, &state, &mut size, &mut buffered).await {
                        return 255;
                    }
                    attempt = 0;
                    continue;
                }
                attempt += 1;
                let of = target.reconnect.attempts;
                let in_secs = target.reconnect.delay.as_secs();
                state.set(ConnState::Reconnecting { attempt, of, in_secs });
                let _ = out.send(notice(&format!(
                    "Connection lost: {why}. Retrying {attempt}/{of} in {in_secs} s. What you type is kept."
                )));
                // Wait, and keep listening: input is kept, a resize is remembered, a close ends it.
                let sleep = tokio::time::sleep(target.reconnect.delay);
                tokio::pin!(sleep);
                loop {
                    tokio::select! {
                        () = &mut sleep => break,
                        msg = cmd.recv() => match msg {
                            None | Some(Cmd::Close) => return 0,
                            Some(Cmd::Data(bytes)) => keep(&mut buffered, &bytes),
                            Some(Cmd::Resize { cols, rows, px_w, px_h }) => size = (cols, rows, px_w, px_h),
                        },
                    }
                }
            }
        }
    }
}

/// The pane stays open after a failure, so the reason can be read. It says so and waits: Enter tries again (`true`),
/// closing the pane ends it (`false`). What else is typed is dropped.
async fn wait_for_enter(
    text: &str,
    cmd: &mut tmpsc::UnboundedReceiver<Cmd>,
    out: &mpsc::Sender<Vec<u8>>,
    state: &SharedState,
    size: &mut (u16, u16, u32, u32),
    buffered: &mut Vec<u8>,
) -> bool {
    state.set(ConnState::Failed);
    let _ = out.send(notice(&format!("{text} Press Enter to try again.")));
    buffered.clear();
    loop {
        match cmd.recv().await {
            None | Some(Cmd::Close) => return false,
            Some(Cmd::Data(bytes)) if bytes.iter().any(|b| *b == b'\r' || *b == b'\n') => {
                state.set(ConnState::Connecting);
                return true;
            }
            Some(Cmd::Data(_)) => {}
            Some(Cmd::Resize { cols, rows, px_w, px_h }) => *size = (cols, rows, px_w, px_h),
        }
    }
}

fn keep(buffered: &mut Vec<u8>, bytes: &[u8]) {
    let room = MAX_BUFFERED.saturating_sub(buffered.len());
    buffered.extend_from_slice(&bytes[..bytes.len().min(room)]);
}

/// The host key check of one connection.
struct Client {
    host: String,
    port: u16,
    known_hosts: Option<PathBuf>,
    policy: HostKeyPolicy,
    /// Why the key was refused, for the message.
    refused: Arc<Mutex<Option<String>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> std::result::Result<bool, Self::Error> {
        let refuse = |why: String| {
            *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = Some(why);
            Ok(false)
        };
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return refuse("the server offered a certificate, which is not supported".into());
        };
        let (Some(path), policy) = (&self.known_hosts, self.policy) else {
            return Ok(true);
        };
        if policy == HostKeyPolicy::AcceptAny {
            return Ok(true);
        }
        match known_hosts::check_known_hosts_path(&self.host, self.port, key, path) {
            Ok(true) => Ok(true),
            Ok(false) => match policy {
                HostKeyPolicy::Strict => refuse(format!("the host key of {} is not known", self.host)),
                _ => {
                    // Trust on first use: remember it.
                    let _ = known_hosts::learn_known_hosts_path(&self.host, self.port, key, path);
                    Ok(true)
                }
            },
            Err(russh::keys::Error::KeyChanged { line }) => refuse(format!(
                "the host key of {} CHANGED (line {line} of the known hosts file). Someone may be intercepting the connection",
                self.host
            )),
            Err(e) => refuse(format!("the known hosts file could not be read: {e}")),
        }
    }
}

/// An error that trying again cannot cure.
fn is_fatal(error: &russh::Error) -> bool {
    matches!(error, russh::Error::UnknownKey | russh::Error::NotAuthenticated | russh::Error::KeyChanged { .. })
}

/// Open a session to `target`, going through its jump host if it has one, and log in.
async fn open(target: &SshTarget) -> std::result::Result<Handle<Client>, Outcome> {
    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    });
    let refused = Arc::new(Mutex::new(None));
    let handler = |t: &SshTarget| Client {
        host: t.host.clone(),
        port: t.port,
        known_hosts: t.known_hosts.clone(),
        policy: t.host_key,
        refused: refused.clone(),
    };
    let lost = |e: &dyn std::fmt::Display| Outcome::Lost(e.to_string());
    let why_refused = || refused.lock().unwrap_or_else(PoisonError::into_inner).clone();

    let mut session = if let Some(jump) = &target.jump {
        let via = Box::pin(open(jump)).await?;
        let channel = via
            .channel_open_direct_tcpip(target.host.clone(), target.port as u32, "127.0.0.1", 0)
            .await
            .map_err(|e| lost(&e))?;
        client::connect_stream(config, channel.into_stream(), handler(target)).await
    } else {
        let connect = client::connect(config, (target.host.as_str(), target.port), handler(target));
        match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
            Ok(result) => result,
            Err(_) => return Err(Outcome::Lost("the connection timed out".into())),
        }
    }
    .map_err(|e| match why_refused() {
        Some(why) => Outcome::Fatal(why),
        None if is_fatal(&e) => Outcome::Fatal(e.to_string()),
        None => lost(&e),
    })?;

    let authenticated = match &target.auth {
        SshAuth::Password(password) => session
            .authenticate_password(target.user.clone(), password.clone())
            .await
            .map_err(|e| lost(&e))?,
        SshAuth::Key { path, passphrase } => {
            let key = load_secret_key(path, passphrase.as_deref())
                .map_err(|e| Outcome::Fatal(format!("the key {} cannot be used: {e}", path.display())))?;
            let hash = session.best_supported_rsa_hash().await.map_err(|e| lost(&e))?.flatten();
            session
                .authenticate_publickey(target.user.clone(), PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(|e| lost(&e))?
        }
    };
    if !authenticated.success() {
        return Err(Outcome::Fatal(format!("{}@{} refused the login", target.user, target.host)));
    }
    Ok(session)
}

/// One connected session: log in, open the shell, and pump bytes until it ends.
async fn session(
    target: &SshTarget,
    size: &mut (u16, u16, u32, u32),
    cmd: &mut tmpsc::UnboundedReceiver<Cmd>,
    out: &mpsc::Sender<Vec<u8>>,
    state: &SharedState,
    buffered: &mut Vec<u8>,
    attempt: &mut u32,
) -> Outcome {
    let handle = match open(target).await {
        Ok(handle) => handle,
        Err(outcome) => return outcome,
    };
    let mut channel = match handle.channel_open_session().await {
        Ok(channel) => channel,
        Err(e) => return Outcome::Lost(e.to_string()),
    };
    let (cols, rows, px_w, px_h) = *size;
    if let Err(e) = channel.request_pty(false, &target.term, cols as u32, rows as u32, px_w, px_h, &[]).await {
        return Outcome::Lost(e.to_string());
    }
    if let Err(e) = channel.request_shell(false).await {
        return Outcome::Lost(e.to_string());
    }
    // From here on the connection works: the count of failed attempts starts again.
    *attempt = 0;
    state.set(ConnState::Connected);
    if let Some(line) = &target.startup {
        let _ = channel.data_bytes(format!("{line}\n").into_bytes()).await;
    }
    if !buffered.is_empty() {
        let kept = std::mem::take(buffered);
        let _ = channel.data_bytes(kept).await;
    }

    let mut exit: Option<u32> = None;
    let outcome = loop {
        tokio::select! {
            msg = cmd.recv() => match msg {
                None | Some(Cmd::Close) => {
                    let _ = channel.close().await;
                    break Outcome::Closed;
                }
                Some(Cmd::Data(bytes)) => {
                    if let Err(e) = channel.data_bytes(bytes).await {
                        break Outcome::Lost(e.to_string());
                    }
                }
                Some(Cmd::Resize { cols, rows, px_w, px_h }) => {
                    *size = (cols, rows, px_w, px_h);
                    if let Err(e) = channel.window_change(cols as u32, rows as u32, px_w, px_h).await {
                        break Outcome::Lost(e.to_string());
                    }
                }
            },
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) => {
                    if out.send(data.to_vec()).is_err() {
                        break Outcome::Closed;
                    }
                }
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    if out.send(data.to_vec()).is_err() {
                        break Outcome::Closed;
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => exit = Some(exit_status),
                Some(ChannelMsg::ExitSignal { .. }) => exit = Some(128),
                Some(ChannelMsg::Eof | ChannelMsg::Close) => break Outcome::Exit(exit.unwrap_or(0)),
                Some(_) => {}
                // The channel's stream ended without a close: the network went away.
                None => break match exit {
                    Some(code) => Outcome::Exit(code),
                    None => Outcome::Lost("the connection was reset".into()),
                },
            },
        }
    };
    let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
    outcome
}

/// Check a public key against a known hosts file; used by the connection manager to show a fingerprint.
pub fn learn_host_key(host: &str, port: u16, key: &PublicKey, path: &std::path::Path) -> Result<()> {
    known_hosts::learn_known_hosts_path(host, port, key, path).map_err(|e| Error::Pty(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::{Algorithm, PrivateKey};
    use russh::server::{self, Auth, Msg, Server as _, Session};
    use russh::{Channel, ChannelId};
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;
    use tokio::net::TcpListener;

    /// A tiny SSH server: password `pw` for user `holz`, a shell that greets, echoes, and ends on the line `exit`.
    #[derive(Clone)]
    struct Fake {
        connections: Arc<AtomicUsize>,
        /// Cut the first connection a moment after the shell starts.
        drop_first: bool,
        resizes: Arc<Mutex<Vec<(u32, u32)>>>,
        index: usize,
    }

    impl server::Server for Fake {
        type Handler = Self;
        fn new_client(&mut self, _: Option<SocketAddr>) -> Self {
            let mut handler = self.clone();
            handler.index = self.connections.fetch_add(1, Ordering::SeqCst);
            handler
        }
    }

    impl server::Handler for Fake {
        type Error = russh::Error;

        async fn auth_password(&mut self, user: &str, password: &str) -> std::result::Result<Auth, Self::Error> {
            Ok(if user == "holz" && password == "pw" { Auth::Accept } else { Auth::reject() })
        }

        async fn channel_open_session(
            &mut self,
            _channel: Channel<Msg>,
            reply: server::ChannelOpenHandle,
            _session: &mut Session,
        ) -> std::result::Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
        }

        async fn pty_request(
            &mut self,
            channel: ChannelId,
            _term: &str,
            cols: u32,
            rows: u32,
            _px_w: u32,
            _px_h: u32,
            _modes: &[(russh::Pty, u32)],
            session: &mut Session,
        ) -> std::result::Result<(), Self::Error> {
            self.resizes.lock().unwrap().push((cols, rows));
            session.channel_success(channel)?;
            Ok(())
        }

        async fn window_change_request(
            &mut self,
            _channel: ChannelId,
            cols: u32,
            rows: u32,
            _px_w: u32,
            _px_h: u32,
            _session: &mut Session,
        ) -> std::result::Result<(), Self::Error> {
            self.resizes.lock().unwrap().push((cols, rows));
            Ok(())
        }

        async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> std::result::Result<(), Self::Error> {
            session.channel_success(channel)?;
            session.data(channel, format!("welcome {}\r\n", self.index).into_bytes())?;
            if self.drop_first && self.index == 0 {
                let handle = session.handle();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let _ = handle.disconnect(Disconnect::ByApplication, "cut".into(), "en".into()).await;
                });
            }
            Ok(())
        }

        async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> std::result::Result<(), Self::Error> {
            let text = String::from_utf8_lossy(data).to_string();
            if text.trim() == "exit" {
                session.exit_status_request(channel, 3)?;
                session.close(channel)?;
            } else {
                session.data(channel, format!("echo:{text}").into_bytes())?;
            }
            Ok(())
        }
    }

    struct Running {
        addr: SocketAddr,
        key: PrivateKey,
        resizes: Arc<Mutex<Vec<(u32, u32)>>>,
        connections: Arc<AtomicUsize>,
    }

    /// Start the fake server on a thread of its own; it runs until the process ends.
    fn serve(drop_first: bool) -> Running {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let config = Arc::new(server::Config {
            keys: vec![key.clone()],
            auth_rejection_time: Duration::from_millis(0),
            auth_rejection_time_initial: Some(Duration::from_millis(0)),
            ..Default::default()
        });
        let resizes = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let mut fake = Fake { connections: connections.clone(), drop_first, resizes: resizes.clone(), index: 0 };
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let socket = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                tx.send(socket.local_addr().unwrap()).unwrap();
                let _ = fake.run_on_socket(config, &socket).await;
            });
        });
        Running { addr: rx.recv().unwrap(), key, resizes, connections }
    }

    fn target(addr: SocketAddr) -> SshTarget {
        let mut t = SshTarget::new("127.0.0.1", "holz", SshAuth::Password("pw".into()));
        t.port = addr.port();
        t.reconnect = Reconnect { attempts: 3, delay: Duration::from_millis(100) };
        t
    }

    /// The link's output, read by one helper thread, so a test can wait for text as often as it likes.
    struct Out {
        rx: mpsc::Receiver<Vec<u8>>,
        seen: String,
    }

    impl Out {
        fn new(mut reader: Box<dyn Read + Send>) -> Self {
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

        /// Wait until `needle` has been seen in the output, or fail after `secs` seconds.
        fn until(&mut self, needle: &str, secs: u64) {
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

    #[test]
    fn a_login_gives_a_shell_that_echoes_and_ends_with_its_code() {
        let server = serve(false);
        let mut link = SshLink::connect(target(server.addr), 100, 30).unwrap();
        let mut out = Out::new(link.take_reader().unwrap());
        let mut writer = link.take_writer().unwrap();
        out.until("welcome 0", 10);
        assert_eq!(link.state(), ConnState::Connected);
        assert_eq!(server.resizes.lock().unwrap()[0], (100, 30), "the PTY request carries the size");
        link.resize_px(120, 40, 0, 0).unwrap();
        let _ = writer.write(b"exit\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut boxed: Box<dyn Link> = Box::new(link);
        while boxed.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the shell's exit was never seen");
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(boxed.try_wait().unwrap(), Some(3), "the remote exit code");
        assert!(server.resizes.lock().unwrap().contains(&(120, 40)), "the resize reached the server");
        assert_eq!(boxed.close().unwrap(), 3);
    }

    #[test]
    fn what_is_typed_reaches_the_shell() {
        let server = serve(false);
        let mut link = SshLink::connect(target(server.addr), 80, 24).unwrap();
        let mut out = Out::new(link.take_reader().unwrap());
        let mut writer = link.take_writer().unwrap();
        out.until("welcome 0", 10);
        writer.write_all(b"hello").unwrap();
        out.until("echo:hello", 10);
        Box::new(link).close().unwrap();
    }

    #[test]
    fn a_wrong_password_keeps_the_pane_open_with_the_reason_and_enter_tries_again() {
        let server = serve(false);
        let mut t = target(server.addr);
        t.auth = SshAuth::Password("wrong".into());
        let mut link = SshLink::connect(t, 80, 24).unwrap();
        let mut out = Out::new(link.take_reader().unwrap());
        let mut writer = link.take_writer().unwrap();
        out.until("refused the login", 10);
        out.until("Press Enter to try again", 10);
        assert_eq!(link.state(), ConnState::Failed);
        assert_eq!(link.try_wait().unwrap(), None, "the pane is not ended: the reason stays on the screen");
        assert_eq!(server.connections.load(Ordering::SeqCst), 1, "a refused login is not tried again by itself");
        writer.write_all(b"x").unwrap();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(server.connections.load(Ordering::SeqCst), 1, "only Enter tries again");
        writer.write_all(b"\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while server.connections.load(Ordering::SeqCst) < 2 {
            assert!(Instant::now() < deadline, "Enter did not try again");
            thread::sleep(Duration::from_millis(20));
        }
        let boxed: Box<dyn Link> = Box::new(link);
        assert_eq!(boxed.close().unwrap(), 255);
    }

    #[test]
    fn a_lost_connection_is_made_again_and_the_typing_in_between_is_kept() {
        let server = serve(true);
        let mut link = SshLink::connect(target(server.addr), 80, 24).unwrap();
        let mut out = Out::new(link.take_reader().unwrap());
        let mut writer = link.take_writer().unwrap();
        out.until("welcome 0", 10);
        // The server cuts the connection 200 ms after the greeting; the pane says so and tries again.
        out.until("Retrying 1/3", 10);
        writer.write_all(b"kept").unwrap();
        out.until("welcome 1", 10);
        out.until("echo:kept", 10);
        assert_eq!(link.state(), ConnState::Connected);
        assert_eq!(server.connections.load(Ordering::SeqCst), 2);
        Box::new(link).close().unwrap();
    }

    #[test]
    fn a_pane_gives_up_after_its_attempts_and_waits_for_enter() {
        // Nothing listens on this port: bind and drop to find a free one.
        let port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
        let mut t = SshTarget::new("127.0.0.1", "holz", SshAuth::Password("pw".into()));
        t.port = port;
        t.reconnect = Reconnect { attempts: 2, delay: Duration::from_millis(50) };
        let mut link = SshLink::connect(t, 80, 24).unwrap();
        let mut out = Out::new(link.take_reader().unwrap());
        out.until("Giving up", 15);
        assert!(out.seen.contains("Retrying 2/2"), "{:?}", out.seen);
        out.until("Press Enter to try again", 5);
        assert_eq!(link.state(), ConnState::Failed);
        assert_eq!(link.try_wait().unwrap(), None);
        let boxed: Box<dyn Link> = Box::new(link);
        assert_eq!(boxed.close().unwrap(), 255);
    }

    #[test]
    fn host_keys_are_learned_once_and_a_change_is_refused() {
        let dir = std::env::temp_dir().join(format!("holzwave-ssh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let known = dir.join("known_hosts");
        let _ = std::fs::remove_file(&known);

        let strict = {
            let server = serve(false);
            let mut t = target(server.addr);
            t.known_hosts = Some(known.clone());
            t.host_key = HostKeyPolicy::Strict;
            t
        };
        let mut link = SshLink::connect(strict, 80, 24).unwrap();
        let mut out = Out::new(link.take_reader().unwrap());
        out.until("is not known", 10);

        let first = serve(false);
        let mut t = target(first.addr);
        t.known_hosts = Some(known.clone());
        let mut link = SshLink::connect(t, 80, 24).unwrap();
        let mut out = Out::new(link.take_reader().unwrap());
        out.until("welcome 0", 10);
        Box::new(link).close().unwrap();
        let learned = std::fs::read_to_string(&known).unwrap();
        assert!(learned.contains("ssh-ed25519"), "{learned}");

        // Another server with another key under the same host and port name: rewrite the port in the file.
        let second = serve(false);
        let rewritten = learned.replace(&format!("[127.0.0.1]:{}", first.addr.port()), &format!("[127.0.0.1]:{}", second.addr.port()));
        let rewritten = if rewritten == learned { learned.replace("127.0.0.1", &format!("[127.0.0.1]:{}", second.addr.port())) } else { rewritten };
        std::fs::write(&known, rewritten).unwrap();
        let mut t = target(second.addr);
        t.known_hosts = Some(known.clone());
        let mut link = SshLink::connect(t, 80, 24).unwrap();
        let mut out = Out::new(link.take_reader().unwrap());
        out.until("CHANGED", 10);
        let _ = second.key;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_secret_never_shows_in_debug_output() {
        let auth = SshAuth::Password("hunter2".into());
        assert!(!format!("{auth:?}").contains("hunter2"));
        let auth = SshAuth::Key { path: "/k".into(), passphrase: Some("hunter2".into()) };
        assert!(!format!("{auth:?}").contains("hunter2"));
    }
}
