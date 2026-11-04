//! The supervisor's client of the spawn helper, driven against a stand-in helper that speaks the helper's own types: the
//! requests and answers, the descriptor that comes with a started worker, the checks on everything the helper sends, and the
//! launcher that the supervisor starts its workers through.
//!
//! See `support/sup_env.rs` for the stand-in.

#[path = "support/scratch.rs"]
mod scratch;
#[path = "support/sup_env.rs"]
mod sup_env;

use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream as StdStream;
use std::time::{Duration, Instant};

use holzwave_server::supervisor::{AttachError, HelperClient, HelperTimeouts, SupervisorSettings};
use holzwave_server::wire::{Ctl, Frame, Note, PROTOCOL_VERSION, read_frame, write_frame};
use holzwave_spawn_helper::protocol::{Answer, ExitedEvent, Mode, Request, Signal};
use sup_env::{FakeHelper, HelperEnv, Next, PATIENCE, account};

/// Short waits, so that a test that waits for a silence takes under a second.
fn quick() -> HelperTimeouts {
    HelperTimeouts { request: Duration::from_millis(400), start: Duration::from_millis(400) }
}

fn client_with(timeouts: HelperTimeouts) -> (HelperClient, FakeHelper) {
    let (fake, end) = FakeHelper::start();
    (HelperClient::from_socket(end, timeouts).expect("the client"), fake)
}

async fn client() -> (HelperClient, FakeHelper) {
    client_with(quick())
}

/// Waits until `condition` holds, or fails after the patience.
async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !condition() {
        assert!(Instant::now() < deadline, "waited {PATIENCE:?} for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A pair of descriptors: one to send and the other to watch. When the sent one is closed everywhere, reading the watched
/// one gives the end of the stream.
fn watched() -> (OwnedFd, StdStream) {
    let (sent, kept) = StdStream::pair().expect("a pair");
    kept.set_read_timeout(Some(Duration::from_millis(50))).expect("a timeout");
    (OwnedFd::from(sent), kept)
}

/// True once the far end of `kept` is closed.
fn closed_at_the_far_end(kept: &mut StdStream) -> bool {
    let mut byte = [0u8; 1];
    matches!(kept.read(&mut byte), Ok(0))
}

async fn wait_closed(what: &str, kept: &mut StdStream) {
    let deadline = Instant::now() + PATIENCE;
    while !closed_at_the_far_end(kept) {
        assert!(Instant::now() < deadline, "{what}: the descriptor stayed open");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ping_is_answered_with_the_number_of_workers() {
    let (client, fake) = client().await;
    assert_eq!(client.ping().await, Ok(0));
    client.spawn(20001, Mode::Serve).await.expect("a worker");
    assert_eq!(client.ping().await, Ok(1));
    assert_eq!(fake.requests(), vec![Request::Ping, Request::Spawn { uid: 20001, mode: Mode::Serve }, Request::Ping]);
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_in_serve_mode_gives_the_process_number_and_one_descriptor_to_a_working_link() {
    let (client, fake) = client().await;
    let spawned = client.spawn(20001, Mode::Serve).await.expect("a worker");
    assert_eq!(spawned.pid, 4242);
    assert_eq!(*spawned.ended.borrow(), None, "the worker runs");
    // The descriptor is the supervisor's end of the link: what is written reaches the worker and its answer comes back.
    let mut link = StdStream::from(spawned.link);
    link.set_read_timeout(Some(PATIENCE)).expect("a timeout");
    write_frame(&mut link, &Frame::Ctl(Ctl::Init { version: PROTOCOL_VERSION, max_panes: 0, max_scrollback_lines: 0, max_image_mib: 64 })).expect("write");
    assert_eq!(read_frame(&mut link).expect("read"), Some(Frame::Note(Note::Ready)));
    assert_eq!(fake.spawns(), vec![(20001, Mode::Serve)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_uid_and_remove_home_are_answered_and_carry_the_uid() {
    let (client, fake) = client().await;
    assert_eq!(client.kill_uid(20003, Signal::Kill).await, Ok(1));
    assert_eq!(client.kill_uid(20003, Signal::Term).await, Ok(1));
    assert_eq!(client.remove_home(20004).await, Ok(()));
    assert_eq!(
        fake.requests(),
        vec![
            Request::KillUid { uid: 20003, signal: Signal::Kill },
            Request::KillUid { uid: 20003, signal: Signal::Term },
            Request::RemoveHome { uid: 20004 },
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_that_gets_no_answer_fails_after_the_timeout_with_a_sentence_and_the_client_goes_on() {
    let (client, fake) = client().await;
    fake.set_silent(true);
    let started = Instant::now();
    let error = client.ping().await.expect_err("no answer");
    assert!(started.elapsed() >= Duration::from_millis(350) && started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(error.sentence(), "the spawn helper did not answer in time");
    assert!(client.is_alive(), "a silence is not a protocol fault");
    fake.set_silent(false);
    assert_eq!(client.ping().await, Ok(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn two_requests_in_flight_get_the_right_answers_when_the_helper_answers_in_the_opposite_order() {
    let (client, fake) = client_with(HelperTimeouts { request: Duration::from_secs(5), start: Duration::from_secs(5) });
    fake.hold_answers(2);
    let (spawned, killed) = tokio::join!(client.spawn(20001, Mode::Serve), client.kill_uid(20002, Signal::Kill));
    let spawned = spawned.expect("the spawn gets its own answer");
    assert_eq!(spawned.pid, 4242, "the spawn answer carries a process number, the kill answer does not");
    assert_eq!(killed, Ok(1), "the kill gets its count");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_with_an_unknown_id_is_dropped_and_its_descriptor_is_closed() {
    let (client, fake) = client().await;
    let (sent, mut kept) = watched();
    fake.send_raw(&Answer::done(987_654).to_bytes(), vec![sent]);
    wait_closed("an answer for no request", &mut kept).await;
    wait_until("the answer to be counted", || client.dropped_answers() == 1).await;
    assert!(client.is_alive(), "an answer nobody waits for is not a fault");
    assert_eq!(client.ping().await, Ok(0), "the client goes on");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spawn_answer_with_two_descriptors_is_an_error_and_every_descriptor_is_closed() {
    let (client, fake) = client().await;
    let (first, mut first_kept) = watched();
    let (second, mut second_kept) = watched();
    fake.override_next(Next::WithFds(vec![first, second]));
    let error = client.spawn(20001, Mode::Serve).await.expect_err("two descriptors");
    assert!(error.sentence().contains("descriptor"), "{error}");
    wait_closed("the first", &mut first_kept).await;
    wait_closed("the second", &mut second_kept).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spawn_answer_that_is_ok_with_no_descriptor_is_an_error() {
    let (client, fake) = client().await;
    fake.override_next(Next::WithFds(Vec::new()));
    let error = client.spawn(20001, Mode::Serve).await.expect_err("no descriptor");
    assert!(error.sentence().contains("descriptor"), "{error}");
    assert!(client.is_alive(), "a missing descriptor is an error of that request, not of the link");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_descriptor_on_an_answer_that_should_have_none_is_closed_and_marks_the_helper_dead() {
    let (client, fake) = client().await;
    let (sent, mut kept) = watched();
    fake.override_next(Next::WithFds(vec![sent]));
    let error = client.ping().await.expect_err("a ping answer with a descriptor");
    assert!(error.sentence().contains("descriptor"), "{error}");
    wait_closed("the descriptor of a ping answer", &mut kept).await;
    wait_until("the helper to be marked dead", || !client.is_alive()).await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn a_datagram_over_4_kib_is_a_fault_and_marks_the_helper_dead() {
    let (client, fake) = client().await;
    fake.send_raw(&vec![b'{'; 5000], Vec::new());
    wait_until("the helper to be marked dead", || !client.is_alive()).await;
    assert_eq!(client.ping().await.expect_err("dead").sentence(), "the spawn helper is gone");
}

#[tokio::test(flavor = "multi_thread")]
async fn bytes_that_are_not_utf8_or_not_json_mark_the_helper_dead() {
    for (what, bytes) in [("not UTF-8", vec![0xffu8, 0xfe, 0x00, 0x80]), ("not JSON", b"hello, supervisor".to_vec()), ("an unknown member", br#"{"id":1,"ok":true,"surprise":1}"#.to_vec())] {
        let (client, fake) = client().await;
        fake.send_raw(&bytes, Vec::new());
        wait_until(what, || !client.is_alive()).await;
        let started = Instant::now();
        let error = client.spawn(20001, Mode::Serve).await.expect_err("dead");
        assert_eq!(error.sentence(), "the spawn helper is gone", "{what}");
        assert!(started.elapsed() < Duration::from_millis(200), "{what}: a call on a dead client fails at once");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exited_event_for_a_known_worker_ends_it_and_one_for_an_unknown_number_is_ignored() {
    let (client, fake) = client().await;
    let spawned = client.spawn(20001, Mode::Serve).await.expect("a worker");
    let mut ended = spawned.ended.clone();
    let mut link = StdStream::from(spawned.link);
    link.set_read_timeout(Some(PATIENCE)).expect("a timeout");

    fake.send_raw(&ExitedEvent::new(31_337, 20001, 1, None).to_bytes(), Vec::new());
    assert_eq!(client.ping().await, Ok(1), "an event for a worker nobody started changes nothing");
    assert!(client.is_alive());
    assert_eq!(*ended.borrow(), None);

    fake.exit(spawned.pid, 3);
    tokio::time::timeout(PATIENCE, ended.wait_for(|status| status.is_some())).await.expect("in time").expect("the channel");
    assert_eq!(*ended.borrow(), Some(3));
    let mut byte = [0u8; 1];
    assert_eq!(link.read(&mut byte).expect("end of link"), 0, "the link of the worker is closed");
}

#[tokio::test(flavor = "multi_thread")]
async fn when_the_helper_closes_its_end_pending_requests_fail_and_later_calls_fail_at_once() {
    let (client, fake) = client_with(HelperTimeouts { request: Duration::from_secs(10), start: Duration::from_secs(10) });
    let spawned = client.spawn(20001, Mode::Serve).await.expect("a worker");
    let mut ended = spawned.ended.clone();
    fake.set_silent(true);
    let pending = tokio::spawn({
        let client = client.clone();
        async move { client.ping().await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    fake.close();
    let started = Instant::now();
    let error = tokio::time::timeout(PATIENCE, pending).await.expect("in time").expect("joined").expect_err("pending fails");
    assert_eq!(error.sentence(), "the spawn helper is gone");
    assert!(started.elapsed() < Duration::from_secs(5), "the pending request did not wait for its timeout");
    assert!(!client.is_alive());
    let again = Instant::now();
    assert_eq!(client.kill_uid(20001, Signal::Kill).await.expect_err("later").sentence(), "the spawn helper is gone");
    assert!(again.elapsed() < Duration::from_millis(200));
    tokio::time::timeout(PATIENCE, ended.wait_for(|status| status.is_some())).await.expect("in time").expect("the channel");
}

#[tokio::test(flavor = "multi_thread")]
async fn at_most_256_requests_wait_at_once_and_the_next_one_fails_at_once_with_a_sentence() {
    let (client, fake) = client_with(HelperTimeouts { request: Duration::from_secs(3), start: Duration::from_secs(3) });
    fake.set_silent(true);
    let mut waiting = Vec::new();
    for _ in 0..256 {
        let client = client.clone();
        waiting.push(tokio::spawn(async move { client.ping().await }));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    let error = client.ping().await.expect_err("the 257th");
    assert_eq!(error.sentence(), "the spawn helper has too many requests waiting");
    assert!(started.elapsed() < Duration::from_millis(200));
    assert!(client.is_alive(), "a full table is not a fault");
    for task in waiting {
        assert_eq!(task.await.expect("joined").expect_err("silence").sentence(), "the spawn helper did not answer in time");
    }
    fake.set_silent(false);
    assert_eq!(client.ping().await, Ok(0), "the table is free again");
}

// ---- the launcher over the client ------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_uid_sent_is_the_accounts_uid_and_the_mode_is_serve_for_an_attach() {
    let env = HelperEnv::start().await;
    let accounts = [account(1, 20001), account(2, 20002), account(3, 20003)];
    let mut clients = Vec::new();
    for key in accounts {
        clients.push(env.client(key).await);
    }
    assert_eq!(env.fake.spawns(), vec![(20001, Mode::Serve), (20002, Mode::Serve), (20003, Mode::Serve)]);
    drop(clients);
    env.supervisor.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wipes_ask_for_their_modes_and_a_failed_exit_is_an_error() {
    let env = HelperEnv::start().await;
    env.supervisor.wipe_secrets(account(1, 20001)).await.expect("the secrets are wiped");
    env.supervisor.wipe_home(account(1, 20001)).await.expect("the home is wiped");
    assert_eq!(env.fake.spawns(), vec![(20001, Mode::WipeSecrets), (20001, Mode::WipeHome)]);
    env.fake.set_wipe_status(1);
    let error = env.supervisor.wipe_secrets(account(2, 20002)).await.expect_err("status 1");
    assert!(error.to_string().contains("did not finish"), "{error}");
    assert_eq!(env.fake.spawns().last(), Some(&(20002, Mode::WipeSecrets)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_that_is_not_ready_in_time_is_killed_by_uid_and_the_attach_fails_with_a_sentence() {
    let settings = SupervisorSettings { ready_timeout: Duration::from_millis(300), ..SupervisorSettings::default() };
    let env = HelperEnv::start_with(settings, quick()).await;
    env.fake.set_no_ready(true);
    let error = env.supervisor.attach(account(1, 20001)).await.err().expect("the attach fails");
    assert_eq!(error, AttachError::Launch("the worker did not become ready".into()));
    assert_eq!(env.fake.kills(), vec![20001], "the uid of the account and no other");
    assert_eq!(env.fake.spawns(), vec![(20001, Mode::Serve)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exit_event_closes_the_sockets_of_that_worker_with_1012_and_the_others_stay() {
    let env = HelperEnv::start().await;
    let mut one = env.client(account(1, 20001)).await;
    let mut two = env.client(account(2, 20002)).await;
    let pids = env.fake.pids();
    assert_eq!(pids.len(), 2);
    env.fake.exit(pids[0], 137);
    assert_eq!(one.close_code(PATIENCE).await, Some(1012));
    assert_eq!(env.supervisor.account_stats(account(2, 20002)).sockets, 1, "the other person is untouched");
    assert!(two.next_message(Duration::from_millis(100)).await.is_none());
    env.supervisor.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn when_the_helper_is_gone_the_stats_say_so_and_a_later_attach_fails_with_the_launch_error() {
    let env = HelperEnv::start().await;
    assert!(env.supervisor.stats().helper_alive);
    let mut client = env.client(account(1, 20001)).await;
    env.fake.close();
    assert_eq!(client.close_code(PATIENCE).await, Some(1012), "the worker is gone with its helper");
    wait_until("the helper to count as gone", || !env.supervisor.stats().helper_alive).await;
    let error = env.supervisor.attach(account(1, 20001)).await.err().expect("no helper, no worker");
    assert_eq!(error, AttachError::Launch("the spawn helper is gone".into()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_user_id_zero_or_outside_the_range_is_refused_in_the_server_and_the_helper_sees_nothing() {
    let (fake, end) = FakeHelper::start();
    let client = HelperClient::from_socket(end, quick()).expect("the client").with_uid_range(20000, 29999);
    for uid in [0u32, 1, 19999, 30000, u32::MAX] {
        let error = client.spawn(uid, Mode::Serve).await.expect_err("outside the range");
        assert!(error.sentence().contains("outside the range"), "{}", error.sentence());
        assert!(!error.may_have_started(), "nothing was started");
        client.kill_uid(uid, Signal::Kill).await.expect_err("kill outside the range");
        client.remove_home(uid).await.expect_err("remove outside the range");
    }
    assert_eq!(fake.requests(), vec![], "no request for a forbidden value reached the helper");
    // The edges of the range are allowed.
    client.spawn(20000, Mode::Serve).await.expect("the first id");
    client.spawn(29999, Mode::Serve).await.expect("the last id");
    assert_eq!(fake.spawns(), vec![(20000, Mode::Serve), (29999, Mode::Serve)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn user_id_zero_is_refused_even_when_the_client_has_no_range() {
    let (client, fake) = client().await;
    client.spawn(0, Mode::Serve).await.expect_err("root");
    assert_eq!(fake.requests(), vec![]);
}
