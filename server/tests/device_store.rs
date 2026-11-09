//! The store of devices, device grants, the feature switches and sync blobs: what is kept, what is found, what is bounded.

#[path = "support/scratch.rs"]
mod scratch;

use std::marker::PhantomData;
use std::net::IpAddr;
use std::sync::Arc;

use holzwave_server::device::{
    DEVICE_PREFIX, DeviceToken, GRANT_TTL, GrantCode, MAX_DEVICES_PER_ACCOUNT, MAX_LISTED_DEVICES, MAX_PENDING,
    MAX_PENDING_PER_ADDR, MAX_POLL_INTERVAL, POLL_INTERVAL, PollVerdict, TOUCH_INTERVAL, USER_CODE_ALPHABET, UserCode,
    poll_verdict,
};
use holzwave_server::store::{
    Account, AccountId, AccountState, Device, Grant, NewAccount, NewDevice, NewGrant, NewSession, Role, SignInMethod, Store,
    StoreError, TokenHash,
};
use scratch::Scratch;

const RANGE: std::ops::RangeInclusive<u32> = 20000..=29999;
const NOW: i64 = 1_772_353_800;
const DAY: i64 = 86_400;
const IDLE: i64 = 90 * DAY;

fn addr() -> IpAddr {
    "192.0.2.1".parse().expect("an address")
}

fn open(tag: &str) -> (Scratch, Arc<Store>) {
    let scratch = Scratch::new(tag);
    let store = Arc::new(Store::open(scratch.path()).expect("open"));
    (scratch, store)
}

fn make(store: &Store, name: &str, role: Role) -> Account {
    store.write(|tx| tx.account_create(&NewAccount::new(name, role), RANGE, NOW)).expect("create an account")
}

/// Makes a device for `account` with a fresh token and returns both.
fn enrol(store: &Store, account: &Account, name: &str, now: i64) -> (DeviceToken, Device) {
    let token = DeviceToken::generate().expect("a token");
    let device = store
        .write(|tx| {
            tx.device_create(&NewDevice {
                token_hash: token.hash(),
                account_id: account.id,
                name,
                platform: "macos",
                now,
                expires_at: now + IDLE,
                addr: addr(),
            })
        })
        .expect("create a device");
    (token, device)
}

fn raw(scratch: &Scratch) -> rusqlite::Connection {
    rusqlite::Connection::open(scratch.join("server/server.sqlite3")).expect("a second connection")
}

fn count(scratch: &Scratch, table: &str) -> i64 {
    // The table names are literals of this file.
    raw(scratch).query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).expect("count")
}

/// Turns the database of a closed store back into one of schema version 2: none of the tables of version 3.
fn downgrade_to_v2(scratch: &Scratch) {
    let conn = raw(scratch);
    conn.execute_batch("DROP TABLE devices; DROP TABLE device_grants; DROP TABLE server_settings; DROP TABLE sync_blobs;")
        .expect("drop the tables of version 3");
    conn.pragma_update(None, "user_version", 2).expect("version");
}

// ---- schema 3 ----

#[test]
fn a_version_2_database_migrates_to_3_keeps_every_row_and_gets_four_empty_tables() {
    let scratch = Scratch::new("device-migrate-3");
    let (alice, bob) = {
        let store = Store::open(scratch.path()).expect("open");
        let alice = make(&store, "alice", Role::Admin);
        let bob = make(&store, "bob", Role::Member);
        for (account, tag) in [(&alice, 1u8), (&bob, 2u8)] {
            store
                .write(|tx| {
                    tx.session_create(&NewSession {
                        id_hash: [tag; 32],
                        account_id: account.id,
                        method: SignInMethod::PasswordTotp,
                        now: NOW,
                        absolute_expires: NOW + DAY,
                        device: "a browser",
                        addr: addr(),
                    })
                })
                .expect("a session");
        }
        (alice, bob)
    };
    downgrade_to_v2(&scratch);
    let names: Vec<String> = {
        let conn = raw(&scratch);
        let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'").expect("prepare");
        stmt.query_map([], |r| r.get(0)).expect("query").collect::<rusqlite::Result<_>>().expect("rows")
    };
    assert!(!names.iter().any(|n| n == "devices"), "the file is a version 2 file: {names:?}");

    let store = Store::open(scratch.path()).expect("a version 2 file opens and is migrated");
    assert_eq!(store.read(|tx| tx.diagnostics()).expect("diagnostics").user_version, 3);
    assert_eq!(store.account_get(alice.id).expect("read"), Some(alice.clone()));
    assert_eq!(store.account_get(bob.id).expect("read"), Some(bob.clone()));
    assert_eq!(store.read(|tx| tx.session_list(alice.id)).expect("sessions").len(), 1);
    assert_eq!(store.read(|tx| tx.session_list(bob.id)).expect("sessions").len(), 1);
    let tables: Vec<String> = store.read(|tx| tx.schema()).expect("schema").into_iter().map(|t| t.name).collect();
    for table in ["devices", "device_grants", "server_settings", "sync_blobs"] {
        assert!(tables.iter().any(|t| t == table), "{table} exists: {tables:?}");
        assert_eq!(count(&scratch, table), 0, "{table} is empty");
    }
    // Forward only: opening again changes nothing more.
    drop(store);
    let again = Store::open(scratch.path()).expect("open again");
    assert_eq!(again.read(|tx| tx.diagnostics()).expect("diagnostics").user_version, 3);
    println!("Migration: version 2 -> 3, 2 accounts and 2 sessions kept, 4 new empty tables");
}

// ---- the token type ----

trait NotImplemented {
    const YES: bool = false;
}
impl<T> NotImplemented for T {}

struct IsClone<T>(PhantomData<T>);
impl<T: Clone> IsClone<T> {
    const YES: bool = true;
}

struct IsSerialize<T>(PhantomData<T>);
impl<T: serde::Serialize> IsSerialize<T> {
    const YES: bool = true;
}

#[test]
fn a_device_token_has_its_shape_parses_back_and_prints_a_marker() {
    // The probe sees what it should: a type with the traits is seen, a type without is not.
    assert!(IsClone::<String>::YES && IsSerialize::<String>::YES, "the probe works");
    assert!(!IsClone::<DeviceToken>::YES, "a token cannot be cloned");
    assert!(!IsSerialize::<DeviceToken>::YES, "a token cannot be serialised");

    let token = DeviceToken::generate().expect("a token");
    let text = token.expose().to_owned();
    assert!(text.starts_with(DEVICE_PREFIX) && DEVICE_PREFIX == "hwd_");
    assert_eq!(text.len(), 4 + 43, "{text}");
    assert!(text[4..].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "base64url without padding");

    let parsed = DeviceToken::parse(&text).expect("the text parses back");
    let hash: TokenHash = parsed.hash();
    assert_eq!(hash.len(), 32);
    assert_eq!(hash, token.hash(), "the same text gives the same hash");
    assert_ne!(DeviceToken::generate().expect("another").hash(), hash, "two tokens differ");

    let shown = [format!("{token:?}"), format!("{token}"), format!("{token:#?}"), format!("{parsed:?}")];
    for printed in shown {
        assert_eq!(printed, "DeviceToken(..)");
        assert!(!printed.contains(&text[4..]), "no part of the token is printed");
        assert!(!printed.contains(&text[4..12]), "nor a fragment of it");
    }

    // Anything but that exact shape is refused.
    let body = &text[4..];
    let mut refused = vec![
        String::new(),
        text[..text.len() - 1].to_owned(),
        format!("{text}A"),
        format!("hwi_{body}"),
        format!("HWD_{body}"),
        body.to_owned(),
        format!("{DEVICE_PREFIX}{}", body.replace(&body[..1], "!")),
        format!(" {text}"),
        format!("{text} "),
        format!("{text}\n"),
    ];
    // The same 32 bytes in another spelling: the last character's spare bits set.
    let last = text.chars().last().expect("a character");
    assert!("AEIMQUYcgkosw048".contains(last), "a canonical last character: {last}");
    refused.push(format!("{}B", &text[..text.len() - 1]));
    for bad in refused {
        assert_eq!(DeviceToken::parse(&bad).map(|t| t.hash()), Err(holzwave_server::device::CodeError::Malformed), "{bad:?}");
    }
}

// ---- one device from token to row and back ----

#[test]
fn tracer_a_device_is_stored_by_hash_only_found_while_unexpired_and_active_and_scoped_by_its_token() {
    let (scratch, store) = open("device-tracer");
    make(&store, "root", Role::Admin);
    let alice = make(&store, "alice", Role::Member);
    let (token, device) = enrol(&store, &alice, "my laptop", NOW);
    assert_eq!(device.account_id, alice.id);
    assert_eq!((device.name.as_str(), device.platform.as_str(), device.addr.as_str()), ("my laptop", "macos", "192.0.2.1"));
    assert_eq!((device.created_at, device.last_seen, device.expires_at), (NOW, NOW, NOW + IDLE));

    // The row holds the hash and nothing of the token, in the file or in the log of the write-ahead file.
    let stored: Vec<u8> = raw(&scratch)
        .query_row("SELECT token_hash FROM devices WHERE id = ?1", [device.id], |r| r.get(0))
        .expect("the stored hash");
    assert_eq!(stored, token.hash());
    for file in ["server.sqlite3", "server.sqlite3-wal"] {
        let bytes = std::fs::read(scratch.join("server").join(file)).unwrap_or_default();
        let fragment = &token.expose().as_bytes()[4..20];
        assert!(!bytes.windows(fragment.len()).any(|w| w == fragment), "{file} holds no fragment of the token");
    }

    // Found while unexpired and the account is active.
    let found = store.read(|tx| tx.device_lookup(&token.hash(), NOW + DAY)).expect("lookup");
    assert_eq!(found, Some(device.clone()));
    assert_eq!(store.read(|tx| tx.device_lookup(&token.hash(), NOW + IDLE - 1)).expect("lookup"), Some(device.clone()));
    // Nothing after the expiry (the last second counts as expired), for the hash of another device, or for a random hash.
    assert_eq!(store.read(|tx| tx.device_lookup(&token.hash(), NOW + IDLE)).expect("lookup"), None);
    assert_eq!(store.read(|tx| tx.device_lookup(&token.hash(), NOW + IDLE + DAY)).expect("lookup"), None);
    let (other_token, other) = enrol(&store, &alice, "phone", NOW);
    assert_ne!(other.id, device.id);
    assert_eq!(store.read(|tx| tx.device_lookup(&other_token.hash(), NOW)).expect("lookup"), Some(other));
    assert_eq!(store.read(|tx| tx.device_lookup(&[7; 32], NOW)).expect("lookup"), None);
    assert_eq!(store.read(|tx| tx.device_lookup(&[0; 32], NOW)).expect("lookup"), None);

    // A disabled account's devices are not found; enabling it again finds them; a deleted account's are gone.
    let bob = make(&store, "bob", Role::Member);
    store.write(|tx| tx.account_set_state(alice.id, AccountState::Disabled)).expect("disable");
    assert_eq!(store.read(|tx| tx.device_lookup(&token.hash(), NOW)).expect("lookup"), None, "disabled");
    store.write(|tx| tx.account_set_state(alice.id, AccountState::Active)).expect("enable");
    assert_eq!(store.read(|tx| tx.device_lookup(&token.hash(), NOW)).expect("lookup"), Some(device));
    let (bob_token, _) = enrol(&store, &bob, "bob's phone", NOW);
    store.write(|tx| tx.account_set_state(bob.id, AccountState::Disabled)).expect("disable");
    assert_eq!(store.read(|tx| tx.device_lookup(&bob_token.hash(), NOW)).expect("lookup"), None);
    let refused = store
        .write(|tx| {
            tx.device_create(&NewDevice {
                token_hash: [9; 32],
                account_id: bob.id,
                name: "late",
                platform: "linux",
                now: NOW,
                expires_at: NOW + IDLE,
                addr: addr(),
            })
        })
        .expect_err("a disabled account gets no device");
    assert_eq!(refused, StoreError::NotFound);
}

#[test]
fn a_device_is_ended_by_its_owner_only_and_the_cap_and_the_name_are_enforced() {
    let (_scratch, store) = open("device-revoke");
    let alice = make(&store, "alice", Role::Admin);
    let bob = make(&store, "bob", Role::Member);
    let (token, device) = enrol(&store, &alice, "laptop", NOW);

    // Another account's attempt finds nothing and the device stays.
    assert!(!store.write(|tx| tx.device_revoke(bob.id, device.id)).expect("revoke"));
    assert_eq!(store.read(|tx| tx.device_lookup(&token.hash(), NOW)).expect("lookup"), Some(device.clone()));
    // The owner ends it; the token is dead at once; a second attempt finds nothing.
    assert!(store.write(|tx| tx.device_revoke(alice.id, device.id)).expect("revoke"));
    assert_eq!(store.read(|tx| tx.device_lookup(&token.hash(), NOW)).expect("lookup"), None);
    assert!(!store.write(|tx| tx.device_revoke(alice.id, device.id)).expect("revoke"));
    assert!(!store.write(|tx| tx.device_revoke(alice.id, 9999)).expect("revoke"));

    // The cap: the 21st device is refused; another account has its own count; an ended device frees a place.
    let mut kept = Vec::new();
    for i in 0..MAX_DEVICES_PER_ACCOUNT {
        kept.push(enrol(&store, &alice, &format!("device {i}"), NOW));
    }
    let over = DeviceToken::generate().expect("a token");
    let refused = store.write(|tx| {
        tx.device_create(&NewDevice {
            token_hash: over.hash(),
            account_id: alice.id,
            name: "one too many",
            platform: "linux",
            now: NOW,
            expires_at: NOW + IDLE,
            addr: addr(),
        })
    });
    assert_eq!(refused, Err(StoreError::DeviceLimit));
    assert_eq!(store.read(|tx| tx.device_lookup(&over.hash(), NOW)).expect("lookup"), None, "nothing was written");
    enrol(&store, &bob, "bob's", NOW);
    assert!(store.write(|tx| tx.device_revoke(alice.id, kept[0].1.id)).expect("revoke"));
    enrol(&store, &alice, "fits again", NOW);

    // Expired devices do not count against the cap (after the idle time all twenty are dead).
    let later = NOW + IDLE + 1;
    enrol(&store, &alice, "a new day", later);

    // The name: control characters dropped, 64 characters kept, nothing left is refused.
    let carol = make(&store, "carol", Role::Member);
    let (_, cleaned) = enrol(&store, &carol, "a\u{7}b\nc\u{0}d\te", NOW);
    assert_eq!(cleaned.name, "abcde");
    let (_, cut) = enrol(&store, &carol, &"x".repeat(100), NOW);
    assert_eq!(cut.name.chars().count(), 64);
    let (_, wide) = enrol(&store, &carol, &"\u{e4}".repeat(100), NOW);
    assert_eq!(wide.name.chars().count(), 64, "counted in characters");
    let platform = store
        .write(|tx| {
            tx.device_create(&NewDevice {
                token_hash: [3; 32],
                account_id: carol.id,
                name: "p",
                platform: &format!("a\u{1}{}", "y".repeat(80)),
                now: NOW,
                expires_at: NOW + IDLE,
                addr: addr(),
            })
        })
        .expect("create");
    assert_eq!(platform.platform.chars().count(), 32);
    assert!(!platform.platform.contains('\u{1}'));
    for nothing in ["", "\u{7}\n"] {
        let r = store.write(|tx| {
            tx.device_create(&NewDevice {
                token_hash: [4; 32],
                account_id: carol.id,
                name: nothing,
                platform: "linux",
                now: NOW,
                expires_at: NOW + IDLE,
                addr: addr(),
            })
        });
        assert!(matches!(r, Err(StoreError::Invalid(_))), "{nothing:?}: {r:?}");
    }
    // The same token twice is a conflict, not a second device.
    let dup = store.write(|tx| {
        tx.device_create(&NewDevice {
            token_hash: cleaned_hash(&store, carol.id),
            account_id: carol.id,
            name: "twin",
            platform: "linux",
            now: NOW,
            expires_at: NOW + IDLE,
            addr: addr(),
        })
    });
    assert_eq!(dup, Err(StoreError::Conflict));
}

/// The hash of a device that exists (read from the file, since no API returns one).
fn cleaned_hash(store: &Store, account: AccountId) -> TokenHash {
    let path = store.path().to_path_buf();
    let conn = rusqlite::Connection::open(path).expect("a second connection");
    let blob: Vec<u8> = conn
        .query_row("SELECT token_hash FROM devices WHERE account_id = ?1 LIMIT 1", [account.get()], |r| r.get(0))
        .expect("a hash");
    blob.try_into().expect("32 bytes")
}

#[test]
fn deleting_an_account_leaves_none_of_its_devices() {
    let (scratch, store) = open("device-delete");
    let _alice = make(&store, "alice", Role::Admin);
    let bob = make(&store, "bob", Role::Member);
    let carol = make(&store, "carol", Role::Member);
    let (bob_token, _) = enrol(&store, &bob, "bob 1", NOW);
    enrol(&store, &bob, "bob 2", NOW);
    let (carol_token, carol_device) = enrol(&store, &carol, "carol 1", NOW);
    assert_eq!(count(&scratch, "devices"), 3);

    store.write(|tx| tx.account_delete(bob.id)).expect("delete");
    assert_eq!(count(&scratch, "devices"), 1, "only carol's device is left");
    assert_eq!(store.read(|tx| tx.device_lookup(&bob_token.hash(), NOW)).expect("lookup"), None);
    assert_eq!(store.read(|tx| tx.device_lookup(&carol_token.hash(), NOW)).expect("lookup"), Some(carol_device));
}

// ---- the user code ----

#[test]
fn the_user_code_has_twenty_plain_letters_is_drawn_without_bias_and_normalises_what_a_person_types() {
    let alphabet = std::str::from_utf8(USER_CODE_ALPHABET).expect("ASCII");
    assert_eq!(alphabet.len(), 20);
    let mut sorted: Vec<char> = alphabet.chars().collect();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 20, "no letter twice");
    for c in alphabet.chars() {
        assert!(c.is_ascii_uppercase(), "{c}");
        assert!(!"AEIOUY".contains(c), "no vowel: {c}");
    }
    assert!(!alphabet.contains(['I', 'O', '0', '1']), "no look-alike");

    let code = UserCode::generate().expect("a code");
    let shown = code.display();
    assert_eq!(shown.len(), 9, "{shown}");
    assert_eq!(shown.as_bytes()[4], b'-');
    assert!(shown.chars().filter(|c| *c != '-').all(|c| alphabet.contains(c)), "{shown}");
    assert_eq!(format!("{code:?}"), "UserCode(..)");

    // What a person types: any case, any separators, vowels and digits dropped.
    let flat = shown.replace('-', "");
    let (first, second) = flat.split_at(4);
    for typed in [
        shown.clone(),
        shown.to_lowercase(),
        flat.clone(),
        format!("{first} {second}"),
        format!("  {} \n", shown.to_lowercase()),
        format!("{first}-a e1-{second}"),
    ] {
        let read = UserCode::normalise(&typed).unwrap_or_else(|| panic!("{typed:?} is read"));
        assert_eq!(read.display(), shown, "{typed:?}");
        assert_eq!(read.hash(), code.hash());
    }
    // Any other length is refused.
    for typed in [
        String::new(),
        "----".to_owned(),
        flat[..7].to_owned(),
        format!("{flat}B"),
        format!("{flat}{flat}"),
        "aeiou".to_owned(),
        "B".repeat(65),
        " ".repeat(10_000),
        format!("{}{flat}", " ".repeat(100)),
    ] {
        assert!(UserCode::normalise(&typed).is_none(), "{:?} is refused", typed.chars().take(30).collect::<String>());
    }
    // Different codes hash differently, the dash is no part of the hash.
    let other = UserCode::generate().expect("another");
    assert_ne!(other.display(), shown);
    assert_ne!(other.hash(), code.hash());

    // 20 000 draws: every letter shows up and none is more than a quarter away from the mean (a modulo of a byte would give
    // the first sixteen letters 6.7 percent more than the last four, which is inside a quarter; this checks the arithmetic
    // of a hand-made count, the rejection itself is in the code).
    let mut counts = std::collections::HashMap::new();
    for _ in 0..2_500 {
        for c in UserCode::generate().expect("a code").display().chars().filter(|c| *c != '-') {
            *counts.entry(c).or_insert(0usize) += 1;
        }
    }
    let total: usize = counts.values().sum();
    assert_eq!(total, 20_000);
    let mean = total as f64 / 20.0;
    for c in alphabet.chars() {
        let n = *counts.get(&c).unwrap_or(&0) as f64;
        assert!(n > 0.0, "{c} was never drawn");
        assert!((n - mean).abs() <= mean * 0.25, "{c}: {n} against a mean of {mean}");
    }
    println!("User code draws: 20 000 letters over {} distinct, mean {mean}", counts.len());
}

#[test]
fn a_grant_code_has_the_shape_of_a_token_with_its_own_prefix() {
    let code = GrantCode::generate().expect("a code");
    assert!(code.expose().starts_with("hwg_") && code.expose().len() == 47);
    assert_eq!(GrantCode::parse(code.expose()).expect("parses").hash(), code.hash());
    assert_eq!(format!("{code:?}{code}"), "GrantCode(..)GrantCode(..)");
    let token = DeviceToken::generate().expect("a token");
    assert!(GrantCode::parse(token.expose()).is_err(), "a device token is no grant code");
    assert!(DeviceToken::parse(code.expose()).is_err(), "a grant code is no device token");
    assert!(!IsClone::<GrantCode>::YES && !IsSerialize::<GrantCode>::YES);
}

// ---- grants ----

fn grant(store: &Store, name: &str, from: &str, now: i64) -> (GrantCode, UserCode, Grant) {
    loop {
        let device_code = GrantCode::generate().expect("a device code");
        let user_code = UserCode::generate().expect("a user code");
        let made = store.write(|tx| {
            tx.grant_create(&NewGrant {
                device_code_hash: device_code.hash(),
                user_code_hash: user_code.hash(),
                name,
                platform: "android",
                now,
                expires_at: now + GRANT_TTL,
                addr: from.parse().expect("an address"),
            })
        });
        match made {
            Ok(grant) => return (device_code, user_code, grant),
            Err(StoreError::Conflict) => continue,
            Err(e) => panic!("create a grant: {e}"),
        }
    }
}

#[test]
fn a_grant_is_stored_by_hashes_found_by_either_decided_once_and_taken_once() {
    let (scratch, store) = open("grant-life");
    make(&store, "root", Role::Admin);
    let alice = make(&store, "alice", Role::Member);
    let bob = make(&store, "bob", Role::Member);
    let (device_code, user_code, g) = grant(&store, "my phone", "192.0.2.7", NOW);
    assert_eq!((g.name.as_str(), g.platform.as_str(), g.addr.as_str()), ("my phone", "android", "192.0.2.7"));
    assert_eq!((g.created_at, g.expires_at, g.last_poll, g.poll_interval), (NOW, NOW + GRANT_TTL, None, POLL_INTERVAL));
    assert_eq!((g.approved_by, g.denied), (None, false));
    assert_eq!(GRANT_TTL, 600);

    // Hashes only, in the file and in the log.
    let (stored_device, stored_user): (Vec<u8>, Vec<u8>) = raw(&scratch)
        .query_row("SELECT device_code_hash, user_code_hash FROM device_grants WHERE id = ?1", [g.id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("hashes");
    assert_eq!((stored_device, stored_user), (device_code.hash().to_vec(), user_code.hash().to_vec()));
    let flat = user_code.display().replace('-', "");
    for file in ["server.sqlite3", "server.sqlite3-wal"] {
        let bytes = std::fs::read(scratch.join("server").join(file)).unwrap_or_default();
        assert!(!bytes.windows(16).any(|w| w == &device_code.expose().as_bytes()[4..20]), "{file}: no device code");
        assert!(!bytes.windows(8).any(|w| w == flat.as_bytes()), "{file}: no user code");
    }

    // Found by either hash; two grants never share a user code, nor a device code.
    assert_eq!(store.read(|tx| tx.grant_by_user_code(&user_code.hash())).expect("find"), Some(g.clone()));
    assert_eq!(store.read(|tx| tx.grant_by_device_code(&device_code.hash())).expect("find"), Some(g.clone()));
    assert_eq!(store.read(|tx| tx.grant_by_user_code(&[1; 32])).expect("find"), None);
    assert_eq!(store.read(|tx| tx.grant_by_device_code(&user_code.hash())).expect("find"), None, "the hashes are not interchangeable");
    let same_user = store.write(|tx| {
        tx.grant_create(&NewGrant {
            device_code_hash: GrantCode::generate().expect("code").hash(),
            user_code_hash: user_code.hash(),
            name: "twin",
            platform: "x",
            now: NOW,
            expires_at: NOW + GRANT_TTL,
            addr: addr(),
        })
    });
    assert_eq!(same_user, Err(StoreError::Conflict));
    let same_device = store.write(|tx| {
        tx.grant_create(&NewGrant {
            device_code_hash: device_code.hash(),
            user_code_hash: UserCode::generate().expect("code").hash(),
            name: "twin",
            platform: "x",
            now: NOW,
            expires_at: NOW + GRANT_TTL,
            addr: addr(),
        })
    });
    assert_eq!(same_device, Err(StoreError::Conflict));
    assert_eq!(raw(&scratch).query_row("SELECT count(*) FROM device_grants", [], |r| r.get::<_, i64>(0)).expect("count"), 1);
    // A name with nothing left is refused; control characters are dropped.
    let refused = store.write(|tx| {
        tx.grant_create(&NewGrant {
            device_code_hash: [5; 32],
            user_code_hash: [6; 32],
            name: "\u{7}",
            platform: "x",
            now: NOW,
            expires_at: NOW + GRANT_TTL,
            addr: addr(),
        })
    });
    assert!(matches!(refused, Err(StoreError::Invalid(_))));

    // Not approved yet: nothing to take.
    assert_eq!(store.write(|tx| tx.grant_take(&device_code.hash(), NOW + 1)).expect("take"), None);
    assert_eq!(store.read(|tx| tx.grant_by_device_code(&device_code.hash())).expect("find"), Some(g.clone()), "a failed take changes nothing");
    // An account that is not active cannot approve; one that is, can, once.
    store.write(|tx| tx.account_set_state(bob.id, AccountState::Disabled)).expect("disable");
    assert!(!store.write(|tx| tx.grant_approve(g.id, bob.id, NOW + 1)).expect("approve"));
    assert!(!store.write(|tx| tx.grant_approve(g.id, AccountId::from_raw(9999), NOW + 1)).expect("approve"), "no such account");
    assert!(store.write(|tx| tx.grant_approve(g.id, alice.id, NOW + 1)).expect("approve"));
    assert!(!store.write(|tx| tx.grant_approve(g.id, alice.id, NOW + 2)).expect("approve"), "decided once");
    assert!(!store.write(|tx| tx.grant_deny(g.id, NOW + 2)).expect("deny"), "an approved grant cannot be refused");
    let approved = store.read(|tx| tx.grant_by_device_code(&device_code.hash())).expect("find").expect("there");
    assert_eq!((approved.approved_by, approved.denied), (Some(alice.id), false));
    // Taken exactly once.
    let taken = store.write(|tx| tx.grant_take(&device_code.hash(), NOW + 3)).expect("take").expect("the first take finds it");
    assert_eq!(taken.approved_by, Some(alice.id));
    assert_eq!(taken.name, "my phone");
    assert_eq!(store.write(|tx| tx.grant_take(&device_code.hash(), NOW + 4)).expect("take"), None, "the second take finds nothing");
    assert_eq!(store.read(|tx| tx.grant_by_device_code(&device_code.hash())).expect("find"), None);
    assert_eq!(store.read(|tx| tx.grant_by_user_code(&user_code.hash())).expect("find"), None);

    // Refused: stays refused, cannot be approved, cannot be taken.
    let (denied_code, _, denied) = grant(&store, "tablet", "192.0.2.7", NOW);
    assert!(store.write(|tx| tx.grant_deny(denied.id, NOW + 1)).expect("deny"));
    assert!(!store.write(|tx| tx.grant_deny(denied.id, NOW + 1)).expect("deny"));
    assert!(!store.write(|tx| tx.grant_approve(denied.id, alice.id, NOW + 1)).expect("approve"));
    assert_eq!(store.write(|tx| tx.grant_take(&denied_code.hash(), NOW + 2)).expect("take"), None);
    let seen = store.read(|tx| tx.grant_by_device_code(&denied_code.hash())).expect("find").expect("still there to answer");
    assert!(seen.denied);

    // Past its time: cannot be approved, refused or taken, even when it was approved before.
    let (late_code, _, late) = grant(&store, "late", "192.0.2.7", NOW);
    assert!(store.write(|tx| tx.grant_approve(late.id, alice.id, NOW + 1)).expect("approve"));
    assert_eq!(store.write(|tx| tx.grant_take(&late_code.hash(), NOW + GRANT_TTL)).expect("take"), None);
    let (slow_code, _, slow) = grant(&store, "slow", "192.0.2.7", NOW);
    assert!(!store.write(|tx| tx.grant_approve(slow.id, alice.id, NOW + GRANT_TTL)).expect("approve"));
    assert!(!store.write(|tx| tx.grant_deny(slow.id, NOW + GRANT_TTL)).expect("deny"));
    assert_eq!(store.write(|tx| tx.grant_take(&slow_code.hash(), NOW + GRANT_TTL + 1)).expect("take"), None);

    // An approver that was deleted afterwards: the grant is not taken (a deleted account's approved grants go with it).
    let (gone_code, _, gone) = grant(&store, "orphan", "192.0.2.7", NOW);
    let carol = make(&store, "carol", Role::Member);
    assert!(store.write(|tx| tx.grant_approve(gone.id, carol.id, NOW + 1)).expect("approve"));
    store.write(|tx| tx.account_delete(carol.id)).expect("delete");
    assert_eq!(store.read(|tx| tx.grant_by_device_code(&gone_code.hash())).expect("find"), None, "deleted with the account");
}

#[test]
fn pending_grants_are_counted_exactly_and_expired_ones_are_not_counted_and_are_purged() {
    let (scratch, store) = open("grant-counts");
    let a: IpAddr = "192.0.2.10".parse().expect("address");
    let b: IpAddr = "2001:db8::10".parse().expect("address");
    let c: IpAddr = "192.0.2.99".parse().expect("address");
    assert!(MAX_PENDING_PER_ADDR == 5 && MAX_PENDING == 200);
    assert_eq!(store.read(|tx| tx.grant_counts(a, NOW)).expect("counts").all, 0);
    for _ in 0..3 {
        grant(&store, "a", "192.0.2.10", NOW);
    }
    for _ in 0..2 {
        grant(&store, "b", "2001:db8::10", NOW);
    }
    let counts = |ip: IpAddr, now: i64| store.read(|tx| tx.grant_counts(ip, now)).expect("counts");
    assert_eq!((counts(a, NOW).all, counts(a, NOW).from_addr), (5, 3));
    assert_eq!((counts(b, NOW).all, counts(b, NOW).from_addr), (5, 2));
    assert_eq!((counts(c, NOW).all, counts(c, NOW).from_addr), (5, 0));
    // One more that expires sooner than the rest.
    store
        .write(|tx| {
            tx.grant_create(&NewGrant {
                device_code_hash: GrantCode::generate().expect("code").hash(),
                user_code_hash: UserCode::generate().expect("code").hash(),
                name: "short",
                platform: "x",
                now: NOW,
                expires_at: NOW + 30,
                addr: a,
            })
        })
        .expect("create");
    assert_eq!((counts(a, NOW + 29).all, counts(a, NOW + 29).from_addr), (6, 4));
    assert_eq!((counts(a, NOW + 30).all, counts(a, NOW + 30).from_addr), (5, 3), "the expired one is not counted");
    assert_eq!(counts(a, NOW + GRANT_TTL).all, 0);

    // A taken grant is no longer pending.
    let alice = make(&store, "alice", Role::Admin);
    let (taken_code, _, taken) = grant(&store, "taken", "192.0.2.10", NOW);
    assert_eq!(counts(a, NOW).from_addr, 5);
    store.write(|tx| tx.grant_approve(taken.id, alice.id, NOW)).expect("approve");
    store.write(|tx| tx.grant_take(&taken_code.hash(), NOW)).expect("take").expect("taken");
    assert_eq!(counts(a, NOW).from_addr, 4);

    // The purge removes expired grants and expired devices inside its row bound, and nothing else.
    let rows = |table: &str| count(&scratch, table);
    assert_eq!(rows("device_grants"), 6);
    let (_t1, d1) = enrol(&store, &alice, "old one", NOW - IDLE - 10);
    let (_t2, _d2) = enrol(&store, &alice, "fresh", NOW);
    assert_eq!(rows("devices"), 2);
    assert_eq!(d1.expires_at, NOW - 10);
    assert_eq!(store.write(|tx| tx.tokens_purge(NOW, 100)).expect("purge"), 1, "only the expired device at NOW");
    assert_eq!((rows("device_grants"), rows("devices")), (6, 1));
    assert_eq!(store.write(|tx| tx.tokens_purge(NOW + 30, 100)).expect("purge"), 1, "the grant that ran out at NOW + 30");
    assert_eq!(rows("device_grants"), 5);
    assert_eq!(store.write(|tx| tx.tokens_purge(NOW + GRANT_TTL, 3)).expect("purge"), 3, "at most the bound");
    assert_eq!(rows("device_grants"), 2);
    assert_eq!(store.write(|tx| tx.tokens_purge(NOW + GRANT_TTL, 100)).expect("purge"), 2);
    assert_eq!((rows("device_grants"), rows("devices")), (0, 1), "the fresh device is not expired");
    assert_eq!(store.write(|tx| tx.tokens_purge(NOW + GRANT_TTL, 100)).expect("purge"), 0);
}

#[test]
fn polling_is_answered_by_arithmetic_on_the_stored_times() {
    let (_scratch, store) = open("grant-poll");
    make(&store, "root", Role::Admin);
    let alice = make(&store, "alice", Role::Member);
    let (code, _, g) = grant(&store, "phone", "192.0.2.20", NOW);
    let fetch = |store: &Store| store.read(|tx| tx.grant_by_device_code(&code.hash())).expect("find").expect("there");
    // The caller's step: judge, then record the poll.
    let poll = |store: &Store, now: i64| {
        let g = fetch(store);
        let verdict = poll_verdict(&g, now);
        store.write(|tx| tx.grant_poll_mark(g.id, now, verdict == PollVerdict::SlowDown)).expect("mark");
        verdict
    };

    // The first poll is never too soon; a poll sooner than the interval is told to slow down and the interval goes up.
    assert_eq!(poll_verdict(&g, NOW), PollVerdict::Pending);
    assert_eq!(poll(&store, NOW + 1), PollVerdict::Pending);
    assert_eq!(fetch(&store).last_poll, Some(NOW + 1));
    assert_eq!(poll(&store, NOW + 2), PollVerdict::SlowDown);
    assert_eq!(fetch(&store).poll_interval, POLL_INTERVAL + 5, "the interval moved up by five seconds");
    // Seven seconds later is more than the old interval but less than the new one.
    assert_eq!(poll(&store, NOW + 2 + POLL_INTERVAL), PollVerdict::SlowDown);
    assert_eq!(fetch(&store).poll_interval, POLL_INTERVAL + 10);
    // Waiting the full interval is answered.
    let last = fetch(&store).last_poll.expect("a poll");
    assert_eq!(poll_verdict(&fetch(&store), last + POLL_INTERVAL + 9), PollVerdict::SlowDown);
    assert_eq!(poll(&store, last + POLL_INTERVAL + 10), PollVerdict::Pending);
    assert_eq!(fetch(&store).poll_interval, POLL_INTERVAL + 10, "it does not go down");

    // Slowed down again and again, the interval stops at the ceiling.
    let mut now = last + 100;
    for _ in 0..40 {
        now += 1;
        poll(&store, now);
    }
    assert_eq!(fetch(&store).poll_interval, MAX_POLL_INTERVAL);

    // Approved, refused and expired, by the stored flags and times alone.
    let mut now = now + MAX_POLL_INTERVAL + 1;
    assert!(store.write(|tx| tx.grant_approve(g.id, alice.id, now)).expect("approve"));
    assert_eq!(poll(&store, now), PollVerdict::Approved);
    assert_eq!(poll(&store, now + 1), PollVerdict::SlowDown, "approved or not, a poll that is too soon is told so");
    now += MAX_POLL_INTERVAL + 1;
    assert_eq!(poll_verdict(&fetch(&store), now), PollVerdict::Approved);
    assert_eq!(poll_verdict(&fetch(&store), NOW + GRANT_TTL - 1 + 1000), PollVerdict::Expired);
    assert_eq!(poll_verdict(&fetch(&store), NOW + GRANT_TTL), PollVerdict::Expired, "the last second is expired, even approved");
    let (_, _, d) = grant(&store, "tablet", "192.0.2.20", NOW);
    store.write(|tx| tx.grant_deny(d.id, NOW)).expect("deny");
    let mut denied_grant = d.clone();
    denied_grant.denied = true;
    assert_eq!(poll_verdict(&denied_grant, NOW + 1), PollVerdict::Denied);
    assert_eq!(poll_verdict(&d, NOW + GRANT_TTL), PollVerdict::Expired);
    // The verdict needs no database: a hand-made grant is judged the same.
    let made = Grant { last_poll: Some(100), poll_interval: 5, expires_at: 1000, ..d };
    assert_eq!(poll_verdict(&made, 104), PollVerdict::SlowDown);
    assert_eq!(poll_verdict(&made, 105), PollVerdict::Pending);
    assert_eq!(poll_verdict(&made, 999), PollVerdict::Pending);
    assert_eq!(poll_verdict(&made, 1000), PollVerdict::Expired);
    // A poll for an unknown grant marks nothing.
    assert!(!store.write(|tx| tx.grant_poll_mark(9999, NOW, true)).expect("mark"));
}

// ---- the two switches ----

#[test]
fn a_switch_that_was_never_set_is_off_a_set_switch_reads_back_and_an_unknown_key_is_refused() {
    let scratch = Scratch::new("settings");
    {
        let store = Store::open(scratch.path()).expect("open");
        for key in ["sync", "remote"] {
            assert!(!store.read(|tx| tx.setting_get(key)).expect("get"), "{key} starts off");
        }
        store.write(|tx| tx.setting_set("sync", true)).expect("set");
        assert!(store.read(|tx| tx.setting_get("sync")).expect("get"));
        assert!(!store.read(|tx| tx.setting_get("remote")).expect("get"), "the other one did not move");
        store.write(|tx| tx.setting_set("remote", true)).expect("set");
        store.write(|tx| tx.setting_set("sync", false)).expect("set");
        assert!(!store.read(|tx| tx.setting_get("sync")).expect("get"));
        assert!(store.read(|tx| tx.setting_get("remote")).expect("get"));
        store.write(|tx| tx.setting_set("remote", true)).expect("setting it again is fine");
        for key in ["", "SYNC", "admin", "sync ", "remote\0", "sync' OR '1'='1", "x".repeat(1000).as_str()] {
            assert!(matches!(store.read(|tx| tx.setting_get(key)), Err(StoreError::Invalid(_))), "{key:?}");
            assert!(matches!(store.write(|tx| tx.setting_set(key, true)), Err(StoreError::Invalid(_))), "{key:?}");
        }
    }
    // The values survive a reopen, and the table holds the two keys only, with the words on and off.
    let store = Store::open(scratch.path()).expect("reopen");
    assert!(!store.read(|tx| tx.setting_get("sync")).expect("get"));
    assert!(store.read(|tx| tx.setting_get("remote")).expect("get"));
    let rows: Vec<(String, String)> = {
        let conn = raw(&scratch);
        let mut stmt = conn.prepare("SELECT key, value FROM server_settings ORDER BY key").expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).expect("query").collect::<rusqlite::Result<_>>().expect("rows")
    };
    assert_eq!(rows, [("remote".to_owned(), "on".to_owned()), ("sync".to_owned(), "off".to_owned())]);
    // The database refuses anything else even when the store is bypassed.
    let conn = raw(&scratch);
    assert!(conn.execute("INSERT INTO server_settings (key, value) VALUES ('other', 'on')", []).is_err());
    assert!(conn.execute("INSERT INTO server_settings (key, value) VALUES ('sync', 'maybe')", []).is_err());
}

// ---- activity and lists of devices ----

#[test]
fn a_device_is_touched_at_most_once_a_minute_and_the_lists_are_scoped_ordered_and_bounded() {
    let (_scratch, store) = open("device-touch");
    make(&store, "root", Role::Admin);
    let alice = make(&store, "alice", Role::Member);
    let bob = make(&store, "bob", Role::Member);
    let (token, device) = enrol(&store, &alice, "laptop", NOW);
    let touch = |id: i64, now: i64| store.write(|tx| tx.device_touch(id, now, IDLE)).expect("touch");
    let lookup = |now: i64| store.read(|tx| tx.device_lookup(&token.hash(), now)).expect("lookup").expect("found");

    assert_eq!(TOUCH_INTERVAL, 60);
    assert!(!touch(device.id, NOW), "no time has passed");
    assert!(!touch(device.id, NOW + TOUCH_INTERVAL - 1), "within a minute nothing changes");
    assert_eq!(lookup(NOW + 59).last_seen, NOW);
    assert!(touch(device.id, NOW + TOUCH_INTERVAL), "a minute later it writes");
    let after = lookup(NOW + 61);
    assert_eq!((after.last_seen, after.expires_at), (NOW + 60, NOW + 60 + IDLE), "the idle time slides");
    assert!(!touch(device.id, NOW + 61), "and again at most once a minute");
    // A token that expired is not brought back by a touch; an unknown id changes nothing.
    let (_t, old) = enrol(&store, &alice, "old", NOW - IDLE - 100);
    assert!(!touch(old.id, NOW), "expired");
    assert!(!touch(9999, NOW));

    // The lists: unexpired only, newest first, one account or all.
    let (_tb, b1) = enrol(&store, &bob, "b1", NOW + 10);
    enrol(&store, &alice, "a2", NOW + 20);
    let (_td, b2) = enrol(&store, &bob, "b2", NOW + 30);
    let now = NOW + 100;
    let alice_list = store.read(|tx| tx.devices_of(alice.id, now)).expect("list");
    assert_eq!(alice_list.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["a2", "laptop"]);
    let bob_list = store.read(|tx| tx.devices_of(bob.id, now)).expect("list");
    assert_eq!(bob_list.iter().map(|d| d.id).collect::<Vec<_>>(), [b2.id, b1.id]);
    let all = store.read(|tx| tx.devices_all(now)).expect("list");
    assert_eq!(all.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["b2", "a2", "b1", "laptop"], "the expired one is not listed");
    assert!(all.iter().all(|d| d.expires_at > now));
    assert_eq!(MAX_LISTED_DEVICES, 1000);

    // The administrator ends any device and learns whose it was; an unknown id answers none.
    assert_eq!(store.write(|tx| tx.device_revoke_any(b1.id)).expect("revoke"), Some(bob.id));
    assert_eq!(store.write(|tx| tx.device_revoke_any(b1.id)).expect("revoke"), None);
    assert_eq!(store.read(|tx| tx.devices_of(bob.id, now)).expect("list").len(), 1);
}
