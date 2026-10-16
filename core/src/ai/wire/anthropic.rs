//! The Anthropic Messages dialect, also spoken by the Cloudflare AI Gateway and by the native route of the Vercel AI
//! Gateway.
//!
//! - **The request is a pure function** of the [`ChatRequest`] and the [`Capabilities`]: the same input gives the same
//!   bytes. The body holds the model, an output bound that is always present, the stream flag, the system prompt when
//!   there is one, the messages and, when the request declares some, the tools. Nothing else is written: no sampling
//!   parameter, no forced tool choice, no caching marker and no hosted feature. A key belongs to the
//!   [`Endpoint`](super::Endpoint) and is added by the chokepoint, never here.
//! - **Two flavors.** [`Flavor::FirstParty`] is the vendor's own host and nothing else; [`Flavor::Gateway`] is every
//!   other address and sends the core fields only. Fields that only the vendor's host is known to accept are added for
//!   the first flavor alone.
//! - **The assistant turn is kept as the provider sent it.** The content blocks are assembled from the stream into
//!   [`Completion::native`] as an [`Opaque`] with the wire tag `anthropic` whose JSON is the array of content blocks.
//! - **The answer is read into typed events.** Text is borrowed from the frame while it is parsed. Unknown event types,
//!   unknown delta types and unknown extra fields are ignored; the structure the adapter depends on is checked, and a
//!   stream that breaks it fails the turn with a fixed phrase that holds no stream text.
//! - **Nothing here touches the network, a key or the environment.** The turn never prints the stream: its `Debug` shows
//!   counts and sizes only.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Endpoint, Wire, WireAdapter, WireRequest, WireTurn};
use crate::ai::http::{Frame, Header, Origin, Rejection};
use crate::ai::provider::{AiError, ChatRequest, Completion, Detail, Flow, StreamEvent};
use crate::ai::sse::StreamEnd;
use crate::ai::types::{Capabilities, Locality, Opaque, Part, Role, Size, StopReason, Usage};

// ---- constants and flavors --------------------------------------------------------------------------------

/// The value of the `anthropic-version` header. Every request carries it.
pub const API_VERSION: &str = "2023-06-01";
/// The path of the Messages endpoint below the base address. A gateway prefix belongs to the base address.
pub const MESSAGES_PATH: &str = "/v1/messages";
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

// ---- the request ------------------------------------------------------------------------------------------

/// One message as it is written: a role and its content blocks.
#[derive(Serialize)]
struct WireMessage {
    role: &'static str,
    content: Vec<Value>,
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
}

/// The text blocks of the request: the user and assistant messages with their text, in order. Empty text is skipped and
/// a message with no block is dropped.
fn text_messages(req: &ChatRequest) -> Result<Vec<WireMessage>, AiError> {
    let mut messages = Vec::with_capacity(req.messages.len());
    for message in &req.messages {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => return Err(invalid("the request holds a part this version cannot send")),
        };
        let mut content = Vec::new();
        for part in &message.parts {
            match part {
                Part::Text { text } => {
                    if !text.is_empty() {
                        content.push(serde_json::json!({ "type": "text", "text": text }));
                    }
                }
                _ => return Err(invalid("the request holds a part this version cannot send")),
            }
        }
        if !content.is_empty() {
            messages.push(WireMessage { role, content });
        }
    }
    if messages.is_empty() {
        return Err(invalid("the request has no messages"));
    }
    Ok(messages)
}

impl WireAdapter for AnthropicAdapter {
    fn wire(&self) -> Wire {
        Wire::Anthropic
    }

    fn capabilities(&self, _model: &str, locality: Locality) -> Capabilities {
        Capabilities { tools: true, ..Capabilities::conservative(locality) }
    }

    fn request(&self, req: &ChatRequest, _caps: &Capabilities) -> Result<WireRequest, AiError> {
        if req.model.is_empty() {
            return Err(invalid("the request names no model"));
        }
        let max_tokens = match req.max_output_tokens {
            Some(0) => return Err(invalid("the output bound must be more than zero")),
            Some(limit) => limit,
            None => DEFAULT_MAX_TOKENS,
        };
        let body =
            Body { model: &req.model, max_tokens, stream: true, system: &req.system, messages: text_messages(req)? };
        let body = serde_json::to_vec(&body).map_err(|_| invalid("the request could not be written as JSON"))?;
        let version =
            Header::new("anthropic-version", API_VERSION).map_err(|_| invalid("the version header is not valid"))?;
        Ok(WireRequest { path: MESSAGES_PATH.to_owned(), headers: vec![version], body })
    }

    fn turn(&self) -> Box<dyn WireTurn> {
        Box::new(AnthropicTurn::new())
    }

    fn rejection(&self, _rejection: &Rejection, _endpoint: &Endpoint) -> Option<AiError> {
        None
    }
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
    TextDelta {
        #[serde(default, borrow)]
        text: Cow<'a, str>,
    },
    ThinkingDelta {
        #[serde(default, borrow)]
        thinking: Cow<'a, str>,
    },
    SignatureDelta {
        #[serde(default, borrow)]
        signature: Cow<'a, str>,
    },
    InputJsonDelta {
        #[serde(default, borrow)]
        partial_json: Cow<'a, str>,
    },
    #[serde(other)]
    Other,
}

// ---- the turn ---------------------------------------------------------------------------------------------

/// One content block of the assistant turn, as far as it has arrived.
enum Block {
    /// A text block: the start object as the provider sent it, the text so far and whether the block was closed.
    Text { start: Value, text: String, done: bool },
}

impl fmt::Debug for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Block::Text { text, done, .. } => {
                f.debug_struct("Text").field("text", &Size(text.len())).field("done", done).finish()
            }
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
    input: u64,
    cache_creation: u64,
    cache_read: u64,
    output: u64,
    stop: Option<Stop>,
}

impl fmt::Debug for AnthropicTurn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicTurn")
            .field("started", &self.started)
            .field("stopped", &self.stopped)
            .field("blocks", &self.blocks)
            .finish()
    }
}

fn saturate(count: u64) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

impl AnthropicTurn {
    fn new() -> Self {
        Self {
            started: false,
            stopped: false,
            blocks: BTreeMap::new(),
            input: 0,
            cache_creation: 0,
            cache_read: 0,
            output: 0,
            stop: None,
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
                match self.blocks.get_mut(&index) {
                    Some(Block::Text { done, .. }) => *done = true,
                    None => return Err(protocol("the response closes a content block that was never opened")),
                }
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
            Event::Error { .. } => return Err(protocol(PHRASE_ERROR_EVENT)),
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
        if self.blocks.contains_key(&index) {
            return Err(protocol("the response opens a content block twice"));
        }
        match start.get("type").and_then(Value::as_str) {
            Some("text") => {
                let initial = start.get("text").and_then(Value::as_str).unwrap_or("").to_owned();
                let mut start = start.clone();
                if let Some(object) = start.as_object_mut() {
                    object.insert("text".to_owned(), Value::String(String::new()));
                }
                let flow =
                    if initial.is_empty() { Flow::Continue } else { out(StreamEvent::TextDelta(initial.clone())) };
                self.blocks.insert(index, Block::Text { start, text: initial, done: false });
                Ok(flow)
            }
            _ => Err(protocol(PHRASE_UNKNOWN_BLOCK)),
        }
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
        match (block, delta) {
            (Block::Text { text, .. }, Delta::TextDelta { text: piece }) => {
                if piece.is_empty() {
                    return Ok(Flow::Continue);
                }
                text.push_str(&piece);
                Ok(out(StreamEvent::TextDelta(piece.into_owned())))
            }
            (_, Delta::Other) => Ok(Flow::Continue),
            _ => Ok(Flow::Continue),
        }
    }
}

impl WireTurn for AnthropicTurn {
    fn frame(&mut self, frame: Frame<'_>, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
        let Frame::Event(event) = frame else {
            return Err(protocol(PHRASE_FRAME));
        };
        if self.stopped || event.is_done() || event.data().trim().is_empty() {
            return Ok(Flow::Continue);
        }
        let event: Event<'_> = serde_json::from_str(event.data()).map_err(|_| protocol(PHRASE_UNREADABLE))?;
        self.handle(event, out)
    }

    fn finish(
        self: Box<Self>,
        end: StreamEnd,
        _out: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<Completion, AiError> {
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
        let mut content = Vec::new();
        for block in self.blocks.into_values() {
            match block {
                Block::Text { mut start, text, done } => {
                    if done && !text.is_empty() {
                        if let Some(object) = start.as_object_mut() {
                            object.insert("text".to_owned(), Value::String(text));
                        }
                        content.push(start);
                    }
                }
            }
        }
        let native = (!content.is_empty())
            .then(|| Opaque { wire: Wire::Anthropic.tag().to_owned(), json: Value::Array(content) });
        Ok(Completion { stop_reason, usage, native })
    }
}
