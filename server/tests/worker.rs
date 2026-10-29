//! The worker driven over a socket pair: the tracer, the dispatch rules, the order of one pane's input, the sink and its queue,
//! the replay to a new socket, the controls of the secret store, the self-check, hostile frames and the canary.
//!
//! See `support/worker_env.rs` for the rig.

#[path = "support/scratch.rs"]
mod scratch;
#[path = "support/worker_env.rs"]
mod worker_env;

use std::time::Duration;

use holzwave_server::wire::{Ctl, Frame, Note, ResBody, PROTOCOL_VERSION};
use holzwave_server::worker::runtime::RunEnd;
use serde_json::{Value, json};
use worker_env::{PATIENCE, Rig, process_exists};

/// The id of the only pane of a fresh layout.
fn first_pane(view: &Value) -> u64 {
    view["focus"].as_u64().expect("the view names the focused pane")
}

#[test]
fn tracer_init_answers_ready_and_nothing_runs_before_it() {
    let mut rig = Rig::start();
    // A command before `Init` is answered with an error and runs nothing.
    let early = rig.call(7, "get_layout", json!({}));
    assert!(matches!(early.body, ResBody::Err(_)), "{early:?}");
    rig.init();
    assert!(rig.seen().iter().any(|f| matches!(f, Frame::Note(Note::Ready))));
    assert_eq!(rig.finish(), RunEnd::LinkClosed);
}

#[test]
fn tracer_the_layout_read_answers_a_view_with_one_workspace() {
    let mut rig = Rig::start();
    rig.init();
    let request = rig.cmd(7, "get_layout", json!({}));
    let res = rig.wait_res(request);
    assert_eq!((res.socket, res.request), (7, request));
    let ResBody::Ok(text) = res.body else { panic!("get_layout did not succeed: {res:?}") };
    let view: Value = serde_json::from_str(&text).expect("the answer is the JSON text of the view");
    assert_eq!(view["workspaces"].as_array().expect("workspaces").len(), 1);
    assert_eq!(view["panes"].as_array().expect("panes").len(), 1);
    rig.finish();
}

#[test]
fn tracer_a_command_that_is_not_in_the_table_is_refused_and_runs_nothing() {
    let mut rig = Rig::start();
    rig.init();
    for name in ["definitely_not_a_command", "GET_LAYOUT", "get-layout", ""] {
        let res = rig.call(3, name, json!({}));
        assert!(matches!(res.body, ResBody::Err(_)), "{name:?}: {res:?}");
    }
    // The core is untouched: still one pane.
    let view = rig.ok(3, "get_layout", json!({}));
    assert_eq!(view["panes"].as_array().expect("panes").len(), 1);
    rig.finish();
}

#[test]
fn tracer_a_pane_starts_draws_and_closes_through_the_real_core() {
    let mut rig = Rig::start();
    rig.init();
    let first = first_pane(&rig.ok(7, "get_layout", json!({})));
    // A second pane, so that closing the first leaves a layout (the last pane of the last window ends the core).
    let view = rig.ok(7, "split", json!({"column": false}));
    assert_eq!(view["panes"].as_array().expect("panes").len(), 2);

    let started = rig.call(7, "start_pane", json!({"pane": first, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}));
    assert!(matches!(started.body, ResBody::Ok(_)), "{started:?}");
    // A frame event for that pane arrives, for every attached socket.
    let frame = rig.wait_event("frame", |_, payload| payload["pane"] == json!(first));
    assert_eq!(frame.target, 0);
    assert_eq!(frame.name, "frame");

    let seen_before = rig.seen().len();
    let closed = rig.call(7, "close_pane", json!({"pane": first}));
    assert!(matches!(closed.body, ResBody::Ok(_)), "{closed:?}");
    let layout = rig.wait_event("layout", |_, payload| payload["panes"].as_array().is_some_and(|p| p.len() == 1));
    assert_eq!(layout.target, 0);
    assert!(rig.seen().len() > seen_before);
    // Printed for the record of the plan: the kinds, the ids and the sizes, no payload text.
    println!("tracer frames: {:#?}", rig.describe());
    rig.finish();
}

#[test]
fn tracer_the_end_of_the_socket_ends_the_run_and_the_shell() {
    let mut rig = Rig::start();
    rig.init();
    let first = first_pane(&rig.ok(7, "get_layout", json!({})));
    rig.ok(7, "start_pane", json!({"pane": first, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}));
    rig.wait_event("frame", |_, payload| payload["pane"] == json!(first));
    let deadline = std::time::Instant::now() + PATIENCE;
    while rig.shell_pid().is_none() {
        assert!(std::time::Instant::now() < deadline, "the shell never wrote its process id");
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid = rig.shell_pid().expect("the shell's process id");
    assert!(process_exists(pid), "the shell runs while the link is open");

    let end = rig.finish();
    assert_eq!(end, RunEnd::LinkClosed);
    // The core's shutdown closed and reaped the shell before `run` returned.
    assert!(!process_exists(pid), "the shell process {pid} is gone after the end of the socket");
}

#[test]
fn tracer_an_init_with_another_protocol_version_ends_the_worker() {
    let mut rig = Rig::start();
    rig.send(Frame::Ctl(Ctl::Init { version: PROTOCOL_VERSION + 1, max_panes: 0, max_scrollback_lines: 0 }));
    let end = rig.finish();
    assert_eq!(end, RunEnd::Failed);
}
