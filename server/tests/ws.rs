//! The WebSocket endpoint over a real TLS listener: the upgrade and its refusals, the first message, a command in a real
//! worker with a real shell, the limits of a socket, the liveness of a connection and the end of a session.
//!
//! The tracer tests come first; the filter `tracer` selects exactly them. Everything here needs the `in-process-workers`
//! feature, and the page must have been built (`npm --prefix app run build`) because the listener embeds nothing else.
#![cfg(feature = "in-process-workers")]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::collections::BTreeSet;
use std::time::Duration;

use edge::{ABSOLUTE, IDLE, ORIGIN};
use edge_tls::Request;
use holzwave_server::auth::session::{Ended, handle_of};
use holzwave_server::http::transport::{Inbound, TransportError};
use holzwave_server::http::ws::{ANSWER_TOO_LARGE, ANSWER_TOO_MANY, ANSWER_UNKNOWN, Bucket, Budget, PortError};
use holzwave_server::store::{AccountId, AccountState};
use serde_json::{Value, json};
use tokio::time::Instant;
use ws_env::{BINARY, CLOSE, CONTINUATION, Got, Incoming, PATIENCE, PING, Rig, Sent, TEXT, WsEnv, WsOptions, focus, masked_frame};

const SECOND: Duration = Duration::from_secs(1);

/// The keys of the golden file `protocol.json` under `section`.
fn golden_keys(section: &str) -> BTreeSet<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures/protocol.json");
    let golden: Value = serde_json::from_slice(&std::fs::read(&path).expect("the golden file")).expect("the golden file is JSON");
    golden[section].as_object().expect("a section of objects").keys().cloned().collect()
}

fn keys(value: &Value) -> BTreeSet<String> {
    value.as_object().expect("an object").keys().cloned().collect()
}

// ---- the tracer ---------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_signed_in_browser_upgrades_and_gets_the_session_message_first() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    let first = socket.read_json(PATIENCE).await.expect("the first message");
    assert_eq!(first["event"], "hw-session", "{first}");
    assert_eq!(first["payload"]["state"], "ok", "{first}");
    let seconds = first["payload"]["seconds"].as_i64().expect("seconds");
    assert!((1..=IDLE).contains(&seconds), "seconds left of the idle time: {seconds}");
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_the_layout_has_the_keys_of_the_golden_view() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    let view = socket.ok("get_layout", "{}").await;
    assert_eq!(keys(&view), golden_keys("view"), "the view of a new person has the keys of the golden file");
    // The events of the page are the golden events and the three the worker adds around them.
    let golden = golden_keys("events");
    assert!(golden.contains("title") && golden.contains("cwd"), "{golden:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_typed_command_reaches_a_real_shell_and_its_output_comes_back_as_frames() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    let view = socket.ok("get_layout", "{}").await;
    let pane = focus(&view);
    socket
        .ok("start_pane", &json!({"pane": pane, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}).to_string())
        .await;
    socket.ok("text", &json!({"pane": pane, "text": "printf 'hw-%s\\n' e2e\r"}).to_string()).await;
    let seen = socket
        .event_where("frame", Duration::from_secs(20), |payload| payload.to_string().contains("hw-e2e"))
        .await;
    assert!(seen.is_some(), "a frame with the output of the shell arrives");
    println!("tracer: the output of the shell came back as a frame event over TLS");
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_wrong_origin_is_closed_with_4403_and_the_word_origin() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect(Some(&alice.signed.cookie), Some("https://evil.example.test")).await;
    assert_eq!(socket.close_code(PATIENCE).await, Some((4403, "origin".to_owned())));
    assert!(socket.ends_within(PATIENCE).await, "the connection ends");
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_missing_origin_is_closed_with_4403() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect(Some(&alice.signed.cookie), None).await;
    assert_eq!(socket.close_code(PATIENCE).await, Some((4403, "origin".to_owned())));
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_no_session_is_closed_with_4401_and_the_word_session() {
    let env = WsEnv::start().await;
    let mut socket = env.connect(None, Some(ORIGIN)).await;
    assert_eq!(socket.close_code(PATIENCE).await, Some((4401, "session".to_owned())));
    let mut forged = env.connect(Some("not-a-session-cookie"), Some(ORIGIN)).await;
    assert_eq!(forged.close_code(PATIENCE).await, Some((4401, "session".to_owned())));
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_an_expired_session_is_closed_with_4401() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    env.test.clock.advance(IDLE + 1);
    let mut socket = env.connect_as(&alice.signed).await;
    assert_eq!(socket.close_code(PATIENCE).await, Some((4401, "session".to_owned())));
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_refused_upgrade_attaches_nothing_starts_no_worker_and_writes_no_audit_line() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let before = env.counts(&alice);
    assert_eq!((before.sockets, before.workers), (0, 0));
    let mut wrong_origin = env.connect(Some(&alice.signed.cookie), Some("https://evil.example.test")).await;
    assert_eq!(wrong_origin.close_code(PATIENCE).await.map(|c| c.0), Some(4403));
    let mut no_session = env.connect(None, Some(ORIGIN)).await;
    assert_eq!(no_session.close_code(PATIENCE).await.map(|c| c.0), Some(4401));
    let after = env.counts(&alice);
    assert_eq!(after, before, "no socket, no worker and no audit line for a refused upgrade");
    assert_eq!(env.totals(), (0, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_request_that_is_no_upgrade_is_a_bad_request_and_a_post_is_not_allowed() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let plain = env.plain(Request::get("/ws").cookie(&alice.signed.cookie).header("Origin", ORIGIN).close()).await;
    assert_eq!(plain.status, 400);
    assert_eq!(plain.json(), json!({"code": "bad_request"}));
    let post = env.plain(Request::new("POST", "/ws").cookie(&alice.signed.cookie).header("Origin", ORIGIN).close()).await;
    assert_eq!(post.status, 405);
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_the_socket_detaches_within_a_second_of_the_client_closing() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    socket.ok("get_layout", "{}").await;
    env.eventually("the socket is attached", |e| e.counts(&alice).sockets == 1).await;
    socket.send_close(1000).await;
    let closed = socket.close_code(PATIENCE).await;
    assert_eq!(closed.map(|c| c.0), Some(1000), "the server answers the close");
    let deadline = tokio::time::Instant::now() + SECOND;
    while env.counts(&alice).sockets != 0 {
        assert!(tokio::time::Instant::now() < deadline, "the socket was not detached within a second");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ---- helpers --------------------------------------------------------------------------------------------------------------

/// A command frame of exactly `total` bytes: spaces before the closing brace make up the difference.
fn padded(cmd: &str, args: &str, total: usize) -> String {
    let head = format!(r#"{{"id":4242,"cmd":"{cmd}","args":{args}"#);
    assert!(head.len() < total, "the frame cannot be that small");
    format!("{head}{}}}", " ".repeat(total - head.len() - 1))
}

/// The socket is still good for a call.
async fn usable(socket: &mut ws_env::WsClient) {
    let view = socket.ok("get_layout", "{}").await;
    assert!(view.get("focus").is_some(), "{view}");
}

/// Reads and answers pings for `span`, so that the socket counts as alive on the client side.
async fn idle_for(socket: &mut ws_env::WsClient, span: Duration) {
    let end = Instant::now() + span;
    while Instant::now() < end {
        let _ = socket.read(end.saturating_duration_since(Instant::now())).await;
    }
}

/// Every `hw-session` state, in order, until `span` has passed.
async fn session_states(socket: &mut ws_env::WsClient, span: Duration) -> Vec<(String, i64)> {
    let end = Instant::now() + span;
    let mut out = Vec::new();
    while let Some(event) = socket.next_event("hw-session", end.saturating_duration_since(Instant::now())).await {
        out.push((event["payload"]["state"].as_str().unwrap_or_default().to_owned(), event["payload"]["seconds"].as_i64().unwrap_or(-1)));
    }
    out
}

fn quick(s: &mut holzwave_server::http::ws::SocketSettings) {
    s.ping = Duration::from_millis(200);
    s.silence = Duration::from_millis(1500);
    s.recheck = Duration::from_millis(150);
    s.stall = Duration::from_millis(400);
}

// ---- the sizes ------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_small_command_is_admitted_at_its_cap_and_refused_one_byte_past_it() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    for (frame_len, too_large) in [(4096, false), (4097, true)] {
        socket.send_text(&padded("key", r#"{"pane":1,"key":"a"}"#, frame_len)).await;
        let answer = socket.answer(4242).await;
        let refused = answer["err"] == ANSWER_TOO_LARGE;
        assert_eq!(refused, too_large, "a key frame of {frame_len} bytes: {answer}");
    }
    usable(&mut socket).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_default_command_is_admitted_at_sixty_four_kib_and_refused_one_byte_past_it() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    for (frame_len, too_large) in [(64 * 1024, false), (64 * 1024 + 1, true)] {
        socket.send_text(&padded("get_layout", "{}", frame_len)).await;
        let answer = socket.answer(4242).await;
        assert_eq!(answer["err"] == ANSWER_TOO_LARGE, too_large, "{frame_len} bytes: {answer}");
    }
    usable(&mut socket).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paste_may_be_eight_mib_and_a_frame_one_byte_over_that_closes_the_socket_with_1009() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    let cap = 8 * 1024 * 1024;
    socket.send_text(&padded("paste", r#"{"pane":1,"text":"x","allow_unsafe":false}"#, cap)).await;
    let answer = socket.answer(4242).await;
    assert_ne!(answer["err"], ANSWER_TOO_LARGE, "a paste of the cap is admitted: {}", answer.to_string().chars().take(200).collect::<String>());
    let mut other = env.connect_as(&alice.signed).await;
    other.send_text(&padded("paste", r#"{"pane":1,"text":"x","allow_unsafe":false}"#, cap + 1)).await;
    assert_eq!(other.close_code(PATIENCE).await, Some((1009, "size".to_owned())));
    assert!(other.ends_within(PATIENCE).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_binary_message_closes_the_socket_with_1003() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    socket.send_frame(0x80 | BINARY, b"\x00\x01\x02").await;
    assert_eq!(socket.close_code(PATIENCE).await, Some((1003, "type".to_owned())));
}

#[tokio::test(flavor = "multi_thread")]
async fn text_that_is_not_utf8_closes_the_socket_with_1007() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    socket.send_frame(0x80 | TEXT, b"{\"id\":1,\"cmd\":\"\xff\xfe\"}").await;
    assert_eq!(socket.close_code(PATIENCE).await, Some((1007, "encoding".to_owned())));
}

// ---- hostile frames -------------------------------------------------------------------------------------------------------

fn hostile_frames() -> Vec<(&'static str, Vec<u8>)> {
    let mut long_control = vec![0x80 | PING, 0x80 | 126, 0, 126, 1, 2, 3, 4];
    long_control.extend(std::iter::repeat_n(0u8, 126));
    let mut huge = vec![0x80 | TEXT, 0x80 | 127];
    huge.extend_from_slice(&(100u64 * 1024 * 1024).to_be_bytes());
    huge.extend_from_slice(&[1, 2, 3, 4]);
    let mut msb = vec![0x80 | TEXT, 0x80 | 127];
    msb.extend_from_slice(&(1u64 << 63).to_be_bytes());
    msb.extend_from_slice(&[1, 2, 3, 4]);
    let mut text_then_text = masked_frame(TEXT, b"{\"id\":1");
    text_then_text.extend(masked_frame(0x80 | TEXT, b"x"));
    vec![
        ("an unmasked frame", vec![0x80 | TEXT, 2, b'h', b'i']),
        ("a reserved bit", masked_frame(0xC0 | TEXT, b"hi")),
        ("an unknown opcode", masked_frame(0x80 | 3, b"hi")),
        ("a fragmented ping", masked_frame(PING, b"hi")),
        ("a ping of 126 bytes", long_control),
        ("a continuation of nothing", masked_frame(0x80 | CONTINUATION, b"x")),
        ("a text inside a text", text_then_text),
        ("a length of 100 MiB", huge),
        ("a length with the top bit set", msb),
        ("a close with one byte", masked_frame(0x80 | CLOSE, &[1])),
        ("a close with code 1005", masked_frame(0x80 | CLOSE, &1005u16.to_be_bytes())),
        ("a close with code 999", masked_frame(0x80 | CLOSE, &999u16.to_be_bytes())),
        ("a close reason that is not UTF-8", masked_frame(0x80 | CLOSE, &[0x03, 0xe8, 0xff, 0xfe])),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn every_hostile_frame_ends_the_socket_and_leaves_the_server_serving() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    for (name, bytes) in hostile_frames() {
        let mut socket = env.connect_as(&alice.signed).await;
        socket.read_json(PATIENCE).await.expect("the session message");
        socket.send_raw(&bytes).await;
        assert!(socket.ends_within(PATIENCE).await, "{name}: the connection did not end");
        env.eventually(&format!("{name}: the socket is detached"), |e| e.counts(&alice).sockets == 0).await;
        let mut fresh = env.connect_as(&alice.signed).await;
        usable(&mut fresh).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_protocol_error_is_closed_with_1002_and_the_word_protocol() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    socket.send_raw(&[0x80 | TEXT, 2, b'h', b'i']).await;
    let closed = socket.close_code(PATIENCE).await;
    assert_eq!(closed.map(|c| c.0), Some(1002));
}

fn bad_texts() -> Vec<(&'static str, String)> {
    let deep = format!("{}{}", "[".repeat(300), "]".repeat(300));
    vec![
        ("an empty message", String::new()),
        ("blanks", "   ".to_owned()),
        ("no JSON", "not json".to_owned()),
        ("an array", r#"[777,"get_layout",{}]"#.to_owned()),
        ("null", "null".to_owned()),
        ("a number", "123".to_owned()),
        ("a string", r#""get_layout""#.to_owned()),
        ("an empty object", "{}".to_owned()),
        ("no command", r#"{"id":777}"#.to_owned()),
        ("no id", r#"{"cmd":"get_layout"}"#.to_owned()),
        ("an id that is a string", r#"{"id":"777","cmd":"get_layout"}"#.to_owned()),
        ("a negative id", r#"{"id":-777,"cmd":"get_layout"}"#.to_owned()),
        ("a fractional id", r#"{"id":7.5,"cmd":"get_layout"}"#.to_owned()),
        ("an id past u64", r#"{"id":18446744073709551616,"cmd":"get_layout"}"#.to_owned()),
        ("args that are an array", r#"{"id":777,"cmd":"get_layout","args":[]}"#.to_owned()),
        ("args that are a string", r#"{"id":777,"cmd":"get_layout","args":"x"}"#.to_owned()),
        ("args that are null", r#"{"id":777,"cmd":"get_layout","args":null}"#.to_owned()),
        ("a window member", r#"{"id":777,"cmd":"get_layout","window":"main"}"#.to_owned()),
        ("an id twice", r#"{"id":777,"id":778,"cmd":"get_layout"}"#.to_owned()),
        ("a command that is a number", r#"{"id":777,"cmd":5}"#.to_owned()),
        ("a command with a NUL", "{\"id\":777,\"cmd\":\"get_layout\\u0000\"}".to_owned()),
        ("a command in capitals", r#"{"id":777,"cmd":"GET_LAYOUT"}"#.to_owned()),
        ("a path as a command", r#"{"id":777,"cmd":"../etc/passwd"}"#.to_owned()),
        ("a nest of 300 arrays", deep),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn every_bad_text_is_a_violation_that_leaves_the_socket_usable() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    for (name, text) in bad_texts() {
        let mut socket = env.connect_as(&alice.signed).await;
        socket.send_text(&text).await;
        usable(&mut socket).await;
        assert!(socket.close_code(Duration::from_millis(50)).await.is_none(), "{name}: the socket stayed open");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_command_is_answered_without_its_name() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    socket.send_text(r#"{"id":31,"cmd":"CANARY-<script>-9f3a","args":{}}"#).await;
    let answer = socket.answer(31).await;
    assert_eq!(answer["err"], ANSWER_UNKNOWN);
    assert!(!answer.to_string().contains("CANARY"), "{answer}");
}

#[tokio::test(flavor = "multi_thread")]
async fn eight_violations_are_answered_and_the_ninth_closes_the_socket_with_1008() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    for n in 0..8u64 {
        socket.send_text(&format!(r#"{{"id":{},"cmd":"nope","args":{{}}}}"#, 100 + n)).await;
        let answer = socket.answer(100 + n).await;
        assert_eq!(answer["err"], ANSWER_UNKNOWN);
    }
    socket.send_text(r#"{"id":200,"cmd":"nope","args":{}}"#).await;
    assert_eq!(socket.close_code(PATIENCE).await, Some((1008, "policy".to_owned())));
}

#[test]
fn the_budget_counts_violations_in_a_sliding_window() {
    let start = Instant::now();
    let mut budget = Budget::new(8, Duration::from_secs(60));
    for n in 0..8 {
        assert!(!budget.record(start + Duration::from_secs(n)), "violation {n} is within the budget");
    }
    assert!(budget.record(start + Duration::from_secs(8)), "the ninth is one too many");
    // Sixty seconds after the first ones, they no longer count.
    let mut budget = Budget::new(8, Duration::from_secs(60));
    for n in 0..8 {
        assert!(!budget.record(start + Duration::from_secs(n)));
    }
    assert!(!budget.record(start + Duration::from_secs(65)), "the early ones have left the window");
}

#[test]
fn the_budget_keeps_a_bounded_memory_whatever_is_recorded() {
    let start = Instant::now();
    let mut budget = Budget::new(8, Duration::from_secs(3600));
    for n in 0..100_000u64 {
        let _ = budget.record(start + Duration::from_millis(n));
    }
    assert!(budget.record(start + Duration::from_secs(200)));
}

#[test]
fn the_bucket_gives_the_burst_then_the_rate_and_never_more_than_the_burst() {
    let start = Instant::now();
    let mut bucket = Bucket::new(400, 800, start);
    for n in 0..800 {
        assert!(bucket.take(start), "token {n} of the burst");
    }
    assert!(!bucket.take(start), "the 801st at the same instant");
    // A quarter of a second brings back a hundred.
    let later = start + Duration::from_millis(250);
    for n in 0..100 {
        assert!(bucket.take(later), "refilled token {n}");
    }
    assert!(!bucket.take(later));
    // A long pause fills the bucket to the burst and not beyond.
    let much_later = later + Duration::from_secs(3600);
    for _ in 0..800 {
        assert!(bucket.take(much_later));
    }
    assert!(!bucket.take(much_later));
}

#[test]
fn the_bucket_survives_an_instant_that_goes_backwards() {
    let start = Instant::now() + Duration::from_secs(10);
    let mut bucket = Bucket::new(400, 800, start);
    for _ in 0..800 {
        assert!(bucket.take(start));
    }
    assert!(!bucket.take(start - Duration::from_secs(5)), "no tokens appear from an earlier instant");
}

// ---- the rate -------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_burst_over_the_bucket_is_refused_with_the_rate_answer_and_the_socket_lives_on() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    let mut bytes = Vec::new();
    for n in 0..1000u64 {
        bytes.extend(masked_frame(0x80 | TEXT, format!(r#"{{"id":{},"cmd":"get_layout","args":{{}}}}"#, 10_000 + n).as_bytes()));
    }
    socket.send_raw(&bytes).await;
    let (mut oks, mut too_many) = (0, 0);
    while oks + too_many < 1000 {
        let answer = socket.read_json(PATIENCE).await.unwrap_or_else(|| panic!("{} of 1000 answers", oks + too_many));
        if answer.get("event").is_some() {
            continue;
        }
        if answer.get("ok").is_some() {
            oks += 1;
        } else if answer["err"] == ANSWER_TOO_MANY {
            too_many += 1;
        } else {
            panic!("unexpected {answer}");
        }
    }
    assert!(oks >= 800, "the burst of 800 is served: {oks}");
    assert!(too_many >= 1, "the rest is refused");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    usable(&mut socket).await;
}

fn few_tokens(s: &mut holzwave_server::http::ws::SocketSettings) {
    s.burst = 5;
    s.per_second = 1;
    s.refusals = 10;
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals_in_a_row_close_the_socket_with_1008() {
    let env = WsEnv::start_with(WsOptions { tune: Some(few_tokens), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    let mut bytes = Vec::new();
    for n in 0..40u64 {
        bytes.extend(masked_frame(0x80 | TEXT, format!(r#"{{"id":{},"cmd":"get_layout","args":{{}}}}"#, 500 + n).as_bytes()));
    }
    socket.send_raw(&bytes).await;
    assert_eq!(socket.close_code(PATIENCE).await, Some((1008, "policy".to_owned())));
}

// ---- the sockets of a person ----------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_ninth_socket_of_a_person_is_closed_with_1013_busy_and_attaches_nothing() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut open = Vec::new();
    for _ in 0..8 {
        let mut socket = env.connect_as(&alice.signed).await;
        socket.read_json(PATIENCE).await.expect("the session message");
        open.push(socket);
    }
    env.eventually("eight sockets are attached", |e| e.counts(&alice).sockets == 8).await;
    let before = env.counts(&alice);
    let mut ninth = env.connect_as(&alice.signed).await;
    assert_eq!(ninth.close_code(PATIENCE).await, Some((1013, "busy".to_owned())));
    assert_eq!(env.counts(&alice), before, "the refused socket attached nothing and wrote no audit line");
    // A place becomes free when one closes.
    let mut first = open.remove(0);
    first.send_close(1000).await;
    env.eventually("a place is free", |e| e.counts(&alice).sockets == 7).await;
    let mut again = env.connect_as(&alice.signed).await;
    usable(&mut again).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn another_person_is_not_held_back_by_a_full_one() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let bob = env.person("bob");
    let mut open = Vec::new();
    for _ in 0..8 {
        open.push(env.connect_as(&alice.signed).await);
    }
    env.eventually("eight sockets are attached", |e| e.counts(&alice).sockets == 8).await;
    let mut socket = env.connect_as(&bob.signed).await;
    usable(&mut socket).await;
    assert_eq!(env.counts(&bob).sockets, 1);
}

// ---- back-pressure --------------------------------------------------------------------------------------------------------

fn stall_short(s: &mut holzwave_server::http::ws::SocketSettings) {
    s.stall = Duration::from_millis(300);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_that_does_not_read_is_closed_with_1013_after_the_stall_time() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut rig = Rig::start(&env, &alice, stall_short).await;
    rig.wait_sent("the session message", PATIENCE, |sent| !sent.is_empty()).await;
    rig.stall.store(true, std::sync::atomic::Ordering::SeqCst);
    rig.text(r#"{"id":1,"cmd":"get_layout","args":{}}"#);
    assert!(rig.finished(Duration::from_secs(5)).await, "the loop ended after the stall");
    assert_eq!(rig.closed_with(), Some((1013, "busy".to_owned())));
    env.eventually("the socket is detached", |e| e.counts(&alice).sockets == 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ping_that_cannot_be_written_is_closed_with_1013() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut rig = Rig::start(&env, &alice, |s| {
        s.ping = Duration::from_millis(100);
        s.stall = Duration::from_millis(200);
    })
    .await;
    rig.stall_ping.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(rig.finished(Duration::from_secs(5)).await);
    assert_eq!(rig.closed_with(), Some((1013, "busy".to_owned())));
}

#[tokio::test(flavor = "multi_thread")]
async fn one_stalled_socket_does_not_hold_back_the_other_sockets_of_the_person() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let rig = Rig::start(&env, &alice, |_| {}).await;
    rig.wait_sent("the session message", PATIENCE, |sent| !sent.is_empty()).await;
    rig.stall.store(true, std::sync::atomic::Ordering::SeqCst);
    rig.text(r#"{"id":1,"cmd":"get_layout","args":{}}"#);
    let mut other = env.connect_as(&alice.signed).await;
    usable(&mut other).await;
    assert_eq!(env.counts(&alice).sockets, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_server_stopping_ends_a_socket_whose_write_is_stalled() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut rig = Rig::start(&env, &alice, |_| {}).await;
    rig.wait_sent("the session message", PATIENCE, |sent| !sent.is_empty()).await;
    rig.stall.store(true, std::sync::atomic::Ordering::SeqCst);
    rig.text(r#"{"id":1,"cmd":"get_layout","args":{}}"#);
    tokio::time::sleep(Duration::from_millis(200)).await;
    rig.shutdown.trigger();
    assert!(rig.finished(SECOND * 2).await, "the loop ended with the shutdown although the write never finishes");
    assert_eq!(rig.closed_with(), Some((1001, "going away".to_owned())));
}

// ---- liveness -------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_server_pings_at_its_interval() {
    let env = WsEnv::start_with(WsOptions { tune: Some(quick), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    let mut pings = 0;
    let end = Instant::now() + Duration::from_millis(1000);
    while Instant::now() < end {
        if let Got::Message(Incoming::Ping(_)) = socket.read(end.saturating_duration_since(Instant::now())).await {
            pings += 1;
        }
    }
    assert!((3..=6).contains(&pings), "about five pings in a second at 200 ms: {pings}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_that_answers_the_pings_stays_open_past_the_silence_time() {
    let env = WsEnv::start_with(WsOptions { tune: Some(quick), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    idle_for(&mut socket, Duration::from_millis(3200)).await;
    usable(&mut socket).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_that_says_nothing_is_closed_with_1001_idle() {
    let env = WsEnv::start_with(WsOptions { tune: Some(quick), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    socket.auto_pong = false;
    assert_eq!(socket.close_code(Duration::from_secs(6)).await, Some((1001, "idle".to_owned())));
    env.eventually("the socket is detached", |e| e.counts(&alice).sockets == 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pong_counts_as_a_sign_of_life() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut rig = Rig::start(&env, &alice, |s| s.silence = Duration::from_millis(600)).await;
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        rig.feed(Ok(Inbound::Pong));
    }
    assert!(rig.closed_with().is_none(), "still open after 1.6 s of pongs");
    assert!(rig.finished(Duration::from_secs(3)).await);
    assert_eq!(rig.closed_with(), Some((1001, "idle".to_owned())));
}

// ---- the session ----------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_session_message_repeats_at_the_recheck_interval() {
    let env = WsEnv::start_with(WsOptions { tune: Some(quick), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    let states = session_states(&mut socket, Duration::from_millis(1000)).await;
    assert!(states.len() >= 4, "the first and about six more in a second: {states:?}");
    assert!(states.iter().all(|(state, _)| state == "ok"), "{states:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_page_is_told_when_the_session_is_about_to_end() {
    let env = WsEnv::start_with(WsOptions { tune: Some(quick), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    env.test.clock.advance(IDLE - 30);
    let event = socket.event_where("hw-session", PATIENCE, |p| p["state"] == "expiring").await.expect("an expiring message");
    let seconds = event["payload"]["seconds"].as_i64().expect("seconds");
    assert!((1..=30).contains(&seconds), "{seconds}");
}

#[tokio::test(flavor = "multi_thread")]
async fn input_commands_keep_a_session_alive_and_other_commands_do_not() {
    let env = WsEnv::start_with(WsOptions { tune: Some(quick), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    env.test.clock.advance(IDLE - 30);
    socket.event_where("hw-session", PATIENCE, |p| p["state"] == "expiring").await.expect("expiring");
    // A command that is no input leaves the end where it was.
    usable(&mut socket).await;
    let still = session_states(&mut socket, Duration::from_millis(500)).await;
    assert!(still.iter().all(|(_, seconds)| *seconds <= 60), "no command but an input one moves the end: {still:?}");
    assert!(still.iter().any(|(state, _)| state == "expiring"), "{still:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn each_input_command_touches_the_session() {
    let env = WsEnv::start_with(WsOptions { tune: Some(quick), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    for cmd in ["key", "text", "paste", "mouse", "wheel", "send_edit"] {
        env.test.clock.advance(IDLE - 30);
        socket.event_where("hw-session", PATIENCE, |p| p["state"] == "expiring").await.unwrap_or_else(|| panic!("{cmd}: expiring"));
        let _ = socket.call(cmd, "{}").await;
        socket
            .event_where("hw-session", PATIENCE, |p| p["state"] == "ok" && p["seconds"].as_i64().unwrap_or(0) > IDLE - 100)
            .await
            .unwrap_or_else(|| panic!("{cmd} did not keep the session alive"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_that_ran_out_is_announced_and_the_socket_closed_with_4401() {
    let env = WsEnv::start_with(WsOptions { tune: Some(quick), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    env.test.clock.advance(IDLE + 1);
    let ended = socket.event_where("hw-session", PATIENCE, |p| p["state"] == "ended").await;
    assert!(ended.is_some(), "the page is told");
    assert_eq!(socket.close_code(PATIENCE).await, Some((4401, "session".to_owned())));
    env.eventually("the socket is detached", |e| e.counts(&alice).sockets == 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_absolute_lifetime_ends_a_session_that_is_never_idle() {
    let env = WsEnv::start_with(WsOptions { tune: Some(quick), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    let mut passed = 0;
    while passed + 1000 < ABSOLUTE {
        env.test.clock.advance(1000);
        passed += 1000;
        let _ = socket.call("key", "{}").await;
    }
    env.test.clock.advance(ABSOLUTE - passed + 1);
    assert_eq!(socket.close_code(PATIENCE).await, Some((4401, "session".to_owned())));
}

#[tokio::test(flavor = "multi_thread")]
async fn signing_out_closes_the_socket_with_4401_at_once() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    usable(&mut socket).await;
    let started = Instant::now();
    env.test.sessions.end(&alice.signed.session).expect("end");
    assert_eq!(socket.close_code(SECOND * 3).await, Some((4401, "session".to_owned())));
    assert!(started.elapsed() < SECOND * 2, "much sooner than the thirty seconds of the recheck");
}

#[tokio::test(flavor = "multi_thread")]
async fn signing_out_everywhere_closes_every_socket_of_the_person_and_no_other() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let bob = env.person("bob");
    let second = env.sign_in_again(&alice);
    let mut one = env.connect_as(&alice.signed).await;
    let mut two = env.connect_as(&second).await;
    let mut other = env.connect_as(&bob.signed).await;
    usable(&mut one).await;
    usable(&mut two).await;
    usable(&mut other).await;
    env.test.sessions.end_all(alice.key.id).expect("end all");
    assert_eq!(one.close_code(SECOND * 3).await.map(|c| c.0), Some(4401));
    assert_eq!(two.close_code(SECOND * 3).await.map(|c| c.0), Some(4401));
    usable(&mut other).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ending_one_session_by_its_handle_closes_only_its_sockets() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let second = env.sign_in_again(&alice);
    let mut one = env.connect_as(&alice.signed).await;
    let mut two = env.connect_as(&second).await;
    usable(&mut one).await;
    usable(&mut two).await;
    assert!(env.test.sessions.end_by_handle(alice.key.id, &handle_of(&second.session)).expect("end by handle"));
    assert_eq!(two.close_code(SECOND * 3).await.map(|c| c.0), Some(4401));
    usable(&mut one).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_account_loses_its_sockets_at_the_next_look() {
    let env = WsEnv::start_with(WsOptions { tune: Some(quick), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    usable(&mut socket).await;
    env.test.set_state("alice", AccountState::Disabled);
    assert_eq!(socket.close_code(PATIENCE).await, Some((4401, "session".to_owned())));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_locked_store_is_told_at_every_look_and_the_unlock_once() {
    let env = WsEnv::start_with(WsOptions { tune: Some(quick), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    env.test.view.set_locked(alice.key.id, true);
    let mut socket = env.connect_as(&alice.signed).await;
    let first = session_states(&mut socket, Duration::from_millis(700)).await;
    let locked = first.iter().filter(|(state, _)| state == "locked").count();
    assert!(locked >= 3, "the first message is followed by locked, again at every look: {first:?}");
    assert_eq!(first[0].0, "ok", "{first:?}");
    env.test.view.set_locked(alice.key.id, false);
    let after = session_states(&mut socket, Duration::from_millis(900)).await;
    assert_eq!(after.iter().filter(|(state, _)| state == "unlocked").count(), 1, "{after:?}");
    assert!(after.iter().filter(|(state, _)| state == "locked").count() <= 1, "{after:?}");
}

// ---- the server stopping ----------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_server_stopping_closes_every_socket_with_1001_within_a_second() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let bob = env.person("bob");
    let mut sockets = vec![env.connect_as(&alice.signed).await, env.connect_as(&alice.signed).await, env.connect_as(&bob.signed).await];
    for socket in &mut sockets {
        usable(socket).await;
    }
    env.shutdown.trigger();
    for socket in &mut sockets {
        assert_eq!(socket.close_code(SECOND).await, Some((1001, "going away".to_owned())));
    }
    assert!(env.state.tracker.wait_empty(SECOND * 2).await, "every socket task has ended");
}

// ---- the fake transport -----------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_socket_over_the_fake_transport_writes_the_session_message_first() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let rig = Rig::start(&env, &alice, |_| {}).await;
    rig.wait_sent("the session message", PATIENCE, |sent| !sent.is_empty()).await;
    assert_eq!(rig.session_states(), ["ok"]);
    rig.text(r#"{"id":1,"cmd":"get_layout","args":{}}"#);
    rig.wait_sent("the answer", PATIENCE, |sent| sent.iter().any(|s| matches!(s, Sent::Text(t) if t.contains(r#""id":1"#)))).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_fake_transport_maps_every_failure_to_its_close() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let cases = [
        (Ok(Inbound::Binary(3)), 1003, "type"),
        (Err(TransportError::TooLarge), 1009, "size"),
        (Err(TransportError::Encoding), 1007, "encoding"),
        (Err(TransportError::Protocol), 1002, "protocol"),
    ];
    for (frame, code, word) in cases {
        let mut rig = Rig::start(&env, &alice, |_| {}).await;
        rig.feed(frame);
        assert!(rig.finished(PATIENCE).await);
        assert_eq!(rig.closed_with(), Some((code, word.to_owned())));
    }
    env.eventually("every socket is detached", |e| e.counts(&alice).sockets == 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_that_closed_gets_no_close_message_and_the_socket_detaches() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut rig = Rig::start(&env, &alice, |_| {}).await;
    rig.feed(Err(TransportError::Closed));
    assert!(rig.finished(PATIENCE).await);
    assert!(rig.closed_with().is_none());
    env.eventually("the socket is detached", |e| e.counts(&alice).sockets == 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn trouble_with_the_store_does_not_end_a_session_inside_its_times() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut rig = Rig::start(&env, &alice, |s| s.recheck = Duration::from_millis(100)).await;
    *rig.port.check_answer.lock().expect("lock") = Err(PortError::Failed);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(rig.closed_with().is_none(), "the session is still inside its times");
    assert!(rig.port.checks.load(std::sync::atomic::Ordering::SeqCst) >= 3);
    env.test.clock.advance(IDLE + 10);
    assert!(rig.finished(Duration::from_secs(3)).await);
    assert_eq!(rig.closed_with(), Some((4401, "session".to_owned())));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_the_port_calls_gone_is_announced_then_closed() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut rig = Rig::start(&env, &alice, |s| s.recheck = Duration::from_millis(100)).await;
    *rig.port.check_answer.lock().expect("lock") = Err(PortError::Gone);
    assert!(rig.finished(Duration::from_secs(3)).await);
    assert!(rig.session_states().contains(&"ended".to_owned()), "{:?}", rig.sent());
    assert_eq!(rig.closed_with(), Some((4401, "session".to_owned())));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_input_command_over_a_session_that_is_gone_closes_the_socket() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut rig = Rig::start(&env, &alice, |_| {}).await;
    *rig.port.touch_answer.lock().expect("lock") = Err(PortError::Gone);
    rig.text(r#"{"id":1,"cmd":"key","args":{}}"#);
    assert!(rig.finished(Duration::from_secs(3)).await);
    assert_eq!(rig.closed_with(), Some((4401, "session".to_owned())));
    assert_eq!(rig.port.touches.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_announcement_for_the_account_closes_the_socket_and_one_for_another_does_not() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut rig = Rig::start(&env, &alice, |_| {}).await;
    rig.port.ended.send(Ended::Account(AccountId::from_raw(987_654))).expect("a listener");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(rig.closed_with().is_none(), "another account's announcement is not ours");
    rig.port.ended.send(Ended::Account(alice.key.id)).expect("a listener");
    assert!(rig.finished(Duration::from_secs(2)).await);
    assert_eq!(rig.closed_with(), Some((4401, "session".to_owned())));
}

#[tokio::test(flavor = "multi_thread")]
async fn missed_announcements_make_the_socket_look_at_its_session() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    let mut rig = Rig::start(&env, &alice, |_| {}).await;
    *rig.port.check_answer.lock().expect("lock") = Err(PortError::Gone);
    for n in 0..16 {
        let _ = rig.port.ended.send(Ended::Account(AccountId::from_raw(900_000 + n)));
    }
    assert!(rig.finished(Duration::from_secs(3)).await);
    assert_eq!(rig.closed_with(), Some((4401, "session".to_owned())));
}

// ---- the canary -------------------------------------------------------------------------------------------------------------

const CANARY: &str = "CANARY-7c1e4f9a2b";

fn audit_text(env: &WsEnv) -> String {
    let mut all = String::new();
    if let Ok(entries) = std::fs::read_dir(&env.test.audit_dir) {
        for entry in entries.flatten() {
            all.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
        }
    }
    all
}

#[tokio::test(flavor = "multi_thread")]
async fn what_the_peer_sent_is_never_in_an_answer_a_close_reason_or_the_audit_log() {
    let env = WsEnv::start().await;
    let alice = env.person("alice");
    // In the name of a command, in the id position, in the arguments of a refused command and in text that is not UTF-8.
    let mut socket = env.connect_as(&alice.signed).await;
    for text in [
        format!(r#"{{"id":51,"cmd":"{CANARY}","args":{{}}}}"#),
        format!(r#"{{"id":52,"cmd":"get_layout","args":{{"{CANARY}":"{CANARY}"}},"{CANARY}":1}}"#),
        format!(r#"{{"id":"{CANARY}","cmd":"get_layout"}}"#),
        format!("[\"{CANARY}\"]"),
    ] {
        socket.send_text(&text).await;
    }
    usable(&mut socket).await;
    let mut seen = Vec::new();
    while let Some(message) = socket.next_event("never-sent", Duration::from_millis(50)).await {
        seen.push(message.to_string());
    }
    let mut bad = env.connect_as(&alice.signed).await;
    let mut bytes = CANARY.as_bytes().to_vec();
    bytes.extend_from_slice(&[0xff, 0xfe]);
    bad.send_frame(0x80 | TEXT, &bytes).await;
    let closed = bad.close_code(PATIENCE).await.expect("a close");
    assert!(!closed.1.contains("CANARY"), "{closed:?}");
    assert!(!audit_text(&env).contains("CANARY"), "the audit log never holds what a peer sent");
    assert!(!audit_text(&env).contains(&alice.signed.cookie), "nor a session cookie");
    assert!(seen.iter().all(|m| !m.contains("CANARY")));
}

#[test]
fn the_endpoint_sources_hold_no_log_call_no_print_and_no_library_type_outside_the_transport() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/http");
    for name in ["ws.rs", "transport.rs"] {
        let source = std::fs::read_to_string(root.join(name)).expect("source");
        let code: String = source.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        assert!(!code.contains("log::"), "{name} writes no log line");
        assert!(!code.contains("println!") && !code.contains("eprintln!") && !code.contains("dbg!"), "{name} prints nothing");
        assert!(!code.contains("unsafe"), "{name} has no unsafe code");
    }
    let ws = std::fs::read_to_string(root.join("ws.rs")).expect("source");
    assert!(!ws.contains("axum::extract::ws"), "ws.rs names no WebSocket library type");
    let code: String = ws.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    assert!(!code.contains("COMMANDS"), "ws.rs holds no list of the commands of the worker");
}
