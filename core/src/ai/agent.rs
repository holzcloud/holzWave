//! The agent loop: one turn of a conversation, as rounds of a provider request, tool calls, approval, execution and
//! result.
//!
//! [`Agent::run_turn`] drives a [`Provider`] and a [`ToolHost`] through the rounds of one turn. It owns the order in
//! which the safety pieces meet: the approval table that decides whether a call may run, the redactor that cleans what
//! a tool returned, and the conversation shapes the providers need. It reads no file, opens no socket, starts no
//! process and touches no terminal; the shell supplies the host, the service supplies the thread.
//!
//! What the loop guarantees:
//!
//! - **The loop cannot approve.** It reads the decision the approval table returns and nothing else. The one decision
//!   it makes itself is a denial, when nobody answered a card within [`Limits::approval_timeout`]. The host is reached
//!   only through [`ApprovalTable::run`], which needs the hash of the card and issues the ticket.
//! - **Calls run one at a time.** A step with several calls is handled call by call (propose, wait, run, next), so each
//!   card is built after the output of the earlier calls of the step was seen, and carries the taint it caused.
//! - **A call from a stream that did not end cleanly is dropped.** A provider error, a cancel, a reply over the size
//!   bound and a malformed batch keep only the text of the round. The loop never retries a round.
//! - **Every call is answered exactly once.** Whatever ends a turn, each tool call in the transcript has one result in
//!   the tool message that follows its assistant message, and no result stands without its call. Calls that did not run
//!   get a fixed text ([`STOPPED_RESULT`], [`TIMED_OUT_RESULT`]).
//! - **Every tool result passes one door.** An error text, a fixed text and a tool's output are all cleaned by
//!   [`redact::clean_text`] (escape and control characters removed, secrets redacted, envelope markers neutralised,
//!   bounded) before the model or the page sees them.
//! - **Everything is bounded.** Rounds, tokens, tool output, reply size, calls per step and request size each have a
//!   limit in [`Limits`] that ends the turn with a stable [`Limit`] name.
//! - **The loop blocks.** [`Agent::run_turn`] belongs on a turn thread, never on a thread that serves page commands.
//!   The only thread the loop starts is the short-lived one that denies an unanswered card.
//! - **Stopping works in every state.** A turn ends within a quarter of a second of the stop, and the approval table
//!   holds nothing of it afterwards (no open call, no taint ledger), also when the sink panics.
//!
//! # Stopping a turn
//!
//! The user's stop sets the [`CancelToken`]. A pane that closes or a conversation that is deleted calls
//! [`ApprovalTable::cancel_pane`] or [`ApprovalTable::cancel_conversation`], which reaches a waiting turn the same way.
//! In every row the calls that did not run are answered with [`STOPPED_RESULT`] through the cleaning door, every card
//! gets its `ToolResult` event so the page can close it, and the tool message of the step is saved with status
//! `Cancelled`.
//!
//! | State | Mechanism | Result |
//! | --- | --- | --- |
//! | before the first round, at every round start | the token is read | no request; the turn ends `Cancelled` |
//! | while the provider streams | the provider returns `Cancelled` | text kept as `Cancelled`; calls dropped |
//! | while a card waits | `wait` returns `Cancelled` (token, closed pane, deleted chat) | stopped text; `Cancelled` |
//! | while a tool runs | the table's `run` returns `Cancelled` | stopped text; the turn ends `Cancelled` |
//! | between two calls of a step | the token is read before each call | the rest gets the stopped text, no card |
//! | after the last result | the next round start reads the token | the turn ends `Cancelled` |
//! | nobody answers a card | denied after [`Limits::approval_timeout`] | [`TIMED_OUT_RESULT`]; the turn goes on |

mod history;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::gate::ApprovedContext;
use super::provider::{AiError, CancelToken, ChatRequest, Collector, Completion, Detail, Flow, Provider, StreamEvent};
use super::redact::{self, Keep};
use super::tools::{
    ApprovalTable, Decision, MAX_RESULT_BYTES, Pending, Scope, TaintSource, ToolError, ToolHost, ToolName,
    denied_result, specs,
};
pub use history::{Repair, repair_dangling};
use history::{fit, fixed_bytes, message_bytes, strip_stored};

use super::types::{Capabilities, Message, MessageStatus, Part, Role, Size, StopReason, ToolCall, ToolResult, Usage};

/// The most provider requests one turn makes.
pub const MAX_ROUNDS: u32 = 20;
/// The most tokens one turn may spend, as the providers report them (an estimate where they do not).
pub const MAX_TURN_TOKENS: u64 = 600_000;
/// The most bytes of tool output one turn may collect.
pub const MAX_TURN_RESULT_BYTES: usize = 262_144;
/// The most bytes of reply one request may stream: 1 MiB.
pub const MAX_REPLY_BYTES: usize = 1_048_576;
/// The most tool calls one step runs; later calls of the step are answered without a card.
pub const MAX_CALLS_PER_ROUND: usize = 8;
/// The number of identical calls (same tool, same arguments) in one turn that stops the turn; the last one does not
/// run.
pub const REPEAT_LIMIT: u32 = 3;
/// The number of consecutive failed calls that stops the turn.
pub const ERROR_STREAK_LIMIT: u32 = 3;
/// The most bytes one request may hold: 2 MiB.
pub const MAX_REQUEST_BYTES: usize = 2_097_152;
/// The answer length asked for when the model does not state its own limit.
pub const DEFAULT_OUTPUT_TOKENS: u32 = 8192;
/// How long a card may wait for a decision before the loop denies it: ten minutes.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);
/// The estimate of the bytes in one token, for budgets and for providers that report no usage.
pub const BYTES_PER_TOKEN: usize = 3;

/// The result of a call that did not run because the turn was stopped.
pub const STOPPED_RESULT: &str = "This call did not run: the turn was stopped.";
/// The result of a call whose card nobody answered in time.
pub const TIMED_OUT_RESULT: &str = "No decision was made in time, so this call did not run.";
/// The result a stored call gets when the application closed before it finished.
pub const INTERRUPTED_RESULT: &str = "This call did not run: the app closed before it finished.";
/// What an old tool result becomes when the history is trimmed to fit the model's window.
pub const TRIMMED_RESULT: &str = "[tool output trimmed to fit the context window]";

/// The estimate of the tokens in `bytes` bytes of text: one token per [`BYTES_PER_TOKEN`] bytes, rounded up.
pub fn estimate_tokens(bytes: usize) -> u64 {
    bytes.div_ceil(BYTES_PER_TOKEN) as u64
}

/// The limits of one turn. The defaults are the constants of this module; a value below its floor is raised to it.
#[derive(Clone, Debug)]
pub struct Limits {
    /// The most provider requests (floor 1).
    pub max_rounds: u32,
    /// The most tokens a turn may spend.
    pub max_turn_tokens: u64,
    /// The most bytes of tool output a turn may collect.
    pub max_result_bytes: usize,
    /// The most bytes of reply one request may stream (floor 1024).
    pub max_reply_bytes: usize,
    /// The most calls one step runs (floor 1).
    pub max_calls_per_round: usize,
    /// The count of identical calls that stops the turn (floor 2).
    pub repeat_limit: u32,
    /// The count of consecutive failed calls that stops the turn (floor 1).
    pub error_streak_limit: u32,
    /// The most bytes one request may hold (floor 4096).
    pub max_request_bytes: usize,
    /// The answer length asked for, never above what the model allows (floor 1).
    pub max_output_tokens: u32,
    /// How long a card may wait for a decision before the loop denies it.
    pub approval_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_rounds: MAX_ROUNDS,
            max_turn_tokens: MAX_TURN_TOKENS,
            max_result_bytes: MAX_TURN_RESULT_BYTES,
            max_reply_bytes: MAX_REPLY_BYTES,
            max_calls_per_round: MAX_CALLS_PER_ROUND,
            repeat_limit: REPEAT_LIMIT,
            error_streak_limit: ERROR_STREAK_LIMIT,
            max_request_bytes: MAX_REQUEST_BYTES,
            max_output_tokens: DEFAULT_OUTPUT_TOKENS,
            approval_timeout: APPROVAL_TIMEOUT,
        }
    }
}

impl Limits {
    /// The limits with every value raised to its floor.
    fn normalised(mut self) -> Self {
        self.max_rounds = self.max_rounds.max(1);
        self.repeat_limit = self.repeat_limit.max(2);
        self.error_streak_limit = self.error_streak_limit.max(1);
        self.max_calls_per_round = self.max_calls_per_round.max(1);
        self.max_request_bytes = self.max_request_bytes.max(4096);
        self.max_reply_bytes = self.max_reply_bytes.max(1024);
        self.max_output_tokens = self.max_output_tokens.max(1);
        self
    }
}

/// Which limit ended a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    /// The turn made as many provider requests as it may.
    Steps,
    /// The turn spent its tokens.
    Tokens,
    /// The tools returned as much as a turn may collect.
    ToolOutput,
    /// A reply grew past its bound.
    ReplySize,
    /// The model asked for the same call again and again.
    RepeatedCall,
    /// Calls failed one after another.
    ToolErrors,
}

impl Limit {
    /// The stable name.
    pub fn as_str(self) -> &'static str {
        match self {
            Limit::Steps => "steps",
            Limit::Tokens => "tokens",
            Limit::ToolOutput => "tool_output",
            Limit::ReplySize => "reply_size",
            Limit::RepeatedCall => "repeated_call",
            Limit::ToolErrors => "tool_errors",
        }
    }
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnEnd {
    /// The model finished its answer.
    Done(StopReason),
    /// The user stopped the turn, or a pane or a conversation it depended on went away.
    Cancelled,
    /// A limit ended the turn.
    Limit(Limit),
    /// An error ended the turn.
    Failed(AiError),
}

impl TurnEnd {
    /// The stable code: `done`, `cancelled`, `failed`, or `limit_` and the name of the limit.
    pub fn code(&self) -> &'static str {
        match self {
            TurnEnd::Done(_) => "done",
            TurnEnd::Cancelled => "cancelled",
            TurnEnd::Failed(_) => "failed",
            TurnEnd::Limit(Limit::Steps) => "limit_steps",
            TurnEnd::Limit(Limit::Tokens) => "limit_tokens",
            TurnEnd::Limit(Limit::ToolOutput) => "limit_tool_output",
            TurnEnd::Limit(Limit::ReplySize) => "limit_reply_size",
            TurnEnd::Limit(Limit::RepeatedCall) => "limit_repeated_call",
            TurnEnd::Limit(Limit::ToolErrors) => "limit_tool_errors",
        }
    }
}

/// Everything one turn needs. `Debug` prints sizes only: the request holds the user's text.
#[derive(Clone)]
pub struct TurnRequest {
    /// Where the turn's calls happen: the conversation, the turn (unique in the conversation) and the pane.
    pub scope: Scope,
    /// The model to ask.
    pub model: String,
    /// The system prompt.
    pub system: String,
    /// The whole transcript so far, with the user message of this turn last.
    pub messages: Vec<Message>,
    /// The context approved for this turn; each is named by exactly one note in `messages`.
    pub context: Vec<ApprovedContext>,
    /// Makes the ids of the messages the loop creates (`<prefix>-1`, `<prefix>-2`, ...): 1 to 64 bytes, no control
    /// character, unique in the conversation.
    pub id_prefix: String,
}

impl fmt::Debug for TurnRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnRequest")
            .field("scope", &self.scope)
            .field("model", &self.model)
            .field("system", &Size(self.system.len()))
            .field("messages", &self.messages.len())
            .field("context", &self.context.len())
            .field("id_prefix", &self.id_prefix)
            .finish()
    }
}

/// Something that happened in a turn, in the order it happened.
///
/// Per round: `RoundStarted`, then `TextDelta` and `ReasoningDelta` as they stream, then `Usage`, then `Message` (the
/// assistant message); for each tool call in order `ToolProposed` (only when a card was built) and `ToolResult`; then
/// `Message` (the tool message with all results of the step); then the next `RoundStarted` or the end of the turn.
/// `Trimmed` precedes the `RoundStarted` of the round it applies to. A tool call never appears as an event of its own:
/// it travels in the assistant message and in its card.
#[derive(Clone)]
pub enum AgentEvent {
    /// A provider request begins. Rounds count from 1.
    RoundStarted {
        /// The round.
        round: u32,
    },
    /// A piece of the answer.
    TextDelta(String),
    /// A piece of reasoning, for display only.
    ReasoningDelta(String),
    /// The history was trimmed to fit the request: old tool results replaced, old exchanges removed.
    Trimmed {
        /// How many old tool results were replaced.
        results: u32,
        /// How many old exchanges were removed.
        exchanges: u32,
    },
    /// The token totals of this turn so far.
    Usage(Usage),
    /// A finished message, to show and to persist.
    Message(Message),
    /// A card: a call that waits for the user's decision, exactly as the approval table built it.
    ToolProposed(Pending),
    /// A call has its result.
    ToolResult {
        /// The id of the call.
        call_id: String,
        /// The name of the tool as the model wrote it, scrubbed.
        tool: String,
        /// What entered the transcript.
        content: String,
        /// Whether the call failed or did not run.
        is_error: bool,
        /// How many secrets, escape sequences and similar were removed from the content.
        redactions: u32,
    },
}

impl fmt::Debug for AgentEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentEvent::RoundStarted { round } => f.debug_struct("RoundStarted").field("round", round).finish(),
            AgentEvent::TextDelta(text) => f.debug_tuple("TextDelta").field(&Size(text.len())).finish(),
            AgentEvent::ReasoningDelta(text) => f.debug_tuple("ReasoningDelta").field(&Size(text.len())).finish(),
            AgentEvent::Trimmed { results, exchanges } => {
                f.debug_struct("Trimmed").field("results", results).field("exchanges", exchanges).finish()
            }
            AgentEvent::Usage(usage) => f.debug_tuple("Usage").field(usage).finish(),
            AgentEvent::Message(message) => f.debug_tuple("Message").field(message).finish(),
            AgentEvent::ToolProposed(pending) => f.debug_tuple("ToolProposed").field(pending).finish(),
            AgentEvent::ToolResult { call_id, tool, content, is_error, redactions } => f
                .debug_struct("ToolResult")
                .field("call_id", call_id)
                .field("tool", tool)
                .field("content", &Size(content.len()))
                .field("is_error", is_error)
                .field("redactions", redactions)
                .finish(),
        }
    }
}

/// What a turn did. `messages` are the messages the turn created, in order, equal one for one to the `Message` events;
/// the history passed in is never changed.
pub struct TurnOutcome {
    /// How the turn ended.
    pub end: TurnEnd,
    /// How many provider requests it made.
    pub rounds: u32,
    /// The token totals, as the providers reported them.
    pub usage: Usage,
    /// The messages it created.
    pub messages: Vec<Message>,
}

impl fmt::Debug for TurnOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnOutcome")
            .field("end", &self.end)
            .field("rounds", &self.rounds)
            .field("usage", &self.usage)
            .field("messages", &self.messages.len())
            .field("bytes", &Size(self.messages.iter().map(|m| m.text().len()).sum()))
            .finish()
    }
}

/// The agent loop over a provider, a tool host and an approval table.
///
/// An agent is cheap: the service builds one per turn.
pub struct Agent<'a> {
    provider: &'a dyn Provider,
    host: &'a dyn ToolHost,
    table: &'a ApprovalTable,
    limits: Limits,
    clock: fn() -> i64,
}

/// Unix milliseconds: the default clock for `Message.ts`.
fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

impl<'a> Agent<'a> {
    /// An agent with the default limits and the system clock.
    pub fn new(provider: &'a dyn Provider, host: &'a dyn ToolHost, table: &'a ApprovalTable) -> Agent<'a> {
        Agent { provider, host, table, limits: Limits::default(), clock: unix_millis }
    }

    /// The same agent with other limits.
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// The same agent with another clock for `Message.ts`. The default is Unix milliseconds; pass the unit the
    /// conversation store's callers use.
    #[must_use]
    pub fn with_clock(mut self, clock: fn() -> i64) -> Self {
        self.clock = clock;
        self
    }

    /// Runs one turn and blocks until it ends.
    ///
    /// Run it on a turn thread, never on a thread that serves page commands. Every ending is a [`TurnEnd`], never a
    /// panic or a bare error. `sink` is called outside every lock and may call [`ApprovalTable::decide`], which never
    /// blocks. The loop starts no thread except the one that denies an unanswered card.
    pub fn run_turn(&self, turn: TurnRequest, sink: &mut dyn FnMut(AgentEvent), cancel: &CancelToken) -> TurnOutcome {
        if let Some(why) = invalid_request(&turn) {
            return TurnOutcome {
                end: TurnEnd::Failed(AiError::InvalidRequest(Detail::new(why))),
                rounds: 0,
                usage: Usage::default(),
                messages: Vec::new(),
            };
        }
        // Dropped when this function ends, however it ends: the table keeps nothing of this turn.
        let _guard = TurnGuard { table: self.table, scope: turn.scope.clone() };
        // A stored transcript with unanswered calls would make every provider refuse the request. The repair is a no-op
        // after the service's own call, and what it adds is not a message of this turn.
        let mut messages = turn.messages;
        repair_dangling(&mut messages);
        let mut run = Run {
            provider: self.provider,
            host: self.host,
            table: self.table,
            clock: self.clock,
            limits: self.limits.clone().normalised(),
            scope: turn.scope,
            model: turn.model,
            system: turn.system,
            messages,
            context: turn.context,
            id_prefix: turn.id_prefix,
            sink,
            cancel,
            produced: Vec::new(),
            counter: 0,
            rounds: 0,
            usage: Usage::default(),
            stop: None,
            repeats: HashMap::new(),
            streak: 0,
            result_bytes: 0,
            turn_start: 0,
            spent: 0,
            request_size: 0,
            announced: (0, 0),
            taint_recorded: false,
        };
        run.turn_start = run.messages.len() - 1;
        let end = run.drive();
        TurnOutcome { end, rounds: run.rounds, usage: run.usage, messages: run.produced }
    }
}

/// Why a turn request cannot start, as a fixed phrase.
fn invalid_request(turn: &TurnRequest) -> Option<&'static str> {
    if turn.messages.is_empty() {
        return Some("a turn needs at least one message");
    }
    if turn.messages.last().is_some_and(|message| message.role != Role::User) {
        return Some("a turn must end with a message from the user");
    }
    if turn.id_prefix.is_empty() || turn.id_prefix.len() > 64 {
        return Some("the id prefix must be 1 to 64 bytes");
    }
    if turn.id_prefix.chars().any(char::is_control) {
        return Some("the id prefix must not contain a control character");
    }
    None
}

/// Leaves the approval table clean when the turn ends, also when the sink panics.
struct TurnGuard<'a> {
    table: &'a ApprovalTable,
    scope: Scope,
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        self.table.cancel_turn(&self.scope);
        self.table.finish_turn(&self.scope);
    }
}

/// Why the rest of a step does not run, and the turn ends after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    Cancelled,
    Limit(Limit),
}

impl Stop {
    fn end(self) -> TurnEnd {
        match self {
            Stop::Cancelled => TurnEnd::Cancelled,
            Stop::Limit(limit) => TurnEnd::Limit(limit),
        }
    }
}

/// What a round decided.
enum Next {
    Go,
    End(TurnEnd),
}

/// How a call went, for the failure streak.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    /// The tool ran and returned.
    Succeeded,
    /// The call was refused or failed.
    Failed,
    /// A denial, a timeout or a stop: neither a failure nor a success.
    Neutral,
}

/// The answer to one call, before it passes the cleaning door.
struct Answer {
    content: String,
    is_error: bool,
    class: Class,
    /// The turn was stopped while this call was open.
    stopped: bool,
}

impl Answer {
    fn stopped() -> Self {
        Self { content: STOPPED_RESULT.to_owned(), is_error: true, class: Class::Neutral, stopped: true }
    }

    /// A call that did not run because a limit ended the turn.
    fn stopped_by_limit() -> Self {
        Self { content: STOPPED_RESULT.to_owned(), is_error: true, class: Class::Neutral, stopped: false }
    }

    fn refused(error: &ToolError) -> Self {
        if *error == ToolError::Cancelled {
            return Self::stopped();
        }
        Self { content: error.to_string(), is_error: true, class: Class::Failed, stopped: false }
    }

    fn timed_out() -> Self {
        Self { content: TIMED_OUT_RESULT.to_owned(), is_error: true, class: Class::Neutral, stopped: false }
    }

    fn denied(call_id: &str) -> Self {
        Self { content: denied_result(call_id).content, is_error: true, class: Class::Neutral, stopped: false }
    }

    fn success(text: String) -> Self {
        Self { content: text, is_error: false, class: Class::Succeeded, stopped: false }
    }
}

/// The state of one turn.
struct Run<'r> {
    provider: &'r dyn Provider,
    host: &'r dyn ToolHost,
    table: &'r ApprovalTable,
    clock: fn() -> i64,
    limits: Limits,
    scope: Scope,
    model: String,
    system: String,
    /// The working copy of the transcript: the history passed in, then what this turn produced.
    messages: Vec<Message>,
    context: Vec<ApprovedContext>,
    id_prefix: String,
    sink: &'r mut dyn FnMut(AgentEvent),
    cancel: &'r CancelToken,
    produced: Vec<Message>,
    counter: u32,
    rounds: u32,
    usage: Usage,
    stop: Option<Stop>,
    /// How often each call (tool and canonical arguments) came up in this turn.
    repeats: HashMap<(String, [u8; 32]), u32>,
    /// Consecutive failed calls.
    streak: u32,
    /// Bytes of cleaned results so far.
    result_bytes: usize,
    /// The index of the user message this turn answers in the working copy.
    turn_start: usize,
    /// Tokens spent so far, as reported or estimated.
    spent: u64,
    /// The bytes of the request that was sent last, for the estimate of a round that reports no usage.
    request_size: usize,
    /// The counts of the last `Trimmed` event, so an event is sent only when they change.
    announced: (u32, u32),
    /// Whether what the turn already holds (context, earlier tool output) has been told to the approval table.
    taint_recorded: bool,
}

impl Run<'_> {
    fn emit(&mut self, event: AgentEvent) {
        (self.sink)(event);
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn next_id(&mut self) -> String {
        self.counter += 1;
        format!("{}-{}", self.id_prefix, self.counter)
    }

    /// Adds a message to the working copy and the produced list, and tells the sink.
    fn record(&mut self, message: Message) {
        self.messages.push(message.clone());
        self.produced.push(message.clone());
        self.emit(AgentEvent::Message(message));
    }

    fn add_usage(&mut self, usage: Usage) {
        self.usage.input_tokens = self.usage.input_tokens.saturating_add(usage.input_tokens);
        self.usage.output_tokens = self.usage.output_tokens.saturating_add(usage.output_tokens);
    }

    /// Records why the rest of the step does not run. A stop by the user outranks a limit.
    fn set_stop(&mut self, stop: Stop) {
        if self.stop.is_none() || stop == Stop::Cancelled {
            self.stop = Some(stop);
        }
    }

    /// The answer length to ask for: ours, and never above what the model allows.
    fn output_limit(&self, capabilities: &Capabilities) -> u32 {
        let ours = self.limits.max_output_tokens;
        capabilities.max_output_tokens.map_or(ours, |own| own.min(ours))
    }

    /// The most bytes a request may hold: ours, and what the model's window leaves after the answer it may write.
    fn request_budget(&self, capabilities: &Capabilities, output_limit: u32) -> usize {
        let ours = self.limits.max_request_bytes;
        capabilities.context_tokens.map_or(ours, |window| {
            let room = (window as usize).saturating_sub(output_limit as usize);
            ours.min(room.saturating_mul(BYTES_PER_TOKEN))
        })
    }

    /// Tells the approval table about the untrusted content the turn holds before its first card: approved context, a
    /// context note in the history, and the output of every tool the history shows a result of.
    fn record_taint(&self) -> Result<(), ToolError> {
        let mut sources: Vec<TaintSource> = Vec::new();
        let mut add = |source: TaintSource| {
            if !sources.contains(&source) {
                sources.push(source);
            }
        };
        if self.context.iter().any(ApprovedContext::is_untrusted) {
            add(TaintSource::Context);
        }
        let mut asked: HashMap<&str, &str> = HashMap::new();
        for part in self.messages.iter().flat_map(|message| message.parts.iter()) {
            match part {
                Part::Context(_) => add(TaintSource::Context),
                Part::ToolCall(call) => {
                    asked.insert(call.id.as_str(), call.name.as_str());
                }
                Part::ToolResult(result) => {
                    let tool = asked.get(result.call_id.as_str()).and_then(|name| ToolName::from_name(name));
                    if let Some(tool) = tool {
                        add(TaintSource::ToolOutput(tool));
                    }
                }
                Part::Text { .. } => {}
            }
        }
        sources.into_iter().try_for_each(|source| self.table.taint(&self.scope, source))
    }

    fn drive(&mut self) -> TurnEnd {
        loop {
            match self.round() {
                Next::Go => {}
                Next::End(end) => return end,
            }
        }
    }

    /// One provider request and, when the model asked for tools, the step that answers it.
    fn round(&mut self) -> Next {
        if self.cancel.is_cancelled() {
            return Next::End(TurnEnd::Cancelled);
        }
        if self.rounds >= self.limits.max_rounds {
            return Next::End(TurnEnd::Limit(Limit::Steps));
        }
        if self.spent >= self.limits.max_turn_tokens {
            return Next::End(TurnEnd::Limit(Limit::Tokens));
        }
        let provider = self.provider;
        let capabilities = provider.capabilities(&self.model);
        let tools = if capabilities.tools { specs(self.host, self.scope.pane) } else { Vec::new() };
        let declared: HashSet<String> = tools.iter().map(|tool| tool.name.clone()).collect();

        // What the model already read steers what it asks for next, so the cards of this turn must show it. A turn that
        // declares no tools has no cards.
        if !declared.is_empty() && !self.taint_recorded {
            if self.record_taint().is_err() {
                let why = "the turn could not record what it has read, so it was not started";
                return Next::End(TurnEnd::Failed(AiError::Policy(Detail::new(why))));
            }
            self.taint_recorded = true;
        }

        let output_limit = self.output_limit(&capabilities);
        let mut request = ChatRequest::new(self.model.clone());
        request.system = self.system.clone();
        request.messages = self.messages.clone();
        request.tools = tools;
        request.context = self.context.clone();
        request.max_output_tokens = Some(output_limit);

        // The request is trimmed on its own copy; the working copy and the history passed in stay whole.
        strip_stored(&mut request.messages);
        let fixed = fixed_bytes(&request.system, &request.tools, &request.context);
        let pinned: HashSet<String> = request.context.iter().map(ApprovedContext::hash_hex).collect();
        let budget = self.request_budget(&capabilities, output_limit);
        let Ok(trim) = fit(&mut request.messages, self.turn_start, &pinned, budget, fixed) else {
            return Next::End(TurnEnd::Failed(AiError::ContextLength));
        };
        self.request_size = fixed.saturating_add(request.messages.iter().map(message_bytes).sum::<usize>());
        if let Err(error) = request.check_consent(provider.id()) {
            return Next::End(TurnEnd::Failed(error));
        }
        if (trim.results, trim.exchanges) != self.announced {
            self.announced = (trim.results, trim.exchanges);
            self.emit(AgentEvent::Trimmed { results: trim.results, exchanges: trim.exchanges });
        }

        self.rounds += 1;
        self.emit(AgentEvent::RoundStarted { round: self.rounds });

        let mut collector = Collector::with_limit(self.limits.max_reply_bytes);
        let result = {
            let sink = &mut *self.sink;
            let collected = &mut collector;
            provider.stream(
                &request,
                &mut |event| {
                    // The collector owns the text; the sink gets its own copy of a delta.
                    let delta = match &event {
                        StreamEvent::TextDelta(text) => Some(AgentEvent::TextDelta(text.clone())),
                        StreamEvent::ReasoningDelta(text) => Some(AgentEvent::ReasoningDelta(text.clone())),
                        StreamEvent::ToolCall(_) | StreamEvent::Usage(_) => None,
                    };
                    let flow = collected.push(event);
                    if flow == Flow::Continue
                        && let Some(delta) = delta
                    {
                        sink(delta);
                    }
                    flow
                },
                self.cancel,
            )
        };

        // A reply over its bound ends the turn, whatever the provider made of the stop it was given.
        if collector.truncated() {
            return self.fail_stream(collector, TurnEnd::Limit(Limit::ReplySize), MessageStatus::Failed);
        }
        match result {
            Ok(completion) => self.finish_round(collector, completion, &declared),
            Err(AiError::Cancelled) => self.fail_stream(collector, TurnEnd::Cancelled, MessageStatus::Cancelled),
            Err(error) => self.fail_stream(collector, TurnEnd::Failed(error), MessageStatus::Failed),
        }
    }

    /// A stream that did not end cleanly: every collected call is dropped and only the text stays.
    fn fail_stream(&mut self, collector: Collector, end: TurnEnd, status: MessageStatus) -> Next {
        let partial = collector.usage();
        if partial != Usage::default() {
            self.add_usage(partial);
            self.emit(AgentEvent::Usage(self.usage));
        }
        if !collector.text().is_empty() {
            let (id, ts) = (self.next_id(), self.now());
            let mut message = collector.into_message(id, ts, self.model.clone(), status, None);
            message.parts.retain(|part| !matches!(part, Part::ToolCall(_)));
            self.record(message);
        }
        Next::End(end)
    }

    /// Adds the tokens of a round to the budget: what the provider reported, else an estimate of the bytes that went
    /// out and came back.
    fn spend(&mut self, reported: Usage, reply: &Message) {
        let counted = u64::from(reported.input_tokens).saturating_add(u64::from(reported.output_tokens));
        let tokens = if counted > 0 {
            counted
        } else {
            estimate_tokens(self.request_size.saturating_add(message_bytes(reply)))
        };
        self.spent = self.spent.saturating_add(tokens);
    }

    /// A stream that ended cleanly: the assistant message, then the step that answers its calls.
    fn finish_round(&mut self, collector: Collector, completion: Completion, declared: &HashSet<String>) -> Next {
        let reported = if completion.usage == Usage::default() { collector.usage() } else { completion.usage };
        self.add_usage(reported);
        self.emit(AgentEvent::Usage(self.usage));
        let calls: Vec<ToolCall> = collector.tool_calls().to_vec();
        let stop_reason = completion.stop_reason;
        let (id, ts) = (self.next_id(), self.now());
        if let Some(why) = malformed_batch(&calls) {
            // A batch the loop cannot answer call by call is not run at all: the text stays, the calls go.
            let mut message = collector.into_message(id, ts, self.model.clone(), MessageStatus::Failed, None);
            message.parts.retain(|part| !matches!(part, Part::ToolCall(_)));
            message.usage = (reported != Usage::default()).then_some(reported);
            self.record(message);
            return Next::End(TurnEnd::Failed(AiError::Protocol(Detail::new(why))));
        }
        let message = collector.into_message(id, ts, self.model.clone(), MessageStatus::Complete, Some(completion));
        self.spend(reported, &message);
        self.record(message);
        if calls.is_empty() {
            return Next::End(TurnEnd::Done(stop_reason));
        }
        self.run_step(&calls, declared)
    }

    /// Answers the calls of one step, in order, and records the tool message.
    fn run_step(&mut self, calls: &[ToolCall], declared: &HashSet<String>) -> Next {
        let results = self.run_calls(calls, declared);
        if self.stop.is_some() && self.cancel.is_cancelled() {
            self.set_stop(Stop::Cancelled);
        }
        let parts = results.into_iter().map(Part::ToolResult).collect();
        let (id, ts) = (self.next_id(), self.now());
        let mut message = Message::new(id, Role::Tool, parts, ts);
        if self.stop == Some(Stop::Cancelled) {
            message.status = MessageStatus::Cancelled;
        }
        self.record(message);
        match self.stop {
            Some(stop) => Next::End(stop.end()),
            None => Next::Go,
        }
    }

    /// Handles the calls one at a time. Every call gets an answer, whatever happens to the ones before it.
    fn run_calls(&mut self, calls: &[ToolCall], declared: &HashSet<String>) -> Vec<ToolResult> {
        let mut results = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            let answer = if self.stop.is_some() {
                Answer::stopped()
            } else if self.cancel.is_cancelled() {
                self.set_stop(Stop::Cancelled);
                Answer::stopped()
            } else {
                self.screen_and_run(index, call, declared)
            };
            if answer.stopped {
                self.set_stop(Stop::Cancelled);
            }
            match answer.class {
                Class::Succeeded => self.streak = 0,
                Class::Failed => {
                    self.streak += 1;
                    if self.streak >= self.limits.error_streak_limit {
                        self.set_stop(Stop::Limit(Limit::ToolErrors));
                    }
                }
                Class::Neutral => {}
            }
            results.push(self.settle(call, &answer));
        }
        results
    }

    /// The checks that need no card, then the call itself.
    fn screen_and_run(&mut self, index: usize, call: &ToolCall, declared: &HashSet<String>) -> Answer {
        if index >= self.limits.max_calls_per_round {
            return Answer::refused(&ToolError::TooMany);
        }
        // A name the catalog does not know goes to the table, which says so; a known tool that this round did not
        // declare is refused here.
        if ToolName::from_name(&call.name).is_some() && !declared.contains(&call.name) {
            return Answer::refused(&ToolError::Unavailable);
        }
        let seen = self.repeats.entry((call.name.clone(), digest_of(&call.args))).or_insert(0);
        *seen += 1;
        if *seen >= self.limits.repeat_limit {
            self.set_stop(Stop::Limit(Limit::RepeatedCall));
            return Answer::stopped_by_limit();
        }
        if self.result_bytes >= self.limits.max_result_bytes {
            self.set_stop(Stop::Limit(Limit::ToolOutput));
            return Answer::stopped_by_limit();
        }
        self.run_call(call)
    }

    /// Proposes the call, waits for the user's decision and runs it through the table.
    fn run_call(&mut self, call: &ToolCall) -> Answer {
        let pending = match self.table.propose(self.host, &self.scope, call) {
            Ok(pending) => pending,
            Err(error) => return Answer::refused(&error),
        };
        let hash = pending.hash.clone();
        self.emit(AgentEvent::ToolProposed(pending));
        let (decision, timed_out) = self.wait_for_decision(&call.id, &hash);
        match decision {
            Err(error) => Answer::refused(&error),
            Ok(Decision::Deny) if timed_out => Answer::timed_out(),
            Ok(Decision::Deny) => Answer::denied(&call.id),
            Ok(_) => match self.table.run(self.host, &self.scope, &call.id, self.cancel) {
                // A tool that ran and did not succeed (a command with a status other than 0) is an error result, not a failure.
                Ok(output) => Answer { is_error: output.is_failure(), ..Answer::success(output.into_text()) },
                Err(error) => Answer::refused(&error),
            },
        }
    }

    /// Waits for the user's decision on a card. A short-lived thread denies the card when nobody answered within the
    /// approval timeout, so a forgotten card cannot park the turn thread for good. It is the only decision the loop
    /// makes: it can deny, and it has no way to approve. The thread ends when the wait does, and the scope joins it
    /// before this function returns; it needs no lock, no sleep and no polling.
    fn wait_for_decision(&self, call_id: &str, hash: &str) -> (Result<Decision, ToolError>, bool) {
        let table = self.table;
        let conversation = self.scope.conversation.as_str();
        let timeout = self.limits.approval_timeout;
        let timed_out = AtomicBool::new(false);
        let flag = &timed_out;
        let decision = thread::scope(|scope| {
            let (done, deadline) = mpsc::channel::<()>();
            // Without the thread (the system refused to start one) the wait still ends with the user or the token.
            let deadline_thread = thread::Builder::new().name("agent-approval-deadline".to_owned());
            let _deadline = deadline_thread.spawn_scoped(scope, move || {
                let lapsed = matches!(deadline.recv_timeout(timeout), Err(RecvTimeoutError::Timeout));
                if lapsed && table.decide(conversation, call_id, Decision::Deny, hash).is_ok() {
                    flag.store(true, Ordering::SeqCst);
                }
            });
            let decision = table.wait(conversation, call_id, self.cancel);
            drop(done);
            decision
        });
        (decision, timed_out.load(Ordering::SeqCst))
    }

    /// Passes the answer through the cleaning door, counts it, and tells the sink.
    fn settle(&mut self, call: &ToolCall, answer: &Answer) -> ToolResult {
        let (result, redactions) = finish_result(&call.id, &answer.content, answer.is_error);
        self.result_bytes = self.result_bytes.saturating_add(result.content.len());
        self.emit(AgentEvent::ToolResult {
            call_id: result.call_id.clone(),
            tool: Detail::new(&call.name).as_str().to_owned(),
            content: result.content.clone(),
            is_error: result.is_error,
            redactions,
        });
        result
    }
}

/// Why a batch of calls cannot be answered call by call: an empty id, or one id used twice.
fn malformed_batch(calls: &[ToolCall]) -> Option<&'static str> {
    if calls.iter().any(|call| call.id.is_empty()) {
        return Some("a tool call came without an id");
    }
    let mut seen: HashSet<&str> = HashSet::with_capacity(calls.len());
    if calls.iter().any(|call| !seen.insert(call.id.as_str())) {
        return Some("two tool calls of one step share an id");
    }
    None
}

/// The one door a tool string enters the transcript through: cleaned, redacted, neutralised and bounded.
///
/// Returns the result and the number of removals that are not the cut itself.
fn finish_result(call_id: &str, content: &str, is_error: bool) -> (ToolResult, u32) {
    let cleaned = redact::clean_text(content, Keep::Head, MAX_RESULT_BYTES);
    let redactions =
        cleaned.hits.iter().filter(|hit| hit.rule != "truncated").fold(0u32, |sum, hit| sum.saturating_add(hit.count));
    (ToolResult { call_id: call_id.to_owned(), content: cleaned.text, is_error }, redactions)
}

/// The SHA-256 of the canonical text of `args`: objects with their keys sorted, arrays in order, so the order in which
/// a model wrote the keys never matters.
fn digest_of(args: &Value) -> [u8; 32] {
    let mut text = String::new();
    canonical(args, &mut text);
    Sha256::digest(text.as_bytes()).into()
}

fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}
