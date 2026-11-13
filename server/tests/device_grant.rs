//! The device grant through the real router and the real store: an app that has no credential starts a grant, shows a code,
//! and gets a device token exactly once after a person approves it under their own login with a recent sign-in. Then its abuse
//! cases: denial, expiry, slowing down, the throttles on wrong codes, the caps on open grants, the one-time delivery, the cap on
//! devices, and what the audit lines hold.
//!
//! The router is called as a `tower` service (no socket) over the real services of `support/api_env.rs`; the manual clock of the
//! environment moves time, so no test sleeps.

#![cfg(feature = "in-process-workers")]

#[path = "support/api_env.rs"]
mod api_env;
#[path = "support/auth_env.rs"]
mod auth_env;
#[path = "support/device_env.rs"]
mod device_env;
#[path = "support/edge.rs"]
mod edge;

use std::net::IpAddr;

use api_env::ApiEnv;
use device_env::{
    APPROVE, DENY, LOOKUP, ME, MY_DEVICES, absent, account_of, approve, bearer_get, code, deny,
    device_rows, devices_of, grant_flow, grant_rows, lookup, native_post_from, open, poll,
    poll_from, shape, start, start_from, turn_on,
};
use edge::{ORIGIN, Resp};
use holzwave_server::clock::{Clock as _, from_unix, rfc3339};
use holzwave_server::device::{
    GRANT_TTL, MAX_DEVICES_PER_ACCOUNT, MAX_PENDING, MAX_PENDING_PER_ADDR, MAX_PENDING_PER_NET, POLL_INTERVAL,
    USER_CODE_ALPHABET,
};
use holzwave_server::http::features::Feature;
use holzwave_server::store::AccountState;
use serde_json::json;

fn env() -> ApiEnv {
    ApiEnv::new()
}

fn state(resp: &Resp) -> String {
    resp.json()["state"].as_str().unwrap_or_default().to_owned()
}

fn is_user_code(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 9
        && bytes[4] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || USER_CODE_ALPHABET.contains(b))
}

// ---------------------------------------------------------------------------------------------------------------------
// The whole flow
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn tracer_an_app_with_nothing_in_hand_gets_a_working_device_token_only_through_a_persons_approval()
 {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let account = account_of(&env, "alice");

    // 1. The start: no cookie, no credential, a name and a platform.
    let resp = env
        .send(device_env::native_post(
            device_env::CODE,
            &device_env::start_body("Alice's laptop", "macos"),
        ))
        .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.header("cache-control").as_deref(), Some("no-store"));
    assert!(resp.all("set-cookie").is_empty());
    let started = resp.json();
    let members: Vec<&str> = {
        let mut names: Vec<&str> = started
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        names
    };
    assert_eq!(
        members,
        [
            "device_code",
            "expires_in",
            "interval",
            "user_code",
            "verification_uri",
            "verification_uri_complete"
        ]
    );
    let device_code = started["device_code"]
        .as_str()
        .expect("a device code")
        .to_owned();
    let user_code = started["user_code"]
        .as_str()
        .expect("a user code")
        .to_owned();
    assert!(
        device_code.starts_with("hwg_") && device_code.len() == 4 + 43,
        "the device code has its shape"
    );
    assert!(
        is_user_code(&user_code),
        "the user code has the form XXXX-XXXX: {user_code}"
    );
    assert_eq!(
        started["verification_uri"],
        format!("{ORIGIN}/"),
        "the address is the server's own origin"
    );
    assert_eq!(
        started["verification_uri_complete"],
        format!("{ORIGIN}/#device={user_code}")
    );
    assert_eq!(
        (started["expires_in"].as_i64(), started["interval"].as_i64()),
        (Some(GRANT_TTL), Some(POLL_INTERVAL))
    );
    assert_eq!((GRANT_TTL, POLL_INTERVAL), (600, 5));
    assert_eq!(grant_rows(&env), 1);
    assert_eq!(device_rows(&env), 0, "no device exists before the approval");

    // 2. Until the person decides, the app is told to wait.
    let waiting = poll(&env, &device_code).await;
    assert_eq!(
        (waiting.status.as_u16(), state(&waiting).as_str()),
        (200, "pending"),
        "{}",
        waiting.text()
    );
    assert_eq!(waiting.json()["interval"], POLL_INTERVAL);
    assert_eq!(device_rows(&env), 0);

    // 3. The lookup shows what the person is asked to approve: the name, the platform, the address and the time.
    let seen = lookup(&env, &alice, &user_code).await;
    assert_eq!(seen.status, 200, "{}", seen.text());
    assert_eq!(
        seen.json(),
        json!({ "name": "Alice's laptop", "platform": "macos", "addr": "0.0.0.0", "created_at": rfc3339(from_unix(env.clock.unix())) })
    );
    assert!(
        !seen.text().contains(&user_code) && !seen.text().contains(&device_code),
        "the answer repeats no code"
    );
    assert_eq!(device_rows(&env), 0, "a lookup decides nothing");

    // 4. The approval.
    let approved = approve(&env, &alice, &user_code).await;
    assert_eq!(approved.status, 204, "{}", approved.text());
    assert_eq!(
        device_rows(&env),
        0,
        "an approval makes no device; the app's poll does"
    );

    // 5. The next poll (after the interval) gets the token, once.
    env.clock.advance(POLL_INTERVAL);
    let delivered = poll(&env, &device_code).await;
    assert_eq!(delivered.status, 200, "{}", delivered.text());
    assert_eq!(
        delivered.header("cache-control").as_deref(),
        Some("no-store")
    );
    assert_eq!(
        delivered.header("content-type").as_deref(),
        Some("application/json")
    );
    let body = delivered.json();
    assert_eq!(body["state"], "approved");
    assert_eq!(
        body.as_object().map(|o| o.len()),
        Some(4),
        "state, token, device id and expiry, nothing else: {body}"
    );
    let token = body["token"].as_str().expect("a token").to_owned();
    assert!(
        token.starts_with("hwd_") && token.len() == 4 + 43,
        "the token has its shape"
    );
    let device_id = body["device_id"].as_i64().expect("a device id");
    assert_eq!(
        body["expires_at"],
        rfc3339(from_unix(
            env.clock.unix() + holzwave_server::device::DEVICE_IDLE_DEFAULT
        ))
    );
    assert_eq!(grant_rows(&env), 0, "the grant is gone");

    // 6. The token works on a device route and names the device.
    let me = env.send(bearer_get(ME, &token)).await;
    assert_eq!(me.status, 200, "{}", me.text());
    assert_eq!(me.json()["device"]["id"], device_id);
    assert_eq!(me.json()["device"]["name"], "Alice's laptop");
    assert_eq!(me.json()["device"]["platform"], "macos");

    // 7. The device is the approver's: it is in the store's list for her account and in nobody else's.
    let devices = devices_of(&env, account);
    assert_eq!(devices.len(), 1);
    assert_eq!(
        (devices[0].id, devices[0].name.as_str()),
        (device_id, "Alice's laptop")
    );
    env.member("bob");
    assert!(devices_of(&env, account_of(&env, "bob")).is_empty());
    let listed = env.send(env.get_as(MY_DEVICES, &alice)).await;
    assert_eq!(
        listed.json()["devices"].as_array().map(Vec::len),
        Some(1),
        "{}",
        listed.text()
    );
}

#[tokio::test]
async fn a_user_code_is_read_as_a_person_types_it_and_a_wrong_letter_is_the_same_answer_as_an_unknown_code()
 {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let started = start(&env, "Phone", "android").await;
    let compact: String = started.user_code.chars().filter(|c| *c != '-').collect();
    let spelled = [
        started.user_code.to_lowercase(),
        compact.clone(),
        compact.to_lowercase(),
        format!("  {} {}  ", compact[..4].to_lowercase(), &compact[4..]),
        format!("{}-{}", &compact[..4], &compact[4..]),
    ];
    for text in &spelled {
        let resp = lookup(&env, &alice, text).await;
        assert_eq!(resp.status, 200, "{text:?}: {}", resp.text());
        assert_eq!(resp.json()["name"], "Phone");
    }
    // A letter outside the alphabet (a vowel) leaves seven letters: it names no code. So does a code nobody made.
    let with_a_vowel = format!("{}A", &compact[..7]);
    let unknown = if compact.starts_with("BCDF") {
        "GHJK-LMNP"
    } else {
        "BCDF-GHJK"
    };
    let wrong = lookup(&env, &alice, &with_a_vowel).await;
    let never_made = lookup(&env, &alice, unknown).await;
    assert_eq!(
        (wrong.status.as_u16(), code(&wrong).as_str()),
        (410, "gone"),
        "{}",
        wrong.text()
    );
    assert_eq!(
        shape(&wrong),
        shape(&never_made),
        "a malformed code answers as an unknown one"
    );
    // Nothing but letters of the alphabet is read: an empty text and a text that is far too long name no code either (four wrong
    // codes in all, so the throttle of five has not started).
    for text in ["", &"B".repeat(500)] {
        let resp = lookup(&env, &alice, text).await;
        assert_eq!(shape(&resp), shape(&never_made), "{}", text.len());
    }
}

#[tokio::test]
async fn the_grant_routes_are_the_unknown_path_answer_while_both_switches_are_off() {
    let env = env();
    let alice = env.member("alice");
    let (anonymous_start, anonymous_poll) = (
        env.send(device_env::native_post(
            device_env::CODE,
            &device_env::start_body("Laptop", "linux"),
        ))
        .await,
        env.send(device_env::native_post(
            device_env::TOKEN,
            &json!({ "device_code": "hwg_nothing" }),
        ))
        .await,
    );
    let absent_answer = env.send(absent()).await;
    assert_eq!(
        (absent_answer.status.as_u16(), code(&absent_answer).as_str()),
        (404, "not_found")
    );
    assert_eq!(shape(&anonymous_start), shape(&absent_answer));
    assert_eq!(shape(&anonymous_poll), shape(&absent_answer));
    for path in [LOOKUP, APPROVE, DENY] {
        let resp = env
            .send(env.post_json(path, &json!({ "code": "BCDF-GHJK" }), &alice))
            .await;
        let unknown = env
            .send(env.post_json(
                "/api/device/no-such-route",
                &json!({ "code": "BCDF-GHJK" }),
                &alice,
            ))
            .await;
        assert_eq!(shape(&resp), shape(&unknown), "{path}");
        assert_eq!(resp.status, 404, "{path}");
    }
    assert_eq!(
        grant_rows(&env),
        0,
        "nothing was stored by a request that was answered as absent"
    );
    assert_eq!(
        env.count_rows("SELECT COUNT(*) FROM throttle"),
        0,
        "and nothing was counted"
    );

    // One switch is enough to open the group; both off closes it again.
    turn_on(&env, Feature::Sync);
    assert_eq!(
        env.send(device_env::native_post(
            device_env::CODE,
            &device_env::start_body("Laptop", "linux")
        ))
        .await
        .status,
        200
    );
    assert!(env.app.features.set(Feature::Sync, false).expect("off"));
    let closed = env
        .send(device_env::native_post(
            device_env::CODE,
            &device_env::start_body("Laptop", "linux"),
        ))
        .await;
    assert_eq!(shape(&closed), shape(&absent_answer));
}

// ---------------------------------------------------------------------------------------------------------------------
// Deny, expiry, slowing down, the one-time delivery
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_denied_grant_never_yields_a_token_and_a_decided_code_is_gone() {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let started = start(&env, "Tablet", "android").await;
    let denied = deny(&env, &alice, &started.user_code).await;
    assert_eq!(denied.status, 204, "{}", denied.text());
    let polled = poll(&env, &started.device_code).await;
    assert_eq!(
        (polled.status.as_u16(), state(&polled).as_str()),
        (200, "denied"),
        "{}",
        polled.text()
    );
    assert!(polled.json().get("token").is_none());
    assert_eq!(device_rows(&env), 0, "no device exists");
    // Approving, denying or looking up a decided grant is the answer for an unknown code.
    for resp in [
        approve(&env, &alice, &started.user_code).await,
        deny(&env, &alice, &started.user_code).await,
        lookup(&env, &alice, &started.user_code).await,
    ] {
        assert_eq!(
            (resp.status.as_u16(), code(&resp).as_str()),
            (410, "gone"),
            "{}",
            resp.text()
        );
    }
    env.clock.advance(POLL_INTERVAL);
    assert_eq!(
        state(&poll(&env, &started.device_code).await),
        "denied",
        "a denial stays a denial"
    );
    assert_eq!(device_rows(&env), 0);
    // The other way round: an approved grant cannot be denied afterwards.
    let second = start(&env, "Tablet", "android").await;
    assert_eq!(approve(&env, &alice, &second.user_code).await.status, 204);
    assert_eq!(deny(&env, &alice, &second.user_code).await.status, 410);
    env.clock.advance(POLL_INTERVAL);
    assert_eq!(state(&poll(&env, &second.device_code).await), "approved");
}

#[tokio::test]
async fn an_expired_grant_is_gone_for_everybody_and_a_code_that_never_existed_cannot_be_told_from_it()
 {
    let env = env();
    open(&env);
    env.member("alice");
    let started = start(&env, "Old phone", "android").await;
    env.clock.advance(GRANT_TTL - 1);
    let alice = env.signed_aged("alice", 0);
    assert_eq!(
        lookup(&env, &alice, &started.user_code).await.status,
        200,
        "still alive one second before the end"
    );
    env.clock.advance(1);
    let expired = poll(&env, &started.device_code).await;
    assert_eq!(
        (expired.status.as_u16(), state(&expired).as_str()),
        (200, "expired"),
        "{}",
        expired.text()
    );
    for resp in [
        lookup(&env, &alice, &started.user_code).await,
        approve(&env, &alice, &started.user_code).await,
    ] {
        assert_eq!(
            (resp.status.as_u16(), code(&resp).as_str()),
            (410, "gone"),
            "{}",
            resp.text()
        );
    }
    assert_eq!(grant_rows(&env), 1, "the row stays until the purge");
    // A well-formed device code nobody made, and a text that is no device code at all, are answered as the expired one is.
    let never = holzwave_server::device::GrantCode::generate().expect("a code");
    let unknown = poll(&env, never.expose()).await;
    let malformed = poll(&env, "hwg_not-a-code").await;
    assert_eq!(
        shape(&unknown),
        shape(&expired),
        "a code that never existed answers as an expired one"
    );
    assert_eq!(shape(&malformed), shape(&expired));
    // The purge removes the row.
    let now = env.clock.unix();
    assert_eq!(
        env.store
            .write(|tx| tx.tokens_purge(now, 100))
            .expect("purge"),
        1
    );
    assert_eq!(grant_rows(&env), 0);
    assert_eq!(device_rows(&env), 0);
}

#[tokio::test]
async fn a_poll_that_comes_too_soon_is_told_to_slow_down_and_the_interval_it_states_grows_by_five_seconds()
 {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let started = start(&env, "Laptop", "linux").await;
    let first = poll(&env, &started.device_code).await;
    assert_eq!(
        (state(&first).as_str(), first.json()["interval"].as_i64()),
        ("pending", Some(5))
    );
    env.clock.advance(2);
    let hurried = poll(&env, &started.device_code).await;
    assert_eq!(
        (
            hurried.status.as_u16(),
            state(&hurried).as_str(),
            hurried.json()["interval"].as_i64()
        ),
        (200, "slow_down", Some(10)),
        "{}",
        hurried.text()
    );
    // Ten seconds from the hurried poll is the stated interval: it is answered as a poll in time.
    env.clock.advance(9);
    let still_early = poll(&env, &started.device_code).await;
    assert_eq!(
        (
            state(&still_early).as_str(),
            still_early.json()["interval"].as_i64()
        ),
        ("slow_down", Some(15)),
        "{}",
        still_early.text()
    );
    env.clock.advance(15);
    let on_time = poll(&env, &started.device_code).await;
    assert_eq!(
        (
            state(&on_time).as_str(),
            on_time.json()["interval"].as_i64()
        ),
        ("pending", Some(15))
    );
    // A poll that is too soon is not told the decision, whichever it is: the approval comes with the first poll in time.
    assert_eq!(approve(&env, &alice, &started.user_code).await.status, 204);
    env.clock.advance(14);
    assert_eq!(
        state(&poll(&env, &started.device_code).await),
        "slow_down",
        "an approved grant waits for a poll in time"
    );
    assert_eq!(device_rows(&env), 0);
    env.clock.advance(20);
    assert_eq!(state(&poll(&env, &started.device_code).await), "approved");
}

#[tokio::test]
async fn the_token_is_delivered_once_and_the_grant_is_gone() {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let granted = grant_flow(&env, &alice, "Desktop").await;
    assert_eq!(grant_rows(&env), 0, "the row of the grant is gone");
    assert_eq!(device_rows(&env), 1);
    env.clock.advance(POLL_INTERVAL);
    let again = poll(&env, &granted.device_code).await;
    assert_eq!(
        (again.status.as_u16(), state(&again).as_str()),
        (200, "expired"),
        "{}",
        again.text()
    );
    assert!(
        !again.text().contains(&granted.token),
        "the token is not in the answer a second time"
    );
    assert_eq!(device_rows(&env), 1, "and no second device was made");
    assert_eq!(
        approve(&env, &alice, &granted.user_code).await.status,
        410,
        "the user code is spent too"
    );
    assert_eq!(
        env.send(bearer_get(ME, &granted.token)).await.status,
        200,
        "the one token keeps working"
    );
}

// ---------------------------------------------------------------------------------------------------------------------
// The throttles and the caps
// ---------------------------------------------------------------------------------------------------------------------

const NOBODY_MADE: &str = "BCDF-GHJK";

fn peer(last: u8) -> IpAddr {
    IpAddr::from([198, 51, 100, last])
}

async fn lookup_from(env: &ApiEnv, who: &edge::Signed, code: &str, from: IpAddr) -> Resp {
    env.send(device_env::browser_post_from(
        env,
        LOOKUP,
        &json!({ "code": code }),
        who,
        from,
    ))
    .await
}

#[tokio::test]
async fn wrong_codes_lock_the_account_and_the_address_and_a_success_clears_nothing() {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let bob = env.member("bob");
    let started = start_from(&env, "Laptop", "linux", Some(peer(200))).await;
    let rule = holzwave_server::device::GRANT_FAILURES;
    assert_eq!(
        (rule.limit, rule.window_secs),
        (5, 600),
        "five wrong codes in ten minutes"
    );

    // Alice: four wrong codes from one address; bob's success in between clears nothing of hers.
    for _ in 0..4 {
        let resp = lookup_from(&env, &alice, NOBODY_MADE, peer(1)).await;
        assert_eq!((resp.status.as_u16(), code(&resp).as_str()), (410, "gone"));
    }
    assert_eq!(
        lookup_from(&env, &bob, &started.user_code, peer(2))
            .await
            .status,
        200,
        "a person with no wrong code is served"
    );
    let fifth = lookup_from(&env, &alice, NOBODY_MADE, peer(1)).await;
    assert_eq!(
        fifth.status, 410,
        "the fifth wrong code is answered as the others"
    );
    // The sixth try is refused, with the time to wait, and so is a correct code.
    let sixth = lookup_from(&env, &alice, NOBODY_MADE, peer(1)).await;
    assert_eq!(
        (sixth.status.as_u16(), code(&sixth).as_str()),
        (429, "throttled"),
        "{}",
        sixth.text()
    );
    let wait = sixth.json()["retry_seconds"]
        .as_u64()
        .expect("a retry time");
    assert!((1..=600).contains(&wait), "{wait}");
    assert_eq!(sixth.header("retry-after"), Some(wait.to_string()));
    let correct = lookup_from(&env, &alice, &started.user_code, peer(1)).await;
    assert_eq!(correct.status, 429, "the lock holds for a correct code");
    // The account key: alice from another address is locked too.
    assert_eq!(
        lookup_from(&env, &alice, &started.user_code, peer(3))
            .await
            .status,
        429
    );
    // The address key: bob, who has no wrong code, from alice's address is locked too.
    assert_eq!(
        lookup_from(&env, &bob, &started.user_code, peer(1))
            .await
            .status,
        429
    );
    // Approving and denying are under the same lock.
    for path in [APPROVE, DENY] {
        let resp = env
            .send(device_env::browser_post_from(
                &env,
                path,
                &json!({ "code": started.user_code }),
                &alice,
                peer(3),
            ))
            .await;
        assert_eq!(resp.status, 429, "{path}");
    }
    assert_eq!(
        state(&poll(&env, &started.device_code).await),
        "pending",
        "the grant was not touched"
    );
    // Bob from his own address is not affected, and the window passes.
    assert_eq!(
        lookup_from(&env, &bob, &started.user_code, peer(2))
            .await
            .status,
        200
    );
    env.clock.advance(wait as i64 + 1);
    let alice = env.signed_aged("alice", 0);
    let later = start_from(&env, "Laptop", "linux", Some(peer(201))).await;
    assert_eq!(
        lookup_from(&env, &alice, &later.user_code, peer(1))
            .await
            .status,
        200,
        "after the window codes are read again"
    );
}

#[tokio::test]
async fn unknown_device_codes_lock_the_address_of_an_anonymous_poll() {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let started = start_from(&env, "Laptop", "linux", Some(peer(9))).await;
    let unknown = holzwave_server::device::GrantCode::generate().expect("a code");
    for n in 0..5 {
        let resp = poll_from(&env, unknown.expose(), peer(50)).await;
        assert_eq!(
            (resp.status.as_u16(), state(&resp).as_str()),
            (200, "expired"),
            "poll {n}"
        );
    }
    let sixth = poll_from(&env, unknown.expose(), peer(50)).await;
    assert_eq!(
        (sixth.status.as_u16(), code(&sixth).as_str()),
        (429, "throttled"),
        "{}",
        sixth.text()
    );
    assert!(
        sixth.json()["retry_seconds"]
            .as_u64()
            .is_some_and(|s| s >= 1)
    );
    // A real code from that address waits too; another address, and the person's own lookups from that address, do not.
    assert_eq!(
        poll_from(&env, &started.device_code, peer(50)).await.status,
        429
    );
    assert_eq!(
        state(&poll_from(&env, &started.device_code, peer(51)).await),
        "pending"
    );
    assert_eq!(
        lookup_from(&env, &alice, &started.user_code, peer(50))
            .await
            .status,
        200,
        "the counts of the two doors never meet"
    );
    env.clock.advance(601);
    let after = poll_from(&env, unknown.expose(), peer(50)).await;
    assert_eq!(
        (after.status.as_u16(), state(&after).as_str()),
        (200, "expired"),
        "the window passes: the address is answered again"
    );
}

#[tokio::test]
async fn open_grants_are_capped_per_ipv6_48_whatever_the_64_they_come_from() {
    let env = env();
    open(&env);
    assert_eq!(MAX_PENDING_PER_NET, 20);
    let body = device_env::start_body("Laptop", "linux");
    let in_net = |sub: u16| IpAddr::from([0x2001, 0x0db8, 0x00aa, sub, 0, 0, 0, 1]);
    // Five /64s of one /48: the first four fill the network with five each, the fifth /64 is refused although it has none.
    for sub in 0..4u16 {
        for _ in 0..MAX_PENDING_PER_ADDR {
            let resp = env.send(native_post_from(device_env::CODE, &body, in_net(sub))).await;
            assert_eq!(resp.status, 200, "{}", resp.text());
        }
    }
    assert_eq!(grant_rows(&env), MAX_PENDING_PER_NET as i64);
    let refused = env.send(native_post_from(device_env::CODE, &body, in_net(0x00ff))).await;
    assert_eq!((refused.status.as_u16(), code(&refused).as_str()), (429, "throttled"), "{}", refused.text());
    assert_eq!(grant_rows(&env), MAX_PENDING_PER_NET as i64, "the refused start stored nothing");
    // The next /48 and an IPv4 address are not held back.
    let other = IpAddr::from([0x2001, 0x0db8, 0x00ab, 0, 0, 0, 0, 1]);
    assert_eq!(env.send(native_post_from(device_env::CODE, &body, other)).await.status, 200);
    assert_eq!(env.send(native_post_from(device_env::CODE, &body, peer(9))).await.status, 200);
    // When the grants run out, the network may start again.
    env.clock.advance(GRANT_TTL + 1);
    assert_eq!(env.send(native_post_from(device_env::CODE, &body, in_net(0x00ff))).await.status, 200);
}

#[tokio::test]
async fn open_grants_are_capped_per_address_and_for_the_whole_server_and_the_counts_drop_with_the_expiry()
 {
    let env = env();
    open(&env);
    assert_eq!((MAX_PENDING_PER_ADDR, MAX_PENDING), (5, 1_000));
    let body = device_env::start_body("Laptop", "linux");
    for _ in 0..MAX_PENDING_PER_ADDR {
        assert_eq!(
            env.send(native_post_from(device_env::CODE, &body, peer(1)))
                .await
                .status,
            200
        );
    }
    let sixth = env
        .send(native_post_from(device_env::CODE, &body, peer(1)))
        .await;
    assert_eq!(
        (sixth.status.as_u16(), code(&sixth).as_str()),
        (429, "throttled"),
        "{}",
        sixth.text()
    );
    assert!(
        sixth.json()["retry_seconds"]
            .as_u64()
            .is_some_and(|s| s >= 1)
    );
    assert_eq!(
        sixth.header("retry-after"),
        sixth.json()["retry_seconds"]
            .as_u64()
            .map(|s| s.to_string())
    );
    assert_eq!(grant_rows(&env), 5, "the refused start stored nothing");
    // The whole server: 995 more from 199 other addresses fill it; the 1001st is refused from a new address.
    for address in 0..199u8 {
        for _ in 0..MAX_PENDING_PER_ADDR {
            let resp = env
                .send(native_post_from(
                    device_env::CODE,
                    &body,
                    IpAddr::from([203, 0, 113, address + 1]),
                ))
                .await;
            assert_eq!(resp.status, 200, "{}", resp.text());
        }
    }
    assert_eq!(grant_rows(&env), 1_000);
    let full = env
        .send(native_post_from(
            device_env::CODE,
            &body,
            IpAddr::from([192, 0, 2, 250]),
        ))
        .await;
    assert_eq!(
        (full.status.as_u16(), code(&full).as_str()),
        (429, "throttled"),
        "{}",
        full.text()
    );
    assert_eq!(grant_rows(&env), 1_000);
    // After the expiry the counts drop (the rows are still there until the purge) and a start works again.
    env.clock.advance(GRANT_TTL + 1);
    assert_eq!(
        env.send(native_post_from(device_env::CODE, &body, peer(1)))
            .await
            .status,
        200
    );
    assert_eq!(
        env.send(native_post_from(
            device_env::CODE,
            &body,
            IpAddr::from([192, 0, 2, 250])
        ))
        .await
        .status,
        200
    );
}

#[tokio::test]
async fn a_bad_start_is_refused_and_stores_nothing() {
    let env = env();
    open(&env);
    let cases = [
        json!({}),
        json!({ "name": "Laptop" }),
        json!({ "platform": "linux" }),
        json!({ "name": "Laptop", "platform": "plan9" }),
        json!({ "name": "Laptop", "platform": "Linux" }),
        json!({ "name": 7, "platform": "linux" }),
        json!({ "name": "Laptop", "platform": "linux", "extra": true }),
        json!({ "name": "\u{7}\u{0}", "platform": "linux" }),
        json!({ "name": "", "platform": "linux" }),
        json!(["Laptop", "linux"]),
    ];
    for body in cases {
        let resp = env
            .send(device_env::native_post(device_env::CODE, &body))
            .await;
        assert_eq!(
            (resp.status.as_u16(), code(&resp).as_str()),
            (400, "bad_request"),
            "{body}: {}",
            resp.text()
        );
    }
    assert_eq!(grant_rows(&env), 0);
    // A long name with control characters is cleaned and cut to 64 characters.
    let long = format!("{}\u{7}{}", "n".repeat(70), "x");
    let started = env
        .send(device_env::native_post(
            device_env::CODE,
            &device_env::start_body(&long, "other"),
        ))
        .await;
    assert_eq!(started.status, 200, "{}", started.text());
    let alice = env.member("alice");
    let seen = lookup(
        &env,
        &alice,
        started.json()["user_code"].as_str().expect("a code"),
    )
    .await;
    assert_eq!(seen.json()["name"], "n".repeat(64));
    // The poll wants a string and nothing else.
    for body in [
        json!({}),
        json!({ "device_code": 7 }),
        json!({ "device_code": "x", "more": 1 }),
    ] {
        let resp = env
            .send(device_env::native_post(device_env::TOKEN, &body))
            .await;
        assert_eq!(
            (resp.status.as_u16(), code(&resp).as_str()),
            (400, "bad_request"),
            "{body}"
        );
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Who may approve
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_approval_by_a_disabled_or_deleted_account_a_stale_sign_in_or_nobody_changes_nothing() {
    let env = env();
    open(&env);
    let started = start(&env, "Laptop", "linux").await;
    let body = json!({ "code": started.user_code });
    let untouched = |env: &ApiEnv, devices: i64| {
        assert_eq!(
            grant_rows(env),
            1,
            "the grant of the test is the only one and is still there"
        );
        assert_eq!(device_rows(env), devices, "no device was made");
    };

    // A stale sign-in: 403 with the hint that signing in again helps.
    let stale = device_env::stale_member(&env, "dora");
    let resp = env.send(env.post_json(APPROVE, &body, &stale)).await;
    assert_eq!(
        (
            resp.status.as_u16(),
            code(&resp).as_str(),
            resp.json()["step_up"].clone()
        ),
        (403, "forbidden", json!(true)),
        "{}",
        resp.text()
    );
    let resp = env.send(env.post_json(DENY, &body, &stale)).await;
    assert_eq!(resp.status, 403);
    untouched(&env, 0);

    // Nobody: no session at all is 401, with the Origin a page would send.
    let resp = env.send(env.post(APPROVE, &body)).await;
    assert_eq!(
        (resp.status.as_u16(), code(&resp).as_str()),
        (401, "invalid"),
        "{}",
        resp.text()
    );
    // A bearer token is not a session.
    let alice = env.member("alice");
    let granted = grant_flow(&env, &alice, "Other laptop").await;
    let resp = env
        .send(
            device_env::bearer(axum::http::Method::POST, APPROVE, &granted.token)
                .header("origin", ORIGIN)
                .header("content-type", "application/json")
                .json(&body),
        )
        .await;
    assert_eq!(resp.status, 401, "{}", resp.text());
    let devices = device_rows(&env);
    assert_eq!(devices, 1, "the device of the other grant is the only one");
    untouched(&env, devices);

    // A disabled account and a deleted one: their sessions end, and the grant is not decided.
    for (name, state) in [
        ("carol", AccountState::Disabled),
        ("dave", AccountState::Deleted),
    ] {
        let who = env.member(name);
        let id = account_of(&env, name);
        match state {
            AccountState::Deleted => env
                .store
                .write(|tx| tx.account_delete(id))
                .expect("delete the account"),
            _ => env
                .store
                .write(|tx| tx.account_set_state(id, state))
                .expect("set the state"),
        }
        let resp = env.send(env.post_json(APPROVE, &body, &who)).await;
        assert_eq!(
            (resp.status.as_u16(), code(&resp).as_str()),
            (401, "invalid"),
            "{name}: {}",
            resp.text()
        );
    }
    untouched(&env, devices);
    assert_eq!(state(&poll(&env, &started.device_code).await), "pending");
    // Even at the store, an inactive account cannot approve.
    let carol = account_of(&env, "carol");
    let grant = env
        .store
        .read(|tx| {
            tx.grant_by_device_code(
                &holzwave_server::device::GrantCode::parse(&started.device_code)
                    .expect("code")
                    .hash(),
            )
        })
        .expect("read")
        .expect("the grant");
    assert!(
        !env.store
            .write(|tx| tx.grant_approve(grant.id, carol, env.clock.unix()))
            .expect("write")
    );
    untouched(&env, devices);
}

#[tokio::test]
async fn the_21st_device_of_an_account_is_refused_at_delivery_and_the_grant_waits() {
    let env = env();
    open(&env);
    let alice = env.member("alice");
    let account = account_of(&env, "alice");
    let mut first = None;
    for n in 0..MAX_DEVICES_PER_ACCOUNT {
        let granted = grant_flow(&env, &alice, &format!("Device {n}")).await;
        first.get_or_insert(granted.device_id);
        env.clock.advance(1);
    }
    assert_eq!(devices_of(&env, account).len(), 20);
    let started = start(&env, "One too many", "linux").await;
    assert_eq!(approve(&env, &alice, &started.user_code).await.status, 204);
    env.clock.advance(POLL_INTERVAL);
    let refused = poll(&env, &started.device_code).await;
    assert_eq!(
        (refused.status.as_u16(), state(&refused).as_str()),
        (200, "too_many"),
        "{}",
        refused.text()
    );
    assert!(refused.json().get("token").is_none(), "{}", refused.text());
    assert_eq!(devices_of(&env, account).len(), 20);
    assert_eq!(
        grant_rows(&env),
        1,
        "the grant stays, approved, until it expires or a device is revoked"
    );
    // Once a device is gone, the next poll in time delivers.
    assert!(
        env.store
            .write(|tx| tx.device_revoke(account, first.expect("a device")))
            .expect("revoke")
    );
    env.clock.advance(POLL_INTERVAL);
    let delivered = poll(&env, &started.device_code).await;
    assert_eq!(state(&delivered), "approved", "{}", delivered.text());
    assert_eq!(devices_of(&env, account).len(), 20);
    assert_eq!(grant_rows(&env), 0);
}

// ---------------------------------------------------------------------------------------------------------------------
// Who may approve: a sign-in by passkey or by single sign-on approves like one by password
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_person_signed_in_with_a_passkey_or_through_single_sign_on_approves_a_device_that_then_works() {
    use holzwave_server::store::{Role, SignInMethod};
    let env = env();
    open(&env);
    let (by_passkey, _key) = env.member_with_passkey("pia");
    assert_eq!(by_passkey.session.method, SignInMethod::Passkey);
    env.bare("olaf", Role::Member);
    let by_oidc = env.signed_with("olaf", 0, SignInMethod::Oidc);
    assert_eq!(by_oidc.session.method, SignInMethod::Oidc);

    for (who, name) in [(&by_passkey, "pia"), (&by_oidc, "olaf")] {
        let granted = grant_flow(&env, who, "Phone").await;
        let me = env.send(bearer_get(ME, &granted.token)).await;
        assert_eq!(me.status, 200, "the token approved by {name} works: {}", me.text());
        let mine = devices_of(&env, account_of(&env, name));
        assert!(mine.iter().any(|d| d.id == granted.device_id), "the device belongs to {name}");
    }
}
