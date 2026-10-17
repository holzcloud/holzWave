//! What text goes to the model, and how much of it fits.
//!
//! The approved envelope of the request's context reaches the prompt byte for byte, and nothing private beyond it:
//! tool calls, tool results, the text a note stored and a provider-native turn are never rendered.
//!
//! The budget comes from the model at run time ([`Budget::for_context`]). The estimate of a token count is deliberately
//! conservative, so a request fails here, before the framework is called, rather than inside it. The exact count of the
//! model is asked only when the estimate says that a request does not fit.

use std::borrow::Cow;
use std::fmt;

use holzwave_core::ai::provider::{AiError, ChatRequest, Detail};
use holzwave_core::ai::types::{ContextNote, Message, Part, Role};

use crate::backend::MAX_INPUT_BYTES;

/// The window assumed when the model cannot report one.
pub const FALLBACK_CONTEXT_TOKENS: u32 = 4096;
/// Tokens kept free for the framework's own framing of a request.
pub const FRAME_MARGIN_TOKENS: u32 = 128;
/// The most tokens the system text may take.
pub const SYSTEM_BUDGET_TOKENS: u32 = 300;

/// The most earlier turns that are considered.
const MAX_HISTORY_TURNS: usize = 40;
/// The heading before the earlier turns, and the one before the current message.
const HISTORY_HEADING: &str = "Earlier in this conversation:";
const CURRENT_HEADING: &str = "Current message:";

/// How a window is shared between the reply, the framework's framing, the system text and the input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// The whole window of the model in tokens: input and reply together.
    pub context: u32,
    /// Tokens kept for the reply: a quarter of the window, at least 256 and at most 1024.
    pub reply: u32,
    /// Tokens kept for the framework's own framing of a request.
    pub margin: u32,
    /// What the instructions and the prompt may take together.
    pub input: u32,
    /// What the system text may take of the input.
    pub system: u32,
}

impl Budget {
    /// The budget of a window of `context` tokens.
    pub fn for_context(context: u32) -> Self {
        let reply = (context / 4).clamp(256, 1024);
        let margin = FRAME_MARGIN_TOKENS;
        let input = context.saturating_sub(reply).saturating_sub(margin);
        let system = SYSTEM_BUDGET_TOKENS.min(input / 4);
        Self { context, reply, margin, input, system }
    }
}

/// A conservative count of tokens: three ASCII bytes per token and one token per other character.
pub fn estimate_tokens(text: &str) -> u32 {
    let ascii = text.bytes().filter(u8::is_ascii).count();
    let other = text.chars().filter(|c| !c.is_ascii()).count();
    u32::try_from(ascii.div_ceil(3) + other).unwrap_or(u32::MAX)
}

/// Counts the estimated tokens of text that arrives in pieces, without keeping it. The total of the pieces equals
/// [`estimate_tokens`] of their concatenation.
#[derive(Debug, Default, Clone, Copy)]
pub struct Estimator {
    ascii: usize,
    other: usize,
}

impl Estimator {
    /// An estimator that has seen nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a piece of text.
    pub fn push(&mut self, piece: &str) {
        self.ascii += piece.bytes().filter(u8::is_ascii).count();
        self.other += piece.chars().filter(|c| !c.is_ascii()).count();
    }

    /// The estimate of everything pushed so far.
    pub fn total(&self) -> u32 {
        u32::try_from(self.ascii.div_ceil(3) + self.other).unwrap_or(u32::MAX)
    }
}

/// What one request renders to.
///
/// `Debug` prints sizes, never text.
#[derive(Clone)]
pub struct Rendered {
    /// The system text.
    pub instructions: String,
    /// The prompt.
    pub prompt: String,
    /// The reply limit to send to the model.
    pub max_response_tokens: u32,
    /// The input size in tokens (an estimate).
    pub input_tokens: u32,
}

impl fmt::Debug for Rendered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rendered")
            .field("instructions", &format_args!("<{} bytes>", self.instructions.len()))
            .field("prompt", &format_args!("<{} bytes>", self.prompt.len()))
            .field("max_response_tokens", &self.max_response_tokens)
            .field("input_tokens", &self.input_tokens)
            .finish()
    }
}

/// One line for a note whose context is not approved for this request (history): the labels and sizes, never text.
fn marker(note: &ContextNote) -> String {
    let items: Vec<String> = note
        .items
        .iter()
        .map(|item| {
            let label: String = item.label.chars().map(|c| if c.is_control() { ' ' } else { c }).take(80).collect();
            format!("{}, {} bytes", label.trim(), item.bytes)
        })
        .collect();
    format!("[context not resent: {}]", items.join("; "))
}

/// The text of a message for the prompt: its text parts and its context, in order, joined by a line feed. A context is
/// the envelope of the approved context with the note's hash, and a marker line when there is none.
fn content(req: &ChatRequest, message: &Message) -> String {
    let mut pieces: Vec<Cow<'_, str>> = Vec::new();
    for part in &message.parts {
        match part {
            Part::Text { text } => pieces.push(Cow::Borrowed(text)),
            Part::Context(note) => pieces.push(match req.approved_for(&note.hash) {
                Some(approved) => Cow::Borrowed(approved.envelope()),
                None => Cow::Owned(marker(note)),
            }),
            Part::ToolCall(_) | Part::ToolResult(_) => {}
        }
    }
    pieces.join("\n")
}

/// The system text cut after the last complete line that fits `tokens`, keeping the head. A text that fits is unchanged.
fn cut_system(text: &str, tokens: u32) -> String {
    if estimate_tokens(text) <= tokens {
        return text.to_owned();
    }
    let mut kept = Estimator::new();
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        kept.push(line);
        if kept.total() > tokens {
            break;
        }
        end += line.len();
    }
    text[..end].trim_end_matches(['\n', '\r']).to_owned()
}

/// One earlier turn: who said it, and what of it is sent.
struct Turn {
    label: &'static str,
    text: String,
}

/// The prompt for a set of earlier turns and the current message.
fn build_prompt(turns: &[Turn], current: &str) -> String {
    if turns.is_empty() {
        return current.to_owned();
    }
    let mut out = String::from(HISTORY_HEADING);
    for turn in turns {
        out.push('\n');
        out.push_str(turn.label);
        out.push(' ');
        out.push_str(&turn.text);
    }
    out.push_str("\n\n");
    out.push_str(CURRENT_HEADING);
    out.push('\n');
    out.push_str(current);
    out
}

/// Renders `req` into the instructions and the prompt of one call, within `budget`.
///
/// The last message must be a user message with some content. The earlier turns are kept newest first as long as they
/// fit: the oldest whole turns are dropped, a turn is never cut, and at most the 40 newest are considered. A request
/// that does not fit even without history is [`AiError::ContextLength`]; its envelope is never cut. `exact` is asked for
/// the exact token count of a text only when the estimate says the request does not fit.
pub fn render(
    req: &ChatRequest,
    budget: &Budget,
    exact: &dyn Fn(&str) -> Option<u32>,
) -> Result<Rendered, AiError> {
    let invalid = || AiError::InvalidRequest(Detail::new("no_user_message"));
    let (last, earlier) = req.messages.split_last().filter(|(last, _)| last.role == Role::User).ok_or_else(invalid)?;
    let current = content(req, last);
    if current.trim().is_empty() {
        return Err(invalid());
    }
    let instructions = cut_system(&req.system, budget.system);

    let mut turns: Vec<Turn> = earlier
        .iter()
        .filter_map(|message| {
            let label = match message.role {
                Role::User => "User:",
                Role::Assistant => "Assistant:",
                Role::Tool => return None,
            };
            let text = content(req, message);
            (!text.trim().is_empty()).then_some(Turn { label, text })
        })
        .collect();
    if turns.len() > MAX_HISTORY_TURNS {
        turns.drain(..turns.len() - MAX_HISTORY_TURNS);
    }

    // The exact count of the instructions does not change while turns are dropped, so it is asked at most once.
    let mut exact_instructions: Option<Option<u32>> = None;
    let mut first = 0;
    loop {
        let prompt = build_prompt(&turns[first..], &current);
        let within_bytes = prompt.len() <= MAX_INPUT_BYTES && instructions.len() <= MAX_INPUT_BYTES;
        let estimate = estimate_tokens(&instructions).saturating_add(estimate_tokens(&prompt));
        let input_tokens = if within_bytes && estimate <= budget.input {
            Some(estimate)
        } else if within_bytes {
            let system = *exact_instructions
                .get_or_insert_with(|| if instructions.is_empty() { Some(0) } else { exact(&instructions) });
            system
                .and_then(|system| exact(&prompt).map(|prompt| system.saturating_add(prompt)))
                .filter(|count| *count <= budget.input)
        } else {
            None
        };
        if let Some(input_tokens) = input_tokens {
            let max_response_tokens = req.max_output_tokens.unwrap_or(budget.reply).min(budget.reply).max(1);
            return Ok(Rendered { instructions, prompt, max_response_tokens, input_tokens });
        }
        if first == turns.len() {
            return Err(AiError::ContextLength);
        }
        first += 1;
    }
}
