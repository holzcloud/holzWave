//! The Gemini `streamGenerateContent` dialect.
//!
//! This adapter speaks `generateContent` streamed as server-sent events (`?alt=sse`), one response object per event.
//!
//! - **The request is a pure function.** [`GeminiAdapter::request`] builds a path and a JSON body from the
//!   [`ChatRequest`] and nothing else: no I/O, no clock, no randomness, and no header. The body holds `contents` and,
//!   when they apply, `systemInstruction`, `tools` and `generationConfig.maxOutputTokens`. No sampling parameter,
//!   safety override, thinking configuration, tool-choice configuration, cached-content reference or hosted tool is
//!   ever sent.
//! - **The key is never seen here.** The endpoint carries it in a header that is bound to its origin, and the HTTP
//!   chokepoint refuses a call whose address holds it. This file therefore names no key type, no secret header and no
//!   key accessor, and builds no address that could hold a key: the model id that becomes part of the path is checked
//!   by [`stream_path`] first.
//! - **The model's own turn is kept and echoed.** A reply is a list of parts, and the signatures that ride on some of
//!   them only work when they are sent back unchanged. The parts are kept in order as the provider-native turn
//!   (`{ "role": "model", "parts": [...] }`, unknown keys dropped) and replayed byte for byte on the next request, but
//!   only when the blob was written by this dialect and passes an allow-list check; anything else is dropped and the
//!   turn is rebuilt from its text and calls, without signatures.
//! - **Results come back in call order, in one content.** A reply with several calls is answered by one user content
//!   whose function responses follow the order of the calls, with the tool name and any provider id.
//! - **Terminal text leaves only as approved.** A context note is written from the exact envelope of the approval the
//!   request carries, or as one marker line made of kinds, labels and byte counts. The text a note may have kept is
//!   never read.
//! - **A tool call is released only after a normal stop.** Calls arrive whole inside one part, are held by the
//!   assembler, and leave from [`WireTurn::finish`] only, after the stream ended cleanly with the finish reason `STOP`.

use std::collections::HashSet;
use std::fmt;

use serde_json::{Map, Value, json};

use super::{Wire, WireAdapter, WireRequest, WireTurn};
use crate::ai::http::Frame;
use crate::ai::provider::{AiError, ChatRequest, Completion, Detail, Flow, StreamEvent};
use crate::ai::sse::{SseEvent, StreamEnd, ToolAssembler};
use crate::ai::types::{
    Capabilities, ContextKind, ContextNote, Locality, Message, Opaque, Part, Role, Size, StopReason, ToolResult,
    ToolSpec, Usage, json_size,
};

// ---- constants --------------------------------------------------------------------------------------------

/// The header the key travels in. The profile binds the key to this name with an empty prefix; this file never sees
/// the key.
pub const API_KEY_HEADER: &str = "x-goog-api-key";

/// The longest model id, in characters, without its optional `models/` prefix.
pub const MAX_MODEL_ID: usize = 64;

/// The context window assumed for every model, in tokens.
pub const CONTEXT_TOKENS: u32 = 1_048_576;

/// The longest answer assumed for every model, in tokens. A request that asks for more is clamped to this.
pub const OUTPUT_TOKENS: u32 = 8192;

/// The most parts one stored turn may hold, in a reply and in a replayed blob.
pub const MAX_PARTS: usize = 1024;

/// The most parts one stream event may hold.
pub const MAX_PARTS_PER_EVENT: usize = 256;

/// The most bytes a stored turn may hold: 4 MiB. It is kept in the chat file and sent back with every request.
pub const MAX_NATIVE_BYTES: usize = 4_194_304;

/// The longest thought signature, in bytes: 256 KiB.
pub const MAX_SIGNATURE_BYTES: usize = 262_144;

/// The most tools one request may declare.
pub const MAX_TOOLS: usize = 64;

/// The longest tool description, in bytes.
pub const MAX_DESCRIPTION: usize = 4096;

/// The largest serialised tool schema, in bytes: 32 KiB.
pub const MAX_SCHEMA_BYTES: usize = 32_768;

/// The longest tool name, in characters.
const MAX_TOOL_NAME: usize = 128;

/// The suffix of the stream path.
const STREAM_METHOD: &str = ":streamGenerateContent?alt=sse";

// ---- errors -----------------------------------------------------------------------------------------------

/// An error of the answer: a fixed phrase and nothing from the stream.
fn protocol(phrase: &str) -> AiError {
    AiError::Protocol(Detail::new(phrase))
}

/// An error of the request: a fixed phrase and nothing from the request.
fn invalid(phrase: &str) -> AiError {
    AiError::InvalidRequest(Detail::new(phrase))
}

const PHRASE_MODEL: &str = "the model name is not valid for this service";
const PHRASE_WRITE: &str = "the request could not be written as JSON";
const PHRASE_LIMIT: &str = "the output limit must be more than zero";
const PHRASE_NO_MESSAGES: &str = "the request has no messages";
const PHRASE_NO_INPUT: &str = "the request has nothing to send";
const PHRASE_WRONG_PART: &str = "a message holds a part that does not belong to its role";
const PHRASE_EDGES: &str = "the conversation must start and end with a user message";
const PHRASE_CALL_ARGS: &str = "a tool call has arguments that are not an object";
const PHRASE_TOO_MANY_TOOLS: &str = "the request declares more tools than one request allows";
const PHRASE_TOOL_NAME: &str = "a tool name is not allowed";
const PHRASE_TOOL_TWICE: &str = "two tools share a name";
const PHRASE_TOOL_DESCRIPTION: &str = "a tool description is missing or too long";
const PHRASE_TOOL_SCHEMA: &str = "a tool schema is not a JSON object schema";
const PHRASE_TOOL_SCHEMA_SIZE: &str = "a tool schema is larger than one request allows";
const PHRASE_ORPHAN: &str = "a tool result does not answer a tool call of the reply before it";
const PHRASE_TWO_RESULTS: &str = "a tool call has more than one result";
const PHRASE_UNANSWERED: &str = "a tool call has no result in the message after it";
const PHRASE_FRAME: &str = "the response is not an event stream";
const PHRASE_UNREADABLE: &str = "the response holds an event that cannot be read";
const PHRASE_PART: &str = "the response holds a part that cannot be read";
const PHRASE_ERROR_EVENT: &str = "the service reported an error while streaming";
const PHRASE_NO_FINISH: &str = "the response ended without a finish reason";
const PHRASE_TRUNCATED: &str = "the response ended before it was complete";
const PHRASE_FAILED_EARLIER: &str = "the response was refused earlier";

// ---- the address ------------------------------------------------------------------------------------------

/// The model id without its optional `models/` prefix.
fn bare_model(model: &str) -> &str {
    model.strip_prefix("models/").unwrap_or(model)
}

/// Whether `id` may become a segment of the request path: 1 to [`MAX_MODEL_ID`] ASCII letters, digits, `.`, `_` and
/// `-`, a letter or a digit first, and no `..`. The id comes from a setting or from the page, so nothing outside this
/// set (a slash, a query, a colon, a percent sign, a space, a line break) may reach the address.
fn valid_model_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_MODEL_ID
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !id.contains("..")
}

/// The request path below the base address of the profile, which ends in the API version segment: for example
/// `/models/gemini-3.8-flash:streamGenerateContent?alt=sse`.
///
/// The model id is checked before it reaches the address: one optional `models/` prefix, then 1 to [`MAX_MODEL_ID`]
/// characters of letters, digits, `.`, `_` and `-`, a letter or a digit first, no `..`. Anything else is
/// [`AiError::InvalidRequest`] with a fixed phrase that does not echo the input. The key is never part of the path:
/// it travels in the [`API_KEY_HEADER`] header that the endpoint adds.
pub fn stream_path(model: &str) -> Result<String, AiError> {
    let id = bare_model(model);
    if !valid_model_id(id) {
        return Err(invalid(PHRASE_MODEL));
    }
    Ok(format!("/models/{id}{STREAM_METHOD}"))
}

// ---- the adapter ------------------------------------------------------------------------------------------

/// The adapter of the Gemini dialect. It has no field: everything it needs comes with the request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GeminiAdapter;

impl GeminiAdapter {
    /// The adapter.
    pub fn new() -> Self {
        Self
    }
}

impl WireAdapter for GeminiAdapter {
    fn wire(&self) -> Wire {
        Wire::Gemini
    }

    fn capabilities(&self, model: &str, locality: Locality) -> Capabilities {
        // The open-weight models that the service also hosts do not take function declarations.
        let tools = !bare_model(model).to_ascii_lowercase().starts_with("gemma");
        Capabilities {
            tools,
            reasoning: false,
            vision: false,
            context_tokens: Some(CONTEXT_TOKENS),
            max_output_tokens: Some(OUTPUT_TOKENS),
            ..Capabilities::conservative(locality)
        }
    }

    fn request(&self, req: &ChatRequest, caps: &Capabilities) -> Result<WireRequest, AiError> {
        let path = stream_path(&req.model)?;
        let body = build_body(req, caps)?;
        let body = serde_json::to_vec(&body).map_err(|_| invalid(PHRASE_WRITE))?;
        // No header: the key is the endpoint's business.
        Ok(WireRequest { path, headers: Vec::new(), body })
    }

    fn turn(&self) -> Box<dyn WireTurn> {
        Box::new(GeminiTurn::new())
    }
}

// ---- the request ------------------------------------------------------------------------------------------

/// The request body: `contents`, and `systemInstruction`, `tools` and `generationConfig` when they apply.
fn build_body(req: &ChatRequest, caps: &Capabilities) -> Result<Value, AiError> {
    let mut body = Map::new();
    body.insert("contents".to_owned(), Value::Array(build_contents(req)?));
    if !req.system.is_empty() {
        body.insert("systemInstruction".to_owned(), json!({ "parts": [{ "text": req.system }] }));
    }
    if caps.tools && !req.tools.is_empty() {
        body.insert("tools".to_owned(), json!([{ "functionDeclarations": tool_declarations(req)? }]));
    }
    if let Some(limit) = req.max_output_tokens {
        if limit == 0 {
            return Err(invalid(PHRASE_LIMIT));
        }
        let limit = caps.max_output_tokens.map_or(limit, |most| limit.min(most));
        body.insert("generationConfig".to_owned(), json!({ "maxOutputTokens": limit }));
    }
    Ok(Value::Object(body))
}

/// The function declarations of the request, in order, with each schema passed through unchanged after the checks of
/// [`check_tool`]. The request is refused before any connection when a declaration is not usable.
fn tool_declarations(req: &ChatRequest) -> Result<Vec<Value>, AiError> {
    if req.tools.len() > MAX_TOOLS {
        return Err(invalid(PHRASE_TOO_MANY_TOOLS));
    }
    let mut names: HashSet<String> = HashSet::new();
    let mut declarations = Vec::with_capacity(req.tools.len());
    for tool in &req.tools {
        check_tool(tool, &mut names)?;
        declarations.push(json!({
            "name": tool.name,
            "description": tool.description,
            "parametersJsonSchema": tool.input_schema,
        }));
    }
    Ok(declarations)
}

/// Checks one declaration: a name of 1 to [`MAX_TOOL_NAME`] letters, digits, `_` and `-` that starts with a letter or
/// `_` and is not used twice; a description that is not empty and has at most [`MAX_DESCRIPTION`] bytes; a schema that
/// is a JSON object whose `type` is `object`, serialised in at most [`MAX_SCHEMA_BYTES`] bytes. Every phrase is fixed:
/// the declaration is data of the application, but it is never echoed.
fn check_tool(tool: &ToolSpec, names: &mut HashSet<String>) -> Result<(), AiError> {
    let bytes = tool.name.as_bytes();
    let name_ok = !bytes.is_empty()
        && bytes.len() <= MAX_TOOL_NAME
        && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && bytes.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    if !name_ok {
        return Err(invalid(PHRASE_TOOL_NAME));
    }
    if !names.insert(tool.name.clone()) {
        return Err(invalid(PHRASE_TOOL_TWICE));
    }
    if tool.description.trim().is_empty() || tool.description.len() > MAX_DESCRIPTION {
        return Err(invalid(PHRASE_TOOL_DESCRIPTION));
    }
    let is_object_schema =
        tool.input_schema.as_object().is_some_and(|schema| schema.get("type") == Some(&json!("object")));
    if !is_object_schema {
        return Err(invalid(PHRASE_TOOL_SCHEMA));
    }
    if json_size(&tool.input_schema).0 > MAX_SCHEMA_BYTES {
        return Err(invalid(PHRASE_TOOL_SCHEMA_SIZE));
    }
    Ok(())
}

/// A call of a model reply, as a later result has to name it.
struct CallRef {
    /// The id the message holds for the call, which a result carries back.
    id: String,
    /// The name of the tool.
    name: String,
    /// The id the provider put on the call part, when the part was replayed and had one.
    wire_id: Option<String>,
}

/// Which side of the conversation a message is on: the model, or everything else (the user and the tool results).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    User,
    Model,
}

fn side_of(role: Role) -> Side {
    match role {
        Role::Assistant => Side::Model,
        Role::User | Role::Tool => Side::User,
    }
}

/// Whether a message has anything to send: a non-empty text, a call, a result or a context note.
fn has_content(message: &Message) -> bool {
    message.parts.iter().any(|part| match part {
        Part::Text { text } => !text.is_empty(),
        Part::ToolCall(_) | Part::ToolResult(_) | Part::Context(_) => true,
    })
}

/// The `contents` array: adjacent messages of one side are one content, empty messages vanish, and the results of a
/// reply come back in one user content in the order of the calls.
fn build_contents(req: &ChatRequest) -> Result<Vec<Value>, AiError> {
    if req.messages.is_empty() {
        return Err(invalid(PHRASE_NO_MESSAGES));
    }
    check_parts(req)?;
    let mut runs: Vec<(Side, Vec<&Message>)> = Vec::new();
    for message in req.messages.iter().filter(|message| has_content(message)) {
        let side = side_of(message.role);
        match runs.last_mut() {
            Some((last, members)) if *last == side => members.push(message),
            _ => runs.push((side, vec![message])),
        }
    }
    // A conversation starts and ends with the user: the service refuses a content list that ends with the model, and a
    // model content with calls needs the user content that answers them.
    match (runs.first(), runs.last()) {
        (None, _) | (_, None) => return Err(invalid(PHRASE_NO_INPUT)),
        (Some((Side::User, _)), Some((Side::User, _))) => {}
        _ => return Err(invalid(PHRASE_EDGES)),
    }
    let mut contents = Vec::with_capacity(runs.len());
    // The calls of the model content right before the run being written.
    let mut earlier: Vec<CallRef> = Vec::new();
    // The envelopes already written, so one approval is never sent twice.
    let mut sent: HashSet<String> = HashSet::new();
    for (side, members) in runs {
        match side {
            Side::Model => {
                let (parts, calls) = model_parts(&members)?;
                contents.push(json!({ "role": "model", "parts": parts }));
                earlier = calls;
            }
            Side::User => {
                let parts = user_parts(req, &members, &earlier, &mut sent)?;
                contents.push(json!({ "role": "user", "parts": parts }));
                earlier = Vec::new();
            }
        }
    }
    Ok(contents)
}

/// Checks that every part belongs to the role of its message: a user holds text and context, the model holds text and
/// calls, a tool holds results. A violation is an error with a fixed phrase that holds no text of the request.
fn check_parts(req: &ChatRequest) -> Result<(), AiError> {
    for message in &req.messages {
        for part in &message.parts {
            let fits = matches!(
                (message.role, part),
                (Role::User, Part::Text { .. } | Part::Context(_))
                    | (Role::Assistant, Part::Text { .. } | Part::ToolCall(_))
                    | (Role::Tool, Part::ToolResult(_))
            );
            if !fits {
                return Err(invalid(PHRASE_WRONG_PART));
            }
        }
    }
    Ok(())
}

/// The parts of a user-side content: the function responses first, in the order of the calls they answer, then the
/// text and context parts in message order.
fn user_parts(
    req: &ChatRequest,
    members: &[&Message],
    earlier: &[CallRef],
    sent: &mut HashSet<String>,
) -> Result<Vec<Value>, AiError> {
    let mut results: Vec<&ToolResult> = Vec::new();
    let mut others: Vec<Value> = Vec::new();
    for message in members {
        for part in &message.parts {
            match part {
                Part::Text { text } if !text.is_empty() => others.push(json!({ "text": text })),
                Part::Context(note) => others.push(json!({ "text": render_context(req, note, sent) })),
                Part::ToolResult(result) => results.push(result),
                _ => {}
            }
        }
    }
    let mut answers: Vec<Option<&ToolResult>> = vec![None; earlier.len()];
    for result in results {
        let at = earlier.iter().position(|call| call.id == result.call_id).ok_or_else(|| invalid(PHRASE_ORPHAN))?;
        if answers[at].replace(result).is_some() {
            return Err(invalid(PHRASE_TWO_RESULTS));
        }
    }
    let mut parts = Vec::with_capacity(earlier.len() + others.len());
    for (call, answer) in earlier.iter().zip(answers) {
        let answer = answer.ok_or_else(|| invalid(PHRASE_UNANSWERED))?;
        parts.push(function_response(call, answer));
    }
    parts.extend(others);
    Ok(parts)
}

/// One function response. A failure is reported under `error` and a success under `result`; the provider's call id is
/// copied only when the call part that is replayed had one.
fn function_response(call: &CallRef, result: &ToolResult) -> Value {
    let key = if result.is_error { "error" } else { "result" };
    let mut response = Map::new();
    response.insert(key.to_owned(), Value::String(result.content.clone()));
    let mut body = Map::new();
    body.insert("name".to_owned(), Value::String(call.name.clone()));
    if let Some(id) = &call.wire_id {
        body.insert("id".to_owned(), Value::String(id.clone()));
    }
    body.insert("response".to_owned(), Value::Object(response));
    json!({ "functionResponse": Value::Object(body) })
}

/// The parts of a model-side content and the calls they hold, in order. Each message is replayed from its stored turn
/// when that passes the check, and rebuilt from its text and calls otherwise.
fn model_parts(members: &[&Message]) -> Result<(Vec<Value>, Vec<CallRef>), AiError> {
    let mut parts = Vec::new();
    let mut calls = Vec::new();
    for message in members {
        let message_calls = message.parts.iter().filter_map(|part| match part {
            Part::ToolCall(call) => Some(call),
            _ => None,
        });
        match replay_parts(message) {
            Some(replayed) => {
                for (call, wire_id) in message_calls.zip(replayed.call_ids) {
                    calls.push(CallRef { id: call.id.clone(), name: call.name.clone(), wire_id });
                }
                parts.extend(replayed.parts);
            }
            None => {
                for part in &message.parts {
                    match part {
                        Part::Text { text } if !text.is_empty() => parts.push(json!({ "text": text })),
                        Part::ToolCall(call) => {
                            if !call.args.is_object() {
                                return Err(invalid(PHRASE_CALL_ARGS));
                            }
                            parts.push(json!({ "functionCall": { "name": call.name, "args": call.args } }));
                            calls.push(CallRef { id: call.id.clone(), name: call.name.clone(), wire_id: None });
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Ok((parts, calls))
}

/// The parts of a stored turn that may be replayed, and the provider ids of its calls in order.
struct Replay {
    parts: Vec<Value>,
    call_ids: Vec<Option<String>>,
}

/// The stored turn of a message, when it may be sent back unchanged.
///
/// The stored turn comes from a chat file and may be stale, from another dialect or edited. It is replayed only for
/// this dialect, as an object whose `role` is absent or `model` and whose `parts` are 1 to [`MAX_PARTS`] objects, and
/// only when the serialised blob is at most [`MAX_NATIVE_BYTES`]. Every part holds only the keys `text`, `thought`,
/// `thoughtSignature` and `functionCall`, at most one of `text` and `functionCall`, and at least one of `text`,
/// `functionCall` and `thoughtSignature`. A signature is 1 to [`MAX_SIGNATURE_BYTES`] base64 characters; a call has
/// only `name`, `args` (an object) and an optional string `id`. The calls must equal the message's calls in number and
/// name. A blob that fails any rule is dropped, never repaired.
fn replay_parts(message: &Message) -> Option<Replay> {
    let blob = message.native_for(Wire::Gemini.tag())?;
    if json_size(blob).0 > MAX_NATIVE_BYTES {
        return None;
    }
    let blob = blob.as_object()?;
    if blob.get("role").is_some_and(|role| role.as_str() != Some("model")) {
        return None;
    }
    let parts = blob.get("parts")?.as_array()?;
    if parts.is_empty() || parts.len() > MAX_PARTS {
        return None;
    }
    let mut call_ids = Vec::new();
    let mut names: Vec<&str> = Vec::new();
    for part in parts {
        let part = part.as_object()?;
        if !part.keys().all(|key| matches!(key.as_str(), "text" | "thought" | "thoughtSignature" | "functionCall")) {
            return None;
        }
        let has_text = match part.get("text") {
            None => false,
            Some(Value::String(_)) => true,
            Some(_) => return None,
        };
        if part.get("thought").is_some_and(|thought| !thought.is_boolean()) {
            return None;
        }
        let has_signature = match part.get("thoughtSignature") {
            None => false,
            Some(Value::String(signature)) if signature_is_valid(signature) => true,
            Some(_) => return None,
        };
        match part.get("functionCall") {
            None => {
                if !has_text && !has_signature {
                    return None;
                }
            }
            Some(call) => {
                if has_text {
                    return None;
                }
                let call = call.as_object()?;
                if !call.keys().all(|key| matches!(key.as_str(), "name" | "args" | "id")) {
                    return None;
                }
                let name = call.get("name")?.as_str().filter(|name| call_name_is_valid(name))?;
                if !call.get("args")?.is_object() {
                    return None;
                }
                let id = match call.get("id") {
                    None => None,
                    Some(Value::String(id)) if call_name_is_valid(id) => Some(id.clone()),
                    Some(_) => return None,
                };
                names.push(name);
                call_ids.push(id);
            }
        }
    }
    let wanted = message.parts.iter().filter_map(|part| match part {
        Part::ToolCall(call) => Some(call.name.as_str()),
        _ => None,
    });
    if !wanted.eq(names.iter().copied()) {
        return None;
    }
    Some(Replay { parts: parts.clone(), call_ids })
}

/// Whether a thought signature is 1 to [`MAX_SIGNATURE_BYTES`] characters of the base64 alphabets.
fn signature_is_valid(signature: &str) -> bool {
    !signature.is_empty()
        && signature.len() <= MAX_SIGNATURE_BYTES
        && signature.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-'))
}

/// Whether the name or id of a call is usable: not empty, at most 256 bytes and free of control characters.
fn call_name_is_valid(name: &str) -> bool {
    !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
}

/// What a context note becomes in a request. With an approval in the request: the envelope the user saw, byte for byte,
/// once. Without one the note is history, and one marker line stands in for it: the kind, label and size of each item.
/// The text a note may have kept is never read.
fn render_context(req: &ChatRequest, note: &ContextNote, sent: &mut HashSet<String>) -> String {
    if let Some(approved) = req.approved_for(&note.hash)
        && sent.insert(note.hash.clone())
    {
        return approved.envelope().to_owned();
    }
    marker_line(note)
}

/// The start of the marker line.
const MARKER_START: &str = "[terminal context shared earlier and not repeated";

/// One line that stands for a note whose text is not sent: the kind, the label (scrubbed) and the byte count of each item.
fn marker_line(note: &ContextNote) -> String {
    if note.items.is_empty() {
        return format!("{MARKER_START}]");
    }
    let items: Vec<String> = note
        .items
        .iter()
        .map(|item| {
            format!("{} \"{}\" ({} bytes)", kind_word(item.kind), Detail::new(&item.label).as_str(), item.bytes)
        })
        .collect();
    format!("{MARKER_START}: {}]", items.join("; "))
}

fn kind_word(kind: ContextKind) -> &'static str {
    match kind {
        ContextKind::Selection => "selection",
        ContextKind::Screen => "screen",
        ContextKind::LastCommand => "last command",
        ContextKind::File => "file",
        ContextKind::Preview => "preview",
    }
}

// ---- the answer -------------------------------------------------------------------------------------------

/// How the reply ended, as far as the finish reason says.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Finish {
    /// `STOP`: the model finished.
    Stop,
    /// `MAX_TOKENS`: the output limit was reached.
    MaxTokens,
    /// Any other reason.
    Other,
}

/// One part of the model's reply, as it is stored.
enum NativePart {
    /// Text, with its thought flag and the signature that rode on the part.
    Text { text: String, thought: bool, signature: Option<String> },
    /// A function call, with the provider's id and the signature when it had them.
    Call { name: String, args: Value, id: Option<String>, signature: Option<String> },
    /// A part that held a signature and nothing else.
    Signature(String),
}

impl NativePart {
    /// The stored form: the keys `text`, `thought`, `thoughtSignature` and `functionCall` and no others.
    fn into_value(self) -> Value {
        let mut part = Map::new();
        match self {
            NativePart::Text { text, thought, signature } => {
                part.insert("text".to_owned(), Value::String(text));
                if thought {
                    part.insert("thought".to_owned(), Value::Bool(true));
                }
                if let Some(signature) = signature {
                    part.insert("thoughtSignature".to_owned(), Value::String(signature));
                }
            }
            NativePart::Call { name, args, id, signature } => {
                let mut call = Map::new();
                call.insert("name".to_owned(), Value::String(name));
                call.insert("args".to_owned(), args);
                if let Some(id) = id {
                    call.insert("id".to_owned(), Value::String(id));
                }
                part.insert("functionCall".to_owned(), Value::Object(call));
                if let Some(signature) = signature {
                    part.insert("thoughtSignature".to_owned(), Value::String(signature));
                }
            }
            NativePart::Signature(signature) => {
                part.insert("thoughtSignature".to_owned(), Value::String(signature));
            }
        }
        Value::Object(part)
    }
}

/// The reader of one answer.
struct GeminiTurn {
    /// The parts of the reply, in order.
    parts: Vec<NativePart>,
    /// The bytes the stored turn would hold.
    native_bytes: usize,
    /// The calls of the reply, held until the stream ends cleanly after a normal stop.
    tools: ToolAssembler,
    /// The token totals last reported to the consumer.
    usage: (u32, u32),
    /// The finish reason, once one arrived.
    finish: Option<Finish>,
    /// An error happened or the consumer stopped the turn.
    halted: bool,
}

impl GeminiTurn {
    fn new() -> Self {
        Self {
            parts: Vec::new(),
            native_bytes: 0,
            tools: ToolAssembler::new(),
            usage: (0, 0),
            finish: None,
            halted: false,
        }
    }
}

impl fmt::Debug for GeminiTurn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeminiTurn")
            .field("parts", &self.parts.len())
            .field("native", &Size(self.native_bytes))
            .field("calls", &(self.tools.open_count() + self.tools.ready_count()))
            .field("usage", &self.usage)
            .field("finished", &self.finish.is_some())
            .finish()
    }
}

impl WireTurn for GeminiTurn {
    fn frame(&mut self, frame: Frame<'_>, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
        if self.halted {
            return Ok(Flow::Stop);
        }
        let result = match frame {
            Frame::Event(event) => self.event(&event, out),
            Frame::Line(_) => Err(protocol(PHRASE_FRAME)),
        };
        if !matches!(result, Ok(Flow::Continue)) {
            self.halted = true;
        }
        result
    }

    fn finish(
        self: Box<Self>,
        end: StreamEnd,
        out: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<Completion, AiError> {
        let GeminiTurn { parts, tools, usage, finish, halted, .. } = *self;
        match end {
            StreamEnd::Stopped => return Err(AiError::Cancelled),
            StreamEnd::Truncated { .. } => return Err(protocol(PHRASE_TRUNCATED)),
            StreamEnd::Clean => {}
        }
        if halted {
            return Err(protocol(PHRASE_FAILED_EARLIER));
        }
        let finish = finish.ok_or_else(|| protocol(PHRASE_NO_FINISH))?;
        // The calls leave here and nowhere else: after a clean end and a normal stop.
        let calls = if tools.open_count() + tools.ready_count() == 0 {
            Vec::new()
        } else {
            tools.finish(end, finish == Finish::Stop)?
        };
        let released = !calls.is_empty();
        for call in calls {
            if out(StreamEvent::ToolCall(call)) == Flow::Stop {
                return Err(AiError::Cancelled);
            }
        }
        let stop_reason = if released {
            StopReason::ToolUse
        } else {
            match finish {
                Finish::Stop => StopReason::EndTurn,
                Finish::MaxTokens => StopReason::MaxTokens,
                Finish::Other => StopReason::Other,
            }
        };
        let native = (!parts.is_empty()).then(|| Opaque {
            wire: Wire::Gemini.tag().to_owned(),
            json: json!({
                "role": "model",
                "parts": parts.into_iter().map(NativePart::into_value).collect::<Vec<Value>>(),
            }),
        });
        Ok(Completion { stop_reason, usage: Usage { input_tokens: usage.0, output_tokens: usage.1 }, native })
    }
}

impl GeminiTurn {
    /// One event: the parts of the first candidate, then the token totals, then the finish reason.
    fn event(&mut self, event: &SseEvent<'_>, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
        if event.data().trim().is_empty() || event.is_done() {
            return Ok(Flow::Continue);
        }
        let Value::Object(body) =
            serde_json::from_str::<Value>(event.data()).map_err(|_| protocol(PHRASE_UNREADABLE))?
        else {
            return Err(protocol(PHRASE_UNREADABLE));
        };
        if body.get("error").is_some_and(|error| !error.is_null()) {
            return Err(protocol(PHRASE_ERROR_EVENT));
        }
        let candidate = match body.get("candidates") {
            None | Some(Value::Null) => None,
            Some(Value::Array(candidates)) => match candidates.first() {
                None => None,
                Some(Value::Object(candidate)) => Some(candidate),
                Some(_) => return Err(protocol(PHRASE_UNREADABLE)),
            },
            Some(_) => return Err(protocol(PHRASE_UNREADABLE)),
        };
        if let Some(candidate) = candidate {
            let parts = match candidate.get("content") {
                None | Some(Value::Null) => None,
                Some(Value::Object(content)) => content.get("parts"),
                Some(_) => return Err(protocol(PHRASE_UNREADABLE)),
            };
            match parts {
                None | Some(Value::Null) => {}
                Some(Value::Array(parts)) => {
                    if self.absorb(parts, out)? == Flow::Stop {
                        return Ok(Flow::Stop);
                    }
                }
                Some(_) => return Err(protocol(PHRASE_UNREADABLE)),
            }
        }
        if let Some(usage) = totals(&body)
            && usage != self.usage
            && usage != (0, 0)
        {
            self.usage = usage;
            let event = StreamEvent::Usage(Usage { input_tokens: usage.0, output_tokens: usage.1 });
            if out(event) == Flow::Stop {
                return Ok(Flow::Stop);
            }
        }
        if let Some(reason) = candidate.and_then(|candidate| candidate.get("finishReason")).and_then(Value::as_str)
            && !reason.is_empty()
            && reason != "FINISH_REASON_UNSPECIFIED"
        {
            self.finish = Some(match reason {
                "STOP" => Finish::Stop,
                "MAX_TOKENS" => Finish::MaxTokens,
                _ => Finish::Other,
            });
        }
        Ok(Flow::Continue)
    }

    /// The parts of one event, in order. A text part is delivered at once; a call goes to the assembler.
    fn absorb(&mut self, parts: &[Value], out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
        for part in parts {
            let Value::Object(part) = part else {
                return Err(protocol(PHRASE_PART));
            };
            let signature = match part.get("thoughtSignature") {
                None | Some(Value::Null) => None,
                Some(Value::String(signature)) if signature.is_empty() => None,
                Some(Value::String(signature)) if signature_is_valid(signature) => Some(signature.clone()),
                Some(_) => return Err(protocol(PHRASE_PART)),
            };
            let text = match part.get("text") {
                None | Some(Value::Null) => None,
                Some(Value::String(text)) => Some(text.as_str()),
                Some(_) => return Err(protocol(PHRASE_PART)),
            };
            let call = part.get("functionCall").filter(|call| !call.is_null());
            match (text, call) {
                (Some(_), Some(_)) => return Err(protocol(PHRASE_PART)),
                (Some(text), None) => {
                    let thought = part.get("thought") == Some(&Value::Bool(true));
                    self.keep_text(text, thought, signature)?;
                    if !text.is_empty() {
                        let event = if thought {
                            StreamEvent::ReasoningDelta(text.to_owned())
                        } else {
                            StreamEvent::TextDelta(text.to_owned())
                        };
                        if out(event) == Flow::Stop {
                            return Ok(Flow::Stop);
                        }
                    }
                }
                (None, Some(call)) => self.keep_call(call, signature)?,
                (None, None) => {
                    if let Some(signature) = signature {
                        self.account(signature.len())?;
                        self.push(NativePart::Signature(signature))?;
                    }
                }
            }
        }
        Ok(Flow::Continue)
    }

    /// Stores a text part. A plain part joins the plain part before it; a part that carries a signature never joins.
    fn keep_text(&mut self, text: &str, thought: bool, signature: Option<String>) -> Result<(), AiError> {
        if text.is_empty() && signature.is_none() {
            return Ok(());
        }
        self.account(text.len() + signature.as_ref().map_or(0, String::len))?;
        if signature.is_none()
            && let Some(NativePart::Text { text: before, thought: was, signature: None }) = self.parts.last_mut()
            && *was == thought
        {
            before.push_str(text);
            return Ok(());
        }
        self.push(NativePart::Text { text: text.to_owned(), thought, signature })
    }

    /// Hands a call to the assembler and stores its part.
    fn keep_call(&mut self, call: &Value, signature: Option<String>) -> Result<(), AiError> {
        let Value::Object(call) = call else {
            return Err(protocol(PHRASE_PART));
        };
        let Some(Value::String(name)) = call.get("name") else {
            return Err(protocol(PHRASE_PART));
        };
        let id = call.get("id").and_then(Value::as_str).filter(|id| !id.is_empty());
        let args = match call.get("args") {
            None | Some(Value::Null) => json!({}),
            Some(args) => args.clone(),
        };
        let size = name.len() + id.map_or(0, str::len) + json_size(&args).0 + signature.as_ref().map_or(0, String::len);
        self.tools.whole(id, name, args.clone())?;
        self.account(size)?;
        self.push(NativePart::Call { name: name.clone(), args, id: id.map(str::to_owned), signature })
    }

    /// Counts bytes of the stored turn against its bound.
    fn account(&mut self, bytes: usize) -> Result<(), AiError> {
        self.native_bytes = self.native_bytes.saturating_add(bytes);
        if self.native_bytes > MAX_NATIVE_BYTES {
            return Err(protocol(PHRASE_PART));
        }
        Ok(())
    }

    /// Appends a part, within the bound on parts.
    fn push(&mut self, part: NativePart) -> Result<(), AiError> {
        if self.parts.len() >= MAX_PARTS {
            return Err(protocol(PHRASE_PART));
        }
        self.parts.push(part);
        Ok(())
    }
}

/// The token totals of an event: the prompt and tool-use prompt tokens in, the candidate and thought tokens out, each
/// saturating into `u32`. `None` when the event has no `usageMetadata` object.
fn totals(body: &Map<String, Value>) -> Option<(u32, u32)> {
    let meta = body.get("usageMetadata")?.as_object()?;
    let count = |key: &str| meta.get(key).and_then(Value::as_u64).unwrap_or(0);
    let clamp = |total: u64| u32::try_from(total).unwrap_or(u32::MAX);
    Some((
        clamp(count("promptTokenCount").saturating_add(count("toolUsePromptTokenCount"))),
        clamp(count("candidatesTokenCount").saturating_add(count("thoughtsTokenCount"))),
    ))
}
