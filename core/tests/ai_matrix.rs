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
use holzwave_core::ai::http::{HttpConfig, Target};
use holzwave_core::ai::profiles::{KeyNeed, PRESETS, Preset, ResolveError, auth_header_for};
use holzwave_core::ai::provider::{AiError, Provider, StreamEvent};
use holzwave_core::ai::types::{Locality, StopReason, Usage};
use holzwave_core::ai::wire::Wire;
use serde_json::json;
use streams::stream;
use support::{
    ANTHROPIC_VERSION, BASELINE, Cell, GATEWAY_HEADER, KEY_HEADERS, KeyRule, Keys, Neutral, Scenario, Side, canary_gateway_key,
    canary_key, cell, cells, chat_request, header_names, json_body, key_header, members, outcome, reply_for, rig, run,
    split_inside_characters, texts_of, try_rig,
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
