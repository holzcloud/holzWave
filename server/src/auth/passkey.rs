//! Passkey (WebAuthn) registration and sign-in ceremonies.
//!
//! This is the only code that talks to `webauthn-rs`; nothing here parses or verifies a WebAuthn message by hand. The module
//! adds what a server needs around the library: the relying party taken from the public address, a store of open challenges
//! that are used once, the accounts, the audit lines and the session.
//!
//! A registration is two calls ([`Passkeys::start_registration`], [`Passkeys::finish_registration`]) and a sign-in is two
//! calls ([`Passkeys::start_sign_in`], [`Passkeys::finish_sign_in`]); the sign-in needs no user name, because the
//! authenticator names the account by the user handle it returns. A session is made in one place only, at the end of a
//! verified sign-in assertion, and never by a registration.
//!
//! Everything the browser sends is untrusted and is looked at in order: the size, then the challenge token, then the
//! library. Every refusal of a sign-in is the one [`PasskeyError::Invalid`] and one audit line, whatever the cause. The
//! values that carry key material ([`PrfOutput`], [`PrfMaterial`]) and the tokens print a marker in `Debug`, cannot be
//! cloned and are wiped when dropped.

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use webauthn_rs::prelude::{
    DiscoverableAuthentication, DiscoverableKey, Passkey, PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder,
};
use zeroize::Zeroizing;

use crate::audit::{Action, Actor, Audit, DetailKey, DetailValue, Event, Outcome};
use crate::auth::session::{Created, Method, SessionManager};
use crate::clock::Clock;
use crate::config::{self, Refusal};
use crate::log::{self, Level};
use crate::store::{
    AccountId, AccountState, HANDLE_BYTES, MAX_PASSKEYS, Session, Store, StoreError, TokenHash, Tx, hash_token,
};
use crate::unlock::random;

/// How long an open challenge lives, in seconds.
pub const CHALLENGE_TTL_SECS: i64 = 300;

/// The most challenges that are open at once. The one after them pushes out the oldest.
pub const MAX_CHALLENGES: usize = 1_024;

/// The most challenges one client address has open at once. The one after them pushes out that address's oldest.
pub const MAX_CHALLENGES_PER_ADDRESS: usize = 16;

/// The longest credential JSON that is looked at, in bytes. A longer text is refused before it is parsed.
pub const MAX_CREDENTIAL_JSON: usize = 32 * 1024;

/// The characters of a challenge token.
pub const TOKEN_CHARS: usize = 43;

/// The bytes of a PRF output and of a stored PRF salt.
pub const PRF_BYTES: usize = 32;

/// The name a passkey gets when the person gave none that is usable.
pub const DEFAULT_NAME: &str = "Passkey";

/// What the PRF evaluation input is derived from, together with the relying party id.
const PRF_LABEL: &[u8] = b"holzwave passkey prf input v1";

/// The version of the stored form of a passkey row.
const STORED_VERSION: u8 = 1;

const TOKEN_DEBUG: &str = "ChallengeToken(..)";
const PRF_DEBUG: &str = "PrfOutput(..)";
const MATERIAL_DEBUG: &str = "PrfMaterial(..)";
const VERIFIED_DEBUG: &str = "VerifiedPasskey(..)";

/// The method of every session this module makes.
const METHOD: Method = Method::Passkey;

/// Why a call was refused. No text holds a credential, a challenge, a PRF value, a name or a session value, and the text of
/// [`PasskeyError::Invalid`] is the same whatever the cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasskeyError {
    /// The response, the token or the credential is not acceptable. One error for every reason.
    Invalid,
    /// The change would leave the account with no way to sign in, or the credential exists already.
    Conflict,
    /// The account has the most passkeys already.
    TooMany,
    /// There is no such passkey among the account's.
    NotFound,
    /// Something failed that the person cannot fix.
    Internal,
}

impl fmt::Display for PasskeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PasskeyError::Invalid => "the passkey was not accepted",
            PasskeyError::Conflict => "that passkey change is not possible",
            PasskeyError::TooMany => "an account can have at most 10 passkeys",
            PasskeyError::NotFound => "there is no such passkey",
            PasskeyError::Internal => "the passkey could not be processed",
        })
    }
}

impl std::error::Error for PasskeyError {}

impl From<StoreError> for PasskeyError {
    fn from(error: StoreError) -> PasskeyError {
        match error {
            StoreError::PasskeyLimit => PasskeyError::TooMany,
            StoreError::Conflict => PasskeyError::Conflict,
            StoreError::NotFound | StoreError::Invalid(_) => PasskeyError::Invalid,
            _ => PasskeyError::Internal,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Tokens and key material
// ---------------------------------------------------------------------------------------------------------------------

/// The value that names an open challenge: 32 random bytes as 43 base64url characters. Only its SHA-256 is kept. Not
/// `Clone`, a marker for `Debug`, wiped when dropped.
pub struct ChallengeToken(Zeroizing<String>);

impl ChallengeToken {
    /// The 43 characters.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ChallengeToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(TOKEN_DEBUG)
    }
}

/// A new token and the hash it is kept under.
fn new_token() -> Result<(ChallengeToken, TokenHash), PasskeyError> {
    let bytes = Zeroizing::new(random::<32>().map_err(|_| PasskeyError::Internal)?);
    let token = ChallengeToken(Zeroizing::new(URL_SAFE_NO_PAD.encode(&bytes[..])));
    Ok((token, hash_token(&bytes[..])))
}

/// The hash a token text is kept under, or `None` for a text that is not 43 characters of the base64url alphabet that decode
/// to 32 bytes.
fn token_hash(text: &str) -> Option<TokenHash> {
    if text.len() != TOKEN_CHARS || !text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return None;
    }
    let mut bytes = Zeroizing::new([0u8; 32]);
    let written = URL_SAFE_NO_PAD.decode_slice(text.as_bytes(), &mut bytes[..]).ok()?;
    (written == 32).then(|| hash_token(&bytes[..]))
}

/// The 32-byte result of the PRF extension that the browser sent. Not `Clone`, a marker for `Debug`, wiped when dropped.
pub struct PrfOutput(Zeroizing<[u8; PRF_BYTES]>);

impl PrfOutput {
    /// The bytes, for the code that turns them into a key.
    pub fn expose(&self) -> &[u8; PRF_BYTES] {
        &self.0
    }
}

impl fmt::Debug for PrfOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PRF_DEBUG)
    }
}

/// What a passkey sign-in hands to the unlock code: the credential id, the PRF output and the credential's own salt. Not
/// `Clone`, a marker for `Debug`, wiped when dropped (the output).
pub struct PrfMaterial {
    credential_id: Vec<u8>,
    output: PrfOutput,
    salt: Vec<u8>,
}

impl PrfMaterial {
    /// The credential the output belongs to.
    pub fn credential_id(&self) -> &[u8] {
        &self.credential_id
    }

    /// The PRF output.
    pub fn output(&self) -> &PrfOutput {
        &self.output
    }

    /// The random salt stored with the credential, for the key derivation.
    pub fn salt(&self) -> &[u8] {
        &self.salt
    }
}

impl fmt::Debug for PrfMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MATERIAL_DEBUG)
    }
}

/// Reads the `prf` value the page sent: exactly 32 bytes as 43 characters of unpadded base64url. Anything else is `None`,
/// which means "no unlock material"; it is never an error, because the sign-in itself does not depend on it.
pub fn read_prf(value: Option<&str>) -> Option<PrfOutput> {
    let text = value?;
    if text.len() != TOKEN_CHARS || !text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return None;
    }
    let mut bytes = Zeroizing::new([0u8; PRF_BYTES]);
    let written = URL_SAFE_NO_PAD.decode_slice(text.as_bytes(), &mut bytes[..]).ok()?;
    (written == PRF_BYTES).then(|| PrfOutput(bytes))
}

/// The PRF evaluation input of a relying party: SHA-256 over the fixed label, a zero byte and the relying party id. It is the
/// same for every credential, so a username-less sign-in can ask for it before the credential is known; the credential's
/// own secret makes the result unique.
pub fn prf_input(rp_id: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(PRF_LABEL);
    hash.update([0u8]);
    hash.update(rp_id.as_bytes());
    hash.finalize().into()
}

/// Adds the PRF request `extensions.prf.eval.first` to the options the library produced, keeping whatever it put in
/// `extensions`. Works on the JSON text the page will receive, because the library's own types drop the request. Answers
/// whether the options had the shape of options.
pub fn add_prf_request(options: &mut Value, input: &[u8; 32]) -> bool {
    let Some(public_key) = options.get_mut("publicKey").and_then(Value::as_object_mut) else {
        return false;
    };
    let extensions = public_key.entry("extensions").or_insert_with(|| serde_json::json!({}));
    let Some(extensions) = extensions.as_object_mut() else {
        return false;
    };
    extensions.insert("prf".to_owned(), serde_json::json!({ "eval": { "first": URL_SAFE_NO_PAD.encode(input) } }));
    true
}

// ---------------------------------------------------------------------------------------------------------------------
// The stored form
// ---------------------------------------------------------------------------------------------------------------------

/// What a `passkeys.passkey_json` holds: the library's credential with the name and the time of last use, which the table has
/// no columns for.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    v: u8,
    name: String,
    last_used: Option<i64>,
    passkey: Passkey,
}

impl Stored {
    fn parse(text: &str) -> Option<Stored> {
        let stored: Stored = serde_json::from_str(text).ok()?;
        (stored.v == STORED_VERSION).then_some(stored)
    }

    fn text(&self) -> Result<String, PasskeyError> {
        serde_json::to_string(self).map_err(|_| PasskeyError::Internal)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The ceremonies' public types
// ---------------------------------------------------------------------------------------------------------------------

/// Who a passkey is registered for. The module does not look this up and does not make an account.
#[derive(Debug, Clone)]
pub struct RegistrationSubject {
    /// The 16-byte WebAuthn user handle of the account (the handle the account will have, when it does not exist yet).
    pub handle: [u8; HANDLE_BYTES],
    /// The sign-in name, shown by the authenticator.
    pub user_name: String,
    /// The name shown to people.
    pub display_name: String,
}

/// A registration that was verified and not stored yet. Not `Clone`, a marker for `Debug`.
pub struct VerifiedPasskey {
    passkey: Passkey,
    credential_id: Vec<u8>,
    prf: Option<PrfOutput>,
}

impl VerifiedPasskey {
    /// The credential id of the new passkey.
    pub fn credential_id(&self) -> &[u8] {
        &self.credential_id
    }

    /// The PRF output the browser sent with it, when it was a valid one.
    pub fn prf(&self) -> Option<&PrfOutput> {
        self.prf.as_ref()
    }

    /// Takes the PRF output out.
    pub fn take_prf(&mut self) -> Option<PrfOutput> {
        self.prf.take()
    }
}

impl fmt::Debug for VerifiedPasskey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(VERIFIED_DEBUG)
    }
}

/// The facts of a row that was just stored. The `Debug` text prints the row id and nothing else.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredRow {
    /// The row id.
    pub id: i64,
    /// The credential id.
    pub credential_id: Vec<u8>,
    /// The new random 32-byte salt of the credential.
    pub prf_salt: Vec<u8>,
}

impl fmt::Debug for StoredRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredRow").field("id", &self.id).finish_non_exhaustive()
    }
}

/// What a finished registration hands back. The `Debug` text prints the row id and nothing else.
pub struct Registered {
    /// The row id.
    pub id: i64,
    /// The credential id.
    pub credential_id: Vec<u8>,
    /// The salt stored with the credential.
    pub prf_salt: Vec<u8>,
    /// The PRF output, when the browser sent a valid one.
    pub prf: Option<PrfOutput>,
}

impl fmt::Debug for Registered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registered").field("id", &self.id).finish_non_exhaustive()
    }
}

/// What a finished sign-in hands back.
#[derive(Debug)]
pub struct SignedIn {
    /// The new session and the value for its cookie.
    pub session: Created,
    /// The account.
    pub account: AccountId,
    /// The unlock material, when the browser sent a valid PRF output.
    pub prf: Option<PrfMaterial>,
}

/// One passkey as the account dialog shows it. No credential bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasskeyInfo {
    /// The row id.
    pub id: i64,
    /// The name.
    pub name: String,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds of the last sign-in with it, or `None`.
    pub last_used: Option<i64>,
}

// ---------------------------------------------------------------------------------------------------------------------
// The open challenges
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Registration,
    SignIn,
    StepUp,
}

enum Ceremony {
    Registration { state: PasskeyRegistration, account: Option<AccountId> },
    SignIn(DiscoverableAuthentication),
    StepUp { state: PasskeyAuthentication, account: AccountId },
}

impl Ceremony {
    fn kind(&self) -> Kind {
        match self {
            Ceremony::Registration { .. } => Kind::Registration,
            Ceremony::SignIn(_) => Kind::SignIn,
            Ceremony::StepUp { .. } => Kind::StepUp,
        }
    }
}

struct Entry {
    ceremony: Ceremony,
    created: i64,
}

#[derive(Default)]
struct Challenges {
    map: HashMap<TokenHash, Entry>,
}

impl Challenges {
    fn insert(&mut self, hash: TokenHash, entry: Entry) {
        self.map.insert(hash, entry);
    }

    /// Removes the entry of a token whatever happens next; answers it when it is within its time.
    fn take(&mut self, hash: &TokenHash, now: i64) -> Option<Entry> {
        let entry = self.map.remove(hash)?;
        (now <= entry.created.saturating_add(CHALLENGE_TTL_SECS)).then_some(entry)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------------------------------------------------

/// Passkey registration and sign-in over a relying party.
pub struct Passkeys {
    webauthn: Webauthn,
    rp_id: String,
    origin: String,
    prf_input: [u8; 32],
    store: Arc<Store>,
    sessions: Arc<SessionManager>,
    audit: Arc<Audit>,
    clock: Arc<dyn Clock>,
    challenges: Mutex<Challenges>,
    verifications: AtomicU64,
}

impl fmt::Debug for Passkeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Passkeys(..)")
    }
}

impl Passkeys {
    /// The service for the public address `public_url`. The relying party id is the host and the origin is exactly the
    /// address's origin. An address that cannot be a relying party is refused with a sentence: not `https`, an IP address as
    /// the host, a host without a dot, a path, a query or user information.
    pub fn new(
        public_url: &str,
        rp_name: &str,
        store: Arc<Store>,
        sessions: Arc<SessionManager>,
        audit: Arc<Audit>,
        clock: Arc<dyn Clock>,
    ) -> Result<Passkeys, Refusal> {
        let origin = config::public_origin(public_url)?;
        if origin.host_is_ip() {
            return Err(Refusal::new("passkeys need a host name, not an IP address, in the public address"));
        }
        if !origin.host().contains('.') {
            return Err(Refusal::new("passkeys need a host name with a dot in the public address"));
        }
        let url = Url::parse(origin.as_str()).map_err(|_| Refusal::new("the public address cannot be a relying party"))?;
        let rp_name = if rp_name.trim().is_empty() { "holzWave" } else { rp_name.trim() };
        let webauthn = WebauthnBuilder::new(origin.host(), &url)
            .map_err(|_| Refusal::new("the public address cannot be a relying party"))?
            .rp_name(rp_name)
            .build()
            .map_err(|_| Refusal::new("the public address cannot be a relying party"))?;
        Ok(Passkeys {
            webauthn,
            rp_id: origin.host().to_owned(),
            origin: origin.as_str().to_owned(),
            prf_input: prf_input(origin.host()),
            store,
            sessions,
            audit,
            clock,
            challenges: Mutex::new(Challenges::default()),
            verifications: AtomicU64::new(0),
        })
    }

    /// The relying party id: the host of the public address.
    pub fn rp_id(&self) -> &str {
        &self.rp_id
    }

    /// The one origin a response must name.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The PRF evaluation input the options carry.
    pub fn prf_input(&self) -> &[u8; 32] {
        &self.prf_input
    }

    /// How many times a credential was handed to the library so far. For tests and metrics.
    pub fn library_calls(&self) -> u64 {
        self.verifications.load(Ordering::SeqCst)
    }

    /// How many challenges are open now.
    pub fn open_challenges(&self) -> usize {
        self.lock().map.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Challenges> {
        self.challenges.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn record(&self, event: &Event) {
        if self.audit.record(event).is_err() {
            log::event(Level::Error, "passkey.audit_failed", &[]);
        }
    }

    fn open(&self, ceremony: Ceremony, now: i64) -> Result<ChallengeToken, PasskeyError> {
        let (token, hash) = new_token()?;
        self.lock().insert(hash, Entry { ceremony, created: now });
        Ok(token)
    }

    /// The ceremony of a token, removed from the map whatever its kind and whatever happens next.
    fn take(&self, token: &str, kind: Kind, now: i64) -> Option<Ceremony> {
        let hash = token_hash(token)?;
        let entry = self.lock().take(&hash, now)?;
        (entry.ceremony.kind() == kind).then_some(entry.ceremony)
    }

    // ---- registration ----

    /// Starts a registration for `subject`: the creation options as JSON text for the browser and the token of the open
    /// challenge. Attestation is none, user verification is required, a discoverable credential is preferred, the
    /// credentials the account has already are excluded and the PRF request is added to `extensions`.
    pub fn start_registration(
        &self,
        subject: &RegistrationSubject,
        _addr: IpAddr,
    ) -> Result<(String, ChallengeToken), PasskeyError> {
        let now = self.clock.unix();
        let (account, existing) = self.store.read(|tx| {
            let Some(found) = tx.account_by_handle(&subject.handle)?.filter(|a| a.state == AccountState::Active) else {
                return Ok((None, Vec::new()));
            };
            let ids = tx.passkey_list(found.id)?.into_iter().map(|row| row.credential_id).collect();
            Ok((Some(found.id), ids))
        })?;
        if existing.len() >= MAX_PASSKEYS {
            return Err(PasskeyError::TooMany);
        }
        let exclude = (!existing.is_empty()).then_some(existing);
        let (options, state) = self
            .webauthn
            .start_passkey_registration(
                Uuid::from_bytes(subject.handle),
                &subject.user_name,
                &subject.display_name,
                exclude,
            )
            .map_err(|_| PasskeyError::Internal)?;
        let mut json = serde_json::to_value(&options).map_err(|_| PasskeyError::Internal)?;
        if let Some(selection) = json.pointer_mut("/publicKey/authenticatorSelection").and_then(Value::as_object_mut) {
            selection.insert("residentKey".to_owned(), Value::from("preferred"));
        }
        if !add_prf_request(&mut json, &self.prf_input) {
            return Err(PasskeyError::Internal);
        }
        let token = self.open(Ceremony::Registration { state, account }, now)?;
        Ok((json.to_string(), token))
    }

    /// The first half of a registration: consumes the token, verifies the response through the library and answers the
    /// verified credential. Nothing is stored and nothing is audited; the caller stores it with [`Passkeys::store_verified`]
    /// inside a transaction of its own and writes its own line.
    pub fn verify_registration(
        &self,
        token: &str,
        credential_json: &str,
        prf: Option<&str>,
    ) -> Result<VerifiedPasskey, PasskeyError> {
        self.verify_registration_for(token, credential_json, prf).map(|(verified, _)| verified)
    }

    fn verify_registration_for(
        &self,
        token: &str,
        credential_json: &str,
        prf: Option<&str>,
    ) -> Result<(VerifiedPasskey, Option<AccountId>), PasskeyError> {
        let now = self.clock.unix();
        let taken = self.take(token, Kind::Registration, now);
        let Some(Ceremony::Registration { state, account }) = taken else {
            return Err(PasskeyError::Invalid);
        };
        if credential_json.len() > MAX_CREDENTIAL_JSON {
            return Err(PasskeyError::Invalid);
        }
        self.verifications.fetch_add(1, Ordering::SeqCst);
        let credential: RegisterPublicKeyCredential =
            serde_json::from_str(credential_json).map_err(|_| PasskeyError::Invalid)?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(&credential, &state)
            .map_err(|_| PasskeyError::Invalid)?;
        let credential_id = passkey.cred_id().to_vec();
        Ok((VerifiedPasskey { passkey, credential_id, prf: read_prf(prf) }, account))
    }

    /// The second half of a registration: stores a verified credential inside the caller's transaction `tx`, with a fresh
    /// random 32-byte salt. The limit of 10 per account and the name policy apply. No audit line is written; the caller's own
    /// action writes it.
    pub fn store_verified(
        &self,
        tx: &Tx<'_>,
        account: AccountId,
        verified: &VerifiedPasskey,
        name: &str,
    ) -> Result<StoredRow, PasskeyError> {
        let now = self.clock.unix();
        let salt = random::<PRF_BYTES>().map_err(|_| PasskeyError::Internal)?;
        let stored = Stored {
            v: STORED_VERSION,
            name: clean_name(name),
            last_used: None,
            passkey: verified.passkey.clone(),
        };
        let row = tx.passkey_add(account, &verified.credential_id, &stored.text()?, Some(&salt), now)?;
        Ok(StoredRow { id: row.id, credential_id: row.credential_id, prf_salt: salt.to_vec() })
    }

    /// Finishes a registration of an account that exists: [`Passkeys::verify_registration`] and
    /// [`Passkeys::store_verified`] in sequence, and one `auth.passkey.add` line. A challenge that was started for an account
    /// that did not exist is refused here.
    pub fn finish_registration(
        &self,
        token: &str,
        credential_json: &str,
        prf: Option<&str>,
        name: &str,
        addr: IpAddr,
    ) -> Result<Registered, PasskeyError> {
        let outcome = self.finish_registration_inner(token, credential_json, prf, name);
        match &outcome {
            Ok((_, account)) => self.record(
                &Event::new(Action::AuthPasskeyAdd, Outcome::Ok, Actor::Account(*account), Some(addr)).target(*account),
            ),
            Err(error) => {
                if matches!(error, PasskeyError::Internal) {
                    log::event(Level::Error, "passkey.store_failed", &[]);
                } else {
                    self.record(
                        &Event::new(Action::AuthPasskeyAdd, Outcome::Fail, Actor::Anonymous, Some(addr))
                            .detail(DetailKey::Reason, DetailValue::Text(reason(*error))),
                    );
                }
            }
        }
        outcome.map(|(registered, _)| registered)
    }

    fn finish_registration_inner(
        &self,
        token: &str,
        credential_json: &str,
        prf: Option<&str>,
        name: &str,
    ) -> Result<(Registered, AccountId), PasskeyError> {
        let (mut verified, account) = self.verify_registration_for(token, credential_json, prf)?;
        let account = account.ok_or(PasskeyError::Invalid)?;
        let row = self.store.write(|tx| Ok(self.store_verified(tx, account, &verified, name)))??;
        Ok((
            Registered { id: row.id, credential_id: row.credential_id, prf_salt: row.prf_salt, prf: verified.take_prf() },
            account,
        ))
    }

    // ---- sign-in ----

    /// Starts a username-less sign-in: discoverable-credential request options with the PRF input added, as JSON text, and
    /// the token of the open challenge.
    pub fn start_sign_in(&self, _addr: IpAddr) -> Result<(String, ChallengeToken), PasskeyError> {
        let now = self.clock.unix();
        let (options, state) =
            self.webauthn.start_discoverable_authentication().map_err(|_| PasskeyError::Internal)?;
        let mut json = serde_json::to_value(&options).map_err(|_| PasskeyError::Internal)?;
        if !add_prf_request(&mut json, &self.prf_input) {
            return Err(PasskeyError::Internal);
        }
        let token = self.open(Ceremony::SignIn(state), now)?;
        Ok((json.to_string(), token))
    }

    /// Finishes a sign-in: identifies the account from the user handle the authenticator returned, checks that it is active,
    /// verifies the assertion through the library, updates the stored credential and makes the session. Every failure is
    /// [`PasskeyError::Invalid`] with one `signin.fail` line of method `passkey`; a success is one `signin.ok` line.
    pub fn finish_sign_in(
        &self,
        token: &str,
        credential_json: &str,
        prf: Option<&str>,
        addr: IpAddr,
        device: &str,
    ) -> Result<SignedIn, PasskeyError> {
        let now = self.clock.unix();
        let taken = self.take(token, Kind::SignIn, now);
        match self.sign_in_inner(taken, credential_json, prf, now, addr, device) {
            Ok(signed_in) => {
                self.record(
                    &Event::new(Action::SigninOk, Outcome::Ok, Actor::Account(signed_in.account), Some(addr))
                        .detail(DetailKey::Method, DetailValue::Text(METHOD.as_str())),
                );
                Ok(signed_in)
            }
            Err(PasskeyError::Internal) => {
                log::event(Level::Error, "passkey.signin_failed", &[]);
                Err(PasskeyError::Internal)
            }
            Err(error) => {
                self.record(
                    &Event::new(Action::SigninFail, Outcome::Fail, Actor::Anonymous, Some(addr))
                        .detail(DetailKey::Method, DetailValue::Text(METHOD.as_str()))
                        .detail(DetailKey::Reason, DetailValue::Text("invalid")),
                );
                Err(error)
            }
        }
    }

    fn sign_in_inner(
        &self,
        taken: Option<Ceremony>,
        credential_json: &str,
        prf: Option<&str>,
        now: i64,
        addr: IpAddr,
        device: &str,
    ) -> Result<SignedIn, PasskeyError> {
        let Some(Ceremony::SignIn(state)) = taken else {
            return Err(PasskeyError::Invalid);
        };
        if credential_json.len() > MAX_CREDENTIAL_JSON {
            return Err(PasskeyError::Invalid);
        }
        self.verifications.fetch_add(1, Ordering::SeqCst);
        let credential: PublicKeyCredential =
            serde_json::from_str(credential_json).map_err(|_| PasskeyError::Invalid)?;
        let (uuid, credential_id) = self
            .webauthn
            .identify_discoverable_authentication(&credential)
            .map_err(|_| PasskeyError::Invalid)?;
        let handle: [u8; HANDLE_BYTES] = *uuid.as_bytes();
        let credential_id = credential_id.to_vec();

        // Read, verify and update in one write transaction: two sign-ins on the same credential are serialized by the
        // store, so the second sees the counter the first wrote.
        let (account, salt) = self.store.write(|tx| {
            let Some(account) = tx.account_by_handle(&handle)?.filter(|a| a.state == AccountState::Active) else {
                return Ok(Err(PasskeyError::Invalid));
            };
            let Some(row) = tx.passkey_by_credential(&credential_id)?.filter(|r| r.account_id == account.id) else {
                return Ok(Err(PasskeyError::Invalid));
            };
            let Some(mut stored) = Stored::parse(&row.passkey_json) else {
                return Ok(Err(PasskeyError::Invalid));
            };
            if stored.passkey.cred_id().as_slice() != row.credential_id.as_slice() {
                return Ok(Err(PasskeyError::Invalid));
            }
            let keys = [DiscoverableKey::from(&stored.passkey)];
            let Ok(result) = self.webauthn.finish_discoverable_authentication(&credential, state, &keys) else {
                return Ok(Err(PasskeyError::Invalid));
            };
            if stored.passkey.update_credential(&result).is_none() {
                return Ok(Err(PasskeyError::Invalid));
            }
            stored.last_used = Some(now);
            let Ok(text) = stored.text() else {
                return Ok(Err(PasskeyError::Internal));
            };
            tx.passkey_update_json(account.id, row.id, &text)?;
            Ok(Ok((account.id, row.prf_salt)))
        })??;

        let session = self.sessions.create(account, METHOD, now, device, Some(addr)).map_err(|error| match error {
            crate::auth::session::SessionError::AccountDisabled => PasskeyError::Invalid,
            _ => PasskeyError::Internal,
        })?;
        let material = read_prf(prf).and_then(|output| {
            salt.map(|salt| PrfMaterial { credential_id: credential_id.clone(), output, salt })
        });
        Ok(SignedIn { session, account, prf: material })
    }

    // ---- management ----

    /// The passkeys of an account: id, name, created time and last-used time, and nothing else. A row whose stored form
    /// cannot be read is listed under the default name so that it can still be removed.
    pub fn list(&self, account: AccountId) -> Result<Vec<PasskeyInfo>, PasskeyError> {
        let rows = self.store.read(|tx| tx.passkey_list(account))?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let stored = Stored::parse(&row.passkey_json);
                PasskeyInfo {
                    id: row.id,
                    name: stored.as_ref().map_or_else(|| DEFAULT_NAME.to_owned(), |s| s.name.clone()),
                    created_at: row.created_at,
                    last_used: stored.and_then(|s| s.last_used),
                }
            })
            .collect())
    }

    /// Removes one passkey of `account`. An id that is not the account's is [`PasskeyError::NotFound`] with no effect and no
    /// line. The last way to sign in cannot be removed ([`PasskeyError::Conflict`], one failed line): an account keeps at
    /// least one of a password with an authenticator, another passkey and a single sign-on identity. A removal writes one
    /// `auth.passkey.remove` line.
    pub fn remove(&self, account: AccountId, id: i64, addr: Option<IpAddr>) -> Result<(), PasskeyError> {
        let outcome = self.store.write(|tx| {
            let rows = tx.passkey_list(account)?;
            if !rows.iter().any(|row| row.id == id) {
                return Ok(Err(PasskeyError::NotFound));
            }
            let another_passkey = rows.iter().any(|row| row.id != id);
            let password_with_authenticator = tx.password_get(account)?.is_some() && tx.totp_get(account)?.is_some();
            let identity = !tx.oidc_list(account)?.is_empty();
            if !(another_passkey || password_with_authenticator || identity) {
                return Ok(Err(PasskeyError::Conflict));
            }
            tx.passkey_remove(account, id)?;
            Ok(Ok(()))
        })?;
        match outcome {
            Ok(()) => {
                self.record(
                    &Event::new(Action::AuthPasskeyRemove, Outcome::Ok, Actor::Account(account), addr).target(account),
                );
                Ok(())
            }
            Err(PasskeyError::Conflict) => {
                self.record(
                    &Event::new(Action::AuthPasskeyRemove, Outcome::Fail, Actor::Account(account), addr)
                        .target(account)
                        .detail(DetailKey::Reason, DetailValue::Text("conflict")),
                );
                Err(PasskeyError::Conflict)
            }
            Err(error) => Err(error),
        }
    }

    // ---- step-up ----

    /// Starts a step-up for the account of `session`: assertion options that name the account's own credentials and nothing
    /// else, as JSON text, and the token of the open challenge. An account without a usable passkey is
    /// [`PasskeyError::Conflict`].
    pub fn start_step_up(&self, session: &Session) -> Result<(String, ChallengeToken), PasskeyError> {
        let now = self.clock.unix();
        let account = session.account_id;
        let passkeys: Vec<Passkey> = self.store.read(|tx| {
            if !tx.account_get(account)?.is_some_and(|a| a.state == AccountState::Active) {
                return Ok(Vec::new());
            }
            Ok(tx
                .passkey_list(account)?
                .into_iter()
                .filter_map(|row| {
                    let stored = Stored::parse(&row.passkey_json)?;
                    (stored.passkey.cred_id().as_slice() == row.credential_id.as_slice()).then_some(stored.passkey)
                })
                .collect())
        })?;
        if passkeys.is_empty() {
            return Err(PasskeyError::Conflict);
        }
        let (options, state) =
            self.webauthn.start_passkey_authentication(&passkeys).map_err(|_| PasskeyError::Internal)?;
        let json = serde_json::to_string(&options).map_err(|_| PasskeyError::Internal)?;
        let token = self.open(Ceremony::StepUp { state, account }, now)?;
        Ok((json, token))
    }

    /// Finishes a step-up: verifies the assertion for the session's own account only (another account's valid assertion is
    /// [`PasskeyError::Invalid`]), moves the session's last sign-in time to now and writes one `session.step_up` line. A
    /// failure is `Invalid` with one `signin.fail` line of method `passkey` and step `step_up`.
    pub fn finish_step_up(
        &self,
        token: &str,
        credential_json: &str,
        session: &mut Session,
        addr: IpAddr,
    ) -> Result<(), PasskeyError> {
        let now = self.clock.unix();
        let account = session.account_id;
        let taken = self.take(token, Kind::StepUp, now);
        match self.step_up_inner(taken, credential_json, session, now) {
            Ok(()) => {
                self.record(
                    &Event::new(Action::SessionStepUp, Outcome::Ok, Actor::Account(account), Some(addr))
                        .detail(DetailKey::Method, DetailValue::Text(METHOD.as_str())),
                );
                Ok(())
            }
            Err(PasskeyError::Internal) => {
                log::event(Level::Error, "passkey.step_up_failed", &[]);
                Err(PasskeyError::Internal)
            }
            Err(error) => {
                self.record(
                    &Event::new(Action::SigninFail, Outcome::Fail, Actor::Account(account), Some(addr))
                        .target(account)
                        .detail(DetailKey::Method, DetailValue::Text(METHOD.as_str()))
                        .detail(DetailKey::Step, DetailValue::Text("step_up"))
                        .detail(DetailKey::Reason, DetailValue::Text("invalid")),
                );
                Err(error)
            }
        }
    }

    fn step_up_inner(
        &self,
        taken: Option<Ceremony>,
        credential_json: &str,
        session: &mut Session,
        now: i64,
    ) -> Result<(), PasskeyError> {
        let Some(Ceremony::StepUp { state, account }) = taken else {
            return Err(PasskeyError::Invalid);
        };
        if account != session.account_id {
            return Err(PasskeyError::Invalid);
        }
        if credential_json.len() > MAX_CREDENTIAL_JSON {
            return Err(PasskeyError::Invalid);
        }
        self.verifications.fetch_add(1, Ordering::SeqCst);
        let credential: PublicKeyCredential =
            serde_json::from_str(credential_json).map_err(|_| PasskeyError::Invalid)?;
        let credential_id = credential.get_credential_id().to_vec();
        self.store.write(|tx| {
            if !tx.account_get(account)?.is_some_and(|a| a.state == AccountState::Active) {
                return Ok(Err(PasskeyError::Invalid));
            }
            let Some(row) = tx.passkey_by_credential(&credential_id)?.filter(|r| r.account_id == account) else {
                return Ok(Err(PasskeyError::Invalid));
            };
            let Some(mut stored) = Stored::parse(&row.passkey_json) else {
                return Ok(Err(PasskeyError::Invalid));
            };
            if stored.passkey.cred_id().as_slice() != row.credential_id.as_slice() {
                return Ok(Err(PasskeyError::Invalid));
            }
            let Ok(result) = self.webauthn.finish_passkey_authentication(&credential, &state) else {
                return Ok(Err(PasskeyError::Invalid));
            };
            if result.cred_id().as_slice() != row.credential_id.as_slice()
                || stored.passkey.update_credential(&result).is_none()
            {
                return Ok(Err(PasskeyError::Invalid));
            }
            stored.last_used = Some(now);
            let Ok(text) = stored.text() else {
                return Ok(Err(PasskeyError::Internal));
            };
            tx.passkey_update_json(account, row.id, &text)?;
            Ok(Ok(()))
        })??;
        self.sessions.reauth(session, now).map_err(|_| PasskeyError::Internal)
    }
}

/// The audit reason of a refusal. The same word for every cause of [`PasskeyError::Invalid`].
fn reason(error: PasskeyError) -> &'static str {
    match error {
        PasskeyError::Invalid | PasskeyError::NotFound => "invalid",
        PasskeyError::Conflict => "conflict",
        PasskeyError::TooMany => "too_many",
        PasskeyError::Internal => "internal",
    }
}

/// The longest name, in characters.
pub const MAX_NAME_CHARS: usize = 64;

/// The most bytes of a name that are looked at: 64 characters of at most 4 bytes, with room for the spaces around them.
const MAX_NAME_INPUT_BYTES: usize = 4 * MAX_NAME_CHARS + 64;

/// Whether a character may be part of a name: not a control character and none of the invisible ones that change how text
/// reads (zero-width spaces, direction marks and overrides, line and paragraph separators, the byte order mark).
fn printable(c: char) -> bool {
    !(c.is_control()
        || matches!(
            u32::from(c),
            0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB
        ))
}

/// The name a passkey is stored under: the text without the spaces around it when it is 1 to 64 printable characters,
/// otherwise [`DEFAULT_NAME`]. A long name is not cut; it is replaced.
pub fn clean_name(name: &str) -> String {
    if name.len() > MAX_NAME_INPUT_BYTES {
        return DEFAULT_NAME.to_owned();
    }
    let trimmed = name.trim();
    let length = trimmed.chars().count();
    if length == 0 || length > MAX_NAME_CHARS || !trimmed.chars().all(printable) {
        return DEFAULT_NAME.to_owned();
    }
    trimmed.to_owned()
}
