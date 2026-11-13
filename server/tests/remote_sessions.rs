//! The device socket over a real TLS listener and real pseudo terminals: a device with a token lists, creates, attaches, types
//! into, watches and takes over the sessions of its own person; it reaches nobody else's; its socket ends at once when its token,
//! its account or the remote switch says so; and a client that does not keep up costs only its own attachment.
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
#[path = "support/sweep.rs"]
mod sweep;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::time::Duration;

use device_socket::{Device, env, env_with, input_frame, mint, mint_for, refused, remote, revoke, sync, upgrade, writer_timeout};
use holzwave_core::remote::proto::{Answer, ErrorWord, Event, MAX_ATTACHED, MAX_TEXT, Op, WriterView};
use holzwave_server::supervisor::SupervisorSettings;
use holzwave_server::worker::sessions::SessionLimits;
use holzwave_server::http::ws::SocketSettings;
use holzwave_server::store::AccountState;
use sweep::{Canary, Category, Counts, Sweep};
use ws_env::{Person, WsEnv, WsOptions};

/// How long a test waits for output of a real shell.
const SHELL: Duration = Duration::from_secs(20);

/// A command whose output cannot be mistaken for the echo of the line that was typed: the shell prints the sum, not the sum's
/// text.
const SUM: &str = "echo hw-$((6*7))\r";

// ---------------------------------------------------------------------------------------------------------------------
// The tracer
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_device_creates_a_session_attaches_it_and_types_into_it() {
    let env = env().await;
    let alice = env.person("alice");
    let laptop = mint(&env, &alice, "laptop");
    let mut device = Device::connect(&env, &laptop.token).await;

    // Nothing is running yet.
    assert_eq!(device.ok(Op::List).await, Answer::List(vec![]));

    // A new session of 80 by 24 answers with the session and a writing view: the creator is the writer.
    let Answer::Created { session, writer } = device.ok(Op::New { cols: 80, rows: 24 }).await else { panic!("created") };
    assert_eq!(writer, WriterView::Writing);

    // Leave and attach again: the answer states the grid, the writer view, whether older lines exist and how many bytes of
    // snapshot follow.
    device.ok(Op::Detach { session }).await;
    let Answer::Attached { session: attached, cols, rows, writer, more, snapshot } = device.attach(session, None).await else {
        panic!("attached")
    };
    assert_eq!((attached, cols, rows), (session, 80, 24));
    assert!(!more, "a fresh session has no older lines");
    assert_ne!(writer, WriterView::Writing, "nobody held the lease while the creator was away, so it is free for this attach");
    assert!(snapshot > 0, "the screen of a shell is more than nothing");

    // The snapshot chunks arrive as binary frames, and as many bytes as the answer announced.
    device.until("the whole snapshot", SHELL, |d| d.snapshot.get(&session).is_some_and(|b| b.len() as u32 >= snapshot)).await;
    assert_eq!(device.snapshot[&session].len() as u32, snapshot);

    // Only the writer's input reaches the program: take the right to write, then type.
    device.ok(Op::Takeover { session }).await;
    device.type_into(session, SUM.as_bytes()).await;
    device.until_live(session, "hw-42", SHELL).await;

    // The sequence numbers of one attach count on from the snapshot into the live bytes, with no gap.
    let seqs = &device.seqs[&session];
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
    assert_eq!(seqs[0], 0);
    assert!(device.strange.is_empty(), "{:?}", device.strange);
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_the_upgrade_is_hidden_while_the_switch_is_off_and_refused_without_a_valid_token() {
    let env = env().await;
    let alice = env.person("alice");
    let laptop = mint(&env, &alice, "laptop");

    // With the remote switch off, and with only the sync switch on, the route is a path that is not there, token or not.
    remote(&env, false);
    for with_token in [Some(laptop.token.as_str()), None] {
        let reply = refused(&env, upgrade(with_token)).await;
        assert_eq!((reply.status, reply.json()["code"].as_str()), (404, Some("not_found")), "remote off");
    }
    sync(&env, true);
    let reply = refused(&env, upgrade(Some(&laptop.token))).await;
    assert_eq!(reply.status, 404, "sync on and remote off");
    let unknown = env.plain(edge_tls::Request::get("/ws/nothing-here")).await;
    assert_eq!((reply.status, reply.json()), (unknown.status, unknown.json()), "the same answer as a path that does not exist");
    sync(&env, false);

    // With it on, a request without a token, with a token that is not one, with a cookie only, with another scheme and with a
    // token that ran out is refused 401 and never upgraded.
    remote(&env, true);
    let expired = mint_for(&env, &alice, "old", -1);
    let cookie_only = upgrade(None).cookie(&alice.signed.cookie).header("Origin", edge::ORIGIN);
    let basic = upgrade(None).header("Authorization", &format!("Basic {}", laptop.token));
    let shortened = upgrade(Some("hwd_tooshort"));
    let wrong_prefix = upgrade(Some(&laptop.token.replacen("hwd_", "hwx_", 1)));
    let two = upgrade(Some(&laptop.token)).header("Authorization", &format!("Bearer {}", laptop.token));
    for (what, request) in [
        ("no token", upgrade(None)),
        ("a malformed token", shortened),
        ("a token of the wrong kind", wrong_prefix),
        ("a cookie only", cookie_only),
        ("another scheme", basic),
        ("two headers", two),
        ("an expired token", upgrade(Some(&expired.token))),
    ] {
        let reply = refused(&env, request).await;
        assert_eq!(reply.status, 401, "{what}: {}", reply.text());
    }

    // The valid token still works after all of that.
    let mut device = Device::connect(&env, &laptop.token).await;
    assert_eq!(device.ok(Op::List).await, Answer::List(vec![]));
}

// ---------------------------------------------------------------------------------------------------------------------
// The upgrade, as the rules lay it down
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_request_that_asks_for_no_upgrade_is_refused_before_it_costs_a_socket() {
    let env = env_with(WsOptions::default()).await;
    let alice = env.person("alice");
    let laptop = mint(&env, &alice, "laptop");
    let plain = env
        .plain(edge_tls::Request::get("/ws/device").header("Authorization", &format!("Bearer {}", laptop.token)))
        .await;
    assert_eq!(plain.status, 400, "a request with a good token that asks for no upgrade");
    // The refusals above did not cost the person a socket.
    assert_eq!(env.counts(&alice).sockets, 0);
}

// ---------------------------------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------------------------------

/// The numbers of the lines `hw-n<number>` of a text, in the order they appear.
fn numbers(text: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("hw-n") {
        rest = &rest[at + 4..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse() {
            out.push(n);
        }
    }
    out
}

/// A program that prints a numbered line every 50 milliseconds and never ends.
const COUNTER: &str = "i=1; while true; do echo hw-n$i; i=$((i+1)); sleep 0.05; done\r";

/// A device of `person`, connected.
async fn device_of(env: &WsEnv, person: &Person, name: &str) -> Device {
    let minted = mint(env, person, name);
    Device::connect(env, &minted.token).await
}

/// The writer events a socket has been told for a session, from the first `want` events on, waiting for them.
async fn writer_events(device: &mut Device, session: u32, want: usize) -> Vec<&'static str> {
    device.until("the writer events", SHELL, |d| d.writer_words(session).len() >= want).await;
    device.writer_words(session)
}

// ---------------------------------------------------------------------------------------------------------------------
// A session outlives its sockets
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_session_runs_on_with_no_socket_and_the_next_attach_shows_every_line_once_and_in_order() {
    let env = env().await;
    let alice = env.person("alice");
    let minted = mint(&env, &alice, "laptop");
    let mut first = Device::connect(&env, &minted.token).await;
    let session = first.new_session().await;
    first.attach(session, None).await;
    first.type_into(session, COUNTER.as_bytes()).await;
    first.until_live(session, "hw-n4", SHELL).await;
    let before = *numbers(&first.live_text(session)).last().expect("a number");

    // The socket goes away and nothing is attached for two seconds, while the program counts on.
    first.ws.send_close(1000).await;
    drop(first);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut second = Device::connect(&env, &minted.token).await;
    let Answer::List(listed) = second.ok(Op::List).await else { panic!("a list") };
    assert_eq!(listed.iter().map(|s| s.id).collect::<Vec<_>>(), vec![session], "the session is still there");
    let Answer::Attached { snapshot, .. } = second.attach(session, None).await else { panic!("attached") };
    second.until("the snapshot", SHELL, |d| d.snapshot.get(&session).is_some_and(|b| b.len() as u32 >= snapshot)).await;

    // The snapshot holds the lines printed meanwhile, each once and in order.
    let held = numbers(&second.snapshot_text(session));
    assert!(held.windows(2).all(|w| w[1] == w[0] + 1), "{held:?}");
    let after = *held.last().expect("lines in the snapshot");
    assert!(after >= before + 20, "about forty lines were printed meanwhile: {before} then {after}");
    assert_eq!(held[0], 1, "the tail brings the whole history of a short run");

    // The live lines continue with no gap and no repeat.
    second.until_live(session, &format!("hw-n{}", after + 5), SHELL).await;
    let mut all = held;
    all.extend(numbers(&second.live_text(session)));
    assert!(all.windows(2).all(|w| w[1] == w[0] + 1), "the snapshot and the live bytes join without a gap: {:?}", &all[all.len().saturating_sub(40)..]);
    assert!(second.strange.is_empty(), "{:?}", second.strange);
}

// ---------------------------------------------------------------------------------------------------------------------
// Another person's session cannot be named
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn another_persons_session_answers_not_found_exactly_like_one_that_never_existed() {
    let env = env().await;
    let alice = env.person("alice");
    let bob = env.person("bob");
    let mut a = device_of(&env, &alice, "a").await;
    let mut b = device_of(&env, &bob, "b").await;

    // Alice has three sessions (numbers 1, 2 and 3 of her own worker); Bob has one (his own number 1).
    let mut hers = Vec::new();
    for _ in 0..3 {
        let session = a.new_session().await;
        a.attach(session, None).await;
        hers.push(session);
    }
    let his = b.new_session().await;
    b.attach(his, None).await;

    // Bob sees only his own.
    let Answer::List(listed) = b.ok(Op::List).await else { panic!("a list") };
    assert_eq!(listed.len(), 1, "only his own session is listed");
    let Answer::List(alices) = a.ok(Op::List).await else { panic!("a list") };
    assert_eq!(alices.len(), 3);

    // Numbers that exist only in Alice's worker, and one that exists nowhere, answer alike for every operation.
    let foreign: Vec<u32> = hers.iter().copied().filter(|s| *s != his).collect();
    assert!(!foreign.is_empty());
    let never = 9_999;
    for target in foreign.iter().copied().chain([never]) {
        for op in [
            Op::Attach { session: target, tail: None },
            Op::Attach { session: target, tail: Some(10) },
            Op::Detach { session: target },
            Op::Resize { session: target, cols: 20, rows: 5 },
            Op::Takeover { session: target },
            Op::End { session: target },
            Op::Beat { session: Some(target) },
        ] {
            let answer = b.err(op).await;
            let unknown = b.err(match op {
                Op::Attach { tail, .. } => Op::Attach { session: never, tail },
                Op::Detach { .. } => Op::Detach { session: never },
                Op::Resize { cols, rows, .. } => Op::Resize { session: never, cols, rows },
                Op::Takeover { .. } => Op::Takeover { session: never },
                Op::End { .. } => Op::End { session: never },
                _ => Op::Beat { session: Some(never) },
            })
            .await;
            assert_eq!((answer, unknown), (ErrorWord::NotFound, ErrorWord::NotFound), "{} of {target}", op.name());
        }
        // Input for a session he is not attached to goes nowhere.
        b.type_into(target, b"echo hw-evil$((1+1))\r").await;
    }

    // Nothing of Alice's changed: all three are alive, at their size, and no byte of Bob's reached any of them.
    let Answer::List(after) = a.ok(Op::List).await else { panic!("a list") };
    assert_eq!(after.len(), 3);
    assert!(after.iter().all(|s| (s.cols, s.rows) == (80, 24)), "{after:?}");
    a.type_into(hers[1], b"echo hw-mine$((2+2))\r").await;
    a.until_live(hers[1], "hw-mine4", SHELL).await;
    assert!(!a.live_text(hers[1]).contains("hw-evil2"), "Bob's input did not reach Alice's session");
    // And his own session is untouched by his guesses.
    b.type_into(his, b"echo hw-his$((3+4))\r").await;
    b.until_live(his, "hw-his7", SHELL).await;
}

// ---------------------------------------------------------------------------------------------------------------------
// The writer rules over the wire
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_creator_writes_a_watcher_does_not_and_a_takeover_moves_the_right_and_tells_everyone() {
    let env = env().await;
    let alice = env.person("alice");
    let mut a = device_of(&env, &alice, "a").await;
    let mut b = device_of(&env, &alice, "b").await;
    let session = a.new_session().await;
    a.attach(session, None).await;
    let Answer::Attached { writer, .. } = b.attach(session, None).await else { panic!("attached") };
    assert_eq!(writer, WriterView::Watching, "the second attachment watches");

    // The writer's input reaches the program and both see it; the watcher's input produces nothing.
    b.type_into(session, b"echo hw-watcher$((10+1))\r").await;
    a.type_into(session, b"echo hw-writer$((10+2))\r").await;
    a.until_live(session, "hw-writer12", SHELL).await;
    b.until_live(session, "hw-writer12", SHELL).await;
    assert!(!a.live_text(session).contains("hw-watcher11"), "a watcher's input is dropped");
    assert!(!b.live_text(session).contains("hw-watcher11"));

    // Only the holder may resize: the watcher is refused and nothing changes.
    assert_eq!(b.err(Op::Resize { session, cols: 100, rows: 30 }).await, ErrorWord::Refused);
    let Answer::List(listed) = a.ok(Op::List).await else { panic!("a list") };
    assert_eq!((listed[0].cols, listed[0].rows), (80, 24));

    // The takeover is explicit, and each is told its own view.
    b.ok(Op::Takeover { session }).await;
    assert_eq!(writer_events(&mut a, session, 1).await, ["watching"]);
    assert_eq!(writer_events(&mut b, session, 1).await, ["writing"]);

    // The old holder's input is dropped now, the new holder's reaches the program.
    a.type_into(session, b"echo hw-old$((20+1))\r").await;
    b.type_into(session, b"echo hw-new$((20+2))\r").await;
    a.until_live(session, "hw-new22", SHELL).await;
    assert!(!a.live_text(session).contains("hw-old21"), "the old holder's input is dropped");

    // The new holder resizes, and both are told the size.
    b.ok(Op::Resize { session, cols: 100, rows: 30 }).await;
    for device in [&mut a, &mut b] {
        device.until("the size event", SHELL, |d| d.events.contains(&Event::Size { session, cols: 100, rows: 30 })).await;
    }
    assert!(a.strange.is_empty() && b.strange.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_silent_holder_frees_the_right_which_moves_to_nobody_until_a_takeover_and_a_beat_keeps_it() {
    let env = env_with(WsOptions { supervisor: Some(writer_timeout(500)), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut a = device_of(&env, &alice, "a").await;
    let mut b = device_of(&env, &alice, "b").await;
    let session = a.new_session().await;
    a.attach(session, None).await;
    b.attach(session, None).await;

    // The holder says nothing; the other only beats. Both are told that the right is free, and it does not move to the beater.
    for _ in 0..8 {
        b.ok(Op::Beat { session: None }).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert_eq!(writer_events(&mut a, session, 1).await, ["free"]);
    assert_eq!(writer_events(&mut b, session, 1).await, ["free"]);
    a.type_into(session, b"echo hw-a$((30+1))\r").await;
    b.type_into(session, b"echo hw-b$((30+2))\r").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    b.type_into(session, b"echo hw-ping$((30+3))\r").await;
    assert!(!b.eventually(Duration::from_secs(1), |d| d.live_text(session).contains("hw-b32")).await, "free means nobody types");
    assert!(!a.live_text(session).contains("hw-a31"));

    // A takeover makes the sender the holder again.
    b.ok(Op::Takeover { session }).await;
    b.type_into(session, b"echo hw-back$((30+4))\r").await;
    b.until_live(session, "hw-back34", SHELL).await;

    // A holder that beats inside the time keeps the right for much longer than the time.
    let before = b.writer_words(session).len();
    for _ in 0..10 {
        b.ok(Op::Beat { session: Some(session) }).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert_eq!(b.writer_words(session).len(), before, "no change of view while it beats: {:?}", b.writer_words(session));
    // And stops keeping it when it stops.
    writer_events(&mut b, session, before + 1).await;
    assert_eq!(b.writer_words(session).last(), Some(&"free"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_holder_leaving_by_detach_or_by_closing_its_socket_frees_the_right_at_once() {
    let env = env().await;
    let alice = env.person("alice");
    let mut watcher = device_of(&env, &alice, "w").await;
    let mut holder = device_of(&env, &alice, "h").await;
    let session = holder.new_session().await;
    holder.attach(session, None).await;
    watcher.attach(session, None).await;

    holder.ok(Op::Detach { session }).await;
    assert_eq!(writer_events(&mut watcher, session, 1).await, ["free"]);
    watcher.ok(Op::Takeover { session }).await;
    assert_eq!(writer_events(&mut watcher, session, 2).await, ["free", "writing"]);

    // The same when the holder's socket is closed instead of detached.
    let mut other = device_of(&env, &alice, "o").await;
    other.attach(session, None).await;
    assert_eq!(other.writer_words(session).len(), 0);
    watcher.ws.send_close(1000).await;
    drop(watcher);
    assert_eq!(writer_events(&mut other, session, 1).await, ["free"]);
}

// ---------------------------------------------------------------------------------------------------------------------
// The end of a session
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn end_kills_the_session_and_a_program_that_ends_by_itself_is_told_with_its_status() {
    let env = env().await;
    let alice = env.person("alice");
    let mut a = device_of(&env, &alice, "a").await;
    let mut b = device_of(&env, &alice, "b").await;

    // A program that ends by itself: every attached socket hears `exited` with its status.
    let first = a.new_session().await;
    a.attach(first, None).await;
    b.attach(first, None).await;
    a.type_into(first, b"exit 3\r").await;
    for device in [&mut a, &mut b] {
        device.until("the exit", SHELL, |d| d.events.contains(&Event::Exited { session: first, code: Some(3) })).await;
    }

    // `end` answers once and the sockets hear `exited`.
    let second = a.new_session().await;
    a.attach(second, None).await;
    b.attach(second, None).await;
    let id = a.send(Op::End { session: second }).await;
    assert_eq!(a.answer(id).await, Ok(Answer::Done));
    for device in [&mut a, &mut b] {
        device.until("the end", SHELL, |d| d.events.iter().any(|e| matches!(e, Event::Exited { session, .. } if *session == second))).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.pump(Duration::from_millis(50)).await;
    assert_eq!(a.unclaimed(), 0, "no second answer");
    assert!(a.strange.is_empty() && b.strange.is_empty(), "{:?} {:?}", a.strange, b.strange);
}

// ---------------------------------------------------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_text_over_the_cap_closes_the_socket_and_a_request_that_is_not_valid_is_answered_and_leaves_it_open() {
    let env = env().await;
    let alice = env.person("alice");
    let mut a = device_of(&env, &alice, "a").await;

    // Each of these is answered `bad_request` (the number 0 when the message had none) and the socket stays open.
    for (id, text) in [
        (0, "this is not json".to_owned()),
        (0, r#"{"op":"list"}"#.to_owned()),
        (1001, r#"{"id":1001,"op":"nuke"}"#.to_owned()),
        (1002, r#"{"id":1002,"op":"detach"}"#.to_owned()),
        (1003, r#"{"id":1003,"op":"new","cols":1,"rows":24}"#.to_owned()),
        (1004, r#"{"id":1004,"op":"list","extra":true}"#.to_owned()),
        (1005, r#"{"id":1005,"op":"attach","session":1,"tail":1000001}"#.to_owned()),
    ] {
        a.send_text(&text).await;
        assert_eq!(a.answer(id).await, Err(ErrorWord::BadRequest), "{text}");
    }
    assert_eq!(a.ok(Op::List).await, Answer::List(vec![]), "the socket is open and works");

    // A text over the cap closes it with 1009.
    a.send_text(&format!(r#"{{"id":1,"op":"list","pad":"{}"}}"#, "x".repeat(MAX_TEXT))).await;
    let (code, reason) = a.close_code(SHELL).await.expect("a close");
    assert_eq!((code, reason.as_str()), (1009, "size"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_socket_holds_at_most_the_most_attachments_and_leaving_one_makes_room() {
    let env = env().await;
    let alice = env.person("alice");
    let mut a = device_of(&env, &alice, "a").await;
    let mut made = Vec::new();
    for _ in 0..MAX_ATTACHED {
        made.push(a.new_session().await);
    }
    assert_eq!(a.err(Op::New { cols: 80, rows: 24 }).await, ErrorWord::TooMany, "one more than the socket may hold");
    // A second socket of the person is not held to this socket's count: it may attach to what the first made.
    let mut b = device_of(&env, &alice, "b").await;
    b.attach(made[0], None).await;
    // Leaving one makes room.
    a.ok(Op::Detach { session: made[0] }).await;
    a.new_session().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_whose_program_ended_no_longer_counts_against_the_cap_of_attachments() {
    let env = env().await;
    let alice = env.person("alice");
    let mut a = device_of(&env, &alice, "a").await;
    let mut made = Vec::new();
    for _ in 0..MAX_ATTACHED {
        made.push(a.new_session().await);
    }
    assert_eq!(a.err(Op::New { cols: 80, rows: 24 }).await, ErrorWord::TooMany);
    // The program of one session ends; the client is told and does not detach.
    let ended = made[0];
    a.attach(ended, None).await;
    a.type_into(ended, b"exit\r").await;
    a.until("the end of the program", SHELL, |d| d.events.iter().any(|e| matches!(e, Event::Exited { session, .. } if *session == ended)))
        .await;
    a.new_session().await;
    assert_eq!(a.err(Op::New { cols: 80, rows: 24 }).await, ErrorWord::TooMany, "the cap still holds");
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_sent_under_one_number_cannot_pass_the_cap_of_attachments() {
    let env = env().await;
    let alice = env.person("alice");
    let mut a = device_of(&env, &alice, "a").await;
    // Far more `new` requests than the socket may hold, all under one number and sent before any answer is read.
    const SAME: u64 = 1_000_000;
    let new = format!(r#"{{"id":{SAME},"op":"new","cols":80,"rows":24}}"#);
    for _ in 0..MAX_ATTACHED + 8 {
        a.send_text(&new).await;
    }
    // A request sent after them is answered after them: by then every one of them was answered or refused.
    let Answer::List(listed) = a.ok(Op::List).await else { panic!("a list") };
    assert!(
        listed.len() <= MAX_ATTACHED,
        "{} sessions were made for one socket that may hold {MAX_ATTACHED}",
        listed.len()
    );
    assert!(!listed.is_empty(), "the first request under the number was done");
}

/// Settings of a socket that allows three requests at once and one more a second, and ends after five refusals in a row.
fn rate_limited(settings: &mut SocketSettings) {
    settings.per_second = 1;
    settings.burst = 3;
    settings.refusals = 5;
}

/// Settings of a socket that allows two violations.
fn two_violations(settings: &mut SocketSettings) {
    settings.violations = 2;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_flood_of_requests_is_answered_busy_and_ends_the_socket_with_the_policy_code() {
    let env = env_with(WsOptions { tune: Some(rate_limited), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut a = device_of(&env, &alice, "a").await;
    for _ in 0..40 {
        a.send(Op::List).await;
    }
    let (code, reason) = a.close_code(SHELL).await.expect("a close");
    assert_eq!((code, reason.as_str()), (1008, "policy"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_that_keeps_sending_requests_that_are_not_valid_is_ended_after_its_budget() {
    let env = env_with(WsOptions { tune: Some(two_violations), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut a = device_of(&env, &alice, "a").await;
    a.send_text(r#"{"id":1001,"op":"nuke"}"#).await;
    assert_eq!(a.answer(1001).await, Err(ErrorWord::BadRequest));
    a.send_text(r#"{"id":1002,"op":"nuke"}"#).await;
    assert_eq!(a.answer(1002).await, Err(ErrorWord::BadRequest));
    assert_eq!(a.ok(Op::List).await, Answer::List(vec![]), "two violations are inside the budget");
    a.send_text(r#"{"id":1003,"op":"nuke"}"#).await;
    let (code, reason) = a.close_code(SHELL).await.expect("a close");
    assert_eq!((code, reason.as_str()), (1008, "policy"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_binary_frame_of_another_kind_or_for_a_session_not_attached_is_dropped_without_an_answer() {
    let env = env().await;
    let alice = env.person("alice");
    let mut a = device_of(&env, &alice, "a").await;
    let session = a.new_session().await;
    a.attach(session, None).await;
    a.send_binary(&[0x7f, 0, 0, 0, 1, b'x']).await;
    a.send_binary(&[0x01, 0, 0, 0, 1, b'x']).await;
    a.send_binary(&[0x10]).await;
    a.send_binary(&[]).await;
    a.send_binary(&input_frame(session + 50, b"echo hw-lost$((1+1))\r")).await;
    // Nothing answers a dropped frame, and nothing it carried reaches the program.
    tokio::time::sleep(Duration::from_millis(700)).await;
    while a.pump(Duration::from_millis(50)).await {}
    assert_eq!(a.unclaimed(), 0);
    assert!(a.strange.is_empty(), "{:?}", a.strange);
    assert!(!a.live_text(session).contains("hw-lost2"));
    // The socket is open and still works.
    let Answer::List(listed) = a.ok(Op::List).await else { panic!("a list") };
    assert_eq!(listed.len(), 1);
}

/// Prints `lines` numbered lines at once.
fn burst(lines: u32) -> String {
    format!("i=1; while [ $i -le {lines} ]; do echo hw-n$i; i=$((i+1)); done; echo hw-end$((7*7))\r")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tail_above_the_server_maximum_is_clamped_and_none_uses_the_default_and_the_answer_states_what_was_sent() {
    let limits = SessionLimits { attach_tail_lines: 5, history_max_lines: 20, ..SessionLimits::DEFAULT };
    let env = env_with(WsOptions {
        supervisor: Some(SupervisorSettings { sessions: limits, ..SupervisorSettings::default() }),
        ..WsOptions::default()
    })
    .await;
    let alice = env.person("alice");
    let mut a = device_of(&env, &alice, "a").await;
    let session = a.new_session().await;
    a.attach(session, None).await;
    a.type_into(session, burst(300).as_bytes()).await;
    a.until_live(session, "hw-end49", SHELL).await;

    // The same screen is attached three ways; what each brings is what its answer announced.
    let mut sizes = Vec::new();
    for tail in [None, Some(20), Some(999_999)] {
        a.snapshot.remove(&session);
        let Answer::Attached { snapshot, more, .. } = a.attach(session, tail).await else { panic!("attached") };
        a.until("the whole snapshot", SHELL, |d| d.snapshot.get(&session).is_some_and(|b| b.len() as u32 >= snapshot)).await;
        assert_eq!(a.snapshot[&session].len() as u32, snapshot, "the answer states the bytes that follow, for tail {tail:?}");
        assert!(more, "older lines exist beyond a tail of {tail:?} of 300 lines");
        sizes.push(snapshot);
    }
    assert!(sizes[0] < sizes[1], "the default of 5 lines brings less than 20: {sizes:?}");
    assert_eq!(sizes[1], sizes[2], "a tail above the maximum is clamped to the maximum: {sizes:?}");
}

// ---------------------------------------------------------------------------------------------------------------------
// The end of a token, an account, the switch and the server
// ---------------------------------------------------------------------------------------------------------------------

/// Short looks at the token, so that a test that wants the periodic recheck does not wait for the real one.
fn looks_often(settings: &mut SocketSettings) {
    settings.recheck = Duration::from_millis(150);
}

/// A write may stall for a long time, so that a client that does not read costs a stream and not the socket.
fn patient(settings: &mut SocketSettings) {
    settings.stall = Duration::from_secs(120);
    settings.silence = Duration::from_secs(120);
}

const ONE_SECOND: Duration = Duration::from_secs(1);

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_token_closes_its_socket_with_4401_within_a_second_and_leaves_the_persons_other_devices_alone() {
    let env = env().await;
    let alice = env.person("alice");
    let doomed = mint(&env, &alice, "doomed");
    let kept = mint(&env, &alice, "kept");
    let mut gone = Device::connect(&env, &doomed.token).await;
    let mut stays = Device::connect(&env, &kept.token).await;
    let session = gone.new_session().await;
    gone.attach(session, None).await;

    revoke(&env, &alice, doomed.id).await;
    assert_eq!(gone.close_code(ONE_SECOND).await, Some((4401, "revoked".to_owned())));
    // The server waits a short time for the peer's own close, then lets the connection go.
    gone.ws.send_close(1000).await;
    assert!(gone.ends_within(Duration::from_secs(5)).await);

    // The other device is not touched, and the session the revoked one made is still there for it.
    let Answer::List(listed) = stays.ok(Op::List).await else { panic!("a list") };
    assert_eq!(listed.iter().map(|s| s.id).collect::<Vec<_>>(), vec![session]);
    // The revoked token opens nothing any more.
    let refusal = refused(&env, upgrade(Some(&doomed.token))).await;
    assert_eq!(refusal.status, 401, "{}", refusal.text());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_that_ends_closes_the_sockets_of_its_devices_when_announced_and_when_only_the_look_finds_it() {
    let env = env_with(WsOptions { tune: Some(looks_often), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let bob = env.person("bob");
    let carol = env.person("carol");
    let a = mint(&env, &alice, "a");
    let b = mint(&env, &bob, "b");
    let c = mint(&env, &carol, "c");
    let mut of_alice = Device::connect(&env, &a.token).await;
    let mut of_bob = Device::connect(&env, &b.token).await;
    let mut of_carol = Device::connect(&env, &c.token).await;

    // Disabled, with the announcement the service makes when it ends the person's sessions.
    env.test.set_state("alice", AccountState::Disabled);
    env.test.sessions.end_all(alice.key.id).expect("sessions ended");
    assert_eq!(of_alice.close_code(ONE_SECOND).await, Some((4401, "revoked".to_owned())));

    // Deleted, with no announcement at all: the next look at the token finds that nothing is left of it.
    env.test.store.write(|tx| tx.account_delete(bob.key.id)).expect("the account is deleted");
    assert_eq!(of_bob.close_code(ONE_SECOND).await, Some((4401, "revoked".to_owned())));

    // Another person's socket is untouched by both.
    assert_eq!(of_carol.ok(Op::List).await, Answer::List(vec![]));
    assert!(of_carol.closed.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revocation_nobody_announced_is_found_by_the_periodic_look_at_the_token() {
    let env = env_with(WsOptions { tune: Some(looks_often), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let minted = mint(&env, &alice, "quiet");
    let mut device = Device::connect(&env, &minted.token).await;
    assert_eq!(device.ok(Op::List).await, Answer::List(vec![]));
    // The row goes without a word to the sockets, as when an announcement was missed.
    assert!(env.test.store.write(|tx| tx.device_revoke(alice.key.id, minted.id)).expect("the row goes"));
    assert_eq!(device.close_code(ONE_SECOND).await, Some((4401, "revoked".to_owned())));
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_remote_access_off_closes_every_device_socket_with_1001_and_the_sessions_run_on_until_it_is_back_on() {
    let env = env().await;
    let alice = env.person("alice");
    let bob = env.person("bob");
    let a = mint(&env, &alice, "a");
    let b = mint(&env, &bob, "b");
    let mut first = Device::connect(&env, &a.token).await;
    let mut second = Device::connect(&env, &a.token).await;
    let mut other = Device::connect(&env, &b.token).await;
    let session = first.new_session().await;
    first.attach(session, None).await;
    first.type_into(session, COUNTER.as_bytes()).await;
    first.until_live(session, "hw-n3", SHELL).await;

    remote(&env, false);
    for device in [&mut first, &mut second, &mut other] {
        assert_eq!(device.close_code(ONE_SECOND).await, Some((1001, "going away".to_owned())));
    }
    // While it is off the door is hidden, whatever the token.
    let hidden = refused(&env, upgrade(Some(&a.token))).await;
    assert_eq!(hidden.status, 404, "{}", hidden.text());

    // The program counted on with nobody there, and the same token works again as soon as the switch is on.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    remote(&env, true);
    let mut back = Device::connect(&env, &a.token).await;
    let Answer::List(listed) = back.ok(Op::List).await else { panic!("a list") };
    assert_eq!(listed.iter().map(|s| s.id).collect::<Vec<_>>(), vec![session], "the session survived the switch");
    let Answer::Attached { snapshot, .. } = back.attach(session, None).await else { panic!("attached") };
    back.until("the snapshot", SHELL, |d| d.snapshot.get(&session).is_some_and(|b| b.len() as u32 >= snapshot)).await;
    let held = numbers(&back.snapshot_text(session));
    assert!(held.last().is_some_and(|n| *n > 3), "it kept counting while no socket was there: {held:?}");
}

/// A program that prints `count` bursts of `size` bytes with a pause between them, then a marker. A reader that keeps up
/// drains a burst in the pause; one that does not read gets further and further behind.
fn bursts(count: usize, size: usize) -> String {
    format!(
        "n=0; while [ $n -lt {count} ]; do head -c {size} /dev/zero | tr '\\0' 'q'; sleep 0.25; n=$((n+1)); done; echo hw-end$((7*7))\r"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_large_output_arrives_whole_and_in_order_for_a_reader_that_keeps_up() {
    let env = env().await;
    let alice = env.person("alice");
    let mut a = device_of(&env, &alice, "a").await;
    let session = a.new_session().await;
    a.attach(session, None).await;
    const BYTES: usize = 40 * 100_000;
    a.type_into(session, bursts(40, 100_000).as_bytes()).await;
    a.until("the end of the output", Duration::from_secs(60), |d| d.live_recent_has(session, "hw-end49")).await;
    let q = a.live_text(session).matches('q').count();
    assert!((BYTES..BYTES + 16).contains(&q), "every byte arrived, and the echo of the line: {q}");
    let seqs = &a.seqs[&session];
    assert!(seqs.windows(2).all(|w| w[1] == w[0].wrapping_add(1)), "the chunks came with consecutive numbers");
}

/// The bytes one socket may queue in the test of the slow reader: a quarter of the default, so that the flood is short, and
/// still far more than a burst of the program that a reader which keeps up has to absorb while its task is not running.
const QUEUE: usize = 1024 * 1024;

/// Small queues, and a writer that may be silent for far longer than the test runs: a lease that changes hands in the test of the
/// slow reader is the loss of the stream's doing, never the clock's.
fn small_queues() -> SupervisorSettings {
    SupervisorSettings {
        socket_queue_bytes: QUEUE,
        sessions: SessionLimits { writer_timeout_ms: 600_000, ..SessionLimits::DEFAULT },
        ..SupervisorSettings::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_does_not_read_loses_its_stream_with_a_resync_and_keeps_its_right_to_write_while_the_other_sees_every_byte_and_the_queue_stays_inside_its_bound() {
    let env = env_with(WsOptions { tune: Some(patient), supervisor: Some(small_queues()), ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut slow = device_of(&env, &alice, "slow").await;
    let mut quick = device_of(&env, &alice, "quick").await;
    let session = slow.new_session().await;
    slow.attach(session, None).await;
    quick.attach(session, None).await;
    let lost_before = env.supervisor.stats().lost_streams;

    // Six million bytes in bursts; the slow socket reads none of them.
    const BYTES: usize = 30 * 200_000;
    slow.type_into(session, bursts(30, 200_000).as_bytes()).await;
    let most = std::cell::Cell::new(0usize);
    let key = alice.key;
    let supervisor = env.supervisor.clone();
    let finished = quick
        .eventually(Duration::from_secs(60), |d| {
            most.set(most.get().max(supervisor.account_stats(key).queued_bytes));
            d.live_recent_has(session, "hw-end49")
        })
        .await;
    let seen = quick.live.get(&session).map_or(0, Vec::len);
    assert!(finished, "the flood did not end in time: {seen} bytes seen, events {:?}", quick.events);
    let q = quick.live_text(session).matches('q').count();
    assert!((BYTES..BYTES + 16).contains(&q), "the reader that kept up saw every byte, and only the one echo of the line: {q}");
    assert!(
        most.get() <= 2 * QUEUE,
        "the queues of both sockets stayed inside their bound while one of them did not read: {} bytes",
        most.get()
    );
    assert!(env.supervisor.stats().lost_streams > lost_before, "the slow stream was counted as lost");
    assert!(quick.events.iter().all(|e| !matches!(e, Event::Resync { .. })), "the reader that kept up lost nothing");
    assert_eq!(quick.writer_words(session), Vec::<&str>::new(), "the lease did not move when the writer's stream was lost");

    // The slow one reads at last: its socket is open, it is told that the stream was cut, and attaching again works.
    slow.until("the resync", Duration::from_secs(60), |d| d.events.contains(&Event::Resync { session })).await;
    assert!(slow.closed.is_none() && !slow.ended, "only the stream was lost, not the socket");
    slow.snapshot.remove(&session);
    let Answer::Attached { snapshot, writer, .. } = slow.attach(session, None).await else { panic!("attached") };
    assert_eq!(writer, WriterView::Writing, "losing the stream cost the writer only the stream, not the right to write");
    slow.until("the snapshot", SHELL, |d| d.snapshot.get(&session).is_some_and(|b| b.len() as u32 >= snapshot)).await;
    assert!(slow.snapshot_text(session).contains("hw-end49"), "the new attachment shows where the output stands");
    slow.type_into(session, SUM.as_bytes()).await;
    slow.until_live(session, "hw-42", SHELL).await;
    assert_eq!(quick.writer_words(session), Vec::<&str>::new(), "nobody was told of a change of the writer");
    assert!(slow.strange.is_empty() && quick.strange.is_empty(), "{:?} {:?}", slow.strange, quick.strange);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cap_of_sockets_is_shared_with_the_browser_and_counts_devices() {
    let env = env().await;
    let alice = env.person("alice");
    let mut devices = Vec::new();
    for n in 0..7 {
        devices.push(device_of(&env, &alice, &format!("d{n}")).await);
    }
    let mut page = env.connect_as(&alice.signed).await;
    env.eventually("eight sockets counted", |e| e.counts(&alice).sockets == 8).await;

    // The ninth is refused, whether it is a device or the page.
    let minted = mint(&env, &alice, "ninth");
    let mut ninth = Device::connect(&env, &minted.token).await;
    assert_eq!(ninth.close_code(ONE_SECOND * 5).await, Some((1013, "busy".to_owned())));
    let mut ninth_page = env.connect_as(&alice.signed).await;
    assert_eq!(ninth_page.close_code(ONE_SECOND * 5).await, Some((1013, "busy".to_owned())));
    assert_eq!(env.counts(&alice).sockets, 8, "a refused socket attached nothing");

    // The page leaving makes room for a device; and the other way round.
    page.send_close(1000).await;
    drop(page);
    env.eventually("seven sockets counted", |e| e.counts(&alice).sockets == 7).await;
    let mut eighth = Device::connect(&env, &minted.token).await;
    assert_eq!(eighth.ok(Op::List).await, Answer::List(vec![]));
    env.eventually("eight again", |e| e.counts(&alice).sockets == 8).await;
    devices.pop().expect("a device").ws.send_close(1000).await;
    env.eventually("seven again", |e| e.counts(&alice).sockets == 7).await;
    let mut page_again = env.connect_as(&alice.signed).await;
    assert!(page_again.close_code(Duration::from_millis(500)).await.is_none(), "the page fits again");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_server_stopping_closes_every_device_socket_with_1001_within_a_second_and_every_task_ends() {
    let env = env().await;
    let alice = env.person("alice");
    let bob = env.person("bob");
    let mut sockets = vec![
        device_of(&env, &alice, "a1").await,
        device_of(&env, &alice, "a2").await,
        device_of(&env, &bob, "b1").await,
    ];
    for socket in &mut sockets {
        assert_eq!(socket.ok(Op::List).await, Answer::List(vec![]));
    }
    env.shutdown.trigger();
    for socket in &mut sockets {
        assert_eq!(socket.close_code(ONE_SECOND).await, Some((1001, "going away".to_owned())));
    }
    assert!(env.state.tracker.wait_empty(ONE_SECOND * 2).await, "every socket task has ended");
}

// ---------------------------------------------------------------------------------------------------------------------
// What is never written down
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn no_terminal_byte_and_no_token_reaches_a_log_the_audit_files_or_the_database() {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let hooks = holzwave_server::worker::runtime::TestHooks { log: Some(log.clone()), ..Default::default() };
    let env = env_with(WsOptions { hooks, ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let minted = mint(&env, &alice, "laptop");
    let typed = Canary::new("typed text", Category::Secret);
    let output = Canary::new("printed text", Category::Secret);
    let token = Canary::with_text("device token", Category::Secret, &minted.token);

    let mut device = Device::connect(&env, &minted.token).await;
    let session = device.new_session().await;
    device.attach(session, None).await;
    // The typed text is in the line the program echoes; the printed text exists only as the result of the program.
    let (first, second) = output.text().split_at(12);
    device.type_into(session, format!("# {}\recho {first}\"\"{second}\r", typed.text()).as_bytes()).await;
    device.until_live(session, output.text(), SHELL).await;
    assert!(device.live_text(session).contains(typed.text()));
    // The person ends the session, a switch is turned, the token is revoked, and the revoked token is refused.
    device.ok(Op::End { session }).await;
    revoke(&env, &alice, minted.id).await;
    let refusal = refused(&env, upgrade(Some(&minted.token))).await;
    let closing = format!("{:?}", device.closed);

    let canaries = vec![typed, output, token];
    let sweep = Sweep::new(&canaries);
    let mut counts = Counts::default();
    let server_dir = env.test.audit_dir.parent().expect("the server directory").to_path_buf();
    let mut findings = sweep.scan_tree(&server_dir, &mut counts);
    let lines = log.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    findings.extend(sweep.scan_lines("the log of the worker", &lines, &mut counts));
    findings.extend(sweep.scan_text("the refusal", &refusal.text()));
    findings.extend(sweep.scan_text("the closing message", &closing));
    findings.extend(sweep.scan_text("the strange messages", &device.strange.join("\n")));
    assert!(counts.files >= 2, "the scan read the database and the audit files: {counts:?}");
    assert!(findings.is_empty(), "{:?}", findings.iter().map(sweep::Finding::line).collect::<Vec<_>>());
    // The scan is not blind.
    assert!(!sweep.scan_text("a planted line", &format!("typed {}", canaries[0].text())).is_empty());
}
