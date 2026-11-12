//! The sessions of the server in the browser build, over the page's own socket: a real TLS listener, a person who is signed in
//! with the page's sign-in, and real in-process workers. The four commands that list, make, open and end a session are
//! answered at the edge with the host's sentence while the operator has remote sessions off, before the worker sees them; with
//! the switch on they reach the person's own worker, which answers from its own sessions. Taking over and asking for older
//! lines act on a pane the window holds and are not gated. Everything here needs the `in-process-workers` feature, and the page
//! must have been built (`npm --prefix app run build`) because the listener embeds nothing else.
#![cfg(feature = "in-process-workers")]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::sync::Arc;
use std::time::{Duration, Instant};

use holzwave_server::http::features::Feature;
use holzwave_server::http::features::REMOTE_PAGE_COMMANDS;
use holzwave_server::worker::runtime::{Probe, TestHooks};
use serde_json::{Value, json};
use ws_env::{PATIENCE, WsClient, WsEnv, WsOptions, focus};

// The fixed sentences, written out here so that a change of one is seen.
const REMOTE_OFF: &str = "Remote sessions are off on this server.";
const NOT_FOUND: &str = "That session is not on the server any more.";
const NOT_A_SESSION_PANE: &str = "This pane does not show a session on a server.";

/// An environment whose workers record every command they run.
async fn probed() -> (WsEnv, Arc<Probe>) {
    let probe = Probe::new();
    let hooks = TestHooks { probe: Some(probe.clone()), ..TestHooks::default() };
    (WsEnv::start_with(WsOptions { hooks, ..WsOptions::default() }).await, probe)
}

/// How many times the workers ran `name`.
fn ran(probe: &Probe, name: &str) -> usize {
    probe.calls().iter().filter(|(command, _)| command == name).count()
}

fn remote(env: &WsEnv, on: bool) {
    env.test.app.features.set(Feature::Remote, on).expect("the switch is stored");
}

fn err(answer: &Value) -> &str {
    answer.get("err").and_then(Value::as_str).unwrap_or_else(|| panic!("not an error: {answer}"))
}

/// The ids of the sessions the socket's person has.
async fn listed(client: &mut WsClient) -> Vec<u64> {
    let rows = client.ok("server_sessions", "{}").await;
    rows.as_array().expect("a list").iter().map(|row| row["id"].as_u64().expect("an id")).collect()
}

async fn until_listed(client: &mut WsClient, want: &[u64]) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let ids = listed(client).await;
        if ids == want {
            return;
        }
        assert!(Instant::now() < deadline, "the list is {ids:?}, not {want:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn with_remote_sessions_off_the_edge_answers_the_four_session_commands_and_the_worker_sees_none() {
    let (env, probe) = probed().await;
    let alice = env.person("alice");
    let mut page = env.connect_as(&alice.signed).await;
    // The worker answers: the socket is attached and its commands run.
    page.ok("get_layout", "{}").await;
    for (name, args) in [
        ("server_sessions", json!({})),
        ("server_session_new", json!({})),
        ("server_session_open", json!({ "session": 1 })),
        ("server_session_end", json!({ "session": 1 })),
    ] {
        assert!(REMOTE_PAGE_COMMANDS.contains(&name));
        let answer = page.call(name, &args.to_string()).await;
        assert_eq!(err(&answer), REMOTE_OFF, "{name}");
        assert_eq!(ran(&probe, name), 0, "{name} never reached the worker");
    }
    assert_eq!(page.ok("get_layout", "{}").await["tabs"].as_array().map(Vec::len), Some(1), "no tab was made");
}

#[tokio::test(flavor = "multi_thread")]
async fn with_remote_sessions_on_the_page_lists_makes_opens_and_ends_sessions_of_its_own_worker() {
    let (env, probe) = probed().await;
    remote(&env, true);
    let alice = env.person("alice");
    let mut page = env.connect_as(&alice.signed).await;
    assert_eq!(listed(&mut page).await, Vec::<u64>::new());

    // New session: a new tab whose pane shows it, and the session in the list with its fields.
    let view = page.ok("server_session_new", "{}").await;
    let pane = focus(&view);
    assert_eq!(view["tabs"].as_array().map(Vec::len), Some(2), "one more tab");
    let rows = page.ok("server_sessions", "{}").await;
    let row = &rows[0];
    let mut members: Vec<&str> = row.as_object().expect("a row").keys().map(String::as_str).collect();
    members.sort_unstable();
    assert_eq!(members, ["attached", "cols", "cwd", "held", "id", "rows", "started", "title"]);
    assert_eq!((row["cols"].as_u64(), row["rows"].as_u64(), row["held"].as_bool()), (Some(80), Some(24), Some(true)));
    let session = row["id"].as_u64().expect("an id");
    assert_eq!(ran(&probe, "server_session_new"), 1);

    // The page starts the pane; the pane line says who writes, in the record's words.
    page.ok("start_pane", &json!({ "pane": pane, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16 }).to_string()).await;
    let deadline = Instant::now() + PATIENCE;
    loop {
        let lines = page.ok("pane_connections", "{}").await;
        let line = lines.as_array().and_then(|all| all.iter().find(|l| l["pane"] == json!(pane)).cloned());
        if let Some(line) = line.filter(|l| l["state"] == "connected" && l["remote"]["phase"] == "") {
            assert_eq!(line["name"], "This server");
            assert_eq!(line["remote"]["writer"], "writing");
            assert_eq!(line["remote"]["session"], json!(session));
            break;
        }
        assert!(Instant::now() < deadline, "the pane did not attach");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Not gated: taking over and older lines act on the pane the window holds.
    assert_eq!(page.call("pane_takeover", &json!({ "pane": pane }).to_string()).await.get("ok"), Some(&Value::Null));
    assert_eq!(page.call("pane_load_older", &json!({ "pane": pane }).to_string()).await.get("ok"), Some(&Value::Null));

    // Closing the pane detaches; Open shows the session again in a new tab.
    page.ok("close_pane", &json!({ "pane": pane }).to_string()).await;
    until_listed(&mut page, &[session]).await;
    let view = page.ok("server_session_open", &json!({ "session": session }).to_string()).await;
    assert_ne!(focus(&view), pane, "a new pane");
    // A number the worker never made is not found, and opens nothing.
    let tabs = view["tabs"].as_array().map(Vec::len);
    assert_eq!(err(&page.call("server_session_open", r#"{"session":999}"#).await), NOT_FOUND);
    assert_eq!(page.ok("get_layout", "{}").await["tabs"].as_array().map(Vec::len), tabs);

    // End: the program is told to end and the session leaves the list.
    page.ok("server_session_end", &json!({ "session": session }).to_string()).await;
    until_listed(&mut page, &[]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_switch_takes_effect_with_the_next_command_and_each_person_has_only_their_own_sessions() {
    let (env, probe) = probed().await;
    let alice = env.person("alice");
    let bob = env.person("bob");
    let mut a = env.connect_as(&alice.signed).await;
    let mut b = env.connect_as(&bob.signed).await;

    assert_eq!(err(&a.call("server_sessions", "{}").await), REMOTE_OFF);
    remote(&env, true);
    assert_eq!(listed(&mut a).await, Vec::<u64>::new(), "on: the same open socket reaches the worker");
    let view = a.ok("server_session_new", "{}").await;
    assert!(focus(&view) > 0);
    let alices = listed(&mut a).await;
    assert_eq!(alices.len(), 1);

    // Bob's worker is his own: his list is empty and Alice's number is not his to open or end.
    assert_eq!(listed(&mut b).await, Vec::<u64>::new());
    assert_eq!(err(&b.call("server_session_open", &json!({ "session": alices[0] }).to_string()).await), NOT_FOUND);
    assert_eq!(err(&b.call("server_session_end", &json!({ "session": alices[0] }).to_string()).await), NOT_FOUND);
    assert_eq!(listed(&mut a).await, alices, "Alice's session is untouched");

    // Off again: the next command is answered at the edge, on both open sockets.
    let before = ran(&probe, "server_sessions");
    remote(&env, false);
    assert_eq!(err(&a.call("server_sessions", "{}").await), REMOTE_OFF);
    assert_eq!(err(&b.call("server_session_new", "{}").await), REMOTE_OFF);
    assert_eq!(ran(&probe, "server_sessions"), before, "nothing was forwarded while off");
    // The pane the window already holds is not new surface: its own commands still reach the worker.
    let pane = focus(&a.ok("get_layout", "{}").await);
    let answer = a.call("pane_takeover", &json!({ "pane": pane }).to_string()).await;
    assert_ne!(answer.get("err").and_then(Value::as_str), Some(REMOTE_OFF));
    assert_eq!(ran(&probe, "pane_takeover"), 1);
    // A pane of the window that shows no session says so, whatever the switch.
    let local = focus(&b.ok("get_layout", "{}").await);
    assert_eq!(err(&b.call("pane_load_older", &json!({ "pane": local }).to_string()).await), NOT_A_SESSION_PANE);
}
