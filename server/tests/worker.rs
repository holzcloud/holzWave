//! The worker driven over a socket pair: the tracer, the dispatch rules, the order of one pane's input, the sink and its queue,
//! the replay to a new socket, the controls of the secret store, the self-check, hostile frames and the canary.
//!
//! See `support/worker_env.rs` for the rig.

#[path = "support/scratch.rs"]
mod scratch;
#[path = "support/worker_env.rs"]
mod worker_env;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use holzwave_host::commands::registry::{COMMANDS, Mode, NOT_IN_BROWSER, Outcome};
use holzwave_host::state::{Out, Sink, Target};
use holzwave_host::state::ShellSpec;
use holzwave_server::wire::{
    CmdFrame, Ctl, CtlOutcome, Evt, ExitReason, Frame, Key, Note, OpeningKey, PROTOCOL_VERSION, Res, ResBody, SlotKey,
    StoreState, read_frame,
};
use holzwave_server::worker::dispatch::{Gate, Outbound, Pushed, QueueLimits, ServerSink, spawn_writer};
use holzwave_server::worker::runtime::{HookRow, Probe, RunEnd, TestHooks};
use holzwave_server::worker::secretstore::{SecretsHandle, SlotKind};
use serde_json::{Value, json};
use worker_env::{PATIENCE, Rig, RigOptions, process_exists};

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

// ---- helpers ------------------------------------------------------------------------------------------------------------

fn rig_with(hooks: TestHooks) -> Rig {
    Rig::start_with(RigOptions { hooks, shell: None })
}

fn rig_with_shell(script: &str) -> Rig {
    Rig::start_with(RigOptions { hooks: TestHooks::default(), shell: Some(ShellSpec::new("/bin/sh", &["-c", script])) })
}

fn key(n: u8) -> Key {
    Key::from_bytes(&[n; 32])
}

fn slot(kind: SlotKind, n: u8) -> SlotKey {
    SlotKey { kind, id: [0; 16], key: key(n) }
}

fn provision(rig: &mut Rig, id: u64) -> CtlOutcome {
    rig.control(
        Ctl::Provision { ctl: id, slots: vec![slot(SlotKind::Password, 1), slot(SlotKind::Recovery, 2)] },
        id,
    )
}

/// Every file below `dir` with its size, sorted, never following a link.
fn tree(dir: &Path) -> Vec<(String, u64)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, u64)>) {
        for entry in std::fs::read_dir(dir).expect("a directory").flatten() {
            let meta = std::fs::symlink_metadata(entry.path()).expect("metadata");
            let name = entry.path().strip_prefix(base).expect("below the base").display().to_string();
            if meta.is_dir() {
                walk(&entry.path(), base, out);
            } else {
                out.push((name, meta.len()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn store_file(rig: &Rig) -> std::path::PathBuf {
    rig.home.path().join(".holzwave").join("secrets.store")
}

/// A view without its revision number, which counts every view that was made.
fn without_rev(mut view: Value) -> Value {
    view.as_object_mut().expect("a view is an object").remove("rev");
    view
}

fn res_text(res: &Res) -> String {
    match &res.body {
        ResBody::Ok(t) | ResBody::Err(t) => t.clone(),
        ResBody::Binary(b) => format!("{} bytes", b.len()),
    }
}

// ---- dispatch -----------------------------------------------------------------------------------------------------------

#[test]
fn dispatch_refuses_every_bad_envelope_and_every_bad_argument_list_with_one_answer_each() {
    let mut rig = Rig::start();
    rig.init();
    let long_name = format!("{{\"cmd\":\"{}\"}}", "a".repeat(65));
    let bad = [
        "not json".to_owned(),
        "[]".to_owned(),
        "\"get_layout\"".to_owned(),
        "{}".to_owned(),
        "{\"args\":{}}".to_owned(),
        "{\"cmd\":5}".to_owned(),
        "{\"cmd\":\"get_layout\",\"extra\":1}".to_owned(),
        "{\"cmd\":\"GET_LAYOUT\"}".to_owned(),
        "{\"cmd\":\"get layout\"}".to_owned(),
        "{\"cmd\":\"get_layout\",\"args\":null}".to_owned(),
        "{\"cmd\":\"get_layout\",\"args\":[]}".to_owned(),
        "{\"cmd\":\"get_layout\",\"args\":\"x\"}".to_owned(),
        "{\"cmd\":\"get_layout\",\"args\":{\"x\":1}}".to_owned(),
        "{\"cmd\":\"rename_pane\",\"args\":{\"pane\":1}}".to_owned(),
        long_name,
    ];
    for (i, json) in bad.iter().enumerate() {
        let id = 1000 + i as u64;
        rig.send_json(5, id, json.clone());
        let res = rig.wait_res(id);
        assert!(matches!(res.body, ResBody::Err(_)), "{json}: {res:?}");
        assert_eq!((res.socket, res.request), (5, id));
    }
    // Absent arguments mean none: a command without arguments runs.
    rig.send_json(5, 2000, "{\"cmd\":\"get_layout\"}".into());
    assert!(matches!(rig.wait_res(2000).body, ResBody::Ok(_)));
    // One answer for every command, no more.
    for i in 0..bad.len() {
        let id = 1000 + i as u64;
        let n = rig.seen().iter().filter(|f| matches!(f, Frame::Res(r) if r.request == id)).count();
        assert_eq!(n, 1, "request {id} was answered {n} times");
    }
    rig.finish();
}

#[test]
fn dispatch_answers_a_row_that_is_not_in_the_browser_with_the_sentence_of_the_table() {
    let mut rig = Rig::start();
    rig.init();
    let unavailable: Vec<&str> = COMMANDS
        .iter()
        .filter(|c| matches!(c.web, holzwave_host::commands::registry::Web::Unavailable))
        .map(|c| c.name)
        .collect();
    assert!(!unavailable.is_empty(), "the table has rows that the browser does not have");
    for name in unavailable.iter().take(5) {
        let res = rig.call(2, name, json!({}));
        let text = res_text(&res);
        assert!(matches!(res.body, ResBody::Err(_)), "{name}: {res:?}");
        assert!(text.contains(NOT_IN_BROWSER), "{name}: {text}");
    }
    rig.finish();
}

#[test]
fn dispatch_holds_no_command_name_of_its_own() {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/worker/dispatch.rs"))
        .expect("the source of the dispatch");
    for command in COMMANDS {
        let quoted = format!("\"{}\"", command.name);
        assert!(!source.contains(&quoted), "dispatch.rs names the command {quoted}");
    }
    assert!(source.contains("lookup"), "the dispatch resolves names with the table of the host");
}

#[test]
fn dispatch_runs_a_sync_row_on_the_dispatcher_and_an_async_row_on_the_pool() {
    let probe = Probe::new();
    let mut rig = rig_with(TestHooks { probe: Some(probe.clone()), ..TestHooks::default() });
    rig.init();
    rig.ok(2, "get_layout", json!({}));
    rig.ok(2, "app_info", json!({}));
    let calls = probe.calls();
    let thread_of = |name: &str| calls.iter().find(|(c, _)| c == name).map(|(_, t)| t.clone()).unwrap_or_default();
    assert_eq!(thread_of("get_layout"), "hw-dispatch", "{calls:?}");
    assert!(thread_of("app_info").starts_with("hw-pool-"), "{calls:?}");
    rig.finish();
}

#[test]
fn dispatch_keeps_the_order_of_sync_commands_of_one_pane() {
    let mut rig = Rig::start();
    rig.init();
    let first = first_pane(&rig.ok(2, "get_layout", json!({})));
    let mut last = 0;
    for i in 0..500 {
        last = rig.cmd(2, "rename_pane", json!({"pane": first, "name": format!("n{i}")}));
    }
    let res = rig.wait_res(last);
    let ResBody::Ok(text) = res.body else { panic!("{res:?}") };
    assert!(text.contains("n499") && !text.contains("n498"), "the last rename won: {text}");
    // Every rename was answered once and in order.
    let order: Vec<u64> = rig
        .seen()
        .iter()
        .filter_map(|f| match f {
            Frame::Res(r) if r.request < last + 1 && r.request > last - 500 => Some(r.request),
            _ => None,
        })
        .collect();
    assert_eq!(order.len(), 500);
    assert!(order.windows(2).all(|w| w[0] < w[1]), "answers came out of order");
    rig.finish();
}

#[test]
fn dispatch_a_slow_async_row_does_not_stop_the_sync_rows_and_a_full_pool_refuses_the_overflow() {
    let gate = Gate::new();
    gate.hold();
    let slow = {
        let gate = gate.clone();
        HookRow::new("hook_slow", Mode::Async, move || {
            gate.pass();
            Outcome::Ok("1".into())
        })
    };
    let mut rig = rig_with(TestHooks { rows: vec![slow], ..TestHooks::default() });
    rig.init();
    let total = 8 + 64 + 12;
    let ids: Vec<u64> = (0..total).map(|_| rig.cmd(2, "hook_slow", json!({}))).collect();
    // A sync command is answered while the pool is full.
    let view = rig.ok(2, "get_layout", json!({}));
    assert_eq!(view["panes"].as_array().expect("panes").len(), 1);
    let refused = rig.wait_for("a refusal for a full pool", PATIENCE, |f| {
        matches!(f, Frame::Res(r) if matches!(&r.body, ResBody::Err(t) if t.contains("too many requests in progress")))
    });
    assert!(matches!(refused, Frame::Res(_)));
    gate.release();
    for id in &ids {
        rig.wait_res(*id);
    }
    for id in &ids {
        let n = rig.seen().iter().filter(|f| matches!(f, Frame::Res(r) if r.request == *id)).count();
        assert_eq!(n, 1, "request {id}");
    }
    let refusals = rig
        .seen()
        .iter()
        .filter(|f| matches!(f, Frame::Res(r) if matches!(&r.body, ResBody::Err(t) if t.contains("too many requests"))))
        .count();
    assert!(refusals >= 12, "{refusals} refusals");
    rig.finish();
}

#[test]
fn dispatch_a_row_that_panics_is_one_error_answer_and_the_worker_goes_on() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let boom_sync = HookRow::new("hook_boom_sync", Mode::Sync, || panic!("PANIC-CANARY-SYNC"));
    let boom_async = HookRow::new("hook_boom_async", Mode::Async, || panic!("PANIC-CANARY-ASYNC"));
    let mut rig = rig_with(TestHooks { rows: vec![boom_sync, boom_async], log: Some(log.clone()), ..TestHooks::default() });
    rig.init();
    for name in ["hook_boom_sync", "hook_boom_async", "hook_boom_sync", "hook_boom_async"] {
        let res = rig.call(2, name, json!({}));
        let ResBody::Err(text) = &res.body else { panic!("{name}: {res:?}") };
        assert!(text.contains("internal error"), "{text}");
        assert!(!text.contains("CANARY"), "{text}");
    }
    // Both executors still work.
    rig.ok(2, "get_layout", json!({}));
    rig.ok(2, "app_info", json!({}));
    let lines = log.lock().expect("log").clone();
    assert!(lines.iter().any(|l| l.contains("hook_boom_sync")), "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains("CANARY")), "{lines:?}");
    rig.finish();
}

#[test]
fn dispatch_a_binary_outcome_is_an_answer_of_status_2_and_a_hook_never_replaces_a_real_row() {
    let bytes = HookRow::new("hook_bytes", Mode::Sync, || Outcome::Binary(vec![0, 255, 7]));
    let fake = HookRow::new("get_layout", Mode::Sync, || Outcome::Err("\"hooked\"".into()));
    let mut rig = rig_with(TestHooks { rows: vec![bytes, fake], ..TestHooks::default() });
    rig.init();
    let res = rig.call(2, "hook_bytes", json!({}));
    assert_eq!(res.body, ResBody::Binary(vec![0, 255, 7]));
    let view = rig.ok(2, "get_layout", json!({}));
    assert_eq!(view["panes"].as_array().expect("panes").len(), 1, "the table of the host answered");
    rig.finish();
}

// ---- the sink and its queue ---------------------------------------------------------------------------------------------

fn frame_event(size: usize) -> Frame {
    Frame::Evt(Evt { target: 0, name: "frame".into(), text: format!("\"{}\"", "x".repeat(size)) })
}

fn layout_event(size: usize) -> Frame {
    Frame::Evt(Evt { target: 0, name: "layout".into(), text: format!("\"{}\"", "y".repeat(size)) })
}

fn limits() -> QueueLimits {
    QueueLimits { soft: 100_000, hard: 400_000, low: 20_000 }
}

#[test]
fn sink_ten_thousand_frame_events_with_a_stopped_writer_stay_under_the_soft_cap() {
    let queue = Outbound::new(limits());
    let mut dropped = 0;
    for _ in 0..10_000 {
        if queue.push(frame_event(1_000)) == Pushed::Dropped {
            dropped += 1;
        }
    }
    assert!(dropped > 9_000, "{dropped} dropped");
    assert!(queue.queued_bytes() <= limits().soft + 2_000, "{} bytes queued", queue.queued_bytes());
    assert_eq!(queue.dropped_frames(), dropped);
    assert!(!queue.is_dead());
}

#[test]
fn sink_other_events_are_kept_past_the_soft_cap_and_the_hard_cap_declares_the_link_dead() {
    let queue = Outbound::new(limits());
    for _ in 0..200 {
        queue.push(frame_event(1_000));
    }
    let before = queue.queued_bytes();
    // A layout event is never dropped at the soft cap.
    for _ in 0..50 {
        assert_eq!(queue.push(layout_event(1_000)), Pushed::Queued);
    }
    assert!(queue.queued_bytes() > before);
    let mut dead = false;
    for _ in 0..1_000 {
        if queue.push(layout_event(1_000)) == Pushed::Dead {
            dead = true;
            break;
        }
    }
    assert!(dead, "the hard cap was never reached");
    assert!(queue.is_dead());
    assert!(queue.queued_bytes() < limits().hard, "the queue was cleared when it died");
    assert_eq!(queue.push(layout_event(10)), Pushed::Dead);
}

#[test]
fn sink_a_dead_link_writes_one_exiting_note_and_closes_the_socket() {
    let (ours, theirs) = UnixStream::pair().expect("a socket pair");
    let queue = Outbound::new(limits());
    let gate = Gate::new();
    gate.hold();
    let writer = spawn_writer(theirs, queue.clone(), Some(gate.clone()));
    let mut dead = false;
    for _ in 0..10_000 {
        if queue.push(layout_event(1_000)) == Pushed::Dead {
            dead = true;
            break;
        }
    }
    assert!(dead, "the hard cap was never reached");
    gate.release();
    let mut ours = ours;
    ours.set_read_timeout(Some(PATIENCE)).expect("timeout");
    let first = read_frame(&mut ours).expect("a frame").expect("not the end");
    assert_eq!(first, Frame::Note(Note::Exiting(ExitReason::Failure)));
    assert!(read_frame(&mut ours).expect("a clean end").is_none(), "the worker closed its end");
    writer.join().expect("the writer returned");
}

#[test]
fn sink_the_writer_writes_what_was_queued_in_order_and_ends_when_the_queue_closes() {
    let (mut ours, theirs) = UnixStream::pair().expect("a socket pair");
    ours.set_read_timeout(Some(PATIENCE)).expect("timeout");
    let queue = Outbound::new(limits());
    let writer = spawn_writer(theirs, queue.clone(), None);
    for i in 0..20u32 {
        queue.push(Frame::Note(Note::PaneCount(i)));
    }
    queue.close();
    writer.join().expect("the writer returned");
    for i in 0..20u32 {
        assert_eq!(read_frame(&mut ours).expect("read").expect("a frame"), Frame::Note(Note::PaneCount(i)));
    }
    assert!(read_frame(&mut ours).expect("end").is_none());
}

#[test]
fn sink_serialises_an_event_once_names_it_and_never_forwards_the_end_of_the_app() {
    let queue = Outbound::new(limits());
    let sink = ServerSink::new(queue.clone());
    sink.send(Target::All, Out::Exit);
    assert_eq!(queue.queued_bytes(), 0, "Out::Exit was queued");
    sink.send(Target::window("main"), Out::Title { pane: 7, title: "hello".into() });
    sink.send(Target::All, Out::Bell { pane: 7 });
    assert!(queue.queued_bytes() > 0);
    let (ours, theirs) = UnixStream::pair().expect("a socket pair");
    let writer = spawn_writer(theirs, queue.clone(), None);
    queue.close();
    writer.join().expect("returned");
    let mut ours = ours;
    let first = read_frame(&mut ours).expect("read").expect("a frame");
    let Frame::Evt(title) = first else { panic!("{first:?}") };
    assert_eq!((title.target, title.name.as_str()), (0, "title"));
    let payload: Value = serde_json::from_str(&title.text).expect("JSON");
    assert_eq!(payload["pane"], json!(7));
    let second = read_frame(&mut ours).expect("read").expect("a frame");
    assert!(matches!(second, Frame::Evt(e) if e.name == "bell"));
}

#[test]
fn sink_a_pane_that_lost_frames_is_asked_for_a_full_one_when_the_queue_is_low_again() {
    let probe = Probe::new();
    let gate = Gate::new();
    let hooks = TestHooks {
        writer_gate: Some(gate.clone()),
        probe: Some(probe.clone()),
        queue: Some(QueueLimits { soft: 1_200, hard: 10_000_000, low: 800 }),
        ..TestHooks::default()
    };
    let mut rig = Rig::start_with(RigOptions {
        hooks,
        shell: Some(ShellSpec::new("/bin/sh", &["-c", "while true; do echo tick; sleep 0.01; done"])),
    });
    rig.init();
    let first = first_pane(&rig.ok(2, "get_layout", json!({})));
    gate.hold();
    rig.ok(2, "start_pane", json!({"pane": first, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}));
    let deadline = std::time::Instant::now() + PATIENCE;
    while probe.repaired().is_empty() && std::time::Instant::now() < deadline {
        // The repair waits for the queue to go low, which happens when the writer is let through.
        std::thread::sleep(Duration::from_millis(300));
        gate.release();
        std::thread::sleep(Duration::from_millis(100));
        gate.hold();
    }
    gate.release();
    assert!(probe.repaired().contains(&first), "{:?}", probe.repaired());
    rig.wait_event("frame", |_, payload| payload["pane"] == json!(first));
    rig.finish();
}

// ---- the replay to a new socket -----------------------------------------------------------------------------------------

const TITLE_SHELL: &str = "printf '\\033]0;hello\\007\\033]7;file://localhost/tmp\\007'; exec cat";

fn start_titled_pane(rig: &mut Rig) -> u64 {
    rig.init();
    let first = first_pane(&rig.ok(2, "get_layout", json!({})));
    rig.ok(2, "start_pane", json!({"pane": first, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}));
    rig.wait_event("title", |_, payload| payload["pane"] == json!(first));
    rig.wait_event("cwd", |_, payload| payload["pane"] == json!(first));
    first
}

#[test]
fn replay_an_attached_socket_gets_the_titles_and_directories_for_itself_alone() {
    let mut rig = rig_with_shell(TITLE_SHELL);
    let pane = start_titled_pane(&mut rig);
    let before = rig.events("title").len();
    rig.send(Frame::Ctl(Ctl::Attach { socket: 9 }));
    let mine = rig.wait_event("title", |e, payload| e.target == 9 && payload["pane"] == json!(pane));
    assert_eq!(mine.target, 9);
    rig.wait_event("cwd", |e, _| e.target == 9);
    // A round trip through the dispatcher: everything the attach caused is behind us.
    rig.ok(2, "get_layout", json!({}));
    assert_eq!(rig.events("title").iter().filter(|e| e.target == 0).count(), before, "nothing new for everybody");
    rig.finish();
}

#[test]
fn replay_the_page_asking_for_it_gets_it_on_its_own_socket_and_other_commands_stay_on_target_zero() {
    let mut rig = rig_with_shell(TITLE_SHELL);
    start_titled_pane(&mut rig);
    rig.ok(4, "window_ready", json!({}));
    rig.wait_event("title", |e, _| e.target == 4);
    // A layout change caused by a command of socket 4 still goes to everybody.
    let first = first_pane(&rig.ok(4, "get_layout", json!({})));
    rig.ok(4, "split", json!({"column": false}));
    rig.ok(4, "close_pane", json!({"pane": first}));
    let layout = rig.wait_event("layout", |e, _| e.target == 0);
    assert_eq!(layout.target, 0);
    assert!(rig.events("layout").iter().all(|e| e.target == 0));
    rig.finish();
}

#[test]
fn replay_a_detach_changes_nothing_and_a_second_attach_replays_again() {
    let mut rig = rig_with_shell(TITLE_SHELL);
    start_titled_pane(&mut rig);
    rig.send(Frame::Ctl(Ctl::Attach { socket: 11 }));
    rig.wait_event("title", |e, _| e.target == 11);
    rig.send(Frame::Ctl(Ctl::Detach { socket: 11 }));
    rig.ok(2, "get_layout", json!({}));
    let seen = rig.events("title").iter().filter(|e| e.target == 11).count();
    rig.send(Frame::Ctl(Ctl::Attach { socket: 11 }));
    rig.ok(2, "get_layout", json!({}));
    let again = rig.events("title").iter().filter(|e| e.target == 11).count();
    assert_eq!((seen, again), (1, 2));
    rig.finish();
}

// ---- the host and its caps ----------------------------------------------------------------------------------------------

#[test]
fn host_the_pane_cap_of_init_refuses_the_next_split_with_the_sentence_and_changes_nothing() {
    let mut rig = Rig::start();
    rig.init_with(3, 0);
    rig.ok(2, "split", json!({"column": false}));
    rig.ok(2, "split", json!({"column": true}));
    let before = without_rev(rig.ok(2, "get_layout", json!({})));
    let res = rig.call(2, "split", json!({"column": false}));
    let ResBody::Err(text) = &res.body else { panic!("{res:?}") };
    assert!(text.contains("this server allows 3 panes per person; close one to open another"), "{text}");
    assert_eq!(without_rev(rig.ok(2, "get_layout", json!({}))), before);
    rig.finish();
}

#[test]
fn host_app_info_says_the_platform_is_web_and_a_scrollback_cap_leaves_the_setting_alone() {
    let mut plain = Rig::start();
    plain.init();
    let info = plain.ok(2, "app_info", json!({}));
    assert_eq!(info["os"], json!("web"), "{info}");
    let settings = plain.ok(2, "get_settings", json!({}));
    plain.finish();

    let mut capped = Rig::start();
    capped.init_with(0, 10);
    assert_eq!(capped.ok(2, "get_settings", json!({})), settings, "the cap is not written into the settings");
    capped.finish();
}

#[test]
fn host_the_number_of_panes_is_a_note_at_the_start_and_after_every_change() {
    let mut rig = Rig::start();
    rig.init();
    let first = first_pane(&rig.ok(2, "get_layout", json!({})));
    rig.ok(2, "split", json!({"column": false}));
    rig.ok(2, "close_pane", json!({"pane": first}));
    rig.ok(2, "get_layout", json!({}));
    assert_eq!(rig.pane_counts(), vec![1, 2, 1], "{:?}", rig.describe());
    rig.finish();
}

// ---- the controls and the secret store ----------------------------------------------------------------------------------

#[test]
fn store_the_state_is_absent_at_the_start_and_a_lock_without_a_store_says_absent() {
    let mut rig = Rig::start();
    rig.init();
    assert_eq!(rig.control(Ctl::Lock { ctl: 1 }, 1), CtlOutcome::Absent);
    assert_eq!(rig.store_states().first(), Some(&StoreState::Absent), "{:?}", rig.describe());
    assert_eq!(rig.control(Ctl::Unlock { ctl: 2, kind: SlotKind::Password, key: key(1) }, 2), CtlOutcome::Absent);
    rig.finish();
}

#[test]
fn store_provision_creates_the_file_with_mode_0600_and_a_second_provision_is_refused() {
    let mut rig = Rig::start();
    rig.init();
    assert_eq!(provision(&mut rig, 1), CtlOutcome::Done);
    let file = store_file(&rig);
    let meta = std::fs::metadata(&file).expect("the store file exists");
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(rig.store_states(), vec![StoreState::Absent, StoreState::Unlocked]);
    let size = meta.len();
    assert_eq!(provision(&mut rig, 2), CtlOutcome::Refused);
    assert_eq!(std::fs::metadata(&file).expect("still there").len(), size);
    // Zero slots are refused, whatever the state.
    assert_eq!(rig.control(Ctl::Provision { ctl: 3, slots: vec![] }, 3), CtlOutcome::Refused);
    rig.finish();
}

/// Saves an AI profile and returns its id (needs no key).
fn ai_profile(rig: &mut Rig) -> String {
    rig.ok(2, "ai_profile_save", json!({"input": {"name": "Cloud", "preset": "anthropic"}}))
        .as_str()
        .expect("an id")
        .to_owned()
}

#[test]
fn store_with_no_store_the_secret_commands_answer_locked_and_write_no_byte() {
    let mut rig = Rig::start();
    rig.init();
    let profile = ai_profile(&mut rig);
    let before = tree(rig.home.path());
    let fault = rig.err(2, "ai_key_set", json!({"profile": profile, "slot": "provider", "api_key": "sk-secret-canary"}));
    assert_eq!(fault["text"], json!("secrets are locked"), "{fault}");
    assert_eq!(fault["reason"], json!("locked"));
    let saved = rig.call(2, "integration_profile_save", json!({"profile": {
        "id": "prod", "tool": "kube", "name": "Production", "context": "prod-cluster", "production": true,
        "isolated": false, "env": {}, "secret_env": ["AWS_SECRET_ACCESS_KEY"]}}));
    assert!(matches!(saved.body, ResBody::Ok(_)), "{saved:?}");
    let after_profile = tree(rig.home.path());
    let fault = rig.err(2, "integration_secret_set", json!({"profile": "prod", "name": "AWS_SECRET_ACCESS_KEY", "value": "sk-secret-canary"}));
    assert_eq!((fault["code"].as_str(), fault["reason"].as_str()), (Some("profile"), Some("locked")), "{fault}");
    assert_eq!(fault["text"], json!("secrets are locked"));
    let refused = rig.call(2, "connection_save", json!({
        "profile": {"id": "box", "name": "Box", "kind": "ssh", "host": "example.invalid", "user": "me", "auth": "password"},
        "secret": {"kind": "set", "value": "sk-secret-canary"}}));
    assert!(res_text(&refused).contains("secrets are locked"), "{refused:?}");
    assert!(!before.is_empty());
    assert_eq!(tree(rig.home.path()).iter().filter(|(n, _)| n.contains("secrets")).count(), 0);
    let _ = after_profile;
    rig.finish();
}

#[test]
fn store_lock_then_a_secret_command_is_locked_and_unlock_with_the_password_key_makes_it_work() {
    let mut rig = Rig::start();
    rig.init();
    let profile = ai_profile(&mut rig);
    assert_eq!(provision(&mut rig, 1), CtlOutcome::Done);
    rig.ok(2, "ai_key_set", json!({"profile": profile, "slot": "provider", "api_key": "sk-one"}));
    assert_eq!(rig.control(Ctl::Lock { ctl: 2 }, 2), CtlOutcome::Done);
    let fault = rig.err(2, "ai_key_set", json!({"profile": profile, "slot": "provider", "api_key": "sk-two"}));
    assert_eq!(fault["text"], json!("secrets are locked"));
    assert_eq!(rig.control(Ctl::Unlock { ctl: 3, kind: SlotKind::Password, key: key(1) }, 3), CtlOutcome::Done);
    rig.ok(2, "ai_key_set", json!({"profile": profile, "slot": "provider", "api_key": "sk-three"}));
    assert_eq!(
        rig.store_states(),
        vec![StoreState::Absent, StoreState::Unlocked, StoreState::Locked, StoreState::Unlocked]
    );
    rig.finish();
}

#[test]
fn store_a_wrong_key_answers_locked_and_changes_nothing() {
    let mut rig = Rig::start();
    rig.init();
    assert_eq!(provision(&mut rig, 1), CtlOutcome::Done);
    assert_eq!(rig.control(Ctl::Lock { ctl: 2 }, 2), CtlOutcome::Done);
    let states = rig.store_states();
    let file = std::fs::read(store_file(&rig)).expect("the file");
    assert_eq!(rig.control(Ctl::Unlock { ctl: 3, kind: SlotKind::Password, key: key(99) }, 3), CtlOutcome::Locked);
    assert_eq!(rig.control(Ctl::Unlock { ctl: 4, kind: SlotKind::Prf, key: key(1) }, 4), CtlOutcome::Locked);
    assert_eq!(rig.store_states(), states, "no new state was announced");
    assert_eq!(std::fs::read(store_file(&rig)).expect("the file"), file);
    // The recovery key opens it.
    assert_eq!(rig.control(Ctl::Unlock { ctl: 5, kind: SlotKind::Recovery, key: key(2) }, 5), CtlOutcome::Done);
    rig.finish();
}

#[test]
fn store_set_slot_adds_a_passphrase_slot_that_opens_the_store_in_a_fresh_handle() {
    let mut rig = Rig::start();
    rig.init();
    assert_eq!(provision(&mut rig, 1), CtlOutcome::Done);
    let outcome = rig.control(
        Ctl::SetSlot { ctl: 2, slot: SlotKey { kind: SlotKind::Passphrase, id: [0; 16], key: key(7) }, opening: None },
        2,
    );
    assert_eq!(outcome, CtlOutcome::Done);
    let handle = SecretsHandle::open(rig.home.path());
    assert_eq!(handle.unlock(SlotKind::Passphrase, &key(7).to_kek()), CtlOutcome::Done);
    assert_eq!(handle.unlock(SlotKind::Passphrase, &key(8).to_kek()), CtlOutcome::Locked);
    rig.finish();
}

#[test]
fn store_set_slot_on_a_locked_store_opens_it_for_the_change_and_locks_it_again() {
    let mut rig = Rig::start();
    rig.init();
    assert_eq!(provision(&mut rig, 1), CtlOutcome::Done);
    assert_eq!(rig.control(Ctl::Lock { ctl: 2 }, 2), CtlOutcome::Done);
    // Without the second key a locked store is not changed.
    let alone = Ctl::SetSlot { ctl: 3, slot: slot(SlotKind::Password, 5), opening: None };
    assert_eq!(rig.control(alone, 3), CtlOutcome::Locked);
    let with = Ctl::SetSlot {
        ctl: 4,
        slot: slot(SlotKind::Password, 5),
        opening: Some(OpeningKey { kind: SlotKind::Recovery, key: key(2) }),
    };
    assert_eq!(rig.control(with, 4), CtlOutcome::Done);
    // Still locked; the new password key opens it, the old one does not.
    assert_eq!(rig.store_states().last(), Some(&StoreState::Locked));
    assert_eq!(rig.control(Ctl::Unlock { ctl: 5, kind: SlotKind::Password, key: key(1) }, 5), CtlOutcome::Locked);
    assert_eq!(rig.control(Ctl::Unlock { ctl: 6, kind: SlotKind::Password, key: key(5) }, 6), CtlOutcome::Done);
    rig.finish();
}

#[test]
fn store_set_slot_with_no_store_and_no_opening_key_creates_it_and_an_opening_key_with_no_store_is_absent() {
    let mut rig = Rig::start();
    rig.init();
    let with = Ctl::SetSlot {
        ctl: 1,
        slot: slot(SlotKind::Password, 5),
        opening: Some(OpeningKey { kind: SlotKind::Recovery, key: key(2) }),
    };
    assert_eq!(rig.control(with, 1), CtlOutcome::Absent);
    assert!(!store_file(&rig).exists());
    let create = Ctl::SetSlot { ctl: 2, slot: slot(SlotKind::Password, 5), opening: None };
    assert_eq!(rig.control(create, 2), CtlOutcome::Done);
    assert!(store_file(&rig).exists());
    assert_eq!(rig.control(Ctl::Lock { ctl: 3 }, 3), CtlOutcome::Done);
    assert_eq!(rig.control(Ctl::Unlock { ctl: 4, kind: SlotKind::Password, key: key(5) }, 4), CtlOutcome::Done);
    rig.finish();
}

#[test]
fn store_remove_slot_removes_one_refuses_the_last_and_answers_absent_without_a_store() {
    let mut rig = Rig::start();
    rig.init();
    assert_eq!(rig.control(Ctl::RemoveSlot { ctl: 1, kind: SlotKind::Password, id: [0; 16] }, 1), CtlOutcome::Absent);
    assert_eq!(provision(&mut rig, 2), CtlOutcome::Done);
    assert_eq!(rig.control(Ctl::RemoveSlot { ctl: 3, kind: SlotKind::Password, id: [9; 16] }, 3), CtlOutcome::Refused);
    assert_eq!(rig.control(Ctl::RemoveSlot { ctl: 4, kind: SlotKind::Recovery, id: [0; 16] }, 4), CtlOutcome::Done);
    assert_eq!(rig.control(Ctl::RemoveSlot { ctl: 5, kind: SlotKind::Password, id: [0; 16] }, 5), CtlOutcome::Refused);
    assert_eq!(rig.control(Ctl::Lock { ctl: 6 }, 6), CtlOutcome::Done);
    assert_eq!(rig.control(Ctl::Unlock { ctl: 7, kind: SlotKind::Recovery, key: key(2) }, 7), CtlOutcome::Locked);
    assert_eq!(rig.control(Ctl::Unlock { ctl: 8, kind: SlotKind::Password, key: key(1) }, 8), CtlOutcome::Done);
    rig.finish();
}

#[test]
fn store_reset_removes_the_file_and_its_siblings_and_leaves_the_home_as_it_was() {
    let mut rig = Rig::start();
    rig.init();
    let before = tree(rig.home.path());
    assert_eq!(provision(&mut rig, 1), CtlOutcome::Done);
    // Siblings the store makes: a lock file and a leftover temporary file.
    let dir = rig.home.path().join(".holzwave");
    std::fs::write(dir.join("secrets.store.tmp-leftover"), b"x").expect("a leftover");
    assert_ne!(tree(rig.home.path()), before);
    assert_eq!(rig.control(Ctl::ResetSecrets { ctl: 2 }, 2), CtlOutcome::Done);
    assert_eq!(rig.store_states().last(), Some(&StoreState::Absent));
    let after: Vec<String> = tree(rig.home.path()).into_iter().map(|(n, _)| n).collect();
    assert!(after.iter().all(|n| !n.contains("secrets")), "{after:?}");
    let expected: Vec<String> = before.into_iter().map(|(n, _)| n).collect();
    let without_runtime: Vec<String> = after.into_iter().filter(|n| expected.contains(n)).collect();
    assert_eq!(without_runtime, expected);
    // The key is gone: a secret command is locked, and a new Provision works.
    let profile = ai_profile(&mut rig);
    let fault = rig.err(2, "ai_key_set", json!({"profile": profile, "slot": "provider", "api_key": "sk"}));
    assert_eq!(fault["text"], json!("secrets are locked"));
    assert_eq!(provision(&mut rig, 3), CtlOutcome::Done);
    rig.finish();
}

// ---- the end ------------------------------------------------------------------------------------------------------------

#[test]
fn end_a_shutdown_control_says_clean_ends_the_run_and_the_shell() {
    let mut rig = Rig::start();
    rig.init();
    let first = first_pane(&rig.ok(2, "get_layout", json!({})));
    rig.ok(2, "start_pane", json!({"pane": first, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}));
    rig.wait_event("frame", |_, p| p["pane"] == json!(first));
    let deadline = std::time::Instant::now() + PATIENCE;
    while rig.shell_pid().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid = rig.shell_pid().expect("the shell's process id");
    rig.send(Frame::Ctl(Ctl::Shutdown));
    rig.wait_for("Exiting clean", PATIENCE, |f| matches!(f, Frame::Note(Note::Exiting(ExitReason::Clean))));
    assert_eq!(rig.finish(), RunEnd::Shutdown);
    assert!(!process_exists(pid));
}

#[test]
fn end_a_second_init_a_frame_that_belongs_to_the_worker_and_a_bad_frame_end_it_with_a_failure_note() {
    let cases: Vec<Frame> = vec![
        Frame::Ctl(Ctl::Init { version: PROTOCOL_VERSION, max_panes: 0, max_scrollback_lines: 0 }),
        Frame::Res(Res { socket: 1, request: 1, body: ResBody::Ok("1".into()) }),
        Frame::Evt(Evt { target: 1, name: "frame".into(), text: "{}".into() }),
        Frame::Note(Note::Ready),
    ];
    for frame in cases {
        let mut rig = Rig::start();
        rig.init();
        rig.send(frame.clone());
        rig.wait_for("Exiting failure", PATIENCE, |f| matches!(f, Frame::Note(Note::Exiting(ExitReason::Failure))));
        assert_eq!(rig.finish(), RunEnd::Failed, "{frame:?}");
    }
    let mut rig = Rig::start();
    rig.init();
    rig.send_raw(&[0, 0, 0, 2, 9, 9]);
    rig.wait_for("Exiting failure", PATIENCE, |f| matches!(f, Frame::Note(Note::Exiting(ExitReason::Failure))));
    assert_eq!(rig.finish(), RunEnd::Failed);
}

#[test]
fn end_the_rig_helper_frames_decode_the_way_the_worker_writes_them() {
    // A command frame built by hand is the same bytes the rig writes.
    let frame = Frame::Cmd(CmdFrame { socket: 3, request: 4, json: "{\"cmd\":\"get_layout\"}".into() });
    let bytes = frame.to_payload().expect("encodes");
    assert_eq!(Frame::from_payload(&bytes).expect("decodes"), frame);
}
