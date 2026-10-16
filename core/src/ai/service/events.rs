//! The page contract of the AI chat: the events a turn streams, the state they build, and the one function that applies
//! them.
//!
//! An event is one JSON object with the keys `conversation`, `turn`, `seq` and `kind`, followed by the fields of its
//! kind. `seq` counts from 1 inside a turn. A consumer folds events into a [`ChatState`] with [`ChatState::apply`]; the
//! service folds its own live state with the same function, under the lock that assigns `seq`, so a snapshot cannot
//! drift from the events. A consumer that sees [`ApplyResult::Gap`] asks for a snapshot and continues from it.
//!
//! What the page receives is a view, never the stored message: [`ChatMessage::from_message`] is the only converter, and
//! it drops the provider-native turn and the stored text of a context. Every type that holds text has a hand-written
//! `Debug` that prints kinds, ids and byte sizes, so a log line or a failed assertion cannot show what a user typed or
//! what a model answered.

use std::fmt;

use serde::Serialize;

use crate::ai::provider::Detail;
use crate::ai::store::Conversation;
use crate::ai::tools::Pending;
use crate::ai::types::{ContextKind, Locality, Message, MessageStatus, Part, Role, Size, Usage};

use super::TITLE_CHARS;

/// The longest tool-call summary, in characters.
pub const SUMMARY_CHARS: usize = 240;
/// The longest tool name shown, in characters.
const NAME_CHARS: usize = 64;

/// Scrubs `text` to one line of at most `max` characters: control, bidirectional and zero-width characters become
/// spaces, white space collapses, and a longer text ends in an ellipsis inside the bound.
fn one_line(text: &str, max: usize) -> String {
    let scrubbed = Detail::new(text);
    let line = scrubbed.as_str();
    if line.chars().count() <= max {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(max.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('\u{2026}');
    cut
}

/// The title of a chat made from its first message: the first line with text, scrubbed, cut at [`TITLE_CHARS`]
/// characters on a character boundary, with an ellipsis after the cut. Blank text gives an empty title.
pub fn title_from(text: &str) -> String {
    for line in text.lines() {
        let scrubbed = Detail::new(line);
        let line = scrubbed.as_str();
        if line.is_empty() {
            continue;
        }
        if line.chars().count() <= TITLE_CHARS {
            return line.to_owned();
        }
        let mut cut: String = line.chars().take(TITLE_CHARS).collect();
        cut.truncate(cut.trim_end().len());
        cut.push('\u{2026}');
        return cut;
    }
    String::new()
}

// ---- the views ----------------------------------------------------------------------------------------------------

/// Where a turn goes: the profile, the model, the host as it is shown, and where the destination runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Route {
    /// The profile id (or the id of a registered provider).
    pub profile: String,
    /// The model.
    pub model: String,
    /// The host as it is shown to the user; `None` for a provider that has no address.
    pub host: Option<String>,
    /// Where the destination runs, as far as it is known when the turn starts.
    pub locality: Locality,
}

/// The token totals of the turn so far are in [`Usage`]; this counts what was trimmed from the history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Trimmed {
    /// How many old tool results were replaced.
    pub results: u32,
    /// How many old exchanges were removed.
    pub exchanges: u32,
}

/// The text of the answer that is being streamed and has no message yet.
#[derive(Clone, Default, PartialEq, Eq, Serialize)]
pub struct Partial {
    /// The answer so far.
    pub text: String,
    /// The reasoning so far, for display only.
    pub reasoning: String,
}

impl fmt::Debug for Partial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Partial")
            .field("text", &Size(self.text.len()))
            .field("reasoning", &Size(self.reasoning.len()))
            .finish()
    }
}

/// Why a turn failed, for the page: the stable class and a scrubbed sentence.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ErrorView {
    /// An `ErrorClass::as_str()` name.
    pub class: String,
    /// The `Display` of the error, or a fixed phrase of the service.
    pub text: String,
}

impl fmt::Debug for ErrorView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ErrorView").field("class", &self.class).field("text", &Size(self.text.len())).finish()
    }
}

/// How a turn ended.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct EndView {
    /// A `TurnEnd::code()`: `done`, `cancelled`, `failed` or a `limit_` name.
    pub end: String,
    /// The error of a failed turn.
    pub error: Option<ErrorView>,
    /// How many provider requests the turn made.
    pub rounds: u32,
    /// The token totals of the turn.
    pub usage: Usage,
    /// Whether every message of the turn reached the chat file.
    pub saved: bool,
}

impl fmt::Debug for EndView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EndView")
            .field("end", &self.end)
            .field("error", &self.error)
            .field("rounds", &self.rounds)
            .field("usage", &self.usage)
            .field("saved", &self.saved)
            .finish()
    }
}

/// One item of a context, described without its text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChatItem {
    /// What the item is.
    pub kind: ContextKind,
    /// The label the user saw in the offer, scrubbed.
    pub label: String,
    /// The size of the text that was approved.
    pub bytes: u64,
}

/// One piece of a message as the page sees it.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatPart {
    /// Plain text.
    Text {
        /// The text.
        text: String,
    },
    /// A call the model made.
    ToolCall {
        /// The id of the call.
        id: String,
        /// The tool, scrubbed.
        name: String,
        /// The arguments as one scrubbed line of at most [`SUMMARY_CHARS`] characters.
        summary: String,
    },
    /// The result of a call.
    ToolResult {
        /// The id of the call this answers.
        call_id: String,
        /// What entered the transcript, or the stored marker.
        content: String,
        /// Whether the call failed or did not run.
        is_error: bool,
    },
    /// The context a message carried: labels and sizes, never the text.
    Context {
        /// The hash of the approved envelope.
        hash: String,
        /// The items.
        items: Vec<ChatItem>,
        /// The provider id the context was approved for.
        provider: String,
        /// Where that provider runs.
        locality: Locality,
    },
}

impl ChatPart {
    fn from_part(part: &Part) -> ChatPart {
        match part {
            Part::Text { text } => ChatPart::Text { text: text.clone() },
            Part::ToolCall(call) => ChatPart::ToolCall {
                id: call.id.clone(),
                name: one_line(&call.name, NAME_CHARS),
                summary: one_line(&call.args.to_string(), SUMMARY_CHARS),
            },
            Part::ToolResult(result) => ChatPart::ToolResult {
                call_id: result.call_id.clone(),
                content: result.content.clone(),
                is_error: result.is_error,
            },
            Part::Context(note) => ChatPart::Context {
                hash: note.hash.clone(),
                items: note
                    .items
                    .iter()
                    .map(|item| ChatItem {
                        kind: item.kind,
                        label: one_line(&item.label, SUMMARY_CHARS),
                        bytes: item.bytes,
                    })
                    .collect(),
                provider: note.consented_to.provider.clone(),
                locality: note.consented_to.locality,
            },
        }
    }
}

impl fmt::Debug for ChatPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChatPart::Text { text } => f.debug_struct("Text").field("text", &Size(text.len())).finish(),
            ChatPart::ToolCall { id, name, summary } => f
                .debug_struct("ToolCall")
                .field("id", id)
                .field("name", name)
                .field("summary", &Size(summary.len()))
                .finish(),
            ChatPart::ToolResult { call_id, content, is_error } => f
                .debug_struct("ToolResult")
                .field("call_id", call_id)
                .field("content", &Size(content.len()))
                .field("is_error", is_error)
                .finish(),
            ChatPart::Context { hash, items, provider, locality } => f
                .debug_struct("Context")
                .field("hash", hash)
                .field("items", &items.len())
                .field("provider", provider)
                .field("locality", locality)
                .finish(),
        }
    }
}

/// A message as the page sees it. It has no provider-native turn and no stored context text.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ChatMessage {
    /// Unique in the conversation.
    pub id: String,
    /// Who wrote it.
    pub role: Role,
    /// When it was written, in the unit of the stored message.
    pub ts: i64,
    /// The model that wrote it.
    pub model: Option<String>,
    /// How it ended.
    pub status: MessageStatus,
    /// What it cost.
    pub usage: Option<Usage>,
    /// What it holds, in order.
    pub parts: Vec<ChatPart>,
}

impl ChatMessage {
    /// The view of a stored message. This is the only converter: the native turn and the stored text of a context stay
    /// behind.
    pub fn from_message(message: &Message) -> ChatMessage {
        ChatMessage {
            id: message.id.clone(),
            role: message.role,
            ts: message.ts,
            model: message.model.clone(),
            status: message.status,
            usage: message.usage,
            parts: message.parts.iter().map(ChatPart::from_part).collect(),
        }
    }

    /// The text parts, joined without a separator.
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|part| match part {
                ChatPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

impl fmt::Debug for ChatMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatMessage")
            .field("id", &self.id)
            .field("role", &self.role)
            .field("ts", &self.ts)
            .field("model", &self.model)
            .field("status", &self.status)
            .field("usage", &self.usage)
            .field("parts", &self.parts)
            .finish()
    }
}

// ---- the events ---------------------------------------------------------------------------------------------------

/// What happened. The tag is the key `kind`; the fields of the kind follow it.
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiEventKind {
    /// The turn began: the user's message and where it goes.
    TurnStarted {
        /// The user's message.
        message: ChatMessage,
        /// Where the turn goes.
        route: Route,
    },
    /// A piece of the answer.
    Delta {
        /// The text.
        text: String,
    },
    /// A piece of reasoning, for display only.
    Reasoning {
        /// The text.
        text: String,
    },
    /// The token totals of the turn so far.
    Usage(Usage),
    /// The history was trimmed to fit the model.
    Trimmed(Trimmed),
    /// A finished message. It replaces the partial text.
    Message {
        /// The message.
        message: ChatMessage,
    },
    /// A call waits for the user's decision.
    ToolProposed {
        /// The card.
        pending: Pending,
    },
    /// A call has its result.
    ToolResult {
        /// The id of the call.
        call_id: String,
        /// The tool, scrubbed.
        tool: String,
        /// What entered the transcript.
        content: String,
        /// Whether the call failed or did not run.
        is_error: bool,
        /// How many secrets and escape sequences were removed from the content.
        redactions: u32,
    },
    /// The turn is over.
    TurnEnded(EndView),
}

impl AiEventKind {
    /// The tag of the kind.
    pub fn name(&self) -> &'static str {
        match self {
            AiEventKind::TurnStarted { .. } => "turn_started",
            AiEventKind::Delta { .. } => "delta",
            AiEventKind::Reasoning { .. } => "reasoning",
            AiEventKind::Usage(_) => "usage",
            AiEventKind::Trimmed(_) => "trimmed",
            AiEventKind::Message { .. } => "message",
            AiEventKind::ToolProposed { .. } => "tool_proposed",
            AiEventKind::ToolResult { .. } => "tool_result",
            AiEventKind::TurnEnded(_) => "turn_ended",
        }
    }
}

impl fmt::Debug for AiEventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AiEventKind::TurnStarted { message, route } => {
                f.debug_struct("TurnStarted").field("message", message).field("route", route).finish()
            }
            AiEventKind::Delta { text } => f.debug_struct("Delta").field("text", &Size(text.len())).finish(),
            AiEventKind::Reasoning { text } => f.debug_struct("Reasoning").field("text", &Size(text.len())).finish(),
            AiEventKind::Usage(usage) => f.debug_tuple("Usage").field(usage).finish(),
            AiEventKind::Trimmed(trimmed) => f.debug_tuple("Trimmed").field(trimmed).finish(),
            AiEventKind::Message { message } => f.debug_struct("Message").field("message", message).finish(),
            AiEventKind::ToolProposed { pending } => f.debug_struct("ToolProposed").field("pending", pending).finish(),
            AiEventKind::ToolResult { call_id, tool, content, is_error, redactions } => f
                .debug_struct("ToolResult")
                .field("call_id", call_id)
                .field("tool", tool)
                .field("content", &Size(content.len()))
                .field("is_error", is_error)
                .field("redactions", redactions)
                .finish(),
            AiEventKind::TurnEnded(end) => f.debug_tuple("TurnEnded").field(end).finish(),
        }
    }
}

/// One event of a turn, as the page receives it: `conversation`, `turn`, `seq`, `kind`, then the fields of the kind.
#[derive(Clone, Serialize)]
pub struct AiEvent {
    /// The chat the event belongs to.
    pub conversation: String,
    /// The turn inside the chat.
    pub turn: u64,
    /// The position inside the turn, from 1.
    pub seq: u64,
    /// What happened.
    #[serde(flatten)]
    pub kind: AiEventKind,
}

impl fmt::Debug for AiEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AiEvent")
            .field("conversation", &self.conversation)
            .field("turn", &self.turn)
            .field("seq", &self.seq)
            .field("kind", &self.kind)
            .finish()
    }
}

// ---- the state and its reducer ------------------------------------------------------------------------------------

/// What [`ChatState::apply`] did with an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyResult {
    /// The event was the next one and changed the state.
    Applied,
    /// The event is old or belongs to another chat; nothing changed.
    Stale,
    /// An event is missing before this one; nothing changed. The consumer asks for a snapshot.
    Gap,
}

/// What the service remembers of the last turn of a chat, so that a snapshot of an idle chat carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastTurn {
    /// The turn number.
    pub turn: u64,
    /// The last `seq` of the turn.
    pub seq: u64,
    /// The token totals of the turn.
    pub usage: Usage,
    /// What was trimmed.
    pub trimmed: Trimmed,
    /// How the turn ended.
    pub ended: Option<EndView>,
}

/// A chat as the page holds it. The keys are written in the order of the fields.
#[derive(Clone, Serialize)]
pub struct ChatState {
    /// The chat.
    pub conversation: String,
    /// The title; empty until the first message.
    pub title: String,
    /// The profile the chat is pinned to.
    pub profile: Option<String>,
    /// The model the chat uses.
    pub model: Option<String>,
    /// The latest turn this state has seen; 0 before any.
    pub turn: u64,
    /// The last `seq` this state has applied in that turn.
    pub seq: u64,
    /// Whether the turn is running.
    pub running: bool,
    /// The finished messages, oldest first.
    pub messages: Vec<ChatMessage>,
    /// The answer that is being streamed.
    pub partial: Partial,
    /// The calls that wait for a decision.
    pub pending: Vec<Pending>,
    /// The token totals of the turn.
    pub usage: Usage,
    /// What was trimmed in the turn.
    pub trimmed: Trimmed,
    /// How the last turn ended.
    pub ended: Option<EndView>,
}

impl fmt::Debug for ChatState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatState")
            .field("conversation", &self.conversation)
            .field("title", &Size(self.title.len()))
            .field("profile", &self.profile)
            .field("model", &self.model)
            .field("turn", &self.turn)
            .field("seq", &self.seq)
            .field("running", &self.running)
            .field("messages", &self.messages)
            .field("partial", &self.partial)
            .field("pending", &self.pending)
            .field("usage", &self.usage)
            .field("trimmed", &self.trimmed)
            .field("ended", &self.ended)
            .finish()
    }
}

impl ChatState {
    /// An empty state for a chat that has no messages and no turn.
    pub fn new(conversation: &str) -> ChatState {
        ChatState {
            conversation: conversation.to_owned(),
            title: String::new(),
            profile: None,
            model: None,
            turn: 0,
            seq: 0,
            running: false,
            messages: Vec::new(),
            partial: Partial::default(),
            pending: Vec::new(),
            usage: Usage::default(),
            trimmed: Trimmed::default(),
            ended: None,
        }
    }

    /// The state of an idle chat: its stored messages and pins, and what is remembered of its last turn.
    pub fn from_conversation(conversation: &Conversation, last: Option<&LastTurn>) -> ChatState {
        let mut state = ChatState::new(conversation.id.as_str());
        state.title.clone_from(&conversation.title);
        state.profile.clone_from(&conversation.profile);
        state.model.clone_from(&conversation.model);
        state.messages = conversation.messages.iter().map(ChatMessage::from_message).collect();
        if let Some(last) = last {
            state.turn = last.turn;
            state.seq = last.seq;
            state.usage = last.usage;
            state.trimmed = last.trimmed;
            state.ended.clone_from(&last.ended);
        }
        state
    }

    /// What the service remembers of the turn this state shows.
    pub fn last_turn(&self) -> LastTurn {
        LastTurn { turn: self.turn, seq: self.seq, usage: self.usage, trimmed: self.trimmed, ended: self.ended.clone() }
    }

    /// Clears the fields that belong to one turn.
    fn reset_turn(&mut self) {
        self.running = false;
        self.partial = Partial::default();
        self.pending.clear();
        self.usage = Usage::default();
        self.trimmed = Trimmed::default();
        self.ended = None;
    }

    /// Applies one event. This is the only rule set: the service folds its live state with it and every consumer does
    /// the same.
    ///
    /// In this order: an event of another conversation is [`ApplyResult::Stale`]. In the same turn, a `seq` at most the
    /// state's is `Stale`, exactly one higher is applied, and anything further ahead is a [`ApplyResult::Gap`]. An
    /// event of another turn is applied only at `seq` 1 (the per-turn fields are reset first) and is a `Gap` at any
    /// other `seq`. A `Gap` changes nothing.
    pub fn apply(&mut self, event: &AiEvent) -> ApplyResult {
        if event.conversation != self.conversation {
            return ApplyResult::Stale;
        }
        if event.turn == self.turn {
            if event.seq <= self.seq {
                return ApplyResult::Stale;
            }
            if event.seq != self.seq + 1 {
                return ApplyResult::Gap;
            }
        } else {
            if event.seq != 1 {
                return ApplyResult::Gap;
            }
            self.reset_turn();
        }
        self.turn = event.turn;
        self.seq = event.seq;
        match &event.kind {
            AiEventKind::TurnStarted { message, route } => {
                if self.title.is_empty() {
                    self.title = title_from(&message.text());
                }
                self.messages.push(message.clone());
                self.reset_turn();
                self.running = true;
                self.profile = Some(route.profile.clone());
                self.model = Some(route.model.clone());
            }
            AiEventKind::Delta { text } => self.partial.text.push_str(text),
            AiEventKind::Reasoning { text } => self.partial.reasoning.push_str(text),
            AiEventKind::Message { message } => {
                self.messages.push(message.clone());
                self.partial = Partial::default();
            }
            AiEventKind::ToolProposed { pending } => self.pending.push(pending.clone()),
            AiEventKind::ToolResult { call_id, .. } => self.pending.retain(|entry| entry.call_id != *call_id),
            AiEventKind::Usage(usage) => self.usage = *usage,
            AiEventKind::Trimmed(trimmed) => self.trimmed = *trimmed,
            AiEventKind::TurnEnded(end) => {
                self.running = false;
                self.partial = Partial::default();
                self.pending.clear();
                self.ended = Some(end.clone());
            }
        }
        ApplyResult::Applied
    }
}
