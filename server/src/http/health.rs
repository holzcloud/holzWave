//! The health report that the container check reads.
//!
//! The server answers one question, "are you ready", on a Unix socket in the runtime directory and nowhere else: there is no
//! network port for it, so nothing outside the machine can ask and nothing about the server's state leaves it. `healthcheck`
//! (the subcommand of the same binary) asks that question and turns the answer into an exit status.
//!
//! The socket is made privately: it is bound inside a directory only the server can enter, given mode 0600, and then renamed
//! onto its name, so it never exists under that name with the wrong mode. The runtime directory must belong to the server's
//! user and must not be writable by anyone else, because anyone who can write there could replace the socket.

use std::convert::Infallible;
use std::fmt;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::body::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, header};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::app::Shutdown;
use crate::log::{self, Level};

/// The name of the health socket inside the runtime directory.
pub const SOCKET_NAME: &str = "health.sock";

/// The longest `healthcheck` waits for an answer.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The most connections the health socket serves at once; a ninth is closed.
pub const MAX_CONNECTIONS: usize = 8;

/// How long a client may take to send its request, and how long a connection may live in all.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a socket that is already there may take to accept before it is taken for a live server's.
const LIVE_CHECK_TIMEOUT: Duration = Duration::from_secs(1);

/// The most bytes of an answer the probe reads. The real answer is under 200.
const MAX_ANSWER: usize = 1024;

type Check = Arc<dyn Fn() -> bool + Send + Sync>;

/// The readiness checks of the subsystems: a fixed name and a function that says whether the subsystem works.
#[derive(Default)]
pub struct Readiness {
    checks: Mutex<Vec<(&'static str, Check)>>,
}

impl Readiness {
    /// A registry with no check: it is ready.
    pub fn new() -> Readiness {
        Readiness::default()
    }

    /// Registers a check under a fixed name. The function runs on a blocking thread and must be quick.
    pub fn register(&self, name: &'static str, check: impl Fn() -> bool + Send + Sync + 'static) {
        self.checks.lock().unwrap_or_else(PoisonError::into_inner).push((name, Arc::new(check)));
    }

    /// The names of the checks that do not pass. A check that panics does not pass.
    pub fn failing(&self) -> Vec<&'static str> {
        let checks: Vec<(&'static str, Check)> = self.checks.lock().unwrap_or_else(PoisonError::into_inner).clone();
        checks.into_iter().filter(|(_, check)| !catch_unwind(AssertUnwindSafe(|| check())).unwrap_or(false)).map(|(name, _)| name).collect()
    }

    /// Whether every check passes.
    pub fn ready(&self) -> bool {
        self.failing().is_empty()
    }
}

/// Why the health socket cannot be made: one sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthError(String);

impl fmt::Display for HealthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HealthError {}

fn refuse(sentence: String) -> HealthError {
    HealthError(sentence)
}

/// The running health socket.
pub struct HealthServer {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl HealthServer {
    /// The path of the socket.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Waits until the socket is closed and its file removed (after the shutdown signal).
    pub async fn finished(self) {
        let _ = self.task.await;
    }
}

/// Removes the socket file when the server ends, but only if it is still the file this server made.
struct Cleanup {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Ok(found) = fs::symlink_metadata(&self.path)
            && found.file_type().is_socket()
            && found.dev() == self.device
            && found.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// The runtime directory must exist, belong to the server's user and be writable by nobody else.
fn check_directory(dir: &Path) -> Result<(), HealthError> {
    let meta = fs::metadata(dir)
        .map_err(|error| refuse(format!("the runtime directory {} cannot be used for the health socket ({error})", dir.display())))?;
    if !meta.is_dir() {
        return Err(refuse(format!("the runtime directory {} is not a directory", dir.display())));
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return Err(refuse(format!("the runtime directory {} does not belong to the user the server runs as", dir.display())));
    }
    if meta.permissions().mode() & 0o022 != 0 {
        return Err(refuse(format!(
            "the runtime directory {} is writable by other users; remove that permission (chmod go-w)",
            dir.display()
        )));
    }
    Ok(())
}

/// What is at `path` now: nothing, or a socket nobody listens on (both fine), or something that stays.
async fn check_path(path: &Path) -> Result<(), HealthError> {
    let found = match fs::symlink_metadata(path) {
        Ok(found) => found,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(refuse(format!("{} cannot be examined ({error})", path.display()))),
    };
    if !found.file_type().is_socket() {
        return Err(refuse(format!("{} exists and is not a socket; remove it or change runtime_dir", path.display())));
    }
    match tokio::time::timeout(LIVE_CHECK_TIMEOUT, UnixStream::connect(path)).await {
        Ok(Ok(_)) | Err(_) => Err(refuse(format!("another server is listening on {}", path.display()))),
        Ok(Err(error)) if matches!(error.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound) => Ok(()),
        Ok(Err(error)) => Err(refuse(format!("{} cannot be replaced ({error})", path.display()))),
    }
}

/// Binds a socket in a private directory next to `path`, gives it mode 0600 and renames it onto `path`.
fn bind_private(dir: &Path, path: &Path) -> Result<(std::os::unix::net::UnixListener, Cleanup), HealthError> {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|_| refuse("the operating system gave no random bytes".to_owned()))?;
    let name: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let scratch = dir.join(format!(".hs-{name}"));
    let fail = |error: std::io::Error| refuse(format!("the health socket {} cannot be made ({error})", path.display()));
    fs::DirBuilder::new().mode(0o700).create(&scratch).map_err(fail)?;
    let made = (|| -> std::io::Result<(std::os::unix::net::UnixListener, Cleanup)> {
        let temporary = scratch.join("s");
        let listener = std::os::unix::net::UnixListener::bind(&temporary)?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temporary, path)?;
        let placed = fs::symlink_metadata(path)?;
        listener.set_nonblocking(true)?;
        Ok((listener, Cleanup { path: path.to_owned(), device: placed.dev(), inode: placed.ino() }))
    })();
    let _ = fs::remove_dir_all(&scratch);
    made.map_err(fail)
}

/// Binds `<runtime_dir>/health.sock` and answers `GET /healthz` until `shutdown` is triggered. The socket file is removed when
/// the server ends.
pub async fn serve(runtime_dir: &Path, readiness: Arc<Readiness>, shutdown: Shutdown) -> Result<HealthServer, HealthError> {
    let path = runtime_dir.join(SOCKET_NAME);
    check_directory(runtime_dir)?;
    check_path(&path).await?;
    let (dir, target) = (runtime_dir.to_owned(), path.clone());
    let (listener, cleanup) = tokio::task::spawn_blocking(move || bind_private(&dir, &target))
        .await
        .map_err(|_| refuse("the health socket could not be made".to_owned()))??;
    let listener = UnixListener::from_std(listener)
        .map_err(|error| refuse(format!("the health socket {} cannot be used ({error})", path.display())))?;
    let task = tokio::spawn(accept(listener, readiness, shutdown, cleanup));
    Ok(HealthServer { path, task })
}

/// Accepts connections until the shutdown signal. A connection beyond [`MAX_CONNECTIONS`] is closed at once.
async fn accept(listener: UnixListener, readiness: Arc<Readiness>, shutdown: Shutdown, _cleanup: Cleanup) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut connections: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            () = shutdown.wait() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
                let readiness = readiness.clone();
                connections.spawn(async move {
                    let _slot = slot;
                    let _ = tokio::time::timeout(CONNECTION_TIMEOUT, converse(stream, readiness)).await;
                });
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

/// One request on one connection.
async fn converse(stream: UnixStream, readiness: Arc<Readiness>) {
    let service = service_fn(move |request: Request<Incoming>| answer(readiness.clone(), request));
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder.timer(TokioTimer::new()).header_read_timeout(REQUEST_TIMEOUT).keep_alive(false).max_headers(32);
    if builder.serve_connection(TokioIo::new(stream), service).await.is_err() {
        log::event(Level::Warn, "health_connection_failed", &[]);
    }
}

fn reply(status: StatusCode, text: &'static str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(text.as_bytes())));
    *response.status_mut() = status;
    response.headers_mut().insert(header::CONTENT_TYPE, header::HeaderValue::from_static("text/plain; charset=utf-8"));
    response.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    response
}

/// `GET /healthz` is the only question; everything else is a 404 with nothing in it.
async fn answer(readiness: Arc<Readiness>, request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    if request.method() != Method::GET || request.uri().path_and_query().map(|target| target.as_str()) != Some("/healthz") {
        return Ok(reply(StatusCode::NOT_FOUND, ""));
    }
    let ready = tokio::task::spawn_blocking(move || readiness.ready()).await.unwrap_or(false);
    Ok(if ready { reply(StatusCode::OK, "ok") } else { reply(StatusCode::SERVICE_UNAVAILABLE, "not ready") })
}

/// What the probe found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// The server answered `ok`.
    Healthy,
    /// The server answered 503 `not ready`.
    NotReady,
    /// Nothing listens on the socket.
    Unreachable,
    /// The answer is not the server's.
    Garbled,
    /// No answer within the time.
    TimedOut,
}

impl Health {
    /// The one sentence for the person running the check.
    pub fn sentence(self, runtime_dir: &Path) -> String {
        let socket = runtime_dir.join(SOCKET_NAME);
        let socket = socket.display();
        match self {
            Health::Healthy => "healthy".to_owned(),
            Health::NotReady => format!("the server answered on {socket} that it is not ready"),
            Health::Unreachable => format!("nothing answers on {socket}; is the server running"),
            Health::Garbled => format!("the answer on {socket} is not the server's health report"),
            Health::TimedOut => format!("no answer on {socket} within {} seconds", PROBE_TIMEOUT.as_secs()),
        }
    }
}

/// Reads an answer: the status and the body of a short HTTP/1.1 response, or `Garbled`.
fn judge(answer: &[u8]) -> Health {
    let Some(split) = answer.windows(4).position(|window| window == b"\r\n\r\n") else { return Health::Garbled };
    let (head, body) = (&answer[..split], &answer[split + 4..]);
    let Ok(head) = std::str::from_utf8(head) else { return Health::Garbled };
    let status = head.lines().next().and_then(|line| line.strip_prefix("HTTP/1.1 ")).and_then(|rest| rest.get(..3));
    match (status, body) {
        (Some("200"), b"ok") => Health::Healthy,
        (Some("503"), b"not ready") => Health::NotReady,
        _ => Health::Garbled,
    }
}

/// Whether the bytes read so far hold a whole answer: the head and as many body bytes as it announces.
fn complete(answer: &[u8]) -> bool {
    let Some(split) = answer.windows(4).position(|window| window == b"\r\n\r\n") else { return false };
    let head = String::from_utf8_lossy(&answer[..split]).to_ascii_lowercase();
    let announced = head.lines().find_map(|line| line.strip_prefix("content-length:")).and_then(|value| value.trim().parse::<usize>().ok());
    announced.is_some_and(|length| answer.len() >= split + 4 + length)
}

/// The exchange on the socket, with the I/O bounded by `within`.
fn ask(path: &Path, within: Duration) -> Health {
    let mut stream = match StdUnixStream::connect(path) {
        Ok(stream) => stream,
        Err(_) => return Health::Unreachable,
    };
    if stream.set_read_timeout(Some(within)).is_err() || stream.set_write_timeout(Some(within)).is_err() {
        return Health::Garbled;
    }
    if stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: health\r\nConnection: close\r\n\r\n").is_err() {
        return Health::Garbled;
    }
    let mut answer = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                answer.extend_from_slice(&chunk[..count]);
                if answer.len() > MAX_ANSWER {
                    return Health::Garbled;
                }
                if complete(&answer) {
                    break;
                }
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Health::TimedOut,
            Err(_) => return Health::Garbled,
        }
    }
    judge(&answer)
}

/// Asks the server on the health socket in `runtime_dir` whether it is ready, within `within`. It blocks the calling thread for
/// at most that long.
pub fn probe(runtime_dir: &Path, within: Duration) -> Health {
    let path = runtime_dir.join(SOCKET_NAME);
    let (sender, receiver) = mpsc::channel();
    // The exchange runs on its own thread so that the wait ends on time even if a connect never returns.
    std::thread::spawn(move || {
        let _ = sender.send(ask(&path, within));
    });
    match receiver.recv_timeout(within) {
        Ok(found) => found,
        Err(RecvTimeoutError::Timeout) => Health::TimedOut,
        Err(RecvTimeoutError::Disconnected) => Health::Garbled,
    }
}
