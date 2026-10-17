//! What text goes to the model.
//!
//! The approved envelope of the request's context reaches the prompt byte for byte, and nothing private beyond it:
//! tool calls, tool results, the text a note stored and a provider-native turn are never rendered.

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

/// The longest reply asked for when the request names none, in tokens.
const DEFAULT_REPLY_TOKENS: u32 = 1024;

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

/// Renders the last message of `req` into the instructions and the prompt of one call.
///
/// The last message must be a user message with some content. A text over [`MAX_INPUT_BYTES`] is
/// [`AiError::ContextLength`].
pub fn render(req: &ChatRequest) -> Result<Rendered, AiError> {
    let invalid = || AiError::InvalidRequest(Detail::new("no_user_message"));
    let last = req.messages.last().filter(|message| message.role == Role::User).ok_or_else(invalid)?;
    let prompt = content(req, last);
    if prompt.trim().is_empty() {
        return Err(invalid());
    }
    let instructions = req.system.clone();
    if prompt.len() > MAX_INPUT_BYTES || instructions.len() > MAX_INPUT_BYTES {
        return Err(AiError::ContextLength);
    }
    let max_response_tokens = req.max_output_tokens.unwrap_or(DEFAULT_REPLY_TOKENS).min(DEFAULT_REPLY_TOKENS).max(1);
    let input_tokens = estimate_tokens(&instructions).saturating_add(estimate_tokens(&prompt));
    Ok(Rendered { instructions, prompt, max_response_tokens, input_tokens })
}
