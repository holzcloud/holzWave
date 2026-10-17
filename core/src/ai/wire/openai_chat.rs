//! The Chat Completions dialect, also spoken by many gateways and local servers: LM Studio, llama.cpp, OpenRouter, the
//! Vercel AI Gateway, LiteLLM and every custom server that calls itself compatible.
//!
//! The servers that speak it disagree about ids, finish markers, tool-argument encoding, usage, reasoning fields and
//! error shapes, so the adapter is strict about what it sends and lenient about what it reads.
//!
//! - **The request is a pure function** of the [`ChatRequest`] and the [`Capabilities`]: the same input gives the same
//!   bytes. The body carries the model, the messages, the two stream flags, an output bound only when the request sets
//!   one and, only when the model can use tools and the request declares some, the function definitions. Nothing else
//!   is written: no forced tool selection, no sampling parameter, no storage flag, no user or machine identifier, and
//!   no header. A key, an address or a header belongs to the [`Endpoint`] and is added by the chokepoint, never here.
//! - **Terminal text leaves only as an approved envelope.** A context note is rendered from the envelope of the
//!   approval the request itself carries, unchanged. A note without an approval is history: it becomes one marker line
//!   with labels and byte counts, and the text a note may have kept is never read.
//! - **The answer is read into typed structs.** Unknown fields, `null` and empty deltas are ignored; text is passed on
//!   as it arrives. The turn never prints the stream: its `Debug` shows counts and flags only.
//! - **A tool call is released once, after a clean end.** Fragments are collected by a
//!   [`ToolAssembler`](crate::ai::sse::ToolAssembler) and handed out from [`WireTurn::finish`] only, when a finish
//!   chunk said the model finished and the stream ended cleanly. Arguments that are not one JSON object are never
//!   repaired: the turn fails and no call leaves.
//! - **The end of a stream is evidence, not a guess.** A finish chunk proves that the model finished; the closing marker
//!   is optional. A stream that ends without either, or ends uncleanly, is an error and never `Ok`.
//! - **No server text is shown without a key mask.** An error in the middle of a stream becomes a classified
//!   [`AiError`] with a fixed phrase and, at most, a short validated code. Only [`rejected`], which has the endpoint,
//!   shows a message from an error body, and only through [`Endpoint::scrub`].

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{Endpoint, Wire, WireAdapter, WireRequest, WireTurn};
use crate::ai::http::{Frame, Rejection};
use crate::ai::provider::{AiError, ChatRequest, Completion, Detail, Flow, StreamEvent};
use crate::ai::sse::{StreamEnd, ToolAssembler, ToolFault};
use crate::ai::types::{
    Capabilities, ContextNote, Locality, Part, Role, StopReason, ToolCall, ToolSpec, Usage,
};

/// The path of the chat endpoint below the base address of the profile.
pub const CHAT_PATH: &str = "/chat/completions";

/// The adapter of the Chat Completions dialect. It has no field: everything it needs comes with the request.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenAiChatAdapter;

impl OpenAiChatAdapter {
    /// The adapter.
    pub fn new() -> Self {
        Self
    }
}

// ---- messages ---------------------------------------------------------------------------------------------
//
// Both chat dialects need the same plan of the transcript: what the user said, what the model said and asked for, and
// what the tools answered. It is made once, here, and the native dialect of Ollama writes it in its own shape.

/// One message of the plan, with the text already rendered.
pub(super) enum Plain {
    /// What the user wrote, with any approved context in front of it.
    User {
        /// The text of the message.
        text: String,
    },
    /// What the model said and asked for.
    Assistant {
        /// The text, empty when the model only asked for tools.
        text: String,
        /// The calls the model made, in order.
        calls: Vec<ToolCall>,
    },
    /// The answer of a tool. Results are never merged.
    Result {
        /// The id of the call this answers.
        call_id: String,
        /// The content, verbatim.
        content: String,
        /// The name of the tool, when an earlier call of the same request carried this id. The native dialect of
        /// Ollama names the tool in its answer; this dialect does not.
        #[allow(dead_code)]
        name: Option<String>,
    },
}

/// Appends `block` to `out` after one blank line: a single line feed when `out` already ends with one.
pub(super) fn join_blocks(out: &mut String, block: &str) {
    if block.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
    }
    out.push_str(block);
}

/// Adds a user message to the plan, merging it into the one before it: servers whose chat templates demand alternation
/// refuse two in a row.
fn push_user(plan: &mut Vec<Plain>, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some(Plain::User { text: before }) = plan.last_mut() {
        join_blocks(before, &text);
    } else {
        plan.push(Plain::User { text });
    }
}

/// Adds an assistant message to the plan, merging it into the one before it. A message with nothing in it is dropped, so
/// its neighbours become adjacent and merge.
fn push_assistant(plan: &mut Vec<Plain>, text: String, calls: Vec<ToolCall>) {
    if text.is_empty() && calls.is_empty() {
        return;
    }
    if let Some(Plain::Assistant { text: before, calls: earlier }) = plan.last_mut() {
        join_blocks(before, &text);
        earlier.extend(calls);
    } else {
        plan.push(Plain::Assistant { text, calls });
    }
}

/// What a context note becomes in a request. With an approval in the request: the envelope the user saw, unchanged.
/// Without one the note is history, and one marker line stands in for it: the kind, the cleaned label and the size of
/// each item. Nothing else of the note is read.
fn render_context(req: &ChatRequest, note: &ContextNote) -> String {
    if let Some(approved) = req.approved_for(&note.hash) {
        return approved.envelope().to_owned();
    }
    super::marker_line(note)
}

/// The plan of `req.messages`: user and assistant text in order, the calls of the model, the results of the tools and
/// the context of the user, adjacent messages of one side merged and empty ones dropped. A request with nothing left is
/// [`AiError::InvalidRequest`].
pub(super) fn plain_messages(req: &ChatRequest) -> Result<Vec<Plain>, AiError> {
    let mut plan: Vec<Plain> = Vec::with_capacity(req.messages.len());
    // The names of the calls seen so far, for the dialect that names the tool in its answer.
    let mut names: HashMap<&str, &str> = HashMap::new();
    for message in &req.messages {
        match message.role {
            Role::User => {
                let mut text = String::new();
                for part in &message.parts {
                    match part {
                        Part::Text { text: block } => join_blocks(&mut text, block),
                        Part::Context(note) => join_blocks(&mut text, &render_context(req, note)),
                        Part::ToolCall(_) | Part::ToolResult(_) => {}
                    }
                }
                push_user(&mut plan, text);
            }
            Role::Assistant => {
                let mut text = String::new();
                let mut calls = Vec::new();
                for part in &message.parts {
                    match part {
                        Part::Text { text: block } => join_blocks(&mut text, block),
                        Part::ToolCall(call) => {
                            names.insert(&call.id, &call.name);
                            calls.push(call.clone());
                        }
                        Part::ToolResult(_) | Part::Context(_) => {}
                    }
                }
                push_assistant(&mut plan, text, calls);
            }
            Role::Tool => {
                for part in &message.parts {
                    if let Part::ToolResult(result) = part {
                        plan.push(Plain::Result {
                            call_id: result.call_id.clone(),
                            content: result.content.clone(),
                            name: names.get(result.call_id.as_str()).map(|name| (*name).to_owned()),
                        });
                    }
                }
            }
        }
    }
    if plan.is_empty() {
        return Err(AiError::InvalidRequest(Detail::new("the request has no messages")));
    }
    Ok(plan)
}

// ---- tool declarations ------------------------------------------------------------------------------------

/// One declared tool in the function-definition shape. The schema is borrowed as it is, and nothing marks the
/// definition as strict.
#[derive(Serialize)]
pub(super) struct ToolDecl<'a> {
    r#type: &'static str,
    function: ToolFunction<'a>,
}

#[derive(Serialize)]
struct ToolFunction<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a Value,
}

/// The function definitions of `tools`, in order.
pub(super) fn declared_tools(tools: &[ToolSpec]) -> Vec<ToolDecl<'_>> {
    tools
        .iter()
        .map(|tool| ToolDecl {
            r#type: "function",
            function: ToolFunction { name: &tool.name, description: &tool.description, parameters: &tool.input_schema },
        })
        .collect()
}

// ---- the request body -------------------------------------------------------------------------------------

/// A call the model made, as it is written back.
#[derive(Serialize)]
struct WireCall<'a> {
    id: &'a str,
    r#type: &'static str,
    function: WireCallFunction<'a>,
}

#[derive(Serialize)]
struct WireCallFunction<'a> {
    name: &'a str,
    /// The arguments as compact JSON text: this dialect wants a string.
    arguments: String,
}

/// One message as it is written.
#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<WireCall<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
}

impl<'a> WireMessage<'a> {
    fn text(role: &'static str, content: &'a str) -> Self {
        Self { role, content: Some(content), tool_calls: Vec::new(), tool_call_id: None }
    }
}

/// The `stream_options` object: the usage of a turn is asked for on every request.
#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// The request body. Every optional key is left out when it is not set; no key outside this list is ever written.
#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    stream: bool,
    stream_options: StreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ToolDecl<'a>>,
}

impl WireAdapter for OpenAiChatAdapter {
    fn wire(&self) -> Wire {
        Wire::OpenAiChat
    }

    fn capabilities(&self, _model: &str, locality: Locality) -> Capabilities {
        Capabilities { tools: true, ..Capabilities::conservative(locality) }
    }

    fn request(&self, req: &ChatRequest, caps: &Capabilities) -> Result<WireRequest, AiError> {
        let plan = plain_messages(req)?;
        let mut messages = Vec::with_capacity(plan.len() + 1);
        if !req.system.is_empty() {
            messages.push(WireMessage::text("system", &req.system));
        }
        for entry in &plan {
            messages.push(match entry {
                Plain::User { text } => WireMessage::text("user", text),
                Plain::Assistant { text, calls } => WireMessage {
                    role: "assistant",
                    content: (!text.is_empty()).then_some(text.as_str()),
                    tool_calls: calls
                        .iter()
                        .map(|call| WireCall {
                            id: &call.id,
                            r#type: "function",
                            function: WireCallFunction { name: &call.name, arguments: call.args.to_string() },
                        })
                        .collect(),
                    tool_call_id: None,
                },
                Plain::Result { call_id, content, .. } => WireMessage {
                    role: "tool",
                    content: Some(content),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(call_id),
                },
            });
        }
        let body = Body {
            model: &req.model,
            messages,
            stream: true,
            stream_options: StreamOptions { include_usage: true },
            max_tokens: req.max_output_tokens,
            tools: if caps.tools { declared_tools(&req.tools) } else { Vec::new() },
        };
        let body = serde_json::to_vec(&body)
            .map_err(|_| AiError::InvalidRequest(Detail::new("the request could not be written as JSON")))?;
        Ok(WireRequest { path: CHAT_PATH.to_owned(), headers: Vec::new(), body })
    }

    fn turn(&self) -> Box<dyn WireTurn> {
        Box::new(ChatTurn::new())
    }

    fn rejection(&self, rejection: &Rejection, endpoint: &Endpoint) -> Option<AiError> {
        rejected(rejection, endpoint)
    }
}

// ---- errors -----------------------------------------------------------------------------------------------

/// The phrase of an error that arrives in the middle of a stream.
const STREAM_ERROR_PHRASE: &str = "the server reported an error while streaming";

/// Words in a code or a type that name a context that is too long.
const CONTEXT_TOKENS: [&str; 3] = ["context_length", "context_window", "exceed_context"];
/// Words in a message that name a context that is too long. Only the class is read from a message, never shown.
const CONTEXT_WORDS: [&str; 5] =
    ["context length", "context window", "context size", "context overflow", "maximum context"];
/// Words in a code or a type that name a content filter.
const FILTER_TOKENS: [&str; 2] = ["content_filter", "content_policy"];
/// Words in a code or a type that name a refused credential or an account that cannot pay.
const AUTH_TOKENS: [&str; 4] = ["invalid_api_key", "authentication", "permission", "insufficient_quota"];

/// A code or a type as text for comparison: lower case and bounded. A number is its digits.
fn lowered(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.chars().take(128).collect::<String>().to_ascii_lowercase()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// A status from a code that is a number, or text of up to three digits, in the range of HTTP statuses.
fn status_of(value: Option<&Value>) -> Option<u16> {
    let number = match value? {
        Value::Number(number) => number.as_u64()?,
        Value::String(text) if text.len() <= 3 && !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
            text.parse::<u64>().ok()?
        }
        _ => return None,
    };
    u16::try_from(number).ok().filter(|status| (100..=599).contains(status))
}

/// A code that is safe to show: lower-case ASCII letters, digits, dot, dash and underscore, at most 48 characters.
fn shown_token(value: Option<&Value>) -> Option<String> {
    let text = match value? {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => return None,
    };
    let fits = !text.is_empty()
        && text.len() <= 48
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_'));
    fits.then_some(text)
}

/// Whether any of the two texts contains any of the words.
fn names_any(texts: &[Option<String>; 2], words: &[&str]) -> bool {
    texts.iter().flatten().any(|text| words.iter().any(|word| text.contains(word)))
}

/// The detail of an error that arrives in the middle of a stream: the fixed phrase and the validated code.
fn stream_detail(token: Option<&str>) -> Detail {
    match token {
        Some(token) => Detail::new(&format!("{STREAM_ERROR_PHRASE} ({token})")),
        None => Detail::new(STREAM_ERROR_PHRASE),
    }
}

/// The error for an `error` member inside a stream: an object with an optional `code` (text or number) and `type`, or a
/// plain string. The class comes from the code, the type and the status the code names; the server's message is never
/// read, and the only server text in the result is a code that passes [`shown_token`].
pub(super) fn stream_error(error: &Value) -> AiError {
    let code = error.get("code");
    let kind = error.get("type");
    let texts = [lowered(code), lowered(kind)];
    let status = status_of(code);
    let token = shown_token(code).or_else(|| shown_token(kind));
    let detail = stream_detail(token.as_deref());
    if names_any(&texts, &CONTEXT_TOKENS) {
        AiError::ContextLength
    } else if names_any(&texts, &FILTER_TOKENS) {
        AiError::ContentFilter
    } else if names_any(&texts, &AUTH_TOKENS) {
        AiError::Auth(detail)
    } else if names_any(&texts, &["rate_limit"]) || status == Some(429) {
        AiError::RateLimited { retry_after: None }
    } else if names_any(&texts, &["overloaded"]) || matches!(status, Some(503 | 529)) {
        AiError::Overloaded
    } else if matches!(status, Some(401..=403)) {
        AiError::Auth(detail)
    } else if names_any(&texts, &["invalid_request"]) || status.is_some_and(|status| (400..500).contains(&status)) {
        AiError::InvalidRequest(detail)
    } else {
        AiError::Server { status: status.filter(|status| *status >= 500).unwrap_or(500), detail }
    }
}

/// Whether an `error` member says anything: `null`, an empty string and an empty object do not.
fn says_something(error: &Value) -> bool {
    !(error.is_null() || error.as_str().is_some_and(str::is_empty) || error.as_object().is_some_and(Map::is_empty))
}

/// The error of an answer that was not a success, when the body is the error object of this dialect: an `error` that is
/// an object or a string, or a top-level `message`. The message is shown only through [`Endpoint::scrub`], so a key the
/// server echoed is masked. A context that is too long and a content filter are named by the body whatever the status;
/// for 408, 413, 429, 503 and 529 and for everything that is not an error body the answer is `None`, and the status
/// decides.
pub(super) fn rejected(rejection: &Rejection, endpoint: &Endpoint) -> Option<AiError> {
    let body: Value = serde_json::from_slice(&rejection.body).ok()?;
    let object = body.as_object()?;
    let (message, code, kind) = match object.get("error") {
        Some(Value::Object(error)) => {
            (error.get("message").and_then(Value::as_str), error.get("code"), error.get("type"))
        }
        Some(Value::String(text)) => (Some(text.as_str()), None, None),
        _ => (Some(object.get("message").and_then(Value::as_str)?), object.get("code"), object.get("type")),
    };
    let texts = [lowered(code), lowered(kind)];
    let spoken = message.map(|text| text.chars().take(400).collect::<String>().to_ascii_lowercase());
    let status = rejection.status;
    if names_any(&texts, &CONTEXT_TOKENS)
        || spoken.as_deref().is_some_and(|text| CONTEXT_WORDS.iter().any(|word| text.contains(word)))
    {
        return Some(AiError::ContextLength);
    }
    if names_any(&texts, &FILTER_TOKENS) {
        return Some(AiError::ContentFilter);
    }
    if matches!(status, 408 | 413 | 429 | 503 | 529) || !(400..600).contains(&status) {
        return None;
    }
    let shown = match message {
        Some(text) => format!("{status}: {}", text.chars().take(400).collect::<String>()),
        None => status.to_string(),
    };
    let detail = endpoint.scrub(&shown);
    Some(match status {
        401..=403 => AiError::Auth(detail),
        400..=499 => AiError::InvalidRequest(detail),
        _ => AiError::Server { status, detail },
    })
}

// ---- ids --------------------------------------------------------------------------------------------------

/// The longest base of a made-unique id: leaves room for the suffix inside the 256-byte bound of an id.
const ID_BASE_BYTES: usize = 240;

/// Takes `id` for a call of this turn: the id itself when no call holds it yet, else the id with `-2`, `-3` and so on,
/// the first suffix that no call holds. The base is cut at a character boundary so the result stays inside the bound
/// of an id.
pub(super) fn claim_id(claimed: &mut Vec<String>, id: &str) -> String {
    if !claimed.iter().any(|held| held == id) {
        claimed.push(id.to_owned());
        return id.to_owned();
    }
    let mut end = id.len().min(ID_BASE_BYTES);
    while !id.is_char_boundary(end) {
        end -= 1;
    }
    let base = &id[..end];
    let mut suffix = 2u32;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !claimed.contains(&candidate) {
            claimed.push(candidate.clone());
            return candidate;
        }
        suffix += 1;
    }
}

/// The calls of a turn with unique ids: the first holder of an id keeps it, a later holder gets a suffix.
pub(super) fn unique_ids(calls: Vec<ToolCall>) -> Vec<ToolCall> {
    let mut claimed = Vec::with_capacity(calls.len());
    calls
        .into_iter()
        .map(|mut call| {
            call.id = claim_id(&mut claimed, &call.id);
            call
        })
        .collect()
}

// ---- the answer -------------------------------------------------------------------------------------------

/// A count that does not fit is the largest count: usage never wraps around to a small number.
pub(super) fn saturate(count: u64) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// A token count from a JSON value: an unsigned number, or a non-negative one written with a fraction.
fn count_of(value: Option<&Value>) -> Option<u32> {
    let number = value?.as_number()?;
    if let Some(whole) = number.as_u64() {
        return Some(saturate(whole));
    }
    number.as_f64().filter(|real| real.is_finite() && *real >= 0.0).map(|real| saturate(real as u64))
}

/// The usage an object reports, when it reports at least one count.
fn usage_of(value: &Value) -> Option<Usage> {
    let prompt = count_of(value.get("prompt_tokens"));
    let completion = count_of(value.get("completion_tokens"));
    if prompt.is_none() && completion.is_none() {
        return None;
    }
    Some(Usage { input_tokens: prompt.unwrap_or(0), output_tokens: completion.unwrap_or(0) })
}

/// One chunk of the stream. Every field is optional, `null` counts as absent and unknown fields are ignored.
#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Option<Vec<Choice>>,
    #[serde(default)]
    usage: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    index: Option<u64>,
    #[serde(default)]
    delta: Option<Delta>,
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    refusal: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<Fragment>>,
}

/// A piece of a tool call.
#[derive(Deserialize)]
struct Fragment {
    #[serde(default)]
    index: Option<u64>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FragmentFunction>,
}

#[derive(Deserialize)]
struct FragmentFunction {
    #[serde(default)]
    name: Option<String>,
    /// Text in a compatible server, an object in some local ones.
    #[serde(default)]
    arguments: Option<Value>,
}

/// The text of a piece of arguments: a string as it is, an object or any other value as compact JSON, `null` as nothing.
fn argument_text(value: Option<&Value>) -> Cow<'_, str> {
    match value {
        None | Some(Value::Null) => Cow::Borrowed(""),
        Some(Value::String(text)) => Cow::Borrowed(text),
        Some(other) => Cow::Owned(other.to_string()),
    }
}

/// The reason a finish chunk gives, as the stop reason of the turn.
fn stop_reason_of(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "content_filter" => StopReason::Refusal,
        "tool_calls" => StopReason::ToolUse,
        _ => StopReason::Other,
    }
}

fn protocol(text: &str) -> AiError {
    AiError::Protocol(Detail::new(text))
}

/// Which slot a call holds in the assembler, and whether it has an id yet.
struct Slot {
    slot: u32,
    has_id: bool,
}

/// The reader of one answer.
///
/// It is public so that its `Debug` can be tested after a stream; the adapter hands it out as a [`WireTurn`]. `Debug`
/// shows counts and flags, never text.
pub struct ChatTurn {
    usage: Usage,
    stop: Option<StopReason>,
    finish_seen: bool,
    done_seen: bool,
    tools: ToolAssembler,
    slots: Vec<Slot>,
    last_slot: Option<u32>,
    claimed: Vec<String>,
}

impl Default for ChatTurn {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatTurn {
    /// A reader for the start of an answer.
    pub fn new() -> Self {
        Self {
            usage: Usage::default(),
            stop: None,
            finish_seen: false,
            done_seen: false,
            tools: ToolAssembler::new(),
            slots: Vec::new(),
            last_slot: None,
            claimed: Vec::new(),
        }
    }

    /// The slot of a fragment. The server's `index` when it sends one. Without one, a fragment with an id opens the next
    /// free slot and one without an id continues the last slot (the first slot when there is none).
    fn slot_of(&self, fragment: &Fragment, has_id: bool) -> Result<u32, AiError> {
        if let Some(index) = fragment.index {
            return u32::try_from(index).map_err(|_| AiError::from(ToolFault::BadField));
        }
        Ok(match (has_id, self.last_slot) {
            (true, _) => self.slots.iter().map(|held| held.slot).max().map_or(0, |highest| highest.saturating_add(1)),
            (false, Some(last)) => last,
            (false, None) => 0,
        })
    }

    /// Hands one fragment to the assembler. The id goes in only with the first fragment of a slot that carries one, so a
    /// later fragment can neither repeat nor change it; a name goes in every time, so a changed name is a conflict.
    fn fragment(&mut self, fragment: &Fragment) -> Result<(), AiError> {
        let id = fragment.id.as_deref().filter(|id| !id.is_empty());
        let slot = self.slot_of(fragment, id.is_some())?;
        let known = self.slots.iter().position(|held| held.slot == slot);
        let has_id = known.is_some_and(|at| self.slots[at].has_id);
        let given = if has_id { None } else { id.map(|id| claim_id(&mut self.claimed, id)) };
        let name = fragment.function.as_ref().and_then(|function| function.name.as_deref());
        let arguments = argument_text(fragment.function.as_ref().and_then(|function| function.arguments.as_ref()));
        self.tools.fragment(slot, given.as_deref(), name, &arguments).map_err(AiError::from)?;
        match known {
            Some(at) => self.slots[at].has_id |= given.is_some(),
            None => self.slots.push(Slot { slot, has_id: given.is_some() }),
        }
        self.last_slot = Some(slot);
        Ok(())
    }

    /// Reports the text of a delta and collects its tool fragments.
    fn read_delta(&mut self, delta: &Delta, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
        let reasoning =
            [&delta.reasoning_content, &delta.reasoning].into_iter().flatten().find(|text| !text.is_empty());
        for (text, reasoning) in [(reasoning, true), (delta.content.as_ref(), false), (delta.refusal.as_ref(), false)] {
            let Some(text) = text.filter(|text| !text.is_empty()) else { continue };
            let event = if reasoning {
                StreamEvent::ReasoningDelta(text.clone())
            } else {
                StreamEvent::TextDelta(text.clone())
            };
            if out(event) == Flow::Stop {
                return Ok(Flow::Stop);
            }
        }
        for fragment in delta.tool_calls.iter().flatten() {
            self.fragment(fragment)?;
        }
        Ok(Flow::Continue)
    }
}

impl fmt::Debug for ChatTurn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatTurn")
            .field("usage", &self.usage)
            .field("stop", &self.stop)
            .field("finish_seen", &self.finish_seen)
            .field("done_seen", &self.done_seen)
            .field("tools", &self.tools)
            .field("slots", &self.slots.len())
            .finish()
    }
}

impl WireTurn for ChatTurn {
    fn frame(&mut self, frame: Frame<'_>, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
        let Frame::Event(event) = frame else {
            return Err(protocol("the answer was framed in a way this dialect does not use"));
        };
        if self.done_seen {
            return Ok(Flow::Continue);
        }
        if event.is_done() {
            self.done_seen = true;
            return Ok(Flow::Continue);
        }
        let data = event.data().trim();
        if event.event() == "error" {
            // An event named `error` carries the error object itself or wraps it in an `error` member.
            let value: Value = serde_json::from_str(data).unwrap_or(Value::Null);
            return Err(stream_error(value.get("error").unwrap_or(&value)));
        }
        if data.is_empty() {
            return Ok(Flow::Continue);
        }
        let chunk: Chunk = serde_json::from_str(data).map_err(|_| protocol("a stream event was not valid JSON"))?;
        if let Some(error) = chunk.error.as_ref().filter(|error| says_something(error)) {
            return Err(stream_error(error));
        }
        for choice in chunk.choices.iter().flatten() {
            if let Some(error) = choice.error.as_ref().filter(|error| says_something(error)) {
                return Err(stream_error(error));
            }
            if choice.index.is_some_and(|index| index != 0) {
                continue;
            }
            if let Some(delta) = &choice.delta
                && self.read_delta(delta, out)? == Flow::Stop
            {
                return Ok(Flow::Stop);
            }
            if let Some(reason) = choice.finish_reason.as_deref().filter(|reason| !reason.is_empty()) {
                if reason == "error" {
                    return Err(stream_error(&Value::Null));
                }
                self.stop = Some(stop_reason_of(reason));
                self.finish_seen = true;
                // Only a finish that says the model is done completes the calls it started; a cut by the length
                // limit or a filter leaves them open, and the end of the turn discards them.
                if matches!(reason, "tool_calls" | "stop") {
                    self.tools.complete_all().map_err(AiError::from)?;
                }
            }
        }
        if let Some(usage) = chunk.usage.as_ref().and_then(usage_of) {
            self.usage = usage;
            if out(StreamEvent::Usage(usage)) == Flow::Stop {
                return Ok(Flow::Stop);
            }
        }
        Ok(Flow::Continue)
    }

    fn finish(
        self: Box<Self>,
        end: StreamEnd,
        out: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<Completion, AiError> {
        let turn = *self;
        if end == StreamEnd::Stopped {
            return Err(AiError::Cancelled);
        }
        if !turn.finish_seen && !turn.done_seen {
            return Err(protocol("the stream ended before the model finished"));
        }
        // The calls leave only here, after a clean end; any other end, an open call or a fault is an error.
        let calls = turn.tools.finish(end, true).map_err(AiError::from)?;
        let mut stop = turn.stop.unwrap_or(StopReason::Other);
        if stop == StopReason::ToolUse && calls.is_empty() {
            return Err(protocol("the server announced a tool call and sent none"));
        }
        if stop == StopReason::EndTurn && !calls.is_empty() {
            stop = StopReason::ToolUse;
        }
        for call in unique_ids(calls) {
            if out(StreamEvent::ToolCall(call)) == Flow::Stop {
                return Err(AiError::Cancelled);
            }
        }
        Ok(Completion { stop_reason: stop, usage: turn.usage, native: None })
    }
}
