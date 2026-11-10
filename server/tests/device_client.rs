//! The core's grant client against the real server: the real router and store behind a real socket on the loopback address, and
//! the client that the apps use, from the code to the token to Sign out. Then every refusal the client can meet (a feature that
//! is off, a server that is not there, a server that fails) and the proof that the token and the device code are in no file the
//! server writes and in no error the client shows.
//!
//! The server's clock is the environment's manual one, so the server never waits; the client's own pace is driven with explicit
//! instants, so no test sleeps.

#![cfg(feature = "in-process-workers")]

#[path = "support/api_env.rs"]
mod api_env;
#[path = "support/auth_env.rs"]
mod auth_env;
#[path = "support/device_env.rs"]
mod device_env;
#[path = "support/edge.rs"]
mod edge;
#[path = "support/sweep.rs"]
mod sweep;

use std::time::{Duration, Instant};

use api_env::ApiEnv;
use device_env::{
    account_of, approve, deny, devices_of, grant_rows, open, serve_loopback, turn_on,
};
use holzwave_core::server_link::{Fault, Token};
use holzwave_core::server_signin::{
    Client, Grant, Me, Poll, SignInError, me, revoke_self, start,
};
use holzwave_server::device::{GRANT_TTL, POLL_INTERVAL};
use holzwave_server::http::features::Feature;
use sweep::{Canary, Category, Counts, Sweep};

/// Runs a blocking call of the client on a thread that may block, while the server goes on in this test's runtime.
async fn client<T: Send + 'static>(call: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(call)
        .await
        .expect("the client's thread")
}

/// One poll at `later` seconds after the start of the test's clock, on the client's thread.
async fn poll_after(
    origin: &str,
    mut grant: Grant,
    t0: Instant,
    later: u64,
) -> (Grant, Result<Poll, SignInError>) {
    let origin = origin.to_owned();
    client(move || {
        let client = Client::new(&origin).expect("a client");
        let result = client.poll_at(&mut grant, t0 + Duration::from_secs(later));
        (grant, result)
    })
    .await
}

fn user_code_shape(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 9
        && bytes[4] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || b"BCDFGHJKLMNPQRSTVWXZ".contains(b))
}

// ---------------------------------------------------------------------------------------------------------------------
// The whole grant
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracer_the_client_signs_in_at_the_real_server_reads_who_it_is_and_signs_out() {
    let env = ApiEnv::new();
    open(&env);
    let alice = env.member("alice");
    let account = account_of(&env, "alice");
    let lb = serve_loopback(&env).await;
    let origin = lb.origin.clone();

    // 1. The start: a code to show and the address to open, which is built from the origin the client was pointed at (the
    //    server states its own public address, which is another one in this rig, and it is not used).
    let t0 = Instant::now();
    let grant = {
        let origin = origin.clone();
        client(move || start(&origin, "Holz's laptop", "macos")).await
    }
    .expect("a grant");
    assert!(user_code_shape(grant.user_code()), "{}", grant.user_code());
    assert_eq!(
        grant.open_url(),
        format!("{origin}/#device={}", grant.user_code())
    );
    assert!(
        !grant.open_url().contains("hw.example.test"),
        "the server's own address is not what the person is sent to"
    );
    assert_eq!(
        (grant.expires_in(), grant.interval()),
        (GRANT_TTL as u64, POLL_INTERVAL as u64)
    );
    assert_eq!(grant_rows(&env), 1);
    let user_code = grant.user_code().to_owned();
    let requests_after_start = lb.requests();

    // 2. A poll before the server's interval is over is not sent at all.
    let (grant, early) = poll_after(&origin, grant, t0, 1).await;
    assert!(matches!(early.expect("a poll"), Poll::Pending));
    assert_eq!(lb.requests(), requests_after_start, "the client held it back");

    // 3. The first poll that is allowed reaches the server and finds nobody has decided.
    let (grant, pending) = poll_after(&origin, grant, t0, 6).await;
    assert!(matches!(pending.expect("a poll"), Poll::Pending));
    assert_eq!(lb.requests(), requests_after_start + 1);

    // 4. The person approves, in the browser, under their own login (the rig plays the browser).
    let approved = approve(&env, &alice, &user_code).await;
    assert_eq!(approved.status, 204, "{}", approved.text());
    env.clock.advance(POLL_INTERVAL);

    // 5. The next poll that is allowed gets the token, once.
    let (grant, answer) = poll_after(&origin, grant, t0, 12).await;
    let Poll::Approved {
        token,
        device_id,
        expires_at,
    } = answer.expect("a poll")
    else {
        panic!("the grant was approved")
    };
    assert_eq!(format!("{token:?}"), "Token(..)");
    assert!(!expires_at.is_empty());
    assert_eq!(grant_rows(&env), 0, "the grant is used up");
    let devices = devices_of(&env, account);
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id.to_string(), device_id);
    assert_eq!(devices[0].name, "Holz's laptop");
    assert_eq!(devices[0].platform, "macos");
    // A second poll of the same grant never hands a second token out.
    env.clock.advance(POLL_INTERVAL);
    let (_grant, again) = poll_after(&origin, grant, t0, 20).await;
    assert!(matches!(again.expect("a poll"), Poll::Expired), "the grant is gone");

    // 6. The token reads who it is: the device, and the two switches of this server (the remote one is on).
    let (token, who) = {
        let origin = origin.clone();
        client(move || {
            let result = me(&origin, &token);
            (token, result)
        })
        .await
    };
    assert_eq!(
        who.expect("me"),
        Me {
            device_id: device_id.clone(),
            name: "Holz's laptop".into(),
            platform: "macos".into(),
            expires_at: rfc3339_of(devices[0].expires_at),
            sync: false,
            remote: true,
        }
    );

    // 7. Sign out ends it at the server, and the same token is a sign-in that ended.
    let (token, ended) = {
        let origin = origin.clone();
        client(move || {
            let result = revoke_self(&origin, &token);
            (token, result)
        })
        .await
    };
    ended.expect("signed out");
    assert!(devices_of(&env, account).is_empty());
    let (_token, after) = {
        let origin = origin.clone();
        client(move || {
            let result = me(&origin, &token);
            (token, result)
        })
        .await
    };
    assert_eq!(after.expect_err("refused"), SignInError::Fault(Fault::SignedOut));
}

/// The time the server writes for a unix second.
fn rfc3339_of(unix: i64) -> String {
    holzwave_server::clock::rfc3339(holzwave_server::clock::from_unix(unix))
}

// ---------------------------------------------------------------------------------------------------------------------
// The ends of a wait
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_denial_and_an_expiry_come_from_the_servers_own_answers_and_end_the_wait() {
    let env = ApiEnv::new();
    open(&env);
    let alice = env.member("alice");
    let lb = serve_loopback(&env).await;
    let origin = lb.origin.clone();
    let t0 = Instant::now();

    // Denied.
    let grant = {
        let origin = origin.clone();
        client(move || start(&origin, "Phone", "android")).await
    }
    .expect("a grant");
    let denied = deny(&env, &alice, grant.user_code()).await;
    assert_eq!(denied.status, 204, "{}", denied.text());
    let (_grant, answer) = poll_after(&origin, grant, t0, 6).await;
    assert!(matches!(answer.expect("a poll"), Poll::Denied));
    assert!(devices_of(&env, device_env::account_of(&env, "alice")).is_empty());

    // Expired: the server's clock passes the life of the grant.
    let grant = {
        let origin = origin.clone();
        client(move || start(&origin, "Phone", "android")).await
    }
    .expect("a grant");
    env.clock.advance(GRANT_TTL);
    let (_grant, answer) = poll_after(&origin, grant, t0, 6).await;
    assert!(matches!(answer.expect("a poll"), Poll::Expired));

    // A grant the server does not know at all (its rows are gone) reads as an expired one.
    let grant = {
        let origin = origin.clone();
        client(move || start(&origin, "Phone", "android")).await
    }
    .expect("a grant");
    rusqlite::Connection::open(env.store.path())
        .expect("a second connection")
        .execute("DELETE FROM device_grants", [])
        .expect("the grants are removed");
    let (_grant, answer) = poll_after(&origin, grant, t0, 6).await;
    assert!(matches!(answer.expect("a poll"), Poll::Expired));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_poll_that_comes_too_soon_for_the_server_makes_the_interval_longer_and_the_client_then_waits_it() {
    let env = ApiEnv::new();
    open(&env);
    let _alice = env.member("alice");
    let lb = serve_loopback(&env).await;
    let origin = lb.origin.clone();
    let t0 = Instant::now();
    let grant = {
        let origin = origin.clone();
        client(move || start(&origin, "Phone", "linux")).await
    }
    .expect("a grant");
    assert_eq!(grant.interval(), 5);

    // The first poll is sent and noted by the server at its time 0.
    let (grant, first) = poll_after(&origin, grant, t0, 6).await;
    assert!(matches!(first.expect("a poll"), Poll::Pending));
    // The client's clock lets the next one go, but the server's clock has not moved: the server asks to slow down.
    let sent = lb.requests();
    let (grant, second) = poll_after(&origin, grant, t0, 12).await;
    assert!(matches!(second.expect("a poll"), Poll::SlowDown));
    assert_eq!(lb.requests(), sent + 1);
    assert_eq!(grant.interval(), 10, "the server's own longer interval is the one in force");
    // Seven seconds after the poll that was sent is inside the new interval: not sent.
    let (grant, third) = poll_after(&origin, grant, t0, 19).await;
    assert!(matches!(third.expect("a poll"), Poll::Pending));
    assert_eq!(lb.requests(), sent + 1, "the client kept the longer interval");
    // Ten seconds after it, with the server's clock moved too, the poll goes out and is answered.
    env.clock.advance(20);
    let (grant, fourth) = poll_after(&origin, grant, t0, 22).await;
    assert!(matches!(fourth.expect("a poll"), Poll::Pending));
    assert_eq!(lb.requests(), sent + 2);
    assert_eq!(grant.interval(), 10, "it never went down");
}

// ---------------------------------------------------------------------------------------------------------------------
// Faults
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_both_switches_off_every_call_is_off_a_stopped_server_is_unreachable_and_a_failing_one_is_a_protocol_fault() {
    let env = ApiEnv::new();
    let _alice = env.member("alice");
    let lb = serve_loopback(&env).await;
    let origin = lb.origin.clone();
    // A token with the right shape that the server has never issued.
    let stray = || Token::parse(&format!("hwd_{}", "A".repeat(43))).expect("a token");

    // Both switches off: the server's 404 is the only information, for every call.
    let results = {
        let origin = origin.clone();
        client(move || {
            vec![
                start(&origin, "Phone", "linux").map(|_| ()),
                me(&origin, &stray()).map(|_| ()),
                revoke_self(&origin, &stray()),
            ]
        })
        .await
    };
    for result in results {
        let error = result.expect_err("off");
        assert_eq!(error, SignInError::Fault(Fault::Off));
        assert_eq!(error.to_string(), Fault::Off.sentence());
    }
    assert_eq!(grant_rows(&env), 0, "nothing was made while the switch was off");

    // Turned on, the same token is refused as a sign-in that ended (the server never issued it).
    turn_on(&env, Feature::Sync);
    let refused = {
        let origin = origin.clone();
        client(move || me(&origin, &stray())).await
    };
    assert_eq!(
        refused.expect_err("unknown token"),
        SignInError::Fault(Fault::SignedOut)
    );

    // A server that fails: the answer is not what a server of this kind says.
    open(&env);
    env.break_writes("hw_broken", "INSERT", "device_grants", "");
    let failed = {
        let origin = origin.clone();
        client(move || start(&origin, "Phone", "linux")).await
    };
    let failed = failed.expect_err("a failing server");
    assert_eq!(failed, SignInError::Fault(Fault::Protocol));
    assert_eq!(failed.to_string(), Fault::Protocol.sentence());
    env.repair("hw_broken");

    // The server stopped: nothing is listening.
    lb.stop().await;
    let gone = {
        let origin = origin.clone();
        client(move || start(&origin, "Phone", "linux")).await
    };
    assert_eq!(gone.expect_err("no server"), SignInError::Fault(Fault::Unreachable));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_address_that_is_not_a_server_origin_is_refused_before_a_request_is_made() {
    let env = ApiEnv::new();
    open(&env);
    let lb = serve_loopback(&env).await;
    let port = lb.origin.rsplit(':').next().expect("a port").to_owned();
    let bad = [
        format!("http://localhost.example:{port}"),
        format!("http://127.0.0.1.example:{port}"),
        format!("http://127.0.0.1:{port}/path"),
        format!("http://127.0.0.1:{port}/?x=1"),
        format!("http://user:pw@127.0.0.1:{port}"),
        format!("http://127.0.0.1:{port}/#f"),
        format!("https://{}.example", "a".repeat(600)),
    ];
    let outcome = client(move || {
        bad.iter()
            .map(|address| start(address, "Phone", "linux").map(|_| ()))
            .collect::<Vec<_>>()
    })
    .await;
    for result in outcome {
        let error = result.expect_err("refused");
        assert!(matches!(error, SignInError::Address(_)), "{error:?}");
    }
    assert_eq!(lb.requests(), 0, "no request went out");
    assert_eq!(grant_rows(&env), 0);
}

// ---------------------------------------------------------------------------------------------------------------------
// Custody
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_file_of_the_server_and_no_error_of_the_client_holds_the_token_or_the_device_code() {
    let env = ApiEnv::new();
    open(&env);
    let alice = env.member("alice");
    let lb = serve_loopback(&env).await;
    let origin = lb.origin.clone();
    let t0 = Instant::now();

    // The device code is not readable from a Grant by design; the poll's body carries it, so the test cannot name it. The token
    // is read from the poll's answer and used for every call, then the whole run's error texts are collected.
    let grant = {
        let origin = origin.clone();
        client(move || start(&origin, "Holz's laptop", "linux")).await
    }
    .expect("a grant");
    let user_code = grant.user_code().to_owned();
    assert_eq!(approve(&env, &alice, &user_code).await.status, 204);
    env.clock.advance(POLL_INTERVAL);
    let (_grant, answer) = poll_after(&origin, grant, t0, 6).await;
    let Poll::Approved { token, .. } = answer.expect("a poll") else {
        panic!("approved")
    };
    let token_text = token.bearer().as_str().trim_start_matches("Bearer ").to_owned();

    // Every error text of the run: refused calls with the real token (a closed feature) and with a revoked one.
    let mut errors: Vec<String> = Vec::new();
    {
        let first = origin.clone();
        let (token, revoked) = client(move || {
            let revoked = revoke_self(&first, &token);
            (token, revoked)
        })
        .await;
        revoked.expect("revoked");
        let second = origin.clone();
        let after = client(move || me(&second, &token)).await;
        let error = after.expect_err("refused");
        errors.push(format!("{error} {error:?}"));
    }
    turn_off(&env);
    {
        let origin = origin.clone();
        let error = client(move || start(&origin, "x", "linux"))
            .await
            .expect_err("off");
        errors.push(format!("{error} {error:?}"));
    }

    let canaries = vec![
        Canary::with_text("token", Category::Secret, &token_text),
        Canary::with_text(
            "token body",
            Category::Secret,
            token_text.trim_start_matches("hwd_"),
        ),
    ];
    let sweep = Sweep::new(&canaries);
    let mut counts = Counts::default();
    let root = env.scratch.path().to_path_buf();
    let mut findings = sweep.scan_tree(&root, &mut counts);
    findings.extend(sweep.scan_text("the audit directory", &env.audit_text()));
    for (number, text) in errors.iter().enumerate() {
        findings.extend(sweep.scan_text(&format!("error text {number}"), text));
    }
    assert!(
        counts.files >= 3,
        "the scan read the database and the audit files: {counts:?}"
    );
    assert!(
        findings.is_empty(),
        "{:?}",
        findings
            .iter()
            .map(sweep::Finding::line)
            .collect::<Vec<_>>()
    );
    // The scan is not blind: the token's text in an error text would be found.
    assert!(!sweep
        .scan_text("a planted error", &format!("failed with {token_text}"))
        .is_empty());
    // Both ends printed the account as the person: no user code is in an error either.
    assert!(errors.iter().all(|e| !e.contains(&user_code)));
}

fn turn_off(env: &ApiEnv) {
    for feature in [Feature::Sync, Feature::Remote] {
        env.app.features.set(feature, false).expect("the switch is written");
    }
}
