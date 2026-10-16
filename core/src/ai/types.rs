//! The data of the AI chat: the message model, what a provider can do, and what a stream reports.
//!
//! These types are the common language of the provider adapters, the agent loop, the conversation store and the page.
//! Two rules hold for all of them.
//!
//! - **The JSON shape is a stored format.** `Message`, `Part` and `Opaque` are written to disk by the conversation
//!   store, so a field name or a tag changes only together with a version of the store.
//! - **`Debug` never prints text.** A value whose fields can hold request text, terminal content, tool arguments or
//!   model output has a hand-written `Debug` that prints names, ids, labels, counts and byte sizes (`<123 bytes>`).
//!   A log line, a panic message or a failing assertion therefore cannot leak what the user typed or what a model
//!   answered. A derived `Debug` is used only when every field already follows this rule.

use std::fmt;
use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

/// A byte count that prints as `<N bytes>`: how the `Debug` of a type stands in for text it holds.
pub(crate) struct Size(pub usize);

impl fmt::Debug for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} bytes>", self.0)
    }
}

/// The size of `value` serialised, counted without building the text.
pub(crate) fn json_size(value: &Value) -> Size {
    struct Count(usize);
    impl io::Write for Count {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    // Serialising a `Value` into a counter cannot fail.
    let _ = serde_json::to_writer(&mut count, value);
    Size(count.0)
}

/// Where a model runs, as far as the data goes.
///
/// The class is computed by the HTTP policy from the resolved address, or set by the Apple provider; it is never read
/// from a label the user typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locality {
    /// On this device, in the operating system's own model.
    OnDevice,
    /// A server on this machine (loopback address).
    Loopback,
    /// A server on the local network.
    Lan,
    /// A server on the internet.
    Cloud,
}

impl Locality {
    /// Whether text sent to this class leaves the device.
    pub fn leaves_device(self) -> bool {
        matches!(self, Locality::Lan | Locality::Cloud)
    }

    /// Whether text sent to this class stays on the device.
    pub fn is_local(self) -> bool {
        matches!(self, Locality::OnDevice | Locality::Loopback)
    }
}

/// What a model can do. A value says what the caller may rely on, so an unknown model gets the conservative record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// The model can call tools.
    pub tools: bool,
    /// The model produces reasoning text.
    pub reasoning: bool,
    /// The model accepts images.
    pub vision: bool,
    /// The context window in tokens, when known.
    pub context_tokens: Option<u32>,
    /// The longest answer in tokens, when known.
    pub max_output_tokens: Option<u32>,
    /// Where the model runs.
    pub locality: Locality,
}

impl Capabilities {
    /// No tools, no reasoning, no vision, sizes unknown: what to assume for a model nothing is known about.
    pub fn conservative(locality: Locality) -> Self {
        Self { tools: false, reasoning: false, vision: false, context_tokens: None, max_output_tokens: None, locality }
    }
}

/// Token counts of a turn. Zero means "not reported". A value is the total so far, not a delta.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens the provider read.
    pub input_tokens: u32,
    /// Tokens the provider wrote.
    pub output_tokens: u32,
}

/// Why a turn ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its answer.
    EndTurn,
    /// The model wants one or more tools run.
    ToolUse,
    /// The answer hit the output limit.
    MaxTokens,
    /// The model declined to answer.
    Refusal,
    /// Anything else a provider reports.
    Other,
}

/// Who wrote a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The user.
    User,
    /// The model.
    Assistant,
    /// A tool result sent back to the model.
    Tool,
}

/// How a message ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    /// Written in full.
    Complete,
    /// The user stopped it.
    Cancelled,
    /// A provider error ended it.
    Failed,
    /// The application ended while it was streaming.
    Interrupted,
}

/// A tool as it is declared to a model.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// The name the model calls.
    pub name: String,
    /// What the tool does, for the model.
    pub description: String,
    /// The JSON schema of its arguments.
    pub input_schema: Value,
}

impl fmt::Debug for ToolSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolSpec")
            .field("name", &self.name)
            .field("description", &Size(self.description.len()))
            .field("input_schema", &json_size(&self.input_schema))
            .finish()
    }
}

/// A tool call a model made. It is always complete and parsed: an adapter delivers it only when the arguments have
/// arrived in full.
///
/// `id` is the provider's id when it has one; otherwise the adapter mints one that is unique in the conversation, so a
/// result can always name the call it answers.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Names the call; the result carries it back.
    pub id: String,
    /// The tool to run.
    pub name: String,
    /// The arguments, parsed.
    pub args: Value,
}

impl fmt::Debug for ToolCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolCall")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("args", &json_size(&self.args))
            .finish()
    }
}

/// What running a tool produced, sent back to the model.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// The id of the call this answers.
    pub call_id: String,
    /// What the tool produced, or why it failed.
    pub content: String,
    /// Whether the tool failed.
    pub is_error: bool,
}

impl fmt::Debug for ToolResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolResult")
            .field("call_id", &self.call_id)
            .field("is_error", &self.is_error)
            .field("content", &Size(self.content.len()))
            .finish()
    }
}

/// What kind of terminal or file content a context item holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    /// Text the user selected.
    Selection,
    /// The visible screen of a pane.
    Screen,
    /// The last command and its output.
    LastCommand,
    /// A file.
    File,
    /// A rendered preview.
    Preview,
}

/// Who a context was approved for: the provider and where it runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consent {
    /// The provider id the user approved the context for.
    pub provider: String,
    /// Where that provider runs.
    pub locality: Locality,
}

/// One item of a context, described without its text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteItem {
    /// What the item is.
    pub kind: ContextKind,
    /// The label the user saw in the offer.
    pub label: String,
    /// The size of the text that was approved.
    pub bytes: u64,
}

/// The record a message keeps of the context it carried.
///
/// It names the approved envelope by its hash and lists the items by label and size. `stored` holds the text only when
/// the user chose to keep it; the default is the redacted marker (`None`). `stored` is never sent to a provider: a
/// follow-up message does not send earlier terminal text again unless the user approves it again.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextNote {
    /// The SHA-256 of the approved envelope, in lower-case hex.
    pub hash: String,
    /// The items, described.
    pub items: Vec<NoteItem>,
    /// The text, when the user kept it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored: Option<String>,
    /// Who the context was approved for.
    pub consented_to: Consent,
}

impl fmt::Debug for ContextNote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContextNote")
            .field("hash", &self.hash)
            .field("items", &self.items)
            .field("stored", &self.stored.as_ref().map(|text| Size(text.len())))
            .field("consented_to", &self.consented_to)
            .finish()
    }
}

/// The provider-native assistant turn, kept exactly as the provider sent it.
///
/// Signed reasoning blocks, encrypted reasoning items and thought signatures only work when they are sent back
/// unchanged, so the normalised message is not enough to continue a conversation. `wire` is the tag of the adapter that
/// wrote the blob; only that adapter may replay it (see `Message::native_for`).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Opaque {
    /// The tag of the adapter that wrote the blob, for example `anthropic`.
    pub wire: String,
    /// The turn as the provider sent it.
    pub json: Value,
}

impl fmt::Debug for Opaque {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Opaque").field("wire", &self.wire).field("json", &json_size(&self.json)).finish()
    }
}

/// One piece of a message.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    /// Plain text.
    Text {
        /// The text.
        text: String,
    },
    /// A call the model made.
    ToolCall(ToolCall),
    /// The result of a tool call.
    ToolResult(ToolResult),
    /// A record of the approved context this message carried.
    Context(ContextNote),
}

impl fmt::Debug for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Part::Text { text } => f.debug_tuple("Text").field(&Size(text.len())).finish(),
            Part::ToolCall(call) => f.debug_tuple("ToolCall").field(call).finish(),
            Part::ToolResult(result) => f.debug_tuple("ToolResult").field(result).finish(),
            Part::Context(note) => f.debug_tuple("Context").field(note).finish(),
        }
    }
}

/// One message of a conversation, in the canonical form every provider adapter reads and writes.
///
/// The fields are written in this order, and the three optional ones are left out when empty.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Unique in the conversation.
    pub id: String,
    /// Who wrote it.
    pub role: Role,
    /// What it holds, in order.
    pub parts: Vec<Part>,
    /// Seconds since the epoch.
    pub ts: i64,
    /// The model that wrote it, for an assistant message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// What it cost, for an assistant message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// How it ended.
    pub status: MessageStatus,
    /// The provider-native turn, kept verbatim for replay to the same adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<Opaque>,
}

impl Message {
    /// A complete message with no model, usage or native turn.
    pub fn new(id: impl Into<String>, role: Role, parts: Vec<Part>, ts: i64) -> Self {
        Self {
            id: id.into(),
            role,
            parts,
            ts,
            model: None,
            usage: None,
            status: MessageStatus::Complete,
            native: None,
        }
    }

    /// A user message holding `text`.
    pub fn user_text(id: impl Into<String>, text: impl Into<String>, ts: i64) -> Self {
        Self::new(id, Role::User, vec![Part::Text { text: text.into() }], ts)
    }

    /// The text parts, joined without a separator.
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|part| match part {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The provider-native turn, only when it was written by the adapter with this wire tag.
    ///
    /// An adapter that finds a blob of another wire drops it and keeps the text and the tool results: a signed
    /// reasoning block or an encrypted item means nothing to another provider and must not be sent to it.
    pub fn native_for(&self, wire: &str) -> Option<&Value> {
        self.native.as_ref().filter(|native| native.wire == wire).map(|native| &native.json)
    }
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Message")
            .field("id", &self.id)
            .field("role", &self.role)
            .field("parts", &self.parts)
            .field("ts", &self.ts)
            .field("model", &self.model)
            .field("usage", &self.usage)
            .field("status", &self.status)
            .field("native", &self.native)
            .finish()
    }
}

/// An API key. It is wiped from memory when dropped and has no way out except [`ApiKey::expose`]: no `Clone`, no
/// serialisation, no `Display`, no comparison, and a `Debug` that prints `ApiKey(<redacted>)`.
///
/// A key has no way to be copied:
///
/// ```compile_fail
/// use holzwave_core::ai::types::ApiKey;
/// let key = ApiKey::new("secret".to_owned());
/// let copy = key.clone();
/// ```
///
/// It has no serialisation:
///
/// ```compile_fail
/// use holzwave_core::ai::types::ApiKey;
/// let key = ApiKey::new("secret".to_owned());
/// let json = serde_json::to_string(&key);
/// ```
///
/// It cannot be formatted for display:
///
/// ```compile_fail
/// use holzwave_core::ai::types::ApiKey;
/// let key = ApiKey::new("secret".to_owned());
/// let shown = format!("{key}");
/// ```
///
/// The failing examples above stand next to this one, which uses the same imports and constructor, so a typo cannot
/// make them pass:
///
/// ```
/// use holzwave_core::ai::types::ApiKey;
/// let key = ApiKey::new("secret".to_owned());
/// assert_eq!(key.expose(), "secret");
/// assert_eq!(format!("{key:?}"), "ApiKey(<redacted>)");
/// ```
pub struct ApiKey(Zeroizing<String>);

impl ApiKey {
    /// Wraps a key. The string is moved in and wiped with the key.
    pub fn new(key: String) -> Self {
        Self(Zeroizing::new(key))
    }

    /// The key, for the one place that puts it in a request header.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether there is no key.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `text` with every occurrence of the key replaced by `<redacted>`, for text that may echo the key (an error
    /// body, a URL). An empty key masks nothing.
    pub fn redact_in(&self, text: &str) -> String {
        if self.0.is_empty() {
            return text.to_owned();
        }
        text.replace(self.0.as_str(), "<redacted>")
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}
