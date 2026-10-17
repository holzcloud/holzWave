//! The OpenAI Responses dialect.
//!
//! This is the one OpenAI path that keeps reasoning and tool calling intact: the Chat Completions path does not support
//! tools together with reasoning on the current model line. The adapter follows the provider's guides on migrating to
//! the Responses API and on reasoning models, and it is deliberately stateless.
//!
//! - **Every request keeps nothing on the server.** The body has exactly these members: `model`, `instructions` (only
//!   with a system text), `input`, `store` (always the boolean false), `stream` (always true), `include` (only for a
//!   reasoning model family), `tools` (only when declared and supported) and `max_output_tokens` (only when set). There
//!   is no conversation, no previous-response id, no metadata, no user id, no cache key, no sampling parameter and no
//!   tool choice, and no built-in provider tool is ever declared. State is carried by sending the earlier output items
//!   back, never by an id the server keeps.
//! - **Reasoning survives tool turns by replay.** With storage off, a reasoning item only works when it is passed back
//!   with its encrypted content, together with the function-call items of the same turn. The items of a reply are kept
//!   verbatim as the provider-native turn (a JSON array) and sent back unchanged, but only when the stored turn is
//!   whole and was written by this dialect for this model. Anything else is written again from the plain parts of the
//!   message: text as assistant messages, calls as function-call items without ids, reasoning dropped. A switched
//!   model, a switched provider or a damaged chat file therefore degrades to a plain replay and never to a request the
//!   server rejects.
//! - **Calls and results pair one to one by call id.** A result needs an earlier call, a call needs its result before
//!   the next user message, and anything else is refused locally before a connection is made.
//! - **Terminal text leaves only as approved.** A context part is written as user-role input text from the exact
//!   envelope of the approval the request carries, or as one marker line made of labels and byte counts when this
//!   request holds no such approval. The text a note may have kept is never read, and the system text never carries
//!   context.
//! - **The answer is read as it arrives.** Events go from the decoder to the callback without buffering. A tool call is
//!   delivered once, complete and parsed, from [`WireTurn::finish`] only, after a clean `response.completed`.
//! - **No dialect text is kept for display except the model's own.** The turn prints counts, never text.
//!
//! The dialect needs no merging of adjacent messages (it has no alternation rule), and a result's error flag has no
//! member in it: the content of a failed result already says so and is sent unchanged.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;

use serde::Deserialize;
use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value, json};

use super::{Endpoint, Wire, WireAdapter, WireRequest, WireTurn};
use crate::ai::http::{Frame, Rejection};
use crate::ai::provider::{AiError, ChatRequest, Completion, Detail, Flow, MAX_DETAIL, StreamEvent};
use crate::ai::sse::{StreamEnd, ToolAssembler};
use crate::ai::types::{
    Capabilities, ContextNote, Locality, Message, Opaque, Part, Role, Size, StopReason, ToolSpec, Usage,
};

// ---- constants --------------------------------------------------------------------------------------------

/// The path of the endpoint below the base address of the profile (which ends in the version segment).
pub const PATH: &str = "/responses";

/// The value of `include` that asks for the encrypted reasoning, so a reasoning item can be sent back with storage off.
pub const INCLUDE_ENCRYPTED: &str = "reasoning.encrypted_content";

/// The most output items one reply may hold, and the most a stored turn may replay. Pinned by the test that sends one
/// finished item too many.
pub const MAX_OUTPUT_ITEMS: usize = 256;

/// The most bytes of finished-item event data one reply may hold: 2 MiB. The native turn is kept in the chat file and
/// sent back with every request, so it is bounded by what the events carried, counted without writing the items again.
/// Pinned by the test with three items of 700,000 bytes.
pub const MAX_NATIVE_BYTES: usize = 2_097_152;

/// The most tools one request may declare. A request is written once per turn and sent in full, so the bound keeps a
/// runaway tool list from growing every request. Pinned by the test that declares 128 and then 129 tools.
pub const MAX_TOOLS: usize = 128;

/// The longest provider error code that is shown, in bytes. A longer code, or one with a character outside `a-z`,
/// `0-9`, `_`, `.` and `-`, is left out of the error, because a code is the only provider text a stream error may
/// carry. Pinned by the failure test with codes of 64 and 65 characters and with hostile characters.
pub const MAX_CODE_BYTES: usize = 64;

/// The adapter of the Responses dialect. It has no field: everything it needs comes with the request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenAiResponsesAdapter;

impl OpenAiResponsesAdapter {
    /// The adapter.
    pub fn new() -> Self {
        Self
    }
}

// ---- errors -----------------------------------------------------------------------------------------------

/// An error of the answer: a fixed phrase and nothing from the stream.
fn protocol(phrase: &str) -> AiError {
    AiError::Protocol(Detail::new(phrase))
}

/// An error of the request: a fixed phrase and nothing from the request.
fn invalid(phrase: &str) -> AiError {
    AiError::InvalidRequest(Detail::new(phrase))
}

const PHRASE_NO_MESSAGES: &str = "the request has no messages";
const PHRASE_NO_INPUT: &str = "the request has nothing to send";
const PHRASE_WRONG_PART: &str = "a message holds a part that does not belong to its role";
const PHRASE_TOO_MANY_TOOLS: &str = "the request declares more tools than one request allows";
const PHRASE_CALL_ID: &str = "a tool call has no id";
const PHRASE_SAME_CALL: &str = "two tool calls share an id";
const PHRASE_ORPHAN: &str = "a tool result does not answer an earlier tool call";
const PHRASE_TWO_RESULTS: &str = "a tool call has more than one result";
const PHRASE_UNANSWERED: &str = "a tool call has no result before the next message";
const PHRASE_ERROR_EVENT: &str = "the provider reported an error while streaming";
const PHRASE_TOO_MANY_ITEMS: &str = "the response holds more output items than one reply allows";
const PHRASE_TOO_LARGE: &str = "the response is larger than one reply allows";
const PHRASE_REPEATED_ITEM: &str = "the response finishes one output item twice";
const PHRASE_QUOTA: &str = "the account has no quota left";
const PHRASE_SERVER: &str = "an internal error";
const PHRASE_STALE_REASONING: &str =
    "the saved reasoning of this chat is not usable; start a new chat or use another model";
const PHRASE_NO_MODEL: &str = "the model is not available to this key; choose another model";
const PHRASE_BAD_VALUE: &str = "a value or parameter of the request is not accepted";
const PHRASE_FRAME: &str = "the response is not an event stream";
const PHRASE_UNREADABLE: &str = "the response holds an event that cannot be read";
const PHRASE_MEMBER: &str = "the response holds an event without a member it needs";
const PHRASE_INDEX: &str = "the response holds an event with an item number that is not usable";
const PHRASE_UNKNOWN_CALL: &str = "the response sends arguments for a tool call that was never started";
const PHRASE_DISAGREE: &str = "the response lists other tool calls than the stream carried";
const PHRASE_NO_TERMINAL: &str = "the response ended without its final event";
const PHRASE_TRUNCATED: &str = "the response stream ended before the response was complete";
const PHRASE_FAILED_EARLIER: &str = "the response was refused earlier";

// ---- error codes ------------------------------------------------------------------------------------------

/// The error for a provider error code, or `None` for a code this version does not know. The class comes from the code
/// alone and every phrase is fixed: the provider's message is never copied, because a stream event has no endpoint to
/// mask a key with. Pinned row by row by the failure and rejection tests.
fn class_for_code(code: &str) -> Option<AiError> {
    Some(match code {
        "context_length_exceeded" => AiError::ContextLength,
        "rate_limit_exceeded" => AiError::RateLimited { retry_after: None },
        // A key that cannot pay is a problem with the credentials from the user's point of view.
        "insufficient_quota" => AiError::Auth(Detail::new(PHRASE_QUOTA)),
        "server_error" | "internal_error" => AiError::Server { status: 500, detail: Detail::new(PHRASE_SERVER) },
        "overloaded" | "server_is_overloaded" | "slow_down" | "service_unavailable" => AiError::Overloaded,
        "invalid_prompt" | "content_filter" | "content_policy_violation" => AiError::ContentFilter,
        "invalid_encrypted_content" => invalid(PHRASE_STALE_REASONING),
        "model_not_found" => invalid(PHRASE_NO_MODEL),
        "invalid_request_error"
        | "invalid_value"
        | "unsupported_parameter"
        | "unsupported_value"
        | "missing_required_parameter" => invalid(PHRASE_BAD_VALUE),
        _ => return None,
    })
}

/// Whether a code may be shown: at most [`MAX_CODE_BYTES`] bytes of lower-case letters, digits, `_`, `.` and `-`.
fn safe_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= MAX_CODE_BYTES
        && code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'))
}

/// The error of a `response.failed` or `error` event: the class of the code, else a protocol error that shows the code
/// only when it is safe to show.
fn event_error(code: Option<&str>) -> AiError {
    match code {
        Some(code) => class_for_code(code).unwrap_or_else(|| {
            if safe_code(code) {
                AiError::Protocol(Detail::new(&format!("{PHRASE_ERROR_EVENT} ({code})")))
            } else {
                protocol(PHRASE_ERROR_EVENT)
            }
        }),
        None => protocol(PHRASE_ERROR_EVENT),
    }
}

/// Text of an error body, made safe to show: every key of the endpoint is masked, control, direction and zero-width
/// characters are replaced, and the whole is within the limit of a detail.
fn shown_text(endpoint: &Endpoint, message: &str) -> Detail {
    let detail = endpoint.scrub(message);
    // A detail that was cut ends in an ellipsis after its limit, so it is cut once more to fit the limit as a whole.
    if detail.as_str().chars().count() > MAX_DETAIL {
        let kept: String = detail.as_str().chars().take(MAX_DETAIL - 1).collect();
        return Detail::new(&format!("{}\u{2026}", kept.trim_end()));
    }
    detail
}

/// The error of an answer that was not a success, read from the provider's error body. The status decides when the body
/// is not an error object, and for 401 and 403 it always decides, so no provider text is shown for a refused key. A
/// known code gives its class (the wait of a rate limit and the status of a server error come from the answer). The
/// two codes with their own phrases keep them. For a client error with another or no known code the provider's message
/// is shown, but only through [`Endpoint::scrub`], which masks the keys of the endpoint.
fn rejected(rejection: &Rejection, endpoint: &Endpoint) -> Option<AiError> {
    let status = rejection.status;
    if matches!(status, 401 | 403) {
        return None;
    }
    let body: Value = serde_json::from_slice(&rejection.body).ok()?;
    let error = body.get("error").filter(|error| error.is_object())?;
    let code = text_of(error, "code");
    let message = text_of(error, "message").filter(|text| !text.trim().is_empty());
    let client_error = matches!(status, 400 | 404 | 409 | 410 | 422);
    match code.and_then(class_for_code) {
        Some(AiError::InvalidRequest(fixed)) => {
            let own_phrase = matches!(code, Some("invalid_encrypted_content" | "model_not_found"));
            Some(match message {
                Some(message) if client_error && !own_phrase => AiError::InvalidRequest(shown_text(endpoint, message)),
                _ => AiError::InvalidRequest(fixed),
            })
        }
        Some(AiError::RateLimited { .. }) => Some(AiError::RateLimited { retry_after: rejection.retry_after }),
        Some(AiError::Server { detail, .. }) => {
            Some(AiError::Server { status: if (500..600).contains(&status) { status } else { 500 }, detail })
        }
        Some(known) => Some(known),
        None => message.filter(|_| client_error).map(|message| AiError::InvalidRequest(shown_text(endpoint, message))),
    }
}

// ---- the model family -------------------------------------------------------------------------------------

/// Whether `model` belongs to a family that returns encrypted reasoning when asked.
///
/// The id is lower-cased and one `vendor/` prefix is dropped (a gateway names models that way). An id that contains
/// `-chat` is a chat model of a reasoning line and does not reason. This is the only place a model family is named, and
/// it decides only whether `include` is sent, so an unknown model degrades to a plain replay and not to a rejection.
fn reasoning_model(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    let id = lower.split_once('/').map_or(lower.as_str(), |(_, rest)| rest);
    if id.contains("-chat") {
        return false;
    }
    ["o1", "o3", "o4", "gpt-5", "gpt-6"].iter().any(|prefix| id.starts_with(prefix))
}

// ---- the request ------------------------------------------------------------------------------------------

impl WireAdapter for OpenAiResponsesAdapter {
    fn wire(&self) -> Wire {
        Wire::OpenAiResponses
    }

    fn capabilities(&self, model: &str, locality: Locality) -> Capabilities {
        Capabilities { tools: true, reasoning: reasoning_model(model), ..Capabilities::conservative(locality) }
    }

    fn request(&self, req: &ChatRequest, caps: &Capabilities) -> Result<WireRequest, AiError> {
        if caps.tools && req.tools.len() > MAX_TOOLS {
            return Err(invalid(PHRASE_TOO_MANY_TOOLS));
        }
        let input = render_input(req)?;
        let mut body = Map::new();
        body.insert("model".to_owned(), Value::String(req.model.clone()));
        if !req.system.is_empty() {
            body.insert("instructions".to_owned(), Value::String(req.system.clone()));
        }
        body.insert("input".to_owned(), Value::Array(input));
        // Nothing is kept on the server: not the prompt, not the terminal context, not the reply.
        body.insert("store".to_owned(), Value::Bool(false));
        body.insert("stream".to_owned(), Value::Bool(true));
        if caps.reasoning {
            body.insert("include".to_owned(), json!([INCLUDE_ENCRYPTED]));
        }
        if caps.tools && !req.tools.is_empty() {
            body.insert("tools".to_owned(), Value::Array(req.tools.iter().map(tool_declaration).collect()));
        }
        if let Some(limit) = req.max_output_tokens {
            body.insert("max_output_tokens".to_owned(), Value::from(limit));
        }
        let body = serde_json::to_vec(&Value::Object(body))
            .map_err(|_| invalid("the request could not be written as JSON"))?;
        Ok(WireRequest { path: PATH.to_owned(), headers: Vec::new(), body })
    }

    fn turn(&self) -> Box<dyn WireTurn> {
        Box::new(ResponsesTurn::new())
    }

    fn rejection(&self, rejection: &Rejection, endpoint: &Endpoint) -> Option<AiError> {
        rejected(rejection, endpoint)
    }
}

/// One declared tool: a top-level function with its schema unchanged. The definition is not strict, because the schemas
/// of the tool catalog list optional properties outside `required`, which strict mode refuses; the typed argument
/// structs of the tools are the real validation.
fn tool_declaration(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.input_schema,
        "strict": false,
    })
}

/// The `input` array: the messages of the request in order, each written by the rules of its role, and every call
/// paired with its result.
fn render_input(req: &ChatRequest) -> Result<Vec<Value>, AiError> {
    check_parts(req)?;
    let mut input = Vec::with_capacity(req.messages.len() + 1);
    let mut pairing = Pairing::default();
    for message in &req.messages {
        match message.role {
            Role::User => {
                let content = user_content(req, message);
                if !content.is_empty() {
                    pairing.expect_none_open()?;
                    input.push(json!({ "role": "user", "content": content }));
                }
            }
            Role::Assistant => {
                let first = input.len();
                match usable_turn(message, &req.model) {
                    Some(items) => input.extend(items.iter().cloned()),
                    None => plain_assistant(message, &mut input),
                }
                for item in &input[first..] {
                    if item_type(item) == Some("function_call") {
                        pairing.call(text_of(item, "call_id").unwrap_or(""))?;
                    }
                }
            }
            Role::Tool => {
                for part in &message.parts {
                    if let Part::ToolResult(result) = part {
                        pairing.result(&result.call_id)?;
                        input.push(json!({
                            "type": "function_call_output",
                            "call_id": result.call_id,
                            "output": result.content,
                        }));
                    }
                }
            }
        }
    }
    pairing.expect_none_open()?;
    if input.is_empty() {
        return Err(invalid(PHRASE_NO_INPUT));
    }
    Ok(input)
}

/// Checks that every part belongs to the role of its message: a user holds text and context, the model holds text and
/// calls, a tool holds results. A violation is an error with a fixed phrase that holds no text of the request.
fn check_parts(req: &ChatRequest) -> Result<(), AiError> {
    if req.messages.is_empty() {
        return Err(invalid(PHRASE_NO_MESSAGES));
    }
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

/// Pairs calls and results over the rendered items, in order, whether the calls come from a replayed turn or from the
/// parts of a message. A result needs an earlier call that has no result yet, and no call may be left without a result
/// when a user message follows or the input ends. The dialect refuses a half pair, so the request is refused here,
/// before any connection, with a phrase that names the kind of problem and nothing of the request.
#[derive(Default)]
struct Pairing {
    /// Every call id seen so far.
    seen: HashSet<String>,
    /// The call ids that have no result yet.
    open: HashSet<String>,
}

impl Pairing {
    fn call(&mut self, id: &str) -> Result<(), AiError> {
        if id.is_empty() {
            return Err(invalid(PHRASE_CALL_ID));
        }
        if !self.seen.insert(id.to_owned()) {
            return Err(invalid(PHRASE_SAME_CALL));
        }
        self.open.insert(id.to_owned());
        Ok(())
    }

    fn result(&mut self, id: &str) -> Result<(), AiError> {
        if self.open.remove(id) {
            Ok(())
        } else if self.seen.contains(id) {
            Err(invalid(PHRASE_TWO_RESULTS))
        } else {
            Err(invalid(PHRASE_ORPHAN))
        }
    }

    fn expect_none_open(&self) -> Result<(), AiError> {
        if self.open.is_empty() { Ok(()) } else { Err(invalid(PHRASE_UNANSWERED)) }
    }
}

/// The content of a user message: one `input_text` part per non-empty text and per context note, in part order.
fn user_content(req: &ChatRequest, message: &Message) -> Vec<Value> {
    let mut content = Vec::with_capacity(message.parts.len());
    for part in &message.parts {
        match part {
            Part::Text { text } if !text.is_empty() => content.push(input_text(text)),
            Part::Context(note) => content.push(input_text(&context_text(req, note))),
            _ => {}
        }
    }
    content
}

fn input_text(text: &str) -> Value {
    json!({ "type": "input_text", "text": text })
}

/// What a context note becomes in a request. With an approval in the request: the envelope the user saw, byte for byte.
/// Without one the note is history, and one marker line stands in for it: the label and the size of each item. The
/// text a note may have kept is never read.
fn context_text(req: &ChatRequest, note: &ContextNote) -> String {
    if let Some(approved) = req.approved_for(&note.hash) {
        return approved.envelope().to_owned();
    }
    super::marker_line(note)
}

/// The stored turn of an assistant message, when it may be replayed.
///
/// The stored turn is the array of output items of one reply, and it comes from disk: it may be stale, from another
/// model or damaged, and the server rejects a reasoning item without its encrypted content. It is replayed only when
/// all of this holds: the message was written by the model of this request; it was stored by this dialect; it is a
/// non-empty array of at most [`MAX_OUTPUT_ITEMS`] objects; every object is an assistant message, a reasoning item with
/// a non-empty encrypted content, or a function call with a non-empty call id. Any other stored turn gives `None`, and
/// the message is then written from its parts: a damaged file degrades, it never stops the chat.
fn usable_turn<'a>(message: &'a Message, model: &str) -> Option<&'a Vec<Value>> {
    if message.model.as_deref() != Some(model) {
        return None;
    }
    let items = message.native_for(Wire::OpenAiResponses.tag())?.as_array()?;
    if items.is_empty() || items.len() > MAX_OUTPUT_ITEMS {
        return None;
    }
    let whole = items.iter().all(|item| match item_type(item) {
        Some("reasoning") => text_of(item, "encrypted_content").is_some_and(|text| !text.is_empty()),
        Some("message") => text_of(item, "role") == Some("assistant"),
        Some("function_call") => text_of(item, "call_id").is_some_and(|id| !id.is_empty()),
        _ => false,
    });
    whole.then_some(items)
}

/// An assistant message written from its parts: text as assistant messages, calls as function-call items without ids.
fn plain_assistant(message: &Message, input: &mut Vec<Value>) {
    for part in &message.parts {
        match part {
            Part::Text { text } if !text.is_empty() => input.push(json!({ "role": "assistant", "content": text })),
            Part::ToolCall(call) => input.push(json!({
                "type": "function_call",
                "call_id": call.id,
                "name": call.name,
                "arguments": call.args.to_string(),
            })),
            _ => {}
        }
    }
}

// ---- the answer: lenient members ---------------------------------------------------------------------------
//
// Only the members the adapter reads are named. A member of another JSON type than expected reads as absent, so the
// handler of a known event decides whether that is an error and an unknown event is ignored whatever it holds.

/// A string member, or `None` when the member is `null` or of another type. A string without escapes is borrowed.
fn lenient_str<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Cow<'de, str>>, D::Error> {
    struct Reader;

    impl<'de> Visitor<'de> for Reader {
        type Value = Option<Cow<'de, str>>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("any JSON value")
        }

        fn visit_borrowed_str<E>(self, text: &'de str) -> Result<Self::Value, E> {
            Ok(Some(Cow::Borrowed(text)))
        }

        fn visit_str<E>(self, text: &str) -> Result<Self::Value, E> {
            Ok(Some(Cow::Owned(text.to_owned())))
        }

        fn visit_string<E>(self, text: String) -> Result<Self::Value, E> {
            Ok(Some(Cow::Owned(text)))
        }

        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(None)
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(None)
        }
    }

    deserializer.deserialize_any(Reader)
}

/// A whole number member, or `None` when the member is `null`, negative, fractional or of another type.
fn lenient_index<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    struct Reader;

    impl<'de> Visitor<'de> for Reader {
        type Value = Option<u64>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("any JSON value")
        }

        fn visit_u64<E>(self, number: u64) -> Result<Self::Value, E> {
            Ok(Some(number))
        }

        fn visit_i64<E>(self, number: i64) -> Result<Self::Value, E> {
            Ok(u64::try_from(number).ok())
        }

        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(None)
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(None)
        }
    }

    deserializer.deserialize_any(Reader)
}

/// The members of one event that the adapter reads. Everything else is skipped without being kept.
#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(default, borrow, deserialize_with = "lenient_str")]
    r#type: Option<Cow<'a, str>>,
    #[serde(default, deserialize_with = "lenient_index")]
    output_index: Option<u64>,
    #[serde(default, borrow, deserialize_with = "lenient_str")]
    delta: Option<Cow<'a, str>>,
    #[serde(default, borrow, deserialize_with = "lenient_str")]
    arguments: Option<Cow<'a, str>>,
    #[serde(default, borrow, deserialize_with = "lenient_str")]
    code: Option<Cow<'a, str>>,
    #[serde(default)]
    item: Option<Value>,
    #[serde(default)]
    response: Option<Value>,
}

/// The kinds of event the adapter acts on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Refusal,
    Reasoning,
    ItemAdded,
    ItemDone,
    ArgsDelta,
    ArgsDone,
    Completed,
    Incomplete,
    Failed,
    Error,
    Other,
}

impl Kind {
    fn of(name: &str) -> Kind {
        match name {
            "response.output_text.delta" => Kind::Text,
            "response.refusal.delta" => Kind::Refusal,
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => Kind::Reasoning,
            "response.output_item.added" => Kind::ItemAdded,
            "response.output_item.done" => Kind::ItemDone,
            "response.function_call_arguments.delta" => Kind::ArgsDelta,
            "response.function_call_arguments.done" => Kind::ArgsDone,
            "response.completed" => Kind::Completed,
            "response.incomplete" => Kind::Incomplete,
            "response.failed" => Kind::Failed,
            "error" => Kind::Error,
            _ => Kind::Other,
        }
    }
}

// ---- the answer: the turn ---------------------------------------------------------------------------------

/// Where the argument text of a call came from, so the first source that has text wins.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fed {
    /// No argument text yet.
    No,
    /// Fragments of the argument deltas.
    Deltas,
    /// The whole text of the arguments-done event or of the finished item.
    Whole,
}

/// A call the assembler accepted, by the output index of its item.
struct Slot {
    index: u32,
    fed: Fed,
}

/// Why a response ended without completing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// The output limit was reached.
    MaxTokens,
    /// The provider's content filter cut the reply.
    ContentFilter,
    /// Any other reason, or none.
    Other,
}

/// How the response ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Terminal {
    Completed,
    Incomplete(Reason),
}

/// The reader of one answer. `Debug` shows counts and flags, never text.
struct ResponsesTurn {
    tools: ToolAssembler,
    slots: Vec<Slot>,
    /// The finished output items with their output index, in arrival order.
    items: Vec<(u32, Value)>,
    /// The bytes of event data the finished items came in.
    native_bytes: usize,
    usage: Usage,
    refused: bool,
    terminal: Option<Terminal>,
    failed: bool,
    stopped: bool,
}

impl ResponsesTurn {
    fn new() -> Self {
        Self {
            tools: ToolAssembler::new(),
            slots: Vec::new(),
            items: Vec::new(),
            native_bytes: 0,
            usage: Usage::default(),
            refused: false,
            terminal: None,
            failed: false,
            stopped: false,
        }
    }

    /// The output index of an event as a slot number.
    fn index(env: &Envelope<'_>) -> Result<u32, AiError> {
        let index = env.output_index.ok_or_else(|| protocol(PHRASE_MEMBER))?;
        u32::try_from(index).map_err(|_| protocol(PHRASE_INDEX))
    }

    /// Hands the id, the name and a piece of argument text of a call to the assembler and remembers the slot.
    fn feed(&mut self, slot: u32, id: Option<&str>, name: Option<&str>, args: &str) -> Result<(), AiError> {
        self.tools.fragment(slot, id, name, args).map_err(AiError::from)?;
        if !self.slots.iter().any(|held| held.index == slot) {
            self.slots.push(Slot { index: slot, fed: Fed::No });
        }
        Ok(())
    }

    fn step(&mut self, frame: Frame<'_>, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
        let Frame::Event(event) = frame else { return Err(protocol(PHRASE_FRAME)) };
        if self.terminal.is_some() || event.is_done() {
            return Ok(Flow::Continue);
        }
        let data = event.data();
        if !data.trim_start().starts_with('{') {
            return Err(protocol(PHRASE_UNREADABLE));
        }
        let mut env: Envelope<'_> = serde_json::from_str(data).map_err(|_| protocol(PHRASE_UNREADABLE))?;
        let name = env.r#type.as_deref().filter(|name| !name.is_empty()).unwrap_or_else(|| event.event());
        let kind = Kind::of(name);
        match kind {
            Kind::Text | Kind::Refusal | Kind::Reasoning => {
                let delta = env.delta.take().ok_or_else(|| protocol(PHRASE_MEMBER))?;
                if kind == Kind::Refusal {
                    self.refused = true;
                }
                if delta.is_empty() {
                    return Ok(Flow::Continue);
                }
                let text = delta.into_owned();
                Ok(out(if kind == Kind::Reasoning {
                    StreamEvent::ReasoningDelta(text)
                } else {
                    StreamEvent::TextDelta(text)
                }))
            }
            Kind::ItemAdded => {
                let item = env.item.take().filter(Value::is_object).ok_or_else(|| protocol(PHRASE_MEMBER))?;
                if item_type(&item) == Some("function_call") {
                    let slot = Self::index(&env)?;
                    self.feed(slot, text_of(&item, "call_id"), text_of(&item, "name"), "")?;
                }
                Ok(Flow::Continue)
            }
            Kind::ArgsDelta | Kind::ArgsDone => {
                let slot = Self::index(&env)?;
                let text = if kind == Kind::ArgsDelta { env.delta.take() } else { env.arguments.take() };
                let text = text.ok_or_else(|| protocol(PHRASE_MEMBER))?;
                self.feed_arguments(slot, &text, if kind == Kind::ArgsDelta { Fed::Deltas } else { Fed::Whole })?;
                Ok(Flow::Continue)
            }
            Kind::ItemDone => {
                let slot = Self::index(&env)?;
                let item = env.item.take().filter(Value::is_object).ok_or_else(|| protocol(PHRASE_MEMBER))?;
                // The kept items are bounded in number and in bytes of event data, and an index finishes once.
                if self.items.len() >= MAX_OUTPUT_ITEMS {
                    return Err(protocol(PHRASE_TOO_MANY_ITEMS));
                }
                self.native_bytes = self.native_bytes.saturating_add(data.len());
                if self.native_bytes > MAX_NATIVE_BYTES {
                    return Err(protocol(PHRASE_TOO_LARGE));
                }
                if self.items.iter().any(|(held, _)| *held == slot) {
                    return Err(protocol(PHRASE_REPEATED_ITEM));
                }
                if item_type(&item) == Some("function_call") {
                    let args = text_of(&item, "arguments").unwrap_or("");
                    let taken =
                        self.slots.iter().find(|held| held.index == slot).is_some_and(|held| held.fed != Fed::No);
                    let args = if taken { "" } else { args };
                    self.feed(slot, text_of(&item, "call_id"), text_of(&item, "name"), args)?;
                    if !args.is_empty() {
                        self.mark(slot, Fed::Whole);
                    }
                    self.tools.complete(slot).map_err(AiError::from)?;
                }
                self.items.push((slot, item));
                Ok(Flow::Continue)
            }
            Kind::Completed => {
                let response = env.response.take().filter(Value::is_object).ok_or_else(|| protocol(PHRASE_MEMBER))?;
                if let Some(output) = response.get("output").and_then(Value::as_array) {
                    let listed = output.iter().filter(|item| item_type(item) == Some("function_call")).count();
                    if listed != self.tools.ready_count() {
                        return Err(protocol(PHRASE_DISAGREE));
                    }
                }
                self.terminal = Some(Terminal::Completed);
                Ok(self.report_usage(&response, out))
            }
            Kind::Incomplete => {
                let response = env.response.take().filter(Value::is_object).ok_or_else(|| protocol(PHRASE_MEMBER))?;
                let reason = match response.get("incomplete_details").and_then(|details| text_of(details, "reason")) {
                    Some("max_output_tokens") => Reason::MaxTokens,
                    Some("content_filter") => Reason::ContentFilter,
                    _ => Reason::Other,
                };
                self.terminal = Some(Terminal::Incomplete(reason));
                Ok(self.report_usage(&response, out))
            }
            Kind::Failed => {
                let code = env
                    .response
                    .as_ref()
                    .and_then(|response| response.get("error"))
                    .and_then(|error| text_of(error, "code"));
                Err(event_error(code))
            }
            Kind::Error => Err(event_error(env.code.as_deref())),
            Kind::Other => Ok(Flow::Continue),
        }
    }

    /// Feeds argument text for a slot that was started, unless an earlier source already gave the text.
    fn feed_arguments(&mut self, slot: u32, text: &str, source: Fed) -> Result<(), AiError> {
        let held =
            self.slots.iter_mut().find(|held| held.index == slot).ok_or_else(|| protocol(PHRASE_UNKNOWN_CALL))?;
        let usable = match source {
            Fed::Deltas => held.fed != Fed::Whole,
            _ => held.fed == Fed::No,
        };
        if usable && !text.is_empty() {
            held.fed = source;
            self.tools.fragment(slot, None, None, text).map_err(AiError::from)?;
        }
        Ok(())
    }

    fn mark(&mut self, slot: u32, source: Fed) {
        if let Some(held) = self.slots.iter_mut().find(|held| held.index == slot) {
            held.fed = source;
        }
    }

    /// Reports the token totals of the response, when it carries some.
    fn report_usage(&mut self, response: &Value, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Flow {
        let Some(usage) = response.get("usage").filter(|usage| usage.is_object()) else { return Flow::Continue };
        self.usage = Usage { input_tokens: count(usage, "input_tokens"), output_tokens: count(usage, "output_tokens") };
        out(StreamEvent::Usage(self.usage))
    }
}

/// The type of an item.
fn item_type(item: &Value) -> Option<&str> {
    text_of(item, "type")
}

/// A string member of an object.
fn text_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// A token count: a whole number as it is, a non-negative fraction cut, anything else (also a negative number) zero,
/// and a count that does not fit is the largest count.
fn count(usage: &Value, key: &str) -> u32 {
    let Some(number) = usage.get(key).and_then(Value::as_number) else { return 0 };
    let whole = number
        .as_u64()
        .or_else(|| number.as_f64().filter(|real| real.is_finite() && *real >= 0.0).map(|real| real as u64));
    whole.map_or(0, |whole| u32::try_from(whole).unwrap_or(u32::MAX))
}

impl fmt::Debug for ResponsesTurn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponsesTurn")
            .field("tools", &self.tools)
            .field("calls", &self.slots.len())
            .field("items", &self.items.len())
            .field("native", &Size(self.native_bytes))
            .field("usage", &self.usage)
            .field("refused", &self.refused)
            .field("completed", &self.terminal.is_some())
            .field("failed", &self.failed)
            .finish()
    }
}

impl WireTurn for ResponsesTurn {
    fn frame(&mut self, frame: Frame<'_>, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
        if self.failed || self.stopped {
            return Ok(Flow::Stop);
        }
        let result = self.step(frame, out);
        match result {
            Err(_) => self.failed = true,
            Ok(Flow::Stop) => self.stopped = true,
            Ok(Flow::Continue) => {}
        }
        result
    }

    fn finish(
        self: Box<Self>,
        end: StreamEnd,
        out: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<Completion, AiError> {
        let mut turn = *self;
        if end == StreamEnd::Stopped {
            return Err(AiError::Cancelled);
        }
        if turn.failed {
            return Err(protocol(PHRASE_FAILED_EARLIER));
        }
        let Some(terminal) = turn.terminal else { return Err(protocol(PHRASE_NO_TERMINAL)) };
        if !end.is_clean() {
            return Err(protocol(PHRASE_TRUNCATED));
        }
        let reason = match terminal {
            Terminal::Completed => None,
            Terminal::Incomplete(reason) => Some(reason),
        };
        if reason.is_some() && turn.tools.open_count() + turn.tools.ready_count() > 0 {
            // The reply was cut while a call was pending: the call may be half of what the model meant, so it is dropped
            // and the turn fails.
            return Err(match turn.tools.finish(end, false) {
                Err(dropped) => AiError::from(dropped),
                Ok(_) => protocol(PHRASE_TRUNCATED),
            });
        }
        // The calls leave only here, after a clean end and the final event; an open call or a fault is an error.
        let calls = turn.tools.finish(end, true).map_err(AiError::from)?;
        let stop_reason = match reason {
            Some(Reason::ContentFilter) => return Err(AiError::ContentFilter),
            Some(Reason::MaxTokens) => StopReason::MaxTokens,
            Some(Reason::Other) => StopReason::Other,
            None if !calls.is_empty() => StopReason::ToolUse,
            None if turn.refused => StopReason::Refusal,
            None => StopReason::EndTurn,
        };
        for call in calls {
            if out(StreamEvent::ToolCall(call)) == Flow::Stop {
                return Err(AiError::Cancelled);
            }
        }
        if reason.is_some() {
            // An incomplete reply is not replayed: its last item is cut.
            return Ok(Completion { stop_reason, usage: turn.usage, native: None });
        }
        turn.items.sort_by_key(|(index, _)| *index);
        let native = (!turn.items.is_empty()).then(|| Opaque {
            wire: Wire::OpenAiResponses.tag().to_owned(),
            json: Value::Array(turn.items.into_iter().map(|(_, item)| item).collect()),
        });
        Ok(Completion { stop_reason, usage: turn.usage, native })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::sse::SseDecoder;

    /// Feeds `stream` to a fresh turn through the decoder, the way the shared driver does it.
    fn fed(stream: &str) -> ResponsesTurn {
        let mut turn = ResponsesTurn::new();
        let mut decoder = SseDecoder::new();
        let mut sink = |_: StreamEvent| Flow::Continue;
        decoder
            .push(stream.as_bytes(), &mut |event| turn.frame(Frame::Event(event), &mut sink).unwrap_or(Flow::Stop))
            .expect("a framed stream");
        turn
    }

    #[test]
    fn the_turn_debug_shows_counts_never_text() {
        let stream = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"TEXT-CANARY-7c1\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",",
            "\"encrypted_content\":\"ENC-CANARY-7c1\",\"summary\":[]}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",",
            "\"call_id\":\"ID-CANARY-7c1\",\"name\":\"NAME-CANARY-7c1\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,",
            "\"delta\":\"{\\\"path\\\":\\\"ARGS-CANARY-7c1\\\"\"}\n\n",
        );
        let turn = fed(stream);
        assert_eq!(turn.items.len(), 1, "the stream was read");
        assert_eq!(turn.slots.len(), 1, "the call was started");

        for shown in [format!("{turn:?}"), format!("{turn:#?}")] {
            assert!(!shown.contains("CANARY"), "{shown}");
            assert!(shown.contains("ResponsesTurn"), "{shown}");
            assert!(shown.contains("items: 1"), "{shown}");
            assert!(shown.contains("calls: 1"), "{shown}");
        }
        assert_eq!(format!("{:?}", OpenAiResponsesAdapter::new()), "OpenAiResponsesAdapter");
    }
}
