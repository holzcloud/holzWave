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

/// The operation number of `Init`.
pub const OP_INIT: u8 = 1;
/// The operation number of `Attach`.
pub const OP_ATTACH: u8 = 2;

/// The operation number of the note `Ready`.
pub const NOTE_READY: u8 = 1;

/// The protocol version of the `Init` control.
pub const PROTOCOL_VERSION: u16 = 1;

/// How much of a payload is allocated before any of it has arrived.
const FIRST_CHUNK: usize = 64 * 1024;

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

/// Whether `name` is a valid event name: one to 255 lower-case ASCII letters, `-` or `_`.
pub fn valid_event_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 255 && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-' || b == b'_')
}

/// A reader over the bytes of a payload that fails with [`WireError::Truncated`] instead of panicking.
struct Fields<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    fn new(bytes: &'a [u8]) -> Fields<'a> {
        Fields { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        let end = self.at.checked_add(n).ok_or(WireError::Truncated)?;
        let piece = self.bytes.get(self.at..end).ok_or(WireError::Truncated)?;
        self.at = end;
        Ok(piece)
    }

    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, WireError> {
        let bytes: [u8; 2] = self.take(2)?.try_into().map_err(|_| WireError::Truncated)?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        let bytes: [u8; 4] = self.take(4)?.try_into().map_err(|_| WireError::Truncated)?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, WireError> {
        let bytes: [u8; 8] = self.take(8)?.try_into().map_err(|_| WireError::Truncated)?;
        Ok(u64::from_be_bytes(bytes))
    }

    /// A socket id that is not 0.
    fn socket(&mut self) -> Result<u32, WireError> {
        match self.u32()? {
            0 => Err(WireError::BadSocket),
            socket => Ok(socket),
        }
    }

    /// Everything that is left.
    fn rest(&mut self) -> &'a [u8] {
        let rest = &self.bytes[self.at..];
        self.at = self.bytes.len();
        rest
    }

    /// Fails when a byte is left.
    fn finish(&self) -> Result<(), WireError> {
        if self.at == self.bytes.len() { Ok(()) } else { Err(WireError::Trailing) }
    }
}

fn text(bytes: &[u8]) -> Result<String, WireError> {
    std::str::from_utf8(bytes).map(str::to_owned).map_err(|_| WireError::BadUtf8)
}

fn nonzero(socket: u32) -> Result<u32, WireError> {
    if socket == 0 { Err(WireError::BadSocket) } else { Ok(socket) }
}

impl Frame {
    /// Appends the payload of this frame (without the length prefix) to `out`.
    fn encode_into(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        let start = out.len();
        match self {
            Frame::Cmd(cmd) => {
                out.push(KIND_CMD);
                out.extend_from_slice(&nonzero(cmd.socket)?.to_be_bytes());
                out.extend_from_slice(&cmd.request.to_be_bytes());
                out.extend_from_slice(cmd.json.as_bytes());
            }
            Frame::Res(res) => {
                out.push(KIND_RES);
                out.extend_from_slice(&nonzero(res.socket)?.to_be_bytes());
                out.extend_from_slice(&res.request.to_be_bytes());
                match &res.body {
                    ResBody::Ok(body) => {
                        out.push(0);
                        out.extend_from_slice(body.as_bytes());
                    }
                    ResBody::Err(body) => {
                        out.push(1);
                        out.extend_from_slice(body.as_bytes());
                    }
                    ResBody::Binary(body) => {
                        out.push(2);
                        out.extend_from_slice(body);
                    }
                }
            }
            Frame::Evt(evt) => {
                if !valid_event_name(&evt.name) {
                    return Err(WireError::BadName);
                }
                out.push(KIND_EVT);
                out.extend_from_slice(&evt.target.to_be_bytes());
                out.push(u8::try_from(evt.name.len()).map_err(|_| WireError::BadName)?);
                out.extend_from_slice(evt.name.as_bytes());
                out.extend_from_slice(evt.text.as_bytes());
            }
            Frame::Ctl(Ctl::Init { version, max_panes, max_scrollback_lines }) => {
                out.extend_from_slice(&[KIND_CTL, OP_INIT]);
                out.extend_from_slice(&version.to_be_bytes());
                out.extend_from_slice(&max_panes.to_be_bytes());
                out.extend_from_slice(&max_scrollback_lines.to_be_bytes());
            }
            Frame::Ctl(Ctl::Attach { socket }) => {
                out.extend_from_slice(&[KIND_CTL, OP_ATTACH]);
                out.extend_from_slice(&nonzero(*socket)?.to_be_bytes());
            }
            Frame::Note(Note::Ready) => out.extend_from_slice(&[KIND_NOTE, NOTE_READY]),
        }
        if out.len() - start > MAX_PAYLOAD {
            return Err(WireError::TooLarge(out.len() - start));
        }
        Ok(())
    }

    /// The payload of this frame (without the length prefix). The buffer is wiped when it is dropped.
    pub fn to_payload(&self) -> Result<Zeroizing<Vec<u8>>, WireError> {
        let mut out = Zeroizing::new(Vec::new());
        self.encode_into(&mut out)?;
        Ok(out)
    }

    /// The frame in `payload` (without the length prefix).
    pub fn from_payload(payload: &[u8]) -> Result<Frame, WireError> {
        let mut fields = Fields::new(payload);
        if payload.is_empty() {
            return Err(WireError::Empty);
        }
        let kind = fields.u8()?;
        let frame = match kind {
            KIND_CMD => {
                let socket = fields.socket()?;
                let request = fields.u64()?;
                Frame::Cmd(CmdFrame { socket, request, json: text(fields.rest())? })
            }
            KIND_RES => {
                let socket = fields.socket()?;
                let request = fields.u64()?;
                let body = match fields.u8()? {
                    0 => ResBody::Ok(text(fields.rest())?),
                    1 => ResBody::Err(text(fields.rest())?),
                    2 => ResBody::Binary(fields.rest().to_vec()),
                    _ => return Err(WireError::BadValue("the status of an answer is not 0, 1 or 2")),
                };
                Frame::Res(Res { socket, request, body })
            }
            KIND_EVT => {
                let target = fields.u32()?;
                let length = usize::from(fields.u8()?);
                let name = std::str::from_utf8(fields.take(length)?).map_err(|_| WireError::BadName)?.to_owned();
                if !valid_event_name(&name) {
                    return Err(WireError::BadName);
                }
                Frame::Evt(Evt { target, name, text: text(fields.rest())? })
            }
            KIND_CTL => {
                let op = fields.u8()?;
                match op {
                    OP_INIT => {
                        let version = fields.u16()?;
                        let max_panes = fields.u32()?;
                        let max_scrollback_lines = fields.u32()?;
                        Frame::Ctl(Ctl::Init { version, max_panes, max_scrollback_lines })
                    }
                    OP_ATTACH => Frame::Ctl(Ctl::Attach { socket: fields.socket()? }),
                    op => return Err(WireError::UnknownOp { kind, op }),
                }
            }
            KIND_NOTE => match fields.u8()? {
                NOTE_READY => Frame::Note(Note::Ready),
                op => return Err(WireError::UnknownOp { kind, op }),
            },
            kind => return Err(WireError::UnknownKind(kind)),
        };
        fields.finish()?;
        Ok(frame)
    }
}

/// Reads one frame, blocking. `Ok(None)` is the end of the stream between two frames. A length over [`MAX_PAYLOAD`] is
/// refused before any payload is read or allocated; the buffer of the payload is wiped when it is dropped.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<Frame>, WireError> {
    let mut head = [0u8; 4];
    let mut got = 0;
    while got < head.len() {
        match reader.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(WireError::Truncated),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let length = usize::try_from(u32::from_be_bytes(head)).map_err(|_| WireError::TooLarge(usize::MAX))?;
    if length == 0 {
        return Err(WireError::Empty);
    }
    if length > MAX_PAYLOAD {
        return Err(WireError::TooLarge(length));
    }
    // The buffer grows as the payload arrives, so a length that promises more than the stream delivers costs only what
    // arrived.
    let mut payload = Zeroizing::new(Vec::with_capacity(length.min(FIRST_CHUNK)));
    let read = reader.by_ref().take(length as u64).read_to_end(&mut payload)?;
    if read < length {
        return Err(WireError::Truncated);
    }
    Frame::from_payload(&payload).map(Some)
}

/// Writes one frame: the length prefix and the payload, in one `write_all`.
pub fn write_frame<W: Write>(writer: &mut W, frame: &Frame) -> Result<(), WireError> {
    let mut out = Zeroizing::new(vec![0u8; 4]);
    frame.encode_into(&mut out)?;
    let length = u32::try_from(out.len() - 4).map_err(|_| WireError::TooLarge(out.len() - 4))?;
    out[..4].copy_from_slice(&length.to_be_bytes());
    writer.write_all(&out)?;
    Ok(())
}

/// The tokio codec for the same layout, for the supervisor's side of the socket.
pub fn codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder().big_endian().length_field_length(4).max_frame_length(MAX_PAYLOAD).new_codec()
}
