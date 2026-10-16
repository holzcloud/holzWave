//! The HTTP chokepoint of the AI chat and the shared provider that drives a dialect through it, with no outside
//! network.
//!
//! Every request in this file goes to `FakeHttp`, an HTTP/1.1 server in this process that records what it was sent and
//! answers byte by byte as the test says. The dialect under test is `LineAdapter`, a tiny one defined here: it turns a
//! chat request into a JSON line and every received frame into a text delta, so what is proved is the chokepoint
//! (where a request may go, how it is bounded, retried and cancelled) and the driver, not any provider's JSON. Every
//! wait is bounded; a stalled server uses a ceiling of a few seconds so a defect fails an assertion instead of
//! hanging.

mod common;

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::fake_http::{Ending, FakeHttp, Reply};
use holzwave_core::ai::http::{
    Call, Exchange, Framing, Header, Http, HttpConfig, NetworkPolicy, Origin, Outcome, ProxyPolicy, Refusal, Resolver,
    Scheme, SecretHeader, Target, classify_ip, proxy_for,
};
use holzwave_core::ai::provider::{AiError, CancelToken, ChatRequest, Completion, Detail, Flow, Provider, StreamEvent};
use holzwave_core::ai::sse::StreamEnd;
use holzwave_core::ai::types::{ApiKey, Capabilities, Locality, Message, Role, StopReason, Usage};
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
    format!("POST {path} HTTP/1.1\r\nHost: test\r\nX-Probe: One\r\nContent-Length: {}\r\n\r\n{body}", body.len())
        .into_bytes()
}

/// The sizes and bytes of the HTTP chunks of a chunked answer, and whether the terminating chunk is there.
fn parse_chunked(answer: &[u8]) -> (Vec<Vec<u8>>, bool) {
    let split = answer.windows(4).position(|w| w == b"\r\n\r\n").expect("the head ends") + 4;
    let mut rest = &answer[split..];
    let mut chunks = Vec::new();
    loop {
        let Some(eol) = rest.windows(2).position(|w| w == b"\r\n") else { return (chunks, false) };
        let size =
            usize::from_str_radix(std::str::from_utf8(&rest[..eol]).expect("hex size").trim(), 16).expect("size");
        rest = &rest[eol + 2..];
        if size == 0 {
            return (chunks, true);
        }
        assert!(rest.len() >= size + 2, "a chunk is cut short");
        chunks.push(rest[..size].to_vec());
        rest = &rest[size + 2..];
    }
}

/// The key of the tests that move a key around. It appears in this file only.
const CANARY_KEY: &str = "sk-canary-0123456789abcdef";

/// A resolver with no DNS behind it: the answers are chosen by the test, one per lookup (the last repeats), and every
/// lookup is counted.
struct StubResolver {
    answers: Vec<Vec<IpAddr>>,
    calls: AtomicUsize,
    delay: Duration,
}

impl StubResolver {
    fn answering(answers: &[&[&str]]) -> Arc<StubResolver> {
        let answers = answers
            .iter()
            .map(|answer| answer.iter().map(|text| text.parse().expect("an address")).collect())
            .collect();
        Arc::new(StubResolver { answers, calls: AtomicUsize::new(0), delay: Duration::ZERO })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Resolver for StubResolver {
    fn lookup(&self, _host: &str) -> io::Result<Vec<IpAddr>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        Ok(self.answers[n.min(self.answers.len() - 1)].clone())
    }
}

/// A chokepoint with a stub resolver.
fn http_with(stub: &Arc<StubResolver>) -> Http {
    Http::with_resolver(HttpConfig::standard(), stub.clone())
}

/// One POST to `target` with nothing but the path `/v1/lines` and an empty JSON body.
fn post_to(http: &Http, target: &Target, policy: NetworkPolicy) -> Result<Exchange, AiError> {
    let call = Call::new(target, policy, "/v1/lines", b"{}", Framing::Sse);
    http.post_stream(&call, &mut |_| Flow::Continue, &CancelToken::new())
}

fn policy_phrase(error: &AiError) -> Option<&str> {
    match error {
        AiError::Policy(detail) => Some(detail.as_str()),
        _ => None,
    }
}

/// A short event stream that ends cleanly.
fn good_stream() -> Reply {
    Reply::sse(&["data: ok\n\n", "data: [DONE]\n\n"])
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

// ---- the destination rules --------------------------------------------------------------------------------

#[test]
fn classify_ip_refuses_link_local_metadata_unspecified_multicast_and_reserved_addresses() {
    let table: [(&str, Result<Locality, Refusal>); 19] = [
        ("169.254.169.254", Err(Refusal::Metadata)),
        ("169.254.1.1", Err(Refusal::LinkLocal)),
        ("fe80::1", Err(Refusal::LinkLocal)),
        ("fd00:ec2::254", Err(Refusal::Metadata)),
        ("100.100.100.200", Err(Refusal::Metadata)),
        ("168.63.129.16", Err(Refusal::Metadata)),
        ("0.0.0.0", Err(Refusal::Unspecified)),
        ("0.1.2.3", Err(Refusal::Unspecified)),
        ("::", Err(Refusal::Unspecified)),
        ("224.0.0.1", Err(Refusal::Multicast)),
        ("ff02::1", Err(Refusal::Multicast)),
        ("255.255.255.255", Err(Refusal::Reserved)),
        ("240.0.0.1", Err(Refusal::Reserved)),
        ("::ffff:169.254.169.254", Err(Refusal::Metadata)),
        ("64:ff9b::a9fe:a9fe", Err(Refusal::Metadata)),
        ("64:ff9b::808:808", Ok(Locality::Cloud)),
        ("::ffff:10.0.0.1", Ok(Locality::Lan)),
        ("fec0::1", Ok(Locality::Lan)),
        ("100.127.255.255", Ok(Locality::Lan)),
    ];
    for (text, expected) in table {
        let ip: IpAddr = text.parse().expect("an address");
        assert_eq!(classify_ip(ip), expected, "{text}");
    }
    // The edges of the carrier-grade and private ranges.
    for (text, expected) in [
        ("100.63.255.255", Locality::Cloud),
        ("100.128.0.0", Locality::Cloud),
        ("172.15.0.1", Locality::Cloud),
        ("192.169.0.1", Locality::Cloud),
    ] {
        assert_eq!(classify_ip(text.parse().expect("an address")), Ok(expected), "{text}");
    }
}

#[test]
fn the_base_address_table_accepts_and_refuses_with_stable_reasons() {
    let long = format!("https://host/{}", "a".repeat(2049 - "https://host/".len()));
    assert_eq!(long.len(), 2049);
    // (text, allow_lan_http, expected): Ok or the name of the refusal.
    let table: Vec<(String, bool, Result<(), Refusal>)> = vec![
        ("https://api.anthropic.com".into(), false, Ok(())),
        ("https://openrouter.ai/api/v1".into(), false, Ok(())),
        ("  https://api.example.com/v1/  ".into(), false, Ok(())),
        ("http://localhost:11434".into(), false, Ok(())),
        ("http://127.0.0.1:1234/v1".into(), false, Ok(())),
        ("http://[::1]:8080".into(), false, Ok(())),
        ("https://192.168.1.20".into(), false, Ok(())),
        ("http://192.168.1.20:8080".into(), true, Ok(())),
        ("http://box.lan:8080".into(), true, Ok(())),
        ("https://2130706433/".into(), false, Ok(())),
        ("ftp://x".into(), false, Err(Refusal::Scheme)),
        ("file:///etc/passwd".into(), false, Err(Refusal::Scheme)),
        ("javascript:alert(1)".into(), false, Err(Refusal::Scheme)),
        ("https://user:pw@host".into(), false, Err(Refusal::Credentials)),
        ("https://host/?a=1".into(), false, Err(Refusal::Query)),
        ("https://host/#f".into(), false, Err(Refusal::Query)),
        ("https://".into(), false, Err(Refusal::Syntax)),
        ("https://host:0".into(), false, Err(Refusal::Port)),
        ("https://ho st/".into(), false, Err(Refusal::Syntax)),
        (String::new(), false, Err(Refusal::Syntax)),
        (long, false, Err(Refusal::TooLong)),
        ("https://host/%2e%2e/x".into(), false, Err(Refusal::Path)),
        ("https://host/%2E%2E/x".into(), false, Err(Refusal::Path)),
        ("https://host/a%2Fb".into(), false, Err(Refusal::Path)),
        ("https://host/a%5cb".into(), false, Err(Refusal::Path)),
        ("https://host/a\\b".into(), false, Err(Refusal::Path)),
        ("http://api.example.com".into(), false, Err(Refusal::PlainHttp)),
        ("http://api.example.com".into(), true, Ok(())),
        ("http://192.168.1.20:8080".into(), false, Err(Refusal::PlainHttp)),
        ("http://203.0.113.7".into(), true, Err(Refusal::PlainHttp)),
        ("https://169.254.169.254/".into(), false, Err(Refusal::Metadata)),
        ("https://[fe80::1]/".into(), false, Err(Refusal::LinkLocal)),
        ("https://0.0.0.0/".into(), false, Err(Refusal::Unspecified)),
        ("https://224.0.0.1/".into(), false, Err(Refusal::Multicast)),
        ("https://255.255.255.255/".into(), false, Err(Refusal::Reserved)),
    ];
    for (text, allow_lan_http, expected) in table {
        let shown: String = text.chars().take(60).collect();
        let got = Target::parse(&text, allow_lan_http).map(|_| ());
        assert_eq!(got, expected, "{shown:?} with allow_lan_http {allow_lan_http}");
    }
    // The normalised numeric form is the loopback address.
    let numeric = Target::parse("https://2130706433/", false).expect("accepted");
    assert_eq!(numeric.origin().host(), "127.0.0.1");
    assert_eq!(numeric.literal_locality(), Some(Locality::Loopback));
    assert_eq!(Target::parse("https://api.example.com", false).expect("accepted").literal_locality(), None);
    assert_eq!(Target::parse("https://192.168.1.20", false).expect("accepted").literal_locality(), Some(Locality::Lan));
    let debug = format!("{:?} {:#?}", numeric, numeric);
    assert!(!debug.contains("/v1"), "Debug prints the origin only");
}

#[test]
fn refusal_names_are_stable_and_become_policy_errors_that_never_echo_the_input() {
    assert_eq!(Refusal::ALL.len(), 18);
    let mut names: Vec<&str> = Refusal::ALL.iter().map(|refusal| refusal.as_str()).collect();
    assert!(names.iter().all(|name| !name.is_empty()));
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 18, "the names are pairwise different");
    let expected = [
        "syntax",
        "too_long",
        "scheme",
        "credentials",
        "host",
        "port",
        "query",
        "path",
        "plain_http",
        "link_local",
        "metadata",
        "unspecified",
        "multicast",
        "reserved",
        "local_only",
        "key_scope",
        "key_in_url",
        "header",
    ];
    let in_order: Vec<&str> = Refusal::ALL.iter().map(|refusal| refusal.as_str()).collect();
    assert_eq!(in_order, expected);
    for refusal in Refusal::ALL {
        let phrase = refusal.to_string();
        assert!(!phrase.is_empty() && !phrase.contains("://"), "{refusal:?}: {phrase}");
        let error = AiError::from(refusal);
        assert_eq!(error.class(), holzwave_core::ai::provider::ErrorClass::Policy);
        assert_eq!(policy_phrase(&error), Some(phrase.as_str()), "{refusal:?}");
    }
    // A long hostile input is refused, and the refusal does not carry it.
    let hostile = format!("https://evil-marker.example/{}?token=SECRET-MARKER#frag", "x".repeat(300));
    let refusal = Target::parse(&hostile, false).expect_err("a query is refused");
    let text = format!("{refusal} {refusal:?} {}", AiError::from(refusal));
    assert!(!text.contains("evil-marker") && !text.contains("SECRET-MARKER") && !text.contains("xxx"), "{text}");
}

#[test]
fn the_permission_matrix_covers_every_scheme_locality_and_policy() {
    use Locality::{Cloud, Lan, Loopback};
    use Scheme::{Http, Https};
    // (scheme, locality, allow_lan_http, local_only, expected)
    let ok = Ok(());
    type Row = (Scheme, Locality, bool, bool, Result<(), Refusal>);
    let table: [Row; 24] = [
        (Https, Loopback, false, false, ok),
        (Https, Loopback, true, false, ok),
        (Https, Loopback, false, true, ok),
        (Https, Loopback, true, true, ok),
        (Https, Lan, false, false, ok),
        (Https, Lan, true, false, ok),
        (Https, Lan, false, true, Err(Refusal::LocalOnly)),
        (Https, Lan, true, true, Err(Refusal::LocalOnly)),
        (Https, Cloud, false, false, ok),
        (Https, Cloud, true, false, ok),
        (Https, Cloud, false, true, Err(Refusal::LocalOnly)),
        (Https, Cloud, true, true, Err(Refusal::LocalOnly)),
        (Http, Loopback, false, false, ok),
        (Http, Loopback, true, false, ok),
        (Http, Loopback, false, true, ok),
        (Http, Loopback, true, true, ok),
        (Http, Lan, false, false, Err(Refusal::PlainHttp)),
        (Http, Lan, true, false, ok),
        (Http, Lan, false, true, Err(Refusal::LocalOnly)),
        (Http, Lan, true, true, Err(Refusal::LocalOnly)),
        (Http, Cloud, false, false, Err(Refusal::PlainHttp)),
        (Http, Cloud, true, false, Err(Refusal::PlainHttp)),
        (Http, Cloud, false, true, Err(Refusal::LocalOnly)),
        (Http, Cloud, true, true, Err(Refusal::LocalOnly)),
    ];
    for (scheme, locality, allow_lan_http, local_only, expected) in table {
        let got = NetworkPolicy::new(allow_lan_http, local_only).permits(scheme, locality);
        assert_eq!(
            got, expected,
            "{scheme:?} to {locality:?}, allow_lan_http {allow_lan_http}, local_only {local_only}"
        );
    }
}

#[test]
fn an_answer_with_a_forbidden_address_is_refused_before_any_connection() {
    let fake = FakeHttp::start(vec![good_stream()]);
    let stub = StubResolver::answering(&[&["127.0.0.1", "169.254.169.254"]]);
    let http = http_with(&stub);
    let target = Target::parse(&format!("http://mixed.test:{}", fake.port()), true).expect("a name with the opt-in");

    let error = post_to(&http, &target, NetworkPolicy::new(true, false)).expect_err("a mixed answer is refused");

    assert_eq!(policy_phrase(&error), Some(Refusal::Metadata.to_string().as_str()), "{error:?}");
    assert_eq!(fake.accepted(), 0, "nothing connected");
    assert_eq!(http.attempts(), 0, "nothing was sent");
    assert_eq!(stub.calls(), 1);

    for (bad, refusal) in
        [("0.0.0.0", Refusal::Unspecified), ("fe80::1", Refusal::LinkLocal), ("224.0.0.9", Refusal::Multicast)]
    {
        let stub = StubResolver::answering(&[&[bad]]);
        let http = http_with(&stub);
        let error = post_to(&http, &target, NetworkPolicy::new(true, false)).expect_err("refused");
        assert_eq!(policy_phrase(&error), Some(refusal.to_string().as_str()), "{bad}");
    }
    assert_eq!(fake.accepted(), 0);
}

#[test]
fn plain_http_to_a_private_address_needs_the_opt_in_at_send_time() {
    // The address is unreachable, so a missing check would hang or time out instead of failing at once.
    let stub = StubResolver::answering(&[&["10.0.0.5"]]);
    let http = http_with(&stub);
    let target = Target::parse("http://box.test:8080", true).expect("a name with the opt-in");

    let started = Instant::now();
    let error = post_to(&http, &target, NetworkPolicy::new(false, false)).expect_err("plain http to the local network");
    assert!(started.elapsed() < Duration::from_secs(1), "refused at once, took {:?}", started.elapsed());
    assert_eq!(policy_phrase(&error), Some(Refusal::PlainHttp.to_string().as_str()), "{error:?}");
    assert_eq!(http.attempts(), 0);

    let resolved = http
        .classify(&target, &NetworkPolicy::new(true, false), &CancelToken::new())
        .expect("the opt-in passes the policy");
    assert_eq!(resolved.locality, Locality::Lan);
    assert_eq!(resolved.addrs, ["10.0.0.5:8080".parse::<SocketAddr>().expect("an address")]);
    assert_eq!(http.attempts(), 0, "classifying sends nothing");
}

#[test]
fn local_only_blocks_lan_and_cloud_and_allows_loopback() {
    let fake = FakeHttp::start(vec![good_stream()]);
    let local_only = NetworkPolicy::new(true, true);

    // The local network and the cloud are refused on the first resolution, before a connection.
    for (answer, name) in [("10.0.0.5", "lan.test"), ("8.8.8.8", "cloud.test")] {
        let stub = StubResolver::answering(&[&[answer]]);
        let http = http_with(&stub);
        let target = Target::parse(&format!("https://{name}"), false).expect("accepted");
        let error = post_to(&http, &target, local_only).expect_err("leaves the device");
        assert_eq!(policy_phrase(&error), Some(Refusal::LocalOnly.to_string().as_str()), "{name}: {error:?}");
        assert_eq!(http.attempts(), 0);
        let error = http.classify(&target, &local_only, &CancelToken::new()).expect_err("classify says the same");
        assert_eq!(policy_phrase(&error), Some(Refusal::LocalOnly.to_string().as_str()), "{name}");
    }
    // An IP literal is refused without any lookup.
    let stub = StubResolver::answering(&[&["127.0.0.1"]]);
    let http = http_with(&stub);
    let literal = Target::parse("https://192.168.1.20", false).expect("accepted");
    assert!(http.classify(&literal, &local_only, &CancelToken::new()).is_err());
    assert_eq!(stub.calls(), 0, "a literal is classified without a lookup");

    // The loopback address of the fake server is allowed, by literal and by a name that resolves to it.
    let by_literal = Target::parse(&fake.base(), false).expect("accepted");
    let exchange = post_to(&http, &by_literal, local_only).expect("loopback is allowed");
    assert_eq!(exchange.locality, Locality::Loopback);
    let by_name = Target::parse(&format!("http://where.test:{}", fake.port()), true).expect("accepted");
    let exchange = post_to(&http, &by_name, local_only).expect("a name that resolves to loopback is allowed");
    assert_eq!(exchange.locality, Locality::Loopback);
    assert_eq!(fake.accepted(), 2);
}

#[test]
fn a_resolved_name_is_pinned_and_looked_up_once_per_request() {
    let fake = FakeHttp::start(vec![good_stream()]);
    // The name has no DNS answer anywhere but in the stub, so a client that looked it up itself could not connect.
    let stub = StubResolver::answering(&[&["127.0.0.1"], &["192.0.2.55"]]);
    let http = http_with(&stub);
    let target = Target::parse(&format!("http://model.rebind.test:{}", fake.port()), true).expect("accepted");
    let policy = NetworkPolicy::new(true, false);

    let exchange = post_to(&http, &target, policy).expect("the pinned address is used");
    assert!(matches!(exchange.outcome, Outcome::Streamed(StreamEnd::Clean)), "{exchange:?}");
    let seen = fake.seen();
    assert_eq!(seen[0].header("host"), Some(format!("model.rebind.test:{}", fake.port()).as_str()));
    assert_eq!(stub.calls(), 1, "one lookup for one request: the client never looked the name up again");

    // The next request looks the name up again; the new answer is a different address, which is what it connects to.
    let second = post_to(&http, &target, policy);
    assert_eq!(stub.calls(), 2, "one lookup per request");
    assert_eq!(
        fake.accepted(),
        1,
        "the second answer was a different address, so the fake saw nothing more: {second:?}"
    );
}

#[test]
fn loopback_and_lan_use_no_proxy_and_cloud_follows_the_system() {
    assert_eq!(proxy_for(Locality::Loopback), ProxyPolicy::Direct);
    assert_eq!(proxy_for(Locality::Lan), ProxyPolicy::Direct);
    assert_eq!(proxy_for(Locality::OnDevice), ProxyPolicy::Direct);
    assert_eq!(proxy_for(Locality::Cloud), ProxyPolicy::System);
}

#[test]
fn a_secret_header_is_sent_only_to_its_own_origin() {
    let fake = FakeHttp::start(vec![good_stream()]);
    let http = Http::new(HttpConfig::standard());
    let target = Target::parse(&fake.base(), false).expect("accepted");
    let secret =
        SecretHeader::new("authorization", "Bearer ", ApiKey::new(CANARY_KEY.to_owned()), target.origin().clone())
            .expect("a secret header");
    let policy = NetworkPolicy::new(false, false);

    let call = Call::new(&target, policy, "/v1/lines", b"{}", Framing::Sse).secret(&secret);
    http.post_stream(&call, &mut |_| Flow::Continue, &CancelToken::new()).expect("sent to its own origin");

    let seen = fake.seen();
    assert_eq!(seen[0].header("authorization"), Some(format!("Bearer {CANARY_KEY}").as_str()));
    for (name, value) in &seen[0].headers {
        if name != "authorization" {
            assert!(!value.contains(CANARY_KEY), "the key is in the header {name}");
        }
    }
    assert!(!seen[0].target.contains(CANARY_KEY));

    // The same secret filed for another port is refused before anything is resolved or connected.
    let other =
        Origin::from_scope(&format!("http://127.0.0.1:{}", fake.port().wrapping_add(1).max(1))).expect("a scope");
    let foreign =
        SecretHeader::new("authorization", "Bearer ", ApiKey::new(CANARY_KEY.to_owned()), other).expect("built");
    let call = Call::new(&target, policy, "/v1/lines", b"{}", Framing::Sse).secret(&foreign);
    let error = http.post_stream(&call, &mut |_| Flow::Continue, &CancelToken::new()).expect_err("another origin");
    assert_eq!(policy_phrase(&error), Some(Refusal::KeyScope.to_string().as_str()), "{error:?}");
    assert_eq!(fake.accepted(), 1, "the refused call opened no connection");
    assert_eq!(http.attempts(), 1);

    // The key is marked sensitive and never printed.
    let text = format!("{secret:?} {secret:#?} {call:?}");
    assert!(!text.contains(CANARY_KEY), "{text}");
}

#[test]
fn a_key_inside_the_address_is_refused() {
    let fake = FakeHttp::start(vec![good_stream()]);
    let http = Http::new(HttpConfig::standard());
    let target = Target::parse(&fake.base(), false).expect("accepted");
    let secret = SecretHeader::new("x-goog-api-key", "", ApiKey::new(CANARY_KEY.to_owned()), target.origin().clone())
        .expect("a secret header");
    let policy = NetworkPolicy::new(false, false);

    let leaking = format!("/v1/lines?key={CANARY_KEY}");
    let call = Call::new(&target, policy, &leaking, b"{}", Framing::Sse).secret(&secret);
    let error =
        http.post_stream(&call, &mut |_| Flow::Continue, &CancelToken::new()).expect_err("a key in the address");
    assert_eq!(policy_phrase(&error), Some(Refusal::KeyInUrl.to_string().as_str()), "{error:?}");
    assert_eq!(fake.accepted(), 0);
    assert_eq!(http.attempts(), 0);
    assert!(!format!("{error} {error:?}").contains(CANARY_KEY));

    // The same key in a path segment.
    let leaking = format!("/v1/{CANARY_KEY}/lines");
    let call = Call::new(&target, policy, &leaking, b"{}", Framing::Sse).secret(&secret);
    assert!(http.post_stream(&call, &mut |_| Flow::Continue, &CancelToken::new()).is_err());

    // A short key is not looked for (it would match by accident), the endpoint masks the key in text it scrubs.
    let endpoint = endpoint(&fake, Wire::OpenAiChat).with_secret(
        SecretHeader::new("authorization", "Bearer ", ApiKey::new(CANARY_KEY.to_owned()), target.origin().clone())
            .expect("a secret header"),
    );
    let scrubbed = endpoint.scrub(&format!("invalid api key {CANARY_KEY} for this account\n\u{1b}[31m"));
    assert!(!scrubbed.as_str().contains(CANARY_KEY), "{}", scrubbed.as_str());
    assert!(scrubbed.as_str().contains("<redacted>"), "{}", scrubbed.as_str());
    assert!(!scrubbed.as_str().contains('\u{1b}'));
}

#[test]
fn header_names_and_values_are_validated() {
    let refused_names = [
        "host",
        "content-length",
        "transfer-encoding",
        "connection",
        "accept-encoding",
        "content-type",
        "accept",
        "user-agent",
        "authorization",
        "x-api-key",
        "x-goog-api-key",
        "cf-aig-authorization",
        "cookie",
        "set-cookie",
        "x-auth-token",
        "x-secret",
        "x-password",
        "",
        "bad name",
        "bad_name",
        "ünï",
    ];
    for name in refused_names {
        assert_eq!(Header::new(name, "v").map(|_| ()), Err(Refusal::Header), "{name:?}");
    }
    assert_eq!(Header::new(&"a".repeat(65), "v").map(|_| ()), Err(Refusal::Header), "65 characters");
    assert!(Header::new(&"a".repeat(64), "v").is_ok(), "64 characters");
    let lowered = Header::new("Anthropic-Version", "2023-06-01").expect("allowed");
    assert_eq!(lowered.name(), "anthropic-version");
    assert!(Header::new("x-title", "holzWave").is_ok());

    for value in ["a\nb", "a\rb", "a\0b", "caf\u{e9}", "a\u{7f}b", "a\tb"] {
        assert_eq!(Header::new("x-title", value).map(|_| ()), Err(Refusal::Header), "{value:?}");
    }
    assert!(Header::new("x-title", &"v".repeat(8192)).is_ok());
    assert_eq!(Header::new("x-title", &"v".repeat(8193)).map(|_| ()), Err(Refusal::Header));

    let origin = Origin::from_scope("https://api.example.com:443").expect("a scope");
    let key = || ApiKey::new(CANARY_KEY.to_owned());
    assert!(SecretHeader::new("x-api-key", "", key(), origin.clone()).is_ok());
    assert!(SecretHeader::new("cf-aig-authorization", "Bearer ", key(), origin.clone()).is_ok());
    assert!(SecretHeader::new("authorization", "Bearer ", key(), origin.clone()).is_ok());
    assert_eq!(SecretHeader::new("host", "", key(), origin.clone()).map(|_| ()), Err(Refusal::Header));
    assert_eq!(SecretHeader::new("content-length", "", key(), origin.clone()).map(|_| ()), Err(Refusal::Header));
    assert_eq!(
        SecretHeader::new("x-api-key", "", ApiKey::new(String::new()), origin.clone()).map(|_| ()),
        Err(Refusal::Header),
        "an empty key"
    );
    assert_eq!(
        SecretHeader::new("x-api-key", "", ApiKey::new("a\nb".to_owned()), origin).map(|_| ()),
        Err(Refusal::Header),
        "a key with a line break"
    );
}

#[test]
fn the_origin_scope_round_trips() {
    for scope in [
        "https://api.example.com:443",
        "http://localhost:11434",
        "http://127.0.0.1:1234",
        "https://[::1]:8443",
        "http://[fd12::1]:80",
        "https://box.lan:8080",
    ] {
        let origin = Origin::from_scope(scope).expect(scope);
        assert_eq!(origin.scope(), scope);
        assert_eq!(Origin::from_scope(&origin.scope()), Ok(origin));
    }
    // The scope a target writes is the scope a profile reads back.
    for text in ["https://API.Example.com/v1/", "http://[::1]:8080", "https://2130706433/", "http://localhost"] {
        let target = Target::parse(text, false).expect(text);
        let scope = target.origin().scope();
        assert_eq!(Origin::from_scope(&scope).as_ref(), Ok(target.origin()), "{text} -> {scope}");
    }
    assert_eq!(
        Target::parse("https://api.example.com", false).expect("accepted").origin().scope(),
        "https://api.example.com:443"
    );
    for bad in [
        "https://host:443/path",
        "https://user@host:443",
        "https://host",
        "ftp://host:21",
        "https://host:0",
        "https://host:99999",
        "https://:443",
        "https://host:",
        "https://[::1]",
        "https://[nope]:443",
        "host:443",
        "https://host:443?x",
        "https://host :443",
        "",
    ] {
        assert!(Origin::from_scope(bad).is_err(), "{bad:?}");
    }
}
