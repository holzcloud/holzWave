#![allow(dead_code)]
//! Generators for the neutral scenarios in the five dialects.
//!
//! Each generator copies the shape of the events in the adapters' golden fixtures; the fixtures themselves are never
//! read. The streams are built by hand like those fixtures: they are not recordings of a real server.

use holzwave_core::ai::wire::Wire;
use serde_json::{Value, json};

use crate::support::{ARG_CANARY, CANARY_TEXT, Neutral, Scenario};

/// A whole stream with the offsets the rows cut at.
#[derive(Clone, Debug)]
pub struct Streamed {
    /// Every byte of the stream.
    pub bytes: Vec<u8>,
    /// The offset just after the event that ends the turn in its dialect.
    pub terminal_end: usize,
    /// The offset just after the event that carries the first text delta.
    pub first_delta_end: usize,
}

/// Writes events into a byte string and reports where each one ends.
struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Writer { bytes: Vec::new() }
    }

    /// One server-sent event, with a name when the dialect uses names. Returns the offset after its blank line.
    fn event(&mut self, name: Option<&str>, data: &Value) -> usize {
        if let Some(name) = name {
            self.bytes.extend_from_slice(format!("event: {name}\n").as_bytes());
        }
        self.bytes.extend_from_slice(format!("data: {data}\n\n").as_bytes());
        self.bytes.len()
    }

    /// The closing line of the Chat Completions dialect.
    fn done(&mut self) -> usize {
        self.bytes.extend_from_slice(b"data: [DONE]\n\n");
        self.bytes.len()
    }

    /// One JSON record on its own line.
    fn line(&mut self, data: &Value) -> usize {
        self.bytes.extend_from_slice(format!("{data}\n").as_bytes());
        self.bytes.len()
    }

    fn finish(self, terminal_end: usize, first_delta_end: usize) -> Streamed {
        Streamed { bytes: self.bytes, terminal_end, first_delta_end }
    }
}

/// A Chat Completions chunk with one choice.
fn chat_chunk(delta: Value, finish: Option<&str>) -> Value {
    json!({
        "id": "chatcmpl-matrix",
        "object": "chat.completion.chunk",
        "created": 1_760_000_000,
        "model": "test-model",
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
    })
}

/// The Chat Completions stream of a scenario.
pub fn chat_stream(scenario: Scenario) -> Streamed {
    let mut out = Writer::new();
    match scenario {
        Scenario::Text => {
            out.event(None, &chat_chunk(json!({ "role": "assistant", "content": "" }), None));
            let first = out.event(None, &chat_chunk(json!({ "content": Neutral::TEXT_PARTS[0] }), None));
            out.event(None, &chat_chunk(json!({ "content": Neutral::TEXT_PARTS[1] }), None));
            let terminal = out.event(None, &chat_chunk(json!({}), Some("stop")));
            out.event(
                None,
                &json!({
                    "id": "chatcmpl-matrix",
                    "object": "chat.completion.chunk",
                    "created": 1_760_000_000,
                    "model": "test-model",
                    "choices": [],
                    "usage": { "prompt_tokens": 11, "completion_tokens": 5, "total_tokens": 16 },
                }),
            );
            out.done();
            out.finish(terminal, first)
        }
        Scenario::Tool | Scenario::Signed => {
            out.event(None, &chat_chunk(json!({ "role": "assistant", "content": null }), None));
            if scenario == Scenario::Signed {
                out.event(None, &chat_chunk(json!({ "content": null, "reasoning_content": Neutral::REASON }), None));
            }
            let first = out.event(None, &chat_chunk(json!({ "content": Neutral::LEAD }), None));
            for (index, piece) in Neutral::ARG_FRAGMENTS.iter().enumerate() {
                let mut call = json!({ "index": 0, "function": { "arguments": piece } });
                if index == 0 {
                    call["id"] = json!(Neutral::CALL_ID);
                    call["type"] = json!("function");
                    call["function"]["name"] = json!(Neutral::CALL_NAME);
                }
                out.event(None, &chat_chunk(json!({ "tool_calls": [call] }), None));
            }
            let terminal = out.event(None, &chat_chunk(json!({}), Some("tool_calls")));
            out.event(
                None,
                &json!({
                    "id": "chatcmpl-matrix",
                    "object": "chat.completion.chunk",
                    "created": 1_760_000_000,
                    "model": "test-model",
                    "choices": [],
                    "usage": { "prompt_tokens": 20, "completion_tokens": 7, "total_tokens": 27 },
                }),
            );
            out.done();
            out.finish(terminal, first)
        }
    }
}

/// The Ollama stream of a scenario.
pub fn ollama_stream(scenario: Scenario) -> Streamed {
    let mut out = Writer::new();
    let record = |message: Value, done: bool| {
        json!({ "model": "test-model", "created_at": "2026-10-16T12:00:00.123456Z", "message": message, "done": done })
    };
    match scenario {
        Scenario::Text => {
            let first =
                out.line(&record(json!({ "role": "assistant", "content": Neutral::TEXT_PARTS[0] }), false));
            out.line(&record(json!({ "role": "assistant", "content": Neutral::TEXT_PARTS[1] }), false));
            let mut last = record(json!({ "role": "assistant", "content": "" }), true);
            last["done_reason"] = json!("stop");
            last["prompt_eval_count"] = json!(11);
            last["eval_count"] = json!(5);
            let terminal = out.line(&last);
            out.finish(terminal, first)
        }
        Scenario::Tool | Scenario::Signed => {
            if scenario == Scenario::Signed {
                out.line(&record(json!({ "role": "assistant", "content": "", "thinking": Neutral::REASON }), false));
            }
            let first = out.line(&record(json!({ "role": "assistant", "content": Neutral::LEAD }), false));
            out.line(&record(
                json!({ "role": "assistant", "content": "", "tool_calls": [{ "function": { "name": Neutral::CALL_NAME, "arguments": Neutral::call_args() } }] }),
                false,
            ));
            let mut last = record(json!({ "role": "assistant", "content": "" }), true);
            last["done_reason"] = json!("stop");
            last["prompt_eval_count"] = json!(20);
            last["eval_count"] = json!(7);
            let terminal = out.line(&last);
            out.finish(terminal, first)
        }
    }
}

/// The Anthropic stream of a scenario.
pub fn anthropic_stream(scenario: Scenario) -> Streamed {
    let mut out = Writer::new();
    let start = |input: u32| {
        json!({ "type": "message_start", "message": {
            "id": "msg_matrix", "type": "message", "role": "assistant", "model": "claude-test-1", "content": [],
            "stop_reason": null, "stop_sequence": null,
            "usage": { "input_tokens": input, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": 1 },
        } })
    };
    match scenario {
        Scenario::Text => {
            out.event(Some("message_start"), &start(11));
            out.event(
                Some("content_block_start"),
                &json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
            );
            out.event(Some("ping"), &json!({ "type": "ping" }));
            let first = out.event(
                Some("content_block_delta"),
                &json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": Neutral::TEXT_PARTS[0] } }),
            );
            out.event(
                Some("content_block_delta"),
                &json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": Neutral::TEXT_PARTS[1] } }),
            );
            out.event(Some("content_block_stop"), &json!({ "type": "content_block_stop", "index": 0 }));
            out.event(
                Some("message_delta"),
                &json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn", "stop_sequence": null }, "usage": { "output_tokens": 5 } }),
            );
            let terminal = out.event(Some("message_stop"), &json!({ "type": "message_stop" }));
            out.finish(terminal, first)
        }
        Scenario::Tool | Scenario::Signed => {
            out.event(Some("message_start"), &start(20));
            let mut index = 0;
            let block = |out: &mut Writer, index: u32, start: Value| {
                out.event(
                    Some("content_block_start"),
                    &json!({ "type": "content_block_start", "index": index, "content_block": start }),
                )
            };
            let delta = |out: &mut Writer, index: u32, delta: Value| {
                out.event(
                    Some("content_block_delta"),
                    &json!({ "type": "content_block_delta", "index": index, "delta": delta }),
                )
            };
            let stop = |out: &mut Writer, index: u32| {
                out.event(Some("content_block_stop"), &json!({ "type": "content_block_stop", "index": index }))
            };
            if scenario == Scenario::Signed {
                block(&mut out, index, json!({ "type": "thinking", "thinking": "", "signature": "" }));
                delta(&mut out, index, json!({ "type": "thinking_delta", "thinking": Neutral::REASON }));
                delta(&mut out, index, json!({ "type": "signature_delta", "signature": Neutral::SIG }));
                stop(&mut out, index);
                index += 1;
            }
            block(&mut out, index, json!({ "type": "text", "text": "" }));
            let first = delta(&mut out, index, json!({ "type": "text_delta", "text": Neutral::LEAD }));
            stop(&mut out, index);
            index += 1;
            block(&mut out, index, json!({ "type": "tool_use", "id": Neutral::CALL_ID, "name": Neutral::CALL_NAME, "input": {} }));
            for piece in Neutral::ARG_FRAGMENTS {
                delta(&mut out, index, json!({ "type": "input_json_delta", "partial_json": piece }));
            }
            stop(&mut out, index);
            out.event(
                Some("message_delta"),
                &json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use", "stop_sequence": null }, "usage": { "output_tokens": 7 } }),
            );
            let terminal = out.event(Some("message_stop"), &json!({ "type": "message_stop" }));
            out.finish(terminal, first)
        }
    }
}

/// The Responses stream of a scenario.
pub fn responses_stream(scenario: Scenario) -> Streamed {
    let mut out = Writer::new();
    let mut sequence = 0u32;
    let mut event = |out: &mut Writer, name: &str, mut data: Value| {
        data["type"] = json!(name);
        data["sequence_number"] = json!(sequence);
        sequence += 1;
        out.event(Some(name), &data)
    };
    let response = |status: &str, output: Value, usage: Option<Value>| {
        let mut response = json!({ "id": "resp_matrix", "object": "response", "status": status, "model": "gpt-test-1", "output": output });
        if let Some(usage) = usage {
            response["usage"] = usage;
        }
        response
    };
    match scenario {
        Scenario::Text => {
            let text = Neutral::TEXT;
            event(&mut out, "response.created", json!({ "response": response("in_progress", json!([]), None) }));
            event(&mut out, "response.in_progress", json!({ "response": response("in_progress", json!([]), None) }));
            event(
                &mut out,
                "response.output_item.added",
                json!({ "output_index": 0, "item": { "id": "msg_matrix", "type": "message", "status": "in_progress", "role": "assistant", "content": [] } }),
            );
            event(
                &mut out,
                "response.content_part.added",
                json!({ "item_id": "msg_matrix", "output_index": 0, "content_index": 0, "part": { "type": "output_text", "annotations": [], "text": "" } }),
            );
            let first = event(
                &mut out,
                "response.output_text.delta",
                json!({ "item_id": "msg_matrix", "output_index": 0, "content_index": 0, "delta": Neutral::TEXT_PARTS[0] }),
            );
            event(
                &mut out,
                "response.output_text.delta",
                json!({ "item_id": "msg_matrix", "output_index": 0, "content_index": 0, "delta": Neutral::TEXT_PARTS[1] }),
            );
            event(
                &mut out,
                "response.output_text.done",
                json!({ "item_id": "msg_matrix", "output_index": 0, "content_index": 0, "text": text }),
            );
            event(
                &mut out,
                "response.content_part.done",
                json!({ "item_id": "msg_matrix", "output_index": 0, "content_index": 0, "part": { "type": "output_text", "annotations": [], "text": text } }),
            );
            let item = json!({ "id": "msg_matrix", "type": "message", "status": "completed", "role": "assistant", "content": [{ "type": "output_text", "annotations": [], "text": text }] });
            event(&mut out, "response.output_item.done", json!({ "output_index": 0, "item": item }));
            let terminal = event(
                &mut out,
                "response.completed",
                json!({ "response": response("completed", json!([item]), Some(json!({ "input_tokens": 11, "output_tokens": 5, "total_tokens": 16 }))) }),
            );
            out.finish(terminal, first)
        }
        Scenario::Tool | Scenario::Signed => {
            event(&mut out, "response.created", json!({ "response": response("in_progress", json!([]), None) }));
            event(&mut out, "response.in_progress", json!({ "response": response("in_progress", json!([]), None) }));
            let mut output: Vec<Value> = Vec::new();
            let mut at = 0u32;
            if scenario == Scenario::Signed {
                event(
                    &mut out,
                    "response.output_item.added",
                    json!({ "output_index": at, "item": { "id": "rs_matrix", "type": "reasoning", "summary": [] } }),
                );
                event(
                    &mut out,
                    "response.reasoning_summary_part.added",
                    json!({ "item_id": "rs_matrix", "output_index": at, "summary_index": 0, "part": { "type": "summary_text", "text": "" } }),
                );
                event(
                    &mut out,
                    "response.reasoning_summary_text.delta",
                    json!({ "item_id": "rs_matrix", "output_index": at, "summary_index": 0, "delta": Neutral::REASON }),
                );
                event(
                    &mut out,
                    "response.reasoning_summary_text.done",
                    json!({ "item_id": "rs_matrix", "output_index": at, "summary_index": 0, "text": Neutral::REASON }),
                );
                event(
                    &mut out,
                    "response.reasoning_summary_part.done",
                    json!({ "item_id": "rs_matrix", "output_index": at, "summary_index": 0, "part": { "type": "summary_text", "text": Neutral::REASON } }),
                );
                let item = json!({ "id": "rs_matrix", "type": "reasoning", "summary": [], "encrypted_content": Neutral::ENC });
                event(&mut out, "response.output_item.done", json!({ "output_index": at, "item": item }));
                output.push(item);
                at += 1;
            }
            event(
                &mut out,
                "response.output_item.added",
                json!({ "output_index": at, "item": { "id": "msg_matrix", "type": "message", "status": "in_progress", "role": "assistant", "content": [] } }),
            );
            event(
                &mut out,
                "response.content_part.added",
                json!({ "item_id": "msg_matrix", "output_index": at, "content_index": 0, "part": { "type": "output_text", "annotations": [], "text": "" } }),
            );
            let first = event(
                &mut out,
                "response.output_text.delta",
                json!({ "item_id": "msg_matrix", "output_index": at, "content_index": 0, "delta": Neutral::LEAD }),
            );
            event(
                &mut out,
                "response.output_text.done",
                json!({ "item_id": "msg_matrix", "output_index": at, "content_index": 0, "text": Neutral::LEAD }),
            );
            event(
                &mut out,
                "response.content_part.done",
                json!({ "item_id": "msg_matrix", "output_index": at, "content_index": 0, "part": { "type": "output_text", "annotations": [], "text": Neutral::LEAD } }),
            );
            let message = json!({ "id": "msg_matrix", "type": "message", "status": "completed", "role": "assistant", "content": [{ "type": "output_text", "annotations": [], "text": Neutral::LEAD }] });
            event(&mut out, "response.output_item.done", json!({ "output_index": at, "item": message }));
            output.push(message);
            at += 1;
            event(
                &mut out,
                "response.output_item.added",
                json!({ "output_index": at, "item": { "id": "fc_matrix", "type": "function_call", "status": "in_progress", "call_id": Neutral::CALL_ID, "name": Neutral::CALL_NAME, "arguments": "" } }),
            );
            for piece in Neutral::ARG_FRAGMENTS {
                event(
                    &mut out,
                    "response.function_call_arguments.delta",
                    json!({ "item_id": "fc_matrix", "output_index": at, "delta": piece }),
                );
            }
            let arguments = Neutral::ARG_FRAGMENTS.concat();
            event(
                &mut out,
                "response.function_call_arguments.done",
                json!({ "item_id": "fc_matrix", "output_index": at, "name": Neutral::CALL_NAME, "arguments": arguments }),
            );
            let call = json!({ "id": "fc_matrix", "type": "function_call", "status": "completed", "call_id": Neutral::CALL_ID, "name": Neutral::CALL_NAME, "arguments": arguments });
            event(&mut out, "response.output_item.done", json!({ "output_index": at, "item": call }));
            output.push(call);
            let terminal = event(
                &mut out,
                "response.completed",
                json!({ "response": response("completed", Value::Array(output), Some(json!({ "input_tokens": 20, "output_tokens": 7, "total_tokens": 27 }))) }),
            );
            out.finish(terminal, first)
        }
    }
}

/// The Gemini stream of a scenario.
pub fn gemini_stream(scenario: Scenario) -> Streamed {
    let mut out = Writer::new();
    let chunk = |parts: Value, finish: Option<&str>, usage: Value| {
        let mut candidate = json!({ "content": { "parts": parts, "role": "model" }, "index": 0 });
        if let Some(finish) = finish {
            candidate["finishReason"] = json!(finish);
        }
        json!({ "candidates": [candidate], "usageMetadata": usage, "modelVersion": "gemini-test", "responseId": "r1" })
    };
    match scenario {
        Scenario::Text => {
            let first = out.event(
                None,
                &chunk(json!([{ "text": Neutral::TEXT_PARTS[0] }]), None, json!({ "promptTokenCount": 11, "totalTokenCount": 11 })),
            );
            let terminal = out.event(
                None,
                &chunk(
                    json!([{ "text": Neutral::TEXT_PARTS[1] }]),
                    Some("STOP"),
                    json!({ "promptTokenCount": 11, "candidatesTokenCount": 5, "totalTokenCount": 16 }),
                ),
            );
            out.finish(terminal, first)
        }
        Scenario::Tool | Scenario::Signed => {
            let usage = json!({ "promptTokenCount": 20, "totalTokenCount": 20 });
            if scenario == Scenario::Signed {
                out.event(None, &chunk(json!([{ "text": Neutral::REASON, "thought": true }]), None, usage.clone()));
            }
            let first = out.event(None, &chunk(json!([{ "text": Neutral::LEAD }]), None, usage));
            let mut part = json!({ "functionCall": { "name": Neutral::CALL_NAME, "args": Neutral::call_args() } });
            if scenario == Scenario::Signed {
                part["thoughtSignature"] = json!(Neutral::SIG);
            }
            let terminal = out.event(
                None,
                &chunk(
                    json!([part]),
                    Some("STOP"),
                    json!({ "promptTokenCount": 20, "candidatesTokenCount": 7, "totalTokenCount": 27 }),
                ),
            );
            out.finish(terminal, first)
        }
    }
}

/// The stream of a wire and a scenario.
pub fn stream(wire: Wire, scenario: Scenario) -> Streamed {
    match wire {
        Wire::OpenAiChat => chat_stream(scenario),
        Wire::Ollama => ollama_stream(scenario),
        Wire::Anthropic => anthropic_stream(scenario),
        Wire::OpenAiResponses => responses_stream(scenario),
        Wire::Gemini => gemini_stream(scenario),
    }
}

// ---- error streams ----------------------------------------------------------------------------------------

/// The kind of failure a server reports inside a stream that answered 200.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The upstream failed.
    ServerError,
    /// The service is overloaded.
    Overloaded,
    /// A rate limit was reached.
    RateLimited,
    /// The conversation does not fit the model.
    ContextLength,
}

/// A stream that delivers the text `partial` and then reports an error in the form of its own dialect, with the
/// server-text canary inside the message. `None` where the dialect has no streaming form for that kind.
pub fn error_stream(wire: Wire, kind: ErrorKind) -> Option<Streamed> {
    let mut out = Writer::new();
    let message = format!("upstream said: {CANARY_TEXT}");
    match (wire, kind) {
        (Wire::OpenAiChat, ErrorKind::ServerError) => {
            let first = out.event(None, &chat_chunk(json!({ "role": "assistant", "content": "partial" }), None));
            let end = out.event(None, &json!({ "error": { "code": 502, "type": "upstream_error", "message": message } }));
            Some(out.finish(end, first))
        }
        (Wire::Ollama, ErrorKind::ServerError) => {
            let first = out.line(&json!({ "model": "test-model", "message": { "role": "assistant", "content": "partial" }, "done": false }));
            let end = out.line(&json!({ "error": message }));
            Some(out.finish(end, first))
        }
        (Wire::Anthropic, ErrorKind::Overloaded) => {
            out.event(
                Some("message_start"),
                &json!({ "type": "message_start", "message": { "id": "msg_matrix", "type": "message", "role": "assistant", "model": "claude-test-1", "content": [], "usage": { "input_tokens": 1, "output_tokens": 1 } } }),
            );
            out.event(
                Some("content_block_start"),
                &json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
            );
            let first = out.event(
                Some("content_block_delta"),
                &json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "partial" } }),
            );
            let end = out.event(
                Some("error"),
                &json!({ "type": "error", "error": { "type": "overloaded_error", "message": message } }),
            );
            Some(out.finish(end, first))
        }
        (Wire::OpenAiResponses, ErrorKind::RateLimited | ErrorKind::ContextLength) => {
            let code = if kind == ErrorKind::RateLimited { "rate_limit_exceeded" } else { "context_length_exceeded" };
            let response = |status: &str| json!({ "id": "resp_matrix", "object": "response", "status": status, "model": "gpt-test-1", "output": [] });
            out.event(
                Some("response.created"),
                &json!({ "type": "response.created", "sequence_number": 0, "response": response("in_progress") }),
            );
            out.event(
                Some("response.output_item.added"),
                &json!({ "type": "response.output_item.added", "sequence_number": 1, "output_index": 0, "item": { "id": "msg_matrix", "type": "message", "status": "in_progress", "role": "assistant", "content": [] } }),
            );
            out.event(
                Some("response.content_part.added"),
                &json!({ "type": "response.content_part.added", "sequence_number": 2, "item_id": "msg_matrix", "output_index": 0, "content_index": 0, "part": { "type": "output_text", "annotations": [], "text": "" } }),
            );
            let first = out.event(
                Some("response.output_text.delta"),
                &json!({ "type": "response.output_text.delta", "sequence_number": 3, "item_id": "msg_matrix", "output_index": 0, "content_index": 0, "delta": "partial" }),
            );
            let mut failed = response("failed");
            failed["error"] = json!({ "code": code, "message": message });
            let end = out.event(
                Some("response.failed"),
                &json!({ "type": "response.failed", "sequence_number": 4, "response": failed }),
            );
            Some(out.finish(end, first))
        }
        (Wire::Gemini, ErrorKind::RateLimited) => {
            let first = out.event(
                None,
                &json!({ "candidates": [{ "content": { "parts": [{ "text": "partial" }], "role": "model" }, "index": 0 }] }),
            );
            let end = out.event(None, &json!({ "error": { "code": 429, "message": message, "status": "RESOURCE_EXHAUSTED" } }));
            Some(out.finish(end, first))
        }
        _ => None,
    }
}

// ---- hostile tool calls -----------------------------------------------------------------------------------

/// The ways a local server can damage the arguments of a tool call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hostile {
    /// The arguments are cut inside an object and a finish reason closes the turn anyway.
    CutObject,
    /// The arguments are a JSON array.
    Array,
    /// The arguments are a JSON number.
    Number,
    /// The arguments are an encoded string that holds an encoded object.
    DoubleEncoded,
    /// The call is open when the output limit ends the turn, and its arguments look complete.
    LengthStop,
    /// The arguments are given as a string (the Ollama dialect sends an object).
    StringArgs,
    /// The call has no name.
    NoName,
}

/// The stream of a hostile call in a dialect, or `None` where the dialect cannot express it. The path inside the
/// arguments is the argument canary.
pub fn hostile_tool_stream(wire: Wire, kind: Hostile) -> Option<Streamed> {
    let mut out = Writer::new();
    match wire {
        Wire::OpenAiChat => {
            let object = json!({ "path": ARG_CANARY });
            let (arguments, finish) = match kind {
                Hostile::CutObject => (format!("{{\"path\":\"{ARG_CANARY}"), "tool_calls"),
                Hostile::Array => (json!([ARG_CANARY]).to_string(), "tool_calls"),
                Hostile::Number => ("42".to_owned(), "tool_calls"),
                Hostile::DoubleEncoded => (json!(object.to_string()).to_string(), "tool_calls"),
                Hostile::LengthStop => (object.to_string(), "length"),
                Hostile::StringArgs | Hostile::NoName => return None,
            };
            out.event(None, &chat_chunk(json!({ "role": "assistant", "content": null }), None));
            let first = out.event(
                None,
                &chat_chunk(
                    json!({ "tool_calls": [{ "index": 0, "id": "call_h1", "type": "function", "function": { "name": "read_file", "arguments": arguments } }] }),
                    None,
                ),
            );
            let end = out.event(None, &chat_chunk(json!({}), Some(finish)));
            out.event(
                None,
                &json!({ "id": "chatcmpl-matrix", "object": "chat.completion.chunk", "created": 1_760_000_000, "model": "test-model", "choices": [], "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 } }),
            );
            out.done();
            Some(out.finish(end, first))
        }
        Wire::Ollama => {
            let object = json!({ "path": ARG_CANARY });
            let call = match kind {
                Hostile::StringArgs | Hostile::DoubleEncoded => json!({ "function": { "name": "read_file", "arguments": object.to_string() } }),
                Hostile::Array => json!({ "function": { "name": "read_file", "arguments": [ARG_CANARY] } }),
                Hostile::Number => json!({ "function": { "name": "read_file", "arguments": 42 } }),
                Hostile::NoName => json!({ "function": { "arguments": object } }),
                Hostile::CutObject | Hostile::LengthStop => return None,
            };
            let first = out.line(&json!({ "model": "test-model", "message": { "role": "assistant", "content": "", "tool_calls": [call] }, "done": false }));
            let mut last = json!({ "model": "test-model", "message": { "role": "assistant", "content": "" }, "done": true, "done_reason": "stop" });
            last["prompt_eval_count"] = json!(5);
            last["eval_count"] = json!(3);
            let end = out.line(&last);
            Some(out.finish(end, first))
        }
        _ => None,
    }
}

// ---- floods -----------------------------------------------------------------------------------------------

/// The text scenario with `events` text deltas of one character each, then the closing events with usage, cut into
/// chunks of 64 KiB. Every delta is one event of the dialect, so the count of text events equals `events`.
pub fn flood(wire: Wire, events: usize) -> Vec<Vec<u8>> {
    let mut out = Writer::new();
    let text = "x".repeat(events);
    match wire {
        Wire::OpenAiChat => {
            out.event(None, &chat_chunk(json!({ "role": "assistant", "content": "" }), None));
            for _ in 0..events {
                out.event(None, &chat_chunk(json!({ "content": "x" }), None));
            }
            out.event(None, &chat_chunk(json!({}), Some("stop")));
            out.event(
                None,
                &json!({ "id": "chatcmpl-matrix", "object": "chat.completion.chunk", "created": 1_760_000_000, "model": "test-model", "choices": [], "usage": { "prompt_tokens": 11, "completion_tokens": 5, "total_tokens": 16 } }),
            );
            out.done();
        }
        Wire::Ollama => {
            for _ in 0..events {
                out.line(&json!({ "model": "test-model", "message": { "role": "assistant", "content": "x" }, "done": false }));
            }
            out.line(&json!({ "model": "test-model", "message": { "role": "assistant", "content": "" }, "done": true, "done_reason": "stop", "prompt_eval_count": 11, "eval_count": 5 }));
        }
        Wire::Anthropic => {
            out.event(
                Some("message_start"),
                &json!({ "type": "message_start", "message": { "id": "msg_matrix", "type": "message", "role": "assistant", "model": "claude-test-1", "content": [], "usage": { "input_tokens": 11, "output_tokens": 1 } } }),
            );
            out.event(
                Some("content_block_start"),
                &json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
            );
            for _ in 0..events {
                out.event(
                    Some("content_block_delta"),
                    &json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "x" } }),
                );
            }
            out.event(Some("content_block_stop"), &json!({ "type": "content_block_stop", "index": 0 }));
            out.event(
                Some("message_delta"),
                &json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn", "stop_sequence": null }, "usage": { "output_tokens": 5 } }),
            );
            out.event(Some("message_stop"), &json!({ "type": "message_stop" }));
        }
        Wire::OpenAiResponses => {
            let mut sequence = 0u32;
            let mut event = |out: &mut Writer, name: &str, mut data: Value| {
                data["type"] = json!(name);
                data["sequence_number"] = json!(sequence);
                sequence += 1;
                out.event(Some(name), &data)
            };
            let response = |status: &str, output: Value| json!({ "id": "resp_matrix", "object": "response", "status": status, "model": "gpt-test-1", "output": output });
            event(&mut out, "response.created", json!({ "response": response("in_progress", json!([])) }));
            event(
                &mut out,
                "response.output_item.added",
                json!({ "output_index": 0, "item": { "id": "msg_matrix", "type": "message", "status": "in_progress", "role": "assistant", "content": [] } }),
            );
            event(
                &mut out,
                "response.content_part.added",
                json!({ "item_id": "msg_matrix", "output_index": 0, "content_index": 0, "part": { "type": "output_text", "annotations": [], "text": "" } }),
            );
            for _ in 0..events {
                event(
                    &mut out,
                    "response.output_text.delta",
                    json!({ "item_id": "msg_matrix", "output_index": 0, "content_index": 0, "delta": "x" }),
                );
            }
            event(
                &mut out,
                "response.output_text.done",
                json!({ "item_id": "msg_matrix", "output_index": 0, "content_index": 0, "text": text }),
            );
            event(
                &mut out,
                "response.content_part.done",
                json!({ "item_id": "msg_matrix", "output_index": 0, "content_index": 0, "part": { "type": "output_text", "annotations": [], "text": text } }),
            );
            let item = json!({ "id": "msg_matrix", "type": "message", "status": "completed", "role": "assistant", "content": [{ "type": "output_text", "annotations": [], "text": text }] });
            event(&mut out, "response.output_item.done", json!({ "output_index": 0, "item": item }));
            let mut done = response("completed", json!([item]));
            done["usage"] = json!({ "input_tokens": 11, "output_tokens": 5, "total_tokens": 16 });
            event(&mut out, "response.completed", json!({ "response": done }));
        }
        Wire::Gemini => {
            let chunk = |text: &str, finish: Option<&str>| {
                let mut candidate = json!({ "content": { "parts": [{ "text": text }], "role": "model" }, "index": 0 });
                if let Some(finish) = finish {
                    candidate["finishReason"] = json!(finish);
                }
                json!({ "candidates": [candidate], "usageMetadata": { "promptTokenCount": 11, "candidatesTokenCount": 5, "totalTokenCount": 16 } })
            };
            for _ in 0..events.saturating_sub(1) {
                out.event(None, &chunk("x", None));
            }
            out.event(None, &chunk("x", Some("STOP")));
        }
    }
    out.bytes.chunks(64 * 1024).map(<[u8]>::to_vec).collect()
}
