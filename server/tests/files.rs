//! The file routes over a real TLS listener and real in-process workers: `GET /files/{token}` for a granted file and
//! `PUT /api/upload` for a dropped one, with their limits, their refusals and their cleaning up.
//!
//! The tracer tests come first; the filter `tracer` selects exactly them. Everything here needs the `in-process-workers`
//! feature, and the page must have been built (`npm --prefix app run build`) because the listener embeds nothing else.
#![cfg(feature = "in-process-workers")]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/files_env.rs"]
mod files_env;
#[path = "support/scratch.rs"]
mod scratch;
#[path = "support/worker_env.rs"]
mod worker_env;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::time::Duration;

use files_env::{FilesEnv, assert_file_headers, png};
use holzwave_server::wire::{Ctl, ExitReason, FILE_SOCKET, FileWhat, Frame, Note, Res, ResBody};
use holzwave_server::worker::runtime::TestHooks;
use serde_json::json;
use worker_env::{PATIENCE, Rig, RigOptions};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

// ---- the worker over a socket pair -----------------------------------------------------------------------------------------

/// Sends a file control and returns the answer on the file socket with the request id given.
fn file_call(rig: &mut Rig, control: Ctl, request: u64) -> ResBody {
    rig.send(Frame::Ctl(control));
    match rig.wait_for(&format!("the answer to file request {request}"), PATIENCE, |f| {
        matches!(f, Frame::Res(r) if r.socket == FILE_SOCKET && r.request == request)
    }) {
        Frame::Res(Res { body, .. }) => body,
        other => unreachable!("{other:?}"),
    }
}

fn stat(rig: &mut Rig, request: u64, token: &str) -> ResBody {
    file_call(rig, Ctl::FileServe { request, what: FileWhat::Stat, token: token.into(), offset: 0, length: 0 }, request)
}

fn read(rig: &mut Rig, request: u64, token: &str, offset: u64, length: u32) -> ResBody {
    file_call(rig, Ctl::FileServe { request, what: FileWhat::Read, token: token.into(), offset, length }, request)
}

/// A worker whose home holds a PNG of `len` bytes, granted: the rig and the token.
fn granted_png(len: usize) -> (Rig, String, Vec<u8>) {
    let mut rig = Rig::start_with(RigOptions { hooks: TestHooks::default(), shell: None });
    rig.init();
    let bytes = png(len);
    let path = rig.home.join("picture.png");
    std::fs::write(&path, &bytes).expect("the picture");
    let granted = rig.ok(1, "file_grant", json!({ "source": "local", "path": path.to_str().expect("UTF-8") }));
    let token = granted["token"].as_str().expect("a token").to_owned();
    (rig, token, bytes)
}

#[test]
fn tracer_the_worker_answers_a_stat_with_the_type_and_the_size_of_a_grant() {
    let (mut rig, token, _) = granted_png(2000);
    match stat(&mut rig, 11, &token) {
        ResBody::Ok(text) => {
            let value: serde_json::Value = serde_json::from_str(&text).expect("JSON");
            assert_eq!(value, json!({ "mime": "image/png", "size": 2000 }));
        }
        other => panic!("{other:?}"),
    }
    rig.finish();
}

#[test]
fn tracer_the_worker_reads_the_whole_file_a_range_and_nothing_past_the_end() {
    let (mut rig, token, bytes) = granted_png(2000);
    assert_eq!(read(&mut rig, 12, &token, 0, 2000), ResBody::Binary(bytes.clone()));
    assert_eq!(read(&mut rig, 13, &token, 100, 50), ResBody::Binary(bytes[100..150].to_vec()));
    // Past the end only the bytes that exist come back: the last 10, then none.
    assert_eq!(read(&mut rig, 14, &token, 1990, 500), ResBody::Binary(bytes[1990..].to_vec()));
    assert_eq!(read(&mut rig, 15, &token, 2000, 500), ResBody::Binary(Vec::new()));
    rig.finish();
}

#[test]
fn tracer_a_token_that_is_not_a_grant_is_refused_and_so_is_a_malformed_one() {
    let (mut rig, _token, _) = granted_png(100);
    let refused = ResBody::Err("\"no such file\"".into());
    // Well formed, never granted.
    assert_eq!(stat(&mut rig, 21, TOKEN), refused);
    assert_eq!(read(&mut rig, 22, TOKEN, 0, 10), refused);
    // A malformed token cannot be encoded; the frame is built by hand and the worker refuses it by closing, never by serving.
    let mut bytes = vec![0x04, 0x08];
    bytes.extend_from_slice(&23u64.to_be_bytes());
    bytes.push(0);
    bytes.extend_from_slice(b"0123456789ABCDEF0123456789abcdef");
    bytes.extend_from_slice(&[0; 12]);
    let mut framed = u32::try_from(bytes.len()).expect("short").to_be_bytes().to_vec();
    framed.extend_from_slice(&bytes);
    rig.send_raw(&framed);
    // The worker takes a frame it cannot read as a broken link and ends; nothing was served.
    rig.wait_for("the failure", PATIENCE, |f| matches!(f, Frame::Note(Note::Exiting(ExitReason::Failure))));
    assert!(!rig.seen().iter().any(|f| matches!(f, Frame::Res(r) if r.request == 23)));
    assert_eq!(rig.finish(), holzwave_server::worker::runtime::RunEnd::Failed);
}

#[test]
fn tracer_a_command_is_answered_while_a_file_is_being_read() {
    let (mut rig, token, bytes) = granted_png(900_000);
    // Ask for the whole piece and for a command at once: both come back, in whatever order, within a second.
    rig.send(Frame::Ctl(Ctl::FileServe { request: 31, what: FileWhat::Read, token: token.clone(), offset: 0, length: 900_000 }));
    let started = std::time::Instant::now();
    let layout = rig.call(2, "get_layout", json!({}));
    assert!(matches!(layout.body, ResBody::Ok(_)));
    assert!(started.elapsed() < Duration::from_secs(1), "the command waited {:?}", started.elapsed());
    match rig.wait_for("the read", PATIENCE, |f| matches!(f, Frame::Res(r) if r.socket == FILE_SOCKET && r.request == 31)) {
        Frame::Res(r) => assert_eq!(r.body, ResBody::Binary(bytes)),
        other => unreachable!("{other:?}"),
    }
    rig.finish();
}

// ---- the route -------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn tracer_a_granted_image_comes_back_with_its_type_its_length_its_bytes_and_the_headers_of_the_file_route() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let path = env.put_png(&alice, "pictures/cat.png", 3000);
    let mut ws = env.worker(&alice).await;
    let grant = env.grant(&mut ws, &path).await;
    assert_eq!((grant.mime.as_str(), grant.size), ("image/png", 3000));

    let reply = env.get(&alice.signed, &grant.url).await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("content-type"), Some("image/png"));
    assert_eq!(reply.header("content-length"), Some("3000"));
    assert_eq!(reply.body, png(3000));
    assert_file_headers(&reply);
    // The application layer's own headers are still there, and it did not replace the policy of the route.
    assert!(reply.header("strict-transport-security").is_some());
    assert!(reply.all("set-cookie").is_empty());
}

#[tokio::test]
async fn tracer_without_a_session_the_route_answers_401_invalid_and_asks_no_worker() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let path = env.put_png(&alice, "cat.png", 100);
    let mut ws = env.worker(&alice).await;
    let grant = env.grant(&mut ws, &path).await;
    let calls = env.env.supervisor.stats().file_calls;

    let reply = env.send(env.get_request(None, &grant.url, None)).await;
    assert_eq!(reply.status, 401);
    assert_eq!(reply.json(), json!({ "code": "invalid" }));
    assert_eq!(env.env.supervisor.stats().file_calls, calls);
}

#[tokio::test]
async fn tracer_an_unknown_token_is_404_not_found_with_no_detail() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let _ws = env.worker(&alice).await;
    let reply = env.get(&alice.signed, &format!("/files/{TOKEN}")).await;
    assert_eq!(reply.status, 404);
    assert_eq!(reply.json(), json!({ "code": "not_found" }));
    assert_file_headers(&reply);
}

#[tokio::test]
async fn tracer_a_malformed_token_is_404_and_no_worker_is_asked() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let path = env.put_png(&alice, "cat.png", 100);
    let mut ws = env.worker(&alice).await;
    let grant = env.grant(&mut ws, &path).await;
    let calls = env.env.supervisor.stats().file_calls;
    let real = grant.token.clone();
    let malformed = [
        real[..31].to_owned(),                    // 31 characters
        format!("{real}0"),                       // 33 characters
        format!("{}F{}", &real[..10], &real[11..]).to_uppercase(), // upper case
        {
            let mut t = real.clone();
            t.replace_range(5..6, "G");
            t
        }, // not hex
        format!("{}.{}", &real[..10], &real[11..]), // a dot
        format!("{}%00{}", &real[..10], &real[13..]), // an encoded NUL
        format!("{}%61{}", &real[..10], &real[12..]), // an encoded letter is not the letter
        format!("{real}/"),
        format!("{real}/x"),
        String::new(),
    ];
    for token in &malformed {
        let reply = env.get(&alice.signed, &format!("/files/{token}")).await;
        assert_eq!(reply.status, 404, "{token}");
        assert_eq!(reply.json(), json!({ "code": "not_found" }), "{token}");
    }
    assert_eq!(env.env.supervisor.stats().file_calls, calls, "no malformed token reached a worker");
    // The real one still works.
    assert_eq!(env.get(&alice.signed, &grant.url).await.status, 200);
}

#[tokio::test]
async fn tracer_another_persons_session_cannot_use_a_token_and_the_owners_worker_is_not_asked() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let bob = env.person("bob");
    let path = env.put_png(&alice, "cat.png", 500);
    let mut alice_ws = env.worker(&alice).await;
    let grant = env.grant(&mut alice_ws, &path).await;

    // Bob has no worker: the token is unknown without anyone being asked.
    let calls = env.env.supervisor.stats().file_calls;
    let reply = env.get(&bob.signed, &grant.url).await;
    assert_eq!((reply.status, reply.json()), (404, json!({ "code": "not_found" })));
    assert_eq!(env.env.supervisor.stats().file_calls, calls);

    // Bob has a worker: his worker is asked, once, and has no such grant. Alice's grant is untouched.
    let _bob_ws = env.worker(&bob).await;
    let asked = env.probe.stats();
    let reply = env.get(&bob.signed, &grant.url).await;
    assert_eq!((reply.status, reply.json()), (404, json!({ "code": "not_found" })));
    assert_eq!(env.probe.stats(), asked + 1, "only bob's worker was asked");
    assert_eq!(env.env.supervisor.stats().file_calls, calls + 1);
    assert_eq!(env.get(&alice.signed, &grant.url).await.status, 200);
}
