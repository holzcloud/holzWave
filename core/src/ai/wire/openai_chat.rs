//! The Chat Completions dialect, also spoken by many gateways and local servers: LM Studio, llama.cpp, OpenRouter, the
//! Vercel AI Gateway, LiteLLM and every custom server that calls itself compatible.
//!
//! The servers that speak it disagree about ids, finish markers, usage and reasoning fields, so the adapter is strict
//! about what it sends and lenient about what it reads.
//!
//! - **The request is a pure function** of the [`ChatRequest`] and the [`Capabilities`]: the same input gives the same
//!   bytes. The body carries the model, the messages, the two stream flags and, only when the request sets one, an
//!   output bound. Nothing else is written: no sampling parameter, no storage flag, no user or machine identifier, and
//!   no header. A key, an address or a header belongs to the [`Endpoint`](super::Endpoint) and is added by the
//!   chokepoint, never here.
//! - **The answer is read into typed structs.** Unknown fields, `null` and empty deltas are ignored; text is passed on
//!   as it arrives. The turn never prints the stream: its `Debug` shows counts and flags only.
//! - **The end of a stream is evidence, not a guess.** A finish chunk proves that the model finished; the closing marker
//!   is optional. A stream that ends without either, or ends uncleanly, is an error and never `Ok`.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Wire, WireAdapter, WireRequest, WireTurn};
use crate::ai::http::Frame;
use crate::ai::provider::{AiError, ChatRequest, Completion, Detail, Flow, StreamEvent};
use crate::ai::sse::{StreamEnd, ToolAssembler};
use crate::ai::types::{Capabilities, Locality, Part, Role, StopReason, Usage};

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
    /// What the model said.
    Assistant {
        /// The text, empty when the model only asked for tools.
        text: String,
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
fn push_assistant(plan: &mut Vec<Plain>, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some(Plain::Assistant { text: before }) = plan.last_mut() {
        join_blocks(before, &text);
    } else {
        plan.push(Plain::Assistant { text });
    }
}

/// The plan of `req.messages`: user and assistant text in order, adjacent messages of one side merged, empty ones
/// dropped. A request with nothing left is [`AiError::InvalidRequest`].
pub(super) fn plain_messages(req: &ChatRequest) -> Result<Vec<Plain>, AiError> {
    let mut plan: Vec<Plain> = Vec::with_capacity(req.messages.len());
    for message in &req.messages {
        let mut text = String::new();
        for part in &message.parts {
            match part {
                Part::Text { text: block } => join_blocks(&mut text, block),
                // Tool calls, results and context notes are rendered in the next step of the dialect.
                Part::ToolCall(_) | Part::ToolResult(_) | Part::Context(_) => {}
            }
        }
        match message.role {
            Role::User => push_user(&mut plan, text),
            Role::Assistant => push_assistant(&mut plan, text),
            Role::Tool => {}
        }
    }
    if plan.is_empty() {
        return Err(AiError::InvalidRequest(Detail::new("the request has no messages")));
    }
    Ok(plan)
}

// ---- the request body -------------------------------------------------------------------------------------

/// One message as it is written.
#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: Option<&'a str>,
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
}

impl WireAdapter for OpenAiChatAdapter {
    fn wire(&self) -> Wire {
        Wire::OpenAiChat
    }

    fn capabilities(&self, _model: &str, locality: Locality) -> Capabilities {
        Capabilities { tools: true, ..Capabilities::conservative(locality) }
    }

    fn request(&self, req: &ChatRequest, _caps: &Capabilities) -> Result<WireRequest, AiError> {
        let plan = plain_messages(req)?;
        let mut messages = Vec::with_capacity(plan.len() + 1);
        if !req.system.is_empty() {
            messages.push(WireMessage { role: "system", content: Some(&req.system) });
        }
        for entry in &plan {
            messages.push(match entry {
                Plain::User { text } => WireMessage { role: "user", content: Some(text) },
                Plain::Assistant { text } => {
                    WireMessage { role: "assistant", content: (!text.is_empty()).then_some(text.as_str()) }
                }
            });
        }
        let body = Body {
            model: &req.model,
            messages,
            stream: true,
            stream_options: StreamOptions { include_usage: true },
            max_tokens: req.max_output_tokens,
        };
        let body = serde_json::to_vec(&body)
            .map_err(|_| AiError::InvalidRequest(Detail::new("the request could not be written as JSON")))?;
        Ok(WireRequest { path: CHAT_PATH.to_owned(), headers: Vec::new(), body })
    }

    fn turn(&self) -> Box<dyn WireTurn> {
        Box::new(ChatTurn::new())
    }
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
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    index: Option<u64>,
    #[serde(default)]
    delta: Option<Delta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
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

/// The reader of one answer.
struct ChatTurn {
    usage: Usage,
    stop: Option<StopReason>,
    finish_seen: bool,
    done_seen: bool,
    tools: ToolAssembler,
}

impl ChatTurn {
    fn new() -> Self {
        Self { usage: Usage::default(), stop: None, finish_seen: false, done_seen: false, tools: ToolAssembler::new() }
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
        if data.is_empty() {
            return Ok(Flow::Continue);
        }
        let chunk: Chunk = serde_json::from_str(data).map_err(|_| protocol("a stream event was not valid JSON"))?;
        for choice in chunk.choices.iter().flatten() {
            if choice.index.is_some_and(|index| index != 0) {
                continue;
            }
            if let Some(text) =
                choice.delta.as_ref().and_then(|delta| delta.content.as_deref()).filter(|t| !t.is_empty())
                && out(StreamEvent::TextDelta(text.to_owned())) == Flow::Stop
            {
                return Ok(Flow::Stop);
            }
            if let Some(reason) = choice.finish_reason.as_deref().filter(|reason| !reason.is_empty()) {
                self.stop = Some(stop_reason_of(reason));
                self.finish_seen = true;
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
        _out: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<Completion, AiError> {
        let turn = *self;
        if end == StreamEnd::Stopped {
            return Err(AiError::Cancelled);
        }
        if !turn.finish_seen && !turn.done_seen {
            return Err(protocol("the stream ended before the model finished"));
        }
        // A clean end is the only end that releases calls; any other end is an error here.
        turn.tools.finish(end, true).map_err(AiError::from)?;
        Ok(Completion { stop_reason: turn.stop.unwrap_or(StopReason::Other), usage: turn.usage, native: None })
    }
}
