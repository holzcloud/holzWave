//! Single sign-on with OpenID Connect: the authorization code flow with PKCE and the ID token checks.
//!
//! The protocol is the library's (`openidconnect`): discovery, the authorization address, the code exchange, the parsing of the
//! ID token and every check of it. This module adds the parts the library leaves to the application: the browser-bound
//! transaction, the single-use rule, the bounds, the choice of algorithms, the time of the server's own clock, the mapping of
//! the verified claims onto an account and a session, and the audit lines.
//!
//! A person starts at [`OidcService::start`], which makes the address of the provider's authorization endpoint (response type
//! `code`, the scopes `openid email profile`, the exact redirect address `<public_url>/auth/oidc/callback`, a PKCE challenge
//! of method S256, a state and a nonce) and a random transaction value for the cookie that the routes set. The provider sends
//! the browser back with a code, and [`OidcService::finish`] exchanges it, verifies the ID token and signs the person in.
//!
//! Every failure of any kind is the one value [`OidcError::Failed`]. The audit line of a failure names only a class from a
//! closed list (`state`, `token`, `claims`, `denied`, `provider`), so nothing the provider or the browser sent can reach the
//! person, the log or the audit file. A transaction is removed from the map before anything is checked, so it works once.

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use openidconnect::core::{
    CoreAuthDisplay, CoreAuthPrompt, CoreAuthenticationFlow, CoreErrorResponseType, CoreGenderClaim, CoreJsonWebKey,
    CoreJweContentEncryptionAlgorithm, CoreJwsSigningAlgorithm, CoreProviderMetadata, CoreRevocableToken,
    CoreRevocationErrorResponse, CoreTokenIntrospectionResponse, CoreTokenType,
};
use openidconnect::{
    AccessTokenHash, AdditionalClaims, AsyncHttpClient, AuthorizationCode, Client, ClientId, ClientSecret, CsrfToken,
    ClaimsVerificationError, DiscoveryError, EmptyExtraTokenFields, EndpointNotSet, IdTokenFields, IdTokenVerifier, IssuerUrl,
    JsonWebKeySet, Nonce, SignatureVerificationError,
    OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RequestTokenError, Scope, StandardErrorResponse,
    StandardTokenResponse, TokenResponse,
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::accounts::{Accounts, OidcClaims, ProviderRules};
use crate::audit::{Action, Actor, Audit, DetailKey, DetailValue, Event, Outcome};
use crate::auth::oidc_http::{HttpError, HttpFuture, ClientSecretText, read_client_secret};
use crate::auth::session::{Created, Method, SessionManager, generate_token, parse_token};
use crate::auth::throttle::address_key;
use crate::clock::Clock;
use crate::config::{self, Refusal};
use crate::log::{self, Level};
use crate::store::{AccountId, TokenHash, hash_token};

/// How long a transaction works, in seconds: ten minutes.
pub const TRANSACTION_TTL_SECS: i64 = 600;

/// The most transactions open at once. The one after them pushes out the oldest.
pub const MAX_TRANSACTIONS: usize = 1_024;

/// The most transactions one client address (IPv6 by its /64) has open. The one after them pushes out that address's oldest.
pub const MAX_TRANSACTIONS_PER_ADDRESS: usize = 16;

/// How long a discovery document and its key set are used before they are fetched again, in seconds: one hour.
pub const DISCOVERY_TTL_SECS: i64 = 3_600;

/// The fewest seconds between two fetches of a provider's key set because of an unknown key id: five minutes.
pub const KEY_REFETCH_SECS: i64 = 300;

/// The longest authorization code that is looked at, in bytes.
pub const MAX_CODE_BYTES: usize = 2_048;

/// The longest state that is looked at, in bytes.
pub const MAX_STATE_BYTES: usize = 256;

/// The longest error code that is looked at, in bytes.
pub const MAX_ERROR_BYTES: usize = 64;

/// The longest issuer parameter that is looked at, in bytes.
pub const MAX_ISS_BYTES: usize = 2_048;

/// How far ahead of the server's clock an ID token may have been issued, in seconds: five minutes.
pub const ISSUE_SKEW_SECS: i64 = 300;

/// The path of the callback below the public address of the server.
pub const CALLBACK_PATH: &str = "/auth/oidc/callback";

/// The scopes that are asked for besides `openid`, which the library adds.
const SCOPES: [&str; 2] = ["email", "profile"];

/// The claim that holds the groups when the provider's configuration names none.
const DEFAULT_GROUPS_CLAIM: &str = "groups";

/// The longest group name that is passed on, in bytes.
const MAX_GROUP_BYTES: usize = 256;

/// The most groups that are read from the claim.
const MAX_GROUPS: usize = 1_024;

/// The signature algorithms an ID token may use: RSA and RSA-PSS with SHA-2, ECDSA on the NIST curves and EdDSA. Never `none`,
/// and never a symmetric algorithm, which the verifier of a public client refuses on its own as well.
const ALLOWED_ALGS: [CoreJwsSigningAlgorithm; 9] = [
    CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
    CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha384,
    CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha512,
    CoreJwsSigningAlgorithm::RsaSsaPssSha256,
    CoreJwsSigningAlgorithm::RsaSsaPssSha384,
    CoreJwsSigningAlgorithm::RsaSsaPssSha512,
    CoreJwsSigningAlgorithm::EcdsaP256Sha256,
    CoreJwsSigningAlgorithm::EcdsaP384Sha384,
    CoreJwsSigningAlgorithm::EdDsa,
];

/// What the `Debug` texts of the values that hold secrets or claims print.
const TRANSACTION_DEBUG: &str = "OidcTransaction(..)";
const START_DEBUG: &str = "OidcStart(..)";
const CALLBACK_DEBUG: &str = "OidcCallback(..)";
const SERVICE_DEBUG: &str = "OidcService(..)";
const EXTRA_DEBUG: &str = "IdTokenExtra(..)";

/// The one error of the single sign-on. It says nothing about the cause: an unknown transaction, a wrong state, a provider
/// that failed or was unreachable, a bad token and a refused account all look the same to the person and to the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OidcError {
    /// The sign-in did not happen.
    Failed,
}

impl fmt::Display for OidcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the single sign-on could not be completed")
    }
}

impl std::error::Error for OidcError {}

/// The class of a failure, for the audit line. The set is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// The transaction is unknown, used, expired or from another browser, or the state is not the stored one.
    State,
    /// The ID token, or the response that carries it, is not acceptable.
    Token,
    /// The verified token holds no usable identity.
    Claims,
    /// The account rules, the account state or the session refused the person.
    Denied,
    /// The provider answered with an error, answered wrongly or was not reachable.
    Provider,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Reason::State => "state",
            Reason::Token => "token",
            Reason::Claims => "claims",
            Reason::Denied => "denied",
            Reason::Provider => "provider",
        }
    }
}

/// The value of the transaction cookie: 43 characters of the base64url alphabet. Not `Clone`, a marker for `Debug`, wiped
/// when dropped.
pub struct TransactionValue(Zeroizing<String>);

impl TransactionValue {
    /// The 43 characters, for the cookie.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TransactionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(TRANSACTION_DEBUG)
    }
}

/// What a start gives the routes: where to send the browser and the value of the transaction cookie.
pub struct OidcStart {
    /// The address of the provider's authorization endpoint with its query.
    pub url: String,
    /// The cookie value that binds the transaction to this browser.
    pub transaction: TransactionValue,
}

impl fmt::Debug for OidcStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(START_DEBUG)
    }
}

/// The parameters the provider sent the browser back with, as the routes read them from the query. Nothing here is trusted.
#[derive(Default)]
pub struct OidcCallback {
    /// The authorization code.
    pub code: Option<String>,
    /// The state that was in the authorization address.
    pub state: Option<String>,
    /// The error the provider reports, if it does.
    pub error: Option<String>,
    /// The issuer the provider names (RFC 9207), if it does.
    pub iss: Option<String>,
}

impl fmt::Debug for OidcCallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CALLBACK_DEBUG)
    }
}

/// A person who signed in: the session to put in the cookie and the account.
#[derive(Debug)]
pub struct OidcGrant {
    /// The new session and its cookie value.
    pub session: Created,
    /// The account that signed in.
    pub account: AccountId,
}

/// A provider as the sign-in page lists it: the id of the route and the name on the button, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInfo {
    /// The id in the route.
    pub id: String,
    /// The name people see.
    pub name: String,
}

// ---------------------------------------------------------------------------------------------------------------------
// The library types
// ---------------------------------------------------------------------------------------------------------------------

/// The claims of the ID token that the library does not know, kept by name so that the provider's configuration decides
/// which one holds the groups. The `Debug` text prints a marker, because every value is about a person.
#[derive(Clone, Default, Deserialize, Serialize)]
struct Extra {
    #[serde(flatten)]
    claims: HashMap<String, serde_json::Value>,
}

impl fmt::Debug for Extra {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(EXTRA_DEBUG)
    }
}

impl AdditionalClaims for Extra {}

type Fields =
    IdTokenFields<Extra, EmptyExtraTokenFields, CoreGenderClaim, CoreJweContentEncryptionAlgorithm, CoreJwsSigningAlgorithm>;
type Tokens = StandardTokenResponse<Fields, CoreTokenType>;

/// The client type of the library with this module's claims.
type OClient<
    HasAuthUrl = EndpointNotSet,
    HasDeviceAuthUrl = EndpointNotSet,
    HasIntrospectionUrl = EndpointNotSet,
    HasRevocationUrl = EndpointNotSet,
    HasTokenUrl = EndpointNotSet,
    HasUserInfoUrl = EndpointNotSet,
> = Client<
    Extra,
    CoreAuthDisplay,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJsonWebKey,
    CoreAuthPrompt,
    StandardErrorResponse<CoreErrorResponseType>,
    Tokens,
    CoreTokenIntrospectionResponse,
    CoreRevocableToken,
    CoreRevocationErrorResponse,
    HasAuthUrl,
    HasDeviceAuthUrl,
    HasIntrospectionUrl,
    HasRevocationUrl,
    HasTokenUrl,
    HasUserInfoUrl,
>;

/// The client the service is generic over: the library's asynchronous client trait, with this module's error and future.
pub trait Http:
    for<'c> AsyncHttpClient<'c, Error = HttpError, Future = HttpFuture<'c>> + Send + Sync + 'static
{
}

impl<T> Http for T where
    T: for<'c> AsyncHttpClient<'c, Error = HttpError, Future = HttpFuture<'c>> + Send + Sync + 'static
{
}

// ---------------------------------------------------------------------------------------------------------------------
// The transactions
// ---------------------------------------------------------------------------------------------------------------------

/// What a start remembers until the callback comes: nothing of it leaves this module.
struct Transaction {
    /// The index of the provider in the service's list.
    provider: usize,
    state: CsrfToken,
    nonce: Nonce,
    verifier: PkceCodeVerifier,
    created: i64,
    /// The key of the client address that started it.
    address: String,
    /// Counts the starts, so that "oldest" is exact when several start in one second.
    sequence: u64,
}

impl Transaction {
    fn expired(&self, now: i64) -> bool {
        now > self.created.saturating_add(TRANSACTION_TTL_SECS)
    }
}

#[derive(Default)]
struct Transactions {
    map: HashMap<TokenHash, Transaction>,
    next: u64,
}

impl Transactions {
    /// Opens a transaction. Room is made first, with work bounded by [`MAX_TRANSACTIONS`]: expired entries go, then the oldest
    /// of the same address while that address has [`MAX_TRANSACTIONS_PER_ADDRESS`], then the oldest of all while the map is
    /// full.
    fn insert(&mut self, hash: TokenHash, mut entry: Transaction) {
        let now = entry.created;
        self.map.retain(|_, held| !held.expired(now));
        while self.map.values().filter(|held| held.address == entry.address).count() >= MAX_TRANSACTIONS_PER_ADDRESS {
            let oldest = self
                .map
                .iter()
                .filter(|(_, held)| held.address == entry.address)
                .min_by_key(|(_, held)| held.sequence)
                .map(|(key, _)| *key);
            match oldest {
                Some(key) => self.map.remove(&key),
                None => break,
            };
        }
        while self.map.len() >= MAX_TRANSACTIONS {
            let oldest = self.map.iter().min_by_key(|(_, held)| held.sequence).map(|(key, _)| *key);
            match oldest {
                Some(key) => self.map.remove(&key),
                None => break,
            };
        }
        entry.sequence = self.next;
        self.next = self.next.wrapping_add(1);
        self.map.insert(hash, entry);
    }

    /// Removes the entry of a cookie value whatever happens next; answers it when it is within its time.
    fn take(&mut self, hash: &TokenHash, now: i64) -> Option<Transaction> {
        let entry = self.map.remove(hash)?;
        (!entry.expired(now)).then_some(entry)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------------------------------------------------

/// What is fetched from a provider and kept for an hour.
struct Loaded {
    metadata: Arc<CoreProviderMetadata>,
    fetched: i64,
}

#[derive(Default)]
struct Cache {
    loaded: Option<Loaded>,
    /// When the key set was last fetched because of an unknown key id. It outlives the discovery document.
    last_refetch: Option<i64>,
}

struct Provider {
    id: String,
    name: String,
    issuer: String,
    client_id: String,
    secret: Option<ClientSecretText>,
    rules: ProviderRules,
    groups_claim: String,
    cache: tokio::sync::Mutex<Cache>,
}

/// Single sign-on over the configured providers.
pub struct OidcService<H: Http> {
    providers: Vec<Provider>,
    redirect: RedirectUrl,
    accounts: Arc<Accounts>,
    sessions: Arc<SessionManager>,
    audit: Arc<Audit>,
    clock: Arc<dyn Clock>,
    http: H,
    transactions: Mutex<Transactions>,
}

impl<H: Http> fmt::Debug for OidcService<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(SERVICE_DEBUG)
    }
}

impl<H: Http> OidcService<H> {
    /// The service over the configured providers. `public_url` is the address people type in the browser; the redirect address
    /// of every provider is `<public_url>/auth/oidc/callback`. The client secret files are read here, through the private-file
    /// opener (a link or a file that is open to others is refused). A provider whose `client_secret_file` is an empty path is a
    /// public client: PKCE only.
    pub fn new(
        providers: &[config::Oidc],
        public_url: &str,
        accounts: Arc<Accounts>,
        sessions: Arc<SessionManager>,
        audit: Arc<Audit>,
        clock: Arc<dyn Clock>,
        http: H,
    ) -> Result<OidcService<H>, Refusal> {
        let origin = config::public_origin(public_url)?;
        let redirect = RedirectUrl::new(format!("{}{CALLBACK_PATH}", origin.as_str()))
            .map_err(|_| Refusal::new("public_url does not make a redirect address for single sign-on"))?;
        let mut list = Vec::with_capacity(providers.len());
        for provider in providers {
            let secret = if provider.client_secret_file.as_os_str().is_empty() {
                None
            } else {
                Some(read_client_secret(&provider.client_secret_file).map_err(|e| Refusal::new(e.sentence()))?)
            };
            list.push(Provider {
                id: provider.id.clone(),
                name: provider.name.clone(),
                issuer: provider.issuer.clone(),
                client_id: provider.client_id.clone(),
                secret,
                rules: ProviderRules::from(provider),
                groups_claim: provider.groups_claim.clone().unwrap_or_else(|| DEFAULT_GROUPS_CLAIM.to_owned()),
                cache: tokio::sync::Mutex::new(Cache::default()),
            });
        }
        Ok(OidcService {
            providers: list,
            redirect,
            accounts,
            sessions,
            audit,
            clock,
            http,
            transactions: Mutex::new(Transactions::default()),
        })
    }

    /// The providers in configuration order: the id and the name, and nothing else.
    pub fn providers(&self) -> Vec<ProviderInfo> {
        self.providers.iter().map(|p| ProviderInfo { id: p.id.clone(), name: p.name.clone() }).collect()
    }

    /// How many transactions are open now.
    pub fn open_transactions(&self) -> usize {
        self.lock().map.len()
    }

    fn lock(&self) -> MutexGuard<'_, Transactions> {
        self.transactions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn provider(&self, id: &str) -> Option<(usize, &Provider)> {
        self.providers.iter().enumerate().find(|(_, p)| p.id == id)
    }

    fn record(&self, event: &Event) {
        if self.audit.record(event).is_err() {
            log::event(Level::Error, "oidc.audit_failed", &[]);
        }
    }

    fn record_failure(&self, reason: Reason, addr: IpAddr) {
        self.record(
            &Event::new(Action::SigninFail, Outcome::Fail, Actor::Anonymous, Some(addr))
                .detail(DetailKey::Method, DetailValue::Text(Method::Oidc.as_str()))
                .detail(DetailKey::Reason, DetailValue::Text(reason.as_str())),
        );
    }

    // ---- the provider's documents ----

    /// The provider's discovery document with its key set: fetched on first use and again after an hour.
    async fn snapshot(&self, provider: &Provider) -> Result<Arc<CoreProviderMetadata>, Reason> {
        let mut cache = provider.cache.lock().await;
        let now = self.clock.unix();
        if let Some(loaded) = &cache.loaded
            && (0..DISCOVERY_TTL_SECS).contains(&now.saturating_sub(loaded.fetched))
        {
            return Ok(loaded.metadata.clone());
        }
        let issuer = IssuerUrl::new(provider.issuer.clone()).map_err(|_| Reason::Provider)?;
        match CoreProviderMetadata::discover_async(issuer, &self.http).await {
            Ok(metadata) => {
                let metadata = Arc::new(metadata);
                cache.loaded = Some(Loaded { metadata: metadata.clone(), fetched: now });
                Ok(metadata)
            }
            Err(error) => {
                cache.loaded = None;
                log::event(Level::Warn, "oidc.discovery_failed", &[("kind", discovery_kind(&error))]);
                Err(Reason::Provider)
            }
        }
    }

    fn client(&self, provider: &Provider, metadata: &CoreProviderMetadata) -> OClient<
        openidconnect::EndpointSet,
        EndpointNotSet,
        EndpointNotSet,
        EndpointNotSet,
        openidconnect::EndpointMaybeSet,
        openidconnect::EndpointMaybeSet,
    > {
        let secret = provider.secret.as_ref().map(|s| ClientSecret::new(s.expose().to_owned()));
        OClient::from_provider_metadata(metadata.clone(), ClientId::new(provider.client_id.clone()), secret)
            .set_redirect_uri(self.redirect.clone())
    }

    // ---- start ----

    /// Starts a sign-in at the provider `provider_id` for the client at `addr`: the address to send the browser to and the
    /// value of the transaction cookie. An unknown provider and a provider that cannot be reached are both
    /// [`OidcError::Failed`]; the second writes one `signin.fail` line of class `provider`.
    pub async fn start(&self, provider_id: &str, addr: IpAddr) -> Result<OidcStart, OidcError> {
        let Some((index, provider)) = self.provider(provider_id) else { return Err(OidcError::Failed) };
        match self.start_inner(index, provider, addr).await {
            Ok(started) => Ok(started),
            Err(reason) => {
                self.record_failure(reason, addr);
                Err(OidcError::Failed)
            }
        }
    }

    async fn start_inner(&self, index: usize, provider: &Provider, addr: IpAddr) -> Result<OidcStart, Reason> {
        let metadata = self.snapshot(provider).await?;
        if metadata.authorization_endpoint().url().scheme() != "https" {
            return Err(Reason::Provider);
        }
        let client = self.client(provider, &metadata);
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut request = client
            .authorize_url(CoreAuthenticationFlow::AuthorizationCode, CsrfToken::new_random, Nonce::new_random)
            .set_pkce_challenge(challenge);
        for scope in SCOPES {
            request = request.add_scope(Scope::new(scope.to_owned()));
        }
        let (url, state, nonce) = request.url();
        let (value, bytes) = generate_token().map_err(|_| Reason::Provider)?;
        let now = self.clock.unix();
        self.lock().insert(
            hash_token(&bytes[..]),
            Transaction { provider: index, state, nonce, verifier, created: now, address: address_key(addr), sequence: 0 },
        );
        Ok(OidcStart { url: url.to_string(), transaction: TransactionValue(Zeroizing::new(value.expose().to_owned())) })
    }

    // ---- finish ----

    /// Finishes a sign-in: the transaction of the cookie value is removed first, then the callback is checked, the code is
    /// exchanged, the ID token is verified and the account is decided. A success is one `signin.ok` line and a session; every
    /// failure is [`OidcError::Failed`] with one `signin.fail` line.
    pub async fn finish(
        &self,
        transaction: &str,
        callback: OidcCallback,
        addr: IpAddr,
        device: &str,
    ) -> Result<OidcGrant, OidcError> {
        match self.finish_inner(transaction, callback, addr, device).await {
            Ok(grant) => {
                self.record(
                    &Event::new(Action::SigninOk, Outcome::Ok, Actor::Account(grant.account), Some(addr))
                        .detail(DetailKey::Method, DetailValue::Text(Method::Oidc.as_str())),
                );
                Ok(grant)
            }
            Err(reason) => {
                self.record_failure(reason, addr);
                Err(OidcError::Failed)
            }
        }
    }

    async fn finish_inner(
        &self,
        transaction: &str,
        callback: OidcCallback,
        addr: IpAddr,
        device: &str,
    ) -> Result<OidcGrant, Reason> {
        let now = self.clock.unix();
        let hash = parse_token(transaction).map_err(|_| Reason::State)?;
        let taken = self.lock().take(&hash, now).ok_or(Reason::State)?;
        let provider = self.providers.get(taken.provider).ok_or(Reason::State)?;

        // The sizes are bounded before anything is compared or sent: nothing a browser carries reaches the provider unchecked.
        let state = callback.state.as_deref().ok_or(Reason::State)?;
        if state.len() > MAX_STATE_BYTES {
            return Err(Reason::State);
        }
        if callback.code.as_deref().is_some_and(|code| code.len() > MAX_CODE_BYTES)
            || callback.error.as_deref().is_some_and(|error| error.len() > MAX_ERROR_BYTES)
            || callback.iss.as_deref().is_some_and(|iss| iss.len() > MAX_ISS_BYTES)
        {
            return Err(Reason::Provider);
        }
        if !bool::from(taken.state.secret().as_bytes().ct_eq(state.as_bytes())) {
            return Err(Reason::State);
        }
        if callback.iss.as_deref().is_some_and(|iss| iss != provider.issuer) {
            return Err(Reason::Provider);
        }
        if callback.error.is_some() {
            return Err(Reason::Provider);
        }
        let code = callback.code.ok_or(Reason::Provider)?;

        let mut metadata = self.snapshot(provider).await?;
        let client = self.client(provider, &metadata);
        let response: Tokens = client
            .exchange_code(AuthorizationCode::new(code))
            .map_err(|_| Reason::Provider)?
            .set_pkce_verifier(taken.verifier)
            .request_async(&self.http)
            .await
            .map_err(token_reason)?;
        let id_token = response.id_token().ok_or(Reason::Token)?;

        let mut verifier = self.verifier(provider, &metadata);
        let claims = match id_token.claims(&verifier, &taken.nonce) {
            Ok(claims) => claims,
            // A key id the cached key set does not know may mean the provider rotated its keys: the set is fetched again, at
            // most once in five minutes, and the token is checked once more against the fresh set.
            Err(ClaimsVerificationError::SignatureVerification(SignatureVerificationError::NoMatchingKey)) => {
                metadata = self.refetch_keys(provider, &metadata).await.ok_or(Reason::Token)?;
                verifier = self.verifier(provider, &metadata);
                id_token.claims(&verifier, &taken.nonce).map_err(|_| Reason::Token)?
            }
            Err(_) => return Err(Reason::Token),
        };
        // The audience check of the library already refuses an audience besides the client; an authorized party that names
        // another client is refused as well.
        if claims.authorized_party().is_some_and(|azp| azp.as_str() != provider.client_id) {
            return Err(Reason::Token);
        }
        if let Some(expected) = claims.access_token_hash() {
            let alg = id_token.signing_alg().map_err(|_| Reason::Token)?;
            let key = id_token.signing_key(&verifier).map_err(|_| Reason::Token)?;
            let actual = AccessTokenHash::from_token(response.access_token(), alg, key).map_err(|_| Reason::Token)?;
            if actual != *expected {
                return Err(Reason::Token);
            }
        }

        let subject = claims.subject().as_str();
        if subject.is_empty() {
            return Err(Reason::Claims);
        }
        let identity = OidcClaims {
            issuer: provider.issuer.clone(),
            subject: subject.to_owned(),
            email: claims.email().map(|e| e.as_str().to_owned()),
            email_verified: claims.email_verified().unwrap_or(false),
            preferred_username: claims.preferred_username().map(|u| u.as_str().to_owned()),
            display_name: claims.name().and_then(|n| n.get(None)).map(|n| n.as_str().to_owned()),
            groups: groups_of(claims.additional_claims(), &provider.groups_claim),
        };
        let account = self.accounts.oidc_sign_in(&identity, &provider.rules, Some(addr)).map_err(|error| {
            if !matches!(error, crate::accounts::AccountError::Denied) {
                log::event(Level::Error, "oidc.account_failed", &[]);
            }
            Reason::Denied
        })?;
        let session = self
            .sessions
            .create(account.id, Method::Oidc, self.clock.unix(), device, Some(addr))
            .map_err(|_| Reason::Denied)?;
        Ok(OidcGrant { session, account: account.id })
    }

    /// The verifier of a provider's ID tokens: a public-client verifier (so that no symmetric algorithm is ever accepted, even
    /// where a client secret exists), the provider's published keys, the configured issuer and client id, the algorithms of
    /// [`ALLOWED_ALGS`] and the time of the server's clock.
    fn verifier<'a>(&self, provider: &Provider, metadata: &CoreProviderMetadata) -> IdTokenVerifier<'a, CoreJsonWebKey> {
        let clock = self.clock.clone();
        IdTokenVerifier::new_public_client(
            ClientId::new(provider.client_id.clone()),
            IssuerUrl::new(provider.issuer.clone()).unwrap_or_else(|_| metadata.issuer().clone()),
            metadata.jwks().clone(),
        )
        .set_allowed_algs(ALLOWED_ALGS)
        .set_time_fn({
            let clock = clock.clone();
            move || SystemTime::into(clock.now())
        })
        .set_issue_time_verifier_fn(move |issued| {
            let issued = crate::clock::unix_seconds(SystemTime::from(issued));
            if issued > clock.unix().saturating_add(ISSUE_SKEW_SECS) {
                return Err("the token was issued in the future".to_owned());
            }
            Ok(())
        })
    }

    /// Fetches the key set of a provider again after an unknown key id, when the last such fetch is five minutes old or more.
    /// The time is taken before the request, so a failed fetch counts. Answers the metadata with the fresh set, or the set
    /// another task fetched meanwhile if it differs from `current`, or `None` when nothing new is available.
    async fn refetch_keys(
        &self,
        provider: &Provider,
        current: &Arc<CoreProviderMetadata>,
    ) -> Option<Arc<CoreProviderMetadata>> {
        let mut cache = provider.cache.lock().await;
        let now = self.clock.unix();
        let held = cache.loaded.as_ref()?.metadata.clone();
        let allowed = !cache.last_refetch.is_some_and(|last| (0..KEY_REFETCH_SECS).contains(&now.saturating_sub(last)));
        if !allowed {
            return (!Arc::ptr_eq(&held, current)).then_some(held);
        }
        cache.last_refetch = Some(now);
        let keys = JsonWebKeySet::fetch_async(held.jwks_uri(), &self.http).await.ok()?;
        let fresh = Arc::new(CoreProviderMetadata::clone(&held).set_jwks(keys));
        if let Some(loaded) = cache.loaded.as_mut() {
            loaded.metadata = fresh.clone();
        }
        Some(fresh)
    }
}

/// The groups the provider's claim lists: an array of strings, or a single string. Other shapes give none.
fn groups_of(extra: &Extra, name: &str) -> Vec<String> {
    use serde_json::Value;
    match extra.claims.get(name) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .filter(|group| group.len() <= MAX_GROUP_BYTES)
            .take(MAX_GROUPS)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(group)) if group.len() <= MAX_GROUP_BYTES => vec![group.clone()],
        _ => Vec::new(),
    }
}

/// The class of a failed code exchange. A response that cannot be read is a `claims` failure when it lacks the subject and a
/// `token` failure otherwise; every other failure is the provider's.
fn token_reason(error: RequestTokenError<HttpError, StandardErrorResponse<CoreErrorResponseType>>) -> Reason {
    match error {
        RequestTokenError::Parse(inner, _) if inner.inner().to_string().contains("`sub`") => Reason::Claims,
        RequestTokenError::Parse(..) => Reason::Token,
        _ => Reason::Provider,
    }
}

/// A short word for the kind of a discovery failure, for the log. It holds nothing the provider sent.
fn discovery_kind(error: &DiscoveryError<HttpError>) -> &'static str {
    match error {
        DiscoveryError::Request(_) => "request",
        DiscoveryError::Response(..) => "response",
        DiscoveryError::Parse(_) => "parse",
        DiscoveryError::Validation(_) => "validation",
        _ => "other",
    }
}
