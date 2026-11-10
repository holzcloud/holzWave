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
use edge::{HOST, ORIGIN, Req, Resp, Signed};
use holzwave_server::audit::Action;
use holzwave_server::clock::{Clock as _, from_unix, rfc3339};
use holzwave_server::config::{self, Env, Probe, Refusal, prepare};
use holzwave_server::keyfile::ServerKey;
use std::path::Path;
use holzwave_server::device::{DEVICE_IDLE_DEFAULT, DeviceToken, TOUCH_INTERVAL};
use holzwave_server::http::features::{Ceiling, DeviceSettings, Feature};
use holzwave_server::store::{AccountId, AccountState, Device, NewDevice, Role};
use serde_json::{Value, json};

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
    type Build<'a> = Box<dyn Fn(&str) -> Req + 'a>;
    let requests: Vec<(&str, Build)> = vec![
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
    for path in ["/api/me", "/api/admin/accounts", "/api/admin/device-access"] {
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

// ---------------------------------------------------------------------------------------------------------------------
// The configuration: the ceiling and the idle time
// ---------------------------------------------------------------------------------------------------------------------

const CONFIG_PATH: &str = "/etc/test/server.json";

/// Serves one configuration text and says that the certificate files exist.
struct FakeProbe(String);

impl Probe for FakeProbe {
    fn read_config(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        if path == Path::new(CONFIG_PATH) { Ok(self.0.clone().into_bytes()) } else { Err(std::io::ErrorKind::NotFound.into()) }
    }

    fn readable(&self, path: &Path) -> bool {
        [Path::new("/certs/cert.pem"), Path::new("/certs/key.pem")].contains(&path)
    }

    fn own_uid(&self) -> u32 {
        10_001
    }

    fn load_key(&self, _path: &Path) -> Result<ServerKey, Refusal> {
        Ok(ServerKey::from_bytes([4; 32]).expect("a key"))
    }
}

fn base() -> Value {
    json!({
        "public_url": "https://hw.example.test",
        "tls": {"mode": "files", "cert_file": "/certs/cert.pem", "key_file": "/certs/key.pem"},
        "auth": {"passkey": true}
    })
}

fn judge(value: &Value) -> Result<config::Prepared, Refusal> {
    let mut env = Env::new();
    env.insert("HOLZWAVE_SERVER_KEY_FILE".to_owned(), "/etc/holzwave/key/server.key".to_owned());
    env.insert("HOLZWAVE_SERVER_CONFIG".to_owned(), CONFIG_PATH.to_owned());
    prepare(&env, &FakeProbe(value.to_string()))
}

#[test]
fn a_configuration_without_the_two_sections_leaves_the_ceiling_open_and_the_idle_time_at_90_days() {
    let p = judge(&base()).expect("the minimal file");
    assert_eq!(p.device_access.ceiling(), Ceiling::OPEN);
    assert_eq!((p.device_access.sync, p.device_access.remote), (None, None));
    assert_eq!(p.devices.idle, std::time::Duration::from_secs(90 * 86_400));
    assert_eq!(DeviceSettings::default().idle, p.devices.idle, "the application default is the configuration default");
}

#[test]
fn the_ceiling_closes_only_the_feature_whose_key_says_false() {
    for (sync, remote) in [(Some(false), None), (None, Some(false)), (Some(false), Some(false)), (Some(true), Some(true)), (Some(true), None)] {
        let mut file = base();
        let mut section = serde_json::Map::new();
        if let Some(sync) = sync {
            section.insert("sync".to_owned(), json!(sync));
        }
        if let Some(remote) = remote {
            section.insert("remote".to_owned(), json!(remote));
        }
        file["device_access"] = Value::Object(section);
        let ceiling = judge(&file).expect("a valid ceiling").device_access.ceiling();
        assert_eq!((ceiling.sync, ceiling.remote), (sync.unwrap_or(true), remote.unwrap_or(true)), "{file}");
    }
}

#[test]
fn a_wrong_type_an_unknown_key_and_an_idle_time_outside_its_bounds_are_refused() {
    for (what, section, value) in [
        ("a text for sync", "device_access", json!({"sync": "yes"})),
        ("a number for remote", "device_access", json!({"remote": 1})),
        ("an unknown key", "device_access", json!({"all": true})),
        ("a number for the idle time", "devices", json!({"idle": 90})),
        ("an unknown key of devices", "devices", json!({"idle": "90d", "max": 5})),
        ("a bad unit", "devices", json!({"idle": "90 days"})),
    ] {
        let mut file = base();
        file[section] = value;
        let error = judge(&file).expect_err(what);
        assert!(error.sentence().contains("is not valid"), "{what}: {error}");
    }
    for (idle, ok) in [("1d", true), ("90d", true), ("365d", true), ("23h", false), ("0s", false), ("366d", false), ("3650d", false)] {
        let mut file = base();
        file["devices"] = json!({"idle": idle});
        match (judge(&file), ok) {
            (Ok(p), true) => assert_eq!(p.devices.idle, config::parse_duration(idle).expect("a duration")),
            (Err(error), false) => {
                assert!(error.sentence().contains(&format!("devices.idle is {idle} but must be between 1d and 365d")), "{error}");
            }
            (other, _) => panic!("{idle}: {:?}", other.map(|p| p.devices.idle)),
        }
    }
}

#[tokio::test]
async fn the_configured_idle_time_is_what_a_device_token_is_renewed_to() {
    let tuning = Tuning {
        devices: DeviceSettings { idle: std::time::Duration::from_secs(5 * 86_400), ..DeviceSettings::default() },
        ..Tuning::default()
    };
    let env = ApiEnv::build(Methods { passkey: true, password: true, oidc: false }, tuning);
    let (token, device) = mint(&env, "alice");
    turn_on(&env, Feature::Remote);
    env.clock.advance(TOUCH_INTERVAL);
    assert_eq!(env.send(bearer(ME, &token)).await.status, 200);
    let row = env.store.read(|tx| tx.devices_of(device.account_id, env.clock.unix())).expect("list").remove(0);
    assert_eq!(row.expires_at, env.clock.unix() + 5 * 86_400);
}

// ---------------------------------------------------------------------------------------------------------------------
// The administrator's switches
// ---------------------------------------------------------------------------------------------------------------------

const ACCESS: &str = "/api/admin/device-access";

fn state(sync: (bool, bool), remote: (bool, bool)) -> Value {
    json!({"sync": {"on": sync.0, "allowed": sync.1}, "remote": {"on": remote.0, "allowed": remote.1}})
}

fn change_lines(env: &ApiEnv) -> Vec<Value> {
    env.audit_lines().into_iter().filter(|l| l["action"] == "device_access.change").collect()
}

async fn post_access(env: &ApiEnv, who: &Signed, body: &Value) -> Resp {
    env.send(env.post_json(ACCESS, body, who)).await
}

#[tokio::test]
async fn an_administrator_reads_each_switch_and_whether_the_configuration_allows_it() {
    let env = env_with_ceiling(Ceiling { sync: true, remote: false });
    let admin = env.admin_fresh("root");
    let resp = env.send(env.get_as(ACCESS, &admin)).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json(), state((false, true), (false, false)));
    assert_eq!(resp.header("cache-control").as_deref(), Some("no-store"));
}

#[tokio::test]
async fn a_member_and_an_anonymous_caller_are_refused_as_on_any_administrator_route() {
    let env = env();
    let member = env.member("alice");
    for (what, resp) in [
        ("a member reads", env.send(env.get_as(ACCESS, &member)).await),
        ("a member changes", post_access(&env, &member, &json!({"sync": true})).await),
        ("an anonymous caller reads", env.send(Req::new(Method::GET, ACCESS).header("host", HOST)).await),
        ("an anonymous caller changes", env.send(env.post(ACCESS, &json!({"sync": true}))).await),
    ] {
        let want = if what.starts_with("a member") { (403, "forbidden") } else { (401, "invalid") };
        assert_eq!((resp.status.as_u16(), code(&resp).as_str()), want, "{what}");
    }
    assert_eq!(env.app.features.effective(), Default::default(), "nothing changed");
    assert!(change_lines(&env).is_empty());
}

#[tokio::test]
async fn an_administrator_with_a_recent_sign_in_turns_a_switch_on_and_off_and_gets_the_new_state() {
    let env = env();
    let admin = env.admin_fresh("root");
    let resp = post_access(&env, &admin, &json!({"sync": true})).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json(), state((true, true), (false, true)));
    assert!(env.app.features.is_on(Feature::Sync) && !env.app.features.is_on(Feature::Remote));
    // The state is stored: a second application over the same store would read it.
    assert!(env.store.read(|tx| tx.setting_get("sync")).expect("read"));
    let resp = post_access(&env, &admin, &json!({"sync": false, "remote": true})).await;
    assert_eq!(resp.json(), state((false, true), (true, true)), "both members in one body");
    assert_eq!(env.send(env.get_as(ACCESS, &admin)).await.json(), state((false, true), (true, true)), "a read shows the same");
}

#[tokio::test]
async fn a_stale_sign_in_cannot_change_a_switch() {
    let env = env();
    let stale = env.admin_stale("root");
    let resp = post_access(&env, &stale, &json!({"sync": true})).await;
    assert_eq!(resp.status, 403);
    assert_eq!(resp.json(), json!({"code": "forbidden", "step_up": true}));
    assert!(!env.app.features.is_on(Feature::Sync));
    assert!(change_lines(&env).is_empty());
    // A read needs no recent sign-in.
    assert_eq!(env.send(env.get_as(ACCESS, &stale)).await.status, 200);
}

#[tokio::test]
async fn a_ceiling_of_false_refuses_to_turn_the_switch_on_and_changes_nothing() {
    let env = env_with_ceiling(Ceiling { sync: false, remote: true });
    let admin = env.admin_fresh("root");
    let resp = post_access(&env, &admin, &json!({"sync": true, "remote": true})).await;
    assert_eq!(resp.status, 409, "{}", resp.text());
    assert_eq!(resp.json(), json!({"code": "conflict", "field": "sync"}));
    assert_eq!(env.app.features.effective(), Default::default(), "the member that was allowed did not change either");
    assert!(!env.app.features.stored(Feature::Remote));
    assert!(change_lines(&env).is_empty());
    // Turning a switch off, or leaving it off, is allowed under any ceiling.
    let resp = post_access(&env, &admin, &json!({"sync": false, "remote": true})).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.json(), state((false, false), (true, true)));
}

#[tokio::test]
async fn an_unknown_member_a_wrong_type_and_an_empty_body_are_refused_and_change_nothing() {
    let env = env();
    let admin = env.admin_fresh("root");
    let cases: Vec<(&str, Value, Option<&str>)> = vec![
        ("an unknown member", json!({"sync": true, "all": true}), None),
        ("only an unknown member", json!({"other": true}), None),
        ("a text", json!({"sync": "true"}), Some("sync")),
        ("a number", json!({"remote": 1}), Some("remote")),
        ("null", json!({"sync": null}), Some("sync")),
        ("a good member and a bad one", json!({"sync": true, "remote": "on"}), Some("remote")),
        ("an empty object", json!({}), None),
        ("an array", json!([true]), None),
        ("a bare boolean", json!(true), None),
    ];
    for (what, body, field) in cases {
        let resp = post_access(&env, &admin, &body).await;
        assert_eq!(resp.status, 400, "{what}: {}", resp.text());
        let expected = match field {
            Some(field) => json!({"code": "bad_request", "field": field}),
            None => json!({"code": "bad_request"}),
        };
        assert_eq!(resp.json(), expected, "{what}");
    }
    assert_eq!(env.app.features.effective(), Default::default());
    assert!(change_lines(&env).is_empty());
}

#[tokio::test]
async fn each_change_writes_one_line_that_names_the_switch_and_the_new_value_and_nothing_else() {
    let env = env();
    let admin = env.admin_fresh("root");
    assert_eq!(post_access(&env, &admin, &json!({"sync": true})).await.status, 200);
    let lines = change_lines(&env);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["result"], "ok");
    assert_eq!(line["detail"], json!({"switch": "sync", "state": "on"}), "the switch and the new value, nothing else");
    assert_eq!(line["actor"], admin.account.to_string());
    // Two changes in one request are two lines; a request that changes nothing writes none.
    assert_eq!(post_access(&env, &admin, &json!({"sync": false, "remote": true})).await.status, 200);
    let lines = change_lines(&env);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[1]["detail"], json!({"switch": "sync", "state": "off"}));
    assert_eq!(lines[2]["detail"], json!({"switch": "remote", "state": "on"}));
    assert_eq!(post_access(&env, &admin, &json!({"sync": false, "remote": true})).await.status, 200);
    assert_eq!(change_lines(&env).len(), 3, "a request that changes nothing writes no line");
    for line in change_lines(&env) {
        let members: Vec<&str> = line.as_object().expect("an object").keys().map(String::as_str).collect();
        assert_eq!(members.len(), 7, "the members every line has: {members:?}");
    }
}

#[tokio::test]
async fn a_switch_turned_on_opens_the_device_route_and_turned_off_closes_it_without_ending_a_token() {
    let env = env();
    let (token, _) = mint(&env, "alice");
    let admin = env.admin_fresh("root");
    assert_eq!(env.send(bearer(ME, &token)).await.status, 404);
    assert_eq!(post_access(&env, &admin, &json!({"remote": true})).await.status, 200);
    assert_eq!(env.send(bearer(ME, &token)).await.status, 200);
    assert_eq!(post_access(&env, &admin, &json!({"remote": false})).await.status, 200);
    let off = env.send(bearer(ME, &token)).await;
    assert_eq!(shape(&off), shape(&env.send(bearer(UNKNOWN, &token)).await), "off again answers as an absent path");
    assert_eq!(post_access(&env, &admin, &json!({"sync": true})).await.status, 200);
    assert_eq!(env.send(bearer(ME, &token)).await.status, 200, "either switch opens the device routes, and the token is still good");
}

#[tokio::test]
async fn a_change_is_announced_to_a_subscriber_only_when_a_feature_really_changes() {
    let env = env_with_ceiling(Ceiling { sync: true, remote: false });
    let mut seen = env.app.features.subscribe();
    assert!(!seen.has_changed().expect("open"));
    env.app.features.set(Feature::Sync, true).expect("on");
    assert!(seen.has_changed().expect("open"));
    assert_eq!(*seen.borrow_and_update(), holzwave_server::http::features::Switches { sync: true, remote: false });
    // The ceiling hides this switch: stored as on, still off, nobody is woken.
    env.app.features.set(Feature::Remote, true).expect("stored");
    assert!(!seen.has_changed().expect("open"));
    env.app.features.set(Feature::Sync, false).expect("off");
    assert!(seen.has_changed().expect("open"));
    assert_eq!(*seen.borrow_and_update(), Default::default());
    assert!(!env.app.features.set(Feature::Sync, false).expect("again"), "no change, no announcement");
    assert!(!seen.has_changed().expect("open"));
}

#[test]
fn the_action_vocabulary_lists_the_switch_change() {
    assert!(Action::ALL.iter().any(|a| a.as_str() == "device_access.change"));
    assert_eq!(Action::ALL.len(), 30);
}
