//! The supervisor's side of the socket to the spawn helper.
//!
//! The helper speaks one JSON object per datagram on a sequenced-packet socket (`spawn-helper/src/protocol.rs`): a request
//! carries an `id` and an `op`, its answer echoes the `id`, and a started worker's end of its link travels with the answer
//! as one descriptor. The helper also sends `exited` events without an id. This client sends only the four requests of that
//! file, with the mode and signal words of the helper's own types, and checks everything it receives: the size of a
//! datagram, that it is UTF-8 and JSON of the known shape, that an answer belongs to a request that waits for it, and that
//! exactly the descriptors it asked for arrive. A datagram that breaks a rule marks the helper dead, because after that
//! nothing it says can be trusted to line up with what was asked.
//!
//! The helper crate's `Answer` and `ExitedEvent` only serialize (the helper only ever sends them), so the answers are read
//! into a mirror struct here; the words of a refusal are matched against the helper's own `Refusal::code`.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use holzwave_spawn_helper::protocol::{MAX_MESSAGE, Mode, Refusal, Signal};
use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendFlags};
use serde::Deserialize;
use tokio::io::unix::AsyncFd;
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::limits;
use crate::log::{self, Level};

/// What a worker that was running under a helper that went away reports as its status.
pub const HELPER_GONE_STATUS: i32 = -1;

const GONE: &str = "the spawn helper is gone";

/// How long `start` waits for the exit status of a helper whose socket closed.
#[cfg(target_os = "linux")]
const EXIT_GRACE: Duration = Duration::from_millis(500);

/// Why the helper could not do what was asked. One sentence; the helper's own words are fixed codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperError {
    sentence: String,
    /// A worker may have been started although the request failed: the caller should kill the user's processes.
    may_have_started: bool,
}

impl HelperError {
    fn new(sentence: impl Into<String>) -> HelperError {
        HelperError { sentence: sentence.into(), may_have_started: false }
    }

    fn started(sentence: impl Into<String>) -> HelperError {
        HelperError { sentence: sentence.into(), may_have_started: true }
    }

    fn gone() -> HelperError {
        HelperError::new(GONE)
    }

    /// The sentence.
    pub fn sentence(&self) -> &str {
        &self.sentence
    }

    /// Whether a worker of the user may be running although the request failed.
    pub fn may_have_started(&self) -> bool {
        self.may_have_started
    }
}

impl fmt::Display for HelperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sentence)
    }
}

impl std::error::Error for HelperError {}

/// How long the client waits.
#[derive(Clone, Copy, Debug)]
pub struct HelperTimeouts {
    /// For the answer to one request.
    pub request: Duration,
    /// For the first answer of a helper that was just started.
    pub start: Duration,
}

impl Default for HelperTimeouts {
    fn default() -> HelperTimeouts {
        HelperTimeouts { request: limits::HELPER_REQUEST_TIMEOUT, start: limits::HELPER_START_TIMEOUT }
    }
}

/// How to start the helper and what to tell it. Everything the helper needs it gets on its command line; it trusts nothing
/// else.
#[derive(Clone, Debug, Default)]
pub struct HelperSettings {
    /// The helper program.
    pub program: std::path::PathBuf,
    /// The worker program the helper starts for each person.
    pub worker_path: std::path::PathBuf,
    /// The directory that holds the homes.
    pub home_base: std::path::PathBuf,
    /// The first and last user id the helper may start workers as.
    pub uid_range: Option<(u32, u32)>,
    /// The process limit of each person.
    pub nproc: Option<u64>,
    /// The open-file limit of each person.
    pub nofile: Option<u64>,
    /// The most workers at once.
    pub max_workers: Option<u64>,
    /// The shell of the panes.
    pub shell: Option<std::path::PathBuf>,
    /// The seconds a wipe may take.
    pub wipe_timeout_seconds: Option<u64>,
    /// The waits.
    pub timeouts: HelperTimeouts,
}

/// A worker the helper started.
#[derive(Debug)]
pub struct Spawned {
    /// The process id of the worker.
    pub pid: i32,
    /// The supervisor's end of the link to the worker.
    pub link: OwnedFd,
    /// `Some(status)` once the helper said the worker ended (or the helper itself went away).
    pub ended: watch::Receiver<Option<i32>>,
}

// ---- what is on the wire ---------------------------------------------------------------------------------------------

/// Every member an answer or an event of the helper may have. Anything else is a fault.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Incoming {
    id: Option<u64>,
    ok: Option<bool>,
    error: Option<String>,
    pid: Option<i32>,
    signalled: Option<u32>,
    workers: Option<u32>,
    event: Option<String>,
    uid: Option<u32>,
    status: Option<i32>,
    signal: Option<i32>,
}

/// Every refusal the helper can send.
const REFUSALS: [Refusal; 14] = [
    Refusal::TooLarge,
    Refusal::BadRequest,
    Refusal::UnknownOp,
    Refusal::UidRefused,
    Refusal::ModeRefused,
    Refusal::BusyUid,
    Refusal::WorkerLimit,
    Refusal::NoHome,
    Refusal::HomeRefused,
    Refusal::SpawnFailed,
    Refusal::WipeFailed,
    Refusal::WipeTimeout,
    Refusal::RemoveFailed,
    Refusal::KillFailed,
];

/// The sentence for a refusal: the helper's fixed word if it is one of the known ones, else a general sentence.
fn refusal_sentence(code: Option<&str>) -> String {
    match code.and_then(|code| REFUSALS.iter().find(|r| r.code() == code)) {
        Some(refusal) => format!("the spawn helper refused the request ({})", refusal.code()),
        None => "the spawn helper refused the request".to_owned(),
    }
}

/// One datagram as received.
struct Datagram {
    bytes: Vec<u8>,
    fds: Vec<OwnedFd>,
    /// The message was longer than the buffer or the control data was cut.
    cut: bool,
    /// More bytes than a message may have.
    oversize: bool,
}

/// What a request waits for.
enum Kind {
    Ping,
    Spawn { exit: watch::Sender<Option<i32>> },
    Kill,
    Remove,
}

/// The parts of an answer the caller needs.
struct Reply {
    pid: Option<i32>,
    signalled: Option<u32>,
    workers: Option<u32>,
    fd: Option<OwnedFd>,
    exit: Option<watch::Sender<Option<i32>>>,
}

struct Pending {
    kind: Kind,
    reply: oneshot::Sender<Result<Reply, HelperError>>,
}

#[derive(Default)]
struct State {
    pending: HashMap<u64, Pending>,
    exits: HashMap<i32, watch::Sender<Option<i32>>>,
}

struct Inner {
    socket: AsyncFd<OwnedFd>,
    timeouts: HelperTimeouts,
    next_id: AtomicU64,
    dropped: AtomicU64,
    alive: AtomicBool,
    state: Mutex<State>,
    cancel: CancellationToken,
}

/// Stops the reader when the last handle of the client is dropped.
struct Guard(CancellationToken);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// The client of the spawn helper. Cloning gives another handle to the same socket.
#[derive(Clone)]
pub struct HelperClient {
    inner: Arc<Inner>,
    _guard: Arc<Guard>,
    /// The user ids a request may name. User id 0 is never allowed; with no range every other id is.
    uid_range: Option<(u32, u32)>,
}

impl fmt::Debug for HelperClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HelperClient(..)")
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The helper cannot be used any more: every waiting request fails and every worker it ran is reported as ended.
    fn fail(&self) {
        self.alive.store(false, Ordering::SeqCst);
        let (pending, exits) = {
            let mut state = self.state();
            (std::mem::take(&mut state.pending), std::mem::take(&mut state.exits))
        };
        for (_, pending) in pending {
            let _ = pending.reply.send(Err(HelperError::gone()));
            if let Kind::Spawn { exit } = pending.kind {
                let _ = exit.send(Some(HELPER_GONE_STATUS));
            }
        }
        for (_, exit) in exits {
            let _ = exit.send(Some(HELPER_GONE_STATUS));
        }
        self.cancel.cancel();
    }

    /// A protocol fault: the one request it belongs to (if any) gets `error`, everything else is gone.
    fn fault(&self, id: Option<u64>, error: Option<HelperError>) {
        log::event(Level::Error, "supervisor.helper_fault", &[]);
        if let (Some(id), Some(error)) = (id, error)
            && let Some(pending) = self.state().pending.remove(&id)
        {
            let _ = pending.reply.send(Err(error));
            if let Kind::Spawn { exit } = pending.kind {
                let _ = exit.send(Some(HELPER_GONE_STATUS));
            }
        }
        self.fail();
    }

    /// Receives one datagram with the descriptors that came with it.
    fn receive(fd: &OwnedFd) -> io::Result<Option<Datagram>> {
        let mut buffer = [0u8; MAX_MESSAGE + 1];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut space);
        let mut iov = [IoSliceMut::new(&mut buffer)];
        #[cfg(target_os = "linux")]
        let flags = RecvFlags::CMSG_CLOEXEC;
        #[cfg(not(target_os = "linux"))]
        let flags = RecvFlags::empty();
        let message = rustix::net::recvmsg(fd, &mut iov, &mut ancillary, flags).map_err(io::Error::from)?;
        let mut fds = Vec::new();
        for item in ancillary.drain() {
            if let RecvAncillaryMessage::ScmRights(received) = item {
                for fd in received {
                    // Close-on-exec for every received descriptor, whatever the platform did with the flag above.
                    let _ = rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC);
                    fds.push(fd);
                }
            }
        }
        if message.bytes == 0 && fds.is_empty() {
            // A closed socket reads as an empty message.
            return Ok(None);
        }
        let cut = message.flags.intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC);
        let oversize = message.bytes > MAX_MESSAGE || message.flags.contains(ReturnFlags::TRUNC);
        let len = message.bytes.min(MAX_MESSAGE);
        Ok(Some(Datagram { bytes: buffer[..len].to_vec(), fds, cut, oversize }))
    }

    async fn receive_one(&self) -> io::Result<Option<Datagram>> {
        loop {
            let mut ready = self.socket.readable().await?;
            match ready.try_io(|socket| Inner::receive(socket.get_ref())) {
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {}
                Ok(result) => return result,
                Err(_would_block) => {}
            }
        }
    }

    async fn send(&self, bytes: &[u8]) -> io::Result<()> {
        loop {
            let mut ready = self.socket.writable().await?;
            match ready.try_io(|socket| rustix::net::send(socket.get_ref(), bytes, SendFlags::empty()).map_err(io::Error::from)) {
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {}
                Ok(Ok(_)) => return Ok(()),
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => {}
            }
        }
    }

    /// Checks one datagram and routes it. False once the helper is marked dead.
    fn route(&self, datagram: Datagram) -> bool {
        let Datagram { bytes, fds, cut, oversize } = datagram;
        if oversize {
            self.fault(None, None);
            return false;
        }
        let incoming: Incoming = match serde_json::from_slice(&bytes) {
            Ok(incoming) => incoming,
            Err(_) => {
                self.fault(None, None);
                return false;
            }
        };
        if let Some(event) = incoming.event.as_deref() {
            // Events carry no id, no descriptor and, for `exited`, a process number and a status.
            let (Some(pid), Some(status)) = (incoming.pid, incoming.status) else {
                self.fault(None, None);
                return false;
            };
            if event != "exited" || incoming.id.is_some() || incoming.ok.is_some() || !fds.is_empty() || incoming.uid.is_none() {
                self.fault(None, None);
                return false;
            }
            let _ = incoming.signal;
            if let Some(exit) = self.state().exits.remove(&pid) {
                let _ = exit.send(Some(status));
            }
            return true;
        }
        let (Some(id), Some(ok)) = (incoming.id, incoming.ok) else {
            self.fault(None, None);
            return false;
        };
        let pending = self.state().pending.remove(&id);
        let Some(pending) = pending else {
            // An answer for a request nobody waits for (or waits for any more): its descriptors close here.
            self.dropped.fetch_add(1, Ordering::Relaxed);
            drop(fds);
            return true;
        };
        if !ok {
            let _ = pending.reply.send(Err(HelperError::new(refusal_sentence(incoming.error.as_deref()))));
            if !fds.is_empty() || cut {
                self.fault(None, None);
                return false;
            }
            return true;
        }
        let wrong = HelperError::new("the spawn helper sent the wrong descriptors");
        match pending.kind {
            Kind::Spawn { exit } => {
                if cut || fds.len() > 1 {
                    // More descriptors than asked for: all of them are closed and the helper cannot be trusted.
                    drop(fds);
                    let _ = pending.reply.send(Err(HelperError::started("the spawn helper sent too many descriptors")));
                    self.fault(None, None);
                    return false;
                }
                let (Some(pid), Some(fd)) = (incoming.pid.filter(|pid| *pid > 0), fds.into_iter().next()) else {
                    let _ = pending.reply.send(Err(HelperError::started("the spawn helper sent no descriptor for the worker")));
                    return true;
                };
                self.state().exits.insert(pid, exit.clone());
                let _ = pending.reply.send(Ok(Reply { pid: Some(pid), signalled: None, workers: None, fd: Some(fd), exit: Some(exit) }));
            }
            kind => {
                if cut || !fds.is_empty() {
                    drop(fds);
                    let _ = pending.reply.send(Err(wrong));
                    self.fault(None, None);
                    return false;
                }
                let complete = match kind {
                    Kind::Ping => incoming.workers.is_some(),
                    Kind::Kill => incoming.signalled.is_some(),
                    Kind::Remove | Kind::Spawn { .. } => true,
                };
                if !complete {
                    let _ = pending.reply.send(Err(HelperError::new("the spawn helper sent an incomplete answer")));
                    self.fault(None, None);
                    return false;
                }
                let _ = pending.reply.send(Ok(Reply {
                    pid: None,
                    signalled: incoming.signalled,
                    workers: incoming.workers,
                    fd: None,
                    exit: None,
                }));
            }
        }
        true
    }
}

/// Reads the socket until it closes, a datagram breaks a rule or the last handle is dropped.
async fn read_loop(inner: Arc<Inner>) {
    loop {
        let received = tokio::select! {
            () = inner.cancel.cancelled() => return,
            received = inner.receive_one() => received,
        };
        match received {
            Ok(Some(datagram)) => {
                if !inner.route(datagram) {
                    return;
                }
            }
            Ok(None) | Err(_) => {
                inner.fail();
                return;
            }
        }
    }
}

impl HelperClient {
    /// A client over a datagram socket whose other end is the helper (or a stand-in). Must be called inside a runtime.
    pub fn from_socket(socket: OwnedFd, timeouts: HelperTimeouts) -> io::Result<HelperClient> {
        let flags = rustix::fs::fcntl_getfl(&socket).map_err(io::Error::from)?;
        rustix::fs::fcntl_setfl(&socket, flags | rustix::fs::OFlags::NONBLOCK).map_err(io::Error::from)?;
        rustix::io::fcntl_setfd(&socket, rustix::io::FdFlags::CLOEXEC).map_err(io::Error::from)?;
        let cancel = CancellationToken::new();
        let inner = Arc::new(Inner {
            socket: AsyncFd::new(socket)?,
            timeouts,
            next_id: AtomicU64::new(1),
            dropped: AtomicU64::new(0),
            alive: AtomicBool::new(true),
            state: Mutex::new(State::default()),
            cancel: cancel.clone(),
        });
        tokio::spawn(read_loop(inner.clone()));
        Ok(HelperClient { inner, _guard: Arc::new(Guard(cancel)), uid_range: None })
    }

    /// The same client, refusing in the server any request that names a user id outside `min..=max` before the helper sees
    /// it. The helper checks the range as well; this is the second line, so that a wrong id in the store never leaves the
    /// server.
    #[must_use]
    pub fn with_uid_range(mut self, min: u32, max: u32) -> HelperClient {
        self.uid_range = Some((min, max));
        self
    }

    /// Whether a request may name `uid`: never 0, and inside the range when there is one.
    fn check_uid(&self, uid: u32) -> Result<(), HelperError> {
        let outside = uid == 0 || self.uid_range.is_some_and(|(min, max)| uid < min || uid > max);
        if outside {
            return Err(HelperError::new(format!("the user id {uid} is outside the range workers may run as; nothing was sent to the spawn helper")));
        }
        Ok(())
    }

    /// Starts the helper with a sequenced-packet socket as its standard input and waits for its first answer. An early exit
    /// of the helper is reported with its status, a helper that does not answer in time is killed and reaped.
    #[cfg(target_os = "linux")]
    pub async fn start(settings: &HelperSettings) -> Result<HelperClient, HelperError> {
        use std::os::unix::process::ExitStatusExt;
        use std::process::{Command, Stdio};

        let program = settings.program.display().to_string();
        let (ours, theirs) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )
        .map_err(|_| HelperError::new("the control socket for the spawn helper could not be made"))?;
        let mut command = Command::new(&settings.program);
        command
            .args(["--control-fd", "0", "--worker-path"])
            .arg(&settings.worker_path)
            .arg("--home-base")
            .arg(&settings.home_base);
        if let Some((min, max)) = settings.uid_range {
            command.args(["--uid-min", &min.to_string(), "--uid-max", &max.to_string()]);
        }
        for (flag, value) in [("--nproc", settings.nproc), ("--nofile", settings.nofile), ("--max-workers", settings.max_workers), ("--wipe-timeout", settings.wipe_timeout_seconds)] {
            if let Some(value) = value {
                command.args([flag, &value.to_string()]);
            }
        }
        if let Some(shell) = &settings.shell {
            command.arg("--shell").arg(shell);
        }
        // The helper's end is its standard input (`--control-fd 0`); the server's end is close-on-exec and never reaches it.
        command.stdin(Stdio::from(theirs)).stdout(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| HelperError::new(format!("the spawn helper {program} could not be started")))?;
        // The command still holds the helper's end; closing it here leaves only the helper's copy.
        drop(command);
        let pid = rustix::process::Pid::from_raw(child.id() as i32);
        let (exit_tx, mut exit_rx) = oneshot::channel::<io::Result<std::process::ExitStatus>>();
        std::thread::Builder::new()
            .name("helper-wait".into())
            .spawn(move || {
                let _ = exit_tx.send(child.wait());
            })
            .map_err(|_| HelperError::new("the spawn helper could not be watched"))?;

        let mut client = HelperClient::from_socket(ours, settings.timeouts)
            .map_err(|_| HelperError::new("the control socket for the spawn helper could not be used"))?;
        if let Some((min, max)) = settings.uid_range {
            client = client.with_uid_range(min, max);
        }
        let first = tokio::select! {
            status = &mut exit_rx => Err(status),
            answer = tokio::time::timeout(settings.timeouts.start, client.ping()) => Ok(answer),
        };
        let ended_at_once = |status: Result<io::Result<std::process::ExitStatus>, oneshot::error::RecvError>| {
            let how = match status {
                Ok(Ok(status)) => match (status.code(), status.signal()) {
                    (Some(code), _) => format!("with status {code}"),
                    (None, Some(signal)) => format!("by signal {signal}"),
                    (None, None) => "for an unknown reason".to_owned(),
                },
                _ => "for an unknown reason".to_owned(),
            };
            HelperError::new(format!("the spawn helper {program} ended at once {how}"))
        };
        match first {
            Err(status) => {
                client.inner.fail();
                Err(ended_at_once(status))
            }
            Ok(Ok(Ok(_workers))) => {
                // Watch the helper: when it ends the client is dead, and the workers it ran are reported as ended.
                let inner = client.inner.clone();
                tokio::spawn(async move {
                    tokio::select! {
                        () = inner.cancel.cancelled() => {}
                        _ = exit_rx => {
                            log::event(Level::Error, "supervisor.helper_exited", &[]);
                            inner.fail();
                        }
                    }
                });
                Ok(client)
            }
            Ok(Ok(Err(error))) => {
                client.inner.fail();
                // A helper that ended closes its socket first: the exit status follows a moment later and says more.
                if let Ok(status) = tokio::time::timeout(EXIT_GRACE, &mut exit_rx).await {
                    return Err(ended_at_once(status));
                }
                kill_and_wait(pid, &mut exit_rx).await;
                Err(HelperError::new(format!("the spawn helper {program} did not start: {error}")))
            }
            Ok(Err(_elapsed)) => {
                kill_and_wait(pid, &mut exit_rx).await;
                client.inner.fail();
                Err(HelperError::new(format!("the spawn helper {program} did not answer in time")))
            }
        }
    }

    /// Whether the helper can still be reached.
    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    /// Answers that carried an id nobody waited for.
    pub fn dropped_answers(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Sends one request and waits for its answer. `body` holds the members after `id` and `op`.
    async fn request(&self, op: &'static str, members: serde_json::Value, kind: Kind) -> Result<Reply, HelperError> {
        let inner = &self.inner;
        if !self.is_alive() {
            return Err(HelperError::gone());
        }
        let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
        let mut message = serde_json::json!({ "id": id, "op": op });
        if let (Some(map), serde_json::Value::Object(extra)) = (message.as_object_mut(), members) {
            map.extend(extra);
        }
        let bytes = serde_json::to_vec(&message).map_err(|_| HelperError::new("the request could not be written"))?;
        debug_assert!(bytes.len() <= MAX_MESSAGE);
        let spawning = matches!(kind, Kind::Spawn { .. });
        let (reply, answer) = oneshot::channel();
        {
            let mut state = inner.state();
            if !self.is_alive() {
                return Err(HelperError::gone());
            }
            if state.pending.len() >= limits::HELPER_PENDING_REQUESTS {
                return Err(HelperError::new("the spawn helper has too many requests waiting"));
            }
            state.pending.insert(id, Pending { kind, reply });
        }
        if inner.send(&bytes).await.is_err() {
            inner.state().pending.remove(&id);
            inner.fail();
            return Err(HelperError::gone());
        }
        match tokio::time::timeout(inner.timeouts.request, answer).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(HelperError::gone()),
            Err(_) => {
                inner.state().pending.remove(&id);
                // A spawn that was answered late has started a worker whose link nobody holds.
                let sentence = "the spawn helper did not answer in time";
                Err(if spawning { HelperError::started(sentence) } else { HelperError::new(sentence) })
            }
        }
    }

    /// Asks whether the helper lives; the number of workers it holds.
    pub async fn ping(&self) -> Result<u32, HelperError> {
        let reply = self.request("ping", serde_json::json!({}), Kind::Ping).await?;
        Ok(reply.workers.unwrap_or(0))
    }

    /// Starts a worker of the user `uid`. The user id is the only thing about the person that is sent.
    pub async fn spawn(&self, uid: u32, mode: Mode) -> Result<Spawned, HelperError> {
        self.check_uid(uid)?;
        let (exit, ended) = watch::channel(None);
        let reply = self.request("spawn", serde_json::json!({ "uid": uid, "mode": mode.as_str() }), Kind::Spawn { exit }).await?;
        match (reply.pid, reply.fd, reply.exit) {
            (Some(pid), Some(link), Some(_)) => Ok(Spawned { pid, link, ended }),
            _ => Err(HelperError::started("the spawn helper sent no descriptor for the worker")),
        }
    }

    /// Signals every process of the user `uid`; how many were signalled.
    pub async fn kill_uid(&self, uid: u32, signal: Signal) -> Result<u32, HelperError> {
        self.check_uid(uid)?;
        let word = match signal {
            Signal::Term => "term",
            Signal::Kill => "kill",
        };
        let reply = self.request("kill_uid", serde_json::json!({ "uid": uid, "signal": word }), Kind::Kill).await?;
        Ok(reply.signalled.unwrap_or(0))
    }

    /// Removes the home of the user `uid`.
    pub async fn remove_home(&self, uid: u32) -> Result<(), HelperError> {
        self.check_uid(uid)?;
        self.request("remove_home", serde_json::json!({ "uid": uid }), Kind::Remove).await.map(|_| ())
    }
}

/// Kills the helper process and waits until the watcher thread has reaped it.
#[cfg(target_os = "linux")]
async fn kill_and_wait(pid: Option<rustix::process::Pid>, exit: &mut oneshot::Receiver<io::Result<std::process::ExitStatus>>) {
    if exit.try_recv().is_ok() {
        return;
    }
    if let Some(pid) = pid {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), exit).await;
}
