//! The routes that make the first administrator and every invited person: `POST /auth/bootstrap`.
//!
//! An enrolment is two steps and creates nothing until the second one proves the chosen method:
//!
//! - **`begin`** checks the token without spending it, the username and the password policy, makes the one derivation of the
//!   password method outside any transaction, and keeps the half-finished enrolment in memory behind the cookie
//!   `__Host-hw_pending`. The answer shows the authenticator secret. No account, no row, no session and no audit line exist
//!   after it, and the token is still valid.
//! - **`finish`** checks a current code for that secret, and only then redeems the token in one transaction of the accounts
//!   service: the account, the password, the sealed authenticator secret with the confirmed step, the unlock-method rows and
//!   the first session commit together or not at all. After the commit the keys of the new account go to the unlock gate and
//!   the one answer that holds the recovery code is sent.
//!
//! What the module guarantees:
//!
//! - **One refusal for every dead token.** An unknown, expired, spent or no-longer-allowed token is the same `410 gone`, byte
//!   for byte, whichever step it meets. Each such refusal counts against the client address in a throttle key space of its
//!   own; the lock answers `429` before any service is called.
//! - **The half-finished state is bounded and wiped.** At most 1,024 entries and 16 per client address (the oldest of an
//!   address goes first), five minutes of life, three wrong codes, removal on first use. The token, the derived password key
//!   and the authenticator secret are wiped when an entry is dropped.
//! - **Nothing secret is written down.** The recovery code and the authenticator secret each appear in exactly one answer.
//!   This file writes no log line with a field and no print on purpose, so that there is no place for a token, a code or a
//!   key to leak to.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use axum::{Extension, Router};
use cookie::Cookie;
use serde::Deserialize;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::accounts::{AccountError, Accounts, NewAccount};
use crate::auth::password::{ArgonGate, PasswordError, Prepared, check_policy, prepare_password, store_prepared};
use crate::auth::session::{generate_token, parse_token};
use crate::auth::throttle::{Policy, Throttle, scoped_key};
use crate::auth::totp::{self, SECRET_BYTES};
use crate::http::router::{Access, ApiError, ApiJson, Authed, ClientAddr, MaybeAuthed, Part};
use crate::proxy::key_of;
use crate::sealed::Sealer;
use crate::store::{Account, NewSession, SignInMethod, TokenHash, UnlockKind, hash_token, valid_username};
use crate::unlock::{SALT_BYTES, generate_recovery_code, kek_from_recovery, random};

use super::cookies::{self, MAX_COOKIE_HEADER_BYTES, MAX_COOKIE_PAIRS, MAX_TOKEN_CHARS, PENDING_COOKIE};
use super::routes_auth::{GATE_TIMEOUT, Object, Refusal, answer, blocking, device_of};
use super::routes_unlock::{Material, ProvisionKey};

/// How long a half-finished enrolment lives, in seconds: the life of its cookie.
pub const PENDING_TTL_SECS: i64 = 300;

/// The most half-finished enrolments at once. The one after them pushes out the oldest.
pub const MAX_PENDING: usize = 1_024;

/// The most half-finished enrolments one client address has at once. The one after them pushes out that address's oldest.
pub const MAX_PENDING_PER_ADDRESS: usize = 16;

/// How many wrong codes one half-finished enrolment allows. The last one removes it.
pub const CODE_ATTEMPTS: u8 = 3;

/// How many dead tokens one client address may present within [`TOKEN_WINDOW_SECS`] before the address is locked.
pub const TOKEN_FAILURES: i64 = 20;

/// The window in which dead tokens are counted, and the length of the lock, in seconds: fifteen minutes.
pub const TOKEN_WINDOW_SECS: i64 = 900;

/// The throttle scope of the dead tokens of one client address. It is neither `a`, `i`, `ua` nor `ui`, the scopes of the
/// sign-in and the unlock, so the counts never meet.
const TOKEN_SCOPE: &str = "en";

/// The longest token text that is looked at, in bytes. A real token is 47 characters.
const MAX_TOKEN_BYTES: usize = 128;

/// The longest code text that is looked at, in bytes. A real code is six digits.
const MAX_CODE_BYTES: usize = 32;

/// The longest display name, in characters.
const MAX_DISPLAY_CHARS: usize = 64;

/// The most bytes of a display name that are looked at: 64 characters of at most 4 bytes, with room for the spaces around.
const MAX_DISPLAY_INPUT_BYTES: usize = 4 * MAX_DISPLAY_CHARS + 64;

/// The most bytes of a password that are looked at.
const MAX_PASSWORD_BYTES: usize = crate::auth::password::MAX_PASSWORD_BYTES;

/// The parameters text of the unlock rows that come from HKDF.
const HKDF_PARAMS: &str = "hkdf-sha256";

/// The throttle key of the dead tokens of one client address (an IPv6 address by its /64, as everywhere). Its own space: it
/// never equals a sign-in or an unlock key.
pub fn enrol_address_key(addr: IpAddr) -> String {
    scoped_key(TOKEN_SCOPE, key_of(addr).to_string().as_bytes())
}

/// The services the enrolment routes call.
pub struct EnrolServices {
    /// The sign-in services: the application state, the passkeys and the unlock gate.
    pub auth: Arc<super::routes_auth::AuthServices>,
    /// The accounts service, which redeems the tokens.
    pub accounts: Arc<Accounts>,
    /// The door to Argon2.
    pub argon: ArgonGate,
    /// Seals the authenticator secret.
    pub sealer: Arc<Sealer>,
}

// ---------------------------------------------------------------------------------------------------------------------
// The half-finished enrolments
// ---------------------------------------------------------------------------------------------------------------------

/// Which route an enrolment belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// `POST /auth/bootstrap`: the first administrator.
    Bootstrap,
}

/// What the person proves at `finish`.
enum Method {
    /// A code for the secret that `begin` showed, and the derived password credential.
    Password { prepared: Prepared, secret: Zeroizing<[u8; SECRET_BYTES]> },
}

/// One half-finished enrolment.
struct Entry {
    flow: Flow,
    token: Zeroizing<String>,
    username: String,
    /// The trimmed display name the person typed; empty when they typed none.
    display_name: String,
    method: Method,
    created: i64,
    /// The key of the client address that started it.
    address: String,
    /// Counts the starts, so that "oldest" is exact when several start in one second.
    sequence: u64,
    attempts_left: u8,
}

impl Entry {
    fn expired(&self, now: i64) -> bool {
        now > self.created.saturating_add(PENDING_TTL_SECS)
    }
}

#[derive(Default)]
struct Pending {
    map: HashMap<TokenHash, Entry>,
    next: u64,
}

impl Pending {
    /// The number for the next new entry.
    fn sequence(&mut self) -> u64 {
        let sequence = self.next;
        self.next = self.next.wrapping_add(1);
        sequence
    }

    /// Puts an entry in. Room is made first, with work bounded by the size of the map: expired entries go, then the oldest of
    /// the same address while that address has [`MAX_PENDING_PER_ADDRESS`], then the oldest of all while the map is full.
    fn push(&mut self, hash: TokenHash, entry: Entry, now: i64) {
        self.map.retain(|_, held| !held.expired(now));
        while self.map.values().filter(|held| held.address == entry.address).count() >= MAX_PENDING_PER_ADDRESS {
            let oldest = self
                .map
                .iter()
                .filter(|(_, held)| held.address == entry.address)
                .min_by_key(|(_, held)| held.sequence)
                .map(|(hash, _)| *hash);
            match oldest {
                Some(hash) => self.map.remove(&hash),
                None => break,
            };
        }
        while self.map.len() >= MAX_PENDING {
            let oldest = self.map.iter().min_by_key(|(_, held)| held.sequence).map(|(hash, _)| *hash);
            match oldest {
                Some(hash) => self.map.remove(&hash),
                None => break,
            };
        }
        self.map.insert(hash, entry);
    }

    /// Takes the entry of a cookie out for its one use. An entry of another route stays where it is and is not answered.
    fn take(&mut self, hash: &TokenHash, flow: Flow, now: i64) -> Option<Entry> {
        if self.map.get(hash).is_some_and(|held| held.flow != flow) {
            return None;
        }
        let entry = self.map.remove(hash)?;
        (!entry.expired(now)).then_some(entry)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The state of the routes
// ---------------------------------------------------------------------------------------------------------------------

/// The enrolment routes and their half-finished states.
pub struct Enrol {
    services: EnrolServices,
    throttle: Throttle,
    pending: Mutex<Pending>,
}

impl Enrol {
    /// The enrolment over `services`.
    pub fn new(services: EnrolServices) -> Arc<Enrol> {
        let throttle = Throttle::new(
            services.auth.app.store.clone(),
            Policy {
                account_limit: TOKEN_FAILURES,
                address_limit: TOKEN_FAILURES,
                window_secs: TOKEN_WINDOW_SECS,
                lock_secs: TOKEN_WINDOW_SECS,
            },
        );
        Arc::new(Enrol { services, throttle, pending: Mutex::new(Pending::default()) })
    }

    /// How many half-finished enrolments exist now.
    pub fn pending_count(&self) -> usize {
        self.lock().map.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn now(&self) -> i64 {
        self.services.auth.app.clock.unix()
    }

    /// `429` while the address is locked for presenting dead tokens. Nothing is called and nothing is counted.
    async fn check_lock(&self, client: IpAddr, now: i64) -> Result<(), Refusal> {
        let throttle = self.throttle.clone();
        let key = enrol_address_key(client);
        let locked = blocking(move || throttle.check(None, &key, now)).await?.map_err(|_| ApiError::Busy)?;
        match locked {
            Some(retry_seconds) => Err(ApiError::Throttled { retry_seconds }.into()),
            None => Ok(()),
        }
    }

    /// Counts a dead token against the address and answers the one `410`. A failure that cannot be counted must not become a
    /// free guess, so it answers `503`.
    async fn gone(&self, client: IpAddr, now: i64) -> Refusal {
        let throttle = self.throttle.clone();
        let key = enrol_address_key(client);
        match blocking(move || throttle.record_failure(None, &key, now)).await {
            Ok(Ok(_)) => Refusal(ApiError::Gone),
            _ => Refusal(ApiError::Busy),
        }
    }

    /// The refusal for a failed redemption.
    async fn refuse(&self, error: AccountError, client: IpAddr, now: i64) -> Refusal {
        match error {
            AccountError::NotRedeemable => self.gone(client, now).await,
            AccountError::Conflict => Refusal(ApiError::Conflict { field: Some("username") }),
            AccountError::Invalid(_) => Refusal(ApiError::BadRequest { field: None }),
            _ => Refusal(ApiError::Busy),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The body
// ---------------------------------------------------------------------------------------------------------------------

/// The members of a body. Each is any JSON value here, so that a member of the wrong type is told apart from a body that is no
/// object and answers with the name of the member. Unknown members and a member named twice are refused by the struct.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    #[serde(default)]
    step: Option<Value>,
    #[serde(default)]
    token: Option<Value>,
    #[serde(default)]
    username: Option<Value>,
    #[serde(default)]
    display_name: Option<Value>,
    #[serde(default)]
    method: Option<Value>,
    #[serde(default)]
    password: Option<Value>,
    #[serde(default)]
    code: Option<Value>,
}

fn bad(field: &'static str) -> ApiError {
    ApiError::BadRequest { field: Some(field) }
}

/// A text member of at most `most` bytes; anything else names the member at fault.
fn text(value: Value, field: &'static str, most: usize) -> Result<String, ApiError> {
    match value {
        Value::String(text) if text.len() <= most => Ok(text),
        _ => Err(bad(field)),
    }
}

/// Whether a character may be part of a display name: not a control character and none of the invisible ones that change how
/// text reads (zero-width spaces, direction marks and overrides, line and paragraph separators, the byte order mark).
fn printable(c: char) -> bool {
    !(c.is_control()
        || matches!(
            u32::from(c),
            0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB
        ))
}

/// The display name a person typed, without the spaces around it: empty (the user name is used then) or 1 to 64 printable
/// characters.
fn display_name(value: Option<Value>) -> Result<String, ApiError> {
    let Some(value) = value else { return Ok(String::new()) };
    let raw = text(value, "display_name", MAX_DISPLAY_INPUT_BYTES)?;
    let trimmed = raw.trim();
    if trimmed.chars().count() > MAX_DISPLAY_CHARS || !trimmed.chars().all(printable) {
        return Err(bad("display_name"));
    }
    Ok(trimmed.to_owned())
}

/// What `begin` was given.
struct Begin {
    token: Zeroizing<String>,
    username: String,
    display_name: String,
    password: Zeroizing<String>,
}

/// One step of a body, checked for shape and size before anything else looks at it.
enum Step {
    Begin(Begin),
    Finish { code: Zeroizing<String> },
}

fn parse(flow: Flow, body: Body, methods: Methods) -> Result<Step, ApiError> {
    let Body { step, token, username, display_name: shown, method, password, code } = body;
    let step = match step {
        Some(Value::String(step)) => step,
        _ => return Err(bad("step")),
    };
    match step.as_str() {
        "begin" => {
            if code.is_some() {
                return Err(ApiError::BadRequest { field: None });
            }
            let token = Zeroizing::new(text(token.ok_or(bad("token"))?, "token", MAX_TOKEN_BYTES)?);
            let username = text(username.ok_or(bad("username"))?, "username", 64)?;
            if !valid_username(&username) {
                return Err(bad("username"));
            }
            let display_name = display_name(shown)?;
            let method = text(method.ok_or(bad("method"))?, "method", 16)?;
            match method.as_str() {
                "password" if methods.password => {}
                _ => return Err(bad("method")),
            }
            let password = match password {
                Some(Value::String(password)) => Zeroizing::new(password),
                _ => return Err(bad("password")),
            };
            if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
                return Err(bad("password"));
            }
            Ok(Step::Begin(Begin { token, username, display_name, password }))
        }
        "finish" => {
            if token.is_some() || username.is_some() || shown.is_some() || method.is_some() || password.is_some() {
                return Err(ApiError::BadRequest { field: None });
            }
            let code = Zeroizing::new(text(code.ok_or(bad("code"))?, "code", MAX_CODE_BYTES)?);
            Ok(Step::Finish { code })
        }
        // A bootstrap token is looked at by `begin` alone; only an invitation has a step of its own for it.
        _ => match flow {
            Flow::Bootstrap => Err(bad("step")),
        },
    }
}

/// Which methods are switched on.
#[derive(Clone, Copy)]
struct Methods {
    password: bool,
}

// ---------------------------------------------------------------------------------------------------------------------
// The cookie
// ---------------------------------------------------------------------------------------------------------------------

/// The hash of the pending cookie of a request, read as strictly as the session cookie: nothing for a header that is too big,
/// too many pairs, a pair that does not parse, the name twice, or a value that is not exactly a token.
fn pending_hash(headers: &HeaderMap) -> Option<TokenHash> {
    let mut total = 0usize;
    let mut pairs = 0usize;
    let mut found: Option<String> = None;
    for line in headers.get_all(header::COOKIE) {
        total = total.saturating_add(line.len());
        if total > MAX_COOKIE_HEADER_BYTES {
            return None;
        }
        let text = line.to_str().ok()?;
        for parsed in Cookie::split_parse(text) {
            pairs += 1;
            if pairs > MAX_COOKIE_PAIRS {
                return None;
            }
            let cookie = parsed.ok()?;
            if cookie.name() == PENDING_COOKIE {
                if found.is_some() {
                    return None;
                }
                found = Some(cookie.value().to_owned());
            }
        }
    }
    let value = found.filter(|value| !value.is_empty() && value.len() <= MAX_TOKEN_CHARS)?;
    parse_token(&value).ok()
}

// ---------------------------------------------------------------------------------------------------------------------
// The steps
// ---------------------------------------------------------------------------------------------------------------------

/// The issuer an authenticator app shows: the host of the public address, or the product name when the host cannot be a label.
fn issuer(host: &str) -> &str {
    if host.is_empty() || host.contains(':') { "holzWave" } else { host }
}

/// What the person sees of a half-finished password enrolment: the secret in two spellings.
fn setup_answer(secret: &[u8], username: &str, host: &str) -> Result<Value, ApiError> {
    let uri = totp::provisioning_uri(issuer(host), username, secret).map_err(|_| ApiError::Busy)?;
    let base32 = Zeroizing::new(totp_rs::Secret::from(secret).to_base32());
    Ok(serde_json::json!({ "next": "totp_setup", "secret": base32.as_str(), "otpauth": uri.as_str() }))
}

impl Enrol {
    /// The first step.
    async fn begin(&self, flow: Flow, client: IpAddr, headers: &HeaderMap, input: Begin) -> Result<Response, Refusal> {
        let now = self.now();
        self.check_lock(client, now).await?;

        // The token is looked at without being spent.
        let accounts = self.services.accounts.clone();
        let token = input.token.clone();
        let peeked = blocking(move || match flow {
            Flow::Bootstrap => accounts.peek_bootstrap(&token),
        })
        .await?;
        if let Err(error) = peeked {
            return Err(self.refuse(error, client, now).await);
        }

        // A name that is taken is told to the holder of a valid token only.
        let store = self.services.auth.app.store.clone();
        let name = input.username.clone();
        let taken = blocking(move || store.account_by_username(&name)).await?.map_err(|_| ApiError::Busy)?;
        if taken.is_some() {
            return Err(ApiError::Conflict { field: Some("username") }.into());
        }

        check_policy(&input.username, &input.password).map_err(|_| bad("password"))?;
        let prepared = match prepare_password(&self.services.argon, &input.username, &input.password).await {
            Ok(prepared) => prepared,
            Err(PasswordError::Policy(_)) => return Err(bad("password").into()),
            Err(_) => return Err(ApiError::Busy.into()),
        };
        let secret = totp::generate_secret().map_err(|_| ApiError::Busy)?;
        let host = self.services.auth.app.origin.host();
        let body = setup_answer(&secret[..], &input.username, host)?;

        let (cookie_token, cookie_bytes) = generate_token().map_err(|_| ApiError::Busy)?;
        let cookie = cookies::pending(cookie_token.expose()).ok_or(ApiError::Busy)?;
        let hash = hash_token(&cookie_bytes[..]);
        {
            let mut pending = self.lock();
            // A browser that begins again leaves its earlier attempt behind.
            if let Some(earlier) = pending_hash(headers) {
                pending.map.remove(&earlier);
            }
            let sequence = pending.sequence();
            let entry = Entry {
                flow,
                token: input.token,
                username: input.username,
                display_name: input.display_name,
                method: Method::Password { prepared, secret },
                created: now,
                address: crate::auth::throttle::address_key(client),
                sequence,
                attempts_left: CODE_ATTEMPTS,
            };
            pending.push(hash, entry, now);
        }
        Ok(answer(StatusCode::OK, &body, vec![cookie]))
    }

    /// Puts an entry back after a wrong code, with the attempts it has left.
    fn put_back(&self, hash: TokenHash, mut entry: Entry, now: i64) {
        entry.attempts_left = entry.attempts_left.saturating_sub(1);
        if entry.attempts_left > 0 {
            self.lock().push(hash, entry, now);
        }
    }

    /// The second step.
    async fn finish(
        &self,
        flow: Flow,
        client: IpAddr,
        held: Option<Authed>,
        headers: &HeaderMap,
        code: Zeroizing<String>,
    ) -> Result<Response, Refusal> {
        let now = self.now();
        let Some(hash) = pending_hash(headers) else { return Err(ApiError::Invalid.into()) };
        let Some(entry) = self.lock().take(&hash, flow, now) else { return Err(ApiError::Invalid.into()) };

        let Method::Password { secret, .. } = &entry.method;
        let Ok(step) = totp::check_candidate(&secret[..], &code, now) else {
            self.put_back(hash, entry, now);
            return Err(ApiError::Invalid.into());
        };
        drop(code);

        let Entry { token, username, display_name, method, .. } = entry;
        let Method::Password { prepared, secret } = method;
        let display = if display_name.is_empty() { username.clone() } else { display_name };

        // Everything the transaction needs is made before it opens.
        let recovery = generate_recovery_code().map_err(|_| ApiError::Busy)?;
        let recovery_salt = random::<SALT_BYTES>().map_err(|_| ApiError::Busy)?;
        let recovery_kek = kek_from_recovery(&recovery, &recovery_salt).map_err(|_| ApiError::Busy)?;
        let recovery_text = recovery.display();
        let keys = vec![
            ProvisionKey::new(UnlockKind::Password, None, prepared.kek()),
            ProvisionKey::new(UnlockKind::Recovery, None, &recovery_kek),
        ];
        drop(recovery_kek);
        let (session_token, session_bytes) = generate_token().map_err(|_| ApiError::Busy)?;
        let session_cookie = cookies::session(session_token.expose()).ok_or(ApiError::Busy)?;
        let session_hash = hash_token(&session_bytes[..]);
        drop(session_bytes);
        let device = device_of(
            headers.get(header::USER_AGENT).and_then(|value| value.to_str().ok()).unwrap_or(""),
        );
        let absolute = i64::try_from(self.services.auth.app.sessions.absolute.as_secs()).unwrap_or(i64::MAX);

        let accounts = self.services.accounts.clone();
        let sealer = self.services.sealer.clone();
        let redeemed = blocking(move || {
            let new = NewAccount { username: &username, display_name: &display, handle: None };
            let account = match flow {
                Flow::Bootstrap => accounts.redeem_bootstrap(&token, &new, Some(client), |tx, account| {
                    store_prepared(tx, account.id, &prepared, now).map_err(|_| fault("the password could not be stored"))?;
                    totp::store_secret(tx, &sealer, account.id, &secret[..], step)
                        .map_err(|_| fault("the authenticator could not be stored"))?;
                    tx.unlock_add(account.id, UnlockKind::Password, prepared.salt(), prepared.params(), None)
                        .map_err(|_| fault("the unlock methods could not be stored"))?;
                    tx.unlock_add(account.id, UnlockKind::Recovery, &recovery_salt, HKDF_PARAMS, None)
                        .map_err(|_| fault("the unlock methods could not be stored"))?;
                    tx.session_create(&NewSession {
                        id_hash: session_hash,
                        account_id: account.id,
                        method: SignInMethod::PasswordTotp,
                        now,
                        absolute_expires: now.saturating_add(absolute),
                        device: &device,
                        addr: client,
                    })
                    .map_err(|_| fault("the session could not be made"))?;
                    Ok::<(), AccountError>(())
                })?,
            };
            Ok::<Account, AccountError>(account)
        })
        .await?;
        let account = match redeemed {
            Ok(account) => account,
            Err(error) => return Err(self.refuse(error, client, now).await),
        };

        self.after(held, account.id, keys).await;
        let body = serde_json::json!({ "next": "recovery", "recovery_code": recovery_text.as_str() });
        Ok(answer(StatusCode::OK, &body, vec![session_cookie, cookies::delete_pending()]))
    }

    /// What follows a committed enrolment: the session the browser still held is ended, the keys of the new account go to the
    /// unlock gate, and the gate is told a session began. A failure of either changes nothing for the person: the old session
    /// expires by itself, and a store that stays locked shows the unlock dialog.
    async fn after(&self, held: Option<Authed>, account: crate::store::AccountId, keys: Vec<ProvisionKey>) {
        if let Some(old) = held {
            let sessions = self.services.auth.app.session_manager.clone();
            let _ = tokio::task::spawn_blocking(move || sessions.end(&old.session)).await;
        }
        let gate = &self.services.auth.gate;
        let _ = tokio::time::timeout(GATE_TIMEOUT, gate.provision(account, keys)).await;
        let _ = tokio::time::timeout(GATE_TIMEOUT, gate.session_started(account, Material::None)).await;
    }
}

/// The error of an enrolment that failed inside the transaction: a fixed sentence, which rolls everything back.
fn fault(sentence: &'static str) -> AccountError {
    AccountError::Enrolment(sentence)
}

// ---------------------------------------------------------------------------------------------------------------------
// The routes
// ---------------------------------------------------------------------------------------------------------------------

async fn bootstrap_route(
    State(enrol): State<Arc<Enrol>>,
    Extension(client): Extension<ClientAddr>,
    MaybeAuthed(held): MaybeAuthed,
    headers: HeaderMap,
    ApiJson(Object(body)): ApiJson<Object<Body>>,
) -> Result<Response, Refusal> {
    let flow = Flow::Bootstrap;
    let app = &enrol.services.auth.app;
    let methods = Methods { password: app.auth.password_totp };
    match parse(flow, body, methods)? {
        Step::Begin(input) => enrol.begin(flow, client.0, &headers, input).await,
        Step::Finish { code } => enrol.finish(flow, client.0, held, &headers, code).await,
    }
}

/// The enrolment routes as parts for [`crate::http::router::build`]: `POST /auth/bootstrap`, public, under the Origin and
/// content-type checks of every change. They exist whenever the password method or the passkey method is on; with single
/// sign-on alone an account is made at its first sign-in, and the paths answer like any unknown path.
pub fn parts(enrol: &Arc<Enrol>) -> Vec<Part> {
    let auth = &enrol.services.auth.app.auth;
    if !(auth.password_totp || auth.passkey) {
        return Vec::new();
    }
    let router = Router::new().route("/auth/bootstrap", post(bootstrap_route)).with_state(enrol.clone());
    vec![Part { access: Access::Public, router }]
}
