//! The six commands that drive the vault: on a host with no vault, on a host with the real vault over a fake keystore and a
//! fake clock, and as rows of the one command table (deny by default, unavailable in the browser).
//!
//! Nothing here needs Android: `FakeKeystore` and `FakeClock` stand in for the Keystore and the system clock. A real prompt,
//! a suspended phone and StrongBox are not run.

mod support;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use holzwave_core::vault::{FakeKeystore, LOCKED_TEXT};
use holzwave_host::commands::registry::{COMMANDS, CAP_DEFAULT, Mode, NOT_IN_BROWSER, Outcome, Scope, Shell, Web, lookup};
use holzwave_host::commands::vault::{
    NO_VAULT, VaultStatusMsg, vault_hidden, vault_lock, vault_protect, vault_reset, vault_status, vault_unlock,
};
use holzwave_host::locked::{SECRETS_LOCKED, is_locked};
use holzwave_host::seams::{Host, NeutralHost};
use serde_json::{Value, json};
use support::{Fixture, WRITE_DETAIL};

const SIX: [&str; 6] = ["vault_status", "vault_unlock", "vault_lock", "vault_protect", "vault_reset", "vault_hidden"];

fn json_of(status: &VaultStatusMsg) -> Value {
    serde_json::to_value(status).expect("a status is JSON")
}

#[test]
fn a_host_has_no_vault_unless_it_says_so_and_a_host_that_has_one_hands_it_out() {
    assert!(NeutralHost::new().vault().is_none());
    let (_dir, core) = Fixture::without_vault("none");
    assert!(core.host().vault().is_none(), "the desktop-style test host has no vault");

    let fixture = Fixture::new("some");
    let vault = fixture.core.host().vault().expect("the vault host has a vault");
    assert!(std::sync::Arc::ptr_eq(&vault, &fixture.vault), "the vault the host hands out is the one the core reads");
    // And the status is the vault's own, with the lowercase names of the core.
    assert_eq!(
        json_of(&vault_status(&fixture.core)),
        json!({
            "state": "new", "protection": "auth", "has_screen_lock": true, "strongbox": false, "can_protect": false,
            "notice": "none", "reason": null
        })
    );
}

#[test]
fn the_locked_text_of_the_host_is_the_text_of_the_vault_and_its_error_is_recognised() {
    assert_eq!(SECRETS_LOCKED, LOCKED_TEXT);
    assert_eq!(SECRETS_LOCKED, "secrets are locked");
    let fixture = Fixture::new("locked-text");
    use holzwave_core::secrets::SecretStore;
    // A new vault on a device with a screen lock holds no secret to write yet, and says locked.
    let refused = fixture.vault.set("ssh:a", "x").expect_err("a new auth vault refuses a write");
    assert!(is_locked(&refused), "{refused}");
    vault_unlock(&fixture.core).expect("created");
    fixture.vault.set("ssh:a", "x").expect("set");
    vault_lock(&fixture.core).expect("locked");
    let refused = fixture.vault.get("ssh:a").expect_err("a locked vault refuses a read");
    assert!(is_locked(&refused), "{refused}");
    assert_eq!(refused.to_string(), SECRETS_LOCKED);
    // An error that is not the vault's lock is not recognised as one.
    assert!(!is_locked(&holzwave_core::Error::Unsupported(SECRETS_LOCKED.to_owned())));
}

#[test]
fn on_a_host_with_no_vault_the_status_says_none_and_the_other_five_refuse_with_one_text_and_change_nothing() {
    let (dir, core) = Fixture::without_vault("novault");
    let status = vault_status(&core);
    assert_eq!(status, VaultStatusMsg::none());
    assert_eq!(
        json_of(&status),
        json!({
            "state": "none", "protection": "plain", "has_screen_lock": false, "strongbox": false, "can_protect": false,
            "notice": "none", "reason": null
        })
    );
    assert_eq!(NO_VAULT, "no secret vault on this device");
    assert_eq!(vault_unlock(&core), Err(NO_VAULT.to_owned()));
    assert_eq!(vault_lock(&core), Err(NO_VAULT.to_owned()));
    assert_eq!(vault_protect(&core, true), Err(NO_VAULT.to_owned()));
    assert_eq!(vault_protect(&core, false), Err(NO_VAULT.to_owned()));
    assert_eq!(vault_reset(&core), Err(NO_VAULT.to_owned()));
    assert_eq!(vault_hidden(&core, true), Err(NO_VAULT.to_owned()));
    assert_eq!(vault_status(&core), VaultStatusMsg::none(), "nothing changed");
    assert!(!dir.join("secrets.vault").exists(), "no file was made");
}

#[test]
fn unlock_creates_a_new_vault_and_a_cancelled_prompt_a_lost_key_and_a_failed_write_are_three_fixed_words() {
    let fixture = Fixture::new("unlock");
    let status = vault_unlock(&fixture.core).expect("a new vault is created");
    assert_eq!(status.state, "unlocked");
    assert_eq!(status.protection, "auth");
    assert!(fixture.dir.join("secrets.vault").is_file());

    // Locked, then a cancelled prompt: the word is `cancelled` and the vault stays locked.
    assert_eq!(vault_lock(&fixture.core).expect("lock").state, "locked");
    fixture.keystore.cancel_next_prompt();
    assert_eq!(vault_unlock(&fixture.core), Err("cancelled".to_owned()));
    assert_eq!(vault_status(&fixture.core).state, "locked", "a cancelled prompt changes nothing");
    // The next unlock works.
    assert_eq!(vault_unlock(&fixture.core).expect("unlock").state, "unlocked");

    // The key is gone (the screen lock was removed): `lost`, and the status says why.
    vault_lock(&fixture.core).expect("lock");
    fixture.keystore.lose_key();
    assert_eq!(vault_unlock(&fixture.core), Err("lost".to_owned()));
    let status = vault_status(&fixture.core);
    assert_eq!((status.state, status.reason), ("lost", Some("key_gone")));

    // Any other failure is one fixed word and the text of the failure never reaches the page.
    let other = Fixture::new("unlock-failed");
    other.files.fail.store(true, Ordering::SeqCst);
    let failed = vault_unlock(&other.core);
    assert_eq!(failed, Err("failed".to_owned()));
    assert!(!format!("{failed:?}").contains(WRITE_DETAIL));
    assert!(!format!("{:?}", vault_status(&other.core)).contains(WRITE_DETAIL));
}

#[test]
fn protect_switches_both_ways_refuses_without_a_screen_lock_and_reset_starts_over_from_a_lost_vault() {
    let fixture = Fixture::new("protect");
    vault_unlock(&fixture.core).expect("created");
    let plain = vault_protect(&fixture.core, false).expect("to plain");
    assert_eq!((plain.state, plain.protection, plain.can_protect), ("unlocked", "plain", true));
    let auth = vault_protect(&fixture.core, true).expect("back to auth");
    assert_eq!((auth.state, auth.protection, auth.can_protect), ("unlocked", "auth", false));

    // A device with no screen lock: auth is refused with its word, the vault is plain and the status carries the notice.
    let bare = Fixture::on("protect-bare", FakeKeystore::without_screen_lock());
    assert_eq!(vault_protect(&bare.core, true), Err("no_screen_lock".to_owned()));
    let status = vault_status(&bare.core);
    assert_eq!((status.state, status.protection, status.notice, status.has_screen_lock), ("new", "plain", "no_lock", false));
    assert_eq!(vault_unlock(&bare.core).expect("a plain vault is made at once").state, "unlocked");
    assert_eq!(vault_status(&bare.core).notice, "no_lock", "the notice stays while the device has no lock");

    // A lost vault is reset to a new one; a reset only deletes.
    vault_lock(&fixture.core).expect("lock");
    fixture.keystore.lose_key();
    assert_eq!(vault_unlock(&fixture.core), Err("lost".to_owned()));
    let fresh = vault_reset(&fixture.core).expect("reset");
    assert_eq!((fresh.state, fresh.reason), ("new", None));
    assert_eq!(vault_unlock(&fixture.core).expect("a new vault").state, "unlocked");
}

#[test]
fn hidden_for_five_minutes_locks_an_auth_vault_and_a_shorter_stay_does_not() {
    let fixture = Fixture::new("hidden");
    vault_unlock(&fixture.core).expect("created");
    vault_hidden(&fixture.core, true).expect("hidden");
    fixture.clock.advance(299_000);
    vault_hidden(&fixture.core, false).expect("shown");
    assert_eq!(vault_status(&fixture.core).state, "unlocked", "299 seconds is not enough");

    vault_hidden(&fixture.core, true).expect("hidden");
    fixture.clock.advance(301_000);
    vault_hidden(&fixture.core, false).expect("shown");
    assert_eq!(vault_status(&fixture.core).state, "locked", "301 seconds locks it");
}

#[test]
fn each_of_the_six_is_a_deny_by_default_row_that_the_browser_does_not_have() {
    const BUILD_RS: &str = include_str!("../../app/src-tauri/build.rs");
    const CAPABILITY: &str = include_str!("../../app/src-tauri/capabilities/default.json");
    let (_dir, core) = Fixture::without_vault("rows");
    for name in SIX {
        let row = COMMANDS.iter().find(|row| row.name == name).unwrap_or_else(|| panic!("{name} is not in the table"));
        assert_eq!((row.group, row.scope, row.cap, row.shell, row.web), ("vault", Scope::None, CAP_DEFAULT, Shell::Generated, Web::Unavailable), "{name}");
        assert!(BUILD_RS.contains(&format!("\"{name}\"")), "{name} is not in build.rs");
        assert!(CAPABILITY.contains(&format!("\"allow-{}\"", name.replace('_', "-"))), "{name} is not in the capability");
        // The browser build answers the sentence whatever it is sent, and reads no argument.
        for args in ["{}", "", r#"{"unknown":1}"#, r#"{"on":true}"#] {
            let outcome = lookup(name).expect("a row").call(&core, "main", args);
            assert_eq!(outcome, Outcome::Err(serde_json::to_string(NOT_IN_BROWSER).expect("json")), "{name} {args:?}");
        }
    }
}

#[test]
fn the_rows_that_may_wait_for_the_person_are_async_and_the_others_are_not() {
    let facts = |name: &str| {
        let row = lookup(name).unwrap_or_else(|| panic!("{name} is not a row"));
        let args: Vec<(&str, &str)> = row.args.iter().map(|arg| (arg.name, arg.ty)).collect();
        (row.mode, args, row.answer.replace(' ', ""), row.error)
    };
    let status = "VaultStatusMsg".to_owned();
    assert_eq!(facts("vault_status"), (Mode::Sync, vec![], status.clone(), None));
    assert_eq!(facts("vault_unlock"), (Mode::Async, vec![], status.clone(), Some("String")));
    assert_eq!(facts("vault_lock"), (Mode::Sync, vec![], status.clone(), Some("String")));
    assert_eq!(facts("vault_protect"), (Mode::Async, vec![("on", "bool")], status.clone(), Some("String")));
    assert_eq!(facts("vault_reset"), (Mode::Sync, vec![], status, Some("String")));
    assert_eq!(facts("vault_hidden"), (Mode::Sync, vec![("hidden", "bool")], "()".to_owned(), Some("String")));
}

#[test]
fn the_status_answers_while_an_unlock_waits() {
    let fixture = Fixture::new("responsive");
    // A new vault: the unlock writes the file, and the write waits as a held prompt does.
    fixture.files.delay.store(1500, Ordering::SeqCst);
    let core = fixture.core.clone();
    let waiting = std::thread::spawn(move || vault_unlock(&core));
    let begun = Instant::now();
    while !fixture.files.entered.load(Ordering::SeqCst) {
        assert!(begun.elapsed() < Duration::from_secs(10), "the unlock never began");
        std::thread::sleep(Duration::from_millis(5));
    }

    let asked = Instant::now();
    let status = vault_status(&fixture.core);
    let took = asked.elapsed();
    assert!(!waiting.is_finished(), "the unlock is still waiting");
    assert!(took < Duration::from_millis(500), "the status waited {took:?} for an unlock that holds for 1.5 s");
    assert_eq!(status.state, "new", "the vault is not unlocked until the wait is over");

    assert_eq!(waiting.join().expect("the unlock thread").expect("unlocked").state, "unlocked");
    println!("status answered in {took:?} while the unlock waited");
}
