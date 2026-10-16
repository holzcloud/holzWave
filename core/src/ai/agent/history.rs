//! What the loop does to the transcript around a request: measuring it, fitting it to the model's window on a copy,
//! and keeping stored text out of it.
//!
//! Sizes are byte counts under one rule ([`message_bytes`]), so a budget, a trim and a token estimate all agree. The
//! text of a context note that the user chose to store is never sent and never counted: the note counts as a fixed
//! marker line.

use std::collections::HashSet;

use super::{INTERRUPTED_RESULT, TRIMMED_RESULT};
use crate::ai::gate::ApprovedContext;
use crate::ai::store::NOT_STORED;
use crate::ai::types::{Message, MessageStatus, Part, Role, ToolResult, ToolSpec, json_size};

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

/// What [`repair_dangling`] changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Repair {
    /// Calls without a result that got the interrupted result.
    pub results_added: usize,
    /// Results without a call, and repeated results, that were removed.
    pub orphans_removed: usize,
    /// Assistant messages that were marked interrupted.
    pub marked: usize,
}

impl Repair {
    /// Whether the transcript was changed at all.
    pub fn changed(&self) -> bool {
        self.results_added > 0 || self.orphans_removed > 0 || self.marked > 0
    }
}

/// Brings a stored transcript into the shape every provider accepts: each tool call has exactly one result, in the tool
/// message that directly follows its assistant message, and no result stands without its call.
///
/// A chat that was interrupted (the application closed while a call was open) holds calls without results; a provider
/// refuses every later message of such a chat. Each missing result becomes an error result with [`INTERRUPTED_RESULT`]
/// (a fixed text, never output), appended to the tool message that follows or, when there is none, in a new tool
/// message with the id `<assistant id>-repair` and status `Interrupted`. An assistant message that was `Complete`
/// becomes `Interrupted`. A result whose call the assistant message before it did not ask for is removed, and so is a
/// second result for one call (the first stays). A transcript that needs none of this is not touched, and a second call
/// changes nothing.
pub fn repair_dangling(messages: &mut Vec<Message>) -> Repair {
    let mut report = Repair::default();
    let mut rest = std::mem::take(messages).into_iter().peekable();
    let mut repaired: Vec<Message> = Vec::new();
    while let Some(mut message) = rest.next() {
        match message.role {
            Role::Tool => {
                // No assistant message with calls introduced this tool message, so its results answer nothing.
                let before = message.parts.len();
                message.parts.retain(|part| !matches!(part, Part::ToolResult(_)));
                report.orphans_removed += before - message.parts.len();
                if !message.parts.is_empty() {
                    repaired.push(message);
                }
            }
            Role::Assistant if message.parts.iter().any(|part| matches!(part, Part::ToolCall(_))) => {
                let asked: Vec<String> = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::ToolCall(call) => Some(call.id.clone()),
                        _ => None,
                    })
                    .collect();
                let mut answers: Vec<Message> = Vec::new();
                while let Some(next) = rest.next_if(|next| next.role == Role::Tool) {
                    answers.push(next);
                }
                let mut answered: HashSet<String> = HashSet::new();
                for answer in &mut answers {
                    answer.parts.retain(|part| {
                        let Part::ToolResult(result) = part else { return true };
                        let keep = asked.contains(&result.call_id) && answered.insert(result.call_id.clone());
                        if !keep {
                            report.orphans_removed += 1;
                        }
                        keep
                    });
                }
                answers.retain(|answer| !answer.parts.is_empty());

                let mut missing: Vec<Part> = Vec::new();
                for id in &asked {
                    if answered.insert(id.clone()) {
                        let content = INTERRUPTED_RESULT.to_owned();
                        let result = ToolResult { call_id: id.clone(), content, is_error: true };
                        missing.push(Part::ToolResult(result));
                    }
                }
                if !missing.is_empty() {
                    report.results_added += missing.len();
                    match answers.first_mut() {
                        Some(first) => first.parts.extend(missing),
                        None => {
                            let id = format!("{}-repair", message.id);
                            let mut added = Message::new(id, Role::Tool, missing, message.ts);
                            added.status = MessageStatus::Interrupted;
                            answers.push(added);
                        }
                    }
                    if message.status == MessageStatus::Complete {
                        message.status = MessageStatus::Interrupted;
                        report.marked += 1;
                    }
                }
                repaired.push(message);
                repaired.extend(answers);
            }
            Role::User | Role::Assistant => repaired.push(message),
        }
    }
    *messages = repaired;
    report
}
