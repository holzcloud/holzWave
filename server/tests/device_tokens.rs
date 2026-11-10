//! What becomes of a device token after the grant: the lists of a person and of an administrator, the revocation and its
//! announcement, the sliding expiry, and the proof that no secret of the grant (the token, the device code, the user code) is in
//! any place the server writes: the audit file, the database and its write-ahead log, a captured log, a list, a `Debug` text.
//!
//! The router is called as a `tower` service over the real services of `support/api_env.rs`; the manual clock of the environment
//! moves time, so no test sleeps.

#![cfg(feature = "in-process-workers")]

#[path = "support/api_env.rs"]
mod api_env;
#[path = "support/auth_env.rs"]
mod auth_env;
#[path = "support/device_env.rs"]
mod device_env;
#[path = "support/edge.rs"]
mod edge;
#[path = "support/route_walk.rs"]
mod route_walk;
#[path = "support/sweep.rs"]
mod sweep;

use api_env::ApiEnv;
use axum::http::Method;
use device_env::{
    ALL_DEVICES, ME, MY_DEVICES, account_of, approve, bearer, bearer_get, code, device_rows,
    devices_of, grant_flow, lookup, native, open, shape, start, turn_on,
};
use holzwave_server::auth::session::Ended;
use holzwave_server::clock::Clock as _;
use holzwave_server::device::{DEVICE_IDLE_DEFAULT, DeviceToken, TOUCH_INTERVAL};
use holzwave_server::http::features::Feature;
use holzwave_server::store::AccountState;
use serde_json::{Value, json};
use sweep::{Canary, Category, Counts, Sweep};

fn env() -> ApiEnv {
    ApiEnv::new()
}

const DAY: i64 = 86_400;

fn revoke_lines(env: &ApiEnv) -> Vec<Value> {
    env.audit_lines()
        .into_iter()
        .filter(|l| l["action"] == "device.revoke")
        .collect()
}

fn id_path(base: &str, id: i64) -> String {
    format!("{base}/{id}")
}

// ---------------------------------------------------------------------------------------------------------------------
// The list of a person
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_person_lists_only_their_own_devices_newest_first_and_the_list_holds_no_secret() {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let bob = env.member("bob");
    let first = grant_flow(&env, &alice, "First").await;
    env.clock.advance(10);
    let second = grant_flow(&env, &alice, "Second").await;
    let others = grant_flow(&env, &bob, "Bob's").await;

    let resp = env.send(env.get_as(MY_DEVICES, &alice)).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.header("cache-control").as_deref(), Some("no-store"));
    let body = resp.json();
    let devices = body["devices"].as_array().expect("a list");
    assert_eq!(
        devices
            .iter()
            .map(|d| d["id"].as_i64().expect("an id"))
            .collect::<Vec<_>>(),
        [second.device_id, first.device_id],
        "newest first"
    );
    for device in devices {
        let mut members: Vec<&str> = device
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        members.sort_unstable();
        assert_eq!(
            members,
            [
                "created_at",
                "expires_at",
                "id",
                "last_seen",
                "name",
                "platform"
            ],
            "no hash, no token, no address"
        );
    }
    assert_eq!(devices[0]["name"], "Second");
    assert_eq!(devices[0]["platform"], "linux");
    let text = resp.text();
    for secret in [
        &first.token,
        &second.token,
        &others.token,
        &first.device_code,
        &first.user_code,
    ] {
        assert!(!text.contains(secret.as_str()), "the list holds a secret");
    }
    assert!(!text.contains("hwd_") && !text.contains("hwg_"));
    // Bob's list is his own.
    let bobs = env.send(env.get_as(MY_DEVICES, &bob)).await.json();
    assert_eq!(
        bobs["devices"].as_array().map(|d| d
            .iter()
            .map(|x| x["id"].as_i64().expect("id"))
            .collect::<Vec<_>>()),
        Some(vec![others.device_id])
    );
    // Nobody and a bearer token get no list.
    assert_eq!(env.send(native(Method::GET, MY_DEVICES)).await.status, 401);
    assert_eq!(
        env.send(bearer_get(MY_DEVICES, &first.token)).await.status,
        401
    );
}

// ---------------------------------------------------------------------------------------------------------------------
// The revocation
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_person_revokes_their_own_device_at_once_and_never_another_persons() {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let bob = env.member("bob");
    let mine = grant_flow(&env, &alice, "Mine").await;
    let theirs = grant_flow(&env, &bob, "Theirs").await;
    let alice_id = account_of(&env, "alice");

    // Another person's device, and ids that name nothing, are the same 404 and change nothing.
    for path in [
        id_path(MY_DEVICES, theirs.device_id),
        id_path(MY_DEVICES, theirs.device_id + 1000),
        format!("{MY_DEVICES}/abc"),
        format!("{MY_DEVICES}/-1"),
        format!("{MY_DEVICES}/99999999999999999999"),
        format!("{MY_DEVICES}/"),
    ] {
        let resp = env.send(env.delete_as(&path, &alice)).await;
        assert_eq!(
            (resp.status.as_u16(), code(&resp).as_str()),
            (404, "not_found"),
            "{path}: {}",
            resp.text()
        );
    }
    assert_eq!(device_rows(&env), 2);
    assert_eq!(env.send(bearer_get(ME, &theirs.token)).await.status, 200);
    assert!(revoke_lines(&env).is_empty());

    let resp = env
        .send(env.delete_as(&id_path(MY_DEVICES, mine.device_id), &alice))
        .await;
    assert_eq!(resp.status, 204, "{}", resp.text());
    assert_eq!(
        env.send(bearer_get(ME, &mine.token)).await.status,
        401,
        "the very next request is refused"
    );
    assert_eq!(device_rows(&env), 1);
    assert!(devices_of(&env, alice_id).is_empty());
    let lines = revoke_lines(&env);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["actor"], alice_id.to_string());
    assert_eq!(lines[0]["target"], alice_id.to_string());
    assert_eq!(lines[0]["detail"], json!({ "device": mine.device_id }));
    // The same call again names nothing.
    assert_eq!(
        env.send(env.delete_as(&id_path(MY_DEVICES, mine.device_id), &alice))
            .await
            .status,
        404
    );
    assert_eq!(revoke_lines(&env).len(), 1);
    // A stale sign-in cannot revoke, and a bearer token is no session.
    let stale = device_env::stale_member(&env, "stale");
    assert_eq!(
        env.send(env.delete_as(&id_path(MY_DEVICES, theirs.device_id), &stale))
            .await
            .status,
        403
    );
    let by_token = bearer(
        Method::DELETE,
        &id_path(MY_DEVICES, theirs.device_id),
        &theirs.token,
    )
    .header("origin", edge::ORIGIN)
    .header("content-type", "application/json");
    assert_eq!(env.send(by_token).await.status, 401);
    assert_eq!(device_rows(&env), 1);
}

#[tokio::test]
async fn an_administrator_lists_every_device_with_its_owner_and_revokes_any_a_member_may_not() {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let bob = env.member("bob");
    let admin = env.admin_fresh("root");
    let one = grant_flow(&env, &alice, "Alice's").await;
    env.clock.advance(5);
    let two = grant_flow(&env, &bob, "Bob's").await;

    let resp = env.send(env.get_as(ALL_DEVICES, &admin)).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    let rows = body["devices"].as_array().expect("a list");
    assert_eq!(
        rows.iter()
            .map(|d| d["id"].as_i64().expect("id"))
            .collect::<Vec<_>>(),
        [two.device_id, one.device_id]
    );
    let owner = |name: &str| {
        env.store
            .account_by_username(name)
            .expect("read")
            .expect("account")
    };
    assert_eq!(
        (
            rows[0]["username"].as_str(),
            rows[0]["display_name"].as_str()
        ),
        (Some("bob"), Some(owner("bob").display_name.as_str()))
    );
    assert_eq!(rows[1]["username"], "alice");
    let text = resp.text();
    assert!(
        !text.contains(&one.token) && !text.contains(&two.token) && !text.contains("hwd_"),
        "no token in the list"
    );
    for row in rows {
        assert!(row.get("token").is_none() && row.get("token_hash").is_none());
    }

    // A member gets the usual refusal on both.
    for request in [
        env.get_as(ALL_DEVICES, &alice),
        env.delete_as(&id_path(ALL_DEVICES, two.device_id), &alice),
    ] {
        let resp = env.send(request).await;
        assert_eq!(
            (resp.status.as_u16(), code(&resp).as_str()),
            (403, "forbidden"),
            "{}",
            resp.text()
        );
    }
    assert_eq!(device_rows(&env), 2);

    // The administrator ends a device of somebody else; the audit line names both and nothing secret.
    let resp = env
        .send(env.delete_as(&id_path(ALL_DEVICES, two.device_id), &admin))
        .await;
    assert_eq!(resp.status, 204, "{}", resp.text());
    assert_eq!(env.send(bearer_get(ME, &two.token)).await.status, 401);
    assert_eq!(env.send(bearer_get(ME, &one.token)).await.status, 200);
    let lines = revoke_lines(&env);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["actor"], account_of(&env, "root").to_string());
    assert_eq!(lines[0]["target"], account_of(&env, "bob").to_string());
    assert_eq!(lines[0]["detail"], json!({ "device": two.device_id }));
    for path in [
        id_path(ALL_DEVICES, two.device_id),
        format!("{ALL_DEVICES}/abc"),
    ] {
        let resp = env.send(env.delete_as(&path, &admin)).await;
        assert_eq!(resp.status, 404, "{path}");
    }
    assert_eq!(bob.account, account_of(&env, "bob"));
    assert_eq!(revoke_lines(&env).len(), 1);
}

#[tokio::test]
async fn every_revocation_is_announced_and_an_ended_account_ends_its_devices() {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let admin = env.admin_fresh("root");
    let mut ended = env.app.session_manager.subscribe_ended();
    let one = grant_flow(&env, &alice, "One").await;
    let two = grant_flow(&env, &alice, "Two").await;
    let three = grant_flow(&env, &alice, "Three").await;

    assert_eq!(
        env.send(env.delete_as(&id_path(MY_DEVICES, one.device_id), &alice))
            .await
            .status,
        204
    );
    assert_eq!(
        ended.try_recv().expect("an announcement"),
        Ended::Device(one.device_id),
        "a person's revocation"
    );
    assert_eq!(
        env.send(env.delete_as(&id_path(ALL_DEVICES, two.device_id), &admin))
            .await
            .status,
        204
    );
    assert_eq!(
        ended.try_recv().expect("an announcement"),
        Ended::Device(two.device_id),
        "an administrator's revocation"
    );
    assert_eq!(
        env.send(bearer(Method::DELETE, ME, &three.token))
            .await
            .status,
        204
    );
    assert_eq!(
        ended.try_recv().expect("an announcement"),
        Ended::Device(three.device_id),
        "a device's own sign-out"
    );
    assert!(
        ended.try_recv().is_err(),
        "one announcement for each, and nothing else"
    );
    // A call that revokes nothing announces nothing.
    assert_eq!(
        env.send(env.delete_as(&id_path(MY_DEVICES, one.device_id), &alice))
            .await
            .status,
        404
    );
    assert!(ended.try_recv().is_err());

    // What an announcement ends: that device, and every device of an account that ended; never a device because a session did.
    let (a, b) = (account_of(&env, "alice"), account_of(&env, "root"));
    assert!(Ended::Device(7).ends_device(7, a) && !Ended::Device(7).ends_device(8, a));
    assert!(
        Ended::Account(a).ends_device(7, a)
            && Ended::Account(a).ends_device(8, a)
            && !Ended::Account(a).ends_device(7, b)
    );
    let session = alice.session.clone();
    assert!(!Ended::Session(session.id_hash).ends_device(7, a));
    assert!(
        !Ended::Handle {
            account: a,
            prefix: [0; holzwave_server::auth::session::HANDLE_BYTES]
        }
        .ends_device(7, a)
    );
    assert!(
        !Ended::Device(7).ends(&session),
        "no device announcement ends a session"
    );
    assert!(
        Ended::Account(a).ends(&session),
        "and the account announcement still ends the sessions"
    );
    env.app
        .session_manager
        .end_all(a)
        .expect("end every session");
    assert_eq!(
        ended.try_recv().expect("the account announcement"),
        Ended::Account(a)
    );

    // An account that is disabled loses its devices' use at once; the same token works again when it is enabled.
    let four = grant_flow(&env, &env.member("carol"), "Four").await;
    let carol = account_of(&env, "carol");
    assert_eq!(env.send(bearer_get(ME, &four.token)).await.status, 200);
    env.store
        .write(|tx| tx.account_set_state(carol, AccountState::Disabled))
        .expect("disable");
    assert_eq!(
        env.send(bearer_get(ME, &four.token)).await.status,
        401,
        "a disabled account's token is refused"
    );
    env.store
        .write(|tx| tx.account_set_state(carol, AccountState::Active))
        .expect("enable");
    assert_eq!(
        env.send(bearer_get(ME, &four.token)).await.status,
        200,
        "and works again"
    );
}

#[tokio::test]
async fn a_device_ends_its_own_sign_in_through_the_same_path_and_only_its_own() {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let one = grant_flow(&env, &alice, "One").await;
    let two = grant_flow(&env, &alice, "Two").await;
    let account = account_of(&env, "alice");

    // Nothing but the device's own token ends it.
    let cookie_only = env.send(native(Method::DELETE, ME).signed(&alice)).await;
    assert_eq!(
        (cookie_only.status.as_u16(), code(&cookie_only).as_str()),
        (401, "invalid")
    );
    assert!(cookie_only.all("set-cookie").is_empty());
    assert_eq!(env.send(native(Method::DELETE, ME)).await.status, 401);
    assert_eq!(
        env.send(bearer(Method::DELETE, ME, "hwd_not-a-token"))
            .await
            .status,
        401
    );
    assert_eq!(device_rows(&env), 2);
    assert!(revoke_lines(&env).is_empty());

    let resp = env.send(bearer(Method::DELETE, ME, &one.token)).await;
    assert_eq!(resp.status, 204, "{}", resp.text());
    assert!(resp.body.is_empty() && resp.all("set-cookie").is_empty());
    assert_eq!(
        env.send(bearer_get(ME, &one.token)).await.status,
        401,
        "the same token is refused on the next request"
    );
    assert_eq!(
        env.send(bearer(Method::DELETE, ME, &one.token))
            .await
            .status,
        401,
        "and cannot end anything twice"
    );
    assert_eq!(
        devices_of(&env, account)
            .iter()
            .map(|d| d.id)
            .collect::<Vec<_>>(),
        [two.device_id],
        "only its own row went"
    );
    assert_eq!(
        env.send(bearer_get(ME, &two.token)).await.status,
        200,
        "the other device keeps working"
    );
    let lines = revoke_lines(&env);
    assert_eq!(lines.len(), 1, "one line for one revocation");
    assert_eq!(
        (lines[0]["actor"].as_str(), lines[0]["target"].as_str()),
        (
            Some(account.to_string().as_str()),
            Some(account.to_string().as_str())
        )
    );
    assert_eq!(lines[0]["detail"], json!({ "device": one.device_id }));

    // With both switches off the sign-out is as hidden as the rest of the group.
    for feature in [Feature::Sync, Feature::Remote] {
        assert!(env.app.features.set(feature, false).is_ok());
    }
    let closed = env.send(bearer(Method::DELETE, ME, &two.token)).await;
    let nothing = env
        .send(bearer(
            Method::DELETE,
            "/api/device/no-such-route",
            &two.token,
        ))
        .await;
    assert_eq!(closed.status, 404);
    assert_eq!(shape(&closed), shape(&nothing));
    assert_eq!(device_rows(&env), 1);
}

#[tokio::test]
async fn the_lists_and_the_revocation_work_while_both_switches_are_off() {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let admin = env.admin_fresh("root");
    let one = grant_flow(&env, &alice, "One").await;
    let two = grant_flow(&env, &alice, "Two").await;
    assert!(env.app.features.set(Feature::Remote, false).expect("off"));
    assert!(env.app.features.is_closed(ME), "both switches are off");
    assert_eq!(
        env.send(bearer_get(ME, &one.token)).await.status,
        404,
        "the device routes are hidden"
    );

    assert_eq!(
        env.send(env.get_as(MY_DEVICES, &alice)).await.json()["devices"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        env.send(env.get_as(ALL_DEVICES, &admin)).await.json()["devices"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        env.send(env.delete_as(&id_path(MY_DEVICES, one.device_id), &alice))
            .await
            .status,
        204
    );
    assert_eq!(
        env.send(env.delete_as(&id_path(ALL_DEVICES, two.device_id), &admin))
            .await
            .status,
        204
    );
    assert_eq!(device_rows(&env), 0, "a device can always be taken back");
    assert_eq!(revoke_lines(&env).len(), 2);
    // Turned on again, the revoked tokens stay revoked.
    turn_on(&env, Feature::Sync);
    assert_eq!(env.send(bearer_get(ME, &one.token)).await.status, 401);
}

// ---------------------------------------------------------------------------------------------------------------------
// Sliding expiry
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_token_lives_for_the_idle_time_after_its_last_use_and_not_a_second_longer() {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let account = account_of(&env, "alice");
    let t0 = env.clock.unix();
    let used_early = grant_flow(&env, &alice, "A").await;
    let never_used = grant_flow(&env, &alice, "B").await;
    let almost = grant_flow(&env, &alice, "C").await;
    let used_early_too = grant_flow(&env, &alice, "D").await;
    assert_eq!(DEVICE_IDLE_DEFAULT, 90 * DAY);
    let expires = |id: i64| {
        devices_of(&env, account)
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.expires_at)
    };
    assert_eq!(expires(never_used.device_id), Some(t0 + 90 * DAY));

    // Day 60: two of them are used; their end moves to day 150.
    env.clock.set(t0 + 60 * DAY);
    for granted in [&used_early, &used_early_too] {
        assert_eq!(env.send(bearer_get(ME, &granted.token)).await.status, 200);
        assert_eq!(
            expires(granted.device_id),
            Some(t0 + 150 * DAY),
            "a token used on day 60 lives until day 150"
        );
    }
    assert_eq!(
        expires(never_used.device_id),
        Some(t0 + 90 * DAY),
        "the others are untouched"
    );
    // One second before day 90 an unused token still works; at day 90 it is refused.
    env.clock.set(t0 + 90 * DAY - 1);
    assert_eq!(env.send(bearer_get(ME, &almost.token)).await.status, 200);
    env.clock.set(t0 + 90 * DAY);
    let refused = env.send(bearer_get(ME, &never_used.token)).await;
    assert_eq!(
        (refused.status.as_u16(), code(&refused).as_str()),
        (401, "invalid"),
        "{}",
        refused.text()
    );
    // The device used on day 60 works until the last second before day 150 and is refused at day 150.
    env.clock.set(t0 + 150 * DAY - 1);
    assert_eq!(
        env.send(bearer_get(ME, &used_early.token)).await.status,
        200
    );
    env.clock.set(t0 + 150 * DAY);
    assert_eq!(
        env.send(bearer_get(ME, &used_early_too.token)).await.status,
        401
    );
}

#[tokio::test]
async fn the_last_seen_time_in_the_list_moves_at_most_once_a_minute() {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let granted = grant_flow(&env, &alice, "Laptop").await;
    let last_seen = |env: &ApiEnv| devices_of(env, account_of(env, "alice"))[0].last_seen;
    let start = env.clock.unix();
    assert_eq!(last_seen(&env), start);
    for step in 1..TOUCH_INTERVAL {
        env.clock.set(start + step);
        assert_eq!(env.send(bearer_get(ME, &granted.token)).await.status, 200);
        assert_eq!(
            last_seen(&env),
            start,
            "inside the minute nothing is written"
        );
        if step % 20 == 0 {
            // The list says the same as the store.
            let list = env.send(env.get_as(MY_DEVICES, &alice)).await;
            assert_eq!(
                list.json()["devices"][0]["last_seen"],
                holzwave_server::clock::rfc3339(holzwave_server::clock::from_unix(start))
            );
        }
    }
    env.clock.set(start + TOUCH_INTERVAL);
    assert_eq!(env.send(bearer_get(ME, &granted.token)).await.status, 200);
    assert_eq!(last_seen(&env), start + TOUCH_INTERVAL);
}

// ---------------------------------------------------------------------------------------------------------------------
// The oracle
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn the_oracle_has_a_hand_written_row_for_each_route_of_the_grant_and_the_lists() {
    use route_walk::Kind::{Admin, Device, DeviceOpen, Fresh};
    let table = route_walk::table();
    let expected: [(&str, &str, route_walk::Kind, u16, Option<&str>); 10] = [
        ("POST", "/api/device/code", DeviceOpen, 200, None),
        ("POST", "/api/device/token", DeviceOpen, 200, None),
        ("POST", "/api/device/lookup", Fresh, 200, None),
        (
            "POST",
            "/api/device/approve",
            Fresh,
            204,
            Some("device.approve"),
        ),
        ("POST", "/api/device/deny", Fresh, 204, Some("device.deny")),
        (
            "DELETE",
            "/api/device/me",
            Device,
            204,
            Some("device.revoke"),
        ),
        ("GET", "/api/me/devices", Fresh, 200, None),
        (
            "DELETE",
            "/api/me/devices/{id}",
            Fresh,
            204,
            Some("device.revoke"),
        ),
        ("GET", "/api/admin/devices", Admin, 200, None),
        (
            "DELETE",
            "/api/admin/devices/{id}",
            Admin,
            204,
            Some("device.revoke"),
        ),
    ];
    for (method, path, kind, success, audit) in expected {
        let row = table
            .iter()
            .find(|r| (r.method, r.path) == (method, path))
            .unwrap_or_else(|| panic!("no row for {method} {path}"));
        assert_eq!(
            (row.kind, row.success, row.audit),
            (kind, success, audit),
            "{method} {path}"
        );
        assert_eq!(row.changes, method != "GET", "{method} {path}");
    }
    // The rows under the gated prefix are walked in both switch states by the oracle's own prefix list; the lists and the
    // revocations of a person and an administrator are not under a gate.
    for path in [
        "/api/device/code",
        "/api/device/token",
        "/api/device/lookup",
        "/api/device/approve",
        "/api/device/deny",
        "/api/device/me",
    ] {
        assert!(route_walk::is_gated(path), "{path}");
    }
    for path in [
        "/api/me/devices",
        "/api/me/devices/{id}",
        "/api/admin/devices",
        "/api/admin/devices/{id}",
    ] {
        assert!(!route_walk::is_gated(path), "{path}");
    }
    let open_row = table
        .iter()
        .find(|r| r.path == "/api/device/code")
        .expect("the start");
    assert_eq!(
        route_walk::unauth_expectation(open_row, false),
        [(404, "not_found")]
    );
    assert_eq!(
        route_walk::unauth_expectation(open_row, true),
        [(400, "bad_request")]
    );
    assert_eq!(
        route_walk::wrong_method_expectation(open_row, true),
        (405, "bad_request")
    );
    let lookup_row = table
        .iter()
        .find(|r| r.path == "/api/device/lookup")
        .expect("the lookup");
    assert_eq!(
        route_walk::unauth_expectation(lookup_row, true),
        [(401, "invalid")]
    );
    assert_eq!(
        route_walk::unauth_expectation(lookup_row, false),
        [(404, "not_found")]
    );
}

// ---------------------------------------------------------------------------------------------------------------------
// Where no secret may be
// ---------------------------------------------------------------------------------------------------------------------

/// Everything a whole use of the grant leaves in the places the server writes, and the secrets it handled.
struct Run {
    env: ApiEnv,
    secrets: Vec<Canary>,
    answers: Vec<(String, String)>,
}

/// A whole use: two grants (one approved, one denied), the lists, the revocation, an abuse that is throttled.
async fn use_the_grant() -> Run {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let admin = env.admin_fresh("root");
    let mut secrets = Vec::new();
    let mut answers = Vec::new();
    let note = |name: &str, text: &str, secrets: &mut Vec<Canary>| {
        secrets.push(Canary::with_text(name, Category::Secret, text))
    };

    let approved = start(&env, "Laptop", "linux").await;
    note("user code", &approved.user_code, &mut secrets);
    note(
        "user code without the dash",
        &approved.user_code.replace('-', ""),
        &mut secrets,
    );
    note("device code", &approved.device_code, &mut secrets);
    let seen = lookup(&env, &alice, &approved.user_code).await;
    answers.push(("lookup answer".to_owned(), seen.text()));
    assert_eq!(approve(&env, &alice, &approved.user_code).await.status, 204);
    env.clock.advance(5);
    let delivered = device_env::poll(&env, &approved.device_code).await;
    let token = delivered.json()["token"]
        .as_str()
        .expect("a token")
        .to_owned();
    note("token", &token, &mut secrets);
    let id = delivered.json()["device_id"].as_i64().expect("an id");
    let me = env.send(bearer_get(ME, &token)).await;
    answers.push(("device answer".to_owned(), me.text()));

    let denied = start(&env, "Phone", "android").await;
    note("denied user code", &denied.user_code, &mut secrets);
    note("denied device code", &denied.device_code, &mut secrets);
    assert_eq!(
        device_env::deny(&env, &alice, &denied.user_code)
            .await
            .status,
        204
    );
    answers.push((
        "denied poll".to_owned(),
        device_env::poll(&env, &denied.device_code).await.text(),
    ));

    answers.push((
        "list of a person".to_owned(),
        env.send(env.get_as(MY_DEVICES, &alice)).await.text(),
    ));
    answers.push((
        "list of an administrator".to_owned(),
        env.send(env.get_as(ALL_DEVICES, &admin)).await.text(),
    ));
    // An abuse that is throttled: unknown codes, and then a lock.
    for _ in 0..6 {
        let resp = device_env::lookup(&env, &alice, "BCDF-GHJK").await;
        answers.push(("a wrong code".to_owned(), resp.text()));
    }
    assert_eq!(
        env.send(env.delete_as(&id_path(MY_DEVICES, id), &alice))
            .await
            .status,
        204
    );
    answers.push((
        "the token after the revocation".to_owned(),
        env.send(bearer_get(ME, &token)).await.text(),
    ));
    Run {
        env,
        secrets,
        answers,
    }
}

#[tokio::test]
async fn the_audit_names_the_actions_and_the_rows_and_none_of_the_secrets() {
    let run = use_the_grant().await;
    let lines = run.env.audit_lines();
    let of = |action: &str| {
        lines
            .iter()
            .filter(|l| l["action"] == action)
            .cloned()
            .collect::<Vec<_>>()
    };
    let (approve, deny, revoke) = (of("device.approve"), of("device.deny"), of("device.revoke"));
    assert_eq!(
        (approve.len(), deny.len(), revoke.len()),
        (1, 1, 1),
        "one line for each decision"
    );
    let alice = account_of(&run.env, "alice").to_string();
    for line in [&approve[0], &deny[0], &revoke[0]] {
        assert_eq!(line["actor"], alice);
        assert_eq!(line["result"], "ok");
    }
    assert!(
        approve[0]["detail"]["grant"].as_i64().is_some(),
        "the approval names the grant's row: {}",
        approve[0]
    );
    assert!(deny[0]["detail"]["grant"].as_i64().is_some());
    assert!(revoke[0]["detail"]["device"].as_i64().is_some());
    assert_eq!(revoke[0]["target"], alice);
    // No secret and no name is in the file, in any form.
    let sweep = Sweep::new(&run.secrets);
    let findings = sweep.scan_text("the audit directory", &run.env.audit_text());
    assert!(
        findings.is_empty(),
        "{:?}",
        findings
            .iter()
            .map(sweep::Finding::line)
            .collect::<Vec<_>>()
    );
    let text = run.env.audit_text();
    assert!(
        !text.contains("Laptop") && !text.contains("Phone"),
        "no device name is in the audit file"
    );
}

#[tokio::test]
async fn no_file_of_the_server_holds_a_code_or_a_token_only_their_hashes_and_the_answers_hold_them_only_where_they_are_handed_out()
 {
    let run = use_the_grant().await;
    let sweep = Sweep::new(&run.secrets);
    let root = run.env.scratch.path().to_path_buf();
    let mut counts = Counts::default();
    let findings = sweep.scan_tree(&root, &mut counts);
    assert!(
        counts.files >= 3 && counts.bytes > 4_000,
        "the scan read the database, its log and the audit file: {counts:?}"
    );
    assert!(
        findings.is_empty(),
        "{:?}",
        findings
            .iter()
            .map(sweep::Finding::line)
            .collect::<Vec<_>>()
    );

    // The scan is not blind. The hash of the token is in the database (as the bytes the store keeps) and the scan of the same
    // tree finds a canary planted in a file of it.
    let token = run
        .secrets
        .iter()
        .find(|c| c.name == "token")
        .expect("the token")
        .text()
        .to_owned();
    let hash = DeviceToken::parse(&token).expect("a token").hash();
    let mut hash_found = false;
    for file in sweep::files_below(&root) {
        if let Ok(bytes) = std::fs::read(&file) {
            hash_found |= bytes.windows(hash.len()).any(|w| w == hash);
        }
    }
    assert!(hash_found, "the store holds the hash of the token");
    let planted = root.join("planted.txt");
    std::fs::write(&planted, format!("x {token} y")).expect("a planted file");
    assert!(
        !sweep.scan_tree(&root, &mut Counts::default()).is_empty(),
        "a planted token is found"
    );
    std::fs::remove_file(&planted).expect("remove");

    // The answers: every secret is in the answer that hands it out and in no other. The one list and lookup answers hold none.
    for (what, text) in &run.answers {
        let findings = sweep.scan_text(what, text);
        assert!(
            findings.is_empty(),
            "{what}: {:?}",
            findings
                .iter()
                .map(sweep::Finding::line)
                .collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn the_debug_text_of_the_application_and_its_services_holds_no_secret() {
    let run = use_the_grant().await;
    let sweep = Sweep::new(&run.secrets);
    let env = &run.env;
    let token = run
        .secrets
        .iter()
        .find(|c| c.name == "token")
        .expect("a token")
        .text()
        .to_owned();
    let parsed = DeviceToken::parse(&token).expect("a token");
    let texts = [
        ("the application", format!("{:?} {:#?}", env.app, env.app)),
        ("the sessions", format!("{:?}", env.app.session_manager)),
        ("the store", format!("{:?}", env.store)),
        ("the token", format!("{parsed:?} {parsed}")),
        (
            "a code",
            format!(
                "{:?}",
                holzwave_server::device::GrantCode::parse(
                    run.secrets
                        .iter()
                        .find(|c| c.name == "device code")
                        .expect("code")
                        .text()
                )
                .expect("a code")
            ),
        ),
        (
            "the user code",
            format!(
                "{:?}",
                holzwave_server::device::UserCode::normalise(
                    run.secrets
                        .iter()
                        .find(|c| c.name == "user code")
                        .expect("code")
                        .text()
                )
                .expect("a code")
            ),
        ),
    ];
    for (what, text) in &texts {
        let findings = sweep.scan_text(what, text);
        assert!(
            findings.is_empty(),
            "{what}: {:?}",
            findings
                .iter()
                .map(sweep::Finding::line)
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(
        texts[3].1, "DeviceToken(..) DeviceToken(..)",
        "a token prints a marker"
    );
}

/// The child half of the log capture: it uses the grant and writes the secrets to a file the parent named. Without the
/// variables it does nothing.
#[tokio::test(flavor = "multi_thread")]
async fn child_grant_run() {
    let Some(sink) = std::env::var_os("HW_GRANT_SINK") else {
        return;
    };
    let run = use_the_grant().await;
    let lines: Vec<String> = run
        .secrets
        .iter()
        .map(|c| format!("{}\t{}", c.name, c.text()))
        .collect();
    std::fs::write(sink, lines.join("\n")).expect("write the secrets for the parent");
    println!("child grant run finished");
}

#[test]
fn log_capture_standard_output_and_error_of_a_whole_use_of_the_grant_hold_no_secret() {
    let dir = auth_env::scratch::Scratch::new("grant-child");
    let sink = dir.path().join("secrets.txt");
    let exe = std::env::current_exe().expect("the test binary");
    let output = std::process::Command::new(exe)
        .args([
            "--exact",
            "child_grant_run",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("HW_GRANT_SINK", &sink)
        .env("RUST_BACKTRACE", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the child process");
    let captured = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "the child failed: {}",
        captured.chars().take(2000).collect::<String>()
    );
    assert!(
        captured.contains("child grant run finished"),
        "the child ran the whole use"
    );
    let text = std::fs::read_to_string(&sink).expect("the secrets of the child");
    let owned: Vec<Canary> = text
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(n, t)| Canary::with_text(n, Category::Secret, t))
        .collect();
    assert!(owned.len() >= 6, "{} secrets", owned.len());
    let sweep = Sweep::new(&owned);
    let lines: Vec<String> = captured.lines().map(str::to_owned).collect();
    let findings = sweep.scan_lines("the captured log", &lines, &mut Counts::default());
    assert!(
        findings.is_empty(),
        "{:?}",
        findings
            .iter()
            .map(sweep::Finding::line)
            .collect::<Vec<_>>()
    );
    // The capture is not blind: a secret written to the stream is found.
    let planted = vec![format!("warn x token={}", owned[0].text())];
    assert!(
        !sweep
            .scan_lines("planted", &planted, &mut Counts::default())
            .is_empty()
    );
    println!(
        "log capture: {} bytes of standard output and error, {} secrets, 0 hits",
        captured.len(),
        owned.len()
    );
}
