//! The HTTP chokepoint of the AI chat and the shared provider that drives a dialect through it, with no outside network.
//!
//! Every request in this file goes to `FakeHttp`, an HTTP/1.1 server in this process that records what it was sent and
//! answers byte by byte as the test says. The dialect under test is `LineAdapter`, a tiny one defined here: it turns a
//! chat request into a JSON line and every received frame into a text delta, so what is proved is the chokepoint (where a
//! request may go, how it is bounded, retried and cancelled) and the driver, not any provider's JSON. Every wait is
//! bounded; a stalled server uses a ceiling of a few seconds so a defect fails an assertion instead of hanging.

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::fake_http::{Ending, FakeHttp, Reply};
use holzwave_core::ai::http::{Framing, Http, HttpConfig, NetworkPolicy, Scheme, Target, classify_ip};
use holzwave_core::ai::provider::{
    AiError, CancelToken, ChatRequest, Completion, Detail, Flow, Provider, StreamEvent,
};
use holzwave_core::ai::sse::StreamEnd;
use holzwave_core::ai::types::{Capabilities, Locality, Message, Role, StopReason, Usage};
use holzwave_core::ai::wire::{Endpoint, HttpProvider, Wire, WireAdapter, WireRequest, WireTurn};
use serde_json::Value;

// ---- the test dialect and the helpers -------------------------------------------------------------------

/// A dialect with one JSON line as the request and one text delta per frame as the answer.
struct LineAdapter {
    wire: Wire,
}

impl LineAdapter {
    fn new(wire: Wire) -> Self {
        Self { wire }
    }
}

struct LineTurn {
    lines: bool,
    done: bool,
}

impl WireAdapter for LineAdapter {
    fn wire(&self) -> Wire {
        self.wire
    }

    fn capabilities(&self, _model: &str, locality: Locality) -> Capabilities {
        Capabilities::conservative(locality)
    }

    fn request(&self, req: &ChatRequest, _caps: &Capabilities) -> Result<WireRequest, AiError> {
        let text: Vec<String> = req.messages.iter().filter(|m| m.role == Role::User).map(Message::text).collect();
        let body = serde_json::json!({ "model": req.model, "text": text.join("\n") });
        let body = serde_json::to_vec(&body).expect("a JSON body");
        Ok(WireRequest { path: "/v1/lines".to_owned(), headers: Vec::new(), body })
    }

    fn turn(&self) -> Box<dyn WireTurn> {
        Box::new(LineTurn { lines: self.wire.framing() == Framing::Ndjson, done: false })
    }
}

impl WireTurn for LineTurn {
    fn frame(
        &mut self,
        frame: holzwave_core::ai::http::Frame<'_>,
        out: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<Flow, AiError> {
        use holzwave_core::ai::http::Frame;
        match frame {
            Frame::Event(event) if event.is_done() => {
                self.done = true;
                Ok(Flow::Continue)
            }
            Frame::Event(event) => Ok(out(StreamEvent::TextDelta(event.data().to_owned()))),
            Frame::Line(line) => {
                let value: Value = serde_json::from_slice(line)
                    .map_err(|_| AiError::Protocol(Detail::new("a line is not a JSON record")))?;
                let text = value.get("t").and_then(Value::as_str).unwrap_or_default();
                Ok(out(StreamEvent::TextDelta(text.to_owned())))
            }
        }
    }

    fn finish(
        self: Box<Self>,
        end: StreamEnd,
        _out: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<Completion, AiError> {
        if end.is_clean() && (self.lines || self.done) {
            Ok(Completion { stop_reason: StopReason::EndTurn, usage: Usage::default(), native: None })
        } else {
            Err(AiError::Protocol(Detail::new("the stream ended without its terminal event")))
        }
    }
}

fn endpoint(fake: &FakeHttp, wire: Wire) -> Endpoint {
    let target = Target::parse(&fake.base(), false).expect("a loopback base address");
    Endpoint::new(wire, target, NetworkPolicy::new(false, false))
}

fn provider_for(fake: &FakeHttp, wire: Wire) -> (Arc<HttpProvider>, Arc<Http>) {
    let http = Arc::new(Http::new(HttpConfig::standard()));
    let provider = HttpProvider::new("line", endpoint(fake, wire), Box::new(LineAdapter::new(wire)), http.clone());
    (Arc::new(provider), http)
}

fn request(text: &str) -> ChatRequest {
    let mut request = ChatRequest::new("m");
    request.messages.push(Message::user_text("u1", text, 1));
    request
}

/// What a run delivered, with the moment of each event.
struct Run {
    started: Instant,
    events: Vec<(Instant, StreamEvent)>,
    result: Result<Completion, AiError>,
}

impl Run {
    fn texts(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|(_, event)| match event {
                StreamEvent::TextDelta(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
}

fn run(provider: &dyn Provider, request: &ChatRequest, cancel: &CancelToken) -> Run {
    let started = Instant::now();
    let mut events = Vec::new();
    let result = provider.stream(
        request,
        &mut |event| {
            events.push((Instant::now(), event));
            Flow::Continue
        },
        cancel,
    );
    Run { started, events, result }
}

/// Sends `bytes` to the server with a plain socket (the code under test is not involved) and reads until it closes.
fn raw(addr: SocketAddr, bytes: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).expect("connect to the fake server");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("read timeout");
    stream.write_all(bytes).expect("send the request");
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer);
    answer
}

fn post(path: &str, body: &str) -> Vec<u8> {
    format!("POST {path} HTTP/1.1\r\nHost: test\r\nX-Probe: One\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

/// The sizes and bytes of the HTTP chunks of a chunked answer, and whether the terminating chunk is there.
fn parse_chunked(answer: &[u8]) -> (Vec<Vec<u8>>, bool) {
    let split = answer.windows(4).position(|w| w == b"\r\n\r\n").expect("the head ends") + 4;
    let mut rest = &answer[split..];
    let mut chunks = Vec::new();
    loop {
        let Some(eol) = rest.windows(2).position(|w| w == b"\r\n") else { return (chunks, false) };
        let size = usize::from_str_radix(std::str::from_utf8(&rest[..eol]).expect("hex size").trim(), 16).expect("size");
        rest = &rest[eol + 2..];
        if size == 0 {
            return (chunks, true);
        }
        assert!(rest.len() >= size + 2, "a chunk is cut short");
        chunks.push(rest[..size].to_vec());
        rest = &rest[size + 2..];
    }
}

// ---- the fake server -------------------------------------------------------------------------------------

#[test]
fn the_fake_server_captures_the_request_and_streams_the_chosen_chunks() {
    let body: Vec<u8> = (0..40u8).map(|i| b'a' + i % 26).collect();
    let fake = FakeHttp::start(vec![
        Reply::sse_split(&body, &[7, 19]),
        Reply::status(429).header("retry-after", "2").body("slow down"),
        Reply::status(404),
    ]);

    let first = raw(fake.addr(), &post("/v1/x?y=1", "hello"));
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].target, "/v1/x?y=1");
    assert_eq!(seen[0].header("x-probe"), Some("One"), "header names are lower case, values as sent");
    assert_eq!(seen[0].body, b"hello");
    assert_eq!(seen[0].body_text(), "hello");
    let head = String::from_utf8_lossy(&first).to_ascii_lowercase();
    assert!(head.contains("transfer-encoding: chunked"), "{head}");
    assert!(head.contains("connection: close"), "{head}");
    let (chunks, terminated) = parse_chunked(&first);
    let sizes: Vec<usize> = chunks.iter().map(Vec::len).collect();
    assert_eq!(sizes, [7, 12, 21], "the body is cut at the chosen offsets");
    assert_eq!(chunks.concat(), body);
    assert!(terminated, "the terminating chunk is written");

    let second = String::from_utf8(raw(fake.addr(), &post("/v1/x", ""))).expect("text");
    assert!(second.starts_with("HTTP/1.1 429 Too Many Requests\r\n"), "{second}");
    assert!(second.contains("retry-after: 2\r\n"), "{second}");
    assert!(second.contains("content-length: 9\r\n"), "{second}");
    assert!(second.ends_with("slow down"), "{second}");

    let third = String::from_utf8(raw(fake.addr(), &post("/v1/x", ""))).expect("text");
    assert!(third.starts_with("HTTP/1.1 404 Not Found\r\n"), "the last reply repeats: {third}");
    assert_eq!(fake.accepted(), 3);
    assert_eq!(fake.seen().len(), 3);
}

#[test]
fn the_fake_server_stalls_hangs_drops_and_reports_the_peer_closing() {
    // Hold: the socket stays open until the client closes it.
    let held = FakeHttp::start(vec![Reply::sse(&["data: x\n\n"]).ending(Ending::Hold)]);
    let mut client = TcpStream::connect(held.addr()).expect("connect");
    client.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    client.write_all(&post("/hold", "")).expect("send");
    let mut got = Vec::new();
    let mut piece = [0u8; 512];
    while !String::from_utf8_lossy(&got).contains("data: x") {
        let n = client.read(&mut piece).expect("read the first chunk");
        assert!(n > 0, "the server closed early");
        got.extend_from_slice(&piece[..n]);
    }
    assert!(held.wait_closed(0, Duration::from_millis(200)).is_none(), "the client has not closed yet");
    let dropped = Instant::now();
    drop(client);
    let closed = held.wait_closed(0, Duration::from_secs(5)).expect("the server notices the close");
    assert!(
        closed.saturating_duration_since(dropped) < Duration::from_millis(250),
        "noticed after {:?}",
        closed.saturating_duration_since(dropped)
    );

    // Drop: the client sees the end of the stream with no terminating chunk.
    let dropping = FakeHttp::start(vec![Reply::sse(&["data: y\n\n"]).ending(Ending::Drop)]);
    let answer = raw(dropping.addr(), &post("/drop", ""));
    let text = String::from_utf8_lossy(&answer);
    assert!(text.contains("data: y"), "{text}");
    assert!(!parse_chunked(&answer).1, "no terminating chunk: {text}");

    // Hang: the request is read and nothing is sent.
    let hanging = FakeHttp::start(vec![Reply::Hang]);
    let mut client = TcpStream::connect(hanging.addr()).expect("connect");
    client.set_read_timeout(Some(Duration::from_millis(300))).expect("timeout");
    client.write_all(&post("/hang", "")).expect("send");
    let read = client.read(&mut piece);
    assert!(
        matches!(&read, Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
        "a hanging server sends nothing: {read:?}"
    );
    assert_eq!(hanging.seen().len(), 1, "the request was read");
    drop(client);
    assert!(hanging.wait_closed(0, Duration::from_secs(5)).is_some());

    // Close: the request is read and the socket is closed.
    let closing = FakeHttp::start(vec![Reply::Close]);
    let answer = raw(closing.addr(), &post("/close", ""));
    assert!(answer.is_empty(), "a closed connection sends nothing");
    assert_eq!(closing.seen().len(), 1);
    let info = closing.connection(0).expect("connection info");
    assert!(info.request_complete.is_some());
    assert!(info.peer_closed.is_none());
}

// ---- the tracer ------------------------------------------------------------------------------------------

#[test]
fn a_chunked_event_stream_reaches_the_provider_callback_in_order() {
    let fake = FakeHttp::start(vec![
        Reply::sse(&["data: Hel\n\n", "data: lo\n\n", "data: [DONE]\n\n"]).gap(Duration::from_millis(20)),
    ]);
    let (provider, http) = provider_for(&fake, Wire::OpenAiChat);
    let dynamic: Arc<dyn Provider> = provider.clone();

    let mut collector = holzwave_core::ai::provider::Collector::new();
    let mut seen_events = Vec::new();
    let result = dynamic.stream(
        &request("hello"),
        &mut |event| {
            seen_events.push(event.clone());
            collector.push(event)
        },
        &CancelToken::new(),
    );

    let completion = result.expect("the turn completes");
    assert_eq!(
        seen_events,
        [StreamEvent::TextDelta("Hel".to_owned()), StreamEvent::TextDelta("lo".to_owned())],
        "the closure sees the deltas in order"
    );
    assert_eq!(collector.text(), "Hello");
    assert_eq!(completion.stop_reason, StopReason::EndTurn);
    assert_eq!(http.attempts(), 1);

    let seen = fake.seen();
    assert_eq!(seen.len(), 1, "exactly one request");
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].target, "/v1/lines");
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    assert_eq!(seen[0].header("accept"), Some("text/event-stream"));
    assert!(seen[0].header("user-agent").is_some_and(|agent| agent.starts_with("holzWave/")), "{:?}", seen[0].headers);
    assert_eq!(seen[0].header("host"), Some(format!("127.0.0.1:{}", fake.port()).as_str()));
    assert_eq!(seen[0].header("accept-encoding"), None, "no compression is asked for");
    assert!(seen[0].body_text().contains("hello"), "{}", seen[0].body_text());

    assert_eq!(provider.locality(), Locality::Loopback);
    assert_eq!(dynamic.capabilities("m").locality, Locality::Loopback);
    println!("tracer: a_chunked_event_stream_reaches_the_provider_callback_in_order ok, text {:?}", collector.text());
}

#[test]
fn events_are_delivered_as_they_arrive_and_not_when_the_stream_ends() {
    let fake = FakeHttp::start(vec![
        Reply::sse(&["data: a\n\n", "data: b\n\n", "data: [DONE]\n\n"]).gap(Duration::from_millis(300)),
    ]);
    let (provider, _) = provider_for(&fake, Wire::OpenAiChat);

    let outcome = run(provider.as_ref(), &request("hi"), &CancelToken::new());

    outcome.result.as_ref().expect("the turn completes");
    assert_eq!(outcome.texts(), ["a", "b"]);
    let first = outcome.events[0].0.duration_since(outcome.started);
    let gap = outcome.events[1].0.duration_since(outcome.events[0].0);
    println!("first delta after {} ms; second delta {} ms after the first", first.as_millis(), gap.as_millis());
    assert!(first < Duration::from_millis(250), "the first delta waited for the stream: {first:?}");
    assert!(gap >= Duration::from_millis(250), "the second delta came with the first: {gap:?}");
}

#[test]
fn ndjson_lines_reach_the_callback_one_by_one() {
    let lines = b"{\"t\":\"a\"}\n{\"t\":\"b\"}\n".to_vec();
    let fake = FakeHttp::start(vec![Reply::sse_bytes(vec![lines]).content_type("application/x-ndjson")]);
    let (provider, _) = provider_for(&fake, Wire::Ollama);

    let outcome = run(provider.as_ref(), &request("hi"), &CancelToken::new());

    outcome.result.as_ref().expect("the turn completes");
    assert_eq!(outcome.texts(), ["a", "b"], "two Frame::Line values");
    assert_eq!(fake.seen()[0].header("accept"), Some("application/x-ndjson"));

    // A last record without its line end is not delivered and the turn is an error.
    let cut = b"{\"t\":\"a\"}\n{\"t\":\"b\"}".to_vec();
    let fake = FakeHttp::start(vec![Reply::sse_bytes(vec![cut]).content_type("application/x-ndjson")]);
    let (cut_provider, _) = provider_for(&fake, Wire::Ollama);
    let outcome = run(cut_provider.as_ref(), &request("hi"), &CancelToken::new());
    assert_eq!(outcome.texts(), ["a"], "the unterminated record is dropped");
    assert!(matches!(outcome.result, Err(AiError::Protocol(_))), "{:?}", outcome.result);
}

// ---- the contract tables ---------------------------------------------------------------------------------

#[test]
fn wire_tags_are_stable_and_choose_the_framing() {
    let tags: Vec<&str> = Wire::ALL.iter().map(|wire| wire.tag()).collect();
    assert_eq!(tags, ["openai_chat", "openai_responses", "anthropic", "gemini", "ollama"]);
    for wire in Wire::ALL {
        assert_eq!(Wire::from_tag(wire.tag()), Some(wire));
        assert_eq!(serde_json::to_string(&wire).expect("serialises"), format!("\"{}\"", wire.tag()));
        assert_eq!(serde_json::from_str::<Wire>(&format!("\"{}\"", wire.tag())).expect("deserialises"), wire);
        let expected = if wire == Wire::Ollama { Framing::Ndjson } else { Framing::Sse };
        assert_eq!(wire.framing(), expected, "{wire:?}");
    }
    assert_eq!(Wire::from_tag("OpenAI"), None);
    assert_eq!(Wire::from_tag(""), None);
}

#[test]
fn classify_ip_tells_loopback_private_and_public_addresses_apart() {
    let table: [(&str, Locality); 12] = [
        ("127.0.0.1", Locality::Loopback),
        ("127.9.9.9", Locality::Loopback),
        ("::1", Locality::Loopback),
        ("::ffff:127.0.0.1", Locality::Loopback),
        ("10.1.2.3", Locality::Lan),
        ("172.16.0.1", Locality::Lan),
        ("192.168.1.1", Locality::Lan),
        ("100.64.0.1", Locality::Lan),
        ("fd12::1", Locality::Lan),
        ("8.8.8.8", Locality::Cloud),
        ("172.32.0.1", Locality::Cloud),
        ("2606:4700::1111", Locality::Cloud),
    ];
    for (text, expected) in table {
        let ip = text.parse().expect("an address");
        assert_eq!(classify_ip(ip), Ok(expected), "{text}");
    }
}

#[test]
fn a_base_address_becomes_an_origin_and_a_base_path() {
    let target = Target::parse("https://API.Example.com:443/v1/", false).expect("accepted");
    assert_eq!(target.origin().scheme(), Scheme::Https);
    assert_eq!(target.origin().host(), "api.example.com");
    assert_eq!(target.origin().port(), 443);
    assert_eq!(target.base_path(), "/v1");

    let local = Target::parse("http://localhost:11434", false).expect("accepted");
    assert_eq!(local.origin().host(), "localhost");
    assert_eq!(local.origin().port(), 11434);
    assert_eq!(local.base_path(), "");

    let https_default = Target::parse("https://api.example.com", false).expect("accepted");
    assert_eq!(https_default.origin().port(), 443);
    let http_default = Target::parse("http://localhost", false).expect("accepted");
    assert_eq!(http_default.origin().port(), 80);
}
