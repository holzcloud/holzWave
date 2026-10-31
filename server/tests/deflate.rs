//! The switch for WebSocket compression: what the configured `transport.compression` does to a real handshake on a real TLS
//! socket, the one sentence that refuses a value this build cannot honour, and the guard that ties the shipped default to
//! the verdict of the measurement.
//!
//! The tracer tests come first; the filter `tracer` selects exactly them. Everything here needs the `in-process-workers`
//! feature, and the page must have been built (`npm --prefix app run build`) because the listener embeds nothing else.
#![cfg(feature = "in-process-workers")]

#[path = "support/deflate_env.rs"]
mod deflate_env;
#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/ws_env.rs"]
mod ws_env;

use edge::ORIGIN;
use holzwave_server::config::Compression;
use holzwave_server::http::transport;
use holzwave_server::http::ws::SocketSettings;
use holzwave_server::supervisor::serve::{Overrides, assemble};
use serde_json::{Value, json};
use ws_env::{PATIENCE, TEXT, WsEnv, WsOptions, masked_frame};

/// The browsers' offer.
const BROWSER_OFFER: &str = "permessage-deflate; client_max_window_bits";

/// The numbered messages of the burst.
const BURST: u64 = 2_000;

/// The commands in flight at once.
const ROUND: u64 = 400;

/// Room for the whole burst in one go: the rate rule is covered by the tests of the endpoint, not here.
fn wide(settings: &mut SocketSettings) {
    settings.burst = 10_000;
    settings.per_second = 10_000;
}

// ---- the tracer ---------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tracer_the_handshake_with_off_carries_no_extension_header() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    for offer in [BROWSER_OFFER, "permessage-deflate", "permessage-deflate; server_max_window_bits=9, permessage-deflate", "x-unknown"] {
        let request = deflate_env::offering(&alice.signed.cookie, ORIGIN, offer);
        let mut socket = env.try_connect_request(request).await.unwrap_or_else(|r| panic!("{offer}: answered {}", r.status));
        assert!(
            deflate_env::answer_values(&socket.handshake, "sec-websocket-extensions").is_empty(),
            "an extension was offered back for {offer:?}: {:?}",
            socket.handshake
        );
        let first = socket.read_json(PATIENCE).await.expect("the first message");
        assert_eq!(first["event"], "hw-session");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_burst_of_2000_numbered_commands_is_answered_complete_and_in_order() {
    let env = WsEnv::start_with(WsOptions { tune: Some(wide), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let request = deflate_env::offering(&alice.signed.cookie, ORIGIN, BROWSER_OFFER);
    let mut socket = env.try_connect_request(request).await.unwrap_or_else(|r| panic!("answered {}", r.status));
    // The supervisor queues up to a thousand commands for a socket; the burst goes out in rounds below that, each round in one
    // write, so every answer is a result and none is the refusal of a full queue.
    let mut seen = Vec::new();
    for round in 0..BURST / ROUND {
        let mut bytes = Vec::new();
        for id in round * ROUND + 1..=(round + 1) * ROUND {
            bytes.extend(masked_frame(0x80 | TEXT, format!(r#"{{"id":{id},"cmd":"get_layout","args":{{}}}}"#).as_bytes()));
        }
        socket.send_raw(&bytes).await;
        let mut got = 0;
        while got < ROUND {
            let message = socket.read_json(PATIENCE).await.expect("the burst is answered");
            if let Some(id) = message.get("id").and_then(Value::as_u64) {
                assert!(message.get("ok").is_some(), "request {id} failed: {message}");
                seen.push(id);
                got += 1;
            }
        }
    }
    let wanted: Vec<u64> = (1..=BURST).collect();
    assert_eq!(seen, wanted, "every answer came, once, in the order of the requests");
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_frame_with_the_compression_bit_closes_the_socket_with_1002() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let request = deflate_env::offering(&alice.signed.cookie, ORIGIN, BROWSER_OFFER);
    let mut socket = env.try_connect_request(request).await.unwrap_or_else(|r| panic!("answered {}", r.status));
    let first = socket.read_json(PATIENCE).await.expect("the first message");
    assert_eq!(first["event"], "hw-session");
    // FIN, RSV1 and the text opcode: a message that claims to be compressed.
    socket.send_raw(&masked_frame(0x80 | 0x40 | TEXT, br#"{"id":1,"cmd":"get_layout","args":{}}"#)).await;
    let (code, _) = socket.close_code(PATIENCE).await.expect("a close message");
    assert_eq!(code, 1002, "a protocol error");
    // The server answers the next connection normally.
    let mut again = env.connect_as(&alice.signed).await;
    let view = again.ok("get_layout", "{}").await;
    assert!(view.get("focus").is_some(), "{view}");
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_deflate_is_refused_by_prepare_and_by_assemble_with_one_sentence() {
    let file = deflate_env::minimal(Some(json!({"compression": "deflate"})));
    let refused = deflate_env::prepare_text(&file).err().expect("the configuration check refuses deflate");
    let sentence = refused.sentence().to_owned();
    assert!(sentence.contains("transport.compression"), "{sentence}");
    assert!(sentence.contains("not available in this build"), "{sentence}");
    assert!(!sentence.contains('\n'));

    // The same words come from the selection point and from the assembly.
    assert_eq!(transport::select(Compression::Deflate).err(), Some(transport::check(Compression::Deflate).expect_err("refused")));
    assert_eq!(transport::select(Compression::Deflate).err(), Some(sentence.as_str()));
    let mut prepared = deflate_env::prepare_text(&deflate_env::minimal(None)).expect("a file without the key is valid");
    prepared.transport.compression = Compression::Deflate;
    let refusal = assemble(prepared, Overrides::default()).await.err().expect("the assembly refuses deflate");
    assert_eq!(refusal.sentence(), sentence);

    // `off` is accepted, written or not.
    for transport in [None, Some(json!({})), Some(json!({"compression": "off"}))] {
        let prepared = deflate_env::prepare_text(&deflate_env::minimal(transport)).expect("off is valid");
        assert_eq!(prepared.transport.compression, Compression::Off);
    }
}
