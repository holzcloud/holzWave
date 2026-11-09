//! The store of devices, device grants, the feature switches and sync blobs: what is kept, what is found, what is bounded.

#[path = "support/scratch.rs"]
mod scratch;

use std::marker::PhantomData;
use std::net::IpAddr;
use std::sync::Arc;

use holzwave_server::device::{DEVICE_PREFIX, DeviceToken, MAX_DEVICES_PER_ACCOUNT};
use holzwave_server::store::{
    Account, AccountState, Device, NewAccount, NewDevice, NewSession, Role, SignInMethod, Store, StoreError, TokenHash,
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
fn cleaned_hash(store: &Store, account: holzwave_server::store::AccountId) -> TokenHash {
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
