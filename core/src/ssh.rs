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
//!
//! [`SshExec`] is the other way in: it runs one command at a time per call on the host of a saved connection, on a
//! channel that asks for no terminal, no environment and no agent forwarding, and never types into a pane. It never
//! learns a host key (an unknown or changed key is refused), keeps only the last bytes of each output stream up to a
//! cap, ends a command at a time limit or when the caller cancels, and reports in an [`ExecResult`] what the command
//! printed, its exit status and how it ended ([`ExecEnd`]). A command is written at most once: a connection lost while
//! it runs is reported as [`ExecEnd::Lost`] and the command is not run again.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use russh::client::{self, Handle};
use russh::keys::{PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate, known_hosts, load_secret_key};
use russh::{Channel, ChannelMsg, Disconnect, Sig};
use tokio::sync::{mpsc as tmpsc, watch};

use crate::conn::{ConnState, Link, SharedState};
use crate::pool::{Connection, Open};
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
pub(crate) enum Outcome {
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
pub(crate) struct Client {
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

/// Open a session to `target`, going through its jump host if it has one, and log in. The pane's connection loop uses it
/// directly; everything else gets its session from a [`crate::pool::Connection`], which calls it.
pub(crate) async fn open(target: &SshTarget) -> std::result::Result<Handle<Client>, Outcome> {
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
                .map_err(|e| {
                    let why = key_error_text(passphrase.is_some(), &e);
                    Outcome::Fatal(format!("the key {} cannot be used: {why}", path.display()))
                })?;
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

/// What the pane says when a key file cannot be used, in words that tell the person what to do.
///
/// The text is decided by the error and by whether a passphrase was given, never by a guess about the key's cipher: a
/// wrong passphrase of an OpenSSH key and a damaged file both fail as a decoding error, so the sentence says both.
/// The function sees no secret (the error and one flag), so no passphrase, password or key byte can be part of the text.
fn key_error_text(passphrase_given: bool, error: &russh::keys::Error) -> String {
    use russh::keys::Error as KeyError;
    match error {
        KeyError::KeyIsEncrypted if passphrase_given => "the passphrase was not accepted for this key".into(),
        KeyError::KeyIsEncrypted => "the key is protected by a passphrase; save the passphrase in the connection".into(),
        KeyError::IO(e) => format!("the key file cannot be read: {e}"),
        KeyError::UnsupportedKeyType { .. } | KeyError::UnknownAlgorithm(_) => "the key type is not supported".into(),
        // A PKCS#8 file whose encryption the library cannot read (PBKDF2 with HMAC-SHA-1, as LibreSSL and OpenSSL 1 write
        // it) is not a wrong passphrase: saying so would send the person to retype one that is right.
        KeyError::Pkcs8(e) if e.to_string().ends_with("is unsupported") => {
            "the key is encrypted in a way that is not supported; save it again in the OpenSSH format".into()
        }
        KeyError::SshKey(_)
        | KeyError::SshEncoding(_)
        | KeyError::Pkcs8(_)
        | KeyError::Pkcs1(_)
        | KeyError::Der(_)
        | KeyError::Spki(_)
        | KeyError::Sec1(_)
        | KeyError::Pad(_)
        | KeyError::Unpad(_)
        | KeyError::Rsa(_)
        | KeyError::Decode(_)
        | KeyError::Ed25519KeyError(_)
        | KeyError::EcdsaKeyError(_)
        | KeyError::KeyIsCorrupt
        | KeyError::CouldNotReadKey => {
            if passphrase_given {
                "the passphrase is wrong or the key file is damaged".into()
            } else {
                "the key file is damaged".into()
            }
        }
        other => other.to_string(),
    }
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

// ---- exec: one command on a saved connection, without a terminal -------------------------------------------------

/// The longest command [`SshExec::exec`] accepts, in bytes.
pub const MAX_EXEC_COMMAND: usize = 16 * 1024;
/// The output kept per stream when the caller does not say.
pub const DEFAULT_EXEC_OUTPUT: usize = 64 * 1024;
/// The most output kept per stream, whatever the caller asks for.
pub const MAX_EXEC_OUTPUT: usize = 1024 * 1024;
/// How long a command may run when the caller does not say.
pub const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest a command may run, whatever the caller asks for.
pub const MAX_EXEC_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Open the channel and write the request.
const EXEC_OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// The shortest time limit; a smaller one is raised to it, so a zero cannot end every command before it starts.
const MIN_EXEC_TIMEOUT: Duration = Duration::from_millis(10);

/// What a caller of [`SshExec::exec`] bounds a command with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecLimits {
    /// The most bytes kept of each of stdout and stderr: the last ones. At most [`MAX_EXEC_OUTPUT`].
    pub max_output: usize,
    /// The wall-clock time the command may run, from the moment the request was written. At most [`MAX_EXEC_TIMEOUT`].
    pub timeout: Duration,
}

impl Default for ExecLimits {
    fn default() -> Self {
        Self { max_output: DEFAULT_EXEC_OUTPUT, timeout: DEFAULT_EXEC_TIMEOUT }
    }
}

impl ExecLimits {
    /// The limits brought into the allowed range: `max_output` from 1 to [`MAX_EXEC_OUTPUT`], `timeout` from 10 ms to
    /// [`MAX_EXEC_TIMEOUT`]. [`SshExec::exec`] applies it to whatever the caller passes, so every caller is bounded.
    pub fn clamped(self) -> Self {
        Self {
            max_output: self.max_output.clamp(1, MAX_EXEC_OUTPUT),
            timeout: self.timeout.clamp(MIN_EXEC_TIMEOUT, MAX_EXEC_TIMEOUT),
        }
    }
}

/// A switch that stops a running [`SshExec::exec`]. One per run: the caller keeps a clone and calls
/// [`ExecCancel::cancel`] from any thread; the run ends promptly with [`ExecEnd::Cancelled`] and keeps what it read.
#[derive(Clone, Debug)]
pub struct ExecCancel(Arc<watch::Sender<bool>>);

impl Default for ExecCancel {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecCancel {
    /// A switch that is not raised.
    pub fn new() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }

    /// Stop the run that holds this switch. Idempotent, and callable from any thread.
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    /// Whether [`ExecCancel::cancel`] was called.
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    /// Completes once [`ExecCancel::cancel`] was called, and otherwise never.
    async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        // The sender lives in `self`, so the wait cannot fail while this future exists.
        if rx.wait_for(|cancelled| *cancelled).await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// How a command ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecEnd {
    /// The server closed the channel: the command ended (or closed its output, see [`ExecResult::status`]).
    Finished,
    /// The time limit passed. The client sent signal KILL and closed the channel; the command may still run.
    TimedOut,
    /// The caller cancelled. The same stop as for a time-out.
    Cancelled,
    /// The connection broke while the command ran, so whether it finished is unknown. This code never runs the
    /// command again: the caller treats the result as an error with partial output.
    Lost,
}

/// What a command printed and how it ended.
///
/// The output fields are bytes, not text: a cut can split a UTF-8 sequence, so callers decode lossily. They are raw and
/// not sanitised: the consumer redacts secrets and removes escape sequences before it shows or sends them.
#[derive(Clone, PartialEq, Eq)]
pub struct ExecResult {
    /// The last [`ExecLimits::max_output`] bytes of standard output.
    pub stdout: Vec<u8>,
    /// The last [`ExecLimits::max_output`] bytes of standard error (extended data of type 1).
    pub stderr: Vec<u8>,
    /// The remote exit code; 128 plus the signal number for a process ended by a signal; `None` when the command did
    /// not end by itself (time-out, cancel, lost connection) or the server sent no status.
    pub status: Option<u32>,
    pub end: ExecEnd,
    /// Whether any output was dropped to stay inside the limit.
    pub truncated: bool,
    /// The number of bytes dropped, both streams together.
    pub dropped: u64,
}

impl std::fmt::Debug for ExecResult {
    // The output may hold a secret; only its size is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecResult")
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .field("status", &self.status)
            .field("end", &self.end)
            .field("truncated", &self.truncated)
            .field("dropped", &self.dropped)
            .finish()
    }
}

/// The last `cap` bytes of everything pushed, and the count of the bytes that fell off the front. A deque,
/// so a chunk is appended and the excess is taken off the front without moving what stays.
struct Tail {
    cap: usize,
    bytes: VecDeque<u8>,
    dropped: u64,
}

impl Tail {
    fn new(cap: usize) -> Self {
        Self { cap, bytes: VecDeque::new(), dropped: 0 }
    }

    fn push(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        if chunk.len() >= self.cap {
            // The chunk alone fills the buffer: everything before it, and the front of the chunk, is dropped.
            let lost = self.bytes.len() + (chunk.len() - self.cap);
            self.dropped = self.dropped.saturating_add(lost as u64);
            self.bytes.clear();
            self.bytes.extend(&chunk[chunk.len() - self.cap..]);
            return;
        }
        let excess = (self.bytes.len() + chunk.len()).saturating_sub(self.cap);
        if excess > 0 {
            self.bytes.drain(..excess);
            self.dropped = self.dropped.saturating_add(excess as u64);
        }
        self.bytes.extend(chunk);
    }

    fn dropped(&self) -> u64 {
        self.dropped
    }

    fn into_vec(self) -> Vec<u8> {
        Vec::from(self.bytes)
    }
}

/// A copy of `target` in which this host and every jump host checks host keys strictly. Refuses a host without a known
/// hosts file and a host set to accept any key.
///
/// Why: [`Client::check_server_key`] learns an unknown key under [`HostKeyPolicy::AcceptNew`] and accepts every key when
/// there is no file. For a shell the user types into that is trust on first use; an action an agent takes must never be
/// the first contact with a host. A host can therefore be used by exec only after the user's own pane or file
/// browser learned its key.
fn require_pinned_hosts(target: &SshTarget) -> Result<SshTarget> {
    pin_hosts(target).map_err(exec_error)
}

/// [`require_pinned_hosts`] with the reason as plain text, for the pool's [`crate::pool::Open::Pinned`] opener.
pub(crate) fn pin_hosts(target: &SshTarget) -> std::result::Result<SshTarget, String> {
    fn pin(target: &mut SshTarget) -> std::result::Result<(), String> {
        if target.known_hosts.is_none() {
            return Err(format!("{} has no known_hosts file, so its host key cannot be checked", target.host));
        }
        if target.host_key == HostKeyPolicy::AcceptAny {
            return Err(format!("{} is set to accept any host key", target.host));
        }
        target.host_key = HostKeyPolicy::Strict;
        match target.jump.as_deref_mut() {
            Some(jump) => pin(jump),
            None => Ok(()),
        }
    }
    let mut pinned = target.clone();
    pin(&mut pinned)?;
    Ok(pinned)
}

/// A refusal or failure of an exec: a fixed phrase or the reason the login gave, never the command or a credential.
fn exec_error(text: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("ssh exec: {text}"))
}

/// Refuse a command that cannot be run, before anything is connected. The texts never include the command: it may hold a
/// secret. Newlines are allowed (a script is a legitimate command); the approval layer owns what may run.
fn check_command(command: &str) -> Result<()> {
    if command.trim().is_empty() {
        return Err(exec_error("the command is empty"));
    }
    if command.contains('\0') {
        return Err(exec_error("the command contains a NUL byte"));
    }
    if command.len() > MAX_EXEC_COMMAND {
        return Err(exec_error(format!("the command is longer than {MAX_EXEC_COMMAND} bytes")));
    }
    Ok(())
}

/// The POSIX number of a signal the SSH protocol names, for the exit status `128 + n` of a shell. A name this code does
/// not know counts as 0.
fn signal_number(signal: &Sig) -> u32 {
    match signal {
        Sig::HUP => 1,
        Sig::INT => 2,
        Sig::QUIT => 3,
        Sig::ILL => 4,
        Sig::ABRT => 6,
        Sig::FPE => 8,
        Sig::KILL => 9,
        Sig::USR1 => 10,
        Sig::SEGV => 11,
        Sig::PIPE => 13,
        Sig::ALRM => 14,
        Sig::TERM => 15,
        Sig::Custom(_) => 0,
    }
}

/// Stop a command: signal KILL, then close the channel. Errors are ignored and the whole stop is bounded, so a dead
/// connection cannot hold the caller.
///
/// A server may ignore the signal and a shell may leave child processes running; the channel is closed either way. A
/// caller who needs a hard stop wraps the command with `timeout` itself.
async fn stop(channel: &Channel<client::Msg>) {
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        let _ = channel.signal(Sig::KILL).await;
        let _ = channel.close().await;
    })
    .await;
}

/// Run commands on the host of a saved connection, one channel per command, without a terminal.
///
/// The commands run on the session of a [`Connection`] (see [`crate::pool`]), so a host that a file browser or a monitor
/// already logged in to costs no second login. The session is opened as [`Open::Pinned`]: jump hosts, authentication and
/// the host key check are `open`'s, and a key is never learned (see [`require_pinned_hosts`]); a session that a person's
/// own open made serves the commands too. Output and time are bounded, and the result says how a command ended. The
/// connection is kept for the next call.
pub struct SshExec {
    conn: Arc<Connection>,
}

impl SshExec {
    /// Prepare an exec primitive for `target` on a connection of its own. Opens no socket: nothing connects before the
    /// first call of [`SshExec::exec`].
    ///
    /// Fails for a host (or jump host) without a known hosts file and for one set to accept any key: the host key of an
    /// exec connection is always checked and never learned.
    pub fn new(label: impl Into<String>, target: SshTarget) -> Result<Self> {
        require_pinned_hosts(&target)?;
        Self::on(Connection::standalone(label, target)?)
    }

    /// Prepare an exec primitive on a connection that others share (the one of a saved profile).
    ///
    /// Fails at once, as [`SshExec::new`] does, for a target that cannot be pinned.
    pub fn on(conn: Arc<Connection>) -> Result<Self> {
        require_pinned_hosts(conn.target())?;
        Ok(Self { conn })
    }

    /// The name the caller gave, for its own messages.
    pub fn label(&self) -> &str {
        self.conn.label()
    }

    /// Run `command` on the host and return its output, its exit status and how it ended.
    ///
    /// This blocks: never call it from an async task or from the UI thread. `reconnect` and `startup` of the target are
    /// ignored. The command runs in the account's login shell exactly as given: nothing is quoted or interpreted here,
    /// and a caller that builds a command from data quotes it for a POSIX shell itself. The run is bounded by
    /// `limits.timeout`, measured from the moment the request was written; connecting and opening the channel have
    /// their own bounds.
    ///
    /// A command is written at most once. [`ExecEnd::Lost`] means the connection broke while it ran: it may or may not
    /// have finished, and this code never runs it again.
    pub fn exec(&self, command: &str, limits: &ExecLimits, cancel: &ExecCancel) -> Result<ExecResult> {
        check_command(command)?;
        let limits = limits.clamped();
        // The connection counts this run as a use, so the idle reaper never closes it under the command.
        let _lease = self.conn.lease();
        self.conn.block_on(self.run(command, &limits, cancel))
    }

    /// The whole run of one command.
    ///
    /// A command is written at most once. A cached session that cannot open a channel, or cannot take the
    /// request, is replaced once; the same failure on a session this call just made, a refused request, and every
    /// failure after the request was written are final. A connection lost while the command runs gives
    /// [`ExecEnd::Lost`], and nothing is ever tried again after that.
    async fn run(&self, command: &str, limits: &ExecLimits, cancel: &ExecCancel) -> Result<ExecResult> {
        let nothing = |end| ExecResult { stdout: Vec::new(), stderr: Vec::new(), status: None, end, truncated: false, dropped: 0 };
        let mut replaced = false;
        loop {
            // Connect and open the channel. A cancel here ends the run before anything is written; the login itself
            // belongs to the connection and finishes for whoever asks next.
            let opening = async {
                let session = self.conn.session(Open::Pinned).await.map_err(exec_error)?;
                let fresh = session.first_use();
                let channel = match tokio::time::timeout(EXEC_OPEN_TIMEOUT, session.handle().channel_open_session()).await {
                    Ok(Ok(channel)) => Ok(channel),
                    Ok(Err(e)) => Err(exec_error(e)),
                    Err(_) => Err(exec_error("opening a channel timed out")),
                };
                Ok::<_, Error>((session, fresh, channel))
            };
            let (session, fresh, channel) = tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(nothing(ExecEnd::Cancelled)),
                opening = opening => opening?,
            };
            // Nothing but the command is requested: no terminal, no environment, no agent forwarding, no X11.
            // The end of input follows at once, so a command that reads it does not wait for the time-out.
            let why = match channel {
                Ok(channel) => match tokio::time::timeout(EXEC_OPEN_TIMEOUT, channel.exec(true, command)).await {
                    Ok(Ok(())) => {
                        let _ = channel.eof().await;
                        // From here the command is written: no failure below is tried again.
                        let result = pump(channel, limits, cancel).await?;
                        if result.end == ExecEnd::Lost {
                            self.conn.invalidate(&session).await;
                        }
                        return Ok(result);
                    }
                    Ok(Err(e)) => exec_error(e),
                    Err(_) => exec_error("writing the command timed out"),
                },
                Err(why) => why,
            };
            // The request was not written. A session this call just made is not tried again, and a cached one is
            // replaced once.
            if fresh || replaced {
                return Err(why);
            }
            replaced = true;
            self.conn.invalidate(&session).await;
        }
    }
}

/// Read a command's channel until it ends, the time runs out or the caller cancels.
async fn pump(mut channel: Channel<client::Msg>, limits: &ExecLimits, cancel: &ExecCancel) -> Result<ExecResult> {
    let mut stdout = Tail::new(limits.max_output);
    let mut stderr = Tail::new(limits.max_output);
    let mut status: Option<u32> = None;
    // An end of output or a close was seen; only a close, the deadline or a cancel ends the wait, because an exit status
    // can still follow the end of output and a process that closed its output may still run.
    let mut output_ended = false;
    let cancelled = cancel.cancelled();
    tokio::pin!(cancelled);
    let deadline = tokio::time::sleep(limits.timeout);
    tokio::pin!(deadline);
    let end = loop {
        tokio::select! {
            biased;
            () = &mut cancelled => {
                stop(&channel).await;
                break ExecEnd::Cancelled;
            }
            () = &mut deadline => {
                stop(&channel).await;
                break ExecEnd::TimedOut;
            }
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Success) => {}
                Some(ChannelMsg::Failure) => {
                    let _ = channel.close().await;
                    return Err(exec_error("the server refused to run the command"));
                }
                Some(ChannelMsg::Data { data }) => stdout.push(&data),
                Some(ChannelMsg::ExtendedData { data, ext }) => {
                    if ext == 1 {
                        stderr.push(&data);
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => status = Some(exit_status),
                Some(ChannelMsg::ExitSignal { signal_name, .. }) => status = Some(128 + signal_number(&signal_name)),
                Some(ChannelMsg::Eof) => output_ended = true,
                Some(ChannelMsg::Close) => break ExecEnd::Finished,
                Some(_) => {}
                None if output_ended => break ExecEnd::Finished,
                None => break ExecEnd::Lost,
            },
        }
    };
    let dropped = stdout.dropped().saturating_add(stderr.dropped());
    Ok(ExecResult { stdout: stdout.into_vec(), stderr: stderr.into_vec(), status, end, truncated: dropped > 0, dropped })
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

    /// The error of really decoding `text` as a key file, for the variants a hand cannot build.
    fn decode_error(text: &str, passphrase: Option<&str>) -> russh::keys::Error {
        russh::keys::decode_secret_key(text, passphrase).expect_err("the text is not a usable key")
    }

    #[test]
    fn key_errors_are_worded_by_one_function() {
        use russh::keys::Error as KeyError;
        use russh::keys::ssh_key;
        const PASSPHRASE: &str = "correct horse battery staple";
        const PROTECTED: &str = "the key is protected by a passphrase; save the passphrase in the connection";
        const NOT_ACCEPTED: &str = "the passphrase was not accepted for this key";
        const WRONG_OR_DAMAGED: &str = "the passphrase is wrong or the key file is damaged";
        const DAMAGED: &str = "the key file is damaged";
        const UNSUPPORTED: &str = "the key type is not supported";
        const UNREADABLE: &str = "the key is encrypted in a way that is not supported; save it again in the OpenSSH format";
        // What the old text for a wrong passphrase of an OpenSSH key was; it must never come back.
        assert_eq!(KeyError::SshKey(ssh_key::Error::Crypto).to_string(), "SshKey: cryptographic error");

        let not_found = || KeyError::IO(io::Error::from(io::ErrorKind::NotFound));
        let unsupported = || KeyError::UnsupportedKeyType { key_type_string: "dsa".into(), key_type_raw: vec![1, 2] };
        // Variants that only a real decode can produce: garbage in a PKCS#8 block, an unknown block, a bad base64 text.
        let pkcs8 = decode_error("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n", None);
        let sealed = decode_error("-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n", Some(PASSPHRASE));
        let no_block = decode_error("this is not a key", None);
        let rsa = decode_error("-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----\n", None);
        // An encrypted PKCS#8 file whose key derivation the library does not read (PBKDF2 with HMAC-SHA-1, which LibreSSL
        // and OpenSSL 1 write). The bytes are a bare algorithm description and a short filler, not key material.
        let old_pbkdf2 = decode_error(
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\n\
             MF0wSQYJKoZIhvcNAQUNMDwwGwYJKoZIhvcNAQUMMA4ECAABAgMEBQYHAgIIADAdBglghkgBZQMEAQIEEAABAgMEBQYH\n\
             CAkKCwwNDg8EEAABAgMEBQYHCAkKCwwNDg8=\n\
             -----END ENCRYPTED PRIVATE KEY-----\n",
            Some(PASSPHRASE),
        );

        let table: Vec<(&str, bool, KeyError, String)> = vec![
            ("KeyIsEncrypted", false, KeyError::KeyIsEncrypted, PROTECTED.into()),
            ("KeyIsEncrypted", true, KeyError::KeyIsEncrypted, NOT_ACCEPTED.into()),
            ("KeyIsCorrupt", false, KeyError::KeyIsCorrupt, DAMAGED.into()),
            ("KeyIsCorrupt", true, KeyError::KeyIsCorrupt, WRONG_OR_DAMAGED.into()),
            ("CouldNotReadKey", false, KeyError::CouldNotReadKey, DAMAGED.into()),
            ("CouldNotReadKey", true, KeyError::CouldNotReadKey, WRONG_OR_DAMAGED.into()),
            ("SshKey(Crypto)", false, KeyError::SshKey(ssh_key::Error::Crypto), DAMAGED.into()),
            ("SshKey(Crypto)", true, KeyError::SshKey(ssh_key::Error::Crypto), WRONG_OR_DAMAGED.into()),
            ("UnsupportedKeyType", false, unsupported(), UNSUPPORTED.into()),
            ("UnsupportedKeyType", true, unsupported(), UNSUPPORTED.into()),
            ("a garbage PKCS#8 block", false, pkcs8, DAMAGED.into()),
            ("a garbage encrypted PKCS#8 block", true, sealed, WRONG_OR_DAMAGED.into()),
            ("a text with no key block", false, no_block, DAMAGED.into()),
            ("a garbage PKCS#1 block", true, rsa, WRONG_OR_DAMAGED.into()),
            ("an encrypted PKCS#8 key with an unsupported key derivation", true, old_pbkdf2, UNREADABLE.into()),
        ];
        for (name, given, error, sentence) in &table {
            let text = key_error_text(*given, error);
            assert_eq!(&text, sentence, "{name}, passphrase given: {given}");
            assert!(!text.contains("cryptographic error"), "{name}: {text}");
            assert!(!text.contains(PASSPHRASE), "{name}: {text}");
        }
        // A file that cannot be read keeps the reason of the system and says it is the file, with or without a passphrase.
        for given in [false, true] {
            let text = key_error_text(given, &not_found());
            assert!(text.starts_with("the key file cannot be read: "), "{text}");
            assert!(!text.contains(PASSPHRASE), "{text}");
        }
        // Anything that is none of the above is the error's own text.
        assert_eq!(key_error_text(true, &KeyError::NoHomeDir), KeyError::NoHomeDir.to_string());
    }

    #[test]
    fn a_secret_never_shows_in_debug_output() {
        let auth = SshAuth::Password("hunter2".into());
        assert!(!format!("{auth:?}").contains("hunter2"));
        let auth = SshAuth::Key { path: "/k".into(), passphrase: Some("hunter2".into()) };
        assert!(!format!("{auth:?}").contains("hunter2"));
    }

    // ---- the exec server and the exec tests ----------------------------------------------------------------------

    /// What the exec server saw, shared with the test that started it.
    #[derive(Default)]
    struct ExecSeen {
        /// Connections accepted so far; the index of a connection is the count before it.
        connections: AtomicUsize,
        /// `channel_close` and `channel_eof` calls: the client closed, or ended its input.
        closes: AtomicUsize,
        eofs: AtomicUsize,
        /// Requests an exec channel must never make.
        ptys: AtomicUsize,
        envs: AtomicUsize,
        agents: AtomicUsize,
        /// Every command text, in the order received.
        commands: Mutex<Vec<String>>,
        /// Every signal name the client sent (`KILL`).
        signals: Mutex<Vec<String>>,
    }

    impl ExecSeen {
        fn commands(&self) -> Vec<String> {
            self.commands.lock().unwrap().clone()
        }
    }

    /// Which session channels the exec server accepts.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ChannelPolicy {
        Open,
        /// The first channel of a connection is accepted and every later one is rejected: a cached connection that
        /// has gone bad without being closed.
        RejectAfterFirst,
        RejectAll,
    }

    /// The exec server: password `pw` for user `holz`, and a "shell" whose commands are the verbs of `exec_request`.
    #[derive(Clone)]
    struct ExecFake {
        seen: Arc<ExecSeen>,
        /// The index of this connection.
        index: usize,
        policy: ChannelPolicy,
        /// Channels opened on this connection so far.
        channels: usize,
        /// Channels of a `cat` command: it ends when the client's end of input arrives.
        cats: std::collections::HashSet<ChannelId>,
    }

    impl server::Server for ExecFake {
        type Handler = Self;
        fn new_client(&mut self, _: Option<SocketAddr>) -> Self {
            let mut handler = self.clone();
            handler.index = self.seen.connections.fetch_add(1, Ordering::SeqCst);
            handler
        }
    }

    /// End a command the way a shell does: the exit status, the end of output, then the close.
    fn end_command(session: &mut Session, channel: ChannelId, status: u32) -> std::result::Result<(), russh::Error> {
        session.exit_status_request(channel, status)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }

    impl server::Handler for ExecFake {
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
            self.channels += 1;
            let accept = match self.policy {
                ChannelPolicy::Open => true,
                ChannelPolicy::RejectAfterFirst => self.channels == 1,
                ChannelPolicy::RejectAll => false,
            };
            if accept {
                reply.accept().await;
            } else {
                // A dropped handle rejects the channel.
                drop(reply);
            }
            Ok(())
        }

        async fn pty_request(
            &mut self,
            _channel: ChannelId,
            _term: &str,
            _cols: u32,
            _rows: u32,
            _px_w: u32,
            _px_h: u32,
            _modes: &[(russh::Pty, u32)],
            _session: &mut Session,
        ) -> std::result::Result<(), Self::Error> {
            self.seen.ptys.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn env_request(
            &mut self,
            _channel: ChannelId,
            _name: &str,
            _value: &str,
            _session: &mut Session,
        ) -> std::result::Result<(), Self::Error> {
            self.seen.envs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn agent_request(&mut self, _channel: ChannelId, _session: &mut Session) -> std::result::Result<bool, Self::Error> {
            self.seen.agents.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        }

        async fn channel_eof(&mut self, channel: ChannelId, session: &mut Session) -> std::result::Result<(), Self::Error> {
            self.seen.eofs.fetch_add(1, Ordering::SeqCst);
            if self.cats.remove(&channel) {
                session.data(channel, b"eof\n".to_vec())?;
                end_command(session, channel, 0)?;
            }
            Ok(())
        }

        async fn channel_close(&mut self, _channel: ChannelId, _session: &mut Session) -> std::result::Result<(), Self::Error> {
            self.seen.closes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn signal(&mut self, _channel: ChannelId, signal: Sig, _session: &mut Session) -> std::result::Result<(), Self::Error> {
            self.seen.signals.lock().unwrap().push(format!("{signal:?}"));
            Ok(())
        }

        async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> std::result::Result<(), Self::Error> {
            let command = String::from_utf8_lossy(data).to_string();
            self.seen.commands.lock().unwrap().push(command.clone());
            let (verb, arg) = command.split_once(' ').unwrap_or((command.as_str(), ""));
            if verb == "refuse" {
                session.channel_failure(channel)?;
                return Ok(());
            }
            session.channel_success(channel)?;
            match verb {
                "exit" => end_command(session, channel, arg.parse().unwrap())?,
                // Ended by a signal: the exit-signal request, then the close, as a shell reports a killed child.
                "kill" => {
                    let signal = match arg {
                        "HUP" => Sig::HUP,
                        "KILL" => Sig::KILL,
                        "TERM" => Sig::TERM,
                        other => Sig::Custom(other.to_string()),
                    };
                    session.exit_signal_request(channel, signal, false, "", "")?;
                    session.close(channel)?;
                }
                // `n` bytes on stdout (byte i is i % 251) and on stderr (byte i is (i + 100) % 251), in 16 KiB chunks.
                "flood" => {
                    let total: usize = arg.parse().unwrap();
                    let mut sent = 0;
                    while sent < total {
                        let n = (total - sent).min(16 * 1024);
                        session.data(channel, (sent..sent + n).map(|i| (i % 251) as u8).collect::<Vec<u8>>())?;
                        session.extended_data(channel, 1, (sent..sent + n).map(|i| ((i + 100) % 251) as u8).collect::<Vec<u8>>())?;
                        sent += n;
                    }
                    end_command(session, channel, 0)?;
                }
                // `conn <index>`: which connection of the server ran the command.
                "conn" => {
                    session.data(channel, format!("conn {}\n", self.index).into_bytes())?;
                    end_command(session, channel, 0)?;
                }
                // The server drops the connection 50 ms after the command, without a reply.
                "cut" => {
                    let handle = session.handle();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        let _ = handle.disconnect(Disconnect::ByApplication, "cut".into(), "en".into()).await;
                    });
                }
                // A finished command, and then the server drops the idle connection.
                "bye" => {
                    session.data(channel, b"bye\n".to_vec())?;
                    end_command(session, channel, 0)?;
                    let handle = session.handle();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        let _ = handle.disconnect(Disconnect::ByApplication, "bye".into(), "en".into()).await;
                    });
                }
                // `nap <ms>`: a slow command, so two calls overlap.
                "nap" => {
                    let wait: u64 = arg.parse().unwrap();
                    let handle = session.handle();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(wait)).await;
                        let _ = handle.data(channel, b"napped\n".to_vec()).await;
                        let _ = handle.exit_status_request(channel, 0).await;
                        let _ = handle.eof(channel).await;
                        let _ = handle.close(channel).await;
                    });
                }
                // Prints a line and then never ends.
                "partial" => session.data(channel, format!("{arg}\n").into_bytes())?,
                // Closes without an exit status.
                "quiet" => session.close(channel)?,
                // The end of output comes first and the exit status 150 ms later.
                "late" => {
                    let status: u32 = arg.parse().unwrap();
                    session.data(channel, b"x".to_vec())?;
                    session.eof(channel)?;
                    let handle = session.handle();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        let _ = handle.exit_status_request(channel, status).await;
                        let _ = handle.close(channel).await;
                    });
                }
                "say" => {
                    session.data(channel, format!("{arg}\n").into_bytes())?;
                    end_command(session, channel, 0)?;
                }
                // Reads its input like `cat`: it ends only when the client's end of input arrives.
                "cat" => {
                    self.cats.insert(channel);
                }
                "mixed" => {
                    session.data(channel, b"out-1\n".to_vec())?;
                    session.extended_data(channel, 1, b"err-1\n".to_vec())?;
                    session.data(channel, b"out-2\n".to_vec())?;
                    end_command(session, channel, 3)?;
                }
                // Any other verb is a command the "shell" does not know.
                _ => {
                    session.extended_data(channel, 1, b"not found\n".to_vec())?;
                    end_command(session, channel, 127)?;
                }
            }
            Ok(())
        }
    }

    struct ExecServer {
        addr: SocketAddr,
        key: PrivateKey,
        seen: Arc<ExecSeen>,
    }

    /// Start the exec server on a thread of its own; it runs until the process ends.
    fn serve_exec() -> ExecServer {
        serve_exec_with(ChannelPolicy::Open)
    }

    fn serve_exec_with(policy: ChannelPolicy) -> ExecServer {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let config = Arc::new(server::Config {
            keys: vec![key.clone()],
            auth_rejection_time: Duration::from_millis(0),
            auth_rejection_time_initial: Some(Duration::from_millis(0)),
            ..Default::default()
        });
        let seen = Arc::new(ExecSeen::default());
        let mut fake = ExecFake { seen: seen.clone(), index: 0, policy, channels: 0, cats: Default::default() };
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let socket = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                tx.send(socket.local_addr().unwrap()).unwrap();
                let _ = fake.run_on_socket(config, &socket).await;
            });
        });
        ExecServer { addr: rx.recv().unwrap(), key, seen }
    }

    /// A directory of the test's own, removed when the test ends (also when it fails).
    struct Scratch(PathBuf);

    impl Scratch {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch_dir(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("holzwave-exec-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// A saved connection to the exec server the way `profiles.rs` builds one: a known hosts file, trust on first use,
    /// and the server's key already learned (as if the user's own pane had connected before).
    fn exec_target(server: &ExecServer, dir: &std::path::Path) -> SshTarget {
        let known = dir.join("known_hosts");
        let mut t = target(server.addr);
        t.known_hosts = Some(known.clone());
        t.host_key = HostKeyPolicy::AcceptNew;
        learn_host_key("127.0.0.1", server.addr.port(), server.key.public_key(), &known).unwrap();
        t
    }

    /// The same target with an existing empty known hosts file: the server's key is unknown.
    fn exec_target_untrusted(server: &ExecServer, dir: &std::path::Path) -> SshTarget {
        let known = dir.join("known_hosts");
        std::fs::write(&known, "").unwrap();
        let mut t = target(server.addr);
        t.known_hosts = Some(known);
        t.host_key = HostKeyPolicy::AcceptNew;
        t
    }

    fn run_default(exec: &SshExec, command: &str) -> ExecResult {
        exec.exec(command, &ExecLimits::default(), &ExecCancel::new()).unwrap()
    }

    /// The error of a target `SshExec::new` refuses (`SshExec` has no `Debug`, so `unwrap_err` cannot be used).
    fn refusal(target: SshTarget) -> String {
        match SshExec::new("Test host", target) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("the target was accepted"),
        }
    }

    /// Poll `condition` until it holds, or fail after five seconds.
    fn wait_until(what: &str, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "{what} was never seen");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn exec_round_trip_carries_stdout_stderr_and_exit_status() {
        let server = serve_exec();
        let dir = scratch_dir("s5");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        assert_eq!(exec.label(), "Test host");
        let result = run_default(&exec, "mixed");
        assert_eq!(result.stdout, b"out-1\nout-2\n", "stdout: Data");
        assert_eq!(result.stderr, b"err-1\n", "stderr: ExtendedData with type 1");
        assert_eq!(result.status, Some(3), "the exit status");
        assert_eq!(result.end, ExecEnd::Finished);
        assert!(!result.truncated);
        assert_eq!(result.dropped, 0);
    }

    #[test]
    fn exec_asks_for_no_terminal_no_environment_and_no_agent_and_closes_stdin() {
        let server = serve_exec();
        let dir = scratch_dir("channel");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        let result = run_default(&exec, "say hello");
        assert_eq!(result.stdout, b"hello\n");
        assert!(result.stderr.is_empty());
        assert_eq!(result.status, Some(0));
        // A command that reads its input ends as soon as the client ends it, instead of waiting for the time-out.
        let limits = ExecLimits { max_output: DEFAULT_EXEC_OUTPUT, timeout: Duration::from_secs(10) };
        let started = Instant::now();
        let result = exec.exec("cat", &limits, &ExecCancel::new()).unwrap();
        assert_eq!(result.stdout, b"eof\n");
        assert_eq!(result.end, ExecEnd::Finished);
        assert!(started.elapsed() < Duration::from_secs(5), "the end of input was not sent at once");
        wait_until("the client's end of input", || server.seen.eofs.load(Ordering::SeqCst) >= 1);
        assert_eq!(server.seen.commands(), vec!["say hello".to_string(), "cat".to_string()], "each command is written once, as given");
        assert_eq!(server.seen.ptys.load(Ordering::SeqCst), 0, "no terminal");
        assert_eq!(server.seen.envs.load(Ordering::SeqCst), 0, "no environment");
        assert_eq!(server.seen.agents.load(Ordering::SeqCst), 0, "no agent forwarding");
    }

    #[test]
    fn exec_never_learns_a_host_key() {
        let server = serve_exec();
        let dir = scratch_dir("learn");
        let target = exec_target_untrusted(&server, dir.path());
        assert_eq!(target.host_key, HostKeyPolicy::AcceptNew, "the policy the caller gave would have learned the key");
        let known = target.known_hosts.clone().unwrap();
        let exec = SshExec::new("Test host", target).unwrap();
        let error = exec.exec("say hello", &ExecLimits::default(), &ExecCancel::new()).unwrap_err().to_string();
        assert!(error.contains("is not known"), "{error}");
        assert_eq!(std::fs::read_to_string(&known).unwrap(), "", "no key was written");
        assert!(server.seen.commands().is_empty(), "no command reached the server");
    }

    #[test]
    fn exec_refuses_a_changed_host_key() {
        let first = serve_exec();
        let second = serve_exec();
        let dir = scratch_dir("changed");
        let known = dir.path().join("known_hosts");
        // The first server's key is known under the second server's name.
        learn_host_key("127.0.0.1", second.addr.port(), first.key.public_key(), &known).unwrap();
        let mut t = target(second.addr);
        t.known_hosts = Some(known);
        t.host_key = HostKeyPolicy::AcceptNew;
        let exec = SshExec::new("Test host", t).unwrap();
        let error = exec.exec("say hello", &ExecLimits::default(), &ExecCancel::new()).unwrap_err().to_string();
        assert!(error.contains("CHANGED"), "{error}");
        assert!(second.seen.commands().is_empty());
    }

    #[test]
    fn exec_refuses_targets_whose_keys_it_cannot_check() {
        let mut no_file = SshTarget::new("alpha.example", "holz", SshAuth::Password("pw".into()));
        no_file.known_hosts = None;
        let error = refusal(no_file.clone());
        assert!(error.contains("alpha.example") && error.contains("known_hosts"), "{error}");

        let mut any = no_file.clone();
        any.known_hosts = Some("/nonexistent/known_hosts".into());
        any.host_key = HostKeyPolicy::AcceptAny;
        let error = refusal(any.clone());
        assert!(error.contains("alpha.example") && error.contains("any host key"), "{error}");

        // The same two checks apply to a jump host, and the error names it.
        let mut good = no_file.clone();
        good.known_hosts = Some("/nonexistent/known_hosts".into());
        let mut jump_no_file = SshTarget::new("bastion.example", "holz", SshAuth::Password("pw".into()));
        jump_no_file.known_hosts = None;
        let mut via = good.clone();
        via.jump = Some(Box::new(jump_no_file));
        let error = refusal(via);
        assert!(error.contains("bastion.example") && error.contains("known_hosts"), "{error}");
        let mut jump_any = SshTarget::new("bastion.example", "holz", SshAuth::Password("pw".into()));
        jump_any.known_hosts = Some("/nonexistent/known_hosts".into());
        jump_any.host_key = HostKeyPolicy::AcceptAny;
        let mut via = good;
        via.jump = Some(Box::new(jump_any));
        let error = refusal(via);
        assert!(error.contains("bastion.example") && error.contains("any host key"), "{error}");
    }

    #[test]
    fn exec_pins_every_host_of_a_jump_chain() {
        let known = std::path::PathBuf::from("/nonexistent/known_hosts");
        let mut inner = SshTarget::new("inner.example", "holz", SshAuth::Password("pw".into()));
        inner.known_hosts = Some(known.clone());
        let mut middle = SshTarget::new("middle.example", "holz", SshAuth::Password("pw".into()));
        middle.known_hosts = Some(known.clone());
        middle.jump = Some(Box::new(inner));
        let mut outer = SshTarget::new("outer.example", "holz", SshAuth::Password("pw".into()));
        outer.known_hosts = Some(known);
        outer.jump = Some(Box::new(middle));

        let pinned = require_pinned_hosts(&outer).unwrap();
        assert_eq!(pinned.host_key, HostKeyPolicy::Strict);
        let middle = pinned.jump.as_deref().unwrap();
        assert_eq!(middle.host_key, HostKeyPolicy::Strict);
        assert_eq!(middle.jump.as_deref().unwrap().host_key, HostKeyPolicy::Strict);
        // The caller's target is untouched: the pinned one is a copy.
        assert_eq!(outer.host_key, HostKeyPolicy::AcceptNew);
        assert_eq!(outer.jump.as_deref().unwrap().host_key, HostKeyPolicy::AcceptNew);
    }

    #[test]
    fn exec_tail_keeps_the_last_bytes_and_counts_the_rest() {
        // 3 then 4 bytes at cap 5: the last 5 of the 7 remain.
        let mut tail = Tail::new(5);
        tail.push(b"abc");
        tail.push(b"defg");
        assert_eq!(tail.dropped(), 2);
        assert_eq!(tail.into_vec(), b"cdefg");

        // One push longer than the cap keeps its last `cap` bytes.
        let mut tail = Tail::new(4);
        tail.push(b"xy");
        tail.push(b"0123456789");
        assert_eq!(tail.dropped(), 8);
        assert_eq!(tail.into_vec(), b"6789");

        // A cap of 1 keeps the last byte.
        let mut tail = Tail::new(1);
        tail.push(b"ab");
        tail.push(b"c");
        assert_eq!(tail.dropped(), 2);
        assert_eq!(tail.into_vec(), b"c");

        // An empty push changes nothing.
        let mut tail = Tail::new(3);
        tail.push(b"ab");
        tail.push(b"");
        assert_eq!(tail.dropped(), 0);
        assert_eq!(tail.into_vec(), b"ab");

        // Order survives the wrap of the ring: many small pushes against a small cap.
        let mut tail = Tail::new(7);
        let all: Vec<u8> = (0..100u8).collect();
        for chunk in all.chunks(3) {
            tail.push(chunk);
        }
        assert_eq!(tail.dropped(), 93);
        assert_eq!(tail.into_vec(), all[93..].to_vec());
    }

    #[test]
    fn exec_result_debug_hides_the_output() {
        let result = ExecResult {
            stdout: b"secret-marker".to_vec(),
            stderr: b"err".to_vec(),
            status: Some(0),
            end: ExecEnd::Finished,
            truncated: false,
            dropped: 0,
        };
        let shown = format!("{result:?}");
        assert!(!shown.contains("secret-marker"), "{shown}");
        assert!(shown.contains("13"), "the byte count of stdout is shown: {shown}");
    }

    #[test]
    fn exec_limits_are_clamped() {
        let low = ExecLimits { max_output: 0, timeout: Duration::ZERO }.clamped();
        assert_eq!(low, ExecLimits { max_output: 1, timeout: MIN_EXEC_TIMEOUT });
        let high = ExecLimits { max_output: usize::MAX, timeout: Duration::MAX }.clamped();
        assert_eq!(high, ExecLimits { max_output: MAX_EXEC_OUTPUT, timeout: MAX_EXEC_TIMEOUT });
        assert_eq!(MAX_EXEC_OUTPUT, 1024 * 1024);
        assert_eq!(MAX_EXEC_TIMEOUT, Duration::from_secs(600));
        let inside = ExecLimits { max_output: 4096, timeout: Duration::from_secs(5) };
        assert_eq!(inside.clamped(), inside);
        assert_eq!(ExecLimits::default().clamped(), ExecLimits::default());

        // `exec` applies it to what the caller passes: a cap of 0 keeps one byte.
        let server = serve_exec();
        let dir = scratch_dir("clamp");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        let limits = ExecLimits { max_output: 0, timeout: Duration::MAX };
        let result = exec.exec("say hello", &limits, &ExecCancel::new()).unwrap();
        assert_eq!(result.stdout, b"\n");
        assert_eq!(result.dropped, 5);
        assert!(result.truncated);
        assert_eq!(result.end, ExecEnd::Finished);
    }

    #[test]
    fn exec_checks_the_command_before_it_connects() {
        let server = serve_exec();
        let dir = scratch_dir("check");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        let too_long = "a".repeat(MAX_EXEC_COMMAND + 1);
        for (command, needle) in
            [("", "empty"), ("   \t\n ", "empty"), ("say a\0b", "NUL"), (too_long.as_str(), "longer")]
        {
            let error = exec.exec(command, &ExecLimits::default(), &ExecCancel::new()).unwrap_err().to_string();
            assert!(error.contains(needle), "{needle}: {error}");
            assert!(command.trim().is_empty() || !error.contains(command), "the error repeats the command: {error}");
        }
        assert_eq!(server.seen.connections.load(Ordering::SeqCst), 0, "nothing was connected for a bad command");
        // The longest command passes the check, and a script with newlines does too.
        check_command(&"a".repeat(MAX_EXEC_COMMAND)).unwrap();
        check_command("set -e\necho one\necho two").unwrap();
    }

    #[test]
    fn exec_errors_never_contain_the_command_or_the_password() {
        let server = serve_exec();
        let dir = scratch_dir("secret");
        let exec = SshExec::new("Test host", exec_target_untrusted(&server, dir.path())).unwrap();
        let error = exec
            .exec("echo SECRET-MARKER-123", &ExecLimits::default(), &ExecCancel::new())
            .unwrap_err()
            .to_string();
        assert!(error.contains("is not known"), "the host key is what was refused: {error}");
        assert!(!error.contains("SECRET-MARKER-123"), "{error}");
        assert!(!error.contains("pw"), "{error}");
    }

    #[test]
    fn exec_flood_is_cut_to_the_tail_per_stream_and_counted() {
        let server = serve_exec();
        let dir = scratch_dir("flood");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        // 2 097 229 bytes per stream is a little more than the 2 MiB window of the connection.
        let total = 2_097_229usize;
        let cap = 65_536usize;
        let limits = ExecLimits { max_output: cap, timeout: Duration::from_secs(60) };
        let result = exec.exec(&format!("flood {total}"), &limits, &ExecCancel::new()).unwrap();
        let expected_out: Vec<u8> = (total - cap..total).map(|i| (i % 251) as u8).collect();
        let expected_err: Vec<u8> = (total - cap..total).map(|i| ((i + 100) % 251) as u8).collect();
        assert!(result.stdout == expected_out, "stdout is not the last {cap} bytes of the sequence");
        assert!(result.stderr == expected_err, "stderr is not the last {cap} bytes of its sequence");
        assert_eq!(result.dropped, 2 * (total - cap) as u64);
        assert!(result.truncated);
        assert_eq!(result.status, Some(0));
        assert_eq!(result.end, ExecEnd::Finished);
    }

    #[test]
    fn exec_a_command_that_never_ends_stops_at_the_timeout_and_keeps_its_output() {
        let server = serve_exec();
        let dir = scratch_dir("timeout");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        let limits = ExecLimits { max_output: DEFAULT_EXEC_OUTPUT, timeout: Duration::from_millis(300) };
        let started = Instant::now();
        let result = exec.exec("partial hello", &limits, &ExecCancel::new()).unwrap();
        assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
        assert_eq!(result.end, ExecEnd::TimedOut);
        assert_eq!(result.stdout, b"hello\n");
        assert_eq!(result.status, None);
        wait_until("signal KILL", || server.seen.signals.lock().unwrap().iter().any(|s| s == "KILL"));
        wait_until("the client's close", || server.seen.closes.load(Ordering::SeqCst) >= 1);
        // The connection outlives the closed channel.
        let again = run_default(&exec, "say again");
        assert_eq!(again.stdout, b"again\n");
        assert_eq!(server.seen.connections.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn exec_cancel_stops_a_running_command_and_keeps_its_output() {
        let server = serve_exec();
        let dir = scratch_dir("cancel");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        let cancel = ExecCancel::new();
        let raised = Arc::new(Mutex::new(None::<Instant>));
        let helper = {
            let (cancel, seen, raised) = (cancel.clone(), server.seen.clone(), raised.clone());
            thread::spawn(move || {
                wait_until("the command", || seen.commands().iter().any(|c| c == "partial hello"));
                // Let the first line cross the loopback before the cancel.
                thread::sleep(Duration::from_millis(300));
                *raised.lock().unwrap() = Some(Instant::now());
                cancel.cancel();
            })
        };
        let limits = ExecLimits { max_output: DEFAULT_EXEC_OUTPUT, timeout: Duration::from_secs(60) };
        let result = exec.exec("partial hello", &limits, &cancel).unwrap();
        let returned = Instant::now();
        helper.join().unwrap();
        let raised = raised.lock().unwrap().unwrap();
        assert!(returned.duration_since(raised) < Duration::from_secs(1), "took {:?}", returned.duration_since(raised));
        assert!(cancel.is_cancelled());
        assert_eq!(result.end, ExecEnd::Cancelled);
        assert_eq!(result.stdout, b"hello\n");
        assert_eq!(result.status, None);
        wait_until("signal KILL", || server.seen.signals.lock().unwrap().iter().any(|s| s == "KILL"));
    }

    #[test]
    fn exec_cancelled_before_it_starts_sends_nothing() {
        let server = serve_exec();
        let dir = scratch_dir("precancel");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        let cancel = ExecCancel::new();
        cancel.cancel();
        cancel.cancel();
        let result = exec.exec("say hello", &ExecLimits::default(), &cancel).unwrap();
        assert_eq!(result.end, ExecEnd::Cancelled);
        assert!(result.stdout.is_empty() && result.stderr.is_empty());
        assert_eq!(result.status, None);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(server.seen.connections.load(Ordering::SeqCst), 0, "nothing was connected");
        assert!(server.seen.commands().is_empty());
    }

    #[test]
    fn exec_a_signal_exit_is_128_plus_the_signal_number() {
        let server = serve_exec();
        let dir = scratch_dir("signals");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        for (command, status) in [("kill TERM", 143), ("kill KILL", 137), ("kill HUP", 129), ("kill XYZ", 128), ("exit 7", 7)] {
            let result = run_default(&exec, command);
            assert_eq!(result.status, Some(status), "{command}");
            assert_eq!(result.end, ExecEnd::Finished, "{command}");
        }
        assert_eq!(signal_number(&Sig::INT), 2);
        assert_eq!(signal_number(&Sig::QUIT), 3);
        assert_eq!(signal_number(&Sig::ABRT), 6);
        assert_eq!(signal_number(&Sig::SEGV), 11);
        assert_eq!(signal_number(&Sig::PIPE), 13);
        assert_eq!(signal_number(&Sig::ALRM), 14);
    }

    #[test]
    fn exec_reads_the_exit_status_that_follows_the_end_of_output() {
        let server = serve_exec();
        let dir = scratch_dir("late");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        let result = run_default(&exec, "late 5");
        assert_eq!(result.stdout, b"x");
        assert_eq!(result.status, Some(5));
        assert_eq!(result.end, ExecEnd::Finished);
    }

    #[test]
    fn exec_a_close_without_a_status_gives_none() {
        let server = serve_exec();
        let dir = scratch_dir("quiet");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        let result = run_default(&exec, "quiet");
        assert_eq!(result.status, None);
        assert_eq!(result.end, ExecEnd::Finished);
    }

    #[test]
    fn exec_a_refused_request_is_an_error() {
        let server = serve_exec();
        let dir = scratch_dir("refuse");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        let error = exec.exec("refuse", &ExecLimits::default(), &ExecCancel::new()).unwrap_err().to_string();
        assert!(error.contains("refused"), "{error}");
    }

    #[test]
    fn exec_replaces_a_cached_connection_once_when_no_channel_opens_on_it() {
        let server = serve_exec_with(ChannelPolicy::RejectAfterFirst);
        let dir = scratch_dir("replace");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        assert_eq!(run_default(&exec, "conn").stdout, b"conn 0\n");
        // The cached connection refuses the next channel; it is replaced, and the command runs on the new one.
        assert_eq!(run_default(&exec, "conn").stdout, b"conn 1\n");
        assert_eq!(server.seen.connections.load(Ordering::SeqCst), 2);
        assert_eq!(server.seen.commands(), vec!["conn".to_string(), "conn".to_string()], "once per connection");
    }

    #[test]
    fn exec_does_not_retry_on_a_connection_it_just_made() {
        let server = serve_exec_with(ChannelPolicy::RejectAll);
        let dir = scratch_dir("noretry");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        let error = exec.exec("conn", &ExecLimits::default(), &ExecCancel::new()).unwrap_err().to_string();
        assert!(error.starts_with("unsupported: ssh exec: "), "{error}");
        assert_eq!(server.seen.connections.load(Ordering::SeqCst), 1);
        assert!(server.seen.commands().is_empty());
    }

    #[test]
    fn exec_never_runs_a_command_twice_when_the_connection_is_lost_while_it_runs() {
        let server = serve_exec();
        let dir = scratch_dir("lost");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        let result = run_default(&exec, "cut");
        assert_eq!(result.end, ExecEnd::Lost);
        assert!(result.stdout.is_empty());
        assert_eq!(result.status, None);
        assert_eq!(server.seen.connections.load(Ordering::SeqCst), 1);
        assert_eq!(server.seen.commands().iter().filter(|c| *c == "cut").count(), 1, "the command was written once");
        // The lost connection is forgotten: the next command runs on a new one, and `cut` is still not repeated.
        assert_eq!(run_default(&exec, "conn").stdout, b"conn 1\n");
        assert_eq!(server.seen.connections.load(Ordering::SeqCst), 2);
        assert_eq!(server.seen.commands().iter().filter(|c| *c == "cut").count(), 1);
    }

    #[test]
    fn exec_replaces_a_connection_the_server_closed_while_idle() {
        let server = serve_exec();
        let dir = scratch_dir("idle");
        let exec = SshExec::new("Test host", exec_target(&server, dir.path())).unwrap();
        let result = run_default(&exec, "bye");
        assert_eq!(result.stdout, b"bye\n");
        assert_eq!(result.end, ExecEnd::Finished);
        wait_until("the cached connection to be closed", || !exec.conn.is_connected());
        // No attempt on the dead handle: the next command goes straight to a new connection.
        assert_eq!(run_default(&exec, "conn").stdout, b"conn 1\n");
        assert_eq!(server.seen.connections.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn exec_calls_at_the_same_time_share_one_connection() {
        let server = serve_exec();
        let dir = scratch_dir("parallel");
        let exec = Arc::new(SshExec::new("Test host", exec_target(&server, dir.path())).unwrap());
        let calls: Vec<_> = (0..2)
            .map(|_| {
                let exec = exec.clone();
                thread::spawn(move || run_default(&exec, "nap 300"))
            })
            .collect();
        for call in calls {
            let result = call.join().unwrap();
            assert_eq!(result.stdout, b"napped\n");
            assert_eq!(result.end, ExecEnd::Finished);
        }
        assert_eq!(server.seen.connections.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn exec_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SshExec>();
        assert_send_sync::<ExecCancel>();
        assert_send_sync::<ExecResult>();
    }

    /// Against a real SSH server, for the questions the in-process server cannot answer: the order of exit status and
    /// close, whether a signal request stops a process, the sessions limit. What it prints goes into the test plan.
    #[test]
    #[ignore = "needs a real SSH server: set HOLZWAVE_TEST_SSH_HOST, _USER, _KEY, _KNOWN_HOSTS and optionally _PORT"]
    fn exec_against_a_real_host() {
        fn var(name: &str) -> String {
            std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
        }
        let mut t = SshTarget::new(
            var("HOLZWAVE_TEST_SSH_HOST"),
            var("HOLZWAVE_TEST_SSH_USER"),
            SshAuth::Key { path: var("HOLZWAVE_TEST_SSH_KEY").into(), passphrase: None },
        );
        if let Ok(port) = std::env::var("HOLZWAVE_TEST_SSH_PORT") {
            t.port = port.parse().expect("HOLZWAVE_TEST_SSH_PORT is a port number");
        }
        t.known_hosts = Some(var("HOLZWAVE_TEST_SSH_KNOWN_HOSTS").into());
        t.host_key = HostKeyPolicy::Strict;
        let exec = SshExec::new("Real host", t).unwrap();

        let result = run_default(&exec, "sh -c 'echo out; echo err >&2; exit 3'");
        assert_eq!(result.stdout, b"out\n");
        assert_eq!(result.stderr, b"err\n");
        assert_eq!(result.status, Some(3));
        assert_eq!(result.end, ExecEnd::Finished);

        // The input is closed at once, so `cat` ends instead of waiting.
        let started = Instant::now();
        let result = run_default(&exec, "cat");
        assert_eq!((result.status, result.end), (Some(0), ExecEnd::Finished));
        assert!(started.elapsed() < Duration::from_secs(10), "cat waited for input");

        let limits = ExecLimits { max_output: 1000, timeout: Duration::from_secs(30) };
        let result = exec.exec("head -c 3000000 /dev/zero", &limits, &ExecCancel::new()).unwrap();
        assert_eq!(result.stdout.len(), 1000);
        assert_eq!(result.dropped, 2_999_000);

        // For a human to read: what a process killed by a signal reports, and what a timed-out `sleep 30` does.
        let result = run_default(&exec, "sh -c 'kill -TERM $$'");
        println!("killed by TERM: status {:?}, end {:?}", result.status, result.end);
        let limits = ExecLimits { max_output: 1000, timeout: Duration::from_secs(1) };
        let started = Instant::now();
        let result = exec.exec("sleep 30", &limits, &ExecCancel::new()).unwrap();
        println!("sleep 30 with a 1 s limit: end {:?} after {:?}, status {:?}", result.end, started.elapsed(), result.status);
    }
}
