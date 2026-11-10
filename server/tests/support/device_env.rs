//! The rig of the device grant and of the device lists: the switches, the requests of a native client (no cookie, no `Origin`),
//! the requests that carry a bearer token, the whole grant in one call, and readers of what the store holds.
//!
//! It sits on top of the environment of the account routes. The test file declares `mod edge;`, `mod auth_env;`, `mod api_env;`
//! and `mod device_env;` at its root (all with `#[path]`).
#![allow(dead_code)]

use std::net::IpAddr;

use axum::http::Method;
use holzwave_server::clock::Clock as _;
use holzwave_server::device::DEVICE_PREFIX;
use holzwave_server::http::features::Feature;
use holzwave_server::store::{AccountId, Device};
use serde_json::{Value, json};

use crate::api_env::ApiEnv;
use crate::edge::{HOST, ORIGIN, Req, Resp, Signed};

/// The path of every route of the grant and of the lists.
pub const CODE: &str = "/api/device/code";
pub const TOKEN: &str = "/api/device/token";
pub const LOOKUP: &str = "/api/device/lookup";
pub const APPROVE: &str = "/api/device/approve";
pub const DENY: &str = "/api/device/deny";
pub const ME: &str = "/api/device/me";
pub const MY_DEVICES: &str = "/api/me/devices";
pub const ALL_DEVICES: &str = "/api/admin/devices";

/// Turns a switch on through the store, the way an administrator's change does.
pub fn turn_on(env: &ApiEnv, feature: Feature) {
    assert!(
        env.app
            .features
            .set(feature, true)
            .expect("the switch is written")
    );
}

/// Turns the remote switch on: the one the whole grant needs (either would do).
pub fn open(env: &ApiEnv) {
    turn_on(env, Feature::Remote);
}

// ---- requests ------------------------------------------------------------------------------------------------------------------

/// A request of a native client: the public host and nothing a browser would add.
pub fn native(method: Method, path: &str) -> Req {
    Req::new(method, path).header("host", HOST)
}

/// A native `POST` with a JSON body.
pub fn native_post(path: &str, body: &Value) -> Req {
    native(Method::POST, path)
        .header("content-type", "application/json")
        .json(body)
}

/// A native `POST` that comes from `peer`.
pub fn native_post_from(path: &str, body: &Value, peer: IpAddr) -> Req {
    native_post(path, body).peer(peer)
}

/// A native request with a bearer token.
pub fn bearer(method: Method, path: &str, token: &str) -> Req {
    native(method, path).header("authorization", &format!("Bearer {token}"))
}

/// A native `GET` with a bearer token.
pub fn bearer_get(path: &str, token: &str) -> Req {
    bearer(Method::GET, path, token)
}

/// A browser request that changes something, with the session and the token the page sends, from `peer`.
pub fn browser_post_from(
    env: &ApiEnv,
    path: &str,
    body: &Value,
    who: &Signed,
    peer: IpAddr,
) -> Req {
    env.post_json(path, body, who).peer(peer)
}

/// The members of a body of a start of a grant.
pub fn start_body(name: &str, platform: &str) -> Value {
    json!({ "name": name, "platform": platform })
}

// ---- the grant, step by step -----------------------------------------------------------------------------------------------------

/// What the start of a grant answered.
#[derive(Debug, Clone)]
pub struct Started {
    pub device_code: String,
    pub user_code: String,
    pub uri: String,
    pub complete: String,
    pub expires_in: i64,
    pub interval: i64,
}

/// Starts a grant as an anonymous native client and checks the answer's shape.
pub async fn start(env: &ApiEnv, name: &str, platform: &str) -> Started {
    start_from(env, name, platform, None).await
}

/// Starts a grant from a given address.
pub async fn start_from(env: &ApiEnv, name: &str, platform: &str, peer: Option<IpAddr>) -> Started {
    let request = native_post(CODE, &start_body(name, platform));
    let resp = env
        .send(match peer {
            Some(ip) => request.peer(ip),
            None => request,
        })
        .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    Started {
        device_code: body["device_code"]
            .as_str()
            .expect("a device code")
            .to_owned(),
        user_code: body["user_code"].as_str().expect("a user code").to_owned(),
        uri: body["verification_uri"]
            .as_str()
            .expect("an address")
            .to_owned(),
        complete: body["verification_uri_complete"]
            .as_str()
            .expect("the complete address")
            .to_owned(),
        expires_in: body["expires_in"].as_i64().expect("an expiry"),
        interval: body["interval"].as_i64().expect("an interval"),
    }
}

/// A poll of the token route.
pub async fn poll(env: &ApiEnv, device_code: &str) -> Resp {
    env.send(native_post(TOKEN, &json!({ "device_code": device_code })))
        .await
}

/// A poll from a given address.
pub async fn poll_from(env: &ApiEnv, device_code: &str, peer: IpAddr) -> Resp {
    env.send(native_post_from(
        TOKEN,
        &json!({ "device_code": device_code }),
        peer,
    ))
    .await
}

/// A lookup, an approval or a refusal of a code by a person.
pub async fn decide(env: &ApiEnv, path: &str, who: &Signed, code: &str) -> Resp {
    env.send(env.post_json(path, &json!({ "code": code }), who))
        .await
}

pub async fn lookup(env: &ApiEnv, who: &Signed, code: &str) -> Resp {
    decide(env, LOOKUP, who, code).await
}

pub async fn approve(env: &ApiEnv, who: &Signed, code: &str) -> Resp {
    decide(env, APPROVE, who, code).await
}

pub async fn deny(env: &ApiEnv, who: &Signed, code: &str) -> Resp {
    decide(env, DENY, who, code).await
}

/// What a whole grant ended with.
#[derive(Debug, Clone)]
pub struct Granted {
    pub token: String,
    pub device_id: i64,
    pub device_code: String,
    pub user_code: String,
}

/// The whole flow for `who`: start, approve, poll. The token that comes back has the shape of a device token.
pub async fn grant_flow(env: &ApiEnv, who: &Signed, name: &str) -> Granted {
    let started = start(env, name, "linux").await;
    let approved = approve(env, who, &started.user_code).await;
    assert_eq!(approved.status, 204, "{}", approved.text());
    let polled = poll(env, &started.device_code).await;
    assert_eq!(polled.status, 200, "{}", polled.text());
    let body = polled.json();
    assert_eq!(body["state"], "approved", "{}", polled.text());
    let token = body["token"].as_str().expect("a token").to_owned();
    assert!(
        token.starts_with(DEVICE_PREFIX) && token.len() == DEVICE_PREFIX.len() + 43,
        "the token has its shape"
    );
    Granted {
        token,
        device_id: body["device_id"].as_i64().expect("a device id"),
        device_code: started.device_code,
        user_code: started.user_code,
    }
}

// ---- what the store holds -------------------------------------------------------------------------------------------------------------

/// The devices of an account that have not expired, newest first.
pub fn devices_of(env: &ApiEnv, account: AccountId) -> Vec<Device> {
    env.store
        .read(|tx| tx.devices_of(account, env.clock.unix()))
        .expect("list the devices")
}

/// The number of device rows of all accounts, expired or not.
pub fn device_rows(env: &ApiEnv) -> i64 {
    env.count_rows("SELECT COUNT(*) FROM devices")
}

/// The number of grant rows, open or not.
pub fn grant_rows(env: &ApiEnv) -> i64 {
    env.count_rows("SELECT COUNT(*) FROM device_grants")
}

/// A member whose sign-in is older than the step-up time: a session that may read but not change anything sensitive.
pub fn stale_member(env: &ApiEnv, name: &str) -> Signed {
    env.bare(name, holzwave_server::store::Role::Member);
    env.signed_aged(name, crate::api_env::SIX_MINUTES)
}

/// The account of a name.
pub fn account_of(env: &ApiEnv, name: &str) -> AccountId {
    env.store
        .account_by_username(name)
        .expect("read")
        .expect("the account exists")
        .id
}

/// The code of an answer.
pub fn code(resp: &Resp) -> String {
    resp.json()["code"].as_str().unwrap_or_default().to_owned()
}

/// Everything of an answer that must not tell two answers apart, without the date.
pub fn shape(resp: &Resp) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut headers: Vec<(String, String)> = resp
        .headers
        .iter()
        .filter(|(name, _)| name.as_str() != "date")
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    headers.sort();
    (resp.status.as_u16(), headers, resp.body.clone())
}

/// A native request that no route answers, to compare the answer of a closed feature with.
pub fn absent() -> Req {
    native(Method::POST, "/api/device/no-such-route")
        .header("content-type", "application/json")
        .json(&json!({}))
}

/// The origin tests expect in an address the server builds.
pub const SERVER_ORIGIN: &str = ORIGIN;

// ---- a real socket ----------------------------------------------------------------------------------------------------------------

/// The router of an environment served on `127.0.0.1` over plain http, for a client that is the real one (the core's, which
/// opens a connection of its own). The public host of the test application is not the loopback address, so each request's
/// `Host` is set to the public one before the router sees it; everything else of a request reaches the router as it came.
///
/// Dropping the value stops the listener and every connection it accepted, so a later request is refused by the system.
pub struct Loopback {
    /// `http://127.0.0.1:<port>`: the address a client is pointed at.
    pub origin: String,
    task: tokio::task::JoinHandle<()>,
    requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Loopback {
    /// Stops the listener and every connection, and returns once they are gone.
    pub async fn stop(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }

    /// How many requests reached the router.
    pub fn requests(&self) -> usize {
        self.requests.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Serves the router of `env` on a free loopback port.
pub async fn serve_loopback(env: &ApiEnv) -> Loopback {
    use hyper::body::Incoming;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use tower::ServiceExt as _;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("the address").port();
    let router = env.router();
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = requests.clone();
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let Ok((stream, peer)) = listener.accept().await else {
                break;
            };
            let router = router.clone();
            let counted = counted.clone();
            connections.spawn(async move {
                let service = service_fn(move |request: hyper::Request<Incoming>| {
                    let router = router.clone();
                    let counted = counted.clone();
                    async move {
                        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let (mut parts, body) = request.into_parts();
                        parts.headers.insert(
                            axum::http::header::HOST,
                            axum::http::HeaderValue::from_static(HOST),
                        );
                        parts.extensions.insert(
                            holzwave_server::http::router::PeerAddr(peer.ip()),
                        );
                        let request =
                            axum::http::Request::from_parts(parts, axum::body::Body::new(body));
                        router.oneshot(request).await
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
            while connections.try_join_next().is_some() {}
        }
    });
    Loopback {
        origin: format!("http://127.0.0.1:{port}"),
        task,
        requests,
    }
}
