//! The hub of a device (`holzwave_core::remote::hub`) against a scripted server on a loopback socket. No pane and no engine:
//! what is proved is the socket, the table of answers, the subscriptions and the bounds.
//!
//! - the upgrade carries the token and no identity, and every message the client sends is one of the vocabulary;
//! - the calls (`list`, `new_session`, `end`) answer typed values, an answer that never comes is a timeout after the bound,
//!   an error word is a fixed fault, and an answer that breaks the vocabulary is a protocol fault;
//! - an attach delivers the grid, then the snapshot chunks the answer promised in order, then the live chunks in order;
//! - a server that breaks a rule (a snapshot that is too long, a number that is not the next one, a size outside the clamps,
//!   a length that is far too large, a frame with an unknown tag) ends that stream or the connection with a fixed fault,
//!   allocates nothing beyond the bounds and never makes the client panic;
//! - a client that does not read is attached again from a fresh snapshot and its queue never passes its bound;
//! - the beat, the reconnect with a fresh token and the final faults.

#[path = "support/fake_device.rs"]
mod fake_device;

use std::time::{Duration, Instant};

use fake_device::{FakeDevice, Got, PATIENCE};
use holzwave_core::remote::hub::{Cause, Hub, HubError, HubOptions, SubEvent, Subscription};
use holzwave_core::remote::proto::{self, Answer, ErrorWord, Event, MAX_ATTACHED, MAX_CHUNK, MAX_LISTED, Op, Request, SessionEntry, WriterView};
use holzwave_core::server_link::Fault;
use holzwave_core::ssh::Reconnect;

fn hub_with(device: &FakeDevice, tune: impl FnOnce(&mut HubOptions)) -> Hub {
    let mut options = HubOptions::default();
    tune(&mut options);
    Hub::new(device.endpoint.clone(), options)
}

fn plain_hub(device: &FakeDevice) -> Hub {
    hub_with(device, |_| {})
}

/// The events of a subscription until `done` says enough, or the time is up. Fails the test on a timeout.
fn events_until(sub: &Subscription, done: impl Fn(&[SubEvent]) -> bool) -> Vec<SubEvent> {
    let deadline = Instant::now() + PATIENCE;
    let mut seen = Vec::new();
    while !done(&seen) {
        assert!(Instant::now() < deadline, "the events did not come in time, got {seen:?}");
        if let Some(event) = sub.recv(Duration::from_millis(50)) {
            seen.push(event);
        }
    }
    seen
}

/// Everything a subscription has for the next `wait`.
fn events_for(sub: &Subscription, wait: Duration) -> Vec<SubEvent> {
    let deadline = Instant::now() + wait;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        if let Some(event) = sub.recv(Duration::from_millis(10)) {
            seen.push(event);
        }
    }
    seen
}

fn is_lost(events: &[SubEvent]) -> bool {
    events.iter().any(|e| matches!(e, SubEvent::Lost(_)))
}

/// Attaches `session` and plays the server's half of a plain attach: the answer, then `snapshot` as one chunk.
fn attach_plain(device: &FakeDevice, hub: &Hub, session: u32, snapshot: &[u8]) -> (Subscription, fake_device::Conn) {
    let sub = hub.attach(session, None);
    let mut conn = device.accept();
    let request = conn.request();
    assert_eq!(request.op, Op::Attach { session, tail: None });
    conn.attached(request.id, session, 80, 24, WriterView::Writing, false, snapshot.len() as u32);
    if !snapshot.is_empty() {
        conn.snapshot(session, 0, snapshot);
    }
    let first = events_until(&sub, |e| e.len() >= if snapshot.is_empty() { 1 } else { 2 });
    assert!(matches!(first[0], SubEvent::Attached { .. }), "{first:?}");
    (sub, conn)
}

// ---------------------------------------------------------------------------------------------------------------------
// The socket and the calls
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn the_socket_opens_with_the_token_in_the_header_and_no_identity_besides() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let h = hub.clone();
    let connecting = std::thread::spawn(move || h.connect());
    let _conn = device.accept();
    connecting.join().unwrap().expect("connected");
    let seen = device.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/ws/device");
    assert!(!seen[0].has_origin, "a native client sends no Origin");
    let handed = device.endpoint.handed();
    assert_eq!(seen[0].authorization.as_deref(), Some(format!("Bearer {}", handed[0]).as_str()));
    assert_eq!(hub.stats().connects, 1);
}

#[test]
fn list_new_session_and_end_answer_typed_values_and_send_exactly_the_vocabulary() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let (h1, h2, h3) = (hub.clone(), hub.clone(), hub.clone());
    let entry = SessionEntry { id: 4, title: "t".into(), cwd: "/x".into(), cols: 80, rows: 24, started: 5, attached: 1, writer_held: true };

    let call = std::thread::spawn(move || h1.list());
    let mut conn = device.accept();
    let request = conn.request();
    assert_eq!(request.op, Op::List);
    conn.answer(request.id, Answer::List(vec![entry.clone()]));
    assert_eq!(call.join().unwrap(), Ok(vec![entry]));

    let call = std::thread::spawn(move || h2.new_session(100, 30));
    let request = conn.request();
    assert_eq!(request.op, Op::New { cols: 100, rows: 30 });
    conn.answer(request.id, Answer::Created { session: 9, writer: WriterView::Writing });
    assert_eq!(call.join().unwrap(), Ok((9, WriterView::Writing)));

    let call = std::thread::spawn(move || h3.end(9));
    let request = conn.request();
    assert_eq!(request.op, Op::End { session: 9 });
    conn.answer(request.id, Answer::Done);
    assert_eq!(call.join().unwrap(), Ok(()));

    // Nothing else was said: three requests on one connection.
    assert_eq!(hub.stats().connects, 1);
    let more = conn.drain(Duration::from_millis(200));
    assert!(more.is_empty(), "{more:?}");
}

#[test]
fn an_answer_that_never_comes_is_a_timeout_after_the_bound() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, |o| o.call_timeout = Duration::from_millis(300));
    let h = hub.clone();
    let started = Instant::now();
    let call = std::thread::spawn(move || h.list());
    let mut conn = device.accept();
    let _request = conn.request();
    assert_eq!(call.join().unwrap(), Err(HubError::Timeout));
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(300) && waited < Duration::from_secs(5), "{waited:?}");
    // The connection is still good: a later answer to something else works.
    let h = hub.clone();
    let call = std::thread::spawn(move || h.list());
    let request = conn.request();
    conn.answer(request.id, Answer::List(vec![]));
    assert_eq!(call.join().unwrap(), Ok(vec![]));
}

#[test]
fn every_error_word_of_the_server_is_a_fixed_fault_that_echoes_nothing() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let h = hub.clone();
    let first = std::thread::spawn(move || h.list());
    let mut conn = device.accept();
    let request = conn.request();
    conn.error(request.id, ErrorWord::NotFound);
    assert_eq!(first.join().unwrap(), Err(HubError::Word(ErrorWord::NotFound)));
    let mut sentences = std::collections::HashSet::new();
    for word in ErrorWord::ALL {
        let h = hub.clone();
        let call = std::thread::spawn(move || h.end(3));
        let request = conn.request();
        conn.error(request.id, word);
        let error = call.join().unwrap().unwrap_err();
        assert_eq!(error, HubError::Word(word));
        assert!(!error.sentence().contains('_') && error.sentence() != word.word(), "the sentence is not the wire word: {error}");
        assert!(sentences.insert(error.sentence()), "every word has its own sentence");
    }
}

#[test]
fn an_answer_with_a_member_over_its_bound_is_a_protocol_fault() {
    for text in [
        // A list longer than a list may be.
        {
            let entry = r#"{"id":1,"title":"","cwd":"","cols":80,"rows":24,"started":0,"attached":0,"writer_held":false}"#;
            format!(r#"{{"id":1,"ok":{{"sessions":[{}]}}}}"#, vec![entry; MAX_LISTED + 1].join(","))
        },
        // A member the vocabulary does not have.
        r#"{"id":1,"ok":{"sessions":[],"extra":1}}"#.to_owned(),
        // An error word that is not one of the six, with text of the server's own.
        r#"{"id":1,"err":"the server says hello"}"#.to_owned(),
        "not json at all".to_owned(),
    ] {
        let device = FakeDevice::start();
        let hub = plain_hub(&device);
        let h = hub.clone();
        let call = std::thread::spawn(move || h.list());
        let mut conn = device.accept();
        let _request = conn.request();
        conn.text(&text);
        let error = call.join().unwrap().unwrap_err();
        assert_eq!(error, HubError::Fault(Fault::Protocol), "{}", &text[..text.len().min(40)]);
        assert!(!error.to_string().contains("hello"));
        assert_eq!(hub.stats().protocol_faults, 1);
    }
}

#[test]
fn a_refused_upgrade_is_one_fixed_fault_by_its_status_and_is_not_tried_again() {
    for (status, fault) in [(401u16, Fault::SignedOut), (404, Fault::Off), (403, Fault::Refused)] {
        let device = FakeDevice::start();
        let hub = plain_hub(&device);
        let h = hub.clone();
        let call = std::thread::spawn(move || h.list());
        device.refuse(status);
        assert_eq!(call.join().unwrap(), Err(HubError::Fault(fault)));
        // A subscription ends with it, at once, and nobody connects again.
        let sub = hub.attach(1, None);
        device.refuse(status);
        let events = events_until(&sub, is_lost);
        assert_eq!(events.last(), Some(&SubEvent::Lost(HubError::Fault(fault))));
        assert!(device.accept_within(Duration::from_millis(400)).is_none(), "a refused token is never retried");
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Attach, snapshot and live
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn an_attach_delivers_the_grid_then_the_snapshot_chunks_in_order_then_the_live_chunks_in_order() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let sub = hub.attach(7, Some(300));
    let mut conn = device.accept();
    let request = conn.request();
    assert_eq!(request.op, Op::Attach { session: 7, tail: Some(300) });
    // Chunks that arrive before the answer belong to an older stream: they are dropped.
    conn.snapshot(7, 0, b"stale");
    conn.attached(request.id, 7, 100, 30, WriterView::Watching, true, 9);
    conn.snapshot(7, 0, b"abc");
    conn.snapshot(7, 1, b"defghi");
    conn.live(7, 2, b"live-1");
    conn.live(7, 3, b"live-2");
    let events = events_until(&sub, |e| e.len() >= 5);
    assert_eq!(
        events,
        vec![
            SubEvent::Attached { cols: 100, rows: 30, writer: WriterView::Watching, more: true, snapshot: 9, again: false },
            SubEvent::Snapshot(b"abc".to_vec()),
            SubEvent::Snapshot(b"defghi".to_vec()),
            SubEvent::Live(b"live-1".to_vec()),
            SubEvent::Live(b"live-2".to_vec()),
        ]
    );
    assert_eq!(hub.stats().dropped_chunks, 1);
}

#[test]
fn a_snapshot_that_is_longer_than_promised_ends_that_stream_and_the_connection_goes_on() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let (sub, mut conn) = attach_plain(&device, &hub, 3, b"abcd");
    // The answer promised four bytes and all four came: the next snapshot chunk is more than was promised.
    conn.snapshot(3, 1, b"more");
    let events = events_until(&sub, is_lost);
    assert_eq!(events.last(), Some(&SubEvent::Lost(HubError::Fault(Fault::Protocol))));
    // The server is told the stream is gone, and the socket still works.
    let request = conn.request();
    assert_eq!(request.op, Op::Detach { session: 3 });
    let h = hub.clone();
    let call = std::thread::spawn(move || h.list());
    let request = conn.request();
    conn.answer(request.id, Answer::List(vec![]));
    assert_eq!(call.join().unwrap(), Ok(vec![]));
}

#[test]
fn a_sequence_number_that_repeats_goes_backwards_or_skips_ends_the_stream() {
    for bad in [1u32, 0, 5] {
        let device = FakeDevice::start();
        let hub = plain_hub(&device);
        let (sub, mut conn) = attach_plain(&device, &hub, 3, b"snap");
        conn.live(3, 1, b"one");
        conn.live(3, bad, b"bad");
        let events = events_until(&sub, is_lost);
        // The first live chunk had the next number; the second did not, whatever it was.
        let expected_live = bad != 1;
        if expected_live {
            assert!(events.contains(&SubEvent::Live(b"one".to_vec())), "{events:?}");
        }
        assert_eq!(events.last(), Some(&SubEvent::Lost(HubError::Fault(Fault::Protocol))), "{bad}: {events:?}");
        assert!(!events.contains(&SubEvent::Live(b"bad".to_vec())), "a chunk out of order is never delivered");
    }
}

#[test]
fn a_chunk_for_a_session_nobody_attached_is_dropped_and_counted_and_a_frame_with_an_unknown_tag_ends_the_connection() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let (sub, mut conn) = attach_plain(&device, &hub, 3, b"snap");
    conn.live(99, 0, b"nobody");
    conn.live(3, 1, b"still fine");
    let events = events_until(&sub, |e| e.iter().any(|e| matches!(e, SubEvent::Live(_))));
    assert_eq!(events, vec![SubEvent::Live(b"still fine".to_vec())]);
    assert_eq!(hub.stats().dropped_chunks, 1);
    assert_eq!(hub.stats().protocol_faults, 0);
    // A tag that is none of the three.
    let mut frame = proto::encode_chunk(proto::Stream::Live, 3, 2, b"x").unwrap();
    frame[0] = 0x7f;
    conn.raw_binary(&frame);
    let events = events_until(&sub, is_lost);
    assert_eq!(events.last(), Some(&SubEvent::Lost(HubError::Fault(Fault::Protocol))));
    assert_eq!(hub.stats().protocol_faults, 1);
    // A frame too short for its head, and one that is not even a frame.
    for bytes in [&[0x02u8, 0, 0][..], &[][..], &[0x01u8; 3][..]] {
        let device = FakeDevice::start();
        let hub = plain_hub(&device);
        let (sub, mut conn) = attach_plain(&device, &hub, 3, b"snap");
        conn.raw_binary(bytes);
        let events = events_until(&sub, is_lost);
        assert_eq!(events.last(), Some(&SubEvent::Lost(HubError::Fault(Fault::Protocol))), "{bytes:?}");
    }
}

#[test]
fn a_size_outside_the_clamps_or_a_huge_promise_ends_the_attachment_with_the_fixed_sentence_and_no_allocation() {
    // The grid of the answer: nothing, one column, and far beyond a pane; then a snapshot that promises 4 GiB.
    for (cols, rows, snapshot) in [(0u16, 24u16, 0u32), (1, 24, 0), (65535, 24, 0), (80, 0, 0), (80, 65535, 0), (80, 24, u32::MAX), (80, 24, 17 * 1024 * 1024)] {
        let device = FakeDevice::start();
        let hub = plain_hub(&device);
        let sub = hub.attach(5, None);
        let mut conn = device.accept();
        let request = conn.request();
        conn.attached(request.id, 5, cols, rows, WriterView::Writing, false, snapshot);
        let events = events_until(&sub, is_lost);
        assert_eq!(events, vec![SubEvent::Lost(HubError::Fault(Fault::Protocol))], "{cols}x{rows} {snapshot}");
        assert_eq!(
            HubError::Fault(Fault::Protocol).sentence(),
            "The server sent something holzWave does not accept."
        );
        // Nothing was queued for the subscription beyond its one event.
        assert!(hub.stats().queue_high_water <= 256, "{:?}", hub.stats());
        assert_eq!(hub.stats().bytes_delivered, 0);
    }
    // A size event outside the clamps ends the attachment too.
    for (cols, rows) in [(0u16, 24u16), (65535, 24), (80, 0), (80, 1001)] {
        let device = FakeDevice::start();
        let hub = plain_hub(&device);
        let (sub, mut conn) = attach_plain(&device, &hub, 5, b"");
        conn.event(Event::Size { session: 5, cols, rows });
        let events = events_until(&sub, is_lost);
        assert_eq!(events.last(), Some(&SubEvent::Lost(HubError::Fault(Fault::Protocol))), "{cols}x{rows}");
    }
}

#[test]
fn a_text_far_over_its_bound_is_refused_before_it_is_read_and_ends_the_connection() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let (sub, mut conn) = attach_plain(&device, &hub, 3, b"snap");
    // 3 MiB of text: over the message bound of the client, which refuses it on the frame header.
    conn.text_lossy(&"x".repeat(3 * 1024 * 1024));
    let events = events_until(&sub, is_lost);
    assert!(matches!(events.last(), Some(SubEvent::Lost(HubError::Fault(_)))), "{events:?}");
}

#[test]
fn a_reader_that_does_not_keep_up_is_attached_again_and_its_queue_never_passes_its_bound() {
    let device = FakeDevice::start();
    let bound = 64 * 1024;
    let hub = hub_with(&device, |o| o.sub_queue_bytes = bound);
    let (sub, mut conn) = attach_plain(&device, &hub, 3, b"snap");
    // 40 chunks of 32 KiB: far more than the bound, and nobody reads the subscription meanwhile.
    let chunk = vec![b'z'; 32 * 1024];
    for seq in 1..=40 {
        conn.live(3, seq, &chunk);
    }
    // The hub asks for a fresh attach instead of buffering.
    let request = conn.request();
    assert_eq!(request.op, Op::Attach { session: 3, tail: None });
    let stats = hub.stats();
    assert!(stats.overflows >= 1, "{stats:?}");
    assert!(stats.queue_high_water <= bound as u64, "the queue passed its bound: {stats:?}");
    // Reading what is left shows the announcement of the fresh start and no more than the bound of bytes.
    conn.attached(request.id, 3, 80, 24, WriterView::Writing, false, 4);
    conn.snapshot(3, 0, b"snap");
    let events = events_until(&sub, |e| e.iter().any(|e| matches!(e, SubEvent::Attached { again: true, .. })) && e.iter().any(|e| matches!(e, SubEvent::Snapshot(_))));
    assert!(events.contains(&SubEvent::Reattaching(Cause::Resync)));
    let live: usize = events.iter().map(|e| if let SubEvent::Live(b) = e { b.len() } else { 0 }).sum();
    assert!(live <= bound, "{live} bytes of live output held for a bound of {bound}");
}

#[test]
fn a_subscription_that_never_catches_up_is_ended_as_too_slow_after_a_few_fresh_starts() {
    let device = FakeDevice::start();
    let bound = 64 * 1024;
    let hub = hub_with(&device, |o| o.sub_queue_bytes = bound);
    let (sub, mut conn) = attach_plain(&device, &hub, 3, b"");
    let chunk = vec![b'z'; 32 * 1024];
    let mut seq = 0;
    let mut ended = false;
    for _ in 0..12 {
        for _ in 0..6 {
            conn.live(3, seq, &chunk);
            seq += 1;
        }
        let Some(request) = conn.request_within(Duration::from_millis(600)) else {
            ended = true;
            break;
        };
        conn.attached(request.id, 3, 80, 24, WriterView::Writing, false, 0);
        seq = 0;
    }
    assert!(ended, "the hub went on attaching a reader that never reads");
    let events = events_until(&sub, is_lost);
    assert_eq!(events.last(), Some(&SubEvent::Lost(HubError::Slow)));
}

#[test]
fn the_beat_is_sent_while_something_is_attached_and_never_when_nothing_is() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, |o| o.beat = Duration::from_millis(80));
    // A call with no subscription: no beat.
    let h = hub.clone();
    let call = std::thread::spawn(move || h.list());
    let mut conn = device.accept();
    let request = conn.request();
    conn.answer(request.id, Answer::List(vec![]));
    call.join().unwrap().unwrap();
    assert!(conn.drain(Duration::from_millis(400)).is_empty(), "no beat without an attachment");
    assert_eq!(hub.stats().beats, 0);
    // With one: beats for every session of the socket.
    let sub = hub.attach(2, None);
    let request = conn.request();
    conn.attached(request.id, 2, 80, 24, WriterView::Writing, false, 0);
    let got = conn.drain(Duration::from_millis(500));
    let beats = got.iter().filter(|g| matches!(g, Got::Request(Request { op: Op::Beat { session: None }, .. }))).count();
    assert!(beats >= 3, "{got:?}");
    drop(sub);
}

#[test]
fn input_is_sent_while_this_client_writes_and_dropped_before_it_is_sent_when_it_does_not() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let (sub, mut conn) = attach_plain(&device, &hub, 3, b"x");
    let handle = sub.handle();
    handle.input(b"ls\r").unwrap();
    let got = conn.drain(Duration::from_millis(300));
    assert_eq!(got, vec![Got::Input { session: 3, bytes: b"ls\r".to_vec() }]);
    // Another client takes the right: the hub's view follows the server's event, and the next input is dropped unsent.
    conn.event(Event::Writer { session: 3, state: WriterView::Watching });
    let events = events_until(&sub, |e| e.iter().any(|e| matches!(e, SubEvent::Writer(_))));
    assert_eq!(events, vec![SubEvent::Writer(WriterView::Watching)]);
    handle.input(b"rm -rf\r").unwrap();
    assert!(conn.drain(Duration::from_millis(300)).is_empty(), "nothing was sent");
    assert_eq!(hub.stats().dropped_input_bytes, 7);
    // Input over the frame limit is split into frames of at most the limit.
    conn.event(Event::Writer { session: 3, state: WriterView::Writing });
    events_until(&sub, |e| e.iter().any(|e| matches!(e, SubEvent::Writer(WriterView::Writing))));
    let big = vec![b'a'; proto::MAX_INPUT * 2 + 5];
    handle.input(&big).unwrap();
    let got = conn.drain(Duration::from_millis(500));
    let sizes: Vec<usize> = got.iter().map(|g| if let Got::Input { bytes, .. } = g { bytes.len() } else { 0 }).collect();
    assert_eq!(sizes, vec![proto::MAX_INPUT, proto::MAX_INPUT, 5]);
}

#[test]
fn a_session_is_shown_once_per_device_and_a_socket_has_a_most_of_sessions() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let (_first, _conn) = attach_plain(&device, &hub, 3, b"");
    let second = hub.attach(3, None);
    let events = events_until(&second, is_lost);
    assert_eq!(events, vec![SubEvent::Lost(HubError::AlreadyAttached)]);
    let mut subs = Vec::new();
    for session in 100..(100 + MAX_ATTACHED as u32) {
        subs.push(hub.attach(session, None));
    }
    // The first of these fits next to session 3 up to the most; the last do not.
    let last = subs.pop().unwrap();
    let events = events_until(&last, is_lost);
    assert_eq!(events, vec![SubEvent::Lost(HubError::Busy)]);
}

#[test]
fn the_exit_of_a_session_is_told_once_and_the_place_is_given_back() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let (sub, mut conn) = attach_plain(&device, &hub, 3, b"x");
    conn.event(Event::Exited { session: 3, code: Some(7) });
    let events = events_until(&sub, |e| !e.is_empty());
    assert_eq!(events, vec![SubEvent::Exited(Some(7))]);
    let request = conn.request();
    assert_eq!(request.op, Op::Detach { session: 3 });
}

// ---------------------------------------------------------------------------------------------------------------------
// Reconnect
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn a_lost_connection_is_made_again_with_a_fresh_token_and_every_subscription_is_attached_again_with_its_tail() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, |o| o.reconnect = Reconnect { attempts: 5, delay: Duration::from_millis(100) });
    let sub = hub.attach(3, Some(200));
    let mut conn = device.accept();
    let request = conn.request();
    conn.attached(request.id, 3, 80, 24, WriterView::Writing, false, 0);
    events_until(&sub, |e| !e.is_empty());
    conn.cut();
    let events = events_until(&sub, |e| e.iter().any(|e| matches!(e, SubEvent::Reconnecting { .. })));
    assert_eq!(events, vec![SubEvent::Reconnecting { attempt: 1, of: 5, in_secs: 0 }]);
    let mut conn = device.accept();
    let request = conn.request();
    assert_eq!(request.op, Op::Attach { session: 3, tail: Some(200) }, "the tail the last attach used");
    conn.attached(request.id, 3, 90, 25, WriterView::Free, false, 0);
    let events = events_until(&sub, |e| !e.is_empty());
    assert_eq!(events, vec![SubEvent::Attached { cols: 90, rows: 25, writer: WriterView::Free, more: false, snapshot: 0, again: true }]);
    // The second connection carried another token than the first.
    let seen = device.seen();
    assert_eq!(seen.len(), 2);
    assert_ne!(seen[0].authorization, seen[1].authorization);
    assert_eq!(device.endpoint.handed().len(), 2);
}

#[test]
fn the_state_says_five_tries_three_seconds_apart_and_then_gives_up_and_waits_for_a_retry() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let sub = hub.attach(3, None);
    let mut conn = device.accept();
    let request = conn.request();
    conn.attached(request.id, 3, 80, 24, WriterView::Writing, false, 0);
    events_until(&sub, |e| !e.is_empty());
    conn.cut();
    // The defaults are the numbers of an SSH pane: five tries, three seconds apart.
    let events = events_until(&sub, |e| !e.is_empty());
    assert_eq!(events, vec![SubEvent::Reconnecting { attempt: 1, of: 5, in_secs: 3 }]);
    drop(sub);

    // With short delays the tries run out, and then one retry is one more try.
    let device = FakeDevice::start();
    let hub = hub_with(&device, |o| o.reconnect = Reconnect { attempts: 3, delay: Duration::from_millis(30) });
    let sub = hub.attach(3, None);
    let mut conn = device.accept();
    let request = conn.request();
    conn.attached(request.id, 3, 80, 24, WriterView::Writing, false, 0);
    events_until(&sub, |e| !e.is_empty());
    conn.cut();
    // The server is there and does not speak: every try fails the upgrade.
    device.drop_connections_for(Duration::from_millis(600));
    let events = events_until(&sub, |e| e.contains(&SubEvent::GaveUp));
    let tries: Vec<_> = events.iter().filter(|e| matches!(e, SubEvent::Reconnecting { .. })).collect();
    assert_eq!(tries.len(), 3, "{events:?}");
    assert_eq!(events.last(), Some(&SubEvent::GaveUp));
    // Nothing more happens by itself.
    assert!(device.accept_within(Duration::from_millis(300)).is_none());
    // One retry is one fresh try: it connects, and the stream is attached again.
    sub.handle().retry().unwrap();
    let mut conn = device.accept();
    let request = conn.request();
    assert_eq!(request.op, Op::Attach { session: 3, tail: None });
    conn.attached(request.id, 3, 80, 24, WriterView::Writing, false, 0);
    let events = events_until(&sub, |e| !e.is_empty());
    assert!(matches!(events[0], SubEvent::Attached { again: true, .. }), "{events:?}");
}

#[test]
fn a_close_code_of_a_revoked_token_is_final_and_is_not_tried_again_but_a_busy_server_is_tried_again() {
    let device = FakeDevice::start();
    let hub = hub_with(&device, |o| o.reconnect = Reconnect { attempts: 3, delay: Duration::from_millis(30) });
    let (sub, conn) = attach_plain(&device, &hub, 3, b"");
    conn.close(proto::CLOSE_REVOKED);
    let events = events_until(&sub, is_lost);
    assert_eq!(events.last(), Some(&SubEvent::Lost(HubError::Fault(Fault::SignedOut))));
    assert!(device.accept_within(Duration::from_millis(400)).is_none(), "4401 is never retried");

    let device = FakeDevice::start();
    let hub = hub_with(&device, |o| o.reconnect = Reconnect { attempts: 3, delay: Duration::from_millis(30) });
    let (sub, conn) = attach_plain(&device, &hub, 3, b"");
    conn.close(proto::CLOSE_BUSY);
    let events = events_until(&sub, |e| !e.is_empty());
    assert!(matches!(events[0], SubEvent::Reconnecting { attempt: 1, .. }), "{events:?}");
    let mut conn = device.accept();
    let request = conn.request();
    assert_eq!(request.op, Op::Attach { session: 3, tail: None });
}

#[test]
fn disconnect_ends_every_subscription_as_signed_out_and_a_new_attach_connects_again() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let (sub, mut conn) = attach_plain(&device, &hub, 3, b"");
    hub.disconnect();
    let events = events_until(&sub, is_lost);
    assert_eq!(events.last(), Some(&SubEvent::Lost(HubError::Fault(Fault::SignedOut))));
    // The session was not ended: the client only left.
    let got = conn.drain(Duration::from_millis(300));
    assert!(!got.iter().any(|g| matches!(g, Got::Request(Request { op: Op::End { .. }, .. }))), "{got:?}");
    let again = hub.attach(3, None);
    let mut conn = device.accept();
    let request = conn.request();
    assert_eq!(request.op, Op::Attach { session: 3, tail: None });
    conn.attached(request.id, 3, 80, 24, WriterView::Writing, false, 0);
    let events = events_until(&again, |e| !e.is_empty());
    assert!(matches!(events[0], SubEvent::Attached { again: false, .. }));
    assert_eq!(device.endpoint.handed().len(), 2, "a fresh token for the new connection");
}

#[test]
fn a_signed_out_endpoint_is_a_final_fault_without_any_connection() {
    let device = FakeDevice::start();
    device.endpoint.sign_out(true);
    let hub = plain_hub(&device);
    let sub = hub.attach(3, None);
    let events = events_until(&sub, is_lost);
    assert_eq!(events, vec![SubEvent::Lost(HubError::Fault(Fault::SignedOut))]);
    assert!(device.accept_within(Duration::from_millis(200)).is_none());
    assert_eq!(hub.list(), Err(HubError::Fault(Fault::SignedOut)));
}

#[test]
fn nothing_that_a_hub_prints_or_says_holds_a_token() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let (sub, mut conn) = attach_plain(&device, &hub, 3, b"");
    let token = device.endpoint.handed()[0].clone();
    conn.cut();
    let mut everything = format!("{hub:?} {:?}", sub.handle());
    for event in events_for(&sub, Duration::from_millis(300)) {
        everything.push_str(&format!("{event:?}"));
    }
    for error in [
        HubError::Timeout,
        HubError::Busy,
        HubError::Slow,
        HubError::Closed,
        HubError::Disconnected,
        HubError::AlreadyAttached,
        HubError::Fault(Fault::SignedOut),
    ] {
        everything.push_str(error.sentence());
    }
    assert!(!everything.contains(&token[4..]), "{everything}");
    assert!(!everything.contains("hwd_"), "{everything}");
}

#[test]
fn a_chunk_is_never_more_than_the_vocabulary_allows_and_the_stats_count_what_was_delivered() {
    let device = FakeDevice::start();
    let hub = plain_hub(&device);
    let sub = hub.attach(3, None);
    let mut conn = device.accept();
    let request = conn.request();
    let total = MAX_CHUNK as u32 * 2;
    conn.attached(request.id, 3, 80, 24, WriterView::Writing, false, total);
    let big = vec![b'q'; MAX_CHUNK];
    conn.snapshot(3, 0, &big);
    conn.snapshot(3, 1, &big);
    let events = events_until(&sub, |e| e.len() >= 3);
    let bytes: usize = events.iter().map(|e| if let SubEvent::Snapshot(b) = e { b.len() } else { 0 }).sum();
    assert_eq!(bytes, total as usize);
    assert_eq!(hub.stats().bytes_delivered, u64::from(total));
}
