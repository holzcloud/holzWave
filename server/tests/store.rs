//! The SQLite store: creation, permissions, the schema, the account and token primitives and their races.

#[path = "support/scratch.rs"]
mod scratch;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use holzwave_server::store::{AccountState, NewAccount, Role, Store, StoreError, hash_token};
use scratch::Scratch;

const RANGE: std::ops::RangeInclusive<u32> = 20000..=29999;
const NOW: i64 = 1_772_353_800;

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).expect("metadata").permissions().mode() & 0o7777
}

#[test]
fn a_new_data_directory_gets_its_first_administrator() {
    let scratch = Scratch::new("store-tracer");
    let store = Store::open(scratch.path()).expect("open");

    let token_hash = hash_token(b"bootstrap-token-value");
    store.bootstrap_set(&token_hash, NOW + 3600).expect("set the bootstrap token");
    let alice = store
        .write(|tx| {
            tx.bootstrap_redeem(&token_hash, NOW)?;
            tx.account_create(&NewAccount::new("alice", Role::Admin), RANGE, NOW)
        })
        .expect("first administrator");
    assert_eq!(alice.uid, 20000);
    assert_eq!(alice.role, Role::Admin);
    assert_eq!(alice.state, AccountState::Active);
    assert_eq!(alice.handle.len(), 16);
    assert_ne!(alice.handle, [0u8; 16]);
    assert_eq!(alice.created_at, NOW);
    println!("Tracer account: {alice:?}");

    let bob = store.write(|tx| tx.account_create(&NewAccount::new("bob", Role::Member), RANGE, NOW)).expect("bob");
    assert_eq!(bob.uid, 20001);
    assert_ne!(bob.handle, alice.handle);

    assert_eq!(store.account_by_username("alice").expect("by name"), Some(alice.clone()));
    assert_eq!(store.account_get(bob.id).expect("by id"), Some(bob));
    assert_eq!(store.account_by_username("carol").expect("by name"), None);

    // The bootstrap token works once and only while no account exists.
    let again = store.write(|tx| tx.bootstrap_redeem(&token_hash, NOW));
    assert_eq!(again, Err(StoreError::NotRedeemable));
}

#[test]
fn the_files_are_private_and_the_connection_is_set_up() {
    let scratch = Scratch::new("store-modes");
    let store = Store::open(scratch.path()).expect("open");
    store.write(|tx| tx.account_create(&NewAccount::new("alice", Role::Admin), RANGE, NOW)).expect("write");

    let db = store.path().to_path_buf();
    let dir = db.parent().expect("server directory").to_path_buf();
    let wal = dir.join("server.sqlite3-wal");
    let shm = dir.join("server.sqlite3-shm");
    let modes = (mode_of(&dir), mode_of(&db), mode_of(&wal), mode_of(&shm));
    println!("Modes: dir {:04o}, db {:04o}, wal {:04o}, shm {:04o}", modes.0, modes.1, modes.2, modes.3);
    assert_eq!(modes, (0o700, 0o600, 0o600, 0o600));

    let d = store.read(|tx| tx.diagnostics()).expect("diagnostics");
    assert_eq!(d.journal_mode, "wal");
    assert_eq!(d.synchronous, 2, "synchronous=FULL");
    assert!(d.foreign_keys);
    assert!(d.secure_delete);
    assert!(!d.trusted_schema);
    assert_eq!(d.busy_timeout_ms, 5000);
    assert_eq!(d.application_id, holzwave_server::store::APPLICATION_ID);
    assert_eq!(d.user_version, 1);
}

#[test]
fn the_username_rule_and_a_taken_name() {
    let scratch = Scratch::new("store-names");
    let store = Store::open(scratch.path()).expect("open");
    for bad in ["", "Alice", "a b", ".dot", "dot.", "semi;colon", "quo'te", &"x".repeat(65), "ümlaut"] {
        let r = store.write(|tx| tx.account_create(&NewAccount::new(bad, Role::Member), RANGE, NOW));
        assert!(matches!(r, Err(StoreError::Invalid(_))), "{bad:?} must be refused, got {r:?}");
    }
    store.write(|tx| tx.account_create(&NewAccount::new("a.b_c-9", Role::Member), RANGE, NOW)).expect("valid name");
    let r = store.write(|tx| tx.account_create(&NewAccount::new("a.b_c-9", Role::Member), RANGE, NOW));
    assert_eq!(r, Err(StoreError::Conflict));
}
