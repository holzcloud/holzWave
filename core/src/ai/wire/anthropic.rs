//! The Anthropic Messages dialect, also spoken by the Cloudflare AI Gateway and by the native route of the Vercel AI
//! Gateway.
//!
//! - **The request is a pure function** of the [`ChatRequest`] and the [`Capabilities`]: the same input gives the same
//!   bytes. The body holds the model, an output bound that is always present, the stream flag, the system prompt when
//!   there is one, the messages and, when the request declares some, the tools. Nothing else is written: no sampling
//!   parameter, no forced tool choice, no caching marker, no effort setting, no model fallback and no hosted feature
//!   (server-side tools, a remote tool connector, web access). A key belongs to the [`Endpoint`](super::Endpoint) and is
//!   added by the chokepoint, never here.
//! - **Two flavors.** [`Flavor::FirstParty`] is the vendor's own host and nothing else; [`Flavor::Gateway`] is every
//!   other address and sends the core fields only. The reasoning request and its binding controls are added for the
//!   first flavor alone, on a model that supports adaptive reasoning.
//! - **Messages are written by side.** Tool results and user parts share one user message with the results first; empty
//!   text and empty messages are dropped; neighbours of one side merge; a request must end with a user message.
//! - **Terminal text leaves only as an approved envelope.** A context note is written from the envelope of the approval
//!   the request itself carries. A note without an approval is history: one marker line with labels and byte counts, and
//!   the text a note may have kept is never read.
//! - **The assistant turn is kept as the provider sent it.** Reasoning blocks with their signatures, redacted reasoning,
//!   text and tool uses are assembled from the stream into [`Completion::native`] as an [`Opaque`] with the wire tag
//!   `anthropic` whose JSON is the array of content blocks, and the next request carries them unchanged and in order. A
//!   stored turn that is not in that shape is ignored and the normalised parts are written instead.
//! - **The answer is read into typed events.** Text is borrowed from the frame while it is parsed. Unknown event types,
//!   unknown delta types and unknown extra fields are ignored; the structure the adapter depends on is checked, and a
//!   stream that breaks it fails the turn with a fixed phrase that holds no stream text.
//! - **A tool call is released once, after a clean end.** Fragments are collected by a
//!   [`ToolAssembler`](crate::ai::sse::ToolAssembler) and handed out from [`WireTurn::finish`] only, after the final
//!   event and a clean end of the stream.
//! - **Nothing here touches the network, a key or the environment.** The turn never prints the stream: its `Debug` shows
//!   counts and sizes only.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Endpoint, Wire, WireAdapter, WireRequest, WireTurn};
use crate::ai::http::{Frame, Header, Origin, Rejection};
use crate::ai::provider::{AiError, ChatRequest, Completion, Detail, Flow, MAX_DETAIL, StreamEvent};
use crate::ai::sse::{StreamEnd, ToolAssembler};
use crate::ai::types::{
    Capabilities, ContextNote, Locality, Message, Opaque, Part, Role, Size, StopReason, ToolCall,
    ToolResult, ToolSpec, Usage, json_size,
};

// ---- constants and flavors --------------------------------------------------------------------------------

/// The value of the `anthropic-version` header. Every request carries it.
pub const API_VERSION: &str = "2023-06-01";
/// The path of the Messages endpoint below the base address. A gateway prefix belongs to the base address.
pub const MESSAGES_PATH: &str = "/v1/messages";
/// The beta that the binding controls of the reasoning request need. It is sent with the first flavor only.
pub const BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";
/// The host name of the vendor's own service. Only this host, over https on port 443, gets [`Flavor::FirstParty`].
pub const FIRST_PARTY_HOST: &str = "api.anthropic.com";
/// The output bound sent when the caller gives none, in tokens.
pub const DEFAULT_MAX_TOKENS: u32 = 16384;
/// The most content blocks one turn may hold.
pub const MAX_BLOCKS: usize = 64;
/// The most text, reasoning and signature one turn may accumulate: 8 MiB.
pub const MAX_TURN_BYTES: usize = 8 * 1024 * 1024;

/// Which service answers: the vendor's own host or anything else that speaks the dialect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// The vendor's own service.
    FirstParty,
    /// A gateway, a local server or any other address. It gets the core fields only.
    Gateway,
}

impl Flavor {
    /// The stable name: `first_party` or `gateway`.
    pub fn as_str(self) -> &'static str {
        match self {
            Flavor::FirstParty => "first_party",
            Flavor::Gateway => "gateway",
        }
    }
}

/// The adapter of the Messages dialect.
pub struct AnthropicAdapter {
    flavor: Flavor,
}

impl AnthropicAdapter {
    /// An adapter of the given flavor.
    pub fn new(flavor: Flavor) -> AnthropicAdapter {
        AnthropicAdapter { flavor }
    }

    /// The adapter for an address: [`Flavor::FirstParty`] exactly for the scheme https, the host [`FIRST_PARTY_HOST`] and
    /// the port 443. A look-alike host, another port, plain http and every gateway are [`Flavor::Gateway`].
    pub fn for_origin(origin: &Origin) -> AnthropicAdapter {
        let first_party =
            origin.scheme().as_str() == "https" && origin.host() == FIRST_PARTY_HOST && origin.port() == 443;
        AnthropicAdapter::new(if first_party { Flavor::FirstParty } else { Flavor::Gateway })
    }

    /// The flavor.
    pub fn flavor(&self) -> Flavor {
        self.flavor
    }
}

impl fmt::Debug for AnthropicAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicAdapter").field("flavor", &self.flavor.as_str()).finish()
    }
}

// ---- model families ---------------------------------------------------------------------------------------

/// What is known about a family of models: whether it supports adaptive reasoning and its limits.
struct Family {
    prefix: &'static str,
    adaptive: bool,
    context_tokens: u32,
    output_tokens: Option<u32>,
}

/// The families the adapter knows. The numbers are the values of the vendor's model table as cached on 2026-10-06. They
/// only clamp the output bound and fill [`Capabilities`]; nothing sent depends on them except whether the reasoning
/// request is written. A model that matches no row is unknown and gets no reasoning request and no limit.
const FAMILIES: [Family; 10] = [
    Family { prefix: "claude-fable-5", adaptive: true, context_tokens: 1_000_000, output_tokens: Some(128_000) },
    Family { prefix: "claude-mythos-5", adaptive: true, context_tokens: 1_000_000, output_tokens: Some(128_000) },
    Family { prefix: "claude-opus-5", adaptive: true, context_tokens: 1_000_000, output_tokens: Some(128_000) },
    Family { prefix: "claude-opus-4-8", adaptive: true, context_tokens: 1_000_000, output_tokens: Some(128_000) },
    Family { prefix: "claude-opus-4-7", adaptive: true, context_tokens: 1_000_000, output_tokens: Some(128_000) },
    Family { prefix: "claude-opus-4-6", adaptive: true, context_tokens: 1_000_000, output_tokens: Some(128_000) },
    Family { prefix: "claude-sonnet-5", adaptive: true, context_tokens: 1_000_000, output_tokens: Some(128_000) },
    Family { prefix: "claude-sonnet-4-6", adaptive: true, context_tokens: 1_000_000, output_tokens: Some(128_000) },
    Family { prefix: "claude-haiku-5", adaptive: true, context_tokens: 1_000_000, output_tokens: Some(128_000) },
    Family { prefix: "claude-haiku-4-5", adaptive: false, context_tokens: 200_000, output_tokens: None },
];

/// The family of `model`. A vendor prefix up to the last `/` is dropped. A row matches when the name is the prefix or
/// goes on with a hyphen right after it, so `claude-opus-5-5` is of the family `claude-opus-5` and `claude-opus-50` is
/// of none.
fn family_of(model: &str) -> Option<&'static Family> {
    let name = model.rsplit('/').next().unwrap_or(model);
    FAMILIES
        .iter()
        .find(|family| name.strip_prefix(family.prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with('-')))
}

// ---- errors -----------------------------------------------------------------------------------------------

/// An error of the answer: a fixed phrase and nothing from the stream.
fn protocol(phrase: &str) -> AiError {
    AiError::Protocol(Detail::new(phrase))
}

/// An error of the request: a fixed phrase and nothing from the request.
fn invalid(phrase: &str) -> AiError {
    AiError::InvalidRequest(Detail::new(phrase))
}

const PHRASE_UNREADABLE: &str = "the response holds an event that cannot be read";
const PHRASE_FRAME: &str = "the response is not an event stream";
const PHRASE_ERROR_EVENT: &str = "the provider reported an error while streaming";
const PHRASE_UNKNOWN_BLOCK: &str = "the response holds a content block type this version does not know";
const PHRASE_NO_MESSAGE: &str = "the response holds no message";
const PHRASE_NO_STOP: &str = "the response ended without its final event";
const PHRASE_TRUNCATED: &str = "the response stream ended before the message was complete";
const PHRASE_BAD_TOOL: &str = "the response holds a tool use without an id or a name";
const PHRASE_TOO_MANY_BLOCKS: &str = "the response holds more content blocks than one turn allows";
const PHRASE_TOO_LARGE: &str = "the response is larger than one turn allows";
const PHRASE_MISFIT: &str = "the response sends a delta that does not fit its content block";
const PHRASE_NO_CALL: &str = "the model stopped to use a tool without making a complete call";
const PHRASE_FAILED_EARLIER: &str = "the response was refused earlier";

/// The error for the type name of a provider error, or `None` for a type this version does not know. `detail` is the text
/// to show for the classes that carry some; a status is given for an HTTP answer and a wait for a rate limit.
fn error_of(kind: &str, status: Option<u16>, retry_after: Option<Duration>, detail: Detail) -> Option<AiError> {
    Some(match kind {
        "overloaded_error" => AiError::Overloaded,
        "rate_limit_error" => AiError::RateLimited { retry_after },
        "authentication_error" | "permission_error" | "billing_error" => AiError::Auth(detail),
        "invalid_request_error" | "not_found_error" | "conflict_error" => AiError::InvalidRequest(detail),
        "request_too_large" => AiError::ContextLength,
        "timeout_error" => AiError::Timeout,
        "api_error" => AiError::Server { status: status.unwrap_or(500), detail },
        _ => return None,
    })
}

/// Text of an error body, made safe to show: every key of the endpoint is masked, control, direction and zero-width
/// characters are replaced, and the whole is within the limit of a detail.
fn shown_text(endpoint: &Endpoint, message: &str) -> Detail {
    let detail = endpoint.scrub(message);
    // A detail that was cut ends in an ellipsis after its limit, so it is cut once more to fit the limit as a whole.
    if detail.as_str().chars().count() > MAX_DETAIL {
        let kept: String = detail.as_str().chars().take(MAX_DETAIL - 1).collect();
        return Detail::new(&format!("{}\u{2026}", kept.trim_end()));
    }
    detail
}

// ---- the request ------------------------------------------------------------------------------------------

/// The text a tool result carries when the tool printed nothing: the provider refuses an empty result.
const NO_OUTPUT: &str = "(no output)";

/// One message as it is written: a role and its content blocks.
#[derive(Serialize)]
struct WireMessage {
    role: &'static str,
    content: Vec<Value>,
}

/// One declared tool. The schema is borrowed as it is, and nothing marks the definition as strict.
#[derive(Serialize)]
struct ToolDecl<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a Value,
}

/// What the preserved reasoning does when the history was edited: the block is dropped instead of failing the request.
#[derive(Serialize)]
struct BlockBinding {
    prefix_mismatch_behavior: &'static str,
}

/// The reasoning request of the first flavor: adaptive, with a summary the chat can show, and with the binding controls.
#[derive(Serialize)]
struct Thinking {
    r#type: &'static str,
    display: &'static str,
    block_binding: BlockBinding,
}

impl Thinking {
    fn adaptive() -> Self {
        Self {
            r#type: "adaptive",
            display: "summarized",
            block_binding: BlockBinding { prefix_mismatch_behavior: "drop_block" },
        }
    }
}

/// The request body. Every optional member is left out when it is not set; no member outside this list is ever written.
#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "str::is_empty")]
    system: &'a str,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ToolDecl<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Thinking>,
}

/// What a context note becomes in a request. With an approval in the request: the envelope the user saw, unchanged.
/// Without one the note is history, and one marker line stands in for it: the kind, the cleaned label and the size of
/// each item. Nothing else of the note is read, and the text it may have kept never.
fn context_text(req: &ChatRequest, note: &ContextNote) -> String {
    if let Some(approved) = req.approved_for(&note.hash) {
        return approved.envelope().to_owned();
    }
    super::marker_line(note)
}

/// Which side of the conversation a message is written on: tool results belong to the user side.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    User,
    Assistant,
}

/// A message before it is serialised.
struct Rendered {
    side: Side,
    blocks: Vec<Value>,
}

fn block_type(block: &Value) -> Option<&str> {
    block.get("type").and_then(Value::as_str)
}

fn push_text(blocks: &mut Vec<Value>, text: &str) {
    if !text.is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
}

fn result_block(result: &ToolResult) -> Value {
    let content = if result.content.is_empty() { NO_OUTPUT } else { result.content.as_str() };
    let mut block = json!({ "type": "tool_result", "tool_use_id": result.call_id, "content": content });
    if result.is_error {
        block["is_error"] = Value::Bool(true);
    }
    block
}

fn use_block(call: &ToolCall) -> Value {
    let input = if call.args.is_object() { call.args.clone() } else { json!({}) };
    json!({ "type": "tool_use", "id": call.id, "name": call.name, "input": input })
}

/// The blocks of a message of the user side, in the order of its parts.
fn user_blocks(req: &ChatRequest, message: &Message) -> Vec<Value> {
    let mut blocks = Vec::with_capacity(message.parts.len());
    for part in &message.parts {
        match part {
            Part::Text { text } => push_text(&mut blocks, text),
            Part::ToolResult(result) => blocks.push(result_block(result)),
            Part::Context(note) => push_text(&mut blocks, &context_text(req, note)),
            Part::ToolCall(_) => {}
        }
    }
    blocks
}

/// Whether a stored block is of a kind this adapter assembles.
fn is_known_block(block: &Value) -> bool {
    block.is_object() && matches!(block_type(block), Some("text" | "thinking" | "redacted_thinking" | "tool_use"))
}

/// The stored provider-native turn of `message`, reconciled with its call parts: the blocks as the provider sent them,
/// without a tool use that no call part names, and with a tool use appended for every call part the turn lacks. A turn
/// of another wire, of another shape or with a block of another kind is not used.
fn native_blocks(message: &Message) -> Option<Vec<Value>> {
    let Value::Array(stored) = message.native_for(Wire::Anthropic.tag())? else {
        return None;
    };
    if stored.is_empty() || !stored.iter().all(is_known_block) {
        return None;
    }
    let calls: Vec<&ToolCall> = message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();
    let id_of = |block: &Value| block.get("id").and_then(Value::as_str).map(str::to_owned);
    let mut blocks: Vec<Value> = stored
        .iter()
        .filter(|block| {
            block_type(block) != Some("tool_use")
                || id_of(block).is_some_and(|id| calls.iter().any(|call| call.id == id))
        })
        .cloned()
        .collect();
    for call in calls {
        let present = blocks
            .iter()
            .any(|block| block_type(block) == Some("tool_use") && id_of(block).as_deref() == Some(&call.id));
        if !present {
            blocks.push(use_block(call));
        }
    }
    Some(blocks)
}

/// The blocks of a message of the assistant side: the stored native turn when it can be used, else the text parts and
/// then one tool use per call part. Empty text is dropped either way, since the provider refuses it.
fn assistant_blocks(message: &Message) -> Vec<Value> {
    let mut blocks = native_blocks(message).unwrap_or_else(|| {
        let mut blocks = Vec::with_capacity(message.parts.len());
        for part in &message.parts {
            if let Part::Text { text } = part {
                push_text(&mut blocks, text);
            }
        }
        for part in &message.parts {
            if let Part::ToolCall(call) = part {
                blocks.push(use_block(call));
            }
        }
        blocks
    });
    blocks.retain(|block| {
        block_type(block) != Some("text")
            || block.get("text").and_then(Value::as_str).is_some_and(|text| !text.is_empty())
    });
    blocks
}

/// Adds a message to the list, merging it into the one before it when both are of one side. A message with no block is
/// dropped, so its neighbours become adjacent.
fn push_merged(list: &mut Vec<Rendered>, side: Side, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    match list.last_mut() {
        Some(last) if last.side == side => last.blocks.extend(blocks),
        _ => list.push(Rendered { side, blocks }),
    }
}

/// The ids that the message next to `neighbour` may answer: the `key` member of its blocks of kind `kind`, when it is on
/// the other `side`.
fn neighbour_ids(neighbour: Option<&Rendered>, side: Side, kind: &str, key: &str) -> Vec<String> {
    match neighbour {
        Some(other) if other.side == side => other
            .blocks
            .iter()
            .filter(|block| block_type(block) == Some(kind))
            .filter_map(|block| block.get(key).and_then(Value::as_str).map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// Drops the tool uses that the next user message does not answer and the tool results that the message before does not
/// ask for, because the provider refuses a request with half a pair. A message that loses its last block goes, and its
/// neighbours merge.
fn pair_tools(list: &mut Vec<Rendered>) {
    loop {
        let mut changed = false;
        for index in 0..list.len() {
            let (kind, key, allowed) = match list[index].side {
                Side::Assistant => {
                    ("tool_use", "id", neighbour_ids(list.get(index + 1), Side::User, "tool_result", "tool_use_id"))
                }
                Side::User => (
                    "tool_result",
                    "tool_use_id",
                    neighbour_ids(
                        index.checked_sub(1).and_then(|before| list.get(before)),
                        Side::Assistant,
                        "tool_use",
                        "id",
                    ),
                ),
            };
            let before = list[index].blocks.len();
            list[index].blocks.retain(|block| {
                block_type(block) != Some(kind)
                    || block.get(key).and_then(Value::as_str).is_some_and(|id| allowed.iter().any(|known| known == id))
            });
            changed |= list[index].blocks.len() != before;
        }
        if !changed {
            return;
        }
        for message in std::mem::take(list) {
            push_merged(list, message.side, message.blocks);
        }
    }
}

/// The messages of the request, written by side. The list is never empty and ends with a user message.
fn render(req: &ChatRequest) -> Result<Vec<WireMessage>, AiError> {
    let mut list: Vec<Rendered> = Vec::with_capacity(req.messages.len());
    for message in &req.messages {
        match message.role {
            Role::User | Role::Tool => push_merged(&mut list, Side::User, user_blocks(req, message)),
            Role::Assistant => push_merged(&mut list, Side::Assistant, assistant_blocks(message)),
        }
    }
    pair_tools(&mut list);
    match list.last() {
        None => return Err(invalid("the request has no messages")),
        Some(last) if last.side != Side::User => return Err(invalid("the request does not end with a user message")),
        Some(_) => {}
    }
    Ok(list
        .into_iter()
        .map(|message| match message.side {
            Side::Assistant => WireMessage { role: "assistant", content: message.blocks },
            Side::User => {
                // The results answer the tool uses of the message before, so they come first, in order.
                let (mut content, rest): (Vec<Value>, Vec<Value>) =
                    message.blocks.into_iter().partition(|block| block_type(block) == Some("tool_result"));
                content.extend(rest);
                WireMessage { role: "user", content }
            }
        })
        .collect())
}

/// The declared tools, each with exactly a name, a description and a schema. A tool with no name or a schema that is not
/// an object schema is refused here instead of by the provider.
fn declared_tools(tools: &[ToolSpec]) -> Result<Vec<ToolDecl<'_>>, AiError> {
    tools
        .iter()
        .map(|tool| {
            if tool.name.is_empty() {
                return Err(invalid("a tool has no name"));
            }
            if tool.input_schema.get("type").and_then(Value::as_str) != Some("object") {
                return Err(invalid("the schema of a tool is not an object schema"));
            }
            Ok(ToolDecl { name: &tool.name, description: &tool.description, input_schema: &tool.input_schema })
        })
        .collect()
}

fn header(name: &str, value: &str) -> Result<Header, AiError> {
    Header::new(name, value).map_err(|_| invalid("a header of the request is not valid"))
}

impl WireAdapter for AnthropicAdapter {
    fn wire(&self) -> Wire {
        Wire::Anthropic
    }

    fn capabilities(&self, model: &str, locality: Locality) -> Capabilities {
        match family_of(model) {
            Some(family) => Capabilities {
                tools: true,
                reasoning: family.adaptive,
                vision: true,
                context_tokens: Some(family.context_tokens),
                max_output_tokens: family.output_tokens,
                locality,
            },
            None => Capabilities { tools: true, ..Capabilities::conservative(locality) },
        }
    }

    fn request(&self, req: &ChatRequest, caps: &Capabilities) -> Result<WireRequest, AiError> {
        if req.model.is_empty() {
            return Err(invalid("the request names no model"));
        }
        let wanted = match req.max_output_tokens {
            Some(0) => return Err(invalid("the output bound must be more than zero")),
            Some(limit) => limit,
            None => DEFAULT_MAX_TOKENS,
        };
        let max_tokens = caps.max_output_tokens.map_or(wanted, |known| wanted.min(known));
        // The reasoning request and its binding controls are for the vendor's own host: a gateway may not accept the
        // field, and a history that was edited legitimately must not end a chat on the other side.
        let thinking = (self.flavor == Flavor::FirstParty && caps.reasoning).then(Thinking::adaptive);
        let body = Body {
            model: &req.model,
            max_tokens,
            stream: true,
            system: &req.system,
            messages: render(req)?,
            tools: declared_tools(&req.tools)?,
            thinking,
        };
        let mut headers = vec![header("anthropic-version", API_VERSION)?];
        if body.thinking.is_some() {
            headers.push(header("anthropic-beta", BINDING_BETA)?);
        }
        let body = serde_json::to_vec(&body).map_err(|_| invalid("the request could not be written as JSON"))?;
        Ok(WireRequest { path: MESSAGES_PATH.to_owned(), headers, body })
    }

    fn turn(&self) -> Box<dyn WireTurn> {
        Box::new(AnthropicTurn::new())
    }

    fn rejection(&self, rejection: &Rejection, endpoint: &Endpoint) -> Option<AiError> {
        rejected(rejection, endpoint)
    }
}

// ---- error bodies -----------------------------------------------------------------------------------------

/// The error object of the vendor's error body. Unknown members are ignored.
#[derive(Deserialize)]
struct ErrorObject {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// The error body of an answer that was not a success: `{"type":"error","error":{"type":..,"message":..}}`.
#[derive(Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    error: Option<ErrorObject>,
}

/// The error of an HTTP answer, read from its body. A body that is not the vendor's error shape, or names a type this
/// version does not know, gives `None` and the status decides. The message of the provider is shown only through
/// [`Endpoint::scrub`], which masks the keys of the endpoint.
fn rejected(rejection: &Rejection, endpoint: &Endpoint) -> Option<AiError> {
    let envelope: ErrorEnvelope = serde_json::from_slice(&rejection.body).ok()?;
    let object = envelope.error?;
    let kind = object.kind?;
    let message = object.message.unwrap_or_default();
    if kind == "invalid_request_error" && message.to_lowercase().contains("prompt is too long") {
        return Some(AiError::ContextLength);
    }
    error_of(&kind, Some(rejection.status), rejection.retry_after, shown_text(endpoint, &message))
}

// ---- typed events -----------------------------------------------------------------------------------------
//
// Only the structure the adapter depends on is named. Unknown members are ignored and an unknown type is a unit
// variant, so a provider that adds an event or a field changes nothing here.

/// The token counters a report can carry. Each is read when present; the last report wins.
#[derive(Deserialize, Default)]
struct Report {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

#[derive(Deserialize, Default)]
struct StartMessage {
    #[serde(default)]
    usage: Report,
}

#[derive(Deserialize, Default)]
struct MessageDeltaBody {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct ErrorBody {
    #[serde(rename = "type", default)]
    kind: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event<'a> {
    MessageStart {
        #[serde(default)]
        message: StartMessage,
    },
    ContentBlockStart {
        index: u32,
        content_block: Value,
    },
    ContentBlockDelta {
        index: u32,
        #[serde(borrow)]
        delta: Delta<'a>,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        #[serde(default)]
        delta: MessageDeltaBody,
        #[serde(default)]
        usage: Report,
    },
    MessageStop,
    Ping,
    Error {
        #[serde(default)]
        error: Option<ErrorBody>,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Delta<'a> {
    #[serde(rename = "text_delta")]
    Text {
        #[serde(default, borrow)]
        text: Cow<'a, str>,
    },
    #[serde(rename = "thinking_delta")]
    Thinking {
        #[serde(default, borrow)]
        thinking: Cow<'a, str>,
    },
    #[serde(rename = "signature_delta")]
    Signature {
        #[serde(default, borrow)]
        signature: Cow<'a, str>,
    },
    #[serde(rename = "input_json_delta")]
    InputJson {
        #[serde(default, borrow)]
        partial_json: Cow<'a, str>,
    },
    #[serde(other)]
    Other,
}

// ---- the turn ---------------------------------------------------------------------------------------------

/// One content block of the assistant turn, as far as it has arrived. `start` is the object the provider opened the
/// block with; it is kept so that members this version does not know are replayed with the block.
enum Block {
    /// A text block: the text so far and whether the block was closed.
    Text { start: Value, text: String, done: bool },
    /// A reasoning block: the summary so far and the signature. An omitted summary is empty; the signature is not.
    Thinking { start: Value, thinking: String, signature: String, done: bool },
    /// A reasoning block the provider withheld. It is kept exactly as it was opened.
    Redacted { start: Value, done: bool },
    /// A tool use: its id, and whether its arguments were closed. The arguments live in the assembler.
    Tool { start: Value, id: String, done: bool },
}

impl fmt::Debug for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Block::Text { text, done, .. } => {
                f.debug_struct("Text").field("text", &Size(text.len())).field("done", done).finish()
            }
            Block::Thinking { thinking, signature, done, .. } => f
                .debug_struct("Thinking")
                .field("thinking", &Size(thinking.len()))
                .field("signature", &Size(signature.len()))
                .field("done", done)
                .finish(),
            Block::Redacted { done, .. } => f.debug_struct("Redacted").field("done", done).finish(),
            Block::Tool { id, done, .. } => {
                f.debug_struct("Tool").field("id", &Size(id.len())).field("done", done).finish()
            }
        }
    }
}

impl Block {
    /// Whether the provider closed the block. A closed block takes no more deltas and no second stop.
    fn is_done(&self) -> bool {
        match self {
            Block::Text { done, .. }
            | Block::Thinking { done, .. }
            | Block::Redacted { done, .. }
            | Block::Tool { done, .. } => *done,
        }
    }
}

/// Why the model stopped, as the provider names it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    Other,
}

fn stop_of(reason: &str) -> Stop {
    match reason {
        "end_turn" | "stop_sequence" => Stop::EndTurn,
        "tool_use" => Stop::ToolUse,
        "max_tokens" => Stop::MaxTokens,
        "refusal" => Stop::Refusal,
        _ => Stop::Other,
    }
}

/// The reader of one answer.
struct AnthropicTurn {
    started: bool,
    stopped: bool,
    blocks: BTreeMap<u32, Block>,
    assembler: ToolAssembler,
    input: u64,
    cache_creation: u64,
    cache_read: u64,
    output: u64,
    stop: Option<Stop>,
    /// The text, reasoning, signature, redacted data and tool-argument bytes of the turn so far.
    bytes: usize,
    /// An error was returned: nothing more is read or delivered.
    failed: bool,
}

impl fmt::Debug for AnthropicTurn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicTurn")
            .field("started", &self.started)
            .field("stopped", &self.stopped)
            .field("failed", &self.failed)
            .field("blocks", &self.blocks)
            .field("assembler", &self.assembler)
            .field("accumulated", &Size(self.bytes))
            .finish()
    }
}

fn saturate(count: u64) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Adds `bytes` to the running total of a turn. Passing [`MAX_TURN_BYTES`] ends the turn, before the bytes are delivered or
/// kept.
fn charge(total: &mut usize, bytes: usize) -> Result<(), AiError> {
    *total = total.saturating_add(bytes);
    if *total > MAX_TURN_BYTES { Err(protocol(PHRASE_TOO_LARGE)) } else { Ok(()) }
}

/// The text member `key` of `object`, empty when it is missing or not text.
fn text_of<'v>(object: &'v Value, key: &str) -> &'v str {
    object.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Sets the member `key` of `object` to `value`, when `object` is an object.
fn set(object: &mut Value, key: &str, value: Value) {
    if let Some(map) = object.as_object_mut() {
        map.insert(key.to_owned(), value);
    }
}

impl AnthropicTurn {
    fn new() -> Self {
        Self {
            started: false,
            stopped: false,
            blocks: BTreeMap::new(),
            assembler: ToolAssembler::new(),
            input: 0,
            cache_creation: 0,
            cache_read: 0,
            output: 0,
            stop: None,
            bytes: 0,
            failed: false,
        }
    }

    /// Folds a report into the counters: each counter that is present replaces the one before it.
    fn apply(&mut self, report: &Report) -> bool {
        let mut seen = false;
        for (slot, value) in [
            (&mut self.input, report.input_tokens),
            (&mut self.cache_creation, report.cache_creation_input_tokens),
            (&mut self.cache_read, report.cache_read_input_tokens),
            (&mut self.output, report.output_tokens),
        ] {
            if let Some(value) = value {
                *slot = value;
                seen = true;
            }
        }
        seen
    }

    fn usage(&self) -> Usage {
        let input = self.input.saturating_add(self.cache_creation).saturating_add(self.cache_read);
        Usage { input_tokens: saturate(input), output_tokens: saturate(self.output) }
    }

    fn handle(&mut self, event: Event<'_>, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
        match event {
            Event::MessageStart { message } => {
                if self.started {
                    return Err(protocol("the response starts a message twice"));
                }
                self.started = true;
                if self.apply(&message.usage) && out(StreamEvent::Usage(self.usage())) == Flow::Stop {
                    return Ok(Flow::Stop);
                }
            }
            Event::ContentBlockStart { index, content_block } => {
                self.require_started()?;
                return self.start_block(index, &content_block, out);
            }
            Event::ContentBlockDelta { index, delta } => {
                self.require_started()?;
                return self.delta(index, delta, out);
            }
            Event::ContentBlockStop { index } => {
                self.require_started()?;
                self.close_block(index)?;
            }
            Event::MessageDelta { delta, usage } => {
                self.require_started()?;
                if let Some(reason) = delta.stop_reason {
                    self.stop = Some(stop_of(&reason));
                }
                if self.apply(&usage) && out(StreamEvent::Usage(self.usage())) == Flow::Stop {
                    return Ok(Flow::Stop);
                }
            }
            Event::MessageStop => {
                self.require_started()?;
                self.stopped = true;
            }
            Event::Error { error } => {
                // The message of the provider is never read: the turn has no endpoint to mask a key with.
                let kind = error.and_then(|error| error.kind);
                let known = kind.and_then(|kind| error_of(&kind, None, None, Detail::new(PHRASE_ERROR_EVENT)));
                return Err(known.unwrap_or_else(|| protocol(PHRASE_ERROR_EVENT)));
            }
            Event::Ping | Event::Other => {}
        }
        Ok(Flow::Continue)
    }

    fn require_started(&self) -> Result<(), AiError> {
        if self.started { Ok(()) } else { Err(protocol("the response sends content before its message starts")) }
    }

    fn start_block(
        &mut self,
        index: u32,
        start: &Value,
        out: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<Flow, AiError> {
        if !usize::try_from(index).is_ok_and(|index| index < MAX_BLOCKS) {
            return Err(protocol(PHRASE_TOO_MANY_BLOCKS));
        }
        if self.blocks.contains_key(&index) {
            return Err(protocol("the response opens a content block twice"));
        }
        // The whole object counts: it holds the first text, the data of a redacted block, and whatever else was sent.
        charge(&mut self.bytes, json_size(start).0)?;
        let mut flow = Flow::Continue;
        let block = match start.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = text_of(start, "text").to_owned();
                if !text.is_empty() {
                    flow = out(StreamEvent::TextDelta(text.clone()));
                }
                let mut start = start.clone();
                set(&mut start, "text", Value::String(String::new()));
                Block::Text { start, text, done: false }
            }
            Some("thinking") => {
                let thinking = text_of(start, "thinking").to_owned();
                let signature = text_of(start, "signature").to_owned();
                if !thinking.is_empty() {
                    flow = out(StreamEvent::ReasoningDelta(thinking.clone()));
                }
                let mut start = start.clone();
                set(&mut start, "thinking", Value::String(String::new()));
                set(&mut start, "signature", Value::String(String::new()));
                Block::Thinking { start, thinking, signature, done: false }
            }
            Some("redacted_thinking") => Block::Redacted { start: start.clone(), done: false },
            Some("tool_use") => {
                let (id, name) = (text_of(start, "id"), text_of(start, "name"));
                if id.is_empty() || name.is_empty() {
                    return Err(protocol(PHRASE_BAD_TOOL));
                }
                self.assembler.fragment(index, Some(id), Some(name), "").map_err(AiError::from)?;
                let mut start = start.clone();
                set(&mut start, "input", json!({}));
                Block::Tool { start, id: id.to_owned(), done: false }
            }
            _ => return Err(protocol(PHRASE_UNKNOWN_BLOCK)),
        };
        self.blocks.insert(index, block);
        Ok(flow)
    }

    fn delta(
        &mut self,
        index: u32,
        delta: Delta<'_>,
        out: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<Flow, AiError> {
        let Some(block) = self.blocks.get_mut(&index) else {
            return Err(protocol("the response sends a delta for a content block that is not open"));
        };
        // A kind of delta this version does not know is ignored, whatever block it comes in.
        if matches!(delta, Delta::Other) {
            return Ok(Flow::Continue);
        }
        if block.is_done() {
            return Err(protocol(PHRASE_MISFIT));
        }
        match (block, delta) {
            (Block::Text { text, .. }, Delta::Text { text: piece }) => {
                if piece.is_empty() {
                    return Ok(Flow::Continue);
                }
                charge(&mut self.bytes, piece.len())?;
                text.push_str(&piece);
                Ok(out(StreamEvent::TextDelta(piece.into_owned())))
            }
            (Block::Thinking { thinking, .. }, Delta::Thinking { thinking: piece }) => {
                if piece.is_empty() {
                    return Ok(Flow::Continue);
                }
                charge(&mut self.bytes, piece.len())?;
                thinking.push_str(&piece);
                Ok(out(StreamEvent::ReasoningDelta(piece.into_owned())))
            }
            (Block::Thinking { signature, .. }, Delta::Signature { signature: piece }) => {
                charge(&mut self.bytes, piece.len())?;
                signature.push_str(&piece);
                Ok(Flow::Continue)
            }
            (Block::Tool { .. }, Delta::InputJson { partial_json }) => {
                charge(&mut self.bytes, partial_json.len())?;
                self.assembler.fragment(index, None, None, &partial_json).map_err(AiError::from)?;
                Ok(Flow::Continue)
            }
            _ => Err(protocol(PHRASE_MISFIT)),
        }
    }

    fn close_block(&mut self, index: u32) -> Result<(), AiError> {
        let Some(block) = self.blocks.get_mut(&index) else {
            return Err(protocol("the response closes a content block that was never opened"));
        };
        if block.is_done() {
            return Err(protocol("the response closes a content block twice"));
        }
        match block {
            Block::Text { done, .. } | Block::Thinking { done, .. } | Block::Redacted { done, .. } => *done = true,
            Block::Tool { done, .. } => {
                *done = true;
                self.assembler.complete(index).map_err(AiError::from)?;
            }
        }
        Ok(())
    }

    fn read(&mut self, frame: Frame<'_>, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
        let Frame::Event(event) = frame else {
            return Err(protocol(PHRASE_FRAME));
        };
        // After the final event nothing is read: a provider may add events there, and none of them matters.
        if self.stopped || event.is_done() || event.data().trim().is_empty() {
            return Ok(Flow::Continue);
        }
        let event: Event<'_> = serde_json::from_str(event.data()).map_err(|_| protocol(PHRASE_UNREADABLE))?;
        self.handle(event, out)
    }
}

impl WireTurn for AnthropicTurn {
    fn frame(&mut self, frame: Frame<'_>, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
        if self.failed {
            return Ok(Flow::Stop);
        }
        let result = self.read(frame, out);
        self.failed = result.is_err();
        result
    }

    fn finish(
        self: Box<Self>,
        end: StreamEnd,
        out: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<Completion, AiError> {
        if self.failed {
            return Err(protocol(PHRASE_FAILED_EARLIER));
        }
        match end {
            StreamEnd::Stopped => return Err(AiError::Cancelled),
            StreamEnd::Truncated { .. } => return Err(AiError::Network(Detail::new(PHRASE_TRUNCATED))),
            StreamEnd::Clean => {}
        }
        if !self.started {
            return Err(protocol(PHRASE_NO_MESSAGE));
        }
        if !self.stopped {
            return Err(protocol(PHRASE_NO_STOP));
        }
        let usage = self.usage();
        let stop_reason = match self.stop {
            Some(Stop::EndTurn) => StopReason::EndTurn,
            Some(Stop::ToolUse) => StopReason::ToolUse,
            Some(Stop::MaxTokens) => StopReason::MaxTokens,
            Some(Stop::Refusal) => StopReason::Refusal,
            Some(Stop::Other) | None => StopReason::Other,
        };
        // The provider declined: whatever tool uses came before are not made, and the text stays with the caller.
        if self.stop == Some(Stop::Refusal) {
            return Err(AiError::ContentFilter);
        }
        let tool_turn = self.stop == Some(Stop::ToolUse);
        let AnthropicTurn { blocks, assembler, .. } = *self;
        let released = assembler.finish(end, true);
        let calls = if tool_turn {
            let calls = released.map_err(AiError::from)?;
            if calls.is_empty() {
                return Err(protocol(PHRASE_NO_CALL));
            }
            calls
        } else {
            // A turn that did not end as a tool-use turn (cut by the output bound, or ended in some other way) makes no
            // call, whatever fragments or complete calls it held, and it keeps no tool block in its native turn.
            let _ = released;
            Vec::new()
        };
        let mut content = Vec::with_capacity(blocks.len());
        for block in blocks.into_values() {
            match block {
                Block::Text { mut start, text, .. } => {
                    if !text.is_empty() {
                        set(&mut start, "text", Value::String(text));
                        content.push(start);
                    }
                }
                Block::Thinking { mut start, thinking, signature, .. } => {
                    // A reasoning block without a signature cannot be replayed, so it is not kept.
                    if !signature.is_empty() {
                        set(&mut start, "thinking", Value::String(thinking));
                        set(&mut start, "signature", Value::String(signature));
                        content.push(start);
                    }
                }
                Block::Redacted { start, .. } => content.push(start),
                Block::Tool { mut start, id, .. } => {
                    if let Some(call) = calls.iter().find(|call| call.id == id) {
                        set(&mut start, "input", call.args.clone());
                        content.push(start);
                    }
                }
            }
        }
        let native = (!content.is_empty())
            .then(|| Opaque { wire: Wire::Anthropic.tag().to_owned(), json: Value::Array(content) });
        for call in calls {
            if out(StreamEvent::ToolCall(call)) == Flow::Stop {
                return Err(AiError::Cancelled);
            }
        }
        Ok(Completion { stop_reason, usage, native })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::gate::{Destination, Gate, OfferRequest, RequestItem};
    use crate::ai::redact::SourceText;
    use crate::ai::types::{Consent, ContextKind, NoteItem};

    #[test]
    fn model_families_decide_thinking_and_capabilities() {
        let adapter = AnthropicAdapter::new(Flavor::Gateway);
        let adaptive = [
            "claude-opus-5-5",
            "claude-opus-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-fable-5-1",
            "claude-mythos-5-1",
            "claude-opus-4-8",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
        ];
        for name in adaptive {
            for model in [name.to_owned(), format!("vendor/{name}")] {
                let caps = adapter.capabilities(&model, Locality::Cloud);
                assert!(caps.tools && caps.reasoning && caps.vision, "{model}: {caps:?}");
                assert_eq!(caps.context_tokens, Some(1_000_000), "{model}");
                assert_eq!(caps.max_output_tokens, Some(128_000), "{model}");
                assert_eq!(caps.locality, Locality::Cloud);
            }
        }

        let small = adapter.capabilities("claude-haiku-4-5", Locality::Cloud);
        assert!(small.tools && small.vision && !small.reasoning, "{small:?}");
        assert_eq!(small.context_tokens, Some(200_000));
        assert_eq!(small.max_output_tokens, None);

        for model in ["claude-opus-4-5", "claude-opus-50", "claude-opus-5x", "qwen2.5-coder", "", "/"] {
            let caps = adapter.capabilities(model, Locality::Loopback);
            assert!(caps.tools, "{model}: an unknown model may still call tools");
            assert!(!caps.reasoning && !caps.vision, "{model}: {caps:?}");
            assert_eq!((caps.context_tokens, caps.max_output_tokens), (None, None), "{model}");
            assert_eq!(caps.locality, Locality::Loopback);
        }
    }

    fn approved(provider: &str, text: &str) -> crate::ai::gate::ApprovedContext {
        let gate = Gate::new();
        let destination = Destination { host: "localhost".to_owned(), locality: Locality::Loopback };
        let item = RequestItem::new(ContextKind::Screen, "build pane", SourceText::plain(text));
        let offer = gate.offer(OfferRequest::new(provider, destination, vec![item])).expect("an offer");
        gate.approve(offer.id(), &[1], provider).expect("an approval")
    }

    #[test]
    fn context_notes_render_the_envelope_only_when_approved_for_this_request() {
        let context = approved("anthropic", "error: the linker failed");
        let envelope = context.envelope().to_owned();
        let note = ContextNote::for_approved(&context, Some("CANARY-STORED-TEXT".to_owned()));
        let message = Message::new(
            "u1",
            Role::User,
            vec![Part::Context(note.clone()), Part::Text { text: "what failed?".to_owned() }],
            1,
        );

        // With the approval in the request: the envelope, unchanged, as its own block in the place of the note.
        let mut request = ChatRequest::new("claude-opus-5-5");
        request.messages.push(message.clone());
        request.context.push(context.clone());
        assert_eq!(context_text(&request, &note), envelope);
        let blocks = user_blocks(&request, &request.messages[0]);
        assert_eq!(
            blocks,
            vec![json!({ "type": "text", "text": envelope }), json!({ "type": "text", "text": "what failed?" })]
        );
        assert_eq!(serde_json::to_string(&blocks).expect("JSON").matches("the linker failed").count(), 1);

        // Without it the note is history: one marker line that names the kind, the label and the size, and never the
        // envelope or the text the note kept.
        let mut history = ChatRequest::new("claude-opus-5-5");
        history.messages.push(message);
        let marker = context_text(&history, &note);
        assert_eq!(
            marker,
            format!(
                "[terminal context from an earlier message is not sent again: screen \"build pane\" ({} bytes)]",
                context.items()[0].bytes
            )
        );
        assert!(!marker.contains("linker") && !marker.contains("CANARY-STORED-TEXT"), "{marker}");
        let blocks = user_blocks(&history, &history.messages[0]);
        assert_eq!(blocks.len(), 2);
        assert!(!serde_json::to_string(&blocks).expect("JSON").contains("CANARY-STORED-TEXT"));

        // A note that was approved for a different request does not match: the hash is the key.
        let other = approved("anthropic", "another screen");
        let mut mismatch = ChatRequest::new("claude-opus-5-5");
        mismatch.context.push(other);
        assert_eq!(context_text(&mismatch, &note), marker);

        // Several items are listed in order, a quote in a label cannot close the quoted label, and a note with no item
        // is one short line.
        let many = ContextNote {
            hash: "0".repeat(64),
            items: vec![
                NoteItem { kind: ContextKind::File, label: "src/\"main\".rs".to_owned(), bytes: 12 },
                NoteItem { kind: ContextKind::LastCommand, label: "cargo test".to_owned(), bytes: 3 },
            ],
            stored: None,
            consented_to: Consent { provider: "anthropic".to_owned(), locality: Locality::Cloud },
        };
        assert_eq!(
            context_text(&history, &many),
            "[terminal context from an earlier message is not sent again: file \"src/'main'.rs\" (12 bytes); last_command \"cargo test\" (3 bytes)]"
        );
        let bare = ContextNote { items: Vec::new(), ..many };
        assert_eq!(context_text(&history, &bare), "[terminal context from an earlier message is not sent again]");
    }

    #[test]
    fn debug_output_never_shows_stream_or_request_text() {
        use crate::ai::sse::SseDecoder;

        const CANARY: &str = "CANARY-STREAM-TEXT";
        let events = [
            json!({ "type": "message_start", "message": { "usage": { "input_tokens": 5, "output_tokens": 1 } } }),
            json!({ "type": "content_block_start", "index": 0,
                    "content_block": { "type": "thinking", "thinking": "", "signature": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": CANARY } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": CANARY } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": CANARY } }),
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": CANARY } }),
            json!({ "type": "content_block_start", "index": 2,
                    "content_block": { "type": "redacted_thinking", "data": CANARY } }),
            json!({ "type": "content_block_start", "index": 3,
                    "content_block": { "type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {} } }),
            json!({ "type": "content_block_delta", "index": 3,
                    "delta": { "type": "input_json_delta", "partial_json": format!("{{\"path\": \"{CANARY}") } }),
        ];
        let mut bytes = Vec::new();
        for event in &events {
            bytes.extend_from_slice(format!("data: {event}\n\n").as_bytes());
        }
        let mut turn = AnthropicTurn::new();
        let mut decoder = SseDecoder::new();
        let mut frames = 0;
        decoder
            .push(&bytes, &mut |event| {
                turn.frame(Frame::Event(event), &mut |_| Flow::Continue).expect("a frame");
                frames += 1;
                Flow::Continue
            })
            .expect("the bytes decode");
        assert_eq!(frames, events.len());

        let adapter = AnthropicAdapter::new(Flavor::FirstParty);
        for shown in [format!("{turn:?}"), format!("{turn:#?}"), format!("{adapter:?}"), format!("{adapter:#?}")] {
            assert!(!shown.contains(CANARY), "the stream text reached a Debug line: {shown}");
        }
        assert!(format!("{turn:?}").contains("bytes"), "{turn:?}");
        assert_eq!(format!("{adapter:?}"), "AnthropicAdapter { flavor: \"first_party\" }");
    }
}
