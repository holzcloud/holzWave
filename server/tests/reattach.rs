//! Panes outlive the socket: a counting program runs on while the socket is dropped and the next socket sees every number once;
//! two browsers of one person show the same screen; people never see each other's output; the pane cap and the idle time of the
//! worker hold at the socket.
//!
//! Everything here needs the `in-process-workers` feature, and the page must have been built (`npm --prefix app run build`).
//! The idle tests drive the clock of the supervisor by hand and call its sweep, so no test waits for the real time of a worker.
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

use holzwave_server::supervisor::SupervisorSettings;
use parity_env::{
    Line, Probed, Screen, check_numbered, counting_program, fresh, process_exists, screen_of, split_and_start, start_focused_pane,
    strictly_increasing, type_into, until_screen, until_screen_has, whole_text,
};
use serde_json::json;
use ws_env::{Person, WsOptions};

/// The numbers the counting program prints.
const COUNT: u64 = 300;

/// The pause between two numbers, as `sleep` takes it.
const PAUSE: &str = "0.02";

/// How long a test waits for a screen or a count.
const BOUND: Duration = Duration::from_secs(45);

/// The title the counting program sets, so that a replay has something to carry.
const TITLE: &str = "hw-count";

/// The program that sets a title and counts from 1 to `COUNT`, then prints `hw-done`.
fn program() -> String {
    format!("printf '\\033]0;{TITLE}\\007'; {}", counting_program(COUNT, PAUSE))
}

/// A pane that counts, and the socket that started it.
async fn counting(probed: &Probed, person: &Person) -> (Line, u64) {
    let mut line = fresh(probed, person).await;
    let pane = start_focused_pane(&mut line).await;
    type_into(&mut line, pane, &program()).await;
    (line, pane)
}

async fn wait_for_number(line: &mut Line, pane: u64, n: u64) {
    let needle = format!("hw-n{n}");
    assert!(until_screen_has(line, pane, &needle, BOUND).await, "{needle} never appeared on the screen of the pane");
}

/// What a page does after a reconnect: the layout, then a full frame of every pane.
async fn page_reconnects(line: &mut Line, panes: &[u64]) {
    line.ok("get_layout", &json!({})).await;
    for pane in panes {
        line.ok("request_full", &json!({"pane": pane})).await;
    }
}

/// Waits for `hw-done` as a line of its own (the echo of the program holds the word inside a longer line), then checks the whole
/// text of the pane: `hw-n1` to `hw-n300` once each in order and `hw-done` once.
async fn finished_without_a_gap(line: &mut Line, pane: u64) -> String {
    assert!(until_screen(line, pane, BOUND, |rows| rows.iter().any(|r| r.trim() == "hw-done")).await, "hw-done never appeared");
    let text = whole_text(line, pane).await;
    check_numbered(&text, "hw-n", 1, COUNT).unwrap_or_else(|d| panic!("the text of the pane: {d}"));
    let done = text.lines().filter(|l| l.trim() == "hw-done").count();
    assert_eq!(done, 1, "hw-done appears {done} times");
    text
}

async fn detached(probed: &Probed, person: &Person) {
    let key = person.key;
    probed.env.eventually("the socket to detach", |env| env.supervisor.account_stats(key).sockets == 0).await;
}

// ---- reattach -----------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn reattach_a_socket_dropped_without_a_close_frame_leaves_no_gap_for_the_next() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (mut a, pane) = counting(&probed, &alice).await;
    wait_for_number(&mut a, pane, 40).await;
    drop(a);
    detached(&probed, &alice).await;
    let mut b = fresh(&probed, &alice).await;
    page_reconnects(&mut b, &[pane]).await;
    assert!(
        !screen_of(&b, pane).rows.iter().any(|r| r.trim() == "hw-done"),
        "the program was done before the second socket attached; the test proves nothing"
    );
    let text = finished_without_a_gap(&mut b, pane).await;
    println!("reattach, TCP dropped: hw-n1 to hw-n{COUNT} once each in order in {} lines of text, hw-done once", text.lines().count());
}

#[tokio::test(flavor = "multi_thread")]
async fn reattach_a_socket_closed_with_a_close_frame_leaves_no_gap_for_the_next() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (mut a, pane) = counting(&probed, &alice).await;
    wait_for_number(&mut a, pane, 40).await;
    a.client.send_close(1000).await;
    assert!(a.client.close_code(Duration::from_secs(5)).await.is_some(), "the server answered the close");
    drop(a);
    detached(&probed, &alice).await;
    let mut b = fresh(&probed, &alice).await;
    page_reconnects(&mut b, &[pane]).await;
    finished_without_a_gap(&mut b, pane).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reattach_two_drops_in_a_row_leave_no_gap() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (mut a, pane) = counting(&probed, &alice).await;
    wait_for_number(&mut a, pane, 40).await;
    drop(a);
    detached(&probed, &alice).await;
    let mut b = fresh(&probed, &alice).await;
    page_reconnects(&mut b, &[pane]).await;
    wait_for_number(&mut b, pane, 120).await;
    drop(b);
    detached(&probed, &alice).await;
    let mut c = fresh(&probed, &alice).await;
    page_reconnects(&mut c, &[pane]).await;
    finished_without_a_gap(&mut c, pane).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reattach_the_pane_counts_on_and_the_dropped_socket_forwards_nothing() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (mut a, pane) = counting(&probed, &alice).await;
    wait_for_number(&mut a, pane, 40).await;
    drop(a);
    detached(&probed, &alice).await;
    let forwarded = probed.ran();
    let workers = probed.env.counts(&alice).workers;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(probed.ran(), forwarded, "a dropped socket forwards no command");
    assert_eq!(probed.env.counts(&alice).workers, workers, "the worker stays");
    assert_eq!(probed.env.counts(&alice).sockets, 0);
    let mut b = fresh(&probed, &alice).await;
    let view = b.ok("get_layout", &json!({})).await;
    assert_eq!(view["panes"].as_array().map(Vec::len), Some(1), "the layout still has its pane");
    let probe_calls = probed.ran_names();
    assert_eq!(probe_calls.iter().filter(|n| *n == "text").count(), 1, "only the program was typed");
    println!("reattach: {forwarded} commands forwarded before the drop and {forwarded} 0.5 s after it; one worker; one pane");
}

#[tokio::test(flavor = "multi_thread")]
async fn reattach_the_new_socket_gets_a_full_frame_first_and_the_live_frames_after_it_in_order() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (mut a, pane) = counting(&probed, &alice).await;
    wait_for_number(&mut a, pane, 40).await;
    drop(a);
    detached(&probed, &alice).await;
    let mut b = fresh(&probed, &alice).await;
    page_reconnects(&mut b, &[pane]).await;
    wait_for_number(&mut b, pane, 80).await;
    let frames = b.frames_of(pane);
    assert_eq!(frames[0]["full"], json!(true), "the first frame is a full one");
    assert!(frames.len() > 5, "live frames followed ({})", frames.len());
    strictly_increasing(&frames).unwrap_or_else(|d| panic!("{d}"));
    let kinds: Vec<&str> = frames.iter().take(6).map(|f| if f["full"] == json!(true) { "full" } else { "partial" }).collect();
    println!("reattach: the frames B saw, by kind: {kinds:?} ... {} frames in all, seq {} to {}", frames.len(), frames[0]["seq"], frames[frames.len() - 1]["seq"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn reattach_the_replay_of_the_title_reaches_the_socket_that_attached_and_no_other() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (mut a, pane) = counting(&probed, &alice).await;
    wait_for_number(&mut a, pane, 20).await;
    assert!(a.until(BOUND, |events| events.iter().any(|e| e["event"] == "title")).await, "the title was set");
    let before = a.events_named("title").len();
    let mut b = fresh(&probed, &alice).await;
    page_reconnects(&mut b, &[pane]).await;
    assert!(
        b.until(BOUND, |events| events.iter().any(|e| e["event"] == "title" && e["payload"]["title"] == TITLE)).await,
        "the socket that attached got the title again"
    );
    a.pump(Duration::from_millis(500)).await;
    assert_eq!(a.events_named("title").len(), before, "the first socket got no second title: the replay is for the new socket only");
}

#[tokio::test(flavor = "multi_thread")]
async fn reattach_a_drop_while_a_large_paste_is_going_in_leaves_the_pane_intact() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut a = fresh(&probed, &alice).await;
    let pane = start_focused_pane(&mut a).await;
    type_into(&mut a, pane, "cat > /dev/null\r").await;
    assert!(until_screen_has(&mut a, pane, "cat > /dev/null", BOUND).await);
    let big = "x".repeat(79) + "\n";
    let text = big.repeat(30_000);
    a.send("paste", &json!({"pane": pane, "text": text, "allow_unsafe": true})).await;
    drop(a);
    detached(&probed, &alice).await;
    let mut b = fresh(&probed, &alice).await;
    page_reconnects(&mut b, &[pane]).await;
    type_into(&mut b, pane, "\u{4}").await;
    type_into(&mut b, pane, "echo hw-intact-$((40+2))\r").await;
    assert!(until_screen_has(&mut b, pane, "hw-intact-42", BOUND).await, "the shell answers after the paste and the drop");
    assert_eq!(probed.env.supervisor.stats().worker_deaths, 0);
}

// ---- two browsers -------------------------------------------------------------------------------------------------------

/// Two sign-ins of one person, both attached to one pane that has a shell. The first is the one that started it.
async fn two_browsers(probed: &Probed, person: &Person) -> (Line, Line, u64) {
    let mut a = fresh(probed, person).await;
    let pane = start_focused_pane(&mut a).await;
    let mut b = fresh(probed, person).await;
    page_reconnects(&mut b, &[pane]).await;
    (a, b, pane)
}

async fn both_show(a: &mut Line, b: &mut Line, pane: u64, needle: &str) {
    assert!(until_screen_has(a, pane, needle, BOUND).await, "the first browser does not show {needle}");
    assert!(until_screen_has(b, pane, needle, BOUND).await, "the second browser does not show {needle}");
    a.pump(Duration::from_millis(300)).await;
    b.pump(Duration::from_millis(300)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn browsers_a_line_typed_through_either_shows_on_both_screens_row_for_row() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (mut a, mut b, pane) = two_browsers(&probed, &alice).await;
    type_into(&mut a, pane, "echo hw-from-a-$((40+2))\r").await;
    both_show(&mut a, &mut b, pane, "hw-from-a-42").await;
    assert_eq!(screen_of(&a, pane).rows, screen_of(&b, pane).rows, "the screens differ after a line typed through the first browser");
    type_into(&mut b, pane, "echo hw-from-b-$((40+2))\r").await;
    both_show(&mut a, &mut b, pane, "hw-from-b-42").await;
    assert_eq!(screen_of(&a, pane).rows, screen_of(&b, pane).rows, "the screens differ after a line typed through the second browser");
    println!("two browsers: after each of the two lines the rebuilt screens were equal ({} rows)", screen_of(&a, pane).rows.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn browsers_an_event_for_all_reaches_both_once_and_a_rename_is_seen_by_the_next_layout() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (mut a, mut b, _first) = two_browsers(&probed, &alice).await;
    let second = split_and_start(&mut a).await;
    // A rename answers the browser that asked; the other one reads it with its next layout.
    let renamed = a.ok("rename_pane", &json!({"pane": second, "name": "shared-name"})).await;
    assert!(renamed.to_string().contains("shared-name"));
    let seen_by_b = b.ok("get_layout", &json!({})).await;
    assert!(seen_by_b.to_string().contains("shared-name"), "the rename is in the layout the other browser reads");
    // The end of a shell is told to every socket: one layout event each.
    let layouts = (a.events_named("layout").len(), b.events_named("layout").len());
    type_into(&mut a, second, "exit\r").await;
    assert!(a.until(BOUND, |e| e.iter().any(|x| x["event"] == "pane-exit")).await);
    assert!(b.until(BOUND, |e| e.iter().any(|x| x["event"] == "pane-exit")).await);
    a.pump(Duration::from_millis(500)).await;
    b.pump(Duration::from_millis(500)).await;
    for (name, line, before) in [("first", &a, layouts.0), ("second", &b, layouts.1)] {
        assert_eq!(line.events_named("pane-exit").len(), 1, "{name}: one pane-exit");
        assert!(line.events_named("layout").len() >= before, "{name}");
    }
    let (exits_a, exits_b) = (a.events_named("pane-exit").len(), b.events_named("pane-exit").len());
    println!("two browsers: the end of a shell reached the first once ({exits_a}) and the second once ({exits_b}); rename_pane sends no event, the other browser reads it with get_layout");
}

#[tokio::test(flavor = "multi_thread")]
async fn browsers_a_third_socket_gets_its_own_replay_and_the_first_two_get_none_of_it() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (mut a, mut b, pane) = two_browsers(&probed, &alice).await;
    type_into(&mut a, pane, "printf '\\033]0;hw-shared-title\\007'\r").await;
    both_show_title(&mut a, &mut b, "hw-shared-title").await;
    let counts = (a.events_named("title").len(), b.events_named("title").len());
    let mut c = fresh(&probed, &alice).await;
    page_reconnects(&mut c, &[pane]).await;
    assert!(
        c.until(BOUND, |events| events.iter().any(|e| e["event"] == "title" && e["payload"]["title"] == "hw-shared-title")).await,
        "the third socket got the replay of the title"
    );
    a.pump(Duration::from_millis(500)).await;
    b.pump(Duration::from_millis(500)).await;
    assert_eq!(a.events_named("title").len(), counts.0, "the first socket got none of the replay");
    assert_eq!(b.events_named("title").len(), counts.1, "the second socket got none of the replay");
}

async fn both_show_title(a: &mut Line, b: &mut Line, title: &str) {
    for line in [a, b] {
        let t = title.to_owned();
        assert!(line.until(BOUND, move |events| events.iter().any(|e| e["event"] == "title" && e["payload"]["title"] == t)).await, "{title} not seen");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn browsers_closing_one_leaves_the_other_receiving_and_the_count_of_sockets_follows() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let (a, mut b, pane) = two_browsers(&probed, &alice).await;
    assert_eq!(probed.env.counts(&alice).sockets, 2);
    drop(a);
    detached_to(&probed, &alice, 1).await;
    type_into(&mut b, pane, "echo hw-after-close-$((40+2))\r").await;
    assert!(until_screen_has(&mut b, pane, "hw-after-close-42", BOUND).await, "the second browser still receives frames");
    assert_eq!(probed.env.counts(&alice).workers, 1);
    let c = fresh(&probed, &alice).await;
    assert_eq!(probed.env.counts(&alice).sockets, 2, "a new socket is counted");
    drop((b, c));
    detached_to(&probed, &alice, 0).await;
    println!("two browsers: sockets counted 2, 1 after one closed, 2 with a third, 0 after both closed");
}

async fn detached_to(probed: &Probed, person: &Person, count: usize) {
    let key = person.key;
    probed.env.eventually("the number of sockets to follow", |env| env.supervisor.account_stats(key).sockets == count).await;
}

// ---- people apart -------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn persons_apart_a_canary_typed_in_one_persons_pane_never_reaches_the_others_socket() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let bob = probed.env.person("bob");
    let canary_a = format!("hw-canary-a-{}", std::process::id());
    let canary_b = format!("hw-canary-b-{}", std::process::id());
    let mut a = fresh(&probed, &alice).await;
    let mut b = fresh(&probed, &bob).await;
    let pane_a = start_focused_pane(&mut a).await;
    let pane_b = start_focused_pane(&mut b).await;
    a.ok("rename_pane", &json!({"pane": pane_a, "name": "alice-only-name"})).await;
    type_into(&mut a, pane_a, &format!("printf '\\033]0;{canary_a}\\007'; echo {canary_a}-$((40+2))\r")).await;
    type_into(&mut b, pane_b, &format!("printf '\\033]0;{canary_b}\\007'; echo {canary_b}-$((40+2))\r")).await;
    assert!(until_screen_has(&mut a, pane_a, &format!("{canary_a}-42"), BOUND).await);
    assert!(until_screen_has(&mut b, pane_b, &format!("{canary_b}-42"), BOUND).await);
    // A third look at everything: layouts, then a pause so that nothing in flight is missed.
    let bobs_layout = b.ok("get_layout", &json!({})).await;
    a.pump(Duration::from_millis(500)).await;
    b.pump(Duration::from_millis(500)).await;
    let heard = |line: &Line| format!("{:?}{:?}", line.events, line.strays);
    let (heard_a, heard_b) = (heard(&a), heard(&b));
    assert!(heard_a.contains(&canary_a), "the canary of alice reached alice (the test can see it)");
    assert!(heard_b.contains(&canary_b), "the canary of bob reached bob");
    assert!(!heard_b.contains(&canary_a), "alice's canary reached bob's socket");
    assert!(!heard_a.contains(&canary_b), "bob's canary reached alice's socket");
    assert!(!bobs_layout.to_string().contains("alice-only-name"), "bob's layout shows alice's pane name");
    println!("persons apart: alice's canary was searched in {} bytes bob's socket received, bob's in {} bytes of alice's: none found", heard_b.len(), heard_a.len());
}

// ---- the pane cap -------------------------------------------------------------------------------------------------------

const CAP: u32 = 4;

fn capped() -> WsOptions {
    WsOptions { supervisor: Some(SupervisorSettings { max_panes: CAP, ..SupervisorSettings::default() }), ..WsOptions::default() }
}

/// The sentence of the host for a cap of `n`.
fn cap_sentence(n: u32) -> String {
    format!("this server allows {n} panes per person; close one to open another")
}

/// Opens (splits and starts) one more pane and answers the pane id, or the refusal.
async fn open_pane(line: &mut Line) -> Result<u64, String> {
    let answer = line.call("split", &json!({"column": false})).await.map_err(|f| format!("{f:?}"))?;
    match answer.get("ok") {
        Some(view) => {
            let pane = ws_env::focus(view);
            parity_env::start_pane_with_size(line, pane).await;
            Ok(pane)
        }
        None => Err(answer["err"].as_str().unwrap_or("?").to_owned()),
    }
}

/// Four panes across two sockets: the first socket holds the first two, the second the next two.
async fn four_panes(probed: &Probed, person: &Person) -> (Line, Line, Vec<u64>) {
    let mut a = fresh(probed, person).await;
    let mut panes = vec![start_focused_pane(&mut a).await];
    panes.push(open_pane(&mut a).await.expect("the second pane"));
    let mut b = fresh(probed, person).await;
    panes.push(open_pane(&mut b).await.expect("the third pane"));
    panes.push(open_pane(&mut b).await.expect("the fourth pane"));
    (a, b, panes)
}

#[tokio::test(flavor = "multi_thread")]
async fn cap_the_fifth_pane_is_refused_on_either_socket_and_the_others_keep_running() {
    let probed = Probed::start(capped()).await;
    let alice = probed.env.person("alice");
    let (mut a, mut b, panes) = four_panes(&probed, &alice).await;
    assert_eq!(panes.len(), CAP as usize);
    let by_a = open_pane(&mut a).await.expect_err("the fifth pane through the first socket");
    let by_b = open_pane(&mut b).await.expect_err("the fifth pane through the second socket");
    assert_eq!(by_a, cap_sentence(CAP));
    assert_eq!(by_b, cap_sentence(CAP));
    for (i, pane) in panes.iter().enumerate() {
        let line = if i < 2 { &mut a } else { &mut b };
        type_into(line, *pane, &format!("echo hw-alive-{i}-$((40+2))\r")).await;
        assert!(until_screen_has(line, *pane, &format!("hw-alive-{i}-42"), BOUND).await, "pane {pane} stopped");
    }
    let total: u64 = a.ok("get_layout", &json!({})).await["workspaces"].as_array().expect("workspaces").iter().filter_map(|w| w["panes"].as_u64()).sum();
    assert_eq!(total, u64::from(CAP), "the layout holds exactly the cap");
    println!("cap: {CAP} panes opened over two sockets; the fifth was answered {by_a:?} on both");
}

#[tokio::test(flavor = "multi_thread")]
async fn cap_closing_one_pane_makes_room_for_another() {
    let probed = Probed::start(capped()).await;
    let alice = probed.env.person("alice");
    let (mut a, mut b, panes) = four_panes(&probed, &alice).await;
    open_pane(&mut a).await.expect_err("full");
    b.ok("close_pane", &json!({"pane": panes[3]})).await;
    let again = open_pane(&mut a).await;
    assert!(again.is_ok(), "a new pane after one was closed: {again:?}");
    assert_eq!(open_pane(&mut b).await.expect_err("full again"), cap_sentence(CAP));
    println!("cap: after close_pane the next pane was accepted ({again:?}) and the one after it refused");
}

#[tokio::test(flavor = "multi_thread")]
async fn cap_the_count_holds_for_a_person_who_reconnects_between_the_openings() {
    let probed = Probed::start(capped()).await;
    let alice = probed.env.person("alice");
    let mut first = fresh(&probed, &alice).await;
    let mut panes = vec![start_focused_pane(&mut first).await];
    for _ in 1..CAP {
        drop(first);
        detached(&probed, &alice).await;
        first = fresh(&probed, &alice).await;
        panes.push(open_pane(&mut first).await.expect("a pane within the cap"));
    }
    drop(first);
    detached(&probed, &alice).await;
    let mut last = fresh(&probed, &alice).await;
    assert_eq!(open_pane(&mut last).await.expect_err("past the cap"), cap_sentence(CAP));
    assert_eq!(panes.len(), CAP as usize);
}

#[tokio::test(flavor = "multi_thread")]
async fn cap_a_second_person_has_a_cap_of_their_own() {
    let probed = Probed::start(capped()).await;
    let alice = probed.env.person("alice");
    let bob = probed.env.person("bob");
    let (mut a, _b, _panes) = four_panes(&probed, &alice).await;
    open_pane(&mut a).await.expect_err("alice is full");
    let mut other = fresh(&probed, &bob).await;
    let mut opened = vec![start_focused_pane(&mut other).await];
    for _ in 1..CAP {
        opened.push(open_pane(&mut other).await.expect("bob's pane within his own cap"));
    }
    assert_eq!(open_pane(&mut other).await.expect_err("bob is full"), cap_sentence(CAP));
}

// ---- the idle time of the worker ----------------------------------------------------------------------------------------

/// The idle time of a worker in these tests, in seconds of the supervisor's clock (one tick is one second).
const IDLE_SECONDS: u64 = 3;

fn idle_options() -> WsOptions {
    WsOptions {
        supervisor: Some(SupervisorSettings { idle_timeout: Duration::from_secs(IDLE_SECONDS), ..SupervisorSettings::default() }),
        ..WsOptions::default()
    }
}

const TICKER: &str = "echo $$ > pid-of-shell.txt; i=0; while true; do i=$((i+1)); echo hw-tick$i; sleep 0.1; done\r";

/// A running shell that prints a growing number and wrote its process id to a file in the home of the person.
async fn ticking(probed: &Probed, person: &Person) -> (u64, u32) {
    let mut line = fresh(probed, person).await;
    let pane = start_focused_pane(&mut line).await;
    type_into(&mut line, pane, &format!("cd {} && {TICKER}", probed.env.launcher.home(person.key.uid).display())).await;
    assert!(until_screen_has(&mut line, pane, "hw-tick3", BOUND).await, "the shell is ticking");
    let pid_file = probed.env.launcher.home(person.key.uid).join("pid-of-shell.txt");
    let pid: u32 = std::fs::read_to_string(&pid_file).expect("the pid file").trim().parse().expect("a process id");
    assert!(process_exists(pid), "the shell exists");
    (pane, pid)
}

/// The highest tick on the screen of a pane.
fn latest_tick(screen: &Screen) -> u64 {
    screen.rows.iter().filter_map(|r| r.trim().strip_prefix("hw-tick")).filter_map(|n| n.parse::<u64>().ok()).max().unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_just_before_the_time_the_panes_are_still_there_and_the_clock_starts_over() {
    let probed = Probed::start(idle_options()).await;
    let alice = probed.env.person("alice");
    let (pane, pid) = ticking(&probed, &alice).await;
    detached(&probed, &alice).await;
    // One tick short of the idle time: the sweep leaves the worker alone and the next socket finds the pane running.
    probed.env.supervisor_clock.advance(IDLE_SECONDS as i64 - 1);
    assert_eq!(probed.env.supervisor.sweep().await, 0, "nothing ended one tick before the time");
    let mut b = fresh(&probed, &alice).await;
    page_reconnects(&mut b, &[pane]).await;
    let view = b.ok("get_layout", &json!({})).await;
    assert_eq!(ws_env::focus(&view), pane, "the same pane");
    assert!(until_screen(&mut b, pane, BOUND, |rows| rows.iter().any(|r| r.trim().starts_with("hw-tick"))).await);
    let first = latest_tick(&screen_of(&b, pane));
    b.pump(Duration::from_millis(600)).await;
    let later = latest_tick(&screen_of(&b, pane));
    assert!(later > first, "the output is still increasing ({first} -> {later})");
    assert!(process_exists(pid));
    // The idle clock starts again when the socket leaves: another wait of one tick short does not end it either.
    drop(b);
    detached(&probed, &alice).await;
    probed.env.supervisor_clock.advance(IDLE_SECONDS as i64 - 1);
    assert_eq!(probed.env.supervisor.sweep().await, 0, "a second wait of one tick short does not end the worker");
    let mut c = fresh(&probed, &alice).await;
    assert_eq!(ws_env::focus(&c.ok("get_layout", &json!({})).await), pane);
    assert!(process_exists(pid));
    println!("idle: attached one tick before {IDLE_SECONDS} s twice; the same pane {pane}, the ticks went {first} -> {later}, the shell {pid} lives");
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_just_after_the_time_the_worker_is_gone_with_its_shell_and_the_layout_is_fresh() {
    let probed = Probed::start(idle_options()).await;
    let alice = probed.env.person("alice");
    let (pane, pid) = ticking(&probed, &alice).await;
    let old_view = {
        let mut line = fresh(&probed, &alice).await;
        line.ok("get_layout", &json!({})).await
    };
    detached(&probed, &alice).await;
    // At the idle time itself and one tick after: the worker is ended.
    probed.env.supervisor_clock.advance(IDLE_SECONDS as i64 + 1);
    assert_eq!(probed.env.supervisor.sweep().await, 1, "the idle worker was ended");
    probed.env.eventually("the shell of the ended worker to be gone", |_| !process_exists(pid)).await;
    assert_eq!(probed.env.counts(&alice).workers, 0, "no worker is left");
    let mut b = fresh(&probed, &alice).await;
    let view = b.ok("get_layout", &json!({})).await;
    assert_eq!(view["panes"].as_array().map(Vec::len), Some(1), "a fresh layout with one pane: {view}");
    assert_eq!(view["tabs"].as_array().map(Vec::len), Some(1));
    assert!(old_view["panes"].as_array().is_some_and(|p| !p.is_empty()), "the old layout had its pane");
    // A new shell can be started in the fresh layout.
    let fresh_pane = ws_env::focus(&view);
    parity_env::start_pane_with_size(&mut b, fresh_pane).await;
    type_into(&mut b, fresh_pane, "echo hw-new-$((40+2))\r").await;
    assert!(until_screen_has(&mut b, fresh_pane, "hw-new-42", BOUND).await, "a new pane starts");
    assert!(
        !screen_of(&b, fresh_pane).rows.iter().any(|r| r.contains("hw-tick")),
        "the screen of the fresh pane shows nothing of the old shell"
    );
    println!("idle: swept one tick after {IDLE_SECONDS} s: the worker ended, shell {pid} gone, the fresh layout has pane {fresh_pane} (the old one had {pane})");
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_exactly_at_the_time_the_worker_is_ended() {
    let probed = Probed::start(idle_options()).await;
    let alice = probed.env.person("alice");
    let (_pane, pid) = ticking(&probed, &alice).await;
    detached(&probed, &alice).await;
    probed.env.supervisor_clock.advance(IDLE_SECONDS as i64);
    assert_eq!(probed.env.supervisor.sweep().await, 1, "the worker ends at the idle time");
    probed.env.eventually("the shell to be gone", |_| !process_exists(pid)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_a_socket_that_stays_attached_keeps_the_worker_far_beyond_the_time() {
    let probed = Probed::start(idle_options()).await;
    let alice = probed.env.person("alice");
    let mut line = fresh(&probed, &alice).await;
    let pane = start_focused_pane(&mut line).await;
    for step in 1..=5 {
        probed.env.supervisor_clock.advance(IDLE_SECONDS as i64 * 10);
        assert_eq!(probed.env.supervisor.sweep().await, 0, "step {step}: an attached worker is never idle");
    }
    type_into(&mut line, pane, "echo hw-still-here-$((40+2))\r").await;
    assert!(until_screen_has(&mut line, pane, "hw-still-here-42", BOUND).await, "the worker still answers");
    assert_eq!(probed.env.counts(&alice).workers, 1);
}
