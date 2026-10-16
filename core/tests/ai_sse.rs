//! The framing layer of the AI core, with no network.
//!
//! Every provider dialect arrives as a byte stream that the network cuts at arbitrary places. These tests feed golden
//! streams to the bounded decoders in `holzwave_core::ai::sse` and prove that the chunk boundaries do not change the
//! result, that only whole events are dispatched, that every size is bounded and an overrun fails closed, that nothing
//! can reconnect, and that stream text never reaches a `Debug` line. A failing case names the chunk size or the split
//! point in its message.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use holzwave_core::ai::provider::{AiError, Collector, ErrorClass, Flow, StreamEvent};
use holzwave_core::ai::sse::{
    Discarded, FrameError, Limits, MAX_EVENT, MAX_LINE, MAX_TOOL_ARGS, MAX_TOOL_CALLS, MAX_TOOL_FIELD, NdjsonDecoder,
    SseDecoder, StreamEnd, ToolAssembler, ToolFault,
};
use holzwave_core::ai::types::{ToolCall, Usage};
use serde_json::{Value, json};

const CANARY: &str = "CANARY-7f3a";

// ---- golden streams and helpers -------------------------------------------------------------------------

/// The basic stream: a keepalive comment, then text, usage and a terminal event, with 2-, 3- and 4-byte characters.
fn golden_basic() -> Vec<u8> {
    let mut s = String::new();
    s.push_str(": keepalive\n\n");
    s.push_str("event: delta\ndata: {\"t\":\"Hel\"}\n\n");
    s.push_str("event: delta\ndata: {\"t\":\"lo \u{e9} \u{65e5}\u{672c} \u{1f600}\"}\n\n");
    s.push_str("event: usage\ndata: {\"in\":12,\"out\":3}\n\n");
    s.push_str("event: done\ndata: {}\n\n");
    s.into_bytes()
}

/// Every line feed becomes a carriage return and a line feed.
fn to_crlf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 2);
    for &b in bytes {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

/// Every line feed becomes a carriage return.
fn to_cr(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|&b| if b == b'\n' { b'\r' } else { b }).collect()
}

/// Line feeds become LF, CRLF and CR in turn.
fn to_mixed(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 2);
    let mut n = 0usize;
    for &b in bytes {
        if b == b'\n' {
            match n % 3 {
                0 => out.push(b'\n'),
                1 => out.extend_from_slice(b"\r\n"),
                _ => out.push(b'\r'),
            }
            n += 1;
        } else {
            out.push(b);
        }
    }
    out
}

fn chunks_of(bytes: &[u8], size: usize) -> Vec<&[u8]> {
    bytes.chunks(size).collect()
}

/// What `decode` saw.
struct Decoded {
    /// `(event, data)` of every event, in order.
    events: Vec<(String, String)>,
    /// The result of the last push (after a failure every push repeats the error).
    last: Result<Flow, FrameError>,
    end: StreamEnd,
}

fn decode(chunks: &[&[u8]], limits: Limits) -> Decoded {
    let mut decoder = SseDecoder::with_limits(limits);
    let mut events = Vec::new();
    let mut last = Ok(Flow::Continue);
    for chunk in chunks {
        last = decoder.push(chunk, &mut |event| {
            events.push((event.event().to_owned(), event.data().to_owned()));
            Flow::Continue
        });
    }
    Decoded { events, last, end: decoder.finish() }
}

/// The events of a whole byte string pushed in one chunk.
fn events_of(bytes: &[u8]) -> Vec<(String, String)> {
    decode(&[bytes], Limits::DEFAULT).events
}

fn ev(event: &str, data: &str) -> (String, String) {
    (event.to_owned(), data.to_owned())
}

/// What the mini adapter made of a stream.
struct Adapted {
    collector: Collector,
    terminal: u32,
    end: StreamEnd,
}

/// A mini adapter in the shape of a wire adapter: `delta` is text, `usage` is the token totals, `done` is the terminal
/// event. It pushes into a `Collector` like the real ones will.
fn adapt(chunks: &[&[u8]]) -> Adapted {
    let mut decoder = SseDecoder::new();
    let mut collector = Collector::new();
    let mut terminal = 0u32;
    for chunk in chunks {
        decoder
            .push(chunk, &mut |event| match event.event() {
                "delta" => {
                    let v: Value = serde_json::from_str(event.data()).expect("delta data is JSON");
                    collector.push(StreamEvent::TextDelta(v["t"].as_str().expect("t is text").to_owned()))
                }
                "usage" => {
                    let v: Value = serde_json::from_str(event.data()).expect("usage data is JSON");
                    collector.push(StreamEvent::Usage(Usage {
                        input_tokens: v["in"].as_u64().expect("in") as u32,
                        output_tokens: v["out"].as_u64().expect("out") as u32,
                    }))
                }
                "done" => {
                    terminal += 1;
                    Flow::Continue
                }
                _ => Flow::Continue,
            })
            .expect("a golden stream decodes");
    }
    Adapted { collector, terminal, end: decoder.finish() }
}

fn assert_adapted(a: &Adapted, how: &str) {
    assert_eq!(a.collector.text(), "Hello \u{e9} \u{65e5}\u{672c} \u{1f600}", "text, {how}");
    assert_eq!(a.collector.usage(), Usage { input_tokens: 12, output_tokens: 3 }, "usage, {how}");
    assert_eq!(a.terminal, 1, "terminal event seen once, {how}");
    assert_eq!(a.end, StreamEnd::Clean, "the stream ended cleanly, {how}");
}

// ---- the tracer -----------------------------------------------------------------------------------------

#[test]
fn a_golden_stream_in_odd_chunks_reaches_the_collector_as_one_text() {
    let bytes = golden_basic();
    assert_adapted(&adapt(&[&bytes]), "unsplit");
    for size in [1usize, 2, 3, 5, 7, 64] {
        assert_adapted(&adapt(&chunks_of(&bytes, size)), &format!("chunks of {size} bytes"));
    }
}

// ---- line endings and fields ----------------------------------------------------------------------------

#[test]
fn lf_crlf_and_cr_line_endings_decode_to_the_same_events() {
    let lf = golden_basic();
    let want = events_of(&lf);
    assert_eq!(want.len(), 4, "the basic stream holds four events");
    for (name, bytes) in [("crlf", to_crlf(&lf)), ("cr", to_cr(&lf)), ("mixed", to_mixed(&lf))] {
        assert_eq!(events_of(&bytes), want, "{name}, one chunk");
        for size in [1usize, 2, 3, 5, 7, 64] {
            assert_eq!(decode(&chunks_of(&bytes, size), Limits::DEFAULT).events, want, "{name}, chunks of {size} bytes");
        }
    }

    // A chunk boundary between the CR and the LF of any line end never makes a phantom blank line.
    let crlf = b"data: a\r\ndata: b\r\n\r\n";
    for at in 0..=crlf.len() {
        let got = decode(&[&crlf[..at], &crlf[at..]], Limits::DEFAULT);
        assert_eq!(got.events, vec![ev("message", "a\nb")], "split at {at}");
        assert_eq!(got.end, StreamEnd::Clean, "split at {at}");
    }

    // An empty chunk between the CR and the LF says nothing: the LF still belongs to the CR.
    let got = decode(&[b"data: a\r", b"", b"\n", b"", b"\r", b"", b"\n"], Limits::DEFAULT);
    assert_eq!(got.events, vec![ev("message", "a")]);
    assert_eq!(got.end, StreamEnd::Clean);

    // Two carriage returns and a line feed are two line ends: the second one is a blank line.
    let double = b"data: a\r\r\ndata: b\r\n\r\n";
    assert_eq!(events_of(double), vec![ev("message", "a"), ev("message", "b")]);
    for at in 0..=double.len() {
        let got = decode(&[&double[..at], &double[at..]], Limits::DEFAULT);
        assert_eq!(got.events, vec![ev("message", "a"), ev("message", "b")], "split at {at}");
    }
}

#[test]
fn data_lines_join_with_a_line_feed_and_fields_follow_the_event_stream_rules() {
    assert_eq!(events_of(b"data: a\ndata: b\n\n"), vec![ev("message", "a\nb")]);
    assert_eq!(events_of(b"data:x\n\n"), vec![ev("message", "x")]);
    assert_eq!(events_of(b"data:  x\n\n"), vec![ev("message", " x")], "one space is stripped, not two");
    assert_eq!(events_of(b"data\n\n"), vec![ev("message", "")], "no colon: empty data");
    assert_eq!(events_of(b"data:\n\n"), vec![ev("message", "")]);
    assert_eq!(events_of(b"data: a\ndata\ndata: b\n\n"), vec![ev("message", "a\n\nb")]);
    assert_eq!(events_of(b"event: tick\ndata: 1\n\ndata: 2\n\n"), vec![ev("tick", "1"), ev("message", "2")]);
    assert_eq!(
        events_of(b"event: lost\n\ndata: 3\n\n"),
        vec![ev("message", "3")],
        "an event name without data is dropped and does not leak"
    );
    assert_eq!(events_of(b"event: a\nevent: b\ndata: 1\n\n"), vec![ev("b", "1")], "the last name wins");
    assert!(events_of(b"DATA: x\n\n").is_empty(), "field names are case sensitive");
    assert!(events_of(b"Event: x\ndata: y\n\n") == vec![ev("message", "y")], "a name in other case is ignored");

    // is_done is the data line `[DONE]` and nothing else.
    for (bytes, done) in [
        (&b"data: [DONE]\n\n"[..], true),
        (&b"data:[DONE]\n\n"[..], true),
        (&b"data: [DONE] \n\n"[..], false),
        (&b"data: [done]\n\n"[..], false),
        (&b"data: x\ndata: [DONE]\n\n"[..], false),
        (&b"data: x\n\n"[..], false),
    ] {
        let mut seen = None;
        let mut decoder = SseDecoder::new();
        decoder
            .push(bytes, &mut |event| {
                seen = Some(event.is_done());
                Flow::Continue
            })
            .unwrap();
        assert_eq!(seen, Some(done), "is_done for {:?}", String::from_utf8_lossy(bytes));
    }
}

#[test]
fn comments_ids_retries_and_unknown_fields_dispatch_nothing_and_change_nothing() {
    let ignored = b": comment\n:\nid: 7\nid\nid: a\x00b\nretry: 3000\nretry: x\nretry\nfoo: bar\nfoo\n\n";
    assert!(events_of(ignored).is_empty(), "ignored lines and their blank line dispatch nothing");

    let mut stream = ignored.to_vec();
    stream.extend_from_slice(b"data: z\n\n");
    for size in [stream.len(), 1, 2, 3, 7] {
        let got = decode(&chunks_of(&stream, size), Limits::DEFAULT);
        assert_eq!(got.events, vec![ev("message", "z")], "chunks of {size} bytes");
        assert_eq!(got.end, StreamEnd::Clean, "chunks of {size} bytes");
    }

    // Between the lines of one event they change nothing either.
    assert_eq!(
        events_of(b"event: e\n: c\nid: 1\nretry: 5\nfoo: bar\ndata: q\n\n"),
        vec![ev("e", "q")]
    );
    assert_eq!(events_of(b"data: a\n\n: c\nid: 1\nretry: 5\nfoo: bar\n\ndata: z\n\n"), vec![ev("message", "a"), ev("message", "z")]);
}

// ---- stop, end of stream --------------------------------------------------------------------------------

#[test]
fn a_stop_from_the_callback_ends_the_decode_and_later_pushes_are_ignored() {
    let mut decoder = SseDecoder::new();
    let mut seen = Vec::new();
    let result = decoder.push(b"data: 1\n\ndata: 2\n\ndata: 3\n\n", &mut |event| {
        seen.push(event.data().to_owned());
        if seen.len() == 2 { Flow::Stop } else { Flow::Continue }
    });
    assert_eq!(result, Ok(Flow::Stop));
    assert_eq!(seen, vec!["1", "2"], "the third event was not looked at");
    let mut later = 0;
    let again = decoder.push(b"data: 4\n\n", &mut |_| {
        later += 1;
        Flow::Continue
    });
    assert_eq!(again, Ok(Flow::Stop), "a later push answers Stop");
    assert_eq!(later, 0, "a later push delivers nothing");
    assert_eq!(decoder.buffered(), 0);
    assert_eq!(decoder.finish(), StreamEnd::Stopped);
}

#[test]
fn an_unfinished_event_is_never_dispatched_and_finish_says_so() {
    let case = |bytes: &[u8]| decode(&[bytes], Limits::DEFAULT);

    let clean = case(b"data: a\n\n");
    assert_eq!(clean.events, vec![ev("message", "a")]);
    assert_eq!(clean.end, StreamEnd::Clean);

    let cut = case(b"data: a\n\ndata: b\n");
    assert_eq!(cut.events, vec![ev("message", "a")], "the event without its blank line is not delivered");
    assert!(matches!(cut.end, StreamEnd::Truncated { discarded } if discarded > 0), "{:?}", cut.end);

    let partial_comment = case(b": pi");
    assert!(partial_comment.events.is_empty());
    assert!(matches!(partial_comment.end, StreamEnd::Truncated { discarded } if discarded > 0));

    let name_only = case(b"event: x\n");
    assert!(name_only.events.is_empty());
    assert!(matches!(name_only.end, StreamEnd::Truncated { discarded } if discarded > 0));

    let partial_data = case(b"data: a");
    assert!(partial_data.events.is_empty(), "a line without its line end is not an event");
    assert!(matches!(partial_data.end, StreamEnd::Truncated { discarded } if discarded > 0));

    assert_eq!(case(b": a complete comment\n").end, StreamEnd::Clean);
    assert_eq!(case(b"").end, StreamEnd::Clean, "no bytes at all");
    assert_eq!(decode(&[], Limits::DEFAULT).end, StreamEnd::Clean, "no push at all");
}

// ---- limits ---------------------------------------------------------------------------------------------

#[test]
fn a_line_or_an_event_over_its_limit_fails_closed_and_stays_failed() {
    let limits = Limits { line: 16, event: 32 };
    let case = |bytes: &[u8]| decode(&[bytes], limits);

    // Exactly at the line limit passes, one more fails.
    let ok = case(b"data: 1234567890\n\n");
    assert_eq!(ok.events, vec![ev("message", "1234567890")], "16 bytes pass");
    assert_eq!(ok.last, Ok(Flow::Continue));
    let over = case(b"data: 12345678901\n\n");
    assert_eq!(over.last, Err(FrameError::LineTooLong { limit: 16 }), "17 bytes fail");
    assert!(over.events.is_empty());

    // It fails as soon as the 17th byte arrives, without waiting for a line end.
    let mut decoder = SseDecoder::with_limits(limits);
    assert_eq!(decoder.push(b"data: 1234567890", &mut |_| Flow::Continue), Ok(Flow::Continue));
    assert_eq!(decoder.push(b"1", &mut |_| Flow::Continue), Err(FrameError::LineTooLong { limit: 16 }));
    assert_eq!(decoder.buffered(), 0, "the buffers are released");
    assert_eq!(decoder.capacity(), 0);

    // The event limit counts the data and one byte per data line.
    let four = b"data: 1234567\ndata: 1234567\ndata: 1234567\ndata: 1234567\n";
    let mut decoder = SseDecoder::with_limits(limits);
    assert_eq!(decoder.push(four, &mut |_| Flow::Continue), Ok(Flow::Continue), "four lines of 8 retained bytes pass");
    assert_eq!(decoder.buffered(), 32);
    assert_eq!(
        decoder.push(b"data: 1234567\n", &mut |_| Flow::Continue),
        Err(FrameError::EventTooLarge { limit: 32 }),
        "the fifth fails"
    );

    // The event name counts toward the event limit too.
    let tight = Limits { line: 64, event: 8 };
    assert_eq!(decode(&[b"event: abcdefghi\n"], tight).last, Err(FrameError::EventTooLarge { limit: 8 }));
    assert_eq!(decode(&[b"event: abcdefgh\n\n"], tight).last, Ok(Flow::Continue));

    // An event delivered earlier in the same chunk as the failure was delivered.
    let got = case(b"data: a\n\ndata: 12345678901\n\ndata: b\n\n");
    assert_eq!(got.events, vec![ev("message", "a")]);
    assert_eq!(got.last, Err(FrameError::LineTooLong { limit: 16 }));

    // After a failure the decoder stays failed: the same error, no event, nothing buffered.
    let mut decoder = SseDecoder::with_limits(limits);
    let first = decoder.push(b"data: 12345678901\n", &mut |_| Flow::Continue);
    assert_eq!(first, Err(FrameError::LineTooLong { limit: 16 }));
    let mut seen = 0;
    let second = decoder.push(b"data: a\n\n", &mut |_| {
        seen += 1;
        Flow::Continue
    });
    assert_eq!(second, first, "the same error again");
    assert_eq!(seen, 0, "no event after a failure");
    assert_eq!(decoder.buffered(), 0);
    assert!(matches!(decoder.finish(), StreamEnd::Truncated { .. }));

    // The defaults are the documented sizes.
    assert_eq!(MAX_LINE, 1_048_576);
    assert_eq!(MAX_EVENT, 4_194_304);
    assert_eq!(Limits::DEFAULT, Limits { line: MAX_LINE, event: MAX_EVENT });
}

// ---- UTF-8 ----------------------------------------------------------------------------------------------

#[test]
fn invalid_utf8_in_an_event_fails_closed_but_a_split_character_is_fine() {
    let mut bad_data = b"data: ".to_vec();
    bad_data.extend_from_slice(&[0xFF, 0xFE]);
    bad_data.extend_from_slice(b"\n\n");
    let got = decode(&[&bad_data], Limits::DEFAULT);
    assert_eq!(got.last, Err(FrameError::InvalidUtf8));
    assert!(got.events.is_empty(), "no event from invalid bytes");

    let mut bad_name = b"event: ".to_vec();
    bad_name.extend_from_slice(&[0xC3, 0x28]);
    bad_name.extend_from_slice(b"\ndata: x\n\n");
    let got = decode(&[&bad_name], Limits::DEFAULT);
    assert_eq!(got.last, Err(FrameError::InvalidUtf8), "an invalid event name fails the same way");
    assert!(got.events.is_empty());

    // The failure is final: a good event after it is not delivered.
    let mut tail = bad_data.clone();
    tail.extend_from_slice(b"data: fine\n\n");
    let got = decode(&[&tail], Limits::DEFAULT);
    assert_eq!(got.last, Err(FrameError::InvalidUtf8));
    assert!(got.events.is_empty());

    // Invalid bytes of lines that are ignored do not matter.
    let mut ignored = Vec::new();
    for line in [&b": "[..], b"id: ", b"retry: ", b"foo: "] {
        ignored.extend_from_slice(line);
        ignored.extend_from_slice(&[0xFF, 0xC0, 0x80]);
        ignored.push(b'\n');
    }
    ignored.extend_from_slice(b"data: ok\n\n");
    let got = decode(&[&ignored], Limits::DEFAULT);
    assert_eq!(got.events, vec![ev("message", "ok")]);
    assert_eq!(got.last, Ok(Flow::Continue));

    // A character split across chunks is decoded whole, at each of the three interior points of four bytes.
    let stream = "data: a\u{1f600}b\n\n".as_bytes();
    let start = 7; // the first byte of the 4-byte character
    for inner in 1..=3 {
        let at = start + inner;
        let got = decode(&[&stream[..at], &stream[at..]], Limits::DEFAULT);
        assert_eq!(got.events, vec![ev("message", "a\u{1f600}b")], "split inside the character at byte {at}");
        assert_eq!(got.last, Ok(Flow::Continue), "split at byte {at}");
    }
}

// ---- Debug ----------------------------------------------------------------------------------------------

#[test]
fn decoder_and_event_debug_show_sizes_never_stream_text() {
    // A pending data line and a pending event name both hold the canary.
    let mut decoder = SseDecoder::new();
    decoder.push(format!("event: {CANARY}\ndata: {CANARY}\n").as_bytes(), &mut |_| Flow::Continue).unwrap();
    decoder.push(format!("data: {CANARY}").as_bytes(), &mut |_| Flow::Continue).unwrap();
    assert!(decoder.buffered() > 0);
    for shown in [format!("{decoder:?}"), format!("{decoder:#?}")] {
        assert!(!shown.contains(CANARY), "the decoder leaks stream text: {shown}");
        assert!(shown.contains("bytes"), "the decoder shows sizes: {shown}");
    }

    // The event inside the callback.
    let mut decoder = SseDecoder::new();
    let mut shown = Vec::new();
    decoder
        .push(format!("event: {CANARY}\ndata: {CANARY}\n\n").as_bytes(), &mut |event| {
            assert_eq!(event.data(), CANARY, "the accessors do give the text");
            shown.push(format!("{event:?}"));
            shown.push(format!("{event:#?}"));
            Flow::Continue
        })
        .unwrap();
    assert_eq!(shown.len(), 2);
    for text in shown {
        assert!(!text.contains(CANARY), "the event leaks stream text: {text}");
        assert!(text.contains("bytes"), "the event shows sizes: {text}");
    }
}

// ---- the independent reference decoders -----------------------------------------------------------------

/// What a reference decoder made of a whole byte string.
struct Reference {
    events: Vec<(String, String)>,
    /// A partial line, undispatched data or a pending event name was left over.
    pending: bool,
}

/// A second implementation of the event-stream rules that reads the whole byte string at once and shares no code with
/// the crate: replace every CRLF pair by LF and then every remaining CR by LF, split at LF, treat the text after the
/// last LF as the partial line, drop one leading byte order mark from the first completed line, then apply the fields.
fn reference_sse(bytes: &[u8]) -> Reference {
    let mut norm = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            norm.push(b'\n');
            if bytes.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
        } else {
            norm.push(bytes[i]);
        }
        i += 1;
    }
    let mut lines: Vec<&[u8]> = norm.split(|&b| b == b'\n').collect();
    let partial = lines.pop().expect("split yields at least one piece");
    if let Some(first) = lines.first_mut()
        && first.starts_with(&[0xEF, 0xBB, 0xBF])
    {
        *first = &first[3..];
    }
    let mut events = Vec::new();
    let mut data: Vec<u8> = Vec::new();
    let mut name: Vec<u8> = Vec::new();
    let mut data_seen = false;
    for line in lines {
        if line.is_empty() {
            if data_seen {
                data.pop();
                let event = if name.is_empty() { "message".to_owned() } else { String::from_utf8(name.clone()).unwrap() };
                events.push((event, String::from_utf8(data.clone()).unwrap()));
            }
            data.clear();
            name.clear();
            data_seen = false;
        } else if line[0] == b':' {
            // a comment
        } else {
            let (field, value): (&[u8], &[u8]) = match line.iter().position(|&b| b == b':') {
                Some(at) => {
                    let value = &line[at + 1..];
                    (&line[..at], if value.first() == Some(&b' ') { &value[1..] } else { value })
                }
                None => (line, &[]),
            };
            if field == b"data" {
                data.extend_from_slice(value);
                data.push(b'\n');
                data_seen = true;
            } else if field == b"event" {
                name = value.to_vec();
            }
        }
    }
    Reference { events, pending: !partial.is_empty() || data_seen || !name.is_empty() }
}

/// The same for newline-delimited records: a line feed ends a record, one carriage return in front of it is dropped,
/// empty records are skipped, and the text after the last line feed is pending.
fn reference_ndjson(bytes: &[u8]) -> (Vec<Vec<u8>>, bool) {
    let mut lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    let partial = lines.pop().expect("split yields at least one piece");
    let records = lines
        .into_iter()
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .filter(|line| !line.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    (records, !partial.is_empty())
}

/// The golden streams: named byte strings, each in LF, CRLF, CR and mixed form where that is meaningful.
fn goldens() -> Vec<(String, Vec<u8>)> {
    let basic = golden_basic();
    let chat = b": keepalive\n\ndata: {\"c\":\"Hel\"}\n\ndata: {\"c\":\"lo\"}\n\ndata: [DONE]\n\n".to_vec();
    let fields = b"id: 1\nretry: 100\nevent: m\ndata: one\ndata: two\n\nfoo\ndata:  spaced\n\nevent: e\ndata\n\ndata:\n\nevent: lost\n\ndata: z\n\n".to_vec();
    let chars = "data: \u{e9}x\u{65e5}y\u{1f600}\n\ndata: a\u{e9}b\u{65e5}c\u{1f600}d\n\nevent: \u{e9}\ndata: z\u{1f600}\n\n"
        .as_bytes()
        .to_vec();
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(b": c\ndata: a\n\ndata: b\n\n");
    let mut out = Vec::new();
    for (name, lf) in [("basic", basic), ("chat", chat), ("fields", fields), ("chars", chars), ("bom", bom)] {
        out.push((format!("{name}/crlf"), to_crlf(&lf)));
        out.push((format!("{name}/cr"), to_cr(&lf)));
        out.push((format!("{name}/mixed"), to_mixed(&lf)));
        out.push((format!("{name}/lf"), lf));
    }
    out
}

/// Fails with the stream name and the way it was split unless the incremental decoder equals the reference.
fn assert_like_reference(name: &str, bytes: &[u8], chunks: &[&[u8]], how: &str) {
    let want = reference_sse(bytes);
    let got = decode(chunks, Limits::DEFAULT);
    assert_eq!(got.last, Ok(Flow::Continue), "{name}: {how}");
    assert_eq!(got.events, want.events, "{name}: {how}");
    match got.end {
        StreamEnd::Clean => assert!(!want.pending, "{name}: {how}: clean, but the reference has something pending"),
        StreamEnd::Truncated { discarded } => {
            assert!(want.pending && discarded > 0, "{name}: {how}: truncated ({discarded}) but the reference is clean")
        }
        StreamEnd::Stopped => panic!("{name}: {how}: stopped without a Stop"),
    }
}

/// A small seeded generator (xorshift), so a failure can be replayed from the seed in its message.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

// ---- split at every offset ------------------------------------------------------------------------------

#[test]
fn every_golden_stream_split_at_every_offset_decodes_like_the_reference() {
    let mut streams = 0;
    let mut splits = 0;
    let mut expected = 0;
    for (name, bytes) in goldens() {
        streams += 1;
        expected += bytes.len() + 1;
        assert_like_reference(&name, &bytes, &[&bytes], "unsplit");
        for at in 0..=bytes.len() {
            assert_like_reference(&name, &bytes, &[&bytes[..at], &bytes[at..]], &format!("split at {at}"));
            splits += 1;
        }
    }
    println!("split at every offset: {streams} golden streams, {splits} split points");
    assert_eq!(streams, 20, "five stories in four line-ending forms");
    assert_eq!(splits, expected, "every offset from 0 to the length was tried");
}

#[test]
fn every_pair_of_split_points_on_the_small_streams_decodes_like_the_reference() {
    let mut streams = 0;
    let mut pairs = 0;
    for (name, bytes) in goldens().into_iter().filter(|(_, bytes)| bytes.len() < 150) {
        streams += 1;
        for a in 0..=bytes.len() {
            for b in a..=bytes.len() {
                assert_like_reference(&name, &bytes, &[&bytes[..a], &bytes[a..b], &bytes[b..]], &format!("splits at {a} and {b}"));
                pairs += 1;
            }
        }
    }
    println!("every pair of split points: {streams} golden streams, {pairs} pairs");
    assert!(streams >= 10 && pairs > 10_000);
}

#[test]
fn single_byte_chunks_and_seeded_random_chunks_decode_like_the_reference() {
    let seeds: [u64; 5] = [1, 42, 2024, 0x9E37_79B9_7F4A_7C15, 0xDEAD_BEEF_CAFE_F00D];
    let mut runs = 0;
    for (name, bytes) in goldens() {
        assert_like_reference(&name, &bytes, &chunks_of(&bytes, 1), "one byte at a time");
        for seed in seeds {
            let mut rng = XorShift(seed);
            for round in 0..40 {
                let mut chunks = Vec::new();
                let mut at = 0;
                while at < bytes.len() {
                    let len = 1 + (rng.next() % 40) as usize;
                    let end = (at + len).min(bytes.len());
                    chunks.push(&bytes[at..end]);
                    at = end;
                }
                assert_like_reference(&name, &bytes, &chunks, &format!("seed {seed}, round {round}"));
                runs += 1;
            }
        }
    }
    println!("random chunk lengths 1 to 40: {runs} runs over {} seeds", seeds.len());
}

#[test]
fn a_stream_cut_at_any_byte_delivers_only_whole_events_and_finish_reports_the_cut() {
    let mut cuts = 0;
    for (name, bytes) in goldens() {
        let full = reference_sse(&bytes);
        for len in 0..=bytes.len() {
            let prefix = &bytes[..len];
            let want = reference_sse(prefix);
            assert!(full.events.starts_with(&want.events), "{name}: the reference of a prefix is a prefix, cut at {len}");
            for size in [prefix.len().max(1), 3] {
                let got = decode(&chunks_of(prefix, size), Limits::DEFAULT);
                assert_eq!(got.events, want.events, "{name}: cut at {len}, chunks of {size}");
                assert!(full.events.starts_with(&got.events), "{name}: cut at {len} delivered something that is not a prefix");
                match got.end {
                    StreamEnd::Clean => assert!(!want.pending, "{name}: cut at {len}: clean with something pending"),
                    StreamEnd::Truncated { discarded } => {
                        assert!(want.pending && discarded > 0, "{name}: cut at {len}: truncated without anything pending")
                    }
                    StreamEnd::Stopped => panic!("{name}: cut at {len}: stopped"),
                }
            }
            cuts += 1;
        }
    }
    println!("cut at every byte: {cuts} prefixes");
}

// ---- the byte order mark --------------------------------------------------------------------------------

#[test]
fn a_byte_order_mark_is_dropped_at_the_start_only_even_when_split() {
    let bom = [0xEF, 0xBB, 0xBF];
    let with = |rest: &[u8]| {
        let mut v = bom.to_vec();
        v.extend_from_slice(rest);
        v
    };

    // Dropped, one chunk and byte by byte.
    let stream = with(b"data: a\n\n");
    assert_eq!(events_of(&stream), vec![ev("message", "a")]);
    assert_eq!(decode(&chunks_of(&stream, 1), Limits::DEFAULT).events, vec![ev("message", "a")]);
    // Dropped when the three bytes arrive in separate chunks.
    let parts: [&[u8]; 4] = [&stream[0..1], &stream[1..2], &stream[2..3], &stream[3..]];
    let got = decode(&parts, Limits::DEFAULT);
    assert_eq!(got.events, vec![ev("message", "a")]);
    assert_eq!(got.end, StreamEnd::Clean);
    // Dropped when the first line is a comment, or blank.
    assert_eq!(events_of(&with(b": c\ndata: a\n\n")), vec![ev("message", "a")]);
    assert_eq!(events_of(&with(b"\ndata: a\n\n")), vec![ev("message", "a")]);

    // After the first line the same bytes are text.
    let late = events_of("data: a\n\ndata: \u{feff}b\n\n".as_bytes());
    assert_eq!(late, vec![ev("message", "a"), ev("message", "\u{feff}b")]);
    assert!(late[1].1.contains('\u{feff}'));
    // At the start of a later line they spoil the field name, so that line is ignored.
    assert_eq!(events_of("data: a\n\n\u{feff}data: b\n\n".as_bytes()), vec![ev("message", "a")]);
    // Only one is dropped.
    let two = with("\u{feff}data: x\n\n".as_bytes());
    assert!(events_of(&two).is_empty(), "a second mark spoils the name of the first line");

    // A stream that is only the three bytes holds a partial line.
    let only = decode(&[&bom], Limits::DEFAULT);
    assert!(only.events.is_empty());
    assert_eq!(only.end, StreamEnd::Truncated { discarded: 3 });
    let partial = decode(&[&bom[..2]], Limits::DEFAULT);
    assert!(matches!(partial.end, StreamEnd::Truncated { discarded: 2 }));
}

// ---- hostile input --------------------------------------------------------------------------------------

#[test]
fn floods_of_comments_and_blank_lines_hold_no_memory_and_dispatch_nothing() {
    let comments = ": x\n".repeat(1_000_000).into_bytes();
    let blanks = vec![b'\n'; 1_000_000];
    for (name, flood) in [("comment lines", comments), ("blank lines", blanks)] {
        let mut decoder = SseDecoder::new();
        let mut seen = 0usize;
        let mut worst = 0usize;
        for chunk in flood.chunks(64 * 1024) {
            decoder
                .push(chunk, &mut |_| {
                    seen += 1;
                    Flow::Continue
                })
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            worst = worst.max(decoder.buffered());
        }
        assert_eq!(seen, 0, "{name} dispatch nothing");
        assert!(worst <= 64, "{name} held {worst} bytes");
        assert!(decoder.capacity() <= 4096, "{name} reserved {} bytes", decoder.capacity());
        assert_eq!(decoder.finish(), StreamEnd::Clean, "{name}");
    }
}

#[test]
fn a_megabyte_line_in_single_byte_chunks_costs_linear_time() {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let started = Instant::now();
        let payload = MAX_LINE - 6; // "data: " is six bytes, so the line is exactly the default limit
        let mut line = b"data: ".to_vec();
        line.resize(MAX_LINE, b'a');
        let mut decoder = SseDecoder::new();
        let mut events = Vec::new();
        for byte in line.chunks(1) {
            decoder
                .push(byte, &mut |_| Flow::Continue)
                .expect("a line at the limit passes");
        }
        let result = decoder.push(b"\n\n", &mut |event| {
            events.push(event.data().len());
            Flow::Continue
        });
        let _ = tx.send((result, events, payload, started.elapsed(), decoder.finish()));
    });
    let (result, events, payload, took, end) =
        rx.recv_timeout(Duration::from_secs(30)).expect("a quadratic scan would not finish within 30 seconds");
    println!("a 1 MiB line, one byte per push: {took:?}");
    assert_eq!(result, Ok(Flow::Continue));
    assert_eq!(events, vec![payload], "one event of 1 MiB minus 6 bytes");
    assert_eq!(end, StreamEnd::Clean);
}

#[test]
fn buffers_stop_growing_after_warm_up() {
    let mut stream = Vec::new();
    let mut boundaries = Vec::new();
    for i in 0..2000 {
        stream.extend_from_slice(format!("event: delta\ndata: {{\"t\":\"x{i:04}\"}}\n\n").as_bytes());
        boundaries.push(stream.len());
    }
    let mut decoder = SseDecoder::new();
    let mut seen = 0usize;
    let mut position = 0usize;
    let mut capacity = 0usize;
    for pass in 0..3 {
        for chunk in stream.chunks(997) {
            decoder
                .push(chunk, &mut |_| {
                    seen += 1;
                    Flow::Continue
                })
                .unwrap();
            position += chunk.len();
            // The offsets of the later passes are shifted by the length of the stream, so look the boundary up.
            let at = position % stream.len();
            if at == 0 || boundaries.binary_search(&at).is_ok() {
                assert_eq!(decoder.buffered(), 0, "pass {pass}: at an event boundary (offset {at}) nothing is held");
            }
        }
        assert_eq!(decoder.buffered(), 0, "pass {pass}: the stream ends at an event boundary");
        if pass == 0 {
            capacity = decoder.capacity();
            assert!(capacity > 0);
        } else {
            assert_eq!(decoder.capacity(), capacity, "pass {pass}: the buffers grew after warm-up");
        }
    }
    println!("capacity after warm-up: {capacity} bytes");
    assert_eq!(seen, 6000);
}

// ---- newline-delimited records --------------------------------------------------------------------------

/// What the NDJSON decoder made of some chunks.
struct DecodedLines {
    records: Vec<Vec<u8>>,
    last: Result<Flow, FrameError>,
    end: StreamEnd,
}

fn decode_lines(decoder: NdjsonDecoder, chunks: &[&[u8]]) -> DecodedLines {
    let mut decoder = decoder;
    let mut records = Vec::new();
    let mut last = Ok(Flow::Continue);
    for chunk in chunks {
        last = decoder.push(chunk, &mut |line| {
            records.push(line.to_vec());
            Flow::Continue
        });
    }
    DecodedLines { records, last, end: decoder.finish() }
}

#[test]
fn ndjson_lines_split_at_every_offset_decode_like_the_reference() {
    let lf = b"{\"a\":1}\n{\"b\":\"\xc3\xa9 \xf0\x9f\x98\x80\"}\n{\"c\":[1,2,3]}\n".to_vec();
    let blanks = b"{\"a\":1}\n\n\r\n\r\r\n{\"b\":2}\n".to_vec();
    let mut streams = vec![("lf", lf.clone()), ("crlf", to_crlf(&lf)), ("mixed", to_mixed(&lf)), ("blanks", blanks)];
    streams.push(("cut", b"{\"a\":1}\n{\"b\":".to_vec()));
    streams.push(("lone cr", b"{\"a\":1}\r{\"b\":2}\n".to_vec()));
    let mut splits = 0;
    for (name, bytes) in streams {
        let (want, pending) = reference_ndjson(&bytes);
        for at in 0..=bytes.len() {
            let got = decode_lines(NdjsonDecoder::new(), &[&bytes[..at], &bytes[at..]]);
            assert_eq!(got.records, want, "{name}: split at {at}");
            assert_eq!(got.last, Ok(Flow::Continue), "{name}: split at {at}");
            match got.end {
                StreamEnd::Clean => assert!(!pending, "{name}: split at {at}"),
                StreamEnd::Truncated { discarded } => assert!(pending && discarded > 0, "{name}: split at {at}"),
                StreamEnd::Stopped => panic!("{name}: stopped"),
            }
            splits += 1;
        }
        let got = decode_lines(NdjsonDecoder::new(), &chunks_of(&bytes, 1));
        assert_eq!(got.records, want, "{name}: one byte at a time");
    }
    println!("ndjson split at every offset: {splits} split points");
    // One carriage return is dropped in front of a line feed, so `\r\r\n` leaves a record that holds one.
    let (records, _) = reference_ndjson(b"{\"a\":1}\n\n\r\n\r\r\n{\"b\":2}\n");
    assert_eq!(records, vec![b"{\"a\":1}".to_vec(), b"\r".to_vec(), b"{\"b\":2}".to_vec()]);
}

#[test]
fn ndjson_enforces_its_bound_skips_blank_lines_reports_a_cut_record_and_hides_its_text() {
    let record = |n: usize| "a".repeat(n).into_bytes();
    let with_end = |n: usize, end: &[u8]| {
        let mut v = record(n);
        v.extend_from_slice(end);
        v
    };

    // Blank lines, and a line that holds only a carriage return, are skipped.
    let got = decode_lines(NdjsonDecoder::new(), &[b"\n\r\n{\"a\":1}\n\n\r\n"]);
    assert_eq!(got.records, vec![b"{\"a\":1}".to_vec()]);
    assert_eq!(got.end, StreamEnd::Clean);

    // Exactly at the bound passes, one more fails: with a line feed, with a CRLF, and before the line end arrives.
    let got = decode_lines(NdjsonDecoder::with_limit(16), &[&with_end(16, b"\n")]);
    assert_eq!(got.records, vec![record(16)], "16 bytes pass");
    let got = decode_lines(NdjsonDecoder::with_limit(16), &[&with_end(16, b"\r\n")]);
    assert_eq!(got.records, vec![record(16)], "16 bytes and a CRLF pass");
    let got = decode_lines(NdjsonDecoder::with_limit(16), &[&with_end(16, b"\r"), b"\n"]);
    assert_eq!(got.records, vec![record(16)], "the carriage return may arrive before the line feed");
    let got = decode_lines(NdjsonDecoder::with_limit(16), &[&with_end(17, b"\n")]);
    assert_eq!(got.last, Err(FrameError::LineTooLong { limit: 16 }), "17 bytes fail");
    assert!(got.records.is_empty());
    let got = decode_lines(NdjsonDecoder::with_limit(16), &[&with_end(17, b"\r\n")]);
    assert_eq!(got.last, Err(FrameError::LineTooLong { limit: 16 }), "17 bytes and a CRLF fail");

    // It fails before the line end arrives, and then stays failed with nothing held.
    let mut decoder = NdjsonDecoder::with_limit(16);
    assert_eq!(decoder.push(&record(16), &mut |_| Flow::Continue), Ok(Flow::Continue));
    assert_eq!(decoder.push(b"a", &mut |_| Flow::Continue), Err(FrameError::LineTooLong { limit: 16 }));
    assert_eq!(decoder.buffered(), 0);
    assert_eq!(decoder.capacity(), 0);
    let mut seen = 0;
    assert_eq!(
        decoder.push(b"{}\n", &mut |_| {
            seen += 1;
            Flow::Continue
        }),
        Err(FrameError::LineTooLong { limit: 16 }),
        "the same error again"
    );
    assert_eq!(seen, 0);
    assert!(matches!(decoder.finish(), StreamEnd::Truncated { .. }));
    // A record is bounded by the default too.
    assert_eq!(NdjsonDecoder::new().push(&vec![b'a'; MAX_EVENT + 1], &mut |_| Flow::Continue), Err(FrameError::LineTooLong { limit: MAX_EVENT }));

    // A last record without a line end was not delivered and is reported as a cut.
    let got = decode_lines(NdjsonDecoder::new(), &[b"{\"a\":1}\n{\"b\":2}"]);
    assert_eq!(got.records, vec![b"{\"a\":1}".to_vec()]);
    assert_eq!(got.end, StreamEnd::Truncated { discarded: 7 });
    assert_eq!(decode_lines(NdjsonDecoder::new(), &[]).end, StreamEnd::Clean);

    // A Stop stops the stream.
    let mut decoder = NdjsonDecoder::new();
    let mut seen = 0;
    let result = decoder.push(b"1\n2\n3\n", &mut |_| {
        seen += 1;
        if seen == 2 { Flow::Stop } else { Flow::Continue }
    });
    assert_eq!(result, Ok(Flow::Stop));
    assert_eq!(seen, 2);
    assert_eq!(decoder.push(b"4\n", &mut |_| panic!("delivered after a stop")), Ok(Flow::Stop));
    assert_eq!(decoder.buffered(), 0);
    assert_eq!(decoder.finish(), StreamEnd::Stopped);

    // Debug shows sizes, never the pending record.
    let mut decoder = NdjsonDecoder::new();
    decoder.push(format!("{{\"k\":\"{CANARY}").as_bytes(), &mut |_| Flow::Continue).unwrap();
    assert!(decoder.buffered() > 0);
    for shown in [format!("{decoder:?}"), format!("{decoder:#?}")] {
        assert!(!shown.contains(CANARY), "leaks stream text: {shown}");
        assert!(shown.contains("bytes"), "shows sizes: {shown}");
    }
}

// ---- speed ----------------------------------------------------------------------------------------------

#[test]
#[ignore = "measures speed and prints it; run with --ignored --nocapture, preferably with --release"]
fn decoder_throughput_is_measured() {
    const EVENTS: usize = 200_000;
    let stream = "data: {\"t\":\"x\"}\n\n".repeat(EVENTS).into_bytes();
    for chunk_size in [16_384usize, 7] {
        let mut decoder = SseDecoder::new();
        let mut seen = 0usize;
        let started = Instant::now();
        for chunk in stream.chunks(chunk_size) {
            decoder
                .push(chunk, &mut |_| {
                    seen += 1;
                    Flow::Continue
                })
                .unwrap();
        }
        let secs = started.elapsed().as_secs_f64();
        assert_eq!(seen, EVENTS);
        assert_eq!(decoder.finish(), StreamEnd::Clean);
        println!(
            "sse throughput: {:.1} MiB/s, {:.0} events/s ({chunk_size} byte chunks)",
            stream.len() as f64 / (1024.0 * 1024.0) / secs,
            EVENTS as f64 / secs
        );
    }
}

// ---- tool calls -----------------------------------------------------------------------------------------

/// The golden tool stream: one call opened, its arguments in two fragments cut in the middle of a key, completed, and
/// the terminal event.
fn golden_tools() -> Vec<u8> {
    let mut s = String::new();
    s.push_str("event: open\ndata: {\"slot\":0,\"id\":\"toolu_1\",\"name\":\"read_file\"}\n\n");
    s.push_str("event: args\ndata: {\"slot\":0,\"json\":\"{\\\"pa\"}\n\n");
    s.push_str("event: args\ndata: {\"slot\":0,\"json\":\"th\\\":\\\"a b\\\"}\"}\n\n");
    s.push_str("event: close\ndata: {\"slot\":0}\n\n");
    s.push_str("event: end\ndata: {}\n\n");
    s.into_bytes()
}

/// What the tool adapter made of a stream.
struct ToolRun {
    /// What `finish` of the assembler said.
    outcome: Result<Vec<ToolCall>, Discarded>,
    /// The collector, filled only from a released vector.
    collector: Collector,
    /// `ready_count()` when the terminal event arrived.
    ready_at_end: Option<usize>,
    decoder_end: StreamEnd,
}

/// The mini adapter of the tracer, extended: `open`, `args` and `close` drive the assembler, `end` is the terminal
/// event, and tool calls reach the collector only from the vector `finish` returns.
fn adapt_tools(chunks: &[&[u8]]) -> ToolRun {
    let mut decoder = SseDecoder::new();
    let mut tools = ToolAssembler::new();
    let mut terminal = false;
    let mut ready_at_end = None;
    for chunk in chunks {
        let _ = decoder.push(chunk, &mut |event| {
            let v: Value = serde_json::from_str(event.data()).expect("event data is JSON");
            let slot = v["slot"].as_u64().unwrap_or(0) as u32;
            match event.event() {
                "open" => tools.fragment(slot, v["id"].as_str(), v["name"].as_str(), "").expect("open"),
                "args" => tools.fragment(slot, None, None, v["json"].as_str().expect("json")).expect("args"),
                "close" => tools.complete(slot).expect("close"),
                "end" => {
                    terminal = true;
                    ready_at_end = Some(tools.ready_count());
                }
                _ => {}
            }
            Flow::Continue
        });
    }
    let decoder_end = decoder.finish();
    let outcome = tools.finish(decoder_end, terminal);
    let mut collector = Collector::new();
    if let Ok(calls) = &outcome {
        for call in calls {
            collector.push(StreamEvent::ToolCall(call.clone()));
        }
    }
    ToolRun { outcome, collector, ready_at_end, decoder_end }
}

fn read_file_call() -> ToolCall {
    ToolCall { id: "toolu_1".into(), name: "read_file".into(), args: json!({"path": "a b"}) }
}

fn assert_released(run: &ToolRun, how: &str) {
    assert_eq!(run.decoder_end, StreamEnd::Clean, "{how}");
    assert_eq!(run.outcome.as_ref().expect("a clean stream releases its call"), &vec![read_file_call()], "{how}");
    assert_eq!(run.collector.tool_calls(), &[read_file_call()], "{how}");
    assert_eq!(run.ready_at_end, Some(1), "the call was held, complete, when the terminal event arrived: {how}");
}

#[test]
fn fragments_complete_into_a_parsed_call_that_only_a_clean_finish_releases() {
    let bytes = golden_tools();
    assert_released(&adapt_tools(&[&bytes]), "unsplit");
    for at in 0..=bytes.len() {
        assert_released(&adapt_tools(&[&bytes[..at], &bytes[at..]]), &format!("split at {at}"));
    }
    for size in [1usize, 3, 7] {
        assert_released(&adapt_tools(&chunks_of(&bytes, size)), &format!("chunks of {size} bytes"));
    }

    // Held, not available: after the completion the call is ready, and the assembler hands it out only at finish.
    let mut tools = ToolAssembler::new();
    tools.fragment(0, Some("toolu_1"), Some("read_file"), "{\"path\":").unwrap();
    assert_eq!((tools.open_count(), tools.ready_count()), (1, 0));
    tools.fragment(0, None, None, "\"a b\"}").unwrap();
    tools.complete(0).unwrap();
    assert_eq!((tools.open_count(), tools.ready_count()), (0, 1));
    let released = tools.finish(StreamEnd::Clean, true).unwrap();
    assert_eq!(released, vec![read_file_call()]);
}

#[test]
fn a_stream_cut_at_any_byte_before_its_end_delivers_no_tool_call() {
    let bytes = golden_tools();
    for len in 0..bytes.len() {
        let prefix = &bytes[..len];
        for size in [len.max(1), 3] {
            let run = adapt_tools(&chunks_of(prefix, size));
            assert!(run.outcome.is_err(), "a call was released from a stream cut at byte {len}, chunks of {size}");
            assert!(run.collector.tool_calls().is_empty(), "the collector holds a call: cut at byte {len}");
        }
    }
}

#[test]
fn a_clean_connection_without_the_terminal_event_discards_the_calls() {
    let bytes = golden_tools();
    // The connection closes at an event boundary after the call was completed, but before the terminal event.
    let end = bytes.windows(b"event: end".len()).position(|w| w == b"event: end").expect("the end event");
    let run = adapt_tools(&[&bytes[..end]]);
    assert_eq!(run.decoder_end, StreamEnd::Clean, "the framing saw nothing wrong");
    assert_eq!(run.outcome, Err(Discarded { dropped: 1 }), "the call is discarded without the terminal event");
    assert!(run.collector.tool_calls().is_empty());

    // The terminal event arrives with a call still open (no close).
    let close = bytes.windows(b"event: close".len()).position(|w| w == b"event: close").expect("the close event");
    let mut open_at_end = bytes[..close].to_vec();
    open_at_end.extend_from_slice(&bytes[end..]);
    let run = adapt_tools(&[&open_at_end]);
    assert_eq!(run.decoder_end, StreamEnd::Clean);
    assert_eq!(run.outcome, Err(Discarded { dropped: 1 }), "an open call is discarded");
    assert!(run.collector.tool_calls().is_empty());

    // A stopped stream and a failed one are not clean either.
    let mut tools = ToolAssembler::new();
    tools.whole(Some("a"), "x", json!({})).unwrap();
    assert_eq!(tools.finish(StreamEnd::Stopped, true), Err(Discarded { dropped: 1 }));
    let mut tools = ToolAssembler::new();
    tools.whole(Some("a"), "x", json!({})).unwrap();
    assert_eq!(tools.finish(StreamEnd::Truncated { discarded: 4 }, true), Err(Discarded { dropped: 1 }));
    let mut tools = ToolAssembler::new();
    tools.whole(Some("a"), "x", json!({})).unwrap();
    assert_eq!(tools.finish(StreamEnd::Clean, false), Err(Discarded { dropped: 1 }));
    // A turn without any call and a clean end is simply empty.
    assert_eq!(ToolAssembler::new().finish(StreamEnd::Clean, true), Ok(Vec::new()));
}

#[test]
fn invalid_arguments_and_bad_fields_are_faults() {
    let complete = |args: &str| {
        let mut tools = ToolAssembler::new();
        tools.fragment(0, Some("id"), Some("tool"), args).unwrap();
        tools.complete(0).map(|()| tools.finish(StreamEnd::Clean, true).unwrap().remove(0).args)
    };
    assert_eq!(complete("{\"pa"), Err(ToolFault::InvalidArgs), "cut arguments");
    for args in ["[1]", "\"x\"", "null", "3", "true", "{\"a\":1} x", "{'a':1}"] {
        assert_eq!(complete(args), Err(ToolFault::InvalidArgs), "arguments {args}");
    }
    assert_eq!(complete(""), Ok(json!({})), "empty arguments");
    assert_eq!(complete(" \n\t "), Ok(json!({})), "white space only");
    assert_eq!(complete(" {\"a\":{\"b\":[1,{\"c\":null}]}} "), Ok(json!({"a": {"b": [1, {"c": null}]}})), "nested values");

    // No name at completion.
    let mut tools = ToolAssembler::new();
    tools.fragment(0, Some("id"), None, "{}").unwrap();
    assert_eq!(tools.complete(0), Err(ToolFault::BadField));
    // Control characters in an id or a name.
    for bad in ["a\nb", "a\u{1b}[31mb", "a\0b", "\u{7f}"] {
        let mut tools = ToolAssembler::new();
        assert_eq!(tools.fragment(0, None, Some(bad), ""), Err(ToolFault::BadField), "name {bad:?}");
        let mut tools = ToolAssembler::new();
        assert_eq!(tools.fragment(0, Some(bad), Some("tool"), ""), Err(ToolFault::BadField), "id {bad:?}");
    }
    // A call that arrives in one piece.
    let mut tools = ToolAssembler::new();
    assert_eq!(tools.whole(None, "", json!({})), Err(ToolFault::BadField), "an empty name");
    assert_eq!(tools.whole(None, "a\nb", json!({})), Err(ToolFault::BadField));
    assert_eq!(tools.whole(Some("a\nb"), "x", json!({})), Err(ToolFault::BadField));
    assert_eq!(tools.whole(None, "x", json!([1])), Err(ToolFault::InvalidArgs));
    assert_eq!(tools.whole(None, "x", Value::Null), Err(ToolFault::InvalidArgs));

    // After any fault a clean finish still discards.
    let mut tools = ToolAssembler::new();
    tools.whole(Some("good"), "x", json!({})).unwrap();
    assert_eq!(tools.whole(None, "", json!({})), Err(ToolFault::BadField));
    assert!(tools.finish(StreamEnd::Clean, true).is_err(), "a fault poisons the turn");
}

#[test]
fn empty_arguments_become_an_object_and_missing_ids_are_minted_uniquely() {
    // A provider id is kept verbatim, an empty id counts as absent.
    let mut tools = ToolAssembler::new();
    tools.fragment(0, Some("toolu_Ab-9"), Some("a"), "").unwrap();
    tools.fragment(1, Some(""), Some("b"), "").unwrap();
    tools.fragment(2, None, Some("c"), "").unwrap();
    tools.complete_all().unwrap();
    let calls = tools.finish(StreamEnd::Clean, true).unwrap();
    assert_eq!(calls[0].id, "toolu_Ab-9");
    assert_eq!(calls[0].args, json!({}), "empty arguments become an empty object");
    for call in &calls[1..] {
        assert!(call.id.starts_with("call_"), "{}", call.id);
        assert_eq!(call.id.len(), "call_".len() + 16 + 1 + 1, "the nonce in sixteen hex digits, an underscore, the counter");
        let nonce = &call.id["call_".len()..];
        assert!(nonce.chars().take(16).all(|c| c.is_ascii_hexdigit()), "{}", call.id);
        assert_eq!(nonce.as_bytes()[16], b'_', "{}", call.id);
    }

    // Four minted ids from two assemblers are pairwise different, also across turns and restarts.
    let mut ids = Vec::new();
    for _ in 0..2 {
        let mut tools = ToolAssembler::new();
        tools.whole(None, "x", json!({})).unwrap();
        tools.fragment(7, None, Some("y"), "").unwrap();
        tools.complete(7).unwrap();
        ids.extend(tools.finish(StreamEnd::Clean, true).unwrap().into_iter().map(|c| c.id));
    }
    assert_eq!(ids.len(), 4);
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 4, "minted ids repeat: {ids:?}");
}

#[test]
fn bounds_hold_for_arguments_calls_and_fields() {
    assert_eq!(MAX_TOOL_ARGS, 4_194_304);
    assert_eq!(MAX_TOOL_CALLS, 64);
    assert_eq!(MAX_TOOL_FIELD, 256);

    // Arguments: 16 bytes in total across fragments pass, the 17th fails.
    let mut tools = ToolAssembler::with_limits(16, 2);
    tools.fragment(0, Some("a"), Some("n"), "0123456789").unwrap();
    tools.fragment(0, None, None, "012345").unwrap();
    assert_eq!(tools.fragment(0, None, None, "0"), Err(ToolFault::TooLarge), "17 bytes");
    assert!(tools.finish(StreamEnd::Clean, true).is_err());
    let mut tools = ToolAssembler::with_limits(16, 2);
    tools.fragment(0, Some("a"), Some("n"), "0123456789").unwrap();
    assert_eq!(tools.fragment(1, Some("b"), Some("n"), "0123456"), Err(ToolFault::TooLarge), "the total spans calls");

    // Calls: a third call fails, whether the others are open, completed or whole.
    let mut tools = ToolAssembler::with_limits(MAX_TOOL_ARGS, 2);
    tools.fragment(0, Some("a"), Some("n"), "").unwrap();
    tools.fragment(1, Some("b"), Some("n"), "").unwrap();
    assert_eq!(tools.fragment(2, Some("c"), Some("n"), ""), Err(ToolFault::TooMany));
    let mut tools = ToolAssembler::with_limits(MAX_TOOL_ARGS, 2);
    tools.fragment(0, Some("a"), Some("n"), "").unwrap();
    tools.complete(0).unwrap();
    tools.whole(Some("b"), "n", json!({})).unwrap();
    assert_eq!(tools.fragment(2, Some("c"), Some("n"), ""), Err(ToolFault::TooMany), "completed calls count");
    assert_eq!(tools.whole(Some("d"), "n", json!({})), Err(ToolFault::TooMany), "whole calls count");
    let mut tools = ToolAssembler::new();
    for slot in 0..MAX_TOOL_CALLS as u32 {
        tools.fragment(slot, None, Some("n"), "").unwrap();
    }
    assert_eq!(tools.fragment(MAX_TOOL_CALLS as u32, None, Some("n"), ""), Err(ToolFault::TooMany), "the default is 64");

    // Fields: 256 bytes pass, 257 fail.
    let mut tools = ToolAssembler::new();
    tools.fragment(0, Some(&"i".repeat(256)), Some(&"n".repeat(256)), "").unwrap();
    let mut tools = ToolAssembler::new();
    assert_eq!(tools.fragment(0, Some(&"i".repeat(257)), Some("n"), ""), Err(ToolFault::TooLarge));
    let mut tools = ToolAssembler::new();
    assert_eq!(tools.fragment(0, None, Some(&"n".repeat(257)), ""), Err(ToolFault::TooLarge));
    let mut tools = ToolAssembler::new();
    assert_eq!(tools.whole(None, &"n".repeat(257), json!({})), Err(ToolFault::TooLarge));

    // The size of a call that arrives whole counts toward the arguments too.
    let mut tools = ToolAssembler::with_limits(16, 4);
    assert_eq!(tools.whole(None, "n", json!({"key": "0123456789abcdef"})), Err(ToolFault::TooLarge));
}

#[test]
fn a_conflicting_name_or_id_a_late_fragment_and_an_unknown_slot_are_faults() {
    // The same value again is fine, an empty value counts as absent.
    let mut tools = ToolAssembler::new();
    tools.fragment(0, Some("a"), Some("n"), "").unwrap();
    tools.fragment(0, Some("a"), Some("n"), "").unwrap();
    tools.fragment(0, Some(""), Some(""), "").unwrap();
    tools.complete(0).unwrap();
    assert!(tools.finish(StreamEnd::Clean, true).is_ok());

    // A different non-empty name or id for an open slot is a conflict.
    let mut tools = ToolAssembler::new();
    tools.fragment(0, Some("a"), Some("n"), "").unwrap();
    assert_eq!(tools.fragment(0, None, Some("other"), ""), Err(ToolFault::Conflict));
    let mut tools = ToolAssembler::new();
    tools.fragment(0, Some("a"), Some("n"), "").unwrap();
    assert_eq!(tools.fragment(0, Some("other"), None, ""), Err(ToolFault::Conflict));
    // Two calls with the same provider id could bind an approval to the wrong call.
    let mut tools = ToolAssembler::new();
    tools.fragment(0, Some("same"), Some("n"), "").unwrap();
    assert_eq!(tools.fragment(1, Some("same"), Some("n"), ""), Err(ToolFault::Conflict));
    let mut tools = ToolAssembler::new();
    tools.whole(Some("same"), "n", json!({})).unwrap();
    assert_eq!(tools.whole(Some("same"), "n", json!({})), Err(ToolFault::Conflict));

    // A fragment for a slot that is complete, and a completion of a slot never opened.
    let mut tools = ToolAssembler::new();
    tools.fragment(0, Some("a"), Some("n"), "{}").unwrap();
    tools.complete(0).unwrap();
    assert_eq!(tools.fragment(0, None, None, "x"), Err(ToolFault::ClosedSlot));
    assert_eq!(tools.complete(0), Err(ToolFault::ClosedSlot), "completed twice");
    assert_eq!(tools.complete(9), Err(ToolFault::UnknownSlot));
    assert!(tools.finish(StreamEnd::Clean, true).is_err(), "after a fault even a clean end discards");
}

#[test]
fn parallel_calls_keep_the_order_in_which_they_were_completed() {
    let fill = |tools: &mut ToolAssembler| {
        tools.fragment(1, Some("b"), Some("second"), "{\"k\"").unwrap();
        tools.fragment(0, Some("a"), Some("first"), "{\"k\"").unwrap();
        tools.fragment(1, None, None, ":2}").unwrap();
        tools.fragment(0, None, None, ":1}").unwrap();
    };
    let mut tools = ToolAssembler::new();
    fill(&mut tools);
    tools.complete(0).unwrap();
    tools.complete(1).unwrap();
    let names: Vec<String> = tools.finish(StreamEnd::Clean, true).unwrap().into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["first", "second"], "the order of completion");

    let mut tools = ToolAssembler::new();
    fill(&mut tools);
    tools.complete_all().unwrap();
    let calls = tools.finish(StreamEnd::Clean, true).unwrap();
    let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["second", "first"], "complete_all follows the order opened");
    assert_eq!(calls[0].args, json!({"k": 2}));
    assert_eq!(calls[1].args, json!({"k": 1}));

    // Whole calls keep their arrival order, among completed ones too.
    let mut tools = ToolAssembler::new();
    tools.whole(Some("w1"), "one", json!({})).unwrap();
    tools.fragment(5, Some("f"), Some("two"), "").unwrap();
    tools.complete(5).unwrap();
    tools.whole(Some("w3"), "three", json!({})).unwrap();
    let names: Vec<String> = tools.finish(StreamEnd::Clean, true).unwrap().into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["one", "two", "three"]);
}

#[test]
fn frame_errors_faults_and_discards_convert_to_protocol_errors_without_stream_text() {
    let frame = [FrameError::LineTooLong { limit: 5 }, FrameError::EventTooLarge { limit: 5 }, FrameError::InvalidUtf8];
    let faults = [
        ToolFault::TooMany,
        ToolFault::TooLarge,
        ToolFault::BadField,
        ToolFault::Conflict,
        ToolFault::ClosedSlot,
        ToolFault::UnknownSlot,
        ToolFault::InvalidArgs,
    ];
    let mut phrases = Vec::new();
    for error in frame {
        let converted = AiError::from(error);
        assert!(matches!(converted, AiError::Protocol(_)), "{error:?}");
        assert_eq!(converted.class(), ErrorClass::Protocol);
        assert_eq!(converted.class().as_str(), "protocol");
        phrases.push(error.to_string());
    }
    for fault in faults {
        let converted = AiError::from(fault);
        assert!(matches!(converted, AiError::Protocol(_)), "{fault:?}");
        assert_eq!(converted.class().as_str(), "protocol");
        phrases.push(fault.to_string());
    }
    let discarded = AiError::from(Discarded { dropped: 2 });
    assert!(matches!(discarded, AiError::Protocol(_)));
    assert_eq!(discarded.class().as_str(), "protocol");
    phrases.push(Discarded { dropped: 2 }.to_string());
    assert!(phrases.iter().all(|p| !p.is_empty()), "{phrases:?}");
    let mut unique = phrases.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), phrases.len(), "every variant has its own phrase: {phrases:?}");
    // The std error trait is implemented.
    let _: &dyn std::error::Error = &FrameError::InvalidUtf8;
    let _: &dyn std::error::Error = &ToolFault::TooMany;
    let _: &dyn std::error::Error = &Discarded { dropped: 0 };

    // Stream text sent as id, name and arguments never shows up in an error, a conversion or a Debug line.
    let id = format!("id-{CANARY}");
    let name = format!("name-{CANARY}");
    let args = format!("{{\"k\":\"{CANARY}");
    let mut tools = ToolAssembler::new();
    tools.fragment(0, Some(&id), Some(&name), &args).unwrap();
    let fault = tools.complete(0).expect_err("cut arguments");
    assert_eq!(fault, ToolFault::InvalidArgs);
    let mut shown = vec![fault.to_string(), format!("{fault:?}"), AiError::from(fault).to_string(), format!("{:?}", AiError::from(fault))];
    shown.push(format!("{tools:?}"));
    shown.push(format!("{tools:#?}"));
    let discarded = tools.finish(StreamEnd::Clean, true).expect_err("a fault poisons the turn");
    shown.push(discarded.to_string());
    shown.push(format!("{discarded:?}"));
    shown.push(format!("{:?}", AiError::from(discarded)));
    for text in shown {
        assert!(!text.contains(CANARY), "stream text reached an error or a Debug line: {text}");
    }
    // A conflict names no text either.
    let mut tools = ToolAssembler::new();
    tools.fragment(0, Some(&id), Some(&name), "").unwrap();
    let fault = tools.fragment(0, Some(&format!("other-{CANARY}")), None, "").expect_err("conflict");
    assert!(!fault.to_string().contains(CANARY) && !AiError::from(fault).to_string().contains(CANARY));
}

#[test]
fn assembler_debug_shows_counts_never_text() {
    let mut tools = ToolAssembler::new();
    tools.fragment(0, Some(&format!("id-{CANARY}")), Some(&format!("name-{CANARY}")), &format!("{{\"k\":\"{CANARY}\"}}")).unwrap();
    tools.complete(0).unwrap();
    tools.whole(Some(&format!("w-{CANARY}")), &format!("n-{CANARY}"), json!({"k": CANARY})).unwrap();
    tools.fragment(3, Some("open"), Some("open"), &format!("{{\"k\":\"{CANARY}")).unwrap();
    for shown in [format!("{tools:?}"), format!("{tools:#?}")] {
        assert!(!shown.contains(CANARY), "the assembler leaks text: {shown}");
        assert!(shown.contains("open") && shown.contains("ready") && shown.contains("bytes"), "counts and sizes: {shown}");
    }
    assert_eq!((tools.open_count(), tools.ready_count()), (1, 2));
}
