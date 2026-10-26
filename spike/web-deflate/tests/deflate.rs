//! The candidate (yawc with permessage-deflate) on the same corpus as the baseline, the compression bombs, and the
//! rules of the verdict.
//!
//! The tests share one process-wide allocator that records its peak, so they run one at a time (`SERIAL`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use web_deflate::candidate::{options, serve_candidate};
use web_deflate::raw::{Client, Outcome};
use web_deflate::verdict::{State, verdict};
use web_deflate::{MESSAGE_CAP, corpus, results, serve_baseline};
use yawc::{Frame, OpCode};

// ---- a global allocator that remembers its peak ----

struct Peak;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Peak {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(now, Ordering::SeqCst);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::SeqCst);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                let now = CURRENT.fetch_add(new_size - layout.size(), Ordering::SeqCst) + (new_size - layout.size());
                PEAK.fetch_max(now, Ordering::SeqCst);
            } else {
                CURRENT.fetch_sub(layout.size() - new_size, Ordering::SeqCst);
            }
        }
        p
    }
}

#[global_allocator]
static ALLOC: Peak = Peak;

/// Start a measurement: the peak is whatever is allocated right now. The result of [`peak_since`] is the most the
/// process held above that.
fn reset_peak() -> usize {
    let now = CURRENT.load(Ordering::SeqCst);
    PEAK.store(now, Ordering::SeqCst);
    now
}

fn peak_since(start: usize) -> usize {
    PEAK.load(Ordering::SeqCst).saturating_sub(start)
}

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn load(workload: &str) -> Vec<String> {
    match corpus::read(&corpus::path(workload)) {
        Ok(lines) => lines,
        Err(why) => panic!("{why}"),
    }
}

// ---- the measurement ----

/// Send the corpus through the baseline and return the wire bytes of the messages.
async fn baseline_bytes(lines: &[String]) -> u64 {
    let server = serve_baseline(loopback(), lines.to_vec()).await.expect("baseline server");
    let stream = tokio::net::TcpStream::connect(server.addr).await.expect("connect");
    let (mut ws, _) = tokio_tungstenite::client_async(server.url(), stream).await.expect("handshake");
    ws.send(Message::text("go")).await.expect("go");
    let mut count = 0;
    while let Some(message) = ws.next().await {
        match message.expect("a message") {
            Message::Text(_) => count += 1,
            Message::Close(_) => break,
            other => panic!("unexpected message {other:?}"),
        }
    }
    let _ = ws.close(None).await;
    assert_eq!(count, lines.len(), "the baseline sent every line");
    server.counters.message_bytes()
}

/// Send the corpus through the candidate to a client that negotiates permessage-deflate, check every line arrived
/// unchanged, and return the wire bytes of the messages.
async fn candidate_bytes(lines: &[String]) -> u64 {
    let server = serve_candidate(loopback(), lines.to_vec(), options()).await.expect("candidate server");
    let stream = tokio::net::TcpStream::connect(server.addr).await.expect("connect");
    let url = server.url().parse().expect("url");
    let mut ws = yawc::WebSocket::handshake(url, stream, yawc::Options::default().with_balanced_compression())
        .await
        .expect("handshake with the candidate");
    ws.send(Frame::text("go")).await.expect("go");
    let mut got: Vec<String> = Vec::new();
    loop {
        match ws.next_frame().await {
            Ok(frame) if frame.opcode() == OpCode::Text => got.push(String::from_utf8(frame.into_payload().to_vec()).expect("utf-8")),
            Ok(frame) if frame.opcode() == OpCode::Close => break,
            Ok(_) => {}
            Err(e) => panic!("the candidate's stream failed: {e}"),
        }
    }
    assert!(got == lines, "a line received through permessage-deflate differs from the corpus line");
    assert_eq!(server.counters.panics(), 0);
    server.counters.message_bytes()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_candidate_is_measured_against_the_baseline_on_the_same_corpus_in_one_run() {
    let _guard = serial();

    // What a browser offers, and what the candidate agrees to: the negotiated parameters.
    let probe = serve_candidate(loopback(), Vec::new(), options()).await.expect("candidate server");
    let offer = "permessage-deflate; client_max_window_bits";
    let (_client, handshake) = Client::connect(probe.addr, "/echo", Some(offer)).await.expect("probe");
    let response = handshake.extensions.clone().unwrap_or_else(|| "none".into());
    println!("offered:    {offer}\nnegotiated: {response}");
    results::set("negotiation", "offered", json!(offer));
    results::set("negotiation", "response", json!(response));
    assert!(response.contains("permessage-deflate"), "the candidate did not agree to permessage-deflate: {response}");
    drop(probe);

    println!("{:<10} {:>9} {:>11} {:>11} {:>11} {:>8}", "workload", "messages", "raw bytes", "baseline", "candidate", "saved");
    for workload in corpus::WORKLOADS {
        let lines = load(workload);
        let raw: u64 = lines.iter().map(|l| l.len() as u64).sum();
        let base = baseline_bytes(&lines).await;
        let cand = candidate_bytes(&lines).await;
        assert!(base > 0 && cand > 0, "{workload}: a counted wire size is zero");
        let saved = (1.0 - cand as f64 / base as f64) * 100.0;
        println!("{workload:<10} {:>9} {raw:>11} {base:>11} {cand:>11} {saved:>7.1}%", lines.len());
        results::set(
            "baseline",
            workload,
            json!({ "messages": lines.len(), "raw_bytes": raw, "wire_bytes": base }),
        );
        results::set(
            "candidate",
            workload,
            json!({ "messages": lines.len(), "raw_bytes": raw, "wire_bytes": cand, "baseline_wire_bytes": base,
                "saved_percent": (saved * 10.0).round() / 10.0 }),
        );
    }
}

// ---- the bombs ----

/// Raw deflate (no zlib header) of `total` zero bytes, as one permessage-deflate message: the stream ends with a sync
/// flush and its four trailing bytes `00 00 ff ff` are removed (RFC 7692, 7.2.1). Never holds the input.
fn deflate_zeros(total: u64) -> Vec<u8> {
    let mut compress = Compress::new(Compression::default(), false);
    let zeros = vec![0u8; 1 << 20];
    let mut out: Vec<u8> = Vec::new();
    let mut left = total;
    while left > 0 {
        let take = left.min(zeros.len() as u64) as usize;
        feed(&mut compress, &zeros[..take], FlushCompress::None, &mut out);
        left -= take as u64;
    }
    feed(&mut compress, &[], FlushCompress::Sync, &mut out);
    assert!(out.ends_with(&[0, 0, 0xff, 0xff]), "a sync flush ends with an empty stored block");
    out.truncate(out.len() - 4);
    out
}

fn feed(compress: &mut Compress, input: &[u8], flush: FlushCompress, out: &mut Vec<u8>) {
    let mut done = 0;
    loop {
        out.reserve(64 * 1024);
        let before = compress.total_in();
        compress.compress_vec(&input[done..], out, flush).expect("deflate");
        done += (compress.total_in() - before) as usize;
        if done == input.len() && out.len() < out.capacity() {
            break;
        }
    }
}

/// How many bytes `data` (a raw deflate stream) inflates to, in total, and the largest amount any one `piece` of it
/// produces. Streams the output away.
fn inflated_sizes(data: &[u8], piece: usize) -> (u64, u64) {
    let mut d = Decompress::new(false);
    let mut sink = vec![0u8; 1 << 20];
    let mut largest = 0;
    for chunk in data.chunks(piece) {
        let start = d.total_out();
        let mut at = 0;
        loop {
            let (in0, out0) = (d.total_in(), d.total_out());
            d.decompress(&chunk[at..], &mut sink, FlushDecompress::Sync).expect("inflate");
            at += (d.total_in() - in0) as usize;
            if at >= chunk.len() && d.total_out() - out0 < sink.len() as u64 {
                break;
            }
        }
        largest = largest.max(d.total_out() - start);
    }
    (d.total_out(), largest)
}

struct BombResult {
    outcome: String,
    refused: bool,
    peak: usize,
    panics: u64,
}

fn describe(outcome: &Outcome) -> (String, bool) {
    match outcome {
        Outcome::Close(Some(code)) => (format!("closed with status {code}"), true),
        Outcome::Close(None) => ("closed without a status".into(), true),
        Outcome::Dropped => ("connection dropped without a close frame".into(), true),
        Outcome::Message(n) => (format!("ACCEPTED: a message of {n} bytes came back"), false),
    }
}

/// Send frames to the candidate's echo endpoint and report what became of them. `frames` is a list of
/// `(opcode, fin, rsv1, payload)`. Writes that fail because the server already closed are not errors.
async fn attack(frames: &[(u8, bool, bool, &[u8])]) -> BombResult {
    let server = serve_candidate(loopback(), Vec::new(), options()).await.expect("candidate server");
    let (mut client, handshake) =
        Client::connect(server.addr, "/echo", Some("permessage-deflate")).await.expect("handshake");
    assert!(handshake.extensions.as_deref().is_some_and(|e| e.contains("permessage-deflate")), "deflate not negotiated");
    let start = reset_peak();
    for (opcode, fin, rsv1, payload) in frames {
        if client.send_frame(*opcode, *fin, *rsv1, payload).await.is_err() {
            break;
        }
    }
    let outcome = tokio::time::timeout(Duration::from_secs(60), client.outcome())
        .await
        .expect("the server answered within a minute")
        .unwrap_or(Outcome::Dropped);
    // Let the connection task end before looking at its panic count and the peak.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (outcome, refused) = describe(&outcome);
    BombResult { outcome, refused, peak: peak_since(start), panics: server.counters.panics() }
}

fn record_bomb(case: &str, r: &BombResult, wire: usize, inflated: u64) {
    let bound = 2 * MESSAGE_CAP;
    println!(
        "bomb {case}: {wire} bytes on the wire inflate to {inflated}; {}; peak {} bytes (bound {bound}); {} panics",
        r.outcome, r.peak, r.panics
    );
    results::set(
        "bomb",
        case,
        json!({ "wire_bytes": wire, "inflated_bytes": inflated, "outcome": r.outcome, "refused": r.refused,
            "peak_bytes": r.peak, "bound_bytes": bound, "within_bound": r.peak <= bound, "panics": r.panics,
            "message_cap_bytes": MESSAGE_CAP }),
    );
    assert!(r.refused, "{case}: the candidate accepted the bomb: {}", r.outcome);
    assert!(r.peak <= bound, "{case}: peak heap {} bytes is above twice the cap ({bound})", r.peak);
    assert_eq!(r.panics, 0, "{case}: a connection task panicked");
}

/// The control: a compressed message that is well under the cap goes in and comes back, so a refusal in the bomb
/// tests is the library's decision and not a mistake in the hand-made frames.
#[tokio::test(flavor = "multi_thread")]
async fn a_small_compressed_message_through_the_raw_client_is_echoed() {
    let _guard = serial();
    let text = "terminal output ".repeat(4096);
    let mut compress = Compress::new(Compression::default(), false);
    let mut wire = Vec::new();
    feed(&mut compress, text.as_bytes(), FlushCompress::None, &mut wire);
    feed(&mut compress, &[], FlushCompress::Sync, &mut wire);
    wire.truncate(wire.len() - 4);
    assert!(wire.len() < text.len() / 10, "the control message really is compressed");

    let server = serve_candidate(loopback(), Vec::new(), options()).await.expect("candidate server");
    let (mut client, _) = Client::connect(server.addr, "/echo", Some("permessage-deflate")).await.expect("handshake");
    client.send_frame(0x1, true, true, &wire).await.expect("send");
    let (opcode, fin, rsv1, reply) = tokio::time::timeout(Duration::from_secs(10), client.read_frame())
        .await
        .expect("answer")
        .expect("read")
        .expect("a frame");
    assert_eq!((opcode, fin, rsv1), (0x1, true, true), "the echo is a compressed text message");
    // Inflate the reply: the stream ends without the empty block that the sender removed.
    let mut inflate = Decompress::new(false);
    let mut back = Vec::with_capacity(text.len() + 1024);
    let mut input = reply.clone();
    input.extend_from_slice(&[0, 0, 0xff, 0xff]);
    inflate.decompress_vec(&input, &mut back, FlushDecompress::Sync).expect("inflate the echo");
    assert_eq!(back, text.as_bytes(), "the echo is the text that was sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_of_one_mebibyte_on_the_wire_that_inflates_to_a_gibibyte_is_refused() {
    let _guard = serial();
    let total: u64 = 1 << 30;
    let bomb = deflate_zeros(total);
    println!("the bomb is {} bytes on the wire and inflates to {total}", bomb.len());
    assert!(bomb.len() < 2 << 20, "the bomb is about a mebibyte on the wire");
    assert!(bomb.len() < MESSAGE_CAP, "the frame itself is under the message cap");
    let result = attack(&[(0x1, true, true, &bomb)]).await;
    record_bomb("single", &result, bomb.len(), total);
}

#[tokio::test(flavor = "multi_thread")]
async fn fragments_that_each_stay_under_the_cap_but_sum_above_it_are_refused() {
    let _guard = serial();
    let total: u64 = 64 << 20;
    let stream = deflate_zeros(total);
    let piece = 4096;
    let (inflated, largest) = inflated_sizes(&stream, piece);
    assert_eq!(inflated, total);
    assert!((largest as usize) < MESSAGE_CAP, "each fragment inflates to {largest} bytes, under the cap");
    assert!(inflated as usize > MESSAGE_CAP, "the fragments together inflate above the cap");
    let chunks: Vec<&[u8]> = stream.chunks(piece).collect();
    let last = chunks.len() - 1;
    let frames: Vec<(u8, bool, bool, &[u8])> = chunks
        .iter()
        .enumerate()
        .map(|(i, c)| (if i == 0 { 0x1 } else { 0x0 }, i == last, i == 0, *c))
        .collect();
    let result = attack(&frames).await;
    println!("{} fragments of {piece} bytes, the largest inflates to {largest} bytes", frames.len());
    record_bomb("fragments", &result, stream.len(), inflated);
}

// ---- the verdict rules ----

/// A results file in which every criterion holds. The numbers are inputs of the rule tests, not measurements.
fn all_met() -> Value {
    json!({
        "candidate": {
            "typing": { "saved_percent": 80.0 }, "listing": { "saved_percent": 70.0 },
            "redraw": { "saved_percent": 90.0 }, "cat-large": { "saved_percent": 75.0 }
        },
        "bomb": {
            "single": { "refused": true, "within_bound": true, "panics": 0, "outcome": "closed with status 1009", "peak_bytes": 1, "bound_bytes": 2 },
            "fragments": { "refused": true, "within_bound": true, "panics": 0, "outcome": "closed with status 1009", "peak_bytes": 1, "bound_bytes": 2 }
        },
        "autobahn": { "ran": true, "cases": 10, "failed": 0, "counts": { "OK": 10 } },
        "browsers": {
            "chromium": { "ran": true, "extensions": "permessage-deflate", "identical": true },
            "webkit": { "ran": true, "extensions": "permessage-deflate; client_max_window_bits", "identical": true }
        }
    })
}

#[test]
fn the_verdict_passes_only_when_every_criterion_was_measured_and_held() {
    let v = verdict(&all_met());
    assert!(v.pass(), "{}", v.text());
    assert!(v.text().starts_with("pass\n"));
    assert_eq!(v.criteria.len(), 5);
}

#[test]
fn each_single_failing_or_missing_criterion_makes_the_verdict_fail_and_names_it() {
    let cases: Vec<(&str, Box<dyn Fn(&mut Value)>, &str, State)> = vec![
        ("a frame workload saves too little", Box::new(|v| v["candidate"]["listing"]["saved_percent"] = json!(29.9)), "frame traffic saved", State::NotMet),
        ("a frame workload is missing", Box::new(|v| { v["candidate"].as_object_mut().unwrap().remove("redraw"); }), "frame traffic saved", State::NotRun),
        ("large text saves too little", Box::new(|v| v["candidate"]["cat-large"]["saved_percent"] = json!(49.9)), "large text saved", State::NotMet),
        ("large text is missing", Box::new(|v| { v["candidate"].as_object_mut().unwrap().remove("cat-large"); }), "large text saved", State::NotRun),
        ("the bomb is accepted", Box::new(|v| v["bomb"]["single"]["refused"] = json!(false)), "bomb refused within the memory bound", State::NotMet),
        ("the bomb peak is over the bound", Box::new(|v| v["bomb"]["fragments"]["within_bound"] = json!(false)), "bomb refused within the memory bound", State::NotMet),
        ("a task panicked", Box::new(|v| v["bomb"]["single"]["panics"] = json!(1)), "bomb refused within the memory bound", State::NotMet),
        ("the bomb was not run", Box::new(|v| { v["bomb"].as_object_mut().unwrap().remove("fragments"); }), "bomb refused within the memory bound", State::NotRun),
        ("a case failed", Box::new(|v| v["autobahn"]["failed"] = json!(1)), "no failed Autobahn case", State::NotMet),
        ("Autobahn was not run", Box::new(|v| v["autobahn"] = json!({ "ran": false, "reason": "Docker is missing" })), "no failed Autobahn case", State::NotRun),
        ("a browser does not negotiate", Box::new(|v| v["browsers"]["webkit"]["extensions"] = json!("")), "both browsers negotiate and round-trip", State::NotMet),
        ("a browser round trip differs", Box::new(|v| v["browsers"]["chromium"]["identical"] = json!(false)), "both browsers negotiate and round-trip", State::NotMet),
        ("a browser is missing", Box::new(|v| v["browsers"]["webkit"] = json!({ "ran": false, "reason": "not installed" })), "both browsers negotiate and round-trip", State::NotRun),
    ];
    for (what, change, name, state) in cases {
        let mut results = all_met();
        change(&mut results);
        let v = verdict(&results);
        assert!(!v.pass(), "{what}: the verdict must fail");
        assert!(v.text().starts_with("fail\n"), "{what}");
        let c = v.criteria.iter().find(|c| c.name == name).unwrap_or_else(|| panic!("{what}: no criterion `{name}`"));
        assert_eq!(c.state, state, "{what}: {}", v.text());
        assert!(v.text().contains(name), "{what}: the criterion is named");
        let others = v.criteria.iter().filter(|c| c.name != name && c.state != State::Met).count();
        assert_eq!(others, 0, "{what}: only `{name}` fails");
    }
    assert!(!verdict(&json!({})).pass(), "an empty results file is a fail");
}
