//! The provider matrix: every wire adapter, every preset and every wire a preset offers, through the same neutral
//! scenarios, with no outside network.
//!
//! The first rows prove that a saved profile and its stored key become a provider the way the service builds it, that a
//! text turn streams through the real chokepoint and adapter into a collector, and that the captured request of every
//! cell carries exactly the pinned header names, path and body members. Later rows hold the tool turn, the signed
//! reasoning round trip and the pairing of results on every cell. Every network row talks to an in-process server;
//! every wait is bounded.

mod common;
#[path = "ai_matrix/streams.rs"]
mod streams;
#[path = "ai_matrix/support.rs"]
mod support;

use std::collections::BTreeSet;

use common::fake_http::Reply;
use holzwave_core::ai::agent::TRIMMED_RESULT;
use holzwave_core::ai::http::{HttpConfig, Origin, Target};
use holzwave_core::ai::profiles::{KeyNeed, PRESETS, Preset, ResolveError, auth_header_for};
use holzwave_core::ai::provider::{AiError, Provider, StreamEvent};
use holzwave_core::ai::service::dispatch::adapter_for;
use holzwave_core::ai::store::NOT_STORED;
use holzwave_core::ai::types::{Locality, Opaque, StopReason, ToolCall, Usage};
use holzwave_core::ai::wire::Wire;
use serde_json::json;
use streams::stream;
use support::{
    ANTHROPIC_VERSION, BASELINE, Cell, GATEWAY_HEADER, KEY_HEADERS, KeyRule, Keys, Neutral, Scenario, Side,
    assistant_of, canary_gateway_key, canary_key, catalog_specs, cell, cells, chat_request, count_in, cuts, drive,
    fingerprint, follow_up, header_names, json_body, key_header, members, one_byte, outcome, pairing_request, pinned_members,
    repeated_ids_request, reply_for, rig, run, seeded, split_inside_characters, texts_of, try_rig,
};

/// The reply of one cell for a scenario: the whole stream as one chunk.
fn whole(wire: Wire, scenario: Scenario) -> Reply {
    reply_for(wire, vec![stream(wire, scenario).bytes])
}

/// The set of names as strings.
fn names(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

/// The header names a cell must send for these keys.
fn expected_names(cell: &Cell, keys: Keys) -> BTreeSet<String> {
    let mut expected = names(&BASELINE);
    let (key_name, _) = key_header(cell.wire);
    if matches!(keys, Keys::Provider | Keys::Both) {
        expected.insert(key_name.to_owned());
    }
    if cell.cloudflare() && matches!(keys, Keys::Both | Keys::GatewayOnly) {
        expected.insert(GATEWAY_HEADER.to_owned());
    }
    if cell.wire == Wire::Anthropic {
        expected.insert("anthropic-version".to_owned());
    }
    expected
}

#[test]
fn a_text_turn_runs_from_profile_and_key_to_collector_through_the_real_path() {
    let custom = cell("custom_chat");
    let bytes = stream(custom.wire, Scenario::Text).bytes;
    let pieces = split_inside_characters(&bytes);
    assert_eq!(pieces.len(), 3, "the stream is cut inside a two-byte and a four-byte character");
    let rig = rig(&custom, vec![reply_for(custom.wire, pieces)], Keys::Provider, HttpConfig::standard());

    let result = run(&rig.provider, &chat_request(&custom, "hello"));
    let done = result.result.as_ref().unwrap_or_else(|error| panic!("the turn failed: {error}"));

    let deltas: Vec<&str> = result
        .events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, Neutral::TEXT_PARTS, "the two pieces arrive whole and in order");
    let last_usage = result.events.iter().rev().find_map(|event| match event {
        StreamEvent::Usage(usage) => Some(*usage),
        _ => None,
    });
    assert_eq!(last_usage, Some(Usage { input_tokens: 11, output_tokens: 5 }), "the totals end the turn");
    let said = outcome(&result);
    assert_eq!(said.text, Neutral::TEXT);
    assert_eq!(done.stop_reason, StopReason::EndTurn);
    assert_eq!(done.usage, Neutral::TEXT_USAGE);
    assert!(done.native.is_none(), "Chat Completions keeps no native turn");

    assert_eq!(rig.fake.accepted(), 1, "one connection");
    let seen = rig.fake.seen();
    assert_eq!(seen.len(), 1, "one request");
    let request = &seen[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.target, "/v1/chat/completions");
    assert_eq!(header_names(request), expected_names(&custom, Keys::Provider));
    assert_eq!(request.header("authorization"), Some(format!("Bearer {}", canary_key()).as_str()));
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("accept"), Some("text/event-stream"));
    assert!(request.header("user-agent").is_some_and(|agent| agent.starts_with("holzWave/")), "{:?}", request.headers);
    assert_eq!(request.header("host"), Some(format!("127.0.0.1:{}", rig.fake.port()).as_str()));
    for (name, value) in &request.headers {
        if name != "authorization" {
            assert!(!value.contains(canary_key()), "the key is in the header {name}");
        }
    }
    assert!(!request.target.contains(canary_key()), "the key is in the target");
    assert!(!request.body_text().contains(canary_key()), "the key is in the body");
}

#[test]
fn the_cell_table_matches_the_pinned_presets_and_key_headers() {
    let all = cells();
    assert_eq!(all.len(), 19, "nineteen cells");
    let unique: BTreeSet<&str> = all.iter().map(|cell| cell.name).collect();
    assert_eq!(unique.len(), 19, "the names are unique");
    for preset in Preset::ALL {
        assert!(all.iter().any(|cell| cell.preset_tag == preset.tag()), "the preset {} has no cell", preset.tag());
    }
    let count = |tag: &str| all.iter().filter(|cell| cell.preset_tag == tag).count();
    assert_eq!(count("custom"), 5, "Custom on all five wires");
    assert_eq!(count("cloudflare_gateway"), 4);
    assert_eq!(count("vercel_gateway"), 2);
    assert_eq!(all.iter().filter(|cell| cell.signs).count(), 10, "ten cells sign");
    assert_eq!(all.iter().filter(|cell| cell.wire == Wire::OpenAiChat).count(), 7, "seven Chat Completions cells");
    assert_eq!(all.iter().filter(|cell| cell.wire == Wire::Ollama).count(), 2, "two Ollama cells");
    assert_eq!(all.iter().filter(|cell| cell.gateway).count(), 13, "thirteen gateway cells");

    for cell in &all {
        let preset = cell.preset();
        let spec = preset.spec();
        assert!(spec.wires.contains(&cell.wire), "{}: the preset does not offer the wire", cell.name);
        if preset != Preset::Custom {
            let expected = if spec.key == KeyNeed::Required { KeyRule::Required } else { KeyRule::Optional };
            assert_eq!(cell.key, expected, "{}: the key rule", cell.name);
            let base = spec.base_for(cell.wire).unwrap_or_else(|| panic!("{}: no default address", cell.name));
            let filled = base.replace("{account_id}", "acct").replace("{gateway_id}", "gw");
            let target = Target::parse(&filled, false).unwrap_or_else(|error| panic!("{}: {error}", cell.name));
            let base_path = target.base_path().trim_end_matches('/');
            assert_eq!(base_path, cell.base_suffix, "{}: the base path of the default address", cell.name);
            if matches!(cell.preset_tag, "ollama" | "lm_studio" | "llama_cpp" | "litellm") {
                assert!(filled.starts_with("http://127.0.0.1:"), "{}: a local preset starts on the loopback address", cell.name);
            } else {
                assert!(
                    filled.starts_with(cell.real_origin()),
                    "{}: the default address {filled} is not at {}",
                    cell.name,
                    cell.real_origin()
                );
            }
        }
        let (name, prefix) = auth_header_for(cell.wire);
        assert_eq!((name, prefix), key_header(cell.wire), "{}: the key header of the wire", cell.name);
    }
    assert_eq!(PRESETS.len(), 11);
    for (wire, name, prefix) in KEY_HEADERS {
        assert_eq!(auth_header_for(wire), (name, prefix), "{}: the pinned key header", wire.tag());
    }
    assert_eq!(key_header(Wire::OpenAiChat), ("authorization", "Bearer "));
    assert_eq!(key_header(Wire::OpenAiResponses), ("authorization", "Bearer "));
    assert_eq!(key_header(Wire::Anthropic), ("x-api-key", ""));
    assert_eq!(key_header(Wire::Gemini), ("x-goog-api-key", ""));
    assert_eq!(key_header(Wire::Ollama), ("authorization", "Bearer "));
}

#[test]
fn building_a_provider_for_every_cell_opens_no_socket() {
    for cell in cells() {
        let both = if cell.cloudflare() { Keys::Both } else { Keys::Provider };
        let rig = rig(&cell, vec![Reply::Close], both, HttpConfig::standard());
        assert_eq!(rig.provider.id(), rig.id, "{}: the id of the provider is the id of the profile", cell.name);
        let caps = rig.provider.capabilities(cell.model);
        assert_eq!(caps.locality, Locality::Loopback, "{}: the locality of the capabilities", cell.name);
        assert_eq!(rig.provider.locality(), Locality::Loopback, "{}: the locality", cell.name);
        assert_eq!(rig.fake.accepted(), 0, "{}: building a provider opened a connection", cell.name);
        assert_eq!(rig.http.attempts(), 0, "{}: building a provider sent a request", cell.name);
    }
}

#[test]
fn every_cell_streams_the_same_text_turn() {
    for cell in cells() {
        let rig = rig(&cell, vec![whole(cell.wire, Scenario::Text)], Keys::Provider, HttpConfig::standard());
        let result = run(&rig.provider, &chat_request(&cell, "hello"));
        let done = result.result.as_ref().unwrap_or_else(|error| panic!("{} text: the turn failed: {error}", cell.name));
        let said = outcome(&result);
        assert_eq!(said.text, Neutral::TEXT, "{} text: the answer", cell.name);
        assert!(said.reasoning.is_empty(), "{} text: no reasoning", cell.name);
        assert!(said.calls.is_empty(), "{} text: no call", cell.name);
        assert_eq!(said.usage, Neutral::TEXT_USAGE, "{} text: the usage", cell.name);
        assert_eq!(done.stop_reason, StopReason::EndTurn, "{} text: the stop reason", cell.name);
        match &done.native {
            Some(native) => {
                assert!(cell.signs, "{} text: a wire that does not sign kept a native turn", cell.name);
                assert_eq!(native.wire, cell.wire.tag(), "{} text: the wire tag of the native turn", cell.name);
            }
            None => assert!(!cell.signs, "{} text: a wire that signs kept no native turn", cell.name),
        }
        assert_eq!(rig.fake.accepted(), 1, "{} text: one connection", cell.name);
    }
}

#[test]
fn every_cell_sends_exactly_the_pinned_headers_and_path() {
    for cell in cells() {
        // Every cell, with its provider key.
        let first = rig(&cell, vec![whole(cell.wire, Scenario::Text)], Keys::Provider, HttpConfig::standard());
        let _ = run(&first.provider, &chat_request(&cell, "hello"));
        let seen = first.fake.seen();
        assert_eq!(seen.len(), 1, "{}: one request", cell.name);
        let request = &seen[0];
        assert_eq!(request.method, "POST", "{}", cell.name);
        assert_eq!(request.target, cell.path(), "{}: the joined path", cell.name);
        assert_eq!(header_names(request), expected_names(&cell, Keys::Provider), "{}: the header names", cell.name);
        let (key_name, prefix) = key_header(cell.wire);
        assert_eq!(
            request.header(key_name),
            Some(format!("{prefix}{}", canary_key()).as_str()),
            "{}: the key header",
            cell.name
        );
        let accept = if cell.wire == Wire::Ollama { "application/x-ndjson" } else { "text/event-stream" };
        assert_eq!(request.header("accept"), Some(accept), "{}: accept", cell.name);
        assert_eq!(request.header("content-type"), Some("application/json"), "{}: content type", cell.name);
        if cell.wire == Wire::Anthropic {
            assert_eq!(request.header("anthropic-version"), Some(ANTHROPIC_VERSION), "{}: the version", cell.name);
            assert!(request.header("anthropic-beta").is_none(), "{}: no beta header", cell.name);
        }
        for (name, value) in &request.headers {
            if name != key_name {
                assert!(!value.contains(canary_key()), "{}: the key is in the header {name}", cell.name);
            }
        }
        assert!(!request.target.contains(canary_key()), "{}: the key is in the target", cell.name);
        assert!(!request.body_text().contains(canary_key()), "{}: the key is in the body", cell.name);
        for forbidden in ["accept-encoding", "cookie", "referer", "origin", "x-title", "x-forwarded-for", "http-referer"] {
            assert!(request.header(forbidden).is_none(), "{}: the header {forbidden} is sent", cell.name);
        }

        // The cells that may go without a key send the baseline only.
        if cell.key == KeyRule::Optional {
            let bare = rig_without_key(&cell);
            assert_eq!(
                bare.names,
                expected_names(&cell, Keys::None),
                "{}: without a key only the baseline goes out",
                cell.name
            );
        } else {
            let refused = try_rig(&cell, vec![Reply::Close], Keys::None).err();
            assert_eq!(refused, Some(ResolveError::KeyMissing(support_slot())), "{}: a required key is missing", cell.name);
            let error = AiError::from(refused.expect("refused"));
            assert!(!error.to_string().is_empty(), "{}: the refusal has a phrase", cell.name);
        }

        // A Cloudflare cell with a gateway token.
        if cell.cloudflare() {
            let both = rig(&cell, vec![whole(cell.wire, Scenario::Text)], Keys::Both, HttpConfig::standard());
            let _ = run(&both.provider, &chat_request(&cell, "hello"));
            let seen = both.fake.seen();
            let request = &seen[0];
            assert_eq!(header_names(request), expected_names(&cell, Keys::Both), "{}: both slots", cell.name);
            assert_eq!(
                request.header(GATEWAY_HEADER),
                Some(format!("Bearer {}", canary_gateway_key()).as_str()),
                "{}: the gateway header",
                cell.name
            );
            assert_eq!(
                request.header(key_name),
                Some(format!("{prefix}{}", canary_key()).as_str()),
                "{}: the provider key beside it",
                cell.name
            );
            let only = rig_with(&cell, Keys::GatewayOnly);
            assert_eq!(only.names, expected_names(&cell, Keys::GatewayOnly), "{}: the gateway token alone", cell.name);
            assert!(!only.names.contains(key_name), "{}: no key header without a key", cell.name);
        }
    }
}

/// What a bare run of a cell sent.
struct Sent {
    names: BTreeSet<String>,
}

fn support_slot() -> holzwave_core::ai::profiles::KeySlot {
    holzwave_core::ai::profiles::KeySlot::Provider
}

fn rig_with(cell: &Cell, keys: Keys) -> Sent {
    let rig = rig(cell, vec![whole(cell.wire, Scenario::Text)], keys, HttpConfig::standard());
    let result = run(&rig.provider, &chat_request(cell, "hello"));
    assert!(result.result.is_ok(), "{}: the turn failed", cell.name);
    let seen = rig.fake.seen();
    Sent { names: header_names(&seen[0]) }
}

fn rig_without_key(cell: &Cell) -> Sent {
    rig_with(cell, Keys::None)
}

#[test]
fn every_cell_sends_only_the_pinned_body_members() {
    for cell in cells() {
        let rig = rig(&cell, vec![whole(cell.wire, Scenario::Text)], Keys::Provider, HttpConfig::standard());
        let result = run(&rig.provider, &chat_request(&cell, "hello"));
        assert!(result.result.is_ok(), "{}: the turn failed", cell.name);
        let seen = rig.fake.seen();
        let body = json_body(cell.name, &seen[0]);
        let mut expected: Vec<&str> = match cell.wire {
            Wire::OpenAiChat => vec!["model", "messages", "stream", "stream_options", "max_tokens"],
            Wire::Ollama => vec!["model", "messages", "stream", "options"],
            Wire::Anthropic => vec!["model", "max_tokens", "stream", "system", "messages"],
            Wire::OpenAiResponses => vec!["model", "instructions", "input", "store", "stream", "max_output_tokens"],
            Wire::Gemini => vec!["contents", "systemInstruction", "generationConfig"],
        };
        if cell.preset_tag == "openrouter" {
            expected.push("provider");
        }
        assert_eq!(members(&body), names(&expected), "{}: the body members", cell.name);
        match cell.wire {
            Wire::OpenAiChat => {
                assert_eq!(body["stream_options"], json!({ "include_usage": true }), "{}", cell.name);
                assert_eq!(body["max_tokens"], json!(1024), "{}", cell.name);
                assert_eq!(body["stream"], json!(true), "{}", cell.name);
            }
            Wire::Ollama => {
                assert_eq!(body["options"], json!({ "num_ctx": 16384, "num_predict": 1024 }), "{}", cell.name);
                assert_eq!(body["stream"], json!(true), "{}", cell.name);
            }
            Wire::Anthropic => {
                assert_eq!(body["max_tokens"], json!(1024), "{}", cell.name);
                assert_eq!(body["stream"], json!(true), "{}", cell.name);
                assert!(body.get("thinking").is_none(), "{}: no thinking member", cell.name);
            }
            Wire::OpenAiResponses => {
                assert_eq!(body["store"], json!(false), "{}", cell.name);
                assert_eq!(body["stream"], json!(true), "{}", cell.name);
                assert_eq!(body["max_output_tokens"], json!(1024), "{}", cell.name);
                assert!(body.get("include").is_none(), "{}: no include for a model that does not reason", cell.name);
            }
            Wire::Gemini => {
                assert_eq!(body["generationConfig"], json!({ "maxOutputTokens": 1024 }), "{}", cell.name);
                assert!(body.get("model").is_none(), "{}: the model is in the path", cell.name);
            }
        }
        if cell.preset_tag == "openrouter" {
            assert_eq!(body["provider"], json!({ "data_collection": "deny" }), "{}: no data collection", cell.name);
        }
        assert_eq!(texts_of(cell.wire, &body, Side::System), vec![support::SYSTEM_TEXT.to_owned()], "{}: system", cell.name);
        assert_eq!(texts_of(cell.wire, &body, Side::User), vec!["hello".to_owned()], "{}: user", cell.name);
    }
}

// ---- the tool turn ----------------------------------------------------------------------------------------

/// A request that declares the seven tools of the catalog and asks `list`.
fn tool_request(cell: &Cell) -> holzwave_core::ai::provider::ChatRequest {
    let mut request = chat_request(cell, "list");
    request.tools = catalog_specs();
    request
}

#[test]
fn every_cell_releases_one_tool_call_after_its_text_and_ends_with_tool_use() {
    for cell in cells() {
        let built = rig(&cell, vec![whole(cell.wire, Scenario::Tool)], Keys::Provider, HttpConfig::standard());
        let result = run(&built.provider, &tool_request(&cell));
        let done = result.result.as_ref().unwrap_or_else(|error| panic!("{} tool: the turn failed: {error}", cell.name));
        assert_eq!(done.stop_reason, StopReason::ToolUse, "{} tool: the stop reason", cell.name);
        let last = result.events.last().unwrap_or_else(|| panic!("{} tool: no event", cell.name));
        assert!(matches!(last, StreamEvent::ToolCall(_)), "{} tool: the call is not the last event: {:?}", cell.name, result.events);
        let calls: Vec<&ToolCall> = result
            .events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolCall(call) => Some(call),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 1, "{} tool: exactly one call", cell.name);
        let call = calls[0];
        assert_eq!(call.name, Neutral::CALL_NAME, "{} tool: the name", cell.name);
        assert_eq!(call.args, Neutral::call_args(), "{} tool: the arguments", cell.name);
        if Neutral::id_is_exact(cell.wire) {
            assert_eq!(call.id, Neutral::CALL_ID, "{} tool: the id", cell.name);
        } else {
            assert!(call.id.starts_with("call_"), "{} tool: a minted id starts with call_: {}", cell.name, call.id);
        }
        let said = outcome(&result);
        assert_eq!(said.text, Neutral::LEAD, "{} tool: the text before the call", cell.name);
        assert_eq!(said.usage, Neutral::TOOL_USAGE, "{} tool: the usage", cell.name);
        let call_at = result.events.len() - 1;
        let usage_at = result.events.iter().position(|event| matches!(event, StreamEvent::Usage(_)));
        assert!(usage_at.is_some_and(|at| at < call_at), "{} tool: usage arrives before the call", cell.name);
        let text_at = result.events.iter().position(|event| matches!(event, StreamEvent::TextDelta(_)));
        assert!(text_at.is_some_and(|at| at < call_at), "{} tool: the text arrives before the call", cell.name);

        assert_eq!(built.fake.accepted(), 1, "{} tool: one connection", cell.name);
        let seen = built.fake.seen();
        let body = json_body(cell.name, &seen[0]);
        let mut expected = pinned_members(&cell);
        expected.insert("tools".to_owned());
        assert_eq!(members(&body), expected, "{} tool: the body members", cell.name);
        let declared = body["tools"].to_string();
        for tool in holzwave_core::ai::tools::ToolName::ALL {
            assert!(declared.contains(tool.as_str()), "{} tool: {} is not declared", cell.name, tool.as_str());
        }
    }
}

// ---- splits and cuts --------------------------------------------------------------------------------------

#[test]
fn every_wire_decodes_the_neutral_streams_identically_at_every_split() {
    for wire in Wire::ALL {
        for scenario in [Scenario::Text, Scenario::Tool, Scenario::Signed] {
            let label = format!("{} {scenario:?}", wire.tag());
            let bytes = stream(wire, scenario).bytes;
            let whole_run = drive(wire, &[bytes.as_slice()]);
            let expected = fingerprint(wire, &whole_run);
            assert!(expected.1.is_ok(), "{label}: the whole stream fails: {:?}", expected.1);
            for at in cuts(bytes.len()) {
                let got = fingerprint(wire, &drive(wire, &[&bytes[..at], &bytes[at..]]));
                assert_eq!(got, expected, "{label}: cut once at {at}");
            }
            assert_eq!(fingerprint(wire, &drive(wire, &one_byte(&bytes))), expected, "{label}: one byte at a time");
            for seed in 1..=20 {
                assert_eq!(fingerprint(wire, &drive(wire, &seeded(&bytes, seed))), expected, "{label}: seed {seed}");
            }
        }
    }
}

#[test]
fn no_wire_releases_a_tool_call_from_a_stream_cut_before_its_end_marker() {
    for wire in Wire::ALL {
        for scenario in [Scenario::Text, Scenario::Tool, Scenario::Signed] {
            let label = format!("{} {scenario:?}", wire.tag());
            let streamed = stream(wire, scenario);
            let full = drive(wire, &[streamed.bytes.as_slice()]);
            let full_outcome = outcome(&full);
            for at in 0..streamed.terminal_end {
                let cut = drive(wire, &[&streamed.bytes[..at]]);
                assert!(
                    !cut.events.iter().any(|event| matches!(event, StreamEvent::ToolCall(_))),
                    "{label}: a call was delivered from a stream cut at {at} of {}",
                    streamed.terminal_end
                );
                match &cut.result {
                    Ok(_) => panic!("{label}: a stream cut at {at} before its end marker ended Ok"),
                    Err(error) => assert!(
                        matches!(error.class().as_str(), "network" | "protocol"),
                        "{label}: cut at {at}: class {}",
                        error.class().as_str()
                    ),
                }
            }
            // After the end marker the answer is complete. A cut that falls between two events still ends the stream
            // unclean, so it may be an error (never Ok with another answer); a cut at the end marker or at the end of
            // the stream is Ok.
            for at in streamed.terminal_end..=streamed.bytes.len() {
                let cut = drive(wire, &[&streamed.bytes[..at]]);
                match &cut.result {
                    Ok(_) => {
                        let got = outcome(&cut);
                        assert_eq!(got.text, full_outcome.text, "{label}: text of a cut at {at}");
                        assert_eq!(got.calls.len(), full_outcome.calls.len(), "{label}: calls of a cut at {at}");
                        for (call, whole_call) in got.calls.iter().zip(&full_outcome.calls) {
                            assert_eq!((&call.name, &call.args), (&whole_call.name, &whole_call.args), "{label}: call of a cut at {at}");
                        }
                    }
                    Err(error) => {
                        assert!(
                            at != streamed.terminal_end && at != streamed.bytes.len(),
                            "{label}: a stream cut at {at}, at an event boundary after its end marker, failed: {error}"
                        );
                        assert!(
                            matches!(error.class().as_str(), "network" | "protocol"),
                            "{label}: cut at {at}: class {}",
                            error.class().as_str()
                        );
                        assert!(
                            !cut.events.iter().any(|event| matches!(event, StreamEvent::ToolCall(_))),
                            "{label}: a call was delivered with the error of a cut at {at}"
                        );
                    }
                }
            }
        }
    }
}

// ---- the signed round trip --------------------------------------------------------------------------------

/// The two requests of a tool exchange on one cell: the second is built from the first completion.
struct Exchange {
    built: support::Rig,
    first: support::Run,
    call: ToolCall,
    second: support::Run,
    first_request: holzwave_core::ai::provider::ChatRequest,
    second_request: holzwave_core::ai::provider::ChatRequest,
}

fn exchange(cell: &Cell, scenario: Scenario) -> Exchange {
    let built = rig(
        cell,
        vec![whole(cell.wire, scenario), whole(cell.wire, Scenario::Text)],
        Keys::Provider,
        HttpConfig::standard(),
    );
    let first_request = tool_request(cell);
    let first = run(&built.provider, &first_request);
    assert!(first.result.is_ok(), "{} {scenario:?}: the first turn failed: {:?}", cell.name, first.result.as_ref().err());
    let said = outcome(&first);
    assert_eq!(said.calls.len(), 1, "{} {scenario:?}: one call", cell.name);
    let call = said.calls[0].clone();
    let second_request = follow_up(&first_request, assistant_of(cell.model, &first), &call, "alpha");
    let second = run(&built.provider, &second_request);
    assert!(second.result.is_ok(), "{} {scenario:?}: the second turn failed: {:?}", cell.name, second.result.as_ref().err());
    assert_eq!(outcome(&second).text, Neutral::TEXT, "{} {scenario:?}: the second answer", cell.name);
    Exchange { built, first, call, second, first_request, second_request }
}

#[test]
fn signed_thinking_round_trips_verbatim_on_the_wires_that_sign() {
    for cell in cells().into_iter().filter(|cell| cell.signs) {
        let ex = exchange(&cell, Scenario::Signed);
        let said = outcome(&ex.first);
        assert_eq!(said.reasoning, Neutral::REASON, "{} signed: the reasoning text", cell.name);
        assert_eq!(said.stop, Some(StopReason::ToolUse), "{} signed: the stop reason", cell.name);
        let native: &Opaque = ex.first.result.as_ref().expect("ok").native.as_ref().unwrap_or_else(|| panic!("{} signed: no native turn", cell.name));
        assert_eq!(native.wire, cell.wire.tag(), "{} signed: the wire of the native turn", cell.name);

        let seen = ex.built.fake.seen();
        assert_eq!(seen.len(), 2, "{} signed: two requests", cell.name);
        let text = seen[1].body_text();
        let body = json_body(cell.name, &seen[1]);
        let signature = if cell.wire == Wire::OpenAiResponses { Neutral::ENC } else { Neutral::SIG };
        assert_eq!(count_in(&text, signature), 1, "{} signed: the signature occurs once in the second request", cell.name);
        match cell.wire {
            Wire::Anthropic => {
                let messages = body["messages"].as_array().expect("messages");
                assert_eq!(messages.len(), 3, "{} signed: three messages", cell.name);
                assert_eq!(messages[1]["role"], json!("assistant"), "{}", cell.name);
                assert_eq!(messages[1]["content"], native.json, "{} signed: the assistant content is the native turn", cell.name);
                assert_eq!(messages[2]["role"], json!("user"), "{}", cell.name);
                let block = &messages[2]["content"][0];
                assert_eq!(block["type"], json!("tool_result"), "{}", cell.name);
                assert_eq!(block["tool_use_id"], json!(ex.call.id), "{}", cell.name);
                assert_eq!(block["content"], json!("alpha"), "{}", cell.name);
            }
            Wire::OpenAiResponses => {
                assert_eq!(body["store"], json!(false), "{} signed: store", cell.name);
                let input = body["input"].as_array().expect("input");
                let items = native.json.as_array().expect("the native turn is the array of output items");
                let start = input
                    .iter()
                    .position(|item| item == &items[0])
                    .unwrap_or_else(|| panic!("{} signed: the first native item is not replayed", cell.name));
                assert_eq!(&input[start..start + items.len()], items.as_slice(), "{} signed: the native items are a contiguous run", cell.name);
                let output = &input[start + items.len()];
                assert_eq!(output["type"], json!("function_call_output"), "{}", cell.name);
                assert_eq!(output["call_id"], json!(ex.call.id), "{}", cell.name);
                assert_eq!(output["output"], json!("alpha"), "{}", cell.name);
            }
            Wire::Gemini => {
                let contents = body["contents"].as_array().expect("contents");
                let at = contents
                    .iter()
                    .position(|content| content["role"] == json!("model"))
                    .unwrap_or_else(|| panic!("{} signed: no model entry", cell.name));
                assert_eq!(contents[at]["parts"], native.json["parts"], "{} signed: the model parts are the native parts", cell.name);
                assert_eq!(contents[at + 1]["role"], json!("user"), "{}", cell.name);
                let reply = contents[at + 1]["parts"].as_array().expect("parts");
                assert!(
                    reply.iter().any(|part| part["functionResponse"]["name"] == json!("read_file")),
                    "{} signed: no function response for read_file: {reply:?}",
                    cell.name
                );
            }
            other => panic!("{}: {} does not sign", cell.name, other.tag()),
        }
    }
}

#[test]
fn reasoning_text_is_display_only_on_the_wires_that_do_not_sign() {
    for cell in cells().into_iter().filter(|cell| !cell.signs) {
        let ex = exchange(&cell, Scenario::Signed);
        let said = outcome(&ex.first);
        assert_eq!(said.reasoning, Neutral::REASON, "{} display only: the reasoning text is shown", cell.name);
        assert_eq!(said.stop, Some(StopReason::ToolUse), "{} display only: the stop reason", cell.name);
        assert!(ex.first.result.as_ref().expect("ok").native.is_none(), "{} display only: a native turn was kept", cell.name);
        let seen = ex.built.fake.seen();
        let text = seen[1].body_text();
        assert!(!text.contains(Neutral::REASON), "{} display only: the reasoning text was sent back", cell.name);
        assert!(text.contains("alpha"), "{} display only: the result is missing", cell.name);
        assert!(
            text.contains(&ex.call.id) || text.contains(&ex.call.name),
            "{} display only: neither the call id nor the name is in the request",
            cell.name
        );
        let _ = (&ex.second, &ex.first_request, &ex.second_request);
    }
}

// ---- replay and pairing -----------------------------------------------------------------------------------

#[test]
fn a_native_turn_is_replayed_only_to_its_own_wire() {
    let producers = ["custom_anthropic", "custom_responses", "custom_gemini"];
    for name in producers {
        let producer = cell(name);
        let ex = exchange(&producer, Scenario::Signed);
        let assistant = assistant_of(producer.model, &ex.first);
        assert!(assistant.native.is_some(), "{name}: the producer kept a native turn");
        let signature = if producer.wire == Wire::OpenAiResponses { Neutral::ENC } else { Neutral::SIG };
        let mut request = ex.first_request.clone();
        request.messages.push(assistant);
        request.messages.push(support::result_message("t1", &ex.call.id, "alpha", false));
        for consumer in Wire::ALL {
            let origin = Origin::from_scope("http://127.0.0.1:1").expect("origin");
            let adapter = adapter_for(consumer, &origin);
            let caps = adapter.capabilities(&request.model, Locality::Loopback);
            let built = adapter
                .request(&request, &caps)
                .unwrap_or_else(|error| panic!("{name} to {}: the request was refused: {error}", consumer.tag()));
            let body = String::from_utf8_lossy(&built.body).into_owned();
            if consumer == producer.wire {
                assert_eq!(count_in(&body, signature), 1, "{name} to {}: its own wire gets the signature once", consumer.tag());
            } else {
                assert!(
                    !body.contains(Neutral::SIG) && !body.contains(Neutral::ENC),
                    "{name} to {}: a signature crossed to another wire",
                    consumer.tag()
                );
            }
        }
    }
}

#[test]
fn tool_results_pair_with_their_calls_in_the_next_request_on_every_wire() {
    for wire in Wire::ALL {
        let origin = Origin::from_scope("http://127.0.0.1:1").expect("origin");
        let adapter = adapter_for(wire, &origin);
        let request = pairing_request("local-model", "alpha", "beta");
        let caps = adapter.capabilities(&request.model, Locality::Loopback);
        let body = adapter.request(&request, &caps).unwrap_or_else(|error| panic!("{}: refused: {error}", wire.tag()));
        let text = String::from_utf8_lossy(&body.body).into_owned();
        assert_eq!(count_in(&text, "alpha"), 1, "{}: alpha occurs once", wire.tag());
        assert_eq!(count_in(&text, "beta"), 1, "{}: beta occurs once", wire.tag());
        assert!(text.find("alpha") < text.find("beta"), "{}: alpha comes before beta", wire.tag());
        match wire {
            Wire::OpenAiChat | Wire::Anthropic | Wire::OpenAiResponses => {
                for id in ["\"c1\"", "\"c2\""] {
                    assert_eq!(count_in(&text, id), 2, "{}: the call id {id} occurs in the call and in its result", wire.tag());
                }
            }
            Wire::Ollama | Wire::Gemini => {
                assert!(text.contains("read_file") && text.contains("list_dir"), "{}: both tool names", wire.tag());
            }
        }
        if wire == Wire::Anthropic {
            assert_eq!(count_in(&text, "is_error"), 1, "anthropic: the failure flag occurs once");
        }

        let kept = pairing_request("local-model", NOT_STORED, TRIMMED_RESULT);
        let body = adapter.request(&kept, &caps).unwrap_or_else(|error| panic!("{}: refused: {error}", wire.tag()));
        let text = String::from_utf8_lossy(&body.body).into_owned();
        assert_eq!(count_in(&text, NOT_STORED), 1, "{}: the stored-output text is an ordinary result", wire.tag());
        assert_eq!(count_in(&text, TRIMMED_RESULT), 1, "{}: the trimmed-output text is an ordinary result", wire.tag());
    }
}

#[test]
fn repeated_tool_call_ids_across_turns_never_lose_or_cross_a_result() {
    for wire in Wire::ALL {
        let origin = Origin::from_scope("http://127.0.0.1:1").expect("origin");
        let adapter = adapter_for(wire, &origin);
        let request = repeated_ids_request("local-model");
        let caps = adapter.capabilities(&request.model, Locality::Loopback);
        match adapter.request(&request, &caps) {
            Err(error) => {
                assert_eq!(error.class().as_str(), "invalid_request", "{}: the refusal class", wire.tag());
                let shown = format!("{error} {error:?}");
                for word in ["first-file", "second-file", "alpha-result", "beta-result"] {
                    assert!(!shown.contains(word), "{}: the refusal shows {word}", wire.tag());
                }
                println!("repeated ids: {}: refused", wire.tag());
            }
            Ok(built) => {
                let text = String::from_utf8_lossy(&built.body).into_owned();
                for word in ["first-file", "second-file", "alpha-result", "beta-result"] {
                    assert_eq!(count_in(&text, word), 1, "{}: {word} occurs once", wire.tag());
                }
                assert!(text.find("first-file") < text.find("second-file"), "{}: the calls are in order", wire.tag());
                assert!(text.find("alpha-result") < text.find("beta-result"), "{}: the results are in order", wire.tag());
                println!("repeated ids: {}: accepted", wire.tag());
            }
        }
    }
}

// ---- dispatch ---------------------------------------------------------------------------------------------

#[test]
fn the_dispatch_fixes_the_flavor_the_window_and_the_capabilities_of_each_origin() {
    // Ollama: the window of the capabilities is the one the request asks for.
    let ollama = cell("ollama");
    let built = rig(&ollama, vec![whole(Wire::Ollama, Scenario::Text)], Keys::None, HttpConfig::standard());
    let caps = holzwave_core::ai::provider::Provider::capabilities(&built.provider, ollama.model);
    assert_eq!(caps.context_tokens, Some(16384), "ollama: the window of the capabilities");
    let _ = run(&built.provider, &chat_request(&ollama, "hello"));
    let body = json_body("ollama", &built.fake.seen()[0]);
    assert_eq!(body["options"]["num_ctx"], json!(16384), "ollama: num_ctx of the captured body");

    // Every adapter can use tools for the model of its cell and echoes the locality it is given.
    let origin = Origin::from_scope("http://127.0.0.1:1").expect("origin");
    for cell in cells() {
        let adapter = adapter_for(cell.wire, &origin);
        for locality in [Locality::Loopback, Locality::Lan, Locality::Cloud] {
            let caps = adapter.capabilities(cell.model, locality);
            assert!(caps.tools, "{}: the model of the cell can use tools", cell.name);
            assert_eq!(caps.locality, locality, "{}: the locality is echoed", cell.name);
        }
    }

    // The first-party controls of Anthropic go to the first-party host only.
    let adaptive = "claude-opus-5-5";
    let thinking = |origin: &str| {
        let origin = Origin::from_scope(origin).expect("origin");
        let adapter = adapter_for(Wire::Anthropic, &origin);
        let mut request = chat_request(&cell("anthropic"), "hello");
        request.model = adaptive.to_owned();
        let caps = adapter.capabilities(adaptive, Locality::Cloud);
        let built = adapter.request(&request, &caps).expect("a request");
        let body: serde_json::Value = serde_json::from_slice(&built.body).expect("json");
        body.get("thinking").is_some()
    };
    assert!(thinking("https://api.anthropic.com:443"), "the vendor host gets the thinking member");
    for other in [
        "http://127.0.0.1:8080",
        "https://ai-gateway.vercel.sh:443",
        "https://api.anthropic.com:8443",
        "https://api.anthropic.com.example:443",
    ] {
        assert!(!thinking(other), "{other}: no thinking member away from the vendor host");
    }

    // The capabilities the service reads, one line per wire.
    for (wire, model) in [
        (Wire::Anthropic, adaptive),
        (Wire::OpenAiResponses, "gpt-5"),
        (Wire::Gemini, "gemini-test-1"),
        (Wire::OpenAiChat, "local-model"),
        (Wire::Ollama, "llama-test"),
    ] {
        let origin = if wire == Wire::Anthropic {
            Origin::from_scope("https://api.anthropic.com:443").expect("origin")
        } else {
            Origin::from_scope("http://127.0.0.1:1").expect("origin")
        };
        let caps = adapter_for(wire, &origin).capabilities(model, Locality::Cloud);
        let size = |value: Option<u32>| value.map_or("none".to_owned(), |number| number.to_string());
        println!(
            "capabilities {}: tools {} reasoning {} context {} output {}",
            wire.tag(),
            caps.tools,
            caps.reasoning,
            size(caps.context_tokens),
            size(caps.max_output_tokens)
        );
    }
}
