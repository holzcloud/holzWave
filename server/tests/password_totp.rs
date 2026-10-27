//! Passwords and one-time codes: the split derivation, the codes and the window they are accepted in.

#[path = "support/scratch.rs"]
mod scratch;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use holzwave_server::auth::password::{
    ArgonGate, MAX_PASSWORD_BYTES, MAX_WAITING, PasswordError, PolicyViolation, StoredPassword, Verified, check_policy,
    derive_split, enrol_password, lookup_password, new_salt, params_text, parse_params, prepare_password, store_prepared,
    verify_password,
};
use holzwave_server::auth::totp::{self, TotpError};
use holzwave_server::keyfile::ServerKey;
use holzwave_server::sealed::Sealer;
use holzwave_server::store::{AccountId, AccountState, NewAccount, PasswordCredential, Role, Store};
use holzwave_server::unlock::{Kdf, SALT_BYTES, argon2id_into};
use scratch::Scratch;

const NOW: i64 = 1_772_353_800;
const RANGE: std::ops::RangeInclusive<u32> = 20000..=29999;
const PASSWORD: &str = "correct horse battery";

fn sealer() -> Sealer {
    Sealer::new(&ServerKey::from_bytes([7u8; 32]).expect("a key that is not all zero"))
}

fn account(store: &Store, name: &str) -> AccountId {
    store.write(|tx| tx.account_create(&NewAccount::new(name, Role::Member), RANGE, NOW)).expect("account").id
}

fn gate() -> ArgonGate {
    ArgonGate::new(2, Kdf::lower_bound())
}

fn store_in(scratch: &Scratch) -> Store {
    Store::open(scratch.path()).expect("open")
}

async fn sign_in(gate: &ArgonGate, store: &Store, name: &str, password: &str) -> Result<Verified, PasswordError> {
    let stored = store.read(|tx| lookup_password(tx, name)).expect("lookup");
    verify_password(gate, stored, password).await
}

fn hex8(bytes: &[u8]) -> String {
    bytes.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn tracer_a_password_and_a_code_sign_one_account_in() {
    let scratch = Scratch::new("pw-tracer");
    let store = store_in(&scratch);
    let sealer = sealer();
    let gate = gate();
    let alice = account(&store, "alice");

    let enrolled = enrol_password(&gate, &store, alice, PASSWORD, NOW).await.expect("enrol");
    let row = store.read(|tx| tx.password_get(alice)).expect("read").expect("a row");
    println!("Row: {}", row.params);
    assert_eq!(row.verifier.len(), 32);
    assert_eq!(row.salt.len(), 16);

    let verified = sign_in(&gate, &store, "alice", PASSWORD).await.expect("verify");
    assert_eq!(verified.account, alice);
    let keys_equal = enrolled.same_as(&verified.kek);
    println!("Tracer: key equal = {keys_equal}");
    assert!(keys_equal);
    let wrong = sign_in(&gate, &store, "alice", "wrong horse battery").await;
    assert_eq!(wrong.expect_err("wrong password"), PasswordError::InvalidCredentials);

    let secret = totp::generate_secret().expect("secret");
    assert_eq!(secret.len(), 20);
    let sealed = sealer.seal(&totp::seal_context(alice), &secret[..]).expect("seal");
    store.write(|tx| tx.totp_set(alice, &sealed)).expect("store the secret");
    let code = totp::code_at(&secret[..], NOW).expect("code");
    store.write(|tx| Ok(totp::verify_and_consume(tx, &sealer, alice, &code, NOW))).expect("write").expect("first use");
    let second = store.write(|tx| Ok(totp::verify_and_consume(tx, &sealer, alice, &code, NOW))).expect("write");
    println!("Tracer: second use = {second:?}");
    assert_eq!(second.expect_err("replay"), TotpError::InvalidCode);
}

// ---- the password ----

#[tokio::test]
async fn dummy_path_costs_one_derivation() {
    let scratch = Scratch::new("pw-dummy");
    let store = store_in(&scratch);
    let gate = gate();
    let alice = account(&store, "alice");
    let _carol = account(&store, "carol");
    let dave = account(&store, "dave");
    enrol_password(&gate, &store, alice, PASSWORD, NOW).await.expect("alice");
    enrol_password(&gate, &store, dave, PASSWORD, NOW).await.expect("dave");
    store.write(|tx| tx.account_set_state(dave, AccountState::Disabled)).expect("disable dave");

    let mut counts = Vec::new();
    let mut errors = Vec::new();
    for (name, password) in [
        ("alice", "wrong horse battery"),
        ("nobody", PASSWORD),
        ("carol", PASSWORD),
        ("dave", PASSWORD),
    ] {
        let before = gate.derivations();
        let outcome = sign_in(&gate, &store, name, password).await;
        counts.push(gate.derivations() - before);
        errors.push(outcome.expect_err("no sign-in"));
    }
    println!("Dummy path: wrong password {}, unknown name {}, no password {}, disabled {}", counts[0], counts[1], counts[2], counts[3]);
    assert_eq!(counts, vec![1, 1, 1, 1]);
    assert!(errors.iter().all(|e| *e == PasswordError::InvalidCredentials), "{errors:?}");
    let texts: HashSet<String> = errors.iter().map(|e| e.to_string()).collect();
    assert_eq!(texts.len(), 1);
}

async fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..2_000 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("gave up waiting for {what}");
}

#[tokio::test]
async fn gate_limits_concurrency() {
    let release = Arc::new(AtomicBool::new(false));
    let hold = release.clone();
    let hook: holzwave_server::auth::password::Hook = Arc::new(move || {
        while !hold.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let gate = ArgonGate::new(2, Kdf::lower_bound()).with_hook(hook);
    assert_eq!((gate.permits(), gate.max_waiting()), (2, MAX_WAITING));
    assert_eq!(gate.max_waiting(), 32);
    let kdf = gate.kdf();

    let mut tasks = Vec::new();
    for i in 0..34u8 {
        let gate = gate.clone();
        tasks.push(tokio::spawn(async move { gate.derive("password number one", [i; SALT_BYTES], kdf).await }));
    }
    wait_for("two running and 32 waiting", || gate.derivations() == 2 && gate.waiting() == 32).await;
    assert_eq!(gate.available(), 0);

    // The 35th caller does not queue.
    let late = gate.derive("password number one", [99; SALT_BYTES], kdf).await;
    println!("Gate: permits 2, waiters cap 32, 35th caller = {:?}", late.as_ref().map(|_| ()));
    assert_eq!(late.expect_err("the 35th"), PasswordError::Busy);
    assert_eq!(gate.waiting(), 32, "a refused caller takes no place");

    // Callers that are dropped while they wait give their places back.
    for task in tasks.drain(24..) {
        task.abort();
        assert!(task.await.expect_err("aborted").is_cancelled());
    }
    wait_for("22 waiting", || gate.waiting() == 22).await;
    let gate2 = gate.clone();
    tasks.push(tokio::spawn(async move { gate2.derive("password number one", [100; SALT_BYTES], kdf).await }));
    wait_for("23 waiting", || gate.waiting() == 23).await;

    release.store(true, Ordering::SeqCst);
    for task in tasks {
        task.await.expect("joined").expect("every waiter completes");
    }
    assert_eq!(gate.waiting(), 0);
    assert_eq!(gate.available(), 2, "no permit was lost");
    assert_eq!(gate.derivations(), 25);
}

#[tokio::test]
async fn policy() {
    let name = "administrator";
    let cases: [(String, Result<(), PolicyViolation>); 11] = [
        ("a1b2c3d4e5f".to_owned(), Err(PolicyViolation::TooShort)),
        ("a1b2c3d4e5f6".to_owned(), Ok(())),
        ("ab".repeat(128), Ok(())),
        (format!("{}c", "ab".repeat(128)), Err(PolicyViolation::TooLong)),
        ("Administrator".to_owned(), Err(PolicyViolation::SameAsUsername)),
        ("ADMINISTRATOR".to_owned(), Err(PolicyViolation::SameAsUsername)),
        ("aaaaaaaaaaaa".to_owned(), Err(PolicyViolation::Repeated)),
        (String::new(), Err(PolicyViolation::TooShort)),
        ("a".repeat(1 << 20), Err(PolicyViolation::TooLong)),
        ("äöüäöüäöüäöü".to_owned(), Ok(())),
        ("日本語日本語日本語日本語".to_owned(), Ok(())),
    ];
    for (password, expected) in &cases {
        assert_eq!(check_policy(name, password), *expected, "{} bytes", password.len());
    }
    assert_eq!("äöüäöüäöüäöü".chars().count(), 12);
    assert_eq!(PolicyViolation::TooShort.as_str(), "too_short");
    assert_eq!(PolicyViolation::SameAsUsername.as_str(), "same_as_username");
    assert_eq!(PolicyViolation::Repeated.as_str(), "repeated");

    // A refusal costs no derivation and names the rule as a value.
    let gate = gate();
    for (password, expected) in cases.iter().filter(|(_, e)| e.is_err()) {
        let refused = prepare_password(&gate, name, password).await.expect_err("refused");
        assert_eq!(refused, PasswordError::Policy(expected.expect_err("a violation")));
    }
    assert_eq!(gate.derivations(), 0);
    let ok = prepare_password(&gate, name, "äöüäöüäöüäöü").await.expect("accepted");
    assert_eq!(gate.derivations(), 1);
    drop(ok);
}

#[tokio::test]
async fn bad_input_costs_nothing_at_sign_in() {
    let scratch = Scratch::new("pw-input");
    let store = store_in(&scratch);
    let gate = gate();
    let alice = account(&store, "alice");
    enrol_password(&gate, &store, alice, PASSWORD, NOW).await.expect("enrol");
    let before = gate.derivations();
    for password in [String::new(), "x".repeat(1 << 20), "x".repeat(MAX_PASSWORD_BYTES + 1)] {
        let outcome = sign_in(&gate, &store, "alice", &password).await;
        assert_eq!(outcome.expect_err("refused"), PasswordError::InvalidCredentials);
    }
    assert_eq!(gate.derivations(), before);
    // A name that is not a username never reaches the database.
    assert!(store.read(|tx| lookup_password(tx, "Alice With Space")).expect("lookup").is_none());
}

async fn corrupt_row(tag: &str, params: &str, salt_len: usize, verifier_len: usize) -> (PasswordError, u64) {
    let scratch = Scratch::new(tag);
    let store = store_in(&scratch);
    let gate = gate();
    let erin = account(&store, "erin");
    store
        .write(|tx| {
            tx.password_set(
                erin,
                &PasswordCredential {
                    verifier: vec![1; verifier_len],
                    salt: vec![2; salt_len],
                    params: params.to_owned(),
                    changed_at: NOW,
                },
            )
        })
        .expect("row");
    let outcome = sign_in(&gate, &store, "erin", PASSWORD).await;
    (outcome.expect_err("refused"), gate.derivations())
}

#[tokio::test]
async fn params_bounds() {
    let texts = [
        ("memory 0", r#"{"alg":"argon2id","v":19,"m":0,"t":3,"p":1}"#),
        ("memory 2 GiB", r#"{"alg":"argon2id","v":19,"m":2097152,"t":3,"p":1}"#),
        ("memory below the floor", r#"{"alg":"argon2id","v":19,"m":1024,"t":3,"p":1}"#),
        ("passes 0", r#"{"alg":"argon2id","v":19,"m":65536,"t":0,"p":1}"#),
        ("passes 11", r#"{"alg":"argon2id","v":19,"m":65536,"t":11,"p":1}"#),
        ("lanes 99", r#"{"alg":"argon2id","v":19,"m":65536,"t":3,"p":99}"#),
        ("lanes 0", r#"{"alg":"argon2id","v":19,"m":65536,"t":3,"p":0}"#),
        ("a missing member", r#"{"alg":"argon2id","v":19,"m":65536,"t":3}"#),
        ("an unknown algorithm", r#"{"alg":"argon2i","v":19,"m":65536,"t":3,"p":1}"#),
        ("an unknown member", r#"{"alg":"argon2id","v":19,"m":65536,"t":3,"p":1,"x":1}"#),
        ("another version", r#"{"alg":"argon2id","v":16,"m":65536,"t":3,"p":1}"#),
        ("a negative number", r#"{"alg":"argon2id","v":19,"m":-1,"t":3,"p":1}"#),
        ("text that is not JSON", "argon2id"),
        ("an empty text", ""),
    ];
    for (what, text) in texts {
        assert_eq!(parse_params(text).expect_err(what), PasswordError::CorruptCredential, "{what}");
        let (error, derivations) = corrupt_row("pw-params", text, 16, 32).await;
        assert_eq!(error, PasswordError::CorruptCredential, "{what}");
        assert_eq!(derivations, 0, "{what} reached Argon2");
    }
    // The text that the module itself writes is read back, bounds included.
    for kdf in [Kdf::default(), Kdf::lower_bound(), Kdf::new(256 * 1024, 10, 4).expect("the top of the bounds")] {
        assert_eq!(parse_params(&params_text(&kdf)).expect("round trip"), kdf);
    }
}

#[tokio::test]
async fn a_row_with_a_wrong_length_is_corrupt_before_any_work() {
    let good = params_text(&Kdf::lower_bound());
    for (salt, verifier) in [(15, 32), (17, 32), (16, 31), (16, 33), (1, 32), (16, 1)] {
        let (error, derivations) = corrupt_row("pw-lengths", &good, salt, verifier).await;
        assert_eq!(error, PasswordError::CorruptCredential, "salt {salt}, verifier {verifier}");
        assert_eq!(derivations, 0);
    }
}

#[test]
fn known_answer() {
    let salt: [u8; SALT_BYTES] = std::array::from_fn(|i| u8::try_from(i + 1).expect("small"));
    let kdf = Kdf::default();
    assert_eq!((kdf.m_kib(), kdf.t(), kdf.p()), (64 * 1024, 3, 1));
    let derived = derive_split(b"correct horse battery staple", &salt, &kdf).expect("derive");
    println!("Known answer: verifier {} key {}", hex8(derived.verifier()), hex8(derived.kek().expose()));
    assert_eq!(hex8(derived.verifier()), KNOWN_VERIFIER);
    assert_eq!(hex8(derived.kek().expose()), KNOWN_KEY);
    assert_ne!(derived.verifier(), derived.kek().expose());

    // The halves are the first and the last 32 of one 64-byte pass.
    let mut raw = [0u8; 64];
    argon2id_into(b"correct horse battery staple", &salt, &kdf, &mut raw).expect("raw");
    assert_eq!(&raw[..32], &derived.verifier()[..]);
    assert_eq!(&raw[32..], &derived.kek().expose()[..]);
}

// First 8 bytes of the two halves; the same 64 bytes come out of the reference `argon2` command line tool
// (`argon2 <salt> -id -t 3 -m 16 -p 1 -l 64 -r` over the same password and the salt 01 02 .. 10).
const KNOWN_VERIFIER: &str = "c048543905406a07";
const KNOWN_KEY: &str = "ad9ed2e347d77f34";

#[tokio::test]
async fn prepare_then_store() {
    let scratch = Scratch::new("pw-prepare");
    let store = store_in(&scratch);
    let gate = gate();
    let alice = account(&store, "alice");

    let prepared = prepare_password(&gate, "alice", PASSWORD).await.expect("prepare");
    assert_eq!(gate.derivations(), 1);
    assert!(store.read(|tx| tx.password_get(alice)).expect("read").is_none(), "nothing is stored yet");
    store.write(|tx| store_prepared(tx, alice, &prepared, NOW).map_err(|_| unreachable_store())).expect("store");
    assert_eq!(gate.derivations(), 1, "storing does not derive again");
    let row = store.read(|tx| tx.password_get(alice)).expect("read").expect("a row");
    assert_eq!(row.salt, prepared.salt().to_vec());
    assert_eq!(row.params, prepared.params());
    assert_eq!(row.changed_at, NOW);
    let verified = sign_in(&gate, &store, "alice", PASSWORD).await.expect("verify");
    assert!(prepared.kek().same_as(&verified.kek));

    // enrol_password is the two in sequence.
    let again = enrol_password(&gate, &store, alice, PASSWORD, NOW + 1).await.expect("enrol");
    assert_eq!(gate.derivations(), 3, "one for the enrolment, one for the sign-in above");
    let second = store.read(|tx| tx.password_get(alice)).expect("read").expect("a row");
    assert_ne!(second.salt, row.salt, "a new salt every time");
    assert_ne!(second.verifier, row.verifier);
    assert!(!again.same_as(&verified.kek), "another salt gives another key");
    assert_eq!(second.changed_at, NOW + 1);
}

fn unreachable_store() -> holzwave_server::store::StoreError {
    holzwave_server::store::StoreError::Invalid("a prepared credential is always accepted")
}

#[tokio::test]
async fn enrolling_again_replaces_the_password() {
    let scratch = Scratch::new("pw-replace");
    let store = store_in(&scratch);
    let gate = gate();
    let alice = account(&store, "alice");
    enrol_password(&gate, &store, alice, PASSWORD, NOW).await.expect("first");
    enrol_password(&gate, &store, alice, "another long password", NOW).await.expect("second");
    assert_eq!(sign_in(&gate, &store, "alice", PASSWORD).await.expect_err("old"), PasswordError::InvalidCredentials);
    sign_in(&gate, &store, "alice", "another long password").await.expect("new");
    // A deleted account cannot be given a password.
    let bob = account(&store, "bob");
    store.write(|tx| tx.account_delete(bob)).expect("delete");
    assert!(enrol_password(&gate, &store, bob, PASSWORD, NOW).await.is_err());
    assert!(store.read(|tx| lookup_password(tx, "bob")).expect("lookup").is_none());
}

#[test]
fn salts_are_unique() {
    let mut seen = HashSet::new();
    for _ in 0..1_000 {
        assert!(seen.insert(new_salt().expect("salt")), "a salt repeated");
    }
}

#[tokio::test]
async fn enrolments_do_not_repeat_a_salt() {
    let scratch = Scratch::new("pw-salts");
    let store = store_in(&scratch);
    let gate = gate();
    let alice = account(&store, "alice");
    let mut seen = HashSet::new();
    for _ in 0..8 {
        enrol_password(&gate, &store, alice, PASSWORD, NOW).await.expect("enrol");
        let row = store.read(|tx| tx.password_get(alice)).expect("read").expect("row");
        assert!(seen.insert(row.salt), "an enrolment reused a salt");
    }
}

#[tokio::test]
async fn constant_time_compare() {
    // The comparison goes through `subtle`: a source check, and a verifier of the right length that differs in one bit
    // (in the first byte and in the last) is refused like any other.
    let source = include_str!("../src/auth/password.rs");
    assert!(source.contains("ct_eq"));
    let scratch = Scratch::new("pw-ct");
    let store = store_in(&scratch);
    let gate = gate();
    let alice = account(&store, "alice");
    enrol_password(&gate, &store, alice, PASSWORD, NOW).await.expect("enrol");
    let row = store.read(|tx| tx.password_get(alice)).expect("read").expect("row");
    for index in [0usize, 31] {
        let mut changed = row.clone();
        changed.verifier[index] ^= 1;
        store.write(|tx| tx.password_set(alice, &changed)).expect("tamper");
        let outcome = sign_in(&gate, &store, "alice", PASSWORD).await;
        assert_eq!(outcome.expect_err("one bit off"), PasswordError::InvalidCredentials);
    }
    store.write(|tx| tx.password_set(alice, &row)).expect("restore");
    sign_in(&gate, &store, "alice", PASSWORD).await.expect("the original works");
}

#[tokio::test]
async fn debug_is_redacted() {
    let scratch = Scratch::new("pw-debug");
    let store = store_in(&scratch);
    let gate = gate();
    let alice = account(&store, "alice");
    let prepared = prepare_password(&gate, "alice", PASSWORD).await.expect("prepare");
    let salt = [4u8; SALT_BYTES];
    let derived = derive_split(PASSWORD.as_bytes(), &salt, &Kdf::lower_bound()).expect("derive");
    let verifier_text = format!("{:?}", derived.verifier());
    let key_text = format!("{:?}", derived.kek().expose());

    store.write(|tx| store_prepared(tx, alice, &prepared, NOW).map_err(|_| unreachable_store())).expect("store");
    let stored: Option<StoredPassword> = store.read(|tx| lookup_password(tx, "alice")).expect("lookup");
    let verified = sign_in(&gate, &store, "alice", PASSWORD).await.expect("verify");
    let errors = [
        PasswordError::InvalidCredentials,
        PasswordError::Policy(PolicyViolation::Repeated),
        PasswordError::Busy,
        PasswordError::CorruptCredential,
        PasswordError::Derive,
    ];
    let mut shown = vec![
        format!("{derived:?}"),
        format!("{prepared:?}"),
        format!("{verified:?}"),
        format!("{stored:?}"),
        format!("{gate:?}"),
        format!("{:?}", derived.kek()),
    ];
    for e in &errors {
        shown.push(format!("{e:?}"));
        shown.push(format!("{e}"));
    }
    for text in &shown {
        assert!(!text.contains("correct horse"), "{text}");
        assert!(!text.contains(&verifier_text) && !text.contains(&key_text), "{text}");
    }
    assert_eq!(format!("{derived:?}"), "Derived(..)");
    assert_eq!(format!("{prepared:?}"), "Prepared(..)");
    assert_eq!(format!("{:?}", derived.kek()), "Kek(..)");
}
