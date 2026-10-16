//! The HTTP client of the AI chat (`holzwave_core::ai::http`), against a server in this process.
//!
//! No test here reaches the network. `Loopback` is a small HTTP/1.1 server on `127.0.0.1` that records what it was sent
//! and answers by path, so a test can say what the client must do (stream in order, follow no redirect, send no
//! compression header, end a silent server) and what the server must have seen. Plan 07-12 builds the shared fake server
//! in `tests/common` and may move this one there.
//!
//! The tests of the TLS handshake itself need a real endpoint; they are the two ignored tests at the end.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use holzwave_core::ai::http::{ClientOptions, ProxyPolicy, build_client};

/// The longest request head the server reads.
const HEAD_LIMIT: usize = 16 * 1024;

/// The largest request body the server reads.
const BODY_LIMIT: usize = 1024 * 1024;

/// One request as the server read it. Header names are lowercased.
#[derive(Clone, Debug)]
struct Seen {
    line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }
}

/// What the connection threads share with the test.
#[derive(Default)]
struct Shared {
    /// Every request read so far, in arrival order.
    seen: Mutex<Vec<Seen>>,
    /// The prepared answer of `/flood/{n}`: the event count and the whole chunked body, built before any timing starts.
    flood: Mutex<Option<(usize, Arc<Vec<u8>>)>>,
}

/// An HTTP/1.1 server with keep-alive on a free loopback port. It stops accepting when it is dropped.
struct Loopback {
    addr: SocketAddr,
    shared: Arc<Shared>,
    accepted: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl Loopback {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let addr = listener.local_addr().expect("local address");
        let shared = Arc::new(Shared::default());
        let accepted = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared_in, accepted_in, stop_in) = (shared.clone(), accepted.clone(), stop.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                if stop_in.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                accepted_in.fetch_add(1, Ordering::SeqCst);
                let shared = shared_in.clone();
                thread::spawn(move || serve(stream, &shared));
            }
        });
        Self { addr, shared, accepted, stop }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn requests(&self) -> Vec<Seen> {
        self.shared.seen.lock().expect("seen").clone()
    }

    /// Builds the body of `/flood/{count}` now, so that the work is not part of a measurement.
    fn prepare_flood(&self, count: usize) {
        *self.shared.flood.lock().expect("flood") = Some((count, Arc::new(flood_body(count))));
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept call so the thread sees the flag.
        let _ = TcpStream::connect(self.addr);
    }
}

/// Reads one request: the head up to the blank line (bounded) and as many body bytes as `content-length` says.
fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Seen> {
    let mut line = String::new();
    let mut used = reader.read_line(&mut line).ok()?;
    if used == 0 {
        return None;
    }
    let request_line = line.trim_end().to_owned();
    let mut headers = Vec::new();
    loop {
        line.clear();
        used += reader.read_line(&mut line).ok()?;
        if used > HEAD_LIMIT || line.is_empty() {
            return None;
        }
        let text = line.trim_end();
        if text.is_empty() {
            break;
        }
        let (name, value) = text.split_once(':')?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length.min(BODY_LIMIT)];
    reader.read_exact(&mut body).ok()?;
    Some(Seen { line: request_line, headers, body })
}

fn serve(stream: TcpStream, shared: &Shared) {
    let _ = stream.set_nodelay(true);
    let Ok(mut writer) = stream.try_clone() else { return };
    let mut reader = BufReader::new(stream);
    while let Some(request) = read_request(&mut reader) {
        let path = request.line.split_whitespace().nth(1).unwrap_or("/").to_owned();
        shared.seen.lock().expect("seen").push(request);
        if respond(&mut writer, &path, shared).is_err() {
            break;
        }
    }
}

/// The text of event `i`: a data line and a blank line.
fn event(i: usize) -> String {
    format!("data: {{\"i\":{i}}}\n\n")
}

/// The head of a chunked event stream.
fn stream_head(writer: &mut TcpStream) -> io::Result<()> {
    writer.write_all(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
    )?;
    writer.flush()
}

/// One chunk in a single write, so that the framing is never split by the server.
fn chunk(writer: &mut TcpStream, data: &str) -> io::Result<()> {
    let mut frame = format!("{:x}\r\n", data.len()).into_bytes();
    frame.extend_from_slice(data.as_bytes());
    frame.extend_from_slice(b"\r\n");
    writer.write_all(&frame)?;
    writer.flush()
}

/// The chunked body of `count` events, one chunk per event, and the terminating chunk.
fn flood_body(count: usize) -> Vec<u8> {
    let mut body = Vec::with_capacity(count * 28);
    for i in 0..count {
        let text = event(i);
        body.extend_from_slice(format!("{:x}\r\n", text.len()).as_bytes());
        body.extend_from_slice(text.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"0\r\n\r\n");
    body
}

fn respond(writer: &mut TcpStream, path: &str, shared: &Shared) -> io::Result<()> {
    if let Some(count) = path.strip_prefix("/events/").and_then(|n| n.parse::<usize>().ok()) {
        stream_head(writer)?;
        for i in 0..count {
            chunk(writer, &event(i))?;
            thread::sleep(Duration::from_millis(5));
        }
        writer.write_all(b"0\r\n\r\n")?;
        return writer.flush();
    }
    if let Some(count) = path.strip_prefix("/flood/").and_then(|n| n.parse::<usize>().ok()) {
        let prepared = shared.flood.lock().expect("flood").clone().filter(|(n, _)| *n == count);
        let body = prepared.map_or_else(|| Arc::new(flood_body(count)), |(_, body)| body);
        stream_head(writer)?;
        for part in body.chunks(64 * 1024) {
            writer.write_all(part)?;
        }
        return writer.flush();
    }
    match path {
        // The head and the first event leave at once; the rest follows after a pause, so that buffering on the way
        // would show up as a late first delta.
        "/first" => {
            stream_head(writer)?;
            chunk(writer, &event(0))?;
            thread::sleep(Duration::from_millis(50));
            chunk(writer, &event(1))?;
            writer.write_all(b"0\r\n\r\n")?;
        }
        "/redirect" => {
            writer.write_all(b"HTTP/1.1 302 Found\r\nlocation: /events/1\r\ncontent-length: 0\r\n\r\n")?;
        }
        // The request is read and recorded, and the answer never comes.
        "/silent" => {
            thread::sleep(Duration::from_secs(5));
            return Ok(());
        }
        _ => writer.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n")?,
    }
    writer.flush()
}

/// A current-thread runtime with the timer and the I/O driver: `connect_timeout` needs the timer.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime")
}

// ---- the tracer -------------------------------------------------------------------------------------------

#[test]
fn the_client_is_built_from_the_ring_provider_and_installs_no_process_default() {
    build_client(&ClientOptions::new(false, ProxyPolicy::Direct)).expect("a client");
    build_client(&ClientOptions::new(true, ProxyPolicy::System)).expect("a client that follows the proxy settings");

    assert!(
        rustls::crypto::CryptoProvider::get_default().is_none(),
        "building the client installed a process-wide crypto provider: the SSH and WebDAV clients would change"
    );
}

#[test]
fn a_chunked_event_stream_arrives_in_order_through_the_built_client() {
    let server = Loopback::start();
    let client = build_client(&ClientOptions::new(false, ProxyPolicy::Direct)).expect("a client");

    let (status, content_type, chunks) = runtime().block_on(async {
        let mut response = client.post(server.url("/events/3")).body("ping").send().await.expect("a response");
        let status = response.status().as_u16();
        let content_type = response.headers().get("content-type").map(|v| v.to_str().expect("text").to_owned());
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        while let Some(piece) = response.chunk().await.expect("a chunk") {
            chunks.push(piece.to_vec());
        }
        (status, content_type, chunks)
    });

    assert_eq!(status, 200);
    assert_eq!(content_type.as_deref(), Some("text/event-stream"));
    assert!(!chunks.is_empty(), "no chunk was delivered");
    assert!(chunks.iter().all(|piece| !piece.is_empty()), "an empty chunk was delivered: {chunks:?}");
    let body: Vec<u8> = chunks.concat();
    let expected = format!("{}{}{}", event(0), event(1), event(2));
    assert_eq!(String::from_utf8(body).expect("utf-8"), expected, "the events arrived out of order or damaged");

    let requests = server.requests();
    assert_eq!(requests.len(), 1, "the server saw {requests:?}");
    assert_eq!(requests[0].line, "POST /events/3 HTTP/1.1");
    assert_eq!(requests[0].body, b"ping");
    assert_eq!(requests[0].header("accept-encoding"), None, "a compression header was sent");
    assert_eq!(server.accepted(), 1);
    assert!(rustls::crypto::CryptoProvider::get_default().is_none(), "a request installed a process-wide provider");
}

// ---- the guarantees of the client -------------------------------------------------------------------------

#[test]
fn a_redirect_is_returned_and_never_followed() {
    let server = Loopback::start();
    let client = build_client(&ClientOptions::new(false, ProxyPolicy::Direct)).expect("a client");

    let (status, location, body) = runtime().block_on(async {
        let response = client.post(server.url("/redirect")).body("the prompt").send().await.expect("a response");
        let status = response.status().as_u16();
        let location = response.headers().get("location").map(|v| v.to_str().expect("text").to_owned());
        (status, location, response.bytes().await.expect("a body").to_vec())
    });

    assert_eq!(status, 302, "the redirect was followed or hidden");
    assert_eq!(location.as_deref(), Some("/events/1"));
    assert!(body.is_empty(), "the body came from the target of the redirect: {body:?}");
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "the prompt was sent again: {requests:?}");
    assert_eq!(requests[0].line, "POST /redirect HTTP/1.1");
    assert_eq!(requests[0].body, b"the prompt");
}

#[test]
fn https_only_refuses_a_plain_url_before_any_connection() {
    let server = Loopback::start();
    let client = build_client(&ClientOptions::new(true, ProxyPolicy::Direct)).expect("a client");

    let error = runtime()
        .block_on(async { client.post(server.url("/events/1")).body("the prompt").send().await })
        .expect_err("a plain url must be refused");

    assert!(error.is_builder(), "the refusal is not a builder error: {error}");
    assert_eq!(server.accepted(), 0, "a connection was opened for a refused request");
    assert!(server.requests().is_empty());
}

#[test]
fn a_silent_server_ends_in_a_read_timeout() {
    let server = Loopback::start();
    let options = ClientOptions { read_timeout: Duration::from_millis(300), ..ClientOptions::new(false, ProxyPolicy::Direct) };
    let client = build_client(&options).expect("a client");

    let started = Instant::now();
    let error = runtime()
        .block_on(async { client.post(server.url("/silent")).body("the prompt").send().await })
        .expect_err("a silent server must end in an error");

    assert!(error.is_timeout(), "the error is not a timeout: {error}");
    assert!(started.elapsed() < Duration::from_secs(3), "the timeout took {:?}", started.elapsed());
    assert_eq!(server.requests().len(), 1, "the request did not reach the server");
}

#[test]
fn a_closed_port_is_a_connect_error() {
    // Bind a port, note it, and close it: nobody listens there any more.
    let port = TcpListener::bind("127.0.0.1:0").expect("bind").local_addr().expect("address").port();
    let client = build_client(&ClientOptions::new(false, ProxyPolicy::Direct)).expect("a client");

    let started = Instant::now();
    let error = runtime()
        .block_on(async { client.post(format!("http://127.0.0.1:{port}/")).body("the prompt").send().await })
        .expect_err("a closed port must be an error");

    assert!(error.is_connect(), "the error is not a connect error: {error}");
    assert!(started.elapsed() < Duration::from_secs(3), "the refusal took {:?}", started.elapsed());
}

// ---- spike S1: the numbers and one real endpoint -----------------------------------------------------------

/// The milliseconds of a duration, for printing.
fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// The median and the maximum of a series of times.
fn median_and_max(mut times: Vec<Duration>) -> (Duration, Duration) {
    times.sort();
    (times[times.len() / 2], times[times.len() - 1])
}

/// The time from calling `send()` to the first non-empty chunk of `/first`; the rest of the body is read afterwards, so
/// that the connection can be reused.
async fn time_to_first_delta(client: &reqwest::Client, url: &str) -> Duration {
    let started = Instant::now();
    let mut response = client.post(url).body("ping").send().await.expect("a response");
    let mut first = None;
    while let Some(piece) = response.chunk().await.expect("a chunk") {
        if first.is_none() && !piece.is_empty() {
            first = Some(started.elapsed());
        }
    }
    first.expect("no chunk arrived")
}

/// What it costs to stream events through the built client (spike S1).
///
/// Time to first delta is measured from `send()` to the first non-empty chunk, on one warm client (one connection, an
/// unmeasured request first) and on a fresh client with a fresh connection each time. The per-event cost is the wall
/// time from the first byte of an unpaced 100,000 event flood to the last, divided by the events counted. That is a
/// wall-clock proxy for the CPU time per event, valid here because the client runs on one thread, the server only
/// copies bytes it built before the timing started, and the stream is unpaced; true CPU accounting would need a new
/// dependency (`libc`) and is not worth a D-03 decision. The run uses plain http on loopback with `Direct`, so it
/// measures the client and its HTTP layer, not the TLS handshake: the handshake appears in the real-HTTPS test.
#[test]
#[ignore = "run with --release: cargo test --release -p holzwave-core --no-default-features --test ai_tls -- --ignored --nocapture"]
fn spike_s1_time_to_first_delta_and_cost_per_event() {
    const SERIES: usize = 20;
    const EVENTS: usize = 100_000;
    let options = ClientOptions::new(false, ProxyPolicy::Direct);
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    println!("spike S1 on {} {}, {profile} build", std::env::consts::OS, std::env::consts::ARCH);

    let started = Instant::now();
    build_client(&options).expect("a client");
    let first_build = started.elapsed();
    let started = Instant::now();
    let client = build_client(&options).expect("a client");
    let second_build = started.elapsed();
    println!("client build: first {:.2} ms, second {:.2} ms", ms(first_build), ms(second_build));

    let runtime = runtime();
    let warm_server = Loopback::start();
    let warm_url = warm_server.url("/first");
    runtime.block_on(time_to_first_delta(&client, &warm_url));
    let warm: Vec<Duration> = (0..SERIES).map(|_| runtime.block_on(time_to_first_delta(&client, &warm_url))).collect();
    let warm_connections = warm_server.accepted();

    let fresh_server = Loopback::start();
    let fresh_url = fresh_server.url("/first");
    let fresh: Vec<Duration> = (0..SERIES)
        .map(|_| {
            let fresh_client = build_client(&options).expect("a client");
            runtime.block_on(time_to_first_delta(&fresh_client, &fresh_url))
        })
        .collect();
    let fresh_connections = fresh_server.accepted();

    let (warm_median, warm_max) = median_and_max(warm);
    let (fresh_median, fresh_max) = median_and_max(fresh);
    println!(
        "time to first delta: warm client median {:.2} ms, max {:.2} ms, {warm_connections} connection(s); fresh client \
         median {:.2} ms, max {:.2} ms, {fresh_connections} connection(s); {SERIES} requests each",
        ms(warm_median),
        ms(warm_max),
        ms(fresh_median),
        ms(fresh_max),
    );

    let flood = Loopback::start();
    flood.prepare_flood(EVENTS);
    let (counted, elapsed) = runtime.block_on(async {
        let mut response = client.post(flood.url(&format!("/flood/{EVENTS}"))).body("ping").send().await.expect("a response");
        let (mut counted, mut previous, mut began) = (0usize, 0u8, None);
        while let Some(piece) = response.chunk().await.expect("a chunk") {
            began.get_or_insert_with(Instant::now);
            // An event ends with a blank line; the last byte of one chunk is carried into the next.
            for &byte in piece.iter() {
                if byte == b'\n' && previous == b'\n' {
                    counted += 1;
                }
                previous = byte;
            }
        }
        (counted, began.expect("no chunk arrived").elapsed())
    });
    let per_event = elapsed.as_secs_f64() * 1e6 / counted as f64;
    println!(
        "per event: {counted} events, {:.1} ms, {per_event:.2} us per event, {:.0} events per second",
        ms(elapsed),
        counted as f64 / elapsed.as_secs_f64()
    );

    assert_eq!(counted, EVENTS, "events were lost or counted twice");
    if cfg!(debug_assertions) {
        println!("budgets: not asserted in a debug build");
        return;
    }
    println!("budgets: warm median <= 10 ms, fresh median <= 50 ms, <= 50 us per event");
    assert!(warm_median <= Duration::from_millis(10), "warm median time to first delta {:.2} ms", ms(warm_median));
    assert!(fresh_median <= Duration::from_millis(50), "fresh-client median time to first delta {:.2} ms", ms(fresh_median));
    assert!(per_event <= 50.0, "{per_event:.2} us per event");
}

/// The error and its causes on one line, without the URL (a query string may carry a token).
fn chain(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut text = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// One request to a real https endpoint, through the platform verifier and the proxy settings of the machine.
///
/// `HOLZWAVE_TEST_HTTPS_URL` is requested with `https_only` and `ProxyPolicy::System`; any HTTP status counts (a 401
/// proves the handshake and the verifier). When `HOLZWAVE_TEST_BAD_CERT_URL` is set, requesting it must fail. Nothing
/// secret is sent: a GET without a body or a header of ours.
#[test]
#[ignore = "needs network: set HOLZWAVE_TEST_HTTPS_URL"]
fn spike_s1_real_https_request_with_the_platform_verifier() {
    let url = std::env::var("HOLZWAVE_TEST_HTTPS_URL")
        .unwrap_or_else(|_| panic!("set HOLZWAVE_TEST_HTTPS_URL to an https URL (HOLZWAVE_TEST_HTTPS_URL is missing)"));
    let client = build_client(&ClientOptions::new(true, ProxyPolicy::System)).expect("a client");
    let runtime = runtime();

    let started = Instant::now();
    let (status, version, head, first_chunk) = runtime.block_on(async {
        let mut response = match client.get(&url).send().await {
            Ok(response) => response,
            Err(error) => panic!("transport error: {}", chain(error)),
        };
        let head = started.elapsed();
        let (status, version) = (response.status(), response.version());
        let first = match response.chunk().await {
            Ok(_) => started.elapsed(),
            Err(error) => panic!("transport error while reading the body: {}", chain(error)),
        };
        (status, version, head, first)
    });
    println!(
        "real https: status {status}, version {version:?}, response head after {:.1} ms, first body chunk after {:.1} ms",
        ms(head),
        ms(first_chunk)
    );

    if let Ok(bad) = std::env::var("HOLZWAVE_TEST_BAD_CERT_URL") {
        let result = runtime.block_on(async { client.get(&bad).send().await });
        match result {
            Ok(response) => panic!("certificate check is off: the bad-certificate URL answered {}", response.status()),
            Err(error) => println!("bad certificate refused: {}", chain(error)),
        }
    }
}
