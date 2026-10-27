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
use std::sync::Arc;
use std::sync::atomic::Ordering;

use holzwave_server::accounts::Accounts;
use holzwave_server::audit::Audit;
use holzwave_server::auth::oidc::{OidcCallback, OidcError, OidcGrant, OidcService, OidcStart};
use holzwave_server::auth::session::{Method, SessionManager};
use holzwave_server::clock::ManualClock;
use holzwave_server::config::{self, Sessions};
use holzwave_server::store::{AccountId, AccountState, Role, Store};
use oidc_provider::{Callback, CLIENT_ID, ISSUER, MemoryHttp, SECRET, TestProvider};
use scratch::Scratch;
use serde_json::Value;

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
