//! The queue of one browser socket, the messages that leave for it, and the handle and receiver the endpoint holds.
//!
//! Every attached socket has its own [`Queue`]: the reader of a worker pushes to it and never waits, and the endpoint's
//! writer takes from it through [`SocketRx::recv`]. A socket that is slow therefore only fills its own queue.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::Notify;

use crate::wire::{Ses, SesAnswer, SesEvent};

/// The close code for a socket whose queue overflowed.
pub const CLOSE_OVERFLOW: u16 = 1013;

/// Something that leaves for a browser socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outbound {
    /// A text message: an answer or an event, already in the form the page reads.
    Text(String),
    /// A binary message: eight bytes of the request number, big endian, then the bytes of the answer.
    Binary(Vec<u8>),
    /// A frame about a session of a device socket: an answer, an event, or a chunk of the screen or of the program's output. Only
    /// a device socket receives it and only a device socket's loop reads it.
    Ses(Box<Ses>),
    /// The socket is to be closed with this code. Nothing follows it.
    Close { code: u16, reason: &'static str },
}

/// The bytes a queued message is counted as. A fixed head stands for the structure around the bytes it carries.
const FRAME_HEAD: usize = 32;

impl Outbound {
    /// The bytes this message holds while it waits.
    fn cost(&self) -> usize {
        match self {
            Outbound::Text(text) => text.len(),
            Outbound::Binary(bytes) => bytes.len(),
            Outbound::Ses(ses) => match &**ses {
                Ses::Snapshot { bytes, .. } | Ses::Live { bytes, .. } => FRAME_HEAD + bytes.len(),
                Ses::Answer { answer: SesAnswer::Listed(list), .. } => {
                    FRAME_HEAD + list.iter().map(|info| 48 + info.title.len() + info.cwd.len()).sum::<usize>()
                }
                _ => FRAME_HEAD,
            },
            Outbound::Close { .. } => 0,
        }
    }
}

/// Why a command was not queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// The command name is not 1 to 64 characters of lower-case letters, digits and `_`.
    BadCommand,
    /// The request number is in the range the supervisor keeps for itself.
    BadRequest,
    /// The command does not fit in a frame.
    TooLarge,
    /// The worker has too many frames waiting.
    Busy,
    /// The socket or its worker is gone.
    Closed,
}

/// What a queue did with a chunk of a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StreamPush {
    /// It is queued.
    Queued,
    /// It did not fit: the stream of its session ended for this socket, and a `resync` is queued. The worker is to be told to
    /// let the socket go.
    Lost,
    /// The stream of its session ended before; the chunk is dropped.
    Dropped,
    /// The queue was closed already.
    Gone,
}

fn is_stream_of(ses: &Ses, session: u32) -> bool {
    matches!(ses, Ses::Snapshot { session: s, .. } | Ses::Live { session: s, .. } if *s == session)
}

/// What a message queue did with a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Push {
    /// It is queued.
    Queued,
    /// It was a frame event that did not fit and was dropped.
    DroppedFrame,
    /// It did not fit; the queue is closed, with a close message as the last one.
    Overflow,
    /// The queue was closed already; the message is gone.
    Gone,
}

/// The sessions whose stream was ended for lack of room, at most this many remembered per socket (the most sessions a worker
/// holds).
const LOST_MOST: usize = 1024;

struct QueueState {
    items: VecDeque<Outbound>,
    bytes: usize,
    /// The sessions whose stream this socket lost: what arrives for them is dropped until the socket attaches again.
    lost: Vec<u32>,
    /// A close message is queued or was taken: nothing is accepted any more.
    closed: bool,
    /// The receiver is gone.
    abandoned: bool,
    /// The close message was taken.
    finished: bool,
}

/// The messages waiting for one socket, counted in bytes.
pub(super) struct Queue {
    state: Mutex<QueueState>,
    ready: Notify,
    /// The bytes that may wait.
    cap: usize,
}

impl Queue {
    pub(super) fn new(cap: usize) -> Queue {
        Queue {
            cap,
            state: Mutex::new(QueueState {
                items: VecDeque::new(),
                bytes: 0,
                lost: Vec::new(),
                closed: false,
                abandoned: false,
                finished: false,
            }),
            ready: Notify::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `message`. `droppable` marks a frame event, which is dropped when it does not fit; any other message that does
    /// not fit closes the queue with 1013. A message larger than the whole cap fits only an empty queue.
    pub(super) fn push(&self, message: Outbound, droppable: bool) -> Push {
        let mut state = self.lock();
        if state.closed || state.abandoned {
            return Push::Gone;
        }
        let cost = message.cost();
        let fits = state.bytes + cost <= self.cap || (cost > self.cap && state.items.is_empty());
        if fits {
            state.bytes += cost;
            state.items.push_back(message);
            drop(state);
            self.ready.notify_one();
            return Push::Queued;
        }
        if droppable {
            return Push::DroppedFrame;
        }
        state.closed = true;
        state.items.push_back(Outbound::Close { code: CLOSE_OVERFLOW, reason: "too many messages waiting" });
        drop(state);
        self.ready.notify_one();
        Push::Overflow
    }

    /// Queues a chunk of the stream of `session`. A chunk that does not fit within three quarters of the queue (the rest is for
    /// answers and events) ends this stream and nothing else: what is queued of it is dropped, one `resync` is queued with no
    /// regard for the size (there is at most one per session until the socket attaches again), and what arrives for the session
    /// is dropped. The socket and the session go on. An empty queue always takes one chunk.
    pub(super) fn push_stream(&self, socket: u32, session: u32, message: Outbound) -> StreamPush {
        let mut state = self.lock();
        if state.closed || state.abandoned {
            return StreamPush::Gone;
        }
        if state.lost.contains(&session) {
            return StreamPush::Dropped;
        }
        let cost = message.cost();
        let stream_cap = self.cap - self.cap / 4;
        if state.bytes + cost <= stream_cap || state.items.is_empty() {
            state.bytes += cost;
            state.items.push_back(message);
            drop(state);
            self.ready.notify_one();
            return StreamPush::Queued;
        }
        let mut freed = 0;
        state.items.retain(|item| match item {
            Outbound::Ses(ses) if is_stream_of(ses, session) => {
                freed += item.cost();
                false
            }
            _ => true,
        });
        state.bytes -= freed;
        if state.lost.len() >= LOST_MOST {
            state.lost.remove(0);
        }
        state.lost.push(session);
        let resync = Outbound::Ses(Box::new(Ses::Event { socket, session, event: SesEvent::Resync }));
        state.bytes += resync.cost();
        state.items.push_back(resync);
        drop(state);
        self.ready.notify_one();
        StreamPush::Lost
    }

    /// The socket attaches to `session` or leaves it: what the queue remembers about its lost stream is forgotten.
    pub(super) fn forget_lost(&self, session: u32) {
        self.lock().lost.retain(|lost| *lost != session);
    }

    /// Queues a close message, whatever the size of the queue, and accepts nothing after it.
    pub(super) fn close(&self, code: u16, reason: &'static str) {
        let mut state = self.lock();
        if state.closed || state.abandoned {
            return;
        }
        state.closed = true;
        state.items.push_back(Outbound::Close { code, reason });
        drop(state);
        self.ready.notify_one();
    }

    /// The receiver is gone: the queue is emptied and accepts nothing.
    pub(super) fn abandon(&self) {
        let mut state = self.lock();
        state.abandoned = true;
        state.closed = true;
        state.items.clear();
        state.bytes = 0;
    }

    /// The bytes waiting.
    pub(super) fn bytes(&self) -> usize {
        self.lock().bytes
    }

    /// The next message, waiting for it. `None` once the close message was taken, or the receiver is gone.
    pub(super) async fn pop(&self) -> Option<Outbound> {
        loop {
            {
                let mut state = self.lock();
                if state.finished || state.abandoned {
                    return None;
                }
                if let Some(message) = state.items.pop_front() {
                    state.bytes -= message.cost();
                    if matches!(message, Outbound::Close { .. }) {
                        state.finished = true;
                    }
                    return Some(message);
                }
            }
            self.ready.notified().await;
        }
    }
}

/// One attached socket as the worker's table holds it.
pub(super) struct SocketCore {
    pub(super) id: u32,
    pub(super) queue: Queue,
    /// Whether this is the socket of a device (sessions and bytes) and not of a page (commands and events).
    pub(super) device: bool,
    detached: AtomicBool,
}

impl SocketCore {
    pub(super) fn new(id: u32, queue_bytes: usize, device: bool) -> SocketCore {
        SocketCore { id, queue: Queue::new(queue_bytes), device, detached: AtomicBool::new(false) }
    }

    /// True the first time it is asked, so that the socket leaves its worker once.
    fn first_detach(&self) -> bool {
        !self.detached.swap(true, Ordering::SeqCst)
    }
}

/// What the endpoint's handle and receiver call on the worker that holds the socket.
pub(super) trait Attached: Send + Sync {
    /// Queues a command of `socket`.
    fn submit(&self, socket: u32, request: u64, command: &str, args: &str) -> Result<(), SubmitError>;
    /// Queues a request about the sessions of a worker for `socket`, which must be the socket of a device.
    fn ses(&self, socket: u32, ses: Ses) -> Result<(), SubmitError>;
    /// The socket left.
    fn detach(&self, socket: u32);
}

/// The sending half of an attached socket. Dropping it detaches the socket.
pub struct SocketHandle {
    core: Arc<SocketCore>,
    worker: Arc<dyn Attached>,
}

impl SocketHandle {
    /// The number of this socket, local to its worker; never 0.
    pub fn id(&self) -> u32 {
        self.core.id
    }

    /// Queues the command `command` with the argument text `args` as request number `request`. The arguments are not
    /// read here: the worker parses them.
    pub fn submit(&self, request: u64, command: &str, args: &str) -> Result<(), SubmitError> {
        self.worker.submit(self.core.id, request, command, args)
    }

    /// Queues a request about a session for the worker. The frame must name this socket, must be one of the requests the
    /// supervisor sends (never `Gone`, which the supervisor itself sends when the socket ends), and the socket must be a device
    /// socket; otherwise it is refused. The worker answers a request with one `Ses::Answer` that carries its number, which
    /// reaches [`SocketRx::recv`] as [`Outbound::Ses`].
    pub fn ses(&self, ses: Ses) -> Result<(), SubmitError> {
        let named = match &ses {
            Ses::List { socket, .. }
            | Ses::New { socket, .. }
            | Ses::Attach { socket, .. }
            | Ses::Detach { socket, .. }
            | Ses::Input { socket, .. }
            | Ses::Resize { socket, .. }
            | Ses::Takeover { socket, .. }
            | Ses::End { socket, .. }
            | Ses::Beat { socket, .. } => *socket,
            _ => return Err(SubmitError::BadCommand),
        };
        if named != self.core.id || !self.core.device {
            return Err(SubmitError::BadCommand);
        }
        match &ses {
            Ses::Attach { session, .. } | Ses::Detach { session, .. } => self.core.queue.forget_lost(*session),
            _ => {}
        }
        self.worker.ses(self.core.id, ses)
    }

    /// Detaches the socket now; the same as dropping the handle.
    pub fn close(self) {}
}

impl Drop for SocketHandle {
    fn drop(&mut self) {
        if self.core.first_detach() {
            self.worker.detach(self.core.id);
        }
    }
}

/// The receiving half of an attached socket. Dropping it frees the socket and its queue.
pub struct SocketRx {
    core: Arc<SocketCore>,
    worker: Arc<dyn Attached>,
}

impl SocketRx {
    /// The next message for the socket, or `None` once the socket is closed and everything is taken.
    pub async fn recv(&mut self) -> Option<Outbound> {
        self.core.queue.pop().await
    }
}

impl Drop for SocketRx {
    fn drop(&mut self) {
        self.core.queue.abandon();
        if self.core.first_detach() {
            self.worker.detach(self.core.id);
        }
    }
}

/// The two halves of a socket that was just attached.
pub(super) fn halves(core: Arc<SocketCore>, worker: Arc<dyn Attached>) -> (SocketHandle, SocketRx) {
    (SocketHandle { core: core.clone(), worker: worker.clone() }, SocketRx { core, worker })
}
