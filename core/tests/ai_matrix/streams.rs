#![allow(dead_code)]
//! Generators for the neutral scenarios in the five dialects.
//!
//! Each generator copies the shape of the events in the adapters' golden fixtures; the fixtures themselves are never
//! read. The streams are built by hand like those fixtures: they are not recordings of a real server.

use holzwave_core::ai::wire::Wire;
use serde_json::{Value, json};

use crate::support::{Neutral, Scenario};

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
