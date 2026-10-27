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
use std::sync::{Arc, Barrier};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use holzwave_server::audit::Audit;
use holzwave_server::auth::passkey::{
    ChallengeToken, PasskeyError, Passkeys, RegistrationSubject, SignedIn, add_prf_request, clean_name, prf_input, read_prf,
};
use holzwave_server::auth::session::{Method, SessionManager};
use holzwave_server::clock::{Clock, ManualClock};
use holzwave_server::config::Sessions;
use holzwave_server::store::{AccountId, AccountState, NewAccount, PasswordCredential, Role, Session, Store};
use scratch::Scratch;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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
    clock: Arc<ManualClock>,
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
    Fixture { scratch, store, clock, sessions, passkeys, origin }
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

    /// A session of the account, made the way a password and a code would make it.
    fn session(&self, account: AccountId) -> Session {
        self.sessions.create(account, Method::PasswordTotp, self.clock.unix(), DEVICE, Some(ADDR)).expect("session").session
    }

    /// A full sign-in with the authenticator; `prf` is what the page sends beside the credential.
    fn sign_in(&self, authenticator: &mut SoftPasskey, prf: Option<&str>) -> Result<SignedIn, PasskeyError> {
        let (options, token) = self.start_sign_in();
        let response = authenticator.authenticate(&options, &self.origin).expect("answers").to_string();
        self.passkeys.finish_sign_in(token.expose(), &response, prf, ADDR, DEVICE)
    }

    /// Gives the account a password and an authenticator row (the bytes are placeholders; the module only asks whether they
    /// exist).
    fn add_password_with_authenticator(&self, account: AccountId) {
        self.store
            .write(|tx| {
                tx.password_set(
                    account,
                    &PasswordCredential { verifier: vec![1; 32], salt: vec![2; 16], params: "test".into(), changed_at: NOW },
                )?;
                tx.totp_set(account, &[3; 40])
            })
            .expect("password and authenticator");
    }

    /// The audit lines of one action without the time.
    fn shapes(&self, action: &str) -> Vec<Value> {
        self.lines()
            .into_iter()
            .filter(|l| l["action"] == action)
            .map(|mut l| {
                l.as_object_mut().expect("object").remove("ts");
                l
            })
            .collect()
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

// ---- policy ----

#[test]
fn the_constructor_refuses_an_address_that_cannot_be_a_relying_party() {
    let scratch = Scratch::new("ctor");
    let clock = Arc::new(ManualClock::new(NOW));
    let store = Arc::new(Store::open(scratch.path()).expect("store"));
    let audit = Arc::new(Audit::open(scratch.path(), clock.clone()).expect("audit"));
    let sessions = Arc::new(SessionManager::new(store.clone(), clock.clone(), Sessions::default()));
    let build = |url: &str| Passkeys::new(url, "holzWave", store.clone(), sessions.clone(), audit.clone(), clock.clone());
    for good in ["https://hw.example.test", "https://hw.example.test/", "https://hw.example.test:8443", "https://A.Example.Test"] {
        let ok = build(good).unwrap_or_else(|e| panic!("{good}: {e}"));
        assert_eq!(ok.rp_id(), good.trim_start_matches("https://").split([':', '/']).next().expect("host").to_lowercase());
    }
    assert_eq!(build("https://hw.example.test:8443").expect("builds").origin(), "https://hw.example.test:8443");
    for bad in [
        "",
        "http://hw.example.test",
        "https://192.0.2.1",
        "https://[2001:db8::1]",
        "https://localhost",
        "https://hw.example.test/path",
        "https://hw.example.test/?q=1",
        "https://hw.example.test/#frag",
        "https://user@hw.example.test",
        "hw.example.test",
    ] {
        let error = build(bad).err().unwrap_or_else(|| panic!("{bad:?} must be refused"));
        assert!(!error.sentence().is_empty());
        println!("refused {bad:?}: {error}");
    }
}

#[test]
fn registration_options_exclude_the_existing_credentials() {
    let f = fixture("exclude");
    let (_alice, handle) = f.account("alice");
    let (first, _) = f.register("alice", handle, "Laptop");
    let (options, _token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
    let options: Value = serde_json::from_str(&options).expect("JSON");
    let excluded = options["publicKey"]["excludeCredentials"].as_array().expect("an exclude list");
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0]["id"], URL_SAFE_NO_PAD.encode(first.credential_id()));
    assert_eq!(options["publicKey"]["authenticatorSelection"]["residentKey"], "preferred");
    assert_eq!(options["publicKey"]["authenticatorSelection"]["userVerification"], "required");
    assert_eq!(options["publicKey"]["attestation"], "none");

    // A person without credentials has no exclude list.
    let (_bob, bob_handle) = f.account("bob");
    let (options, _token) = f.passkeys.start_registration(&f.subject("bob", bob_handle), ADDR).expect("starts");
    let options: Value = serde_json::from_str(&options).expect("JSON");
    assert!(options["publicKey"].get("excludeCredentials").is_none_or(|v| v.as_array().is_some_and(Vec::is_empty)));
}

#[test]
fn the_same_credential_cannot_be_stored_twice() {
    let f = fixture("twice");
    let (alice, handle) = f.account("alice");
    let mut authenticator = SoftPasskey::new();
    let (options, token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
    let options: Value = serde_json::from_str(&options).expect("JSON");
    let response = authenticator.register(&options, &f.origin).expect("answers").to_string();
    let verified = f.passkeys.verify_registration(token.expose(), &response, None).expect("verifies");
    f.store.write(|tx| Ok(f.passkeys.store_verified(tx, alice, &verified, "One"))).expect("tx").expect("first store");
    let second = f.store.write(|tx| Ok(f.passkeys.store_verified(tx, alice, &verified, "Two"))).expect("tx");
    assert_eq!(second.err(), Some(PasskeyError::Conflict));
    assert_eq!(f.store.read(|tx| tx.passkey_count(alice)).expect("count"), 1);
    let names: Vec<_> = f.passkeys.list(alice).expect("list").into_iter().map(|p| p.name).collect();
    assert_eq!(names, ["One"]);
}

#[test]
fn the_eleventh_credential_is_too_many() {
    let f = fixture("eleven");
    let (alice, handle) = f.account("alice");
    // A registration started while there was room, finished after the room was used up.
    let mut late = SoftPasskey::new();
    let (late_options, late_token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
    let late_options: Value = serde_json::from_str(&late_options).expect("JSON");
    let late_response = late.register(&late_options, &f.origin).expect("answers").to_string();
    for n in 0..10 {
        f.register("alice", handle, &format!("Key {n}"));
    }
    assert_eq!(f.store.read(|tx| tx.passkey_count(alice)).expect("count"), 10);
    let finished = f.passkeys.finish_registration(late_token.expose(), &late_response, None, "Late", ADDR);
    assert_eq!(finished.err(), Some(PasskeyError::TooMany));
    assert_eq!(f.store.read(|tx| tx.passkey_count(alice)).expect("count"), 10, "nothing was stored");
    let refused = f.passkeys.start_registration(&f.subject("alice", handle), ADDR);
    assert_eq!(refused.err(), Some(PasskeyError::TooMany), "a start is refused too");
    let fails = f.shapes("auth.passkey.add").into_iter().filter(|l| l["result"] == "fail").count();
    assert_eq!(fails, 1);
}

#[test]
fn the_name_policy() {
    let long = "x".repeat(64);
    let too_long = "x".repeat(65);
    let cases: Vec<(String, String)> = vec![
        ("Laptop".into(), "Laptop".into()),
        ("  Phone  ".into(), "Phone".into()),
        (long.clone(), long.clone()),
        (too_long, "Passkey".into()),
        (String::new(), "Passkey".into()),
        ("   ".into(), "Passkey".into()),
        ("bad\u{7}name".into(), "Passkey".into()),
        ("tab\there".into(), "Passkey".into()),
        ("new\nline".into(), "Passkey".into()),
        ("zero\u{200B}width".into(), "Passkey".into()),
        ("flip\u{202E}text".into(), "Passkey".into()),
        ("Büro-Rechner".into(), "Büro-Rechner".into()),
        ("y".repeat(100_000), "Passkey".into()),
        (format!(" {long} "), long),
    ];
    for (input, expected) in cases {
        assert_eq!(clean_name(&input), expected, "input of {} bytes", input.len());
    }
}

#[test]
fn the_name_is_stored_cleaned() {
    let f = fixture("names");
    let (alice, handle) = f.account("alice");
    f.register("alice", handle, "  Laptop ");
    f.register("alice", handle, "bad\u{1}");
    f.register("alice", handle, &"z".repeat(65));
    f.register("alice", handle, &"z".repeat(64));
    f.register("alice", handle, "");
    let names: Vec<_> = f.passkeys.list(alice).expect("list").into_iter().map(|p| p.name).collect();
    assert_eq!(names, ["Laptop", "Passkey", "Passkey", "z".repeat(64).as_str(), "Passkey"]);
}

// ---- the PRF value ----

#[test]
fn the_prf_input_is_the_documented_derivation_and_is_in_both_options() {
    let f = fixture("prf-input");
    let mut hash = Sha256::new();
    hash.update(b"holzwave passkey prf input v1");
    hash.update([0u8]);
    hash.update(b"hw.example.test");
    let expected: [u8; 32] = hash.finalize().into();
    assert_eq!(prf_input("hw.example.test"), expected);
    assert_eq!(f.passkeys.prf_input(), &expected);
    println!("PRF input, first 4 bytes: {:02x?}", &expected[..4]);
    assert_ne!(prf_input("other.example.test"), expected);

    let (_alice, handle) = f.account("alice");
    let (registration, _token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
    let registration: Value = serde_json::from_str(&registration).expect("JSON");
    let (sign_in, _token) = f.start_sign_in();
    let want = URL_SAFE_NO_PAD.encode(expected);
    assert_eq!(registration["publicKey"]["extensions"]["prf"]["eval"]["first"], want);
    assert_eq!(sign_in["publicKey"]["extensions"]["prf"]["eval"]["first"], want);
}

#[test]
fn the_prf_reader_accepts_exactly_32_bytes_of_unpadded_base64url() {
    let good = URL_SAFE_NO_PAD.encode([0xa5u8; 32]);
    assert_eq!(read_prf(Some(&good)).expect("32 bytes").expose(), &[0xa5u8; 32]);
    let table: Vec<(&str, Option<String>)> = vec![
        ("absent", None),
        ("empty", Some(String::new())),
        ("31 bytes", Some(URL_SAFE_NO_PAD.encode([1u8; 31]))),
        ("33 bytes", Some(URL_SAFE_NO_PAD.encode([1u8; 33]))),
        ("padded", Some(format!("{good}="))),
        ("a plus", Some(good.replacen(&good[10..11], "+", 1))),
        ("a slash", Some(good.replacen(&good[10..11], "/", 1))),
        ("not base64", Some("!".repeat(43))),
        ("standard alphabet", Some(base64::engine::general_purpose::STANDARD_NO_PAD.encode([0xfbu8; 32]))),
        ("leading space", Some(format!(" {}", &good[..42]))),
        ("trailing newline", Some(format!("{}\n", &good[..42]))),
        ("non-canonical last character", Some(format!("{}B", &good[..42]))),
        ("a multi-byte character", Some(format!("é{}", &good[..41]))),
        ("a huge text", Some("A".repeat(1_000_000))),
    ];
    for (case, value) in table {
        let read = read_prf(value.as_deref());
        // The one table row that is a valid value by luck of its last character is checked on its own below.
        if case == "non-canonical last character" {
            continue;
        }
        assert!(read.is_none(), "{case} must be ignored");
    }
    // A last character that sets unused bits is not canonical and is refused.
    let noncanonical = format!("{}B", "A".repeat(42));
    assert!(read_prf(Some(&noncanonical)).is_none());
}

#[test]
fn a_valid_prf_value_comes_back_from_the_registration_and_the_sign_in() {
    let f = fixture("prf-flow");
    let (alice, handle) = f.account("alice");
    let output = [0x5au8; 32];
    let prf = URL_SAFE_NO_PAD.encode(output);

    let mut authenticator = SoftPasskey::new();
    let (options, token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
    let options: Value = serde_json::from_str(&options).expect("JSON");
    let response = authenticator.register(&options, &f.origin).expect("answers").to_string();
    let registered = f.passkeys.finish_registration(token.expose(), &response, Some(&prf), "Key", ADDR).expect("verifies");
    assert_eq!(registered.prf.as_ref().expect("material").expose(), &output);
    let row = f.store.read(|tx| tx.passkey_list(alice)).expect("rows").remove(0);
    assert_eq!(row.prf_salt.as_deref(), Some(registered.prf_salt.as_slice()));
    assert_eq!(registered.prf_salt.len(), 32);

    let signed_in = f.sign_in(&mut authenticator, Some(&prf)).expect("signs in");
    let material = signed_in.prf.as_ref().expect("material");
    assert_eq!(material.output().expose(), &output);
    assert_eq!(material.credential_id(), authenticator.credential_id());
    assert_eq!(material.salt(), registered.prf_salt.as_slice());
}

#[test]
fn a_malformed_prf_value_does_not_fail_the_sign_in_and_gives_no_material() {
    let f = fixture("prf-bad");
    let (_alice, handle) = f.account("alice");
    let (mut authenticator, _) = f.register("alice", handle, "Key");
    let good = URL_SAFE_NO_PAD.encode([9u8; 32]);
    for bad in [
        URL_SAFE_NO_PAD.encode([9u8; 31]),
        URL_SAFE_NO_PAD.encode([9u8; 33]),
        format!("{good}="),
        good.replacen(&good[5..6], "+", 1),
        "not base64 at all".to_owned(),
        String::new(),
    ] {
        let signed_in = f.sign_in(&mut authenticator, Some(&bad)).expect("the sign-in still works");
        assert!(signed_in.prf.is_none(), "no material for {bad:?}");
    }
    assert!(f.sign_in(&mut authenticator, None).expect("works").prf.is_none());
    assert!(f.sign_in(&mut authenticator, Some(&good)).expect("works").prf.is_some());
}

#[test]
fn the_types_that_hold_key_material_print_none_of_it() {
    let f = fixture("debug");
    let (alice, handle) = f.account("alice");
    let output = [0x42u8; 32];
    let prf = URL_SAFE_NO_PAD.encode(output);
    let mut authenticator = SoftPasskey::new();
    let (options, token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
    let token_text = token.expose().to_owned();
    let options: Value = serde_json::from_str(&options).expect("JSON");
    let response = authenticator.register(&options, &f.origin).expect("answers").to_string();
    let verified = f.passkeys.verify_registration(token.expose(), &response, Some(&prf)).expect("verifies");
    let credential_b64 = URL_SAFE_NO_PAD.encode(authenticator.credential_id());
    let row = f.store.write(|tx| Ok(f.passkeys.store_verified(tx, alice, &verified, "Key"))).expect("tx").expect("stores");
    let signed_in = f.sign_in(&mut authenticator, Some(&prf)).expect("signs in");
    let session_value = signed_in.session.value.expose().to_owned();
    let material_text = format!("{:?}", signed_in.prf.as_ref().expect("material"));
    let texts = [
        format!("{token:?}"),
        format!("{verified:?}"),
        format!("{row:?}"),
        format!("{signed_in:?}"),
        material_text,
        format!("{:?}", signed_in.prf.as_ref().expect("material").output()),
        format!("{:?}", f.passkeys),
        format!("{:?}", PasskeyError::Invalid),
        format!("{}", PasskeyError::Invalid),
        format!("{:?}", f.passkeys.list(alice).expect("list")),
    ];
    for text in &texts {
        for secret in [&prf, &token_text, &credential_b64, &session_value, &"42, 42, 42".to_owned(), &"0x42".to_owned()] {
            assert!(!text.contains(secret.as_str()), "{text} holds {secret}");
        }
    }
}

// ---- step-up ----

#[test]
fn a_step_up_with_the_accounts_own_passkey_moves_the_last_sign_in_forward() {
    let f = fixture("stepup");
    let (alice, handle) = f.account("alice");
    let (mut authenticator, _) = f.register("alice", handle, "Laptop");
    let mut session = f.session(alice);
    let before = session.last_auth;

    let (options, token) = f.passkeys.start_step_up(&session).expect("starts");
    let options: Value = serde_json::from_str(&options).expect("JSON");
    let allowed: Vec<_> =
        options["publicKey"]["allowCredentials"].as_array().expect("a list").iter().map(|c| c["id"].clone()).collect();
    assert_eq!(allowed, [json!(URL_SAFE_NO_PAD.encode(authenticator.credential_id()))]);

    f.clock.advance(120);
    let response = authenticator.authenticate(&options, &f.origin).expect("answers").to_string();
    f.passkeys.finish_step_up(token.expose(), &response, &mut session, ADDR).expect("steps up");
    assert_eq!(session.last_auth, before + 120);
    let stored = f.store.read(|tx| tx.session_get(&session.id_hash)).expect("read").expect("the row");
    assert_eq!(stored.last_auth, before + 120, "the row moved as well");
    let lines = f.shapes("session.step_up");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["detail"], json!({ "method": "passkey" }));
    assert_eq!(lines[0]["result"], "ok");
}

#[test]
fn another_accounts_assertion_does_not_step_up_this_session() {
    let f = fixture("stepup-other");
    let (alice, alice_handle) = f.account("alice");
    let (bob, bob_handle) = f.account("bob");
    let (_alice_key, _) = f.register("alice", alice_handle, "Laptop");
    let (mut bob_key, _) = f.register("bob", bob_handle, "Phone");
    let mut alice_session = f.session(alice);
    let before = alice_session.last_auth;

    let (options, token) = f.passkeys.start_step_up(&alice_session).expect("starts");
    let options: Value = serde_json::from_str(&options).expect("JSON");
    // Bob's authenticator signs the challenge; the allow list is hers, so the client side is helped past it.
    let mut forced = options.clone();
    forced["publicKey"]["allowCredentials"] =
        json!([{ "type": "public-key", "id": URL_SAFE_NO_PAD.encode(bob_key.credential_id()) }]);
    f.clock.advance(60);
    let response = bob_key.authenticate(&forced, &f.origin).expect("bob's authenticator answers").to_string();
    let result = f.passkeys.finish_step_up(token.expose(), &response, &mut alice_session, ADDR);
    assert_eq!(result.err(), Some(PasskeyError::Invalid));
    assert_eq!(alice_session.last_auth, before);
    assert_eq!(f.count("session.step_up"), 0);
    assert_eq!(f.shapes("signin.fail").len(), 1);

    // Bob's session cannot finish a challenge that was started for alice's, even with a valid assertion of his own.
    let mut bob_session = f.session(bob);
    let (options, token) = f.passkeys.start_step_up(&alice_session).expect("starts");
    let options: Value = serde_json::from_str(&options).expect("JSON");
    let mut forced = options;
    forced["publicKey"]["allowCredentials"] =
        json!([{ "type": "public-key", "id": URL_SAFE_NO_PAD.encode(bob_key.credential_id()) }]);
    let response = bob_key.authenticate(&forced, &f.origin).expect("answers").to_string();
    let result = f.passkeys.finish_step_up(token.expose(), &response, &mut bob_session, ADDR);
    assert_eq!(result.err(), Some(PasskeyError::Invalid));
    assert_eq!(f.count("session.step_up"), 0);
}

#[test]
fn a_session_without_a_passkey_cannot_start_a_step_up() {
    let f = fixture("stepup-none");
    let (alice, _handle) = f.account("alice");
    let session = f.session(alice);
    assert_eq!(f.passkeys.start_step_up(&session).err(), Some(PasskeyError::Conflict));
    assert_eq!(f.passkeys.open_challenges(), 0);
}

#[test]
fn a_step_up_token_cannot_finish_a_sign_in_and_the_reverse() {
    let f = fixture("kinds");
    let (alice, handle) = f.account("alice");
    let (mut authenticator, _) = f.register("alice", handle, "Laptop");
    let mut session = f.session(alice);

    let (options, token) = f.passkeys.start_step_up(&session).expect("starts");
    let options: Value = serde_json::from_str(&options).expect("JSON");
    let response = authenticator.authenticate(&options, &f.origin).expect("answers").to_string();
    let cross = f.passkeys.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE);
    assert_eq!(cross.err(), Some(PasskeyError::Invalid));
    // The wrong-kind token was consumed: the right call with it fails now as well.
    let after = f.passkeys.finish_step_up(token.expose(), &response, &mut session, ADDR);
    assert_eq!(after.err(), Some(PasskeyError::Invalid));

    let (options, token) = f.start_sign_in();
    let response = authenticator.authenticate(&options, &f.origin).expect("answers").to_string();
    let cross = f.passkeys.finish_step_up(token.expose(), &response, &mut session, ADDR);
    assert_eq!(cross.err(), Some(PasskeyError::Invalid));
    let after = f.passkeys.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE);
    assert_eq!(after.err(), Some(PasskeyError::Invalid));
    assert_eq!(f.count("signin.ok"), 0);
    assert_eq!(f.store.read(|tx| tx.session_list(alice)).expect("sessions").len(), 1, "only the first session exists");
}

// ---- management ----

#[test]
fn the_list_has_the_fields_of_that_account_only_and_last_used_follows_the_clock() {
    let f = fixture("list");
    let (alice, alice_handle) = f.account("alice");
    let (bob, bob_handle) = f.account("bob");
    let (mut alice_key, alice_id) = f.register("alice", alice_handle, "Laptop");
    let (_bob_key, bob_id) = f.register("bob", bob_handle, "Phone");

    let listed = f.passkeys.list(alice).expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, alice_id);
    assert_eq!(listed[0].name, "Laptop");
    assert_eq!(listed[0].created_at, NOW);
    assert_eq!(listed[0].last_used, None);
    assert_eq!(f.passkeys.list(bob).expect("list")[0].id, bob_id);

    f.clock.advance(3_600);
    f.sign_in(&mut alice_key, None).expect("signs in");
    let listed = f.passkeys.list(alice).expect("list");
    assert_eq!(listed[0].last_used, Some(NOW + 3_600));
    assert_eq!(f.passkeys.list(bob).expect("list")[0].last_used, None);
    assert!(f.passkeys.list(AccountId::from_raw(9_999)).expect("list").is_empty());
}

#[test]
fn removing_another_accounts_passkey_does_nothing() {
    let f = fixture("remove-foreign");
    let (alice, alice_handle) = f.account("alice");
    let (bob, bob_handle) = f.account("bob");
    f.register("alice", alice_handle, "Laptop");
    let (_bob_key, bob_id) = f.register("bob", bob_handle, "Phone");
    f.add_password_with_authenticator(alice);
    assert_eq!(f.passkeys.remove(alice, bob_id, Some(ADDR)).err(), Some(PasskeyError::NotFound));
    assert_eq!(f.passkeys.remove(alice, 9_999, Some(ADDR)).err(), Some(PasskeyError::NotFound));
    assert_eq!(f.count("auth.passkey.remove"), 0, "no line for an id that is not the account's");
    assert_eq!(f.passkeys.list(bob).expect("list").len(), 1);
}

#[test]
fn the_last_way_to_sign_in_cannot_be_removed() {
    // Case 1: only this passkey.
    let f = fixture("last-1");
    let (alice, handle) = f.account("alice");
    let (_key, id) = f.register("alice", handle, "Only");
    assert_eq!(f.passkeys.remove(alice, id, Some(ADDR)).err(), Some(PasskeyError::Conflict));
    assert_eq!(f.passkeys.list(alice).expect("list").len(), 1);
    let lines = f.shapes("auth.passkey.remove");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["result"], "fail");

    // A password without an authenticator is no way to sign in.
    f.store
        .write(|tx| {
            tx.password_set(
                alice,
                &PasswordCredential { verifier: vec![1; 32], salt: vec![2; 16], params: "test".into(), changed_at: NOW },
            )
        })
        .expect("password only");
    assert_eq!(f.passkeys.remove(alice, id, Some(ADDR)).err(), Some(PasskeyError::Conflict));

    // Case 2: the passkey and a password with an authenticator.
    f.add_password_with_authenticator(alice);
    f.passkeys.remove(alice, id, Some(ADDR)).expect("allowed with a password and an authenticator");

    // Case 3: two passkeys.
    let f = fixture("last-3");
    let (alice, handle) = f.account("alice");
    let (_a, first) = f.register("alice", handle, "One");
    let (_b, second) = f.register("alice", handle, "Two");
    f.passkeys.remove(alice, first, Some(ADDR)).expect("allowed with another passkey");
    assert_eq!(f.passkeys.remove(alice, second, Some(ADDR)).err(), Some(PasskeyError::Conflict));

    // Case 4: the passkey and a single sign-on identity.
    let f = fixture("last-4");
    let (alice, handle) = f.account("alice");
    let (_key, id) = f.register("alice", handle, "Only");
    f.store.write(|tx| tx.oidc_link(alice, "https://idp.example.test", "subject-1")).expect("link");
    f.passkeys.remove(alice, id, None).expect("allowed with a single sign-on identity");
}

#[test]
fn a_removal_writes_one_line_and_the_removed_credential_no_longer_signs_in() {
    let f = fixture("remove-line");
    let (alice, handle) = f.account("alice");
    let (mut first, first_id) = f.register("alice", handle, "One");
    let (mut second, _second_id) = f.register("alice", handle, "Two");
    f.passkeys.remove(alice, first_id, Some(ADDR)).expect("removes");
    let lines = f.shapes("auth.passkey.remove");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["result"], "ok");
    assert_eq!(lines[0]["actor"], alice.to_string());
    assert_eq!(f.sign_in(&mut first, None).err(), Some(PasskeyError::Invalid));
    f.sign_in(&mut second, None).expect("the other passkey still signs in");
    assert_eq!(f.passkeys.remove(alice, first_id, Some(ADDR)).err(), Some(PasskeyError::NotFound), "gone");
}

// ---- accounts that cannot sign in ----

#[test]
fn a_disabled_or_a_deleted_account_cannot_sign_in_with_a_valid_credential() {
    let f = fixture("states");
    let (_alice, alice_handle) = f.account("alice");
    let (bob, bob_handle) = f.account("bob");
    let (carol, carol_handle) = f.account("carol");
    let (mut alice_key, _) = f.register("alice", alice_handle, "A");
    let (mut bob_key, _) = f.register("bob", bob_handle, "B");
    let (mut carol_key, _) = f.register("carol", carol_handle, "C");
    f.store.write(|tx| tx.account_set_state(bob, AccountState::Disabled)).expect("disable");
    f.store.write(|tx| tx.account_delete(carol)).expect("delete");

    // A bad signature, for the audit shape to compare with.
    let (options, token) = f.start_sign_in();
    let mut response = alice_key.authenticate(&options, &f.origin).expect("answers");
    response["response"]["signature"] = json!(URL_SAFE_NO_PAD.encode([0u8; 64]));
    let bad = f.passkeys.finish_sign_in(token.expose(), &response.to_string(), None, ADDR, DEVICE);
    assert_eq!(bad.err(), Some(PasskeyError::Invalid));

    assert_eq!(f.sign_in(&mut bob_key, None).err(), Some(PasskeyError::Invalid));
    assert_eq!(f.sign_in(&mut carol_key, None).err(), Some(PasskeyError::Invalid));
    let fails = f.shapes("signin.fail");
    assert_eq!(fails.len(), 3);
    assert_eq!(fails[0], fails[1], "a disabled account fails like a bad signature");
    assert_eq!(fails[0], fails[2], "a deleted account fails like a bad signature");
    assert_eq!(fails[0]["detail"], json!({ "method": "passkey", "reason": "invalid" }));
    assert_eq!(fails[0]["actor"], "anonymous");
    assert_eq!(fails[0]["target"], "");
    assert_eq!(f.count("signin.ok"), 0);
    for account in [bob, carol] {
        assert!(f.store.read(|tx| tx.session_list(account)).expect("sessions").is_empty());
    }
    f.sign_in(&mut alice_key, None).expect("the active account still signs in");
}


// ---- the bounds of the challenge store ----

/// An address of the test network with a distinct key per `n`.
fn addr_n(n: usize) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(10, 7, u8::try_from(n / 256).expect("fits"), u8::try_from(n % 256).expect("fits")))
}

/// Starts a sign-in from `addr` and has the authenticator answer it at once.
fn started_at(f: &Fixture, key: &mut SoftPasskey, addr: IpAddr) -> (ChallengeToken, String) {
    let (options, token) = f.passkeys.start_sign_in(addr).expect("starts");
    let options: Value = serde_json::from_str(&options).expect("JSON");
    let response = key.authenticate(&options, &f.origin).expect("answers").to_string();
    (token, response)
}

#[test]
fn the_1025th_challenge_pushes_out_the_oldest_of_all() {
    let f = fixture("cap-total");
    let (_alice, handle) = f.account("alice");
    let (mut key, _) = f.register("alice", handle, "Key");

    let (first, first_response) = started_at(&f, &mut key, addr_n(0));
    let (second, second_response) = started_at(&f, &mut key, addr_n(0));
    // 16 per address: 1,024 live challenges come from 64 addresses.
    for n in 2..1_024 {
        f.passkeys.start_sign_in(addr_n(n / 16)).expect("starts");
    }
    assert_eq!(f.passkeys.open_challenges(), 1_024);
    f.passkeys.start_sign_in(addr_n(500)).expect("the 1,025th starts");
    assert_eq!(f.passkeys.open_challenges(), 1_024, "the map did not grow");

    let evicted = f.passkeys.finish_sign_in(first.expose(), &first_response, None, ADDR, DEVICE);
    assert_eq!(evicted.err(), Some(PasskeyError::Invalid), "the oldest is gone");
    f.passkeys.finish_sign_in(second.expose(), &second_response, None, ADDR, DEVICE).expect("the second is still there");
}

#[test]
fn the_17th_challenge_of_one_address_pushes_out_that_addresss_oldest() {
    let f = fixture("cap-address");
    let (_alice, handle) = f.account("alice");
    let (mut key, _) = f.register("alice", handle, "Key");
    let a = addr_n(1);
    let b = addr_n(2);

    // The other address's challenge is the oldest of all.
    let (b0, b0_response) = {
        let (options, token) = f.passkeys.start_sign_in(b).expect("starts");
        let options: Value = serde_json::from_str(&options).expect("JSON");
        (token, (options, ()))
    };
    let mut a_tokens = Vec::new();
    for _ in 0..16 {
        let (options, token) = f.passkeys.start_sign_in(a).expect("starts");
        a_tokens.push((token, serde_json::from_str::<Value>(&options).expect("JSON")));
    }
    assert_eq!(f.passkeys.open_challenges(), 17);
    f.passkeys.start_sign_in(a).expect("the 17th of the address starts");
    assert_eq!(f.passkeys.open_challenges(), 17, "one went out and one came in");

    let a0 = key.authenticate(&a_tokens[0].1, &f.origin).expect("answers").to_string();
    let a1 = key.authenticate(&a_tokens[1].1, &f.origin).expect("answers").to_string();
    let b0_text = key.authenticate(&b0_response.0, &f.origin).expect("answers").to_string();
    let gone = f.passkeys.finish_sign_in(a_tokens[0].0.expose(), &a0, None, ADDR, DEVICE);
    assert_eq!(gone.err(), Some(PasskeyError::Invalid), "the oldest of address A is gone");
    f.passkeys.finish_sign_in(a_tokens[1].0.expose(), &a1, None, ADDR, DEVICE).expect("the next of A is still there");
    f.passkeys.finish_sign_in(b0.expose(), &b0_text, None, ADDR, DEVICE).expect("address B's older challenge was not touched");
}

#[test]
fn a_challenge_works_for_five_minutes_exactly_and_not_a_second_longer() {
    let f = fixture("ttl");
    let (_alice, handle) = f.account("alice");
    let (mut key, _) = f.register("alice", handle, "Key");

    let (token, response) = started_at(&f, &mut key, ADDR);
    f.clock.advance(300);
    f.passkeys.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE).expect("works at 300 seconds");

    let (token, response) = started_at(&f, &mut key, ADDR);
    f.clock.advance(301);
    let late = f.passkeys.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE);
    assert_eq!(late.err(), Some(PasskeyError::Invalid), "refused at 301 seconds");
    assert_eq!(f.passkeys.open_challenges(), 0, "and it is gone");

    // A registration token has the same time.
    let (_options, token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
    f.clock.advance(301);
    assert_eq!(f.passkeys.verify_registration(token.expose(), "{}", None).err(), Some(PasskeyError::Invalid));
}

#[test]
fn expired_challenges_are_purged_by_a_bounded_call_and_by_every_start() {
    let f = fixture("purge");
    for n in 0..6 {
        f.passkeys.start_sign_in(addr_n(n)).expect("starts");
    }
    f.clock.advance(301);
    assert_eq!(f.passkeys.purge(4), 4, "at most the number asked for");
    assert_eq!(f.passkeys.open_challenges(), 2);
    assert_eq!(f.passkeys.purge(10), 2);
    assert_eq!(f.passkeys.open_challenges(), 0);

    for n in 0..3 {
        f.passkeys.start_sign_in(addr_n(n)).expect("starts");
    }
    f.clock.advance(301);
    f.passkeys.start_sign_in(addr_n(9)).expect("starts");
    assert_eq!(f.passkeys.open_challenges(), 1, "a start removed the expired ones");
    assert_eq!(f.passkeys.purge(10), 0, "nothing that is alive is purged");
}

#[test]
fn a_token_of_the_wrong_kind_is_refused_and_consumed() {
    let f = fixture("wrong-kind");
    let (_alice, handle) = f.account("alice");
    let mut key = SoftPasskey::new();
    let (reg_options, reg_token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
    let reg_options: Value = serde_json::from_str(&reg_options).expect("JSON");
    let reg_response = key.register(&reg_options, &f.origin).expect("answers").to_string();

    // A registration token used for a sign-in is refused, and is gone for the registration too.
    let cross = f.passkeys.finish_sign_in(reg_token.expose(), &reg_response, None, ADDR, DEVICE);
    assert_eq!(cross.err(), Some(PasskeyError::Invalid));
    let after = f.passkeys.finish_registration(reg_token.expose(), &reg_response, None, "Key", ADDR);
    assert_eq!(after.err(), Some(PasskeyError::Invalid));

    // And the reverse.
    let (options, token) = f.start_sign_in();
    let _ = options;
    let cross = f.passkeys.finish_registration(token.expose(), &reg_response, None, "Key", ADDR);
    assert_eq!(cross.err(), Some(PasskeyError::Invalid));
    assert_eq!(f.passkeys.open_challenges(), 0);
}

#[test]
fn a_token_from_another_process_or_made_up_is_refused_without_calling_the_library() {
    let f = fixture("foreign-token");
    let (_alice, handle) = f.account("alice");
    let (mut key, _) = f.register("alice", handle, "Key");
    let (token, response) = started_at(&f, &mut key, ADDR);

    // A second service over the same store is a restarted process: it has none of the first one's challenges.
    let clock: Arc<ManualClock> = f.clock.clone();
    let audit = Arc::new(Audit::open(f.scratch.path(), clock.clone()).expect("audit"));
    let restarted =
        Passkeys::new(PUBLIC_URL, "holzWave", f.store.clone(), f.sessions.clone(), audit, clock).expect("builds");
    let result = restarted.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE);
    assert_eq!(result.err(), Some(PasskeyError::Invalid));
    assert_eq!(restarted.library_calls(), 0, "no token, no library call");

    let before = f.passkeys.library_calls();
    for text in ["", "short", &"A".repeat(42), &"A".repeat(44), &format!("{}!", "A".repeat(42)), &"é".repeat(22), &"A".repeat(1_000_000)] {
        let result = f.passkeys.finish_sign_in(text, &response, None, ADDR, DEVICE);
        assert_eq!(result.err(), Some(PasskeyError::Invalid), "token {:?}", text.chars().take(8).collect::<String>());
    }
    assert_eq!(f.passkeys.library_calls(), before, "a made-up token never reaches the library");
    f.passkeys.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE).expect("the real token still works");
}

// ---- replay and concurrency ----

#[test]
fn a_successful_response_sent_again_fails_against_a_new_challenge_and_against_the_old_token() {
    let f = fixture("replay");
    let (_alice, handle) = f.account("alice");
    let (mut key, _) = f.register("alice", handle, "Key");
    let (token, response) = started_at(&f, &mut key, ADDR);
    f.passkeys.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE).expect("the first use verifies");

    let (_options, fresh) = f.start_sign_in();
    let replay = f.passkeys.finish_sign_in(fresh.expose(), &response, None, ADDR, DEVICE);
    assert_eq!(replay.err(), Some(PasskeyError::Invalid), "a new challenge does not accept the old answer");
    let again = f.passkeys.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE);
    assert_eq!(again.err(), Some(PasskeyError::Invalid), "the old token is used up");
    assert_eq!(f.count("signin.ok"), 1);
    assert_eq!(f.shapes("signin.fail").len(), 2);
}

#[test]
fn a_counter_that_goes_backwards_is_refused() {
    let f = fixture("counter");
    let (alice, handle) = f.account("alice");
    let (mut key, id) = f.register("alice", handle, "Key");
    f.sign_in(&mut key, None).expect("counter 1");
    f.sign_in(&mut key, None).expect("counter 2");

    // The stored row is ahead of the authenticator, as after a clone of the credential was used.
    let row = f.store.read(|tx| tx.passkey_list(alice)).expect("rows").remove(0);
    assert_eq!(row.id, id);
    let mut stored: Value = serde_json::from_str(&row.passkey_json).expect("JSON");
    assert_eq!(stored["passkey"]["cred"]["counter"], 2, "the sign-ins moved the stored counter");
    stored["passkey"]["cred"]["counter"] = json!(9);
    f.store.write(|tx| tx.passkey_update_json(alice, id, &stored.to_string())).expect("write");
    assert_eq!(f.sign_in(&mut key, None).err(), Some(PasskeyError::Invalid));
    let after = f.store.read(|tx| tx.passkey_list(alice)).expect("rows").remove(0);
    assert_eq!(after.passkey_json, stored.to_string(), "the row was not repaired or removed");
}

#[test]
fn two_threads_finishing_one_sign_in_token_make_one_session() {
    let f = fixture("race-sign-in");
    let (alice, handle) = f.account("alice");
    let (mut key, _) = f.register("alice", handle, "Key");
    for round in 0..5 {
        let (token, response) = started_at(&f, &mut key, ADDR);
        let barrier = Barrier::new(8);
        let results: Vec<Result<(), PasskeyError>> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        f.passkeys.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE).map(|_| ())
                    })
                })
                .collect();
            workers.into_iter().map(|w| w.join().expect("no panic")).collect()
        });
        let ok = results.iter().filter(|r| r.is_ok()).count();
        let invalid = results.iter().filter(|r| **r == Err(PasskeyError::Invalid)).count();
        println!("round {round}: ok {ok}, invalid {invalid}");
        assert_eq!((ok, invalid), (1, 7));
    }
    assert_eq!(f.store.read(|tx| tx.session_list(alice)).expect("sessions").len(), 5, "one session per round");
    assert_eq!(f.count("signin.ok"), 5);
}

#[test]
fn threads_registering_with_one_token_store_one_row() {
    let f = fixture("race-register");
    let (alice, handle) = f.account("alice");
    let mut key = SoftPasskey::new();
    let (options, token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
    let options: Value = serde_json::from_str(&options).expect("JSON");
    let response = key.register(&options, &f.origin).expect("answers").to_string();
    let barrier = Barrier::new(6);
    let results: Vec<Result<i64, PasskeyError>> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..6)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    f.passkeys.finish_registration(token.expose(), &response, None, "Key", ADDR).map(|r| r.id)
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().expect("no panic")).collect()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|r| **r == Err(PasskeyError::Invalid)).count(), 5);
    assert_eq!(f.store.read(|tx| tx.passkey_count(alice)).expect("count"), 1);
    assert_eq!(f.shapes("auth.passkey.add").iter().filter(|l| l["result"] == "ok").count(), 1);
}

// ---- hostile input ----

fn flip_bit_in(field: &mut Value, byte: usize) {
    let mut bytes = URL_SAFE_NO_PAD.decode(field.as_str().expect("a string")).expect("base64url");
    bytes[byte] ^= 1;
    *field = json!(URL_SAFE_NO_PAD.encode(bytes));
}

fn edit_client_data(response: &mut Value, edit: impl FnOnce(&mut Value)) {
    let field = &mut response["response"]["clientDataJSON"];
    let bytes = URL_SAFE_NO_PAD.decode(field.as_str().expect("a string")).expect("base64url");
    let mut client_data: Value = serde_json::from_slice(&bytes).expect("client data is JSON");
    edit(&mut client_data);
    *field = json!(URL_SAFE_NO_PAD.encode(client_data.to_string()));
}

#[test]
fn an_oversize_credential_never_reaches_the_library() {
    let f = fixture("oversize");
    let (alice, handle) = f.account("alice");
    let (mut key, _) = f.register("alice", handle, "Key");
    let session = f.session(alice);
    let mut session_mut = session.clone();
    let big = "x".repeat(32 * 1024 + 1);
    let exact = " ".repeat(32 * 1024);
    let before = f.passkeys.library_calls();

    let (_options, token) = f.start_sign_in();
    assert_eq!(f.passkeys.finish_sign_in(token.expose(), &big, None, ADDR, DEVICE).err(), Some(PasskeyError::Invalid));
    let (_options, token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
    assert_eq!(f.passkeys.verify_registration(token.expose(), &big, None).err(), Some(PasskeyError::Invalid));
    let (_options, token) = f.passkeys.start_step_up(&session).expect("starts");
    assert_eq!(
        f.passkeys.finish_step_up(token.expose(), &big, &mut session_mut, ADDR).err(),
        Some(PasskeyError::Invalid)
    );
    assert_eq!(f.passkeys.library_calls(), before, "32 KiB plus one byte: the library was not called");

    // Exactly 32 KiB is looked at (and is not a credential).
    let (_options, token) = f.start_sign_in();
    assert_eq!(f.passkeys.finish_sign_in(token.expose(), &exact, None, ADDR, DEVICE).err(), Some(PasskeyError::Invalid));
    assert_eq!(f.passkeys.library_calls(), before + 1, "exactly 32 KiB reaches the parser");

    // The same sizes with a real, correct response around them still work.
    f.sign_in(&mut key, None).expect("a normal sign-in works");
}

#[test]
fn hostile_sign_in_responses_all_end_in_the_one_error_with_the_same_audit_shape() {
    let f = fixture("hostile");
    let (_alice, handle) = f.account("alice");
    let (mut key, _) = f.register("alice", handle, "Key");
    type Edit = Box<dyn Fn(Value) -> String>;
    let cases: Vec<(&str, Edit)> = vec![
        ("truncated JSON", Box::new(|v| { let t = v.to_string(); t[..t.len() / 2].to_owned() })),
        ("an array", Box::new(|_| "[1,2,3]".to_owned())),
        ("a string", Box::new(|_| "\"MARKER-text\"".to_owned())),
        ("empty text", Box::new(|_| String::new())),
        ("missing id", Box::new(|mut v| { v.as_object_mut().expect("o").remove("id"); v.to_string() })),
        ("rawId not base64", Box::new(|mut v| { v["rawId"] = json!("MARKER-rawid!!"); v.to_string() })),
        ("another client data type", Box::new(|mut v| { edit_client_data(&mut v, |c| c["type"] = json!("webauthn.create")); v.to_string() })),
        ("challenge off by one bit", Box::new(|mut v| {
            edit_client_data(&mut v, |c| {
                let mut challenge = c["challenge"].clone();
                flip_bit_in(&mut challenge, 0);
                c["challenge"] = challenge;
            });
            v.to_string()
        })),
        ("another origin in the client data", Box::new(|mut v| { edit_client_data(&mut v, |c| c["origin"] = json!("https://evil.example.test")); v.to_string() })),
        ("signature off by one bit", Box::new(|mut v| { flip_bit_in(&mut v["response"]["signature"], 5); v.to_string() })),
        ("relying party hash off by one bit", Box::new(|mut v| { flip_bit_in(&mut v["response"]["authenticatorData"], 10); v.to_string() })),
        ("counter off by one bit", Box::new(|mut v| { flip_bit_in(&mut v["response"]["authenticatorData"], 36); v.to_string() })),
        ("user handle of 15 bytes", Box::new(|mut v| { v["response"]["userHandle"] = json!(URL_SAFE_NO_PAD.encode([7u8; 15])); v.to_string() })),
        ("user handle of 17 bytes", Box::new(|mut v| { v["response"]["userHandle"] = json!(URL_SAFE_NO_PAD.encode([7u8; 17])); v.to_string() })),
        ("user handle of no account", Box::new(|mut v| { v["response"]["userHandle"] = json!(URL_SAFE_NO_PAD.encode([7u8; 16])); v.to_string() })),
        ("no user handle", Box::new(|mut v| { v["response"].as_object_mut().expect("o").remove("userHandle"); v.to_string() })),
        ("credential id of no credential", Box::new(|mut v| { v["rawId"] = json!(URL_SAFE_NO_PAD.encode([9u8; 32])); v["id"] = v["rawId"].clone(); v.to_string() })),
    ];
    let mut markers_seen = Vec::new();
    for (name, edit) in &cases {
        let (token, response) = {
            let (options, token) = f.start_sign_in();
            (token, key.authenticate(&options, &f.origin).expect("answers"))
        };
        let text = edit(response);
        let result = f.passkeys.finish_sign_in(token.expose(), &text, None, ADDR, DEVICE);
        let error = result.expect_err(name);
        assert_eq!(error, PasskeyError::Invalid, "{name}");
        for shown in [format!("{error}"), format!("{error:?}")] {
            assert!(!shown.contains("MARKER") && (text.is_empty() || !shown.contains(&text)), "{name}: the error shows input");
            markers_seen.push(shown);
        }
    }
    // The hash of another relying party id, signed correctly.
    let (options, token) = f.start_sign_in();
    let tweaks = Tweaks { skip_browser_checks: true, rp_id: Some("example.test".into()), ..Tweaks::default() };
    let response = key.authenticate_with(&options, &f.origin, &tweaks).expect("signs").to_string();
    assert_eq!(f.passkeys.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE).err(), Some(PasskeyError::Invalid));

    let fails = f.shapes("signin.fail");
    assert_eq!(fails.len(), cases.len() + 1);
    assert!(fails.iter().all(|line| *line == fails[0]), "one audit shape for every cause");
    assert_eq!(fails[0]["detail"], json!({ "method": "passkey", "reason": "invalid" }));
    assert_eq!(f.count("signin.ok"), 0);
    let audit_text: String = f.lines().iter().map(Value::to_string).collect();
    assert!(!audit_text.contains("MARKER"));
    f.sign_in(&mut key, None).expect("the real authenticator still signs in after all of it");
    println!("hostile cases: {}", cases.iter().map(|(n, _)| *n).collect::<Vec<_>>().join("; "));
}

#[test]
fn hostile_registrations_are_refused_and_store_nothing() {
    let f = fixture("hostile-reg");
    let (alice, handle) = f.account("alice");
    let mut key = SoftPasskey::new();
    type Make = Box<dyn Fn(&Fixture, &mut SoftPasskey, &Value) -> String>;
    let cases: Vec<(&str, Make)> = vec![
        ("truncated", Box::new(|f, k, o| { let t = k.register(o, &f.origin).expect("a").to_string(); t[..t.len() / 3].to_owned() })),
        ("an array", Box::new(|_, _, _| "[]".to_owned())),
        ("another origin", Box::new(|_, k, o| k.register_with(o, &Url::parse("https://evil.example.test").expect("u"), &Tweaks::unchecked()).expect("a").to_string())),
        ("another relying party id", Box::new(|f, k, o| k.register_with(o, &f.origin, &Tweaks { skip_browser_checks: true, rp_id: Some("example.test".into()), ..Tweaks::default() }).expect("a").to_string())),
        ("no user verification", Box::new(|f, k, o| k.register_with(o, &f.origin, &Tweaks { user_verification: Some("discouraged".into()), ..Tweaks::default() }).expect("a").to_string())),
        ("another client data type", Box::new(|f, k, o| { let mut v = k.register(o, &f.origin).expect("a"); edit_client_data(&mut v, |c| c["type"] = json!("webauthn.get")); v.to_string() })),
        ("challenge off by one bit", Box::new(|f, k, o| {
            let mut v = k.register(o, &f.origin).expect("a");
            edit_client_data(&mut v, |c| { let mut ch = c["challenge"].clone(); flip_bit_in(&mut ch, 3); c["challenge"] = ch; });
            v.to_string()
        })),
    ];
    for (name, make) in &cases {
        let (options, token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
        let options: Value = serde_json::from_str(&options).expect("JSON");
        let text = make(&f, &mut key, &options);
        let result = f.passkeys.finish_registration(token.expose(), &text, None, "Key", ADDR);
        assert_eq!(result.err(), Some(PasskeyError::Invalid), "{name}");
    }
    assert_eq!(f.store.read(|tx| tx.passkey_count(alice)).expect("count"), 0, "nothing was stored");
    let fails = f.shapes("auth.passkey.add");
    assert_eq!(fails.len(), cases.len());
    assert!(fails.iter().all(|line| *line == fails[0] && line["result"] == "fail"), "one audit shape");
    assert!(f.store.read(|tx| tx.session_list(alice)).expect("sessions").is_empty());
}

// ---- damage ----

#[test]
fn a_damaged_stored_credential_is_refused_and_neither_removed_nor_repaired() {
    let f = fixture("tamper");
    let (alice, handle) = f.account("alice");
    let (mut key, id) = f.register("alice", handle, "Key");
    let original = f.store.read(|tx| tx.passkey_list(alice)).expect("rows").remove(0).passkey_json;
    let value: Value = serde_json::from_str(&original).expect("JSON");

    let mut damaged: Vec<(&str, String)> = vec![("empty", String::new()), ("not JSON", "x".repeat(40))];
    let mut first_byte = original.clone();
    first_byte.replace_range(0..1, "[");
    damaged.push(("first byte changed", first_byte));
    for (name, pointer) in [
        ("credential id inside the JSON", "/passkey/cred/cred_id"),
        ("public key x", "/passkey/cred/cred/key/EC_EC2/x"),
        ("public key y", "/passkey/cred/cred/key/EC_EC2/y"),
    ] {
        let mut copy = value.clone();
        let text = copy.pointer(pointer).and_then(Value::as_str).expect(name).to_owned();
        let mut chars: Vec<char> = text.chars().collect();
        chars[3] = if chars[3] == 'A' { 'B' } else { 'A' };
        *copy.pointer_mut(pointer).expect(name) = json!(chars.into_iter().collect::<String>());
        damaged.push((name, copy.to_string()));
    }
    let mut versioned = value.clone();
    versioned["v"] = json!(2);
    damaged.push(("another version", versioned.to_string()));
    let mut extra = value.clone();
    extra["evil"] = json!(1);
    damaged.push(("an unknown member", extra.to_string()));
    let mut policy = value.clone();
    policy["passkey"]["cred"]["user_verified"] = json!("yes");
    damaged.push(("a wrong type", policy.to_string()));

    for (name, text) in &damaged {
        f.store.write(|tx| tx.passkey_update_json(alice, id, text)).expect("write the damage");
        assert_eq!(f.sign_in(&mut key, None).err(), Some(PasskeyError::Invalid), "{name}");
        let row = f.store.read(|tx| tx.passkey_list(alice)).expect("rows");
        assert_eq!(row.len(), 1, "{name}: the row is still there");
        assert_eq!(&row[0].passkey_json, text, "{name}: the row was not repaired");
        // A damaged row can still be listed and removed.
        assert_eq!(f.passkeys.list(alice).expect("list").len(), 1);
    }
    f.store.write(|tx| tx.passkey_update_json(alice, id, &original)).expect("restore");
    f.sign_in(&mut key, None).expect("the untouched row signs in");
    assert_eq!(f.shapes("signin.fail").len(), damaged.len());
    assert!(f.shapes("signin.fail").iter().all(|l| l["detail"] == json!({ "method": "passkey", "reason": "invalid" })));
}

#[test]
fn a_row_whose_credential_id_does_not_match_its_json_is_refused() {
    let f = fixture("mismatch");
    let (alice, handle) = f.account("alice");
    let (mut first, _) = f.register("alice", handle, "One");
    let first_row = f.store.read(|tx| tx.passkey_list(alice)).expect("rows").remove(0);

    // A second authenticator that is verified but never stored, then stored under its own id with the first one's JSON.
    let mut second = SoftPasskey::new();
    let (options, token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
    let options: Value = serde_json::from_str(&options).expect("JSON");
    let response = second.register(&options, &f.origin).expect("answers").to_string();
    let verified = f.passkeys.verify_registration(token.expose(), &response, None).expect("verifies");
    f.store
        .write(|tx| tx.passkey_add(alice, verified.credential_id(), &first_row.passkey_json, Some(&[1u8; 32]), NOW))
        .expect("the mismatched row");
    assert_eq!(f.sign_in(&mut second, None).err(), Some(PasskeyError::Invalid));
    f.sign_in(&mut first, None).expect("the first, untouched row still signs in");
}

// ---- user verification and attestation ----

#[test]
fn an_assertion_without_the_user_verified_flag_is_refused() {
    let f = fixture("uv");
    let (_alice, handle) = f.account("alice");
    let (mut key, _) = f.register("alice", handle, "Key");
    let (options, token) = f.start_sign_in();
    let tweaks = Tweaks { user_verification: Some("discouraged".into()), ..Tweaks::default() };
    let response = key.authenticate_with(&options, &f.origin, &tweaks).expect("answers").to_string();
    assert_eq!(f.passkeys.finish_sign_in(token.expose(), &response, None, ADDR, DEVICE).err(), Some(PasskeyError::Invalid));
    f.sign_in(&mut key, None).expect("with the flag it works");
}

fn attestation_format(response: &Value) -> String {
    let object = URL_SAFE_NO_PAD
        .decode(response["response"]["attestationObject"].as_str().expect("an attestation object"))
        .expect("base64url");
    // The object is a CBOR map; the format is the text after the key "fmt" (0x63 'f' 'm' 't').
    let at = object.windows(4).position(|w| w == [0x63, b'f', b'm', b't']).expect("fmt key") + 4;
    let length = usize::from(object[at] & 0x1f);
    String::from_utf8(object[at + 1..at + 1 + length].to_vec()).expect("text")
}

fn with_attestation_format(response: &Value, to: &str) -> Value {
    let mut object = URL_SAFE_NO_PAD
        .decode(response["response"]["attestationObject"].as_str().expect("an attestation object"))
        .expect("base64url");
    let at = object.windows(4).position(|w| w == [0x63, b'f', b'm', b't']).expect("fmt key") + 4;
    let length = usize::from(object[at] & 0x1f);
    let mut replacement = vec![0x60 | u8::try_from(to.len()).expect("short")];
    replacement.extend_from_slice(to.as_bytes());
    object.splice(at..at + 1 + length, replacement);
    let mut out = response.clone();
    out["response"]["attestationObject"] = json!(URL_SAFE_NO_PAD.encode(object));
    out
}

#[test]
fn a_registration_whose_attestation_is_not_none_is_pinned() {
    let f = fixture("attestation");
    let (alice, handle) = f.account("alice");
    let mut outcomes = Vec::new();
    for (case, format) in [("as sent by the authenticator", None), ("none", Some("none")), ("a format of a vendor", Some("android-key")), ("a format nobody knows", Some("nonsense"))] {
        let mut key = SoftPasskey::new();
        let (options, token) = f.passkeys.start_registration(&f.subject("alice", handle), ADDR).expect("starts");
        let options: Value = serde_json::from_str(&options).expect("JSON");
        let sent = key.register(&options, &f.origin).expect("answers");
        let sent = match format {
            None => sent,
            Some(to) => with_attestation_format(&sent, to),
        };
        let own = attestation_format(&sent);
        let result = f.passkeys.finish_registration(token.expose(), &sent.to_string(), None, case, ADDR);
        println!("attestation {case}: format {own:?}: {}", if result.is_ok() { "accepted" } else { "refused" });
        outcomes.push((case, own, result.is_ok()));
    }
    // What was observed with the pinned library: the soft authenticator sends a self-attested statement and it is accepted,
    // with the statement not trusted for anything (the stored form says `Self_`); a changed format with a statement that does
    // not fit it is refused.
    println!("{outcomes:?}");
    assert!(outcomes[0].2, "{outcomes:?}");
    let row = f.store.read(|tx| tx.passkey_list(alice)).expect("rows").remove(0);
    let stored: Value = serde_json::from_str(&row.passkey_json).expect("JSON");
    assert_eq!(stored["passkey"]["cred"]["attestation"]["data"], "Self_", "no attestation authority is recorded");
    assert_eq!(stored["passkey"]["cred"]["attestation"]["metadata"], "None");
    assert_eq!(f.store.read(|tx| tx.passkey_count(alice)).expect("count"), outcomes.iter().filter(|o| o.2).count());
}

// ---- the canary ----

#[test]
fn a_canary_in_the_names_shows_up_nowhere_it_should_not() {
    const CANARY: &str = "canary-7c1e9f";
    let f = fixture("canary");
    let (alice, handle) = f.account(CANARY);
    let subject = RegistrationSubject {
        handle,
        user_name: CANARY.to_owned(),
        display_name: format!("{CANARY} display"),
    };
    let mut shown: Vec<String> = vec![format!("{subject:?}")];

    let mut key = SoftPasskey::new();
    let (options, token) = f.passkeys.start_registration(&subject, ADDR).expect("starts");
    let options: Value = serde_json::from_str(&options).expect("JSON");
    let response = key.register(&options, &f.origin).expect("answers").to_string();
    let registered =
        f.passkeys.finish_registration(token.expose(), &response, None, &format!("{CANARY} name"), ADDR).expect("verifies");
    shown.push(format!("{registered:?}"));
    // The name is stored where it belongs, and nowhere else.
    assert_eq!(f.passkeys.list(alice).expect("list")[0].name, format!("{CANARY} name"));
    shown.push(format!("{:?}", f.passkeys.list(alice).expect("list")));

    // Failures of every kind.
    let (_o, token) = f.passkeys.start_registration(&subject, ADDR).expect("starts");
    shown.push(format!("{:?}", f.passkeys.finish_registration(token.expose(), &response, None, CANARY, ADDR)));
    shown.push(format!("{:?}", f.passkeys.finish_registration(CANARY, CANARY, Some(CANARY), CANARY, ADDR)));
    shown.push(format!("{:?}", f.passkeys.finish_sign_in(CANARY, CANARY, Some(CANARY), ADDR, CANARY)));
    let (_o, token) = f.start_sign_in();
    shown.push(format!("{:?}", f.passkeys.finish_sign_in(token.expose(), CANARY, Some(CANARY), ADDR, CANARY)));
    shown.push(format!("{:?}", f.passkeys.remove(alice, 99, Some(ADDR))));
    shown.push(format!("{:?}", f.passkeys.remove(alice, 1, Some(ADDR))));
    let mut session = f.session(alice);
    let (_o, token) = f.passkeys.start_step_up(&session).expect("starts");
    shown.push(format!("{:?}", f.passkeys.finish_step_up(token.expose(), CANARY, &mut session, ADDR)));
    shown.push(format!("{:?}", f.passkeys));
    let signed_in = f.sign_in(&mut key, Some(CANARY)).expect("signs in");
    shown.push(format!("{signed_in:?}"));
    shown.push(format!("{}", PasskeyError::Invalid));

    for text in &shown {
        assert!(!text.contains(CANARY), "the canary is in: {text}");
    }
    let audit = std::fs::read_dir(f.scratch.path().join("server/audit"))
        .expect("audit dir")
        .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("read"))
        .collect::<String>();
    assert!(!audit.is_empty() && !audit.contains(CANARY), "the audit file has no name");
}
