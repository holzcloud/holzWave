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
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/sweep.rs"]
mod sweep;

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
    device_id: i64,
}

async fn person(env: &ApiEnv, name: &str) -> Person {
    env.app.features.set(Feature::Sync, true).expect("the switch is written");
    let who = env.member(name);
    let granted = grant_flow(env, &who, &format!("{name}'s laptop")).await;
    Person { who, token: granted.token, device_id: granted.device_id }
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

// ---------------------------------------------------------------------------------------------------------------------
// The product's own engine and client against the server, over a socket
// ---------------------------------------------------------------------------------------------------------------------

mod rig {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use holzwave_core::Result;
    use holzwave_core::secrets::MemoryStore;
    use holzwave_core::server_link::Token;
    use holzwave_core::sync::crypto::Kdf;
    use holzwave_core::sync::engine::{ConfigSource, Syncer, promote_incoming};
    use holzwave_core::sync::server::ServerBackend;
    use holzwave_core::sync::webdav::{Backend, Cond, Entry};

    /// The cheap key derivation of the tests: the parameters are stored in `meta.json`, so a device of any setting opens it.
    pub const KDF: Kdf = Kdf { m_kib: 8 * 1024, t: 1, p: 1 };

    /// One device of a person: its own folder and its own keychain stand-in.
    pub struct Dev {
        pub dir: PathBuf,
        pub secrets: Arc<MemoryStore>,
    }

    pub fn dev(tag: &str) -> Dev {
        let dir = std::env::temp_dir().join(format!("hw-sync-target-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory");
        Dev { dir, secrets: Arc::new(MemoryStore::default()) }
    }

    impl Drop for Dev {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Dev {
        /// An edit by the person, after the device started again (staged downloads are applied first, as at every start).
        pub fn write(&self, name: &str, text: &str) {
            promote_incoming(&self.dir);
            let path = self.dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("a folder");
            }
            std::fs::write(path, text).expect("a file");
        }

        /// The file as the device has it after its next start: staged downloads are moved into place first.
        pub fn text(&self, name: &str) -> Option<String> {
            promote_incoming(&self.dir);
            std::fs::read_to_string(self.dir.join(name)).ok()
        }
    }

    pub type Engine = Syncer<ServerBackend, ConfigSource>;

    /// The engine of `dev` against the server at `origin` with `token`.
    pub fn engine(origin: &str, token: &str, dev: &Dev, passphrase: &str) -> Result<Engine> {
        let backend = ServerBackend::new(origin, &Token::parse(token).expect("a token"))?;
        Syncer::connect_with(backend, ConfigSource::new(&dev.dir, dev.secrets.clone()), &dev.dir, passphrase, KDF)
    }

    /// A backend that lets something happen just before the first write of a file: another device's whole run, so that this
    /// write finds the file changed and meets the condition it carries.
    pub struct Racing {
        pub inner: ServerBackend,
        pub before_first_write: Mutex<Option<Box<dyn FnOnce() + Send>>>,
        /// The text of every error a write ended with, as the engine saw it.
        pub write_errors: Arc<Mutex<Vec<String>>>,
    }

    impl Backend for Racing {
        fn ensure(&self, dir: &str) -> Result<()> {
            self.inner.ensure(dir)
        }
        fn list(&self, dir: &str) -> Result<Vec<Entry>> {
            self.inner.list(dir)
        }
        fn get(&self, path: &str) -> Result<Option<(Vec<u8>, String)>> {
            self.inner.get(path)
        }
        fn put(&self, path: &str, bytes: &[u8], cond: &Cond) -> Result<String> {
            if path.starts_with("files/")
                && let Some(hook) = self.before_first_write.lock().expect("lock").take()
            {
                hook();
            }
            let result = self.inner.put(path, bytes, cond);
            if let Err(e) = &result {
                self.write_errors.lock().expect("lock").push(e.to_string());
            }
            result
        }
    }
}

use holzwave_core::sync::engine::Report;
use holzwave_core::sync::webdav::CONFLICT;
use rig::{Dev, dev, engine};

/// Runs a blocking call of the engine off the test's runtime, which goes on serving.
async fn blocking<T: Send + 'static>(call: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(call).await.expect("the blocking thread")
}

async fn sync_of(origin: &str, token: &str, dev: &std::sync::Arc<Dev>, passphrase: &str) -> Report {
    let (origin, token, dev, passphrase) = (origin.to_owned(), token.to_owned(), dev.clone(), passphrase.to_owned());
    blocking(move || engine(&origin, &token, &dev, &passphrase).expect("connect").sync_once().expect("a run")).await
}

fn shared(tag: &str) -> std::sync::Arc<Dev> {
    std::sync::Arc::new(dev(tag))
}

const PASSPHRASE: &str = "a long enough passphrase";

/// A person with two devices (two tokens) and a server on a loopback socket.
async fn two_devices(env: &ApiEnv, name: &str) -> (Person, String) {
    let first = person(env, name).await;
    let second = grant_flow(env, &first.who, &format!("{name}'s phone")).await.token;
    (first, second)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_device_with_its_own_token_and_the_same_passphrase_ends_with_the_files_of_the_first() {
    let env = ApiEnv::new();
    let (first, second) = two_devices(&env, "alice").await;
    let lb = device_env::serve_loopback(&env).await;
    let (a, b) = (shared("round-a"), shared("round-b"));
    a.write("settings.json", r#"{"font_size":14,"theme":"dark"}"#);
    a.write("widget_notes.json", r#"{"notes":["one","two"]}"#);
    a.write("themes/dark.json", r#"{"name":"dark"}"#);

    let up = sync_of(&lb.origin, &first.token, &a, PASSPHRASE).await;
    assert_eq!(up.uploaded.len(), 3, "{up:?}");
    assert!(up.errors.is_empty(), "{up:?}");
    let down = sync_of(&lb.origin, &second, &b, PASSPHRASE).await;
    assert_eq!(down.downloaded.len(), 3, "{down:?}");
    for name in ["settings.json", "widget_notes.json", "themes/dark.json"] {
        assert_eq!(b.text(name), a.text(name), "{name}");
        assert!(b.text(name).is_some());
    }
    // Nothing more to do on either side, and the second run writes nothing.
    let held = usage(&env, &first.who);
    assert_eq!(sync_of(&lb.origin, &second, &b, PASSPHRASE).await, Report::default());
    assert_eq!(sync_of(&lb.origin, &first.token, &a, PASSPHRASE).await, Report::default());
    assert_eq!(usage(&env, &first.who), held);
    // The server holds the metadata file and one blob per file, all of them sealed.
    assert_eq!(held.0, 4);
    assert_eq!(names(&env.send(list("files", &first.token)).await).len(), 3);
}

/// Both devices changed the same file; the second to sync wins a tie of the counters (its clock is the later), and the other
/// version is kept on the server as a backup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn when_both_devices_changed_a_file_the_newer_wins_and_the_older_is_a_backup_on_the_server() {
    let env = ApiEnv::new();
    let (first, second) = two_devices(&env, "alice").await;
    let lb = device_env::serve_loopback(&env).await;
    let (a, b) = (shared("conf-a"), shared("conf-b"));
    a.write("settings.json", "base");
    sync_of(&lb.origin, &first.token, &a, PASSPHRASE).await;
    sync_of(&lb.origin, &second, &b, PASSPHRASE).await;

    a.write("settings.json", "from a");
    b.write("settings.json", "from b");
    let up = sync_of(&lb.origin, &first.token, &a, PASSPHRASE).await;
    assert_eq!(up.uploaded, ["settings.json"]);
    let late = sync_of(&lb.origin, &second, &b, PASSPHRASE).await;
    assert_eq!(late.conflicts, ["settings.json"], "{late:?}");
    assert_eq!(late.uploaded, ["settings.json"]);

    let backups = names(&env.send(list("backups", &first.token)).await);
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert!(backups[0].starts_with("settings.json."), "{backups:?}");
    // The other device takes the winner's version.
    let follow = sync_of(&lb.origin, &first.token, &a, PASSPHRASE).await;
    assert_eq!(follow.downloaded, ["settings.json"], "{follow:?}");
    assert_eq!(a.text("settings.json").as_deref(), Some("from b"));
    assert_eq!(b.text("settings.json").as_deref(), Some("from b"));
}

/// The loser's own version goes to its conflicts folder, as with WebDAV: the version with the higher counter wins whatever the
/// clocks say.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_higher_counter_wins_and_the_losers_version_is_kept_next_to_its_files() {
    let env = ApiEnv::new();
    let (first, second) = two_devices(&env, "alice").await;
    let lb = device_env::serve_loopback(&env).await;
    let (a, b) = (shared("high-a"), shared("high-b"));
    a.write("settings.json", "v0");
    sync_of(&lb.origin, &first.token, &a, PASSPHRASE).await;
    sync_of(&lb.origin, &second, &b, PASSPHRASE).await;
    for text in ["v1", "v2"] {
        a.write("settings.json", text);
        sync_of(&lb.origin, &first.token, &a, PASSPHRASE).await;
    }
    b.write("settings.json", "b only");
    let report = sync_of(&lb.origin, &second, &b, PASSPHRASE).await;
    assert_eq!(report.conflicts, ["settings.json"], "{report:?}");
    assert_eq!(report.downloaded, ["settings.json"]);
    assert_eq!(b.text("settings.json").as_deref(), Some("v2"));
    let kept: Vec<_> = std::fs::read_dir(b.dir.join("sync-conflicts")).expect("the conflicts folder").flatten().collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(std::fs::read_to_string(kept[0].path()).expect("the kept copy"), "b only");
}

/// The write of a device that lost a race reaches the engine as the text of a lost condition, and the engine, as with WebDAV,
/// looks again and settles it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_that_loses_a_race_reaches_the_engine_as_its_conflict_text_and_is_settled() {
    let env = ApiEnv::new();
    let (first, second) = two_devices(&env, "alice").await;
    let lb = device_env::serve_loopback(&env).await;
    let (a, b) = (shared("race-a"), shared("race-b"));
    a.write("settings.json", "base");
    sync_of(&lb.origin, &first.token, &a, PASSPHRASE).await;
    sync_of(&lb.origin, &second, &b, PASSPHRASE).await;
    a.write("settings.json", "from a");
    b.write("settings.json", "from b");

    // Device b has listed and read the server and is about to write; device a's whole run comes first.
    let errors = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let hook = {
        let (origin, token, a) = (lb.origin.clone(), first.token.clone(), a.clone());
        move || {
            let uploaded = engine(&origin, &token, &a, PASSPHRASE).expect("connect").sync_once().expect("a run").uploaded;
            assert_eq!(uploaded, ["settings.json"], "the interloper's run wrote its file");
        }
    };
    let report = {
        let (origin, token, b, errors) = (lb.origin.clone(), second.clone(), b.clone(), errors.clone());
        blocking(move || {
            let backend = rig::Racing {
                inner: holzwave_core::sync::server::ServerBackend::new(
                    &origin,
                    &holzwave_core::server_link::Token::parse(&token).expect("a token"),
                )
                .expect("a backend"),
                before_first_write: std::sync::Mutex::new(Some(Box::new(hook))),
                write_errors: errors,
            };
            let source = holzwave_core::sync::engine::ConfigSource::new(&b.dir, b.secrets.clone());
            holzwave_core::sync::engine::Syncer::connect_with(backend, source, &b.dir, PASSPHRASE, rig::KDF)
                .expect("connect")
                .sync_once()
                .expect("a run")
        })
        .await
    };
    let errors = errors.lock().expect("lock").clone();
    assert!(errors.iter().any(|text| text.contains(CONFLICT)), "the lost write was a conflict for the engine: {errors:?}");
    assert!(report.errors.is_empty(), "the engine settled it: {report:?}");
    assert_eq!(report.conflicts, ["settings.json"], "{report:?}");
    assert_eq!(report.uploaded, ["settings.json"]);
    assert_eq!(names(&env.send(list("backups", &first.token)).await).len(), 1, "the interloper's version is kept");
    assert_eq!(b.text("settings.json").as_deref(), Some("from b"));
}

// ---------------------------------------------------------------------------------------------------------------------
// What the server keeps and never keeps
// ---------------------------------------------------------------------------------------------------------------------

use sweep::{Canary, Category, Counts, Sweep};

/// Whether `needle` occurs in `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What a whole use of the target left behind or let go: the canaries, the key the engine derived from the passphrase, and the
/// environment to look at.
struct Used {
    env: ApiEnv,
    canaries: Vec<Canary>,
    key: [u8; 32],
    _lb: device_env::Loopback,
}

/// A person with two devices syncs a canary setting through the server with a canary passphrase: a first upload, a second
/// device, a conflict and a backup, a wrong passphrase, and refused requests whose paths and bodies hold canaries.
async fn use_the_target() -> Used {
    let value = Canary::new("setting value", Category::Content { owner: "alice".to_owned() });
    let passphrase = Canary::new("passphrase", Category::Secret);
    let name = Canary::new("file name", Category::Content { owner: "alice".to_owned() });
    let env = ApiEnv::new();
    let (first, second) = two_devices(&env, "alice").await;
    let lb = device_env::serve_loopback(&env).await;
    let (a, b) = (shared("sweep-a"), shared("sweep-b"));
    let pass = passphrase.text().to_owned();
    a.write("settings.json", &format!(r#"{{"font_size":14,"note":"{}"}}"#, value.text()));
    sync_of(&lb.origin, &first.token, &a, &pass).await;
    sync_of(&lb.origin, &second, &b, &pass).await;
    a.write("settings.json", &format!(r#"{{"font_size":15,"note":"{} again"}}"#, value.text()));
    b.write("settings.json", &format!(r#"{{"font_size":16,"note":"{} too"}}"#, value.text()));
    sync_of(&lb.origin, &first.token, &a, &pass).await;
    let conflicted = sync_of(&lb.origin, &second, &b, &pass).await;
    assert_eq!(conflicted.conflicts, ["settings.json"], "{conflicted:?}");

    // A wrong passphrase on a third try is refused by the engine; nothing but the words of the engine is said about it.
    let wrong = {
        let (origin, token, dev) = (lb.origin.clone(), second.clone(), b.clone());
        blocking(move || engine(&origin, &token, &dev, "not the passphrase at all, no").err().map(|e| e.to_string())).await
    };
    assert!(wrong.is_some_and(|text| text.contains("passphrase")), "the wrong passphrase is refused");

    // Refused requests with canaries in the path and in the body: a name that is a file name, and a body that is a value.
    let junk = value.text().as_bytes().to_vec();
    let refused = env.send(put_if_absent(&format!("files/{}", name.text()), &first.token, junk.clone())).await;
    assert_eq!(refused.status, 400);
    let refused = env.send(put_if_absent("meta.json", &first.token, junk)).await;
    assert_eq!(refused.status, 400, "a body that is not a JSON object is refused before the condition is looked at");
    assert_eq!(env.send(get(&format!("files/{}", name.text()), &first.token)).await.status, 404);
    // A stored file under a canary name, then read: the name is a name and the body is sealed bytes.
    assert_eq!(env.send(put_if_absent(&format!("files/{}", name.text()), &first.token, blob(1, 4, 20))).await.status, 201);

    // The key the engine derived from the passphrase and the salt in the metadata file.
    let account = first.who.account;
    let meta = env.store.read(|tx| tx.blob_get(account, "meta.json")).expect("read").expect("the metadata file");
    let meta: serde_json::Value = serde_json::from_slice(&meta.body).expect("JSON");
    let salt: Vec<u8> = (0..32).step_by(2).map(|i| u8::from_str_radix(&meta["salt"].as_str().expect("salt")[i..i + 2], 16).expect("hex")).collect();
    let params = argon2::Params::new(
        u32::try_from(meta["m_kib"].as_u64().expect("m")).expect("fits"),
        u32::try_from(meta["t"].as_u64().expect("t")).expect("fits"),
        u32::try_from(meta["p"].as_u64().expect("p")).expect("fits"),
        Some(32),
    )
    .expect("parameters");
    let mut key = [0u8; 32];
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(pass.as_bytes(), &salt, &mut key)
        .expect("the key derives");

    let tokens = [Canary::with_text("device token a", Category::Secret, &first.token), Canary::with_text("device token b", Category::Secret, &second)];
    let mut canaries = vec![value, passphrase, name, Canary::with_text("derived key", Category::Secret, &hex(&key))];
    canaries.extend(tokens);
    Used { env, canaries, key, _lb: lb }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_file_the_server_writes_holds_a_setting_value_the_passphrase_or_the_key_and_the_blobs_are_sealed() {
    let used = use_the_target().await;
    let sweep = Sweep::new(&used.canaries);
    let root = used.env.store.path().parent().expect("the directory of the database").to_path_buf();
    let mut counts = Counts::default();
    // The database, its log and shared-memory file, and every audit file are below the scratch directory of the server.
    let findings = sweep.scan_tree(&root, &mut counts);
    assert!(findings.iter().all(|f| f.canary == "file name" && f.surface.contains("sqlite")), "{:?}", findings.iter().map(sweep::Finding::line).collect::<Vec<_>>());
    // A file name is stored as the path of a row, as it must be to be found again; no other canary is anywhere.
    assert!(!findings.is_empty(), "the sweep sees the one name that is stored on purpose, so it is not blind");
    // The key itself, as raw bytes, is nowhere.
    let mut raw_key_found = 0;
    for file in sweep::files_below(&root) {
        let bytes = std::fs::read(&file).expect("a file of the server");
        raw_key_found += usize::from(contains(&bytes, &used.key));
    }
    assert_eq!(raw_key_found, 0, "the raw key is in no file of the server");
    assert!(counts.files >= 3 && counts.bytes > 0, "the sweep read the database and the audit log: {counts:?}");

    // Every blob the server holds begins with the format's magic, in the files and in the backups.
    let account = used.env.store.account_by_username("alice").expect("read").expect("alice").id;
    let rows = used.env.store.read(|tx| {
        let mut all = Vec::new();
        for folder in ["files/", "backups/"] {
            for info in tx.blob_list(account, folder)? {
                all.push((format!("{folder}{}", info.name), tx.blob_get(account, &format!("{folder}{}", info.name))?.expect("listed").body));
            }
        }
        Ok(all)
    })
    .expect("the blobs");
    assert!(rows.len() >= 3, "files and a backup: {}", rows.len());
    for (path, body) in &rows {
        assert_eq!(&body[..3], holzwave_core::sync::crypto::MAGIC, "{path} begins with the magic");
        assert!(!contains(body, used.canaries[0].text().as_bytes()), "{path} holds no setting value");
    }
    assert!(rows.iter().any(|(path, _)| path.starts_with("backups/")), "the conflict left a backup");

    // The Debug text of the server's types that hold a request or a stored file prints no name and no byte.
    let named = format!("files/{}", used.canaries[2].text());
    let path = holzwave_server::sync_target::BlobPath::parse(&named).expect("a path");
    let blob_row = used.env.store.read(|tx| tx.blob_get(account, &named)).expect("read").expect("the file");
    for (what, text) in [("a path", format!("{path:?}")), ("a stored file", format!("{blob_row:?}")), ("the bounds", format!("{:?}", Bounds::DEFAULT))] {
        assert!(sweep.scan_text(what, &text).is_empty(), "{what}: {text}");
    }
}

#[test]
fn child_sync_run() {
    // Run only by the test below, in a process of its own whose output is captured.
    let Some(sink) = std::env::var_os("HW_SYNC_SINK") else { return };
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("a runtime");
    let used = runtime.block_on(use_the_target());
    let lines: Vec<String> = used.canaries.iter().map(|c| format!("{}\t{}", c.name, c.text())).collect();
    std::fs::write(sink, lines.join("\n")).expect("write the canaries for the parent");
    println!("child sync run finished");
}

#[test]
fn log_capture_standard_output_and_error_of_a_whole_use_of_the_target_hold_no_secret_and_no_value() {
    let dir = auth_env::scratch::Scratch::new("sync-child");
    let sink = dir.path().join("canaries.txt");
    let exe = std::env::current_exe().expect("the test binary");
    let output = std::process::Command::new(exe)
        .args(["--exact", "child_sync_run", "--nocapture", "--test-threads=1"])
        .env("HW_SYNC_SINK", &sink)
        .env("RUST_BACKTRACE", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the child process");
    let captured = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "the child failed: {}", captured.chars().take(2000).collect::<String>());
    assert!(captured.contains("child sync run finished"), "the child ran the whole use");
    let text = std::fs::read_to_string(&sink).expect("the canaries of the child");
    let owned: Vec<Canary> = text
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(name, text)| Canary::with_text(name, Category::Secret, text))
        .collect();
    assert!(owned.len() >= 6, "{} canaries", owned.len());
    let sweep = Sweep::new(&owned);
    let lines: Vec<String> = captured.lines().map(str::to_owned).collect();
    let findings = sweep.scan_lines("the captured log", &lines, &mut Counts::default());
    assert!(findings.is_empty(), "{:?}", findings.iter().map(sweep::Finding::line).collect::<Vec<_>>());
    // The capture is not blind: a canary written to the stream is found.
    let planted = vec![format!("warn x path={}", owned[0].text())];
    assert!(!sweep.scan_lines("planted", &planted, &mut Counts::default()).is_empty());
    println!("log capture: {} bytes of standard output and error, {} canaries, 0 hits", captured.len(), owned.len());
}

// ---------------------------------------------------------------------------------------------------------------------
// One person from another, and the end of a person or a token
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_person_can_not_read_list_or_overwrite_another_persons_files_and_a_wrong_passphrase_writes_nothing() {
    let env = ApiEnv::new();
    let (alice, alice_phone) = two_devices(&env, "alice").await;
    let bob = person(&env, "bob").await;
    let lb = device_env::serve_loopback(&env).await;
    let a = shared("iso-a");
    a.write("settings.json", "alice's settings");
    sync_of(&lb.origin, &alice.token, &a, PASSPHRASE).await;
    let alice_files = env.send(list("files", &alice.token)).await.json();
    let etag = alice_files[0]["etag"].as_str().expect("an etag").to_owned();
    let name = format!("files/{}", alice_files[0]["name"].as_str().expect("a name"));
    let before = usage(&env, &alice.who);

    // Bob's token finds an empty space under the same names.
    for dir in ["files", "backups", "-"] {
        assert!(names(&env.send(list(dir, &bob.token)).await).is_empty(), "bob lists {dir}");
    }
    assert_eq!(env.send(get(&name, &bob.token)).await.status, 404);
    assert_eq!(env.send(get("meta.json", &bob.token)).await.status, 404);
    // He can not replace hers by naming her tag, nor by overwriting with no condition (his own file appears, hers does not move).
    let swap = env.send(put_if_match(&name, &bob.token, &etag, blob(9, 9, 10))).await;
    assert_eq!(answer(&swap), (412, "precondition_failed".to_owned()));
    assert_eq!(usage(&env, &bob.who), (0, 0));
    assert_eq!(env.send(put(&name, &bob.token, blob(9, 8, 10))).await.status, 201);
    assert_eq!(env.send(put_if_absent("meta.json", &bob.token, br#"{"bob":true}"#.to_vec())).await.status, 201);
    let after = env.send(list("files", &alice.token)).await.json();
    assert_eq!(after, alice_files, "alice's files did not move");
    assert_eq!(usage(&env, &alice.who), before);
    assert!(!contains(&env.send(get("meta.json", &alice.token)).await.body, b"bob"), "bob's metadata file is not alice's");

    // A second device of alice's, with the wrong passphrase, is refused by the engine and writes nothing.
    let phone = shared("iso-phone");
    let wrong = {
        let (origin, token, phone) = (lb.origin.clone(), alice_phone.clone(), phone.clone());
        blocking(move || engine(&origin, &token, &phone, "a different long passphrase").err().map(|e| e.to_string())).await
    };
    assert!(wrong.is_some_and(|text| text.contains("passphrase is wrong")), "the engine refuses the passphrase");
    assert_eq!(usage(&env, &alice.who), before, "nothing was written");
    assert_eq!(env.send(list("files", &alice.token)).await.json(), alice_files);
    // The first device still reads everything and has nothing to do.
    assert_eq!(sync_of(&lb.origin, &alice.token, &a, PASSPHRASE).await, Report::default());
    // The right passphrase on the second device works.
    let down = sync_of(&lb.origin, &alice_phone, &phone, PASSPHRASE).await;
    assert_eq!(down.downloaded, ["settings.json"], "{down:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_person_loses_their_token_and_their_files_and_nobody_elses_are_touched_and_a_revoked_token_is_a_sign_in_fault() {
    let env = ApiEnv::new();
    let alice = person(&env, "alice").await;
    let bob = person(&env, "bob").await;
    let carol = person(&env, "carol").await;
    let lb = device_env::serve_loopback(&env).await;
    let (a, b, c) = (shared("del-a"), shared("del-b"), shared("del-c"));
    a.write("settings.json", "alice");
    b.write("settings.json", "bob");
    c.write("settings.json", "carol");
    for (token, dev) in [(&alice.token, &a), (&bob.token, &b), (&carol.token, &c)] {
        sync_of(&lb.origin, token, dev, PASSPHRASE).await;
    }
    let (alice_id, bob_id) = (alice.who.account.get(), bob.who.account.get());
    let rows = |id: i64| env.count_rows(&format!("SELECT COUNT(*) FROM sync_blobs WHERE account_id = {id}"));
    assert_eq!((rows(alice_id), rows(bob_id)), (2, 2));
    let bobs_files = env.send(list("files", &bob.token)).await.json();

    // The administrator deletes alice: her token ends and her files go with her.
    let admin = env.admin_fresh("root");
    let gone = env.send(env.delete_as(&format!("/api/admin/accounts/{alice_id}"), &admin)).await;
    assert_eq!(gone.status, 204, "{}", gone.text());
    for request in [list("files", &alice.token), get("meta.json", &alice.token), put_if_absent("files/x.hwx", &alice.token, blob(1, 1, 10))] {
        assert_eq!(answer(&env.send(request).await), (401, "invalid".to_owned()));
    }
    assert_eq!(rows(alice_id), 0, "alice's files are gone");
    assert_eq!(rows(bob_id), 2, "bob's are not touched");
    assert_eq!(env.send(list("files", &bob.token)).await.json(), bobs_files);

    // A token that was revoked: the engine's next request is refused as a sign-in, in the one fixed sentence, and nothing of the
    // person's files is touched.
    let revoked = env.send(env.delete_as(&format!("{}/{}", device_env::MY_DEVICES, bob.device_id), &bob.who)).await;
    assert_eq!(revoked.status, 204, "{}", revoked.text());
    let (origin, token, dev) = (lb.origin.clone(), bob.token.clone(), b.clone());
    let fault = blocking(move || engine(&origin, &token, &dev, PASSPHRASE).err().map(|e| e.to_string())).await;
    assert_eq!(fault.as_deref(), Some(holzwave_core::server_link::Fault::SignedOut.sentence()));
    assert_eq!(rows(bob_id), 2, "a revoked token does not delete files");
    // Carol, with another token of another person, is not affected by either.
    assert_eq!(sync_of(&lb.origin, &carol.token, &c, PASSPHRASE).await, Report::default());
}

// ---------------------------------------------------------------------------------------------------------------------
// The client's own rules about the address
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_client_works_over_plain_http_on_loopback_and_refuses_a_self_made_certificate_where_the_server_speaks_tls() {
    // On the loopback address the product's client takes plain http: this is the whole of every test above.
    let env = ApiEnv::new();
    let alice = person(&env, "alice").await;
    let lb = device_env::serve_loopback(&env).await;
    let a = shared("tls-a");
    a.write("settings.json", "x");
    assert_eq!(sync_of(&lb.origin, &alice.token, &a, PASSPHRASE).await.uploaded, ["settings.json"]);
    assert!(lb.requests() > 0);

    // A listener that speaks TLS with a certificate made for the test: the platform's verifier does not trust it, so the client
    // ends the connection before it says a word, and the token is never sent. The listener itself is fine: a client that is
    // given its certificate connects.
    let server = edge_tls::start().await;
    assert!(server.try_tls(&[b"http/1.1"]).await.is_ok(), "the listener answers a client that trusts its certificate");
    let origin = format!("https://127.0.0.1:{}", server.addr.port());
    let token = alice.token.clone();
    let fault = blocking(move || {
        let backend = holzwave_core::sync::server::ServerBackend::new(&origin, &holzwave_core::server_link::Token::parse(&token).expect("a token"))
            .expect("a backend");
        holzwave_core::sync::webdav::Backend::ensure(&backend, "files").err().map(|e| e.to_string())
    })
    .await;
    assert_eq!(fault.as_deref(), Some(holzwave_core::server_link::Fault::Unreachable.sentence()));
}
