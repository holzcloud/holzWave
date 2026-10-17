//! The provider matrix, failures and destinations: one class per failure on every wire, bounded streams, Stop,
//! hostile and silent local servers, a name that changes its answer, the local-only switch and the gateway rules, with no
//! outside network.
//!
//! Every network row talks to an in-process server or to a stub resolver, and every wait is bounded. The rows that
//! measure time hold the timing guard and assert the contract bounds, not a tuned number.

mod common;
#[path = "ai_matrix/streams.rs"]
mod streams;
#[path = "ai_matrix/support.rs"]
mod support;

use std::cell::Cell as StdCell;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::fake_http::{Ending, Reply};
use holzwave_core::ai::http::{Http, HttpConfig, Origin};
use holzwave_core::ai::profiles::{AiConfig, KeySlot, ResolveError};
use holzwave_core::ai::provider::{AiError, CancelToken, Flow, Provider, StreamEvent};
use holzwave_core::ai::service::dispatch::adapter_for;
use holzwave_core::ai::wire::Wire;
use serde_json::json;
use streams::{Hostile, error_stream, flood, hostile_tool_stream, stream};
use support::{
    ARG_CANARY, CANARY_TEXT, Case, Cell, GATEWAY_HEADER, HTTP_ROWS, Keys, Neutral, RigOptions, STREAM_ROWS, Scenario,
    StubResolver, assert_clean, canary_gateway_key, canary_key, catalog_specs, cell, cells, chat_request, closed_port,
    fast_retry, header_names, http_with_stub, json_body, key_header, members, outcome, reply_for, rejection_body, rig,
    run, save_profile, short_idle, store_keys, test_vault, timing_guard, try_rig_with, upsert_again,
};

/// The five Custom cells, one per wire.
fn custom_cells() -> Vec<Cell> {
    ["custom_chat", "custom_ollama", "custom_anthropic", "custom_responses", "custom_gemini"].map(cell).to_vec()
}

fn has_tool_call(events: &[StreamEvent]) -> bool {
    events.iter().any(|event| matches!(event, StreamEvent::ToolCall(_)))
}

// ---- HTTP failures ----------------------------------------------------------------------------------------

#[test]
fn http_rejections_are_the_same_class_on_every_wire() {
    for cell in custom_cells() {
        for row in &HTTP_ROWS {
            let label = format!("{} {:?}", cell.name, row.case);
            let (status, body) = rejection_body(cell.wire, row.case);
            let mut reply = Reply::status(status).body(body);
            if row.case == Case::RateLimit {
                reply = reply.header("retry-after", "7");
            }
            let built = rig(&cell, vec![reply], Keys::Provider, fast_retry());
            let result = run(&built.provider, &chat_request(&cell, "hello"));
            assert!(result.events.is_empty(), "{label}: an event was delivered");
            let error = result.result.err().unwrap_or_else(|| panic!("{label}: the turn did not fail"));
            assert_eq!(error.class().as_str(), row.class, "{label}: {error}");
            match row.case {
                Case::RateLimit => assert_eq!(
                    error,
                    AiError::RateLimited { retry_after: Some(Duration::from_secs(7)) },
                    "{label}: the delay that was asked for"
                ),
                Case::Server => assert!(error.to_string().contains("500"), "{label}: the status is shown: {error}"),
                _ => {}
            }
            let port = built.fake.port().to_string();
            assert_clean(&label, &error, &[canary_key(), &port]);
        }
    }

    // A context that does not fit, in the form each dialect uses for it.
    let overflow: [(Wire, u16, serde_json::Value); 4] = [
        (
            Wire::OpenAiChat,
            400,
            json!({ "error": { "message": "This model's maximum context length is 8192 tokens.", "type": "invalid_request_error", "code": "context_length_exceeded" } }),
        ),
        (
            Wire::Anthropic,
            400,
            json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "prompt is too long: 250000 tokens > 200000 maximum" } }),
        ),
        (
            Wire::Gemini,
            400,
            json!({ "error": { "code": 400, "message": "The input token count (1100000) exceeds the maximum number of tokens allowed (1048576).", "status": "INVALID_ARGUMENT" } }),
        ),
        (
            Wire::OpenAiResponses,
            400,
            json!({ "error": { "message": "Your input exceeds the context window of this model.", "type": "invalid_request_error", "code": "context_length_exceeded" } }),
        ),
    ];
    for (wire, status, body) in overflow {
        let cell = custom_cells().into_iter().find(|cell| cell.wire == wire).expect("a custom cell");
        let built = rig(&cell, vec![Reply::status(status).body(body.to_string())], Keys::Provider, fast_retry());
        let result = run(&built.provider, &chat_request(&cell, "hello"));
        let class = result.result.as_ref().err().map_or("ok", |error| error.class().as_str());
        println!("context overflow: {}: {class}", wire.tag());
        assert_eq!(class, "context_length", "{}: a context that does not fit", wire.tag());
    }
}

#[test]
fn in_stream_errors_are_the_same_class_on_every_wire_and_never_echo_server_text() {
    for (wire, kind, class) in STREAM_ROWS {
        let cell = custom_cells().into_iter().find(|cell| cell.wire == wire).expect("a custom cell");
        let label = format!("{} {kind:?}", wire.tag());
        let streamed = error_stream(wire, kind).unwrap_or_else(|| panic!("{label}: no stream"));
        let built = rig(&cell, vec![reply_for(wire, vec![streamed.bytes])], Keys::Provider, fast_retry());
        let result = run(&built.provider, &chat_request(&cell, "hello"));
        assert_eq!(outcome(&result).text, "partial", "{label}: the text before the error was delivered");
        assert!(!has_tool_call(&result.events), "{label}: a call followed the error");
        let error = result.result.err().unwrap_or_else(|| panic!("{label}: the turn did not fail"));
        assert_eq!(error.class().as_str(), class, "{label}: {error}");
        assert_clean(&label, &error, &[CANARY_TEXT, canary_key()]);
        assert_eq!(built.fake.accepted(), 1, "{label}: a stream that started is not repeated");
    }
}

#[test]
fn retries_and_attempts_are_the_same_on_every_wire() {
    let _guard = timing_guard();
    for cell in custom_cells() {
        for row in &HTTP_ROWS {
            let label = format!("{} {:?}", cell.name, row.case);
            let (status, body) = rejection_body(cell.wire, row.case);
            let mut reply = Reply::status(status).body(body);
            if row.case == Case::RateLimit {
                reply = reply.header("retry-after", "7");
            }
            let built = rig(&cell, vec![reply], Keys::Provider, fast_retry());
            let _ = run(&built.provider, &chat_request(&cell, "hello"));
            assert_eq!(built.fake.accepted(), row.connections, "{label}: connections");
            assert_eq!(built.http.attempts(), row.connections as u64, "{label}: attempts");
        }

        // A port nobody listens on is tried three times and ends in a network error that shows no address.
        let port = closed_port();
        let options = RigOptions {
            keys: Keys::Provider,
            http: Some(Arc::new(Http::new(fast_retry()))),
            origin: Some(format!("http://127.0.0.1:{port}")),
            ..RigOptions::default()
        };
        let built = try_rig_with(&cell, Vec::new(), options).expect("a rig");
        let result = run(&built.provider, &chat_request(&cell, "hello"));
        let error = result.result.err().expect("the turn failed");
        assert_eq!(error.class().as_str(), "network", "{}: a closed port: {error}", cell.name);
        assert_eq!(built.http.attempts(), 3, "{}: a closed port is tried three times", cell.name);
        assert_clean(cell.name, &error, &[&port.to_string(), canary_key()]);

        // A stream that delivered one event and then dropped is never repeated.
        let streamed = stream(cell.wire, Scenario::Text);
        let reply = reply_for(cell.wire, vec![streamed.bytes[..streamed.first_delta_end].to_vec()]).ending(Ending::Drop);
        let built = rig(&cell, vec![reply], Keys::Provider, fast_retry());
        let result = run(&built.provider, &chat_request(&cell, "hello"));
        assert!(
            result.events.iter().any(|event| matches!(event, StreamEvent::TextDelta(_))),
            "{}: the event before the drop was delivered",
            cell.name
        );
        let error = result.result.err().unwrap_or_else(|| panic!("{}: a dropped stream ended Ok", cell.name));
        assert!(matches!(error.class().as_str(), "network" | "protocol"), "{}: {error}", cell.name);
        assert_eq!(built.fake.accepted(), 1, "{}: nothing is repeated after the first byte", cell.name);
        assert_eq!(built.http.attempts(), 1, "{}: one attempt after the first byte", cell.name);
    }
}

// ---- bounds and Stop --------------------------------------------------------------------------------------

#[test]
fn a_runaway_stream_and_an_oversize_event_are_cut_on_every_wire() {
    let _guard = timing_guard();
    for cell in custom_cells() {
        // 20,000 events against a bound of 64 KiB: protocol, after some events, and the socket is closed.
        let config = HttpConfig { max_stream_bytes: 64 * 1024, ..fast_retry() };
        let reply = reply_for(cell.wire, flood(cell.wire, 20_000)).ending(Ending::Hold);
        let built = rig(&cell, vec![reply], Keys::Provider, config);
        let result = run(&built.provider, &chat_request(&cell, "hello"));
        assert!(!result.events.is_empty(), "{}: nothing was delivered before the bound", cell.name);
        assert!(!has_tool_call(&result.events), "{}: a call came from a runaway stream", cell.name);
        let error = result.result.err().unwrap_or_else(|| panic!("{}: a runaway stream ended Ok", cell.name));
        assert_eq!(error.class().as_str(), "protocol", "{}: {error}", cell.name);
        assert!(built.fake.wait_closed(0, Duration::from_secs(1)).is_some(), "{}: the socket stays open", cell.name);

        // One event of 3 MiB with no line end.
        let mut bytes = if cell.wire == Wire::Ollama { Vec::new() } else { b"data: ".to_vec() };
        bytes.extend(std::iter::repeat_n(b'x', 3 * 1024 * 1024));
        let chunks: Vec<Vec<u8>> = bytes.chunks(64 * 1024).map(<[u8]>::to_vec).collect();
        let built = rig(&cell, vec![reply_for(cell.wire, chunks)], Keys::Provider, fast_retry());
        let result = run(&built.provider, &chat_request(&cell, "hello"));
        assert!(result.events.is_empty(), "{}: the callback was called for an oversize event", cell.name);
        let error = result.result.err().unwrap_or_else(|| panic!("{}: an oversize event ended Ok", cell.name));
        assert_eq!(error.class().as_str(), "protocol", "{}: {error}", cell.name);
    }
}

#[test]
fn every_wire_stops_within_250_ms_of_a_cancel() {
    let _guard = timing_guard();
    for cell in custom_cells() {
        let streamed = stream(cell.wire, Scenario::Text);
        let prefix = streamed.bytes[..streamed.first_delta_end].to_vec();
        for by_token in [true, false] {
            let label = format!("{} {}", cell.name, if by_token { "token" } else { "flow" });
            let reply = reply_for(cell.wire, vec![prefix.clone()]).ending(Ending::Hold);
            let built = rig(&cell, vec![reply], Keys::Provider, HttpConfig::standard());
            let token = CancelToken::new();
            let cancelled_at: StdCell<Option<Instant>> = StdCell::new(None);
            let result = built.provider.stream(
                &chat_request(&cell, "hello"),
                &mut |event| {
                    if matches!(event, StreamEvent::TextDelta(_)) && cancelled_at.get().is_none() {
                        cancelled_at.set(Some(Instant::now()));
                        if by_token {
                            token.cancel();
                            return Flow::Continue;
                        }
                        return Flow::Stop;
                    }
                    Flow::Continue
                },
                &token,
            );
            let returned = Instant::now();
            let cancelled_at = cancelled_at.get().unwrap_or_else(|| panic!("{label}: no text delta arrived"));
            let error = result.err().unwrap_or_else(|| panic!("{label}: the turn ended Ok"));
            assert_eq!(error, AiError::Cancelled, "{label}");
            let returned_after = returned.saturating_duration_since(cancelled_at);
            assert!(returned_after < Duration::from_millis(250), "{label}: the call returned after {returned_after:?}");
            let closed = built
                .fake
                .wait_closed(0, Duration::from_secs(2))
                .unwrap_or_else(|| panic!("{label}: the socket was not closed"));
            let closed_after = closed.saturating_duration_since(cancelled_at);
            assert!(closed_after < Duration::from_millis(250), "{label}: the socket closed after {closed_after:?}");
            if by_token {
                println!("cancel return {}: {}", cell.wire.tag(), returned_after.as_millis());
                println!("socket close {}: {}", cell.wire.tag(), closed_after.as_millis());
            }
        }
    }
}

// ---- local servers ----------------------------------------------------------------------------------------

#[test]
fn invalid_tool_json_from_a_local_server_fails_the_turn_without_a_call() {
    let chat = ["lm_studio", "llama_cpp", "litellm", "custom_chat"];
    let chat_cases = [Hostile::CutObject, Hostile::Array, Hostile::Number, Hostile::DoubleEncoded, Hostile::LengthStop];
    let ollama = ["ollama", "custom_ollama"];
    let ollama_cases = [Hostile::StringArgs, Hostile::Array, Hostile::Number, Hostile::NoName];
    let rows = chat.iter().map(|name| (name, &chat_cases[..])).chain(ollama.iter().map(|name| (name, &ollama_cases[..])));
    for (name, cases) in rows {
        let cell = cell(name);
        for kind in cases {
            let label = format!("{name} {kind:?}");
            let streamed = hostile_tool_stream(cell.wire, *kind).unwrap_or_else(|| panic!("{label}: no stream"));
            let built = rig(&cell, vec![reply_for(cell.wire, vec![streamed.bytes])], Keys::None, fast_retry());
            let mut request = chat_request(&cell, "list");
            request.tools = catalog_specs();
            let result = run(&built.provider, &request);
            assert!(!has_tool_call(&result.events), "{label}: a call was delivered");
            let error = result.result.err().unwrap_or_else(|| panic!("{label}: the turn ended Ok"));
            assert_eq!(error.class().as_str(), "protocol", "{label}: {error}");
            assert_clean(&label, &error, &[ARG_CANARY]);
            assert_eq!(built.fake.accepted(), 1, "{label}: one connection");
        }
    }
}

#[test]
fn a_silent_server_and_a_stalled_stream_end_in_a_timeout_on_every_local_preset() {
    let _guard = timing_guard();
    for name in ["ollama", "lm_studio", "llama_cpp", "litellm", "custom_chat", "custom_ollama"] {
        let cell = cell(name);
        // Nothing is ever answered.
        let built = rig(&cell, vec![Reply::Hang], Keys::None, short_idle(300));
        let started = Instant::now();
        let result = run(&built.provider, &chat_request(&cell, "hello"));
        let error = result.result.err().unwrap_or_else(|| panic!("{name}: a silent server ended Ok"));
        assert_eq!(error.class().as_str(), "timeout", "{name}: {error}");
        assert!(result.events.is_empty(), "{name}: an event came from a silent server");
        assert!(started.elapsed() < Duration::from_secs(3), "{name}: the timeout took {:?}", started.elapsed());
        assert_eq!(built.fake.accepted(), 1, "{name}: a timeout is not retried");

        // The first delta arrives and then the server holds the socket.
        let streamed = stream(cell.wire, Scenario::Text);
        let reply = reply_for(cell.wire, vec![streamed.bytes[..streamed.first_delta_end].to_vec()]).ending(Ending::Hold);
        let built = rig(&cell, vec![reply], Keys::None, short_idle(300));
        let started = Instant::now();
        let result = run(&built.provider, &chat_request(&cell, "hello"));
        assert_eq!(outcome(&result).text, Neutral::TEXT_PARTS[0], "{name}: the delta before the stall was delivered");
        let error = result.result.err().unwrap_or_else(|| panic!("{name}: a stalled stream ended Ok"));
        assert_eq!(error.class().as_str(), "timeout", "{name}: {error}");
        assert!(started.elapsed() < Duration::from_secs(3), "{name}: the stall took {:?}", started.elapsed());
        assert_eq!(built.fake.accepted(), 1, "{name}: a stall is not retried");
    }
}

#[test]
fn the_standard_idle_limit_covers_a_slow_model_load() {
    let _guard = timing_guard();
    assert!(
        HttpConfig::standard().idle_timeout >= Duration::from_secs(60),
        "a model that is still loading may stay silent for a minute"
    );
    let service = include_str!("../src/ai/service.rs");
    assert!(
        service.contains("Http::new(HttpConfig::standard())"),
        "the service builds its chokepoint from the standard settings"
    );

    for name in ["ollama", "lm_studio"] {
        let cell = cell(name);
        let bytes = stream(cell.wire, Scenario::Text).bytes;
        let pieces = support::split_inside_characters(&bytes);
        // Silence shorter than the limit is not cut, and there is no deadline for the whole turn.
        let slow = reply_for(cell.wire, pieces.clone()).gap(Duration::from_millis(400));
        let built = rig(&cell, vec![slow], Keys::None, short_idle(1000));
        let started = Instant::now();
        let result = run(&built.provider, &chat_request(&cell, "hello"));
        assert!(result.result.is_ok(), "{name}: a slow stream was cut: {:?}", result.result.as_ref().err());
        assert_eq!(outcome(&result).text, Neutral::TEXT, "{name}: the slow answer");
        assert!(started.elapsed() >= Duration::from_millis(800), "{name}: the stream was not slow");

        // Silence longer than the limit is a timeout.
        let first = pieces[0].clone();
        let rest: Vec<u8> = pieces[1..].concat();
        let stalled = reply_for(cell.wire, vec![first, rest]).gap(Duration::from_millis(1500));
        let built = rig(&cell, vec![stalled], Keys::None, short_idle(1000));
        let result = run(&built.provider, &chat_request(&cell, "hello"));
        let error = result.result.err().unwrap_or_else(|| panic!("{name}: a stall of 1.5 s ended Ok"));
        assert_eq!(error.class().as_str(), "timeout", "{name}: {error}");
    }
}

#[test]
fn a_name_is_pinned_once_and_refused_when_its_answer_turns_bad() {
    let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let metadata = IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254));
    let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9));
    for name in ["ollama", "lm_studio"] {
        let cell = cell(name);
        let stub = StubResolver::new(vec![vec![loopback], vec![metadata], vec![loopback, metadata], vec![lan], vec![loopback]]);
        let http = http_with_stub(fast_retry(), &stub);
        let options = RigOptions {
            keys: Keys::None,
            http: Some(http),
            origin: Some("http://rebind.localhost:{port}".to_owned()),
            ..RigOptions::default()
        };
        let reply = reply_for(cell.wire, vec![stream(cell.wire, Scenario::Text).bytes]);
        let built = try_rig_with(&cell, vec![reply], options).expect("a rig");
        // Building the provider and asking it what it is look nothing up.
        let _ = built.provider.capabilities(cell.model);
        let _ = built.provider.locality();
        assert_eq!(stub.calls(), 0, "{name}: construction looked a name up");

        let expected = [true, false, false, false, true];
        for (step, ok) in expected.into_iter().enumerate() {
            let label = format!("{name} step {}", step + 1);
            let result = run(&built.provider, &chat_request(&cell, "hello"));
            assert_eq!(stub.calls(), step + 1, "{label}: one lookup per request, names {:?}", stub.names());
            if ok {
                assert!(result.result.is_ok(), "{label}: a good answer was refused: {:?}", result.result.as_ref().err());
            } else {
                let error = result.result.err().unwrap_or_else(|| panic!("{label}: a bad answer was served"));
                assert_eq!(error.class().as_str(), "policy", "{label}: {error}");
                for address in ["169.254.169.254", "192.168.1.9", "127.0.0.1"] {
                    assert!(!error.to_string().contains(address), "{label}: the refusal shows {address}");
                }
            }
        }
        assert_eq!(built.fake.accepted(), 2, "{name}: the refused requests opened no connection");
        let seen = built.fake.seen();
        let host = format!("rebind.localhost:{}", built.fake.port());
        assert_eq!(seen.len(), 2, "{name}: two requests reached the server");
        for request in &seen {
            assert_eq!(request.header("host"), Some(host.as_str()), "{name}: the host header names the host");
        }
        assert!(stub.names().iter().all(|looked| looked == "rebind.localhost"), "{name}: {:?}", stub.names());
    }
}

// ---- destinations -----------------------------------------------------------------------------------------

#[test]
fn local_only_blocks_lan_and_cloud_destinations_before_any_connection() {
    let cloud = [
        "anthropic",
        "openai",
        "gemini",
        "openrouter",
        "vercel_chat",
        "vercel_anthropic",
        "cloudflare_chat",
        "cloudflare_anthropic",
        "cloudflare_responses",
        "cloudflare_gemini",
        "custom_chat",
    ];
    for name in cloud {
        let cell = cell(name);
        let keys = if cell.cloudflare() { Keys::Both } else { Keys::Provider };
        let stub = StubResolver::new(vec![vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9))]]);
        let http = http_with_stub(fast_retry(), &stub);
        let options = RigOptions {
            keys,
            http: Some(http),
            origin: Some(cell.real_origin().to_owned()),
            local_only: true,
            acknowledge: true,
            ..RigOptions::default()
        };
        let class = match try_rig_with(&cell, Vec::new(), options) {
            Err(error) => AiError::from(error).class().as_str(),
            Ok(built) => {
                let result = run(&built.provider, &chat_request(&cell, "hello"));
                assert_eq!(built.http.attempts(), 0, "{name}: a request was sent");
                assert!(result.events.is_empty(), "{name}: an event was delivered");
                result.result.err().map_or("ok", |error| error.class().as_str())
            }
        };
        assert_eq!(class, "policy", "{name}: local-only did not refuse the real address");
        assert!(stub.calls() <= 1, "{name}: looked up {} times: {:?}", stub.calls(), stub.names());
    }

    // A literal address on the local network is refused with no lookup at all.
    let ollama = cell("ollama");
    let stub = StubResolver::new(vec![vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20))]]);
    let options = RigOptions {
        keys: Keys::None,
        http: Some(http_with_stub(fast_retry(), &stub)),
        origin: Some("http://192.168.1.20:11434".to_owned()),
        local_only: true,
        allow_lan_http: true,
        ..RigOptions::default()
    };
    let built = try_rig_with(&ollama, Vec::new(), options).expect("a rig");
    let result = run(&built.provider, &chat_request(&ollama, "hello"));
    let error = result.result.err().expect("a LAN literal is refused under local-only");
    assert_eq!(error.class().as_str(), "policy", "{error}");
    assert_eq!(built.http.attempts(), 0, "a LAN literal opened a connection");
    assert_eq!(stub.calls(), 0, "a literal address was looked up");

    // The same switch lets the loopback cells stream.
    for name in ["ollama", "lm_studio", "llama_cpp", "litellm", "custom_chat"] {
        let cell = cell(name);
        let reply = reply_for(cell.wire, vec![stream(cell.wire, Scenario::Text).bytes]);
        let options = RigOptions { keys: Keys::None, local_only: true, ..RigOptions::default() };
        let built = try_rig_with(&cell, vec![reply], options).expect("a rig");
        let result = run(&built.provider, &chat_request(&cell, "hello"));
        assert!(result.result.is_ok(), "{name}: local-only cut a loopback server: {:?}", result.result.as_ref().err());
        assert_eq!(outcome(&result).text, Neutral::TEXT, "{name}: the answer");
    }
}

#[test]
fn a_gateway_needs_its_acknowledgement_once_per_host() {
    for name in ["openrouter", "vercel_chat", "cloudflare_chat", "litellm", "custom_chat"] {
        let cell = cell(name);
        let address = format!("{}{}", cell.real_origin(), cell.base_suffix);
        let vault = test_vault();
        let mut config = AiConfig::default();
        let options = RigOptions::keyed(Keys::Provider);
        let id = save_profile(&mut config, &vault, &cell, &address, &options);

        assert_eq!(config.resolve(&id, &vault, false).err(), Some(ResolveError::DisclosureNeeded), "{name}: before the acknowledgement");
        let views = config.views(&vault);
        let disclosure = views[0].disclosure.as_ref().unwrap_or_else(|| panic!("{name}: the view carries no disclosure"));
        assert!(address.contains(&disclosure.host), "{name}: the disclosure names the host: {}", disclosure.host);

        upsert_again(&mut config, &id, &cell, &address, true);
        let resolved = config.resolve(&id, &vault, false).unwrap_or_else(|error| panic!("{name}: after the acknowledgement: {error}"));
        assert!(config.views(&vault)[0].disclosure.is_none(), "{name}: the disclosure stays after the acknowledgement");
        let stub = StubResolver::new(vec![vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9))]]);
        let http = http_with_stub(fast_retry(), &stub);
        let origin = Origin::from_scope(resolved.scope()).expect("origin");
        let provider = resolved.into_provider(adapter_for(cell.wire, &origin), Arc::clone(&http));
        let _ = provider.capabilities(cell.model);
        assert_eq!(stub.calls(), 0, "{name}: building the provider looked a name up");
        assert_eq!(http.attempts(), 0, "{name}: building the provider sent a request");

        // Another host asks again, and the key of the old host is not used for it.
        let moved = format!("https://moved.example.net{}", cell.base_suffix);
        upsert_again(&mut config, &id, &cell, &moved, false);
        assert_eq!(config.resolve(&id, &vault, false).err(), Some(ResolveError::DisclosureNeeded), "{name}: a new host");
        upsert_again(&mut config, &id, &cell, &moved, true);
        assert_eq!(
            config.resolve(&id, &vault, false).err(),
            Some(ResolveError::KeyScope(KeySlot::Provider)),
            "{name}: the key of the old host"
        );
        store_keys(&mut config, &vault, &id, Keys::Provider, &cell);
        assert!(config.resolve(&id, &vault, false).is_ok(), "{name}: the key entered again for the new host");
    }

    // The named providers and the loopback presets never ask.
    for name in ["anthropic", "openai", "gemini", "ollama", "lm_studio", "llama_cpp"] {
        let cell = cell(name);
        let vault = test_vault();
        let mut config = AiConfig::default();
        let id = save_profile(&mut config, &vault, &cell, "", &RigOptions::keyed(Keys::Provider));
        assert!(config.resolve(&id, &vault, false).is_ok(), "{name}: a default address needs no acknowledgement");
        assert!(config.views(&vault)[0].disclosure.is_none(), "{name}: a default address has no disclosure");
    }
}

#[test]
fn gateway_requests_carry_no_identifying_header_and_openrouter_denies_data_collection() {
    let forbidden = ["http-referer", "x-title", "referer", "origin", "x-forwarded-for", "cookie"];
    for cell in cells().into_iter().filter(|cell| cell.gateway) {
        let keys = if cell.cloudflare() { Keys::Both } else { Keys::Provider };
        let built = rig(&cell, vec![reply_for(cell.wire, vec![stream(cell.wire, Scenario::Tool).bytes]), reply_for(cell.wire, vec![stream(cell.wire, Scenario::Text).bytes])], keys, HttpConfig::standard());
        let mut first = chat_request(&cell, "list");
        first.tools = catalog_specs();
        let one = run(&built.provider, &first);
        assert!(one.result.is_ok(), "{}: the tool turn failed", cell.name);
        let call = outcome(&one).calls[0].clone();
        let second = support::follow_up(&first, support::assistant_of(cell.model, &one), &call, "alpha");
        let two = run(&built.provider, &second);
        assert!(two.result.is_ok(), "{}: the follow-up failed", cell.name);

        let (key_name, _) = key_header(cell.wire);
        let mut allowed: Vec<&str> = support::BASELINE.to_vec();
        allowed.push(key_name);
        if cell.wire == Wire::Anthropic {
            allowed.push("anthropic-version");
        }
        if cell.cloudflare() {
            allowed.push(GATEWAY_HEADER);
        }
        let seen = built.fake.seen();
        assert_eq!(seen.len(), 2, "{}: two requests", cell.name);
        for (index, request) in seen.iter().enumerate() {
            for name in header_names(request) {
                assert!(allowed.contains(&name.as_str()), "{} request {index}: the header {name} is not allowed", cell.name);
                assert!(!forbidden.contains(&name.as_str()), "{} request {index}: the header {name} identifies", cell.name);
                let identifying = name.split('-').any(|part| matches!(part, "id" | "session" | "uuid" | "device" | "machine" | "client"));
                assert!(!identifying, "{} request {index}: the header {name} looks like an identifier", cell.name);
            }
            let body = json_body(cell.name, request);
            if cell.preset_tag == "openrouter" {
                assert_eq!(body["provider"], json!({ "data_collection": "deny" }), "{} request {index}", cell.name);
            } else {
                assert!(!members(&body).contains("provider"), "{} request {index}: a provider member", cell.name);
            }
        }
        let _ = canary_gateway_key();
    }
}
