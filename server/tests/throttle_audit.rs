//! The password sign-in in two steps, the throttle and lock-out, the step-up and the audit lines they leave.

#[path = "support/scratch.rs"]
mod scratch;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::{Arc, Mutex};

use holzwave_server::audit::Audit;
use holzwave_server::auth::password::{ArgonGate, enrol_password};
use holzwave_server::auth::session::{Method, SessionManager, generate_token};
use holzwave_server::auth::signin::{SignIn, SignInError};
use holzwave_server::auth::throttle::{account_key, address_key};
use holzwave_server::auth::totp;
use holzwave_server::clock::{Clock, ManualClock};
use holzwave_server::config::Sessions;
use holzwave_server::keyfile::ServerKey;
use holzwave_server::limits::Limits;
use holzwave_server::sealed::Sealer;
use holzwave_server::store::{AccountId, NewAccount, Role, Store};
use holzwave_server::unlock::Kdf;
use scratch::Scratch;
use serde_json::Value;
use zeroize::Zeroizing;

/// 2026-03-01T08:30:00Z, a multiple of thirty seconds.
const NOW: i64 = 1_772_353_800;
const RANGE: std::ops::RangeInclusive<u32> = 20000..=29999;
const ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));
const PASSWORD: &str = "correct horse battery";
const WRONG: &str = "wrong horse battery";
const DEVICE: &str = "Firefox on Linux";

struct Fixture {
    scratch: Scratch,
    store: Arc<Store>,
    clock: Arc<ManualClock>,
    sessions: Arc<SessionManager>,
    gate: ArgonGate,
    signin: Arc<SignIn>,
    secrets: Mutex<HashMap<String, Zeroizing<[u8; totp::SECRET_BYTES]>>>,
    sealer: Arc<Sealer>,
}

fn fixture(tag: &str) -> Fixture {
    fixture_with(tag, Limits::DEFAULT)
}

fn fixture_with(tag: &str, limits: Limits) -> Fixture {
    let scratch = Scratch::new(tag);
    let clock = Arc::new(ManualClock::new(NOW));
    let store = Arc::new(Store::open(scratch.path()).expect("open the store"));
    let audit = Arc::new(Audit::open(scratch.path(), clock.clone()).expect("open the audit log"));
    let sessions = Arc::new(SessionManager::new(store.clone(), clock.clone(), Sessions::default()));
    let gate = ArgonGate::new(2, Kdf::lower_bound());
    let sealer = Arc::new(Sealer::new(&ServerKey::from_bytes([7u8; 32]).expect("a key that is not all zero")));
    let signin = Arc::new(
        SignIn::new(store.clone(), clock.clone(), audit, sessions.clone(), gate.clone(), sealer.clone()).with_limits(limits),
    );
    Fixture { scratch, store, clock, sessions, gate, signin, secrets: Mutex::new(HashMap::new()), sealer }
}

impl Fixture {
    fn bare_account(&self, name: &str) -> AccountId {
        self.store
            .write(|tx| tx.account_create(&NewAccount::new(name, Role::Member), RANGE, NOW))
            .expect("account")
            .id
    }

    async fn password(&self, id: AccountId) {
        enrol_password(&self.gate, &self.store, id, PASSWORD, NOW).await.expect("password");
    }

    fn authenticator(&self, name: &str, id: AccountId) {
        let secret = totp::generate_secret().expect("secret");
        let sealed = self.sealer.seal(&totp::seal_context(id), &secret[..]).expect("seal");
        self.store.write(|tx| tx.totp_set(id, &sealed)).expect("store the secret");
        self.secrets.lock().expect("lock").insert(name.to_owned(), secret);
    }

    /// An account with a password and an authenticator.
    async fn person(&self, name: &str) -> AccountId {
        let id = self.bare_account(name);
        self.password(id).await;
        self.authenticator(name, id);
        id
    }

    /// The code the person's app shows now.
    fn code(&self, name: &str) -> String {
        let secrets = self.secrets.lock().expect("lock");
        let secret = secrets.get(name).expect("a secret");
        totp::code_at(&secret[..], self.clock.unix()).expect("code").to_string()
    }

    /// A six-digit code that is none of the three the window accepts.
    fn wrong_code(&self, name: &str) -> String {
        let secrets = self.secrets.lock().expect("lock");
        let secret = secrets.get(name).expect("a secret");
        let now = self.clock.unix();
        let accepted: Vec<String> =
            [-30, 0, 30].iter().map(|d| totp::code_at(&secret[..], now + d).expect("code").to_string()).collect();
        (0..1_000_000).map(|n| format!("{n:06}")).find(|c| !accepted.contains(c)).expect("a code")
    }

    fn lines(&self) -> Vec<Value> {
        audit_lines(self.scratch.path())
    }

    fn count(&self, action: &str) -> usize {
        self.lines().iter().filter(|l| l["action"] == action).count()
    }

    fn admin(&self, name: &str) -> AccountId {
        self.store
            .write(|tx| tx.account_create(&NewAccount::new(name, Role::Admin), RANGE, NOW))
            .expect("administrator")
            .id
    }

    /// The failures counted under a key now.
    fn failures(&self, key: &str) -> i64 {
        self.store.read(|tx| tx.throttle_get(key, self.clock.unix(), 900)).expect("throttle").failures
    }

    fn session_rows(&self, id: AccountId) -> usize {
        self.store.read(|tx| tx.session_list(id)).expect("list").len()
    }
}

/// Every audit line of every day, in order, as JSON.
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

/// A line as one short text for the log of a test: the time and the address are cut short.
fn show(line: &Value) -> String {
    format!(
        "{} {} actor={} addr={} target={} detail={}",
        line["action"].as_str().unwrap_or(""),
        line["result"].as_str().unwrap_or(""),
        line["actor"].as_str().unwrap_or(""),
        line["addr"].as_str().unwrap_or(""),
        line["target"].as_str().unwrap_or(""),
        line["detail"]
    )
}

// ---- the tracer ----

#[tokio::test]
async fn tracer_a_password_and_a_code_make_a_session() {
    let f = fixture("so-tracer");
    let alice = f.person("alice").await;
    let before = f.gate.derivations();
    let token = f.signin.begin_password("alice", PASSWORD, ADDR).await.expect("first step");
    let after_first = f.gate.derivations() - before;
    assert_eq!(token.expose().len(), 43);
    assert_eq!(f.session_rows(alice), 0, "the first step made no session");
    assert_eq!(f.signin.pending_count(), 1);

    let grant = f.signin.finish_totp(token.expose(), &f.code("alice"), ADDR, DEVICE).expect("second step");
    assert_eq!(grant.session.value.expose().len(), 43);
    let found = f.sessions.lookup(grant.session.value.expose(), f.clock.unix()).expect("the session");
    assert_eq!(found.account_id, alice);
    assert_eq!(found.method, Method::PasswordTotp);
    assert_eq!(found.device, DEVICE);
    assert_eq!(f.signin.pending_count(), 0, "the pending state is gone");
    println!("Derivations: after the happy path = {after_first}");
    assert_eq!(after_first, 1);

    // The same token a second time, with a code of the next step, is Invalid: the state is gone.
    f.clock.advance(30);
    let again = f.signin.finish_totp(token.expose(), &f.code("alice"), ADDR, DEVICE);
    assert_eq!(again.expect_err("used up"), SignInError::Invalid);

    let ok: Vec<Value> = f.lines().into_iter().filter(|l| l["action"] == "signin.ok").collect();
    assert_eq!(ok.len(), 1, "one success line");
    println!("Tracer: {}", show(&ok[0]));
    assert_eq!(ok[0]["actor"], alice.get().to_string());
    assert_eq!(ok[0]["addr"], "192.0.2.7");
    assert_eq!(ok[0]["detail"]["method"], "password_totp");
    let text = std::fs::read_to_string(f.scratch.path().join("server/audit/audit-2026-03-01.jsonl")).expect("file");
    assert!(!text.contains(PASSWORD) && !text.contains(token.expose()) && !text.contains(grant.session.value.expose()));
}

#[tokio::test]
async fn tracer_a_wrong_password_is_invalid_and_leaves_one_line() {
    let f = fixture("so-tracer-pw");
    let alice = f.person("alice").await;
    let before = f.gate.derivations();
    let outcome = f.signin.begin_password("alice", WRONG, ADDR).await;
    let derivations = f.gate.derivations() - before;
    println!("Derivations: after a wrong password = {derivations}");
    assert_eq!(outcome.expect_err("wrong"), SignInError::Invalid);
    assert_eq!(derivations, 1);
    assert_eq!(f.signin.pending_count(), 0);
    assert_eq!(f.session_rows(alice), 0);
    let lines = f.lines();
    assert_eq!(lines.len(), 1);
    println!("Tracer: {}", show(&lines[0]));
    assert_eq!(lines[0]["action"], "signin.fail");
    assert_eq!(lines[0]["actor"], "anonymous");
    assert_eq!(lines[0]["target"], alice.get().to_string());
    assert_eq!(lines[0]["detail"]["step"], "password");
    assert_eq!(lines[0]["detail"]["reason"], "invalid");
}

#[tokio::test]
async fn tracer_a_right_password_with_a_wrong_code_makes_no_session() {
    let f = fixture("so-tracer-code");
    let alice = f.person("alice").await;
    let token = f.signin.begin_password("alice", PASSWORD, ADDR).await.expect("first step");
    let outcome = f.signin.finish_totp(token.expose(), &f.wrong_code("alice"), ADDR, DEVICE);
    assert_eq!(outcome.expect_err("wrong code"), SignInError::Invalid);
    assert_eq!(f.session_rows(alice), 0, "no session exists afterwards");
    let lines = f.lines();
    assert_eq!(lines.len(), 1);
    println!("Tracer: {}", show(&lines[0]));
    assert_eq!(lines[0]["action"], "signin.fail");
    assert_eq!(lines[0]["target"], alice.get().to_string());
    assert_eq!(lines[0]["detail"]["step"], "totp");
    // The right code still works afterwards: the state allows three tries.
    let grant = f.signin.finish_totp(token.expose(), &f.code("alice"), ADDR, DEVICE).expect("then the right code");
    assert_eq!(f.session_rows(alice), 1);
    drop(grant);
}

#[tokio::test]
async fn tracer_a_token_that_was_never_issued_is_invalid() {
    let f = fixture("so-tracer-token");
    let alice = f.person("alice").await;
    let stranger = generate_token().expect("a well-formed value").0;
    let outcome = f.signin.finish_totp(stranger.expose(), &f.code("alice"), ADDR, DEVICE);
    assert_eq!(outcome.expect_err("never issued"), SignInError::Invalid);
    for malformed in ["", "short", &"A".repeat(44), &"!".repeat(43)] {
        let outcome = f.signin.finish_totp(malformed, &f.code("alice"), ADDR, DEVICE);
        assert_eq!(outcome.expect_err("malformed"), SignInError::Invalid);
    }
    assert_eq!(f.session_rows(alice), 0);
    assert_eq!(f.signin.pending_count(), 0);
}

// ---- the throttle ----

fn limits(account: u32, address: u32) -> Limits {
    Limits { password_attempts_per_account: account, password_attempts_per_address: address, ..Limits::DEFAULT }
}

async fn wrong(f: &Fixture, name: &str) -> SignInError {
    f.signin.begin_password(name, WRONG, ADDR).await.expect_err("a wrong password")
}

#[tokio::test]
async fn throttle_the_configured_number_of_wrong_passwords_locks_the_name() {
    let f = fixture("th-lock");
    f.person("alice").await;
    for n in 1..=4 {
        assert_eq!(wrong(&f, "alice").await, SignInError::Invalid);
        assert_eq!(f.count("signin.fail"), n);
        assert_eq!(f.count("signin.locked"), 0);
    }
    assert_eq!(wrong(&f, "alice").await, SignInError::Invalid, "the fifth is still answered as a wrong password");
    assert_eq!((f.count("signin.fail"), f.count("signin.locked")), (4, 1), "and writes the lock line, not a fail line");

    let derivations = f.gate.derivations();
    let lines = f.lines().len();
    let at_start = f.signin.begin_password("alice", PASSWORD, ADDR).await.expect_err("locked");
    let SignInError::Throttled { retry_seconds: start } = at_start else { panic!("{at_start:?}") };
    assert!((1..=900).contains(&start));
    assert_eq!(f.gate.derivations(), derivations, "a locked name derives nothing");
    assert_eq!(f.lines().len(), lines, "a refusal during a lock writes no line");

    f.clock.advance(450);
    let half = f.signin.begin_password("alice", PASSWORD, ADDR).await.expect_err("still locked");
    let SignInError::Throttled { retry_seconds: middle } = half else { panic!("{half:?}") };
    println!("Lock: retry_seconds at the start = {start}, half-way = {middle}");
    assert_eq!((start, middle), (900, 450));

    f.clock.advance(449);
    let last = f.signin.begin_password("alice", PASSWORD, ADDR).await.expect_err("the last second");
    assert_eq!(last, SignInError::Throttled { retry_seconds: 1 });

    f.clock.advance(1);
    let token = f.signin.begin_password("alice", PASSWORD, ADDR).await.expect("the lock has ended");
    f.signin.finish_totp(token.expose(), &f.code("alice"), ADDR, DEVICE).expect("signed in");
    assert_eq!(f.failures(&account_key("alice")), 0, "the completed sign-in cleared the name");
}

/// What a client can see of one attempt, and what the audit file shows of its shape (without the target).
type Seen = (String, u64, Vec<String>);

async fn sequence(f: &Fixture, name: &str) -> Vec<Seen> {
    let mut seen = Vec::new();
    for password in [WRONG, WRONG, WRONG, WRONG, WRONG, PASSWORD, WRONG] {
        let lines = f.lines().len();
        let derivations = f.gate.derivations();
        let outcome = f.signin.begin_password(name, password, ADDR).await;
        let shown = match outcome {
            Ok(_) => "pending".to_owned(),
            Err(e) => format!("{e:?} / {e}"),
        };
        let shapes = f.lines()[lines..]
            .iter()
            .map(|l| format!("{} {} {} {}", l["action"], l["result"], l["actor"], l["detail"]))
            .collect();
        seen.push((shown, f.gate.derivations() - derivations, shapes));
        f.clock.advance(1);
    }
    seen
}

#[tokio::test]
async fn throttle_an_unknown_name_is_throttled_exactly_like_a_known_one() {
    let known = fixture("th-parity-a");
    known.person("alice").await;
    let unknown = fixture("th-parity-b");
    unknown.person("carol").await;
    let a = sequence(&known, "alice").await;
    let b = sequence(&unknown, "nobody").await;
    for (x, y) in a.iter().zip(&b) {
        println!("Parity: known {x:?} | unknown {y:?}");
    }
    assert_eq!(a, b);
    assert!(a[5].0.starts_with("Throttled"), "the right password during the lock is refused: {:?}", a[5]);
    assert_eq!(a[5].1, 0);
    assert!(a[5].2.is_empty());
    // The target is the only difference in the file, and only for the operator.
    let target = |f: &Fixture| f.lines().iter().map(|l| l["target"].as_str().unwrap_or("?").to_owned()).collect::<Vec<_>>();
    assert!(target(&known).iter().all(|t| t == "1"));
    assert!(target(&unknown).iter().all(String::is_empty));
}

#[test]
fn throttle_the_account_key_is_a_hash_of_the_folded_name() {
    let key = account_key("Alice");
    assert_eq!(key, account_key("alice"));
    assert_ne!(key, account_key("alice "), "no trimming");
    assert_ne!(key, account_key(" alice"));
    assert!(key.starts_with("a:") && key.len() == 2 + 64, "{key}");
    assert!(!key.contains("alice"));
    let long = "x".repeat(129);
    assert_eq!(account_key(&long), account_key(&"y".repeat(5_000)), "names that cannot exist share one key");
    assert_ne!(account_key(&long), account_key(&"x".repeat(128)));
}

#[tokio::test]
async fn throttle_upper_case_shares_a_counter_and_a_space_does_not() {
    let f = fixture("th-fold");
    f.person("alice").await;
    for name in ["Alice", "ALICE", "alice"] {
        assert_eq!(wrong(&f, name).await, SignInError::Invalid);
    }
    assert_eq!(f.failures(&account_key("alice")), 3);
    assert_eq!(wrong(&f, "alice ").await, SignInError::Invalid);
    assert_eq!(f.failures(&account_key("alice ")), 1);
    assert_eq!(f.failures(&account_key("alice")), 3);
    for name in ["Alice", "alice"] {
        assert_eq!(wrong(&f, name).await, SignInError::Invalid);
    }
    assert_eq!(f.count("signin.locked"), 1);
    let locked = f.signin.begin_password("ALICE", PASSWORD, ADDR).await.expect_err("locked");
    assert!(matches!(locked, SignInError::Throttled { .. }));
}

#[tokio::test]
async fn throttle_twenty_wrong_passwords_lock_the_address_for_every_name() {
    let f = fixture("th-address");
    f.person("alice").await;
    for n in 0..20 {
        assert_eq!(wrong(&f, &format!("user{n}")).await, SignInError::Invalid);
    }
    assert_eq!((f.count("signin.fail"), f.count("signin.locked")), (19, 1));
    let lines = f.lines().len();
    let derivations = f.gate.derivations();
    for n in 20..50 {
        let outcome = f.signin.begin_password(&format!("user{n}"), WRONG, ADDR).await;
        assert!(matches!(outcome, Err(SignInError::Throttled { .. })), "{outcome:?}");
    }
    let alice = f.signin.begin_password("alice", PASSWORD, ADDR).await;
    assert!(matches!(alice, Err(SignInError::Throttled { .. })), "even a name that was never tried: {alice:?}");
    assert_eq!((f.lines().len(), f.gate.derivations()), (lines, derivations), "no line and no derivation");
    let other: IpAddr = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9));
    f.signin.begin_password("alice", PASSWORD, other).await.expect("another address is unaffected");
}

#[tokio::test]
async fn throttle_two_ipv6_addresses_of_one_64_share_a_counter() {
    let f = fixture_with("th-v6", limits(100, 2));
    f.person("alice").await;
    let a: IpAddr = "2001:db8:0:1::1".parse().expect("address");
    let b: IpAddr = "2001:db8:0:1:ffff:ffff:ffff:ffff".parse().expect("address");
    let c: IpAddr = "2001:db8:0:1::3".parse().expect("address");
    let elsewhere: IpAddr = "2001:db8:0:2::1".parse().expect("address");
    assert_eq!(address_key(a), address_key(b));
    assert_ne!(address_key(a), address_key(elsewhere));
    let mapped: IpAddr = "::ffff:192.0.2.7".parse().expect("address");
    assert_eq!(address_key(mapped), address_key(ADDR), "an IPv4-mapped address counts as the IPv4 address");
    f.signin.begin_password("one", WRONG, a).await.expect_err("wrong");
    f.signin.begin_password("two", WRONG, b).await.expect_err("wrong");
    let third = f.signin.begin_password("alice", PASSWORD, c).await;
    assert!(matches!(third, Err(SignInError::Throttled { .. })), "{third:?}");
    f.signin.begin_password("alice", PASSWORD, elsewhere).await.expect("another /64 is unaffected");
}

#[tokio::test]
async fn throttle_a_wrong_code_counts_on_both_keys_and_three_remove_the_state() {
    let f = fixture("th-codes");
    f.person("alice").await;
    let token = f.signin.begin_password("alice", PASSWORD, ADDR).await.expect("first step");
    assert_eq!((f.failures(&account_key("alice")), f.failures(&address_key(ADDR))), (0, 0), "a right password counts nothing");
    for n in 1..=3 {
        let outcome = f.signin.finish_totp(token.expose(), &f.wrong_code("alice"), ADDR, DEVICE);
        assert_eq!(outcome.expect_err("wrong code"), SignInError::Invalid);
        assert_eq!((f.failures(&account_key("alice")), f.failures(&address_key(ADDR))), (n, n));
    }
    assert_eq!(f.signin.pending_count(), 0, "the third wrong code removed the state");
    let late = f.signin.finish_totp(token.expose(), &f.code("alice"), ADDR, DEVICE);
    assert_eq!(late.expect_err("gone"), SignInError::Invalid, "the right code no longer helps");
    assert_eq!(f.session_rows(AccountId::from_raw(1)), 0);
}

#[tokio::test]
async fn throttle_a_completed_sign_in_clears_the_name_and_leaves_the_address() {
    let f = fixture("th-clear");
    f.person("alice").await;
    wrong(&f, "alice").await;
    wrong(&f, "alice").await;
    let token = f.signin.begin_password("alice", PASSWORD, ADDR).await.expect("first step");
    assert_eq!(f.failures(&account_key("alice")), 2, "the first step alone clears nothing");
    f.signin.finish_totp(token.expose(), &f.code("alice"), ADDR, DEVICE).expect("signed in");
    assert_eq!(f.failures(&account_key("alice")), 0);
    assert_eq!(f.failures(&address_key(ADDR)), 2, "the address counter is never cleared by a success");
}

#[tokio::test]
async fn throttle_a_right_password_with_a_wrong_code_does_not_clear_the_name() {
    let f = fixture("th-no-clear");
    f.person("alice").await;
    wrong(&f, "alice").await;
    let token = f.signin.begin_password("alice", PASSWORD, ADDR).await.expect("first step");
    f.signin.finish_totp(token.expose(), &f.wrong_code("alice"), ADDR, DEVICE).expect_err("wrong code");
    assert_eq!(f.failures(&account_key("alice")), 2);
}

#[tokio::test]
async fn throttle_a_lock_that_arrives_between_the_steps_ends_the_pending_state() {
    let f = fixture("th-between");
    f.person("alice").await;
    let token = f.signin.begin_password("alice", PASSWORD, ADDR).await.expect("first step");
    let elsewhere: IpAddr = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9));
    for _ in 0..5 {
        f.signin.begin_password("alice", WRONG, elsewhere).await.expect_err("wrong");
    }
    let lines = f.lines().len();
    let outcome = f.signin.finish_totp(token.expose(), &f.code("alice"), ADDR, DEVICE);
    assert!(matches!(outcome, Err(SignInError::Throttled { .. })), "{outcome:?}");
    assert_eq!(f.signin.pending_count(), 0);
    assert_eq!(f.lines().len(), lines, "no line for a refusal during a lock");
}

#[tokio::test]
async fn throttle_an_administrator_lifts_the_lock_of_a_name() {
    let f = fixture("th-unlock");
    let alice = f.person("alice").await;
    let root = f.admin("root");
    for _ in 0..5 {
        wrong(&f, "alice").await;
    }
    let now = f.clock.unix();
    assert!(f.signin.is_locked("alice", now).expect("state"));
    assert_eq!(f.signin.locked_until("alice", now).expect("state"), Some(now + 900));
    assert!(f.signin.begin_password("alice", PASSWORD, ADDR).await.is_err());

    f.signin.unlock_signin(root, Some(ADDR), alice).expect("unlock");
    let unlock: Vec<Value> = f.lines().into_iter().filter(|l| l["action"] == "account.unlock").collect();
    assert_eq!(unlock.len(), 1);
    println!("Unlock: {}", show(&unlock[0]));
    assert_eq!((unlock[0]["actor"].as_str(), unlock[0]["target"].as_str()), (Some("2"), Some("1")));
    assert_eq!(unlock[0]["result"], "ok");
    assert!(!f.signin.is_locked("alice", now).expect("state"));
    assert_eq!(f.signin.locked_until("alice", now).expect("state"), None);
    f.signin.begin_password("alice", PASSWORD, ADDR).await.expect("not throttled any more");

    // An unlock of a name that is not locked writes its line and changes nothing.
    f.signin.unlock_signin(root, Some(ADDR), alice).expect("unlock again");
    assert_eq!(f.count("account.unlock"), 2);
    assert_eq!(f.failures(&address_key(ADDR)), 5);

    // Only an active administrator can; the refusal leaves its own line.
    let bob = f.bare_account("bob");
    assert_eq!(f.signin.unlock_signin(bob, Some(ADDR), alice).expect_err("not an administrator"), SignInError::Invalid);
    let nobody = AccountId::from_raw(9_999);
    assert_eq!(f.signin.unlock_signin(root, Some(ADDR), nobody).expect_err("no such account"), SignInError::Invalid);
    let refused: Vec<Value> = f.lines().into_iter().filter(|l| l["action"] == "account.unlock" && l["result"] == "fail").collect();
    assert_eq!(refused.len(), 2);
}

#[tokio::test]
async fn throttle_the_lock_answer_is_the_same_for_a_name_that_does_not_exist() {
    let f = fixture("th-state");
    f.person("alice").await;
    let now = f.clock.unix();
    assert!(!f.signin.is_locked("nobody", now).expect("state"));
    for _ in 0..5 {
        wrong(&f, "nobody").await;
    }
    assert!(f.signin.is_locked("nobody", now).expect("state"), "an unknown name is locked like a known one");
    assert_eq!(f.signin.locked_until("nobody", now).expect("state"), Some(now + 900));
    f.clock.advance(900);
    assert!(!f.signin.is_locked("nobody", f.clock.unix()).expect("state"));
}

#[tokio::test]
async fn throttle_a_lock_does_not_touch_the_methods_that_cannot_be_guessed() {
    let f = fixture("th-passkey");
    let alice = f.person("alice").await;
    for _ in 0..5 {
        wrong(&f, "alice").await;
    }
    assert!(f.signin.is_locked("alice", f.clock.unix()).expect("state"));
    // A session by passkey or single sign-on never passes through the throttle.
    for method in [Method::Passkey, Method::Oidc] {
        f.sessions.create(alice, method, f.clock.unix(), DEVICE, Some(ADDR)).expect("a session of a method that cannot be guessed");
    }
    assert_eq!(f.session_rows(alice), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn throttle_sixteen_racing_failures_are_counted_exactly() {
    let f = fixture_with("th-race", limits(100, 100));
    f.person("alice").await;
    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let signin = f.signin.clone();
            tokio::spawn(async move { signin.begin_password("alice", WRONG, ADDR).await.expect_err("wrong") })
        })
        .collect();
    for task in tasks {
        assert_eq!(task.await.expect("task"), SignInError::Invalid);
    }
    let (account, address) = (f.failures(&account_key("alice")), f.failures(&address_key(ADDR)));
    println!("Race: count = {account} (address {address}), signin.locked lines = {}", f.count("signin.locked"));
    assert_eq!((account, address), (16, 16));
    assert_eq!((f.count("signin.fail"), f.count("signin.locked")), (16, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn throttle_of_racing_failures_exactly_one_starts_the_lock() {
    let f = fixture_with("th-race-lock", limits(5, 100));
    f.person("alice").await;
    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let signin = f.signin.clone();
            tokio::spawn(async move { signin.begin_password("alice", WRONG, ADDR).await.expect_err("wrong") })
        })
        .collect();
    let mut counted = 0;
    for task in tasks {
        match task.await.expect("task") {
            SignInError::Invalid => counted += 1,
            SignInError::Throttled { .. } => {}
            other => panic!("{other:?}"),
        }
    }
    println!("Race: limit 5, counted failures = {counted}, signin.locked lines = {}", f.count("signin.locked"));
    assert!(counted >= 5);
    assert_eq!(f.count("signin.locked"), 1, "exactly one failure reached the limit");
    assert_eq!(f.count("signin.fail") + f.count("signin.locked"), counted, "one line for each failure that was counted");
    assert_eq!(f.failures(&account_key("alice")), i64::try_from(counted).expect("small"));
    assert!(f.signin.is_locked("alice", f.clock.unix()).expect("state"));
}

#[tokio::test]
async fn throttle_purge_removes_ended_windows_and_keeps_a_lock() {
    let f = fixture_with("th-purge", Limits { lock_duration: std::time::Duration::from_secs(3_600), ..Limits::DEFAULT });
    f.person("alice").await;
    for _ in 0..5 {
        wrong(&f, "alice").await;
    }
    for name in ["x1", "x2", "x3", "x4", "x5"] {
        wrong(&f, name).await;
    }
    f.clock.advance(1_000);
    let now = f.clock.unix();
    assert!(f.signin.is_locked("alice", now).expect("state"), "the lock outlives the window");
    let first = f.signin.purge(now, 2).expect("purge");
    assert_eq!(first.throttle, 2, "at most the number asked for");
    let mut total = first.throttle;
    loop {
        let more = f.signin.purge(now, 2).expect("purge").throttle;
        total += more;
        if more == 0 {
            break;
        }
    }
    assert_eq!(total, 6, "five names and the address; the locked name stays");
    assert!(f.signin.is_locked("alice", now).expect("state"), "a purge during a lock keeps the lock");
    assert_eq!(f.failures(&account_key("alice")), 5);
}

#[tokio::test]
async fn throttle_purge_removes_old_pending_states_a_few_at_a_time() {
    let f = fixture("th-purge-pending");
    f.person("alice").await;
    let mut tokens = Vec::new();
    for _ in 0..3 {
        tokens.push(f.signin.begin_password("alice", PASSWORD, ADDR).await.expect("first step"));
    }
    assert_eq!(f.signin.pending_count(), 3);
    assert_eq!(f.signin.purge(f.clock.unix() + 300, 10).expect("purge").pending, 0, "still valid at the last second");
    let late = f.clock.unix() + 301;
    assert_eq!(f.signin.purge(late, 2).expect("purge").pending, 2);
    assert_eq!(f.signin.purge(late, 2).expect("purge").pending, 1);
    assert_eq!(f.signin.pending_count(), 0);
    assert_eq!(f.signin.wiped_count(), 3, "every state let go of its material");
}
