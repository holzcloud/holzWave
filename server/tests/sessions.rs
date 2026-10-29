//! The supervisor driven through real in-process workers and scripted workers: one worker per person, the order and the
//! wrapping of commands and events, the bounded queue of every socket, hostile worker frames, the replay of a new socket, the
//! owner of a pane's size, the idle rule, the limits, the account observer, the wipes and the shutdown.
//!
//! See `support/sup_env.rs` for the environment.

#[path = "support/scratch.rs"]
mod scratch;
#[path = "support/sup_env.rs"]
mod sup_env;

use std::time::{Duration, Instant};

use holzwave_server::limits::{REMEMBERED_SIZES, SYNTHETIC_REQUEST_BASE, WORKER_QUEUE_FRAMES};
use holzwave_server::supervisor::{AccountKey, AttachError, Outbound, SubmitError, SupervisorSettings};
use holzwave_server::wire::{CmdFrame, Ctl, Evt, Frame, MAX_PAYLOAD, Note, Res, ResBody};
use serde_json::Value;
use sup_env::{Client, PATIENCE, Reply, ScriptEnv, ScriptHandle, SupEnv, account, expected_init};

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

// ---- helpers of the scripted tests --------------------------------------------------------------------------------------

fn evt(target: u32, name: &str, text: &str) -> Frame {
    Frame::Evt(Evt { target, name: name.into(), text: text.into() })
}

fn res_ok(socket: u32, request: u64, body: &str) -> Frame {
    Frame::Res(Res { socket, request, body: ResBody::Ok(body.into()) })
}

/// A length prefix and a payload, as they go on the link.
fn raw(payload: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(payload.len()).expect("a small payload").to_be_bytes().to_vec();
    out.extend_from_slice(payload);
    out
}

/// An event frame with any bytes as its name and body.
fn raw_evt(name: &[u8], body: &[u8]) -> Vec<u8> {
    let mut payload = vec![3, 0, 0, 0, 0, u8::try_from(name.len()).expect("a short name")];
    payload.extend_from_slice(name);
    payload.extend_from_slice(body);
    raw(&payload)
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !condition() {
        assert!(Instant::now() < deadline, "waited {PATIENCE:?} for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The length of the text an event of this name and payload becomes.
fn wrapped_len(name: &str, payload: usize) -> usize {
    "{\"event\":\"".len() + name.len() + "\",\"payload\":".len() + payload + 1
}

/// A JSON payload of exactly `total` bytes that carries the number `n`.
fn payload_of(n: usize, total: usize) -> String {
    let head = format!("{{\"n\":\"{n:06}\",\"p\":\"");
    let tail = "\"}";
    format!("{head}{}{tail}", "x".repeat(total - head.len() - tail.len()))
}

/// The number a payload made by `payload_of` carries.
fn number_of(text: &str) -> usize {
    let value: Value = serde_json::from_str(text).expect("a payload is JSON");
    value["payload"]["n"].as_str().and_then(|n| n.parse().ok()).expect("the number")
}

/// A supervisor over a scripted worker with `sockets` sockets of the person 1, and the script of the worker.
struct Rig {
    env: ScriptEnv,
    script: ScriptHandle,
    clients: Vec<Client>,
}

const ONE: u32 = 20001;

async fn rig(sockets: usize) -> Rig {
    rig_with(sockets, SupervisorSettings::default()).await
}

async fn rig_with(sockets: usize, settings: SupervisorSettings) -> Rig {
    let env = ScriptEnv::start_with(settings).await;
    let mut clients = Vec::new();
    for _ in 0..sockets {
        clients.push(env.client(account(1, ONE)).await);
    }
    let script = env.launcher.script(PATIENCE);
    Rig { env, script, clients }
}

fn one() -> AccountKey {
    account(1, ONE)
}

/// The commands the script has seen, as (socket, request, command, arguments). A command whose text is not JSON (the
/// supervisor forwards unreadable arguments as they are) has the name `?` and no arguments.
fn cmds(script: &ScriptHandle) -> Vec<(u32, u64, String, Value)> {
    script
        .cmds()
        .into_iter()
        .map(|c| match serde_json::from_str::<Value>(&c.json) {
            Ok(v) => (c.socket, c.request, v["cmd"].as_str().unwrap_or("?").to_owned(), v["args"].clone()),
            Err(_) => (c.socket, c.request, "?".to_owned(), Value::Null),
        })
        .collect()
}

fn resize_args(pane: u64, cols: u32, rows: u32) -> String {
    format!(r#"{{"pane":{pane},"cols":{cols},"rows":{rows}}}"#)
}

fn key_args(pane: u64) -> String {
    format!(r#"{{"pane":{pane},"code":"KeyA","key":"a","mods":0,"repeat":false,"release":false,"composing":false}}"#)
}

// ---- order --------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn order_300_commands_of_one_socket_arrive_in_the_order_submitted_with_their_request_numbers() {
    let mut rig = rig(1).await;
    for i in 0..300u64 {
        rig.clients[0].handle.submit(1000 + i, "get_layout", &format!(r#"{{"i":{i}}}"#)).expect("submit");
    }
    let id = rig.clients[0].id();
    rig.script.wait_for("the last command", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == 1299));
    let seen = rig.script.cmds();
    assert_eq!(seen.len(), 300);
    for (i, cmd) in seen.iter().enumerate() {
        assert_eq!((cmd.socket, cmd.request), (id, 1000 + i as u64));
        assert_eq!(cmd.json, format!(r#"{{"cmd":"get_layout","args":{{"i":{i}}}}}"#));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn order_two_sockets_interleaving_keep_each_sockets_own_order() {
    let mut rig = rig(2).await;
    let (a, b) = (rig.clients[0].id(), rig.clients[1].id());
    for i in 0..100u64 {
        rig.clients[0].handle.submit(i, "get_layout", "{}").expect("submit a");
        rig.clients[1].handle.submit(i, "get_layout", "{}").expect("submit b");
    }
    rig.script.wait_for("the last command", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == 99 && c.socket == b));
    rig.script.settle(Duration::from_millis(100));
    for socket in [a, b] {
        let requests: Vec<u64> = rig.script.cmds().iter().filter(|c| c.socket == socket).map(|c| c.request).collect();
        assert_eq!(requests, (0..100).collect::<Vec<u64>>(), "socket {socket} keeps its own order");
    }
}

/// Command arguments that make one frame of exactly `frame` bytes with its length prefix.
fn args_for_frame(frame: usize) -> String {
    let json_overhead = r#"{"cmd":"get_layout","args":{"pad":""}}"#.len();
    let fixed = 4 + 1 + 4 + 8 + json_overhead;
    format!(r#"{{"pad":"{}"}}"#, "x".repeat(frame - fixed))
}

#[tokio::test(flavor = "multi_thread")]
async fn order_a_worker_that_stops_reading_gets_1024_frames_and_the_next_one_is_busy() {
    let env = ScriptEnv::start().await;
    env.launcher.set_send_buffer(1);
    let client = env.client(one()).await;
    let mut script = env.launcher.script(PATIENCE);
    script.wait_for("Attach", PATIENCE, |f| matches!(f, Frame::Ctl(Ctl::Attach { .. })));
    script.pause();
    let args = args_for_frame(4096);
    let mut accepted = 0u64;
    let refused = loop {
        match client.handle.submit(accepted, "get_layout", &args) {
            Ok(()) => accepted += 1,
            Err(e) => break e,
        }
        assert!(accepted < 2 * WORKER_QUEUE_FRAMES as u64, "the queue never answered Busy");
        if accepted % 64 == 0 {
            tokio::task::yield_now().await;
        }
    };
    assert_eq!(refused, SubmitError::Busy);
    // The frames the operating system took into its buffers do not count; everything else is at most the bound.
    let slack = 128;
    assert!(accepted >= WORKER_QUEUE_FRAMES as u64 && accepted <= WORKER_QUEUE_FRAMES as u64 + slack, "{accepted} frames accepted");
    let queued = env.supervisor.account_stats(one()).worker_queue_frames;
    assert!(queued <= WORKER_QUEUE_FRAMES, "{queued} frames are queued");
    // The refused call queued nothing: when the worker reads again exactly the accepted frames arrive, in order.
    script.resume();
    let mut got = 0u64;
    while got < accepted {
        let Frame::Cmd(cmd) = script.wait_for("a command", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == got)) else { unreachable!() };
        assert_eq!(cmd.request, got);
        got += 1;
    }
    script.settle(Duration::from_millis(200));
    assert_eq!(script.cmds().len() as u64, accepted, "nothing beyond the accepted frames arrives");
}

#[tokio::test(flavor = "multi_thread")]
async fn order_a_worker_that_stops_reading_gets_32_mib_in_four_frames_and_the_fifth_is_busy() {
    let env = ScriptEnv::start().await;
    env.launcher.set_send_buffer(1);
    let client = env.client(one()).await;
    let mut script = env.launcher.script(PATIENCE);
    script.wait_for("Attach", PATIENCE, |f| matches!(f, Frame::Ctl(Ctl::Attach { .. })));
    script.pause();
    let args = args_for_frame(8 * 1024 * 1024);
    for i in 0..4u64 {
        client.handle.submit(i, "get_layout", &args).unwrap_or_else(|e| panic!("frame {i} of 8 MiB: {e:?}"));
    }
    assert_eq!(client.handle.submit(4, "get_layout", &args), Err(SubmitError::Busy), "the fifth frame is one past 32 MiB");
    assert_eq!(client.handle.submit(5, "get_layout", "{}"), Err(SubmitError::Busy), "even a small frame does not fit");
    script.resume();
    for i in 0..4u64 {
        script.wait_for("a big command", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == i));
    }
    script.settle(Duration::from_millis(200));
    assert_eq!(script.cmds().len(), 4, "the refused calls queued nothing");
    // With the queue empty again a call is accepted.
    client.handle.submit(9, "get_layout", "{}").expect("accepted after the queue drained");
}

#[tokio::test(flavor = "multi_thread")]
async fn order_a_command_name_outside_the_character_set_or_length_is_refused_and_sends_nothing() {
    let mut rig = rig(1).await;
    let long_ok = "a".repeat(64);
    let long_bad = "a".repeat(65);
    for name in ["", "A", "get-layout", "a b", "a\"}", "é", "get_layout\n", long_bad.as_str()] {
        assert_eq!(rig.clients[0].handle.submit(1, name, "{}"), Err(SubmitError::BadCommand), "{name:?}");
    }
    for (i, name) in [long_ok.as_str(), "a0_z", "x"].into_iter().enumerate() {
        rig.clients[0].handle.submit(10 + i as u64, name, "{}").expect(name);
    }
    rig.script.wait_for("the last command", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == 12));
    rig.script.settle(Duration::from_millis(100));
    let requests: Vec<u64> = rig.script.cmds().iter().map(|c| c.request).collect();
    assert_eq!(requests, vec![10, 11, 12], "only the accepted names were sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn order_a_request_number_in_the_range_of_the_supervisor_is_refused() {
    let mut rig = rig(1).await;
    let below = SYNTHETIC_REQUEST_BASE - 1;
    rig.clients[0].handle.submit(below, "get_layout", "{}").expect("the last number of the page's range");
    for request in [SYNTHETIC_REQUEST_BASE, SYNTHETIC_REQUEST_BASE + 1, u64::MAX] {
        assert_eq!(rig.clients[0].handle.submit(request, "get_layout", "{}"), Err(SubmitError::BadRequest), "{request}");
    }
    rig.script.wait_for("the command", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == below));
    rig.script.settle(Duration::from_millis(100));
    assert_eq!(rig.script.cmds().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn order_the_argument_text_is_passed_on_as_it_is() {
    let mut rig = rig(1).await;
    let id = rig.clients[0].id();
    for (request, args) in [(1u64, r#"{"x": 1}"#), (2, "[1, 2 ]"), (3, ""), (4, "   "), (5, "not json at all")] {
        rig.clients[0].handle.submit(request, "get_layout", args).expect("submit");
    }
    rig.script.wait_for("the last command", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == 5));
    let json: Vec<(u32, u64, String)> = rig.script.cmds().into_iter().map(|c| (c.socket, c.request, c.json)).collect();
    assert_eq!(
        json,
        vec![
            (id, 1, r#"{"cmd":"get_layout","args":{"x": 1}}"#.to_owned()),
            (id, 2, r#"{"cmd":"get_layout","args":[1, 2 ]}"#.to_owned()),
            (id, 3, r#"{"cmd":"get_layout"}"#.to_owned()),
            (id, 4, r#"{"cmd":"get_layout"}"#.to_owned()),
            (id, 5, r#"{"cmd":"get_layout","args":not json at all}"#.to_owned()),
        ]
    );
}

// ---- wrapping -----------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn wrapping_a_result_becomes_ok_text_for_its_socket_only() {
    let mut rig = rig(2).await;
    let (a, b) = (rig.clients[0].id(), rig.clients[1].id());
    rig.script.send(res_ok(a, 7, r#"{"a":1}"#));
    let text = rig.clients[0].next_message(PATIENCE).await.expect("the answer");
    assert_eq!(text, Outbound::Text(r#"{"id":7,"ok":{"a":1}}"#.into()));
    assert_eq!(rig.clients[1].next_message(Duration::from_millis(200)).await, None, "socket {b} got nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn wrapping_an_error_becomes_err_text() {
    let mut rig = rig(1).await;
    let id = rig.clients[0].id();
    rig.script.send(Frame::Res(Res { socket: id, request: 7, body: ResBody::Err(r#""nope""#.into()) }));
    let text = rig.clients[0].next_message(PATIENCE).await.expect("the answer");
    assert_eq!(text, Outbound::Text(r#"{"id":7,"err":"nope"}"#.into()));
}

#[tokio::test(flavor = "multi_thread")]
async fn wrapping_a_binary_result_is_eight_bytes_of_the_request_number_then_the_bytes() {
    let mut rig = rig(1).await;
    let id = rig.clients[0].id();
    let request = 0x0102_0304_0506_0708u64;
    rig.script.send(Frame::Res(Res { socket: id, request, body: ResBody::Binary(vec![9, 8, 7]) }));
    let bytes = rig.clients[0].binary(PATIENCE).await.expect("a binary message");
    assert_eq!(bytes, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 8, 7]);
}

#[tokio::test(flavor = "multi_thread")]
async fn wrapping_an_event_for_target_0_reaches_every_socket_in_the_order_sent() {
    let mut rig = rig(3).await;
    for n in 0..20 {
        rig.script.send(evt(0, "layout", &format!(r#"{{"n":{n}}}"#)));
    }
    for client in &mut rig.clients {
        for n in 0..20 {
            let message = client.next_message(PATIENCE).await.expect("an event");
            assert_eq!(message, Outbound::Text(format!(r#"{{"event":"layout","payload":{{"n":{n}}}}}"#)));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn wrapping_an_event_for_a_target_reaches_that_socket_only() {
    let mut rig = rig(3).await;
    let target = rig.clients[1].id();
    rig.script.send(evt(target, "title", r#"{"pane":1,"title":"x"}"#));
    let message = rig.clients[1].next_message(PATIENCE).await.expect("the event");
    assert_eq!(message, Outbound::Text(r#"{"event":"title","payload":{"pane":1,"title":"x"}}"#.into()));
    for i in [0, 2] {
        assert_eq!(rig.clients[i].next_message(Duration::from_millis(200)).await, None, "socket {i}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn wrapping_a_message_for_a_socket_that_is_not_attached_is_dropped_and_counted() {
    let mut rig = rig(1).await;
    let id = rig.clients[0].id();
    assert_eq!(rig.env.supervisor.stats().dropped_unknown_socket, 0);
    rig.script.send(evt(id + 100, "title", "{}"));
    rig.script.send(res_ok(id + 100, 1, "null"));
    // A socket that left while its answer was on the way.
    let gone = rig.env.client(one()).await;
    let gone_id = gone.id();
    drop(gone);
    wait_until("the detach", || rig.env.supervisor.account_stats(one()).sockets == 1).await;
    rig.script.send(res_ok(gone_id, 2, "null"));
    wait_until("three drops", || rig.env.supervisor.stats().dropped_unknown_socket == 3).await;
    assert_eq!(rig.clients[0].next_message(Duration::from_millis(100)).await, None);
}

// ---- checks of what a worker sends ----------------------------------------------------------------------------------

/// Two valid events, then what `hostile` does: every socket gets the two events and then the close 1012, and the next
/// attach starts a new worker.
async fn hostile_case(what: &str, hostile: impl FnOnce(&ScriptHandle)) {
    let mut rig = rig(2).await;
    assert_eq!(rig.env.launcher.launches(), 1);
    rig.script.send(evt(0, "layout", r#"{"n":1}"#));
    rig.script.send(evt(0, "layout", r#"{"n":2}"#));
    // Wait until both are delivered, so that the order of the checks is the order of the frames.
    for client in &mut rig.clients {
        let first = client.next_message(PATIENCE).await;
        assert_eq!(first, Some(Outbound::Text(r#"{"event":"layout","payload":{"n":1}}"#.into())), "{what}");
    }
    hostile(&rig.script);
    for client in &mut rig.clients {
        let second = client.next_message(PATIENCE).await;
        assert_eq!(second, Some(Outbound::Text(r#"{"event":"layout","payload":{"n":2}}"#.into())), "{what}");
        assert_eq!(client.close_code(PATIENCE).await, Some(1012), "{what}: the socket closes with 1012");
    }
    assert_eq!(rig.env.launcher.launches(), 1, "{what}: nothing was started again by itself");
    wait_until("the account to have no sockets", || rig.env.supervisor.account_stats(one()).sockets == 0).await;
    assert!(!rig.env.supervisor.account_stats(one()).worker_running, "{what}: the worker is declared dead");
    assert!(rig.env.launcher.kills().contains(&ONE), "{what}: the processes of the person are killed");
    let again = rig.env.client(one()).await;
    assert_eq!(rig.env.launcher.launches(), 2, "{what}: the next attach starts a new worker");
    drop(again);
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_a_length_over_the_limit_ends_the_link_before_the_frame_is_read() {
    let over = u32::try_from(MAX_PAYLOAD + 1).expect("fits").to_be_bytes();
    hostile_case("a length of 16 MiB + 65", |script| script.send_raw(&over)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_a_length_of_zero_ends_the_link() {
    hostile_case("a length of zero", |script| script.send_raw(&[0, 0, 0, 0])).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_an_unknown_kind_ends_the_link() {
    hostile_case("kind 9", |script| script.send_raw(&raw(&[9, 0, 0]))).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_a_kind_that_only_the_supervisor_sends_ends_the_link() {
    hostile_case("a command from the worker", |script| {
        script.send(Frame::Cmd(CmdFrame { socket: 1, request: 1, json: "{}".into() }))
    })
    .await;
    hostile_case("a control from the worker", |script| script.send(Frame::Ctl(Ctl::Lock { ctl: 1 }))).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_an_unknown_note_ends_the_link() {
    hostile_case("note 99", |script| script.send_raw(&raw(&[5, 99]))).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_a_frame_cut_off_at_the_end_of_the_file_ends_the_link() {
    hostile_case("a truncated frame", |script| {
        script.send_raw(&[0, 0, 0, 10, 3, 0, 0]);
        script.close();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_an_event_name_that_is_empty_too_long_or_outside_the_set_ends_the_link() {
    let thirty_three = [b'a'; 33];
    let cases: [(&str, &[u8]); 5] = [
        ("an empty name", b""),
        ("a name of 33 bytes", &thirty_three),
        ("a capital letter", b"Layout"),
        ("a space", b"lay out"),
        ("a byte over 127", &[b'a', 0xC3, 0xA9]),
    ];
    for (what, name) in cases {
        hostile_case(what, |script| script.send_raw(&raw_evt(name, b"{}"))).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_an_event_name_of_32_bytes_is_accepted() {
    let mut rig = rig(1).await;
    let name = "a".repeat(32);
    rig.script.send(evt(0, &name, "{}"));
    let message = rig.clients[0].next_message(PATIENCE).await.expect("the event");
    assert_eq!(message, Outbound::Text(format!(r#"{{"event":"{name}","payload":{{}}}}"#)));
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_an_event_body_that_is_not_utf8_ends_the_link() {
    hostile_case("a body that is not UTF-8", |script| script.send_raw(&raw_evt(b"layout", &[b'{', 0xFF, b'}']))).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_a_second_ready_ends_the_link() {
    hostile_case("a second Ready", |script| script.send(Frame::Note(Note::Ready))).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_a_worker_that_never_says_ready_fails_the_attach_with_a_sentence_and_the_uid_is_killed() {
    let settings = SupervisorSettings { ready_timeout: Duration::from_millis(300), ..SupervisorSettings::default() };
    let env = ScriptEnv::start_with(settings).await;
    env.launcher.set_auto_ready(false);
    let started = Instant::now();
    let refused = env.supervisor.attach(one()).await;
    let Err(AttachError::Launch(sentence)) = refused else { panic!("the attach must fail: {:?}", refused.map(|_| ())) };
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(sentence.contains("ready"), "{sentence}");
    assert_eq!(env.launcher.kills(), vec![ONE]);
    assert!(!env.supervisor.account_stats(one()).worker_running);
    // The failed worker left nothing behind: the next attach starts another and succeeds.
    env.launcher.set_auto_ready(true);
    let _client = env.client(one()).await;
    assert_eq!(env.launcher.launches(), 2);
}

// ---- the queue of a socket ----------------------------------------------------------------------------------------------

const CAP: usize = 4 * 1024 * 1024;

async fn send_frames(script: &ScriptHandle, from: usize, to: usize, size: usize) {
    for n in from..to {
        script.send(evt(0, "frame", &payload_of(n, size - wrapped_len("frame", 0))));
        if n % 256 == 0 {
            tokio::task::yield_now().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn queues_a_socket_that_never_reads_keeps_the_first_4_mib_and_drops_the_frames_after_it() {
    let mut rig = rig(1).await;
    // 5,000 frame events of 1 KiB each; exactly 4,096 fit.
    send_frames(&rig.script, 0, 5000, 1024).await;
    wait_until("the drops", || rig.env.supervisor.stats().dropped_frames == 904).await;
    let stats = rig.env.supervisor.account_stats(one());
    assert_eq!(stats.queued_bytes, CAP, "the queue holds exactly the cap");
    assert_eq!(rig.env.supervisor.stats().overflow_closes, 0, "a socket is not closed for a dropped frame");
    // What it kept are the first 4,096, in order, and no close message.
    for n in 0..4096 {
        let Some(Outbound::Text(text)) = rig.clients[0].next_message(PATIENCE).await else { panic!("message {n}") };
        assert_eq!(number_of(&text), n);
    }
    assert_eq!(rig.clients[0].next_message(Duration::from_millis(200)).await, None);
    assert_eq!(rig.env.supervisor.account_stats(one()).queued_bytes, 0, "the bytes are released as the receiver takes them");
    assert_eq!(rig.env.supervisor.account_stats(one()).sockets, 1, "the socket is still attached");
}

#[tokio::test(flavor = "multi_thread")]
async fn queues_exactly_the_cap_fits_and_one_byte_more_does_not() {
    // 4,095 messages of 1 KiB leave exactly 1 KiB of room.
    let rig = rig(1).await;
    send_frames(&rig.script, 0, 4095, 1024).await;
    wait_until("4,095 queued", || rig.env.supervisor.account_stats(one()).queued_bytes == CAP - 1024).await;
    send_frames(&rig.script, 4095, 4096, 1024).await;
    wait_until("the cap", || rig.env.supervisor.account_stats(one()).queued_bytes == CAP).await;
    assert_eq!(rig.env.supervisor.stats().dropped_frames, 0, "a message that fills the queue exactly fits");

    let rig = self::rig(1).await;
    send_frames(&rig.script, 0, 4095, 1024).await;
    wait_until("4,095 queued", || rig.env.supervisor.account_stats(one()).queued_bytes == CAP - 1024).await;
    send_frames(&rig.script, 4095, 4096, 1025).await;
    wait_until("the drop", || rig.env.supervisor.stats().dropped_frames == 1).await;
    assert_eq!(rig.env.supervisor.account_stats(one()).queued_bytes, CAP - 1024, "a message one byte over is dropped whole");
}

#[tokio::test(flavor = "multi_thread")]
async fn queues_the_next_event_that_is_not_a_frame_and_does_not_fit_closes_that_socket_with_1013() {
    let mut rig = rig(1).await;
    send_frames(&rig.script, 0, 4096, 1024).await;
    wait_until("a full queue", || rig.env.supervisor.account_stats(one()).queued_bytes == CAP).await;
    rig.script.send(evt(0, "layout", &payload_of(7777, 100)));
    wait_until("the close", || rig.env.supervisor.stats().overflow_closes == 1).await;
    let mut taken = 0;
    let last = loop {
        match rig.clients[0].next_message(PATIENCE).await.expect("a message") {
            Outbound::Close { code, .. } => break code,
            Outbound::Text(_) => taken += 1,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!((taken, last), (4096, 1013), "the queued messages come first and the close is the last one");
    assert_eq!(rig.clients[0].next_message(Duration::from_millis(100)).await, None, "nothing follows the close");
    wait_until("the socket to leave", || rig.env.supervisor.account_stats(one()).sockets == 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn queues_a_slow_socket_does_not_delay_a_fast_one_and_the_worker_is_untouched() {
    let rig = rig(2).await;
    let Rig { env, mut script, mut clients } = rig;
    let mut slow = clients.remove(0);
    let mut fast = clients.remove(0);
    let fast_id = fast.id();
    let reader = tokio::spawn(async move {
        let mut seen = Vec::new();
        while seen.len() < 5001 {
            match fast.next_message(PATIENCE).await {
                Some(Outbound::Text(text)) => seen.push(text),
                other => panic!("the fast socket got {other:?} after {} messages", seen.len()),
            }
        }
        (fast, seen)
    });
    // 5,000 frame events and then an event that is not a frame: the slow socket drops frames and is then closed.
    send_frames(&script, 0, 5000, 1024).await;
    script.send(evt(0, "layout", &payload_of(5000, 900)));
    let (mut fast, seen) = reader.await.expect("the reader task");
    for (n, text) in seen.iter().enumerate() {
        assert_eq!(number_of(text), n, "the fast socket gets every event, in order");
    }
    // The worker's answer for the fast socket still arrives, and the worker got the fast socket's command.
    fast.handle.submit(3, "get_layout", "{}").expect("the fast socket still works");
    script.wait_for("the command", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.socket == fast_id));
    script.send(res_ok(fast_id, 3, "1"));
    assert_eq!(fast.next_message(PATIENCE).await, Some(Outbound::Text(r#"{"id":3,"ok":1}"#.into())));
    // The slow socket kept its first 4 MiB and then was closed with 1013.
    wait_until("the close of the slow socket", || env.supervisor.stats().overflow_closes == 1).await;
    let mut taken = 0;
    let code = loop {
        match slow.next_message(PATIENCE).await.expect("a message") {
            Outbound::Close { code, .. } => break code,
            _ => taken += 1,
        }
    };
    assert_eq!((taken, code), (4096, 1013));
    assert_eq!(env.launcher.launches(), 1, "the worker was never touched");
}

#[tokio::test(flavor = "multi_thread")]
async fn queues_a_message_over_the_whole_cap_is_accepted_only_when_the_queue_is_empty() {
    // Empty queue: a message of 6 MiB is accepted whole.
    let mut rig = rig(1).await;
    let big = payload_of(1, 6 * 1024 * 1024);
    rig.script.send(evt(0, "layout", &big));
    wait_until("the big message", || rig.env.supervisor.account_stats(one()).queued_bytes > 6 * 1024 * 1024).await;
    let Some(Outbound::Text(text)) = rig.clients[0].next_message(PATIENCE).await else { panic!("the big message") };
    assert_eq!(text.len(), wrapped_len("layout", big.len()));
    assert_eq!(rig.env.supervisor.stats().overflow_closes, 0);
    // A queue that holds something: the same message closes the socket.
    rig.script.send(evt(0, "layout", &payload_of(2, 100)));
    rig.script.send(evt(0, "layout", &big));
    wait_until("the close", || rig.env.supervisor.stats().overflow_closes == 1).await;
    let mut codes = Vec::new();
    while let Some(message) = rig.clients[0].next_message(Duration::from_millis(300)).await {
        match message {
            Outbound::Close { code, .. } => codes.push(code),
            Outbound::Text(t) => assert!(t.len() < 1000, "the big message was not queued"),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(codes, vec![1013]);
}

#[tokio::test(flavor = "multi_thread")]
async fn queues_bytes_are_counted_when_queued_and_released_when_taken() {
    let mut rig = rig(1).await;
    let size = 1000;
    for n in 0..10 {
        rig.script.send(evt(0, "layout", &payload_of(n, size)));
    }
    let each = wrapped_len("layout", size);
    wait_until("ten queued", || rig.env.supervisor.account_stats(one()).queued_bytes == 10 * each).await;
    for taken in 1..=10 {
        rig.clients[0].next_message(PATIENCE).await.expect("a message");
        assert_eq!(rig.env.supervisor.account_stats(one()).queued_bytes, (10 - taken) * each);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn queues_a_dropped_receiver_frees_the_socket_and_its_queue() {
    let mut rig = rig(2).await;
    let Client { handle, rx, .. } = rig.clients.remove(0);
    let gone = handle.id();
    for n in 0..10 {
        rig.script.send(evt(0, "layout", &payload_of(n, 500)));
    }
    wait_until("queued", || rig.env.supervisor.account_stats(one()).queued_bytes >= 10 * 500).await;
    drop(rx);
    wait_until("the socket to leave", || rig.env.supervisor.account_stats(one()).sockets == 1).await;
    rig.script.wait_for("Detach", PATIENCE, |f| matches!(f, Frame::Ctl(Ctl::Detach { socket }) if *socket == gone));
    assert_eq!(handle.submit(1, "get_layout", "{}"), Err(SubmitError::Closed), "a socket that left cannot submit");
    // What is left in the account is the other socket's queue only.
    let others = rig.env.supervisor.account_stats(one()).queued_bytes;
    assert_eq!(others, 10 * wrapped_len("layout", 500));
}

// ---- replay -------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn replay_attach_is_the_first_control_after_ready_and_follows_the_frames_for_a_later_socket() {
    let mut rig = rig(1).await;
    let a = rig.clients[0].id();
    rig.clients[0].handle.submit(1, "get_layout", "{}").expect("submit");
    let b = rig.env.client(one()).await;
    let b_id = b.id();
    rig.script.wait_for("Attach of the second", PATIENCE, |f| matches!(f, Frame::Ctl(Ctl::Attach { socket }) if *socket == b_id));
    let kinds: Vec<String> = rig
        .script
        .seen()
        .iter()
        .map(|f| match f {
            Frame::Ctl(Ctl::Init { .. }) => "init".to_owned(),
            Frame::Ctl(Ctl::Attach { socket }) => format!("attach {socket}"),
            Frame::Cmd(c) => format!("cmd {} {}", c.socket, c.request),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(kinds, vec!["init".to_owned(), format!("attach {a}"), format!("cmd {a} 1"), format!("attach {b_id}")]);
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_events_addressed_to_the_new_socket_reach_it_alone() {
    let mut rig = rig(1).await;
    let mut b = rig.env.client(one()).await;
    let b_id = b.id();
    rig.script.wait_for("Attach", PATIENCE, |f| matches!(f, Frame::Ctl(Ctl::Attach { socket }) if *socket == b_id));
    for pane in 1..=3 {
        rig.script.send(evt(b_id, "title", &format!(r#"{{"pane":{pane},"title":"t{pane}"}}"#)));
        rig.script.send(evt(b_id, "cwd", &format!(r#"{{"pane":{pane},"cwd":"/home/{pane}"}}"#)));
    }
    let mut names = Vec::new();
    for _ in 0..6 {
        let Some(Outbound::Text(text)) = b.next_message(PATIENCE).await else { panic!("a replay event") };
        let v: Value = serde_json::from_str(&text).expect("JSON");
        names.push(v["event"].as_str().expect("name").to_owned());
    }
    assert_eq!(names, ["title", "cwd", "title", "cwd", "title", "cwd"]);
    assert_eq!(rig.clients[0].next_message(Duration::from_millis(200)).await, None, "the first socket saw none of it");
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_comes_before_the_later_events_for_everybody_and_after_none_of_the_earlier_ones() {
    let mut rig = rig(1).await;
    rig.script.send(evt(0, "layout", r#"{"n":1}"#));
    let first = rig.clients[0].next_message(PATIENCE).await.expect("the first event reaches the first socket");
    assert_eq!(first, Outbound::Text(r#"{"event":"layout","payload":{"n":1}}"#.into()));
    let mut b = rig.env.client(one()).await;
    let b_id = b.id();
    rig.script.wait_for("Attach", PATIENCE, |f| matches!(f, Frame::Ctl(Ctl::Attach { socket }) if *socket == b_id));
    rig.script.send(evt(b_id, "title", r#"{"pane":1,"title":"replayed"}"#));
    rig.script.send(evt(0, "layout", r#"{"n":2}"#));
    let seen: Vec<String> = {
        let mut out = Vec::new();
        for _ in 0..2 {
            let Some(Outbound::Text(text)) = b.next_message(PATIENCE).await else { panic!("an event") };
            out.push(text);
        }
        out
    };
    assert_eq!(
        seen,
        vec![
            r#"{"event":"title","payload":{"pane":1,"title":"replayed"}}"#.to_owned(),
            r#"{"event":"layout","payload":{"n":2}}"#.to_owned()
        ],
        "the replay first, then the later event, and not the earlier one"
    );
    assert_eq!(b.next_message(Duration::from_millis(200)).await, None);
    let second = rig.clients[0].next_message(PATIENCE).await;
    assert_eq!(second, Some(Outbound::Text(r#"{"event":"layout","payload":{"n":2}}"#.into())), "the first socket sees n=2 too");
}

// ---- the owner of a pane's size ---------------------------------------------------------------------------------------

/// What the script has seen of the commands of the size rule, as (socket, request is synthetic, command, pane, cols, rows).
fn size_cmds(script: &ScriptHandle) -> Vec<(u32, bool, String, u64, u64, u64)> {
    cmds(script)
        .into_iter()
        .map(|(socket, request, name, args)| {
            (
                socket,
                request >= SYNTHETIC_REQUEST_BASE,
                name,
                args["pane"].as_u64().unwrap_or(0),
                args["cols"].as_u64().unwrap_or(0),
                args["rows"].as_u64().unwrap_or(0),
            )
        })
        .collect()
}

async fn start_pane_by(rig: &mut Rig, client: usize, pane: u64) {
    let request = 9000 + pane;
    rig.clients[client].handle.submit(request, "start_pane", &resize_args(pane, 80, 24)).expect("submit");
    rig.script.wait_for("start_pane", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == request));
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_a_resize_from_the_owner_reaches_the_worker_and_one_from_another_socket_is_answered_null() {
    let mut rig = rig(2).await;
    let (a, b) = (rig.clients[0].id(), rig.clients[1].id());
    start_pane_by(&mut rig, 0, 5).await;
    rig.clients[0].handle.submit(1, "resize", &resize_args(5, 100, 30)).expect("a resizes");
    rig.clients[1].handle.submit(2, "resize", &resize_args(5, 90, 20)).expect("b resizes");
    // B's resize is answered at once with null and never reaches the worker.
    assert_eq!(rig.clients[1].wait_reply(2).await, Reply::Ok(Value::Null));
    rig.script.wait_for("a's resize", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == 1));
    rig.script.settle(Duration::from_millis(150));
    assert_eq!(
        size_cmds(&rig.script),
        vec![(a, false, "start_pane".to_owned(), 5, 80, 24), (a, false, "resize".to_owned(), 5, 100, 30)],
        "the worker saw start_pane and a's resize, and nothing from b ({b})"
    );
    println!("owner frames the worker saw: {:?}", size_cmds(&rig.script));
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_input_from_another_socket_moves_the_pane_and_sends_its_remembered_size_first() {
    let mut rig = rig(2).await;
    let (a, b) = (rig.clients[0].id(), rig.clients[1].id());
    start_pane_by(&mut rig, 0, 5).await;
    rig.clients[1].handle.submit(2, "resize", &resize_args(5, 90, 20)).expect("b resizes");
    assert_eq!(rig.clients[1].wait_reply(2).await, Reply::Ok(Value::Null));
    // B types: the supervisor makes B's size the pane's size, then the key follows.
    rig.clients[1].handle.submit(3, "key", &key_args(5)).expect("b types");
    rig.script.wait_for("b's key", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == 3));
    let seen = cmds(&rig.script);
    let tail: Vec<(u32, bool, &str)> = seen
        .iter()
        .skip(1)
        .map(|(socket, request, name, _)| (*socket, *request >= SYNTHETIC_REQUEST_BASE, name.as_str()))
        .collect();
    assert_eq!(tail, vec![(b, true, "resize"), (b, false, "key")], "the synthetic resize comes before the key");
    let (_, _, _, args) = &seen[1];
    assert_eq!((args["pane"].as_u64(), args["cols"].as_u64(), args["rows"].as_u64()), (Some(5), Some(90), Some(20)));
    // A is no longer the owner: its resize is the one that is answered null now, and B's goes through.
    rig.clients[0].handle.submit(4, "resize", &resize_args(5, 111, 41)).expect("a resizes");
    assert_eq!(rig.clients[0].wait_reply(4).await, Reply::Ok(Value::Null));
    rig.clients[1].handle.submit(5, "resize", &resize_args(5, 91, 21)).expect("b resizes");
    rig.script.wait_for("b's resize", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == 5));
    rig.script.settle(Duration::from_millis(100));
    let resizes: Vec<(u32, u64)> =
        size_cmds(&rig.script).into_iter().filter(|c| c.2 == "resize" && !c.1).map(|c| (c.0, c.4)).collect();
    assert_eq!(resizes, vec![(b, 91)], "only b's own resize went through; a's did not ({a})");
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_the_answer_to_the_supervisors_own_resize_is_dropped_by_its_request_number() {
    let mut rig = rig(2).await;
    let b = rig.clients[1].id();
    start_pane_by(&mut rig, 0, 5).await;
    rig.clients[1].handle.submit(2, "resize", &resize_args(5, 90, 20)).expect("b resizes");
    assert_eq!(rig.clients[1].wait_reply(2).await, Reply::Ok(Value::Null));
    rig.clients[1].handle.submit(3, "key", &key_args(5)).expect("b types");
    rig.script.wait_for("b's key", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == 3));
    let synthetic = rig.script.cmds().into_iter().find(|c| c.request >= SYNTHETIC_REQUEST_BASE).expect("a synthetic resize");
    assert_eq!(synthetic.socket, b);
    let before = rig.env.supervisor.stats();
    rig.script.send(res_ok(synthetic.socket, synthetic.request, "null"));
    rig.script.send(res_ok(b, 3, "null"));
    // The answer to the key arrives; the answer to the synthetic resize does not.
    assert_eq!(rig.clients[1].next_message(PATIENCE).await, Some(Outbound::Text(r#"{"id":3,"ok":null}"#.into())));
    assert_eq!(rig.clients[1].next_message(Duration::from_millis(200)).await, None);
    let after = rig.env.supervisor.stats();
    assert_eq!(after.dropped_synthetic, before.dropped_synthetic + 1);
    assert_eq!(after.dropped_unknown_socket, before.dropped_unknown_socket, "it is not counted as a socket that left");
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_text_paste_send_edit_and_mouse_each_move_the_owner_and_wheel_and_scroll_do_not() {
    let mut rig = rig(2).await;
    let b = rig.clients[1].id();
    let inputs: [(&str, String, bool); 7] = [
        ("text", r#"{"pane":P,"text":"x"}"#.to_owned(), true),
        ("paste", r#"{"pane":P,"text":"x","allow_unsafe":false}"#.to_owned(), true),
        ("send_edit", r#"{"pane":P,"action":"delete_word"}"#.to_owned(), true),
        ("mouse", r#"{"pane":P,"action":"press","button":"left","x":1.0,"y":2.0,"mods":0}"#.to_owned(), true),
        ("key", key_args(0).replace("\"pane\":0", "\"pane\":P"), true),
        ("wheel", r#"{"pane":P,"rows":-1,"x":1.0,"y":2.0,"mods":0}"#.to_owned(), false),
        ("scroll", r#"{"pane":P,"rows":-1}"#.to_owned(), false),
    ];
    for (i, (name, template, moves)) in inputs.iter().enumerate() {
        let pane = 10 + i as u64;
        start_pane_by(&mut rig, 0, pane).await;
        let request = 100 + i as u64 * 10;
        rig.clients[1].handle.submit(request, "resize", &resize_args(pane, 70 + i as u32, 20)).expect("b resizes");
        assert_eq!(rig.clients[1].wait_reply(request).await, Reply::Ok(Value::Null), "{name}");
        rig.clients[1].handle.submit(request + 1, name, &template.replace('P', &pane.to_string())).expect("b sends input");
        rig.script.wait_for(name, PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == request + 1));
        let synthetic: Vec<_> = size_cmds(&rig.script)
            .into_iter()
            .filter(|c| c.1 && c.3 == pane)
            .collect();
        if *moves {
            assert_eq!(synthetic, vec![(b, true, "resize".to_owned(), pane, 70 + i as u64, 20)], "{name} moves the owner");
        } else {
            assert!(synthetic.is_empty(), "{name} does not move the owner: {synthetic:?}");
            // B's next resize is still answered null: A still owns the pane.
            rig.clients[1].handle.submit(request + 2, "resize", &resize_args(pane, 1, 1)).expect("b resizes");
            assert_eq!(rig.clients[1].wait_reply(request + 2).await, Reply::Ok(Value::Null), "{name}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_when_the_owner_leaves_the_socket_with_the_latest_remembered_size_takes_over() {
    let mut rig = rig(3).await;
    let c = rig.clients[2].id();
    start_pane_by(&mut rig, 0, 5).await;
    rig.clients[1].handle.submit(1, "resize", &resize_args(5, 60, 10)).expect("b resizes");
    rig.clients[2].handle.submit(2, "resize", &resize_args(5, 70, 11)).expect("c resizes");
    assert_eq!(rig.clients[1].wait_reply(1).await, Reply::Ok(Value::Null));
    assert_eq!(rig.clients[2].wait_reply(2).await, Reply::Ok(Value::Null));
    let a = rig.clients.remove(0);
    drop(a);
    // C's size was remembered last, so C takes over and its size is applied.
    rig.script.wait_for("the takeover", PATIENCE, |f| matches!(f, Frame::Cmd(cmd) if cmd.request >= SYNTHETIC_REQUEST_BASE));
    let synthetic: Vec<_> = size_cmds(&rig.script).into_iter().filter(|x| x.1).collect();
    assert_eq!(synthetic, vec![(c, true, "resize".to_owned(), 5, 70, 11)]);
    // From now on C's resize goes through and B's does not.
    rig.clients[1].handle.submit(3, "resize", &resize_args(5, 71, 12)).expect("c resizes");
    rig.clients[0].handle.submit(4, "resize", &resize_args(5, 61, 11)).expect("b resizes");
    assert_eq!(rig.clients[0].wait_reply(4).await, Reply::Ok(Value::Null));
    rig.script.wait_for("c's resize", PATIENCE, |f| matches!(f, Frame::Cmd(cmd) if cmd.request == 3));
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_a_resize_for_a_pane_with_no_owner_claims_it() {
    let mut rig = rig(2).await;
    let b = rig.clients[1].id();
    rig.clients[1].handle.submit(1, "resize", &resize_args(9, 50, 15)).expect("b resizes");
    rig.script.wait_for("b's resize", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == 1));
    rig.clients[0].handle.submit(2, "resize", &resize_args(9, 80, 24)).expect("a resizes");
    assert_eq!(rig.clients[0].wait_reply(2).await, Reply::Ok(Value::Null), "b claimed the pane");
    let seen = size_cmds(&rig.script);
    assert_eq!(seen, vec![(b, false, "resize".to_owned(), 9, 50, 15)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_a_second_start_of_a_pane_moves_it_and_applies_the_size_of_the_new_owner() {
    let mut rig = rig(2).await;
    let b = rig.clients[1].id();
    start_pane_by(&mut rig, 0, 7).await;
    rig.clients[1].handle.submit(2, "start_pane", &resize_args(7, 100, 30)).expect("b starts the pane");
    rig.script.wait_for("b's start", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == 2));
    rig.script.settle(Duration::from_millis(150));
    let tail: Vec<_> = size_cmds(&rig.script).into_iter().skip(1).collect();
    assert_eq!(
        tail,
        vec![(b, false, "start_pane".to_owned(), 7, 100, 30), (b, true, "resize".to_owned(), 7, 100, 30)],
        "the start goes through and then the size of the new owner is applied"
    );
    rig.clients[0].handle.submit(3, "resize", &resize_args(7, 1, 1)).expect("a resizes");
    assert_eq!(rig.clients[0].wait_reply(3).await, Reply::Ok(Value::Null));
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_the_remembered_sizes_are_bounded_and_the_oldest_is_forgotten_first() {
    let mut rig = rig(2).await;
    let b = rig.clients[1].id();
    let panes = REMEMBERED_SIZES as u64 + 88;
    for pane in 1..=panes {
        rig.clients[0].handle.submit(pane, "start_pane", &resize_args(pane, 80, 24)).expect("a starts");
    }
    for pane in 1..=panes {
        let request = 10_000 + pane;
        rig.clients[1].handle.submit(request, "resize", &resize_args(pane, 60, 20)).expect("b resizes");
        assert_eq!(rig.clients[1].wait_reply(request).await, Reply::Ok(Value::Null));
    }
    let stats = rig.env.supervisor.account_stats(one());
    assert_eq!(stats.remembered_sizes, REMEMBERED_SIZES, "the table does not grow past its bound");
    assert_eq!(stats.owned_panes as u64, panes);
    // The oldest remembered size is gone: B taking pane 1 sends no resize; B taking the last pane sends one.
    rig.clients[1].handle.submit(20_001, "key", &key_args(1)).expect("b types in pane 1");
    rig.clients[1].handle.submit(20_002, "key", &key_args(panes)).expect("b types in the last pane");
    rig.script.wait_for("the last key", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == 20_002));
    let synthetic: Vec<_> = size_cmds(&rig.script).into_iter().filter(|c| c.1).collect();
    assert_eq!(synthetic, vec![(b, true, "resize".to_owned(), panes, 60, 20)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_arguments_that_cannot_be_read_are_forwarded_unchanged_and_move_nothing() {
    let mut rig = rig(2).await;
    start_pane_by(&mut rig, 0, 5).await;
    let odd: [(&str, &str); 6] = [
        ("resize", "not json"),
        ("resize", "{}"),
        ("resize", r#"{"pane":5,"cols":"80","rows":24}"#),
        ("resize", r#"{"pane":5,"rows":24}"#),
        ("key", r#"{"pane":"five"}"#),
        ("text", r#"{"text":"x"}"#),
    ];
    for (i, (name, args)) in odd.iter().enumerate() {
        rig.clients[1].handle.submit(i as u64 + 1, name, args).expect("b submits");
    }
    rig.script.wait_for("the last", PATIENCE, |f| matches!(f, Frame::Cmd(c) if c.request == 6));
    let forwarded: Vec<(u64, String)> = rig
        .script
        .cmds()
        .into_iter()
        .filter(|c| c.request <= 6)
        .map(|c| (c.request, c.json))
        .collect();
    assert_eq!(forwarded.len(), 6, "all six went to the worker");
    assert!(forwarded[2].1.ends_with(r#""args":{"pane":5,"cols":"80","rows":24}}"#), "{:?}", forwarded[2]);
    // Nobody moved: no synthetic resize, A still owns pane 5 and B's resize is still answered null.
    assert!(size_cmds(&rig.script).iter().all(|c| !c.1));
    rig.clients[1].handle.submit(7, "resize", &resize_args(5, 90, 20)).expect("b resizes");
    assert_eq!(rig.clients[1].wait_reply(7).await, Reply::Ok(Value::Null));
}

// ---- several sockets ------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn several_the_ninth_attach_is_refused_before_anything_is_queued() {
    let mut rig = rig(8).await;
    assert_eq!(rig.env.supervisor.account_stats(one()).sockets, 8);
    rig.script.settle(Duration::from_millis(100));
    let frames_before = rig.script.seen().len();
    let refused = rig.env.supervisor.attach(one()).await;
    assert!(matches!(refused, Err(AttachError::TooManySockets)), "{:?}", refused.map(|_| ()));
    assert_eq!(AttachError::TooManySockets.to_string(), "too many connections");
    rig.script.settle(Duration::from_millis(200));
    assert_eq!(rig.script.seen().len(), frames_before, "nothing was sent to the worker for the ninth socket");
    assert_eq!(rig.env.supervisor.account_stats(one()).sockets, 8, "the eight others are unchanged");
    // The eight keep working.
    for (i, client) in rig.clients.iter_mut().enumerate() {
        client.handle.submit(i as u64 + 1, "get_layout", "{}").expect("an old socket still submits");
    }
    // After one leaves a new one is accepted.
    drop(rig.clients.remove(0));
    wait_until("a free place", || rig.env.supervisor.account_stats(one()).sockets == 7).await;
    assert!(rig.env.supervisor.attach(one()).await.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn several_closing_a_handle_sends_detach_once() {
    let mut rig = rig(2).await;
    let Client { handle, rx, .. } = rig.clients.remove(1);
    let id = handle.id();
    handle.close();
    wait_until("the detach", || rig.env.supervisor.account_stats(one()).sockets == 1).await;
    drop(rx);
    rig.script.wait_for("Detach", PATIENCE, |f| matches!(f, Frame::Ctl(Ctl::Detach { socket }) if *socket == id));
    rig.script.settle(Duration::from_millis(200));
    let detaches = rig.script.ctls().iter().filter(|c| matches!(c, Ctl::Detach { socket } if *socket == id)).count();
    assert_eq!(detaches, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn several_eight_sockets_read_2000_events_in_order_with_random_delays() {
    let rig = rig(8).await;
    let Rig { env, script, clients } = rig;
    let mut readers = Vec::new();
    for (index, mut client) in clients.into_iter().enumerate() {
        readers.push(tokio::spawn(async move {
            let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ (index as u64 + 1);
            let mut seen = Vec::new();
            while seen.len() < 2000 {
                // A small xorshift: about one message in four waits a little.
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                if state % 4 == 0 {
                    tokio::time::sleep(Duration::from_micros(state % 2000)).await;
                }
                match client.next_message(PATIENCE).await {
                    Some(Outbound::Text(text)) => seen.push(text),
                    other => panic!("socket {index} got {other:?} after {} messages", seen.len()),
                }
            }
            seen
        }));
    }
    for n in 0..2000 {
        let name = if n % 10 == 0 { "layout" } else { "frame" };
        script.send(evt(0, name, &payload_of(n, 700)));
        if n % 100 == 0 {
            tokio::task::yield_now().await;
        }
    }
    for (index, reader) in readers.into_iter().enumerate() {
        let seen = reader.await.expect("the reader task");
        let numbers: Vec<usize> = seen.iter().map(|t| number_of(t)).collect();
        assert!(numbers.windows(2).all(|w| w[0] < w[1]), "socket {index} saw the events out of order");
        // Any gap is a frame event.
        let missing: Vec<usize> = (0..2000).filter(|n| !numbers.contains(n)).collect();
        assert!(missing.iter().all(|n| n % 10 != 0), "socket {index} lost a layout event");
    }
    assert_eq!(env.supervisor.stats().overflow_closes, 0);
}
