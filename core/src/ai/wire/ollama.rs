//! The native chat dialect of Ollama: one JSON record per line.
//!
//! The native route is chosen over the compatible one for two reasons. The context window can be sent with each
//! request, so the server never falls back to its small default and drops the start of a long prompt without a word;
//! and the final record reports how many prompt tokens were read, which the compatible route does not.
//!
//! - **The request is a pure function** of the [`ChatRequest`], the [`Capabilities`] and the window the adapter was
//!   built with. The body holds the model, the messages, the stream flag, the options (always the window, and an output
//!   bound only when the request sets one) and, only when the model can use tools and the request declares some, the
//!   function definitions. Nothing else is written: no forced tool selection, no sampling parameter, no thinking switch,
//!   no keep-alive, no output format and no header.
//! - **Tool calls arrive whole and carry arguments as an object.** Most servers of this dialect send no call id, so the
//!   assembler makes one up; an id that is sent is kept, and a repeated one is made unique inside the turn. Calls are
//!   handed out from the end of the turn, after the record that says the model is done and a clean close.
//! - **Thinking models think by default.** Their thinking is for display only: it is never sent back.
//! - **A prompt that fills the window may have been cut.** The server does not say so; the prompt counter of the final
//!   record is the only sign. A counter at or above the window is reported as a conversation that is too long.
//! - **Terminal context, merging and errors are treated exactly as in the compatible dialect.** The planning of
//!   messages, the declaration of tools, the classification of errors and the ids come from that file.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::openai_chat::{
    Plain, ToolDecl, claim_id, declared_tools, plain_messages, rejected, saturate, stream_error, unique_ids,
};
use super::{Endpoint, Wire, WireAdapter, WireRequest, WireTurn};
use crate::ai::http::{Frame, Rejection};
use crate::ai::provider::{AiError, ChatRequest, Completion, Detail, Flow, StreamEvent};
use crate::ai::sse::{StreamEnd, ToolAssembler};
use crate::ai::types::{Capabilities, Locality, StopReason, Usage};

/// The path of the chat endpoint below the base address of the profile.
pub const CHAT_PATH: &str = "/api/chat";

/// The context window asked for when the profile names none: 16384 tokens.
pub const DEFAULT_NUM_CTX: u32 = 16_384;
/// The smallest window that can be asked for: 2048 tokens.
pub const MIN_NUM_CTX: u32 = 2_048;
/// The largest window that can be asked for: 1048576 tokens.
pub const MAX_NUM_CTX: u32 = 1_048_576;

/// The adapter of the native Ollama chat dialect. Its one setting is the context window it asks for.
#[derive(Clone, Copy, Debug)]
pub struct OllamaAdapter {
    num_ctx: u32,
}

impl Default for OllamaAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl OllamaAdapter {
    /// An adapter that asks for [`DEFAULT_NUM_CTX`] tokens.
    pub fn new() -> Self {
        Self { num_ctx: DEFAULT_NUM_CTX }
    }

    /// An adapter that asks for `num_ctx` tokens, kept between [`MIN_NUM_CTX`] and [`MAX_NUM_CTX`].
    #[must_use]
    pub fn with_num_ctx(self, num_ctx: u32) -> Self {
        Self { num_ctx: num_ctx.clamp(MIN_NUM_CTX, MAX_NUM_CTX) }
    }

    /// The window the adapter asks for, in tokens.
    pub fn num_ctx(&self) -> u32 {
        self.num_ctx
    }
}

// ---- the request body -------------------------------------------------------------------------------------

/// A call the model made, as it is written back: the arguments are an object, and there is no id.
#[derive(Serialize)]
struct WireCall<'a> {
    function: WireCallFunction<'a>,
}

#[derive(Serialize)]
struct WireCallFunction<'a> {
    name: &'a str,
    arguments: &'a Value,
}

/// One message as it is written. The content is always a string.
#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<WireCall<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_name: Option<&'a str>,
}

impl<'a> WireMessage<'a> {
    fn text(role: &'static str, content: &'a str) -> Self {
        Self { role, content, tool_calls: Vec::new(), tool_name: None }
    }
}

/// The options of a request: the window always, the output bound when the request sets one.
#[derive(Serialize)]
struct Options {
    num_ctx: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
}

/// The request body. Every optional key is left out when it is not set; no key outside this list is ever written.
#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    stream: bool,
    options: Options,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ToolDecl<'a>>,
}

impl WireAdapter for OllamaAdapter {
    fn wire(&self) -> Wire {
        Wire::Ollama
    }

    /// Tools are declared to every model, and the window is the one that is asked for, so the agent loop budgets
    /// against the number the server is given.
    fn capabilities(&self, _model: &str, locality: Locality) -> Capabilities {
        Capabilities { tools: true, context_tokens: Some(self.num_ctx), ..Capabilities::conservative(locality) }
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
                    content: text,
                    tool_calls: calls
                        .iter()
                        .map(|call| WireCall { function: WireCallFunction { name: &call.name, arguments: &call.args } })
                        .collect(),
                    tool_name: None,
                },
                Plain::Result { content, name, .. } => {
                    WireMessage { role: "tool", content, tool_calls: Vec::new(), tool_name: name.as_deref() }
                }
            });
        }
        let body = Body {
            model: &req.model,
            messages,
            stream: true,
            options: Options { num_ctx: self.num_ctx, num_predict: req.max_output_tokens },
            tools: if caps.tools { declared_tools(&req.tools) } else { Vec::new() },
        };
        let body = serde_json::to_vec(&body)
            .map_err(|_| AiError::InvalidRequest(Detail::new("the request could not be written as JSON")))?;
        Ok(WireRequest { path: CHAT_PATH.to_owned(), headers: Vec::new(), body })
    }

    fn turn(&self) -> Box<dyn WireTurn> {
        Box::new(OllamaTurn::new(self.num_ctx))
    }

    fn rejection(&self, rejection: &Rejection, endpoint: &Endpoint) -> Option<AiError> {
        rejected(rejection, endpoint)
    }
}

// ---- the answer -------------------------------------------------------------------------------------------

/// One record of the stream. Every field is optional and unknown fields are ignored.
#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    done: Option<bool>,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    prompt_eval_count: Option<Value>,
    #[serde(default)]
    eval_count: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<Call>>,
}

/// A whole tool call. Newer servers send an id; older ones do not.
#[derive(Deserialize)]
struct Call {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<CallFunction>,
}

#[derive(Deserialize)]
struct CallFunction {
    #[serde(default)]
    name: Option<String>,
    /// An object in this dialect; anything else is refused when the call is taken.
    #[serde(default)]
    arguments: Option<Value>,
}

/// A counter of the final record: an unsigned number, or a non-negative one written with a fraction. A missing or
/// unreadable counter is zero.
fn counter(value: Option<&Value>) -> u64 {
    let Some(number) = value.and_then(Value::as_number) else { return 0 };
    number
        .as_u64()
        .or_else(|| number.as_f64().filter(|real| real.is_finite() && *real >= 0.0).map(|real| real as u64))
        .unwrap_or(0)
}

/// The reason the final record gives, as the stop reason of the turn. No reason is a normal end.
fn stop_reason_of(reason: Option<&str>) -> StopReason {
    match reason {
        None | Some("" | "stop") => StopReason::EndTurn,
        Some("length") => StopReason::MaxTokens,
        Some(_) => StopReason::Other,
    }
}

fn protocol(text: &str) -> AiError {
    AiError::Protocol(Detail::new(text))
}

/// The reader of one answer.
///
/// It is public so that its `Debug` can be tested after a stream; the adapter hands it out as a [`WireTurn`]. `Debug`
/// shows counts and flags, never text.
pub struct OllamaTurn {
    window: u32,
    usage: Usage,
    prompt_tokens: u64,
    stop: StopReason,
    done_seen: bool,
    tools: ToolAssembler,
    claimed: Vec<String>,
}

impl OllamaTurn {
    /// A reader for the start of an answer to a request that asked for a window of `num_ctx` tokens.
    pub fn new(num_ctx: u32) -> Self {
        Self {
            window: num_ctx,
            usage: Usage::default(),
            prompt_tokens: 0,
            stop: StopReason::EndTurn,
            done_seen: false,
            tools: ToolAssembler::new(),
            claimed: Vec::new(),
        }
    }

    /// Hands one whole call to the assembler. An id that the server sent is kept, and made unique when an earlier call
    /// of the turn holds it; without one the assembler makes one up.
    fn take_call(&mut self, call: Call) -> Result<(), AiError> {
        let id = call.id.filter(|id| !id.is_empty()).map(|id| claim_id(&mut self.claimed, &id));
        let (name, arguments) = match call.function {
            Some(function) => (function.name.unwrap_or_default(), function.arguments),
            None => (String::new(), None),
        };
        let arguments = match arguments {
            None | Some(Value::Null) => Value::Object(serde_json::Map::new()),
            Some(other) => other,
        };
        self.tools.whole(id.as_deref(), &name, arguments).map_err(AiError::from)
    }
}

impl fmt::Debug for OllamaTurn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OllamaTurn")
            .field("window", &self.window)
            .field("usage", &self.usage)
            .field("prompt_tokens", &self.prompt_tokens)
            .field("stop", &self.stop)
            .field("done_seen", &self.done_seen)
            .field("tools", &self.tools)
            .field("claimed", &self.claimed.len())
            .finish()
    }
}

impl WireTurn for OllamaTurn {
    fn frame(&mut self, frame: Frame<'_>, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
        let Frame::Line(line) = frame else {
            return Err(protocol("the answer was framed in a way this dialect does not use"));
        };
        if self.done_seen {
            return Ok(Flow::Continue);
        }
        let Record { message, done, done_reason, prompt_eval_count, eval_count, error } =
            serde_json::from_slice(line).map_err(|_| protocol("a stream line was not valid JSON"))?;
        if let Some(error) = error.filter(|error| !error.is_null()) {
            return Err(stream_error(&error));
        }
        if let Some(Message { content, thinking, tool_calls }) = message {
            for (text, is_thinking) in [(thinking, true), (content, false)] {
                let Some(text) = text.filter(|text| !text.is_empty()) else { continue };
                let event = if is_thinking { StreamEvent::ReasoningDelta(text) } else { StreamEvent::TextDelta(text) };
                if out(event) == Flow::Stop {
                    return Ok(Flow::Stop);
                }
            }
            for call in tool_calls.into_iter().flatten() {
                self.take_call(call)?;
            }
        }
        if done == Some(true) {
            self.done_seen = true;
            self.stop = stop_reason_of(done_reason.as_deref());
            self.prompt_tokens = counter(prompt_eval_count.as_ref());
            self.usage = Usage {
                input_tokens: saturate(self.prompt_tokens),
                output_tokens: saturate(counter(eval_count.as_ref())),
            };
            if (prompt_eval_count.is_some() || eval_count.is_some())
                && out(StreamEvent::Usage(self.usage)) == Flow::Stop
            {
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
        if !turn.done_seen {
            return Err(protocol("the stream ended before the model finished"));
        }
        // The server may have cut the prompt to fit its window without saying so; a prompt that fills the window is the
        // only sign.
        if turn.prompt_tokens >= u64::from(turn.window) {
            return Err(AiError::ContextLength);
        }
        // The calls leave only here, after a clean end; any other end or a fault is an error.
        let calls = turn.tools.finish(end, true).map_err(AiError::from)?;
        let mut stop = turn.stop;
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
