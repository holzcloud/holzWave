//! The socket of a device: `GET /ws/device`, the one long-lived, bidirectional, binary channel of the device features.
//!
//! A device that holds a valid token upgrades with `Authorization: Bearer`; the router has checked the token and the gate in
//! front of it has made the route look absent while the remote switch is off. The socket then speaks the closed vocabulary of
//! [`holzwave_core::remote::proto`] with the person's worker, through the supervisor: each text message is one request about the
//! person's own sessions, each answer and event comes back as text, and the bytes of the terminals travel as binary frames. The
//! loop reads no byte of a terminal and writes none to any log or audit line; the log names events and counts only.
//!
//! # Ending
//!
//! A socket ends at once when its token is revoked or ends with its account (close 4401), when the remote switch goes off or the
//! server stops (1001), and at the latest at the next periodic look at the token's hash if an announcement was missed. A person
//! who has too many sockets is refused (1013), and so is a peer that does not take what is written to it. A peer whose stream
//! cannot be kept up loses that stream only: the supervisor ends the attachment with a `resync` and the socket goes on.
//!
//! # Who sees what
//!
//! The sessions are keys in the table of the person's own worker, and a worker is one person's user id; another person's number
//! is a number the worker never made and is answered exactly like one that never existed.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::Router;
use axum::extract::{FromRequestParts, Request};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use holzwave_core::remote::proto::{
    self, Answer, CLOSE_BUSY, CLOSE_ENCODING, CLOSE_GOING_AWAY, CLOSE_POLICY, CLOSE_PROTOCOL, CLOSE_REVOKED, CLOSE_TOO_LARGE,
    CLOSE_TYPE, ErrorWord, Event, Input, MAX_ATTACHED, MAX_BINARY, MAX_PENDING, MAX_TEXT, Op, Reply, Request as Call,
    SessionEntry, Stream, WriterView, close_word,
};
use tokio::sync::{broadcast, watch};
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until, timeout};

use super::app::{App, ConnectionPermit, Shutdown};
use super::features::Switches;
use super::router::{Access, DeviceAuthed, Part};
use super::transport::{Inbound, Outgoing, SocketLimits, Transport, TransportError, Upgrade};
use super::ws::{Bucket, Budget, SocketSettings, WsState};
use crate::auth::session::Ended;
use crate::limits::SYNTHETIC_REQUEST_BASE;
use crate::log::{self, Level};
use crate::store::{AccountId, TokenHash};
use crate::supervisor::{AccountKey, AttachError, Outbound, SocketHandle, SocketRx, SubmitError, Supervisor};
use crate::wire::{Ses, SesAnswer, SesEvent};
use crate::worker::lease::WriterState;

/// The longest a close message may take to write.
const CLOSE_BOUND: Duration = Duration::from_secs(3);

/// How long a worker may take to start for a socket that has been accepted.
const ATTACH_BOUND: Duration = Duration::from_secs(30);

/// The part of the application that serves `GET /ws/device`.
pub fn parts(state: Arc<WsState>) -> Vec<Part> {
    let device = Router::new().route("/ws/device", get(handler)).layer(Extension(state));
    vec![Part { access: Access::Device, router: device }]
}

async fn handler(request: Request) -> Response {
    let (mut parts, _body) = request.into_parts();
    let upgrade = match Upgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(error) => return error.into_response(),
    };
    let Some(state) = parts.extensions.get::<Arc<WsState>>().cloned() else {
        return super::router::ApiError::Busy.into_response();
    };
    let Some(who) = parts.extensions.remove::<DeviceAuthed>() else {
        return super::router::ApiError::Invalid.into_response();
    };
    let shutdown = parts.extensions.get::<Shutdown>().cloned().unwrap_or_default();
    let permit = parts.extensions.get::<ConnectionPermit>().cloned();
    // The transport reads a message of at most the largest binary frame; a text over its own, smaller cap is refused by the loop.
    let limits = SocketLimits::for_message_bytes(MAX_BINARY).keeping_binary();
    upgrade.accept(limits, move |transport| async move {
        // The admission slot of the connection lives as long as the socket does.
        let _permit = permit;
        let _open = state.tracker.enter();
        run_device_socket(transport, who, state.app.clone(), state.supervisor.clone(), state.settings, shutdown).await;
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------------------------------------------------

/// Runs one accepted socket: it is attached to the person's worker (which this starts when none runs) and served until it ends.
/// The socket is detached whatever the end.
pub async fn run_device_socket<T: Transport>(
    mut transport: T,
    who: DeviceAuthed,
    app: Arc<App>,
    supervisor: Supervisor,
    settings: SocketSettings,
    shutdown: Shutdown,
) {
    let key = AccountKey { id: who.account.id, uid: who.account.uid };
    // The switch is looked at again here: it may have gone off between the gate and the upgrade.
    let mut features = app.features.subscribe();
    if !features.borrow_and_update().remote {
        close_bounded(&mut transport, CLOSE_GOING_AWAY).await;
        return;
    }
    let attached = tokio::select! {
        () = shutdown.wait() => Err(CLOSE_GOING_AWAY),
        attached = timeout(ATTACH_BOUND, supervisor.attach_device(key)) => match attached {
            Ok(Ok(halves)) => Ok(halves),
            Ok(Err(AttachError::ShuttingDown)) => Err(CLOSE_GOING_AWAY),
            Ok(Err(AttachError::TooManySockets | AttachError::Launch(_))) | Err(_) => Err(CLOSE_BUSY),
        },
    };
    let (handle, mut rx) = match attached {
        Ok(halves) => halves,
        Err(code) => {
            close_bounded(&mut transport, code).await;
            return;
        }
    };
    log::event(
        Level::Info,
        "device_socket.attached",
        &[("account", &who.account.id.to_string()), ("device", &who.device.id.to_string())],
    );
    let started = Instant::now();
    let mut machine = Machine {
        app: &app,
        settings: &settings,
        handle: &handle,
        shutdown: &shutdown,
        ended: app.session_manager.subscribe_ended(),
        features,
        device: who.device.id,
        account: who.account.id,
        token: who.token_hash,
        bucket: Bucket::new(settings.per_second, settings.burst, started),
        budget: Budget::new(settings.violations, settings.violation_window),
        refusals: 0,
        attached: HashSet::new(),
        pending: HashMap::new(),
        synthetic: 0,
        requests: 0,
    };
    let (code, requests) = match drive(&mut machine, &mut transport, &mut rx, started).await {
        Exit::Close(code) => {
            close_bounded(&mut transport, code).await;
            (code, machine.requests)
        }
        Exit::Gone => (0, machine.requests),
    };
    log::event(
        Level::Info,
        "device_socket.closed",
        &[
            ("account", &who.account.id.to_string()),
            ("device", &who.device.id.to_string()),
            ("code", &code.to_string()),
            ("requests", &requests.to_string()),
        ],
    );
    // The receiver and the handle drop here: the socket leaves its worker and its queue is freed.
}

async fn close_bounded<T: Transport>(transport: &mut T, code: u16) {
    let _ = timeout(CLOSE_BOUND, transport.close(code, close_word(code))).await;
}

// ---------------------------------------------------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------------------------------------------------

/// Why a socket loop ended.
enum Exit {
    /// Close with this code.
    Close(u16),
    /// The transport is gone; nothing more can be written.
    Gone,
}

/// What a request that went to the worker was, so that its answer can be turned into text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    Attach(u32),
    New,
    Other,
}

struct Machine<'a> {
    app: &'a Arc<App>,
    settings: &'a SocketSettings,
    handle: &'a SocketHandle,
    shutdown: &'a Shutdown,
    ended: broadcast::Receiver<Ended>,
    features: watch::Receiver<Switches>,
    device: i64,
    account: AccountId,
    token: TokenHash,
    bucket: Bucket,
    budget: Budget,
    refusals: u32,
    /// The sessions this socket is attached to, as the worker confirmed.
    attached: HashSet<u32>,
    /// The requests that wait for the worker's answer, by the number the client gave.
    pending: HashMap<u64, Pending>,
    /// The numbers of the requests this loop makes up itself, counted from the supervisor's range.
    synthetic: u64,
    /// The requests read, for the log.
    requests: u64,
}

fn view(state: WriterState) -> WriterView {
    match state {
        WriterState::Writing => WriterView::Writing,
        WriterState::Watching => WriterView::Watching,
        WriterState::Free => WriterView::Free,
    }
}

fn word(error: crate::worker::sessions::SesError) -> ErrorWord {
    use crate::worker::sessions::SesError;
    match error {
        SesError::NotFound => ErrorWord::NotFound,
        SesError::Refused => ErrorWord::Refused,
        SesError::Busy => ErrorWord::Busy,
        SesError::TooMany => ErrorWord::TooMany,
        SesError::BadRequest => ErrorWord::BadRequest,
    }
}

fn answer_text(id: u64, result: Result<Answer, ErrorWord>) -> String {
    Reply::Answer { id, result }.encode()
}

impl Machine<'_> {
    /// Resolves when the token ends, the remote switch goes off, or the server stops.
    async fn interrupt(&mut self) -> Exit {
        loop {
            tokio::select! {
                () = self.shutdown.wait() => return Exit::Close(CLOSE_GOING_AWAY),
                told = self.ended.recv() => match told {
                    Ok(ended) if ended.ends_device(self.device, self.account) => return Exit::Close(CLOSE_REVOKED),
                    Ok(_) => {}
                    // Some announcements were missed: the token is looked at again, and the answer decides.
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if !self.token_alive().await {
                            return Exit::Close(CLOSE_REVOKED);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
                },
                changed = self.features.changed() => match changed {
                    Ok(()) => {
                        if !self.features.borrow_and_update().remote {
                            return Exit::Close(CLOSE_GOING_AWAY);
                        }
                    }
                    Err(_) => std::future::pending::<()>().await,
                },
            }
        }
    }

    /// Whether the device is still a device of an active account with a token that has not ended; the socket's own activity
    /// counts as the device being seen (at most once a minute, as for every other request). A store that cannot be read says yes:
    /// what cannot be confirmed as gone is not closed on that account.
    async fn token_alive(&self) -> bool {
        let app = self.app.clone();
        let hash = self.token;
        tokio::task::spawn_blocking(move || {
            let now = app.clock.unix();
            let found = app.store.read(|tx| tx.device_lookup(&hash, now));
            match found {
                Ok(Some(device)) => {
                    let idle = i64::try_from(app.devices.idle.as_secs()).unwrap_or(i64::MAX);
                    let _ = app.store.write(|tx| tx.device_touch(device.id, now, idle));
                    true
                }
                Ok(None) => false,
                Err(_) => true,
            }
        })
        .await
        .unwrap_or(true)
    }

    /// Writes one message; a write that makes no progress for the stall time closes the socket, and an end of the token, the
    /// switch or the server interrupts it.
    async fn write<T: Transport>(&mut self, transport: &mut T, message: Outgoing) -> Result<(), Exit> {
        let stall = self.settings.stall;
        tokio::select! {
            biased;
            exit = self.interrupt() => Err(exit),
            sent = timeout(stall, transport.send(message)) => match sent {
                Ok(Ok(())) => Ok(()),
                Ok(Err(_)) => Err(Exit::Gone),
                Err(_) => Err(Exit::Close(CLOSE_BUSY)),
            },
        }
    }

    async fn text<T: Transport>(&mut self, transport: &mut T, text: String) -> Result<(), Exit> {
        self.write(transport, Outgoing::Text(text)).await
    }

    async fn error<T: Transport>(&mut self, transport: &mut T, id: u64, error: ErrorWord) -> Result<(), Exit> {
        self.text(transport, answer_text(id, Err(error))).await
    }

    fn violation(&mut self, now: Instant) -> Result<(), Exit> {
        if self.budget.record(now) { Err(Exit::Close(CLOSE_POLICY)) } else { Ok(()) }
    }

    /// Takes a token of the rate bucket for one message; `false` when the message is over the rate (and the socket is closed
    /// when too many were in a row).
    fn rate(&mut self, now: Instant) -> Result<bool, Exit> {
        if self.bucket.take(now) {
            self.refusals = 0;
            return Ok(true);
        }
        self.refusals = self.refusals.saturating_add(1);
        if self.refusals >= self.settings.refusals { Err(Exit::Close(CLOSE_POLICY)) } else { Ok(false) }
    }

    /// The sessions that count against the cap: those confirmed and those a request waits to confirm.
    fn counted(&self) -> usize {
        let waiting = self.pending.values().filter(|p| matches!(p, Pending::Attach(_) | Pending::New)).count();
        self.attached.len() + waiting
    }

    /// Sends a request about a session to the worker and remembers what it was. A refusal of the supervisor is answered with its
    /// word at once.
    async fn forward<T: Transport>(
        &mut self,
        transport: &mut T,
        id: u64,
        kind: Pending,
        ses: Ses,
    ) -> Result<(), Exit> {
        if self.pending.len() >= MAX_PENDING {
            return self.error(transport, id, ErrorWord::Busy).await;
        }
        self.pending.insert(id, kind);
        match self.handle.ses(ses) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.pending.remove(&id);
                match error {
                    SubmitError::Busy => self.error(transport, id, ErrorWord::Busy).await,
                    // The worker or the socket is gone: the queue delivers the close.
                    SubmitError::Closed => Ok(()),
                    SubmitError::TooLarge | SubmitError::BadCommand | SubmitError::BadRequest => {
                        self.error(transport, id, ErrorWord::BadRequest).await
                    }
                }
            }
        }
    }

    /// One text frame.
    async fn request<T: Transport>(&mut self, transport: &mut T, text: String) -> Result<(), Exit> {
        if text.len() > MAX_TEXT {
            return Err(Exit::Close(CLOSE_TOO_LARGE));
        }
        let now = Instant::now();
        if !self.rate(now)? {
            let id = Call::parse(&text).map_or_else(|rejected| rejected.id, |call| call.id);
            return if id == 0 { Ok(()) } else { self.error(transport, id, ErrorWord::Busy).await };
        }
        self.requests = self.requests.saturating_add(1);
        let call = match Call::parse(&text) {
            Ok(call) => call,
            Err(rejected) => {
                self.violation(now)?;
                return self.error(transport, rejected.id, ErrorWord::BadRequest).await;
            }
        };
        let socket = self.handle.id();
        let id = call.id;
        // A number that a request still waits under is refused: a second one would take the first one's place, so the answer
        // of one of them would be lost, and requests sent under one number would count against the cap of attachments once.
        if self.pending.contains_key(&id) {
            return self.error(transport, id, ErrorWord::BadRequest).await;
        }
        match call.op {
            Op::List => self.forward(transport, id, Pending::Other, Ses::List { socket, request: id }).await,
            Op::New { cols, rows } => {
                if self.counted() >= MAX_ATTACHED {
                    return self.error(transport, id, ErrorWord::TooMany).await;
                }
                self.forward(transport, id, Pending::New, Ses::New { socket, request: id, cols, rows }).await
            }
            Op::Attach { session, tail } => {
                if !self.attached.contains(&session) && self.counted() >= MAX_ATTACHED {
                    return self.error(transport, id, ErrorWord::TooMany).await;
                }
                self.forward(transport, id, Pending::Attach(session), Ses::Attach { socket, request: id, session, tail })
                    .await
            }
            Op::Detach { session } => {
                self.attached.remove(&session);
                self.forward(transport, id, Pending::Other, Ses::Detach { socket, request: id, session }).await
            }
            Op::Resize { session, cols, rows } => {
                self.forward(transport, id, Pending::Other, Ses::Resize { socket, request: id, session, cols, rows }).await
            }
            Op::Takeover { session } => {
                self.forward(transport, id, Pending::Other, Ses::Takeover { socket, request: id, session }).await
            }
            Op::End { session } => {
                self.forward(transport, id, Pending::Other, Ses::End { socket, request: id, session }).await
            }
            Op::Beat { session: Some(session) } => {
                if !self.attached.contains(&session) {
                    return self.error(transport, id, ErrorWord::NotFound).await;
                }
                self.forward(transport, id, Pending::Other, Ses::Beat { socket, request: id, session }).await
            }
            Op::Beat { session: None } => {
                // A beat is a sign of life for every session of the socket. It is answered here and the worker's answers to
                // the beats are not wanted: they carry numbers of the supervisor's range and are dropped on the way back.
                let sessions: Vec<u32> = self.attached.iter().copied().collect();
                for session in sessions {
                    self.synthetic += 1;
                    let request = SYNTHETIC_REQUEST_BASE + self.synthetic;
                    let _ = self.handle.ses(Ses::Beat { socket, request, session });
                }
                self.text(transport, answer_text(id, Ok(Answer::Done))).await
            }
        }
    }

    /// One binary frame: input for a session. A frame of another kind, for a session this socket is not attached to, or over the
    /// rate is dropped without an answer: input is a stream, and the worker drops what a watcher sends anyway.
    fn input(&mut self, bytes: &[u8]) -> Result<(), Exit> {
        let now = Instant::now();
        if !self.rate(now)? {
            return Ok(());
        }
        let Ok(frame) = Input::parse(bytes) else { return Ok(()) };
        if !self.attached.contains(&frame.session) {
            return Ok(());
        }
        let ses = Ses::Input { socket: self.handle.id(), session: frame.session, bytes: frame.bytes.to_vec() };
        // A worker that is behind drops the input; a socket that is gone is closed by its queue.
        let _ = self.handle.ses(ses);
        Ok(())
    }

    /// What the worker said to this socket.
    async fn outbound<T: Transport>(&mut self, transport: &mut T, ses: Ses) -> Result<(), Exit> {
        match ses {
            Ses::Answer { request, answer, .. } => {
                let Some(kind) = self.pending.remove(&request) else { return Ok(()) };
                let result = match answer {
                    SesAnswer::Listed(list) => Ok(Answer::List(
                        list.into_iter()
                            .map(|info| SessionEntry {
                                id: info.id,
                                title: info.title,
                                cwd: info.cwd,
                                cols: info.cols,
                                rows: info.rows,
                                started: info.started,
                                attached: info.attached,
                                writer_held: info.writer_held,
                            })
                            .collect(),
                    )),
                    SesAnswer::Created { session } => {
                        self.attached.insert(session);
                        // The creator is the writer; the answer says so.
                        Ok(Answer::Created { session, writer: WriterView::Writing })
                    }
                    SesAnswer::Attached(a) => {
                        if let Pending::Attach(session) = kind {
                            self.attached.insert(session);
                            Ok(Answer::Attached {
                                session,
                                cols: a.cols,
                                rows: a.rows,
                                writer: view(a.writer),
                                more: a.more,
                                snapshot: a.snapshot_bytes,
                            })
                        } else {
                            Err(ErrorWord::BadRequest)
                        }
                    }
                    SesAnswer::Done | SesAnswer::Ended => Ok(Answer::Done),
                    SesAnswer::Failed(error) => Err(word(error)),
                };
                self.text(transport, answer_text(request, result)).await
            }
            Ses::Event { session, event, .. } => {
                let event = match event {
                    SesEvent::Writer(state) => Event::Writer { session, state: view(state) },
                    SesEvent::Exited(code) => Event::Exited { session, code },
                    SesEvent::Resync => {
                        // The stream is gone and the client attaches again; until it does the session no longer counts.
                        self.attached.remove(&session);
                        Event::Resync { session }
                    }
                    SesEvent::Size { cols, rows } => Event::Size { session, cols, rows },
                };
                self.text(transport, Reply::Event(event).encode()).await
            }
            Ses::Snapshot { session, seq, bytes, .. } => self.chunk(transport, Stream::Snapshot, session, seq, &bytes).await,
            Ses::Live { session, seq, bytes, .. } => self.chunk(transport, Stream::Live, session, seq, &bytes).await,
            // A request is the supervisor's to send and never reaches a socket.
            _ => Ok(()),
        }
    }

    /// A chunk of the screen or of the program's output, as the binary frame of the vocabulary. A chunk the vocabulary cannot
    /// carry (the worker never makes one) is dropped.
    async fn chunk<T: Transport>(
        &mut self,
        transport: &mut T,
        stream: Stream,
        session: u32,
        seq: u32,
        bytes: &[u8],
    ) -> Result<(), Exit> {
        match proto::encode_chunk(stream, session, seq, bytes) {
            Ok(frame) => self.write(transport, Outgoing::Binary(frame)).await,
            Err(_) => Ok(()),
        }
    }
}

async fn drive<T: Transport>(m: &mut Machine<'_>, transport: &mut T, rx: &mut SocketRx, started: Instant) -> Exit {
    let settings = *m.settings;
    let mut ping = interval_at(started + settings.ping, settings.ping);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut recheck = interval_at(started + settings.recheck, settings.recheck);
    recheck.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_inbound = started;

    loop {
        let silent_at = last_inbound + settings.silence;
        let step: Result<(), Exit> = tokio::select! {
            biased;
            exit = m.interrupt() => Err(exit),
            () = sleep_until(silent_at) => Err(Exit::Close(CLOSE_GOING_AWAY)),
            frame = transport.recv() => {
                last_inbound = Instant::now();
                match frame {
                    Ok(Inbound::Text(text)) => m.request(transport, text).await,
                    Ok(Inbound::Bytes(bytes)) => m.input(&bytes),
                    Ok(Inbound::Binary(_)) => Err(Exit::Close(CLOSE_TYPE)),
                    Ok(Inbound::Pong) => Ok(()),
                    Err(TransportError::TooLarge) => Err(Exit::Close(CLOSE_TOO_LARGE)),
                    Err(TransportError::Encoding) => Err(Exit::Close(CLOSE_ENCODING)),
                    Err(TransportError::Protocol) => Err(Exit::Close(CLOSE_PROTOCOL)),
                    Err(TransportError::Closed) => Err(Exit::Gone),
                }
            },
            out = rx.recv() => match out {
                Some(Outbound::Ses(ses)) => m.outbound(transport, *ses).await,
                Some(Outbound::Close { code, .. }) => Err(Exit::Close(code)),
                // Nothing else is ever queued for a device socket.
                Some(Outbound::Text(_) | Outbound::Binary(_)) => Ok(()),
                None => Err(Exit::Close(CLOSE_GOING_AWAY)),
            },
            _ = ping.tick() => {
                let stall = settings.stall;
                match timeout(stall, transport.ping()).await {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(_)) => Err(Exit::Gone),
                    Err(_) => Err(Exit::Close(CLOSE_BUSY)),
                }
            },
            _ = recheck.tick() => {
                if m.token_alive().await { Ok(()) } else { Err(Exit::Close(CLOSE_REVOKED)) }
            },
        };
        if let Err(exit) = step {
            return exit;
        }
    }
}
