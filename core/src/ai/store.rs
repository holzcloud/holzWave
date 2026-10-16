//! Conversation persistence: where the chats of the AI panel live between runs.
//!
//! A conversation is stored by a [`ConversationStore`]. [`JsonDirStore`] keeps one compact JSON file per chat in one
//! directory, named `<id>.json`, in version 1 of the form described on [`FORMAT_VERSION`]. The store never calls a
//! provider, never opens a connection and never starts a thread: it keeps what the service hands it, and the service
//! decides when to save (at message boundaries, not per streamed delta).
//!
//! # Privacy defaults
//!
//! A saved chat is the longest-lived copy of what the user asked, what a model answered and, if nobody is careful,
//! what was in the terminal. Three rules keep that copy small.
//!
//! - **Owner only.** A chat file is written through [`crate::persist::write_atomic_private`], so on Unix it is created
//!   with mode 0600 by the temporary file itself, and the directory the store creates has mode 0700. On Windows a file
//!   inherits the access list of the per-user data folder; no mode is claimed there.
//! - **Context is a marker by default.** Terminal context that reached a message (`Part::Context`) and tool output
//!   (`Part::ToolResult`) are private text the user did not type and the model did not write. With the default
//!   [`ContextStorage::Marker`] the file keeps the hash, the item labels, the byte counts and the consent record of a
//!   context, and the fixed text [`NOT_STORED`] in place of a tool output. The caller passes
//!   [`ContextStorage::Text`] at every save to keep them. The text the user typed and the text the model wrote are
//!   always kept. The store does not inspect what it keeps: `ContextNote::stored` is only ever text from an approved,
//!   already redacted envelope, which the gate guarantees.
//! - **Chats are not synced.** The sync allow-list does not name the chat directory, and nothing here adds to it.
//!
//! [`ConversationStore::delete`] is an unlink. It removes the files of a chat; it does not overwrite their blocks, so
//! it makes no promise that the bytes are unrecoverable from the disk.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::de::{self, Deserializer, IgnoredAny};
use serde::{Deserialize, Serialize, Serializer};

use super::provider::Detail;
use super::types::{Message, Part, Size};
use crate::persist::{bad_path, read_limited, write_atomic_private};

/// The version of the file form this build writes and reads.
///
/// Version 1 is one JSON object per file, keys in this order: `version`, `id`, `title`, `created`, `updated`, then
/// `profile` and `model` when set, then `messages`, followed by one newline. A change to that form is a new version
/// with a migration in this module.
pub const FORMAT_VERSION: u32 = 1;
/// The largest chat file, in bytes. A larger conversation is refused on save; a larger file is never read.
pub const MAX_CHAT_BYTES: u64 = 8 * 1024 * 1024;
/// The most messages one chat holds.
pub const MAX_MESSAGES: usize = 2000;
/// The most chats one directory holds. A new chat is refused beyond it; an existing one can still be saved.
pub const MAX_CHATS: usize = 500;
/// The longest conversation id, in characters.
pub const MAX_ID_LEN: usize = 64;
/// The longest `profile` or `model` label, in bytes.
pub const MAX_LABEL: usize = 256;
/// What a tool output is replaced by when it is not stored.
pub const NOT_STORED: &str = "[tool output not stored]";

/// Names Windows reserves for devices, in any case and with any extension. An id equal to one cannot be a file name
/// there, so it is refused everywhere and a data directory can be copied between systems.
const DEVICE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

// ---- errors -------------------------------------------------------------------------------------------------------

/// Why a store call failed.
///
/// No variant carries chat content: a reason is a fixed phrase, a number or a file name, so an error can be logged or
/// shown to the user as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// A conversation id is not 1 to 64 characters of letters, digits, `_` and `-`, or is a Windows device name.
    InvalidId,
    /// A conversation breaks a rule of the form; the text says which, in fixed words.
    Invalid(&'static str),
    /// The encoded conversation is larger than the limit. The user should start a new chat.
    TooLarge {
        /// The size of the encoded conversation.
        bytes: u64,
        /// The limit.
        max: u64,
    },
    /// The directory holds as many chats as it may. The user should delete one.
    Full {
        /// The limit.
        max: usize,
    },
    /// A chat file cannot be used. It was kept (see the reason) and is reported, never quoted.
    Damaged {
        /// The chat.
        id: ConversationId,
        /// What is wrong, from the error category, the line and the column only.
        reason: String,
    },
    /// A chat file was written by a newer build. It is left exactly as it is.
    Newer {
        /// The chat.
        id: ConversationId,
        /// The version the file declares.
        found: u32,
        /// The version this build reads.
        supported: u32,
    },
    /// The operating system refused a read or a write.
    Io(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::InvalidId => f.write_str("the conversation id is not valid"),
            StoreError::Invalid(why) => write!(f, "the conversation cannot be stored: {why}"),
            StoreError::TooLarge { bytes, max } => {
                write!(f, "the conversation is {bytes} bytes, larger than the limit of {max} bytes")
            }
            StoreError::Full { max } => write!(f, "there are already {max} chats"),
            StoreError::Damaged { id, reason } => write!(f, "chat {id} cannot be used: {reason}"),
            StoreError::Newer { id, found, supported } => write!(
                f,
                "chat {id} was written by a newer version of the app (format {found}, this build reads {supported})"
            ),
            StoreError::Io(why) => write!(f, "chats: {why}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}

impl From<StoreError> for crate::Error {
    fn from(e: StoreError) -> Self {
        crate::Error::Io(io::Error::other(format!("chats: {e}")))
    }
}

// ---- the id -------------------------------------------------------------------------------------------------------

/// The id of a conversation, which is also its file name.
///
/// It is a validated type, not a string: 1 to [`MAX_ID_LEN`] characters of `A-Z a-z 0-9 _ -` that are not a Windows
/// device name (`CON`, `NUL`, `COM1` and so on, in any case), checked in [`ConversationId::parse`] and again when it
/// is deserialised. A value taken from the page therefore cannot name a
/// path outside the chat directory. An id is not a credential, so it may be printed.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ConversationId(String);

impl ConversationId {
    /// Checks `text` and wraps it.
    pub fn parse(text: &str) -> Result<Self, StoreError> {
        let shape = (1..=MAX_ID_LEN).contains(&text.len())
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            && !DEVICE_NAMES.iter().any(|device| device.eq_ignore_ascii_case(text));
        if shape { Ok(Self(text.to_owned())) } else { Err(StoreError::InvalidId) }
    }

    /// A fresh random id: 32 lower-case hex characters from the operating system's random source.
    pub fn generate() -> Result<Self, StoreError> {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).map_err(|e| StoreError::Io(format!("no random numbers: {e}")))?;
        let mut text = String::with_capacity(32);
        for byte in raw {
            for nibble in [byte >> 4, byte & 0x0f] {
                text.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
            }
        }
        Ok(Self(text))
    }

    /// The id as text; it is also the file stem.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConversationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ConversationId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ConversationId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(de::Error::custom)
    }
}

// ---- the data -----------------------------------------------------------------------------------------------------

/// Whether terminal context and tool output are written to the file as text or as a marker.
///
/// The caller passes it at every save; the store keeps no hidden state. The service maps the setting "keep terminal
/// context in saved chats" to it. See the module documentation for what the marker form keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContextStorage {
    /// Keep the hash, labels, byte counts and consent of a context, and replace tool output by [`NOT_STORED`].
    #[default]
    Marker,
    /// Keep the context text and the tool output as they are.
    Text,
}

/// One conversation as the store keeps it.
///
/// `created` and `updated` use the unit of `Message::ts`; the store only orders by `updated`. `profile` is the id of
/// the profile the chat is pinned to and `model` the model name. `Debug` prints the id and sizes, never the title or a
/// message.
#[derive(Clone)]
pub struct Conversation {
    /// The id, which is the file name.
    pub id: ConversationId,
    /// The title shown in the chat list. The store scrubs it of control and bidirectional characters.
    pub title: String,
    /// When the chat was started.
    pub created: i64,
    /// When the chat last changed.
    pub updated: i64,
    /// The profile the chat is pinned to.
    pub profile: Option<String>,
    /// The model the chat uses.
    pub model: Option<String>,
    /// The messages, oldest first.
    pub messages: Vec<Message>,
}

impl Conversation {
    /// An empty conversation with no title, last changed when it was created.
    pub fn new(id: ConversationId, created: i64) -> Self {
        Self { id, title: String::new(), created, updated: created, profile: None, model: None, messages: Vec::new() }
    }

    /// The fields the chat list shows.
    pub fn summary(&self) -> ConversationSummary {
        ConversationSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            created: self.created,
            updated: self.updated,
            profile: self.profile.clone(),
            model: self.model.clone(),
        }
    }
}

impl fmt::Debug for Conversation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Conversation")
            .field("id", &self.id)
            .field("title", &Size(self.title.len()))
            .field("created", &self.created)
            .field("updated", &self.updated)
            .field("profile", &self.profile)
            .field("model", &self.model)
            .field("messages", &self.messages.len())
            .finish()
    }
}

/// What the chat list shows of a conversation. `Debug` prints the id and the size of the title, not the title.
#[derive(Clone, Serialize)]
pub struct ConversationSummary {
    /// The id.
    pub id: ConversationId,
    /// The title.
    pub title: String,
    /// When the chat was started.
    pub created: i64,
    /// When the chat last changed.
    pub updated: i64,
    /// The profile the chat is pinned to.
    pub profile: Option<String>,
    /// The model the chat uses.
    pub model: Option<String>,
}

impl fmt::Debug for ConversationSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConversationSummary")
            .field("id", &self.id)
            .field("title", &Size(self.title.len()))
            .field("created", &self.created)
            .field("updated", &self.updated)
            .field("profile", &self.profile)
            .field("model", &self.model)
            .finish()
    }
}

/// The chats of a store, newest first, and what could not be listed.
#[derive(Debug, Default)]
pub struct Listing {
    /// The chats, newest `updated` first, ties by id.
    pub chats: Vec<ConversationSummary>,
    /// One line `"<id or file name>: <reason>"` for each file that was skipped. A note never quotes file content.
    pub notes: Vec<String>,
}

// ---- the trait ----------------------------------------------------------------------------------------------------

/// Where conversations are kept.
///
/// A store is safe to share between threads (`Arc<dyn ConversationStore>`). It serialises its own writes, a reader
/// always sees a whole file because a writer renames a finished file into place, and two saves of one chat resolve as
/// last writer wins: the service serialises the turns of one chat.
pub trait ConversationStore: Send + Sync {
    /// The chats in the store, newest first. A directory that does not exist yet is an empty listing.
    fn list(&self) -> Result<Listing, StoreError>;

    /// The chat with this id, or `Ok(None)` when there is none.
    fn load(&self, id: &ConversationId) -> Result<Option<Conversation>, StoreError>;

    /// Writes the whole conversation, atomically. The caller saves at message boundaries, never per delta, and passes
    /// the [`ContextStorage`] every time.
    fn save(&self, conversation: &Conversation, storage: ContextStorage) -> Result<(), StoreError>;

    /// Removes the chat: its file and any temporary or kept-aside copy of it. `true` when at least one file was
    /// removed.
    ///
    /// This is an unlink, not a secure overwrite.
    fn delete(&self, id: &ConversationId) -> Result<bool, StoreError>;
}

// ---- the storage policy -------------------------------------------------------------------------------------------

/// The one place that knows which parts hold private text: the note text of a `Part::Context` and the content of a
/// `Part::ToolResult`. Tool output follows the same switch as terminal context because a file read, a pane read and a
/// command output are the same kind of private text, and a resumed chat must not send them again without a new
/// approval. `Part::Text` and `Message::native` are never changed. The rule is idempotent.
fn apply_storage(conversation: &mut Conversation, storage: ContextStorage) {
    if storage == ContextStorage::Text {
        return;
    }
    for message in &mut conversation.messages {
        for part in &mut message.parts {
            match part {
                Part::Context(note) => note.stored = None,
                Part::ToolResult(result) => {
                    if result.content != NOT_STORED {
                        NOT_STORED.clone_into(&mut result.content);
                    }
                }
                Part::Text { .. } | Part::ToolCall(_) => {}
            }
        }
    }
}

// ---- the file form ------------------------------------------------------------------------------------------------

/// The file form for writing: borrowed, keys in the order of the pinned form, optional keys left out.
#[derive(Serialize)]
struct FileOut<'a> {
    version: u32,
    id: &'a ConversationId,
    title: &'a str,
    created: i64,
    updated: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    messages: &'a [Message],
}

/// The file form for reading: strict, so an unknown field is damage.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIn {
    /// Required by the form; its value was judged before this struct was read.
    #[serde(rename = "version")]
    _version: u32,
    id: ConversationId,
    title: String,
    created: i64,
    updated: i64,
    profile: Option<String>,
    model: Option<String>,
    messages: Vec<Message>,
}

/// The file form for listing: everything but the messages, which are counted and skipped without being built.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileHeader {
    /// Required by the form; its value was judged before this struct was read.
    #[serde(rename = "version")]
    _version: u32,
    id: ConversationId,
    title: String,
    created: i64,
    updated: i64,
    profile: Option<String>,
    model: Option<String>,
    messages: Vec<IgnoredAny>,
}

/// Reads only the version, so a file of a newer form is recognised before its other fields are judged.
#[derive(Deserialize)]
struct Probe {
    version: Option<u32>,
}

/// Whether `text` is a short plain label: at most [`MAX_LABEL`] bytes and no control character.
fn is_label(text: &str) -> bool {
    text.len() <= MAX_LABEL && !text.chars().any(char::is_control)
}

/// What a store writes for `conversation`: checked against the limits that need no disk, with a scrubbed title and the
/// storage policy applied. The caller's value is not changed.
fn prepare(conversation: &Conversation, storage: ContextStorage) -> Result<Conversation, StoreError> {
    if conversation.messages.len() > MAX_MESSAGES {
        return Err(StoreError::Invalid("too many messages"));
    }
    let labels = conversation.profile.iter().chain(conversation.model.iter());
    if !labels.into_iter().all(|label| is_label(label)) {
        return Err(StoreError::Invalid("profile or model is not a short plain label"));
    }
    let mut stored = conversation.clone();
    stored.title = Detail::new(&stored.title).as_str().to_owned();
    apply_storage(&mut stored, storage);
    Ok(stored)
}

/// The conversation as the bytes of its file: compact JSON and one newline. Over [`MAX_CHAT_BYTES`] is refused here,
/// before anything is written.
fn encode(conversation: &Conversation) -> Result<Vec<u8>, StoreError> {
    let file = FileOut {
        version: FORMAT_VERSION,
        id: &conversation.id,
        title: &conversation.title,
        created: conversation.created,
        updated: conversation.updated,
        profile: conversation.profile.as_deref(),
        model: conversation.model.as_deref(),
        messages: &conversation.messages,
    };
    let mut bytes = serde_json::to_vec(&file).map_err(|_| StoreError::Invalid("the chat cannot be written as JSON"))?;
    bytes.push(b'\n');
    let size = bytes.len() as u64;
    if size > MAX_CHAT_BYTES {
        return Err(StoreError::TooLarge { bytes: size, max: MAX_CHAT_BYTES });
    }
    Ok(bytes)
}

/// Words for a parse failure, from the error category, the line and the column only. The error's own text can quote
/// the file, so it is never used.
fn json_reason(error: &serde_json::Error) -> String {
    use serde_json::error::Category;
    let (line, column) = (error.line(), error.column());
    match error.classify() {
        Category::Syntax => format!("not valid JSON at line {line} column {column}"),
        Category::Data => format!("a field is missing, unknown or of the wrong type at line {line} column {column}"),
        Category::Eof => format!("the file ended early at line {line} column {column}"),
        Category::Io => format!("the file could not be read at line {line} column {column}"),
    }
}

/// Judges the shape and the version of a file before any other field: a document that is not an object, one without
/// a version or with version 0 is damaged, and one of a newer version is [`StoreError::Newer`], whatever else it holds.
fn check_version(bytes: &[u8], expected: &ConversationId) -> Result<(), StoreError> {
    let damaged = |reason: String| StoreError::Damaged { id: expected.clone(), reason };
    if bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(damaged("the file is not a JSON object".to_owned()));
    }
    let probe: Probe = serde_json::from_slice(bytes).map_err(|e| damaged(json_reason(&e)))?;
    match probe.version {
        None => Err(damaged("the file has no version".to_owned())),
        Some(found) if found > FORMAT_VERSION => {
            Err(StoreError::Newer { id: expected.clone(), found, supported: FORMAT_VERSION })
        }
        Some(found) if found != FORMAT_VERSION => Err(damaged(format!("version {found} is not {FORMAT_VERSION}"))),
        Some(_) => Ok(()),
    }
}

/// The conversation in `bytes`, which must be the file of `expected`. Reads strictly.
fn decode(bytes: &[u8], expected: &ConversationId) -> Result<Conversation, StoreError> {
    let damaged = |reason: &str| StoreError::Damaged { id: expected.clone(), reason: reason.to_owned() };
    check_version(bytes, expected)?;
    let file: FileIn = serde_json::from_slice(bytes)
        .map_err(|e| StoreError::Damaged { id: expected.clone(), reason: json_reason(&e) })?;
    if &file.id != expected {
        return Err(damaged("the id does not match the file name"));
    }
    if file.messages.len() > MAX_MESSAGES {
        return Err(damaged("too many messages"));
    }
    Ok(Conversation {
        id: file.id,
        title: Detail::new(&file.title).as_str().to_owned(),
        created: file.created,
        updated: file.updated,
        profile: file.profile,
        model: file.model,
        messages: file.messages,
    })
}

/// The list entry in `bytes`, judged by the same rules as [`decode`] without building the messages.
fn decode_summary(bytes: &[u8], expected: &ConversationId) -> Result<ConversationSummary, StoreError> {
    let damaged = |reason: &str| StoreError::Damaged { id: expected.clone(), reason: reason.to_owned() };
    check_version(bytes, expected)?;
    let file: FileHeader = serde_json::from_slice(bytes)
        .map_err(|e| StoreError::Damaged { id: expected.clone(), reason: json_reason(&e) })?;
    if &file.id != expected {
        return Err(damaged("the id does not match the file name"));
    }
    if file.messages.len() > MAX_MESSAGES {
        return Err(damaged("too many messages"));
    }
    Ok(ConversationSummary {
        id: file.id,
        title: Detail::new(&file.title).as_str().to_owned(),
        created: file.created,
        updated: file.updated,
        profile: file.profile,
        model: file.model,
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Keeps a file that could not be used: renames it to its `.bad` sibling, replacing an older copy, and returns the
/// [`StoreError::Damaged`] that says why. A rename that finds no file means another thread already moved it. The reason
/// names the kept file by name, never by path, and never quotes what was in it.
fn quarantine(path: &Path, id: &ConversationId, reason: String) -> StoreError {
    let kept = match fs::rename(path, bad_path(path)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    };
    let reason = match kept {
        Ok(()) => format!("{reason}; the file was kept as {id}.json.bad"),
        Err(e) => format!("{reason}; and it could not be kept: {e}"),
    };
    StoreError::Damaged { id: id.clone(), reason }
}

/// Reads and judges the file of one chat with `decode`.
///
/// `Ok(None)` when there is no file. A symlink or any other file that is not a regular file is damage that is neither
/// opened nor moved, so a link named like a chat never leads the store to its target. A file over [`MAX_CHAT_BYTES`]
/// is not read and is moved aside. A [`StoreError::Damaged`] from `decode` moves the file aside too; a
/// [`StoreError::Newer`] leaves it exactly where it is.
fn read_with<T>(
    path: &Path,
    id: &ConversationId,
    decode: impl FnOnce(&[u8], &ConversationId) -> Result<T, StoreError>,
) -> Result<Option<T>, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err(StoreError::Damaged { id: id.clone(), reason: "not a regular file".to_owned() }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let bytes = match read_limited(path, MAX_CHAT_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            return Err(quarantine(path, id, format!("larger than {MAX_CHAT_BYTES} bytes")));
        }
        Err(e) => return Err(e.into()),
    };
    match decode(&bytes, id) {
        Ok(value) => Ok(Some(value)),
        Err(StoreError::Damaged { reason, .. }) => Err(quarantine(path, id, reason)),
        Err(other) => Err(other),
    }
}

/// The conversation in the file at `path`; see [`read_with`].
fn read_chat(path: &Path, id: &ConversationId) -> Result<Option<Conversation>, StoreError> {
    read_with(path, id, decode)
}

/// The note a listing keeps for a chat it could not list, or the error when it is not a problem of that one chat.
fn note_for(error: StoreError) -> Result<String, StoreError> {
    match error {
        StoreError::Damaged { id, reason } => Ok(format!("{id}: {reason}")),
        StoreError::Newer { id, found, supported } => {
            Ok(format!("{id}: written by a newer version of the app (format {found}, this build reads {supported})"))
        }
        other => Err(other),
    }
}

/// The id of a chat file from its directory entry name: `<valid id>.json`. Other names are not chats.
fn chat_id(name: &std::ffi::OsStr) -> Option<ConversationId> {
    name.to_str().and_then(|name| name.strip_suffix(".json")).and_then(|stem| ConversationId::parse(stem).ok())
}

// ---- the directory store ------------------------------------------------------------------------------------------

/// Chats as files in one directory: `<id>.json`, with `<id>.json.tmp` while a write is in flight and `<id>.json.bad`
/// for a file that could not be used.
///
/// Nothing touches the disk at construction; the directory is created, owner-only, at the first save.
pub struct JsonDirStore {
    dir: PathBuf,
    max_chats: usize,
    /// Serialises writers, so the fixed temporary name of one chat cannot collide.
    write: Mutex<()>,
}

impl JsonDirStore {
    /// A store over `dir`, which need not exist yet, holding at most [`MAX_CHATS`] chats.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), max_chats: MAX_CHATS, write: Mutex::new(()) }
    }

    /// The same store limited to `n` chats, clamped to 1 up to [`MAX_CHATS`].
    #[must_use]
    pub fn with_max_chats(mut self, n: usize) -> Self {
        self.max_chats = n.clamp(1, MAX_CHATS);
        self
    }

    /// The directory the chats live in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_of(&self, id: &ConversationId) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn sibling_of(&self, id: &ConversationId, suffix: &str) -> PathBuf {
        self.dir.join(format!("{id}.json{suffix}"))
    }

    /// Creates the directory, owner-only on Unix, when it is missing.
    fn ensure_dir(&self) -> io::Result<()> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.dir)
    }

    /// How many regular files in the directory are named like a chat.
    fn chat_count(&self) -> io::Result<usize> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let mut count = 0;
        for entry in entries {
            let entry = entry?;
            if chat_id(&entry.file_name()).is_some() && entry.file_type()?.is_file() {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Writes finished bytes. The caller holds the write lock, which the parameter proves. A chat that does not exist
    /// yet is refused when the directory already holds as many as it may; one that exists can always be saved.
    fn write_bytes(&self, _held: &MutexGuard<'_, ()>, id: &ConversationId, bytes: &[u8]) -> Result<(), StoreError> {
        let path = self.path_of(id);
        if matches!(fs::symlink_metadata(&path), Err(e) if e.kind() == io::ErrorKind::NotFound)
            && self.chat_count()? >= self.max_chats
        {
            return Err(StoreError::Full { max: self.max_chats });
        }
        self.ensure_dir()?;
        write_atomic_private(&path, bytes)?;
        Ok(())
    }
}

impl ConversationStore for JsonDirStore {
    /// Reads each chat file once, so the cost of a listing is bounded by [`MAX_CHATS`] times [`MAX_CHAT_BYTES`]; more
    /// files than that are not looked at, and one note says so. A file that cannot be used is moved aside as in
    /// [`load`](ConversationStore::load) (a newer one stays where it is) and reported in `notes`; names that are not
    /// `<valid id>.json` are ignored.
    fn list(&self) -> Result<Listing, StoreError> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Listing::default()),
            Err(e) => return Err(e.into()),
        };
        let mut listing = Listing::default();
        let mut looked_at = 0;
        for entry in entries {
            let entry = entry?;
            let Some(id) = chat_id(&entry.file_name()) else { continue };
            if looked_at == MAX_CHATS {
                listing.notes.push(format!("more than {MAX_CHATS} chats: the rest are not listed"));
                break;
            }
            looked_at += 1;
            match read_with(&self.path_of(&id), &id, decode_summary) {
                Ok(Some(summary)) => listing.chats.push(summary),
                Ok(None) => {}
                Err(e) => listing.notes.push(note_for(e)?),
            }
        }
        listing.chats.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
        Ok(listing)
    }

    fn load(&self, id: &ConversationId) -> Result<Option<Conversation>, StoreError> {
        read_chat(&self.path_of(id), id)
    }

    fn save(&self, conversation: &Conversation, storage: ContextStorage) -> Result<(), StoreError> {
        let stored = prepare(conversation, storage)?;
        let bytes = encode(&stored)?;
        let held = lock(&self.write);
        self.write_bytes(&held, &stored.id, &bytes)
    }

    /// A symlink named like a chat is removed as a link and its target is never touched.
    fn delete(&self, id: &ConversationId) -> Result<bool, StoreError> {
        let _held = lock(&self.write);
        let mut removed = false;
        for path in [self.path_of(id), self.sibling_of(id, ".tmp"), self.sibling_of(id, ".bad")] {
            match fs::remove_file(&path) {
                Ok(()) => removed = true,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(removed)
    }
}
