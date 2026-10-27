//! The passkey module with a software authenticator and no browser: registration, username-less sign-in, step-up, management,
//! the bounds of the challenge store, hostile input, damage and concurrency.
//!
//! The authenticator is the one written for the library gate (`support/webauthn_client.rs`). The credentials travel as JSON
//! text in both directions, in the form the page posts.

#[path = "support/scratch.rs"]
mod scratch;
#[path = "support/webauthn_client.rs"]
mod webauthn_client;

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use holzwave_server::audit::Audit;
use holzwave_server::auth::passkey::{
    ChallengeToken, PasskeyError, Passkeys, RegistrationSubject, add_prf_request,
};
use holzwave_server::auth::session::{Method, SessionManager};
use holzwave_server::clock::ManualClock;
use holzwave_server::config::Sessions;
use holzwave_server::store::{AccountId, NewAccount, Role, Store};
use scratch::Scratch;
use serde_json::{Value, json};
use webauthn_client::{SoftPasskey, Tweaks};
use webauthn_rs::prelude::Url;

/// 2026-03-01T08:30:00Z.
const NOW: i64 = 1_772_353_800;
const RANGE: std::ops::RangeInclusive<u32> = 20000..=29999;
const ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));
const PUBLIC_URL: &str = "https://hw.example.test";
const DEVICE: &str = "Firefox on Linux";

struct Fixture {
    scratch: Scratch,
    store: Arc<Store>,
    sessions: Arc<SessionManager>,
    passkeys: Arc<Passkeys>,
    origin: Url,
}

fn fixture(tag: &str) -> Fixture {
    let scratch = Scratch::new(tag);
    let clock = Arc::new(ManualClock::new(NOW));
    let store = Arc::new(Store::open(scratch.path()).expect("open the store"));
    let audit = Arc::new(Audit::open(scratch.path(), clock.clone()).expect("open the audit log"));
    let sessions = Arc::new(SessionManager::new(store.clone(), clock.clone(), Sessions::default()));
    let passkeys = Arc::new(
        Passkeys::new(PUBLIC_URL, "holzWave", store.clone(), sessions.clone(), audit, clock.clone())
            .expect("the relying party builds"),
    );
    let origin = Url::parse(PUBLIC_URL).expect("origin parses");
    Fixture { scratch, store, sessions, passkeys, origin }
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

impl Fixture {
    fn account(&self, name: &str) -> (AccountId, [u8; 16]) {
        let mut handle = [0u8; 16];
        getrandom::fill(&mut handle).expect("random");
        let account = self
            .store
            .write(|tx| tx.account_create(&NewAccount::new(name, Role::Member).handle(handle), RANGE, NOW))
            .expect("account");
        (account.id, handle)
    }

    fn subject(&self, name: &str, handle: [u8; 16]) -> RegistrationSubject {
        RegistrationSubject { handle, user_name: name.to_owned(), display_name: name.to_owned() }
    }

    fn lines(&self) -> Vec<Value> {
        audit_lines(self.scratch.path())
    }

    fn count(&self, action: &str) -> usize {
        self.lines().iter().filter(|l| l["action"] == action).count()
    }

    /// A registration through the module with a fresh software authenticator; answers the authenticator and the row id.
    fn register(&self, name: &str, handle: [u8; 16], label: &str) -> (SoftPasskey, i64) {
        let mut authenticator = SoftPasskey::new();
        let (options, token) = self.passkeys.start_registration(&self.subject(name, handle), ADDR).expect("starts");
        let options: Value = serde_json::from_str(&options).expect("options are JSON");
        let response = authenticator.register(&options, &self.origin).expect("the authenticator answers");
        let registered = self
            .passkeys
            .finish_registration(token.expose(), &response.to_string(), None, label, ADDR)
            .expect("the registration verifies");
        assert_eq!(registered.credential_id, authenticator.credential_id());
        (authenticator, registered.id)
    }

    /// A sign-in options object and its token.
    fn start_sign_in(&self) -> (Value, ChallengeToken) {
        let (options, token) = self.passkeys.start_sign_in(ADDR).expect("sign-in starts");
        (serde_json::from_str(&options).expect("options are JSON"), token)
    }
}

// ---- the tracer ----

#[test]
fn tracer_registers_a_passkey_and_signs_in_with_it() {
    let f = fixture("tracer-happy");
    let (alice, handle) = f.account("alice");
    let mut authenticator = SoftPasskey::new();

    let (options_text, token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
    assert_eq!(token.expose().len(), 43);
    assert!(options_text.contains("\"attestation\":\"none\""), "attestation is none: {options_text}");
    let options: Value = serde_json::from_str(&options_text).expect("options are JSON");
    let public_key = &options["publicKey"];
    assert_eq!(public_key["user"]["id"], URL_SAFE_NO_PAD.encode(handle));
    assert_eq!(public_key["rp"]["id"], "hw.example.test");
    assert!(public_key["extensions"]["prf"]["eval"]["first"].is_string(), "the PRF request is in extensions");
    assert_eq!(public_key["authenticatorSelection"]["userVerification"], "required");
    println!("registration options keys: {:?}", public_key.as_object().expect("object").keys().collect::<Vec<_>>());

    let response = authenticator.register(&options, &f.origin).expect("the authenticator answers");
    let registered = f
        .passkeys
        .finish_registration(token.expose(), &response.to_string(), None, "", ADDR)
        .expect("the registration verifies");
    assert_eq!(registered.credential_id, authenticator.credential_id());
    assert_eq!(registered.prf_salt.len(), 32);
    assert!(registered.prf.is_none());

    let rows = f.store.read(|tx| tx.passkey_list(alice)).expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].credential_id, authenticator.credential_id());
    assert_eq!(rows[0].prf_salt.as_deref().map(<[u8]>::len), Some(32));
    let listed = f.passkeys.list(alice).expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "Passkey");
    assert_eq!(f.count("auth.passkey.add"), 1);
    assert!(f.store.read(|tx| tx.session_list(alice)).expect("sessions").is_empty(), "a registration makes no session");

    let (options, token) = f.start_sign_in();
    assert_eq!(token.expose().len(), 43);
    assert_eq!(options["publicKey"]["allowCredentials"], json!([]));
    assert_eq!(options["publicKey"]["userVerification"], "required");
    println!("sign-in options keys: {:?}", options["publicKey"].as_object().expect("object").keys().collect::<Vec<_>>());
    let response = authenticator.authenticate(&options, &f.origin).expect("the authenticator answers");
    let signed_in = f
        .passkeys
        .finish_sign_in(token.expose(), &response.to_string(), None, ADDR, DEVICE)
        .expect("the sign-in verifies");
    assert_eq!(signed_in.account, alice);
    assert_eq!(signed_in.session.value.expose().len(), 43);
    let session = f.sessions.lookup(signed_in.session.value.expose(), NOW).expect("the session is alive");
    assert_eq!(session.account_id, alice);
    assert_eq!(session.method, Method::Passkey);
    let lines = f.lines();
    let ok: Vec<_> = lines.iter().filter(|l| l["action"] == "signin.ok").collect();
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0]["detail"]["method"], "passkey");
    for line in &lines {
        println!("{} {} {} {}", line["action"], line["result"], line["actor"], line["detail"]);
    }
}

#[test]
fn tracer_a_second_use_of_the_sign_in_token_is_refused() {
    let f = fixture("tracer-replay");
    let (alice, handle) = f.account("alice");
    let (mut authenticator, _) = f.register("alice", handle, "Laptop");

    let (options, token) = f.start_sign_in();
    let response = authenticator.authenticate(&options, &f.origin).expect("answers").to_string();
    f.passkeys.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE).expect("the first use verifies");
    let again = f.passkeys.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE);
    assert!(matches!(again, Err(PasskeyError::Invalid)), "got {again:?}");
    assert_eq!(f.store.read(|tx| tx.session_list(alice)).expect("sessions").len(), 1, "no second session");
    assert_eq!(f.count("signin.ok"), 1);
    assert_eq!(f.count("signin.fail"), 1);
}

#[test]
fn tracer_a_response_signed_for_another_origin_is_refused() {
    let f = fixture("tracer-origin");
    let (alice, handle) = f.account("alice");
    let (mut authenticator, _) = f.register("alice", handle, "Laptop");

    let (options, token) = f.start_sign_in();
    let other = Url::parse("https://other.example.test").expect("url parses");
    let response = authenticator
        .authenticate_with(&options, &other, &Tweaks::unchecked())
        .expect("the unchecked path signs")
        .to_string();
    let result = f.passkeys.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE);
    assert!(matches!(result, Err(PasskeyError::Invalid)), "got {result:?}");
    assert!(f.store.read(|tx| tx.session_list(alice)).expect("sessions").is_empty());
    assert_eq!(f.count("signin.ok"), 0);
    assert_eq!(f.count("signin.fail"), 1);
}

#[test]
fn registration_through_the_two_halves_stores_only_when_the_transaction_commits() {
    let f = fixture("halves");
    let (alice, handle) = f.account("alice");
    for commit in [false, true] {
        let mut authenticator = SoftPasskey::new();
        let (options, token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
        let options: Value = serde_json::from_str(&options).expect("JSON");
        let response = authenticator.register(&options, &f.origin).expect("answers").to_string();
        let verified = f.passkeys.verify_registration(token.expose(), &response, None).expect("verifies");
        assert_eq!(verified.credential_id(), authenticator.credential_id());
        assert_eq!(f.store.read(|tx| tx.passkey_count(alice)).expect("count"), 0, "nothing stored before store_verified");
        let outcome: Result<_, _> = f.store.write(|tx| {
            let row = f.passkeys.store_verified(tx, alice, &verified, "Phone").expect("stores");
            assert_eq!(row.prf_salt.len(), 32);
            if commit {
                Ok(row)
            } else {
                Err(holzwave_server::store::StoreError::Invalid("roll back"))
            }
        });
        assert_eq!(outcome.is_ok(), commit);
        assert_eq!(f.store.read(|tx| tx.passkey_count(alice)).expect("count"), usize::from(commit));
    }
    assert_eq!(f.count("auth.passkey.add"), 0, "the halves write no line");
}

#[test]
fn the_prf_request_helper_edits_the_options_and_keeps_what_was_there() {
    let input = [7u8; 32];
    let mut options = json!({ "publicKey": { "challenge": "abc", "extensions": { "uvm": true } } });
    assert!(add_prf_request(&mut options, &input));
    assert_eq!(options["publicKey"]["extensions"]["uvm"], true);
    assert_eq!(options["publicKey"]["extensions"]["prf"]["eval"]["first"], URL_SAFE_NO_PAD.encode(input));
    assert_eq!(options["publicKey"]["challenge"], "abc");

    let mut bare = json!({ "publicKey": { "challenge": "abc" } });
    assert!(add_prf_request(&mut bare, &input));
    assert_eq!(bare["publicKey"]["extensions"]["prf"]["eval"]["first"], URL_SAFE_NO_PAD.encode(input));

    assert!(!add_prf_request(&mut json!({ "other": 1 }), &input), "not options");
    assert!(!add_prf_request(&mut json!({ "publicKey": { "extensions": 3 } }), &input), "extensions is not an object");
}
