//! The test application of the HTTP side: a scratch store, a manual clock, an audit directory, the configuration of a small
//! server and the router built from them. The route plans include it with `#[path = "support/edge.rs"] mod edge;`.
//!
//! Requests are built by hand and sent to the router as a `tower` service; no socket is opened.
#![allow(dead_code)]

#[path = "scratch.rs"]
mod scratch;

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::routing::{MethodRouter, get};
use axum::{Extension, Json, Router};
use axum::http::header::{HeaderName, HeaderValue};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use holzwave_server::audit::Audit;
use holzwave_server::auth::session::{Method as SignInMethod, SessionManager};
use holzwave_server::clock::{Clock, ManualClock};
use holzwave_server::config::{Auth, Cidr, Oidc, Sessions};
use holzwave_server::http::app::{App, AppInit, PublicOrigin, StoreState, StoreView};
use holzwave_server::http::router::{self, Access, Admin, ApiJson, ClientAddr, Fresh, MaybeAuthed, Part, PeerAddr, SocketVerdict};
use holzwave_server::keyfile::ServerKey;
use holzwave_server::limits::Limits;
use holzwave_server::proxy::{Mode, TrustedProxies};
use holzwave_server::store::{AccountId, AccountState, NewAccount, Role, Session, Store};
use scratch::Scratch;
use tower::ServiceExt as _;

/// The time at which every test application starts, in unix seconds.
pub const NOW: i64 = 1_772_353_800;

/// The public host of the default test application.
pub const HOST: &str = "hw.example.test";

/// The public origin of the default test application.
pub const ORIGIN: &str = "https://hw.example.test";

/// The range of user ids the test accounts are made in.
pub const UID_RANGE: std::ops::RangeInclusive<u32> = 20000..=29999;

/// The idle, absolute and step-up times of the default configuration, in seconds.
pub const IDLE: i64 = 1_800;
pub const ABSOLUTE: i64 = 86_400;
pub const STEP_UP: i64 = 300;

/// A [`StoreView`] that a test can set: the accounts in the set are locked.
#[derive(Default)]
pub struct TestView {
    locked: Mutex<HashSet<i64>>,
}

impl TestView {
    /// Marks the secret store of `account` as locked or open.
    pub fn set_locked(&self, account: AccountId, locked: bool) {
        let mut set = self.locked.lock().expect("lock");
        if locked {
            set.insert(account.get());
        } else {
            set.remove(&account.get());
        }
    }
}

impl StoreView for TestView {
    fn state(&self, account: AccountId) -> StoreState {
        if self.locked.lock().expect("lock").contains(&account.get()) { StoreState::Locked } else { StoreState::Open }
    }
}

/// How a test application differs from the default one.
pub struct Options {
    /// The `public_url`.
    pub url: String,
    /// Whether the listener is in the proxy mode with `10.0.0.0/8` as the trusted proxies.
    pub proxy: bool,
    /// The routes of the route plan under test.
    pub parts: Vec<Part>,
}

impl Default for Options {
    fn default() -> Options {
        Options { url: ORIGIN.to_owned(), proxy: false, parts: Vec::new() }
    }
}

/// A signed-in person: the cookie value, the session row and the token the page would send.
pub struct Signed {
    /// The value of the session cookie.
    pub cookie: String,
    /// The session row as it was made.
    pub session: Session,
    /// The account.
    pub account: AccountId,
    /// The anti-forgery token.
    pub csrf: String,
}

/// The application under test.
pub struct TestApp {
    _scratch: Scratch,
    pub store: Arc<Store>,
    pub clock: Arc<ManualClock>,
    pub sessions: Arc<SessionManager>,
    pub audit_dir: PathBuf,
    pub view: Arc<TestView>,
    pub app: Arc<App>,
    router: Router,
}

/// The server key every test application uses.
pub fn server_key() -> ServerKey {
    ServerKey::from_bytes([7u8; 32]).expect("a key")
}

fn auth() -> Auth {
    Auth {
        passkey: true,
        password_totp: true,
        oidc: vec![Oidc {
            id: "corp".to_owned(),
            name: "Company".to_owned(),
            issuer: "https://idp.example.test".to_owned(),
            client_id: "client".to_owned(),
            client_secret_file: PathBuf::from("/nonexistent/secret"),
            allowed_domains: Vec::new(),
            allowed_groups: Vec::new(),
            groups_claim: None,
        }],
    }
}

impl TestApp {
    /// The default application: `https://hw.example.test`, no routes but `GET /api/session`.
    pub fn new() -> TestApp {
        TestApp::with(Options::default())
    }

    /// An application mounted with `parts`.
    pub fn with_parts(parts: Vec<Part>) -> TestApp {
        TestApp::with(Options { parts, ..Options::default() })
    }

    /// An application with the given options.
    pub fn with(options: Options) -> TestApp {
        TestApp::with_store_at(options, None, NOW)
    }

    /// A new application over the store of this one, as a server that restarts finds it on disk: the accounts, the
    /// sessions and the devices are kept, everything held in memory is new. The clock starts where this one stands.
    pub fn reopened(&self) -> TestApp {
        TestApp::with_store_at(Options::default(), Some(self._scratch.path()), self.clock.unix())
    }

    fn with_store_at(options: Options, store_dir: Option<&std::path::Path>, now: i64) -> TestApp {
        let scratch = Scratch::new("edge");
        let store = Arc::new(Store::open(store_dir.unwrap_or(scratch.path())).expect("open the store"));
        let clock = Arc::new(ManualClock::new(now));
        let dyn_clock: Arc<dyn Clock> = clock.clone();
        let audit = Arc::new(Audit::open(scratch.path(), dyn_clock.clone()).expect("open the audit log"));
        let sessions = Arc::new(SessionManager::new(store.clone(), dyn_clock.clone(), Sessions::default()).with_audit(audit.clone()));
        let audit_dir = audit.dir().to_path_buf();
        let view = Arc::new(TestView::default());
        let proxies = if options.proxy {
            TrustedProxies::new(&[Cidr::parse("10.0.0.0/8").expect("a network")]).expect("proxies")
        } else {
            TrustedProxies::none()
        };
        let app = App::new(
            AppInit {
                origin: PublicOrigin::parse(&options.url).expect("a public url"),
                auth: auth(),
                sessions: Sessions::default(),
                limits: Limits::DEFAULT,
                mode: if options.proxy { Mode::Proxy } else { Mode::Files },
                proxies,
                store: store.clone(),
                session_manager: sessions.clone(),
                audit,
                clock: dyn_clock,
                view: view.clone(),
                devices: Default::default(),
            },
            &server_key(),
        );
        let router = router::build(app.clone(), options.parts);
        TestApp { _scratch: scratch, store, clock, sessions, audit_dir, view, app, router }
    }

    /// Makes an account.
    pub fn create_account(&self, name: &str, role: Role) -> AccountId {
        let now = self.clock.unix();
        self.store.write(|tx| tx.account_create(&NewAccount::new(name, role), UID_RANGE, now)).expect("account").id
    }

    /// Changes the state of an account.
    pub fn set_state(&self, name: &str, state: AccountState) {
        let account = self.store.account_by_username(name).expect("read").expect("the account exists");
        self.store.write(|tx| tx.account_set_state(account.id, state)).expect("set the state");
    }

    /// Signs the account `name` in now, with the password method.
    pub fn sign_in(&self, name: &str) -> Signed {
        let account = self.store.account_by_username(name).expect("read").expect("the account exists");
        let now = self.clock.unix();
        let created = self
            .sessions
            .create(account.id, SignInMethod::PasswordTotp, now, "Firefox on Linux", Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7))))
            .expect("a session");
        let csrf = self.app.csrf_token(&created.session);
        Signed { cookie: created.value.expose().to_owned(), session: created.session, account: account.id, csrf }
    }

    /// A request with the public `Host` and nothing else.
    pub fn req(&self, method: Method, path: &str) -> Req {
        Req::new(method, path).header("host", self.app.origin.authority())
    }

    /// A request that changes something, as the page sends it: `Host`, `Origin`, JSON content type and an empty object.
    pub fn change(&self, method: Method, path: &str) -> Req {
        self.req(method, path)
            .header("origin", self.app.origin.text())
            .header("content-type", "application/json")
            .bytes(b"{}".to_vec())
    }

    /// A change as a signed-in person sends it: the cookie and the token as well.
    pub fn change_as(&self, method: Method, path: &str, who: &Signed) -> Req {
        self.change(method, path).signed(who)
    }

    /// Sends a request to the router.
    pub async fn send(&self, req: Req) -> Resp {
        let response = self.router.clone().oneshot(req.build()).await.expect("the router never fails");
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.expect("the body can be read");
        Resp { status: parts.status, headers: parts.headers, body: bytes.to_vec() }
    }
}

impl Default for TestApp {
    fn default() -> TestApp {
        TestApp::new()
    }
}

/// A request under construction.
pub struct Req {
    method: Method,
    uri: String,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Vec<u8>,
    chunks: Option<Vec<Vec<u8>>>,
    peer: Option<IpAddr>,
}

/// A body that arrives in several frames and says nothing about its length.
struct Chunks(VecDeque<Bytes>);

impl hyper::body::Body for Chunks {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
        Poll::Ready(self.0.pop_front().map(|bytes| Ok(hyper::body::Frame::data(bytes))))
    }
}

impl Req {
    /// A request with no header at all.
    pub fn new(method: Method, uri: &str) -> Req {
        Req { method, uri: uri.to_owned(), headers: Vec::new(), body: Vec::new(), chunks: None, peer: None }
    }

    /// Adds a header line (an earlier line of the same name stays).
    pub fn header(mut self, name: &str, value: &str) -> Req {
        self.headers.push((HeaderName::from_bytes(name.as_bytes()).expect("a name"), HeaderValue::from_str(value).expect("a value")));
        self
    }

    /// Adds a header line from raw bytes.
    pub fn header_bytes(mut self, name: &str, value: &[u8]) -> Req {
        self.headers
            .push((HeaderName::from_bytes(name.as_bytes()).expect("a name"), HeaderValue::from_bytes(value).expect("a value")));
        self
    }

    /// Replaces every line of that name with one.
    pub fn set(self, name: &str, value: &str) -> Req {
        self.without(name).header(name, value)
    }

    /// Removes every line of that name.
    pub fn without(mut self, name: &str) -> Req {
        let name = HeaderName::from_bytes(name.as_bytes()).expect("a name");
        self.headers.retain(|(n, _)| *n != name);
        self
    }

    /// Sets the request target (for example an absolute URI, as an HTTP/2 request has).
    pub fn uri(mut self, uri: &str) -> Req {
        self.uri = uri.to_owned();
        self
    }

    /// Sets the body.
    pub fn bytes(mut self, body: Vec<u8>) -> Req {
        self.body = body;
        self
    }

    /// Sets the body to several frames, with no declared length.
    pub fn chunks(mut self, chunks: Vec<Vec<u8>>) -> Req {
        self.chunks = Some(chunks);
        self
    }

    /// Sets the body to a JSON value.
    pub fn json(self, value: &serde_json::Value) -> Req {
        self.bytes(serde_json::to_vec(value).expect("json"))
    }

    /// Adds the session cookie.
    pub fn cookie(self, value: &str) -> Req {
        self.header("cookie", &format!("__Host-hw_session={value}"))
    }

    /// Sets the anti-forgery token.
    pub fn csrf(self, token: &str) -> Req {
        self.header("x-csrf-token", token)
    }

    /// Adds the cookie and the token of a signed-in person.
    pub fn signed(self, who: &Signed) -> Req {
        self.cookie(&who.cookie).csrf(&who.csrf)
    }

    /// Sets the address of the peer of the connection.
    pub fn peer(mut self, ip: IpAddr) -> Req {
        self.peer = Some(ip);
        self
    }

    /// The request as the router receives it (for a harness that owns its own router).
    pub fn build(self) -> Request<Body> {
        let mut builder = Request::builder().method(self.method).uri(self.uri);
        for (name, value) in self.headers {
            builder = builder.header(name, value);
        }
        let body = match self.chunks {
            Some(chunks) => Body::new(Chunks(chunks.into_iter().map(Bytes::from).collect())),
            None => Body::from(self.body),
        };
        let mut request = builder.body(body).expect("a request");
        if let Some(ip) = self.peer {
            request.extensions_mut().insert(PeerAddr(ip));
        }
        request
    }
}

/// An answer, read to the end.
pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Resp {
    /// The body as JSON.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("the body is not json ({e}): {:?}", String::from_utf8_lossy(&self.body)))
    }

    /// The body as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The first value of a header.
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers.get(name).map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
    }

    /// Every value of a header.
    pub fn all(&self, name: &str) -> Vec<String> {
        self.headers.get_all(name).iter().map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned()).collect()
    }

    /// The names of the headers, sorted and without repeats.
    pub fn header_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.headers.keys().map(|n| n.as_str().to_owned()).collect();
        names.sort();
        names.dedup();
        names
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Routes to test the access kinds with
// ---------------------------------------------------------------------------------------------------------------------

/// What a probe route saw: the person (if any), the client address, the verdict of a socket part and the size of the body.
async fn probe(
    MaybeAuthed(who): MaybeAuthed,
    Extension(client): Extension<ClientAddr>,
    verdict: Option<Extension<SocketVerdict>>,
    body: Bytes,
) -> Json<serde_json::Value> {
    let debug = format!("{who:?}");
    Json(serde_json::json!({
        "ok": true,
        "user": who.as_ref().map(|a| a.account.username.clone()),
        "role": who.as_ref().map(|a| a.account.role.as_str()),
        "locked": who.as_ref().map(|a| a.locked),
        "client": client.0.to_string(),
        "bytes": body.len(),
        "verdict": verdict.map(|Extension(v)| serde_json::json!({
            "origin_ok": v.origin_ok, "host_ok": v.host_ok, "session": v.session, "allowed": v.allowed(),
        })),
        "debug": debug,
    }))
}

/// A body type that denies unknown members.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Strict {
    name: String,
}

async fn strict(ApiJson(body): ApiJson<Strict>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "name": body.name }))
}

async fn anything(ApiJson(body): ApiJson<serde_json::Value>) -> Json<serde_json::Value> {
    Json(body)
}

async fn admin_only(Admin(who): Admin) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "admin": who.account.username }))
}

async fn fresh_only(Fresh(who): Fresh) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "fresh": who.account.username }))
}

async fn sets_sharing_headers() -> ([(&'static str, &'static str); 2], &'static str) {
    ([("access-control-allow-origin", "*"), ("access-control-allow-credentials", "true")], "ok")
}

fn every_method() -> MethodRouter {
    get(probe).post(probe).put(probe).patch(probe).delete(probe)
}

/// One probe route per access kind: `/t/public`, `/t/session`, `/t/fresh`, `/t/socket` and `/t/upload`, each answering
/// `GET`, `POST`, `PUT`, `PATCH` and `DELETE` with what the handler saw; `/t/cors`, a public route that sets cross-origin
/// sharing headers (the router must remove them); `/t/json` and `/t/value`, public `POST` routes that read their body with
/// `ApiJson`; `/t/admin`, a session route that needs the `Admin` extractor; and `/t/stepup`, a session route that needs
/// the `Fresh` extractor.
pub fn probe_parts() -> Vec<Part> {
    let one = |access: Access, path: &str| Part { access, router: Router::new().route(path, every_method()) };
    vec![
        one(Access::Public, "/t/public"),
        one(Access::Session, "/t/session"),
        one(Access::SessionFresh, "/t/fresh"),
        one(Access::Socket, "/t/socket"),
        one(Access::Upload, "/t/upload"),
        Part { access: Access::Public, router: Router::new().route("/t/cors", get(sets_sharing_headers)) },
        Part {
            access: Access::Public,
            router: Router::new()
                .route("/t/json", axum::routing::post(strict))
                .route("/t/value", axum::routing::post(anything)),
        },
        Part { access: Access::Session, router: Router::new().route("/t/admin", get(admin_only).post(admin_only)) },
        Part { access: Access::Session, router: Router::new().route("/t/stepup", get(fresh_only).post(fresh_only)) },
    ]
}
