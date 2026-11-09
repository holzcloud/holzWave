//! The SQLite store: creation, permissions, the schema, the account and token primitives and their races.

#[path = "support/scratch.rs"]
mod scratch;

use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Barrier};

use holzwave_server::store::{
    Account, AccountId, AccountState, NewAccount, NewInvite, NewSession, PasswordCredential, Role, SignInMethod, Store,
    StoreError, ThrottleRule, TokenHash, UnlockKind, hash_token,
};
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
    assert_eq!(d.user_version, 3);
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

// ---- helpers ----

const THREADS: usize = 16;

fn open(tag: &str) -> (Scratch, Arc<Store>) {
    let scratch = Scratch::new(tag);
    let store = Arc::new(Store::open(scratch.path()).expect("open"));
    (scratch, store)
}

fn make(store: &Store, name: &str, role: Role) -> Account {
    store.write(|tx| tx.account_create(&NewAccount::new(name, role), RANGE, NOW)).expect("create an account")
}

/// Runs `job` on 16 threads that start together and returns their results.
fn race<T: Send>(job: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let barrier = Barrier::new(THREADS);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..THREADS)
            .map(|i| {
                let (job, barrier) = (&job, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    job(i)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("a racing thread panicked")).collect()
    })
}

fn invite_for(store: &Store, creator: AccountId, token: &[u8], expires_at: i64) -> TokenHash {
    let token_hash = hash_token(token);
    store
        .write(|tx| {
            tx.invite_create(&NewInvite { token_hash, account_id: None, role: Role::Member, created_by: creator, expires_at })
        })
        .expect("create an invitation");
    token_hash
}

// ---- races ----

#[test]
fn race_invite_redeem() {
    let (_scratch, store) = open("store-race-invite");
    let admin = make(&store, "root", Role::Admin);
    let token_hash = invite_for(&store, admin.id, b"invite-token", NOW + 600);
    let results = race(|_| store.write(|tx| tx.invite_redeem(&token_hash, NOW + 1)));
    let winners = results.iter().filter(|r| r.is_ok()).count();
    println!("Race invite: {winners} winner of {THREADS} threads");
    assert_eq!(winners, 1);
    assert!(results.iter().filter(|r| r.is_err()).all(|r| *r == Err(StoreError::NotRedeemable)));
}

#[test]
fn unknown_expired_and_redeemed_tokens_give_the_identical_error() {
    let (_scratch, store) = open("store-token-errors");
    let admin = make(&store, "root", Role::Admin);
    let expired = invite_for(&store, admin.id, b"old", NOW + 10);
    let used = invite_for(&store, admin.id, b"used", NOW + 600);
    store.write(|tx| tx.invite_redeem(&used, NOW)).expect("the first redemption works");

    let unknown = store.write(|tx| tx.invite_redeem(&hash_token(b"never-issued"), NOW));
    let late = store.write(|tx| tx.invite_redeem(&expired, NOW + 10));
    let again = store.write(|tx| tx.invite_redeem(&used, NOW + 1));
    assert_eq!(unknown, Err(StoreError::NotRedeemable));
    assert_eq!(unknown, late);
    assert_eq!(unknown, again);
    assert_eq!(StoreError::NotRedeemable.to_string(), "the token is not valid");
}

#[test]
fn race_bootstrap_redeem_creates_exactly_one_first_administrator() {
    let (_scratch, store) = open("store-race-bootstrap");
    let token_hash = hash_token(b"bootstrap");
    store.bootstrap_set(&token_hash, NOW + 600).expect("set");
    let results = race(|i| {
        store.write(|tx| {
            tx.bootstrap_redeem(&token_hash, NOW)?;
            tx.account_create(&NewAccount::new(&format!("admin{i}"), Role::Admin), RANGE, NOW)
        })
    });
    let winners = results.iter().filter(|r| r.is_ok()).count();
    println!("Race bootstrap: {winners} winner of {THREADS} threads");
    assert_eq!(winners, 1);
    assert_eq!(store.read(|tx| tx.account_count()).expect("count"), 1);
}

#[test]
fn a_failed_first_administrator_gives_the_bootstrap_token_back() {
    let (_scratch, store) = open("store-bootstrap-rollback");
    let token_hash = hash_token(b"bootstrap");
    store.bootstrap_set(&token_hash, NOW + 600).expect("set");
    let failed = store.write(|tx| {
        tx.bootstrap_redeem(&token_hash, NOW)?;
        tx.account_create(&NewAccount::new("Not A Name", Role::Admin), RANGE, NOW)
    });
    assert!(matches!(failed, Err(StoreError::Invalid(_))));
    store
        .write(|tx| {
            tx.bootstrap_redeem(&token_hash, NOW)?;
            tx.account_create(&NewAccount::new("alice", Role::Admin), RANGE, NOW)
        })
        .expect("the token still works after the rollback");
    // Once an account exists the token is dead, and a new one cannot be set.
    assert_eq!(store.write(|tx| tx.bootstrap_redeem(&token_hash, NOW)), Err(StoreError::NotRedeemable));
    assert_eq!(store.bootstrap_set(&hash_token(b"another"), NOW + 600), Err(StoreError::Conflict));
}

#[test]
fn an_expired_bootstrap_token_does_not_work() {
    let (_scratch, store) = open("store-bootstrap-expired");
    let token_hash = hash_token(b"bootstrap");
    store.bootstrap_set(&token_hash, NOW + 600).expect("set");
    assert_eq!(store.write(|tx| tx.bootstrap_redeem(&token_hash, NOW + 600)), Err(StoreError::NotRedeemable));
    assert_eq!(store.write(|tx| tx.bootstrap_redeem(&hash_token(b"other"), NOW)), Err(StoreError::NotRedeemable));
}

#[test]
fn race_totp_step() {
    let (_scratch, store) = open("store-race-totp");
    let alice = make(&store, "alice", Role::Admin);
    store.write(|tx| tx.totp_set(alice.id, b"sealed-secret-bytes")).expect("totp");
    let results = race(|_| store.write(|tx| tx.totp_accept_step(alice.id, 100)).expect("accept"));
    let winners = results.iter().filter(|accepted| **accepted).count();
    println!("Race totp: {winners} winner of {THREADS} threads");
    assert_eq!(winners, 1);
    assert!(!store.write(|tx| tx.totp_accept_step(alice.id, 99)).expect("older step"));
    assert!(!store.write(|tx| tx.totp_accept_step(alice.id, 100)).expect("same step"));
    assert!(store.write(|tx| tx.totp_accept_step(alice.id, 101)).expect("newer step"));
    assert_eq!(store.read(|tx| tx.totp_get(alice.id)).expect("get").expect("row").last_step, 101);
}

#[test]
fn race_last_admin() {
    let (_scratch, store) = open("store-race-admin");
    let a = make(&store, "ann", Role::Admin);
    let b = make(&store, "ben", Role::Admin);
    let mut winners_total = 0;
    for round in 0..30 {
        let ids = [a.id, b.id];
        let barrier = Barrier::new(2);
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|i| {
                    let (store, barrier) = (&store, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        // Each thread disables the other administrator.
                        store.write(|tx| tx.account_set_state(ids[1 - i], AccountState::Disabled))
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().expect("thread")).collect::<Vec<_>>()
        });
        let ok = results.iter().filter(|r| r.is_ok()).count();
        winners_total += ok;
        assert_eq!(ok, 1, "round {round}: exactly one disable succeeds, got {results:?}");
        assert!(results.contains(&Err(StoreError::LastAdmin)), "the loser gets LastAdmin");
        assert_eq!(store.read(|tx| tx.admin_count_active()).expect("count"), 1, "round {round}");
        for id in ids {
            store.write(|tx| tx.account_set_state(id, AccountState::Active)).expect("enable again");
        }
    }
    println!("Race last admin: 30 rounds, {winners_total} winners, never fewer than one active administrator");
}

#[test]
fn the_only_administrator_cannot_be_demoted_disabled_or_deleted() {
    let (_scratch, store) = open("store-last-admin");
    let alice = make(&store, "alice", Role::Admin);
    let member = make(&store, "mia", Role::Member);
    let before = store.account_get(alice.id).expect("get");
    assert_eq!(store.write(|tx| tx.account_set_role(alice.id, Role::Member)), Err(StoreError::LastAdmin));
    assert_eq!(store.write(|tx| tx.account_set_state(alice.id, AccountState::Disabled)), Err(StoreError::LastAdmin));
    assert_eq!(store.write(|tx| tx.account_delete(alice.id)), Err(StoreError::LastAdmin));
    assert_eq!(store.account_get(alice.id).expect("get"), before, "nothing changed");
    // A member does not count, and another administrator frees the first.
    store.write(|tx| tx.account_set_role(member.id, Role::Admin)).expect("promote");
    store.write(|tx| tx.account_set_role(alice.id, Role::Member)).expect("demote with another administrator");
    assert_eq!(store.write(|tx| tx.account_delete(member.id)), Err(StoreError::LastAdmin));
    assert_eq!(
        StoreError::LastAdmin.to_string(),
        "the last active administrator cannot be demoted, disabled or deleted"
    );
}

// ---- uids ----

#[test]
fn uid_never_reused() {
    let (_scratch, store) = open("store-uid");
    let root = make(&store, "root", Role::Admin);
    let b = make(&store, "bee", Role::Member);
    let c = make(&store, "cat", Role::Member);
    assert_eq!((root.uid, b.uid, c.uid), (20000, 20001, 20002));
    store.write(|tx| tx.account_delete(b.id)).expect("delete the middle one");
    let d = make(&store, "dog", Role::Member);
    assert_eq!(d.uid, 20003, "the deleted account keeps its id");
    let row = store.account_get(b.id).expect("get").expect("the row stays");
    assert_eq!((row.state, row.uid, row.username.as_str(), row.handle), (AccountState::Deleted, 20001, "bee", b.handle));
    assert_eq!(
        store.write(|tx| tx.account_create(&NewAccount::new("bee", Role::Member), RANGE, NOW)),
        Err(StoreError::Conflict)
    );
    assert_eq!(
        store.write(|tx| tx.account_create(&NewAccount::new("bee2", Role::Member).handle(b.handle), RANGE, NOW)),
        Err(StoreError::Conflict)
    );
    // Deleting the account with the highest id does not hand its id out again either.
    store.write(|tx| tx.account_delete(d.id)).expect("delete the last one");
    assert_eq!(make(&store, "elk", Role::Member).uid, 20004);
}

#[test]
fn a_full_range_refuses_with_a_sentence_and_a_range_that_moved_starts_at_its_minimum() {
    let (_scratch, store) = open("store-uid-range");
    let small = 30000..=30001;
    for name in ["one", "two"] {
        store.write(|tx| tx.account_create(&NewAccount::new(name, Role::Admin), small.clone(), NOW)).expect("create");
    }
    let full = store.write(|tx| tx.account_create(&NewAccount::new("three", Role::Member), small.clone(), NOW));
    assert_eq!(full, Err(StoreError::UidRangeFull));
    assert_eq!(full.expect_err("full").to_string(), "no free user ids in the configured range");
    assert_eq!(store.read(|tx| tx.account_count()).expect("count"), 2, "the refusal changed nothing");
    // A range that starts above every id so far starts at its minimum.
    let moved = store
        .write(|tx| tx.account_create(&NewAccount::new("four", Role::Member), 40000..=40009, NOW))
        .expect("create in the moved range");
    assert_eq!(moved.uid, 40000);
    // An empty range has no id to give.
    let (first, last) = (50000u32, 49999u32);
    assert_eq!(
        store.write(|tx| tx.account_create(&NewAccount::new("five", Role::Member), first..=last, NOW)),
        Err(StoreError::UidRangeFull)
    );
}

#[test]
fn race_account_create_gives_every_account_its_own_uid() {
    let (_scratch, store) = open("store-race-uid");
    let results = race(|i| make_result(&store, &format!("user{i}")));
    let mut uids: Vec<u32> = results.into_iter().map(|r| r.expect("created").uid).collect();
    uids.sort_unstable();
    uids.dedup();
    assert_eq!(uids.len(), THREADS);
    assert_eq!((uids[0], uids[THREADS - 1]), (20000, 20000 + THREADS as u32 - 1));
}

fn make_result(store: &Store, name: &str) -> Result<Account, StoreError> {
    store.write(|tx| tx.account_create(&NewAccount::new(name, Role::Member), RANGE, NOW))
}

// ---- delete, credentials ----

#[test]
fn account_delete_removes_everything_the_person_owned_and_keeps_the_row() {
    let (_scratch, store) = open("store-delete");
    let root = make(&store, "root", Role::Admin);
    let mia = make(&store, "mia", Role::Member);
    let ip: IpAddr = "198.51.100.7".parse().expect("ip");
    let session_hash = hash_token(b"session-of-mia");
    store
        .write(|tx| {
            tx.password_set(
                mia.id,
                &PasswordCredential { verifier: vec![1; 32], salt: vec![2; 16], params: "m=19456,t=2,p=1".into(), changed_at: NOW },
            )?;
            tx.totp_set(mia.id, b"sealed")?;
            tx.passkey_add(mia.id, b"credential-1", "{}", Some(&[9; 32]), NOW)?;
            tx.oidc_link(mia.id, "https://issuer.example", "subject-1")?;
            tx.unlock_add(mia.id, UnlockKind::Password, &[3; 16], "argon2id", None)?;
            tx.session_create(&NewSession {
                id_hash: session_hash,
                account_id: mia.id,
                method: SignInMethod::Passkey,
                now: NOW,
                absolute_expires: NOW + 3600,
                device: "Firefox on Linux",
                addr: ip,
            })?;
            // An open invitation for her and one she made are gone with her; one root made for somebody else stays.
            tx.invite_create(&NewInvite {
                token_hash: hash_token(b"for-mia"),
                account_id: Some(mia.id),
                role: Role::Member,
                created_by: root.id,
                expires_at: NOW + 600,
            })?;
            tx.invite_create(&NewInvite {
                token_hash: hash_token(b"by-mia"),
                account_id: None,
                role: Role::Member,
                created_by: mia.id,
                expires_at: NOW + 600,
            })?;
            tx.invite_create(&NewInvite {
                token_hash: hash_token(b"by-root"),
                account_id: None,
                role: Role::Member,
                created_by: root.id,
                expires_at: NOW + 600,
            })?;
            Ok(())
        })
        .expect("fill");

    store.write(|tx| tx.account_delete(mia.id)).expect("delete");
    store
        .read(|tx| {
            assert_eq!(tx.account_get(mia.id)?.expect("row").state, AccountState::Deleted);
            assert!(tx.password_get(mia.id)?.is_none());
            assert!(tx.totp_get(mia.id)?.is_none());
            assert!(tx.passkey_list(mia.id)?.is_empty());
            assert!(tx.oidc_find("https://issuer.example", "subject-1")?.is_none());
            assert!(tx.unlock_list(mia.id)?.is_empty());
            assert!(tx.session_get(&session_hash)?.is_none());
            let left = tx.invite_list()?;
            assert_eq!(left.len(), 1);
            assert_eq!(left[0].token_hash, hash_token(b"by-root"));
            Ok(())
        })
        .expect("check");
    // A deleted account cannot be changed or given new credentials, and cannot be deleted twice.
    assert_eq!(store.write(|tx| tx.account_set_role(mia.id, Role::Admin)), Err(StoreError::NotFound));
    assert_eq!(store.write(|tx| tx.totp_set(mia.id, b"x")), Err(StoreError::NotFound));
    assert_eq!(store.write(|tx| tx.account_delete(mia.id)), Err(StoreError::NotFound));
}

#[test]
fn password_and_totp_rows_round_trip_and_hide_their_bytes_in_debug() {
    let (_scratch, store) = open("store-secrets");
    let alice = make(&store, "alice", Role::Admin);
    let credential =
        PasswordCredential { verifier: vec![251, 252, 253], salt: vec![7; 16], params: "m=19456,t=2,p=1".into(), changed_at: NOW };
    store.write(|tx| tx.password_set(alice.id, &credential)).expect("set");
    let newer = PasswordCredential { changed_at: NOW + 5, ..credential.clone() };
    store.write(|tx| tx.password_set(alice.id, &newer)).expect("replace");
    let got = store.read(|tx| tx.password_get(alice.id)).expect("get").expect("row");
    assert_eq!(got, newer);
    assert!(!format!("{got:?}").contains("251"), "Debug must not print the verifier");

    store.write(|tx| tx.totp_set(alice.id, &[240, 241, 242])).expect("totp");
    let totp = store.read(|tx| tx.totp_get(alice.id)).expect("get").expect("row");
    assert_eq!(totp.sealed_secret, vec![240, 241, 242]);
    assert!(!format!("{totp:?}").contains("241"));

    assert!(store.write(|tx| tx.password_delete(alice.id)).expect("delete"));
    assert!(!store.write(|tx| tx.password_delete(alice.id)).expect("delete again"));
    assert!(store.write(|tx| tx.totp_delete(alice.id)).expect("delete"));
    assert!(!store.write(|tx| tx.totp_accept_step(alice.id, 5)).expect("no row, no step"));
}

// ---- throttle ----

const RULE: ThrottleRule = ThrottleRule { window_secs: 900, limit: 5, lock_secs: 900 };

#[test]
fn throttle_locks_at_the_limit_keeps_the_lock_and_starts_again_after_the_window() {
    let (_scratch, store) = open("store-throttle");
    for n in 1..=4 {
        let state = store.write(|tx| tx.throttle_fail("account:7", NOW + n, &RULE)).expect("fail");
        assert_eq!((state.failures, state.locked_until), (n, 0));
        assert!(!state.is_locked(NOW + n));
    }
    let fifth = store.write(|tx| tx.throttle_fail("account:7", NOW + 5, &RULE)).expect("fifth");
    assert_eq!((fifth.failures, fifth.locked_until), (5, NOW + 5 + 900));
    assert!(fifth.is_locked(NOW + 6));
    let sixth = store.write(|tx| tx.throttle_fail("account:7", NOW + 100, &RULE)).expect("sixth");
    assert_eq!((sixth.failures, sixth.locked_until), (6, NOW + 905), "a failure in the lock keeps its end");
    assert_eq!(store.read(|tx| tx.throttle_get("account:7", NOW + 200, RULE.window_secs)).expect("get"), sixth);
    // After the lock and the window the count starts at one.
    let after = NOW + 1000;
    assert_eq!(store.read(|tx| tx.throttle_get("account:7", after, RULE.window_secs)).expect("get").failures, 0);
    let again = store.write(|tx| tx.throttle_fail("account:7", after, &RULE)).expect("again");
    assert_eq!((again.window_start, again.failures, again.locked_until), (after, 1, 0));
    // Other keys are independent, and clear forgets.
    assert_eq!(store.read(|tx| tx.throttle_get("addr:203.0.113.1", NOW, RULE.window_secs)).expect("get").failures, 0);
    assert!(store.write(|tx| tx.throttle_clear("account:7")).expect("clear"));
    assert!(!store.write(|tx| tx.throttle_clear("account:7")).expect("clear again"));
    assert_eq!(
        store.write(|tx| tx.throttle_fail("", NOW, &RULE)),
        Err(StoreError::Invalid("the throttle key or rule has a wrong shape"))
    );
}

#[test]
fn race_throttle_counts_every_failure() {
    let (_scratch, store) = open("store-race-throttle");
    let rule = ThrottleRule { window_secs: 900, limit: 1000, lock_secs: 900 };
    race(|_| store.write(|tx| tx.throttle_fail("account:1", NOW, &rule)).expect("fail"));
    let state = store.read(|tx| tx.throttle_get("account:1", NOW, 900)).expect("get");
    println!("Race throttle: {} failures counted of {THREADS} threads", state.failures);
    assert_eq!(state.failures, THREADS as i64);
}

// ---- sessions ----

#[test]
fn sessions_are_created_touched_listed_and_purged() {
    let (_scratch, store) = open("store-sessions");
    let alice = make(&store, "alice", Role::Admin);
    let ip: IpAddr = "2001:db8::1".parse().expect("ip");
    let long_device = format!("{}\n{}", "x".repeat(100), "y");
    let new = |hash: TokenHash, now: i64, expires: i64, device: &'static str| NewSession {
        id_hash: hash,
        account_id: alice.id,
        method: SignInMethod::PasswordTotp,
        now,
        absolute_expires: expires,
        device,
        addr: ip,
    };
    let (h1, h2, h3) = (hash_token(b"s1"), hash_token(b"s2"), hash_token(b"s3"));
    let first = store.write(|tx| tx.session_create(&NewSession { device: &long_device, ..new(h1, NOW, NOW + 1000, "") })).expect("create");
    assert_eq!(first.device.chars().count(), 64, "the device text is cut to 64 characters without control characters");
    assert!(!first.device.contains('\n'));
    assert_eq!(first.addr, "2001:db8::1");
    store.write(|tx| tx.session_create(&new(h2, NOW + 10, NOW + 100_000, "b"))).expect("create");
    store.write(|tx| tx.session_create(&new(h3, NOW + 20, NOW + 100_000, "c"))).expect("create");

    assert!(store.write(|tx| tx.session_touch(&h2, NOW + 600)).expect("touch"));
    assert!(store.write(|tx| tx.session_touch(&h2, NOW + 400)).expect("touch backwards"));
    assert_eq!(store.read(|tx| tx.session_get(&h2)).expect("get").expect("row").last_seen, NOW + 600, "never backwards");
    assert!(store.write(|tx| tx.session_set_last_auth(&h2, NOW + 700)).expect("step-up"));
    assert_eq!(store.read(|tx| tx.session_get(&h2)).expect("get").expect("row").last_auth, NOW + 700);
    assert!(!store.write(|tx| tx.session_touch(&hash_token(b"missing"), NOW)).expect("touch missing"));
    let listed = store.read(|tx| tx.session_list(alice.id)).expect("list");
    assert_eq!(listed.len(), 3);
    assert!(!format!("{:?}", listed[0]).contains("id_hash"), "Debug leaves the hash out");

    // Purge: s1 reached its absolute expiry; s3 was idle for longer than 1,000 s; s2 is neither.
    let purged = store.write(|tx| tx.sessions_purge(NOW + 1500, 1000, 100)).expect("purge");
    assert_eq!(purged, 2);
    assert!(store.read(|tx| tx.session_get(&h1)).expect("get").is_none());
    assert!(store.read(|tx| tx.session_get(&h2)).expect("get").is_some());
    assert!(store.read(|tx| tx.session_get(&h3)).expect("get").is_none());

    // A cap bounds the work of one call.
    for n in 0..250u32 {
        let hash = hash_token(&n.to_be_bytes());
        store.write(|tx| tx.session_create(&new(hash, NOW, NOW + 10, "d"))).expect("create");
    }
    assert_eq!(store.write(|tx| tx.sessions_purge(NOW + 20, 1_000_000, 100)).expect("purge"), 100);
    assert_eq!(store.write(|tx| tx.sessions_purge(NOW + 20, 1_000_000, 100)).expect("purge"), 100);
    assert_eq!(store.write(|tx| tx.sessions_purge(NOW + 20, 1_000_000, 100)).expect("purge"), 50);

    assert!(store.write(|tx| tx.session_delete(&h2)).expect("delete"));
    assert!(!store.write(|tx| tx.session_delete(&h2)).expect("delete again"));
    store.write(|tx| tx.session_create(&new(h1, NOW, NOW + 10, "e"))).expect("create");
    store.write(|tx| tx.session_create(&new(h3, NOW, NOW + 10, "f"))).expect("create");
    assert_eq!(store.write(|tx| tx.session_delete_all(alice.id)).expect("delete all"), 2);
}

#[test]
fn a_session_row_for_a_missing_account_is_refused_by_the_database() {
    let (_scratch, store) = open("store-fk");
    let r = store.write(|tx| {
        tx.session_create(&NewSession {
            id_hash: hash_token(b"x"),
            account_id: AccountId::from_raw(4242),
            method: SignInMethod::Oidc,
            now: NOW,
            absolute_expires: NOW + 10,
            device: "d",
            addr: "192.0.2.1".parse().expect("ip"),
        })
    });
    assert_eq!(r, Err(StoreError::NotFound));
}

// ---- passkeys, identities, unlock methods, invitations ----

#[test]
fn passkeys_are_limited_unique_and_removed_only_by_their_owner() {
    let (_scratch, store) = open("store-passkeys");
    let alice = make(&store, "alice", Role::Admin);
    let bob = make(&store, "bob", Role::Member);
    for n in 0..10u8 {
        store.write(|tx| tx.passkey_add(alice.id, &[n; 20], "{}", None, NOW)).expect("add");
    }
    assert_eq!(store.write(|tx| tx.passkey_add(alice.id, &[99; 20], "{}", None, NOW)), Err(StoreError::PasskeyLimit));
    // The credential id is unique across accounts.
    assert_eq!(store.write(|tx| tx.passkey_add(bob.id, &[3; 20], "{}", None, NOW)), Err(StoreError::Conflict));
    let salt = [0xAB; 32];
    let with_prf = store.write(|tx| tx.passkey_add(bob.id, b"bobs-key", "{\"k\":1}", Some(&salt), NOW)).expect("add");
    let found = store.read(|tx| tx.passkey_by_credential(b"bobs-key")).expect("find").expect("row");
    assert_eq!(found, with_prf);
    assert_eq!(found.prf_salt.as_deref(), Some(&salt[..]));
    store.write(|tx| tx.passkey_update_json(bob.id, with_prf.id, "{\"k\":2}")).expect("update");
    assert_eq!(store.read(|tx| tx.passkey_list(bob.id)).expect("list")[0].passkey_json, "{\"k\":2}");
    // Alice cannot remove or change Bob's key; Bob can remove it.
    assert_eq!(store.write(|tx| tx.passkey_remove(alice.id, with_prf.id)), Err(StoreError::NotFound));
    assert_eq!(store.write(|tx| tx.passkey_update_json(alice.id, with_prf.id, "{}")), Err(StoreError::NotFound));
    store.write(|tx| tx.passkey_remove(bob.id, with_prf.id)).expect("remove");
    assert_eq!(store.read(|tx| tx.passkey_count(bob.id)).expect("count"), 0);
    // Removing one of Alice's makes room.
    let first = store.read(|tx| tx.passkey_list(alice.id)).expect("list")[0].id;
    store.write(|tx| tx.passkey_remove(alice.id, first)).expect("remove");
    store.write(|tx| tx.passkey_add(alice.id, &[99; 20], "{}", None, NOW)).expect("add again");
}

#[test]
fn oidc_identities_link_once_and_unlink() {
    let (_scratch, store) = open("store-oidc");
    let alice = make(&store, "alice", Role::Admin);
    let bob = make(&store, "bob", Role::Member);
    store.write(|tx| tx.oidc_link(alice.id, "https://id.example", "sub-1")).expect("link");
    assert_eq!(store.read(|tx| tx.oidc_find("https://id.example", "sub-1")).expect("find"), Some(alice.id));
    assert_eq!(store.write(|tx| tx.oidc_link(bob.id, "https://id.example", "sub-1")), Err(StoreError::Conflict));
    assert_eq!(store.write(|tx| tx.oidc_link(alice.id, "https://id.example", "sub-1")), Err(StoreError::Conflict));
    store.write(|tx| tx.oidc_link(bob.id, "https://id.example", "sub-2")).expect("another subject");
    assert_eq!(store.read(|tx| tx.oidc_list(alice.id)).expect("list").len(), 1);
    assert_eq!(store.write(|tx| tx.oidc_unlink(bob.id, "https://id.example", "sub-1")), Err(StoreError::NotFound));
    store.write(|tx| tx.oidc_unlink(alice.id, "https://id.example", "sub-1")).expect("unlink");
    assert_eq!(store.read(|tx| tx.oidc_find("https://id.example", "sub-1")).expect("find"), None);
    assert!(matches!(store.write(|tx| tx.oidc_link(alice.id, "", "x")), Err(StoreError::Invalid(_))));
}

#[test]
fn unlock_methods_list_in_insertion_order_and_are_removed_by_their_owner() {
    let (_scratch, store) = open("store-unlock");
    let alice = make(&store, "alice", Role::Admin);
    let bob = make(&store, "bob", Role::Member);
    let kinds = [UnlockKind::Password, UnlockKind::Passphrase, UnlockKind::Prf, UnlockKind::Recovery];
    for (i, kind) in kinds.into_iter().enumerate() {
        let credential: Option<&[u8]> = if kind == UnlockKind::Prf { Some(b"cred") } else { None };
        store.write(|tx| tx.unlock_add(alice.id, kind, &[i as u8 + 1; 16], "argon2id", credential)).expect("add");
    }
    let listed = store.read(|tx| tx.unlock_list(alice.id)).expect("list");
    assert_eq!(listed.iter().map(|m| m.kind).collect::<Vec<_>>(), kinds);
    assert_eq!(listed[2].credential_id.as_deref(), Some(&b"cred"[..]));
    assert_eq!(store.write(|tx| tx.unlock_remove(bob.id, listed[0].id)), Err(StoreError::NotFound));
    store.write(|tx| tx.unlock_remove(alice.id, listed[0].id)).expect("remove");
    assert_eq!(store.read(|tx| tx.unlock_list(alice.id)).expect("list").len(), 3);
}

#[test]
fn invitations_are_created_listed_revoked_and_purged_when_unused_and_expired() {
    let (_scratch, store) = open("store-invites");
    let admin = make(&store, "root", Role::Admin);
    let keep = invite_for(&store, admin.id, b"keep", NOW + 10_000);
    let old = invite_for(&store, admin.id, b"old", NOW + 10);
    let spent = invite_for(&store, admin.id, b"spent", NOW + 20);
    store.write(|tx| tx.invite_redeem(&spent, NOW)).expect("redeem");
    let withdrawn = invite_for(&store, admin.id, b"withdrawn", NOW + 10_000);
    let list = store.read(|tx| tx.invite_list()).expect("list");
    assert_eq!(list.len(), 4);
    assert!(!format!("{:?}", list[0]).contains("token_hash"), "Debug leaves the hash out");
    let id = list.iter().find(|i| i.token_hash == withdrawn).expect("row").id;
    store.write(|tx| tx.invite_revoke(id)).expect("revoke");
    assert_eq!(store.write(|tx| tx.invite_redeem(&withdrawn, NOW)), Err(StoreError::NotRedeemable));
    let spent_id = list.iter().find(|i| i.token_hash == spent).expect("row").id;
    assert_eq!(store.write(|tx| tx.invite_revoke(spent_id)), Err(StoreError::NotFound), "a redeemed invitation stays");

    // Purge: the expired unused one goes; the kept one, the redeemed one (history) stay.
    assert_eq!(store.write(|tx| tx.tokens_purge(NOW + 100, 100)).expect("purge"), 1);
    assert_eq!(store.write(|tx| tx.invite_redeem(&old, NOW + 1)), Err(StoreError::NotRedeemable));
    let rest = store.read(|tx| tx.invite_list()).expect("list");
    assert_eq!(rest.len(), 2);
    assert!(rest.iter().any(|i| i.token_hash == keep));
}

// ---- hostile data ----

#[test]
fn hostile_text_is_refused_as_a_name_and_kept_as_data_elsewhere() {
    let (_scratch, store) = open("store-hostile");
    let hostile = "x'; DROP TABLE accounts; --";
    let r = store.write(|tx| tx.account_create(&NewAccount::new(hostile, Role::Member), RANGE, NOW));
    assert!(matches!(r, Err(StoreError::Invalid(_))), "the username rule refuses it, got {r:?}");
    let ok = store
        .write(|tx| tx.account_create(&NewAccount::new("mallory", Role::Admin).display_name(hostile), RANGE, NOW))
        .expect("the same text is fine as a display name");
    assert_eq!(store.account_get(ok.id).expect("get").expect("row").display_name, hostile);
    assert_eq!(store.account_by_username(hostile).expect("lookup"), None, "and as a lookup it is data too");
    // The table is still there, and a control character in a display name is refused.
    assert_eq!(store.read(|tx| tx.account_count()).expect("count"), 1);
    let control = store.write(|tx| tx.account_set_display_name(ok.id, "bell\u{7}"));
    assert!(matches!(control, Err(StoreError::Invalid(_))));
    store.write(|tx| tx.account_set_display_name(ok.id, "Mallory M.")).expect("rename");
}

#[test]
fn accounts_are_found_by_every_key_and_listed_in_order() {
    let (_scratch, store) = open("store-lookup");
    let alice = make(&store, "alice", Role::Admin);
    let bob = make(&store, "bob", Role::Member);
    store
        .read(|tx| {
            assert_eq!(tx.account_by_handle(&bob.handle)?, Some(bob.clone()));
            assert_eq!(tx.account_by_uid(alice.uid)?, Some(alice.clone()));
            assert_eq!(tx.account_by_uid(1)?, None);
            assert_eq!(tx.account_list()?, vec![alice.clone(), bob.clone()]);
            Ok(())
        })
        .expect("lookups");
    // A handle the caller chose is kept as it is; the all-zero handle is refused.
    let chosen = [5u8; 16];
    let carol = store
        .write(|tx| tx.account_create(&NewAccount::new("carol", Role::Member).handle(chosen), RANGE, NOW))
        .expect("create");
    assert_eq!(carol.handle, chosen);
    let zero = store.write(|tx| tx.account_create(&NewAccount::new("dave", Role::Member).handle([0; 16]), RANGE, NOW));
    assert!(matches!(zero, Err(StoreError::Invalid(_))));
}

#[test]
fn a_write_that_fails_changes_nothing() {
    let (_scratch, store) = open("store-rollback");
    let result: Result<(), StoreError> = store.write(|tx| {
        tx.account_create(&NewAccount::new("alice", Role::Admin), RANGE, NOW)?;
        tx.account_create(&NewAccount::new("alice", Role::Admin), RANGE, NOW)?;
        Ok(())
    });
    assert_eq!(result, Err(StoreError::Conflict));
    assert_eq!(store.read(|tx| tx.account_count()).expect("count"), 0, "the first insert was rolled back");
    // A write inside `read` is refused, and the store works afterwards.
    let refused = store.read(|tx| tx.account_create(&NewAccount::new("eve", Role::Member), RANGE, NOW));
    assert!(matches!(refused, Err(StoreError::Storage(_))));
    make(&store, "alice", Role::Admin);
}

// ---- schema, versions, damaged files, links ----

/// The schema of version 3 as a literal: table, then `(column, declared type)` sorted by column.
const SCHEMA_V3: &[(&str, &[(&str, &str)])] = &[
    (
        "accounts",
        &[
            ("created_at", "INTEGER"),
            ("display_name", "TEXT"),
            ("handle", "BLOB"),
            ("id", "INTEGER"),
            ("last_signin", "INTEGER"),
            ("role", "TEXT"),
            ("state", "TEXT"),
            ("uid", "INTEGER"),
            ("username", "TEXT"),
        ],
    ),
    ("bootstrap", &[("expires_at", "INTEGER"), ("redeemed_at", "INTEGER"), ("token_hash", "BLOB")]),
    (
        "device_grants",
        &[
            ("addr", "TEXT"),
            ("approved_account_id", "INTEGER"),
            ("created_at", "INTEGER"),
            ("denied", "INTEGER"),
            ("device_code_hash", "BLOB"),
            ("expires_at", "INTEGER"),
            ("id", "INTEGER"),
            ("last_poll", "INTEGER"),
            ("name", "TEXT"),
            ("platform", "TEXT"),
            ("poll_interval", "INTEGER"),
            ("user_code_hash", "BLOB"),
        ],
    ),
    (
        "devices",
        &[
            ("account_id", "INTEGER"),
            ("addr", "TEXT"),
            ("created_at", "INTEGER"),
            ("expires_at", "INTEGER"),
            ("id", "INTEGER"),
            ("last_seen", "INTEGER"),
            ("name", "TEXT"),
            ("platform", "TEXT"),
            ("token_hash", "BLOB"),
        ],
    ),
    (
        "invites",
        &[
            ("account_id", "INTEGER"),
            ("created_by", "INTEGER"),
            ("expires_at", "INTEGER"),
            ("id", "INTEGER"),
            ("redeemed_at", "INTEGER"),
            ("role", "TEXT"),
            ("token_hash", "BLOB"),
        ],
    ),
    ("oidc_identities", &[("account_id", "INTEGER"), ("issuer", "TEXT"), ("sub", "TEXT")]),
    (
        "passkeys",
        &[
            ("account_id", "INTEGER"),
            ("created_at", "INTEGER"),
            ("credential_id", "BLOB"),
            ("id", "INTEGER"),
            ("passkey_json", "TEXT"),
            ("prf_salt", "BLOB"),
        ],
    ),
    (
        "password_credentials",
        &[
            ("account_id", "INTEGER"),
            ("changed_at", "INTEGER"),
            ("params", "TEXT"),
            ("salt", "BLOB"),
            ("verifier", "BLOB"),
        ],
    ),
    ("server_settings", &[("key", "TEXT"), ("value", "TEXT")]),
    (
        "sessions",
        &[
            ("absolute_expires", "INTEGER"),
            ("account_id", "INTEGER"),
            ("addr", "TEXT"),
            ("created_at", "INTEGER"),
            ("device", "TEXT"),
            ("id_hash", "BLOB"),
            ("last_auth", "INTEGER"),
            ("last_seen", "INTEGER"),
            ("method", "TEXT"),
        ],
    ),
    (
        "sync_blobs",
        &[
            ("account_id", "INTEGER"),
            ("body", "BLOB"),
            ("etag", "TEXT"),
            ("path", "TEXT"),
            ("size", "INTEGER"),
            ("updated_at", "INTEGER"),
        ],
    ),
    (
        "throttle",
        &[("failures", "INTEGER"), ("key", "TEXT"), ("locked_until", "INTEGER"), ("window_start", "INTEGER")],
    ),
    ("totp", &[("account_id", "INTEGER"), ("last_step", "INTEGER"), ("sealed_secret", "BLOB")]),
    (
        "unlock_methods",
        &[
            ("account_id", "INTEGER"),
            ("credential_id", "BLOB"),
            ("kind", "TEXT"),
            ("params", "TEXT"),
            ("salt", "BLOB"),
        ],
    ),
];

#[test]
fn schema_v3_is_pinned() {
    let (_scratch, store) = open("store-schema");
    let found = store.read(|tx| tx.schema()).expect("schema");
    let found: Vec<(String, Vec<(String, String)>)> = found.into_iter().map(|t| (t.name, t.columns)).collect();
    let expected: Vec<(String, Vec<(String, String)>)> = SCHEMA_V3
        .iter()
        .map(|(table, columns)| {
            (table.to_string(), columns.iter().map(|(c, t)| (c.to_string(), t.to_string())).collect())
        })
        .collect();
    assert_eq!(found, expected);
    let columns: usize = SCHEMA_V3.iter().map(|(_, c)| c.len()).sum();
    println!("Schema pin: {} tables, {columns} columns", SCHEMA_V3.len());
}

/// Makes `<dir>/server/server.sqlite3` by hand with `setup` and returns the data directory.
fn hand_made(tag: &str, setup: impl FnOnce(&rusqlite::Connection)) -> Scratch {
    let scratch = Scratch::new(tag);
    let server = scratch.join("server");
    std::fs::create_dir(&server).expect("server dir");
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o700)).expect("mode");
    let conn = rusqlite::Connection::open(server.join("server.sqlite3")).expect("create");
    setup(&conn);
    drop(conn);
    scratch
}

fn db_bytes(scratch: &Scratch) -> Vec<u8> {
    std::fs::read(scratch.join("server/server.sqlite3")).expect("read the database file")
}

fn siblings(scratch: &Scratch) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(scratch.join("server"))
        .expect("read the server directory")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_database_with_a_newer_schema_is_refused_and_left_as_it_was() {
    let scratch = hand_made("store-newer", |conn| {
        conn.execute_batch("CREATE TABLE future (x INTEGER); INSERT INTO future VALUES (1);").expect("table");
        conn.pragma_update(None, "application_id", holzwave_server::store::APPLICATION_ID).expect("id");
        conn.pragma_update(None, "user_version", 4).expect("version");
    });
    let before = db_bytes(&scratch);
    let refused = Store::open(scratch.path()).expect_err("must refuse");
    assert_eq!(refused, StoreError::NewerSchema { found: 4, supported: 3 });
    let sentence = refused.to_string();
    println!("Newer schema sentence: {sentence}");
    assert!(sentence.contains("version 4") && sentence.contains("up to 3"), "{sentence}");
    assert_eq!(db_bytes(&scratch), before, "the bytes are unchanged");
    assert_eq!(siblings(&scratch), ["server.sqlite3"], "no -wal or -shm file was left behind");
}

#[test]
fn a_file_that_is_not_ours_is_refused_and_left_untouched() {
    let random = {
        let mut bytes = vec![0u8; 8192];
        getrandom::fill(&mut bytes).expect("random");
        bytes
    };
    let cases: Vec<(&str, Box<dyn FnOnce(&Scratch)>)> = vec![
        ("random", Box::new(move |s| {
            std::fs::write(s.join("server/server.sqlite3"), &random).expect("write");
        })),
        ("short", Box::new(|s| {
            std::fs::write(s.join("server/server.sqlite3"), b"not a database").expect("write");
        })),
    ];
    for (name, fill) in cases {
        let scratch = Scratch::new(&format!("store-foreign-{name}"));
        std::fs::create_dir(scratch.join("server")).expect("dir");
        fill(&scratch);
        std::fs::set_permissions(scratch.join("server/server.sqlite3"), std::fs::Permissions::from_mode(0o644))
            .expect("mode");
        let before = db_bytes(&scratch);
        assert_eq!(Store::open(scratch.path()).expect_err(name), StoreError::NotOurDatabase, "{name}");
        assert_eq!(db_bytes(&scratch), before, "{name}: the bytes are unchanged");
        assert_eq!(mode_of(&scratch.join("server/server.sqlite3")), 0o644, "{name}: the mode is unchanged");
    }
    // A real SQLite database of something else, with and without an application id.
    for (name, app_id) in [("other-app", 42i64), ("no-app", 0)] {
        let scratch = hand_made(&format!("store-foreign-{name}"), |conn| {
            conn.execute_batch("CREATE TABLE notes (t TEXT); INSERT INTO notes VALUES ('keep me');").expect("table");
            conn.pragma_update(None, "application_id", app_id).expect("id");
        });
        let before = db_bytes(&scratch);
        assert_eq!(Store::open(scratch.path()).expect_err(name), StoreError::NotOurDatabase, "{name}");
        assert_eq!(db_bytes(&scratch), before, "{name}: the bytes are unchanged");
        assert_eq!(siblings(&scratch), ["server.sqlite3"], "{name}");
    }
    assert_eq!(
        StoreError::NotOurDatabase.to_string().lines().count(),
        1,
        "the refusal is one sentence"
    );
}

/// Turns the database of a closed store back into one of schema version 1: no `last_signin` column, none of the tables of
/// version 3 and `user_version` 1.
fn downgrade_to_v1(scratch: &Scratch) {
    let conn = rusqlite::Connection::open(scratch.join("server/server.sqlite3")).expect("a second connection");
    conn.execute_batch(
        "DROP TABLE devices; DROP TABLE device_grants; DROP TABLE server_settings; DROP TABLE sync_blobs; \
         ALTER TABLE accounts DROP COLUMN last_signin;",
    )
    .expect("drop the column and the tables");
    conn.pragma_update(None, "user_version", 1).expect("version");
}

fn columns_of_accounts(scratch: &Scratch) -> Vec<(String, String, bool)> {
    let conn = rusqlite::Connection::open(scratch.join("server/server.sqlite3")).expect("a second connection");
    let mut stmt = conn.prepare("SELECT name, type, \"notnull\" FROM pragma_table_info('accounts') ORDER BY cid").expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
}

#[test]
fn tracer_migration_a_version_1_database_gets_a_nullable_last_signin_and_keeps_its_rows() {
    let scratch = Scratch::new("store-migrate-2");
    let (alice, bob) = {
        let store = Store::open(scratch.path()).expect("open");
        let alice = make(&store, "alice", Role::Admin);
        let bob = make(&store, "bob", Role::Member);
        (alice, bob)
    };
    downgrade_to_v1(&scratch);
    assert!(!columns_of_accounts(&scratch).iter().any(|(name, _, _)| name == "last_signin"), "the file is a version 1 file");

    let store = Store::open(scratch.path()).expect("a version 1 file opens and is migrated");
    assert_eq!(store.read(|tx| tx.diagnostics()).expect("diagnostics").user_version, 3);
    let columns = columns_of_accounts(&scratch);
    let added = columns.iter().find(|(name, _, _)| name == "last_signin").expect("the column exists");
    assert_eq!(added, &("last_signin".to_owned(), "INTEGER".to_owned(), false), "nullable INTEGER: {columns:?}");
    assert_eq!(columns.last().map(|c| c.0.as_str()), Some("last_signin"), "added at the end");

    // The rows are as they were; nobody has signed in.
    assert_eq!(store.account_get(alice.id).expect("read"), Some(alice.clone()));
    assert_eq!(store.account_get(bob.id).expect("read"), Some(bob.clone()));
    for id in [alice.id, bob.id] {
        assert_eq!(store.read(|tx| tx.account_last_signin(id)).expect("read"), None);
    }
    // And it is a forward-only step: open again does nothing more, and the second open sees version 2.
    drop(store);
    let again = Store::open(scratch.path()).expect("open again");
    assert_eq!(again.read(|tx| tx.diagnostics()).expect("diagnostics").user_version, 3);
    println!("Migration: version 1 -> 3, accounts.last_signin INTEGER NULL added at the end, 2 rows kept, no value");
}

#[test]
fn an_empty_file_is_made_into_a_database_and_tightened() {
    let scratch = Scratch::new("store-empty-file");
    std::fs::create_dir(scratch.join("server")).expect("dir");
    std::fs::set_permissions(scratch.join("server"), std::fs::Permissions::from_mode(0o755)).expect("mode");
    let file = scratch.join("server/server.sqlite3");
    std::fs::write(&file, b"").expect("empty file");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("mode");
    let store = Store::open(scratch.path()).expect("an empty file is a new database");
    make(&store, "alice", Role::Admin);
    assert_eq!(mode_of(&file), 0o600);
    assert_eq!(mode_of(&scratch.join("server")), 0o700, "a loose directory is tightened");
}

#[test]
fn a_database_that_carries_our_id_but_no_version_is_migrated() {
    let scratch = hand_made("store-v0", |conn| {
        conn.pragma_update(None, "application_id", holzwave_server::store::APPLICATION_ID).expect("id");
    });
    let store = Store::open(scratch.path()).expect("open");
    assert_eq!(store.read(|tx| tx.diagnostics()).expect("diagnostics").user_version, 3);
    make(&store, "alice", Role::Admin);
}

#[test]
fn two_stores_on_one_directory_see_each_others_writes_and_data_survives_a_reopen() {
    let scratch = Scratch::new("store-two");
    let first = Store::open(scratch.path()).expect("first");
    let second = Store::open(scratch.path()).expect("second");
    let alice = make(&first, "alice", Role::Admin);
    assert_eq!(second.account_by_username("alice").expect("lookup"), Some(alice.clone()));
    let bob = make(&second, "bob", Role::Member);
    assert_eq!(bob.uid, alice.uid + 1, "the second store continues after the first");
    assert_eq!(first.account_get(bob.id).expect("get"), Some(bob.clone()));
    drop((first, second));
    let again = Store::open(scratch.path()).expect("reopen");
    assert_eq!(again.read(|tx| tx.account_list()).expect("list"), vec![alice, bob]);
}

#[test]
fn a_link_in_place_of_the_database_or_its_directory_is_refused() {
    let scratch = Scratch::new("store-links");
    let elsewhere = scratch.write_file("elsewhere.sqlite3", b"", 0o600);
    std::fs::create_dir(scratch.join("data")).expect("data dir");
    std::fs::create_dir(scratch.join("data/server")).expect("server dir");
    std::os::unix::fs::symlink(&elsewhere, scratch.join("data/server/server.sqlite3")).expect("link");
    let refused = Store::open(&scratch.join("data"));
    assert!(matches!(refused, Err(StoreError::Storage(_))), "a link at the database path: {refused:?}");
    assert_eq!(std::fs::read(&elsewhere).expect("read"), b"", "the target was not touched");

    let other = Scratch::new("store-links-dir");
    std::fs::create_dir(other.join("real")).expect("real dir");
    std::os::unix::fs::symlink(other.join("real"), other.join("server")).expect("link");
    let refused = Store::open(other.path());
    assert!(matches!(refused, Err(StoreError::Storage(_))), "a link at the server directory: {refused:?}");
    assert!(std::fs::read_dir(other.join("real")).expect("read dir").next().is_none());
}

#[test]
fn error_texts_carry_no_values_paths_or_sql() {
    let (scratch, store) = open("store-text");
    let secret_name = "canary-username-0123";
    make(&store, secret_name, Role::Admin);
    let conflict = store
        .write(|tx| tx.account_create(&NewAccount::new(secret_name, Role::Member), RANGE, NOW))
        .expect_err("conflict");
    for text in [
        conflict.to_string(),
        format!("{conflict:?}"),
        StoreError::from(rusqlite::Error::InvalidQuery).to_string(),
        StoreError::NotFound.to_string(),
    ] {
        assert!(!text.contains(secret_name), "an error text must not hold a value: {text}");
        assert!(!text.contains(scratch.path().to_str().expect("utf-8 path")), "nor a path: {text}");
        assert!(!text.to_lowercase().contains("select ") && !text.to_lowercase().contains("insert "), "nor SQL: {text}");
    }
}
