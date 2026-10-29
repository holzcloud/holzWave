//! The supervisor driven through real in-process workers and scripted workers: one worker per person, the order and the
//! wrapping of commands and events, the bounded queue of every socket, hostile worker frames, the replay of a new socket, the
//! owner of a pane's size, the idle rule, the limits, the account observer, the wipes and the shutdown.
//!
//! See `support/sup_env.rs` for the environment.

#[path = "support/scratch.rs"]
mod scratch;
#[path = "support/sup_env.rs"]
mod sup_env;

use std::time::Duration;

use holzwave_server::supervisor::Outbound;
use holzwave_server::wire::{Ctl, Frame};
use serde_json::Value;
use sup_env::{PATIENCE, Reply, ScriptEnv, SupEnv, account, expected_init};

/// The id of the focused pane of a layout answer.
fn focus(view: &Value) -> u64 {
    view["focus"].as_u64().expect("the view names the focused pane")
}

// ---- the tracer ---------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tracer_one_person_runs_a_command_in_a_real_worker_and_gets_the_event() {
    let env = SupEnv::start().await;
    let mut client = env.client(env.account(1, 20001)).await;
    assert_eq!(env.launcher.launches(), 1, "the first attach starts one worker");

    let view = client.ok("get_layout", "{}").await;
    assert_eq!(view["workspaces"].as_array().expect("workspaces").len(), 1);
    let first = focus(&view);
    // A second pane, so that closing the first leaves a layout.
    let split = client.ok("split", r#"{"column":false}"#).await;
    assert_eq!(split["panes"].as_array().expect("panes").len(), 2);

    let started = client.call("start_pane", &format!(r#"{{"pane":{first},"cols":80,"rows":24,"cell_w":8,"cell_h":16}}"#)).await;
    assert!(matches!(started, Reply::Ok(_)), "{started:?}");
    // A frame event for that pane arrives as text of the form {"event":"frame","payload":{...}}.
    let frame = client
        .event_where("frame", Duration::from_secs(5), |payload| payload["pane"] == serde_json::json!(first))
        .await
        .expect("a frame event for the pane within five seconds");
    assert!(frame["payload"].is_object(), "{frame}");
    assert_eq!(frame["event"], "frame");

    let closed = client.call("close_pane", &format!(r#"{{"pane":{first}}}"#)).await;
    assert!(matches!(closed, Reply::Ok(_)), "{closed:?}");
    let layout = client.event_where("layout", PATIENCE, |p| p["panes"].as_array().is_some_and(|p| p.len() == 1)).await;
    assert!(layout.is_some(), "a layout event follows the close");
    // Printed for the record of the plan: the kinds, the ids and the sizes, no payload text.
    println!("tracer messages at the socket: {:#?}", client.log());
    env.supervisor.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_two_people_get_two_workers_and_nothing_crosses() {
    let env = SupEnv::start().await;
    let mut one = env.client(env.account(1, 20001)).await;
    let mut two = env.client(env.account(2, 20002)).await;
    assert_eq!(env.launcher.launches(), 2, "each person has a worker of their own");

    // The same request number at both sockets gets two separate answers, each at its own socket.
    one.handle.submit(77, "split", r#"{"column":true}"#).expect("submit one");
    two.handle.submit(77, "get_layout", "{}").expect("submit two");
    let (Reply::Ok(split), Reply::Ok(layout)) = (one.wait_reply(77).await, two.wait_reply(77).await) else {
        panic!("both commands succeed")
    };
    assert_eq!(split["panes"].as_array().expect("panes").len(), 2, "person one split their window");
    assert_eq!(layout["panes"].as_array().expect("panes").len(), 1, "person two never saw the split");

    // A pane in each: the events of person one's pane never reach person two.
    let first_one = focus(&split);
    let first_two = focus(&layout);
    let args = |pane: u64| format!(r#"{{"pane":{pane},"cols":80,"rows":24,"cell_w":8,"cell_h":16}}"#);
    one.ok("start_pane", &args(first_one)).await;
    two.ok("start_pane", &args(first_two)).await;
    let seen_one = one
        .event_where("frame", Duration::from_secs(5), |p| p["pane"] == serde_json::json!(first_one))
        .await
        .expect("person one sees their frame");
    let seen_two = two
        .event_where("frame", Duration::from_secs(5), |p| p["pane"] == serde_json::json!(first_two))
        .await
        .expect("person two sees their frame");
    assert_eq!(seen_one["payload"]["pane"], serde_json::json!(first_one));
    assert_eq!(seen_two["payload"]["pane"], serde_json::json!(first_two));
    // Everything that is left in either queue belongs to its own person: no text of one names the other's pane.
    for text in one.drain_texts().await {
        assert!(!text.contains(&format!("\"pane\":{first_two},")) || first_one == first_two, "{text}");
    }
    for text in two.drain_texts().await {
        assert!(!text.contains(&format!("\"pane\":{first_one},")) || first_one == first_two, "{text}");
    }
    env.supervisor.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_the_first_frames_are_init_with_the_caps_then_attach_and_the_end_detaches() {
    let env = ScriptEnv::start().await;
    let client = env.client(account(1, 20001)).await;
    let mut script = env.launcher.script(PATIENCE);
    let id = client.id();
    assert_ne!(id, 0, "the socket number is never 0");
    let settings = holzwave_server::supervisor::SupervisorSettings::default();
    script.wait_for("Attach", PATIENCE, |f| matches!(f, Frame::Ctl(Ctl::Attach { .. })));
    assert_eq!(
        script.ctls(),
        vec![expected_init(settings.max_panes, settings.max_scrollback_lines), Ctl::Attach { socket: id }],
        "Init comes first and carries the caps, Attach follows it"
    );
    assert_eq!(env.supervisor.account_stats(account(1, 20001)).sockets, 1);

    drop(client);
    script.wait_for("Detach", PATIENCE, |f| matches!(f, Frame::Ctl(Ctl::Detach { socket }) if *socket == id));
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while env.supervisor.account_stats(account(1, 20001)).sockets != 0 {
        assert!(std::time::Instant::now() < deadline, "the socket count did not return to zero within a second");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(script.ctls().iter().filter(|c| matches!(c, Ctl::Detach { .. })).count(), 1, "Detach is sent once");
    println!("tracer controls seen by the worker: {:?}", script.ctls());
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_command_answer_comes_back_wrapped_for_its_socket_only() {
    let env = ScriptEnv::start().await;
    let mut client = env.client(account(1, 20001)).await;
    let script = env.launcher.script(PATIENCE);
    let id = client.id();
    let mut script = script;
    client.handle.submit(7, "get_layout", "{}").expect("submit");
    let cmd = script.wait_for("the command", PATIENCE, |f| matches!(f, Frame::Cmd(_)));
    let Frame::Cmd(cmd) = cmd else { unreachable!() };
    assert_eq!((cmd.socket, cmd.request), (id, 7));
    assert_eq!(cmd.json, r#"{"cmd":"get_layout","args":{}}"#);
    script.send(Frame::Res(holzwave_server::wire::Res {
        socket: id,
        request: 7,
        body: holzwave_server::wire::ResBody::Ok(r#"{"a":1}"#.into()),
    }));
    let message = client.next_message(PATIENCE).await.expect("the answer");
    assert_eq!(message, Outbound::Text(r#"{"id":7,"ok":{"a":1}}"#.into()));
}
