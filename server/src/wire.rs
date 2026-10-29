//! The frames between the supervisor and one worker, over the Unix socket pair that joins them.
//!
//! # Framing
//!
//! Every frame is a 4-byte big-endian length `n` and then `n` bytes of payload. The length is at least 1 and at most
//! [`MAX_PAYLOAD`] (16 MiB plus 64 bytes: the largest command a page may send plus the fixed head of a frame). The
//! worker reads and writes the layout with blocking [`std::io`] ([`read_frame`], [`write_frame`]); the supervisor uses the
//! same layout with the tokio codec that [`codec`] returns. The first payload byte is the kind. All numbers are big
//! endian. A decoder is strict: the lengths must be exact, nothing may follow the last field, an unknown kind or
//! operation is an error, text must be UTF-8, and no error carries a byte of the frame.
//!
//! # Layout of the payload
//!
//! Offsets count from the first payload byte (the kind).
//!
//! ```text
//! 0x01 Cmd   supervisor -> worker
//!   0      u8    kind = 1
//!   1..5   u32   socket id of the browser connection that sent the command (not 0)
//!   5..13  u64   request id (the page's own number; the answer carries it back)
//!   13..   text  JSON {"cmd":"<name>","args":{...}}, UTF-8, to the end of the frame
//!
//! 0x02 Res   worker -> supervisor, exactly one for every Cmd
//!   0      u8    kind = 2
//!   1..5   u32   socket id of the Cmd (not 0)
//!   5..13  u64   request id of the Cmd
//!   13     u8    status: 0 the result, 1 the error value, 2 a binary result
//!   14..   body  status 0 and 1: the JSON text of the value the page gets, UTF-8; status 2: the bytes
//!
//! 0x03 Evt   worker -> supervisor
//!   0      u8    kind = 3
//!   1..5   u32   target socket id; 0 is every attached socket of the person
//!   5      u8    name length n, 1 to 255
//!   6..6+n text  event name: lower-case ASCII letters, '-' and '_'
//!   6+n..  text  the final JSON text of the payload, UTF-8, to the end of the frame
//!
//! 0x04 Ctl   supervisor -> worker
//!   0      u8    kind = 4
//!   1      u8    operation
//!   1 Init          2..4 u16 protocol version (1) | 4..8 u32 most panes, 0 for no cap | 8..12 u32 most scrollback lines,
//!                   0 for no cap                                                                      (12 bytes)
//!   2 Attach        2..6 u32 socket id (not 0)                                                         (6 bytes)
//!   3 Detach        2..6 u32 socket id (not 0)                                                         (6 bytes)
//!   4 Unlock        2..10 u64 control id | 10 u8 key kind | 11..43 the key                             (43 bytes)
//!   5 Lock          2..10 u64 control id                                                               (10 bytes)
//!   6 Shutdown      nothing                                                                            (2 bytes)
//!   7 ResetSecrets  2..10 u64 control id                                                               (10 bytes)
//!   8 to 11         reserved for the file routes (upload and file serving); a decoder refuses them
//!   12 Provision    2..10 u64 control id | 10 u8 count 0 to 4 | 11.. count slot keys of 49 bytes       (11 + 49 n bytes)
//!   13 SetSlot      2..10 u64 control id | 10..59 slot key | 59 u8 opening key 0 or 1 | 60 u8 kind | 61..93 key
//!                   (the last two only when the opening key is 1)                                      (60 or 93 bytes)
//!   14 RemoveSlot   2..10 u64 control id | 10 u8 key kind | 11..27 slot id                             (27 bytes)
//!   A slot key is: u8 key kind | 16 bytes slot id | 32 bytes key. A key kind is 1 password, 2 passphrase, 3 prf,
//!   4 recovery. The control id is chosen by the supervisor; the answer to a control that has one is a `CtlDone` note
//!   with the same id.
//!
//! 0x05 Note  worker -> supervisor
//!   0      u8    kind = 5
//!   1      u8    operation
//!   1 Ready         nothing                                                                            (2 bytes)
//!   2 StoreState    2 u8 state: 0 absent, 1 locked, 2 unlocked                                         (3 bytes)
//!   3 PaneCount     2..6 u32 panes in all windows                                                      (6 bytes)
//!   4 Exiting       2 u8 reason: 0 clean, 1 failure                                                    (3 bytes)
//!   5 CtlDone       2..10 u64 control id | 10 u8 outcome: 0 done, 1 locked, 2 refused, 3 failed, 4 absent (11 bytes)
//! ```
//!
//! A frame that holds a key prints nothing of it in `Debug`, and the key is wiped from the frame when it is dropped.

use std::fmt;
use std::io::{self, Read, Write};

use tokio_util::codec::LengthDelimitedCodec;
use zeroize::Zeroizing;

/// The most bytes of payload a frame carries: the largest command message (8 MiB for a paste, doubled for the JSON
/// escaping of a document) is below 16 MiB, plus room for the fixed head.
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024 + 64;

/// The kind byte of a command.
pub const KIND_CMD: u8 = 1;
/// The kind byte of an answer.
pub const KIND_RES: u8 = 2;
/// The kind byte of an event.
pub const KIND_EVT: u8 = 3;
/// The kind byte of a control.
pub const KIND_CTL: u8 = 4;
/// The kind byte of a note.
pub const KIND_NOTE: u8 = 5;

/// The protocol version of the `Init` control.
pub const PROTOCOL_VERSION: u16 = 1;

/// Why a frame could not be read or written. None of the texts holds a byte of the frame.
#[derive(Debug)]
pub enum WireError {
    /// The stream failed.
    Io(io::Error),
    /// A frame with no payload.
    Empty,
    /// The length is over [`MAX_PAYLOAD`].
    TooLarge(usize),
    /// The stream ended inside a frame, or a field is cut off.
    Truncated,
    /// Bytes follow the last field.
    Trailing,
    /// The kind byte is not one of the five.
    UnknownKind(u8),
    /// The operation byte of a control or note is not one the decoder knows.
    UnknownOp { kind: u8, op: u8 },
    /// The operation number is reserved for the file routes.
    ReservedOp(u8),
    /// Text that is not UTF-8.
    BadUtf8,
    /// An event name that is empty or has a character outside lower-case ASCII, `-` and `_`.
    BadName,
    /// A socket id of 0 where a socket is required.
    BadSocket,
    /// A value that is out of range.
    BadValue(&'static str),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Io(e) => write!(f, "the link failed: {}", e.kind()),
            WireError::Empty => f.write_str("a frame has no payload"),
            WireError::TooLarge(len) => write!(f, "a frame of {len} bytes is over the limit of {MAX_PAYLOAD}"),
            WireError::Truncated => f.write_str("a frame is cut off"),
            WireError::Trailing => f.write_str("a frame has bytes after its last field"),
            WireError::UnknownKind(kind) => write!(f, "frame kind {kind} is unknown"),
            WireError::UnknownOp { kind, op } => write!(f, "operation {op} of frame kind {kind} is unknown"),
            WireError::ReservedOp(op) => write!(f, "operation {op} is reserved"),
            WireError::BadUtf8 => f.write_str("a text field is not UTF-8"),
            WireError::BadName => f.write_str("an event name is not lower-case ASCII with '-' and '_'"),
            WireError::BadSocket => f.write_str("a socket id is 0"),
            WireError::BadValue(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for WireError {}

impl From<io::Error> for WireError {
    fn from(e: io::Error) -> Self {
        WireError::Io(e)
    }
}

impl From<WireError> for io::Error {
    fn from(e: WireError) -> Self {
        match e {
            WireError::Io(e) => e,
            other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
        }
    }
}

/// A command from a browser connection: the JSON text of `{"cmd","args"}` is read by the dispatcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmdFrame {
    pub socket: u32,
    pub request: u64,
    pub json: String,
}

/// The body of an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResBody {
    /// Status 0: the JSON text of the result.
    Ok(String),
    /// Status 1: the JSON text of the error value.
    Err(String),
    /// Status 2: a binary result.
    Binary(Vec<u8>),
}

/// The answer to a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Res {
    pub socket: u32,
    pub request: u64,
    pub body: ResBody,
}

/// An event for one socket or for all of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evt {
    /// The socket it is for; 0 is every attached socket.
    pub target: u32,
    pub name: String,
    /// The final JSON text of the payload.
    pub text: String,
}

/// A control from the supervisor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ctl {
    /// The first frame: the protocol version and the caps of this person (0 is no cap).
    Init { version: u16, max_panes: u32, max_scrollback_lines: u32 },
    /// A browser connection is attached: it gets the replay of titles and working directories.
    Attach { socket: u32 },
}

/// A note from the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// The core is built and the worker answers commands.
    Ready,
}

/// One frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Cmd(CmdFrame),
    Res(Res),
    Evt(Evt),
    Ctl(Ctl),
    Note(Note),
}

impl Frame {
    /// The payload of this frame (without the length prefix).
    pub fn to_payload(&self) -> Result<Zeroizing<Vec<u8>>, WireError> {
        Err(WireError::Empty)
    }

    /// The frame in `payload` (without the length prefix).
    pub fn from_payload(_payload: &[u8]) -> Result<Frame, WireError> {
        Err(WireError::Empty)
    }
}

/// Reads one frame, blocking. `Ok(None)` is the end of the stream between two frames.
pub fn read_frame<R: Read>(_reader: &mut R) -> Result<Option<Frame>, WireError> {
    Err(WireError::Empty)
}

/// Writes one frame: the length prefix and the payload, in one `write_all`.
pub fn write_frame<W: Write>(_writer: &mut W, _frame: &Frame) -> Result<(), WireError> {
    Err(WireError::Empty)
}

/// The tokio codec for the same layout, for the supervisor's side of the socket.
pub fn codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder().big_endian().length_field_length(4).max_frame_length(MAX_PAYLOAD).new_codec()
}
