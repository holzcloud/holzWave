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

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use files_env::{FilesEnv, MIB, Opts, assert_file_headers, encode, names, png, sparse_bytes, tree};
use holzwave_server::config::Upload;
use holzwave_server::http::routes_files::clean_name;
use holzwave_server::limits::Limits;
use holzwave_server::wire::{Ctl, ExitReason, FILE_SOCKET, FileWhat, Frame, Note, Res, ResBody};
use holzwave_server::worker::files::{FileHooks, numbered, valid_dir, valid_name};
use holzwave_server::worker::runtime::TestHooks;
use serde_json::{Value, json};
use worker_env::{PATIENCE, Rig, RigOptions};
use scratch::Scratch;

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

// ==== the upload ================================================================================================================

// ---- the names, pure ---------------------------------------------------------------------------------------------------------

#[test]
fn a_name_is_cleaned_as_the_table_says() {
    let table: &[(&str, &str)] = &[
        ("hello.txt", "hello.txt"),
        ("../../etc/passwd", "passwd"),
        ("C:\\Users\\x\\a.txt", "a.txt"),
        ("a/b/", "upload"),
        ("..", "upload"),
        (".", "upload"),
        ("...", "upload"),
        ("", "upload"),
        ("     ", "upload"),
        ("a\0b", "a_b"),
        ("a\nb", "a_b"),
        ("a\tb", "a_b"),
        ("a\u{7f}b", "a_b"),
        ("a\u{202e}b.txt", "a_b.txt"),
        ("a\u{200b}b", "a_b"),
        ("a\u{2028}b", "a_b"),
        ("  report.pdf. ", "report.pdf"),
        (".bashrc", "bashrc"),
        (".part-1-deadbeef", "part-1-deadbeef"),
        ("\0", "_"),
        ("a b.tar.gz", "a b.tar.gz"),
        ("na\u{ef}ve caf\u{e9}.txt", "na\u{ef}ve caf\u{e9}.txt"),
        ("\u{65e5}\u{672c}\u{8a9e}.txt", "\u{65e5}\u{672c}\u{8a9e}.txt"),
        ("a/../b", "b"),
        ("..\\..\\x", "x"),
        ("/etc/", "upload"),
        ("trailing dots...", "trailing dots"),
        ("a\r\nb: c", "a__b: c"),
    ];
    for (raw, cleaned) in table {
        assert_eq!(clean_name(raw), *cleaned, "{raw:?}");
    }
}

#[test]
fn a_long_name_is_cut_on_a_character_boundary_and_keeps_a_short_extension() {
    let long = format!("{}.pdf", "\u{e9}".repeat(150));
    let cleaned = clean_name(&long);
    assert_eq!(cleaned.len(), 200, "{} bytes", cleaned.len());
    assert!(cleaned.ends_with(".pdf") && cleaned.starts_with('\u{e9}'));
    assert_eq!(clean_name(&"\u{e9}".repeat(150)).len(), 200);
    assert_eq!(clean_name(&"\u{1f600}".repeat(100)), "\u{1f600}".repeat(50));
    // An extension that is too long is not kept; a cut that ends on a space loses it.
    let no_keep = format!("{}.{}", "a".repeat(250), "b".repeat(20));
    assert_eq!(clean_name(&no_keep), "a".repeat(200));
    assert_eq!(clean_name(&format!("{} b", "a".repeat(199))), "a".repeat(199));
    // A name already within the limit is not touched.
    let exact = format!("{}.txt", "a".repeat(196));
    assert_eq!(clean_name(&exact), exact);
    // Only an extension, however long the stem was before it, still leaves a name.
    assert_eq!(clean_name(&format!("{}.b", "a".repeat(250))).len(), 200);
}

#[test]
fn whatever_a_name_is_the_clean_one_is_valid_for_the_worker_and_cleaning_it_again_changes_nothing() {
    let pool = [
        "a", "\u{e9}", "\u{1f600}", ".", " ", "/", "\\", "\0", "\u{202e}", "..", "x.y", "\n", "report", ".pdf", "\u{200b}", "%", "~", "\u{fffd}",
    ];
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move |n: usize| {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        usize::try_from(state.wrapping_mul(0x2545_F491_4F6C_DD1D) % n as u64).expect("small")
    };
    for _ in 0..4000 {
        let parts = next(70);
        let raw: String = (0..parts).map(|_| pool[next(pool.len())]).collect();
        let cleaned = clean_name(&raw);
        assert!(valid_name(&cleaned), "{raw:?} -> {cleaned:?}");
        assert!(cleaned.len() <= 200, "{raw:?}");
        assert!(!cleaned.chars().any(char::is_control), "{raw:?}");
        assert!(!cleaned.starts_with('.') && !cleaned.ends_with('.') && !cleaned.starts_with(' ') && !cleaned.ends_with(' '), "{raw:?} -> {cleaned:?}");
        assert_eq!(clean_name(&cleaned), cleaned, "{raw:?}");
    }
}

#[test]
fn the_worker_accepts_a_name_of_one_plain_component_and_nothing_else() {
    let good = ["a", "a.txt", "name (1).txt", "\u{e9}", ".hidden", "no-extension"];
    for name in good {
        assert!(valid_name(name), "{name:?}");
    }
    assert!(valid_name(&"a".repeat(255)));
    let bad = ["", ".", "..", "a/b", "/", "a/", "a\0b", ".part-1-deadbeef", ".part-"];
    for name in bad {
        assert!(!valid_name(name), "{name:?}");
    }
    assert!(!valid_name(&"a".repeat(256)));
}

#[test]
fn the_worker_accepts_a_folder_of_plain_relative_components_and_nothing_else() {
    for dir in ["uploads", "a/b", "uploads/2026", "a.b/c d"] {
        assert!(valid_dir(dir), "{dir:?}");
    }
    assert!(valid_dir(&"a".repeat(64)));
    assert!(valid_dir(&vec!["a"; 16].join("/")));
    let bad = ["", "/", "/abs", "a/", "/a", "a//b", "a/../b", "..", ".", "a/./b", "a\0b", "../x"];
    for dir in bad {
        assert!(!valid_dir(dir), "{dir:?}");
    }
    assert!(!valid_dir(&"a".repeat(65)));
    assert!(!valid_dir(&vec!["a"; 17].join("/")));
    assert!(!valid_dir(&format!("{}/{}", "a".repeat(60), "b".repeat(1000))));
}

#[test]
fn a_taken_name_gets_a_number_before_the_extension() {
    assert_eq!(numbered("a.txt", 0), "a.txt");
    assert_eq!(numbered("a.txt", 1), "a (1).txt");
    assert_eq!(numbered("a.tar.gz", 2), "a.tar (2).gz");
    assert_eq!(numbered("noext", 3), "noext (3)");
    assert_eq!(numbered(".hidden", 1), ".hidden (1)");
}

#[test]
fn the_default_upload_settings_are_a_folder_called_uploads_and_256_mib() {
    let upload = Upload::default();
    assert_eq!(upload.dir, Path::new("uploads"));
    assert_eq!(upload.max_bytes, 256 * 1024 * 1024);
}

// ---- the worker's writer over a socket pair -----------------------------------------------------------------------------

fn upload_rig() -> Rig {
    rig_with(TestHooks::default())
}

fn rig_with(hooks: TestHooks) -> Rig {
    let mut rig = Rig::start_with(RigOptions { hooks, shell: None });
    rig.init();
    rig
}

fn answer_since(rig: &mut Rig, mark: usize, id: u64) -> ResBody {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let found = rig.seen()[mark..].iter().find_map(|f| match f {
            Frame::Res(r) if r.socket == FILE_SOCKET && r.request == id => Some(r.body.clone()),
            _ => None,
        });
        if let Some(body) = found {
            return body;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(rig.next(left).is_some(), "no answer to the upload step of {id}");
    }
}

/// One upload step, answered; unlike `file_call` it finds only an answer that came after the step was sent.
fn step(rig: &mut Rig, control: Ctl, id: u64) -> ResBody {
    let mark = rig.seen().len();
    rig.send(Frame::Ctl(control));
    answer_since(rig, mark, id)
}

fn open(rig: &mut Rig, id: u64, size: u64, folder: &str, name: &str) -> ResBody {
    step(rig, Ctl::UploadOpen { id, size, folder: folder.into(), name: name.into() }, id)
}

fn piece(rig: &mut Rig, id: u64, bytes: &[u8]) -> ResBody {
    step(rig, Ctl::UploadPiece { id, bytes: bytes.to_vec() }, id)
}

fn close(rig: &mut Rig, id: u64, commit: bool) -> ResBody {
    step(rig, Ctl::UploadClose { id, commit }, id)
}

fn is_ok(body: &ResBody) -> bool {
    matches!(body, ResBody::Ok(_))
}

/// A whole upload: open, the bytes in pieces of `at_most`, close with commit. The answer to the close.
fn whole(rig: &mut Rig, id: u64, folder: &str, name: &str, bytes: &[u8], at_most: usize) -> ResBody {
    assert!(is_ok(&open(rig, id, bytes.len() as u64, folder, name)));
    for chunk in bytes.chunks(at_most) {
        assert!(is_ok(&piece(rig, id, chunk)));
    }
    close(rig, id, true)
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).expect("metadata").permissions().mode() & 0o7777
}

fn stored(body: ResBody) -> Value {
    match body {
        ResBody::Ok(text) => serde_json::from_str(&text).expect("JSON"),
        other => panic!("{other:?}"),
    }
}

/// A directory outside the home with a file in it that no upload may touch.
struct Canary {
    dir: Scratch,
}

impl Canary {
    fn new() -> Canary {
        let dir = Scratch::new("canary");
        dir.write_file("canary.txt", b"do not touch", 0o600);
        Canary { dir }
    }

    fn untouched(&self) {
        assert_eq!(names(self.dir.path()), vec!["canary.txt".to_owned()], "nothing was made beside the canary");
        assert_eq!(std::fs::read(self.dir.join("canary.txt")).expect("canary"), b"do not touch");
    }
}

#[test]
fn opening_an_upload_makes_the_folder_with_mode_0700_and_an_exclusive_part_file_with_mode_0600() {
    let mut rig = upload_rig();
    let uploads = rig.home.join("uploads");
    assert!(is_ok(&open(&mut rig, 1, 10, "uploads", "a.txt")));
    assert_eq!(mode_of(&uploads), 0o700);
    let found = names(&uploads);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with(".part-1-") && found[0].len() == ".part-1-".len() + 8, "{found:?}");
    assert_eq!(mode_of(&uploads.join(&found[0])), 0o600);
    // A second open with the same id is refused and leaves the first alone.
    assert!(!is_ok(&open(&mut rig, 1, 10, "uploads", "b.txt")));
    assert_eq!(names(&uploads), found);
    assert!(is_ok(&close(&mut rig, 1, false)));
    assert!(names(&uploads).is_empty());
    rig.finish();
}

#[test]
fn nested_folders_are_made_one_component_at_a_time_each_with_mode_0700() {
    let mut rig = upload_rig();
    let result = stored(whole(&mut rig, 1, "up/2026/may", "a.txt", b"x", 10));
    let path = result["path"].as_str().expect("path").to_owned();
    assert_eq!(path, rig.home.join("up/2026/may/a.txt").to_str().expect("UTF-8"));
    for dir in ["up", "up/2026", "up/2026/may"] {
        assert_eq!(mode_of(&rig.home.join(dir)), 0o700, "{dir}");
    }
    rig.finish();
}

#[test]
fn pieces_are_appended_and_each_one_is_answered_and_a_commit_publishes_the_file_with_mode_0600() {
    let mut rig = upload_rig();
    let uploads = rig.home.join("uploads");
    let bytes: Vec<u8> = (0..2500u32).map(|i| u8::try_from(i % 251).expect("byte")).collect();
    assert!(is_ok(&open(&mut rig, 1, 2500, "uploads", "data.bin")));
    for chunk in bytes.chunks(1000) {
        assert_eq!(piece(&mut rig, 1, chunk), ResBody::Ok("{}".into()));
    }
    let result = stored(close(&mut rig, 1, true));
    assert_eq!(result, json!({ "path": uploads.join("data.bin").to_str().expect("UTF-8"), "size": 2500 }));
    assert_eq!(std::fs::read(uploads.join("data.bin")).expect("the file"), bytes);
    assert_eq!(mode_of(&uploads.join("data.bin")), 0o600);
    assert_eq!(names(&uploads), vec!["data.bin".to_owned()], "the part file is gone and nothing else is there");
    // The id is free again and the upload is over: a piece for it is refused.
    assert!(!is_ok(&piece(&mut rig, 1, b"late")));
    rig.finish();
}

#[test]
fn an_empty_upload_is_a_file_of_no_bytes() {
    let mut rig = upload_rig();
    let result = stored(whole(&mut rig, 1, "uploads", "empty.txt", b"", 10));
    assert_eq!(result["size"], 0);
    assert_eq!(std::fs::read(rig.home.join("uploads/empty.txt")).expect("the file"), b"");
    rig.finish();
}

#[test]
fn an_existing_file_is_never_changed_the_new_one_gets_a_number() {
    let mut rig = upload_rig();
    let uploads = rig.home.join("uploads");
    std::fs::create_dir(&uploads).expect("folder");
    let old = uploads.join("a.txt");
    std::fs::write(&old, b"original").expect("old");
    let modified = std::fs::metadata(&old).expect("meta").modified().expect("mtime");
    std::thread::sleep(Duration::from_millis(30));

    let first = stored(whole(&mut rig, 1, "uploads", "a.txt", b"new one", 100));
    assert_eq!(first["path"], uploads.join("a (1).txt").to_str().expect("UTF-8"));
    let second = stored(whole(&mut rig, 2, "uploads", "a.txt", b"newer", 100));
    assert_eq!(second["path"], uploads.join("a (2).txt").to_str().expect("UTF-8"));
    assert_eq!(std::fs::read(&old).expect("old"), b"original");
    assert_eq!(std::fs::metadata(&old).expect("meta").modified().expect("mtime"), modified);
    assert_eq!(std::fs::read(uploads.join("a (1).txt")).expect("1"), b"new one");
    assert_eq!(std::fs::read(uploads.join("a (2).txt")).expect("2"), b"newer");
    assert_eq!(names(&uploads), vec!["a (1).txt", "a (2).txt", "a.txt"]);
    rig.finish();
}

#[test]
fn a_link_where_the_folder_should_be_is_refused_and_nothing_is_written_outside() {
    let mut rig = upload_rig();
    let canary = Canary::new();
    symlink(canary.dir.path(), rig.home.join("uploads")).expect("link");
    assert!(!is_ok(&open(&mut rig, 1, 4, "uploads", "x.txt")));
    assert!(!is_ok(&piece(&mut rig, 1, b"data")));
    canary.untouched();
    rig.finish();
}

#[test]
fn a_link_as_a_component_of_the_folder_is_refused_and_nothing_is_written_outside() {
    let mut rig = upload_rig();
    let canary = Canary::new();
    std::fs::create_dir(rig.home.join("up")).expect("folder");
    symlink(canary.dir.path(), rig.home.join("up/inner")).expect("link");
    assert!(!is_ok(&open(&mut rig, 1, 4, "up/inner/deeper", "x.txt")));
    assert!(!is_ok(&open(&mut rig, 2, 4, "up/inner", "x.txt")));
    canary.untouched();
    // A link to a file in place of a folder component is refused too.
    symlink(canary.dir.join("canary.txt"), rig.home.join("up/file")).expect("link");
    assert!(!is_ok(&open(&mut rig, 3, 4, "up/file", "x.txt")));
    canary.untouched();
    rig.finish();
}

#[test]
fn a_link_with_the_name_of_the_upload_is_not_followed_and_the_file_it_points_to_is_not_changed() {
    let mut rig = upload_rig();
    let canary = Canary::new();
    let uploads = rig.home.join("uploads");
    std::fs::create_dir(&uploads).expect("folder");
    symlink(canary.dir.join("canary.txt"), uploads.join("x.txt")).expect("link");
    symlink(canary.dir.join("not-there"), uploads.join("y.txt")).expect("dangling link");
    let first = stored(whole(&mut rig, 1, "uploads", "x.txt", b"data", 10));
    assert_eq!(first["path"], uploads.join("x (1).txt").to_str().expect("UTF-8"));
    let second = stored(whole(&mut rig, 2, "uploads", "y.txt", b"data", 10));
    assert_eq!(second["path"], uploads.join("y (1).txt").to_str().expect("UTF-8"));
    canary.untouched();
    assert!(!canary.dir.join("not-there").exists());
    rig.finish();
}

#[test]
fn a_link_as_the_home_path_of_the_folder_is_not_followed_even_when_it_points_to_a_folder() {
    let mut rig = upload_rig();
    let canary = Canary::new();
    std::fs::create_dir(rig.home.join("real")).expect("folder");
    symlink(rig.home.join("real"), rig.home.join("alias")).expect("link inside the home");
    assert!(!is_ok(&open(&mut rig, 1, 1, "alias", "x.txt")), "not even a link that stays inside the home");
    assert!(names(&rig.home.join("real")).is_empty());
    canary.untouched();
    rig.finish();
}

#[test]
fn a_piece_for_an_upload_that_was_never_opened_is_refused_and_so_is_a_close() {
    let mut rig = upload_rig();
    assert!(!is_ok(&piece(&mut rig, 77, b"data")));
    assert!(!is_ok(&close(&mut rig, 77, true)));
    assert!(!is_ok(&close(&mut rig, 78, false)));
    assert!(!rig.home.join("uploads").exists());
    rig.finish();
}

#[test]
fn an_abort_removes_the_part_file_and_publishes_nothing() {
    let mut rig = upload_rig();
    let uploads = rig.home.join("uploads");
    assert!(is_ok(&open(&mut rig, 1, 100, "uploads", "a.txt")));
    assert!(is_ok(&piece(&mut rig, 1, b"half of it")));
    assert_eq!(names(&uploads).len(), 1);
    assert!(is_ok(&close(&mut rig, 1, false)));
    assert!(names(&uploads).is_empty());
    rig.finish();
}

#[test]
fn a_close_with_fewer_bytes_than_declared_and_a_piece_over_the_declared_size_are_refused_and_leave_nothing() {
    let mut rig = upload_rig();
    let uploads = rig.home.join("uploads");
    assert!(is_ok(&open(&mut rig, 1, 10, "uploads", "short.txt")));
    assert!(is_ok(&piece(&mut rig, 1, b"1234")));
    assert!(!is_ok(&close(&mut rig, 1, true)));
    assert!(names(&uploads).is_empty());

    assert!(is_ok(&open(&mut rig, 2, 3, "uploads", "long.txt")));
    assert!(!is_ok(&piece(&mut rig, 2, b"1234")));
    assert!(names(&uploads).is_empty(), "{:?}", names(&uploads));
    rig.finish();
}

#[test]
fn a_folder_or_a_name_that_is_not_plain_is_refused_before_anything_is_made() {
    let mut rig = upload_rig();
    let mut id = 0;
    for (folder, name) in [
        ("../x", "a"),
        ("/abs", "a"),
        ("a//b", "a"),
        ("a/./b", "a"),
        ("uploads", "../a"),
        ("uploads", "a/b"),
        ("uploads", ".."),
        ("uploads", ".part-1-deadbeef"),
        ("uploads", "a\0b"),
    ] {
        id += 1;
        assert!(!is_ok(&open(&mut rig, id, 1, folder, name)), "{folder:?} {name:?}");
    }
    let made: Vec<String> = tree(rig.home.path())
        .into_iter()
        .map(|(path, _, _)| path)
        .filter(|path| !path.starts_with(".holzwave") && !path.starts_with(".tmp") && path != "shell.pid")
        .collect();
    assert!(made.is_empty(), "{made:?}");
    rig.finish();
}

#[test]
fn a_part_file_older_than_an_hour_is_removed_at_the_next_open_and_a_younger_one_and_other_files_are_kept() {
    let mut rig = upload_rig();
    let uploads = rig.home.join("uploads");
    std::fs::create_dir(&uploads).expect("folder");
    let old_time = SystemTime::now() - Duration::from_secs(2 * 3600);
    let write = |name: &str, time: Option<SystemTime>| {
        let path = uploads.join(name);
        std::fs::write(&path, b"x").expect("file");
        if let Some(time) = time {
            std::fs::File::options().write(true).open(&path).expect("open").set_modified(time).expect("mtime");
        }
    };
    write(".part-9-deadbeef", Some(old_time));
    write(".part-8-cafebabe", None);
    write("old-but-not-a-part.txt", Some(old_time));
    write(".part-not-ours", Some(old_time));
    assert!(is_ok(&open(&mut rig, 1, 1, "uploads", "a.txt")));
    let found = names(&uploads);
    assert!(!found.contains(&".part-9-deadbeef".to_owned()), "{found:?}");
    for kept in [".part-8-cafebabe", "old-but-not-a-part.txt", ".part-not-ours"] {
        assert!(found.contains(&kept.to_owned()), "{kept} in {found:?}");
    }
    assert_eq!(found.len(), 4, "{found:?}");
    rig.finish();
}

#[test]
fn a_full_disk_aborts_the_upload_and_removes_the_part_file_before_the_refusal_is_answered() {
    let hooks = TestHooks { files: FileHooks { fail_write_after: Some(100), ..FileHooks::default() }, ..TestHooks::default() };
    let mut rig = rig_with(hooks);
    let uploads = rig.home.join("uploads");
    assert!(is_ok(&open(&mut rig, 1, 1000, "uploads", "big.bin")));
    assert!(is_ok(&piece(&mut rig, 1, &[7; 50])));
    assert!(!is_ok(&piece(&mut rig, 1, &[7; 100])));
    assert!(names(&uploads).is_empty(), "{:?}", names(&uploads));
    // The upload is over: another piece for it is refused as unknown.
    assert!(!is_ok(&piece(&mut rig, 1, &[7; 1])));
    rig.finish();
}

#[test]
fn a_worker_holds_at_most_four_uploads_open() {
    let mut rig = upload_rig();
    let uploads = rig.home.join("uploads");
    for id in 1..=4 {
        assert!(is_ok(&open(&mut rig, id, 10, "uploads", &format!("f{id}.txt"))), "{id}");
    }
    assert!(!is_ok(&open(&mut rig, 5, 10, "uploads", "f5.txt")));
    assert_eq!(names(&uploads).len(), 4);
    for id in 1..=4 {
        assert!(is_ok(&close(&mut rig, id, false)));
    }
    assert!(names(&uploads).is_empty());
    assert!(is_ok(&open(&mut rig, 6, 10, "uploads", "f6.txt")), "a place is free again");
    rig.finish();
}

#[test]
fn a_command_is_answered_within_a_second_all_through_a_64_mib_upload() {
    let hooks = TestHooks { files: FileHooks { write_delay: Some(Duration::from_millis(15)), ..FileHooks::default() }, ..TestHooks::default() };
    let mut rig = rig_with(hooks);
    let total: u64 = 64 * 1024 * 1024;
    assert!(is_ok(&open(&mut rig, 1, total, "uploads", "big.bin")));
    let chunk = vec![0xA5u8; 1024 * 1024];
    let mut slowest = Duration::ZERO;
    for _ in 0..64 {
        let mark = rig.seen().len();
        rig.send(Frame::Ctl(Ctl::UploadPiece { id: 1, bytes: chunk.clone() }));
        let started = Instant::now();
        let layout = rig.call(2, "get_layout", json!({}));
        slowest = slowest.max(started.elapsed());
        assert!(matches!(layout.body, ResBody::Ok(_)));
        assert!(is_ok(&answer_since(&mut rig, mark, 1)));
    }
    assert!(slowest < Duration::from_secs(1), "the slowest answer of a command took {slowest:?}");
    let result = stored(close(&mut rig, 1, true));
    assert_eq!(result["size"], total);
    assert_eq!(std::fs::metadata(rig.home.join("uploads/big.bin")).expect("meta").len(), total);
    rig.finish();
}

// ---- the route ----------------------------------------------------------------------------------------------------------

/// The state of the folder of `person` after an upload: the names in it.
fn uploads_of(env: &FilesEnv, person: &ws_env::Person) -> Vec<String> {
    names(&env.home(person).join("uploads"))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upload_answers_200_with_the_path_and_the_size_and_the_file_is_there_with_mode_0600() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let body: Vec<u8> = (0..1024u32).map(|i| u8::try_from(i % 253).expect("byte")).collect();
    let reply = env.upload(&alice.signed, "hello.txt", &body).await;
    assert_eq!(reply.status, 200, "{}", reply.text());
    let home = env.home(&alice);
    assert_eq!(reply.json(), json!({ "path": home.join("uploads/hello.txt").to_str().expect("UTF-8"), "size": 1024 }));
    assert_eq!(reply.header("content-type"), Some("application/json"));
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    assert!(reply.header("strict-transport-security").is_some());
    assert_eq!(std::fs::read(home.join("uploads/hello.txt")).expect("file"), body);
    assert_eq!(mode_of(&home.join("uploads/hello.txt")), 0o600);
    assert_eq!(mode_of(&home.join("uploads")), 0o700);
    assert_eq!(uploads_of(&env, &alice), vec!["hello.txt".to_owned()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_name_again_lands_as_a_numbered_file_and_the_first_is_unchanged() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    assert_eq!(env.upload(&alice.signed, "hello.txt", b"first").await.status, 200);
    let second = env.upload(&alice.signed, "hello.txt", b"second").await;
    assert_eq!(second.status, 200);
    let home = env.home(&alice);
    assert_eq!(second.json()["path"], home.join("uploads/hello (1).txt").to_str().expect("UTF-8"));
    assert_eq!(std::fs::read(home.join("uploads/hello.txt")).expect("first"), b"first");
    assert_eq!(std::fs::read(home.join("uploads/hello (1).txt")).expect("second"), b"second");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upload_in_several_pieces_arrives_byte_for_byte_and_the_file_is_the_size_of_the_limit_at_most() {
    let max = 3 * 1024 * 1024;
    let env = FilesEnv::start_with(Opts { upload: Some(Upload { dir: "uploads".into(), max_bytes: max }), ..Opts::default() }).await;
    let alice = env.person("alice");
    // Exactly the limit, in three pieces; and a size that leaves a part of a piece.
    for (name, size) in [("exact.bin", max), ("odd.bin", 2 * 1024 * 1024 + 12_345)] {
        let body: Vec<u8> = (0..size).map(|i| u8::try_from(i % 251).expect("byte")).collect();
        let reply = env.upload(&alice.signed, name, &body).await;
        assert_eq!(reply.status, 200, "{name}: {}", reply.text());
        assert_eq!(reply.json()["size"], size);
        assert_eq!(std::fs::read(env.home(&alice).join("uploads").join(name)).expect("file"), body, "{name}");
    }
    assert_eq!(uploads_of(&env, &alice), vec!["exact.bin".to_owned(), "odd.bin".to_owned()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_byte_over_the_limit_is_400_size_before_any_body_byte_is_read() {
    let max = 3 * 1024 * 1024;
    let env = FilesEnv::start_with(Opts { upload: Some(Upload { dir: "uploads".into(), max_bytes: max }), ..Opts::default() }).await;
    let alice = env.person("alice");
    // Only the head is sent: the answer comes without a body byte.
    let mut conn = env.begin_upload(&alice.signed, "name=big.bin", max + 1).await;
    let reply = conn.reply(false).await.expect("an answer to the head alone");
    assert_eq!(reply.status, 400);
    assert_eq!(reply.json(), json!({ "code": "bad_request", "field": "size" }));
    assert!(uploads_of(&env, &alice).is_empty());
    // And the same for a size that is not a number or is huge.
    for declared in ["99999999999999999999", "-1", "1e3", ""] {
        let mut head = env.upload_request(&alice.signed, "name=a", &[]);
        head.headers.retain(|(n, _)| !n.eq_ignore_ascii_case("content-length"));
        head = head.header("Content-Length", declared);
        let reply = env.send(head).await;
        assert!(reply.status == 400 || reply.status == 0, "{declared:?}: {}", reply.status);
    }
    assert!(uploads_of(&env, &alice).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_with_no_length_or_a_chunked_body_is_400_and_leaves_nothing() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    // No Content-Length and no body.
    let mut request = env.upload_request(&alice.signed, "name=a.txt", &[]);
    request.headers.retain(|(n, _)| !n.eq_ignore_ascii_case("content-length"));
    let reply = env.send(request).await;
    assert_eq!((reply.status, reply.json()), (400, json!({ "code": "bad_request", "field": "size" })));

    // A chunked body.
    let mut request = env.upload_request(&alice.signed, "name=a.txt", &[]);
    request.headers.retain(|(n, _)| !n.eq_ignore_ascii_case("content-length"));
    request = request.header("Transfer-Encoding", "chunked");
    let mut bytes = request.bytes();
    bytes.extend_from_slice(b"5\r\nhello\r\n0\r\n\r\n");
    let mut conn = env.env.connection().await;
    conn.send(&bytes).await;
    let reply = conn.reply(false).await.expect("an answer");
    assert_eq!((reply.status, reply.json()), (400, json!({ "code": "bad_request", "field": "size" })));
    assert!(uploads_of(&env, &alice).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_that_ends_before_its_declared_length_leaves_no_file_and_no_part_file() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let mut conn = env.begin_upload(&alice.signed, "name=short.txt", 12).await;
    conn.send(b"hello").await;
    // The part file exists while the upload waits for the rest.
    env.env.eventually("the part file", |_| uploads_of(&env, &alice).len() == 1).await;
    drop(conn);
    env.env.eventually("the part file is gone", |_| uploads_of(&env, &alice).is_empty()).await;
    // The place of the upload is free again.
    assert_eq!(env.upload(&alice.signed, "after.txt", b"ok").await.status, 200);
    assert_eq!(uploads_of(&env, &alice), vec!["after.txt".to_owned()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_longer_than_declared_is_cut_at_the_declared_length_and_never_written_past_it() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let mut conn = env.begin_upload(&alice.signed, "name=cut.txt", 5).await;
    conn.send(b"hello world!").await;
    let reply = conn.reply(false).await.expect("an answer");
    // The five declared bytes are the whole upload; what follows them is not part of it, and the server closes the connection.
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["size"], 5);
    assert_eq!(std::fs::read(env.home(&alice).join("uploads/cut.txt")).expect("file"), b"hello");
    assert_eq!(uploads_of(&env, &alice), vec!["cut.txt".to_owned()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_two_or_extra_name_and_a_name_of_5000_characters_are_400_name() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let queries = [
        String::new(),
        "x=1".to_owned(),
        "name=a&name=b".to_owned(),
        "name=a&extra=1".to_owned(),
        "extra=1&name=a".to_owned(),
        format!("name={}", "a".repeat(5000)),
        format!("name={}", "%C3%A9".repeat(600)),
    ];
    for query in &queries {
        let target = if query.is_empty() { "/api/upload".to_owned() } else { format!("/api/upload?{query}") };
        let mut request = env.upload_request(&alice.signed, "", b"data");
        request.target = target;
        let reply = env.send(request).await;
        assert_eq!(reply.status, 400, "{query:.40}");
        assert_eq!(reply.json(), json!({ "code": "bad_request", "field": "name" }), "{query:.40}");
    }
    assert!(uploads_of(&env, &alice).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn names_from_the_request_are_cleaned_and_the_file_stays_in_the_upload_folder() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let home = env.home(&alice);
    let long = format!("{}.pdf", "\u{e9}".repeat(150));
    let cases: [(&str, String); 7] = [
        ("..%2f..%2fx", "x".to_owned()),
        ("%00", "_".to_owned()),
        ("%2e%2e", "upload".to_owned()),
        ("", "upload (1)".to_owned()),
        ("C%3A%5CUsers%5Cx%5Ca.txt", "a.txt".to_owned()),
        ("a%0D%0Ab", "a__b".to_owned()),
        (&encode(&long), clean_name(&long)),
    ];
    for (raw, expected) in &cases {
        let reply = env.send(env.upload_request(&alice.signed, &format!("name={raw}"), b"data")).await;
        assert_eq!(reply.status, 200, "{raw}: {}", reply.text());
        assert_eq!(reply.json()["path"], home.join("uploads").join(expected).to_str().expect("UTF-8"), "{raw}");
    }
    let found = uploads_of(&env, &alice);
    assert_eq!(found.len(), 7, "{found:?}");
    // Nothing was made beside or above the folder.
    let beside: Vec<String> = names(&home).into_iter().filter(|n| n != ".holzwave" && n != ".tmp").collect();
    assert_eq!(beside, vec!["uploads".to_owned()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_percent_encoded_name_and_a_plus_are_read_as_text_and_a_name_in_the_body_is_never_used() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let home = env.home(&alice);
    let reply = env.send(env.upload_request(&alice.signed, "name=my%20file%2B1.txt", b"a")).await;
    assert_eq!(reply.json()["path"], home.join("uploads/my file+1.txt").to_str().expect("UTF-8"));
    // A header that looks like a name is not one.
    let request = env.upload_request(&alice.signed, "name=real.txt", b"b").header("Content-Disposition", "attachment; filename=\"evil.txt\"");
    let reply = env.send(request).await;
    assert_eq!(reply.json()["path"], home.join("uploads/real.txt").to_str().expect("UTF-8"));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_cookie_the_upload_is_401_and_writes_nothing() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let mut request = env.upload_request(&alice.signed, "name=a.txt", b"data");
    request.headers.retain(|(n, _)| !n.eq_ignore_ascii_case("cookie"));
    let reply = env.send(request).await;
    assert_eq!((reply.status, reply.json()), (401, json!({ "code": "invalid" })));
    assert!(!env.home(&alice).join("uploads").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_or_foreign_origin_and_a_missing_or_wrong_token_are_403_and_write_nothing() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let base = env.upload_request(&alice.signed, "name=a.txt", b"data");
    let without = |name: &str| {
        let mut request = base.clone();
        request.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        request
    };
    let with = |name: &str, value: &str| without(name).header(name, value);
    let attempts = [
        ("no origin", without("origin")),
        ("foreign origin", with("origin", "https://evil.example")),
        ("null origin", with("origin", "null")),
        ("http origin", with("origin", "http://hw.example.test")),
        ("no token", without("x-csrf-token")),
        ("wrong token", with("x-csrf-token", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")),
    ];
    for (what, request) in attempts {
        let reply = env.send(request).await;
        assert_eq!((reply.status, reply.json()), (403, json!({ "code": "forbidden" })), "{what}");
    }
    assert!(!env.home(&alice).join("uploads").exists(), "nothing was written");
    // The same request with everything right works.
    assert_eq!(env.send(base).await.status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn another_content_type_than_octet_stream_is_400_and_writes_nothing() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    for content_type in ["text/plain", "multipart/form-data; boundary=x", "application/json", "image/png", ""] {
        let mut request = env.upload_request(&alice.signed, "name=a.txt", b"data");
        request.headers.retain(|(n, _)| !n.eq_ignore_ascii_case("content-type"));
        if !content_type.is_empty() {
            request = request.header("Content-Type", content_type);
        }
        let reply = env.send(request).await;
        assert_eq!(reply.status, 400, "{content_type:?}");
    }
    assert!(!env.home(&alice).join("uploads").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn get_post_and_delete_on_the_upload_path_are_405() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    for method in ["GET", "POST", "DELETE", "PATCH"] {
        let mut request = env.upload_request(&alice.signed, "name=a.txt", b"data");
        request.method = method.to_owned();
        let reply = env.send(request).await;
        assert_eq!(reply.status, 405, "{method}");
        assert_eq!(reply.json(), json!({ "code": "bad_request" }), "{method}");
        assert!(reply.header("allow").is_none(), "{method}");
    }
    assert!(!env.home(&alice).join("uploads").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_third_parallel_upload_of_a_person_is_503_busy_and_the_first_two_still_finish() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let mut first = env.begin_upload(&alice.signed, "name=one.txt", 10).await;
    let mut second = env.begin_upload(&alice.signed, "name=two.txt", 10).await;
    env.env.eventually("two part files", |_| uploads_of(&env, &alice).len() == 2).await;

    let started = Instant::now();
    let third = env.upload(&alice.signed, "three.txt", b"x").await;
    assert_eq!((third.status, third.json()), (503, json!({ "code": "busy" })));
    assert!(started.elapsed() < Duration::from_secs(2), "refused at once, not after a wait");
    assert_eq!(uploads_of(&env, &alice).len(), 2, "the refused one left nothing");

    first.send(b"0123456789").await;
    second.send(b"abcdefghij").await;
    assert_eq!(first.reply(false).await.expect("first").status, 200);
    assert_eq!(second.reply(false).await.expect("second").status, 200);
    // The places are free again.
    assert_eq!(env.upload(&alice.signed, "three.txt", b"x").await.status, 200);
    assert_eq!(std::fs::read(env.home(&alice).join("uploads/one.txt")).expect("one"), b"0123456789");
    assert_eq!(std::fs::read(env.home(&alice).join("uploads/two.txt")).expect("two"), b"abcdefghij");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_server_wide_limit_of_uploads_refuses_a_person_who_has_none_running() {
    let limits = Limits { uploads_per_person: 1, uploads_total: 2, ..Limits::DEFAULT };
    let env = FilesEnv::start_with(Opts { limits: Some(limits), ..Opts::default() }).await;
    let (alice, bob, carol) = (env.person("alice"), env.person("bob"), env.person("carol"));
    let mut a = env.begin_upload(&alice.signed, "name=a.txt", 3).await;
    assert_eq!(env.upload(&alice.signed, "again.txt", b"x").await.status, 503, "one per person");
    let mut b = env.begin_upload(&bob.signed, "name=b.txt", 3).await;
    env.env.eventually("two part files", |_| uploads_of(&env, &alice).len() == 1 && uploads_of(&env, &bob).len() == 1).await;
    let refused = env.upload(&carol.signed, "c.txt", b"x").await;
    assert_eq!((refused.status, refused.json()), (503, json!({ "code": "busy" })), "the whole server is full");
    assert!(uploads_of(&env, &carol).is_empty());
    a.send(b"abc").await;
    assert_eq!(a.reply(false).await.expect("a").status, 200);
    assert_eq!(env.upload(&carol.signed, "c.txt", b"x").await.status, 200, "a place came free");
    b.send(b"abc").await;
    assert_eq!(b.reply(false).await.expect("b").status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upload_that_stops_sending_is_ended_after_the_stall_time_and_its_part_file_is_removed() {
    let limits = Limits { upload_stall: Duration::from_secs(1), ..Limits::DEFAULT };
    let env = FilesEnv::start_with(Opts { limits: Some(limits), ..Opts::default() }).await;
    let alice = env.person("alice");
    let mut conn = env.begin_upload(&alice.signed, "name=slow.txt", 100).await;
    conn.send(b"0123456789").await;
    env.env.eventually("the part file", |_| uploads_of(&env, &alice).len() == 1).await;
    let started = Instant::now();
    let reply = conn.reply(false).await;
    let waited = started.elapsed();
    assert!(reply.as_ref().is_none_or(|r| r.status != 200), "{reply:?}");
    assert!(waited >= Duration::from_millis(800) && waited < Duration::from_secs(8), "ended after {waited:?}");
    env.env.eventually("the part file is gone", |_| uploads_of(&env, &alice).is_empty()).await;
    assert_eq!(env.upload(&alice.signed, "after.txt", b"ok").await.status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn signing_out_in_the_middle_of_an_upload_aborts_it_and_removes_the_part_file() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let mut conn = env.begin_upload(&alice.signed, "name=cut.txt", 100).await;
    conn.send(b"0123456789").await;
    env.env.eventually("the part file", |_| uploads_of(&env, &alice).len() == 1).await;
    env.env.test.sessions.end(&alice.signed.session).expect("end");
    let reply = conn.reply(false).await;
    assert!(reply.as_ref().is_none_or(|r| r.status == 401), "no success status: {reply:?}");
    env.env.eventually("the part file is gone", |_| uploads_of(&env, &alice).is_empty()).await;
    assert_eq!(uploads_of(&env, &alice), Vec::<String>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_goes_away_in_the_middle_leaves_nothing_and_frees_its_places() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let mut one = env.begin_upload(&alice.signed, "name=one.txt", 5000).await;
    let mut two = env.begin_upload(&alice.signed, "name=two.txt", 5000).await;
    one.send(&[1u8; 1000]).await;
    two.send(&[2u8; 1000]).await;
    env.env.eventually("two part files", |_| uploads_of(&env, &alice).len() == 2).await;
    drop(one);
    drop(two);
    env.env.eventually("nothing is left", |_| uploads_of(&env, &alice).is_empty()).await;
    // Both places are free: two new uploads are accepted.
    let mut again = env.begin_upload(&alice.signed, "name=three.txt", 3).await;
    let mut more = env.begin_upload(&alice.signed, "name=four.txt", 3).await;
    again.send(b"abc").await;
    more.send(b"def").await;
    assert_eq!(again.reply(false).await.expect("again").status, 200);
    assert_eq!(more.reply(false).await.expect("more").status, 200);
    assert_eq!(uploads_of(&env, &alice), vec!["four.txt".to_owned(), "three.txt".to_owned()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_disk_in_the_worker_ends_the_upload_503_and_leaves_nothing() {
    let env = FilesEnv::start_with(Opts { fail_write_after: Some(1000), ..Opts::default() }).await;
    let alice = env.person("alice");
    let reply = env.upload(&alice.signed, "big.bin", &[3u8; 4000]).await;
    assert_eq!((reply.status, reply.json()), (503, json!({ "code": "busy" })));
    assert!(uploads_of(&env, &alice).is_empty());
    // A small file still goes through: the failure was the disk's, not the person's account.
    assert_eq!(env.upload(&alice.signed, "small.bin", &[3u8; 500]).await.status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upload_lands_in_the_home_of_the_person_who_made_it_and_in_no_other() {
    let env = FilesEnv::start().await;
    let (alice, bob) = (env.person("alice"), env.person("bob"));
    let reply = env.upload(&bob.signed, "mine.txt", b"bob's").await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["path"], env.home(&bob).join("uploads/mine.txt").to_str().expect("UTF-8"));
    assert!(!env.home(&alice).join("uploads").exists());
    assert_eq!(env.upload(&alice.signed, "mine.txt", b"alice's").await.json()["path"], env.home(&alice).join("uploads/mine.txt").to_str().expect("UTF-8"));
    assert_eq!(std::fs::read(env.home(&bob).join("uploads/mine.txt")).expect("bob"), b"bob's");
    assert_eq!(std::fs::read(env.home(&alice).join("uploads/mine.txt")).expect("alice"), b"alice's");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_configured_folder_of_two_levels_is_used_and_made() {
    let upload = Upload { dir: "drop/in".into(), max_bytes: 1024 * 1024 };
    let env = FilesEnv::start_with(Opts { upload: Some(upload), ..Opts::default() }).await;
    let alice = env.person("alice");
    let reply = env.upload(&alice.signed, "a.txt", b"data").await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["path"], env.home(&alice).join("drop/in/a.txt").to_str().expect("UTF-8"));
    assert_eq!(mode_of(&env.home(&alice).join("drop")), 0o700);
    assert_eq!(mode_of(&env.home(&alice).join("drop/in")), 0o700);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upload_does_not_hold_up_the_socket_of_the_same_person() {
    let env = FilesEnv::start_with(Opts { write_delay: Some(Duration::from_millis(5)), ..Opts::default() }).await;
    let alice = env.person("alice");
    let mut ws = env.worker(&alice).await;
    let body = vec![9u8; 6 * 1024 * 1024];
    let upload = env.upload(&alice.signed, "six.bin", &body);
    let layout = async {
        let mut slowest = Duration::ZERO;
        for _ in 0..20 {
            let started = Instant::now();
            ws.ok("get_layout", "{}").await;
            slowest = slowest.max(started.elapsed());
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        slowest
    };
    let (reply, slowest) = tokio::join!(upload, layout);
    assert_eq!(reply.status, 200);
    assert!(slowest < Duration::from_secs(1), "a command took {slowest:?} during the upload");
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_but_the_file_is_in_the_folder_after_any_mix_of_good_and_refused_uploads() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    assert_eq!(env.upload(&alice.signed, "a.txt", b"one").await.status, 200);
    let mut cut = env.begin_upload(&alice.signed, "name=b.txt", 50).await;
    cut.send(b"half").await;
    drop(cut);
    assert_eq!(env.send(env.upload_request(&alice.signed, "name=a&name=b", b"x")).await.status, 400);
    assert_eq!(env.upload(&alice.signed, "c.txt", b"three").await.status, 200);
    env.env.eventually("only complete files", |_| uploads_of(&env, &alice) == vec!["a.txt".to_owned(), "c.txt".to_owned()]).await;
    let kinds: Vec<(String, u64, u32)> = tree(&env.home(&alice).join("uploads"));
    assert_eq!(kinds, vec![("a.txt".to_owned(), 3, 0o600), ("c.txt".to_owned(), 5, 0o600)]);
}

// ==== the download, completed =================================================================================================

const BIG: u64 = 70 * MIB;
const CHUNK: u64 = 8 * MIB;

/// A person with a worker and a sparse WAV file of `len` bytes granted: the socket (keep it alive), the grant.
async fn with_big_file(env: &FilesEnv, person: &ws_env::Person, len: u64) -> (ws_env::WsClient, files_env::Granted) {
    let mut ws = env.worker(person).await;
    let path = env.put_sparse_wav(person, "media/big.wav", len);
    let grant = env.grant(&mut ws, &path).await;
    (ws, grant)
}

/// A request without `Connection: close`, for a connection that carries several.
fn keep_alive(env: &FilesEnv, who: &edge::Signed, target: &str) -> edge_tls::Request {
    let mut request = env.get_request(Some(who), target, None);
    request.headers.retain(|(n, _)| !n.eq_ignore_ascii_case("connection"));
    request
}

#[tokio::test(flavor = "multi_thread")]
async fn a_range_is_a_206_with_its_content_range_its_length_and_exactly_its_bytes() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let (_ws, grant) = with_big_file(&env, &alice, BIG).await;
    assert!(grant.mime.starts_with("audio/"), "{}", grant.mime);
    assert_eq!(grant.size, BIG);
    let last = BIG - 1;
    let cases: [(&str, u64, u64); 9] = [
        ("bytes=0-99", 0, 99),
        ("bytes=100-199", 100, 199),
        ("bytes=0-0", 0, 0),
        ("bytes=1048000-2097300", 1_048_000, 2_097_300),
        ("bytes=-50", BIG - 50, last),
        ("bytes=73400000-", 73_400_000, last),
        ("bytes=100-", 100, 100 + CHUNK - 1),
        ("bytes=0-99999999999", 0, CHUNK - 1),
        ("bytes=8388000-9999999", 8_388_000, 9_999_999),
    ];
    for (header, start, end) in cases {
        let reply = env.get_range(&alice.signed, &grant.url, header).await;
        assert_eq!(reply.status, 206, "{header}");
        assert_eq!(reply.header("content-range"), Some(format!("bytes {start}-{end}/{BIG}").as_str()), "{header}");
        assert_eq!(reply.header("content-length"), Some((end - start + 1).to_string().as_str()), "{header}");
        assert_eq!(reply.header("content-type"), Some(grant.mime.as_str()), "{header}");
        assert_eq!(reply.body.len() as u64, end - start + 1, "{header}");
        assert!(reply.body == sparse_bytes(start, end - start + 1), "{header}: the bytes are not the bytes of that range");
        assert_file_headers(&reply);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_range_that_cannot_be_satisfied_is_416_with_the_size_and_no_body() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let (_ws, grant) = with_big_file(&env, &alice, BIG).await;
    let reads = env.probe.reads();
    for header in ["bytes=73400320-", "bytes=999999999999-", "bytes=50-10"] {
        let reply = env.get_range(&alice.signed, &grant.url, header).await;
        assert_eq!(reply.status, 416, "{header}");
        assert_eq!(reply.header("content-range"), Some(format!("bytes */{BIG}").as_str()), "{header}");
        assert!(reply.body.is_empty(), "{header}");
        assert_file_headers(&reply);
    }
    assert_eq!(env.probe.reads(), reads, "no byte was read for a range that cannot be met");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_over_the_whole_file_limit_without_a_range_is_413_with_the_field_range_and_reads_nothing() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let (_ws, grant) = with_big_file(&env, &alice, BIG).await;
    let reply = env.get(&alice.signed, &grant.url).await;
    assert_eq!(reply.status, 413);
    assert_eq!(reply.json(), json!({ "code": "bad_request", "field": "range" }));
    assert_file_headers(&reply);
    assert_eq!(env.probe.reads(), 0);
    // A header that is not one plain range is the whole file too, and so is refused the same way.
    let reply = env.get_range(&alice.signed, &grant.url, "bytes=0-1,5-6").await;
    assert_eq!(reply.status, 413);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_range_header_that_is_not_one_plain_range_gets_the_whole_small_file() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let path = env.put_png(&alice, "p.png", 3000);
    let mut ws = env.worker(&alice).await;
    let grant = env.grant(&mut ws, &path).await;
    for header in ["bytes=0-1,5-6", "items=0-1", "bytes=x-y", "bytes=-0", "", "bytes", "bytes=", "BYTES=0-5"] {
        let reply = env.get_range(&alice.signed, &grant.url, header).await;
        assert_eq!(reply.status, 200, "{header:?}");
        assert_eq!(reply.body, png(3000), "{header:?}");
    }
    // Two Range headers are not one range.
    let request = env.get_request(Some(&alice.signed), &grant.url, Some("bytes=0-9")).header("Range", "bytes=12-19");
    let reply = env.send(request).await;
    assert_eq!((reply.status, reply.body.len()), (200, 3000));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_whole_file_is_streamed_in_pieces_of_1_mib_and_every_byte_arrives() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let size = 5 * MIB + 777;
    let path = env.put_png(&alice, "big.png", usize::try_from(size).expect("small"));
    let mut ws = env.worker(&alice).await;
    let grant = env.grant(&mut ws, &path).await;
    let reply = env.get(&alice.signed, &grant.url).await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("content-length"), Some(size.to_string().as_str()));
    assert!(reply.body == png(usize::try_from(size).expect("small")));
    assert_eq!(env.probe.reads(), 6, "five full pieces and the rest");
    assert_eq!(env.probe.read_bytes(), size);
    assert_file_headers(&reply);
}

#[tokio::test(flavor = "multi_thread")]
async fn head_answers_the_headers_of_the_get_with_no_body_and_reads_nothing() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let (_ws, grant) = with_big_file(&env, &alice, BIG).await;
    let mut conn = env.env.connection().await;
    let mut head = keep_alive(&env, &alice.signed, &grant.url);
    head.method = "HEAD".into();
    head = head.header("Range", "bytes=1048576-2097151");
    conn.send(&head.bytes()).await;
    let reply = conn.reply(true).await.expect("an answer");
    assert_eq!(reply.status, 206);
    assert_eq!(reply.header("content-range"), Some(format!("bytes 1048576-2097151/{BIG}").as_str()));
    assert_eq!(reply.header("content-length"), Some("1048576"));
    assert_eq!(reply.header("content-type"), Some(grant.mime.as_str()));
    assert_file_headers(&reply);
    // On the same connection: HEAD of the whole file is the 413 of the GET, and HEAD of an unknown token is a 404.
    let mut head = keep_alive(&env, &alice.signed, &grant.url);
    head.method = "HEAD".into();
    conn.send(&head.bytes()).await;
    assert_eq!(conn.reply(true).await.expect("an answer").status, 413);
    let mut head = keep_alive(&env, &alice.signed, "/files/0123456789abcdef0123456789abcdef");
    head.method = "HEAD".into();
    conn.send(&head.bytes()).await;
    assert_eq!(conn.reply(true).await.expect("an answer").status, 404);
    assert_eq!(env.probe.reads(), 0, "a HEAD reads no byte");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_reader_holds_back_the_reads_and_a_client_that_leaves_stops_them() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let size = 40 * MIB;
    let (_ws, grant) = with_big_file(&env, &alice, size).await;
    let mut slow = files_env::SlowGet::start(&env, env.get_request(Some(&alice.signed), &grant.url, None)).await;
    assert_eq!(slow.status, 200);
    assert_eq!(slow.header("content-length"), Some(size.to_string().as_str()));
    // The client reads nothing: the server stops asking the worker once what the connection can hold is full.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let held = env.probe.read_bytes();
    assert!(held < 24 * MIB, "the worker read {held} bytes for a client that read none of 40 MiB");
    assert!(held > 0);
    // Reading some lets it go on, but only as far as the client has taken.
    let mut taken = slow.early.len() as u64;
    while taken < 4 * MIB {
        let n = slow.read(64 * 1024).await;
        assert!(n > 0);
        taken += n as u64;
    }
    slow.close().await;
    // The client left: the reads stop, far from the end of the file.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let (reads, bytes) = (env.probe.reads(), env.probe.read_bytes());
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!((env.probe.reads(), env.probe.read_bytes()), (reads, bytes), "reads went on after the client left");
    assert!(bytes < size, "{bytes}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_that_dies_in_the_middle_ends_the_connection_before_the_promised_length() {
    use holzwave_server::supervisor::launcher::WorkerLauncher;
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let size = 40 * MIB;
    let (_ws, grant) = with_big_file(&env, &alice, size).await;
    let mut slow = files_env::SlowGet::start(&env, env.get_request(Some(&alice.signed), &grant.url, None)).await;
    assert_eq!(slow.status, 200);
    let mut taken = slow.early.len() as u64;
    while taken < MIB {
        let n = slow.read(64 * 1024).await;
        assert!(n > 0);
        taken += n as u64;
    }
    env.env.launcher.kill_uid(alice.key.uid).await.expect("kill");
    let started = Instant::now();
    let rest = slow.rest().await;
    assert!(taken + (rest.len() as u64) < size, "the body ended short ({} of {size})", taken + rest.len() as u64);
    assert!(started.elapsed() < Duration::from_secs(10), "the connection ended at once, not by a timeout");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_is_gone_after_the_grant_is_a_404_and_not_a_200_with_a_broken_body() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let path = env.put_png(&alice, "p.png", 3000);
    let mut ws = env.worker(&alice).await;
    let grant = env.grant(&mut ws, &path).await;
    assert_eq!(env.get(&alice.signed, &grant.url).await.status, 200);
    std::fs::remove_file(&path).expect("remove");
    let reply = env.get(&alice.signed, &grant.url).await;
    assert_eq!(reply.status, 404);
    assert_eq!(reply.json(), json!({ "code": "not_found" }));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_eighth_download_of_a_person_and_the_sixty_fifth_of_all_are_503_busy_until_one_ends() {
    let limits = Limits { downloads_per_person: 1, downloads_total: 2, ..Limits::DEFAULT };
    let env = FilesEnv::start_with(Opts { limits: Some(limits), ..Opts::default() }).await;
    let (alice, bob, carol) = (env.person("alice"), env.person("bob"), env.person("carol"));
    let (_wa, ga) = with_big_file(&env, &alice, 40 * MIB).await;
    let (_wb, gb) = with_big_file(&env, &bob, 40 * MIB).await;
    let (_wc, gc) = with_big_file(&env, &carol, 40 * MIB).await;
    let first = files_env::SlowGet::start(&env, env.get_request(Some(&alice.signed), &ga.url, None)).await;
    assert_eq!(first.status, 200);
    let again = env.get(&alice.signed, &ga.url).await;
    assert_eq!((again.status, again.json()), (503, json!({ "code": "busy" })), "one per person");
    assert_file_headers(&again);
    let second = files_env::SlowGet::start(&env, env.get_request(Some(&bob.signed), &gb.url, None)).await;
    assert_eq!(second.status, 200);
    let full = env.get(&carol.signed, &gc.url).await;
    assert_eq!((full.status, full.json()), (503, json!({ "code": "busy" })), "the server is full");
    // A small answer without a stream needs no place: a range of one byte is still a stream, a 416 is not.
    assert_eq!(env.get_range(&carol.signed, &gc.url, "bytes=999999999999-").await.status, 416);
    first.close().await;
    let mut freed = 0;
    for _ in 0..40 {
        let retry = env.get_range(&carol.signed, &gc.url, "bytes=0-9").await;
        if retry.status == 206 {
            freed += 1;
            break;
        }
        assert_eq!(retry.status, 503);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(freed, 1, "the place came back when the first download ended");
    second.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grant_stops_working_after_the_idle_time_and_each_use_renews_it() {
    let env = FilesEnv::start_with(Opts { grant_idle: Some(Duration::from_millis(1200)), ..Opts::default() }).await;
    let alice = env.person("alice");
    let path = env.put_png(&alice, "p.png", 500);
    let mut ws = env.worker(&alice).await;

    let idle = env.grant(&mut ws, &path).await;
    assert_eq!(env.get(&alice.signed, &idle.url).await.status, 200);
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let reply = env.get(&alice.signed, &idle.url).await;
    assert_eq!((reply.status, reply.json()), (404, json!({ "code": "not_found" })), "unused for longer than the idle time");

    let used = env.grant(&mut ws, &path).await;
    for _ in 0..5 {
        assert_eq!(env.get(&alice.signed, &used.url).await.status, 200);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    tokio::time::sleep(Duration::from_millis(1600)).await;
    assert_eq!(env.get(&alice.signed, &used.url).await.status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_grant_is_a_404_at_once_and_a_new_grant_is_a_new_token() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let path = env.put_png(&alice, "p.png", 500);
    let mut ws = env.worker(&alice).await;
    let grant = env.grant(&mut ws, &path).await;
    assert_eq!(env.get(&alice.signed, &grant.url).await.status, 200);
    ws.ok("file_revoke", &json!({ "token": grant.token }).to_string()).await;
    assert_eq!(env.get(&alice.signed, &grant.url).await.status, 404);
    let again = env.grant(&mut ws, &path).await;
    assert_ne!(again.token, grant.token);
    assert_eq!(env.get(&alice.signed, &again.url).await.status, 200);
    assert_eq!(env.get(&alice.signed, &grant.url).await.status, 404, "the old token stays dead");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_needs_the_session_at_every_request_and_a_signed_out_one_is_401() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let path = env.put_png(&alice, "p.png", 500);
    let mut ws = env.worker(&alice).await;
    let grant = env.grant(&mut ws, &path).await;
    assert_eq!(env.get(&alice.signed, &grant.url).await.status, 200);
    env.env.test.sessions.end(&alice.signed.session).expect("end");
    let reply = env.get(&alice.signed, &grant.url).await;
    assert_eq!((reply.status, reply.json()), (401, json!({ "code": "invalid" })));
    assert_eq!(reply.header("x-content-type-options"), Some("nosniff"));
    // A new sign-in of the same person gets the same file with the same token.
    let again = env.env.sign_in_again(&alice);
    assert_eq!(env.get(&again, &grant.url).await.status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_that_was_killed_has_lost_its_grants_and_the_route_does_not_start_one() {
    use holzwave_server::supervisor::launcher::WorkerLauncher;
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let path = env.put_png(&alice, "p.png", 500);
    let mut ws = env.worker(&alice).await;
    let grant = env.grant(&mut ws, &path).await;
    env.env.launcher.kill_uid(alice.key.uid).await.expect("kill");
    drop(ws);
    env.env.eventually("the worker is gone", |e| e.counts(&alice).workers == 0).await;
    let launches = env.env.launcher.launches();
    let reply = env.get(&alice.signed, &grant.url).await;
    assert_eq!((reply.status, reply.json()), (404, json!({ "code": "not_found" })));
    assert_eq!(env.env.launcher.launches(), launches, "a download starts no worker");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_people_each_get_their_own_file_and_each_others_token_is_a_404() {
    let env = FilesEnv::start().await;
    let (alice, bob) = (env.person("alice"), env.person("bob"));
    let pa = env.put_png(&alice, "a.png", 1000);
    let pb = env.put_png(&bob, "b.png", 2000);
    let (mut wa, mut wb) = (env.worker(&alice).await, env.worker(&bob).await);
    let (ga, gb) = (env.grant(&mut wa, &pa).await, env.grant(&mut wb, &pb).await);
    assert_ne!(ga.token, gb.token);
    assert_eq!(env.get(&alice.signed, &ga.url).await.body, png(1000));
    assert_eq!(env.get(&bob.signed, &gb.url).await.body, png(2000));
    for (who, url) in [(&alice, &gb.url), (&bob, &ga.url)] {
        let reply = env.get(&who.signed, url).await;
        assert_eq!((reply.status, reply.json()), (404, json!({ "code": "not_found" })), "{}", who.name);
    }
}

/// The headers of an answer that say something about the request, in a fixed order, without the date and the connection.
fn comparable(reply: &edge_tls::Reply) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = reply
        .headers
        .iter()
        .filter(|(n, _)| !n.eq_ignore_ascii_case("date") && !n.eq_ignore_ascii_case("connection"))
        .map(|(n, v)| (n.to_ascii_lowercase(), v.clone()))
        .collect();
    headers.sort();
    headers
}

#[tokio::test(flavor = "multi_thread")]
async fn two_thousand_random_tokens_and_a_foreign_token_get_one_and_the_same_404() {
    let env = FilesEnv::start().await;
    let (alice, bob) = (env.person("alice"), env.person("bob"));
    let path = env.put_png(&alice, "p.png", 500);
    let (mut wa, _wb) = (env.worker(&alice).await, env.worker(&bob).await);
    let grant = env.grant(&mut wa, &path).await;

    let reference = env.get(&alice.signed, "/files/00000000000000000000000000000000").await;
    assert_eq!(reference.status, 404);
    let reference_headers = comparable(&reference);
    let reference_body = reference.body.clone();

    let mut state = 0xDEAD_BEEF_CAFE_F00Du64;
    let mut random_token = move || {
        let mut text = String::new();
        for _ in 0..4 {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            text.push_str(&format!("{:08x}", (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32));
        }
        text
    };
    let calls = env.env.supervisor.stats().file_calls;
    let mut unknown_times = Vec::new();
    for _ in 0..4 {
        let mut conn = env.env.connection().await;
        for _ in 0..500 {
            let token = random_token();
            assert_ne!(token, grant.token);
            let request = keep_alive(&env, &alice.signed, &format!("/files/{token}"));
            let started = Instant::now();
            conn.send(&request.bytes()).await;
            let reply = conn.reply(false).await.expect("an answer");
            unknown_times.push(started.elapsed());
            assert_eq!(reply.status, 404);
            assert_eq!(reply.body, reference_body);
            assert_eq!(comparable(&reply), reference_headers);
        }
    }
    assert_eq!(env.env.supervisor.stats().file_calls, calls + 2000, "each one asked alice's worker once, and only hers");

    // Bob, who has a worker too, asking for alice's real token: the same answer to the byte. The time of that and of an unknown
    // token is measured the same way, on a new connection each.
    let mut foreign_times = Vec::new();
    let mut unknown_new_times = Vec::new();
    for _ in 0..200 {
        let started = Instant::now();
        let reply = env.get(&bob.signed, &grant.url).await;
        foreign_times.push(started.elapsed());
        assert_eq!(reply.status, 404);
        assert_eq!(reply.body, reference_body);
        assert_eq!(comparable(&reply), reference_headers);
        let started = Instant::now();
        let reply = env.get(&bob.signed, &format!("/files/{}", random_token())).await;
        unknown_new_times.push(started.elapsed());
        assert_eq!((reply.status, reply.body), (404, reference_body.clone()));
    }
    // The time is recorded, not pinned to a number: the median of an unknown token and of a foreign one are of one order.
    unknown_times.sort();
    foreign_times.sort();
    unknown_new_times.sort();
    let median = |times: &[Duration]| times[times.len() / 2];
    let (kept, foreign, unknown) = (median(&unknown_times), median(&foreign_times), median(&unknown_new_times));
    eprintln!("TIMING median of 2000 unknown tokens on a kept connection {kept:?}; on new connections: 200 unknown {unknown:?}, 200 foreign {foreign:?}");
    assert!(kept < Duration::from_millis(200) && foreign < Duration::from_millis(500) && unknown < Duration::from_millis(500));
    let (low, high) = (foreign.min(unknown), foreign.max(unknown));
    assert!(high < low * 4 + Duration::from_millis(2), "foreign {foreign:?} and unknown {unknown:?} differ by more than noise");
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_paths_are_a_404_and_no_worker_is_asked() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let path = env.put_png(&alice, "p.png", 500);
    let mut ws = env.worker(&alice).await;
    let grant = env.grant(&mut ws, &path).await;
    let t = grant.token.clone();
    let upper = t.to_uppercase();
    let targets = [
        "/files".to_owned(),
        "/files/".to_owned(),
        "/files//".to_owned(),
        "//files/x".to_owned(),
        format!("//files/{t}"),
        "/files/..".to_owned(),
        "/files/../api/session".to_owned(),
        "/files/%2e%2e".to_owned(),
        "/files/%2e%2e%2f".to_owned(),
        format!("/files/{t}/"),
        format!("/files/{t}/x"),
        format!("/files/{t}/../{t}"),
        format!("/files/{t}%2f"),
        format!("/files/{t}%00"),
        format!("/files/{t}%20"),
        format!("/files/{t}.png"),
        format!("/files/%{:02x}{}", t.as_bytes()[0], &t[1..]),
        format!("/files/{upper}"),
        format!("/FILES/{t}"),
        format!("/files/{}", "a".repeat(5000)),
        "/files/\u{e9}".replace('\u{e9}', "%C3%A9"),
        format!("/files/{t};x=y"),
        format!("/files/{t}\\"),
        "/files/%00".to_owned(),
        "/files/%25".to_owned(),
        format!("/files/{}%25{}", &t[..10], &t[12..]),
    ];
    let calls = env.env.supervisor.stats().file_calls;
    for target in &targets {
        let reply = env.get(&alice.signed, target).await;
        assert_eq!(reply.status, 404, "{:.60}", target);
        assert_eq!(reply.json(), json!({ "code": "not_found" }), "{:.60}", target);
    }
    assert_eq!(env.env.supervisor.stats().file_calls, calls, "no hostile path reached a worker");
    // The query string is not part of the token and is ignored.
    assert_eq!(env.get(&alice.signed, &format!("/files/{t}?token=other&x=1")).await.status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn other_methods_on_a_file_path_are_405_with_the_plain_error() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let path = env.put_png(&alice, "p.png", 500);
    let mut ws = env.worker(&alice).await;
    let grant = env.grant(&mut ws, &path).await;
    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        let request = env
            .upload_request(&alice.signed, "", b"{}")
            .header("X-Unused", "1");
        let mut request = request;
        request.method = method.to_owned();
        request.target = grant.url.clone();
        request.headers.retain(|(n, _)| !n.eq_ignore_ascii_case("content-type"));
        request = request.header("Content-Type", "application/json");
        let reply = env.send(request).await;
        assert_eq!(reply.status, 405, "{method}");
        assert_eq!(reply.json(), json!({ "code": "bad_request" }), "{method}");
    }
    assert_eq!(env.env.supervisor.stats().file_calls, 0, "no worker was asked for a method that is not served");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_response_of_the_download_route_carries_the_same_header_set() {
    let limits = Limits { downloads_per_person: 1, ..Limits::DEFAULT };
    let env = FilesEnv::start_with(Opts { limits: Some(limits), ..Opts::default() }).await;
    let alice = env.person("alice");
    let (_ws, big) = with_big_file(&env, &alice, BIG).await;
    let small_path = env.put_png(&alice, "p.png", 500);
    let mut ws = env.worker(&alice).await;
    let small = env.grant(&mut ws, &small_path).await;

    let ok = env.get(&alice.signed, &small.url).await;
    let partial = env.get_range(&alice.signed, &big.url, "bytes=0-9").await;
    let unknown = env.get(&alice.signed, "/files/ffffffffffffffffffffffffffffffff").await;
    let malformed = env.get(&alice.signed, "/files/short").await;
    let too_large = env.get(&alice.signed, &big.url).await;
    let unsatisfiable = env.get_range(&alice.signed, &big.url, "bytes=999999999999-").await;
    let hold = files_env::SlowGet::start(&env, env.get_request(Some(&alice.signed), &big.url, Some("bytes=0-8388607"))).await;
    let busy = env.get(&alice.signed, &small.url).await;
    hold.close().await;

    let expected = [(&ok, 200), (&partial, 206), (&unknown, 404), (&malformed, 404), (&too_large, 413), (&unsatisfiable, 416), (&busy, 503)];
    for (reply, status) in expected {
        assert_eq!(reply.status, status);
        assert_file_headers(reply);
        assert!(reply.header("strict-transport-security").is_some(), "{status}");
        assert!(reply.header("referrer-policy").is_some(), "{status}");
        assert!(reply.all("access-control-allow-origin").is_empty(), "{status}");
        assert!(reply.all("set-cookie").is_empty(), "{status}");
        assert!(reply.all("content-disposition").is_empty(), "{status}");
    }
    assert_eq!(ok.header("content-type"), Some("image/png"));
}

#[tokio::test(flavor = "multi_thread")]
async fn only_what_file_grant_allows_can_be_downloaded_a_text_file_gets_no_token() {
    let env = FilesEnv::start().await;
    let alice = env.person("alice");
    let text = env.put_file(&alice, "notes.txt", b"just text");
    let mut ws = env.worker(&alice).await;
    let args = json!({ "source": "local", "path": text.to_str().expect("UTF-8") }).to_string();
    let answer = ws.call("file_grant", &args).await;
    assert!(answer.get("err").is_some(), "{answer}");
    assert_eq!(env.env.supervisor.stats().file_calls, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_a_person_does_with_files_reaches_the_logs_the_audit_log_or_another_persons_home() {
    let env = FilesEnv::start().await;
    let (alice, bob) = (env.person("alice"), env.person("bob"));
    let marker = b"CANARY-CONTENT-8f3a1c";
    let mut content = png(300);
    content.extend_from_slice(marker);
    let path = env.put_file(&alice, "private-name-9d2e.png", &content);
    let mut ws = env.worker(&alice).await;
    let _bob_ws = env.worker(&bob).await;
    let grant = env.grant(&mut ws, &path).await;
    assert_eq!(env.get(&alice.signed, &grant.url).await.status, 200);
    let up = env.upload(&alice.signed, "uploaded-name-4b7c.txt", marker).await;
    assert_eq!(up.status, 200);
    // Hostile and refused requests too.
    for target in ["/files/ffffffffffffffffffffffffffffffff", "/files/../x"] {
        let _ = env.get(&alice.signed, target).await;
    }
    let _ = env.get(&bob.signed, &grant.url).await;

    let root = env.home(&alice).parent().and_then(Path::parent).expect("the root of the homes").to_path_buf();
    // The bytes of the file are in alice's home and nowhere else.
    let holding = files_env::files_holding(&root, marker);
    assert_eq!(holding.len(), 2, "{holding:?}");
    assert!(holding.iter().all(|p| p.starts_with(env.home(&alice))), "{holding:?}");
    // The token, the cookie and the anti-forgery token are in no file of the server: not the store, not the audit log, not a home.
    for secret in [grant.token.as_bytes(), alice.signed.cookie.as_bytes(), alice.signed.csrf.as_bytes()] {
        let found = files_env::files_holding(&root, secret);
        assert!(found.is_empty(), "a secret is in {found:?}");
    }
    let audit = files_env::files_holding(&env.env.test.audit_dir, b"private-name-9d2e");
    assert!(audit.is_empty(), "{audit:?}");
    let audit = files_env::files_holding(&env.env.test.audit_dir, b"uploaded-name-4b7c");
    assert!(audit.is_empty(), "{audit:?}");
    // Bob's home holds nothing of alice's.
    assert!(files_env::tree(&env.home(&bob)).iter().all(|(p, _, _)| !p.contains("uploads") && !p.contains("private-name")));
}
