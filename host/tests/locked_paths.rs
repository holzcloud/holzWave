//! What a locked vault does to the things that need a secret: a new connection, an AI send, the save of a secret and the
//! sync, and what it leaves alone. All of it runs on the desktop with the real `VaultStore` over the fake keystore.
//!
//! The "backend" of the sync is a loopback listener that counts the connections it is given and closes each at once, so
//! a run that reaches the network is counted and a run that does not is zero. A run that does reach it fails, which is
//! the point of the check that a failure of the backend is still an `error`.
//!
//! Not run: Android, a real Keystore prompt, a real provider of the AI chat. The live SSH session that survives the lock
//! is proved in the core's tests (`core/tests/vault_live.rs`), where the in-process SSH server is.

mod support;

use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use holzwave_core::ai::profiles::{KeySlot, Preset};
use holzwave_core::ai::scripted::{Script, ScriptedProvider};
use holzwave_core::ai::types::{Capabilities, Locality};
use holzwave_core::connections::SecretChange;
use holzwave_core::profiles::{Auth, Profile};
use holzwave_core::secrets::SecretStore;
use holzwave_core::vault::FakeKeystore;
use holzwave_host::ai_host::{AiFault, ProfileInputMsg};
use holzwave_host::commands::vault::{vault_lock, vault_unlock};
use holzwave_host::locked::{SECRETS_LOCKED, is_locked};
use support::Fixture;

const PASSWORD: &str = "sync:password";
const PASSPHRASE: &str = "sync:passphrase";

/// A server that accepts a connection, counts it and closes it.
struct Backend {
    port: u16,
    seen: Arc<AtomicUsize>,
}

impl Backend {
    fn start() -> Backend {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("an address").port();
        let seen = Arc::new(AtomicUsize::new(0));
        let counted = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                counted.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        });
        Backend { port, seen }
    }

    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/dav", self.port)
    }

    fn calls(&self) -> usize {
        self.seen.load(Ordering::SeqCst)
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let begun = Instant::now();
    while !done() {
        assert!(begun.elapsed() < Duration::from_secs(15), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A core whose sync is set up for `backend` (the account file is there before the core reads it) and whose vault holds
/// the app password and the passphrase and is locked.
fn syncing(tag: &str, backend: &Backend) -> Fixture {
    let endpoint = backend.endpoint();
    let fixture = Fixture::prepared(tag, FakeKeystore::new(), |dir: &Path| {
        let account = serde_json::json!({ "endpoint": endpoint, "user": "holz" });
        std::fs::write(dir.join("sync.json"), account.to_string()).expect("the account");
    });
    vault_unlock(&fixture.core).expect("created");
    fixture.vault.set(PASSWORD, "app-password-canary").expect("password");
    fixture.vault.set(PASSPHRASE, "a long enough passphrase").expect("passphrase");
    vault_lock(&fixture.core).expect("locked");
    fixture
}

#[test]
fn a_locked_vault_pauses_the_sync_without_an_error_a_network_call_or_a_flicker() {
    let backend = Backend::start();
    let fixture = syncing("sync-paused", &backend);
    assert_eq!(fixture.core.sync_status().state, "idle", "a configured sync starts idle");

    // Sample the state on another thread while the scheduled run is called again and again.
    let core = fixture.core.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::<&'static str>::new()));
    let sampler = {
        let (core, stop, seen) = (core.clone(), stop.clone(), seen.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                seen.lock().expect("seen").insert(core.sync_status().state);
            }
        })
    };
    let first = fixture.core.sync_now().expect("a paused sync is not an error");
    assert_eq!((first.state, first.error.as_deref()), ("paused", None));
    for _ in 0..200 {
        fixture.core.sync_background();
    }
    stop.store(true, Ordering::SeqCst);
    sampler.join().expect("sampler");

    let view = fixture.core.sync_status();
    assert_eq!((view.state, view.error.as_deref(), view.configured), ("paused", None, true));
    assert_eq!(backend.calls(), 0, "no network call while paused");
    let states = seen.lock().expect("seen").clone();
    assert!(!states.contains("syncing") && !states.contains("error"), "the state only went through {states:?}");
}

#[test]
fn an_unlock_starts_one_run_at_once_and_a_lock_pauses_the_next_and_a_failing_backend_is_still_an_error() {
    let backend = Backend::start();
    let fixture = syncing("sync-resume", &backend);
    assert_eq!(fixture.core.sync_now().expect("paused").state, "paused");
    assert_eq!(backend.calls(), 0);

    vault_unlock(&fixture.core).expect("unlocked");
    wait_for("the run that the unlock starts", || backend.calls() > 0);
    wait_for("the run to end", || fixture.core.sync_status().state != "syncing");
    let view = fixture.core.sync_status();
    // The backend closes every connection, so the run fails: a failure that is not the lock is an error, with its text.
    assert_eq!(view.state, "error", "{view:?}");
    assert!(view.error.is_some_and(|text| !text.is_empty()));

    let calls = backend.calls();
    vault_lock(&fixture.core).expect("locked");
    let view = fixture.core.sync_now().expect("paused again");
    assert_eq!((view.state, view.error.as_deref()), ("paused", None), "the error of the last run is not carried into the pause");
    fixture.core.sync_background();
    assert_eq!(backend.calls(), calls, "the lock stops the runs");

    // The second unlock resumes the run that the pause held back, and so does every later one.
    vault_unlock(&fixture.core).expect("unlocked");
    wait_for("the second resume", || backend.calls() > calls);
    wait_for("the second run to end", || fixture.core.sync_status().state != "syncing");
    let after = backend.calls();

    // An unlock while the sync is not paused (its last run ended in an error) starts nothing by itself.
    vault_lock(&fixture.core).expect("lock");
    vault_unlock(&fixture.core).expect("unlock");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(backend.calls(), after, "only a paused sync is resumed by the unlock");
    vault_lock(&fixture.core).expect("lock");
    assert_eq!(fixture.core.sync_now().expect("paused").state, "paused");
    vault_unlock(&fixture.core).expect("unlock");
    wait_for("the third resume", || backend.calls() > after);
}

#[test]
fn saving_the_sync_while_locked_answers_the_locked_text_and_writes_nothing() {
    // A vault that was never unlocked (new) and one that was and is locked again: both refuse before anything is written.
    for locked_again in [false, true] {
        let backend = Backend::start();
        let fixture = Fixture::new(if locked_again { "save-locked" } else { "save-new" });
        if locked_again {
            vault_unlock(&fixture.core).expect("created");
            vault_lock(&fixture.core).expect("locked");
        }
        let refused = fixture.core.sync_save(&backend.endpoint(), "holz", "app-password-canary", "a long enough passphrase");
        assert_eq!(refused, Err(SECRETS_LOCKED.to_owned()), "locked_again={locked_again}");
        assert!(!fixture.dir.join("sync.json").exists(), "no account file");
        assert_eq!(fixture.core.sync_status().state, "off");
        assert_eq!(backend.calls(), 0);
        // Nothing was stored: after the unlock the vault has neither secret.
        vault_unlock(&fixture.core).expect("unlock");
        assert_eq!(fixture.vault.get(PASSWORD).expect("get"), None);
        assert_eq!(fixture.vault.get(PASSPHRASE).expect("get"), None);

        // Unlocked, the save goes on as before: it reaches the backend, which fails, and that failure rolls everything back.
        let failed = fixture.core.sync_save(&backend.endpoint(), "holz", "app-password-canary", "a long enough passphrase");
        let text = failed.expect_err("the backend closes every connection");
        assert_ne!(text, SECRETS_LOCKED);
        assert!(backend.calls() > 0, "an unlocked vault lets the save reach the backend");
        assert!(!text.contains("app-password-canary"));
        assert!(!fixture.dir.join("sync.json").exists(), "the failed first run leaves no account behind");
    }
}

/// A password connection and an agent connection, both saved while the vault was unlocked.
fn saved_connections(fixture: &Fixture) {
    let mut password = Profile::new("pw", "with password", "host.example");
    password.user = "holz".to_owned();
    password.auth = Auth::Password;
    fixture.core.connection_save(password, SecretChange::Set { value: "login-canary".to_owned() }).expect("saved");
    let mut agent = Profile::new("agent", "with agent", "host.example");
    agent.user = "holz".to_owned();
    agent.auth = Auth::Agent;
    fixture.core.connection_save(agent, SecretChange::Keep).expect("saved");
    let mut ask = Profile::new("ask", "asks", "host.example");
    ask.user = "holz".to_owned();
    ask.auth = Auth::Interactive;
    fixture.core.connection_save(ask, SecretChange::Keep).expect("saved");
}

#[test]
fn a_connection_that_needs_a_stored_secret_is_refused_while_locked_and_one_that_needs_none_is_not() {
    let fixture = Fixture::new("connect");
    vault_unlock(&fixture.core).expect("created");
    saved_connections(&fixture);
    let tabs = |fixture: &Fixture| fixture.core.view_in("main").expect("main").tabs.len();
    let before = tabs(&fixture);

    vault_lock(&fixture.core).expect("locked");
    let refused = fixture.core.connect_in("main", "pw");
    assert_eq!(refused.as_ref().err().map(String::as_str), Some(SECRETS_LOCKED), "{refused:?}");
    assert_eq!(tabs(&fixture), before, "no tab was made");
    assert!(fixture.core.pane_connections().is_empty(), "no connection was assigned to a pane");
    // The same refusal is the text `is_locked` recognises on the typed error of the store.
    assert!(is_locked(&fixture.vault.get("ssh:pw").expect_err("locked")));
    // A login that asks the store for nothing is not held back.
    assert!(fixture.core.connect_in("main", "agent").is_ok());
    assert!(fixture.core.connect_in("main", "ask").is_ok());
    assert_eq!(tabs(&fixture), before + 2);
    // A connection that does not exist is still said as before.
    assert_eq!(fixture.core.connect_in("main", "nope").err().as_deref(), Some("no connection nope"));

    vault_unlock(&fixture.core).expect("unlocked");
    assert!(fixture.core.connect_in("main", "pw").is_ok(), "with the vault open the same call goes on");
    assert_eq!(tabs(&fixture), before + 3);
}

#[test]
fn saving_a_secret_while_locked_answers_the_locked_text_and_stores_nothing() {
    let fixture = Fixture::new("save-secret");
    vault_unlock(&fixture.core).expect("created");
    vault_lock(&fixture.core).expect("locked");

    let mut profile = Profile::new("pw", "with password", "host.example");
    profile.user = "holz".to_owned();
    profile.auth = Auth::Password;
    let refused = fixture.core.connection_save(profile.clone(), SecretChange::Set { value: "login-canary".to_owned() });
    assert_eq!(refused.err().as_deref(), Some(SECRETS_LOCKED));
    assert!(fixture.core.connections_list().is_empty(), "the connection was not saved without its secret");

    vault_unlock(&fixture.core).expect("unlocked");
    assert_eq!(fixture.vault.get("ssh:pw").expect("get"), None, "the refused secret was not stored");
    fixture.core.connection_save(profile, SecretChange::Set { value: "login-canary".to_owned() }).expect("saved");
    assert_eq!(fixture.vault.get("ssh:pw").expect("get").as_deref(), Some("login-canary"));
}

fn profile_input(name: &str) -> ProfileInputMsg {
    ProfileInputMsg {
        id: None,
        name: name.to_owned(),
        preset: Preset::OpenAi,
        wire: None,
        base_url: String::new(),
        model: String::new(),
        allow_lan_http: false,
        acknowledge_gateway: false,
    }
}

fn json_of(fault: &AiFault) -> serde_json::Value {
    serde_json::to_value(fault).expect("a fault is JSON")
}

#[test]
fn the_ai_chat_uses_the_vault_and_is_refused_as_locked_only_for_a_key_that_the_vault_holds_back() {
    const KEY: &str = "sk-vault-canary-123";
    let fixture = Fixture::new("ai");
    vault_unlock(&fixture.core).expect("created");
    let core = &fixture.core;
    let keyed = core.ai().profile_save(profile_input("keyed")).expect("a profile");
    core.ai().key_set(&keyed, KeySlot::Provider, KEY.to_owned()).expect("the key goes into the vault");
    let bare = core.ai().profile_save(profile_input("bare")).expect("a profile without a key");
    // A provider that needs no key at all, as the on-device or a local one.
    let double = ScriptedProvider::new("scripted").with_capabilities(Capabilities::conservative(Locality::OnDevice));
    let double = (0..3).fold(double, |double, _| double.with_turn(Script::reply("ok")));
    core.ai().register_provider("scripted", Arc::new(double)).expect("registered");
    let pane = core.view_in("main").expect("main").focus;
    let chat = || holzwave_core::ai::store::ConversationId::generate().expect("an id").as_str().to_owned();

    // Unlocked: the key was read from the vault by the service, nothing refuses, and the scripted provider sends.
    assert_eq!(core.ai().has_key(&keyed, KeySlot::Provider), Ok(true), "the chat's key store is the host's store");
    assert!(core.ai_send(&chat(), "hello".to_owned(), pane, Some("scripted".to_owned()), None).is_ok());

    vault_lock(core).expect("locked");
    // Locked: the profile whose key is in the vault is the locked fault, with no network and no turn.
    let locked = core.ai_send(&chat(), "hello".to_owned(), pane, Some(keyed.clone()), None).expect_err("held back");
    assert_eq!(json_of(&locked), json_of(&AiFault::locked()));
    assert_eq!((locked.code, locked.reason, locked.text.as_str()), ("key", Some("locked"), SECRETS_LOCKED));
    // With no profile named the chat's own pin applies, which is not known here: the keyed profile counts.
    let unnamed = core.ai_send(&chat(), "hello".to_owned(), pane, None, None).expect_err("held back");
    assert_eq!(json_of(&unnamed), json_of(&AiFault::locked()));
    // A provider that needs no key from the vault is not held back by the lock, nor is a profile that has no key saved.
    assert!(core.ai_send(&chat(), "hello".to_owned(), pane, Some("scripted".to_owned()), None).is_ok());
    // (Its turn ends in the service's own words, later; the send itself is not the locked fault.)
    assert!(core.ai_send(&chat(), "hello".to_owned(), pane, Some(bare.clone()), None).is_ok(), "a profile with no key is not held back");

    // Saving or reading a key while locked is the locked fault and stores nothing.
    let refused = core.ai().key_set(&bare, KeySlot::Provider, "another-key-9".to_owned()).expect_err("locked");
    assert_eq!(json_of(&refused), json_of(&AiFault::locked()));
    assert_eq!(json_of(&core.ai().has_key(&keyed, KeySlot::Provider).expect_err("locked")), json_of(&AiFault::locked()));
    vault_unlock(core).expect("unlocked");
    assert_eq!(core.ai().has_key(&bare, KeySlot::Provider), Ok(false), "the refused key was not stored");
    assert_eq!(core.ai().has_key(&keyed, KeySlot::Provider), Ok(true), "the first key is still there");
    for fault in [&locked, &unnamed, &refused] {
        assert!(!format!("{fault:?} {}", json_of(fault)).contains(KEY));
    }
}

#[test]
fn a_secret_that_went_through_the_commands_is_in_no_environment_answer_error_or_file() {
    let canaries = ["login-canary-77", "app-password-canary-77", "a canary passphrase 77", "sk-canary-77"];
    let backend = Backend::start();
    let fixture = Fixture::new("canary");
    let core = &fixture.core;
    let mut said = Vec::<String>::new();

    // Locked first: every refusal is a text, and none holds a secret.
    said.push(format!("{:?}", core.sync_save(&backend.endpoint(), "holz", canaries[1], canaries[2])));
    let mut profile = Profile::new("pw", "with password", "host.example");
    profile.user = "holz".to_owned();
    profile.auth = Auth::Password;
    said.push(format!("{:?}", core.connection_save(profile.clone(), SecretChange::Set { value: canaries[0].to_owned() })));

    vault_unlock(core).expect("unlocked");
    fixture.core.connection_save(profile, SecretChange::Set { value: canaries[0].to_owned() }).expect("saved");
    let keyed = core.ai().profile_save(profile_input("keyed")).expect("a profile");
    core.ai().key_set(&keyed, KeySlot::Provider, canaries[3].to_owned()).expect("a key");
    said.push(format!("{:?}", core.sync_save(&backend.endpoint(), "holz", canaries[1], canaries[2])));
    said.push(serde_json::to_string(&core.sync_status()).expect("a view"));
    said.push(serde_json::to_string(&core.connections_list()).expect("a list"));
    said.push(serde_json::to_string(&core.ai().profiles()).expect("profiles"));
    vault_lock(core).expect("locked");
    said.push(format!("{:?}", core.connect_in("main", "pw")));
    said.push(format!("{:?}", core.ai().has_key(&keyed, KeySlot::Provider)));
    said.push(serde_json::to_string(&core.sync_now()).expect("a view"));
    said.push(serde_json::to_string(&core.sync_status()).expect("a view"));

    for canary in canaries {
        // The vault holds them: it must be able to give the login back after the unlock, and no other place may have it.
        for text in &said {
            assert!(!text.contains(canary), "an answer holds {canary}: {text}");
        }
        assert!(std::env::vars().all(|(name, value)| !value.contains(canary) && !name.contains(canary)), "the environment holds {canary}");
        let mut files = vec![fixture.dir.clone()];
        let mut checked = 0;
        while let Some(path) = files.pop() {
            if path.is_dir() {
                files.extend(std::fs::read_dir(&path).expect("a folder").flatten().map(|entry| entry.path()));
            } else if let Ok(bytes) = std::fs::read(&path) {
                checked += 1;
                assert!(!bytes.windows(canary.len()).any(|window| window == canary.as_bytes()), "{} holds {canary}", path.display());
            }
        }
        assert!(checked > 0, "the scratch folder has files to look in, among them the vault file");
    }
    assert!(fixture.dir.join("secrets.vault").is_file());
    vault_unlock(core).expect("unlocked");
    assert_eq!(fixture.vault.get("ssh:pw").expect("get").as_deref(), Some(canaries[0]), "the vault gives the login back");
}
