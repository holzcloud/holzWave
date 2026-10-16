//! The framing layer of the AI core, with no network.
//!
//! Every provider dialect arrives as a byte stream that the network cuts at arbitrary places. These tests feed golden
//! streams to the bounded decoders in `holzwave_core::ai::sse` and prove that the chunk boundaries do not change the
//! result, that only whole events are dispatched, that every size is bounded and an overrun fails closed, that nothing
//! can reconnect, and that stream text never reaches a `Debug` line. A failing case names the chunk size or the split
//! point in its message.

use holzwave_core::ai::provider::{Collector, Flow, StreamEvent};
use holzwave_core::ai::sse::{FrameError, Limits, MAX_EVENT, MAX_LINE, SseDecoder, StreamEnd};
use holzwave_core::ai::types::Usage;
use serde_json::Value;

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
