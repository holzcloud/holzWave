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
#[path = "support/ws_env.rs"]
mod ws_env;

use std::time::Duration;

use device_socket::{Device, env, env_with, mint, mint_for, refused, remote, sync, upgrade};
use holzwave_core::remote::proto::{Answer, Op, WriterView};
use ws_env::WsOptions;

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
