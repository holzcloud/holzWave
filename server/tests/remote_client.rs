//! The pane of the app against the real server: the hub, the link and the pane of the core talk to the device socket over a real
//! TLS listener (through a relay that stands for the network) and show a real shell on a real pseudo terminal.
//!
//! The tracer tests come first; the filter `tracer` selects exactly them. Everything here needs the `in-process-workers` feature,
//! and the page must have been built (`npm --prefix app run build`) because the listener embeds nothing else.
#![cfg(feature = "in-process-workers")]

#[path = "support/device_socket.rs"]
mod device_socket;
#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/remote_env.rs"]
mod remote_env;
#[path = "support/sweep.rs"]
mod sweep;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::time::Duration;

use device_socket::{Device, env_with, revoke, writer_timeout};
use holzwave_core::conn::{ConnState, RemoteState};
use holzwave_core::remote::proto::{Answer, Op, WriterView};
use holzwave_core::server_link::Fault;
use remote_env::{Remote, blocking, open_pane, screen, until_prompt, until_screen, until_view};
use ws_env::WsOptions;

const SHELL: Duration = Duration::from_secs(20);

/// A command whose output cannot be mistaken for the echo of the line that was typed: the shell prints the sum, not the sum's
/// text.
const SUM: &str = "echo hw-$((6*7))\r";

// ---------------------------------------------------------------------------------------------------------------------
// The tracer
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_pane_shows_the_prompt_of_a_real_session_takes_typed_input_and_detaches_when_it_closes() {
    let remote = Remote::start().await;
    let (hub, laptop, _endpoint) = remote.hub("laptop");
    let (session, writer) = blocking({
        let hub = hub.clone();
        move || hub.new_session(80, 24).expect("a new session")
    })
    .await;
    assert_eq!(writer, WriterView::Writing);

    let (pane, probe) = open_pane(&hub, session, (80, 24));
    // The prompt of the shell, as the snapshot and the live bytes bring it.
    until_prompt(&pane).await;
    assert_eq!(pane.conn_state(), ConnState::Connected);
    let view = pane.remote_view().expect("a view");
    assert_eq!((view.session, view.writer, view.state), (session, WriterView::Writing, RemoteState::None));

    // Typed input reaches the shell and its answer comes back.
    pane.text(SUM.to_owned()).unwrap();
    until_screen(&pane, "the sum", SHELL, |t| t.lines().any(|l| l.trim() == "hw-42")).await;

    // Closing the pane gives the place back and leaves the session running: the server still lists it.
    pane.close();
    assert!(pane.join(Duration::from_secs(10)));
    assert_eq!(probe.exits().len(), 1);
    let mut device = Device::connect(&remote.env, &laptop.token).await;
    device
        .until("the pane to be detached", Duration::from_secs(10), |_| true)
        .await;
    let Answer::List(entries) = device.ok(Op::List).await else { panic!("a list") };
    assert!(entries.iter().any(|e| e.id == session), "the session is still there: {entries:?}");
    // And it can be attached again, now by another socket: what the first pane typed is in its snapshot.
    let Answer::Attached { .. } = device.attach(session, None).await else { panic!("attached") };
    device.until("the snapshot", SHELL, |d| d.snapshot_text(session).contains("hw-42")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_an_unknown_session_and_a_refused_token_end_with_one_sentence_each_and_no_detail() {
    let remote = Remote::start().await;
    let (hub, laptop, endpoint) = remote.hub("laptop");

    // A session the server does not have.
    let (pane, probe) = open_pane(&hub, 9999, (80, 24));
    until_screen(&pane, "the sentence", SHELL, |t| t.contains("That session is not on the server any more.")).await;
    pane.close();
    assert!(pane.join(Duration::from_secs(10)));
    assert_eq!(probe.exits().len(), 1);

    // A token that is not the server's: the person is signed out, and the pane says so and waits.
    endpoint.set_token(Some(&format!("hwd_{}", "a".repeat(43))));
    hub.disconnect();
    let (pane, probe) = open_pane(&hub, 1, (80, 24));
    until_screen(&pane, "the sentence", SHELL, |t| t.contains(Fault::SignedOut.sentence())).await;
    assert_eq!(pane.conn_state(), ConnState::Failed);
    assert!(probe.exits().is_empty(), "the pane stays for a new sign-in");
    let text = screen(&pane);
    assert!(!text.contains("hwd_") && !text.contains("401"), "{text:?}");
    pane.close();
    assert!(pane.join(Duration::from_secs(10)));

    // A token that was revoked while the pane was open ends the socket with the code that says so, and the pane says the same.
    endpoint.set_token(Some(&laptop.token));
    hub.disconnect();
    let (session, _) = blocking({
        let hub = hub.clone();
        move || hub.new_session(80, 24).expect("a new session")
    })
    .await;
    let (pane, _probe) = open_pane(&hub, session, (80, 24));
    until_prompt(&pane).await;
    revoke(&remote.env, &remote.alice, laptop.id).await;
    until_screen(&pane, "the sentence", SHELL, |t| t.contains(Fault::SignedOut.sentence())).await;
    assert_eq!(pane.conn_state(), ConnState::Failed);
    pane.close();
    assert!(pane.join(Duration::from_secs(10)));
}

// ---------------------------------------------------------------------------------------------------------------------
// Reconnect, with a real session that goes on while the network is away
// ---------------------------------------------------------------------------------------------------------------------

/// The numbers of the lines `n <k>` on a screen, in the order they are shown.
fn numbered_lines(text: &str) -> Vec<u32> {
    text.lines().filter_map(|l| l.trim().strip_prefix("n ")).filter_map(|n| n.trim().parse().ok()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cut_of_two_seconds_loses_no_line_repeats_none_and_keeps_the_order() {
    let remote = Remote::start().await;
    let (hub, _laptop, endpoint) = remote.hub("laptop");
    let (session, _) = blocking({
        let hub = hub.clone();
        move || hub.new_session(80, 24).expect("a new session")
    })
    .await;
    let (pane, probe) = open_pane(&hub, session, (80, 24));
    until_prompt(&pane).await;

    // A program that numbers its lines, twenty a second, and goes on whatever the network does.
    pane.text("i=0; while :; do echo n $i; i=$((i+1)); sleep 0.05; done\r".to_owned()).unwrap();
    until_screen(&pane, "line 20", SHELL, |t| numbered_lines(t).last().is_some_and(|n| *n >= 20)).await;
    let before = numbered_lines(&screen(&pane)).last().copied().unwrap();

    // The network is away for two seconds.
    remote.relay.outage(Duration::from_secs(2)).await;
    until_view(&pane, "the connection again", SHELL, |_| pane.conn_state() == ConnState::Connected).await;
    // The program went on: lines that were written while the pane was cut off are there, and after them the live ones.
    until_screen(&pane, "lines after the cut", SHELL, |t| numbered_lines(t).last().is_some_and(|n| *n >= before + 60)).await;

    // Exactly once and in order: every number from the first to the last, each once, increasing by one. (A pane that was
    // attached again holds the tail the server gave it, so the first number is not the first line of the program.)
    let lines = numbered_lines(&screen(&pane));
    assert!(lines.len() > 60, "{} lines", lines.len());
    for pair in lines.windows(2) {
        assert_eq!(pair[1], pair[0] + 1, "a number repeated, was lost or came out of order: {pair:?}");
    }
    assert!(hub.stats().connects >= 2, "the hub made a second connection: {:?}", hub.stats());
    assert!(endpoint.asked() >= 2, "a token was asked for again for every connection");
    assert!(probe.exits().is_empty());

    // The new socket is a new attachment: it has no right to write, because another device may have taken it while the
    // connection was away. The pane says so, and the person takes it again.
    let view = pane.remote_view().unwrap();
    assert_ne!(view.writer, WriterView::Writing, "{view:?}");
    pane.takeover().unwrap();
    until_view(&pane, "writing", SHELL, |v| v.writer == WriterView::Writing).await;
    // The pane goes on afterwards: the program is stopped and the shell answers.
    pane.text("\x03".to_owned()).unwrap();
    pane.text(SUM.to_owned()).unwrap();
    until_screen(&pane, "the sum", SHELL, |t| t.lines().any(|l| l.trim() == "hw-42")).await;
    pane.close();
    assert!(pane.join(Duration::from_secs(10)));
}

// ---------------------------------------------------------------------------------------------------------------------
// Two devices: one writes, one watches and takes over
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_second_device_watches_drops_what_it_types_and_takes_the_right_and_the_size_over() {
    let remote = Remote::start().await;
    let (hub_a, _a, _) = remote.hub("laptop");
    let (hub_b, _b, _) = remote.hub("phone");
    let (session, _) = blocking({
        let hub = hub_a.clone();
        move || hub.new_session(80, 24).expect("a new session")
    })
    .await;
    let (pane_a, _pa) = open_pane(&hub_a, session, (80, 24));
    until_prompt(&pane_a).await;
    // The second device's tile is smaller than the session.
    let (pane_b, _pb) = open_pane(&hub_b, session, (50, 12));
    until_view(&pane_b, "the attach", SHELL, |v| v.state == RemoteState::None && v.cols == 80).await;
    until_prompt(&pane_b).await;
    // It watches: the first device writes. And its engine has the grid of the session, not its tile.
    assert_eq!(pane_b.remote_view().unwrap().writer, WriterView::Watching);
    assert_eq!(pane_a.remote_view().unwrap().writer, WriterView::Writing);

    // What the watcher types is not sent, and the count says it was dropped.
    pane_b.text("echo hw-from-the-watcher\r".to_owned()).unwrap();
    until_view(&pane_b, "the dropped bytes", SHELL, |v| v.dropped_input >= 25).await;
    pane_a.text(SUM.to_owned()).unwrap();
    until_screen(&pane_b, "the sum on the watcher", SHELL, |t| t.lines().any(|l| l.trim() == "hw-42")).await;
    assert!(!screen(&pane_a).contains("hw-from-the-watcher"), "the typed line never reached the shell");

    // The watcher takes the right: it writes now, the other watches, and the session has the size of the new writer's tile.
    pane_b.takeover().unwrap();
    until_view(&pane_b, "writing", SHELL, |v| v.writer == WriterView::Writing).await;
    until_view(&pane_a, "watching", SHELL, |v| v.writer == WriterView::Watching).await;
    until_view(&pane_a, "the new size", SHELL, |v| (v.cols, v.rows) == (50, 12)).await;
    until_view(&pane_b, "the new size", SHELL, |v| (v.cols, v.rows) == (50, 12)).await;
    pane_b.text("echo hw-$((7*7))\r".to_owned()).unwrap();
    until_screen(&pane_a, "the new writer's output", SHELL, |t| t.lines().any(|l| l.trim() == "hw-49")).await;

    pane_a.close();
    pane_b.close();
    assert!(pane_a.join(Duration::from_secs(10)) && pane_b.join(Duration::from_secs(10)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_writer_that_stays_silent_is_free_after_the_timeout_drops_input_until_it_takes_the_right_again() {
    let env = env_with(WsOptions { supervisor: Some(writer_timeout(600)), ..WsOptions::default() }).await;
    let remote = Remote::from(env).await;
    let (hub, _laptop, _) = remote.hub("laptop");
    let (session, _) = blocking({
        let hub = hub.clone();
        move || hub.new_session(80, 24).expect("a new session")
    })
    .await;
    let (pane, _probe) = open_pane(&hub, session, (80, 24));
    until_prompt(&pane).await;
    pane.text("echo hw-$((1+1))\r".to_owned()).unwrap();
    until_screen(&pane, "the answer", SHELL, |t| t.lines().any(|l| l.trim() == "hw-2")).await;

    // Silence: the server gives the right up, and the pane's view follows from the event.
    until_view(&pane, "free", SHELL, |v| v.writer == WriterView::Free).await;
    // Nobody writes: what is typed is dropped, and counted, until the pane takes the right.
    pane.text("echo hw-lost\r".to_owned()).unwrap();
    until_view(&pane, "the dropped bytes", SHELL, |v| v.dropped_input >= 13).await;
    pane.takeover().unwrap();
    until_view(&pane, "writing", SHELL, |v| v.writer == WriterView::Writing).await;
    pane.text(SUM.to_owned()).unwrap();
    until_screen(&pane, "the sum", SHELL, |t| t.lines().any(|l| l.trim() == "hw-42")).await;
    assert!(!screen(&pane).contains("hw-lost\r\n") || !screen(&pane).lines().any(|l| l.trim() == "hw-lost"), "the dropped line never ran");
    pane.close();
    assert!(pane.join(Duration::from_secs(10)));
}
