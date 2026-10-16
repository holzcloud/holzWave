//! A provider that plays a script: the test double every AI test drives.
//!
//! It implements [`Provider`] like a wire adapter does, so the agent loop, the service and the shell's headless tests
//! are written against the trait and run without a network. It follows the same contract: it checks the token before
//! every step, honours [`Flow::Stop`], and returns an error for a turn that cannot be served.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::Value;

use super::provider::{AiError, CancelToken, ChatRequest, Completion, Detail, Flow, Provider, StreamEvent};
use super::types::{Capabilities, Locality, Opaque, StopReason, ToolCall, Usage};

/// One step of a scripted turn.
#[derive(Clone, PartialEq)]
pub enum Step {
    /// Deliver a text delta.
    Text(String),
    /// Deliver a reasoning delta.
    Reasoning(String),
    /// Deliver a complete tool call.
    Tool(ToolCall),
    /// Deliver token totals.
    Usage(Usage),
    /// Go quiet for this long, or until the token is cancelled.
    Stall(Duration),
    /// End the turn with this error.
    Fail(AiError),
}

impl fmt::Debug for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Step::Text(text) => write!(f, "Text(<{} bytes>)", text.len()),
            Step::Reasoning(text) => write!(f, "Reasoning(<{} bytes>)", text.len()),
            Step::Tool(call) => f.debug_tuple("Tool").field(call).finish(),
            Step::Usage(usage) => f.debug_tuple("Usage").field(usage).finish(),
            Step::Stall(wait) => f.debug_tuple("Stall").field(wait).finish(),
            Step::Fail(error) => f.debug_tuple("Fail").field(error).finish(),
        }
    }
}

/// The steps of one turn and how it ends. Built with chained calls.
#[derive(Clone, PartialEq)]
pub struct Script {
    steps: Vec<Step>,
    stop_reason: StopReason,
    usage: Usage,
    native: Option<Opaque>,
}

impl Default for Script {
    fn default() -> Self {
        Self::new()
    }
}

impl Script {
    /// An empty script that ends with [`StopReason::EndTurn`].
    pub fn new() -> Self {
        Self { steps: Vec::new(), stop_reason: StopReason::EndTurn, usage: Usage::default(), native: None }
    }

    /// A script with one text step.
    pub fn reply(text: impl Into<String>) -> Self {
        Self::new().text(text)
    }

    /// Adds a text delta.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.steps.push(Step::Text(text.into()));
        self
    }

    /// Adds a reasoning delta.
    pub fn reasoning(mut self, text: impl Into<String>) -> Self {
        self.steps.push(Step::Reasoning(text.into()));
        self
    }

    /// Adds a complete tool call.
    pub fn tool_call(mut self, id: impl Into<String>, name: impl Into<String>, args: Value) -> Self {
        self.steps.push(Step::Tool(ToolCall { id: id.into(), name: name.into(), args }));
        self
    }

    /// Adds a usage report; the last one is also the usage of the completion.
    pub fn usage(mut self, input_tokens: u32, output_tokens: u32) -> Self {
        let usage = Usage { input_tokens, output_tokens };
        self.usage = usage;
        self.steps.push(Step::Usage(usage));
        self
    }

    /// Adds a pause that a cancel ends early.
    pub fn stall(mut self, wait: Duration) -> Self {
        self.steps.push(Step::Stall(wait));
        self
    }

    /// Ends the turn with `error`.
    pub fn fail(mut self, error: AiError) -> Self {
        self.steps.push(Step::Fail(error));
        self
    }

    /// Sets the stop reason of the completion.
    pub fn stop(mut self, reason: StopReason) -> Self {
        self.stop_reason = reason;
        self
    }

    /// Sets the provider-native turn of the completion.
    pub fn native(mut self, native: Opaque) -> Self {
        self.native = Some(native);
        self
    }
}

impl fmt::Debug for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Script")
            .field("steps", &self.steps)
            .field("stop_reason", &self.stop_reason)
            .field("usage", &self.usage)
            .field("native", &self.native)
            .finish()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A [`Provider`] that serves one queued [`Script`] per call and records every request it served.
pub struct ScriptedProvider {
    id: String,
    capabilities: Capabilities,
    turns: Mutex<VecDeque<Script>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedProvider {
    /// A provider with this id, tools on, no reasoning or vision, and loopback locality.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            capabilities: Capabilities {
                tools: true,
                reasoning: false,
                vision: false,
                context_tokens: Some(32768),
                max_output_tokens: Some(4096),
                locality: Locality::Loopback,
            },
            turns: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Replaces the capabilities every model reports.
    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Queues a turn.
    pub fn with_turn(self, script: Script) -> Self {
        lock(&self.turns).push_back(script);
        self
    }

    /// Every request served, in order. A call refused before it started (cancelled, or failing the consent check)
    /// records nothing.
    pub fn requests(&self) -> Vec<ChatRequest> {
        lock(&self.requests).clone()
    }

    /// How many requests were served.
    pub fn calls(&self) -> usize {
        lock(&self.requests).len()
    }

    /// How many turns are still queued.
    pub(crate) fn turns_left(&self) -> usize {
        lock(&self.turns).len()
    }
}

impl fmt::Debug for ScriptedProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptedProvider")
            .field("id", &self.id)
            .field("turns_left", &self.turns_left())
            .field("calls", &self.calls())
            .finish()
    }
}

impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self, _model: &str) -> Capabilities {
        self.capabilities
    }

    fn stream(
        &self,
        req: &ChatRequest,
        out: &mut dyn FnMut(StreamEvent) -> Flow,
        cancel: &CancelToken,
    ) -> Result<Completion, AiError> {
        if cancel.is_cancelled() {
            return Err(AiError::Cancelled);
        }
        req.check_consent(&self.id)?;
        lock(&self.requests).push(req.clone());
        let script = lock(&self.turns)
            .pop_front()
            .ok_or_else(|| AiError::Unavailable(Detail::new("the scripted provider has no turn left")))?;

        for step in script.steps {
            if cancel.is_cancelled() {
                return Err(AiError::Cancelled);
            }
            let event = match step {
                Step::Text(text) => StreamEvent::TextDelta(text),
                Step::Reasoning(text) => StreamEvent::ReasoningDelta(text),
                Step::Tool(call) => StreamEvent::ToolCall(call),
                Step::Usage(usage) => StreamEvent::Usage(usage),
                Step::Stall(wait) => {
                    if cancel.wait_cancelled(wait) {
                        return Err(AiError::Cancelled);
                    }
                    continue;
                }
                Step::Fail(error) => return Err(error),
            };
            if out(event) == Flow::Stop {
                return Err(AiError::Cancelled);
            }
        }
        if cancel.is_cancelled() {
            return Err(AiError::Cancelled);
        }
        Ok(Completion { stop_reason: script.stop_reason, usage: script.usage, native: script.native })
    }
}
