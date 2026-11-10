//! The mount points of the device features through the real router: the gate that makes a feature that is off look like a path
//! that does not exist, the bearer token that only a device route accepts, the switches an administrator turns and the
//! operator's ceiling over them.
//!
//! The router is called as a `tower` service (no socket) over the real services of `support/api_env.rs`.

#![cfg(feature = "in-process-workers")]

#[path = "support/api_env.rs"]
mod api_env;
#[path = "support/auth_env.rs"]
mod auth_env;
#[path = "support/edge.rs"]
mod edge;

use api_env::ApiEnv;
use auth_env::{Methods, Tuning};
use axum::http::Method;
use edge::{HOST, ORIGIN, Req, Resp};
use holzwave_server::clock::{Clock as _, from_unix, rfc3339};
use holzwave_server::device::{DEVICE_IDLE_DEFAULT, DeviceToken, TOUCH_INTERVAL};
use holzwave_server::http::features::{Ceiling, DeviceSettings, Feature};
use holzwave_server::store::{AccountId, AccountState, Device, NewDevice, Role};
use serde_json::json;

const ME: &str = "/api/device/me";
const UNKNOWN: &str = "/api/device/no-such-route";

fn env() -> ApiEnv {
    ApiEnv::new()
}

/// An environment whose configuration holds a ceiling.
fn env_with_ceiling(ceiling: Ceiling) -> ApiEnv {
    let tuning = Tuning { devices: DeviceSettings { ceiling, ..DeviceSettings::default() }, ..Tuning::default() };
    ApiEnv::build(Methods { passkey: true, password: true, oidc: false }, tuning)
}

/// A device of a new member, made through the store the way the grant routes make one.
fn mint(env: &ApiEnv, name: &str) -> (DeviceToken, Device) {
    env.bare(name, Role::Member);
    let account = env.store.account_by_username(name).expect("read").expect("the account exists").id;
    mint_for(env, account, "Laptop")
}

fn mint_for(env: &ApiEnv, account: AccountId, device_name: &str) -> (DeviceToken, Device) {
    let token = DeviceToken::generate().expect("a token");
    let now = env.clock.unix();
    let device = env
        .store
        .write(|tx| {
            tx.device_create(&NewDevice {
                token_hash: token.hash(),
                account_id: account,
                name: device_name,
                platform: "linux",
                now,
                expires_at: now + DEVICE_IDLE_DEFAULT,
                addr: "192.0.2.9".parse().expect("an address"),
            })
        })
        .expect("a device");
    (token, device)
}

/// A request of a native client: the public host and nothing a browser would add.
fn native(path: &str) -> Req {
    Req::new(Method::GET, path).header("host", HOST)
}

fn bearer(path: &str, token: &DeviceToken) -> Req {
    native(path).header("authorization", &format!("Bearer {}", token.expose()))
}

fn turn_on(env: &ApiEnv, feature: Feature) {
    assert!(env.app.features.set(feature, true).expect("the switch is written"));
}

/// Everything of an answer that must not tell two paths apart, without the date.
fn shape(resp: &Resp) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut headers: Vec<(String, String)> = resp
        .headers
        .iter()
        .filter(|(name, _)| name.as_str() != "date")
        .map(|(name, value)| (name.as_str().to_owned(), String::from_utf8_lossy(value.as_bytes()).into_owned()))
        .collect();
    headers.sort();
    (resp.status.as_u16(), headers, resp.body.clone())
}

fn code(resp: &Resp) -> String {
    resp.json()["code"].as_str().unwrap_or_default().to_owned()
}

// ---------------------------------------------------------------------------------------------------------------------
// The gate: off is the same as absent
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn tracer_a_device_route_of_a_feature_that_is_off_answers_exactly_as_a_path_that_is_not_there() {
    let env = env();
    let (token, _) = mint(&env, "alice");
    let member = env.signed_aged("alice", 0);
    let (other, _) = mint(&env, "bob");
    let requests: Vec<(&str, Box<dyn Fn(&str) -> Req>)> = vec![
        ("no header", Box::new(native)),
        ("a valid token", Box::new(|p| bearer(p, &token))),
        ("an invalid token", Box::new(|p| native(p).header("authorization", "Bearer hwd_not-a-token"))),
        ("a token of another device", Box::new(|p| bearer(p, &other))),
        ("a session cookie", Box::new(|p| native(p).signed(&member))),
    ];
    for (what, make) in &requests {
        let gated = env.send(make(ME)).await;
        let absent = env.send(make(UNKNOWN)).await;
        assert_eq!(gated.status, 404, "{what}");
        assert_eq!(code(&gated), "not_found", "{what}");
        assert_eq!(shape(&gated), shape(&absent), "{what}: a switch that is off answers as a path that is not there");
        assert!(gated.all("set-cookie").is_empty(), "{what}");
    }
}

#[tokio::test]
async fn a_changing_method_on_a_gated_path_is_answered_as_an_absent_path_too() {
    let env = env();
    for method in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
        let build = |path: &str| {
            Req::new(method.clone(), path)
                .header("host", HOST)
                .header("origin", ORIGIN)
                .header("content-type", "application/json")
                .bytes(b"{}".to_vec())
        };
        let gated = env.send(build(ME)).await;
        let absent = env.send(build(UNKNOWN)).await;
        assert_eq!(gated.status, 404, "{method}");
        assert_eq!(shape(&gated), shape(&absent), "{method}");
    }
}

#[tokio::test]
async fn a_spelling_that_could_reach_a_gated_path_is_gated_and_reaches_no_handler() {
    let env = env();
    let (token, _) = mint(&env, "alice");
    let variants = ["/api/device/me/", "/API/device/me", "/api/Device/me", "//api/device/me", "/api//device/me", "/api/./device/me"];
    for on in [false, true] {
        if on {
            turn_on(&env, Feature::Remote);
        }
        for path in variants {
            let resp = env.send(bearer(path, &token)).await;
            assert_eq!(resp.status, 404, "{path} (switch on: {on})");
            assert_eq!(code(&resp), "not_found", "{path} (switch on: {on})");
            let absent = env.send(bearer("/api/device/no-such-route", &token)).await;
            assert_eq!(shape(&resp).0, shape(&absent).0, "{path}");
        }
    }
    // With the switch on the plain spelling works, so the variants were refused for their spelling, not by the switch.
    assert_eq!(env.send(bearer(ME, &token)).await.status, 200);
}

#[tokio::test]
async fn each_gate_follows_its_own_feature() {
    let env = env();
    let features = &env.app.features;
    assert!(features.is_closed("/api/device/me") && features.is_closed("/sync/v1/list/-") && features.is_closed("/ws/device"));
    turn_on(&env, Feature::Sync);
    assert!(!features.is_closed("/api/device/me"), "either switch opens the grant routes");
    assert!(!features.is_closed("/sync/v1/list/-"));
    assert!(features.is_closed("/ws/device"), "the socket needs the remote switch");
    assert!(!features.is_closed("/api/devices"), "a path that only starts with the same letters is not under the gate");
    assert!(!features.is_closed("/ws"), "the existing socket is not a device route");
    assert!(features.set(Feature::Sync, false).expect("write"));
    turn_on(&env, Feature::Remote);
    assert!(features.is_closed("/sync/v1/list/-"), "the sync routes need the sync switch");
    assert!(!features.is_closed("/ws/device"));
}

// ---------------------------------------------------------------------------------------------------------------------
// The bearer: the one credential of a device route
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_valid_token_gets_the_device_and_both_switch_values() {
    let env = env();
    let (token, device) = mint(&env, "alice");
    turn_on(&env, Feature::Remote);
    let resp = env.send(bearer(ME, &token)).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.header("cache-control").as_deref(), Some("no-store"));
    assert!(resp.all("set-cookie").is_empty());
    let body = resp.json();
    assert_eq!(body["device"]["id"], device.id);
    assert_eq!(body["device"]["name"], "Laptop");
    assert_eq!(body["device"]["platform"], "linux");
    assert_eq!(body["device"]["expires_at"], rfc3339(from_unix(device.expires_at)));
    assert_eq!(body["switches"], json!({"sync": false, "remote": true}));
    let text = resp.text();
    assert!(!text.contains(token.expose()) && !text.contains("alice"), "the answer names neither the token nor the account: {text}");
    assert_eq!(body.as_object().expect("an object").len(), 2);
}

#[tokio::test]
async fn a_request_without_a_good_bearer_is_refused_without_a_cookie() {
    let env = env();
    let (token, device) = mint(&env, "alice");
    turn_on(&env, Feature::Remote);
    let text = token.expose().to_owned();
    // A token of a disabled account and a revoked one.
    let (disabled_token, _) = mint(&env, "carol");
    let (revoked_token, revoked) = mint(&env, "dave");
    let carol = env.store.account_by_username("carol").expect("read").expect("carol").id;
    env.store.write(|tx| tx.account_set_state(carol, AccountState::Disabled)).expect("disable");
    let dave = env.store.account_by_username("dave").expect("read").expect("dave").id;
    assert!(env.store.write(|tx| tx.device_revoke(dave, revoked.id)).expect("revoke"));

    let strange = format!("Bearer  {text}");
    let cases: Vec<(&str, Req)> = vec![
        ("no header", native(ME)),
        ("an empty header", native(ME).header("authorization", "")),
        ("another scheme", native(ME).header("authorization", &format!("Basic {text}"))),
        ("the scheme alone", native(ME).header("authorization", "Bearer")),
        ("two spaces", native(ME).header("authorization", &strange)),
        ("a trailing space", native(ME).header("authorization", &format!("Bearer {text} "))),
        ("a token that is not one", native(ME).header("authorization", "Bearer abc")),
        ("a token without the prefix", native(ME).header("authorization", &format!("Bearer {}", &text[4..]))),
        ("an unknown token", native(ME).header("authorization", &format!("Bearer {}", DeviceToken::generate().expect("token").expose()))),
        ("two headers", bearer(ME, &token).header("authorization", &format!("Bearer {text}"))),
        ("the token in a cookie", native(ME).header("cookie", &format!("__Host-hw_session={text}"))),
        ("the token as another cookie", native(ME).header("cookie", &format!("token={text}"))),
        ("the token in the query", native(&format!("{ME}?token={text}"))),
        ("the token of a disabled account", bearer(ME, &disabled_token)),
        ("a revoked token", bearer(ME, &revoked_token)),
    ];
    for (what, request) in cases {
        let resp = env.send(request).await;
        assert_eq!((resp.status.as_u16(), code(&resp).as_str()), (401, "invalid"), "{what}: {}", resp.text());
        assert!(resp.all("set-cookie").is_empty(), "{what}");
    }
    // An expired token: the clock moves past the end of the token.
    let ends = device.expires_at;
    env.clock.set(ends + 1);
    let resp = env.send(bearer(ME, &token)).await;
    assert_eq!((resp.status.as_u16(), code(&resp).as_str()), (401, "invalid"), "an expired token");
    assert!(resp.all("set-cookie").is_empty());
}

#[tokio::test]
async fn a_session_alone_does_not_authenticate_a_device_route() {
    let env = env();
    mint(&env, "alice");
    turn_on(&env, Feature::Remote);
    let member = env.signed_aged("alice", 0);
    let resp = env.send(native(ME).signed(&member)).await;
    assert_eq!((resp.status.as_u16(), code(&resp).as_str()), (401, "invalid"));
    assert!(resp.all("set-cookie").is_empty(), "the cookie was not even looked at, so it was not deleted either");
    // A stale cookie is not looked at either: no deletion header.
    let resp = env.send(native(ME).header("cookie", "__Host-hw_session=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")).await;
    assert_eq!(resp.status, 401);
    assert!(resp.all("set-cookie").is_empty());
}

#[tokio::test]
async fn a_bearer_token_never_authenticates_a_session_route() {
    let env = env();
    let (token, _) = mint(&env, "alice");
    turn_on(&env, Feature::Remote);
    assert_eq!(env.send(bearer(ME, &token)).await.status, 200, "the token is good on its own route");
    for path in ["/api/me", "/api/admin/accounts"] {
        let resp = env.send(bearer(path, &token)).await;
        assert_eq!((resp.status.as_u16(), code(&resp).as_str()), (401, "invalid"), "{path}");
    }
    let change = Req::new(Method::POST, "/api/me/totp")
        .header("host", HOST)
        .header("origin", ORIGIN)
        .header("content-type", "application/json")
        .header("authorization", &format!("Bearer {}", token.expose()))
        .bytes(b"{}".to_vec());
    assert_eq!(env.send(change).await.status, 401, "a change with a bearer and no session");
}

#[tokio::test]
async fn a_device_route_needs_no_origin_and_no_anti_forgery_token() {
    let env = env();
    let (token, _) = mint(&env, "alice");
    turn_on(&env, Feature::Remote);
    // A native client sends neither; a cross-site Origin does not matter either, because nothing ambient carries the token.
    let resp = env.send(bearer(ME, &token).header("origin", "https://evil.example.test")).await;
    assert_eq!(resp.status, 200);
}

#[tokio::test]
async fn the_last_seen_time_is_written_at_most_once_a_minute() {
    let env = env();
    let (token, device) = mint(&env, "alice");
    turn_on(&env, Feature::Remote);
    let seen = |env: &ApiEnv| -> (i64, i64) {
        let account = device.account_id;
        let list = env.store.read(|tx| tx.devices_of(account, env.clock.unix())).expect("list");
        let row = list.iter().find(|d| d.id == device.id).expect("the device");
        (row.last_seen, row.expires_at)
    };
    let start = env.clock.unix();
    assert_eq!(seen(&env).0, start, "made now");
    for step in 0..(TOUCH_INTERVAL - 1) {
        env.clock.set(start + step + 1);
        assert_eq!(env.send(bearer(ME, &token)).await.status, 200);
        assert_eq!(seen(&env).0, start, "inside the minute nothing is written (second {})", step + 1);
    }
    env.clock.set(start + TOUCH_INTERVAL);
    assert_eq!(env.send(bearer(ME, &token)).await.status, 200);
    assert_eq!(seen(&env), (start + TOUCH_INTERVAL, start + TOUCH_INTERVAL + DEVICE_IDLE_DEFAULT), "a minute later it is written and the end moves");
    env.clock.set(start + TOUCH_INTERVAL + 10);
    assert_eq!(env.send(bearer(ME, &token)).await.status, 200);
    assert_eq!(seen(&env).0, start + TOUCH_INTERVAL, "and again the next minute only");
}

#[tokio::test]
async fn the_token_works_again_after_the_switch_was_off_and_on() {
    let env = env();
    let (token, _) = mint(&env, "alice");
    turn_on(&env, Feature::Remote);
    assert_eq!(env.send(bearer(ME, &token)).await.status, 200);
    assert!(env.app.features.set(Feature::Remote, false).expect("off"));
    assert_eq!(env.send(bearer(ME, &token)).await.status, 404);
    turn_on(&env, Feature::Remote);
    assert_eq!(env.send(bearer(ME, &token)).await.status, 200, "a token that was valid stays valid");
}

#[tokio::test]
async fn an_operator_ceiling_keeps_a_switch_off_even_when_it_is_stored_as_on() {
    let env = env_with_ceiling(Ceiling { sync: true, remote: false });
    let (token, _) = mint(&env, "alice");
    // The store says on (as it could after the configuration was tightened); the ceiling wins.
    turn_on(&env, Feature::Remote);
    assert!(env.app.features.stored(Feature::Remote));
    assert!(!env.app.features.is_on(Feature::Remote));
    assert_eq!(env.send(bearer(ME, &token)).await.status, 404);
    turn_on(&env, Feature::Sync);
    assert_eq!(env.send(bearer(ME, &token)).await.status, 200, "the sync switch is allowed and opens the route");
    assert_eq!(env.send(bearer(ME, &token)).await.json()["switches"], json!({"sync": true, "remote": false}));
}
