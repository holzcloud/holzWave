//! The state machine of the Android secret store, through the public API only: new, locked, unlocked and lost, the
//! protection switch, unlock once per session and the lock after five minutes with the page hidden. The keystore and the
//! clock are fakes, so this proves the logic of the vault and not the platform. A canary string stands in for a secret and
//! is searched for in everything the vault shows.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use holzwave_core::secrets::SecretStore;
use holzwave_core::vault::{
    BACKGROUND_LOCK_MS, FakeClock, FakeKeystore, LOCKED_TEXT, LostReason, MemoryFiles, Notice, Protection, VaultError,
    VaultState, VaultStore,
};
use holzwave_core::Error;

const CANARY: &str = "CANARY-77c1e0-do-not-leak";
const T0: u64 = 10_000;

struct Rig {
    keystore: Arc<FakeKeystore>,
    files: Arc<MemoryFiles>,
    clock: Arc<FakeClock>,
}

impl Rig {
    /// A device with a screen lock.
    fn new() -> Rig {
        Rig { keystore: Arc::new(FakeKeystore::new()), files: Arc::new(MemoryFiles::default()), clock: Arc::new(FakeClock::new(T0)) }
    }

    /// A device without a screen lock.
    fn no_lock() -> Rig {
        let rig = Rig::new();
        rig.keystore.set_screen_lock(false);
        rig
    }

    /// The vault as the process starts: the same file, keystore and clock.
    fn open(&self) -> VaultStore {
        VaultStore::open_with_files("vault.bin", self.keystore.clone(), self.clock.clone(), self.files.clone())
    }
}

fn is_locked(error: &Error) -> bool {
    matches!(error, Error::Io(_)) && error.to_string() == LOCKED_TEXT
}

/// An unlocked auth vault holding one secret.
fn unlocked(rig: &Rig) -> VaultStore {
    let vault = rig.open();
    vault.unlock().unwrap();
    vault.set("ssh:a", CANARY).unwrap();
    vault
}

#[test]
fn a_new_auth_vault_reads_nothing_refuses_writes_and_is_made_by_one_unlock() {
    let rig = Rig::new();
    let vault = rig.open();
    assert_eq!(vault.state(), VaultState::New);
    assert_eq!(vault.get("ssh:a").unwrap(), None);
    assert!(is_locked(&vault.set("ssh:a", "x").unwrap_err()));
    assert!(is_locked(&vault.delete("ssh:a").unwrap_err()));
    assert_eq!(rig.keystore.prompts(), 0);
    assert!(rig.files.bytes().is_none(), "nothing is written before the unlock");

    vault.unlock().unwrap();
    assert_eq!(rig.keystore.prompts(), 1, "one prompt for the wrap");
    assert_eq!(vault.state(), VaultState::Unlocked);
    vault.set("ssh:a", "one").unwrap();
    vault.set("ssh:b", "two").unwrap();
    assert_eq!(vault.get("ssh:a").unwrap().as_deref(), Some("one"));
    assert_eq!(rig.keystore.prompts(), 1, "no prompt for a secret");
    vault.unlock().unwrap();
    assert_eq!(rig.keystore.prompts(), 1, "unlocking an unlocked vault asks nothing");
}

#[test]
fn after_a_restart_the_vault_is_locked_and_an_unlock_is_one_prompt() {
    let rig = Rig::new();
    drop(unlocked(&rig));
    let prompts = rig.keystore.prompts();

    let vault = rig.open();
    assert_eq!(vault.state(), VaultState::Locked);
    assert!(is_locked(&vault.get("ssh:a").unwrap_err()));
    assert!(is_locked(&vault.set("ssh:a", "x").unwrap_err()));
    assert!(is_locked(&vault.delete("ssh:a").unwrap_err()));
    assert_eq!(rig.keystore.prompts(), prompts, "a refused call shows no prompt");

    let before = rig.files.bytes();
    rig.keystore.cancel_next_prompt();
    assert_eq!(vault.unlock(), Err(VaultError::Cancelled));
    assert_eq!(vault.state(), VaultState::Locked);
    assert_eq!(rig.files.bytes(), before, "a cancelled unlock changes nothing");

    vault.unlock().unwrap();
    assert_eq!(vault.state(), VaultState::Unlocked);
    assert_eq!(rig.keystore.prompts(), prompts + 2, "the cancelled prompt and the one that worked");
    assert_eq!(vault.get("ssh:a").unwrap().as_deref(), Some(CANARY));
    vault.set("ssh:c", "more").unwrap();
    assert_eq!(rig.keystore.prompts(), prompts + 2);
}

#[test]
fn a_device_without_a_screen_lock_gets_a_plain_vault_and_can_protect_it_later() {
    let rig = Rig::no_lock();
    let vault = rig.open();
    let status = vault.status();
    assert_eq!((status.state, status.protection, status.notice), (VaultState::New, Protection::Plain, Notice::NoLock));
    vault.set("ssh:a", CANARY).unwrap();
    assert_eq!(rig.keystore.prompts(), 0, "no prompt at all");
    let status = vault.status();
    assert_eq!(status.protection, Protection::Plain);
    assert!(!status.can_protect);
    assert_eq!(status.notice, Notice::NoLock);
    assert_eq!(vault.set_protection(Protection::Auth), Err(VaultError::NoScreenLock));
    assert_eq!(vault.get("ssh:a").unwrap().as_deref(), Some(CANARY), "a refused switch changes nothing");

    rig.keystore.set_screen_lock(true);
    let status = vault.status();
    assert!(status.can_protect);
    assert_eq!(status.notice, Notice::None);

    let data_before = rig.files.bytes().unwrap();
    vault.set_protection(Protection::Auth).unwrap();
    assert_eq!(rig.keystore.prompts(), 1, "one prompt to re-wrap");
    assert_eq!(rig.keystore.key_count(), 1, "the old wrapping key is retired after the new file is written");
    let status = vault.status();
    assert_eq!((status.protection, status.can_protect), (Protection::Auth, false));
    assert_ne!(rig.files.bytes().unwrap(), data_before);
    assert_eq!(rig.files.bytes().unwrap()[4], 1, "the header says auth");

    drop(vault);
    let vault = rig.open();
    assert_eq!(vault.state(), VaultState::Locked, "an auth vault asks again after a restart");
    vault.unlock().unwrap();
    assert_eq!(vault.get("ssh:a").unwrap().as_deref(), Some(CANARY), "no secret was lost");
}

#[test]
fn switching_protection_off_needs_no_prompt_beyond_the_unlock_that_happened() {
    let rig = Rig::new();
    let vault = unlocked(&rig);
    let prompts = rig.keystore.prompts();
    vault.set_protection(Protection::Plain).unwrap();
    assert_eq!(rig.keystore.prompts(), prompts, "plain needs no prompt");
    assert_eq!(rig.keystore.key_count(), 1);
    assert_eq!(vault.status().protection, Protection::Plain);
    assert_eq!(vault.get("ssh:a").unwrap().as_deref(), Some(CANARY));
    vault.set_protection(Protection::Plain).unwrap();

    drop(vault);
    let vault = rig.open();
    assert_eq!(vault.state(), VaultState::Unlocked, "opens unlocked");
    assert_eq!(rig.keystore.prompts(), prompts, "with no prompt");
    assert_eq!(vault.get("ssh:a").unwrap().as_deref(), Some(CANARY));
}

#[test]
fn switching_off_a_locked_auth_vault_asks_for_the_unlock_first_and_a_new_one_is_made_plain_at_once() {
    let rig = Rig::new();
    drop(unlocked(&rig));
    let prompts = rig.keystore.prompts();
    let vault = rig.open();
    assert_eq!(vault.state(), VaultState::Locked);
    rig.keystore.cancel_next_prompt();
    assert_eq!(vault.set_protection(Protection::Plain), Err(VaultError::Cancelled));
    assert_eq!(vault.status().protection, Protection::Auth);
    vault.set_protection(Protection::Plain).unwrap();
    assert_eq!(rig.keystore.prompts(), prompts + 2, "the unlock, nothing for the switch");
    assert_eq!(vault.get("ssh:a").unwrap().as_deref(), Some(CANARY));

    let fresh = Rig::new();
    let vault = fresh.open();
    vault.set_protection(Protection::Plain).unwrap();
    assert_eq!(fresh.keystore.prompts(), 0);
    assert_eq!(vault.state(), VaultState::Unlocked);
    assert_eq!(vault.status().protection, Protection::Plain);
    vault.set("a", "b").unwrap();
    assert_eq!(fresh.open().get("a").unwrap().as_deref(), Some("b"));
}

#[test]
fn a_failed_switch_leaves_a_vault_that_still_opens() {
    let rig = Rig::new();
    let vault = unlocked(&rig);
    rig.files.fail_writes(true);
    assert!(vault.set_protection(Protection::Plain).is_err());
    assert_eq!(vault.status().protection, Protection::Auth);
    rig.files.fail_writes(false);
    drop(vault);
    let vault = rig.open();
    vault.unlock().unwrap();
    assert_eq!(vault.get("ssh:a").unwrap().as_deref(), Some(CANARY), "the old wrapping key was kept");

    // A cancelled prompt of a switch to auth changes nothing either.
    let plain = Rig::no_lock();
    let vault = plain.open();
    vault.set("ssh:a", CANARY).unwrap();
    plain.keystore.set_screen_lock(true);
    plain.keystore.cancel_next_prompt();
    assert_eq!(vault.set_protection(Protection::Auth), Err(VaultError::Cancelled));
    assert_eq!(vault.status().protection, Protection::Plain);
    drop(vault);
    assert_eq!(plain.open().get("ssh:a").unwrap().as_deref(), Some(CANARY));
}

#[test]
fn a_lost_key_is_a_lost_vault_that_is_reset_on_request_and_never_read_as_empty() {
    // An auth vault finds out at the unlock.
    let rig = Rig::new();
    drop(unlocked(&rig));
    rig.keystore.lose_key();
    let vault = rig.open();
    assert_eq!(vault.state(), VaultState::Locked, "it cannot know before it asks");
    assert_eq!(vault.unlock(), Err(VaultError::Lost(LostReason::KeyGone)));
    assert_eq!(vault.state(), VaultState::Lost);
    let status = vault.status();
    assert_eq!((status.state.as_str(), status.reason), ("lost", Some(LostReason::KeyGone)));
    assert!(is_locked(&vault.get("ssh:a").unwrap_err()));
    assert!(is_locked(&vault.set("ssh:a", "x").unwrap_err()));
    assert!(is_locked(&vault.delete("ssh:a").unwrap_err()));
    assert_eq!(vault.set_protection(Protection::Plain), Err(VaultError::Lost(LostReason::KeyGone)));
    assert!(rig.files.bytes().is_some(), "nothing is deleted until reset");

    vault.reset().unwrap();
    assert_eq!(vault.state(), VaultState::New);
    assert!(rig.files.bytes().is_none());
    assert_eq!(rig.keystore.key_count(), 0);
    assert_eq!(vault.get("ssh:a").unwrap(), None, "no secret is left");
    vault.unlock().unwrap();
    vault.set("ssh:a", "again").unwrap();
    assert_eq!(vault.get("ssh:a").unwrap().as_deref(), Some("again"));

    // A plain vault finds out at the open.
    let plain = Rig::no_lock();
    let vault = plain.open();
    vault.set("ssh:a", CANARY).unwrap();
    drop(vault);
    plain.keystore.lose_key();
    let vault = plain.open();
    assert_eq!(vault.state(), VaultState::Lost);
    assert_eq!(vault.lost_reason(), Some(LostReason::KeyGone));
    assert!(is_locked(&vault.get("ssh:a").unwrap_err()));
}

#[test]
fn a_file_that_fails_authentication_is_lost_as_corrupt_and_goes_the_same_reset_path() {
    let probe = Rig::new();
    drop(unlocked(&probe));
    let last = probe.files.bytes().unwrap().len() - 1;
    for i in [0, 3, 4, 8, 20, last] {
        // A fresh device each time: a reset removes the wrapping key as well.
        let rig = Rig::new();
        drop(unlocked(&rig));
        let good = rig.files.bytes().unwrap();
        let mut bad = good.clone();
        bad[i] ^= 0x40;
        rig.files.replace(bad);
        let vault = rig.open();
        // The structure is checked at the open; the seal needs the data key, so the rest shows at the unlock.
        let answer = match vault.state() {
            VaultState::Lost => Err(VaultError::Lost(LostReason::Corrupt)),
            _ => vault.unlock(),
        };
        assert_eq!(answer, Err(VaultError::Lost(LostReason::Corrupt)), "byte {i}");
        assert_eq!(vault.state(), VaultState::Lost, "byte {i}");
        assert_eq!(vault.status().reason, Some(LostReason::Corrupt), "byte {i}");
        assert_eq!(rig.files.bytes().unwrap().len(), good.len(), "nothing deleted");
        vault.reset().unwrap();
        assert_eq!(vault.state(), VaultState::New);
        assert!(rig.files.bytes().is_none());
        vault.unlock().unwrap();
        vault.set("ssh:a", "fresh").unwrap();
    }
}

#[test]
fn a_file_replaced_while_the_vault_is_unlocked_makes_it_lost_and_not_empty() {
    let rig = Rig::new();
    let vault = unlocked(&rig);
    let mut other = rig.files.bytes().unwrap();
    let last = other.len() - 1;
    other[last] ^= 1;
    rig.files.replace(other);
    assert!(is_locked(&vault.get("ssh:a").unwrap_err()));
    assert_eq!(vault.state(), VaultState::Lost);
    assert_eq!(vault.status().reason, Some(LostReason::Corrupt));

    let rig = Rig::new();
    let vault = unlocked(&rig);
    rig.files.replace(Vec::new());
    assert!(is_locked(&vault.set("ssh:b", "x").unwrap_err()));
    assert_eq!(vault.state(), VaultState::Lost);
}

#[test]
fn the_page_hidden_for_five_minutes_locks_an_auth_vault_and_the_data_key_goes() {
    let rig = Rig::new();
    let vault = unlocked(&rig);
    vault.hidden(true);
    rig.clock.set(T0 + BACKGROUND_LOCK_MS - 1_000);
    vault.hidden(false);
    assert_eq!(vault.state(), VaultState::Unlocked, "299 seconds is not enough");
    assert_eq!(vault.get("ssh:a").unwrap().as_deref(), Some(CANARY));

    rig.clock.set(T0 + 2 * BACKGROUND_LOCK_MS);
    vault.hidden(true);
    rig.clock.set(T0 + 3 * BACKGROUND_LOCK_MS);
    vault.hidden(false);
    assert_eq!(vault.state(), VaultState::Locked, "300 seconds locks");
    assert!(is_locked(&vault.get("ssh:a").unwrap_err()));
    let prompts = rig.keystore.prompts();
    vault.unlock().unwrap();
    assert_eq!(rig.keystore.prompts(), prompts + 1, "the key had to be unwrapped again");
    assert_eq!(vault.get("ssh:a").unwrap().as_deref(), Some(CANARY));
}

#[test]
fn the_lock_comes_at_exactly_300_seconds_a_plain_vault_never_locks_on_a_clock_and_lock_is_at_once() {
    assert_eq!(BACKGROUND_LOCK_MS, 300_000);
    let rig = Rig::new();
    let vault = unlocked(&rig);
    vault.hidden(true);
    rig.clock.set(T0 + 299_999);
    vault.tick();
    assert_eq!(vault.state(), VaultState::Unlocked);
    rig.clock.set(T0 + 300_000);
    vault.hidden(false);
    assert_eq!(vault.state(), VaultState::Locked);

    let plain = Rig::no_lock();
    let vault = plain.open();
    vault.set("ssh:a", CANARY).unwrap();
    vault.hidden(true);
    plain.clock.set(T0 + 10 * BACKGROUND_LOCK_MS);
    vault.tick();
    vault.hidden(false);
    assert_eq!(vault.state(), VaultState::Unlocked, "no clock locks a plain vault");
    assert_eq!(vault.get("ssh:a").unwrap().as_deref(), Some(CANARY));
    plain.clock.set(1);
    vault.tick();
    assert_eq!(vault.state(), VaultState::Unlocked, "not even a clock that went back");

    let rig = Rig::new();
    let vault = unlocked(&rig);
    vault.lock();
    assert_eq!(vault.state(), VaultState::Locked);
    assert!(is_locked(&vault.get("ssh:a").unwrap_err()));
}

#[test]
fn a_second_hidden_does_not_move_the_start() {
    let rig = Rig::new();
    let vault = unlocked(&rig);
    vault.hidden(true);
    rig.clock.set(T0 + 200_000);
    vault.hidden(true);
    rig.clock.set(T0 + 300_000);
    vault.hidden(false);
    assert_eq!(vault.state(), VaultState::Locked, "counted from the first hidden");
}

#[test]
fn a_timer_locks_while_the_page_is_still_hidden_and_a_clock_that_goes_back_locks() {
    let rig = Rig::new();
    let vault = unlocked(&rig);
    vault.hidden(true);
    rig.clock.set(T0 + 100_000);
    vault.tick();
    assert_eq!(vault.state(), VaultState::Unlocked);
    rig.clock.set(T0 + BACKGROUND_LOCK_MS + 1);
    vault.tick();
    assert_eq!(vault.state(), VaultState::Locked, "the timer locks with the page still hidden");

    let rig = Rig::new();
    let vault = unlocked(&rig);
    rig.clock.set(T0 + 50_000);
    vault.tick();
    assert_eq!(vault.state(), VaultState::Unlocked);
    rig.clock.set(T0 + 10_000);
    vault.tick();
    assert_eq!(vault.state(), VaultState::Locked, "a bad clock never keeps the vault open");

    // Even a read does not slip through after the five minutes when nobody ticked.
    let rig = Rig::new();
    let vault = unlocked(&rig);
    vault.hidden(true);
    rig.clock.set(T0 + BACKGROUND_LOCK_MS);
    assert!(is_locked(&vault.get("ssh:a").unwrap_err()));
}

#[test]
fn a_get_set_and_lock_from_three_threads_never_deadlock_and_never_answer_after_the_lock() {
    for round in 0..200 {
        let rig = Rig::new();
        let vault = Arc::new(unlocked(&rig));
        let barrier = Arc::new(Barrier::new(3));
        let lock_returned = Arc::new(AtomicBool::new(false));

        let mut readers = Vec::new();
        for n in 0..2 {
            let (vault, barrier, lock_returned) = (vault.clone(), barrier.clone(), lock_returned.clone());
            readers.push(thread::spawn(move || {
                barrier.wait();
                let mut leaked = false;
                for i in 0..50 {
                    let after = lock_returned.load(Ordering::SeqCst);
                    let answer = if n == 0 { vault.get("ssh:a").map(|s| s.is_some()) } else { vault.set("ssh:b", "x").map(|()| true) };
                    if after && answer.is_ok() {
                        leaked = true;
                    }
                    if i % 7 == 0 {
                        thread::yield_now();
                    }
                }
                leaked
            }));
        }
        barrier.wait();
        thread::yield_now();
        vault.lock();
        lock_returned.store(true, Ordering::SeqCst);

        for reader in readers {
            assert!(!reader.join().unwrap(), "round {round}: a secret was answered after the lock returned");
        }
        assert_eq!(vault.state(), VaultState::Locked, "round {round}");
        assert!(is_locked(&vault.get("ssh:a").unwrap_err()));
    }
}

#[test]
fn two_unlocks_at_once_share_one_prompt() {
    let rig = Rig::new();
    drop(unlocked(&rig));
    let prompts = rig.keystore.prompts();
    for _ in 0..20 {
        let vault = Arc::new(rig.open());
        vault.lock();
        let before = rig.keystore.prompts();
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let (vault, barrier) = (vault.clone(), barrier.clone());
                thread::spawn(move || {
                    barrier.wait();
                    vault.unlock()
                })
            })
            .collect();
        barrier.wait();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), Ok(()));
        }
        assert_eq!(rig.keystore.prompts(), before + 1, "one prompt for two callers");
        assert_eq!(vault.get("ssh:a").unwrap().as_deref(), Some(CANARY));
    }
    assert!(rig.keystore.prompts() > prompts);
}

#[test]
fn the_status_has_fixed_names_and_never_prompts() {
    let rig = Rig::new();
    let vault = rig.open();
    let json = |v: &VaultStore| serde_json::to_value(v.status()).unwrap();
    assert_eq!(
        json(&vault),
        serde_json::json!({
            "state": "new", "protection": "auth", "has_screen_lock": true, "strongbox": false,
            "can_protect": false, "notice": "none", "reason": null
        })
    );
    vault.unlock().unwrap();
    assert_eq!(json(&vault)["state"], "unlocked");
    vault.lock();
    assert_eq!(json(&vault)["state"], "locked");
    rig.keystore.set_strongbox(true);
    assert_eq!(json(&vault)["strongbox"], true);

    rig.keystore.lose_key();
    vault.unlock().unwrap_err();
    let lost = json(&vault);
    assert_eq!((lost["state"].as_str(), lost["reason"].as_str()), (Some("lost"), Some("key_gone")));

    let plain = Rig::no_lock();
    let vault = plain.open();
    let status = json(&vault);
    assert_eq!((status["protection"].as_str(), status["notice"].as_str()), (Some("plain"), Some("no_lock")));

    let prompts = rig.keystore.prompts() + plain.keystore.prompts();
    for _ in 0..5 {
        vault.status();
    }
    assert_eq!(rig.keystore.prompts() + plain.keystore.prompts(), prompts);
    // The names the page gets, in one place.
    assert_eq!(
        [VaultState::New, VaultState::Locked, VaultState::Unlocked, VaultState::Lost].map(VaultState::as_str),
        ["new", "locked", "unlocked", "lost"]
    );
    assert_eq!([Protection::Auth, Protection::Plain].map(Protection::as_str), ["auth", "plain"]);
    assert_eq!([Notice::None, Notice::NoLock].map(Notice::as_str), ["none", "no_lock"]);
    assert_eq!([LostReason::KeyGone, LostReason::Corrupt].map(LostReason::as_str), ["key_gone", "corrupt"]);
}

#[test]
fn nothing_the_vault_says_holds_the_canary() {
    let rig = Rig::new();
    let vault = unlocked(&rig);
    let mut shown = vec![format!("{vault:?}"), format!("{:?}", vault.status()), format!("{:?}", rig.keystore)];
    vault.lock();
    shown.push(vault.get("ssh:a").unwrap_err().to_string());
    shown.push(format!("{:?}", vault.get("ssh:a").unwrap_err()));
    rig.keystore.cancel_next_prompt();
    shown.push(format!("{:?}", vault.unlock().unwrap_err()));
    rig.keystore.lose_key();
    let err = vault.unlock().unwrap_err();
    shown.push(format!("{err} {err:?}"));
    for text in &shown {
        assert!(!text.contains(CANARY), "{text}");
    }
    let bytes = rig.files.bytes().unwrap();
    assert!(!bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()));
    assert!(!bytes.windows(5).any(|w| w == b"ssh:a"));
}
