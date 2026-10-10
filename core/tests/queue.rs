//! Tests for `PaneQueue`: the byte-bounded queue between a PTY reader thread and the engine thread.
//!
//! They use real threads. Every wait is bounded, so a broken queue fails a test instead of hanging the run.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use common::wait_until;
use holzwave_core::queue::{OUTPUT_LIMIT, PaneQueue, Popped};

const CHUNK: usize = 64 * 1024;
const SHORT: Duration = Duration::from_millis(50);

/// What `pop` returned, with output reduced to its first byte so the assertions read well.
fn pop_all_ready(q: &PaneQueue<u32>) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        match q.pop(SHORT) {
            Popped::Control(c) => out.push(format!("control {c}")),
            Popped::Output(o) => out.push(format!("output {}", String::from_utf8_lossy(&o))),
            Popped::Finished => {
                out.push("finished".into());
                return out;
            }
            Popped::Closed => {
                out.push("closed".into());
                return out;
            }
            Popped::Timeout => {
                out.push("timeout".into());
                return out;
            }
        }
    }
}

#[test]
fn control_pops_before_queued_output_and_keeps_fifo_among_controls() {
    let q = PaneQueue::<u32>::new();
    assert!(q.push_output(b"a".to_vec()));
    assert!(q.push_control(1));
    assert!(q.push_output(b"b".to_vec()));
    assert!(q.push_control(2));
    assert!(q.push_control(3));
    assert_eq!(q.pop(SHORT), Popped::Control(1));
    assert_eq!(q.pop(SHORT), Popped::Control(2));
    assert_eq!(q.pop(SHORT), Popped::Control(3));
    assert_eq!(q.pop(SHORT), Popped::Output(b"a".to_vec()));
    assert_eq!(q.pop(SHORT), Popped::Output(b"b".to_vec()));
    assert_eq!(q.pop(SHORT), Popped::Timeout);
}

#[test]
fn output_keeps_its_order_and_the_byte_count_follows_pushes_and_pops() {
    let q = PaneQueue::<u32>::new();
    assert!(q.push_output(vec![1; 10]));
    assert!(q.push_output(vec![2; 20]));
    assert_eq!(q.queued_bytes(), 30);
    assert_eq!(q.pop(SHORT), Popped::Output(vec![1; 10]));
    assert_eq!(q.queued_bytes(), 20);
    assert_eq!(q.pop(SHORT), Popped::Output(vec![2; 20]));
    assert_eq!(q.queued_bytes(), 0);
    assert_eq!(q.high_water(), 30);
}

#[test]
fn an_empty_chunk_is_accepted_and_not_queued() {
    let q = PaneQueue::<u32>::new();
    assert!(q.push_output(Vec::new()));
    assert_eq!(q.queued_bytes(), 0);
    assert_eq!(q.pop(SHORT), Popped::Timeout);
}

#[test]
fn push_output_blocks_at_the_limit_and_wakes_when_the_consumer_pops() {
    let q = Arc::new(PaneQueue::<u32>::with_limit(100));
    assert!(q.push_output(vec![0; 100]), "the push that reaches the limit is accepted");

    let done = Arc::new(AtomicBool::new(false));
    let producer = {
        let (q, done) = (q.clone(), done.clone());
        thread::spawn(move || {
            let accepted = q.push_output(vec![1; 10]);
            done.store(true, Ordering::SeqCst);
            accepted
        })
    };

    thread::sleep(Duration::from_millis(150));
    assert!(!done.load(Ordering::SeqCst), "the producer must wait while the queue is at the limit");
    assert_eq!(q.queued_bytes(), 100);

    assert_eq!(q.pop(SHORT), Popped::Output(vec![0; 100]));
    assert!(wait_until(Duration::from_secs(2), || done.load(Ordering::SeqCst)), "pop must wake the producer");
    assert!(producer.join().unwrap());
    assert_eq!(q.pop(SHORT), Popped::Output(vec![1; 10]));
}

#[test]
fn a_flood_of_100_mib_never_drives_high_water_past_limit_plus_one_chunk() {
    const TOTAL: usize = 100 << 20;
    let q = Arc::new(PaneQueue::<()>::new());
    let started = Instant::now();

    let producer = {
        let q = q.clone();
        thread::spawn(move || {
            let chunks = TOTAL / CHUNK;
            for i in 0..chunks {
                let mut v = vec![0u8; CHUNK];
                v[..8].copy_from_slice(&(i as u64).to_le_bytes());
                if !q.push_output(v) {
                    return i;
                }
            }
            q.finish();
            chunks
        })
    };

    // The consumer sleeps before its first pop: the producer has to stop at the bound on its own.
    thread::sleep(Duration::from_millis(250));
    assert!(
        q.queued_bytes() <= OUTPUT_LIMIT + CHUNK,
        "the queue grew to {} bytes while nobody consumed",
        q.queued_bytes()
    );

    let mut received = 0usize;
    let mut expected_seq = 0u64;
    loop {
        match q.pop(Duration::from_secs(10)) {
            Popped::Output(v) => {
                let seq = u64::from_le_bytes(v[..8].try_into().unwrap());
                assert_eq!(seq, expected_seq, "chunks must arrive in order");
                expected_seq += 1;
                received += v.len();
            }
            Popped::Finished => break,
            other => panic!("unexpected {other:?} after {received} bytes"),
        }
    }
    assert_eq!(received, TOTAL);
    assert_eq!(producer.join().unwrap(), TOTAL / CHUNK);
    assert!(
        q.high_water() <= OUTPUT_LIMIT + CHUNK,
        "high water {} exceeds the limit {} plus one chunk",
        q.high_water(),
        OUTPUT_LIMIT
    );
    assert!(q.high_water() >= OUTPUT_LIMIT, "the producer never reached the bound: backpressure did not engage");
    assert!(started.elapsed() < Duration::from_secs(30), "the flood took {:?}", started.elapsed());
}

#[test]
fn input_is_never_stuck_behind_a_full_output_queue() {
    let q = Arc::new(PaneQueue::<u32>::with_limit(64));
    assert!(q.push_output(vec![9; 64]));
    let blocked = {
        let q = q.clone();
        thread::spawn(move || q.push_output(vec![9; 64]))
    };
    thread::sleep(Duration::from_millis(100));

    let started = Instant::now();
    assert!(q.push_control(42), "push_control never blocks");
    assert_eq!(q.pop(Duration::from_secs(1)), Popped::Control(42));
    assert!(started.elapsed() < Duration::from_millis(500), "the control message waited behind output");

    // Free the producer so the test ends cleanly.
    assert_eq!(q.pop(SHORT), Popped::Output(vec![9; 64]));
    assert!(blocked.join().unwrap());
}

#[test]
fn close_wakes_a_blocked_producer_and_pop_drains_before_closed() {
    let q = Arc::new(PaneQueue::<u32>::with_limit(10));
    assert!(q.push_output(vec![7; 10]));
    assert!(q.push_control(5));

    let done = Arc::new(AtomicBool::new(false));
    let producer = {
        let (q, done) = (q.clone(), done.clone());
        thread::spawn(move || {
            let accepted = q.push_output(vec![8; 10]);
            done.store(true, Ordering::SeqCst);
            accepted
        })
    };
    thread::sleep(Duration::from_millis(100));
    assert!(!done.load(Ordering::SeqCst), "the producer must be blocked before close");

    q.close();
    assert!(wait_until(Duration::from_secs(2), || done.load(Ordering::SeqCst)), "close must wake the producer");
    assert!(!producer.join().unwrap(), "a producer released by close gets false");

    assert!(!q.push_output(vec![1]), "pushes after close return false");
    assert!(!q.push_control(6), "control pushes after close return false");

    assert_eq!(q.pop(SHORT), Popped::Control(5));
    assert_eq!(q.pop(SHORT), Popped::Output(vec![7; 10]));
    assert_eq!(q.pop(SHORT), Popped::Closed);
    assert_eq!(q.pop(SHORT), Popped::Closed, "Closed repeats");
}

#[test]
fn close_wakes_a_waiting_consumer() {
    let q = Arc::new(PaneQueue::<u32>::new());
    let consumer = {
        let q = q.clone();
        thread::spawn(move || q.pop(Duration::from_secs(10)))
    };
    thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    q.close();
    assert_eq!(consumer.join().unwrap(), Popped::Closed);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn finish_never_discards_queued_output() {
    let q = PaneQueue::<u32>::new();
    assert!(q.push_output(b"a".to_vec()));
    assert!(q.push_output(b"b".to_vec()));
    q.finish();
    assert_eq!(pop_all_ready(&q), ["output a", "output b", "finished"]);
    assert_eq!(q.pop(SHORT), Popped::Finished, "Finished repeats");
    assert_eq!(q.pop(SHORT), Popped::Finished);
}

#[test]
fn control_pushed_around_finish_is_delivered_before_the_next_output_chunk() {
    let q = PaneQueue::<u32>::new();
    assert!(q.push_output(b"a".to_vec()));
    assert!(q.push_control(1));
    assert!(q.push_output(b"b".to_vec()));
    q.finish();
    assert!(q.push_control(2), "control is still accepted after finish");
    assert_eq!(pop_all_ready(&q), ["control 1", "control 2", "output a", "output b", "finished"]);
}

#[test]
fn push_output_after_finish_returns_false() {
    let q = PaneQueue::<u32>::new();
    q.finish();
    assert!(!q.push_output(b"late".to_vec()));
    assert_eq!(q.queued_bytes(), 0);
    assert_eq!(q.pop(SHORT), Popped::Finished);
}

#[test]
fn finish_wakes_a_waiting_consumer() {
    let q = Arc::new(PaneQueue::<u32>::new());
    let consumer = {
        let q = q.clone();
        thread::spawn(move || q.pop(Duration::from_secs(10)))
    };
    thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    q.finish();
    assert_eq!(consumer.join().unwrap(), Popped::Finished);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_reader_that_ends_right_after_its_last_push_loses_nothing() {
    // The shape of a shell that prints and exits at once: many chunks, then EOF.
    for round in 0..20u32 {
        let q = Arc::new(PaneQueue::<u32>::with_limit(256));
        let producer = {
            let q = q.clone();
            thread::spawn(move || {
                for i in 0..500u32 {
                    assert!(q.push_output(i.to_le_bytes().to_vec()));
                }
                q.finish();
            })
        };
        let mut next = 0u32;
        loop {
            match q.pop(Duration::from_secs(5)) {
                Popped::Output(v) => {
                    assert_eq!(u32::from_le_bytes(v.try_into().unwrap()), next, "round {round}");
                    next += 1;
                }
                Popped::Finished => break,
                other => panic!("round {round}: unexpected {other:?} after {next} chunks"),
            }
        }
        assert_eq!(next, 500, "round {round}: output was lost before Finished");
        producer.join().unwrap();
    }
}

#[test]
fn pop_times_out_when_nothing_arrives() {
    let q = PaneQueue::<u32>::new();
    let started = Instant::now();
    assert_eq!(q.pop(Duration::from_millis(80)), Popped::Timeout);
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(70), "returned after only {took:?}");
    assert!(took < Duration::from_secs(2), "took {took:?}");
}

#[test]
fn a_huge_timeout_does_not_overflow() {
    let q = Arc::new(PaneQueue::<u32>::new());
    let consumer = {
        let q = q.clone();
        thread::spawn(move || q.pop(Duration::MAX))
    };
    thread::sleep(Duration::from_millis(50));
    assert!(q.push_control(1));
    assert_eq!(consumer.join().unwrap(), Popped::Control(1));
}
