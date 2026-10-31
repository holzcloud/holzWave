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

use edge::{IDLE, ORIGIN};
use edge_tls::Request;
use serde_json::{Value, json};
use ws_env::{Got, Incoming, PATIENCE, WsEnv, focus};

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
    let _ = (Got::Quiet, Incoming::Pong(Vec::new()));
}
