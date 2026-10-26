//! The uncompressed baseline: every corpus line goes over a real WebSocket and the bytes the server's socket sent are
//! counted. The corpus is not recorded here; it is created by the recorder (the error says the command).

use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use web_deflate::{corpus, results, serve_baseline};

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn load(workload: &str) -> Vec<String> {
    match corpus::read(&corpus::path(workload)) {
        Ok(lines) => lines,
        Err(why) => panic!("{why}"),
    }
}

#[tokio::test]
async fn the_baseline_sends_the_corpus_unchanged_and_counts_the_wire_bytes() {
    println!("{:<10} {:>9} {:>11} {:>11} {:>10}", "workload", "messages", "raw bytes", "wire bytes", "handshake");
    for workload in corpus::WORKLOADS {
        let lines = load(workload);
        let raw: u64 = lines.iter().map(|l| l.len() as u64).sum();
        let server = serve_baseline(loopback(), lines.clone()).await.expect("the baseline server starts");

        let stream = tokio::net::TcpStream::connect(server.addr).await.expect("connect");
        let (mut ws, _) = tokio_tungstenite::client_async(server.url(), stream).await.expect("handshake");
        ws.send(Message::text("go")).await.expect("go");
        let mut got: Vec<String> = Vec::new();
        while let Some(message) = ws.next().await {
            match message.expect("a message") {
                Message::Text(t) => got.push(t.as_str().to_owned()),
                Message::Close(_) => break,
                other => panic!("unexpected message {other:?}"),
            }
        }
        let _ = ws.close(None).await;

        assert_eq!(got.len(), lines.len(), "{workload}: every line arrives as one message");
        assert!(got == lines, "{workload}: a received line differs from the corpus line");

        let wire = server.counters.message_bytes();
        assert!(wire > 0, "{workload}: the counted wire bytes are zero");
        assert!(wire >= raw, "{workload}: without compression the wire carries at least the text ({wire} < {raw})");
        println!(
            "{:<10} {:>9} {:>11} {:>11} {:>10}",
            workload,
            lines.len(),
            raw,
            wire,
            server.counters.handshake_bytes()
        );
        results::set(
            "baseline",
            workload,
            serde_json::json!({ "messages": lines.len(), "raw_bytes": raw, "wire_bytes": wire,
                "handshake_bytes": server.counters.handshake_bytes() }),
        );
    }
}

/// The recorder writes its JSON with a small emitter of its own (its crate has no serde_json). This is the check
/// that the emitter and serde_json agree: every line parses, and writing the parsed value again, with the key order
/// kept, gives the very same bytes. It also checks that a line is a frame.
#[test]
fn every_corpus_line_is_a_frame_in_the_compact_form_serde_json_writes() {
    for workload in corpus::WORKLOADS {
        let mut last_seq = 0;
        for (n, line) in load(workload).iter().enumerate() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("{workload}:{n}: {e}"));
            assert_eq!(&serde_json::to_string(&value).unwrap(), line, "{workload}:{n} is not in serde_json's form");
            let seq = value["seq"].as_u64().unwrap_or_else(|| panic!("{workload}:{n}: no seq"));
            assert_eq!(seq, last_seq + 1, "{workload}:{n}: the sequence has a gap");
            last_seq = seq;
            assert_eq!((value["cols"].as_u64(), value["rows"].as_u64()), (Some(120), Some(40)), "{workload}:{n}");
            assert!(value["changed"].is_array(), "{workload}:{n}: no changed rows");
        }
    }
}
