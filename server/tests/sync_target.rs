//! The server as a sync target, through the real router and the real store: the gate and the token in front of it, the four
//! routes with their strong tags and the three condition forms, the grammar of a path, what the server checks of a body and
//! the bounds it holds a person to, and, with the product's own sync engine and its own client backend over a socket, the
//! round trip, the conflict rule, what the server keeps and never keeps, and the separation of one person from another.
//!
//! The router is called as a `tower` service (no socket) over the real services of `support/api_env.rs`, except where a real
//! client is the point, which are served on the loopback address. The manual clock of the environment moves time.

#![cfg(feature = "in-process-workers")]

#[path = "support/api_env.rs"]
mod api_env;
#[path = "support/auth_env.rs"]
mod auth_env;
#[path = "support/device_env.rs"]
mod device_env;
#[path = "support/edge.rs"]
mod edge;

use api_env::ApiEnv;
use axum::http::Method;
use device_env::{bearer, grant_flow, native, shape, turn_on};
use edge::{Req, Resp, Signed};
use holzwave_core::sync::crypto::{FORMAT, MAGIC};
use holzwave_core::sync::webdav::sha256_hex;
use holzwave_server::http::features::Feature;
use holzwave_server::sync_target::Bounds;

// ---- helpers ---------------------------------------------------------------------------------------------------------------

/// A body with the header the sync format gives a sealed file: the magic, the format, a counter and a time as the stamp, a
/// device, and then `fill` bytes standing for the nonce, the sealed text and the tag. The server never opens it, so a body of
/// this shape is all it can tell from a real one.
fn blob(counter: u64, fill: u8, extra: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.push(FORMAT);
    bytes.extend_from_slice(&counter.to_be_bytes());
    bytes.extend_from_slice(&1_700_000_000i64.to_be_bytes());
    bytes.extend_from_slice(&[9u8; 16]);
    bytes.extend(std::iter::repeat_n(fill, 24 + 16 + extra));
    bytes
}

/// The smallest body that passes the structure check.
const SMALLEST: usize = 3 + 1 + 8 + 8 + 16 + 24 + 16;

fn tag(body: &[u8]) -> String {
    sha256_hex(body)
}

fn quoted(body: &[u8]) -> String {
    format!("\"{}\"", tag(body))
}

/// A device of `name`: the sync switch is on (the remote one is not) and the person has a token.
struct Person {
    who: Signed,
    token: String,
}

async fn person(env: &ApiEnv, name: &str) -> Person {
    env.app.features.set(Feature::Sync, true).expect("the switch is written");
    let who = env.member(name);
    let token = grant_flow(env, &who, &format!("{name}'s laptop")).await.token;
    Person { who, token }
}

fn put(path: &str, token: &str, body: Vec<u8>) -> Req {
    bearer(Method::PUT, &format!("/sync/v1/blob/{path}"), token).header("content-type", "application/octet-stream").bytes(body)
}

fn put_if_absent(path: &str, token: &str, body: Vec<u8>) -> Req {
    put(path, token, body).header("if-none-match", "*")
}

fn put_if_match(path: &str, token: &str, etag: &str, body: Vec<u8>) -> Req {
    put(path, token, body).header("if-match", &format!("\"{etag}\""))
}

fn get(path: &str, token: &str) -> Req {
    bearer(Method::GET, &format!("/sync/v1/blob/{path}"), token)
}

fn list(dir: &str, token: &str) -> Req {
    bearer(Method::GET, &format!("/sync/v1/list/{dir}"), token)
}

fn ensure(dir: &str, token: &str) -> Req {
    bearer(Method::POST, &format!("/sync/v1/ensure/{dir}"), token)
}

fn code(resp: &Resp) -> String {
    resp.json()["code"].as_str().unwrap_or_default().to_owned()
}

/// An answer without the length line: the connection adds it to every answer alike, and the router's own paths differ in it
/// only because they are called without one.
fn wire_shape(resp: &Resp) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let (status, headers, body) = shape(resp);
    (status, headers.into_iter().filter(|(name, _)| name != "content-length").collect(), body)
}

/// The status and the `code` of an answer.
fn answer(resp: &Resp) -> (u16, String) {
    (resp.status.as_u16(), code(resp))
}

fn names(resp: &Resp) -> Vec<String> {
    resp.json().as_array().expect("an array").iter().map(|e| e["name"].as_str().expect("a name").to_owned()).collect()
}

// ---------------------------------------------------------------------------------------------------------------------
// The gate and the token
// ---------------------------------------------------------------------------------------------------------------------

/// The four requests of a client, in the forms the sync transport sends them.
fn four(token: Option<&str>) -> Vec<(&'static str, Req)> {
    let with = |req: Req| match token {
        Some(token) => req.header("authorization", &format!("Bearer {token}")),
        None => req,
    };
    let body = blob(1, 1, 8);
    vec![
        ("ensure", with(native(Method::POST, "/sync/v1/ensure/files"))),
        ("list", with(native(Method::GET, "/sync/v1/list/files"))),
        ("get", with(native(Method::GET, "/sync/v1/blob/files/a"))),
        ("put", with(native(Method::PUT, "/sync/v1/blob/files/a").header("if-none-match", "*").bytes(body))),
    ]
}

#[tokio::test]
async fn with_the_sync_switch_off_every_sync_route_answers_what_an_unknown_path_answers_and_with_it_on_a_token_is_needed() {
    let env = ApiEnv::new();
    // A token from a grant needs one of the two switches on; then every switch goes off again.
    turn_on(&env, Feature::Remote);
    let who = env.member("alice");
    let token = grant_flow(&env, &who, "laptop").await.token;
    assert!(env.app.features.set(Feature::Remote, false).expect("the switch is written"));

    // The answer of a path that is not there, for each of the methods used.
    let unknown = |method: Method| native(method, "/no-such-place/files/a");
    let absent = [
        wire_shape(&env.send(unknown(Method::POST)).await),
        wire_shape(&env.send(unknown(Method::GET)).await),
        wire_shape(&env.send(unknown(Method::PUT)).await),
    ];
    assert_eq!(absent[0].0, 404);

    for state in ["every switch off", "only the remote switch on"] {
        if state == "only the remote switch on" {
            turn_on(&env, Feature::Remote);
        }
        for credential in [Some(token.as_str()), None, Some("hwd_0000000000000000000000000000000000000000000")] {
            for (name, request) in four(credential) {
                let resp = env.send(request).await;
                let same = match name {
                    "ensure" => &absent[0],
                    "put" => &absent[2],
                    _ => &absent[1],
                };
                assert_eq!(&wire_shape(&resp), same, "{name} with {credential:?} while {state}");
            }
        }
        // A session cookie of a signed-in person reads no differently.
        let cookie = native(Method::GET, "/sync/v1/list/files").cookie(&who.cookie);
        assert_eq!(&wire_shape(&env.send(cookie).await), &absent[1], "a cookie while {state}");
    }

    // With the sync switch on and no usable credential every route says 401 and nothing else.
    turn_on(&env, Feature::Sync);
    for (name, request) in four(None) {
        let resp = env.send(request).await;
        assert_eq!(answer(&resp), (401, "invalid".to_owned()), "{name} without a token");
    }
    for (name, request) in four(Some("hwd_0000000000000000000000000000000000000000000")) {
        let resp = env.send(request).await;
        assert_eq!(answer(&resp), (401, "invalid".to_owned()), "{name} with an unknown token");
    }
    for (name, request) in four(None) {
        let resp = env.send(request.cookie(&who.cookie).csrf(&who.csrf)).await;
        assert_eq!(answer(&resp), (401, "invalid".to_owned()), "{name} with a session and no token");
        assert!(resp.all("set-cookie").is_empty());
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// One blob, written and read back
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_blob_is_written_with_a_content_hash_tag_read_back_and_listed() {
    let env = ApiEnv::new();
    let alice = person(&env, "alice").await;
    let body = blob(3, 0xAB, 100);

    let created = env.send(put_if_absent("files/settings.json.hwx", &alice.token, body.clone())).await;
    assert_eq!(created.status, 201, "{}", created.text());
    assert_eq!(created.header("etag"), Some(quoted(&body)));
    assert_eq!(created.header("cache-control").as_deref(), Some("no-store"));
    assert!(created.body.is_empty());
    assert!(created.all("set-cookie").is_empty());

    let read = env.send(get("files/settings.json.hwx", &alice.token)).await;
    assert_eq!(read.status, 200);
    assert_eq!(read.body, body);
    assert_eq!(read.header("etag"), Some(quoted(&body)));
    assert_eq!(read.header("content-type").as_deref(), Some("application/octet-stream"));
    assert_eq!(read.header("cache-control").as_deref(), Some("no-store"));

    let listed = env.send(list("files", &alice.token)).await;
    assert_eq!(listed.status, 200);
    assert_eq!(listed.header("cache-control").as_deref(), Some("no-store"));
    let entries = listed.json();
    let entries = entries.as_array().expect("an array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "settings.json.hwx");
    assert_eq!(entries[0]["etag"], tag(&body));
    assert_eq!(entries[0]["checksum"], entries[0]["etag"]);
    let mut members: Vec<_> = entries[0].as_object().expect("an object").keys().cloned().collect();
    members.sort();
    assert_eq!(members, ["checksum", "etag", "name"]);

    // The other folders do not show it, and a name that is not there is 404.
    assert!(names(&env.send(list("backups", &alice.token)).await).is_empty());
    assert!(names(&env.send(list("-", &alice.token)).await).is_empty());
    let missing = env.send(get("files/nothing.hwx", &alice.token)).await;
    assert_eq!(answer(&missing), (404, "not_found".to_owned()));
}

#[tokio::test]
async fn the_three_condition_forms_decide_whether_a_write_goes_through() {
    let env = ApiEnv::new();
    let alice = person(&env, "alice").await;
    let first = blob(1, 1, 10);
    let second = blob(2, 2, 10);
    let third = blob(3, 3, 10);
    let path = "files/a.json.hwx";

    assert_eq!(env.send(put_if_absent(path, &alice.token, first.clone())).await.status, 201);
    // It exists now: a write that wants it absent loses and stores nothing.
    let lost = env.send(put_if_absent(path, &alice.token, second.clone())).await;
    assert_eq!(answer(&lost), (412, "precondition_failed".to_owned()));
    assert_eq!(env.send(get(path, &alice.token)).await.body, first);

    // The tag of the current file lets a replacement through; another tag does not.
    let replaced = env.send(put_if_match(path, &alice.token, &tag(&first), second.clone())).await;
    assert_eq!(replaced.status, 200, "{}", replaced.text());
    assert_eq!(replaced.header("etag"), Some(quoted(&second)));
    let stale = env.send(put_if_match(path, &alice.token, &tag(&first), third.clone())).await;
    assert_eq!(stale.status, 412);
    let unknown = env.send(put_if_match(path, &alice.token, &"0".repeat(64), third.clone())).await;
    assert_eq!(unknown.status, 412);
    assert_eq!(env.send(get(path, &alice.token)).await.body, second);

    // A match on a file that is not there is a lost race too.
    assert_eq!(env.send(put_if_match("files/b.json.hwx", &alice.token, &tag(&first), first.clone())).await.status, 412);

    // With no condition the write is unconditional, and says whether it made or replaced.
    let any = env.send(put(path, &alice.token, third.clone())).await;
    assert_eq!((any.status.as_u16(), any.header("etag")), (200, Some(quoted(&third))));
    let made = env.send(put("files/c.json.hwx", &alice.token, first.clone())).await;
    assert_eq!(made.status, 201);

    // A condition the contract does not have is a bad request, whatever else the request says.
    for headers in [
        vec![("if-none-match", "*"), ("if-match", "\"x\"")],
        vec![("if-none-match", "\"x\"")],
        vec![("if-match", "x")],
        vec![("if-match", "*")],
        vec![("if-match", "W/\"x\"")],
        vec![("if-none-match", "*"), ("if-none-match", "*")],
    ] {
        let mut request = put("files/d.json.hwx", &alice.token, first.clone());
        for (name, value) in &headers {
            request = request.header(name, value);
        }
        let resp = env.send(request).await;
        assert_eq!(answer(&resp), (400, "bad_request".to_owned()), "{headers:?}");
    }
    assert_eq!(names(&env.send(list("files", &alice.token)).await), ["a.json.hwx", "c.json.hwx"]);
}

#[tokio::test]
async fn the_folder_and_the_path_grammar_refuse_everything_but_three_shapes() {
    let env = ApiEnv::new();
    let alice = person(&env, "alice").await;
    let body = blob(1, 1, 10);

    for dir in ["-", "files", "backups"] {
        let resp = env.send(ensure(dir, &alice.token)).await;
        assert_eq!(resp.status, 204, "{dir}");
        assert!(resp.body.is_empty());
        assert_eq!(env.send(list(dir, &alice.token)).await.status, 200, "{dir}");
    }
    for dir in ["meta.json", "File", "files%2F", "%66iles", "files.", "a", "..", "-x", "x-"] {
        let resp = env.send(ensure(dir, &alice.token)).await;
        assert_eq!(answer(&resp), (400, "bad_request".to_owned()), "ensure {dir}");
        let resp = env.send(list(dir, &alice.token)).await;
        assert_eq!(answer(&resp), (400, "bad_request".to_owned()), "list {dir}");
    }

    let long = format!("files/{}", "a".repeat(129));
    let edge = format!("files/{}", "a".repeat(128));
    let bad = [
        "files",
        "files/",
        "backups/",
        "files/..",
        "files/.",
        "files/a/b",
        "files/%2e%2e",
        "files/a%2Fb",
        "files%2Fa",
        "%66iles/a",
        "files/a%00",
        "files/a%20b",
        "files/%E4",
        "other/a",
        "meta.json/a",
        "metas.json",
        "../meta.json",
        "files/../meta.json",
        long.as_str(),
    ];
    for path in bad {
        let resp = env.send(put_if_absent(path, &alice.token, body.clone())).await;
        assert_eq!(answer(&resp), (400, "bad_request".to_owned()), "put {path}");
        let resp = env.send(get(path, &alice.token)).await;
        assert_eq!(resp.status, 400, "get {path}");
    }
    // 128 bytes is the limit of a name and is a file.
    assert_eq!(env.send(put_if_absent(&edge, &alice.token, body.clone())).await.status, 201);
    assert_eq!(env.send(get(&edge, &alice.token)).await.status, 200);
    // Nothing of the refused paths was stored.
    assert_eq!(names(&env.send(list("files", &alice.token)).await), [edge.trim_start_matches("files/")]);
}

// ---------------------------------------------------------------------------------------------------------------------
// What the server checks of a body, and the bounds
// ---------------------------------------------------------------------------------------------------------------------

/// What a person holds on the server, read from the store directly.
fn usage(env: &ApiEnv, who: &Signed) -> (usize, u64) {
    let used = env.store.read(|tx| tx.blob_usage(who.account)).expect("the usage");
    (used.files, used.bytes)
}

#[tokio::test]
async fn only_a_body_with_the_structure_of_a_sealed_file_is_stored_and_it_is_never_opened() {
    let env = ApiEnv::new();
    let alice = person(&env, "alice").await;
    let good = blob(1, 1, 0);
    assert_eq!(good.len(), SMALLEST);

    let mut other_magic = good.clone();
    other_magic[..3].copy_from_slice(b"HWY");
    let mut other_format = good.clone();
    other_format[3] = FORMAT + 1;
    let mut zero_format = good.clone();
    zero_format[3] = 0;
    let bad: [(&str, Vec<u8>); 7] = [
        ("empty", Vec::new()),
        ("one byte short", good[..SMALLEST - 1].to_vec()),
        ("the header alone", good[..36].to_vec()),
        ("another magic", other_magic),
        ("another format", other_format),
        ("format zero", zero_format),
        ("text", br#"{"font_size":14}"#.to_vec()),
    ];
    for folder in ["files", "backups"] {
        for (what, body) in &bad {
            let resp = env.send(put_if_absent(&format!("{folder}/x.hwx"), &alice.token, body.clone())).await;
            assert_eq!(answer(&resp), (400, "bad_request".to_owned()), "{what} in {folder}");
        }
    }
    assert_eq!(usage(&env, &alice.who), (0, 0), "nothing of the refused bodies was stored");

    // The smallest body that has the structure is stored, in either folder, and read back as it was sent.
    for folder in ["files", "backups"] {
        let path = format!("{folder}/x.hwx");
        assert_eq!(env.send(put_if_absent(&path, &alice.token, good.clone())).await.status, 201, "{folder}");
        assert_eq!(env.send(get(&path, &alice.token)).await.body, good);
    }
    // A body does not have to be a real sealed file: the server holds no key and can not tell.
    let random_tail = blob(7, 0x5A, 4000);
    assert_eq!(env.send(put("files/y.hwx", &alice.token, random_tail)).await.status, 201);
}

#[tokio::test]
async fn the_metadata_file_is_written_once_as_a_small_json_object() {
    let env = ApiEnv::new();
    let alice = person(&env, "alice").await;
    let meta = br#"{"format":1,"m_kib":65536,"t":3,"p":1,"salt":"00","check":"00"}"#.to_vec();
    let put_meta = |token: &str, body: Vec<u8>| put("meta.json", token, body);

    // Only "must not be there" is a condition it takes: a match, or none, is a request that makes no sense.
    for request in [put_meta(&alice.token, meta.clone()), put_meta(&alice.token, meta.clone()).header("if-match", &quoted(&meta))] {
        let resp = env.send(request).await;
        assert_eq!(answer(&resp), (400, "bad_request".to_owned()));
    }
    // A body that is not a JSON object, or is too large for the file, stores nothing.
    for (what, body) in [
        ("an array", b"[1,2]".to_vec()),
        ("a number", b"7".to_vec()),
        ("text", b"holzwave".to_vec()),
        ("an empty body", Vec::new()),
        ("a truncated object", br#"{"a":"#.to_vec()),
        ("a sealed file", blob(1, 1, 10)),
    ] {
        let resp = env.send(put_if_absent("meta.json", &alice.token, body)).await;
        assert_eq!(answer(&resp), (400, "bad_request".to_owned()), "{what}");
    }
    let mut too_big = br#"{"pad":""#.to_vec();
    too_big.extend(std::iter::repeat_n(b'a', 4096));
    too_big.extend_from_slice(b"\"}");
    let resp = env.send(put_if_absent("meta.json", &alice.token, too_big)).await;
    assert_eq!(answer(&resp), (413, "too_large".to_owned()));
    assert_eq!(usage(&env, &alice.who), (0, 0));

    // A file of exactly 4 KiB is the largest.
    let mut largest = br#"{"pad":""#.to_vec();
    let fill = 4096 - largest.len() - 2;
    largest.extend(std::iter::repeat_n(b'a', fill));
    largest.extend_from_slice(b"\"}");
    assert_eq!(largest.len(), 4096);
    let first = env.send(put_if_absent("meta.json", &alice.token, largest.clone())).await;
    assert_eq!(first.status, 201, "{}", first.text());

    // The second write loses, whoever sends it, and the first stays.
    let second = env.send(put_if_absent("meta.json", &alice.token, meta.clone())).await;
    assert_eq!(answer(&second), (412, "precondition_failed".to_owned()));
    assert_eq!(env.send(get("meta.json", &alice.token)).await.body, largest);
    let listed = env.send(list("-", &alice.token)).await;
    assert_eq!(names(&listed), ["meta.json"]);
}

#[tokio::test]
async fn a_body_over_the_cap_is_refused_as_too_large_before_it_is_read_and_a_declared_length_is_enough() {
    let bounds = Bounds { max_blob_bytes: 64 * 1024, ..Bounds::DEFAULT };
    let env = ApiEnv::with_sync(bounds);
    let alice = person(&env, "alice").await;

    let fits = blob(1, 1, 64 * 1024 - SMALLEST);
    assert_eq!(fits.len(), 64 * 1024);
    assert_eq!(env.send(put_if_absent("files/fits.hwx", &alice.token, fits)).await.status, 201);
    let over = blob(1, 1, 64 * 1024 - SMALLEST + 1);
    let resp = env.send(put_if_absent("files/over.hwx", &alice.token, over.clone())).await;
    assert_eq!(answer(&resp), (413, "too_large".to_owned()));

    // A declared length over the cap is enough: the body is not looked at, here it is even a different size.
    let declared = put_if_absent("files/declared.hwx", &alice.token, blob(1, 1, 8)).header("content-length", "1000000");
    assert_eq!(answer(&env.send(declared).await), (413, "too_large".to_owned()));

    // A body with no length, sent in pieces, is read only as far as the cap.
    let pieces: Vec<Vec<u8>> = over.chunks(10_000).map(<[u8]>::to_vec).collect();
    let chunked = bearer(Method::PUT, "/sync/v1/blob/files/chunked.hwx", &alice.token).header("if-none-match", "*").chunks(pieces);
    assert_eq!(answer(&env.send(chunked).await), (413, "too_large".to_owned()));
    assert_eq!(usage(&env, &alice.who), (1, 64 * 1024), "only the file that fit is stored");

    // The format's own limit holds whatever the operator says: the default is 8 MiB and nothing above it is a body.
    let big = ApiEnv::new();
    let bob = person(&big, "bob").await;
    let resp = big.send(put_if_absent("files/big.hwx", &bob.token, vec![b'H'; 8 * 1024 * 1024 + 1])).await;
    assert_eq!(answer(&resp), (413, "too_large".to_owned()));
}

#[tokio::test]
async fn a_write_over_the_space_or_the_number_of_files_is_refused_with_the_quota_code_and_stores_nothing() {
    let bounds = Bounds { quota_bytes: 1000, max_files: 3, backups_keep: 20, max_blob_bytes: 8 * 1024 * 1024 };
    let env = ApiEnv::with_sync(bounds);
    let alice = person(&env, "alice").await;
    let bob = person(&env, "bob").await;

    let a = blob(1, 1, 300 - SMALLEST);
    let b = blob(1, 2, 300 - SMALLEST);
    assert_eq!((a.len(), b.len()), (300, 300));
    assert_eq!(env.send(put_if_absent("files/a.hwx", &alice.token, a.clone())).await.status, 201);
    assert_eq!(env.send(put_if_absent("files/b.hwx", &alice.token, b.clone())).await.status, 201);
    assert_eq!(usage(&env, &alice.who), (2, 600));

    // 600 and another 500 is over 1000.
    let over = env.send(put_if_absent("files/c.hwx", &alice.token, blob(1, 3, 500 - SMALLEST))).await;
    assert_eq!(answer(&over), (507, "quota".to_owned()));
    assert_eq!(usage(&env, &alice.who), (2, 600), "nothing was stored");
    assert_eq!(env.send(get("files/c.hwx", &alice.token)).await.status, 404);

    // Replacing counts only the difference: a file of 300 becomes one of 700 (600 - 300 + 700 = 1000, not over).
    let grown = blob(2, 4, 700 - SMALLEST);
    assert_eq!(env.send(put_if_match("files/a.hwx", &alice.token, &tag(&a), grown.clone())).await.status, 200);
    assert_eq!(usage(&env, &alice.who), (2, 1000));
    // One byte more is over; shrinking it is always allowed.
    let one_more = blob(3, 5, 701 - SMALLEST);
    let resp = env.send(put("files/a.hwx", &alice.token, one_more)).await;
    assert_eq!(answer(&resp), (507, "quota".to_owned()));
    assert_eq!(env.send(put("files/a.hwx", &alice.token, blob(3, 6, 100 - SMALLEST))).await.status, 200);
    assert_eq!(usage(&env, &alice.who), (2, 400));

    // The number of files: three are allowed, a fourth is not, and the quota code is the same.
    assert_eq!(env.send(put_if_absent("files/c.hwx", &alice.token, blob(1, 7, 10))).await.status, 201);
    let fourth = env.send(put_if_absent("files/d.hwx", &alice.token, blob(1, 8, 10))).await;
    assert_eq!(answer(&fourth), (507, "quota".to_owned()));
    assert_eq!(usage(&env, &alice.who).0, 3);
    // A file that exists can still be replaced at the limit of the number.
    assert_eq!(env.send(put("files/c.hwx", &alice.token, blob(2, 9, 10))).await.status, 200);

    // Another person with the same names is not touched by this person's limits.
    assert_eq!(env.send(put_if_absent("files/a.hwx", &bob.token, a.clone())).await.status, 201);
    assert_eq!(env.send(put_if_absent("files/b.hwx", &bob.token, b)).await.status, 201);
    assert_eq!(usage(&env, &bob.who), (2, 600));
    assert_eq!(usage(&env, &alice.who), (3, 100 + 300 + 86));
}

#[tokio::test]
async fn after_each_write_into_the_backups_folder_only_the_newest_ones_stay() {
    let bounds = Bounds { backups_keep: 3, ..Bounds::DEFAULT };
    let env = ApiEnv::with_sync(bounds);
    let alice = person(&env, "alice").await;
    let bob = person(&env, "bob").await;
    assert_eq!(env.send(put_if_absent("files/keep.hwx", &alice.token, blob(1, 1, 10))).await.status, 201);

    for n in 1..=6u8 {
        env.clock.advance(10);
        let resp = env.send(put_if_absent(&format!("backups/a.json.{n}.hwx"), &alice.token, blob(u64::from(n), n, 10))).await;
        assert_eq!(resp.status, 201, "{}", resp.text());
        let kept = names(&env.send(list("backups", &alice.token)).await);
        let from = usize::from(n).saturating_sub(3) + 1;
        let want: Vec<String> = (from..=usize::from(n)).map(|k| format!("a.json.{k}.hwx")).collect();
        assert_eq!(kept, want, "after backup {n}");
    }
    // The pruned ones are gone, the live files and the other person's backups are not touched.
    assert_eq!(env.send(get("backups/a.json.1.hwx", &alice.token)).await.status, 404);
    assert_eq!(names(&env.send(list("files", &alice.token)).await), ["keep.hwx"]);
    assert_eq!(env.send(put_if_absent("backups/a.json.1.hwx", &bob.token, blob(1, 1, 10))).await.status, 201);
    assert_eq!(env.send(put_if_absent("backups/a.json.2.hwx", &bob.token, blob(1, 1, 10))).await.status, 201);
    env.clock.advance(10);
    assert_eq!(env.send(put_if_absent("backups/a.json.9.hwx", &alice.token, blob(9, 9, 10))).await.status, 201);
    assert_eq!(names(&env.send(list("backups", &bob.token)).await), ["a.json.1.hwx", "a.json.2.hwx"]);
}

/// A backend in memory, so the real engine can be run without a server and the file it writes can be read.
struct Memory(std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>>);

impl holzwave_core::sync::webdav::Backend for Memory {
    fn ensure(&self, _dir: &str) -> holzwave_core::Result<()> {
        Ok(())
    }
    fn list(&self, _dir: &str) -> holzwave_core::Result<Vec<holzwave_core::sync::webdav::Entry>> {
        Ok(Vec::new())
    }
    fn get(&self, path: &str) -> holzwave_core::Result<Option<(Vec<u8>, String)>> {
        Ok(self.0.lock().expect("lock").get(path).map(|b| (b.clone(), sha256_hex(b))))
    }
    fn put(&self, path: &str, bytes: &[u8], _cond: &holzwave_core::sync::webdav::Cond) -> holzwave_core::Result<String> {
        self.0.lock().expect("lock").insert(path.to_owned(), bytes.to_vec());
        Ok(sha256_hex(bytes))
    }
}

#[test]
fn the_metadata_file_the_real_engine_writes_is_far_below_the_limit_the_server_holds_it_to() {
    use holzwave_core::secrets::MemoryStore;
    use holzwave_core::sync::engine::{ConfigSource, Syncer};

    let files = std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
    let dir = std::env::temp_dir().join(format!("hw-sync-target-meta-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory");
    // The product's own parameters (64 MiB, 3 passes), so the numbers in the file have the width they have in use.
    let source = ConfigSource::new(&dir, std::sync::Arc::new(MemoryStore::default()));
    Syncer::connect(Memory(files.clone()), source, &dir, "a long enough passphrase").expect("the engine sets a folder up");
    let meta = files.lock().expect("lock").get("meta.json").cloned().expect("the engine wrote meta.json");
    let _ = std::fs::remove_dir_all(&dir);

    let object: serde_json::Value = serde_json::from_slice(&meta).expect("meta.json is JSON");
    assert!(object.is_object());
    println!("meta.json written by the real engine: {} bytes", meta.len());
    let limit = holzwave_server::sync_target::META_MAX_BYTES;
    assert!(meta.len() * 8 < limit, "the file is {} bytes and the limit {limit} leaves room, but not unbounded room", meta.len());
    assert!(meta.len() * 20 > limit, "the limit {limit} is not more than twenty times the file of {} bytes", meta.len());
}
