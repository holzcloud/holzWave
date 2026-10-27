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
use holzwave_server::auth::totp;
use holzwave_server::clock::{Clock, ManualClock};
use holzwave_server::config::Sessions;
use holzwave_server::keyfile::ServerKey;
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
    signin: SignIn,
    secrets: Mutex<HashMap<String, Zeroizing<[u8; totp::SECRET_BYTES]>>>,
    sealer: Arc<Sealer>,
}

fn fixture(tag: &str) -> Fixture {
    fixture_with(tag, holzwave_server::limits::Limits::DEFAULT)
}

fn fixture_with(tag: &str, limits: holzwave_server::limits::Limits) -> Fixture {
    let scratch = Scratch::new(tag);
    let clock = Arc::new(ManualClock::new(NOW));
    let store = Arc::new(Store::open(scratch.path()).expect("open the store"));
    let audit = Arc::new(Audit::open(scratch.path(), clock.clone()).expect("open the audit log"));
    let sessions = Arc::new(SessionManager::new(store.clone(), clock.clone(), Sessions::default()));
    let gate = ArgonGate::new(2, Kdf::lower_bound());
    let sealer = Arc::new(Sealer::new(&ServerKey::from_bytes([7u8; 32]).expect("a key that is not all zero")));
    let signin = SignIn::new(store.clone(), clock.clone(), audit, sessions.clone(), gate.clone(), sealer.clone())
        .with_limits(limits);
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
