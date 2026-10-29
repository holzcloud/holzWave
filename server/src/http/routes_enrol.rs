//! The routes that make the first administrator and every invited person: `POST /auth/bootstrap` and
//! `POST /auth/invite/redeem`.
//!
//! An enrolment is two steps and creates nothing until the second one proves the chosen method:
//!
//! - **`begin`** checks the token without spending it, the username and the password policy, makes the one derivation of the
//!   password method outside any transaction (or starts the passkey registration), and keeps the half-finished enrolment in
//!   memory behind the cookie `__Host-hw_pending`. The answer shows the authenticator secret or the creation options. No
//!   account, no row, no session and no audit line exist after it, and the token is still valid.
//! - **`finish`** checks a current code for that secret (or verifies the passkey), and only then redeems the token in one
//!   transaction of the accounts service: the account, the credential, the unlock-method rows and the first session commit
//!   together or not at all. After the commit the keys of the new account go to the unlock gate and the one answer that
//!   holds the recovery code is sent.
//!
//! The invitation route has a third step, **`peek`**, which tells a person what an invitation is for without spending it. The
//! bootstrap route has none: nobody needs to be told that the first account is an administrator.
//!
//! What the module guarantees:
//!
//! - **One refusal for every dead token.** An unknown, expired, spent or no-longer-allowed token is the same `410 gone`, byte
//!   for byte, whichever step it meets. Each such refusal counts against the client address in a throttle key space of its
//!   own; the lock answers `429` before any service is called.
//! - **The half-finished state is bounded and wiped.** At most 1,024 entries and 16 per client address (the oldest of an
//!   address goes first), five minutes of life, three wrong codes, removal on first use. The token, the derived password key
//!   and the authenticator secret are wiped when an entry is dropped.
//! - **An invitation does not add a second way in.** An invitation to an account that exists is accepted only while the
//!   account has no way to sign in, and then it keeps the account's name, role and user id whatever the request says.
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

use crate::accounts::{AccountError, Accounts, InviteTarget, NewAccount};
use crate::auth::passkey::{ChallengeToken, RegistrationSubject, VerifiedPasskey};
use crate::auth::password::{ArgonGate, PasswordError, Prepared, check_policy, prepare_password, store_prepared};
use crate::auth::session::{generate_token, parse_token};
use crate::auth::throttle::{Policy, Throttle, scoped_key};
use crate::auth::totp::{self, SECRET_BYTES};
use crate::http::router::{Access, ApiError, ApiJson, Authed, ClientAddr, MaybeAuthed, Part};
use crate::proxy::key_of;
use crate::sealed::Sealer;
use crate::store::{
    Account, HANDLE_BYTES, NewSession, Role, SignInMethod, TokenHash, UnlockKind, hash_token, valid_username,
};
use crate::unlock::{SALT_BYTES, generate_recovery_code, kek_from_prf, kek_from_recovery, random};

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

/// The longest PRF text that is looked at, in bytes. A real output is 43 characters of base64url.
const MAX_PRF_BYTES: usize = 64;

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
    /// `POST /auth/invite/redeem`: a new person, or an account that exists and has no way to sign in yet.
    Invite,
}

/// What the person proves at `finish`.
enum Proof {
    /// A code for the secret that `begin` showed, and the derived password credential.
    Password { prepared: Prepared, secret: Zeroizing<[u8; SECRET_BYTES]> },
    /// A credential for the options that `begin` sent, answering the challenge held by the passkey service.
    Passkey { challenge: ChallengeToken },
}

/// One half-finished enrolment.
struct Entry {
    flow: Flow,
    token: Zeroizing<String>,
    /// The name the account has or will have: the account's own for an existing one, the typed one otherwise.
    username: String,
    /// The display name the account has or will have.
    display_name: String,
    /// Whether the invitation is for an account that exists.
    existing: bool,
    /// The WebAuthn user handle chosen at `begin`: the account's own, or a fresh one. Passkey enrolments only.
    handle: Option<[u8; HANDLE_BYTES]>,
    proof: Proof,
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
            AccountError::Enrolment("passkey") => Refusal(ApiError::Invalid),
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
    #[serde(default)]
    credential: Option<Value>,
    #[serde(default)]
    prf: Option<Value>,
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

/// Which way a person chose to sign in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Password,
    Passkey,
}

/// What `begin` was given.
struct Begin {
    token: Zeroizing<String>,
    /// The typed name; an invitation to an account that exists ignores it.
    username: Option<String>,
    display_name: String,
    choice: Choice,
    password: Option<Zeroizing<String>>,
}

/// What `finish` was given. Which of the members is wanted depends on the half-finished enrolment, which `parse` does not know.
struct Finish {
    code: Option<Zeroizing<String>>,
    credential: Option<Value>,
    prf: Option<String>,
}

/// One step of a body, checked for shape and size before anything else looks at it.
enum Step {
    Peek { token: Zeroizing<String> },
    Begin(Begin),
    Finish(Finish),
}

/// Which methods are switched on.
#[derive(Clone, Copy)]
struct Methods {
    password: bool,
    passkey: bool,
}

fn parse(flow: Flow, body: Body, methods: Methods) -> Result<Step, ApiError> {
    let Body { step, token, username, display_name: shown, method, password, code, credential, prf } = body;
    let step = match step {
        Some(Value::String(step)) => step,
        _ => return Err(bad("step")),
    };
    let nothing = || ApiError::BadRequest { field: None };
    match step.as_str() {
        // Only an invitation is asked about; nobody needs to be told that the first account is an administrator.
        "peek" if flow == Flow::Invite => {
            if username.is_some()
                || shown.is_some()
                || method.is_some()
                || password.is_some()
                || code.is_some()
                || credential.is_some()
                || prf.is_some()
            {
                return Err(nothing());
            }
            let token = Zeroizing::new(text(token.ok_or(bad("token"))?, "token", MAX_TOKEN_BYTES)?);
            Ok(Step::Peek { token })
        }
        "begin" => {
            if code.is_some() || credential.is_some() || prf.is_some() {
                return Err(nothing());
            }
            let token = Zeroizing::new(text(token.ok_or(bad("token"))?, "token", MAX_TOKEN_BYTES)?);
            let username = match username {
                None => None,
                Some(value) => {
                    let name = text(value, "username", 64)?;
                    if !valid_username(&name) {
                        return Err(bad("username"));
                    }
                    Some(name)
                }
            };
            if username.is_none() && flow == Flow::Bootstrap {
                return Err(bad("username"));
            }
            let display_name = display_name(shown)?;
            let choice = match text(method.ok_or(bad("method"))?, "method", 16)?.as_str() {
                "password" if methods.password => Choice::Password,
                "passkey" if methods.passkey => Choice::Passkey,
                _ => return Err(bad("method")),
            };
            let password = match (choice, password) {
                (Choice::Password, Some(Value::String(password))) => {
                    let password = Zeroizing::new(password);
                    if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
                        return Err(bad("password"));
                    }
                    Some(password)
                }
                (Choice::Password, _) | (Choice::Passkey, Some(_)) => return Err(bad("password")),
                (Choice::Passkey, None) => None,
            };
            Ok(Step::Begin(Begin { token, username, display_name, choice, password }))
        }
        "finish" => {
            if token.is_some() || username.is_some() || shown.is_some() || method.is_some() || password.is_some() {
                return Err(nothing());
            }
            let code = match code {
                None => None,
                Some(value) => Some(Zeroizing::new(text(value, "code", MAX_CODE_BYTES)?)),
            };
            let credential = match credential {
                None => None,
                Some(value @ Value::Object(_)) => Some(value),
                Some(_) => return Err(bad("credential")),
            };
            let prf = match prf {
                None => None,
                Some(value) => Some(text(value, "prf", MAX_PRF_BYTES)?),
            };
            Ok(Step::Finish(Finish { code, credential, prf }))
        }
        _ => Err(bad("step")),
    }
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

/// What a peek at a token found: the role the person will get, and the account the token is for when it is for one.
struct Seen {
    role: Role,
    account: Option<InviteTarget>,
}

impl Enrol {
    /// Looks at a token without spending it. An invitation to an account that has a way to sign in already is as dead as one
    /// that was spent. Every dead token counts against the address.
    async fn look(&self, flow: Flow, client: IpAddr, token: &str, now: i64) -> Result<Seen, Refusal> {
        let accounts = self.services.accounts.clone();
        let token = Zeroizing::new(token.to_owned());
        let peeked = blocking(move || match flow {
            Flow::Bootstrap => accounts.peek_bootstrap(&token).map(|()| Seen { role: Role::Admin, account: None }),
            Flow::Invite => accounts.peek_invite(&token).map(|found| Seen { role: found.role, account: found.account }),
        })
        .await?;
        match peeked {
            Ok(seen) if seen.account.as_ref().is_some_and(|target| target.has_method) => Err(self.gone(client, now).await),
            Ok(seen) => Ok(seen),
            Err(error) => Err(self.refuse(error, client, now).await),
        }
    }

    /// The `peek` step.
    async fn peek(&self, flow: Flow, client: IpAddr, token: Zeroizing<String>) -> Result<Response, Refusal> {
        let now = self.now();
        self.check_lock(client, now).await?;
        let seen = self.look(flow, client, &token, now).await?;
        let mut body = serde_json::json!({ "role": seen.role.as_str(), "existing": seen.account.is_some() });
        if let Some(target) = &seen.account {
            body["username"] = Value::from(target.username.as_str());
        }
        Ok(answer(StatusCode::OK, &body, Vec::new()))
    }

    /// The first step.
    async fn begin(&self, flow: Flow, client: IpAddr, headers: &HeaderMap, input: Begin) -> Result<Response, Refusal> {
        let now = self.now();
        self.check_lock(client, now).await?;

        // The token is looked at without being spent.
        let seen = self.look(flow, client, &input.token, now).await?;

        // The name of an account that exists is fixed. A name that is taken is told to the holder of a valid token only.
        let existing = seen.account.is_some();
        let (username, display_name, known_handle) = match &seen.account {
            Some(target) => (target.username.clone(), target.display_name.clone(), Some(target.handle)),
            None => {
                let username = input.username.clone().ok_or(bad("username"))?;
                let store = self.services.auth.app.store.clone();
                let name = username.clone();
                let taken = blocking(move || store.account_by_username(&name)).await?.map_err(|_| ApiError::Busy)?;
                if taken.is_some() {
                    return Err(ApiError::Conflict { field: Some("username") }.into());
                }
                let shown = if input.display_name.is_empty() { username.clone() } else { input.display_name.clone() };
                (username, shown, None)
            }
        };

        let (body, proof, handle) = match input.choice {
            Choice::Password => {
                let password = input.password.as_ref().ok_or(bad("password"))?;
                check_policy(&username, password).map_err(|_| bad("password"))?;
                let prepared = match prepare_password(&self.services.argon, &username, password).await {
                    Ok(prepared) => prepared,
                    Err(PasswordError::Policy(_)) => return Err(bad("password").into()),
                    Err(_) => return Err(ApiError::Busy.into()),
                };
                let secret = totp::generate_secret().map_err(|_| ApiError::Busy)?;
                let host = self.services.auth.app.origin.host();
                let body = setup_answer(&secret[..], &username, host)?;
                (body, Proof::Password { prepared, secret }, None)
            }
            Choice::Passkey => {
                // The handle is chosen here and kept: the options carry it, and the account gets exactly it.
                let handle = match known_handle {
                    Some(handle) => handle,
                    None => random::<HANDLE_BYTES>().map_err(|_| ApiError::Busy)?,
                };
                let subject =
                    RegistrationSubject { handle, user_name: username.clone(), display_name: display_name.clone() };
                let passkeys = self.services.auth.passkeys.clone();
                let started = blocking(move || passkeys.start_registration(&subject, client)).await?;
                let (options, challenge) = started.map_err(|_| ApiError::Busy)?;
                let options: Value = serde_json::from_str(&options).map_err(|_| ApiError::Busy)?;
                let body = serde_json::json!({ "next": "passkey_register", "options": options });
                (body, Proof::Passkey { challenge }, Some(handle))
            }
        };

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
                username,
                display_name,
                existing,
                handle,
                proof,
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

    /// Puts an entry back untouched: the request was the wrong shape for it, which costs no attempt.
    fn keep(&self, hash: TokenHash, entry: Entry, now: i64, field: Option<&'static str>) -> Refusal {
        self.lock().push(hash, entry, now);
        Refusal(ApiError::BadRequest { field })
    }

    /// The second step.
    async fn finish(
        &self,
        flow: Flow,
        client: IpAddr,
        held: Option<Authed>,
        headers: &HeaderMap,
        input: Finish,
    ) -> Result<Response, Refusal> {
        let now = self.now();
        let Some(hash) = pending_hash(headers) else { return Err(ApiError::Invalid.into()) };
        let Some(entry) = self.lock().take(&hash, flow, now) else { return Err(ApiError::Invalid.into()) };
        let Finish { code, credential, prf } = input;

        // The proof: a right code for the secret, or a credential that answers the challenge. Both are checked before the
        // transaction opens. A request of the wrong shape for the enrolment costs no attempt.
        let proof = if let Proof::Password { secret, .. } = &entry.proof {
            if credential.is_some() || prf.is_some() {
                return Err(self.keep(hash, entry, now, None));
            }
            let Some(code) = code else { return Err(self.keep(hash, entry, now, Some("code"))) };
            match totp::check_candidate(&secret[..], &code, now) {
                Ok(step) => Verified::Password { step },
                Err(_) => {
                    self.put_back(hash, entry, now);
                    return Err(ApiError::Invalid.into());
                }
            }
        } else if let Proof::Passkey { challenge } = &entry.proof {
            if code.is_some() {
                return Err(self.keep(hash, entry, now, None));
            }
            let Some(credential) = credential else { return Err(self.keep(hash, entry, now, Some("credential"))) };
            let passkeys = self.services.auth.passkeys.clone();
            let challenge = Zeroizing::new(challenge.expose().to_owned());
            let credential = Zeroizing::new(credential.to_string());
            let verified = blocking(move || passkeys.verify_registration(&challenge, &credential, prf.as_deref())).await?;
            // The challenge is used up whatever the answer: a wrong response starts the enrolment again.
            match verified {
                Ok(verified) => Verified::Passkey { verified: Box::new(verified) },
                Err(_) => return Err(ApiError::Invalid.into()),
            }
        } else {
            return Err(ApiError::Invalid.into());
        };

        let Entry { token, username, display_name, existing, handle, proof: held_proof, .. } = entry;

        // Everything the transaction needs is made before it opens.
        let recovery = generate_recovery_code().map_err(|_| ApiError::Busy)?;
        let recovery_salt = random::<SALT_BYTES>().map_err(|_| ApiError::Busy)?;
        let recovery_kek = kek_from_recovery(&recovery, &recovery_salt).map_err(|_| ApiError::Busy)?;
        let recovery_text = recovery.display();
        let (session_token, session_bytes) = generate_token().map_err(|_| ApiError::Busy)?;
        let session_cookie = cookies::session(session_token.expose()).ok_or(ApiError::Busy)?;
        let session_hash = hash_token(&session_bytes[..]);
        drop(session_bytes);
        let device =
            device_of(headers.get(header::USER_AGENT).and_then(|value| value.to_str().ok()).unwrap_or(""));
        let absolute = i64::try_from(self.services.auth.app.sessions.absolute.as_secs()).unwrap_or(i64::MAX);
        let method = match &proof {
            Verified::Password { .. } => SignInMethod::PasswordTotp,
            Verified::Passkey { .. } => SignInMethod::Passkey,
        };

        let accounts = self.services.accounts.clone();
        let sealer = self.services.sealer.clone();
        let passkeys = self.services.auth.passkeys.clone();
        let redeemed = blocking(move || {
            let mut keys: Vec<ProvisionKey> = Vec::new();
            let new = NewAccount { username: &username, display_name: &display_name, handle };
            let enrol = |tx: &crate::store::Tx<'_>, account: &Account| -> Result<(), AccountError> {
                // An invitation to an account that exists does not add a second way in, whatever happened since `begin`.
                if existing && Accounts::has_method(tx, account.id)? {
                    return Err(AccountError::NotRedeemable);
                }
                match (&held_proof, &proof) {
                    (Proof::Password { prepared, secret }, Verified::Password { step }) => {
                        store_prepared(tx, account.id, prepared, now)
                            .map_err(|_| fault("the password could not be stored"))?;
                        totp::store_secret(tx, &sealer, account.id, &secret[..], *step)
                            .map_err(|_| fault("the authenticator could not be stored"))?;
                        tx.unlock_add(account.id, UnlockKind::Password, prepared.salt(), prepared.params(), None)
                            .map_err(|_| fault("the unlock methods could not be stored"))?;
                        keys.push(ProvisionKey::new(UnlockKind::Password, None, prepared.kek()));
                    }
                    (Proof::Passkey { .. }, Verified::Passkey { verified }) => {
                        let row = passkeys
                            .store_verified(tx, account.id, verified, "")
                            .map_err(|_| AccountError::Enrolment("passkey"))?;
                        if let Some(output) = verified.prf() {
                            let kek = kek_from_prf(output.expose(), &row.prf_salt)
                                .map_err(|_| fault("the unlock methods could not be stored"))?;
                            tx.unlock_add(
                                account.id,
                                UnlockKind::Prf,
                                &row.prf_salt,
                                HKDF_PARAMS,
                                Some(&row.credential_id),
                            )
                            .map_err(|_| fault("the unlock methods could not be stored"))?;
                            keys.push(ProvisionKey::new(UnlockKind::Prf, Some(row.credential_id.clone()), &kek));
                        }
                    }
                    _ => return Err(fault("the proof does not belong to the enrolment")),
                }
                tx.unlock_add(account.id, UnlockKind::Recovery, &recovery_salt, HKDF_PARAMS, None)
                    .map_err(|_| fault("the unlock methods could not be stored"))?;
                keys.push(ProvisionKey::new(UnlockKind::Recovery, None, &recovery_kek));
                tx.session_create(&NewSession {
                    id_hash: session_hash,
                    account_id: account.id,
                    method,
                    now,
                    absolute_expires: now.saturating_add(absolute),
                    device: &device,
                    addr: client,
                })
                .map_err(|_| fault("the session could not be made"))?;
                Ok(())
            };
            let account = match flow {
                Flow::Bootstrap => accounts.redeem_bootstrap(&token, &new, Some(client), enrol),
                Flow::Invite => accounts.redeem_invite(&token, &new, Some(client), enrol),
            }?;
            Ok::<(Account, Vec<ProvisionKey>), AccountError>((account, keys))
        })
        .await?;
        let (account, keys) = match redeemed {
            Ok(done) => done,
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

/// What was proven at `finish`.
enum Verified {
    /// A right code, and the step it belongs to.
    Password { step: i64 },
    /// A credential that answers the challenge, not stored yet.
    Passkey { verified: Box<VerifiedPasskey> },
}

/// The error of an enrolment that failed inside the transaction: a fixed sentence, which rolls everything back.
fn fault(sentence: &'static str) -> AccountError {
    AccountError::Enrolment(sentence)
}

// ---------------------------------------------------------------------------------------------------------------------
// The routes
// ---------------------------------------------------------------------------------------------------------------------

async fn handle(
    enrol: Arc<Enrol>,
    flow: Flow,
    client: IpAddr,
    held: Option<Authed>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Refusal> {
    let auth = &enrol.services.auth.app.auth;
    let methods = Methods { password: auth.password_totp, passkey: auth.passkey };
    match parse(flow, body, methods)? {
        Step::Peek { token } => enrol.peek(flow, client, token).await,
        Step::Begin(input) => enrol.begin(flow, client, &headers, input).await,
        Step::Finish(input) => enrol.finish(flow, client, held, &headers, input).await,
    }
}

async fn bootstrap_route(
    State(enrol): State<Arc<Enrol>>,
    Extension(client): Extension<ClientAddr>,
    MaybeAuthed(held): MaybeAuthed,
    headers: HeaderMap,
    ApiJson(Object(body)): ApiJson<Object<Body>>,
) -> Result<Response, Refusal> {
    handle(enrol, Flow::Bootstrap, client.0, held, headers, body).await
}

async fn invite_route(
    State(enrol): State<Arc<Enrol>>,
    Extension(client): Extension<ClientAddr>,
    MaybeAuthed(held): MaybeAuthed,
    headers: HeaderMap,
    ApiJson(Object(body)): ApiJson<Object<Body>>,
) -> Result<Response, Refusal> {
    handle(enrol, Flow::Invite, client.0, held, headers, body).await
}

/// The enrolment routes as parts for [`crate::http::router::build`]: `POST /auth/bootstrap` and `POST /auth/invite/redeem`,
/// public, under the Origin and content-type checks of every change. They exist whenever the password method or the passkey
/// method is on; with single sign-on alone an account is made at its first sign-in, and the paths answer like any unknown
/// path.
pub fn parts(enrol: &Arc<Enrol>) -> Vec<Part> {
    let auth = &enrol.services.auth.app.auth;
    if !(auth.password_totp || auth.passkey) {
        return Vec::new();
    }
    let router = Router::new()
        .route("/auth/bootstrap", post(bootstrap_route))
        .route("/auth/invite/redeem", post(invite_route))
        .with_state(enrol.clone());
    vec![Part { access: Access::Public, router }]
}
