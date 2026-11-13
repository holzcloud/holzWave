//! The vocabulary of the device socket: what a client says to a server about the sessions that live there, what the server
//! answers and tells on its own, and the binary frames that carry terminal bytes. The server and the client both use this
//! module, so the two ends cannot drift apart.
//!
//! The socket carries two kinds of message and nothing else.
//!
//! * **Text** is one JSON object. A client sends `{"id":n,"op":"<name>", ...}` with an `id` from 1 to [`MAX_ID`] and the
//!   members of its operation and no other member. The server answers `{"id":n,"ok":{...}}` or `{"id":n,"err":"<word>"}` and
//!   tells things on its own as `{"event":"<name>", ...}`. An error is one of the fixed words of [`ErrorWord`]; it never echoes
//!   anything the client sent. A request whose `id` cannot be read is answered with the id 0.
//! * **Binary** carries terminal bytes. From the server: a tag byte ([`TAG_SNAPSHOT`] or [`TAG_LIVE`]), the session as a
//!   big-endian `u32`, a sequence number as a big-endian `u32` and the bytes. From the client: [`TAG_INPUT`], the session and the
//!   bytes. A snapshot chunk and a live chunk share one running sequence per attach.
//!
//! Every size and count has a name here. Parsing is strict: an unknown operation, an unknown or missing member, a value of the
//! wrong type or range, and a message over its size are refused with their own [`ParseError`], and nothing here panics on any
//! input. Nothing in this module does any input or output.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::pane::{MAX_DIM, MIN_COLS, MIN_ROWS};

// ---------------------------------------------------------------------------------------------------------------------
// Limits, tags and close codes
// ---------------------------------------------------------------------------------------------------------------------

/// The most bytes of a text message of a client.
pub const MAX_TEXT: usize = 4096;
/// The most bytes of a text message of the server. Only a list can come near it: [`Reply::encode`] leaves out the sessions of
/// the end of a list that would not fit.
pub const MAX_REPLY: usize = 1024 * 1024;
/// The most bytes of input one binary frame carries.
pub const MAX_INPUT: usize = 64 * 1024;
/// The most bytes of terminal output one binary frame carries.
pub const MAX_CHUNK: usize = 64 * 1024;
/// The bytes before the payload of a frame from the server: the tag, the session and the sequence number.
pub const CHUNK_HEAD: usize = 1 + 4 + 4;
/// The bytes before the payload of a frame from the client: the tag and the session.
pub const INPUT_HEAD: usize = 1 + 4;
/// The most bytes of any binary frame, either way.
pub const MAX_BINARY: usize = CHUNK_HEAD + MAX_CHUNK;
/// The most sessions one socket is attached to at once.
pub const MAX_ATTACHED: usize = 16;
/// The most requests of one socket that wait for an answer.
pub const MAX_PENDING: usize = 32;
/// The largest request number: the largest integer every JSON reader keeps exactly.
pub const MAX_ID: u64 = (1 << 53) - 1;
/// The most rows of scrollback an attach may ask for. A server may bring fewer.
pub const MAX_TAIL: u32 = 1_000_000;
/// The smallest and the largest size of a session, in cells: those of a pane.
pub const MIN_SIZE: (u16, u16) = (MIN_COLS, MIN_ROWS);
/// The most cells in either direction.
pub const MAX_SIZE: u16 = MAX_DIM;
/// The most sessions a list carries.
pub const MAX_LISTED: usize = 1024;
/// How often a client attached to a session says that it is alive, in seconds.
pub const BEAT_SECONDS: u64 = 10;

/// A server to client binary frame: a chunk of the screen at the time of the attach.
pub const TAG_SNAPSHOT: u8 = 0x01;
/// A server to client binary frame: bytes the program wrote after the attach.
pub const TAG_LIVE: u8 = 0x02;
/// A client to server binary frame: bytes for the program.
pub const TAG_INPUT: u8 = 0x10;

/// The token was revoked, expired or the account ended.
pub const CLOSE_REVOKED: u16 = 4401;
/// Too many sockets, or a peer that does not keep up.
pub const CLOSE_BUSY: u16 = 1013;
/// The server is stopping, or the feature was switched off.
pub const CLOSE_GOING_AWAY: u16 = 1001;
/// A message over its size.
pub const CLOSE_TOO_LARGE: u16 = 1009;
/// A peer that broke the rules too often.
pub const CLOSE_POLICY: u16 = 1008;
/// A message of a kind the socket does not take.
pub const CLOSE_TYPE: u16 = 1003;
/// A peer that broke the WebSocket protocol.
pub const CLOSE_PROTOCOL: u16 = 1002;
/// Text that is not UTF-8.
pub const CLOSE_ENCODING: u16 = 1007;
/// The worker of the person ended.
pub const CLOSE_RESTART: u16 = 1012;

/// The fixed word that goes with a close code. A code this module does not know has the word of going away.
pub fn close_word(code: u16) -> &'static str {
    match code {
        CLOSE_REVOKED => "revoked",
        CLOSE_BUSY => "busy",
        CLOSE_TOO_LARGE => "size",
        CLOSE_POLICY => "policy",
        CLOSE_TYPE => "type",
        CLOSE_PROTOCOL => "protocol",
        CLOSE_ENCODING => "encoding",
        CLOSE_RESTART => "restart",
        _ => "going away",
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------------------------------

/// Why a message was refused. A closed set of fixed words that hold no byte of the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParseError {
    /// The text is over [`MAX_TEXT`] bytes.
    TooLong,
    /// The text is not JSON.
    NotJson,
    /// The JSON is not an object.
    NotObject,
    /// The `id` is missing.
    MissingId,
    /// The `id` is not an integer from 1 to [`MAX_ID`].
    BadId,
    /// The `op` or the `event` is missing or is not text.
    MissingOp,
    /// The operation or the event is not one the vocabulary has.
    UnknownOp,
    /// A member the operation does not have.
    UnknownMember,
    /// A member the operation needs is missing.
    MissingMember,
    /// A member has the wrong type or is out of the range of its type.
    WrongType,
    /// A size outside the clamps of a pane.
    BadSize,
    /// A tail above [`MAX_TAIL`].
    TailTooLarge,
    /// A session number of 0.
    BadSession,
    /// A binary frame with a tag that is not one of the three.
    BadTag,
    /// A binary frame that is too short for its head or has no bytes.
    BadLength,
    /// A binary frame over [`MAX_BINARY`] bytes.
    TooLarge,
    /// An error word that is not one of the six.
    BadWord,
}

impl ParseError {
    /// The fixed word.
    pub fn word(self) -> &'static str {
        match self {
            ParseError::TooLong => "text too long",
            ParseError::NotJson => "not json",
            ParseError::NotObject => "not an object",
            ParseError::MissingId => "missing id",
            ParseError::BadId => "bad id",
            ParseError::MissingOp => "missing operation",
            ParseError::UnknownOp => "unknown operation",
            ParseError::UnknownMember => "unknown member",
            ParseError::MissingMember => "missing member",
            ParseError::WrongType => "wrong type",
            ParseError::BadSize => "bad size",
            ParseError::TailTooLarge => "tail too large",
            ParseError::BadSession => "bad session",
            ParseError::BadTag => "bad tag",
            ParseError::BadLength => "bad length",
            ParseError::TooLarge => "too large",
            ParseError::BadWord => "bad word",
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

impl std::error::Error for ParseError {}

/// A request that was refused, with the number to answer it by (0 when the message had no readable number).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejected {
    /// The request number, or 0.
    pub id: u64,
    /// Why.
    pub error: ParseError,
}

/// Why the server did not do what was asked. The text on the wire is the word and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorWord {
    /// No such session on this server for this person: an id that never existed, one that ended and one that is not theirs.
    NotFound,
    /// Understood and not allowed: not the writer, a snapshot over its cap, a pane that cannot start.
    Refused,
    /// The server cannot answer now.
    Busy,
    /// A cap is reached.
    TooMany,
    /// The request is not valid.
    BadRequest,
    /// The feature is off.
    Disabled,
}

impl ErrorWord {
    /// All six, in the order of the vocabulary.
    pub const ALL: [ErrorWord; 6] = [
        ErrorWord::NotFound,
        ErrorWord::Refused,
        ErrorWord::Busy,
        ErrorWord::TooMany,
        ErrorWord::BadRequest,
        ErrorWord::Disabled,
    ];

    /// The word on the wire.
    pub fn word(self) -> &'static str {
        match self {
            ErrorWord::NotFound => "not_found",
            ErrorWord::Refused => "refused",
            ErrorWord::Busy => "busy",
            ErrorWord::TooMany => "too_many",
            ErrorWord::BadRequest => "bad_request",
            ErrorWord::Disabled => "disabled",
        }
    }

    /// The error a word names.
    pub fn from_word(word: &str) -> Result<ErrorWord, ParseError> {
        ErrorWord::ALL.into_iter().find(|w| w.word() == word).ok_or(ParseError::BadWord)
    }
}

impl fmt::Display for ErrorWord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

impl std::error::Error for ErrorWord {}

// ---------------------------------------------------------------------------------------------------------------------
// What a client says
// ---------------------------------------------------------------------------------------------------------------------

/// One operation of a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// The sessions of the person.
    List,
    /// A new session of this size; the client is its writer.
    New { cols: u16, rows: u16 },
    /// Attach to a session: the screen and `tail` rows of scrollback (the server's default when `None`), then live bytes.
    Attach { session: u32, tail: Option<u32> },
    /// Leave a session.
    Detach { session: u32 },
    /// Resize a session; only the writer may.
    Resize { session: u32, cols: u16, rows: u16 },
    /// Take the right to write.
    Takeover { session: u32 },
    /// End the program of a session.
    End { session: u32 },
    /// A sign of life. With a session it is for that one; without, it is for every session this socket is attached to.
    Beat { session: Option<u32> },
}

impl Op {
    /// The name of the operation on the wire.
    pub fn name(&self) -> &'static str {
        match self {
            Op::List => "list",
            Op::New { .. } => "new",
            Op::Attach { .. } => "attach",
            Op::Detach { .. } => "detach",
            Op::Resize { .. } => "resize",
            Op::Takeover { .. } => "takeover",
            Op::End { .. } => "end",
            Op::Beat { .. } => "beat",
        }
    }
}

/// A request of a client: a number and an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    /// The number the answer carries, from 1 to [`MAX_ID`].
    pub id: u64,
    /// What is asked.
    pub op: Op,
}

/// The members of a request that are left to read. Taking a member removes it, and what is left at the end is unknown.
struct Members(Map<String, Value>);

impl Members {
    fn take(&mut self, name: &str) -> Option<Value> {
        self.0.remove(name)
    }

    fn number(&mut self, name: &str, max: u64, required: bool) -> Result<Option<u64>, ParseError> {
        match self.take(name) {
            None if required => Err(ParseError::MissingMember),
            None => Ok(None),
            Some(value) => match value.as_u64() {
                Some(n) if n <= max => Ok(Some(n)),
                _ => Err(ParseError::WrongType),
            },
        }
    }

    fn session(&mut self, required: bool) -> Result<Option<u32>, ParseError> {
        match self.number("session", u64::from(u32::MAX), required)? {
            Some(0) => Err(ParseError::BadSession),
            Some(n) => Ok(u32::try_from(n).ok()),
            None => Ok(None),
        }
    }

    fn dimension(&mut self, name: &str, min: u16) -> Result<u16, ParseError> {
        // A value that is not a number or does not fit 16 bits is a wrong type; one that fits and is out of the clamps is a bad size.
        let n = self.number(name, u64::from(u16::MAX), true)?.unwrap_or(0);
        let n = u16::try_from(n).map_err(|_| ParseError::WrongType)?;
        if (min..=MAX_SIZE).contains(&n) { Ok(n) } else { Err(ParseError::BadSize) }
    }

    fn finished(self) -> Result<(), ParseError> {
        if self.0.is_empty() { Ok(()) } else { Err(ParseError::UnknownMember) }
    }
}

impl Request {
    /// Reads one request from a text message.
    pub fn parse(text: &str) -> Result<Request, Rejected> {
        let rejected = |id: u64, error: ParseError| Rejected { id, error };
        if text.len() > MAX_TEXT {
            return Err(rejected(0, ParseError::TooLong));
        }
        let Ok(value) = serde_json::from_str::<Value>(text) else { return Err(rejected(0, ParseError::NotJson)) };
        let Value::Object(map) = value else { return Err(rejected(0, ParseError::NotObject)) };
        let mut members = Members(map);
        let id = match members.take("id") {
            None => return Err(rejected(0, ParseError::MissingId)),
            Some(value) => match value.as_u64() {
                Some(n) if (1..=MAX_ID).contains(&n) => n,
                _ => return Err(rejected(0, ParseError::BadId)),
            },
        };
        let fail = |error: ParseError| rejected(id, error);
        let Some(Value::String(name)) = members.take("op") else { return Err(fail(ParseError::MissingOp)) };
        let op = match name.as_str() {
            "list" => Op::List,
            "new" => Op::New {
                cols: members.dimension("cols", MIN_COLS).map_err(fail)?,
                rows: members.dimension("rows", MIN_ROWS).map_err(fail)?,
            },
            "attach" => {
                let session = members.session(true).map_err(fail)?.unwrap_or(0);
                let tail = match members.number("tail", u64::from(u32::MAX), false).map_err(fail)? {
                    Some(n) if n > u64::from(MAX_TAIL) => return Err(fail(ParseError::TailTooLarge)),
                    Some(n) => u32::try_from(n).ok(),
                    None => None,
                };
                Op::Attach { session, tail }
            }
            "detach" => Op::Detach { session: members.session(true).map_err(fail)?.unwrap_or(0) },
            "takeover" => Op::Takeover { session: members.session(true).map_err(fail)?.unwrap_or(0) },
            "end" => Op::End { session: members.session(true).map_err(fail)?.unwrap_or(0) },
            "resize" => {
                let session = members.session(true).map_err(fail)?.unwrap_or(0);
                Op::Resize {
                    session,
                    cols: members.dimension("cols", MIN_COLS).map_err(fail)?,
                    rows: members.dimension("rows", MIN_ROWS).map_err(fail)?,
                }
            }
            "beat" => Op::Beat { session: members.session(false).map_err(fail)? },
            _ => return Err(fail(ParseError::UnknownOp)),
        };
        members.finished().map_err(fail)?;
        Ok(Request { id, op })
    }

    /// The text of the request. A request that was built by hand with a number outside 1 to [`MAX_ID`] is not checked here:
    /// the server refuses it.
    pub fn encode(&self) -> String {
        let mut map = Map::new();
        map.insert("id".into(), self.id.into());
        map.insert("op".into(), self.op.name().into());
        match self.op {
            Op::List | Op::Beat { session: None } => {}
            Op::New { cols, rows } => {
                map.insert("cols".into(), cols.into());
                map.insert("rows".into(), rows.into());
            }
            Op::Attach { session, tail } => {
                map.insert("session".into(), session.into());
                if let Some(tail) = tail {
                    map.insert("tail".into(), tail.into());
                }
            }
            Op::Detach { session } | Op::Takeover { session } | Op::End { session } | Op::Beat { session: Some(session) } => {
                map.insert("session".into(), session.into());
            }
            Op::Resize { session, cols, rows } => {
                map.insert("session".into(), session.into());
                map.insert("cols".into(), cols.into());
                map.insert("rows".into(), rows.into());
            }
        }
        Value::Object(map).to_string()
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// What the server says
// ---------------------------------------------------------------------------------------------------------------------

/// A client's own view of the right to write in a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriterView {
    /// This client's input reaches the program.
    Writing,
    /// Another client writes; this one watches and its input is dropped.
    Watching,
    /// Nobody writes; input is dropped until a client takes over.
    Free,
}

impl WriterView {
    /// The word on the wire.
    pub fn word(self) -> &'static str {
        match self {
            WriterView::Writing => "writing",
            WriterView::Watching => "watching",
            WriterView::Free => "free",
        }
    }
}

/// One line of the list of sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEntry {
    /// The number of the session on this server.
    pub id: u32,
    /// The program's own title, bounded; empty when it set none.
    pub title: String,
    /// The program's working directory as the shell reported it; empty when it did not.
    pub cwd: String,
    /// The size in cells.
    pub cols: u16,
    /// The size in cells.
    pub rows: u16,
    /// The start, in seconds since the Unix epoch.
    pub started: u64,
    /// How many clients are attached.
    pub attached: u16,
    /// Whether a client holds the right to write.
    pub writer_held: bool,
}

/// What the server answers when it did what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The sessions of the person.
    List(Vec<SessionEntry>),
    /// A session was made and the asker is its writer.
    Created { session: u32, writer: WriterView },
    /// The attach was accepted. The client sets its grid to `cols` by `rows`, then feeds the `snapshot` bytes that follow as
    /// binary frames, then the live bytes. `more` says that older lines exist beyond the tail that was sent.
    Attached { session: u32, cols: u16, rows: u16, writer: WriterView, more: bool, snapshot: u32 },
    /// Done: a detach, a resize, a takeover, a beat or an end.
    Done,
}

/// Something the server tells on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// This client's own view of the right to write changed.
    Writer { session: u32, state: WriterView },
    /// The program ended, with its status when it could be read.
    Exited { session: u32, code: Option<u32> },
    /// The stream of this client for the session was lost: attach again.
    Resync { session: u32 },
    /// The grid of the session changed.
    Size { session: u32, cols: u16, rows: u16 },
    /// Input this client sent was dropped because the program in the session leaves too much of its input unread. Said at
    /// most about once a second while it lasts.
    Dropped { session: u32 },
}

impl Event {
    /// The name on the wire.
    pub fn name(&self) -> &'static str {
        match self {
            Event::Writer { .. } => "writer",
            Event::Exited { .. } => "exited",
            Event::Resync { .. } => "resync",
            Event::Size { .. } => "size",
            Event::Dropped { .. } => "dropped",
        }
    }

    /// The session the event is about.
    pub fn session(&self) -> u32 {
        match self {
            Event::Writer { session, .. }
            | Event::Exited { session, .. }
            | Event::Resync { session }
            | Event::Size { session, .. }
            | Event::Dropped { session } => *session,
        }
    }
}

/// A text message of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The answer to the request with this number.
    Answer { id: u64, result: Result<Answer, ErrorWord> },
    /// Something that happened.
    Event(Event),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListBody {
    sessions: Vec<SessionEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachedBody {
    session: u32,
    cols: u16,
    rows: u16,
    writer: WriterView,
    more: bool,
    snapshot: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreatedBody {
    session: u32,
    writer: WriterView,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DoneBody {}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum OkBody {
    List(ListBody),
    Attached(AttachedBody),
    Created(CreatedBody),
    Done(DoneBody),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OkText {
    id: u64,
    ok: OkBody,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrText {
    id: u64,
    err: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase", deny_unknown_fields)]
enum EventText {
    Writer { session: u32, state: WriterView },
    Exited { session: u32, code: Option<u32> },
    Resync { session: u32 },
    Size { session: u32, cols: u16, rows: u16 },
    Dropped { session: u32 },
}

impl From<Event> for EventText {
    fn from(event: Event) -> EventText {
        match event {
            Event::Writer { session, state } => EventText::Writer { session, state },
            Event::Exited { session, code } => EventText::Exited { session, code },
            Event::Resync { session } => EventText::Resync { session },
            Event::Size { session, cols, rows } => EventText::Size { session, cols, rows },
            Event::Dropped { session } => EventText::Dropped { session },
        }
    }
}

impl From<EventText> for Event {
    fn from(event: EventText) -> Event {
        match event {
            EventText::Writer { session, state } => Event::Writer { session, state },
            EventText::Exited { session, code } => Event::Exited { session, code },
            EventText::Resync { session } => Event::Resync { session },
            EventText::Size { session, cols, rows } => Event::Size { session, cols, rows },
            EventText::Dropped { session } => Event::Dropped { session },
        }
    }
}

impl Reply {
    /// The text of the message, at most [`MAX_REPLY`] bytes: a list that would be longer is cut after the last session that fits.
    pub fn encode(&self) -> String {
        if let Reply::Answer { id, result: Ok(Answer::List(sessions)) } = self {
            let mut sessions = sessions.clone();
            loop {
                let text = serde_json::to_string(&OkText { id: *id, ok: OkBody::List(ListBody { sessions: sessions.clone() }) })
                    .unwrap_or_default();
                if text.len() <= MAX_REPLY || sessions.is_empty() {
                    return text;
                }
                let keep = sessions.len() / 2;
                sessions.truncate(keep);
            }
        }
        let text = match self {
            Reply::Answer { id, result: Ok(answer) } => {
                let ok = match answer {
                    Answer::List(sessions) => OkBody::List(ListBody { sessions: sessions.clone() }),
                    Answer::Created { session, writer } => {
                        OkBody::Created(CreatedBody { session: *session, writer: *writer })
                    }
                    Answer::Attached { session, cols, rows, writer, more, snapshot } => OkBody::Attached(AttachedBody {
                        session: *session,
                        cols: *cols,
                        rows: *rows,
                        writer: *writer,
                        more: *more,
                        snapshot: *snapshot,
                    }),
                    Answer::Done => OkBody::Done(DoneBody {}),
                };
                serde_json::to_string(&OkText { id: *id, ok })
            }
            Reply::Answer { id, result: Err(error) } => {
                serde_json::to_string(&ErrText { id: *id, err: error.word().to_owned() })
            }
            Reply::Event(event) => serde_json::to_string(&EventText::from(*event)),
        };
        // A value of these types holds only numbers, booleans and text; serialising it cannot fail.
        text.unwrap_or_default()
    }

    /// Reads one text message of the server, as a client does. The server is not trusted: a text over [`MAX_REPLY`] bytes is
    /// refused, and so is any member or shape the vocabulary does not have.
    pub fn parse(text: &str) -> Result<Reply, ParseError> {
        if text.len() > MAX_REPLY {
            return Err(ParseError::TooLong);
        }
        let Ok(value) = serde_json::from_str::<Value>(text) else { return Err(ParseError::NotJson) };
        let Value::Object(map) = &value else { return Err(ParseError::NotObject) };
        if map.contains_key("event") {
            let event: EventText = serde_json::from_value(value).map_err(|_| ParseError::WrongType)?;
            let event = Event::from(event);
            return if event.session() == 0 { Err(ParseError::BadSession) } else { Ok(Reply::Event(event)) };
        }
        if map.contains_key("err") {
            let err: ErrText = serde_json::from_value(value).map_err(|_| ParseError::WrongType)?;
            return Ok(Reply::Answer { id: err.id, result: Err(ErrorWord::from_word(&err.err)?) });
        }
        if map.contains_key("ok") {
            let ok: OkText = serde_json::from_value(value).map_err(|_| ParseError::WrongType)?;
            let answer = match ok.ok {
                OkBody::List(body) if body.sessions.len() <= MAX_LISTED => Answer::List(body.sessions),
                OkBody::List(_) => return Err(ParseError::TooLarge),
                OkBody::Attached(b) => Answer::Attached {
                    session: b.session,
                    cols: b.cols,
                    rows: b.rows,
                    writer: b.writer,
                    more: b.more,
                    snapshot: b.snapshot,
                },
                OkBody::Created(b) => Answer::Created { session: b.session, writer: b.writer },
                OkBody::Done(_) => Answer::Done,
            };
            return Ok(Reply::Answer { id: ok.id, result: Ok(answer) });
        }
        Err(ParseError::MissingOp)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Binary frames
// ---------------------------------------------------------------------------------------------------------------------

/// Which stream a chunk of the server belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// The screen at the time of the attach.
    Snapshot,
    /// What the program wrote after it.
    Live,
}

impl Stream {
    /// The tag of the stream.
    pub fn tag(self) -> u8 {
        match self {
            Stream::Snapshot => TAG_SNAPSHOT,
            Stream::Live => TAG_LIVE,
        }
    }
}

/// One binary frame of the server, read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk<'a> {
    /// Snapshot or live.
    pub stream: Stream,
    /// The session the bytes belong to.
    pub session: u32,
    /// The running number of the chunk since the attach: the snapshot starts it, the live bytes continue it.
    pub seq: u32,
    /// 1 to [`MAX_CHUNK`] bytes.
    pub bytes: &'a [u8],
}

/// The frame of a chunk. A chunk with no bytes or over [`MAX_CHUNK`] is refused.
pub fn encode_chunk(stream: Stream, session: u32, seq: u32, bytes: &[u8]) -> Result<Vec<u8>, ParseError> {
    if bytes.is_empty() {
        return Err(ParseError::BadLength);
    }
    if bytes.len() > MAX_CHUNK {
        return Err(ParseError::TooLarge);
    }
    let mut out = Vec::with_capacity(CHUNK_HEAD + bytes.len());
    out.push(stream.tag());
    out.extend_from_slice(&session.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(out)
}

impl<'a> Chunk<'a> {
    /// Reads a binary frame of the server.
    pub fn parse(frame: &'a [u8]) -> Result<Chunk<'a>, ParseError> {
        if frame.len() > MAX_BINARY {
            return Err(ParseError::TooLarge);
        }
        let stream = match frame.first() {
            Some(&TAG_SNAPSHOT) => Stream::Snapshot,
            Some(&TAG_LIVE) => Stream::Live,
            Some(_) => return Err(ParseError::BadTag),
            None => return Err(ParseError::BadLength),
        };
        if frame.len() <= CHUNK_HEAD {
            return Err(ParseError::BadLength);
        }
        let session = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]);
        let seq = u32::from_be_bytes([frame[5], frame[6], frame[7], frame[8]]);
        if session == 0 {
            return Err(ParseError::BadSession);
        }
        Ok(Chunk { stream, session, seq, bytes: &frame[CHUNK_HEAD..] })
    }
}

/// One binary frame of a client, read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Input<'a> {
    /// The session the bytes are for.
    pub session: u32,
    /// 1 to [`MAX_INPUT`] bytes.
    pub bytes: &'a [u8],
}

/// The frame of input for a session. No bytes, or more than [`MAX_INPUT`], is refused.
pub fn encode_input(session: u32, bytes: &[u8]) -> Result<Vec<u8>, ParseError> {
    if bytes.is_empty() {
        return Err(ParseError::BadLength);
    }
    if bytes.len() > MAX_INPUT {
        return Err(ParseError::TooLarge);
    }
    let mut out = Vec::with_capacity(INPUT_HEAD + bytes.len());
    out.push(TAG_INPUT);
    out.extend_from_slice(&session.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(out)
}

impl<'a> Input<'a> {
    /// Reads a binary frame of a client.
    pub fn parse(frame: &'a [u8]) -> Result<Input<'a>, ParseError> {
        if frame.len() > INPUT_HEAD + MAX_INPUT {
            return Err(ParseError::TooLarge);
        }
        match frame.first() {
            Some(&TAG_INPUT) => {}
            Some(_) => return Err(ParseError::BadTag),
            None => return Err(ParseError::BadLength),
        }
        if frame.len() <= INPUT_HEAD {
            return Err(ParseError::BadLength);
        }
        let session = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]);
        if session == 0 {
            return Err(ParseError::BadSession);
        }
        Ok(Input { session, bytes: &frame[INPUT_HEAD..] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requests() -> Vec<Request> {
        let ops = [
            Op::List,
            Op::New { cols: 80, rows: 24 },
            Op::Attach { session: 3, tail: None },
            Op::Attach { session: 3, tail: Some(200) },
            Op::Attach { session: u32::MAX, tail: Some(MAX_TAIL) },
            Op::Detach { session: 3 },
            Op::Resize { session: 3, cols: MIN_COLS, rows: MAX_DIM },
            Op::Takeover { session: 3 },
            Op::End { session: 3 },
            Op::Beat { session: None },
            Op::Beat { session: Some(7) },
        ];
        ops.into_iter().enumerate().map(|(i, op)| Request { id: i as u64 + 1, op }).collect()
    }

    #[test]
    fn every_operation_of_a_client_round_trips() {
        for request in requests() {
            let text = request.encode();
            assert_eq!(Request::parse(&text), Ok(request), "{text}");
        }
        let largest = Request { id: MAX_ID, op: Op::List };
        assert_eq!(Request::parse(&largest.encode()), Ok(largest));
    }

    #[test]
    fn the_eight_operations_have_the_names_of_the_vocabulary() {
        let names: Vec<_> = requests().iter().map(|r| r.op.name()).collect();
        for name in ["list", "new", "attach", "detach", "resize", "takeover", "end", "beat"] {
            assert!(names.contains(&name), "{name}");
        }
    }

    fn refused(text: &str) -> Rejected {
        Request::parse(text).expect_err(text)
    }

    #[test]
    fn a_request_that_breaks_a_rule_is_refused_with_its_own_error() {
        let cases = [
            ("not json", ParseError::NotJson, 0),
            ("", ParseError::NotJson, 0),
            ("[1,2]", ParseError::NotObject, 0),
            ("\"list\"", ParseError::NotObject, 0),
            (r#"{"op":"list"}"#, ParseError::MissingId, 0),
            (r#"{"id":0,"op":"list"}"#, ParseError::BadId, 0),
            (r#"{"id":-1,"op":"list"}"#, ParseError::BadId, 0),
            (r#"{"id":"1","op":"list"}"#, ParseError::BadId, 0),
            (r#"{"id":1.5,"op":"list"}"#, ParseError::BadId, 0),
            (r#"{"id":9007199254740992,"op":"list"}"#, ParseError::BadId, 0),
            (r#"{"id":1}"#, ParseError::MissingOp, 1),
            (r#"{"id":1,"op":7}"#, ParseError::MissingOp, 1),
            (r#"{"id":1,"op":"nuke"}"#, ParseError::UnknownOp, 1),
            (r#"{"id":1,"op":"list","extra":1}"#, ParseError::UnknownMember, 1),
            (r#"{"id":1,"op":"detach","session":3,"x":null}"#, ParseError::UnknownMember, 1),
            (r#"{"id":1,"op":"detach"}"#, ParseError::MissingMember, 1),
            (r#"{"id":1,"op":"new","cols":80}"#, ParseError::MissingMember, 1),
            (r#"{"id":1,"op":"detach","session":"3"}"#, ParseError::WrongType, 1),
            (r#"{"id":1,"op":"detach","session":4294967296}"#, ParseError::WrongType, 1),
            (r#"{"id":1,"op":"attach","session":3,"tail":null}"#, ParseError::WrongType, 1),
            (r#"{"id":1,"op":"detach","session":0}"#, ParseError::BadSession, 1),
            (r#"{"id":1,"op":"new","cols":1,"rows":24}"#, ParseError::BadSize, 1),
            (r#"{"id":1,"op":"new","cols":80,"rows":0}"#, ParseError::BadSize, 1),
            (r#"{"id":1,"op":"resize","session":3,"cols":1001,"rows":24}"#, ParseError::BadSize, 1),
            (r#"{"id":1,"op":"new","cols":70000,"rows":24}"#, ParseError::WrongType, 1),
            (r#"{"id":1,"op":"attach","session":3,"tail":1000001}"#, ParseError::TailTooLarge, 1),
            (r#"{"id":1,"op":"beat","session":0}"#, ParseError::BadSession, 1),
        ];
        for (text, error, id) in cases {
            assert_eq!(refused(text), Rejected { id, error }, "{text}");
        }
    }

    #[test]
    fn a_text_over_four_kib_is_refused_before_it_is_read() {
        let long = format!(r#"{{"id":1,"op":"list","pad":"{}"}}"#, "x".repeat(MAX_TEXT));
        assert_eq!(refused(&long), Rejected { id: 0, error: ParseError::TooLong });
        let edge = format!(r#"{{"id":1,"op":"list"{}}}"#, " ".repeat(MAX_TEXT - 20));
        assert_eq!(edge.len(), MAX_TEXT);
        assert_eq!(Request::parse(&edge), Ok(Request { id: 1, op: Op::List }));
    }

    #[test]
    fn nothing_panics_on_hostile_input() {
        let deep = format!("{}1{}", "[".repeat(2000), "]".repeat(2000));
        for text in [deep.as_str(), "{\"id\":", "{\"id\":1,\"op\":\"list\"", "\u{0}", "{\"id\":1e999,\"op\":\"list\"}", "{\"id\":1,\"op\":\"\u{0}\"}"] {
            let _ = Request::parse(text);
            let _ = Reply::parse(text);
        }
        for len in 0..=CHUNK_HEAD + 2 {
            let frame = vec![TAG_LIVE; len];
            let _ = Chunk::parse(&frame);
            let _ = Input::parse(&frame);
        }
    }

    #[test]
    fn the_clamps_are_those_of_a_pane() {
        assert_eq!(MIN_SIZE, (crate::pane::MIN_COLS, crate::pane::MIN_ROWS));
        assert_eq!(MAX_SIZE, crate::pane::MAX_DIM);
        let at = |cols: u16, rows: u16| Request::parse(&Request { id: 1, op: Op::New { cols, rows } }.encode());
        assert!(at(MIN_COLS, MIN_ROWS).is_ok() && at(MAX_DIM, MAX_DIM).is_ok());
        assert!(at(MIN_COLS - 1, MIN_ROWS).is_err() && at(MAX_DIM + 1, MIN_ROWS).is_err());
    }

    fn entry(id: u32) -> SessionEntry {
        SessionEntry {
            id,
            title: "vim: notes \u{e9}".into(),
            cwd: "/home/a b".into(),
            cols: 80,
            rows: 24,
            started: 1_800_000_000,
            attached: 2,
            writer_held: true,
        }
    }

    fn replies() -> Vec<Reply> {
        let mut out = vec![
            Reply::Answer { id: 1, result: Ok(Answer::List(vec![])) },
            Reply::Answer { id: 2, result: Ok(Answer::List(vec![entry(1), entry(2)])) },
            Reply::Answer { id: 3, result: Ok(Answer::Created { session: 9, writer: WriterView::Writing }) },
            Reply::Answer {
                id: 4,
                result: Ok(Answer::Attached {
                    session: 9,
                    cols: 80,
                    rows: 24,
                    writer: WriterView::Watching,
                    more: true,
                    snapshot: 4_194_304,
                }),
            },
            Reply::Answer { id: 5, result: Ok(Answer::Done) },
            Reply::Event(Event::Writer { session: 9, state: WriterView::Free }),
            Reply::Event(Event::Writer { session: 9, state: WriterView::Writing }),
            Reply::Event(Event::Writer { session: 9, state: WriterView::Watching }),
            Reply::Event(Event::Exited { session: 9, code: Some(0) }),
            Reply::Event(Event::Exited { session: 9, code: None }),
            Reply::Event(Event::Resync { session: 9 }),
            Reply::Event(Event::Size { session: 9, cols: 120, rows: 40 }),
            Reply::Event(Event::Dropped { session: 9 }),
        ];
        out.extend(ErrorWord::ALL.into_iter().enumerate().map(|(i, e)| Reply::Answer { id: i as u64 + 6, result: Err(e) }));
        out
    }

    #[test]
    fn every_answer_and_event_of_the_server_round_trips() {
        for reply in replies() {
            let text = reply.encode();
            assert!(text.len() <= MAX_REPLY, "{text}");
            assert_eq!(Reply::parse(&text), Ok(reply), "{text}");
        }
    }

    #[test]
    fn the_texts_are_the_ones_the_vocabulary_names() {
        assert_eq!(Reply::Answer { id: 5, result: Ok(Answer::Done) }.encode(), r#"{"id":5,"ok":{}}"#);
        assert_eq!(
            Reply::Answer { id: 5, result: Err(ErrorWord::NotFound) }.encode(),
            r#"{"id":5,"err":"not_found"}"#
        );
        assert_eq!(
            Reply::Event(Event::Writer { session: 4, state: WriterView::Free }).encode(),
            r#"{"event":"writer","session":4,"state":"free"}"#
        );
        assert_eq!(
            Reply::Event(Event::Exited { session: 4, code: None }).encode(),
            r#"{"event":"exited","session":4,"code":null}"#
        );
        assert_eq!(Reply::Event(Event::Dropped { session: 4 }).encode(), r#"{"event":"dropped","session":4}"#);
        for (word, error) in [
            ("not_found", ErrorWord::NotFound),
            ("refused", ErrorWord::Refused),
            ("busy", ErrorWord::Busy),
            ("too_many", ErrorWord::TooMany),
            ("bad_request", ErrorWord::BadRequest),
            ("disabled", ErrorWord::Disabled),
        ] {
            assert_eq!(ErrorWord::from_word(word), Ok(error));
            assert_eq!(error.word(), word);
            assert_eq!(error.to_string(), word);
        }
        assert_eq!(ErrorWord::from_word("not found"), Err(ParseError::BadWord));
    }

    #[test]
    fn a_reply_that_is_not_of_the_vocabulary_is_refused() {
        let cases = [
            (r#"{"id":1,"ok":{"x":1}}"#, ParseError::WrongType),
            (r#"{"id":1,"ok":{},"extra":1}"#, ParseError::WrongType),
            (r#"{"id":1,"err":"because"}"#, ParseError::BadWord),
            (r#"{"id":1,"err":7}"#, ParseError::WrongType),
            (r#"{"event":"nuke","session":1}"#, ParseError::WrongType),
            (r#"{"event":"resync"}"#, ParseError::WrongType),
            (r#"{"event":"resync","session":0}"#, ParseError::BadSession),
            (r#"{"event":"resync","session":1,"x":1}"#, ParseError::WrongType),
            (r#"{"event":"writer","session":1,"state":"typing"}"#, ParseError::WrongType),
            (r#"{"id":1}"#, ParseError::MissingOp),
            ("[]", ParseError::NotObject),
            ("nope", ParseError::NotJson),
        ];
        for (text, error) in cases {
            assert_eq!(Reply::parse(text), Err(error), "{text}");
        }
        let long = format!(r#"{{"id":1,"err":"{}"}}"#, "x".repeat(MAX_REPLY));
        assert_eq!(Reply::parse(&long), Err(ParseError::TooLong));
    }

    #[test]
    fn a_list_that_would_not_fit_is_cut_and_the_rest_still_parses() {
        let many: Vec<SessionEntry> = (1..=MAX_LISTED as u32)
            .map(|id| SessionEntry { title: "t".repeat(256), cwd: "/".repeat(4000), ..entry(id) })
            .collect();
        let text = Reply::Answer { id: 1, result: Ok(Answer::List(many)) }.encode();
        assert!(text.len() <= MAX_REPLY);
        let Ok(Reply::Answer { result: Ok(Answer::List(kept)), .. }) = Reply::parse(&text) else { panic!("a list") };
        assert!(!kept.is_empty() && kept.len() < MAX_LISTED);
        assert_eq!(kept[0].id, 1, "the sessions at the start of the list are the ones kept");
    }

    #[test]
    fn a_text_of_the_server_never_holds_what_the_client_sent() {
        // An error is the word and nothing else, whatever the request said.
        let text = Reply::Answer { id: 1, result: Err(ErrorWord::BadRequest) }.encode();
        assert_eq!(text, r#"{"id":1,"err":"bad_request"}"#);
        for error in ErrorWord::ALL {
            let text = Reply::Answer { id: 1, result: Err(error) }.encode();
            assert!(text.len() < 40, "{text}");
        }
    }

    #[test]
    fn binary_frames_round_trip_with_their_tags_and_numbers() {
        for stream in [Stream::Snapshot, Stream::Live] {
            for (session, seq, len) in [(1u32, 0u32, 1usize), (7, 41, 300), (u32::MAX, u32::MAX, MAX_CHUNK)] {
                let bytes = vec![0xa5; len];
                let frame = encode_chunk(stream, session, seq, &bytes).expect("encodes");
                assert_eq!(frame[0], stream.tag());
                assert_eq!(&frame[1..5], &session.to_be_bytes());
                assert_eq!(&frame[5..9], &seq.to_be_bytes());
                assert_eq!(Chunk::parse(&frame), Ok(Chunk { stream, session, seq, bytes: &bytes }));
            }
        }
        assert_eq!((TAG_SNAPSHOT, TAG_LIVE, TAG_INPUT), (0x01, 0x02, 0x10));
        for len in [1usize, 5, MAX_INPUT] {
            let bytes = vec![b'x'; len];
            let frame = encode_input(12, &bytes).expect("encodes");
            assert_eq!(frame[0], TAG_INPUT);
            assert_eq!(Input::parse(&frame), Ok(Input { session: 12, bytes: &bytes }));
        }
    }

    #[test]
    fn binary_frames_that_break_a_rule_are_refused_with_their_own_error() {
        assert_eq!(encode_chunk(Stream::Live, 1, 0, &[]), Err(ParseError::BadLength));
        assert_eq!(encode_chunk(Stream::Live, 1, 0, &vec![0; MAX_CHUNK + 1]), Err(ParseError::TooLarge));
        assert_eq!(encode_input(1, &[]), Err(ParseError::BadLength));
        assert_eq!(encode_input(1, &vec![0; MAX_INPUT + 1]), Err(ParseError::TooLarge));
        assert_eq!(Chunk::parse(&[]), Err(ParseError::BadLength));
        assert_eq!(Chunk::parse(&[TAG_LIVE, 0, 0, 0, 1, 0, 0, 0, 0]), Err(ParseError::BadLength));
        assert_eq!(Chunk::parse(&[0x33, 0, 0, 0, 1, 0, 0, 0, 0, 9]), Err(ParseError::BadTag));
        assert_eq!(Chunk::parse(&[TAG_INPUT, 0, 0, 0, 1, 0, 0, 0, 0, 9]), Err(ParseError::BadTag));
        assert_eq!(Chunk::parse(&[TAG_LIVE, 0, 0, 0, 0, 0, 0, 0, 0, 9]), Err(ParseError::BadSession));
        assert_eq!(Chunk::parse(&vec![TAG_LIVE; MAX_BINARY + 1]), Err(ParseError::TooLarge));
        assert_eq!(Input::parse(&[]), Err(ParseError::BadLength));
        assert_eq!(Input::parse(&[TAG_INPUT, 0, 0, 0, 1]), Err(ParseError::BadLength));
        assert_eq!(Input::parse(&[TAG_LIVE, 0, 0, 0, 1, 9]), Err(ParseError::BadTag));
        assert_eq!(Input::parse(&[0x7f, 0, 0, 0, 1, 9]), Err(ParseError::BadTag));
        assert_eq!(Input::parse(&[TAG_INPUT, 0, 0, 0, 0, 9]), Err(ParseError::BadSession));
        assert_eq!(Input::parse(&vec![TAG_INPUT; INPUT_HEAD + MAX_INPUT + 1]), Err(ParseError::TooLarge));
    }

    #[test]
    fn the_close_codes_have_fixed_words() {
        assert_eq!(CLOSE_REVOKED, 4401);
        assert_eq!(CLOSE_BUSY, 1013);
        assert_eq!(CLOSE_GOING_AWAY, 1001);
        for code in [CLOSE_REVOKED, CLOSE_BUSY, CLOSE_GOING_AWAY, CLOSE_TOO_LARGE, CLOSE_POLICY, CLOSE_TYPE, CLOSE_PROTOCOL, CLOSE_ENCODING, CLOSE_RESTART, 4999] {
            let word = close_word(code);
            assert!(!word.is_empty() && word.len() < 24);
        }
        assert_eq!(close_word(CLOSE_REVOKED), "revoked");
    }
}
