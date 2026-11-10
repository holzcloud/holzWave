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
//!   The socket id [`FILE_SOCKET`] (`u32::MAX`, never given to a browser connection) marks the answer to a file control
//!   (operations 8 to 11 below): its request id is the id the control carried, and the body is the answer of that control.
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
//!   1 Init          2..4 u16 protocol version (3) | 4..8 u32 most panes, 0 for no cap | 8..12 u32 most scrollback lines,
//!                   0 for no cap | 12..16 u32 most MiB of pictures, 0 for none | 16..20 u32 rows of scrollback an attach
//!                   brings by default | 20..24 u32 most rows an attach may bring | 24..28 u32 most bytes of a snapshot |
//!                   28..32 u32 milliseconds of silence after which the writer of a session loses the right  (32 bytes)
//!   2 Attach        2..6 u32 socket id (not 0)                                                         (6 bytes)
//!   3 Detach        2..6 u32 socket id (not 0)                                                         (6 bytes)
//!   4 Unlock        2..10 u64 control id | 10 u8 key kind | 11..43 the key                             (43 bytes)
//!   5 Lock          2..10 u64 control id                                                               (10 bytes)
//!   6 Shutdown      nothing                                                                            (2 bytes)
//!   7 ResetSecrets  2..10 u64 control id                                                               (10 bytes)
//!   8 FileServe     2..10 u64 request id (not 0) | 10 u8 what: 0 stat, 1 read | 11..43 the token: 32 bytes of 0-9a-f |
//!                   43..51 u64 offset | 51..55 u32 length; a stat has offset 0 and length 0, a read has a length of
//!                   1 to [`FILE_PIECE`] (1 MiB)                                                       (55 bytes)
//!                   The answer is a `Res` on [`FILE_SOCKET`]: a stat is status 0 with the JSON `{"mime","size"}`, a read is
//!                   status 2 with the bytes (fewer than asked at the end of the file), a refusal is status 1 with a short
//!                   fixed text.
//!   9 UploadOpen    2..10 u64 upload id (not 0) | 10..18 u64 declared size | 18..20 u16 folder length n, 1 to 1024 |
//!                   20..20+n the folder, UTF-8 | 20+n.. the file name, UTF-8, 1 to 255 bytes, to the end of the frame
//!                   The answer is a `Res` on [`FILE_SOCKET`] with the upload id as request id: status 0 with `{}` once the
//!                   folder is open and the part file is made, or status 1 with a short fixed text.
//!   10 UploadPiece  2..10 u64 upload id | 10.. the bytes of the body, 1 to [`FILE_PIECE`] (1 MiB), to the end of the frame
//!                   The answer is a `Res` with the upload id as request id: status 0 with `{}` once the bytes are written,
//!                   or status 1.
//!   11 UploadClose  2..10 u64 upload id | 10 u8 commit: 0 abort, 1 publish                              (11 bytes)
//!                   Commit answers status 0 with `{"path","size"}` once the file is complete, synced and published under a
//!                   name that did not exist; abort removes the part file and answers status 0 with `{}`. After either the
//!                   upload id is free.
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
//!
//! 0x06 Ses   both ways: the sessions that live in the worker, for the sockets of devices. `socket` is the id of the
//!            device socket (not 0), `session` the id the worker made (not 0), `request` the supervisor's number (not 0).
//!   0      u8    kind = 6
//!   1      u8    operation
//!   supervisor -> worker (a request is answered by exactly one `Answer` with its number, except `Input` and `Gone`):
//!   1 List       2..6 socket | 6..14 request                                                              (14 bytes)
//!   2 New        2..6 socket | 6..14 request | 14..16 u16 cols, not 0 | 16..18 u16 rows, not 0            (18 bytes)
//!   3 Attach     2..6 socket | 6..14 request | 14..18 session | 18 u8 has a tail: 0 or 1 | 19..23 u32 rows of tail,
//!                only when there is one                                                              (19 or 23 bytes)
//!   4 Detach     2..6 socket | 6..14 request | 14..18 session                                              (18 bytes)
//!   5 Input      2..6 socket | 6..10 session | 10.. the bytes, 1 to 65536, to the end of the frame; never answered
//!   6 Resize     2..6 socket | 6..14 request | 14..18 session | 18..20 u16 cols | 20..22 u16 rows, neither 0 (22 bytes)
//!   7 Takeover   2..6 socket | 6..14 request | 14..18 session                                              (18 bytes)
//!   8 End        2..6 socket | 6..14 request | 14..18 session                                              (18 bytes)
//!   9 Beat       2..6 socket | 6..14 request | 14..18 session                                              (18 bytes)
//!   10 Gone      2..6 socket; the socket ended: it leaves every session; never answered                    (6 bytes)
//!   worker -> supervisor:
//!   16 Answer    2..6 socket | 6..14 request | 14 u8 answer: 1 list, 2 created, 3 attached, 4 done, 5 ended, 6 failed
//!                1 list: u16 count 0 to 1024, then per session: u32 id | u16 title length 0 to 256 and the title |
//!                  u16 directory length 0 to 4096 and the directory | u16 cols | u16 rows | u64 start in Unix seconds |
//!                  u16 attached | u8 writer held: 0 or 1
//!                2 created: u32 session
//!                3 attached: u16 cols | u16 rows | u8 writer: 0 writing, 1 watching, 2 free | u8 older lines exist: 0 or 1 |
//!                  u32 bytes of snapshot that follow in `Snapshot` frames
//!                6 failed: u8 error: 1 not found, 2 refused, 3 busy, 4 too many, 5 bad request
//!   17 Snapshot  2..6 socket | 6..10 session | 10..14 u32 seq | 14.. the bytes, 1 to 65536
//!   18 Live      2..6 socket | 6..10 session | 10..14 u32 seq, which continues the count of the snapshot | 14.. the bytes,
//!                1 to 65536
//!   19 Event     2..6 socket | 6..10 session | 10 u8 event: 1 writer (u8 state as above), 2 exited (u8 has a status: 0 or 1,
//!                u32 status only when there is one), 3 resync, 4 size (u16 cols, u16 rows)
//! ```
//!
//! A frame that holds a key prints nothing of it in `Debug`, and the key is wiped from the frame when it is dropped.

use std::fmt;
use std::io::{self, Read, Write};

use subtle::ConstantTimeEq;
use tokio_util::codec::LengthDelimitedCodec;
use zeroize::{ZeroizeOnDrop, Zeroizing};

use crate::unlock::{KEY_BYTES, Kek};
use crate::worker::lease::WriterState;
use crate::worker::secretstore::SlotKind;
use crate::worker::sessions::{
    Attached, CHUNK_BYTES, MAX_CWD_BYTES, MAX_INPUT_BYTES, MAX_LISTED, MAX_TITLE_BYTES, SesError, SessionInfo,
    SessionLimits,
};

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
/// The kind byte of a session frame.
pub const KIND_SES: u8 = 6;

/// The operation number of `Init`.
pub const OP_INIT: u8 = 1;
/// The operation number of `Attach`.
pub const OP_ATTACH: u8 = 2;
/// The operation number of `Detach`.
pub const OP_DETACH: u8 = 3;
/// The operation number of `Unlock`.
pub const OP_UNLOCK: u8 = 4;
/// The operation number of `Lock`.
pub const OP_LOCK: u8 = 5;
/// The operation number of `Shutdown`.
pub const OP_SHUTDOWN: u8 = 6;
/// The operation number of `ResetSecrets`.
pub const OP_RESET_SECRETS: u8 = 7;
/// The operation number of `FileServe`.
pub const OP_FILE_SERVE: u8 = 8;
/// The operation number of `UploadOpen`.
pub const OP_UPLOAD_OPEN: u8 = 9;
/// The operation number of `UploadPiece`.
pub const OP_UPLOAD_PIECE: u8 = 10;
/// The operation number of `UploadClose`.
pub const OP_UPLOAD_CLOSE: u8 = 11;
/// The operation number of `Provision`.
pub const OP_PROVISION: u8 = 12;
/// The operation number of `SetSlot`.
pub const OP_SET_SLOT: u8 = 13;
/// The operation number of `RemoveSlot`.
pub const OP_REMOVE_SLOT: u8 = 14;

/// The operation number of the note `Ready`.
pub const NOTE_READY: u8 = 1;
/// The operation number of the note `StoreState`.
pub const NOTE_STORE_STATE: u8 = 2;
/// The operation number of the note `PaneCount`.
pub const NOTE_PANE_COUNT: u8 = 3;
/// The operation number of the note `Exiting`.
pub const NOTE_EXITING: u8 = 4;
/// The operation number of the note `CtlDone`.
pub const NOTE_CTL_DONE: u8 = 5;

/// The protocol version of the `Init` control. Version 3 added the session frames and the session limits of `Init`.
pub const PROTOCOL_VERSION: u16 = 3;

/// The socket id on which a worker answers a file control. A browser connection never has this id.
pub const FILE_SOCKET: u32 = u32::MAX;

/// The most bytes of a file in one `FileServe` read or one `UploadPiece`.
pub const FILE_PIECE: usize = crate::limits::FILE_PIECE_BYTES;

/// The length of a token in a `FileServe`, in bytes.
pub const TOKEN_BYTES: usize = 32;

/// The longest folder of an `UploadOpen`, in bytes.
pub const FOLDER_BYTES: usize = 1024;

/// The longest file name of an `UploadOpen`, in bytes.
pub const NAME_BYTES: usize = 255;

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
    /// The kind byte is not one of the six.
    UnknownKind(u8),
    /// The operation byte of a control or note is not one the decoder knows.
    UnknownOp { kind: u8, op: u8 },
    /// The operation number is reserved.
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

/// The bytes of a slot id.
pub const SLOT_ID_BYTES: usize = 16;

/// The most slot keys one `Provision` carries.
pub const MAX_PROVISION_SLOTS: usize = 4;

/// A 32-byte key as it travels: wiped when dropped, a marker for `Debug`, compared in constant time.
#[derive(Clone)]
pub struct Key(Zeroizing<[u8; KEY_BYTES]>);

impl Key {
    /// A key from 32 bytes. The caller wipes its own copy.
    pub fn from_bytes(bytes: &[u8; KEY_BYTES]) -> Key {
        Key(Zeroizing::new(*bytes))
    }

    /// A key from a slice of exactly 32 bytes.
    fn from_slice(bytes: &[u8]) -> Result<Key, WireError> {
        let mut raw = Zeroizing::new([0u8; KEY_BYTES]);
        if bytes.len() != KEY_BYTES {
            return Err(WireError::Truncated);
        }
        raw.copy_from_slice(bytes);
        Ok(Key::from_bytes(&raw))
    }

    /// The bytes, for the code that uses the key and nothing else.
    pub fn expose(&self) -> &[u8; KEY_BYTES] {
        &self.0
    }

    /// The same bytes as a key-encryption key.
    pub fn to_kek(&self) -> Kek {
        Kek::from_bytes(&self.0)
    }
}

impl ZeroizeOnDrop for Key {}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(..)")
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Key) -> bool {
        self.0.ct_eq(&*other.0).into()
    }
}

impl Eq for Key {}

/// A key with the kind and the id of the slot it opens: 49 bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotKey {
    pub kind: SlotKind,
    pub id: [u8; SLOT_ID_BYTES],
    pub key: Key,
}

/// A key that opens a locked store for one change: its kind and the key, 33 bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpeningKey {
    pub kind: SlotKind,
    pub key: Key,
}

/// What a `FileServe` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileWhat {
    /// The media type and size of the grant.
    Stat = 0,
    /// A range of the bytes of the file.
    Read = 1,
}

/// Whether `token` is exactly 32 characters of `0-9a-f`.
pub fn valid_token(token: &str) -> bool {
    token.len() == TOKEN_BYTES && token.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A control from the supervisor.
#[derive(Clone, PartialEq, Eq)]
pub enum Ctl {
    /// The first frame: the protocol version and the caps of this person. 0 is no cap for the panes and the scrollback;
    /// for `max_image_mib`, the most MiB of pictures one pane keeps, 0 means no pictures at all. `sessions` are the four
    /// numbers that bound what a device may ask of a session.
    Init { version: u16, max_panes: u32, max_scrollback_lines: u32, max_image_mib: u32, sessions: SessionLimits },
    /// A browser connection is attached: it gets the replay of titles and working directories.
    Attach { socket: u32 },
    /// A browser connection is gone.
    Detach { socket: u32 },
    /// Open the store with the key of a slot of `kind`.
    Unlock { ctl: u64, kind: SlotKind, key: Key },
    /// Close the store and wipe its data key.
    Lock { ctl: u64 },
    /// Close every pane, save the layout, say `Exiting` and end.
    Shutdown,
    /// Remove the store file and its siblings and wipe the key.
    ResetSecrets { ctl: u64 },
    /// Create the store with these slots (one to four).
    Provision { ctl: u64, slots: Vec<SlotKey> },
    /// Add or replace one slot; a store with none is created with it; a locked store is opened for the change with
    /// `opening` and is locked again afterwards.
    SetSlot { ctl: u64, slot: SlotKey, opening: Option<OpeningKey> },
    /// Remove one slot.
    RemoveSlot { ctl: u64, kind: SlotKind, id: [u8; SLOT_ID_BYTES] },
    /// Ask for the media type and size of a granted file, or for a range of its bytes. The answer is a `Res` on
    /// [`FILE_SOCKET`] with `request` as its request id.
    FileServe { request: u64, what: FileWhat, token: String, offset: u64, length: u32 },
    /// Open an upload of `size` bytes into `folder` (relative to the person's home) under `name`.
    UploadOpen { id: u64, size: u64, folder: String, name: String },
    /// The next bytes of an upload.
    UploadPiece { id: u64, bytes: Vec<u8> },
    /// Publish (`commit`) or remove an upload.
    UploadClose { id: u64, commit: bool },
}

/// What a frame that holds a key prints: its name and nothing of its content.
impl fmt::Debug for Ctl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ctl::Init { version, max_panes, max_scrollback_lines, max_image_mib, sessions } => f
                .debug_struct("Init")
                .field("version", version)
                .field("max_panes", max_panes)
                .field("max_scrollback_lines", max_scrollback_lines)
                .field("max_image_mib", max_image_mib)
                .field("sessions", sessions)
                .finish(),
            Ctl::Attach { socket } => f.debug_struct("Attach").field("socket", socket).finish(),
            Ctl::Detach { socket } => f.debug_struct("Detach").field("socket", socket).finish(),
            Ctl::Lock { ctl } => f.debug_struct("Lock").field("ctl", ctl).finish(),
            Ctl::Shutdown => f.write_str("Shutdown"),
            Ctl::ResetSecrets { ctl } => f.debug_struct("ResetSecrets").field("ctl", ctl).finish(),
            Ctl::RemoveSlot { ctl, .. } => f.debug_struct("RemoveSlot").field("ctl", ctl).finish_non_exhaustive(),
            Ctl::Unlock { ctl, .. } => f.debug_struct("Unlock").field("ctl", ctl).finish_non_exhaustive(),
            Ctl::Provision { ctl, slots } => {
                f.debug_struct("Provision").field("ctl", ctl).field("slots", &slots.len()).finish_non_exhaustive()
            }
            Ctl::SetSlot { ctl, .. } => f.debug_struct("SetSlot").field("ctl", ctl).finish_non_exhaustive(),
            // A token, a folder, a name and the bytes of a file are never printed.
            Ctl::FileServe { request, what, .. } => {
                f.debug_struct("FileServe").field("request", request).field("what", what).finish_non_exhaustive()
            }
            Ctl::UploadOpen { id, size, .. } => {
                f.debug_struct("UploadOpen").field("id", id).field("size", size).finish_non_exhaustive()
            }
            Ctl::UploadPiece { id, bytes } => {
                f.debug_struct("UploadPiece").field("id", id).field("bytes", &bytes.len()).finish()
            }
            Ctl::UploadClose { id, commit } => f.debug_struct("UploadClose").field("id", id).field("commit", commit).finish(),
        }
    }
}

/// What the store is, as the worker tells the supervisor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreState {
    /// There is no store file.
    Absent = 0,
    /// There is a store and the data key is not held.
    Locked = 1,
    /// The data key is held.
    Unlocked = 2,
}

/// Why the worker ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    /// The supervisor asked for it.
    Clean = 0,
    /// The worker cannot go on.
    Failure = 1,
}

/// What a control came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtlOutcome {
    /// It was done.
    Done = 0,
    /// The store is locked or the key does not open it; nothing changed.
    Locked = 1,
    /// The worker will not do it (a store exists already, the last slot, no such slot).
    Refused = 2,
    /// It could not be done (the file system failed).
    Failed = 3,
    /// There is no store.
    Absent = 4,
}

/// A note from the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// The core is built and the worker answers commands.
    Ready,
    /// The store changed.
    StoreState(StoreState),
    /// The number of panes in all windows changed.
    PaneCount(u32),
    /// The worker is ending.
    Exiting(ExitReason),
    /// The answer to the control `ctl`.
    CtlDone { ctl: u64, outcome: CtlOutcome },
}

/// One frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Cmd(CmdFrame),
    Res(Res),
    Evt(Evt),
    Ctl(Ctl),
    Note(Note),
    Ses(Ses),
}

/// What a worker answers to a request about sessions.
#[derive(Clone, PartialEq, Eq)]
pub enum SesAnswer {
    /// The sessions of the person, with an id, a title and a size.
    Listed(Vec<SessionInfo>),
    /// A session was made.
    Created { session: u32 },
    /// The attach was accepted: what follows are the snapshot's bytes and then the live bytes.
    Attached(Attached),
    /// Done (a detach, a resize, a takeover, a beat).
    Done,
    /// The program of the session was told to end.
    Ended,
    /// Not done, for one of a closed set of reasons.
    Failed(SesError),
}

/// What a worker tells a device socket about a session on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SesEvent {
    /// This socket's own view of the right to write changed.
    Writer(WriterState),
    /// The program ended, with its status when it could be read.
    Exited(Option<u32>),
    /// The stream of this socket was lost; attach again.
    Resync,
    /// The grid of the session changed.
    Size { cols: u16, rows: u16 },
}

/// A frame about the sessions that live in a worker. The requests come from the supervisor on behalf of a device socket,
/// the answers, the bytes and the events go back.
#[derive(Clone, PartialEq, Eq)]
pub enum Ses {
    List {
        socket: u32,
        request: u64,
    },
    New {
        socket: u32,
        request: u64,
        cols: u16,
        rows: u16,
    },
    Attach {
        socket: u32,
        request: u64,
        session: u32,
        tail: Option<u32>,
    },
    Detach {
        socket: u32,
        request: u64,
        session: u32,
    },
    /// Input bytes; never answered, dropped unless the socket holds the right to write.
    Input {
        socket: u32,
        session: u32,
        bytes: Vec<u8>,
    },
    Resize {
        socket: u32,
        request: u64,
        session: u32,
        cols: u16,
        rows: u16,
    },
    Takeover {
        socket: u32,
        request: u64,
        session: u32,
    },
    End {
        socket: u32,
        request: u64,
        session: u32,
    },
    Beat {
        socket: u32,
        request: u64,
        session: u32,
    },
    /// The socket ended: it leaves every session. Never answered.
    Gone {
        socket: u32,
    },
    Answer {
        socket: u32,
        request: u64,
        answer: SesAnswer,
    },
    Snapshot {
        socket: u32,
        session: u32,
        seq: u32,
        bytes: Vec<u8>,
    },
    Live {
        socket: u32,
        session: u32,
        seq: u32,
        bytes: Vec<u8>,
    },
    Event {
        socket: u32,
        session: u32,
        event: SesEvent,
    },
}

impl Ses {
    /// Whether the supervisor sends this frame (and a worker accepts it), as opposed to the frames only a worker writes.
    pub fn is_request(&self) -> bool {
        matches!(
            self,
            Ses::List { .. }
                | Ses::New { .. }
                | Ses::Attach { .. }
                | Ses::Detach { .. }
                | Ses::Input { .. }
                | Ses::Resize { .. }
                | Ses::Takeover { .. }
                | Ses::End { .. }
                | Ses::Beat { .. }
                | Ses::Gone { .. }
        )
    }
}

/// What a session frame prints: its name and the numbers that name it, never a byte of a terminal, a title or a directory.
impl fmt::Debug for Ses {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ses::List { socket, request } => {
                f.debug_struct("List").field("socket", socket).field("request", request).finish()
            }
            Ses::New { socket, request, cols, rows } => f
                .debug_struct("New")
                .field("socket", socket)
                .field("request", request)
                .field("size", &(cols, rows))
                .finish(),
            Ses::Attach { socket, request, session, tail } => f
                .debug_struct("Attach")
                .field("socket", socket)
                .field("request", request)
                .field("session", session)
                .field("tail", tail)
                .finish(),
            Ses::Detach { socket, request, session } => f
                .debug_struct("Detach")
                .field("socket", socket)
                .field("request", request)
                .field("session", session)
                .finish(),
            Ses::Input { socket, session, bytes } => f
                .debug_struct("Input")
                .field("socket", socket)
                .field("session", session)
                .field("bytes", &bytes.len())
                .finish(),
            Ses::Resize { socket, request, session, cols, rows } => f
                .debug_struct("Resize")
                .field("socket", socket)
                .field("request", request)
                .field("session", session)
                .field("size", &(cols, rows))
                .finish(),
            Ses::Takeover { socket, request, session } => f
                .debug_struct("Takeover")
                .field("socket", socket)
                .field("request", request)
                .field("session", session)
                .finish(),
            Ses::End { socket, request, session } => f
                .debug_struct("End")
                .field("socket", socket)
                .field("request", request)
                .field("session", session)
                .finish(),
            Ses::Beat { socket, request, session } => f
                .debug_struct("Beat")
                .field("socket", socket)
                .field("request", request)
                .field("session", session)
                .finish(),
            Ses::Gone { socket } => f.debug_struct("Gone").field("socket", socket).finish(),
            Ses::Answer { socket, request, answer } => f
                .debug_struct("Answer")
                .field("socket", socket)
                .field("request", request)
                .field("answer", answer)
                .finish(),
            Ses::Snapshot { socket, session, seq, bytes } => f
                .debug_struct("Snapshot")
                .field("socket", socket)
                .field("session", session)
                .field("seq", seq)
                .field("bytes", &bytes.len())
                .finish(),
            Ses::Live { socket, session, seq, bytes } => f
                .debug_struct("Live")
                .field("socket", socket)
                .field("session", session)
                .field("seq", seq)
                .field("bytes", &bytes.len())
                .finish(),
            Ses::Event { socket, session, event } => {
                f.debug_struct("Event").field("socket", socket).field("session", session).field("event", event).finish()
            }
        }
    }
}

/// An answer prints its kind, and a list its count: a title and a directory are what a program chose.
impl fmt::Debug for SesAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SesAnswer::Listed(list) => f.debug_struct("Listed").field("sessions", &list.len()).finish(),
            SesAnswer::Created { session } => f.debug_struct("Created").field("session", session).finish(),
            SesAnswer::Attached(attached) => f.debug_tuple("Attached").field(attached).finish(),
            SesAnswer::Done => f.write_str("Done"),
            SesAnswer::Ended => f.write_str("Ended"),
            SesAnswer::Failed(error) => f.debug_tuple("Failed").field(error).finish(),
        }
    }
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

    /// A request or upload id that is not 0.
    fn id(&mut self) -> Result<u64, WireError> {
        match self.u64()? {
            0 => Err(WireError::BadValue("an id is not 0")),
            id => Ok(id),
        }
    }

    /// A session id that is not 0.
    fn session(&mut self) -> Result<u32, WireError> {
        match self.u32()? {
            0 => Err(WireError::BadValue("a session id is not 0")),
            session => Ok(session),
        }
    }

    /// A number of columns or rows that is not 0.
    fn dim(&mut self) -> Result<u16, WireError> {
        match self.u16()? {
            0 => Err(WireError::BadValue("a size is not 0")),
            dim => Ok(dim),
        }
    }

    /// A flag that is 0 or 1.
    fn flag(&mut self, what: &'static str) -> Result<bool, WireError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(WireError::BadValue(what)),
        }
    }

    /// A text of at most `max` bytes behind a u16 length.
    fn text16(&mut self, max: usize, what: &'static str) -> Result<String, WireError> {
        let length = usize::from(self.u16()?);
        if length > max {
            return Err(WireError::BadValue(what));
        }
        text(self.take(length)?)
    }

    /// Bytes to the end of the frame, 1 to `max`.
    fn chunk(&mut self, max: usize, what: &'static str) -> Result<Vec<u8>, WireError> {
        let bytes = self.rest();
        if bytes.is_empty() || bytes.len() > max {
            return Err(WireError::BadValue(what));
        }
        Ok(bytes.to_vec())
    }

    /// A socket id that is not 0.
    fn socket(&mut self) -> Result<u32, WireError> {
        match self.u32()? {
            0 => Err(WireError::BadSocket),
            socket => Ok(socket),
        }
    }

    fn kind(&mut self) -> Result<SlotKind, WireError> {
        SlotKind::from_byte(self.u8()?).ok_or(WireError::BadValue("a key kind is not 1, 2, 3 or 4"))
    }

    fn key(&mut self) -> Result<Key, WireError> {
        Key::from_slice(self.take(KEY_BYTES)?)
    }

    fn slot_id(&mut self) -> Result<[u8; SLOT_ID_BYTES], WireError> {
        self.take(SLOT_ID_BYTES)?.try_into().map_err(|_| WireError::Truncated)
    }

    fn slot_key(&mut self) -> Result<SlotKey, WireError> {
        Ok(SlotKey { kind: self.kind()?, id: self.slot_id()?, key: self.key()? })
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

fn put_slot_key(out: &mut Vec<u8>, slot: &SlotKey) {
    out.push(slot.kind.byte());
    out.extend_from_slice(&slot.id);
    out.extend_from_slice(slot.key.expose());
}

// ---- the session frames -------------------------------------------------------------------------------------------------

const SES_LIST: u8 = 1;
const SES_NEW: u8 = 2;
const SES_ATTACH: u8 = 3;
const SES_DETACH: u8 = 4;
const SES_INPUT: u8 = 5;
const SES_RESIZE: u8 = 6;
const SES_TAKEOVER: u8 = 7;
const SES_END: u8 = 8;
const SES_BEAT: u8 = 9;
const SES_GONE: u8 = 10;
const SES_ANSWER: u8 = 16;
const SES_SNAPSHOT: u8 = 17;
const SES_LIVE: u8 = 18;
const SES_EVENT: u8 = 19;

const ANSWER_LIST: u8 = 1;
const ANSWER_CREATED: u8 = 2;
const ANSWER_ATTACHED: u8 = 3;
const ANSWER_DONE: u8 = 4;
const ANSWER_ENDED: u8 = 5;
const ANSWER_FAILED: u8 = 6;

const EVENT_WRITER: u8 = 1;
const EVENT_EXITED: u8 = 2;
const EVENT_RESYNC: u8 = 3;
const EVENT_SIZE: u8 = 4;

fn error_byte(error: SesError) -> u8 {
    match error {
        SesError::NotFound => 1,
        SesError::Refused => 2,
        SesError::Busy => 3,
        SesError::TooMany => 4,
        SesError::BadRequest => 5,
    }
}

fn error_of(byte: u8) -> Result<SesError, WireError> {
    match byte {
        1 => Ok(SesError::NotFound),
        2 => Ok(SesError::Refused),
        3 => Ok(SesError::Busy),
        4 => Ok(SesError::TooMany),
        5 => Ok(SesError::BadRequest),
        _ => Err(WireError::BadValue("a session error is not 1 to 5")),
    }
}

fn writer_byte(state: WriterState) -> u8 {
    match state {
        WriterState::Writing => 0,
        WriterState::Watching => 1,
        WriterState::Free => 2,
    }
}

fn writer_of(byte: u8) -> Result<WriterState, WireError> {
    match byte {
        0 => Ok(WriterState::Writing),
        1 => Ok(WriterState::Watching),
        2 => Ok(WriterState::Free),
        _ => Err(WireError::BadValue("a writer state is not 0, 1 or 2")),
    }
}

fn nonzero_id(request: u64) -> Result<u64, WireError> {
    if request == 0 { Err(WireError::BadValue("an id is not 0")) } else { Ok(request) }
}

fn nonzero_session(session: u32) -> Result<u32, WireError> {
    if session == 0 { Err(WireError::BadValue("a session id is not 0")) } else { Ok(session) }
}

fn nonzero_dim(dim: u16) -> Result<u16, WireError> {
    if dim == 0 { Err(WireError::BadValue("a size is not 0")) } else { Ok(dim) }
}

fn put_text16(out: &mut Vec<u8>, text: &str, max: usize, what: &'static str) -> Result<(), WireError> {
    if text.len() > max {
        return Err(WireError::BadValue(what));
    }
    out.extend_from_slice(&u16::try_from(text.len()).map_err(|_| WireError::BadValue(what))?.to_be_bytes());
    out.extend_from_slice(text.as_bytes());
    Ok(())
}

fn put_chunk(out: &mut Vec<u8>, bytes: &[u8], max: usize, what: &'static str) -> Result<(), WireError> {
    if bytes.is_empty() || bytes.len() > max {
        return Err(WireError::BadValue(what));
    }
    out.extend_from_slice(bytes);
    Ok(())
}

fn encode_ses(ses: &Ses, out: &mut Vec<u8>) -> Result<(), WireError> {
    out.push(KIND_SES);
    // The head every request carries: the operation, the socket, the request number.
    let head = |out: &mut Vec<u8>, op: u8, socket: u32, request: u64| -> Result<(), WireError> {
        out.push(op);
        out.extend_from_slice(&nonzero(socket)?.to_be_bytes());
        out.extend_from_slice(&nonzero_id(request)?.to_be_bytes());
        Ok(())
    };
    match ses {
        Ses::List { socket, request } => head(out, SES_LIST, *socket, *request)?,
        Ses::New { socket, request, cols, rows } => {
            head(out, SES_NEW, *socket, *request)?;
            out.extend_from_slice(&nonzero_dim(*cols)?.to_be_bytes());
            out.extend_from_slice(&nonzero_dim(*rows)?.to_be_bytes());
        }
        Ses::Attach { socket, request, session, tail } => {
            head(out, SES_ATTACH, *socket, *request)?;
            out.extend_from_slice(&nonzero_session(*session)?.to_be_bytes());
            match tail {
                None => out.push(0),
                Some(rows) => {
                    out.push(1);
                    out.extend_from_slice(&rows.to_be_bytes());
                }
            }
        }
        Ses::Detach { socket, request, session }
        | Ses::Takeover { socket, request, session }
        | Ses::End { socket, request, session }
        | Ses::Beat { socket, request, session } => {
            let op = match ses {
                Ses::Detach { .. } => SES_DETACH,
                Ses::Takeover { .. } => SES_TAKEOVER,
                Ses::End { .. } => SES_END,
                _ => SES_BEAT,
            };
            head(out, op, *socket, *request)?;
            out.extend_from_slice(&nonzero_session(*session)?.to_be_bytes());
        }
        Ses::Input { socket, session, bytes } => {
            out.push(SES_INPUT);
            out.extend_from_slice(&nonzero(*socket)?.to_be_bytes());
            out.extend_from_slice(&nonzero_session(*session)?.to_be_bytes());
            put_chunk(out, bytes, MAX_INPUT_BYTES, "input is 1 byte to 64 KiB")?;
        }
        Ses::Resize { socket, request, session, cols, rows } => {
            head(out, SES_RESIZE, *socket, *request)?;
            out.extend_from_slice(&nonzero_session(*session)?.to_be_bytes());
            out.extend_from_slice(&nonzero_dim(*cols)?.to_be_bytes());
            out.extend_from_slice(&nonzero_dim(*rows)?.to_be_bytes());
        }
        Ses::Gone { socket } => {
            out.push(SES_GONE);
            out.extend_from_slice(&nonzero(*socket)?.to_be_bytes());
        }
        Ses::Answer { socket, request, answer } => {
            head(out, SES_ANSWER, *socket, *request)?;
            match answer {
                SesAnswer::Listed(list) => {
                    if list.len() > MAX_LISTED {
                        return Err(WireError::BadValue("a list has at most 1024 sessions"));
                    }
                    out.push(ANSWER_LIST);
                    out.extend_from_slice(
                        &u16::try_from(list.len()).map_err(|_| WireError::BadValue("too many sessions"))?.to_be_bytes(),
                    );
                    for info in list {
                        out.extend_from_slice(&nonzero_session(info.id)?.to_be_bytes());
                        put_text16(out, &info.title, MAX_TITLE_BYTES, "a title is at most 256 bytes")?;
                        put_text16(out, &info.cwd, MAX_CWD_BYTES, "a directory is at most 4096 bytes")?;
                        out.extend_from_slice(&nonzero_dim(info.cols)?.to_be_bytes());
                        out.extend_from_slice(&nonzero_dim(info.rows)?.to_be_bytes());
                        out.extend_from_slice(&info.started.to_be_bytes());
                        out.extend_from_slice(&info.attached.to_be_bytes());
                        out.push(u8::from(info.writer_held));
                    }
                }
                SesAnswer::Created { session } => {
                    out.push(ANSWER_CREATED);
                    out.extend_from_slice(&nonzero_session(*session)?.to_be_bytes());
                }
                SesAnswer::Attached(attached) => {
                    out.push(ANSWER_ATTACHED);
                    out.extend_from_slice(&nonzero_dim(attached.cols)?.to_be_bytes());
                    out.extend_from_slice(&nonzero_dim(attached.rows)?.to_be_bytes());
                    out.push(writer_byte(attached.writer));
                    out.push(u8::from(attached.more));
                    out.extend_from_slice(&attached.snapshot_bytes.to_be_bytes());
                }
                SesAnswer::Done => out.push(ANSWER_DONE),
                SesAnswer::Ended => out.push(ANSWER_ENDED),
                SesAnswer::Failed(error) => {
                    out.push(ANSWER_FAILED);
                    out.push(error_byte(*error));
                }
            }
        }
        Ses::Snapshot { socket, session, seq, bytes } | Ses::Live { socket, session, seq, bytes } => {
            out.push(if matches!(ses, Ses::Snapshot { .. }) { SES_SNAPSHOT } else { SES_LIVE });
            out.extend_from_slice(&nonzero(*socket)?.to_be_bytes());
            out.extend_from_slice(&nonzero_session(*session)?.to_be_bytes());
            out.extend_from_slice(&seq.to_be_bytes());
            put_chunk(out, bytes, CHUNK_BYTES, "a chunk is 1 byte to 64 KiB")?;
        }
        Ses::Event { socket, session, event } => {
            out.push(SES_EVENT);
            out.extend_from_slice(&nonzero(*socket)?.to_be_bytes());
            out.extend_from_slice(&nonzero_session(*session)?.to_be_bytes());
            match event {
                SesEvent::Writer(state) => out.extend_from_slice(&[EVENT_WRITER, writer_byte(*state)]),
                SesEvent::Exited(None) => out.extend_from_slice(&[EVENT_EXITED, 0]),
                SesEvent::Exited(Some(code)) => {
                    out.extend_from_slice(&[EVENT_EXITED, 1]);
                    out.extend_from_slice(&code.to_be_bytes());
                }
                SesEvent::Resync => out.push(EVENT_RESYNC),
                SesEvent::Size { cols, rows } => {
                    out.push(EVENT_SIZE);
                    out.extend_from_slice(&nonzero_dim(*cols)?.to_be_bytes());
                    out.extend_from_slice(&nonzero_dim(*rows)?.to_be_bytes());
                }
            }
        }
    }
    Ok(())
}

fn decode_ses(fields: &mut Fields<'_>) -> Result<Ses, WireError> {
    let op = fields.u8()?;
    let ses = match op {
        SES_LIST => Ses::List { socket: fields.socket()?, request: fields.id()? },
        SES_NEW => {
            Ses::New { socket: fields.socket()?, request: fields.id()?, cols: fields.dim()?, rows: fields.dim()? }
        }
        SES_ATTACH => {
            let (socket, request, session) = (fields.socket()?, fields.id()?, fields.session()?);
            let tail = if fields.flag("the tail flag is not 0 or 1")? { Some(fields.u32()?) } else { None };
            Ses::Attach { socket, request, session, tail }
        }
        SES_DETACH => Ses::Detach { socket: fields.socket()?, request: fields.id()?, session: fields.session()? },
        SES_TAKEOVER => Ses::Takeover { socket: fields.socket()?, request: fields.id()?, session: fields.session()? },
        SES_END => Ses::End { socket: fields.socket()?, request: fields.id()?, session: fields.session()? },
        SES_BEAT => Ses::Beat { socket: fields.socket()?, request: fields.id()?, session: fields.session()? },
        SES_INPUT => {
            let (socket, session) = (fields.socket()?, fields.session()?);
            Ses::Input { socket, session, bytes: fields.chunk(MAX_INPUT_BYTES, "input is 1 byte to 64 KiB")? }
        }
        SES_RESIZE => Ses::Resize {
            socket: fields.socket()?,
            request: fields.id()?,
            session: fields.session()?,
            cols: fields.dim()?,
            rows: fields.dim()?,
        },
        SES_GONE => Ses::Gone { socket: fields.socket()? },
        SES_ANSWER => {
            let (socket, request) = (fields.socket()?, fields.id()?);
            let answer = match fields.u8()? {
                ANSWER_LIST => {
                    let count = usize::from(fields.u16()?);
                    if count > MAX_LISTED {
                        return Err(WireError::BadValue("a list has at most 1024 sessions"));
                    }
                    let mut list = Vec::with_capacity(count);
                    for _ in 0..count {
                        list.push(SessionInfo {
                            id: fields.session()?,
                            title: fields.text16(MAX_TITLE_BYTES, "a title is at most 256 bytes")?,
                            cwd: fields.text16(MAX_CWD_BYTES, "a directory is at most 4096 bytes")?,
                            cols: fields.dim()?,
                            rows: fields.dim()?,
                            started: fields.u64()?,
                            attached: fields.u16()?,
                            writer_held: fields.flag("the writer flag is not 0 or 1")?,
                        });
                    }
                    SesAnswer::Listed(list)
                }
                ANSWER_CREATED => SesAnswer::Created { session: fields.session()? },
                ANSWER_ATTACHED => SesAnswer::Attached(Attached {
                    cols: fields.dim()?,
                    rows: fields.dim()?,
                    writer: writer_of(fields.u8()?)?,
                    more: fields.flag("the older-lines flag is not 0 or 1")?,
                    snapshot_bytes: fields.u32()?,
                }),
                ANSWER_DONE => SesAnswer::Done,
                ANSWER_ENDED => SesAnswer::Ended,
                ANSWER_FAILED => SesAnswer::Failed(error_of(fields.u8()?)?),
                _ => return Err(WireError::BadValue("a session answer is not 1 to 6")),
            };
            Ses::Answer { socket, request, answer }
        }
        SES_SNAPSHOT | SES_LIVE => {
            let (socket, session, seq) = (fields.socket()?, fields.session()?, fields.u32()?);
            let bytes = fields.chunk(CHUNK_BYTES, "a chunk is 1 byte to 64 KiB")?;
            if op == SES_SNAPSHOT {
                Ses::Snapshot { socket, session, seq, bytes }
            } else {
                Ses::Live { socket, session, seq, bytes }
            }
        }
        SES_EVENT => {
            let (socket, session) = (fields.socket()?, fields.session()?);
            let event = match fields.u8()? {
                EVENT_WRITER => SesEvent::Writer(writer_of(fields.u8()?)?),
                EVENT_EXITED => {
                    if fields.flag("the status flag is not 0 or 1")? {
                        SesEvent::Exited(Some(fields.u32()?))
                    } else {
                        SesEvent::Exited(None)
                    }
                }
                EVENT_RESYNC => SesEvent::Resync,
                EVENT_SIZE => SesEvent::Size { cols: fields.dim()?, rows: fields.dim()? },
                _ => return Err(WireError::BadValue("a session event is not 1 to 4")),
            };
            Ses::Event { socket, session, event }
        }
        op => return Err(WireError::UnknownOp { kind: KIND_SES, op }),
    };
    Ok(ses)
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
            Frame::Ctl(Ctl::Init { version, max_panes, max_scrollback_lines, max_image_mib, sessions }) => {
                out.extend_from_slice(&[KIND_CTL, OP_INIT]);
                out.extend_from_slice(&version.to_be_bytes());
                out.extend_from_slice(&max_panes.to_be_bytes());
                out.extend_from_slice(&max_scrollback_lines.to_be_bytes());
                out.extend_from_slice(&max_image_mib.to_be_bytes());
                out.extend_from_slice(&sessions.attach_tail_lines.to_be_bytes());
                out.extend_from_slice(&sessions.history_max_lines.to_be_bytes());
                out.extend_from_slice(&sessions.snapshot_max_bytes.to_be_bytes());
                out.extend_from_slice(&sessions.writer_timeout_ms.to_be_bytes());
            }
            Frame::Ctl(Ctl::Attach { socket }) => {
                out.extend_from_slice(&[KIND_CTL, OP_ATTACH]);
                out.extend_from_slice(&nonzero(*socket)?.to_be_bytes());
            }
            Frame::Ctl(Ctl::Detach { socket }) => {
                out.extend_from_slice(&[KIND_CTL, OP_DETACH]);
                out.extend_from_slice(&nonzero(*socket)?.to_be_bytes());
            }
            Frame::Ctl(Ctl::Unlock { ctl, kind, key }) => {
                out.extend_from_slice(&[KIND_CTL, OP_UNLOCK]);
                out.extend_from_slice(&ctl.to_be_bytes());
                out.push(kind.byte());
                out.extend_from_slice(key.expose());
            }
            Frame::Ctl(Ctl::Lock { ctl }) => {
                out.extend_from_slice(&[KIND_CTL, OP_LOCK]);
                out.extend_from_slice(&ctl.to_be_bytes());
            }
            Frame::Ctl(Ctl::Shutdown) => out.extend_from_slice(&[KIND_CTL, OP_SHUTDOWN]),
            Frame::Ctl(Ctl::ResetSecrets { ctl }) => {
                out.extend_from_slice(&[KIND_CTL, OP_RESET_SECRETS]);
                out.extend_from_slice(&ctl.to_be_bytes());
            }
            Frame::Ctl(Ctl::Provision { ctl, slots }) => {
                if slots.len() > MAX_PROVISION_SLOTS {
                    return Err(WireError::BadValue("a provision carries at most four slot keys"));
                }
                out.extend_from_slice(&[KIND_CTL, OP_PROVISION]);
                out.extend_from_slice(&ctl.to_be_bytes());
                out.push(u8::try_from(slots.len()).map_err(|_| WireError::BadValue("too many slot keys"))?);
                for slot in slots {
                    put_slot_key(out, slot);
                }
            }
            Frame::Ctl(Ctl::SetSlot { ctl, slot, opening }) => {
                out.extend_from_slice(&[KIND_CTL, OP_SET_SLOT]);
                out.extend_from_slice(&ctl.to_be_bytes());
                put_slot_key(out, slot);
                match opening {
                    None => out.push(0),
                    Some(opening) => {
                        out.push(1);
                        out.push(opening.kind.byte());
                        out.extend_from_slice(opening.key.expose());
                    }
                }
            }
            Frame::Ctl(Ctl::RemoveSlot { ctl, kind, id }) => {
                out.extend_from_slice(&[KIND_CTL, OP_REMOVE_SLOT]);
                out.extend_from_slice(&ctl.to_be_bytes());
                out.push(kind.byte());
                out.extend_from_slice(id);
            }
            Frame::Ctl(Ctl::FileServe { request, what, token, offset, length }) => {
                if *request == 0 || !valid_token(token) {
                    return Err(WireError::BadValue("a file request needs an id and a token of 32 lower-case hex digits"));
                }
                match what {
                    FileWhat::Stat if *offset != 0 || *length != 0 => {
                        return Err(WireError::BadValue("a stat has no offset and no length"));
                    }
                    FileWhat::Read if *length == 0 || usize::try_from(*length).is_ok_and(|l| l > FILE_PIECE) => {
                        return Err(WireError::BadValue("a read has a length of 1 to 1 MiB"));
                    }
                    _ => {}
                }
                out.extend_from_slice(&[KIND_CTL, OP_FILE_SERVE]);
                out.extend_from_slice(&request.to_be_bytes());
                out.push(*what as u8);
                out.extend_from_slice(token.as_bytes());
                out.extend_from_slice(&offset.to_be_bytes());
                out.extend_from_slice(&length.to_be_bytes());
            }
            Frame::Ctl(Ctl::UploadOpen { id, size, folder, name }) => {
                if *id == 0 {
                    return Err(WireError::BadValue("an upload id is not 0"));
                }
                if folder.is_empty() || folder.len() > FOLDER_BYTES {
                    return Err(WireError::BadValue("a folder is 1 to 1024 bytes"));
                }
                if name.is_empty() || name.len() > NAME_BYTES {
                    return Err(WireError::BadValue("a file name is 1 to 255 bytes"));
                }
                out.extend_from_slice(&[KIND_CTL, OP_UPLOAD_OPEN]);
                out.extend_from_slice(&id.to_be_bytes());
                out.extend_from_slice(&size.to_be_bytes());
                out.extend_from_slice(&u16::try_from(folder.len()).map_err(|_| WireError::TooLarge(folder.len()))?.to_be_bytes());
                out.extend_from_slice(folder.as_bytes());
                out.extend_from_slice(name.as_bytes());
            }
            Frame::Ctl(Ctl::UploadPiece { id, bytes }) => {
                if *id == 0 {
                    return Err(WireError::BadValue("an upload id is not 0"));
                }
                if bytes.is_empty() || bytes.len() > FILE_PIECE {
                    return Err(WireError::BadValue("a piece is 1 byte to 1 MiB"));
                }
                out.extend_from_slice(&[KIND_CTL, OP_UPLOAD_PIECE]);
                out.extend_from_slice(&id.to_be_bytes());
                out.extend_from_slice(bytes);
            }
            Frame::Ctl(Ctl::UploadClose { id, commit }) => {
                if *id == 0 {
                    return Err(WireError::BadValue("an upload id is not 0"));
                }
                out.extend_from_slice(&[KIND_CTL, OP_UPLOAD_CLOSE]);
                out.extend_from_slice(&id.to_be_bytes());
                out.push(u8::from(*commit));
            }
            Frame::Ses(ses) => encode_ses(ses, out)?,
            Frame::Note(Note::Ready) => out.extend_from_slice(&[KIND_NOTE, NOTE_READY]),
            Frame::Note(Note::StoreState(state)) => out.extend_from_slice(&[KIND_NOTE, NOTE_STORE_STATE, *state as u8]),
            Frame::Note(Note::PaneCount(count)) => {
                out.extend_from_slice(&[KIND_NOTE, NOTE_PANE_COUNT]);
                out.extend_from_slice(&count.to_be_bytes());
            }
            Frame::Note(Note::Exiting(reason)) => out.extend_from_slice(&[KIND_NOTE, NOTE_EXITING, *reason as u8]),
            Frame::Note(Note::CtlDone { ctl, outcome }) => {
                out.extend_from_slice(&[KIND_NOTE, NOTE_CTL_DONE]);
                out.extend_from_slice(&ctl.to_be_bytes());
                out.push(*outcome as u8);
            }
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
                        let max_image_mib = fields.u32()?;
                        let sessions = SessionLimits {
                            attach_tail_lines: fields.u32()?,
                            history_max_lines: fields.u32()?,
                            snapshot_max_bytes: fields.u32()?,
                            writer_timeout_ms: fields.u32()?,
                        };
                        Frame::Ctl(Ctl::Init { version, max_panes, max_scrollback_lines, max_image_mib, sessions })
                    }
                    OP_ATTACH => Frame::Ctl(Ctl::Attach { socket: fields.socket()? }),
                    OP_DETACH => Frame::Ctl(Ctl::Detach { socket: fields.socket()? }),
                    OP_UNLOCK => {
                        let ctl = fields.u64()?;
                        let kind = fields.kind()?;
                        Frame::Ctl(Ctl::Unlock { ctl, kind, key: fields.key()? })
                    }
                    OP_LOCK => Frame::Ctl(Ctl::Lock { ctl: fields.u64()? }),
                    OP_SHUTDOWN => Frame::Ctl(Ctl::Shutdown),
                    OP_RESET_SECRETS => Frame::Ctl(Ctl::ResetSecrets { ctl: fields.u64()? }),
                    OP_FILE_SERVE => {
                        let request = fields.id()?;
                        let what = match fields.u8()? {
                            0 => FileWhat::Stat,
                            1 => FileWhat::Read,
                            _ => return Err(WireError::BadValue("a file request asks for 0 (stat) or 1 (read)")),
                        };
                        let token = std::str::from_utf8(fields.take(TOKEN_BYTES)?)
                            .map_err(|_| WireError::BadValue("a token is 32 lower-case hex digits"))?;
                        if !valid_token(token) {
                            return Err(WireError::BadValue("a token is 32 lower-case hex digits"));
                        }
                        let token = token.to_owned();
                        let offset = fields.u64()?;
                        let length = fields.u32()?;
                        match what {
                            FileWhat::Stat if offset != 0 || length != 0 => {
                                return Err(WireError::BadValue("a stat has no offset and no length"));
                            }
                            FileWhat::Read if length == 0 || usize::try_from(length).is_ok_and(|l| l > FILE_PIECE) => {
                                return Err(WireError::BadValue("a read has a length of 1 to 1 MiB"));
                            }
                            _ => {}
                        }
                        Frame::Ctl(Ctl::FileServe { request, what, token, offset, length })
                    }
                    OP_UPLOAD_OPEN => {
                        let id = fields.id()?;
                        let size = fields.u64()?;
                        let folder_len = usize::from(fields.u16()?);
                        if folder_len == 0 || folder_len > FOLDER_BYTES {
                            return Err(WireError::BadValue("a folder is 1 to 1024 bytes"));
                        }
                        let folder = text(fields.take(folder_len)?)?;
                        let name = text(fields.rest())?;
                        if name.is_empty() || name.len() > NAME_BYTES {
                            return Err(WireError::BadValue("a file name is 1 to 255 bytes"));
                        }
                        Frame::Ctl(Ctl::UploadOpen { id, size, folder, name })
                    }
                    OP_UPLOAD_PIECE => {
                        let id = fields.id()?;
                        let bytes = fields.rest();
                        if bytes.is_empty() || bytes.len() > FILE_PIECE {
                            return Err(WireError::BadValue("a piece is 1 byte to 1 MiB"));
                        }
                        Frame::Ctl(Ctl::UploadPiece { id, bytes: bytes.to_vec() })
                    }
                    OP_UPLOAD_CLOSE => {
                        let id = fields.id()?;
                        let commit = match fields.u8()? {
                            0 => false,
                            1 => true,
                            _ => return Err(WireError::BadValue("the commit flag is not 0 or 1")),
                        };
                        Frame::Ctl(Ctl::UploadClose { id, commit })
                    }
                    OP_PROVISION => {
                        let ctl = fields.u64()?;
                        let count = usize::from(fields.u8()?);
                        if count > MAX_PROVISION_SLOTS {
                            return Err(WireError::BadValue("a provision carries at most four slot keys"));
                        }
                        let mut slots = Vec::with_capacity(count);
                        for _ in 0..count {
                            slots.push(fields.slot_key()?);
                        }
                        Frame::Ctl(Ctl::Provision { ctl, slots })
                    }
                    OP_SET_SLOT => {
                        let ctl = fields.u64()?;
                        let slot = fields.slot_key()?;
                        let opening = match fields.u8()? {
                            0 => None,
                            1 => Some(OpeningKey { kind: fields.kind()?, key: fields.key()? }),
                            _ => return Err(WireError::BadValue("the opening-key flag is not 0 or 1")),
                        };
                        Frame::Ctl(Ctl::SetSlot { ctl, slot, opening })
                    }
                    OP_REMOVE_SLOT => {
                        let ctl = fields.u64()?;
                        let kind = fields.kind()?;
                        Frame::Ctl(Ctl::RemoveSlot { ctl, kind, id: fields.slot_id()? })
                    }
                    op => return Err(WireError::UnknownOp { kind, op }),
                }
            }
            KIND_NOTE => match fields.u8()? {
                NOTE_READY => Frame::Note(Note::Ready),
                NOTE_STORE_STATE => Frame::Note(Note::StoreState(match fields.u8()? {
                    0 => StoreState::Absent,
                    1 => StoreState::Locked,
                    2 => StoreState::Unlocked,
                    _ => return Err(WireError::BadValue("a store state is not 0, 1 or 2")),
                })),
                NOTE_PANE_COUNT => Frame::Note(Note::PaneCount(fields.u32()?)),
                NOTE_EXITING => Frame::Note(Note::Exiting(match fields.u8()? {
                    0 => ExitReason::Clean,
                    1 => ExitReason::Failure,
                    _ => return Err(WireError::BadValue("an exit reason is not 0 or 1")),
                })),
                NOTE_CTL_DONE => {
                    let ctl = fields.u64()?;
                    let outcome = match fields.u8()? {
                        0 => CtlOutcome::Done,
                        1 => CtlOutcome::Locked,
                        2 => CtlOutcome::Refused,
                        3 => CtlOutcome::Failed,
                        4 => CtlOutcome::Absent,
                        _ => return Err(WireError::BadValue("a control outcome is not 0 to 4")),
                    };
                    Frame::Note(Note::CtlDone { ctl, outcome })
                }
                op => return Err(WireError::UnknownOp { kind, op }),
            },
            KIND_SES => Frame::Ses(decode_ses(&mut fields)?),
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
