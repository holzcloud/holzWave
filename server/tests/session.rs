//! Sessions: the cookie value, the lifetimes and what ends a session.

#[path = "support/scratch.rs"]
mod scratch;

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use holzwave_server::auth::session::{
    MAX_SESSIONS_PER_ACCOUNT, Method, SessionError, SessionManager, generate_token, parse_token,
};
use holzwave_server::clock::ManualClock;
use holzwave_server::config::Sessions;
use holzwave_server::store::{AccountId, AccountState, NewAccount, Role, Session, Store, hash_token};
use scratch::Scratch;

const NOW: i64 = 1_772_353_800;
const RANGE: std::ops::RangeInclusive<u32> = 20000..=29999;
const ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));
const IDLE: i64 = 1_800;
const ABSOLUTE: i64 = 86_400;

struct Fixture {
    _scratch: Scratch,
    store: Arc<Store>,
    clock: Arc<ManualClock>,
    manager: Arc<SessionManager>,
}

fn fixture(tag: &str) -> (Fixture, AccountId) {
    let scratch = Scratch::new(tag);
    let store = Arc::new(Store::open(scratch.path()).expect("open"));
    let clock = Arc::new(ManualClock::new(NOW));
    let manager = Arc::new(SessionManager::new(store.clone(), clock.clone(), Sessions::default()));
    let alice = account(&store, "alice");
    (Fixture { _scratch: scratch, store, clock, manager }, alice)
}

fn account(store: &Store, name: &str) -> AccountId {
    store.write(|tx| tx.account_create(&NewAccount::new(name, Role::Member), RANGE, NOW)).expect("account").id
}

fn make(f: &Fixture, account: AccountId, now: i64) -> (String, Session) {
    let created = f.manager.create(account, Method::PasswordTotp, now, "Firefox on Linux", Some(ADDR)).expect("create");
    (created.value.expose().to_owned(), created.session)
}

fn rows(f: &Fixture, account: AccountId) -> usize {
    f.store.read(|tx| tx.session_list(account)).expect("list").len()
}

#[test]
fn tracer_a_session_is_found_until_it_has_been_idle_too_long() {
    let (f, alice) = fixture("session-tracer");
    let created = f.manager.create(alice, Method::PasswordTotp, NOW, "Firefox on Linux", Some(ADDR)).expect("create");
    let value = created.value.expose().to_owned();
    println!("Tracer: value length = {}", value.len());
    assert_eq!(value.len(), 43);
    let found = f.manager.lookup(&value, NOW).expect("lookup");
    assert_eq!(found.account_id, alice);
    assert_eq!(rows(&f, alice), 1);
    f.clock.advance(1_801);
    let late = f.manager.lookup(&value, f.manager.now());
    println!("Tracer: after the idle time = {late:?}");
    assert_eq!(late.expect_err("idle"), SessionError::Expired);
}

#[test]
fn token_shape() {
    let (f, alice) = fixture("session-shape");
    let mut seen = HashSet::new();
    for _ in 0..1_000 {
        let (token, bytes) = generate_token().expect("token");
        let text = token.expose();
        assert_eq!(text.len(), 43);
        assert!(text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "{text}");
        assert!(seen.insert(text.to_owned()), "a value repeated");
        assert_eq!(URL_SAFE_NO_PAD.decode(text).expect("base64url"), bytes.to_vec());
        assert_eq!(parse_token(text).expect("parse"), hash_token(&bytes[..]));
    }

    // The stored id is the hash of the 32 decoded bytes, not the value.
    let (value, session) = make(&f, alice, NOW);
    let decoded = URL_SAFE_NO_PAD.decode(&value).expect("decode");
    assert_eq!(decoded.len(), 32);
    assert_eq!(session.id_hash, hash_token(&decoded));
    assert_ne!(&session.id_hash[..], value.as_bytes());
    assert_ne!(&session.id_hash[..], &decoded[..]);
    assert!(f.store.read(|tx| tx.session_get(&session.id_hash)).expect("get").is_some());

    // The one canonical spelling of a value: a last character with stray bits is not another name for the session.
    let last = value.chars().last().expect("last");
    let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let index = alphabet.find(last).expect("in the alphabet");
    let other = alphabet.chars().nth(index ^ 1).expect("neighbour");
    let twin = format!("{}{other}", &value[..42]);
    assert_eq!(f.manager.lookup(&twin, NOW).expect_err("non-canonical"), SessionError::Unknown);
    f.manager.lookup(&value, NOW).expect("the real one");
}

#[test]
fn a_malformed_value_does_not_touch_the_store() {
    let (f, alice) = fixture("session-malformed");
    let (value, _) = make(&f, alice, NOW);
    let unknown = URL_SAFE_NO_PAD.encode([9u8; 32]);
    let bad: Vec<String> = vec![
        value[..42].to_owned(),
        format!("{value}A"),
        format!("{}=", &value[..42]),
        format!("{}+", &value[..42]),
        format!("{}/", &value[..42]),
        format!("{} ", &value[..42]),
        format!("{}é", &value[..41]),
        format!("{}\u{0661}", &value[..41]),
        String::new(),
        "A".repeat(1 << 16),
    ];

    // While a write transaction is open on this thread nobody else can use the connection. A lookup on another thread that
    // needs the store has to wait for it; one that does not, answers at once.
    let (send, receive) = mpsc::channel();
    let outcome = f
        .store
        .write(|_tx| {
            for text in &bad {
                let manager = f.manager.clone();
                let sent = text.clone();
                let send = send.clone();
                std::thread::spawn(move || {
                    let _ = send.send(manager.lookup(&sent, NOW));
                });
                let answer = receive.recv_timeout(Duration::from_secs(5)).expect("answered without the store");
                assert_eq!(answer.expect_err("malformed"), SessionError::Unknown, "{:?}", &text[..text.len().min(50)]);
            }
            // The control: a well-formed value that nobody has needs the store, and waits.
            let manager = f.manager.clone();
            let send = send.clone();
            let unknown = unknown.clone();
            std::thread::spawn(move || {
                let _ = send.send(manager.lookup(&unknown, NOW));
            });
            let waited = receive.recv_timeout(Duration::from_millis(300));
            assert!(waited.is_err(), "a well-formed value should have waited for the store");
            Ok(())
        })
        .map(|()| receive.recv_timeout(Duration::from_secs(5)));
    let after = outcome.expect("write").expect("the waiting lookup answered once the store was free");
    assert_eq!(after.expect_err("unknown"), SessionError::Unknown);
}

#[test]
fn lifetimes() {
    let (f, alice) = fixture("session-lifetimes");

    // Idle time: valid at the last second, refused one second later, and the row goes.
    let (value, _) = make(&f, alice, NOW);
    f.manager.lookup(&value, NOW + IDLE).expect("valid at created + 1,800");
    assert_eq!(f.manager.lookup(&value, NOW + IDLE + 1).expect_err("1,801"), SessionError::Expired);
    println!("Boundaries: idle +1800 valid, +1801 {:?}", SessionError::Expired);
    assert_eq!(rows(&f, alice), 0, "the expiry removed the row");
    assert_eq!(f.manager.lookup(&value, NOW).expect_err("gone"), SessionError::Unknown);

    // A touch moves the idle window.
    let (value, mut session) = make(&f, alice, NOW);
    assert!(f.manager.touch(&mut session, NOW + 1_000).expect("touch"));
    f.manager.lookup(&value, NOW + 1_000 + IDLE).expect("valid at the touch + 1,800");
    assert_eq!(f.manager.lookup(&value, NOW + 1_000 + IDLE + 1).expect_err("one later"), SessionError::Expired);

    // Absolute time: however fresh the touches, the end is the end.
    let (value, mut session) = make(&f, alice, NOW);
    let mut t = NOW;
    while t + 1_000 <= NOW + ABSOLUTE - 400 {
        t += 1_000;
        let mut current = f.manager.lookup(&value, t).expect("alive while busy");
        assert!(f.manager.touch(&mut current, t).expect("touch"));
        session = current;
    }
    assert_eq!(session.last_seen, t);
    f.manager.lookup(&value, NOW + ABSOLUTE).expect("valid at created + 86,400");
    assert_eq!(f.manager.lookup(&value, NOW + ABSOLUTE + 1).expect_err("86,401"), SessionError::Expired);
    println!("Boundaries: absolute +86400 valid, +86401 Expired");
    assert_eq!(rows(&f, alice), 0);

    // A lookup that is not past either limit changes nothing.
    let (value, session) = make(&f, alice, NOW);
    f.manager.lookup(&value, NOW + 10).expect("lookup");
    let stored = f.store.read(|tx| tx.session_get(&session.id_hash)).expect("get").expect("row");
    assert_eq!(stored, session, "a lookup is a read");
}

#[test]
fn lifetimes_come_from_the_configuration() {
    let (f, alice) = fixture("session-config");
    let short = Sessions {
        idle: Duration::from_secs(60),
        absolute: Duration::from_secs(600),
        step_up: Duration::from_secs(30),
    };
    let manager = SessionManager::new(f.store.clone(), f.clock.clone(), short);
    let created = manager.create(alice, Method::Passkey, NOW, "Safari", None).expect("create");
    assert_eq!(created.session.absolute_expires, NOW + 600);
    assert_eq!(created.session.addr, "0.0.0.0", "no address is the unspecified one");
    let value = created.value.expose().to_owned();
    manager.lookup(&value, NOW + 60).expect("valid");
    assert_eq!(manager.lookup(&value, NOW + 61).expect_err("idle"), SessionError::Expired);
    let fresh = manager.create(alice, Method::Passkey, NOW, "Safari", Some(ADDR)).expect("create");
    assert!(manager.step_up_fresh(&fresh.session, NOW + 30));
    assert!(!manager.step_up_fresh(&fresh.session, NOW + 31));
}

#[test]
fn touch_rate() {
    let (f, alice) = fixture("session-touch");
    let (value, mut session) = make(&f, alice, NOW);
    let stored = |f: &Fixture, s: &Session| f.store.read(|tx| tx.session_get(&s.id_hash)).expect("get").expect("row").last_seen;

    assert!(!f.manager.touch(&mut session, NOW + 10).expect("10 s"), "10 s after creation is no write");
    assert_eq!(stored(&f, &session), NOW);
    assert!(f.manager.touch(&mut session, NOW + 30).expect("30 s"), "30 s writes");
    assert_eq!(stored(&f, &session), NOW + 30);
    assert!(!f.manager.touch(&mut session, NOW + 40).expect("10 s later"), "two touches 10 s apart write once");
    assert_eq!(stored(&f, &session), NOW + 30);
    assert!(f.manager.touch(&mut session, NOW + 60).expect("30 s later"), "two touches 30 s apart write twice");
    assert_eq!(stored(&f, &session), NOW + 60);
    assert_eq!(session.last_seen, NOW + 60);

    // The time never moves backward, and a session that has gone cannot be touched.
    assert!(!f.manager.touch(&mut session, NOW).expect("older time"));
    f.manager.end(&session).expect("end");
    assert_eq!(f.manager.touch(&mut session, NOW + 200).expect_err("gone"), SessionError::Unknown);
    assert_eq!(f.manager.lookup(&value, NOW + 200).expect_err("gone"), SessionError::Unknown);
}

#[test]
fn step_up() {
    let (f, alice) = fixture("session-stepup");
    let (_, mut session) = make(&f, alice, NOW);
    assert!(f.manager.step_up_fresh(&session, NOW));
    assert!(f.manager.step_up_fresh(&session, NOW + 300), "fresh at last_auth + 300");
    assert!(!f.manager.step_up_fresh(&session, NOW + 301), "not fresh at + 301");
    println!("Boundaries: step-up +300 fresh, +301 not fresh");

    f.manager.reauth(&mut session, NOW + 301).expect("reauth");
    assert!(f.manager.step_up_fresh(&session, NOW + 301));
    assert!(f.manager.step_up_fresh(&session, NOW + 301 + 300));
    assert!(!f.manager.step_up_fresh(&session, NOW + 301 + 301));
    let stored = f.store.read(|tx| tx.session_get(&session.id_hash)).expect("get").expect("row");
    assert_eq!(stored.last_auth, NOW + 301, "the row moved");

    // A reauth with an older time does not make the session less fresh.
    f.manager.reauth(&mut session, NOW).expect("older");
    assert_eq!(session.last_auth, NOW + 301);

    // Every method makes a fresh session.
    let oidc = f.manager.create(alice, Method::Oidc, NOW + 5_000, "Chrome", Some(ADDR)).expect("oidc");
    assert!(f.manager.step_up_fresh(&oidc.session, NOW + 5_000));
    assert_eq!(oidc.session.method, Method::Oidc);

    // A session that is gone cannot be re-authenticated.
    f.manager.end(&session).expect("end");
    assert_eq!(f.manager.reauth(&mut session, NOW + 400).expect_err("gone"), SessionError::Unknown);
}

#[test]
fn account_state() {
    let (f, alice) = fixture("session-state");
    let bob = account(&f.store, "bob");
    let (alice_value, _) = make(&f, alice, NOW);
    let (bob_value, _) = make(&f, bob, NOW);

    // Disabled: the next lookup refuses and the row goes; enabling later does not bring it back.
    f.store.write(|tx| tx.account_set_state(alice, AccountState::Disabled)).expect("disable");
    assert_eq!(rows(&f, alice), 1);
    assert_eq!(f.manager.lookup(&alice_value, NOW + 1).expect_err("disabled"), SessionError::AccountDisabled);
    assert_eq!(rows(&f, alice), 0, "that session's row was deleted");
    assert_eq!(f.manager.lookup(&alice_value, NOW + 1).expect_err("again"), SessionError::Unknown);
    f.store.write(|tx| tx.account_set_state(alice, AccountState::Active)).expect("enable");
    assert_eq!(f.manager.lookup(&alice_value, NOW + 2).expect_err("not back"), SessionError::Unknown);

    // A disabled account cannot get a new session.
    f.store.write(|tx| tx.account_set_state(alice, AccountState::Disabled)).expect("disable");
    let refused = f.manager.create(alice, Method::Oidc, NOW, "x", None);
    assert_eq!(refused.expect_err("disabled").to_string(), SessionError::AccountDisabled.to_string());
    f.store.write(|tx| tx.account_set_state(alice, AccountState::Active)).expect("enable");

    // Deleted: the sessions go with the account, and nothing is found afterwards.
    f.manager.lookup(&bob_value, NOW + 2).expect("bob works");
    f.store.write(|tx| tx.account_delete(bob)).expect("delete");
    assert_eq!(rows(&f, bob), 0);
    let gone = f.manager.lookup(&bob_value, NOW + 3).expect_err("deleted");
    assert!(matches!(gone, SessionError::Unknown | SessionError::AccountDisabled), "{gone:?}");
    assert!(matches!(f.manager.create(bob, Method::Passkey, NOW, "x", None), Err(SessionError::AccountDisabled)));
}

#[test]
fn fixation() {
    let (f, alice) = fixture("session-fixation");
    let (first, a) = make(&f, alice, NOW);
    let (second, b) = make(&f, alice, NOW);
    assert_ne!(first, second, "every sign-in makes a new value");
    assert_ne!(a.id_hash, b.id_hash);
    assert_eq!(rows(&f, alice), 2);

    // A value a browser brings is not adopted: a well-formed value that nobody issued is simply unknown, and signing in
    // makes a different one.
    let planted = URL_SAFE_NO_PAD.encode([42u8; 32]);
    assert_eq!(f.manager.lookup(&planted, NOW).expect_err("planted"), SessionError::Unknown);
    let (made, _) = make(&f, alice, NOW);
    assert_ne!(made, planted);
    assert_eq!(f.manager.lookup(&planted, NOW).expect_err("still unknown"), SessionError::Unknown);

    // In the source: one public function makes a session, and its inputs hold no cookie value.
    let source = include_str!("../src/auth/session.rs");
    assert_eq!(source.matches("pub fn create").count(), 1);
    let start = source.find("pub fn create(").expect("create");
    let signature = &source[start..source[start..].find(") -> ").map(|i| start + i).expect("end of signature")];
    assert!(!signature.contains("value") && !signature.contains("token") && !signature.contains("cookie"), "{signature}");
    for forbidden in ["fn adopt", "fn create_from", "fn with_value", "fn from_value", "fn resume"] {
        assert!(!source.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn eviction() {
    let (f, alice) = fixture("session-evict");
    let bob = account(&f.store, "bob");
    let (bob_value, _) = make(&f, bob, NOW);
    let mut made: Vec<(String, Session)> = Vec::new();
    for i in 0..i64::try_from(MAX_SESSIONS_PER_ACCOUNT).expect("small") {
        made.push(make(&f, alice, NOW + i));
    }
    assert_eq!(MAX_SESSIONS_PER_ACCOUNT, 20);
    assert_eq!(rows(&f, alice), 20);
    // The oldest was seen again, so the second oldest is the one seen least recently.
    assert!(f.manager.touch(&mut made[0].1, NOW + 100).expect("touch"));

    let (newest, _) = make(&f, alice, NOW + 200);
    assert_eq!(rows(&f, alice), 20, "still the cap");
    assert_eq!(f.manager.lookup(&made[1].0, NOW + 200).expect_err("evicted"), SessionError::Unknown);
    f.manager.lookup(&made[0].0, NOW + 200).expect("the touched one stays");
    for (value, _) in made.iter().skip(2) {
        f.manager.lookup(value, NOW + 200).expect("the others stay");
    }
    f.manager.lookup(&newest, NOW + 200).expect("the new one");
    f.manager.lookup(&bob_value, NOW + 200).expect("another account is never touched");
    assert_eq!(rows(&f, bob), 1);

    // A twenty-second takes the next least recently seen.
    let (_, _) = make(&f, alice, NOW + 210);
    assert_eq!(f.manager.lookup(&made[2].0, NOW + 210).expect_err("next"), SessionError::Unknown);
    assert_eq!(rows(&f, alice), 20);
}

#[test]
fn end_and_purge() {
    let (f, alice) = fixture("session-end");
    let bob = account(&f.store, "bob");

    // end: one session, and only that one.
    let (first, s1) = make(&f, alice, NOW);
    let (second, _) = make(&f, alice, NOW);
    assert!(f.manager.end(&s1).expect("end"));
    assert!(!f.manager.end(&s1).expect("end again"), "ending twice is no error and no second removal");
    assert_eq!(f.manager.lookup(&first, NOW).expect_err("ended"), SessionError::Unknown);
    f.manager.lookup(&second, NOW).expect("the other stays");

    // end_all: every session of the account and no other.
    make(&f, alice, NOW);
    let (bob_value, _) = make(&f, bob, NOW);
    assert_eq!(f.manager.end_all(alice).expect("end all"), 2);
    assert_eq!(rows(&f, alice), 0);
    assert_eq!(f.manager.end_all(alice).expect("none left"), 0);
    f.manager.lookup(&bob_value, NOW).expect("bob is untouched");
    f.manager.end_all(bob).expect("bob");

    // purge: only what is past a limit, at most max_rows per call.
    for _ in 0..5 {
        make(&f, alice, NOW);
    }
    for _ in 0..2 {
        make(&f, alice, NOW + 500);
    }
    assert_eq!(f.manager.purge(NOW + IDLE, 100).expect("purge"), 0, "nothing is past its limit at the last second");
    assert_eq!(rows(&f, alice), 7);
    assert_eq!(f.manager.purge(NOW + IDLE + 1, 3).expect("purge"), 3, "the cap of one call");
    assert_eq!(f.manager.purge(NOW + IDLE + 1, 3).expect("purge"), 2);
    assert_eq!(f.manager.purge(NOW + IDLE + 1, 3).expect("purge"), 0);
    assert_eq!(rows(&f, alice), 2, "the two that were seen later remain");

    // A session past its absolute time goes however recently it was seen.
    f.manager.end_all(alice).expect("clear");
    let (_, mut busy) = make(&f, alice, NOW);
    f.manager.touch(&mut busy, NOW + ABSOLUTE - 10).expect("touch");
    assert_eq!(f.manager.purge(NOW + ABSOLUTE, 100).expect("purge"), 0, "valid at the last second");
    assert_eq!(f.manager.purge(NOW + ABSOLUTE + 1, 100).expect("purge"), 1);
    assert_eq!(rows(&f, alice), 0);
    assert!(f.store.read(|tx| tx.session_get(&busy.id_hash)).expect("get").is_none());
}

#[test]
fn a_device_is_cut_and_cleaned() {
    let (f, alice) = fixture("session-device");
    let long = "Firefox on Linux with a very long description that goes on and on and on and on forever".to_owned();
    let created = f.manager.create(alice, Method::PasswordTotp, NOW, &long, Some(ADDR)).expect("create");
    assert_eq!(created.session.device.chars().count(), 64);
    assert!(long.starts_with(&created.session.device));
    let created = f.manager.create(alice, Method::PasswordTotp, NOW, "Fire\nfox\u{7}", Some(ADDR)).expect("create");
    assert_eq!(created.session.device, "Firefox");
    assert_eq!(created.session.addr, "192.0.2.7");
    let text = format!("{created:?}");
    assert!(!text.contains(created.value.expose()), "the value is not printed");
    assert_eq!(format!("{:?}", created.value), "SessionToken(..)");
}

#[test]
fn method_list() {
    // A match with no wildcard: a fourth value fails to compile here until the list below is changed on purpose.
    fn spelled(method: Method) -> &'static str {
        match method {
            Method::PasswordTotp => "password_totp",
            Method::Passkey => "passkey",
            Method::Oidc => "oidc",
        }
    }
    let all = [Method::PasswordTotp, Method::Passkey, Method::Oidc];
    let strings: Vec<&str> = all.iter().map(|m| m.as_str()).collect();
    println!("Method list: {strings:?}");
    assert_eq!(strings, vec!["password_totp", "passkey", "oidc"]);
    for method in all {
        assert_eq!(method.as_str(), spelled(method));
        assert_eq!(Method::parse(method.as_str()), Some(method));
    }
    for text in ["password", "totp", "PASSWORD_TOTP", "", "oidc ", "passkey2"] {
        assert_eq!(Method::parse(text), None, "{text:?}");
    }

    // The row keeps the spelling.
    let (f, alice) = fixture("session-methods");
    for method in all {
        let (_, session) = make_with(&f, alice, method);
        assert_eq!(session.method, method);
    }
    let listed = f.store.read(|tx| tx.session_list(alice)).expect("list");
    assert_eq!(listed.iter().map(|s| s.method).collect::<Vec<_>>(), all.to_vec());
}

fn make_with(f: &Fixture, account: AccountId, method: Method) -> (String, Session) {
    let created = f.manager.create(account, method, NOW, "x", Some(ADDR)).expect("create");
    (created.value.expose().to_owned(), created.session)
}

fn last_signin(f: &Fixture, account: AccountId) -> Option<i64> {
    f.store.read(|tx| tx.account_last_signin(account)).expect("read the last sign-in")
}

#[test]
fn tracer_the_last_sign_in_is_stamped_when_a_session_is_made_and_by_nothing_else() {
    let (f, alice) = fixture("session-last-signin");
    assert_eq!(last_signin(&f, alice), None, "nobody has signed in yet");

    let (value, mut session) = make(&f, alice, NOW);
    assert_eq!(last_signin(&f, alice), Some(NOW), "stamped by the creation");

    // A touch, a lookup and a step-up are not a sign-in.
    f.clock.advance(60);
    let mut found = f.manager.lookup(&value, NOW + 60).expect("lookup");
    f.manager.touch(&mut found, NOW + 60).expect("touch");
    f.manager.reauth(&mut session, NOW + 120).expect("reauth");
    assert_eq!(last_signin(&f, alice), Some(NOW), "touch, lookup and step-up leave it alone");

    // A second session moves it forward; the stamp is the time of the newest one.
    let (_, second) = make(&f, alice, NOW + 600);
    assert_eq!(last_signin(&f, alice), Some(NOW + 600));
    assert_eq!(second.created_at, NOW + 600);

    // Ending sessions does not clear it.
    f.manager.end_all(alice).expect("end all");
    assert_eq!(last_signin(&f, alice), Some(NOW + 600), "the stamp outlives the sessions");
    println!("Tracer: last_signin None -> {NOW} -> {} (touch, lookup, step-up and logout leave it)", NOW + 600);
}

#[test]
fn a_session_that_cannot_be_made_leaves_the_last_sign_in_as_it_was() {
    let (f, alice) = fixture("session-last-signin-fail");
    let bob = account(&f.store, "bob");
    make(&f, bob, NOW);
    f.store.write(|tx| tx.account_set_state(bob, AccountState::Disabled)).expect("disable");
    assert!(f.manager.create(bob, Method::Passkey, NOW + 5, "x", None).is_err());
    assert_eq!(last_signin(&f, bob), Some(NOW), "the refused creation did not move it");
    assert_eq!(last_signin(&f, alice), None, "and nobody else changed");

    // A creation that fails inside the transaction (a trigger on the table) is rolled back with its stamp.
    let conn = rusqlite::Connection::open(f.store.path()).expect("a second connection");
    conn.execute_batch("CREATE TRIGGER hw_broken BEFORE INSERT ON sessions BEGIN SELECT RAISE(ABORT, 'broken'); END;")
        .expect("trigger");
    assert!(f.manager.create(alice, Method::Passkey, NOW + 9, "x", None).is_err());
    assert_eq!(last_signin(&f, alice), None, "the rolled-back creation left no stamp");
}
