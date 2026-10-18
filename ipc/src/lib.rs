//! The local socket between the running app and `wsh`.
//!
//! The app listens on a socket in a directory only its user can enter (a Unix domain socket; a named pipe on Windows), and
//! hands every pane it starts three variables: `HOLZWAVE_IPC` (the directory), `HOLZWAVE_TOKEN` (a random secret of this
//! run of the app) and `HOLZWAVE_PANE`. A request is one line of JSON and so is the answer. A request without the token
//! is refused before anything is read from it, so only a process that was started inside a pane (or that can read its
//! environment, which is the same user) can control the app. Every size is bounded and every field is checked here,
//! before the app's handler sees it.
//!
//! # Streaming commands
//!
//! A few commands carry a body and answer with a stream (today: `ai`). On the wire every message is one JSON object per
//! line (UTF-8, `\n`), except the body, which is raw UTF-8 text of an announced length:
//!
//! 1. The client writes the request line, for example `{"token":..,"pane":..,"cmd":"ai","prompt":..,"stdin_bytes":N}`.
//!    `stdin_bytes` announces the length of the body.
//! 2. A request that is refused before the stream starts (wrong token, malformed request, a member the command does not
//!    have, a size over its limit, no free stream slot) is answered by one line `{"ok":false,"message":..}`, and the server
//!    closes. The body is never read in that case, and the token is checked before anything else is looked at.
//! 3. Otherwise the server writes `{"event":"ready"}`. Only then the client writes exactly N bytes of UTF-8 text without a
//!    NUL, and nothing more. The server reads them through the same buffered reader that read the request line, so no byte
//!    is lost or read twice.
//! 4. The server writes zero or more `{"event":"text","text":..}` lines (the answer, for standard output) and
//!    `{"event":"note","text":..}` lines (status, for standard error), then exactly one
//!    `{"event":"end","ok":..,"message":..}`, and closes. After `ready` every problem is an `end` line.
//! 5. The client keeps its end open until `end`. Closing it earlier, or sending anything after the body, cancels the request:
//!    the handler is told through [`Gone`] and its next write fails.
//!
//! This crate has no command that confirms, approves or decides anything on the user's behalf: those exist only as commands
//! of the webview, because every process in a pane holds the token and the token cannot prove what a person agreed to.
//!
//! # The remote scope
//!
//! A request that arrives through an SSH forward (a remote pane's `wsh`) is answered by [`serve_remote`], one request per
//! connection, and may ask four things only: `ping`, `view`, `edit` and `setmeta` ([`Cmd::allowed_remote`]). Every other
//! command is refused with [`REMOTE_REFUSED`] before a body is read and before the handler is called. The pane comes from
//! the caller (the forward and its token identify it), never from the request: the `pane` member of a request is ignored.
//! The token is checked in constant time before anything else is looked at, and a pane may send [`REMOTE_REQUESTS`]
//! requests in [`REMOTE_WINDOW`] ([`RateLimit`]); the next one is answered `busy`. The local socket does not use any of this.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::{ListenerExt as _, Stream as _};
use interprocess::local_socket::{GenericFilePath, GenericNamespaced, ListenerOptions, Stream, ToFsName, ToNsName};
use serde::{Deserialize, Serialize};

mod stream;
pub use stream::{Event, Gone, Outcome, Sink, stream_request, stream_request_at};
pub use stream::{MAX_CHUNK, MAX_NOTE, MAX_OUTPUT, MAX_REPLY};

/// The longest request line, in bytes.
pub const MAX_LINE: usize = 64 * 1024;
/// The longest path in a command.
pub const MAX_PATH: usize = 4096;
/// The longest command of `hist`.
pub const MAX_HIST: usize = 8192;
/// The longest value of `setmeta`.
pub const MAX_VALUE: usize = 200;
/// The longest prompt of `ai`, in bytes.
pub const MAX_PROMPT: usize = 16 * 1024;
/// The longest body (the text piped into `wsh ai`), in bytes.
pub const MAX_STDIN: usize = 1024 * 1024;
/// How many requests are handled at once.
const MAX_CLIENTS: usize = 8;
/// How many streams run at once. They count against [`MAX_CLIENTS`] too, so the rest stays free for the shell hooks.
const MAX_STREAMS: usize = 2;
/// How long a connection may stay silent before it is dropped (not on Windows: named pipes have no timeouts).
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// What a remote pane is refused with when it asks for a command outside its scope.
pub const REMOTE_REFUSED: &str = "not available over a remote connection";
/// How many requests a remote pane may send in [`REMOTE_WINDOW`].
pub const REMOTE_REQUESTS: usize = 20;
/// The window of [`REMOTE_REQUESTS`].
pub const REMOTE_WINDOW: Duration = Duration::from_secs(10);

/// Refusal texts that tests and clients rely on.
const PROMPT_TOO_LONG: &str = "the prompt is longer than 16384 bytes or has a NUL";
const INPUT_TOO_LONG: &str = "the input is longer than 1048576 bytes";
const NOTHING_TO_ASK: &str = "there is nothing to ask: give a prompt or pipe text in";
const BUSY_STREAMS: &str = "busy: wsh ai requests are running already; try again when one has ended";
const NO_REQUEST_IN_TIME: &str = "no request arrived in time";

/// What `wsh` can ask.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Cmd {
    /// Show a file in a preview beside the pane.
    View { path: String },
    /// Open a file in the editor beside the pane.
    Edit { path: String },
    /// Set a property of the pane. `key` is `title`.
    Setmeta { key: String, value: String },
    /// A command the shell ran, for the history. The app decides whether it is worth keeping.
    Hist { line: String, cwd: String, exit: Option<i32> },
    /// Ask the app to open its history search for the pane.
    HistOpen,
    /// Ask the model of the running app. `prompt` is the text as typed; `stdin_bytes` is the length of the text that follows
    /// the request line.
    Ai { prompt: String, stdin_bytes: u32 },
    /// Nothing: asks whether the app answers.
    Ping,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub token: String,
    /// The pane the command was typed in (`HOLZWAVE_PANE`).
    pub pane: Option<u64>,
    #[serde(flatten)]
    pub cmd: Cmd,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub message: String,
}

impl std::fmt::Debug for Request {
    /// The token is a secret: it never shows in a debug print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request").field("token", &format_args!("<hidden>")).field("pane", &self.pane).field("cmd", &self.cmd).finish()
    }
}

impl Cmd {
    /// Whether the command answers with a stream (and so carries a body).
    pub fn is_stream(&self) -> bool {
        matches!(self, Cmd::Ai { .. })
    }

    /// The length in bytes of the body that follows the request line.
    pub fn body_len(&self) -> usize {
        match self {
            Cmd::Ai { stdin_bytes, .. } => *stdin_bytes as usize,
            _ => 0,
        }
    }

    /// Whether a request that arrives through an SSH forward may ask this command: `ping`, `view`, `edit` and `setmeta`.
    /// Every other command is refused in the remote scope ([`serve_remote`]). There is no wildcard arm on purpose: a
    /// command added to the enum does not compile until it is given a decision here.
    pub fn allowed_remote(&self) -> bool {
        match self {
            Cmd::Ping | Cmd::View { .. } | Cmd::Edit { .. } | Cmd::Setmeta { .. } => true,
            Cmd::Hist { .. } | Cmd::HistOpen | Cmd::Ai { .. } => false,
        }
    }

    /// Whether the command is well formed: sizes and characters. The app checks what it means.
    pub fn check(&self) -> Result<(), String> {
        let path_ok = |p: &str| !p.is_empty() && p.len() <= MAX_PATH && !p.contains('\0');
        match self {
            Cmd::View { path } | Cmd::Edit { path } if !path_ok(path) => Err("the path is empty, too long or has a NUL".into()),
            Cmd::Setmeta { key, value } => {
                if key != "title" {
                    return Err(format!("unknown property {key:?}; the property is `title`"));
                }
                if value.chars().count() > MAX_VALUE || value.chars().any(char::is_control) {
                    return Err(format!("the title is longer than {MAX_VALUE} characters or has a control character"));
                }
                Ok(())
            }
            Cmd::Hist { line, cwd, .. } => {
                if line.is_empty() || line.len() > MAX_HIST || cwd.len() > MAX_PATH {
                    return Err("the command or the directory is empty or too long".into());
                }
                Ok(())
            }
            Cmd::Ai { prompt, stdin_bytes } => {
                if prompt.len() > MAX_PROMPT || prompt.contains('\0') {
                    return Err(PROMPT_TOO_LONG.into());
                }
                if *stdin_bytes as usize > MAX_STDIN {
                    return Err(INPUT_TOO_LONG.into());
                }
                if prompt.trim().is_empty() && *stdin_bytes == 0 {
                    return Err(NOTHING_TO_ASK.into());
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// What the app does with a command. `Ok` is the text `wsh` prints, `Err` the reason it exits with an error.
pub trait Handler: Send + Sync {
    fn handle(&self, pane: Option<u64>, cmd: Cmd) -> Result<String, String>;

    /// A command that answers with a stream (see the module documentation): `input` is the body, `gone` tells when the
    /// client left, `out` takes the text (for standard output) and the notes (for standard error). `Ok(())` ends the stream
    /// with `end` ok, `Err(message)` with `end` not ok. A handler must return soon after `gone` fires. The default answers
    /// through [`Handler::handle`] and sends a non-empty answer as one text.
    fn handle_stream(&self, pane: Option<u64>, cmd: Cmd, input: String, gone: &Gone, out: &mut dyn Sink) -> Result<(), String> {
        let _ = (input, gone);
        let answer = self.handle(pane, cmd)?;
        if !answer.is_empty() {
            out.text(&answer).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// A new random token: 128 bits as hex.
pub fn new_token() -> io::Result<String> {
    use std::fmt::Write;
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|e| io::Error::other(format!("no random bytes: {e}")))?;
    Ok(raw.iter().fold(String::with_capacity(32), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// Compare two secrets without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().min(b.len()) {
        diff |= usize::from(a[i] ^ b[i]);
    }
    diff == 0
}

/// Where the app listens, as a client finds it: the folder that holds the socket of this computer (`HOLZWAVE_IPC`), or the path
/// of one socket (`HOLZWAVE_SOCKET`, the end of a reverse forward on a remote host). This is the one place that decides how a
/// name is made from either; [`connect`], [`request`] and [`stream_request`] are the folder forms of the `_at` functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// The folder of the app's socket (`ipc.sock` in it on Unix, a pipe made from the folder on Windows).
    Dir(PathBuf),
    /// The path of a Unix socket. A pipe of Windows has no path, so this is not supported there.
    Socket(PathBuf),
}

impl From<&Path> for Endpoint {
    fn from(dir: &Path) -> Self {
        Endpoint::Dir(dir.to_path_buf())
    }
}

/// The longest socket path: `sun_path` is about 104 bytes on macOS and 108 on Linux.
const MAX_SOCKET_PATH: usize = 100;

fn endpoint_name(endpoint: &Endpoint) -> io::Result<interprocess::local_socket::Name<'static>> {
    match endpoint {
        Endpoint::Dir(dir) => name(dir),
        Endpoint::Socket(_) if cfg!(windows) => {
            Err(io::Error::new(io::ErrorKind::Unsupported, "a socket path is not supported on Windows"))
        }
        Endpoint::Socket(path) => {
            if path.as_os_str().len() > MAX_SOCKET_PATH {
                return Err(io::Error::other("the socket path is too long"));
            }
            path.as_path().to_fs_name::<GenericFilePath>().map(interprocess::local_socket::Name::into_owned)
        }
    }
}

fn name(dir: &Path) -> io::Result<interprocess::local_socket::Name<'static>> {
    if cfg!(windows) {
        // A named pipe is not in the file system; the directory's last parts make it the user's own.
        let tag: String = dir.to_string_lossy().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let tag = &tag[tag.len().saturating_sub(48)..];
        format!("holzwave-{tag}").to_ns_name::<GenericNamespaced>().map(interprocess::local_socket::Name::into_owned)
    } else {
        let path = dir.join("ipc.sock");
        if path.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(io::Error::other("the socket path is too long; set XDG_RUNTIME_DIR to a shorter one"));
        }
        path.to_fs_name::<GenericFilePath>().map(interprocess::local_socket::Name::into_owned)
    }
}

/// Read one line of at most [`MAX_LINE`] bytes, for a client that reads one answer.
fn read_line(stream: &mut impl Read) -> io::Result<String> {
    read_bounded_line(&mut BufReader::new(stream))
}

/// Read one line of at most [`MAX_LINE`] bytes through `reader`. Whatever the reader buffered beyond the newline stays in
/// it, so a body that follows the line is read through the same reader.
fn read_bounded_line(reader: &mut impl BufRead) -> io::Result<String> {
    let mut line = String::new();
    (&mut *reader).take(MAX_LINE as u64 + 1).read_line(&mut line)?;
    if line.len() > MAX_LINE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "the request is too long"));
    }
    Ok(line)
}

fn answer(stream: &mut impl Write, ok: bool, message: impl Into<String>) {
    let mut line = serde_json::to_string(&Response { ok, message: message.into() }).unwrap_or_default();
    line.push('\n');
    let _ = stream.write_all(line.as_bytes());
}

fn serve_one(stream: Stream, token: &str, handler: &dyn Handler, limits: &Limits, streams: &Arc<AtomicUsize>) {
    // A connection that says nothing is dropped after the timeout (named pipes have none: the result is ignored).
    let _ = stream.set_recv_timeout(Some(limits.read_timeout));
    let (recv, mut send) = stream.split();
    let mut reader = BufReader::new(recv);
    let line = match read_bounded_line(&mut reader) {
        Ok(l) => l,
        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
            return answer(&mut send, false, NO_REQUEST_IN_TIME);
        }
        Err(e) => return answer(&mut send, false, e.to_string()),
    };
    // The token first: a request that does not carry it is not looked at any further.
    let value: serde_json::Value = match serde_json::from_str(&line) {
        Ok(v) => v,
        Err(_) => return answer(&mut send, false, "not a valid request"),
    };
    if !value.get("token").and_then(|t| t.as_str()).is_some_and(|t| same(t, token)) {
        return answer(&mut send, false, "refused: wsh must run in a holzWave terminal (the token is missing or wrong)");
    }
    // The structure of the request type ignores members it does not know; an `ai` request is held to its five names, so a
    // request that carries `consent: true` or the like fails before any handler.
    if let Some(key) = stream::stray_member(&value) {
        return answer(&mut send, false, format!("unknown field `{key}` in an ai request"));
    }
    let request: Request = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => return answer(&mut send, false, format!("unknown command: {e}")),
    };
    if let Err(why) = request.cmd.check() {
        return answer(&mut send, false, why);
    }
    if request.cmd.is_stream() {
        return stream::serve(reader, send, request, handler, limits, streams);
    }
    match handler.handle(request.pane, request.cmd) {
        Ok(m) => answer(&mut send, true, m),
        Err(e) => answer(&mut send, false, e),
    }
}

/// How many requests may be accepted in a window. One per pane in the remote scope; it counts only requests that passed the
/// token and the scope, so a flood of refused ones neither uses it up nor is helped by it.
pub struct RateLimit {
    max: usize,
    window: Duration,
    accepted: Mutex<VecDeque<Instant>>,
}

impl RateLimit {
    /// At most `max` requests in any `window`.
    pub fn new(max: usize, window: Duration) -> Self {
        Self { max, window, accepted: Mutex::new(VecDeque::new()) }
    }

    /// Whether a request at `now` is inside the budget. A request that is let in is counted; one that is not, is not.
    pub fn allow(&self, now: Instant) -> bool {
        let mut accepted = self.accepted.lock().unwrap_or_else(PoisonError::into_inner);
        while accepted.front().is_some_and(|at| now.saturating_duration_since(*at) >= self.window) {
            accepted.pop_front();
        }
        if accepted.len() >= self.max {
            return false;
        }
        accepted.push_back(now);
        true
    }
}

/// Answer one request that arrived through an SSH forward (see the module documentation, "The remote scope").
///
/// The order is fixed: one bounded line is read; the token is checked in constant time before the line is looked at any
/// further; the request is parsed; the scope is checked ([`Cmd::allowed_remote`]), so a command outside it is refused
/// before its own checks, before the budget and before a body is read; then the budget ([`RateLimit`]); then the command's
/// own checks; then the handler, with `pane` (the caller's, never the request's). Exactly one answer line is written.
pub fn serve_remote(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    token: &str,
    pane: u64,
    handler: &dyn Handler,
    limit: &RateLimit,
) {
    let line = match read_bounded_line(reader) {
        Ok(line) => line,
        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
            return answer(writer, false, NO_REQUEST_IN_TIME);
        }
        Err(e) => return answer(writer, false, e.to_string()),
    };
    let value: serde_json::Value = match serde_json::from_str(&line) {
        Ok(value) => value,
        Err(_) => return answer(writer, false, "not a valid request"),
    };
    if !value.get("token").and_then(|t| t.as_str()).is_some_and(|t| same(t, token)) {
        return answer(writer, false, "refused: wsh must run in a holzWave terminal (the token is missing or wrong)");
    }
    let request: Request = match serde_json::from_value(value) {
        Ok(request) => request,
        Err(e) => return answer(writer, false, format!("unknown command: {e}")),
    };
    if !request.cmd.allowed_remote() {
        return answer(writer, false, REMOTE_REFUSED);
    }
    if !limit.allow(Instant::now()) {
        return answer(writer, false, "busy");
    }
    if let Err(why) = request.cmd.check() {
        return answer(writer, false, why);
    }
    match handler.handle(Some(pane), request.cmd) {
        Ok(message) => answer(writer, true, message),
        Err(message) => answer(writer, false, message),
    }
}

/// A running server. Dropping it stops accepting.
pub struct Server {
    stop: Arc<AtomicBool>,
    dir: PathBuf,
}

impl Server {
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // A connection wakes the blocked accept; the socket file goes.
        let _ = connect(&self.dir);
        let _ = std::fs::remove_file(self.dir.join("ipc.sock"));
    }
}

/// How many connections run at once and how long a silent one is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Requests of every kind that are handled at once.
    pub clients: usize,
    /// Streams that run at once; they are part of `clients`, so `clients - streams` slots stay for the others.
    pub streams: usize,
    /// How long a connection may say nothing (Unix only; a named pipe has no timeouts).
    pub read_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { clients: MAX_CLIENTS, streams: MAX_STREAMS, read_timeout: READ_TIMEOUT }
    }
}

/// A place in a counted pool, given back when the guard is dropped (so a panic or a failed thread start leaks none).
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(counter: &Arc<AtomicUsize>, max: usize) -> Option<Slot> {
        if counter.fetch_add(1, Ordering::SeqCst) >= max {
            counter.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Slot(counter.clone()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Make the directory (only its user may enter it) and listen in it, with the default [`Limits`].
pub fn serve(dir: &Path, token: String, handler: Arc<dyn Handler>) -> io::Result<Server> {
    serve_with(dir, token, handler, Limits::default())
}

/// Make the directory (only its user may enter it) and listen in it.
pub fn serve_with(dir: &Path, token: String, handler: Arc<dyn Handler>, limits: Limits) -> io::Result<Server> {
    make_private_dir(dir)?;
    let socket = dir.join("ipc.sock");
    // A socket left by a crashed run is not a reason to refuse to start.
    if !cfg!(windows) {
        let _ = std::fs::remove_file(&socket);
    }
    let listener = ListenerOptions::new().name(name(dir)?).create_sync()?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let active = Arc::new(AtomicUsize::new(0));
    let streams = Arc::new(AtomicUsize::new(0));
    thread::Builder::new().name("holzwave-ipc".into()).spawn(move || {
        for stream in listener.incoming() {
            if flag.load(Ordering::SeqCst) {
                return;
            }
            let Ok(stream) = stream else { continue };
            let Some(slot) = Slot::take(&active, limits.clients) else {
                let mut stream = stream;
                answer(&mut stream, false, "busy");
                continue;
            };
            let (token, handler, streams) = (token.clone(), handler.clone(), streams.clone());
            // The slot moves into the thread: it is given back when the thread ends, however it ends, and when the thread
            // cannot start the closure (and the slot with it) is dropped.
            let _ = thread::Builder::new().name("holzwave-ipc-client".into()).spawn(move || {
                let _slot = slot;
                serve_one(stream, &token, &*handler, &limits, &streams);
            });
        }
    })?;
    Ok(Server { stop, dir: dir.to_path_buf() })
}

#[cfg(unix)]
fn make_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    // A directory that was there before keeps its mode otherwise.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn make_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Connect to the app whose socket is in `dir`.
pub fn connect(dir: &Path) -> io::Result<Stream> {
    connect_endpoint(&Endpoint::from(dir))
}

/// Connect to the app at `endpoint`.
pub fn connect_endpoint(endpoint: &Endpoint) -> io::Result<Stream> {
    Stream::connect(endpoint_name(endpoint)?)
}

/// Send one request and read the answer. A command that streams needs [`stream_request`].
pub fn request(dir: &Path, request: &Request) -> io::Result<Response> {
    request_at(&Endpoint::from(dir), request)
}

/// [`request`] to an [`Endpoint`].
pub fn request_at(endpoint: &Endpoint, request: &Request) -> io::Result<Response> {
    if request.cmd.is_stream() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "this command streams; use stream_request"));
    }
    let mut stream = connect_endpoint(endpoint)?;
    let mut line = serde_json::to_string(request).map_err(io::Error::other)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let answer = read_line(&mut stream)?;
    serde_json::from_str(&answer).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the app sent no valid answer"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Echo(Mutex<Vec<(Option<u64>, Cmd)>>);

    impl Handler for Echo {
        fn handle(&self, pane: Option<u64>, cmd: Cmd) -> Result<String, String> {
            self.0.lock().unwrap().push((pane, cmd.clone()));
            match cmd {
                Cmd::View { path } if path == "/boom" => Err("no such file".into()),
                _ => Ok("done".into()),
            }
        }
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hw-ipc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn req(token: &str, pane: Option<u64>, cmd: Cmd) -> Request {
        Request { token: token.into(), pane, cmd }
    }

    #[test]
    fn a_command_with_the_token_reaches_the_handler_and_its_answer_comes_back() {
        let d = dir("a");
        let echo = Arc::new(Echo(Mutex::default()));
        let _server = serve(&d, "secret".into(), echo.clone()).unwrap();
        let ok = request(&d, &req("secret", Some(3), Cmd::View { path: "/a/b".into() })).unwrap();
        assert_eq!(ok, Response { ok: true, message: "done".into() });
        assert_eq!(echo.0.lock().unwrap().as_slice(), &[(Some(3), Cmd::View { path: "/a/b".into() })]);
        let err = request(&d, &req("secret", None, Cmd::View { path: "/boom".into() })).unwrap();
        assert_eq!(err, Response { ok: false, message: "no such file".into() });
        assert!(request(&d, &req("secret", None, Cmd::Ping)).unwrap().ok);
    }

    #[test]
    fn a_wrong_or_missing_token_is_refused_and_the_handler_never_runs() {
        let d = dir("b");
        let echo = Arc::new(Echo(Mutex::default()));
        let _server = serve(&d, "secret".into(), echo.clone()).unwrap();
        let r = request(&d, &req("guess", None, Cmd::Ping)).unwrap();
        assert!(!r.ok && r.message.contains("refused"));
        let mut stream = connect(&d).unwrap();
        stream.write_all(b"{\"cmd\":\"view\",\"path\":\"/x\"}\n").unwrap();
        let answer = read_line(&mut stream).unwrap();
        assert!(answer.contains("refused"), "{answer}");
        assert!(echo.0.lock().unwrap().is_empty());
    }

    #[test]
    fn malformed_oversized_and_unknown_requests_are_answered_not_crashed_on() {
        let d = dir("c");
        let echo = Arc::new(Echo(Mutex::default()));
        let _server = serve(&d, "secret".into(), echo.clone()).unwrap();
        for (line, expect) in [
            ("not json\n".to_owned(), "not a valid request"),
            ("{\"token\":\"secret\",\"cmd\":\"rm\"}\n".to_owned(), "unknown command"),
            (format!("{}\n", "x".repeat(MAX_LINE + 10)), "too long"),
        ] {
            let mut stream = connect(&d).unwrap();
            stream.write_all(line.as_bytes()).unwrap();
            let answer = read_line(&mut stream).unwrap();
            assert!(answer.contains(expect), "{answer}");
        }
        let bad = request(&d, &req("secret", None, Cmd::Setmeta { key: "cwd".into(), value: "x".into() })).unwrap();
        assert!(!bad.ok);
        let bad = request(&d, &req("secret", None, Cmd::Edit { path: "a\0b".into() })).unwrap();
        assert!(!bad.ok);
        assert!(echo.0.lock().unwrap().is_empty());
    }

    #[test]
    fn tokens_are_32_hex_characters_and_differ() {
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert!(a.len() == 32 && a.bytes().all(|c| c.is_ascii_hexdigit()) && a != b);
    }

    #[test]
    fn the_secrets_compare_exactly() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd") && !same("abc", "ab") && !same("", "a"));
    }

    #[cfg(unix)]
    #[test]
    fn the_directory_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("d");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _server = serve(&d, "t".into(), Arc::new(Echo(Mutex::default()))).unwrap();
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn a_path_that_is_too_long_for_a_socket_is_said() {
        let long = PathBuf::from("/tmp").join("x".repeat(120));
        if !cfg!(windows) {
            assert!(serve(&long, "t".into(), Arc::new(Echo(Mutex::default()))).is_err());
        }
    }

    #[test]
    fn the_ai_request_names_a_prompt_and_an_input_length_and_nothing_else() {
        let r = req("t", Some(2), Cmd::Ai { prompt: "why".into(), stdin_bytes: 5 });
        let value = serde_json::to_value(&r).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["cmd", "pane", "prompt", "stdin_bytes", "token"]);
        assert_eq!(value["cmd"], "ai");
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn the_ai_command_is_checked_for_size_text_and_emptiness() {
        let ai = |prompt: &str, n: u32| Cmd::Ai { prompt: prompt.into(), stdin_bytes: n };
        assert_eq!(ai(&"x".repeat(MAX_PROMPT + 1), 0).check(), Err(PROMPT_TOO_LONG.into()));
        assert_eq!(ai("a\0b", 0).check(), Err(PROMPT_TOO_LONG.into()));
        assert_eq!(ai("ok", MAX_STDIN as u32 + 1).check(), Err(INPUT_TOO_LONG.into()));
        assert_eq!(ai("  \n", 0).check(), Err(NOTHING_TO_ASK.into()));
        assert_eq!(ai("", 0).check(), Err(NOTHING_TO_ASK.into()));
        assert_eq!(ai(&"x".repeat(MAX_PROMPT), MAX_STDIN as u32).check(), Ok(()));
        assert_eq!(ai("", 1).check(), Ok(()), "the question may be in the input");
        assert_eq!(ai("q", 0).check(), Ok(()));
    }

    #[test]
    fn the_history_open_request_names_nothing_but_the_command() {
        let r = req("t", Some(2), Cmd::HistOpen);
        let value = serde_json::to_value(&r).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["cmd", "pane", "token"]);
        assert_eq!(value["cmd"], "hist_open");
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn history_open_is_a_plain_command_and_passes_the_check() {
        assert!(!Cmd::HistOpen.is_stream());
        assert_eq!(Cmd::HistOpen.body_len(), 0);
        assert_eq!(Cmd::HistOpen.check(), Ok(()));
    }

    #[test]
    fn history_open_reaches_the_handler_with_the_pane() {
        let d = dir("ho");
        let echo = Arc::new(Echo(Mutex::default()));
        let _server = serve(&d, "secret".into(), echo.clone()).unwrap();
        let answer = request(&d, &req("secret", Some(3), Cmd::HistOpen)).unwrap();
        assert!(answer.ok, "{answer:?}");
        assert_eq!(echo.0.lock().unwrap().as_slice(), &[(Some(3), Cmd::HistOpen)]);
    }

    #[test]
    fn a_stream_command_needs_the_stream_client() {
        let d = dir("s");
        let echo = Arc::new(Echo(Mutex::default()));
        let _server = serve(&d, "secret".into(), echo.clone()).unwrap();
        let err = request(&d, &req("secret", None, Cmd::Ai { prompt: "hi".into(), stdin_bytes: 0 })).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(echo.0.lock().unwrap().is_empty(), "nothing was sent");
        assert!(!Cmd::Ping.is_stream() && Cmd::Ai { prompt: String::new(), stdin_bytes: 3 }.is_stream());
        assert_eq!((Cmd::Ping.body_len(), Cmd::Ai { prompt: String::new(), stdin_bytes: 3 }.body_len()), (0, 3));
    }

    /// The words that say a command decides, approves, confirms or grants something.
    const DECIDING_WORDS: [&str; 14] = [
        "decide", "decision", "approve", "approval", "consent", "grant", "allow", "deny", "confirm", "accept", "permit",
        "authorize", "authorise", "trust",
    ];

    /// The names among `names` that say a command decides, approves, confirms or grants something.
    fn deciding_words(names: &[String]) -> Vec<String> {
        names.iter().filter(|name| DECIDING_WORDS.iter().any(|w| name.to_lowercase().contains(w))).cloned().collect()
    }

    /// The identifiers in the block of the enum `name` in `source` (its doc comments left out) that say the command decides,
    /// approves, confirms or grants something.
    fn deciding_names(source: &str, name: &str) -> Vec<String> {
        let header = format!("pub enum {name} {{");
        let Some(start) = source.find(&header) else { return vec![format!("no enum named {name}")] };
        let mut found = Vec::new();
        for line in source[start + header.len()..].lines().skip(1) {
            if line == "}" {
                return found;
            }
            if line.trim_start().starts_with("//") {
                continue;
            }
            for word in line.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()) {
                if !deciding_words(&[word.to_owned()]).is_empty() {
                    found.push(word.to_owned());
                }
            }
        }
        vec![format!("the block of {name} does not end")]
    }

    #[test]
    fn no_command_of_the_local_socket_can_decide_approve_confirm_or_grant() {
        let names = deciding_names(include_str!("lib.rs"), "Cmd");
        assert!(names.is_empty(), "the socket must not decide, approve, confirm or grant: {names:?}");
    }

    #[test]
    fn the_deciding_name_guard_flags_a_synthetic_enum_and_passes_the_real_one() {
        let (a, b, c) = (["Ai", "Decide"].concat(), ["Con", "sent"].concat(), ["Grant", "Access"].concat());
        let synthetic = format!("pub enum Fake {{\n    /// A doc line that says allow.\n    {a} {{ call: String }},\n    {b},\n    {c} {{ id: u32 }},\n    Ping,\n}}\n");
        assert_eq!(deciding_names(&synthetic, "Fake"), [a, b, c]);
        assert!(deciding_names(&synthetic, "Missing")[0].starts_with("no enum"));
        assert!(deciding_names("pub enum Fake {\n    Ping,\n", "Fake")[0].contains("does not end"));
    }

    /// One value of every variant of `Cmd`. The match has no wildcard: a variant added to the enum does not compile here
    /// until it is listed, and `allowed_remote` has the same property, so a new command always gets a remote decision.
    fn every_command() -> Vec<Cmd> {
        let list = vec![
            Cmd::View { path: "/a".into() },
            Cmd::Edit { path: "/a".into() },
            Cmd::Setmeta { key: "title".into(), value: "t".into() },
            Cmd::Hist { line: "ls".into(), cwd: "/".into(), exit: Some(0) },
            Cmd::HistOpen,
            Cmd::Ai { prompt: "q".into(), stdin_bytes: 0 },
            Cmd::Ping,
        ];
        for cmd in &list {
            match cmd {
                Cmd::View { .. }
                | Cmd::Edit { .. }
                | Cmd::Setmeta { .. }
                | Cmd::Hist { .. }
                | Cmd::HistOpen
                | Cmd::Ai { .. }
                | Cmd::Ping => {}
            }
        }
        list
    }

    #[test]
    fn every_command_has_a_remote_decision() {
        let decisions: Vec<(String, bool)> =
            every_command().iter().map(|cmd| (serde_json::to_value(cmd).unwrap()["cmd"].as_str().unwrap().to_owned(), cmd.allowed_remote())).collect();
        assert_eq!(
            decisions,
            [
                ("view".to_owned(), true),
                ("edit".to_owned(), true),
                ("setmeta".to_owned(), true),
                ("hist".to_owned(), false),
                ("hist_open".to_owned(), false),
                ("ai".to_owned(), false),
                ("ping".to_owned(), true),
            ]
        );
    }

    #[test]
    fn no_remote_command_can_decide_approve_confirm_or_grant() {
        let accepted: Vec<Cmd> = every_command().into_iter().filter(Cmd::allowed_remote).collect();
        // Both spellings of every accepted command: the name on the wire and the name of the variant.
        let mut names: Vec<String> = accepted.iter().map(|cmd| serde_json::to_value(cmd).unwrap()["cmd"].as_str().unwrap().to_owned()).collect();
        names.extend(accepted.iter().map(|cmd| format!("{cmd:?}").split([' ', '{']).next().unwrap_or("").to_owned()));
        assert_eq!(accepted.len(), 4, "the remote scope has four commands");
        assert!(deciding_words(&names).is_empty(), "a remote command must not decide, approve, confirm or grant: {names:?}");
        // The matching itself flags a decision, and a name that merely contains no such word passes.
        let (consent, grant) = (["con", "sent"].concat(), ["Grant", "Access"].concat());
        assert_eq!(deciding_words(&[consent.clone(), "view".into(), grant.clone()]), [consent, grant]);
    }

    #[test]
    fn the_debug_output_of_a_request_hides_the_token() {
        let r = req("s3cr3t-token-value", Some(4), Cmd::Ai { prompt: "hello".into(), stdin_bytes: 2 });
        let shown = format!("{r:?}");
        assert!(shown.contains("hidden") && !shown.contains("s3cr3t-token-value"), "{shown}");
        assert!(shown.contains("pane"), "{shown}");
        assert!(!format!("{r:#?}").contains("s3cr3t"));
    }
}
