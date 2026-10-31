//! Order through the socket: the lines typed into a pane arrive in the order they were sent, the output of a pane arrives with
//! growing numbers, and a screen rebuilt from the frames is the screen the core reports.
//!
//! The tracer of the parity suite proves one command of each group; this file proves the sequence of many. Everything here
//! needs the `in-process-workers` feature, and the page must have been built (`npm --prefix app run build`).
#![cfg(feature = "in-process-workers")]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/parity_env.rs"]
mod parity_env;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::time::Duration;

use parity_env::{
    Line, Probed, Screen, check_numbered, fresh, screen_of, split_and_start, start_focused_pane, strictly_increasing, type_into,
    until_screen_has, whole_text,
};
use serde_json::{Value, json};
use ws_env::{Person, WsOptions};

/// How many numbered lines go into each pane.
const LINES: u64 = 300;

/// The first number of the second pane, so that the two files cannot be mistaken for one another.
const SECOND_BASE: u64 = 1_001;

/// How long a test waits for a file or a screen to be complete.
const BOUND: Duration = Duration::from_secs(30);

/// The end-of-file character of a terminal.
const END_OF_FILE: &str = "\u{4}";

/// The path of a file in the home of `person`, as the shell and the file commands both name it.
fn file_in_home(probed: &Probed, person: &Person, name: &str) -> String {
    probed.env.launcher.home(person.key.uid).join(name).to_string_lossy().into_owned()
}

/// Reads a file of the person through `file_open`, as the page would.
async fn read_file(line: &mut Line, path: &str) -> Option<String> {
    let answer = line.call("file_open", &json!({"source": "local", "path": path})).await.ok()?;
    answer.get("ok")?.get("text")?.as_str().map(str::to_owned)
}

/// Waits until the file holds `last` as its last line, and returns its text.
async fn file_ends_with(line: &mut Line, path: &str, last: u64) -> String {
    let deadline = tokio::time::Instant::now() + BOUND;
    let mut text = String::new();
    while tokio::time::Instant::now() < deadline {
        if let Some(read) = read_file(line, path).await {
            text = read;
            if text.lines().last().is_some_and(|l| l.trim() == last.to_string()) {
                return text;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let tail: Vec<&str> = text.lines().rev().take(5).collect();
    panic!("{path} did not end with {last} in time; it holds {} bytes, the last lines (backwards) {tail:?}", text.len());
}

/// Starts `cat` into `file` in `pane` and waits until the shell shows the command.
async fn start_cat(line: &mut Line, pane: u64, file: &str) {
    type_into(line, pane, &format!("cat > {file}\r")).await;
    assert!(until_screen_has(line, pane, "cat > ", BOUND).await, "the shell echoed the command");
    // The terminal queues what is typed before `cat` reads it, so no more waiting is needed for the order.
}

/// The numbers `from..from + LINES` as the lines of a file.
fn check_file(text: &str, from: u64) -> Result<(), String> {
    check_numbered(text, "", from, from + LINES - 1)?;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() as u64 != LINES {
        return Err(format!("{} lines where {LINES} were due", lines.len()));
    }
    Ok(())
}

/// Two panes of one person, a file in each.
struct Panes {
    first: u64,
    second: u64,
    file_a: String,
    file_b: String,
}

async fn two_panes_with_cat(probed: &Probed, person: &Person, line: &mut Line) -> Panes {
    let first = start_focused_pane(line).await;
    let second = split_and_start(line).await;
    let file_a = file_in_home(probed, person, "seq-a.txt");
    let file_b = file_in_home(probed, person, "seq-b.txt");
    start_cat(line, first, &file_a).await;
    start_cat(line, second, &file_b).await;
    Panes { first, second, file_a, file_b }
}

fn text_args(pane: u64, text: &str) -> Value {
    json!({"pane": pane, "text": text})
}

// ---- input --------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn input_300_lines_into_each_of_two_panes_interleaved_on_one_socket_arrive_in_order() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    let panes = two_panes_with_cat(&probed, &alice, &mut line).await;
    // 600 commands without waiting for an answer, one pane and the other in turn.
    let mut ids = (Vec::new(), Vec::new());
    for i in 0..LINES {
        ids.0.push(line.send("text", &text_args(panes.first, &format!("{}\n", 1 + i))).await);
        ids.1.push(line.send("text", &text_args(panes.second, &format!("{}\n", SECOND_BASE + i))).await);
    }
    let answers = line.answers(2 * LINES as usize, BOUND).await;
    assert_eq!(answers.len() as u64, 2 * LINES, "every command was answered");
    assert!(answers.iter().all(|a| a.get("ok").is_some()), "every command was accepted");
    let arrival: Vec<u64> = answers.iter().filter_map(|a| a["id"].as_u64()).collect();
    for (name, sent) in [("first", &ids.0), ("second", &ids.1)] {
        let seen: Vec<u64> = arrival.iter().copied().filter(|id| sent.contains(id)).collect();
        assert_eq!(&seen, sent, "the answers for the {name} pane came in the order of the requests");
    }
    type_into(&mut line, panes.first, END_OF_FILE).await;
    type_into(&mut line, panes.second, END_OF_FILE).await;
    let a = file_ends_with(&mut line, &panes.file_a, LINES).await;
    let b = file_ends_with(&mut line, &panes.file_b, SECOND_BASE + LINES - 1).await;
    check_file(&a, 1).unwrap_or_else(|d| panic!("seq-a.txt: {d}"));
    check_file(&b, SECOND_BASE).unwrap_or_else(|d| panic!("seq-b.txt: {d}"));
    println!(
        "input order, one socket: seq-a.txt {} lines from {} to {}; seq-b.txt {} lines from {} to {}",
        a.lines().count(),
        a.lines().next().unwrap_or_default(),
        a.lines().last().unwrap_or_default(),
        b.lines().count(),
        b.lines().next().unwrap_or_default(),
        b.lines().last().unwrap_or_default()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn input_the_same_with_the_two_panes_driven_from_two_sockets_of_the_person() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut one = fresh(&probed, &alice).await;
    let panes = two_panes_with_cat(&probed, &alice, &mut one).await;
    let mut two = fresh(&probed, &alice).await;
    // Both sockets send at the same time; each sends its own 300 lines in order.
    let (first, second) = (panes.first, panes.second);
    let drive_one = async {
        let mut ids = Vec::new();
        for i in 0..LINES {
            ids.push(one.send("text", &text_args(first, &format!("{}\n", 1 + i))).await);
        }
        ids
    };
    let drive_two = async {
        let mut ids = Vec::new();
        for i in 0..LINES {
            ids.push(two.send("text", &text_args(second, &format!("{}\n", SECOND_BASE + i))).await);
        }
        ids
    };
    let (ids_one, ids_two) = tokio::join!(drive_one, drive_two);
    let answers_one = one.answers(ids_one.len(), BOUND).await;
    let answers_two = two.answers(ids_two.len(), BOUND).await;
    let arrival = |answers: &[Value]| answers.iter().filter_map(|a| a["id"].as_u64()).collect::<Vec<u64>>();
    assert_eq!(arrival(&answers_one), ids_one, "the answers of the first socket came in order");
    assert_eq!(arrival(&answers_two), ids_two, "the answers of the second socket came in order");
    type_into(&mut one, first, END_OF_FILE).await;
    type_into(&mut one, second, END_OF_FILE).await;
    let a = file_ends_with(&mut one, &panes.file_a, LINES).await;
    let b = file_ends_with(&mut one, &panes.file_b, SECOND_BASE + LINES - 1).await;
    check_file(&a, 1).unwrap_or_else(|d| panic!("seq-a.txt: {d}"));
    check_file(&b, SECOND_BASE).unwrap_or_else(|d| panic!("seq-b.txt: {d}"));
    println!("input order, two sockets: seq-a.txt and seq-b.txt each hold their {LINES} lines in order");
}

#[tokio::test(flavor = "multi_thread")]
async fn input_300_lines_into_one_pane_with_more_than_one_command_kind_in_between_arrive_in_order() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    let pane = start_focused_pane(&mut line).await;
    let file = file_in_home(&probed, &alice, "seq-c.txt");
    start_cat(&mut line, pane, &file).await;
    // Keys and text are two commands of one pane: the order between them is the order of sending. Two commands per line keep the
    // burst (600) under what a socket may send at once.
    let mut sent = 0;
    for n in 1..=LINES {
        line.send("text", &text_args(pane, &n.to_string())).await;
        line.send("key", &json!({"pane": pane, "code": "Enter", "key": "Enter", "mods": 0, "repeat": false, "release": false, "composing": false})).await;
        sent += 2;
    }
    let answers = line.answers(sent, BOUND).await;
    assert_eq!(answers.len(), sent);
    assert!(answers.iter().all(|a| a.get("ok").is_some()), "every command was accepted (none refused for the rate)");
    type_into(&mut line, pane, END_OF_FILE).await;
    let text = file_ends_with(&mut line, &file, LINES).await;
    check_file(&text, 1).unwrap_or_else(|d| panic!("seq-c.txt: {d}"));
}

// ---- output -------------------------------------------------------------------------------------------------------------

const PRINT_500: &str = "i=1; while [ $i -le 500 ]; do echo \"hw-out$i\"; i=$((i+1)); done; echo hw-out-done-$((40+2))\r";

#[tokio::test(flavor = "multi_thread")]
async fn output_the_frames_of_a_pane_that_prints_500_lines_have_strictly_increasing_seq() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    let pane = start_focused_pane(&mut line).await;
    type_into(&mut line, pane, PRINT_500).await;
    assert!(until_screen_has(&mut line, pane, "hw-out-done-42", BOUND).await, "the pane printed everything");
    line.pump(Duration::from_millis(300)).await;
    let frames = line.frames_of(pane);
    strictly_increasing(&frames).unwrap_or_else(|d| panic!("{d}"));
    assert_eq!(frames[0]["full"], json!(true), "the first frame is a full one");
    println!(
        "output order: {} frames, seq {} to {}, strictly increasing",
        frames.len(),
        frames[0]["seq"],
        frames.last().map_or(Value::Null, |f| f["seq"].clone())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn output_a_pane_that_prints_slowly_sends_many_frames_with_growing_seq() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    let pane = start_focused_pane(&mut line).await;
    type_into(&mut line, pane, "i=1; while [ $i -le 120 ]; do echo \"hw-slow$i\"; i=$((i+1)); sleep 0.02; done; echo hw-slow-done-$((40+2))\r").await;
    assert!(until_screen_has(&mut line, pane, "hw-slow-done-42", BOUND).await);
    let frames = line.frames_of(pane);
    strictly_increasing(&frames).unwrap_or_else(|d| panic!("{d}"));
    assert!(frames.len() >= 30, "only {} frames for 120 lines printed over 2.4 seconds", frames.len());
    let screen = Screen::rebuilt(&frames);
    assert!(screen.rows.iter().any(|r| r.contains("hw-slow120")), "the last numbered line is on the rebuilt screen");
    println!("output order, slow: {} frames, seq {} to {}", frames.len(), frames[0]["seq"], frames.last().map_or(Value::Null, |f| f["seq"].clone()));
}

#[tokio::test(flavor = "multi_thread")]
async fn output_the_screen_rebuilt_from_the_frames_is_the_end_of_the_text_the_core_reports() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    let pane = start_focused_pane(&mut line).await;
    type_into(&mut line, pane, PRINT_500).await;
    assert!(until_screen_has(&mut line, pane, "hw-out-done-42", BOUND).await, "the pane printed everything");
    line.pump(Duration::from_millis(300)).await;
    let screen = screen_of(&line, pane);
    let text = whole_text(&mut line, pane).await;
    let core: Vec<&str> = text.lines().map(str::trim_end).collect();
    let shown: Vec<&str> = screen.rows.iter().map(|r| r.trim_end()).collect();
    let shown: Vec<&str> = {
        let mut rows = shown;
        while rows.last().is_some_and(|r| r.is_empty()) {
            rows.pop();
        }
        rows
    };
    let core_trimmed: Vec<&str> = {
        let mut rows = core;
        while rows.last().is_some_and(|r| r.is_empty()) {
            rows.pop();
        }
        rows
    };
    assert!(!shown.is_empty());
    assert!(
        core_trimmed.ends_with(&shown),
        "the rebuilt screen is not the end of the core's text\nscreen: {shown:?}\ncore (end): {:?}",
        &core_trimmed[core_trimmed.len().saturating_sub(shown.len() + 2)..]
    );
    check_numbered(&text, "hw-out", 1, 500).unwrap_or_else(|d| panic!("the text of the pane: {d}"));
    println!("output order: the {} rows of the rebuilt screen are the last {} lines of the core's text of {} lines", shown.len(), shown.len(), core_trimmed.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn output_two_panes_interleave_but_each_pane_keeps_its_own_order() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    let first = start_focused_pane(&mut line).await;
    let second = split_and_start(&mut line).await;
    type_into(&mut line, first, PRINT_500).await;
    type_into(&mut line, second, PRINT_500).await;
    assert!(until_screen_has(&mut line, first, "hw-out-done-42", BOUND).await);
    assert!(until_screen_has(&mut line, second, "hw-out-done-42", BOUND).await);
    line.pump(Duration::from_millis(300)).await;
    for pane in [first, second] {
        let frames = line.frames_of(pane);
        strictly_increasing(&frames).unwrap_or_else(|d| panic!("pane {pane}: {d}"));
        assert!(!frames.is_empty());
    }
    // The two sequences really were mixed on the wire, or the test proves nothing about interleaving.
    let order: Vec<u64> = line
        .events
        .iter()
        .filter(|e| e["event"] == "frame")
        .filter_map(|e| e["payload"]["pane"].as_u64())
        .collect();
    let switches = order.windows(2).filter(|w| w[0] != w[1]).count();
    println!("output order: {} frames of two panes with {switches} switches between them, each pane strictly increasing", order.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn output_no_frame_arrives_for_a_pane_after_its_pane_exit() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    let _first = start_focused_pane(&mut line).await;
    let second = split_and_start(&mut line).await;
    type_into(&mut line, second, "echo hw-last-$((40+2))\r").await;
    assert!(until_screen_has(&mut line, second, "hw-last-42", BOUND).await);
    type_into(&mut line, second, "exit\r").await;
    assert!(line.until(BOUND, |events| events.iter().any(|e| e["event"] == "pane-exit" && e["payload"]["pane"] == second)).await);
    // The shell ended, the pane stays in the layout: nothing may still be drawn for it afterwards.
    line.pump(Duration::from_secs(1)).await;
    let exit_at = line.events.iter().position(|e| e["event"] == "pane-exit" && e["payload"]["pane"] == second).expect("pane-exit");
    let late: Vec<&Value> = line.events[exit_at + 1..]
        .iter()
        .filter(|e| e["event"] == "frame" && e["payload"]["pane"] == second)
        .collect();
    assert!(late.is_empty(), "{} frames for the pane after pane-exit", late.len());
    let all = Screen::rebuilt(&line.frames_of(second));
    assert!(all.rows.iter().any(|r| r.contains("hw-last-42")), "the last output was drawn before the exit");
}
