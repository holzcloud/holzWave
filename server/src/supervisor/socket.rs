//! The queue of one browser socket, the messages that leave for it, and the handle and receiver the endpoint holds.

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

/// The sending half of an attached socket.
pub struct SocketHandle {
    _private: (),
}

impl SocketHandle {
    /// The number of this socket, local to its worker; never 0.
    pub fn id(&self) -> u32 {
        0
    }

    /// Queues the command `command` with the argument text `args` as request number `request`.
    pub fn submit(&self, _request: u64, _command: &str, _args: &str) -> Result<(), SubmitError> {
        Err(SubmitError::Closed)
    }
}

/// The receiving half of an attached socket.
pub struct SocketRx {
    _private: (),
}

impl SocketRx {
    /// The next message for the socket, or `None` once the socket is closed and everything is taken.
    pub async fn recv(&mut self) -> Option<Outbound> {
        None
    }
}
