//! What the loop does to the transcript around a request: measuring it, fitting it to the model's window on a copy,
//! and keeping stored text out of it.
//!
//! Sizes are byte counts under one rule ([`message_bytes`]), so a budget, a trim and a token estimate all agree. The
//! text of a context note that the user chose to store is never sent and never counted: the note counts as a fixed
//! marker line.

use std::collections::HashSet;

use super::TRIMMED_RESULT;
use crate::ai::gate::ApprovedContext;
use crate::ai::store::NOT_STORED;
use crate::ai::types::{Message, Part, Role, ToolSpec, json_size};

/// What counts for a context note without an approved context in the request: the marker line an adapter renders.
const NOTE_BYTES: usize = 128;

/// What trimming changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Trim {
    /// Old tool results that were replaced by the trimmed marker.
    pub results: u32,
    /// Old exchanges that were removed.
    pub exchanges: u32,
}

/// The bytes one message adds to a request: every text, each call's name and arguments, each result's content, a fixed
/// marker for a context note, and the provider-native turn.
pub(super) fn message_bytes(message: &Message) -> usize {
    let mut total = message.native.as_ref().map_or(0, |native| json_size(&native.json).0);
    for part in &message.parts {
        total = total.saturating_add(match part {
            Part::Text { text } => text.len(),
            Part::ToolCall(call) => call.name.len().saturating_add(json_size(&call.args).0),
            Part::ToolResult(result) => result.content.len(),
            Part::Context(_) => NOTE_BYTES,
        });
    }
    total
}

/// The bytes of a request that do not depend on the history: the system prompt, the tool declarations and the approved
/// contexts.
pub(super) fn fixed_bytes(system: &str, tools: &[ToolSpec], context: &[ApprovedContext]) -> usize {
    let tools: usize =
        tools.iter().map(|tool| tool.description.len().saturating_add(json_size(&tool.input_schema).0)).sum();
    let context: usize = context.iter().map(ApprovedContext::bytes).sum();
    system.len().saturating_add(tools).saturating_add(context)
}

/// Removes the stored text of every context note: the loop strips it itself instead of trusting each adapter to ignore
/// it.
pub(super) fn strip_stored(messages: &mut [Message]) {
    for part in messages.iter_mut().flat_map(|message| message.parts.iter_mut()) {
        if let Part::Context(note) = part {
            note.stored = None;
        }
    }
}

/// Makes `messages` fit `budget` bytes together with the `fixed` bytes of the request, on the copy it is given.
///
/// Phase A replaces the content of old tool results, oldest first, by [`TRIMMED_RESULT`]. Phase B removes whole old
/// exchanges (a block that starts at a user message), oldest first, and skips an exchange that holds a context note
/// named in `pinned`. A message at or after `turn_start` is never touched. Calls and results stay paired because only
/// results are rewritten in place and only whole blocks are removed. `Err` means that even the trimmed history does not
/// fit.
pub(super) fn fit(
    messages: &mut Vec<Message>,
    turn_start: usize,
    pinned: &HashSet<String>,
    budget: usize,
    fixed: usize,
) -> Result<Trim, ()> {
    let mut trim = Trim::default();
    let mut total: usize = messages.iter().map(message_bytes).sum();
    let fits = |total: usize| fixed.saturating_add(total) <= budget;
    if fits(total) {
        return Ok(trim);
    }
    let turn_start = turn_start.min(messages.len());

    for message in &mut messages[..turn_start] {
        for part in &mut message.parts {
            let Part::ToolResult(result) = part else { continue };
            // A result that is already a marker, or too short to gain from one, stays as it is.
            let marker = result.content == NOT_STORED || result.content == TRIMMED_RESULT;
            if marker || result.content.len() <= TRIMMED_RESULT.len() {
                continue;
            }
            total = total - result.content.len() + TRIMMED_RESULT.len();
            TRIMMED_RESULT.clone_into(&mut result.content);
            trim.results += 1;
            if fits(total) {
                return Ok(trim);
            }
        }
    }

    let blocks = blocks(&messages[..turn_start]);
    let mut remove = vec![false; turn_start];
    for (start, end) in blocks {
        if messages[start..end].iter().any(|message| names_pinned(message, pinned)) {
            continue;
        }
        total -= messages[start..end].iter().map(message_bytes).sum::<usize>();
        remove[start..end].fill(true);
        trim.exchanges += 1;
        if fits(total) {
            break;
        }
    }
    let mut index = 0;
    messages.retain(|_| {
        let keep = !remove.get(index).copied().unwrap_or(false);
        index += 1;
        keep
    });
    if fits(total) { Ok(trim) } else { Err(()) }
}

/// The exchanges of `messages` as index ranges: a block starts at each user message, and the messages before the first
/// user message form a block of their own.
fn blocks(messages: &[Message]) -> Vec<(usize, usize)> {
    let mut starts: Vec<usize> = vec![0];
    starts.extend((1..messages.len()).filter(|index| messages[*index].role == Role::User));
    starts
        .iter()
        .enumerate()
        .map(|(position, start)| (*start, starts.get(position + 1).copied().unwrap_or(messages.len())))
        .filter(|(start, end)| start < end)
        .collect()
}

fn names_pinned(message: &Message, pinned: &HashSet<String>) -> bool {
    message.parts.iter().any(|part| matches!(part, Part::Context(note) if pinned.contains(&note.hash)))
}
