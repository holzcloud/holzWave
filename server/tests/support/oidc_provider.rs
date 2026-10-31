//! An identity provider that runs inside the test process, and the in-memory client that reaches it.
//!
//! The provider answers the discovery document, the key set and the token endpoint through an `axum` router. The token
//! endpoint checks the client id and secret, the redirect address, the one-time code and the PKCE verifier against the stored
//! challenge, and answers an ID token signed with ES256 by a test key. The "browser" step (asking the provider for a code for
//! an authorization address) is [`TestProvider::authorize`]. A [`Behaviour`] makes the provider misbehave in one chosen way.
//!
//! [`MemoryHttp`] implements the library's client trait by calling the router directly, so a whole flow runs without a
//! socket. The tokens are built by hand here and only here: the server never parses or builds one itself.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use holzwave_server::auth::oidc_http::{HttpError, HttpFuture};
use holzwave_server::clock::{Clock, ManualClock};
use http_body_util::BodyExt;
use openidconnect::url::{Url, form_urlencoded};
use openidconnect::{AsyncHttpClient, HttpRequest, HttpResponse};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

pub const ISSUER: &str = "https://idp.example.test";
pub const CLIENT_ID: &str = "hw-client";
pub const SECRET: &str = "s3cret-value-for-the-test-client";
pub const ACCESS_TOKEN: &str = "access-token-of-the-test-provider";
pub const REDIRECT: &str = "https://hw.example.test/auth/oidc/callback";

/// How an ID token is signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signer {
    /// ES256 with the key the key set publishes.
    Primary,
    /// ES256 with a key the key set does not publish, under the published key id.
    Other,
    /// ES256 with the second key, which the key set publishes only after `publish_second_key`.
    Second,
    /// `alg` none and no signature.
    None,
    /// HS256 with the client secret as the key.
    Hs256,
}

/// How the access token hash claim is made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtHash {
    Absent,
    Correct,
    Wrong,
}

/// What the provider does wrong, one thing at a time. The default is a correct provider.
#[derive(Clone, Debug)]
pub struct Behaviour {
    pub issuer: Option<String>,
    pub audience: Option<Vec<String>>,
    pub azp: Option<String>,
    /// `None` echoes the nonce of the request, `Some(None)` leaves it out, `Some(Some(x))` sends `x`.
    pub nonce: Option<Option<String>>,
    pub exp_in: i64,
    pub iat_offset: i64,
    pub signer: Signer,
    pub kid: Option<String>,
    pub omit_sub: bool,
    pub sub: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub groups: Option<Value>,
    pub groups_claim: String,
    pub extra_claims: Vec<(String, Value)>,
    pub token_status: u16,
    pub omit_id_token: bool,
    pub discovery_issuer: Option<String>,
    pub discovery_status: u16,
    pub unreachable: bool,
    pub at_hash: AtHash,
    pub preferred_username: Option<String>,
    pub name: Option<String>,
}

impl Default for Behaviour {
    fn default() -> Behaviour {
        Behaviour {
            issuer: None,
            audience: None,
            azp: None,
            nonce: None,
            exp_in: 300,
            iat_offset: 0,
            signer: Signer::Primary,
            kid: None,
            omit_sub: false,
            sub: "sub-1".to_owned(),
            email: Some("a@example.org".to_owned()),
            email_verified: true,
            groups: None,
            groups_claim: "groups".to_owned(),
            extra_claims: Vec::new(),
            token_status: 200,
            omit_id_token: false,
            discovery_issuer: None,
            discovery_status: 200,
            unreachable: false,
            at_hash: AtHash::Absent,
            preferred_username: None,
            name: None,
        }
    }
}

struct AuthRequest {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    nonce: Option<String>,
}

/// What the token endpoint saw last, for the tests that look at the client authentication.
#[derive(Clone, Debug, Default)]
pub struct TokenRequest {
    pub authorization: Option<String>,
    pub cookie: Option<String>,
    pub form: Vec<(String, String)>,
}

#[derive(Default)]
struct Inner {
    behaviour: Behaviour,
    codes: HashMap<String, AuthRequest>,
    second_published: bool,
    last_token_request: Option<TokenRequest>,
    next_code: u64,
}

/// The provider.
pub struct TestProvider {
    pub issuer: String,
    pub host: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub kid: String,
    clock: Arc<ManualClock>,
    key: EcdsaKeyPair,
    other: EcdsaKeyPair,
    second: EcdsaKeyPair,
    inner: Mutex<Inner>,
    pub discovery_requests: AtomicUsize,
    pub jwks_requests: AtomicUsize,
    pub token_requests: AtomicUsize,
    pub requests: AtomicUsize,
}

fn new_key() -> EcdsaKeyPair {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).expect("a key is generated");
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).expect("the key loads")
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn jwk(kid: &str, key: &EcdsaKeyPair) -> Value {
    let point = key.public_key().as_ref();
    json!({"kty": "EC", "crv": "P-256", "use": "sig", "alg": "ES256", "kid": kid,
        "x": b64(&point[1..33]), "y": b64(&point[33..65])})
}

fn response(status: u16, content_type: &str, body: Vec<u8>) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Body::from(body))
        .expect("a response is built")
}

fn json_response(status: u16, value: &Value) -> Response {
    response(status, "application/json", value.to_string().into_bytes())
}

impl TestProvider {
    /// A provider for the issuer `https://<host>` with a confidential client when `secret` is given.
    pub fn new(host: &str, clock: Arc<ManualClock>, secret: Option<&str>) -> Arc<TestProvider> {
        Arc::new(TestProvider {
            issuer: format!("https://{host}"),
            host: host.to_owned(),
            client_id: CLIENT_ID.to_owned(),
            client_secret: secret.map(str::to_owned),
            kid: "k1".to_owned(),
            clock,
            key: new_key(),
            other: new_key(),
            second: new_key(),
            inner: Mutex::new(Inner::default()),
            discovery_requests: AtomicUsize::new(0),
            jwks_requests: AtomicUsize::new(0),
            token_requests: AtomicUsize::new(0),
            requests: AtomicUsize::new(0),
        })
    }

    /// The usual provider: the issuer `https://idp.example.test` and a client with the test secret.
    pub fn standard(clock: Arc<ManualClock>) -> Arc<TestProvider> {
        TestProvider::new("idp.example.test", clock, Some(SECRET))
    }

    /// Changes how the provider behaves from now on.
    pub fn set(&self, change: impl FnOnce(&mut Behaviour)) {
        change(&mut self.lock().behaviour);
    }

    /// Puts the provider back to a correct one.
    pub fn reset(&self) {
        self.lock().behaviour = Behaviour::default();
    }

    /// From now on the key set also lists the second key under the key id `k2`.
    pub fn publish_second_key(&self) {
        self.lock().second_published = true;
    }

    pub fn last_token_request(&self) -> Option<TokenRequest> {
        self.lock().last_token_request.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The browser step: the provider remembers the request an authorization address carries and answers a one-time code. The
    /// state is returned as it was sent.
    pub fn authorize(&self, address: &str) -> Callback {
        let url = Url::parse(address).expect("the authorization address parses");
        assert_eq!(url.scheme(), "https", "the authorization endpoint is https");
        // A provider on a loopback port is named `host:port`; the address then has to carry that port.
        let (bare, port) = match self.host.split_once(':') {
            Some((bare, port)) => (bare, port.parse::<u16>().ok()),
            None => (self.host.as_str(), None),
        };
        assert_eq!(url.host_str(), Some(bare), "the address points at this provider");
        assert_eq!(url.port(), port, "the address carries the port of this provider");
        assert_eq!(url.path(), "/authorize");
        let params: HashMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let request = AuthRequest {
            client_id: params.get("client_id").cloned().unwrap_or_default(),
            redirect_uri: params.get("redirect_uri").cloned().unwrap_or_default(),
            challenge: params.get("code_challenge").cloned().unwrap_or_default(),
            nonce: params.get("nonce").cloned(),
        };
        let mut inner = self.lock();
        inner.next_code += 1;
        let code = format!("code-{}-{}", inner.next_code, b64(&Sha256::digest(address.as_bytes()))[..8].to_owned());
        inner.codes.insert(code.clone(), request);
        Callback { code, state: params.get("state").cloned().unwrap_or_default() }
    }

    /// The names of the query of an authorization address, in the order they appear.
    pub fn query_names(address: &str) -> Vec<String> {
        Url::parse(address).expect("the address parses").query_pairs().map(|(k, _)| k.into_owned()).collect()
    }

    /// The value of one query parameter of an authorization address.
    pub fn query_value(address: &str, name: &str) -> Option<String> {
        Url::parse(address).expect("the address parses").query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned())
    }

    fn now(&self) -> i64 {
        self.clock.unix()
    }

    fn id_token(&self, behaviour: &Behaviour, request: &AuthRequest, second_kid: bool) -> String {
        let now = self.now();
        let aud = match &behaviour.audience {
            Some(list) if list.len() == 1 => json!(list[0]),
            Some(list) => json!(list),
            None => json!(self.client_id),
        };
        let mut claims = json!({
            "iss": behaviour.issuer.clone().unwrap_or_else(|| self.issuer.clone()),
            "aud": aud,
            "exp": now + behaviour.exp_in,
            "iat": now + behaviour.iat_offset,
        });
        let object = claims.as_object_mut().expect("claims are an object");
        if !behaviour.omit_sub {
            object.insert("sub".into(), json!(behaviour.sub));
        }
        match &behaviour.nonce {
            None => {
                if let Some(nonce) = &request.nonce {
                    object.insert("nonce".into(), json!(nonce));
                }
            }
            Some(Some(nonce)) => {
                object.insert("nonce".into(), json!(nonce));
            }
            Some(None) => {}
        }
        if let Some(azp) = &behaviour.azp {
            object.insert("azp".into(), json!(azp));
        }
        if let Some(email) = &behaviour.email {
            object.insert("email".into(), json!(email));
            object.insert("email_verified".into(), json!(behaviour.email_verified));
        }
        if let Some(groups) = &behaviour.groups {
            object.insert(behaviour.groups_claim.clone(), groups.clone());
        }
        if let Some(name) = &behaviour.preferred_username {
            object.insert("preferred_username".into(), json!(name));
        }
        if let Some(name) = &behaviour.name {
            object.insert("name".into(), json!(name));
        }
        for (key, value) in &behaviour.extra_claims {
            object.insert(key.clone(), value.clone());
        }
        match behaviour.at_hash {
            AtHash::Absent => {}
            AtHash::Correct => {
                let digest = Sha256::digest(ACCESS_TOKEN.as_bytes());
                object.insert("at_hash".into(), json!(b64(&digest[..16])));
            }
            AtHash::Wrong => {
                let digest = Sha256::digest(b"another access token");
                object.insert("at_hash".into(), json!(b64(&digest[..16])));
            }
        }
        let alg = match behaviour.signer {
            Signer::None => "none",
            Signer::Hs256 => "HS256",
            _ => "ES256",
        };
        let kid = behaviour
            .kid
            .clone()
            .unwrap_or_else(|| if second_kid || behaviour.signer == Signer::Second { "k2".to_owned() } else { self.kid.clone() });
        let header = json!({"alg": alg, "typ": "JWT", "kid": kid});
        let signing_input = format!("{}.{}", b64(header.to_string().as_bytes()), b64(claims.to_string().as_bytes()));
        let signature = match behaviour.signer {
            Signer::Primary => self.sign(&self.key, &signing_input),
            Signer::Other => self.sign(&self.other, &signing_input),
            Signer::Second => self.sign(&self.second, &signing_input),
            Signer::None => String::new(),
            Signer::Hs256 => {
                let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, self.client_secret.as_deref().unwrap_or("").as_bytes());
                b64(ring::hmac::sign(&key, signing_input.as_bytes()).as_ref())
            }
        };
        format!("{signing_input}.{signature}")
    }

    fn sign(&self, key: &EcdsaKeyPair, input: &str) -> String {
        let rng = SystemRandom::new();
        b64(key.sign(&rng, input.as_bytes()).expect("the signature is made").as_ref())
    }

    fn discovery(&self) -> Response {
        self.discovery_requests.fetch_add(1, Ordering::SeqCst);
        let behaviour = self.lock().behaviour.clone();
        if behaviour.discovery_status != 200 {
            return json_response(behaviour.discovery_status, &json!({"error": "server_error"}));
        }
        let issuer = behaviour.discovery_issuer.unwrap_or_else(|| self.issuer.clone());
        json_response(
            200,
            &json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{}/authorize", self.issuer),
                "token_endpoint": format!("{}/token", self.issuer),
                "jwks_uri": format!("{}/jwks", self.issuer),
                "response_types_supported": ["code"],
                "subject_types_supported": ["public"],
                "id_token_signing_alg_values_supported": ["ES256"],
                "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
                "scopes_supported": ["openid", "email", "profile"],
                "code_challenge_methods_supported": ["S256"],
            }),
        )
    }

    fn jwks(&self) -> Response {
        self.jwks_requests.fetch_add(1, Ordering::SeqCst);
        let mut keys = vec![jwk(&self.kid, &self.key)];
        if self.lock().second_published {
            keys.push(jwk("k2", &self.second));
        }
        json_response(200, &json!({ "keys": keys }))
    }

    fn token(&self, authorization: Option<String>, cookie: Option<String>, body: &[u8]) -> Response {
        self.token_requests.fetch_add(1, Ordering::SeqCst);
        let form: Vec<(String, String)> =
            form_urlencoded::parse(body).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let field = |name: &str| form.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        let mut inner = self.lock();
        inner.last_token_request = Some(TokenRequest { authorization: authorization.clone(), cookie, form: form.clone() });
        let behaviour = inner.behaviour.clone();
        if behaviour.token_status != 200 {
            return json_response(behaviour.token_status, &json!({"error": "server_error"}));
        }
        let invalid = |error: &str| json_response(400, &json!({"error": error}));
        // The client authenticates with the secret (basic) or, as a public client, with its id in the form.
        match &self.client_secret {
            Some(secret) => {
                let expected = format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(format!(
                        "{}:{}",
                        form_urlencoded::byte_serialize(self.client_id.as_bytes()).collect::<String>(),
                        form_urlencoded::byte_serialize(secret.as_bytes()).collect::<String>()
                    ))
                );
                if authorization.as_deref() != Some(expected.as_str()) {
                    return json_response(401, &json!({"error": "invalid_client"}));
                }
            }
            None => {
                if authorization.is_some() || field("client_id").as_deref() != Some(self.client_id.as_str()) {
                    return json_response(401, &json!({"error": "invalid_client"}));
                }
            }
        }
        if field("grant_type").as_deref() != Some("authorization_code") {
            return invalid("unsupported_grant_type");
        }
        let Some(code) = field("code") else { return invalid("invalid_request") };
        let Some(request) = inner.codes.remove(&code) else { return invalid("invalid_grant") };
        if request.client_id != self.client_id || field("redirect_uri").as_deref() != Some(request.redirect_uri.as_str()) {
            return invalid("invalid_grant");
        }
        let Some(verifier) = field("code_verifier") else { return invalid("invalid_grant") };
        if b64(&Sha256::digest(verifier.as_bytes())) != request.challenge {
            return invalid("invalid_grant");
        }
        let mut answer = json!({"access_token": ACCESS_TOKEN, "token_type": "Bearer", "expires_in": 3600});
        if !behaviour.omit_id_token {
            let second = inner.second_published && behaviour.signer == Signer::Second;
            answer["id_token"] = json!(self.id_token(&behaviour, &request, second));
        }
        json_response(200, &answer)
    }
}

/// What the browser brings back.
#[derive(Clone, Debug)]
pub struct Callback {
    pub code: String,
    pub state: String,
}

async fn handle(State(provider): State<Arc<TestProvider>>, request: Request) -> Response {
    provider.requests.fetch_add(1, Ordering::SeqCst);
    let path = request.uri().path().to_owned();
    let (authorization, cookie) = {
        let header = |name: &str| request.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
        (header("authorization"), header("cookie"))
    };
    let method = request.method().clone();
    let body = request.into_body().collect().await.map(|b| b.to_bytes().to_vec()).unwrap_or_default();
    match (method.as_str(), path.as_str()) {
        ("GET", "/.well-known/openid-configuration") => provider.discovery(),
        ("GET", "/jwks") => provider.jwks(),
        ("POST", "/token") => provider.token(authorization, cookie, &body),
        _ => response(404, "text/plain", b"not found".to_vec()),
    }
}

/// The routes of the provider (discovery, key set, token endpoint), for a caller that serves them on a socket of its own.
pub fn router(provider: Arc<TestProvider>) -> Router {
    Router::new().fallback(handle).with_state(provider)
}

/// The in-memory client: it calls the router of the provider that owns the host of the address.
pub struct MemoryHttp {
    providers: Vec<Arc<TestProvider>>,
}

impl MemoryHttp {
    pub fn new(providers: &[Arc<TestProvider>]) -> MemoryHttp {
        MemoryHttp { providers: providers.to_vec() }
    }
}

impl<'c> AsyncHttpClient<'c> for MemoryHttp {
    type Error = HttpError;
    type Future = HttpFuture<'c>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            if parts.uri.scheme_str() != Some("https") {
                return Err(HttpError::NotHttps);
            }
            let host = parts.uri.host().unwrap_or_default().to_owned();
            let authority = parts.uri.authority().map(|a| a.as_str().to_owned()).unwrap_or_default();
            let provider = self.providers.iter().find(|p| p.host == host || p.host == authority).ok_or(HttpError::Failed)?.clone();
            if provider.lock().behaviour.unreachable {
                return Err(HttpError::Failed);
            }
            let router = router(provider);
            let answer = router
                .oneshot(axum::http::Request::from_parts(parts, Body::from(body)))
                .await
                .map_err(|_| HttpError::Failed)?;
            let (parts, body) = answer.into_parts();
            let bytes = body.collect().await.map_err(|_| HttpError::Failed)?.to_bytes().to_vec();
            Ok(HttpResponse::from_parts(parts, bytes))
        })
    }
}
