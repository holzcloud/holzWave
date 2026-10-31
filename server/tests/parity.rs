//! Parity of the socket with the desktop: every command of the host's table is reachable by name through a signed-in
//! WebSocket, with the desktop's answers and events; nothing runs without a session; hostile arguments end in plain answers.
//!
//! The tracer tests come first; the filter `tracer` selects exactly them. Everything here needs the `in-process-workers`
//! feature, and the page must have been built (`npm --prefix app run build`) because the listener embeds nothing else.
//!
//! The table (`COMMANDS`), the golden file and the metadata of a row are read when a test runs; no test holds a copy of the
//! command list or a count of it.
#![cfg(feature = "in-process-workers")]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/parity_env.rs"]
mod parity_env;
#[path = "support/ws_env.rs"]
mod ws_env;

use holzwave_host::commands::registry::{NOT_IN_BROWSER, lookup};
use holzwave_server::http::ws::ANSWER_UNKNOWN;
use parity_env::{Line, Probed, args_for, golden_at, groups, row_for_group, same_shape, unavailable_rows};
use serde_json::{Value, json};
use ws_env::{PATIENCE, WsOptions};

/// A person who is signed in, with a socket whose first message (`hw-session`) has been read.
async fn signed_in(probed: &Probed, name: &str) -> (ws_env::Person, Line) {
    let person = probed.env.person(name);
    let mut line = Line::new(probed.env.connect_as(&person.signed).await);
    assert!(line.until(PATIENCE, |events| events.iter().any(|e| e["event"] == "hw-session")).await, "the session message");
    (person, line)
}

fn err_text(answer: &Value) -> &str {
    answer["err"].as_str().unwrap_or_else(|| panic!("an error string was due: {answer}"))
}

// ---- the tracer ---------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tracer_one_command_of_each_group_answers_with_the_id_of_the_request() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let mut report = Vec::new();
    for group in groups() {
        let row = row_for_group(group).expect("a row of the group the browser has");
        let id = line.send(row.name, &args_for(row)).await;
        let answer = line.wait(id, parity_env::ANSWER_WITHIN).await.unwrap_or_else(|f| panic!("{} ({group}): {f:?}", row.name));
        assert_eq!(answer["id"].as_u64(), Some(id), "{} answers with the id of its request", row.name);
        let kind = if answer.get("ok").is_some() {
            "ok"
        } else {
            assert!(answer.get("err").is_some(), "{} gave neither ok nor err: {answer}", row.name);
            assert_ne!(answer["err"], json!(ANSWER_UNKNOWN), "{} was not found by the endpoint", row.name);
            "err"
        };
        report.push(format!("{group}: {} -> {kind}", row.name));
    }
    println!("tracer groups:\n{}", report.join("\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_the_layout_the_settings_and_the_app_info_have_the_shape_of_the_golden() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    for (command, member) in [("get_layout", "view"), ("get_settings", "settings_view"), ("app_info", "app_info")] {
        let result = line.ok(command, &json!({})).await;
        let golden = golden_at(member).unwrap_or_else(|| panic!("the golden has no {member}"));
        if let Err(difference) = same_shape(&result, golden) {
            panic!("{command} against the golden {member}: {difference}");
        }
        println!("tracer shape: {command} matches {member}");
    }
    let info = line.ok("app_info", &json!({})).await;
    assert_eq!(info["os"], "web", "the page is told it runs in the browser build");
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_command_the_browser_does_not_have_answers_the_sentence_of_the_host_and_runs_nothing() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let row = *unavailable_rows().first().expect("the table marks at least one row unavailable in the browser");
    let before = probed.ran();
    let answer = line.call(row.name, &args_for(row)).await.expect("an answer");
    assert_eq!(err_text(&answer), NOT_IN_BROWSER, "{}", row.name);
    assert_eq!(probed.ran(), before, "{} ran nothing", row.name);
    println!("tracer unavailable: {} -> {NOT_IN_BROWSER}", row.name);
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_name_that_is_not_in_the_table_is_answered_unknown_and_nothing_is_forwarded() {
    let probed = Probed::start(WsOptions::default()).await;
    let (alice, mut line) = signed_in(&probed, "alice").await;
    assert!(lookup("not_a_command").is_none(), "the probe name must not be in the table");
    let before = (probed.ran(), probed.env.counts(&alice));
    let id = line.send_unchecked("not_a_command", &json!({})).await;
    let answer = line.wait(id, parity_env::ANSWER_WITHIN).await.expect("an answer");
    assert_eq!(answer["id"].as_u64(), Some(id));
    assert_eq!(err_text(&answer), ANSWER_UNKNOWN);
    let after = (probed.ran(), probed.env.counts(&alice));
    assert_eq!(before, after, "the unknown name changed nothing the server counts");
    println!("tracer refusal: not_a_command -> {ANSWER_UNKNOWN}; forwarded {} -> {}, sockets {} -> {}, workers {} -> {}", before.0, after.0, before.1.sockets, after.1.sockets, before.1.workers, after.1.workers);
}

#[tokio::test(flavor = "multi_thread")]
#[should_panic(expected = "get_layoutx")]
async fn tracer_the_sender_refuses_a_name_that_is_not_in_the_table() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let _ = line.send("get_layoutx", &json!({})).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_an_upgrade_without_a_cookie_is_closed_4401_and_a_person_who_never_signed_in_has_no_worker() {
    let probed = Probed::start(WsOptions::default()).await;
    let bob = probed.env.person("bob");
    let before = probed.env.counts(&bob);
    let mut client = probed.env.connect(None, Some(edge::ORIGIN)).await;
    let closed = client.close_code(PATIENCE).await.expect("a close message");
    assert_eq!(closed.0, 4401, "{closed:?}");
    let after = probed.env.counts(&bob);
    assert_eq!(before, after, "the refused upgrade changed nothing");
    assert_eq!((after.sockets, after.workers), (0, 0), "no socket and no worker for a person who never signed in");
    assert_eq!(probed.env.totals(), (0, 0), "no worker and no socket on the server");
    assert_eq!(probed.ran(), 0);
    println!("tracer refusal: no cookie -> {closed:?}; counts before {before:?} after {after:?}");
}
