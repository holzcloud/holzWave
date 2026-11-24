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
use holzwave_core::remote::hub::{Hub, HubOptions};
use holzwave_core::remote::proto::{Answer, Op, WriterView};
use holzwave_core::server_link::Fault;
use holzwave_core::ssh::Reconnect;
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

    // The cut was far inside the time the server keeps the right for its writer: the pane is the writer again with no
    // takeover, and the program is stopped and the shell answers what is typed next.
    let view = pane.remote_view().unwrap();
    assert_eq!(view.writer, WriterView::Writing, "{view:?}");
    pane.text("\x03".to_owned()).unwrap();
    pane.text(SUM.to_owned()).unwrap();
    until_screen(&pane, "the sum", SHELL, |t| t.lines().any(|l| l.trim() == "hw-42")).await;
    pane.close();
    assert!(pane.join(Duration::from_secs(10)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_writer_cut_for_less_than_the_timeout_is_writing_again_with_no_takeover_and_its_next_line_runs_once() {
    let remote = Remote::start().await;
    let (hub, _laptop, _endpoint) = remote.hub("laptop");
    let (session, _) = blocking({
        let hub = hub.clone();
        move || hub.new_session(80, 24).expect("a new session")
    })
    .await;
    let (pane, probe) = open_pane(&hub, session, (80, 24));
    until_prompt(&pane).await;
    // The pane wrote before the cut.
    pane.text("echo hw-$((1+1))\r".to_owned()).unwrap();
    until_screen(&pane, "the first answer", SHELL, |t| t.lines().any(|l| l.trim() == "hw-2")).await;
    assert_eq!(pane.remote_view().unwrap().writer, WriterView::Writing);

    // A program that numbers its lines goes on while the network is away, so that the lines of the attach that follows the cut
    // show the pane is attached again before the view is read.
    pane.text("i=0; while :; do echo n $i; i=$((i+1)); sleep 0.05; done\r".to_owned()).unwrap();
    until_screen(&pane, "line 20", SHELL, |t| numbered_lines(t).last().is_some_and(|n| *n >= 20)).await;
    let before = numbered_lines(&screen(&pane)).last().copied().unwrap();
    remote.relay.outage(Duration::from_secs(2)).await;
    until_screen(&pane, "lines after the cut", SHELL, |t| numbered_lines(t).last().is_some_and(|n| *n >= before + 60)).await;
    assert!(hub.stats().connects >= 2, "the hub made a second connection: {:?}", hub.stats());

    // No takeover was sent: the view says the pane writes, and what it types after that reaches the shell, once.
    let view = pane.remote_view().unwrap();
    assert_eq!((view.writer, view.state, view.not_sent), (WriterView::Writing, RemoteState::None, false), "{view:?}");
    pane.text("\x03".to_owned()).unwrap();
    pane.text(SUM.to_owned()).unwrap();
    until_screen(&pane, "the sum", SHELL, |t| t.lines().any(|l| l.trim() == "hw-42")).await;
    // The shell has run the line once: the sum appears on one line only (the echo of the typed text is another text).
    assert_eq!(screen(&pane).lines().filter(|l| l.trim() == "hw-42").count(), 1);
    assert_eq!(hub.stats().dropped_input_bytes, 0, "nothing the pane typed was dropped");
    assert!(probe.exits().is_empty());
    pane.close();
    assert!(pane.join(Duration::from_secs(10)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_writer_cut_for_longer_than_the_timeout_is_free_and_drops_what_it_types_until_it_takes_the_right() {
    // The server keeps the right of a cut writer for three seconds; the hub beats fast enough that a writer that is not cut never
    // runs out of them, so that only the cut can free the right.
    let env = env_with(WsOptions { supervisor: Some(writer_timeout(3000)), ..WsOptions::default() }).await;
    let remote = Remote::from(env).await;
    let (_minted, endpoint) = remote.device("laptop");
    let options = HubOptions {
        beat: Duration::from_millis(300),
        reconnect: Reconnect { attempts: 60, delay: Duration::from_millis(250) },
        ..HubOptions::default()
    };
    let hub = Hub::new(endpoint.clone(), options);
    let (session, _) = blocking({
        let hub = hub.clone();
        move || hub.new_session(80, 24).expect("a new session")
    })
    .await;
    let (pane, _probe) = open_pane(&hub, session, (80, 24));
    until_prompt(&pane).await;
    pane.text("echo hw-$((1+1))\r".to_owned()).unwrap();
    until_screen(&pane, "the answer", SHELL, |t| t.lines().any(|l| l.trim() == "hw-2")).await;
    // Without a cut the beats keep the right for longer than the server's timeout.
    tokio::time::sleep(Duration::from_millis(4000)).await;
    assert_eq!(pane.remote_view().unwrap().writer, WriterView::Writing, "the beats keep the right");

    // Five seconds away is longer than the three the server keeps the right: it is free when the pane is back.
    remote.relay.outage(Duration::from_secs(5)).await;
    until_view(&pane, "the connection again", SHELL, |v| {
        v.state == RemoteState::None && v.writer == WriterView::Free
    })
    .await;
    assert_eq!(pane.conn_state(), ConnState::Connected);
    pane.text("echo hw-lost\r".to_owned()).unwrap();
    until_view(&pane, "the dropped bytes", SHELL, |v| v.dropped_input >= 13 && v.not_sent).await;
    pane.takeover().unwrap();
    until_view(&pane, "writing", SHELL, |v| v.writer == WriterView::Writing).await;
    pane.text(SUM.to_owned()).unwrap();
    until_screen(&pane, "the sum", SHELL, |t| t.lines().any(|l| l.trim() == "hw-42")).await;
    assert!(!screen(&pane).lines().any(|l| l.trim() == "hw-lost"), "the dropped line never ran");
    pane.close();
    assert!(pane.join(Duration::from_secs(10)));
}

/// How many lines of the screen hold exactly `text` (the echo of a typed command is another text).
fn lines_of(screen: &str, text: &str) -> usize {
    screen.lines().filter(|l| l.trim() == text).count()
}

/// The row of the first line of the screen that holds exactly `text`.
fn row_of(screen: &str, text: &str) -> Option<usize> {
    screen.lines().position(|l| l.trim() == text)
}

#[tokio::test(flavor = "multi_thread")]
async fn lines_typed_during_a_short_cut_reach_the_shell_once_each_and_in_order_after_the_reconnect() {
    let remote = Remote::start().await;
    let (_minted, endpoint) = remote.device("laptop");
    // The hub tries again for as long as the cut lasts, so that only the cut's length decides what happens.
    let options = HubOptions { reconnect: Reconnect { attempts: 60, delay: Duration::from_millis(250) }, ..HubOptions::default() };
    let hub = Hub::new(endpoint.clone(), options);
    let (session, _) = blocking({
        let hub = hub.clone();
        move || hub.new_session(80, 24).expect("a new session")
    })
    .await;
    let (pane, probe) = open_pane(&hub, session, (80, 24));
    until_prompt(&pane).await;
    pane.text("echo hw-before\r".to_owned()).unwrap();
    until_screen(&pane, "the first answer", SHELL, |t| lines_of(t, "hw-before") == 1).await;

    // The network is away: the pane says that it connects again, and what is typed meanwhile is kept by the hub.
    remote.relay.block(true);
    remote.relay.cut();
    until_view(&pane, "the lost connection", SHELL, |_| matches!(pane.conn_state(), ConnState::Reconnecting { .. })).await;
    pane.text("echo hw-during-1\r".to_owned()).unwrap();
    pane.text("echo hw-during-2\r".to_owned()).unwrap();
    // Both lines are in the hub's hands before the network comes back.
    let typed = "echo hw-during-1\r".len() + "echo hw-during-2\r".len();
    let deadline = std::time::Instant::now() + SHELL;
    while hub.stats().kept_input_bytes < typed as u64 {
        assert!(std::time::Instant::now() < deadline, "the hub kept nothing: {:?}", hub.stats());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!pane.remote_view().unwrap().not_sent, "kept keys are not a lost key");
    remote.relay.block(false);

    // The same writer is back inside the time the server keeps its right: each line runs once, after the first, in the order typed.
    until_screen(&pane, "the replayed lines", SHELL, |t| lines_of(t, "hw-during-1") == 1 && lines_of(t, "hw-during-2") == 1).await;
    // Nothing more comes: wait for a line typed afterwards, so that a repeat would have had time to show.
    pane.text("echo hw-after\r".to_owned()).unwrap();
    until_screen(&pane, "the last answer", SHELL, |t| lines_of(t, "hw-after") == 1).await;
    let text = screen(&pane);
    for line in ["hw-before", "hw-during-1", "hw-during-2", "hw-after"] {
        assert_eq!(lines_of(&text, line), 1, "{line} must run exactly once: {text:?}");
    }
    let rows: Vec<usize> = ["hw-before", "hw-during-1", "hw-during-2", "hw-after"].iter().map(|l| row_of(&text, l).unwrap()).collect();
    assert!(rows.windows(2).all(|w| w[0] < w[1]), "the lines ran out of order: {rows:?}");
    let view = pane.remote_view().unwrap();
    assert_eq!((view.writer, view.not_sent), (WriterView::Writing, false), "{view:?}");
    assert!(hub.stats().connects >= 2, "{:?}", hub.stats());
    assert!(probe.exits().is_empty());
    pane.close();
    assert!(pane.join(Duration::from_secs(10)));
}

#[tokio::test(flavor = "multi_thread")]
async fn lines_typed_during_a_cut_longer_than_the_timeout_never_reach_the_shell_and_the_pane_says_not_sent() {
    // The server keeps the right of a cut writer for three seconds; the cut is longer. The hub tries again for as long as it takes.
    let env = env_with(WsOptions { supervisor: Some(writer_timeout(3000)), ..WsOptions::default() }).await;
    let remote = Remote::from(env).await;
    let (_minted, endpoint) = remote.device("laptop");
    let options = HubOptions {
        beat: Duration::from_millis(300),
        reconnect: Reconnect { attempts: 60, delay: Duration::from_millis(250) },
        ..HubOptions::default()
    };
    let hub = Hub::new(endpoint.clone(), options);
    let (session, _) = blocking({
        let hub = hub.clone();
        move || hub.new_session(80, 24).expect("a new session")
    })
    .await;
    let (pane, _probe) = open_pane(&hub, session, (80, 24));
    until_prompt(&pane).await;
    pane.text("echo hw-before\r".to_owned()).unwrap();
    until_screen(&pane, "the first answer", SHELL, |t| lines_of(t, "hw-before") == 1).await;

    remote.relay.block(true);
    remote.relay.cut();
    until_view(&pane, "the lost connection", SHELL, |_| matches!(pane.conn_state(), ConnState::Reconnecting { .. })).await;
    pane.text("echo hw-during-1\r".to_owned()).unwrap();
    let deadline = std::time::Instant::now() + SHELL;
    while hub.stats().kept_input_bytes == 0 {
        assert!(std::time::Instant::now() < deadline, "the hub kept nothing: {:?}", hub.stats());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Longer than the three seconds: the right is free when the pane is back.
    tokio::time::sleep(Duration::from_secs(5)).await;
    remote.relay.block(false);
    until_view(&pane, "the pane free and told", SHELL, |v| v.state == RemoteState::None && v.writer == WriterView::Free && v.not_sent).await;
    assert_eq!(pane.conn_state(), ConnState::Connected);
    assert_eq!(hub.stats().kept_input_bytes, 0, "nothing stays kept after the answer");

    // The line never runs, not even after the pane takes the right: a line typed afterwards runs and shows that the first did not.
    pane.takeover().unwrap();
    until_view(&pane, "writing", SHELL, |v| v.writer == WriterView::Writing).await;
    pane.text(SUM.to_owned()).unwrap();
    until_screen(&pane, "the sum", SHELL, |t| lines_of(t, "hw-42") == 1).await;
    let text = screen(&pane);
    assert_eq!(lines_of(&text, "hw-during-1"), 0, "a kept line must not run in a session the pane no longer wrote in: {text:?}");
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
