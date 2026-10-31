//! The identity provider of the browser tests: the support provider of the single sign-on tests, served on a real TLS listener on
//! `https://127.0.0.1:<port>` so that a browser can leave for it, plus an `/authorize` page that approves the identity the test
//! set last (it has no login page of its own).
//!
//! The server's own calls to the provider (discovery, key set, token endpoint) do not go over this socket: the harness hands the
//! server an in-process client through the `oidc_client` field of `Overrides`, which reaches the same router. The browser's
//! step is the one that needs the socket.
#![allow(dead_code)]
#![cfg(feature = "in-process-workers")]

#[path = "oidc_provider.rs"]
pub mod oidc_provider;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::Response;
use axum::routing::get;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use holzwave_server::clock::{Clock, ManualClock, SystemClock};
use holzwave_server::supervisor::serve::OidcClient;
use oidc_provider::{CLIENT_ID, MemoryHttp, SECRET, TestProvider};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;

/// What the provider does with the next authorization request.
#[derive(Clone, Default)]
struct Choice {
    /// Answer `error=access_denied` instead of a code.
    deny: bool,
}

/// The provider and its socket.
pub struct Idp {
    pub provider: Arc<TestProvider>,
    pub port: u16,
    /// The address the server lets the browser come back to.
    redirect: String,
    clock: Arc<ManualClock>,
    choice: Mutex<Choice>,
}

impl Idp {
    /// A provider for `https://127.0.0.1:<port>` whose single client returns to `<origin>/auth/oidc/callback`.
    pub fn new(port: u16, origin: &str) -> Arc<Idp> {
        let clock = Arc::new(ManualClock::new(SystemClock.unix()));
        let provider = TestProvider::new(&format!("127.0.0.1:{port}"), clock.clone(), Some(SECRET));
        Arc::new(Idp { provider, port, redirect: format!("{origin}/auth/oidc/callback"), clock, choice: Mutex::new(Choice::default()) })
    }

    /// The issuer the server's configuration names.
    pub fn issuer(&self) -> String {
        self.provider.issuer.clone()
    }

    pub fn client_id(&self) -> &'static str {
        CLIENT_ID
    }

    pub fn secret(&self) -> &'static str {
        SECRET
    }

    /// The server's own client: it calls the provider's router in the process.
    pub fn client(&self) -> OidcClient {
        OidcClient::new(MemoryHttp::new(&[self.provider.clone()]))
    }

    /// The identity the next authorization approves.
    pub fn set_identity(&self, sub: &str, email: Option<&str>, name: Option<&str>, groups: &[String], email_verified: bool, deny: bool) {
        self.clock.set(SystemClock.unix());
        let groups: Vec<serde_json::Value> = groups.iter().map(|g| serde_json::json!(g)).collect();
        self.provider.set(|b| {
            b.sub = sub.to_owned();
            b.email = email.map(str::to_owned);
            b.email_verified = email_verified;
            b.name = name.map(str::to_owned);
            b.preferred_username = None;
            b.groups = if groups.is_empty() { None } else { Some(serde_json::Value::Array(groups)) };
        });
        self.choice.lock().expect("lock").deny = deny;
    }

    /// Serves the provider's pages on `127.0.0.1:<port>` until the task is aborted.
    pub async fn serve(self: &Arc<Idp>, certificate: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> std::io::Result<JoinHandle<()>> {
        let mut config = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("protocol versions")
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)
            .expect("the certificate and key fit");
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], self.port))).await?;
        let router: Router = Router::new().route("/authorize", get(authorize)).with_state(self.clone());
        Ok(tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                let Ok((tcp, _)) = listener.accept().await else { break };
                let acceptor = acceptor.clone();
                let service = TowerToHyperService::new(router.clone());
                connections.spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else { return };
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tls), service).await;
                });
                while connections.try_join_next().is_some() {}
            }
        }))
    }
}

fn plain(status: StatusCode, text: &str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(text.to_owned()))
        .expect("a response")
}

fn redirect(location: &str) -> Response {
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::empty())
        .expect("a response")
}

/// `GET /authorize`: checks the request a client must send (its id, its one return address, the code flow, a PKCE challenge, a
/// state and a nonce) and returns the browser to the client with a code, or with `error=access_denied`.
async fn authorize(State(idp): State<Arc<Idp>>, uri: Uri, Query(query): Query<HashMap<String, String>>) -> Response {
    let get = |name: &str| query.get(name).map(String::as_str).unwrap_or_default();
    if get("client_id") != CLIENT_ID {
        return plain(StatusCode::BAD_REQUEST, "unknown client");
    }
    if get("redirect_uri") != idp.redirect {
        return plain(StatusCode::BAD_REQUEST, "unknown return address");
    }
    if get("response_type") != "code" {
        return plain(StatusCode::BAD_REQUEST, "only the code flow");
    }
    if get("code_challenge").is_empty() || get("code_challenge_method") != "S256" {
        return plain(StatusCode::BAD_REQUEST, "a PKCE challenge is required");
    }
    if get("state").is_empty() || get("nonce").is_empty() {
        return plain(StatusCode::BAD_REQUEST, "state and nonce are required");
    }
    idp.clock.set(SystemClock.unix());
    let state = urlencode(get("state"));
    let iss = urlencode(&idp.issuer());
    if idp.choice.lock().expect("lock").deny {
        return redirect(&format!("{}?error=access_denied&state={state}&iss={iss}", idp.redirect));
    }
    let address = format!("{}/authorize?{}", idp.issuer(), uri.query().unwrap_or_default());
    let callback = idp.provider.authorize(&address);
    redirect(&format!("{}?code={}&state={state}&iss={iss}", idp.redirect, urlencode(&callback.code)))
}

fn urlencode(text: &str) -> String {
    openidconnect::url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}
