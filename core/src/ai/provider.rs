//! The seam between the AI chat and a model: the [`Provider`] trait, the events it pushes, the token that stops it, and
//! the errors it returns.
//!
//! The trait is blocking and push-style on purpose. One signature serves the scripted test double, the wire adapters
//! (which drive a private runtime) and the Apple on-device model (a C interface), and stopping a turn stays a token the
//! caller owns instead of a property of an async trait object.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::watch;

use super::types::{
    Capabilities, Message, MessageStatus, Opaque, Part, Role, Size, StopReason, ToolCall, ToolSpec, Usage, json_size,
};

/// What the consumer of a stream answers after each event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Go on.
    Continue,
    /// End the turn now.
    Stop,
}

/// One thing a provider reports while it streams an answer.
#[derive(Clone, PartialEq)]
pub enum StreamEvent {
    /// A piece of the answer text.
    TextDelta(String),
    /// A piece of reasoning text. It is for display only and is not kept.
    ReasoningDelta(String),
    /// A tool call, delivered once, complete and parsed.
    ToolCall(ToolCall),
    /// Token totals so far. The last one wins.
    Usage(Usage),
}

impl fmt::Debug for StreamEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamEvent::TextDelta(text) => f.debug_tuple("TextDelta").field(&Size(text.len())).finish(),
            StreamEvent::ReasoningDelta(text) => f.debug_tuple("ReasoningDelta").field(&Size(text.len())).finish(),
            StreamEvent::ToolCall(call) => f.debug_tuple("ToolCall").field(call).finish(),
            StreamEvent::Usage(usage) => f.debug_tuple("Usage").field(usage).finish(),
        }
    }
}

/// How a turn ended, when it ended cleanly.
#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    /// Why the model stopped.
    pub stop_reason: StopReason,
    /// The final token totals; zero where the provider did not report.
    pub usage: Usage,
    /// The assistant turn exactly as the provider sent it.
    pub native: Option<Opaque>,
}

/// Everything a provider needs for one turn.
///
/// `Debug` prints sizes and names only: a request holds the user's text and the system prompt, and must never reach a
/// log line.
#[derive(Clone)]
pub struct ChatRequest {
    /// The model to ask.
    pub model: String,
    /// The system prompt.
    pub system: String,
    /// The conversation so far.
    pub messages: Vec<Message>,
    /// The tools the model may call.
    pub tools: Vec<ToolSpec>,
    /// The longest answer to ask for, in tokens.
    pub max_output_tokens: Option<u32>,
}

impl ChatRequest {
    /// An empty request for `model`.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_output_tokens: None,
        }
    }
}

impl fmt::Debug for ChatRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tools: Vec<&str> = self.tools.iter().map(|tool| tool.name.as_str()).collect();
        f.debug_struct("ChatRequest")
            .field("model", &self.model)
            .field("system", &Size(self.system.len()))
            .field("messages", &self.messages)
            .field("tools", &tools)
            .field("max_output_tokens", &self.max_output_tokens)
            .finish()
    }
}

/// A model behind one interface.
///
/// Contract of [`Provider::stream`]:
///
/// - It blocks the calling thread until the turn ends. Never call it from an async command thread; run it on a worker.
/// - Events reach `out` in order. A tool call is delivered once, complete.
/// - `cancel` is checked at least once per event. As soon as it is set, and also when `out` answers [`Flow::Stop`], the
///   call returns `Err(AiError::Cancelled)` and delivers nothing more.
/// - A stream that ends uncleanly is an error, never `Ok`.
/// - `Completion::native` is the assistant turn verbatim.
/// - The request is never logged or formatted.
pub trait Provider: Send + Sync {
    /// A stable id for the provider, the one a context is approved for.
    fn id(&self) -> &str;

    /// What `model` can do.
    fn capabilities(&self, model: &str) -> Capabilities;

    /// Runs one turn.
    fn stream(
        &self,
        req: &ChatRequest,
        out: &mut dyn FnMut(StreamEvent) -> Flow,
        cancel: &CancelToken,
    ) -> Result<Completion, AiError>;
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct TokenState {
    flag: AtomicBool,
    /// Pairs with `wake` for the blocking wait. It guards no data.
    gate: Mutex<()>,
    wake: Condvar,
    /// Carries the flag to async waiters.
    watch: watch::Sender<bool>,
}

/// Stops a turn. Clones share one flag; the user's Stop sets it and every step of a turn looks at it.
#[derive(Clone)]
pub struct CancelToken {
    state: Arc<TokenState>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    /// A token that is not cancelled.
    pub fn new() -> Self {
        Self {
            state: Arc::new(TokenState {
                flag: AtomicBool::new(false),
                gate: Mutex::new(()),
                wake: Condvar::new(),
                watch: watch::channel(false).0,
            }),
        }
    }

    /// Cancels the token and wakes every waiter.
    pub fn cancel(&self) {
        self.state.flag.store(true, Ordering::SeqCst);
        self.state.watch.send_replace(true);
        // Taking the lock before notifying means a waiter between its check and its sleep cannot miss the wake-up.
        let _gate = lock(&self.state.gate);
        self.state.wake.notify_all();
    }

    /// Whether the token is cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.state.flag.load(Ordering::SeqCst)
    }

    /// Blocks up to `timeout` and returns true as soon as the token is cancelled, false when the time ran out.
    pub fn wait_cancelled(&self, timeout: Duration) -> bool {
        let gate = lock(&self.state.gate);
        let (gate, _) = self
            .state
            .wake
            .wait_timeout_while(gate, timeout, |_| !self.is_cancelled())
            .unwrap_or_else(PoisonError::into_inner);
        drop(gate);
        self.is_cancelled()
    }

    /// Completes when the token is cancelled, at once if it already is.
    pub async fn cancelled(&self) {
        let mut receiver = self.state.watch.subscribe();
        // The sender lives in `self`, so the wait ends in a value and never in a closed channel.
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}

impl fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancelToken").field("cancelled", &self.is_cancelled()).finish()
    }
}

/// The most text and reasoning a [`Collector`] keeps: 4 MiB.
pub const MAX_COLLECTED: usize = 4 * 1024 * 1024;

/// Folds a stream of events into the parts of an assistant message, within a size bound.
///
/// The bound counts text, reasoning and tool arguments. The first event that would pass it is not kept, and the
/// collector answers [`Flow::Stop`] from then on, so a provider that never stops cannot fill the memory.
pub struct Collector {
    text: String,
    reasoning: String,
    calls: Vec<ToolCall>,
    usage: Usage,
    used: usize,
    limit: usize,
    truncated: bool,
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector {
    /// A collector bounded by [`MAX_COLLECTED`].
    pub fn new() -> Self {
        Self::with_limit(MAX_COLLECTED)
    }

    /// A collector bounded by `bytes`.
    pub fn with_limit(bytes: usize) -> Self {
        Self {
            text: String::new(),
            reasoning: String::new(),
            calls: Vec::new(),
            usage: Usage::default(),
            used: 0,
            limit: bytes,
            truncated: false,
        }
    }

    /// Takes one event. Answer with the result from the stream callback.
    pub fn push(&mut self, event: StreamEvent) -> Flow {
        if self.truncated {
            return Flow::Stop;
        }
        let size = match &event {
            StreamEvent::TextDelta(text) | StreamEvent::ReasoningDelta(text) => text.len(),
            StreamEvent::ToolCall(call) => call.id.len() + call.name.len() + json_size(&call.args).0,
            StreamEvent::Usage(_) => 0,
        };
        if self.used.saturating_add(size) > self.limit {
            self.truncated = true;
            return Flow::Stop;
        }
        self.used += size;
        match event {
            StreamEvent::TextDelta(text) => self.text.push_str(&text),
            StreamEvent::ReasoningDelta(text) => self.reasoning.push_str(&text),
            StreamEvent::ToolCall(call) => self.calls.push(call),
            StreamEvent::Usage(usage) => self.usage = usage,
        }
        Flow::Continue
    }

    /// The answer text so far.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The reasoning text so far.
    pub fn reasoning(&self) -> &str {
        &self.reasoning
    }

    /// The tool calls so far, in order.
    pub fn tool_calls(&self) -> &[ToolCall] {
        &self.calls
    }

    /// The latest token totals.
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// Whether the bound stopped the stream.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Builds the assistant message: the text when there is some, then the tool calls in order. Reasoning is for
    /// display and is dropped. Usage and the native turn come from `completion` when the turn ended cleanly.
    pub fn into_message(
        self,
        id: impl Into<String>,
        ts: i64,
        model: impl Into<String>,
        status: MessageStatus,
        completion: Option<Completion>,
    ) -> Message {
        let mut parts = Vec::with_capacity(1 + self.calls.len());
        if !self.text.is_empty() {
            parts.push(Part::Text { text: self.text });
        }
        parts.extend(self.calls.into_iter().map(Part::ToolCall));
        let mut message = Message::new(id, Role::Assistant, parts, ts);
        message.model = Some(model.into());
        message.status = status;
        let (usage, native) = match completion {
            Some(completion) => (completion.usage, completion.native),
            None => (Usage::default(), None),
        };
        let reported = if usage == Usage::default() { self.usage } else { usage };
        message.usage = (reported != Usage::default()).then_some(reported);
        message.native = native;
        message
    }
}

impl fmt::Debug for Collector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Collector")
            .field("text", &Size(self.text.len()))
            .field("reasoning", &Size(self.reasoning.len()))
            .field("tool_calls", &self.calls.len())
            .field("usage", &self.usage)
            .field("limit", &Size(self.limit))
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// The longest free text an [`AiError`] carries, in characters.
pub const MAX_DETAIL: usize = 240;

/// Free text for an error, made safe to show.
///
/// It is the only door for free text into [`AiError`]. Provider error text is untrusted: it may carry an escape
/// sequence that rewrites a terminal, a bidirectional override that reorders what the page shows, or a very long body.
/// [`Detail::new`] replaces control, bidirectional, zero-width and tag characters with a space, collapses white space,
/// and cuts the result at [`MAX_DETAIL`] characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail(String);

impl Detail {
    /// Scrubs `text`.
    pub fn new(text: &str) -> Self {
        let cleaned: String = text.chars().map(|c| if is_unsafe(c) { ' ' } else { c }).collect();
        let mut out = String::with_capacity(cleaned.len().min(MAX_DETAIL * 4));
        for word in cleaned.split_whitespace() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(word);
            if out.chars().count() > MAX_DETAIL {
                break;
            }
        }
        if out.chars().count() > MAX_DETAIL {
            out = out.chars().take(MAX_DETAIL).collect::<String>().trim_end().to_owned();
            out.push('\u{2026}');
        }
        Self(out)
    }

    /// The scrubbed text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether there is no text.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Characters that never belong in a message shown to the user.
fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2060}'..='\u{2064}'
                | '\u{FEFF}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

/// Why a turn did not end cleanly.
///
/// Free text can only enter through [`Detail`]. `Display` is built from fixed phrases plus that text or a status code,
/// and never carries a URL, a header or a body of ours.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AiError {
    /// The user stopped the turn.
    Cancelled,
    /// The provider refused the credentials.
    Auth(Detail),
    /// Too many requests.
    RateLimited {
        /// How long the provider asked to wait.
        retry_after: Option<Duration>,
    },
    /// The provider is overloaded.
    Overloaded,
    /// The conversation does not fit the model.
    ContextLength,
    /// The provider's content filter stopped the answer.
    ContentFilter,
    /// The provider rejected the request as malformed.
    InvalidRequest(Detail),
    /// The provider or the model cannot be used right now.
    Unavailable(Detail),
    /// The connection failed.
    Network(Detail),
    /// The provider went quiet.
    Timeout,
    /// The provider answered in a way the adapter cannot read.
    Protocol(Detail),
    /// The provider reported an error of its own.
    Server {
        /// The status code.
        status: u16,
        /// What it said, scrubbed.
        detail: Detail,
    },
    /// A rule of this application refused the request.
    Policy(Detail),
}

impl AiError {
    fn phrase(&self) -> (&'static str, Option<&Detail>) {
        match self {
            AiError::Cancelled => ("the request was cancelled", None),
            AiError::Auth(d) => ("the provider refused the credentials", Some(d)),
            AiError::RateLimited { .. } => ("the provider's rate limit was reached", None),
            AiError::Overloaded => ("the provider is overloaded", None),
            AiError::ContextLength => ("the conversation is longer than the model accepts", None),
            AiError::ContentFilter => ("the provider's content filter stopped the answer", None),
            AiError::InvalidRequest(d) => ("the provider rejected the request", Some(d)),
            AiError::Unavailable(d) => ("the provider is not available", Some(d)),
            AiError::Network(d) => ("the connection failed", Some(d)),
            AiError::Timeout => ("the provider did not answer in time", None),
            AiError::Protocol(d) => ("the provider answered in an unexpected way", Some(d)),
            AiError::Server { detail, .. } => ("the provider reported an error", Some(detail)),
            AiError::Policy(d) => ("the request was refused by a rule of the application", Some(d)),
        }
    }
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (phrase, detail) = self.phrase();
        f.write_str(phrase)?;
        match self {
            AiError::Server { status, .. } => write!(f, " (status {status})")?,
            AiError::RateLimited { retry_after: Some(wait) } => write!(f, " (retry after {} s)", wait.as_secs())?,
            _ => {}
        }
        match detail {
            Some(detail) if !detail.is_empty() => write!(f, ": {}", detail.as_str()),
            _ => Ok(()),
        }
    }
}

impl std::error::Error for AiError {}
