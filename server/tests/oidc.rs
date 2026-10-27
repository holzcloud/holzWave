//! Single sign-on against a provider that runs inside the test process: the flow, the verification matrix, the transaction
//! bounds, the discovery cache, the client secret file and the HTTP adapter.
//!
//! The provider and the in-memory client are in `support/oidc_provider.rs`. The adapter tests at the end use a plain local
//! socket.

#[path = "support/oidc_provider.rs"]
mod oidc_provider;
#[path = "support/scratch.rs"]
mod scratch;

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use holzwave_server::accounts::Accounts;
use holzwave_server::audit::Audit;
use holzwave_server::auth::oidc::{OidcCallback, OidcError, OidcGrant, OidcService, OidcStart};
use holzwave_server::auth::oidc_http::{
    CONNECT_TIMEOUT, HttpError, HttpsClient, Limits, MAX_RESPONSE_BYTES, MAX_SECRET_FILE_BYTES, TOTAL_TIMEOUT, read_client_secret,
};
use holzwave_server::auth::session::{Method, SessionManager};
use holzwave_server::clock::ManualClock;
use holzwave_server::config::{self, Sessions};
use holzwave_server::store::{AccountId, AccountState, Role, Store};
use oidc_provider::{AtHash, Behaviour, Callback, Signer, CLIENT_ID, ISSUER, MemoryHttp, SECRET, TestProvider};
use openidconnect::{AsyncHttpClient, HttpRequest};
use scratch::Scratch;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 2026-03-01T08:30:00Z.
const NOW: i64 = 1_772_353_800;
const RANGE: std::ops::RangeInclusive<u32> = 20000..=29999;
const ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));
const PUBLIC_URL: &str = "https://hw.example.test";
const DEVICE: &str = "Firefox on Linux";

type Service = OidcService<MemoryHttp>;

#[allow(dead_code)]
struct Fixture {
    scratch: Scratch,
    store: Arc<Store>,
    clock: Arc<ManualClock>,
    sessions: Arc<SessionManager>,
    provider: Arc<TestProvider>,
    service: Arc<Service>,
}

/// A provider entry of the configuration with the rules a test needs.
fn entry(id: &str, issuer: &str, secret_file: PathBuf) -> config::Oidc {
    config::Oidc {
        id: id.to_owned(),
        name: format!("Company {id}"),
        issuer: issuer.to_owned(),
        client_id: CLIENT_ID.to_owned(),
        client_secret_file: secret_file,
        allowed_domains: vec!["example.org".to_owned()],
        allowed_groups: Vec::new(),
        groups_claim: None,
    }
}

fn audit_lines(dir: &Path) -> Vec<Value> {
    let audit = dir.join("server/audit");
    let mut files: Vec<_> = match std::fs::read_dir(&audit) {
        Ok(entries) => entries.map(|e| e.expect("entry").path()).collect(),
        Err(_) => return Vec::new(),
    };
    files.sort();
    files
        .iter()
        .flat_map(|f| std::fs::read_to_string(f).expect("read an audit file").lines().map(str::to_owned).collect::<Vec<_>>())
        .map(|l| serde_json::from_str(&l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}")))
        .collect()
}

/// The fixture over one provider `corp` at `https://idp.example.test`, configured with the rules `rules` and a secret file.
fn fixture_with(tag: &str, rules: impl FnOnce(&mut config::Oidc)) -> Fixture {
    let scratch = Scratch::new(tag);
    let clock = Arc::new(ManualClock::new(NOW));
    let provider = TestProvider::standard(clock.clone());
    let secret = scratch.write_file("secret", format!("{SECRET}\n").as_bytes(), 0o600);
    let mut config = entry("corp", ISSUER, secret);
    rules(&mut config);
    let store = Arc::new(Store::open(scratch.path()).expect("open the store"));
    let audit = Arc::new(Audit::open(scratch.path(), clock.clone()).expect("open the audit log"));
    let sessions = Arc::new(SessionManager::new(store.clone(), clock.clone(), Sessions::default()));
    let accounts = Arc::new(Accounts::new(store.clone(), clock.clone(), audit.clone(), RANGE));
    let service = Arc::new(
        OidcService::new(
            &[config],
            PUBLIC_URL,
            accounts,
            sessions.clone(),
            audit,
            clock.clone(),
            MemoryHttp::new(std::slice::from_ref(&provider)),
        )
        .expect("the service is built"),
    );
    Fixture { scratch, store, clock, sessions, provider, service }
}

fn fixture(tag: &str) -> Fixture {
    fixture_with(tag, |_| {})
}

impl Fixture {
    fn lines(&self) -> Vec<Value> {
        audit_lines(self.scratch.path())
    }

    fn count(&self, action: &str) -> usize {
        self.lines().iter().filter(|l| l["action"] == action).count()
    }

    /// The `signin.fail` lines.
    fn failures(&self) -> Vec<Value> {
        self.lines().into_iter().filter(|l| l["action"] == "signin.fail").collect()
    }

    fn accounts(&self) -> i64 {
        self.store.read(|tx| tx.account_count()).expect("count")
    }

    /// A start and the provider's answer to the browser.
    async fn begin(&self) -> (OidcStart, Callback) {
        let start = self.service.start("corp", ADDR).await.expect("the start works");
        let callback = self.provider.authorize(&start.url);
        (start, callback)
    }

    async fn finish(&self, start: &OidcStart, callback: &Callback) -> Result<OidcGrant, OidcError> {
        let parameters = OidcCallback {
            code: Some(callback.code.clone()),
            state: Some(callback.state.clone()),
            ..OidcCallback::default()
        };
        self.service.finish(start.transaction.expose(), parameters, ADDR, DEVICE).await
    }

    /// A whole sign-in.
    async fn sign_in(&self) -> Result<OidcGrant, OidcError> {
        let (start, callback) = self.begin().await;
        self.finish(&start, &callback).await
    }

    /// The failure that was just seen: it is `Failed`, makes no session and writes one `signin.fail` line of class `reason`.
    fn assert_failed(&self, result: Result<OidcGrant, OidcError>, reason: &str, sessions_before: usize, accounts_before: i64) {
        assert_eq!(result.err(), Some(OidcError::Failed));
        assert_eq!(self.accounts(), accounts_before, "no account was made");
        assert_eq!(self.session_rows(), sessions_before, "no session was made");
        let failures = self.failures();
        let last = failures.last().expect("a failure line was written");
        assert_eq!(last["detail"]["method"], "oidc");
        assert_eq!(last["detail"]["reason"], reason, "the class of the failure: {last}");
        assert_eq!(last["result"], "fail");
    }

    fn session_rows(&self) -> usize {
        self.store
            .read(|tx| {
                let mut total = 0;
                for account in tx.account_list()? {
                    total += tx.session_list(account.id)?.len();
                }
                Ok(total)
            })
            .expect("count the sessions")
    }

    fn account_of(&self, subject: &str) -> Option<AccountId> {
        self.store.read(|tx| tx.oidc_find(ISSUER, subject)).expect("find")
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Task 1: the tracer
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn tracer_a_person_signs_in_and_gets_a_member_account() {
    let f = fixture("tracer-ok");
    let (start, callback) = f.begin().await;

    // The authorization address, as the provider saw it.
    let names = TestProvider::query_names(&start.url);
    println!("authorization query names: {names:?}");
    assert!(start.url.starts_with("https://idp.example.test/authorize?"), "{}", start.url);
    let value = |name: &str| TestProvider::query_value(&start.url, name);
    assert_eq!(value("response_type").as_deref(), Some("code"));
    assert_eq!(value("client_id").as_deref(), Some(CLIENT_ID));
    assert_eq!(value("redirect_uri").as_deref(), Some("https://hw.example.test/auth/oidc/callback"));
    assert_eq!(value("code_challenge_method").as_deref(), Some("S256"));
    let scope = value("scope").expect("a scope");
    assert!(scope.split(' ').any(|s| s == "openid"), "scope: {scope}");
    assert_eq!(scope, "openid email profile");
    assert!(value("state").is_some_and(|s| !s.is_empty()));
    assert!(value("nonce").is_some_and(|s| !s.is_empty()));
    assert!(value("code_challenge").is_some_and(|s| s.len() == 43));
    assert_eq!(start.transaction.expose().len(), 43);
    assert_eq!(f.service.open_transactions(), 1);

    let grant = f.finish(&start, &callback).await.expect("the sign-in works");
    assert_eq!(f.service.open_transactions(), 0, "the transaction is used");

    // The account: a member, linked by (issuer, subject), made through the accounts rules.
    assert_eq!(f.accounts(), 1);
    assert_eq!(f.account_of("sub-1"), Some(grant.account));
    let account = f.store.read(|tx| tx.account_get(grant.account)).expect("read").expect("the account exists");
    assert_eq!(account.role, Role::Member);
    assert_eq!(account.state, AccountState::Active);
    assert_eq!(f.count("oidc.create"), 1);

    // The session: 43 characters, accepted by the lookup with the method oidc.
    assert_eq!(grant.session.value.expose().len(), 43);
    let session = f.sessions.lookup(grant.session.value.expose(), NOW).expect("the session works");
    assert_eq!(session.method, Method::Oidc);
    assert_eq!(session.account_id, grant.account);

    // One success line, method oidc.
    let ok: Vec<Value> = f.lines().into_iter().filter(|l| l["action"] == "signin.ok").collect();
    assert_eq!(ok.len(), 1, "{ok:?}");
    assert_eq!(ok[0]["detail"]["method"], "oidc");
    assert!(f.failures().is_empty());
    // The provider saw the client authenticate with its secret and the verifier.
    let request = f.provider.last_token_request().expect("the token endpoint was called");
    assert!(request.authorization.is_some_and(|a| a.starts_with("Basic ")));
    assert!(request.form.iter().any(|(k, _)| k == "code_verifier"));
}

#[tokio::test]
async fn tracer_the_second_sign_in_gets_the_same_account() {
    let f = fixture("tracer-second");
    let first = f.sign_in().await.expect("first sign-in");
    let second = f.sign_in().await.expect("second sign-in");
    assert_eq!(first.account, second.account);
    assert_eq!(f.accounts(), 1);
    assert_eq!(f.count("oidc.create"), 1, "nothing was created the second time");
    assert_eq!(f.count("signin.ok"), 2);
    assert_ne!(first.session.value.expose(), second.session.value.expose());
}

#[tokio::test]
async fn tracer_a_wrong_state_fails_and_makes_nothing() {
    let f = fixture("tracer-state");
    let (start, mut callback) = f.begin().await;
    callback.state.push('x');
    let result = f.finish(&start, &callback).await;
    f.assert_failed(result, "state", 0, 0);
    assert_eq!(f.failures().len(), 1);
    assert_eq!(f.provider.token_requests.load(Ordering::SeqCst), 0, "the provider was not asked");
    assert_eq!(f.service.open_transactions(), 0, "the transaction is gone");
}

#[tokio::test]
async fn tracer_a_transaction_value_that_was_never_issued_fails() {
    let f = fixture("tracer-unissued");
    let (_start, callback) = f.begin().await;
    let parameters =
        OidcCallback { code: Some(callback.code.clone()), state: Some(callback.state.clone()), ..OidcCallback::default() };
    let never = "A".repeat(43);
    let result = f.service.finish(&never, parameters, ADDR, DEVICE).await;
    f.assert_failed(result, "state", 0, 0);
    assert_eq!(f.provider.token_requests.load(Ordering::SeqCst), 0);
    // Not even the right shape.
    let result = f.service.finish("short", OidcCallback::default(), ADDR, DEVICE).await;
    assert_eq!(result.err(), Some(OidcError::Failed));
}

#[tokio::test]
async fn tracer_a_transaction_works_once() {
    let f = fixture("tracer-once");
    let (start, callback) = f.begin().await;
    f.finish(&start, &callback).await.expect("the first use works");
    let again = f.finish(&start, &callback).await;
    assert_eq!(again.err(), Some(OidcError::Failed));
    assert_eq!(f.count("signin.ok"), 1);
    assert_eq!(f.failures().len(), 1);
}

#[tokio::test]
async fn tracer_a_unknown_provider_has_no_start() {
    let f = fixture("tracer-unknown");
    assert_eq!(f.service.start("nobody", ADDR).await.err(), Some(OidcError::Failed));
    assert!(f.failures().is_empty(), "an unknown route is not a sign-in");
    assert_eq!(f.provider.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn tracer_the_future_of_a_sign_in_can_move_between_threads() {
    let f = fixture("tracer-send");
    let service = f.service.clone();
    let provider = f.provider.clone();
    let handle = tokio::spawn(async move {
        let start = service.start("corp", ADDR).await.expect("start");
        let callback = provider.authorize(&start.url);
        let parameters = OidcCallback { code: Some(callback.code), state: Some(callback.state), ..OidcCallback::default() };
        service.finish(start.transaction.expose(), parameters, ADDR, DEVICE).await
    });
    assert!(handle.await.expect("the task ends").is_ok());
}

// ---------------------------------------------------------------------------------------------------------------------
// Task 2: the verification matrix
// ---------------------------------------------------------------------------------------------------------------------

/// A sign-in against a provider that misbehaves in the way `setup` says: it must fail with `reason`, make no account and no
/// session, consume the transaction and write one `signin.fail` line.
async fn fails_with(tag: &str, reason: &str, setup: impl FnOnce(&mut Behaviour)) -> Fixture {
    let f = fixture(tag);
    f.provider.set(setup);
    let result = f.sign_in().await;
    println!("{tag}: {:?} class {reason}", result.as_ref().err());
    f.assert_failed(result, reason, 0, 0);
    assert_eq!(f.failures().len(), 1, "exactly one failure line");
    assert_eq!(f.count("signin.ok"), 0);
    assert_eq!(f.service.open_transactions(), 0, "the transaction is consumed");
    f
}

#[tokio::test]
async fn matrix_a_token_of_another_issuer_signs_nobody_in() {
    fails_with("m-issuer", "token", |b| b.issuer = Some("https://evil.example.test".to_owned())).await;
}

#[tokio::test]
async fn matrix_a_token_for_another_audience_signs_nobody_in() {
    fails_with("m-aud", "token", |b| b.audience = Some(vec!["another-client".to_owned()])).await;
}

#[tokio::test]
async fn matrix_two_audiences_with_an_authorized_party_of_another_client_sign_nobody_in() {
    fails_with("m-azp", "token", |b| {
        b.audience = Some(vec![CLIENT_ID.to_owned(), "another-client".to_owned()]);
        b.azp = Some("another-client".to_owned());
    })
    .await;
}

#[tokio::test]
async fn matrix_an_authorized_party_of_another_client_is_refused_even_with_one_audience() {
    fails_with("m-azp-one", "token", |b| b.azp = Some("another-client".to_owned())).await;
}

#[tokio::test]
async fn matrix_an_authorized_party_that_is_the_client_is_accepted() {
    let f = fixture("m-azp-ok");
    f.provider.set(|b| b.azp = Some(CLIENT_ID.to_owned()));
    f.sign_in().await.expect("azp equal to the client id is fine");
}

#[tokio::test]
async fn matrix_a_nonce_that_differs_signs_nobody_in() {
    fails_with("m-nonce", "token", |b| b.nonce = Some(Some("not-the-nonce".to_owned()))).await;
}

#[tokio::test]
async fn matrix_a_token_without_a_nonce_signs_nobody_in() {
    fails_with("m-nonce-missing", "token", |b| b.nonce = Some(None)).await;
}

#[tokio::test]
async fn matrix_an_expired_token_signs_nobody_in() {
    fails_with("m-exp", "token", |b| b.exp_in = -10).await;
}

#[tokio::test]
async fn matrix_a_token_issued_far_in_the_future_signs_nobody_in() {
    fails_with("m-iat", "token", |b| b.iat_offset = 3_600).await;
}

#[tokio::test]
async fn matrix_a_token_issued_a_little_ahead_is_accepted() {
    let f = fixture("m-iat-ok");
    f.provider.set(|b| b.iat_offset = 299);
    f.sign_in().await.expect("a small skew is fine");
}

#[tokio::test]
async fn matrix_a_signature_made_with_another_key_signs_nobody_in() {
    let f = fails_with("m-sig", "token", |b| b.signer = Signer::Other).await;
    assert_eq!(f.provider.jwks_requests.load(Ordering::SeqCst), 1, "a bad signature is not a reason to fetch keys again");
}

#[tokio::test]
async fn matrix_an_unknown_key_id_fetches_the_keys_once_and_still_fails() {
    let f = fails_with("m-kid", "token", |b| b.kid = Some("unknown-kid".to_owned())).await;
    assert_eq!(f.provider.jwks_requests.load(Ordering::SeqCst), 2, "the first fetch with the discovery, one refetch");
}

#[tokio::test]
async fn matrix_alg_none_signs_nobody_in() {
    fails_with("m-none", "token", |b| b.signer = Signer::None).await;
}

#[tokio::test]
async fn matrix_a_symmetric_token_signed_with_the_client_secret_signs_nobody_in() {
    fails_with("m-hs256", "token", |b| b.signer = Signer::Hs256).await;
}

#[tokio::test]
async fn matrix_a_token_without_a_subject_signs_nobody_in() {
    fails_with("m-sub", "claims", |b| b.omit_sub = true).await;
}

#[tokio::test]
async fn matrix_an_unverified_email_does_not_satisfy_the_domain_rule() {
    fails_with("m-unverified", "denied", |b| b.email_verified = false).await;
}

#[tokio::test]
async fn matrix_a_subject_outside_the_allowed_domain_gets_no_account() {
    fails_with("m-domain", "denied", |b| b.email = Some("a@other.org".to_owned())).await;
}

#[tokio::test]
async fn matrix_a_disabled_account_is_refused_like_any_other_denial() {
    let f = fixture("m-disabled");
    let first = f.sign_in().await.expect("first sign-in");
    f.store.write(|tx| tx.account_set_state(first.account, AccountState::Disabled)).expect("disable");
    let sessions = f.session_rows();
    let accounts = f.accounts();
    let result = f.sign_in().await;
    f.assert_failed(result, "denied", sessions, accounts);
}

#[tokio::test]
async fn matrix_an_error_in_the_callback_ends_the_flow() {
    let f = fixture("m-error");
    let (start, callback) = f.begin().await;
    let parameters = OidcCallback { error: Some("access_denied".to_owned()), state: Some(callback.state.clone()), ..OidcCallback::default() };
    let result = f.service.finish(start.transaction.expose(), parameters, ADDR, DEVICE).await;
    f.assert_failed(result, "provider", 0, 0);
    assert_eq!(f.provider.token_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn matrix_a_callback_without_a_code_ends_the_flow() {
    let f = fixture("m-nocode");
    let (start, callback) = f.begin().await;
    let parameters = OidcCallback { state: Some(callback.state.clone()), ..OidcCallback::default() };
    let result = f.service.finish(start.transaction.expose(), parameters, ADDR, DEVICE).await;
    f.assert_failed(result, "provider", 0, 0);
    assert_eq!(f.provider.token_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn matrix_a_callback_naming_another_issuer_ends_the_flow() {
    let f = fixture("m-iss");
    let (start, callback) = f.begin().await;
    let parameters = OidcCallback {
        code: Some(callback.code.clone()),
        state: Some(callback.state.clone()),
        iss: Some("https://evil.example.test".to_owned()),
        ..OidcCallback::default()
    };
    let result = f.service.finish(start.transaction.expose(), parameters, ADDR, DEVICE).await;
    f.assert_failed(result, "provider", 0, 0);
    assert_eq!(f.provider.token_requests.load(Ordering::SeqCst), 0, "the code was not even sent");
}

#[tokio::test]
async fn matrix_a_callback_naming_the_right_issuer_is_accepted() {
    let f = fixture("m-iss-ok");
    let (start, callback) = f.begin().await;
    let parameters = OidcCallback {
        code: Some(callback.code.clone()),
        state: Some(callback.state.clone()),
        iss: Some(ISSUER.to_owned()),
        ..OidcCallback::default()
    };
    f.service.finish(start.transaction.expose(), parameters, ADDR, DEVICE).await.expect("the issuer matches");
}

#[tokio::test]
async fn matrix_a_token_endpoint_that_answers_500_ends_the_flow() {
    fails_with("m-500", "provider", |b| b.token_status = 500).await;
}

#[tokio::test]
async fn matrix_a_token_response_without_an_id_token_is_a_failure() {
    fails_with("m-noid", "token", |b| b.omit_id_token = true).await;
}

#[tokio::test]
async fn matrix_the_access_token_hash_is_checked_when_the_token_carries_one() {
    fails_with("m-athash-bad", "token", |b| b.at_hash = AtHash::Wrong).await;
    let f = fixture("m-athash-ok");
    f.provider.set(|b| b.at_hash = AtHash::Correct);
    f.sign_in().await.expect("a correct hash is fine");
}

// ---- the groups claim ----

fn staff_rules(c: &mut config::Oidc) {
    c.allowed_domains.clear();
    c.allowed_groups = vec!["staff".to_owned()];
}

#[tokio::test]
async fn groups_a_listed_group_creates_the_account_and_another_case_does_not() {
    let f = fixture_with("groups", staff_rules);
    f.provider.set(|b| {
        b.sub = "member".to_owned();
        b.email = Some("m@other.org".to_owned());
        b.groups = Some(json!(["staff", "x"]));
    });
    let grant = f.sign_in().await.expect("a member of staff signs in");
    assert_eq!(f.account_of("member"), Some(grant.account));
    f.provider.set(|b| {
        b.sub = "shouting".to_owned();
        b.email = Some("s@other.org".to_owned());
        b.groups = Some(json!(["Staff"]));
    });
    let sessions = f.session_rows();
    let result = f.sign_in().await;
    f.assert_failed(result, "denied", sessions, 1);
    assert_eq!(f.account_of("shouting"), None);
}

#[tokio::test]
async fn groups_the_claim_name_comes_from_the_configuration() {
    let f = fixture_with("groups-name", |c| {
        staff_rules(c);
        c.groups_claim = Some("roles".to_owned());
    });
    // The provider lists the group under `groups`, but the configuration reads `roles`.
    f.provider.set(|b| {
        b.email = Some("m@other.org".to_owned());
        b.groups = Some(json!(["staff"]));
    });
    let result = f.sign_in().await;
    f.assert_failed(result, "denied", 0, 0);
    f.provider.set(|b| {
        b.email = Some("m@other.org".to_owned());
        b.groups = Some(json!(["staff"]));
        b.groups_claim = "roles".to_owned();
    });
    f.sign_in().await.expect("the named claim is read");
}

#[tokio::test]
async fn groups_a_claim_that_is_not_a_list_of_strings_gives_no_group() {
    let f = fixture_with("groups-shape", staff_rules);
    f.provider.set(|b| {
        b.email = Some("m@other.org".to_owned());
        b.groups = Some(json!({"staff": true}));
    });
    let result = f.sign_in().await;
    f.assert_failed(result, "denied", 0, 0);
}

// ---------------------------------------------------------------------------------------------------------------------
// Task 2: the bounds
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn bounds_oversize_callback_parameters_end_before_any_request() {
    for (name, oversize) in [("code", 2_049usize), ("state", 257), ("error", 65)] {
        let f = fixture(&format!("b-{name}"));
        let (start, callback) = f.begin().await;
        let long = "a".repeat(oversize);
        let mut parameters =
            OidcCallback { code: Some(callback.code.clone()), state: Some(callback.state.clone()), ..OidcCallback::default() };
        match name {
            "code" => parameters.code = Some(long),
            "state" => parameters.state = Some(long),
            _ => parameters.error = Some(long),
        }
        let before = f.provider.requests.load(Ordering::SeqCst);
        let result = f.service.finish(start.transaction.expose(), parameters, ADDR, DEVICE).await;
        assert_eq!(result.err(), Some(OidcError::Failed), "{name}");
        assert_eq!(f.provider.requests.load(Ordering::SeqCst), before, "{name}: the provider was not contacted");
        assert_eq!(f.failures().len(), 1, "{name}");
        assert_eq!(f.service.open_transactions(), 0, "{name}");
        println!("bound {name}: {oversize} bytes refused before any request");
    }
}

#[tokio::test]
async fn bounds_a_code_of_exactly_two_kib_reaches_the_provider() {
    let f = fixture("b-code-edge");
    let (start, callback) = f.begin().await;
    let parameters = OidcCallback { code: Some("a".repeat(2_048)), state: Some(callback.state.clone()), ..OidcCallback::default() };
    let result = f.service.finish(start.transaction.expose(), parameters, ADDR, DEVICE).await;
    f.assert_failed(result, "provider", 0, 0);
    assert_eq!(f.provider.token_requests.load(Ordering::SeqCst), 1, "the code is within the bound, so it was sent");
}

/// An address for the n-th client.
fn client(n: u32) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(10, (n >> 8) as u8, n as u8, 1))
}

async fn finish_for(f: &Fixture, start: &OidcStart, addr: IpAddr) -> Result<OidcGrant, OidcError> {
    let callback = f.provider.authorize(&start.url);
    let parameters = OidcCallback { code: Some(callback.code), state: Some(callback.state), ..OidcCallback::default() };
    f.service.finish(start.transaction.expose(), parameters, addr, DEVICE).await
}

#[tokio::test]
async fn bounds_the_1025th_start_pushes_out_the_oldest() {
    let f = fixture("b-total");
    let mut starts = Vec::new();
    for n in 0..1_024u32 {
        starts.push((client(n), f.service.start("corp", client(n)).await.expect("start")));
    }
    assert_eq!(f.service.open_transactions(), 1_024);
    let extra = f.service.start("corp", client(5_000)).await.expect("the 1025th start");
    assert_eq!(f.service.open_transactions(), 1_024, "still 1,024");
    let (addr, oldest) = &starts[0];
    assert_eq!(finish_for(&f, oldest, *addr).await.err(), Some(OidcError::Failed), "the oldest is gone");
    let (addr, second) = &starts[1];
    finish_for(&f, second, *addr).await.expect("the second oldest still works");
    finish_for(&f, &extra, client(5_000)).await.expect("the newest works");
}

#[tokio::test]
async fn bounds_the_17th_start_of_one_address_pushes_out_that_addresses_oldest_only() {
    let f = fixture("b-address");
    let other = f.service.start("corp", client(9)).await.expect("another client starts first");
    let mut starts = Vec::new();
    for _ in 0..17 {
        starts.push(f.service.start("corp", client(1)).await.expect("start"));
    }
    assert_eq!(f.service.open_transactions(), 17, "16 of the one address and the other client's");
    assert_eq!(finish_for(&f, &starts[0], client(1)).await.err(), Some(OidcError::Failed), "the oldest of the address is gone");
    finish_for(&f, &starts[1], client(1)).await.expect("the next one works");
    finish_for(&f, &other, client(9)).await.expect("the older transaction of another address is untouched");
}

#[tokio::test]
async fn bounds_a_transaction_lives_ten_minutes_and_not_a_second_more() {
    let f = fixture("b-ttl");
    let (start, callback) = f.begin().await;
    f.clock.advance(600);
    f.finish(&start, &callback).await.expect("at 10 minutes exactly it works");
    let (start, callback) = f.begin().await;
    f.clock.advance(601);
    let accounts = f.accounts();
    let sessions = f.session_rows();
    let result = f.finish(&start, &callback).await;
    f.assert_failed(result, "state", sessions, accounts);
    assert_eq!(f.service.open_transactions(), 0, "an expired transaction is removed");
}

#[tokio::test]
async fn bounds_a_transaction_of_one_provider_does_not_finish_with_a_token_of_another() {
    let scratch = Scratch::new("b-two");
    let clock = Arc::new(ManualClock::new(NOW));
    let a = TestProvider::new("idp-a.example.test", clock.clone(), Some(SECRET));
    let b = TestProvider::new("idp-b.example.test", clock.clone(), Some(SECRET));
    let secret = scratch.write_file("secret", SECRET.as_bytes(), 0o600);
    let configs = [entry("a", &a.issuer, secret.clone()), entry("b", &b.issuer, secret)];
    let store = Arc::new(Store::open(scratch.path()).expect("store"));
    let audit = Arc::new(Audit::open(scratch.path(), clock.clone()).expect("audit"));
    let sessions = Arc::new(SessionManager::new(store.clone(), clock.clone(), Sessions::default()));
    let accounts = Arc::new(Accounts::new(store.clone(), clock.clone(), audit.clone(), RANGE));
    let service = OidcService::new(
        &configs,
        PUBLIC_URL,
        accounts,
        sessions,
        audit,
        clock.clone(),
        MemoryHttp::new(&[a.clone(), b.clone()]),
    )
    .expect("service");
    // Provider a answers the code exchange with a token that says it is from b.
    a.set(|x| x.issuer = Some(b.issuer.clone()));
    let start = service.start("a", ADDR).await.expect("start at a");
    let callback = a.authorize(&start.url);
    let parameters = OidcCallback { code: Some(callback.code), state: Some(callback.state), ..OidcCallback::default() };
    let result = service.finish(start.transaction.expose(), parameters, ADDR, DEVICE).await;
    assert_eq!(result.err(), Some(OidcError::Failed));
    let lines = audit_lines(scratch.path());
    let last = lines.iter().rev().find(|l| l["action"] == "signin.fail").expect("a failure line");
    assert_eq!(last["detail"]["reason"], "token");
    // The providers are listed in configuration order, with nothing else.
    let listed = service.providers();
    assert_eq!(listed.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bounds_two_threads_finishing_one_transaction_give_exactly_one_success() {
    let f = fixture("b-race");
    for round in 0..5 {
        let (start, callback) = f.begin().await;
        let value = start.transaction.expose().to_owned();
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let service = f.service.clone();
            let (value, callback) = (value.clone(), callback.clone());
            tasks.push(tokio::spawn(async move {
                let parameters = OidcCallback { code: Some(callback.code), state: Some(callback.state), ..OidcCallback::default() };
                service.finish(&value, parameters, ADDR, DEVICE).await.is_ok()
            }));
        }
        let mut ok = 0;
        for task in tasks {
            ok += usize::from(task.await.expect("the task ends"));
        }
        assert_eq!(ok, 1, "round {round}: exactly one success");
        println!("race round {round}: ok {ok}, failed {}", 8 - ok);
    }
    assert_eq!(f.count("signin.ok"), 5);
    assert_eq!(f.failures().len(), 35);
}

#[test]
fn bounds_the_state_is_compared_in_constant_time() {
    let source = include_str!("../src/auth/oidc.rs");
    assert!(source.contains("ct_eq(state.as_bytes())"), "the state goes through the constant-time comparison");
    assert!(source.contains("use subtle::ConstantTimeEq"));
    assert!(!source.contains("== state"), "no plain comparison of the state");
    assert!(!source.contains("state =="), "no plain comparison of the state");
}

#[test]
fn bounds_the_module_uses_no_unchecked_verifier_option() {
    let source = include_str!("../src/auth/oidc.rs").to_ascii_lowercase();
    assert!(!source.contains("insecure"));
    assert!(!source.contains("allow_any_alg"));
    assert!(!source.contains("new_confidential_client"));
}

// ---- the key set ----

#[tokio::test]
async fn refetch_ten_unknown_key_ids_in_a_minute_cause_one_refetch() {
    let f = fixture("r-ten");
    f.provider.set(|b| b.kid = Some("unknown-kid".to_owned()));
    for i in 0..10 {
        let (start, callback) = f.begin().await;
        let result = f.finish(&start, &callback).await;
        assert_eq!(result.err(), Some(OidcError::Failed), "token {i}");
        f.clock.advance(5);
    }
    println!(
        "ten unknown key ids: discovery {} key set {} token {}",
        f.provider.discovery_requests.load(Ordering::SeqCst),
        f.provider.jwks_requests.load(Ordering::SeqCst),
        f.provider.token_requests.load(Ordering::SeqCst)
    );
    assert_eq!(f.provider.discovery_requests.load(Ordering::SeqCst), 1);
    assert_eq!(f.provider.jwks_requests.load(Ordering::SeqCst), 2, "the fetch with the discovery and one refetch");
    f.clock.advance(300);
    let (start, callback) = f.begin().await;
    assert!(f.finish(&start, &callback).await.is_err());
    assert_eq!(f.provider.jwks_requests.load(Ordering::SeqCst), 3, "after five minutes another refetch is allowed");
}

#[tokio::test]
async fn refetch_a_rotated_key_is_found_after_one_refetch() {
    let f = fixture("r-rotate");
    f.sign_in().await.expect("the first key works and is cached");
    assert_eq!(f.provider.jwks_requests.load(Ordering::SeqCst), 1);
    f.provider.publish_second_key();
    f.provider.set(|b| b.signer = Signer::Second);
    f.sign_in().await.expect("the new key is fetched and the token is checked against it");
    assert_eq!(f.provider.jwks_requests.load(Ordering::SeqCst), 2);
    f.sign_in().await.expect("the fresh set is kept");
    assert_eq!(f.provider.jwks_requests.load(Ordering::SeqCst), 2);
}

// ---- nothing secret is shown ----

#[tokio::test]
async fn canary_no_secret_code_state_nonce_or_token_reaches_an_error_a_debug_text_or_the_audit_file() {
    let f = fixture("canary");
    let (start, callback) = f.begin().await;
    let nonce = TestProvider::query_value(&start.url, "nonce").expect("nonce");
    let challenge = TestProvider::query_value(&start.url, "code_challenge").expect("challenge");
    let transaction = start.transaction.expose().to_owned();
    let mut shown = vec![
        format!("{start:?}"),
        format!("{:?}", start.transaction),
        format!("{:?}", f.service),
        format!("{:?}", OidcCallback { code: Some(callback.code.clone()), state: Some(callback.state.clone()), ..OidcCallback::default() }),
    ];
    let grant = f.finish(&start, &callback).await.expect("sign in");
    shown.push(format!("{grant:?}"));
    // Failures of every class.
    f.provider.set(|b| b.nonce = Some(Some("wrong".to_owned())));
    let (s2, c2) = f.begin().await;
    let error = f.finish(&s2, &c2).await.expect_err("fails");
    shown.push(format!("{error:?} {error}"));
    f.provider.set(|b| b.token_status = 500);
    let (s3, c3) = f.begin().await;
    shown.push(format!("{:?}", f.finish(&s3, &c3).await.err()));
    let audit = std::fs::read_dir(f.scratch.path().join("server/audit"))
        .expect("audit dir")
        .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("read"))
        .collect::<String>();
    shown.push(audit);
    let session = grant.session.value.expose().to_owned();
    for text in &shown {
        for secret in [SECRET, &callback.code, &callback.state, &nonce, &challenge, &transaction, &session, oidc_provider::ACCESS_TOKEN] {
            assert!(!text.contains(secret), "a secret value appears in: {}", &text[..text.len().min(80)]);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Task 3: the discovery cache and the providers
// ---------------------------------------------------------------------------------------------------------------------

fn requests(f: &Fixture) -> (usize, usize) {
    (f.provider.discovery_requests.load(Ordering::SeqCst), f.provider.jwks_requests.load(Ordering::SeqCst))
}

#[tokio::test]
async fn cache_a_second_start_within_the_hour_makes_no_discovery_request() {
    let f = fixture("c-hit");
    f.service.start("corp", ADDR).await.expect("first start");
    assert_eq!(requests(&f), (1, 1));
    f.clock.advance(3_599);
    f.service.start("corp", ADDR).await.expect("second start");
    assert_eq!(requests(&f), (1, 1), "still cached 3,599 seconds later");
}

#[tokio::test]
async fn cache_the_discovery_is_fetched_again_once_the_hour_has_passed() {
    let f = fixture("c-expire");
    f.sign_in().await.expect("first sign-in");
    f.clock.advance(3_600);
    f.sign_in().await.expect("sign-in after an hour");
    println!("discovery requests over the clock steps 0, 3599, 3600: {:?}", requests(&f));
    assert_eq!(requests(&f), (2, 2), "one more discovery and key set after the hour");
    f.clock.advance(3_599);
    f.sign_in().await.expect("and cached again");
    assert_eq!(requests(&f), (2, 2));
}

#[tokio::test]
async fn cache_a_discovery_document_of_another_issuer_is_refused() {
    let f = fixture("c-issuer");
    f.provider.set(|b| b.discovery_issuer = Some("https://evil.example.test".to_owned()));
    assert_eq!(f.service.start("corp", ADDR).await.err(), Some(OidcError::Failed));
    let failures = f.failures();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["detail"]["reason"], "provider");
    assert_eq!(f.service.open_transactions(), 0);
}

#[tokio::test]
async fn cache_an_unreachable_provider_fails_alone_and_the_other_keeps_working() {
    let scratch = Scratch::new("c-two");
    let clock = Arc::new(ManualClock::new(NOW));
    let a = TestProvider::new("idp-a.example.test", clock.clone(), Some(SECRET));
    let b = TestProvider::new("idp-b.example.test", clock.clone(), Some(SECRET));
    let secret = scratch.write_file("secret", SECRET.as_bytes(), 0o600);
    let configs = [entry("a", &a.issuer, secret.clone()), entry("b", &b.issuer, secret)];
    let store = Arc::new(Store::open(scratch.path()).expect("store"));
    let audit = Arc::new(Audit::open(scratch.path(), clock.clone()).expect("audit"));
    let sessions = Arc::new(SessionManager::new(store.clone(), clock.clone(), Sessions::default()));
    let accounts = Arc::new(Accounts::new(store, clock.clone(), audit.clone(), RANGE));
    let service =
        OidcService::new(&configs, PUBLIC_URL, accounts, sessions, audit, clock, MemoryHttp::new(&[a.clone(), b.clone()]))
            .expect("service");
    a.set(|x| x.unreachable = true);
    assert_eq!(service.start("a", ADDR).await.err(), Some(OidcError::Failed));
    let start = service.start("b", ADDR).await.expect("b still works");
    let callback = b.authorize(&start.url);
    let parameters = OidcCallback { code: Some(callback.code), state: Some(callback.state), ..OidcCallback::default() };
    service.finish(start.transaction.expose(), parameters, ADDR, DEVICE).await.expect("b signs in");
    let lines = audit_lines(scratch.path());
    let failures: Vec<&Value> = lines.iter().filter(|l| l["action"] == "signin.fail").collect();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["detail"]["reason"], "provider");
}

#[tokio::test]
async fn cache_a_failed_discovery_is_not_tried_again_for_five_seconds() {
    let f = fixture("c-backoff");
    f.provider.set(|b| b.discovery_status = 500);
    assert!(f.service.start("corp", ADDR).await.is_err());
    assert_eq!(requests(&f).0, 1);
    f.provider.reset();
    f.clock.advance(4);
    assert!(f.service.start("corp", ADDR).await.is_err(), "the provider is left alone");
    assert_eq!(requests(&f).0, 1, "no request was made");
    f.clock.advance(1);
    f.service.start("corp", ADDR).await.expect("after five seconds the provider is asked again");
    assert_eq!(requests(&f).0, 2);
}

#[tokio::test]
async fn cache_the_provider_list_has_the_id_and_the_name_in_configuration_order() {
    let f = fixture("c-list");
    let listed = f.service.providers();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "corp");
    assert_eq!(listed[0].name, "Company corp");
    assert_eq!(format!("{:?}", listed[0]), "ProviderInfo { id: \"corp\", name: \"Company corp\" }");
}

// ---------------------------------------------------------------------------------------------------------------------
// Task 3: the client secret file
// ---------------------------------------------------------------------------------------------------------------------

fn expected_basic() -> String {
    use base64::Engine;
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{CLIENT_ID}:{SECRET}")))
}

#[tokio::test]
async fn secret_a_private_file_is_read_trimmed_and_used_as_the_client_authentication() {
    let f = fixture("s-basic");
    f.sign_in().await.expect("sign in");
    let request = f.provider.last_token_request().expect("token request");
    assert_eq!(request.authorization.as_deref(), Some(expected_basic().as_str()));
    assert!(!request.form.iter().any(|(k, _)| k == "client_secret"), "the secret is not in the form");
    assert!(!request.form.iter().any(|(k, _)| k == "client_id"), "basic authentication carries the id");
    // The same file read directly: one trailing newline is dropped, nothing else.
    let scratch = Scratch::new("s-read");
    let path = scratch.write_file("secret", b"value-with-space \n", 0o640);
    assert_eq!(read_client_secret(&path).expect("0640 is private enough").expose(), "value-with-space ");
    let path = scratch.write_file("exact", &vec![b'x'; MAX_SECRET_FILE_BYTES as usize], 0o600);
    assert_eq!(read_client_secret(&path).expect("4 KiB is allowed").expose().len(), 4_096);
}

#[test]
fn secret_a_file_that_is_not_private_a_link_an_empty_or_a_large_file_is_refused() {
    let scratch = Scratch::new("s-refuse");
    let canary = "CANARY-SECRET-CONTENT";
    let open = scratch.write_file("open", canary.as_bytes(), 0o644);
    let target = scratch.write_file("target", canary.as_bytes(), 0o600);
    let link = scratch.join("link");
    std::os::unix::fs::symlink(&target, &link).expect("make a link");
    let empty = scratch.write_file("empty", b"\n", 0o600);
    let large = scratch.write_file("large", &vec![b'y'; MAX_SECRET_FILE_BYTES as usize + 1], 0o600);
    let missing = scratch.join("missing");
    let cases = [
        ("mode 0644", open, "too open"),
        ("a link", link, "link"),
        ("empty", empty, "empty"),
        ("larger than 4 KiB", large, "larger than 4096"),
        ("missing", missing, "does not exist"),
    ];
    for (what, path, word) in cases {
        let error = read_client_secret(&path).err().unwrap_or_else(|| panic!("{what} must be refused"));
        let sentence = error.sentence();
        println!("secret file refused ({what}): {sentence}");
        assert!(sentence.contains(&path.display().to_string()), "{what}: the sentence names the path: {sentence}");
        assert!(sentence.contains(word), "{what}: the sentence names the problem: {sentence}");
        assert!(!sentence.contains(canary) && !sentence.contains("yyyy"), "{what}: no content: {sentence}");
        assert!(!format!("{error:?}").contains(canary));
    }
}

#[test]
fn secret_a_refused_file_stops_the_service_from_being_built_with_the_same_sentence() {
    let scratch = Scratch::new("s-service");
    let clock = Arc::new(ManualClock::new(NOW));
    let open = scratch.write_file("open", SECRET.as_bytes(), 0o644);
    let store = Arc::new(Store::open(scratch.path()).expect("store"));
    let audit = Arc::new(Audit::open(scratch.path(), clock.clone()).expect("audit"));
    let sessions = Arc::new(SessionManager::new(store.clone(), clock.clone(), Sessions::default()));
    let accounts = Arc::new(Accounts::new(store, clock.clone(), audit.clone(), RANGE));
    let provider = TestProvider::standard(clock.clone());
    let refusal = OidcService::new(
        &[entry("corp", ISSUER, open.clone())],
        PUBLIC_URL,
        accounts,
        sessions,
        audit,
        clock,
        MemoryHttp::new(&[provider]),
    )
    .expect_err("a file open to others is refused");
    assert!(refusal.sentence().contains(&open.display().to_string()));
    assert!(refusal.sentence().contains("too open"));
    assert!(!refusal.sentence().contains(SECRET));
}

#[tokio::test]
async fn secret_a_provider_without_a_secret_file_is_a_public_client() {
    let scratch = Scratch::new("s-public");
    let clock = Arc::new(ManualClock::new(NOW));
    let provider = TestProvider::new("idp.example.test", clock.clone(), None);
    let store = Arc::new(Store::open(scratch.path()).expect("store"));
    let audit = Arc::new(Audit::open(scratch.path(), clock.clone()).expect("audit"));
    let sessions = Arc::new(SessionManager::new(store.clone(), clock.clone(), Sessions::default()));
    let accounts = Arc::new(Accounts::new(store, clock.clone(), audit.clone(), RANGE));
    let service = OidcService::new(
        &[entry("corp", ISSUER, PathBuf::new())],
        PUBLIC_URL,
        accounts,
        sessions,
        audit,
        clock,
        MemoryHttp::new(std::slice::from_ref(&provider)),
    )
    .expect("a public client needs no file");
    let start = service.start("corp", ADDR).await.expect("start");
    let callback = provider.authorize(&start.url);
    let parameters = OidcCallback { code: Some(callback.code), state: Some(callback.state), ..OidcCallback::default() };
    service.finish(start.transaction.expose(), parameters, ADDR, DEVICE).await.expect("PKCE alone is enough");
    let request = provider.last_token_request().expect("token request");
    assert_eq!(request.authorization, None, "no Authorization header");
    assert!(request.form.iter().any(|(k, v)| k == "client_id" && v == CLIENT_ID), "the client id is in the form");
    assert!(request.form.iter().any(|(k, _)| k == "code_verifier"));
}

#[test]
fn secret_the_debug_texts_print_no_secret_and_the_secret_type_wipes_on_drop() {
    fn wipes<T: zeroize::ZeroizeOnDrop>() {}
    let scratch = Scratch::new("s-debug");
    let path = scratch.write_file("secret", SECRET.as_bytes(), 0o600);
    let secret = read_client_secret(&path).expect("read");
    wipes::<holzwave_server::auth::oidc_http::ClientSecretText>();
    assert!(std::mem::needs_drop::<holzwave_server::auth::oidc_http::ClientSecretText>());
    let text = format!("{secret:?} {:#?}", entry("corp", ISSUER, path.clone()));
    assert!(!text.contains(SECRET), "{text}");
    assert_eq!(format!("{secret:?}"), "ClientSecretText(..)");
    let f = fixture("s-debug-service");
    assert!(!format!("{:?}", f.service).contains(SECRET));
    assert_eq!(format!("{:?}", f.service), "OidcService(..)");
}

// ---------------------------------------------------------------------------------------------------------------------
// Task 3: the HTTP client, against a plain local server
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct LocalServer {
    port: u16,
    accepts: AtomicUsize,
    second_hits: AtomicUsize,
    sent: AtomicUsize,
    cookies: Mutex<Vec<bool>>,
}

async fn local_server() -> Arc<LocalServer> {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind a local port");
    let port = listener.local_addr().expect("address").port();
    let server = Arc::new(LocalServer { port, ..LocalServer::default() });
    let shared = server.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            shared.accepts.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(serve(shared.clone(), stream));
        }
    });
    server
}

async fn serve(server: Arc<LocalServer>, mut stream: TcpStream) {
    let mut head = Vec::new();
    let mut buffer = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buffer[..n]),
        }
        if head.len() > 16_384 {
            return;
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
    server.cookies.lock().expect("lock").push(text.to_ascii_lowercase().contains("\r\ncookie:"));
    let chunk = vec![b'z'; 64 * 1024];
    match path.as_str() {
        "/ok" => {
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nSet-Cookie: session=abc\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .await;
        }
        "/exact" | "/declared" => {
            let length = if path == "/exact" { MAX_RESPONSE_BYTES } else { MAX_RESPONSE_BYTES + 1 };
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n");
            if stream.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            let mut left = length;
            while left > 0 {
                let n = left.min(chunk.len());
                if stream.write_all(&chunk[..n]).await.is_err() {
                    return;
                }
                server.sent.fetch_add(n, Ordering::SeqCst);
                left -= n;
            }
        }
        "/stream" => {
            if stream.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n").await.is_err() {
                return;
            }
            for _ in 0..1_024 {
                if stream.write_all(&chunk).await.is_err() {
                    return;
                }
                server.sent.fetch_add(chunk.len(), Ordering::SeqCst);
            }
        }
        "/redirect" => {
            let _ = stream
                .write_all(b"HTTP/1.1 302 Found\r\nLocation: /second\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
        }
        "/second" => {
            server.second_hits.fetch_add(1, Ordering::SeqCst);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        }
        "/silent" => tokio::time::sleep(Duration::from_secs(30)).await,
        _ => {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        }
    }
}

fn get(server: &LocalServer, path: &str) -> HttpRequest {
    openidconnect::http::Request::builder()
        .method("GET")
        .uri(format!("http://127.0.0.1:{}{path}", server.port))
        .body(Vec::new())
        .expect("a request")
}

fn plain_client() -> HttpsClient {
    HttpsClient::new(false).expect("the client is built")
}

#[test]
fn adapter_the_limits_are_ten_seconds_and_one_mib() {
    assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(10));
    assert_eq!(TOTAL_TIMEOUT, Duration::from_secs(10));
    assert_eq!(MAX_RESPONSE_BYTES, 1024 * 1024);
    assert_eq!(Limits::PRODUCTION, Limits { connect: CONNECT_TIMEOUT, total: TOTAL_TIMEOUT, max_bytes: MAX_RESPONSE_BYTES });
    assert_eq!(format!("{:?}", plain_client()), "HttpsClient(..)");
}

#[tokio::test]
async fn adapter_reads_exactly_one_mib_and_refuses_one_byte_more() {
    let server = local_server().await;
    let client = plain_client();
    let answer = client.call(get(&server, "/exact")).await.expect("exactly 1 MiB is read");
    assert_eq!(answer.body().len(), MAX_RESPONSE_BYTES);
    assert_eq!(answer.status(), 200);
    let error = client.call(get(&server, "/declared")).await.expect_err("1 MiB and a byte is refused");
    assert_eq!(error, HttpError::TooLarge);
}

#[tokio::test]
async fn adapter_does_not_buffer_past_the_cap_and_closes_the_connection() {
    let server = local_server().await;
    let client = plain_client();
    let error = client.call(get(&server, "/stream")).await.expect_err("a stream without end is refused");
    assert_eq!(error, HttpError::TooLarge);
    // The server tried to send 64 MiB; it must notice the closed connection long before that.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let sent = server.sent.load(Ordering::SeqCst);
    println!("stream: the server sent {sent} bytes of 67108864 before the client dropped the connection");
    assert!(sent < 16 * 1024 * 1024, "the server was stopped after {sent} bytes");
}

#[tokio::test]
async fn adapter_returns_a_redirect_and_does_not_follow_it() {
    let server = local_server().await;
    let answer = plain_client().call(get(&server, "/redirect")).await.expect("the redirect is an answer");
    assert_eq!(answer.status(), 302);
    assert!(answer.headers().contains_key("location"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(server.second_hits.load(Ordering::SeqCst), 0, "the second path was never requested");
}

#[tokio::test]
async fn adapter_cuts_off_a_server_that_says_nothing() {
    let server = local_server().await;
    let limits = Limits { connect: Duration::from_secs(1), total: Duration::from_secs(1), max_bytes: MAX_RESPONSE_BYTES };
    let client = HttpsClient::with_limits(false, limits).expect("client");
    let started = Instant::now();
    let error = client.call(get(&server, "/silent")).await.expect_err("a silent server is cut off");
    println!("silent server: {error:?} after {:?} with a 1 second limit", started.elapsed());
    assert_eq!(error, HttpError::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn adapter_sends_no_cookie_and_keeps_none() {
    let server = local_server().await;
    let client = plain_client();
    let first = client.call(get(&server, "/ok")).await.expect("first");
    assert!(first.headers().contains_key("set-cookie"), "the answer is passed on as it is");
    client.call(get(&server, "/ok")).await.expect("second");
    assert_eq!(*server.cookies.lock().expect("lock"), vec![false, false], "no request carried a cookie");
}

#[tokio::test]
async fn adapter_with_the_https_flag_on_refuses_http_before_connecting() {
    let server = local_server().await;
    let client = HttpsClient::new(true).expect("client");
    let error = client.call(get(&server, "/ok")).await.expect_err("http is refused");
    assert_eq!(error, HttpError::NotHttps);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(server.accepts.load(Ordering::SeqCst), 0, "no connection was attempted");
    assert!(error.to_string().contains("not https"));
}

#[tokio::test]
async fn adapter_ignores_the_proxy_variables_of_the_environment() {
    let server = local_server().await;
    // A child process of this test binary has the variables set; set_var in a process with threads is not safe.
    let url = format!("http://127.0.0.1:{}/ok", server.port);
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args(["--exact", "adapter_proxy_probe", "--include-ignored", "--nocapture", "--test-threads=1"])
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("http_proxy", "http://127.0.0.1:9")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .env("all_proxy", "http://127.0.0.1:9")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .env("HW_OIDC_PROBE_URL", url)
            .output()
            .expect("the child runs")
    })
    .await
    .expect("the blocking task ends");
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(output.status.success(), "the child failed: {text}");
    assert!(text.contains("proxy probe: 200"), "{text}");
    assert_eq!(server.accepts.load(Ordering::SeqCst), 1, "the request reached the server directly");
}

/// Run by the test above in a child process whose environment names an unusable proxy.
#[tokio::test]
#[ignore = "run by adapter_ignores_the_proxy_variables_of_the_environment"]
async fn adapter_proxy_probe() {
    let url = std::env::var("HW_OIDC_PROBE_URL").expect("the parent names the address");
    let request = openidconnect::http::Request::builder().method("GET").uri(url).body(Vec::new()).expect("request");
    let answer = plain_client().call(request).await.expect("the request goes direct, not through the proxy");
    println!("proxy probe: {}", answer.status().as_u16());
}
