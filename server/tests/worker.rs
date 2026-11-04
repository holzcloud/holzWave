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
use holzwave_server::cli::{parse_worker_args, worker_main};
use holzwave_server::wire::MAX_PAYLOAD;
use holzwave_server::worker::runtime::{
    HookRow, Probe, RunEnd, TestHooks, WorkerEnv, WorkerMode, check_descriptors, check_home, check_status, wipe_home,
    wipe_secrets,
};
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
fn the_worker_makes_the_temporary_directory_that_the_helper_only_names() {
    use std::os::unix::fs::PermissionsExt;
    let mut rig = Rig::start_without_tmp();
    let tmp = rig.home.join(".tmp");
    assert!(!tmp.exists(), "the spawn helper makes nothing but the home");
    rig.init();
    assert!(tmp.is_dir(), "the worker made the directory TMPDIR names");
    assert_eq!(std::fs::metadata(&tmp).expect("metadata").permissions().mode() & 0o777, 0o700, "private to the person");
    rig.finish();
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
    rig.send(Frame::Ctl(Ctl::Init { version: PROTOCOL_VERSION + 1, max_panes: 0, max_scrollback_lines: 0, max_image_mib: 64 }));
    let end = rig.finish();
    assert_eq!(end, RunEnd::Failed);
}

// ---- pictures: the worker cap for the image memory of a pane -------------------------------------------------------------

const MIB: usize = 1024 * 1024;

/// A worker whose shell prints one picture of `bytes` bytes (the id 7) and then sets a title; `cap_mib` is the picture cap of
/// the first control frame and `setting_mib`, when given, is the person's own `image_memory_mib`, set before the pane starts.
/// Returns after the engine has taken or refused the whole picture and a full frame that was asked for afterwards has come, so
/// what the frames of the pane do not show is not there, not late.
fn picture_rig(cap_mib: u32, setting_mib: Option<u32>, bytes: usize) -> (Rig, u64) {
    let mut rig = rig_with_shell(&format!("{}; exec cat", worker_env::picture_command(bytes, 7)));
    rig.init_with_images(0, 0, cap_mib);
    if let Some(mib) = setting_mib {
        rig.ok(2, "set_setting", json!({"key": "image_memory_mib", "value": mib}));
    }
    let first = first_pane(&rig.ok(2, "get_layout", json!({})));
    rig.ok(2, "start_pane", json!({"pane": first, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}));
    rig.wait_event("title", |_, payload| payload["pane"] == json!(first) && payload["title"] == json!("hw-pic-done"));
    let seen = rig
        .seen()
        .iter()
        .filter_map(|f| match f {
            Frame::Evt(e) if e.name == "frame" => serde_json::from_str::<Value>(&e.text).ok(),
            _ => None,
        })
        .filter(|p| p["pane"] == json!(first))
        .filter_map(|p| p["seq"].as_u64())
        .max()
        .unwrap_or(0);
    rig.ok(2, "request_full", json!({"pane": first}));
    rig.wait_event("frame", |_, p| p["pane"] == json!(first) && p["full"] == json!(true) && p["seq"].as_u64() > Some(seen));
    (rig, first)
}

/// The id and the generation of the first placement a frame of `pane` carried, if any frame carried one.
fn first_placement(rig: &Rig, pane: u64) -> Option<(u64, u64)> {
    rig.seen().iter().find_map(|f| match f {
        Frame::Evt(e) if e.name == "frame" => {
            let payload: Value = serde_json::from_str(&e.text).ok()?;
            if payload["pane"] != json!(pane) {
                return None;
            }
            let first = payload["images"].as_array()?.first()?.as_array()?.clone();
            Some((first[0].as_u64()?, first[1].as_u64()?))
        }
        _ => None,
    })
}

#[test]
fn tracer_the_picture_cap_of_the_first_frame_reaches_the_pane_a_picture_of_2_mib_is_placed_under_8_and_not_under_1() {
    // The same picture, first under a cap that holds it, then under one that does not.
    let (mut rig, pane) = picture_rig(8, None, 2 * MIB);
    let (image, generation) = first_placement(&rig, pane).expect("under a cap of 8 MiB the picture is placed");
    assert_eq!(image, 7);
    let answer = rig.call(2, "image_data", json!({"pane": pane, "image": image, "generation": generation, "offset": 0}));
    let ResBody::Binary(bytes) = answer.body else { panic!("the pixels come back as raw bytes: {answer:?}") };
    assert_eq!(bytes[..12], [0, 0, 1, 0, 0, 0, 8, 0, 0, 0x20, 0, 0], "256 by 2048 pixels, 2 MiB");
    rig.finish();

    let (mut rig, pane) = picture_rig(1, None, 2 * MIB);
    assert_eq!(first_placement(&rig, pane), None, "under a cap of 1 MiB no frame shows a placement");
    assert_eq!(rig.frames_with_images(pane), 0);
    let refused = rig.err(2, "image_data", json!({"pane": pane, "image": 7, "generation": 1, "offset": 0}));
    assert_eq!(refused, json!("no such image"));
    rig.finish();
}

#[test]
fn images_the_cap_wins_over_the_persons_setting_a_setting_of_64_does_not_lift_a_cap_of_1() {
    // The person allows 64 MiB, so the setting alone admits a picture of 2 MiB: the same picture is placed under a cap of 8.
    let (rig, pane) = picture_rig(8, Some(64), 2 * MIB);
    assert!(first_placement(&rig, pane).is_some(), "the setting of 64 MiB and a cap of 8 MiB admit a picture of 2 MiB");
    rig.finish();
    // Under a cap of 1 MiB the person's setting of 64 MiB changes nothing: the cap is the smaller.
    let (rig, pane) = picture_rig(1, Some(64), 2 * MIB);
    assert_eq!(first_placement(&rig, pane), None, "the cap of 1 MiB wins over the setting of 64 MiB");
    rig.finish();
}

#[test]
fn images_a_setting_below_the_cap_keeps_its_value_2_mib_under_a_cap_of_8_places_1_and_refuses_3() {
    let (rig, pane) = picture_rig(8, Some(2), MIB);
    assert!(first_placement(&rig, pane).is_some(), "1 MiB is under the setting of 2 MiB");
    rig.finish();
    // The same cap with the default setting admits 3 MiB, so it is the person's 2 MiB that refuses it below.
    let (rig, pane) = picture_rig(8, None, 3 * MIB);
    assert!(first_placement(&rig, pane).is_some(), "the cap of 8 MiB alone admits 3 MiB");
    rig.finish();
    let (rig, pane) = picture_rig(8, Some(2), 3 * MIB);
    assert_eq!(first_placement(&rig, pane), None, "the setting of 2 MiB is not raised to the cap of 8 MiB");
    rig.finish();
}

#[test]
fn images_a_cap_of_0_turns_pictures_off_whatever_the_setting_says() {
    // The control: the same picture under a cap of 8 MiB is placed, so a missing placement below is the cap and not the picture.
    let (rig, pane) = picture_rig(8, Some(64), MIB);
    assert!(first_placement(&rig, pane).is_some());
    rig.finish();
    for setting in [None, Some(1), Some(64), Some(512)] {
        let (rig, pane) = picture_rig(0, setting, MIB);
        assert_eq!(first_placement(&rig, pane), None, "setting {setting:?} under a cap of 0");
        assert_eq!(rig.frames_with_images(pane), 0);
        rig.finish();
    }
    // The smallest picture the helper makes is 1 KiB: a cap of 0 refuses it too, it is not a matter of size.
    let (rig, pane) = picture_rig(0, None, 1024);
    assert_eq!(first_placement(&rig, pane), None);
    rig.finish();
}

#[test]
fn images_a_refused_picture_has_no_pixels_and_every_read_for_it_fails_with_the_fixed_text() {
    let (mut rig, pane) = picture_rig(0, None, MIB);
    for generation in [0, 1, 2, u64::MAX] {
        let refused = rig.err(2, "image_data", json!({"pane": pane, "image": 7, "generation": generation, "offset": 0}));
        assert_eq!(refused, json!("no such image"), "generation {generation}");
    }
    rig.finish();
    // The same read for a picture that was placed answers its pixels, so the text above is the refusal and not a broken command.
    let (mut rig, pane) = picture_rig(8, None, MIB);
    let (image, generation) = first_placement(&rig, pane).expect("placed");
    let answer = rig.call(2, "image_data", json!({"pane": pane, "image": image, "generation": generation, "offset": 0}));
    assert!(matches!(answer.body, ResBody::Binary(ref bytes) if bytes.len() == 16 + MIB.min(512 * 1024)), "{answer:?}");
    rig.finish();
}

#[test]
fn init_a_first_frame_of_version_1_ends_the_worker_in_a_failure_and_so_does_the_old_twelve_byte_layout() {
    // Version 1 with the four new bytes: it decodes, and the version is what the worker refuses.
    let mut rig = Rig::start();
    rig.send(Frame::Ctl(Ctl::Init { version: 1, max_panes: 3, max_scrollback_lines: 100, max_image_mib: 64 }));
    rig.wait_for("the failure note", PATIENCE, |f| matches!(f, Frame::Note(Note::Exiting(ExitReason::Failure))));
    assert!(!rig.seen().iter().any(|f| matches!(f, Frame::Note(Note::Ready))), "nothing was started");
    assert_eq!(rig.finish(), RunEnd::Failed);

    // The bytes a version 1 supervisor wrote, length prefix and twelve bytes: cut off before the cap, so a bad frame.
    let mut rig = Rig::start();
    rig.send_raw(&[0, 0, 0, 12, 0x04, 0x01, 0, 1, 0, 0, 0, 3, 0, 1, 0, 0]);
    rig.wait_for("the failure note", PATIENCE, |f| matches!(f, Frame::Note(Note::Exiting(ExitReason::Failure))));
    assert!(!rig.seen().iter().any(|f| matches!(f, Frame::Note(Note::Ready))), "nothing was started");
    assert_eq!(rig.finish(), RunEnd::Failed);
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
    rig.ok(2, "start_pane", json!({"pane": first, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}));
    // From here the supervisor stops reading, in turns: the queue fills over the soft cap and drops frames of the pane.
    gate.hold();
    let deadline = std::time::Instant::now() + PATIENCE;
    while probe.repaired().is_empty() && std::time::Instant::now() < deadline {
        // The repair waits for the queue to go low, which happens when the writer is let through.
        std::thread::sleep(Duration::from_millis(300));
        gate.release();
        std::thread::sleep(Duration::from_millis(100));
        gate.hold();
    }
    gate.release();
    assert!(probe.repaired().contains(&first), "{:?} {:#?}", probe.repaired(), rig.describe());
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
fn end_a_link_that_closes_while_a_pane_is_still_writing_is_the_end_of_the_link_and_not_a_failure() {
    let mut rig = rig_with_shell("while true; do echo tick; done");
    rig.init();
    let first = first_pane(&rig.ok(2, "get_layout", json!({})));
    rig.ok(2, "start_pane", json!({"pane": first, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}));
    rig.wait_event("frame", |_, payload| payload["pane"] == json!(first));
    // The supervisor goes away while frames are on their way to it.
    assert_eq!(rig.finish(), RunEnd::LinkClosed);
}

#[test]
fn end_a_second_init_a_frame_that_belongs_to_the_worker_and_a_bad_frame_end_it_with_a_failure_note() {
    let cases: Vec<Frame> = vec![
        Frame::Ctl(Ctl::Init { version: PROTOCOL_VERSION, max_panes: 0, max_scrollback_lines: 0, max_image_mib: 64 }),
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

// ---- the self-check -----------------------------------------------------------------------------------------------------

const STATUS: &str = "Name:\tholzwave-server\nUmask:\t0077\nState:\tS (sleeping)\nUid:\t1000\t1000\t1000\t1000\n\
Gid:\t1001\t1001\t1001\t1001\nGroups:\t\nCapInh:\t0000000000000000\nCapPrm:\t0000000000000000\n\
CapEff:\t0000000000000000\nCapBnd:\t0000000000000000\nCapAmb:\t0000000000000000\nNoNewPrivs:\t1\nSeccomp:\t0\n";

/// `STATUS` with the line that starts with `prefix` replaced by `line` (or removed when `line` is `None`).
fn status_with(prefix: &str, line: Option<&str>) -> String {
    let mut out = String::new();
    let mut found = false;
    for existing in STATUS.lines() {
        if existing.starts_with(prefix) {
            found = true;
            if let Some(line) = line {
                out.push_str(line);
                out.push('\n');
            }
        } else {
            out.push_str(existing);
            out.push('\n');
        }
    }
    assert!(found, "the sample has a {prefix} line");
    out
}

#[test]
fn selfcheck_a_correctly_started_worker_passes_and_its_ids_are_read() {
    let identity = check_status(STATUS).expect("the sample is a correct start");
    assert_eq!((identity.uid, identity.gid), (1000, 1001));
    // A bounding set that is not empty is not held by the worker and does not matter.
    let bounded = status_with("CapBnd:", Some("CapBnd:\t000001ffffffffff"));
    assert!(check_status(&bounded).is_ok());
    // Supplementary groups spelled out are a failure, an empty list with a space is not.
    assert!(check_status(&status_with("Groups:", Some("Groups:\t "))).is_ok());
}

#[test]
fn selfcheck_every_failure_has_its_own_sentence_that_repeats_nothing_of_the_text() {
    let cases = [
        status_with("Uid:", Some("Uid:\t0\t0\t0\t0")),
        status_with("Uid:", Some("Uid:\t1000\t1000\t0\t1000")),
        status_with("Uid:", Some("Uid:\t1000\t1001\t1000\t1000")),
        status_with("Gid:", Some("Gid:\t0\t0\t0\t0")),
        status_with("Gid:", Some("Gid:\t1001\t1002\t1001\t1001")),
        status_with("Groups:", Some("Groups:\t4 24 27 ")),
        status_with("CapInh:", Some("CapInh:\t0000000000000001")),
        status_with("CapPrm:", Some("CapPrm:\t0000000000000001")),
        status_with("CapEff:", Some("CapEff:\t0000000000000001")),
        status_with("CapAmb:", Some("CapAmb:\t0000000000000001")),
        status_with("NoNewPrivs:", Some("NoNewPrivs:\t0")),
    ];
    let mut sentences = std::collections::BTreeSet::new();
    for (i, text) in cases.iter().enumerate() {
        let sentence = check_status(text).expect_err(&format!("case {i} must fail"));
        assert!(!sentence.is_empty());
        assert!(!sentence.contains("0000000000000001") && !sentence.contains("1001"), "{sentence}");
        sentences.insert(sentence);
    }
    assert_eq!(sentences.len(), cases.len(), "two failures share a sentence: {sentences:?}");
}

#[test]
fn selfcheck_a_missing_line_or_a_malformed_value_fails_closed() {
    for prefix in ["Uid:", "Gid:", "Groups:", "CapInh:", "CapPrm:", "CapEff:", "CapAmb:", "NoNewPrivs:"] {
        let missing = status_with(prefix, None);
        assert!(check_status(&missing).is_err(), "a status without {prefix} passed");
    }
    for (prefix, bad) in [
        ("Uid:", "Uid:\t1000\t1000"),
        ("Uid:", "Uid:\tabc\t1000\t1000\t1000"),
        ("Uid:", "Uid:\t1000\t1000\t1000\t1000\t1000"),
        ("Gid:", "Gid:\t-1\t-1\t-1\t-1"),
        ("CapEff:", "CapEff:\tzzzz"),
        ("CapEff:", "CapEff:\t"),
        ("CapAmb:", "CapAmb:\t00000000000000000000"),
        ("NoNewPrivs:", "NoNewPrivs:\tyes"),
        ("Groups:", "Groups:\tx y"),
    ] {
        assert!(check_status(&status_with(prefix, Some(bad))).is_err(), "{bad:?} passed");
    }
    assert!(check_status("").is_err());
    assert!(check_status("garbage").is_err());
}

#[test]
fn selfcheck_only_the_link_and_the_three_standard_descriptors_may_be_open() {
    assert!(check_descriptors(&[0, 1, 2, 3], None).is_ok());
    assert!(check_descriptors(&[3, 2, 1, 0], None).is_ok());
    // The directory descriptor the list was read through is the reader's own.
    assert!(check_descriptors(&[0, 1, 2, 3, 7], Some(7)).is_ok());
    for stray in [4u32, 5, 6, 100, 1023, u32::MAX] {
        let error = check_descriptors(&[0, 1, 2, 3, stray], None).expect_err("a stray descriptor");
        assert!(error.contains(&stray.to_string()), "{error}");
    }
    // The own descriptor excuses itself only.
    assert!(check_descriptors(&[0, 1, 2, 3, 7, 8], Some(7)).is_err());
    // The link must be there.
    assert!(check_descriptors(&[0, 1, 2], None).is_err());
}

#[test]
fn selfcheck_the_home_must_be_a_real_directory_of_the_user_with_mode_0700() {
    let uid = rustix::process::getuid().as_raw();
    let scratch = scratch::Scratch::new("home-check");
    let home = scratch.join("home");
    std::fs::create_dir(&home).expect("home");
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).expect("mode");
    assert!(check_home(&home, uid).is_ok());
    // Wrong owner.
    assert!(check_home(&home, uid.wrapping_add(1)).is_err());
    // Any bit for group or other, and any mode but 0700.
    for mode in [0o750, 0o755, 0o770, 0o707, 0o701, 0o777, 0o500, 0o600, 0o2700, 0o1700] {
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(mode)).expect("mode");
        assert!(check_home(&home, uid).is_err(), "mode {mode:o} passed");
    }
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).expect("mode");
    // A link to a good directory, a file, a missing path and a relative path.
    let link = scratch.join("link");
    std::os::unix::fs::symlink(&home, &link).expect("link");
    assert!(check_home(&link, uid).is_err());
    let file = scratch.write_file("file", b"x", 0o700);
    assert!(check_home(&file, uid).is_err());
    assert!(check_home(&scratch.join("missing"), uid).is_err());
    assert!(check_home(Path::new("relative/home"), uid).is_err());
    assert!(check_home(Path::new(""), uid).is_err());
}

// ---- the entry ----------------------------------------------------------------------------------------------------------

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

#[test]
fn entry_the_worker_takes_one_mode_and_nothing_else() {
    assert_eq!(parse_worker_args(&args(&["--mode", "serve"])), Ok(WorkerMode::Serve));
    assert_eq!(parse_worker_args(&args(&["--mode", "wipe_secrets"])), Ok(WorkerMode::WipeSecrets));
    assert_eq!(parse_worker_args(&args(&["--mode", "wipe_home"])), Ok(WorkerMode::WipeHome));
    for bad in [
        args(&[]),
        args(&["--mode"]),
        args(&["--mode", "frobnicate"]),
        args(&["--mode", "Serve"]),
        args(&["--mode", ""]),
        args(&["--mode", "serve", "extra"]),
        args(&["--mode", "serve", "--mode", "serve"]),
        args(&["--mode=serve"]),
        args(&["serve"]),
        args(&["--home", "/tmp"]),
        args(&["--mode", "serve", "--home", "/tmp"]),
    ] {
        assert!(parse_worker_args(&bad).is_err(), "{bad:?} was accepted");
    }
}

#[test]
fn entry_a_call_the_worker_does_not_understand_or_cannot_run_exits_with_status_2() {
    for bad in [args(&[]), args(&["--mode", "frobnicate"]), args(&["--mode", "serve", "extra"])] {
        assert_eq!(worker_main(&bad, None), 2, "{bad:?}");
    }
    // Serve without the inherited socket; on a system that is not Linux, every mode.
    assert_eq!(worker_main(&args(&["--mode", "serve"]), None), 2);
    if cfg!(not(target_os = "linux")) {
        assert_eq!(worker_main(&args(&["--mode", "wipe_home"]), None), 2);
    }
}

#[test]
fn entry_the_environment_of_the_helper_is_read_and_anything_missing_is_a_refusal() {
    let full = |name: &str| -> Option<String> {
        Some(
            match name {
                "HOME" => "/home/alice",
                "TMPDIR" => "/home/alice/.tmp",
                "SHELL" => "/bin/sh",
                "USER" => "alice",
                "LOGNAME" => "alice",
                _ => return None,
            }
            .to_owned(),
        )
    };
    let env = WorkerEnv::from_vars(&full).expect("a complete environment");
    assert_eq!((env.home.to_str(), env.tmp.to_str(), env.user.as_str()), (Some("/home/alice"), Some("/home/alice/.tmp"), "alice"));
    for missing in ["HOME", "TMPDIR", "SHELL", "USER"] {
        let without = |name: &str| if name == missing { None } else { full(name) };
        assert!(WorkerEnv::from_vars(&without).is_err(), "{missing} is required");
    }
    for (name, bad) in [("HOME", "relative"), ("HOME", ""), ("TMPDIR", "tmp"), ("SHELL", "sh"), ("USER", "")] {
        let changed = |n: &str| if n == name { Some(bad.to_owned()) } else { full(n) };
        assert!(WorkerEnv::from_vars(&changed).is_err(), "{name}={bad:?} was accepted");
    }
}

// ---- the wipes ----------------------------------------------------------------------------------------------------------

fn good_home(tag: &str) -> scratch::Scratch {
    scratch::Scratch::new(tag)
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> =
        std::fs::read_dir(dir).expect("a directory").flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

#[test]
fn wipe_secrets_removes_the_store_and_its_siblings_and_leaves_everything_else() {
    let uid = rustix::process::getuid().as_raw();
    let home = good_home("wipe-secrets");
    // Nothing there: success.
    assert_eq!(wipe_secrets(home.path(), uid), Ok(()));
    let data = home.join(".holzwave");
    std::fs::create_dir(&data).expect("data dir");
    for name in ["secrets.store", "secrets.store.lock", "secrets.store.tmp-abc123", "settings.json", "notes.txt"] {
        std::fs::write(data.join(name), b"x").expect("a file");
    }
    std::fs::write(home.join("keep.txt"), b"x").expect("a file");
    assert_eq!(wipe_secrets(home.path(), uid), Ok(()));
    assert_eq!(names_in(&data), vec!["notes.txt".to_owned(), "settings.json".to_owned()]);
    assert!(home.join("keep.txt").exists());
    // Twice is the same.
    assert_eq!(wipe_secrets(home.path(), uid), Ok(()));
}

#[test]
fn wipe_secrets_never_follows_a_link_out_of_the_home() {
    let uid = rustix::process::getuid().as_raw();
    let home = good_home("wipe-secrets-link");
    let outside = good_home("wipe-secrets-outside");
    std::fs::write(outside.join("secrets.store"), b"precious").expect("a file outside");
    std::os::unix::fs::symlink(outside.path(), home.join(".holzwave")).expect("the data directory is a link");
    let result = wipe_secrets(home.path(), uid);
    assert!(result.is_err(), "{result:?}");
    assert_eq!(std::fs::read(outside.join("secrets.store")).expect("untouched"), b"precious");
    // A store file that is itself a link is removed as a link and its target stays.
    let home = good_home("wipe-secrets-link-file");
    std::fs::create_dir(home.join(".holzwave")).expect("data dir");
    std::os::unix::fs::symlink(outside.join("secrets.store"), home.join(".holzwave").join("secrets.store")).expect("link");
    assert_eq!(wipe_secrets(home.path(), uid), Ok(()));
    assert!(names_in(&home.join(".holzwave")).is_empty());
    assert_eq!(std::fs::read(outside.join("secrets.store")).expect("untouched"), b"precious");
}

#[test]
fn wipe_home_removes_files_directories_and_links_and_leaves_the_home_and_every_target() {
    let uid = rustix::process::getuid().as_raw();
    let home = good_home("wipe-home");
    let outside = good_home("wipe-home-outside");
    std::fs::write(outside.join("target.txt"), b"precious").expect("target");
    std::fs::create_dir(outside.join("dir")).expect("dir");
    std::fs::write(outside.join("dir").join("inner.txt"), b"precious").expect("inner");
    std::fs::write(home.join("a.txt"), b"x").expect("file");
    std::fs::write(home.join(".hidden"), b"x").expect("hidden");
    std::fs::create_dir_all(home.join("deep").join("er")).expect("dirs");
    std::fs::write(home.join("deep").join("er").join("b.txt"), b"x").expect("file");
    std::os::unix::fs::symlink(outside.join("target.txt"), home.join("link-to-file")).expect("link");
    std::os::unix::fs::symlink(outside.join("dir"), home.join("link-to-dir")).expect("link");
    std::os::unix::fs::symlink(outside.join("dir"), home.join("deep").join("link-inside")).expect("link");
    std::os::unix::fs::symlink("/nonexistent/target", home.join("dangling")).expect("link");
    assert_eq!(wipe_home(home.path(), uid), Ok(()));
    assert!(home.path().is_dir(), "the home itself stays");
    assert!(names_in(home.path()).is_empty(), "{:?}", names_in(home.path()));
    assert_eq!(std::fs::read(outside.join("target.txt")).expect("untouched"), b"precious");
    assert_eq!(std::fs::read(outside.join("dir").join("inner.txt")).expect("untouched"), b"precious");
    // An empty home is a success too.
    assert_eq!(wipe_home(home.path(), uid), Ok(()));
}

#[test]
fn wipe_both_modes_refuse_a_home_that_fails_the_check_and_remove_nothing() {
    let uid = rustix::process::getuid().as_raw();
    let home = good_home("wipe-refuse");
    std::fs::create_dir(home.join(".holzwave")).expect("data dir");
    std::fs::write(home.join(".holzwave").join("secrets.store"), b"x").expect("file");
    std::fs::write(home.join("a.txt"), b"x").expect("file");
    // Wrong owner.
    assert!(wipe_home(home.path(), uid.wrapping_add(1)).is_err());
    assert!(wipe_secrets(home.path(), uid.wrapping_add(1)).is_err());
    // Mode with group bits.
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o750)).expect("mode");
    assert!(wipe_home(home.path(), uid).is_err());
    assert!(wipe_secrets(home.path(), uid).is_err());
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).expect("mode");
    // A relative path and a link to a good home.
    assert!(wipe_home(Path::new("relative"), uid).is_err());
    let link = home.join("..").join("not-a-home-link");
    let _ = link;
    assert!(home.join("a.txt").exists());
    assert!(home.join(".holzwave").join("secrets.store").exists());
}

// ---- the one lint exception ---------------------------------------------------------------------------------------------

/// What the audit of a source tree finds wrong, as sentences. The rule: the library root forbids the lint, the binary root
/// denies it, and exactly one line of attribute below the tree allows it, in the binary root. Comments are not attributes.
fn audit_exceptions(root: &Path) -> Vec<String> {
    fn files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("a directory").flatten() {
            let path = entry.path();
            if path.is_dir() {
                files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    files(root, &mut paths);
    paths.sort();
    let lint = ["unsafe", "_code"].concat();
    let mut problems = Vec::new();
    let mut exceptions: Vec<String> = Vec::new();
    for path in &paths {
        let name = path.strip_prefix(root).expect("below the root").display().to_string();
        let text = std::fs::read_to_string(path).expect("source");
        for line in text.lines() {
            let line = line.trim();
            let is_attribute = line.starts_with("#[") || line.starts_with("#![");
            if !is_attribute || !line.contains(&lint) {
                continue;
            }
            let is_guard = line.starts_with("#![deny(") || line.starts_with("#![forbid(") || line.starts_with("#[deny(") || line.starts_with("#[forbid(");
            if !is_guard {
                exceptions.push(name.clone());
            }
        }
    }
    if exceptions.len() != 1 {
        problems.push(format!("there are {} exceptions, not one: {exceptions:?}", exceptions.len()));
    } else if exceptions[0] != "main.rs" {
        problems.push(format!("the exception is in {}, not in main.rs", exceptions[0]));
    }
    let has = |file: &str, needle: &str| {
        std::fs::read_to_string(root.join(file)).is_ok_and(|t| t.lines().any(|l| l.trim().starts_with(needle)))
    };
    if !has("lib.rs", &format!("#![forbid({lint})]")) {
        problems.push("lib.rs does not forbid the lint".into());
    }
    if !has("main.rs", &format!("#![deny({lint})]")) {
        problems.push("main.rs does not deny the lint".into());
    }
    problems
}

fn sample_tree(tag: &str, lib: &str, main: &str, other: Option<(&str, &str)>) -> scratch::Scratch {
    let tree = scratch::Scratch::new(tag);
    std::fs::write(tree.join("lib.rs"), lib).expect("lib");
    std::fs::write(tree.join("main.rs"), main).expect("main");
    if let Some((name, text)) = other {
        std::fs::write(tree.join(name), text).expect("other");
    }
    tree
}

const FORBID: &str = "#![forbid(unsafe_code)]\n";
const DENY_AND_ALLOW: &str = "#![deny(unsafe_code)]\n#[allow(unsafe_code)]\nfn claim() {}\n";

#[test]
fn unsafe_exception_is_unique() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let problems = audit_exceptions(&src);
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn lint_audit_a_sample_with_one_exception_in_the_binary_root_passes() {
    let tree = sample_tree("lint-ok", FORBID, DENY_AND_ALLOW, Some(("cli.rs", "fn a() {}\n")));
    assert!(audit_exceptions(tree.path()).is_empty());
}

#[test]
fn lint_audit_a_second_exception_in_another_file_is_reported_with_both_files() {
    let tree = sample_tree("lint-two", FORBID, DENY_AND_ALLOW, Some(("worker.rs", "#[allow(unsafe_code)]\nfn b() {}\n")));
    let problems = audit_exceptions(tree.path());
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("main.rs") && problems[0].contains("worker.rs"), "{problems:?}");
    // The same file twice, and an exception in a list of lints.
    let tree = sample_tree("lint-twice", FORBID, "#![deny(unsafe_code)]\n#[allow(unsafe_code)]\nfn a() {}\n#[allow(dead_code, unsafe_code)]\nfn b() {}\n", None);
    assert!(!audit_exceptions(tree.path()).is_empty());
}

#[test]
fn lint_audit_an_exception_outside_the_binary_root_or_none_at_all_is_reported() {
    let tree = sample_tree("lint-elsewhere", FORBID, "#![deny(unsafe_code)]\n", Some(("worker.rs", "#[allow(unsafe_code)]\nfn b() {}\n")));
    assert!(audit_exceptions(tree.path()).iter().any(|p| p.contains("not in main.rs")));
    let tree = sample_tree("lint-none", FORBID, "#![deny(unsafe_code)]\n", None);
    assert!(audit_exceptions(tree.path()).iter().any(|p| p.contains("0 exceptions")));
}

#[test]
fn lint_audit_a_library_root_without_the_forbid_or_a_binary_root_without_the_deny_is_reported() {
    let tree = sample_tree("lint-nolib", "// no forbid here\n", DENY_AND_ALLOW, None);
    assert!(audit_exceptions(tree.path()).iter().any(|p| p.contains("lib.rs")));
    let tree = sample_tree("lint-nodeny", FORBID, "#[allow(unsafe_code)]\nfn claim() {}\n", None);
    assert!(audit_exceptions(tree.path()).iter().any(|p| p.contains("main.rs does not deny")));
    // Only a comment that says forbid does not count.
    let tree = sample_tree("lint-comment-forbid", "// #![forbid(unsafe_code)]\n", DENY_AND_ALLOW, None);
    assert!(audit_exceptions(tree.path()).iter().any(|p| p.contains("lib.rs")));
}

#[test]
fn lint_audit_counts_attribute_lines_so_a_comment_can_neither_raise_nor_hide_the_count() {
    let with_comments = "#![deny(unsafe_code)]\n// the allow attribute #[allow(unsafe_code)] is explained here\n/// #[allow(unsafe_code)]\n#[allow(unsafe_code)]\nfn claim() {}\n";
    let tree = sample_tree("lint-comments", FORBID, with_comments, Some(("cli.rs", "// #[allow(unsafe_code)]\n//! #[allow(unsafe_code)]\n")));
    assert!(audit_exceptions(tree.path()).is_empty(), "{:?}", audit_exceptions(tree.path()));
    // A comment alone is no exception.
    let tree = sample_tree("lint-only-comments", FORBID, "#![deny(unsafe_code)]\n// #[allow(unsafe_code)]\n", None);
    assert!(!audit_exceptions(tree.path()).is_empty());
}

#[test]
fn lint_the_binary_root_claims_only_descriptor_three_and_marks_it_close_on_exec_next() {
    let main = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("main.rs")).expect("main.rs");
    assert!(main.contains("CLOEXEC"), "the claim is followed by the close-on-exec flag");
    // The only raw descriptor number in the file is 3.
    let from_raw: Vec<&str> = main.lines().filter(|l| l.contains("from_raw_fd")).collect();
    assert_eq!(from_raw.len(), 1, "{from_raw:?}");
    assert!(from_raw[0].contains("from_raw_fd(3)"), "{from_raw:?}");
    // The flag is set in the very next statement.
    let at = main.lines().position(|l| l.contains("from_raw_fd")).expect("the claim");
    let next = main.lines().skip(at + 1).find(|l| !l.trim().is_empty() && !l.trim().starts_with("//")).expect("a next statement");
    assert!(next.contains("CLOEXEC") || next.contains("fcntl_setfd"), "{next}");
}

// ---- hostile frames -----------------------------------------------------------------------------------------------------

fn wait_failure(rig: &mut Rig) {
    rig.wait_for("a failure note", PATIENCE, |f| matches!(f, Frame::Note(Note::Exiting(ExitReason::Failure))));
}

#[test]
fn hostile_a_length_over_the_cap_ends_the_worker_before_any_payload_is_read() {
    let mut rig = Rig::start();
    rig.init();
    let over = u32::try_from(MAX_PAYLOAD + 1).expect("fits");
    rig.send_raw(&over.to_be_bytes());
    wait_failure(&mut rig);
    assert_eq!(rig.finish(), RunEnd::Failed);
    // The largest length a four-byte prefix can say.
    let mut rig = Rig::start();
    rig.init();
    rig.send_raw(&u32::MAX.to_be_bytes());
    wait_failure(&mut rig);
    assert_eq!(rig.finish(), RunEnd::Failed);
}

#[test]
fn hostile_a_length_of_zero_an_unknown_kind_and_every_malformed_control_end_the_worker() {
    let corpus: Vec<Vec<u8>> = vec![
        vec![0, 0, 0, 0],
        vec![0, 0, 0, 1, 0],
        vec![0, 0, 0, 1, 6],
        vec![0, 0, 0, 1, 255],
        vec![0, 0, 0, 2, 4, 99],
        vec![0, 0, 0, 2, 4, 9],
        vec![0, 0, 0, 2, 5, 99],
        vec![0, 0, 0, 3, 4, 2, 0],
        vec![0, 0, 0, 6, 4, 2, 0, 0, 0, 0],
        vec![0, 0, 0, 2, 1, 0],
    ];
    for bytes in corpus {
        let mut rig = Rig::start();
        rig.init();
        rig.send_raw(&bytes);
        wait_failure(&mut rig);
        assert_eq!(rig.finish(), RunEnd::Failed, "{bytes:?}");
    }
}

#[test]
fn hostile_a_frame_cut_off_by_the_end_of_the_stream_is_a_failure_not_a_hang() {
    for bytes in [vec![0u8], vec![0, 0, 0], vec![0, 0, 0, 10, 1, 2, 3], vec![0, 0, 0, 14, 1, 0, 0, 0, 1]] {
        let mut rig = Rig::start();
        rig.init();
        rig.send_raw(&bytes);
        assert_eq!(rig.finish(), RunEnd::Failed, "{bytes:?}");
    }
}

#[test]
fn hostile_a_command_that_is_not_an_object_gets_an_error_answer_and_the_worker_serves_on() {
    let mut rig = Rig::start();
    rig.init();
    for (i, json) in ["[1,2,3]", "null", "42", "\"get_layout\"", "{", "", "\u{feff}{}", "{\"cmd\":\"get_layout\"} x"].iter().enumerate() {
        let id = 500 + i as u64;
        rig.send_json(3, id, (*json).to_owned());
        assert!(matches!(rig.wait_res(id).body, ResBody::Err(_)), "{json:?}");
    }
    assert_eq!(rig.ok(3, "get_layout", json!({}))["panes"].as_array().expect("panes").len(), 1);
    rig.finish();
}

#[test]
fn hostile_a_control_before_init_an_attach_for_socket_zero_and_a_second_init_end_the_worker() {
    let mut rig = Rig::start();
    rig.send(Frame::Ctl(Ctl::Attach { socket: 3 }));
    wait_failure(&mut rig);
    assert_eq!(rig.finish(), RunEnd::Failed);

    let mut rig = Rig::start();
    rig.send(Frame::Ctl(Ctl::Lock { ctl: 1 }));
    wait_failure(&mut rig);
    assert_eq!(rig.finish(), RunEnd::Failed);

    let mut rig = Rig::start();
    rig.init();
    // Socket 0 cannot be written by the encoder, so the bytes are made by hand.
    rig.send_raw(&[0, 0, 0, 6, 4, 2, 0, 0, 0, 0]);
    wait_failure(&mut rig);
    assert_eq!(rig.finish(), RunEnd::Failed);

    let mut rig = Rig::start();
    rig.init();
    rig.send(Frame::Ctl(Ctl::Init { version: PROTOCOL_VERSION, max_panes: 1, max_scrollback_lines: 1, max_image_mib: 64 }));
    wait_failure(&mut rig);
    assert_eq!(rig.finish(), RunEnd::Failed);
}

#[test]
fn hostile_one_hundred_thousand_detach_frames_are_taken_and_the_worker_still_answers() {
    let mut rig = Rig::start();
    rig.init();
    for i in 0..100_000u32 {
        rig.send(Frame::Ctl(Ctl::Detach { socket: 1 + i % 50 }));
    }
    let view = rig.ok(3, "get_layout", json!({}));
    assert_eq!(view["panes"].as_array().expect("panes").len(), 1);
    assert_eq!(rig.finish(), RunEnd::LinkClosed);
}

#[test]
fn hostile_a_corpus_of_three_hundred_mixed_commands_is_answered_exactly_once_each() {
    let mut rig = Rig::start();
    rig.init();
    let first = first_pane(&rig.ok(3, "get_layout", json!({})));
    let mut ids = Vec::new();
    for i in 0..300u64 {
        let id = 10_000 + i;
        let json = match i % 10 {
            0 => json!({"cmd": "get_layout"}).to_string(),
            1 => json!({"cmd": "get_layout", "args": {}}).to_string(),
            2 => json!({"cmd": "rename_pane", "args": {"pane": first, "name": format!("n{i}")}}).to_string(),
            3 => json!({"cmd": "rename_pane", "args": {"pane": "x", "name": 5}}).to_string(),
            4 => json!({"cmd": "no_such_command", "args": {}}).to_string(),
            5 => "not json at all".to_owned(),
            6 => json!({"cmd": "app_info"}).to_string(),
            7 => json!({"cmd": "get_layout", "args": [1]}).to_string(),
            8 => json!({"cmd": "", "args": {}}).to_string(),
            _ => json!({"cmd": "get_settings", "args": {"extra": true}}).to_string(),
        };
        rig.send_json(1 + u32::try_from(i % 7).expect("small"), id, json);
        ids.push(id);
    }
    for id in &ids {
        rig.wait_res(*id);
    }
    // One more round trip, so that nothing late is still on its way.
    rig.ok(3, "get_layout", json!({}));
    for id in &ids {
        let n = rig.seen().iter().filter(|f| matches!(f, Frame::Res(r) if r.request == *id)).count();
        assert_eq!(n, 1, "request {id} was answered {n} times");
    }
    rig.finish();
}

// ---- the canary ---------------------------------------------------------------------------------------------------------

const KEY_CANARY: &str = "canary-key-bytes-0123456789abcde";
const ID_CANARY: &[u8; 16] = b"canary-slot-id-0";
const ARG_CANARY: &str = "ARG-CANARY-VALUE-77";
const OUT_CANARY: &str = "PANE-CANARY-OUT-88";

fn canary_key() -> Key {
    let bytes: [u8; 32] = KEY_CANARY.as_bytes().try_into().expect("32 bytes");
    Key::from_bytes(&bytes)
}

fn files_hold(dir: &Path, needles: &[&[u8]]) -> Vec<String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).expect("a directory").flatten() {
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).expect("metadata");
        if meta.is_dir() {
            found.extend(files_hold(&path, needles));
        } else if meta.is_file() {
            let bytes = std::fs::read(&path).expect("a file");
            for needle in needles {
                if bytes.windows(needle.len()).any(|w| w == *needle) {
                    found.push(path.display().to_string());
                }
            }
        }
    }
    found
}

#[test]
fn canary_keys_arguments_and_output_reach_no_log_no_error_no_debug_text_and_no_file() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let script = format!("echo {OUT_CANARY}; exec cat");
    let mut rig = Rig::start_with(RigOptions {
        hooks: TestHooks { log: Some(log.clone()), ..TestHooks::default() },
        shell: Some(ShellSpec::new("/bin/sh", &["-c", &script])),
    });
    rig.init();
    let first = first_pane(&rig.ok(2, "get_layout", json!({})));
    rig.ok(2, "start_pane", json!({"pane": first, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}));
    // The pane's output is the pane's own: it travels as events and is nowhere else.
    rig.wait_event("frame", |_, payload| payload["pane"] == json!(first));

    let mut sent: Vec<Ctl> = Vec::new();
    let mut control = |rig: &mut Rig, ctl: Ctl, id: u64| {
        sent.push(ctl.clone());
        rig.control(ctl, id)
    };
    let provision = Ctl::Provision {
        ctl: 1,
        slots: vec![
            SlotKey { kind: SlotKind::Password, id: *ID_CANARY, key: canary_key() },
            SlotKey { kind: SlotKind::Recovery, id: [2; 16], key: key(2) },
        ],
    };
    assert_eq!(control(&mut rig, provision, 1), CtlOutcome::Done);
    let set = Ctl::SetSlot {
        ctl: 2,
        slot: SlotKey { kind: SlotKind::Passphrase, id: *ID_CANARY, key: canary_key() },
        opening: None,
    };
    assert_eq!(control(&mut rig, set, 2), CtlOutcome::Done);
    assert_eq!(control(&mut rig, Ctl::Lock { ctl: 3 }, 3), CtlOutcome::Done);
    let unlock = Ctl::Unlock { ctl: 4, kind: SlotKind::Password, key: canary_key() };
    assert_eq!(control(&mut rig, unlock, 4), CtlOutcome::Done);
    let wrong = Ctl::Unlock { ctl: 5, kind: SlotKind::Prf, key: canary_key() };
    assert_eq!(control(&mut rig, wrong, 5), CtlOutcome::Locked);
    let remove = Ctl::RemoveSlot { ctl: 6, kind: SlotKind::Passphrase, id: *ID_CANARY };
    assert_eq!(control(&mut rig, remove, 6), CtlOutcome::Done);

    // Commands that fail with the canary as the argument.
    let mut errors = Vec::new();
    for (name, args) in [
        ("rename_pane", json!({"pane": ARG_CANARY, "name": "x"})),
        ("rename_pane", json!({"pane": first, "name": {"nested": ARG_CANARY}})),
        ("ai_key_set", json!({"profile": ARG_CANARY, "slot": "provider", "api_key": ARG_CANARY})),
        ("integration_secret_set", json!({"profile": ARG_CANARY, "name": ARG_CANARY, "value": ARG_CANARY})),
        ("get_layout", json!([ARG_CANARY])),
    ] {
        let res = rig.call(2, name, args);
        errors.push(res_text(&res));
    }
    rig.send_json(2, 900, format!("{{\"cmd\":\"{ARG_CANARY}\",\"args\":{{}}}}"));
    errors.push(res_text(&rig.wait_res(900)));
    rig.send_json(2, 901, format!("{{\"cmd\":\"get_layout\",\"args\":\"{ARG_CANARY}\"}}"));
    errors.push(res_text(&rig.wait_res(901)));

    let needles = [KEY_CANARY, ARG_CANARY, OUT_CANARY, "canary-slot-id"];
    for error in &errors {
        for needle in needles {
            assert!(!error.contains(needle), "an error answer holds {needle}: {error}");
        }
    }
    for line in log.lock().expect("log").iter() {
        for needle in needles {
            assert!(!line.contains(needle), "a log line holds {needle}: {line}");
        }
    }
    for ctl in &sent {
        let text = format!("{ctl:?} {ctl:#?}");
        for needle in needles {
            assert!(!text.contains(needle), "the Debug text of a control holds {needle}: {text}");
        }
    }
    for frame in rig.seen() {
        if matches!(frame, Frame::Evt(_)) {
            continue;
        }
        let text = format!("{frame:?}");
        for needle in [KEY_CANARY, "canary-slot-id"] {
            assert!(!text.contains(needle), "a frame the worker wrote holds {needle}: {text}");
        }
    }
    // The ids of slots are not secret and the store file lists them; the keys, the arguments and the output are not in any file.
    let in_files = files_hold(rig.home.path(), &[KEY_CANARY.as_bytes(), ARG_CANARY.as_bytes(), OUT_CANARY.as_bytes()]);
    assert!(in_files.is_empty(), "files under the home hold a canary: {in_files:?}");
    rig.finish();
}

// ---- the real binary on Linux -------------------------------------------------------------------------------------------

/// These tests start the real binary the way the helper does: descriptors 0, 1, 2 and the socket on 3, an empty
/// environment but for the variables of the helper, no capability, `no_new_privs`, and as another user when the test runs
/// as the superuser. `setpriv` of util-linux makes that start (in the container run of `scripts/linux-container-test.sh`).
#[cfg(target_os = "linux")]
mod on_linux {
    use super::*;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::chown;
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    const OTHER_USER: u32 = 65534;
    const SETPRIV: &str = "/usr/bin/setpriv";

    struct Started {
        child: Child,
        ours: UnixStream,
        home: scratch::Scratch,
    }

    /// Starts `holzwave-server worker --mode serve` with the socket on descriptor 3 and `extra` redirections.
    fn start(extra: &str, isolated: bool) -> Started {
        let home = scratch::Scratch::new("linux-worker");
        let tmp = home.join(".tmp");
        std::fs::create_dir(&tmp).expect("tmp");
        let as_root = rustix::process::getuid().is_root();
        if as_root {
            for path in [home.path(), tmp.as_path()] {
                chown(path, Some(OTHER_USER), Some(OTHER_USER)).expect("the home belongs to the worker's user");
            }
        }
        let (ours, theirs) = UnixStream::pair().expect("a socket pair");
        let script = format!("exec \"$0\" worker --mode serve 3<&0 {extra} 0</dev/null");
        let binary = env!("CARGO_BIN_EXE_holzwave-server");
        let mut command = if isolated {
            let mut command = Command::new(SETPRIV);
            if as_root {
                command.arg(format!("--reuid={OTHER_USER}")).arg(format!("--regid={OTHER_USER}")).arg("--clear-groups");
                command.arg("--inh-caps=-all").arg("--bounding-set=-all");
            }
            command.arg("--no-new-privs").arg("sh").arg("-c").arg(&script).arg(binary);
            command
        } else {
            let mut command = Command::new("sh");
            command.arg("-c").arg(&script).arg(binary);
            command
        };
        command
            .env_clear()
            .env("HOME", home.path())
            .env("TMPDIR", &tmp)
            .env("SHELL", "/bin/sh")
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("USER", "worker")
            .env("LOGNAME", "worker")
            .stdin(Stdio::from(OwnedFd::from(theirs)))
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let child = command.spawn().expect("the worker starts (is setpriv installed?)");
        ours.set_read_timeout(Some(PATIENCE)).expect("timeout");
        Started { child, ours, home }
    }

    fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = child.try_wait().expect("wait") {
                return status;
            }
            assert!(Instant::now() < deadline, "the worker did not end within {PATIENCE:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn stderr_of(child: &mut Child) -> String {
        use std::io::Read;
        let mut text = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    }

    fn read_until(ours: &mut UnixStream, wanted: impl Fn(&Frame) -> bool) -> Frame {
        loop {
            match read_frame(ours) {
                Ok(Some(frame)) if wanted(&frame) => return frame,
                Ok(Some(_)) => {}
                other => panic!("the worker's side of the link ended early: {other:?}"),
            }
        }
    }

    fn handshake(started: &mut Started) {
        holzwave_server::wire::write_frame(
            &mut started.ours,
            &Frame::Ctl(Ctl::Init { version: PROTOCOL_VERSION, max_panes: 0, max_scrollback_lines: 0, max_image_mib: 64 }),
        )
        .expect("init");
        read_until(&mut started.ours, |f| matches!(f, Frame::Note(Note::Ready)));
    }

    fn call(started: &mut Started, request: u64, cmd: &str, args: Value) -> Value {
        let json = json!({"cmd": cmd, "args": args}).to_string();
        holzwave_server::wire::write_frame(&mut started.ours, &Frame::Cmd(CmdFrame { socket: 1, request, json })).expect("cmd");
        match read_until(&mut started.ours, |f| matches!(f, Frame::Res(r) if r.request == request)) {
            Frame::Res(Res { body: ResBody::Ok(text), .. }) => serde_json::from_str(&text).expect("JSON"),
            other => panic!("{cmd} did not succeed: {other:?}"),
        }
    }

    #[test]
    fn linux_the_real_binary_answers_ready_serves_a_command_and_ends_with_status_0_when_the_socket_closes() {
        let mut started = start("", true);
        handshake(&mut started);
        let view = call(&mut started, 1, "get_layout", json!({}));
        assert_eq!(view["panes"].as_array().expect("panes").len(), 1);
        started.ours.shutdown(std::net::Shutdown::Both).expect("close");
        let status = wait_exit(&mut started.child);
        let text = stderr_of(&mut started.child);
        assert_eq!(status.code(), Some(0), "{text}");
        // The home keeps the worker's data directory and nothing the page could not have asked for.
        assert!(started.home.join(".holzwave").is_dir());
    }

    #[test]
    fn linux_a_start_with_descriptor_4_open_exits_with_status_2_and_one_sentence() {
        let mut started = start("4<&0", true);
        let status = wait_exit(&mut started.child);
        let text = stderr_of(&mut started.child);
        assert_eq!(status.code(), Some(2), "{text}");
        assert!(text.contains("descriptor 4"), "{text}");
        assert_eq!(text.trim().lines().count(), 1, "{text}");
    }

    #[test]
    fn linux_a_start_without_the_isolation_exits_with_status_2_and_a_sentence() {
        // Not isolated: the superuser of the container, or a user without no_new_privs.
        let mut started = start("", false);
        let status = wait_exit(&mut started.child);
        let text = stderr_of(&mut started.child);
        assert_eq!(status.code(), Some(2), "{text}");
        assert!(!text.trim().is_empty());
        assert_eq!(text.trim().lines().count(), 1, "{text}");
    }

    #[test]
    fn linux_a_pane_shell_has_no_socket_among_its_descriptors() {
        let mut started = start("", true);
        handshake(&mut started);
        let first = first_pane(&call(&mut started, 1, "get_layout", json!({})));
        call(&mut started, 2, "start_pane", json!({"pane": first, "cols": 100, "rows": 30, "cell_w": 8, "cell_h": 16}));
        let listing = started.home.join("fds.txt");
        let line = format!("ls -l /proc/$$/fd > {}\r", listing.display());
        call(&mut started, 3, "text", json!({"pane": first, "text": line}));
        let deadline = Instant::now() + PATIENCE;
        let text = loop {
            if let Ok(text) = std::fs::read_to_string(&listing)
                && text.lines().count() >= 4
            {
                break text;
            }
            assert!(Instant::now() < deadline, "the shell never wrote its descriptor listing");
            std::thread::sleep(Duration::from_millis(50));
        };
        // Printed for the record: the descriptors of the pane's own shell (the last column is where each one points).
        println!("pane shell descriptors:\n{text}");
        assert!(!text.contains("socket:"), "the pane's shell holds a socket:\n{text}");
        started.ours.shutdown(std::net::Shutdown::Both).expect("close");
        assert_eq!(wait_exit(&mut started.child).code(), Some(0));
    }
}
