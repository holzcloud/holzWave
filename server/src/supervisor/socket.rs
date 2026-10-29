//! The queue of one browser socket, the messages that leave for it, and the handle and receiver the endpoint holds.
//!
//! Every attached socket has its own [`Queue`]: the reader of a worker pushes to it and never waits, and the endpoint's
//! writer takes from it through [`SocketRx::recv`]. A socket that is slow therefore only fills its own queue.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::Notify;

/// Something that leaves for a browser socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outbound {
    /// A text message: an answer or an event, already in the form the page reads.
    Text(String),
    /// A binary message: eight bytes of the request number, big endian, then the bytes of the answer.
    Binary(Vec<u8>),
    /// The socket is to be closed with this code. Nothing follows it.
    Close { code: u16, reason: &'static str },
}

impl Outbound {
    /// The bytes this message holds while it waits.
    fn cost(&self) -> usize {
        match self {
            Outbound::Text(text) => text.len(),
            Outbound::Binary(bytes) => bytes.len(),
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

struct QueueState {
    items: VecDeque<Outbound>,
    bytes: usize,
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
}

impl Queue {
    pub(super) fn new() -> Queue {
        Queue {
            state: Mutex::new(QueueState {
                items: VecDeque::new(),
                bytes: 0,
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

    /// Queues `message`. `droppable` marks a frame event.
    pub(super) fn push(&self, message: Outbound, _droppable: bool) -> Push {
        let mut state = self.lock();
        if state.closed || state.abandoned {
            return Push::Gone;
        }
        state.bytes += message.cost();
        state.items.push_back(message);
        drop(state);
        self.ready.notify_one();
        Push::Queued
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
    detached: AtomicBool,
}

impl SocketCore {
    pub(super) fn new(id: u32) -> SocketCore {
        SocketCore { id, queue: Queue::new(), detached: AtomicBool::new(false) }
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
