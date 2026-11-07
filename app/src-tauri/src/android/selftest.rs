//! The self-test of the vault, for the emulator: debug builds of the Android app only.
//!
//! This module is compiled when `debug_assertions` is on and the target is Android, so a release build does not contain it,
//! and the APK check fails a release APK that carries the name of the intent extra that starts a step. A step is named by
//! the launch intent of the debug app (`adb shell am start -n <app>/<activity> --es <extra> <step>`); the debug-only Kotlin
//! hook keeps the name until this thread asks for it through the plugin, and the step runs against the real host objects
//! (the real vault over the real Keystore, the real clock, the AI settings of the core). It reports one line, written to
//! stderr, which the Android runtime sends to the log (tag `RustStdoutStderr`):
//!
//! ```text
//! holzwave-selftest <step>: ok [detail]
//! holzwave-selftest <step>: FAIL <reason>
//! ```
//!
//! The reasons are fixed texts. The canary secret, `HW-CANARY-<random>`, is made by the first step and written only to the
//! file `selftest-canary` in the app's private folder, from which the emulator script reads it with `run-as`; it is never
//! printed and never logged, and the script then searches for it in the app's files, the log and the process's environment,
//! where it must not be found.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use holzwave_core::ai::profiles::KeySlot;
use holzwave_core::secrets::SecretStore;
use holzwave_core::vault::{Clock, LostReason, Protection, VaultError, VaultState, VaultStore};
use holzwave_host::ai_host::ProfileInputMsg;
use holzwave_host::keystore_wire::RecentsMode;
use holzwave_host::locked::is_locked;
use holzwave_host::seams::Host;
use holzwave_host::state::AppCore;
use serde_json::{Value, json};

use crate::android::host::AndroidHost;
use crate::android::keystore::{AndroidClock, PluginBridge};
use crate::android::plugin;

/// The account the steps use.
const ACCOUNT: &str = "ssh:selftest";
/// The file of the canary, in the app's private folder.
const CANARY_FILE: &str = "selftest-canary";
/// The name of the profile the AI-key steps make.
const PROFILE_NAME: &str = "Selftest";
/// The key the AI-key steps save. It is not a real key and it is not the canary.
const AI_KEY: &str = "HW-SELFTEST-AI-KEY";

type Outcome = Result<String, String>;

fn fail<T>(reason: &str) -> Result<T, String> {
    Err(reason.to_owned())
}

/// A random text for the canary, from the operating system's own seed (no dependency for it).
fn canary_text() -> String {
    let random = RandomState::new();
    let mut a = random.build_hasher();
    a.write_u64(1);
    let mut b = random.build_hasher();
    b.write_u64(2);
    format!("HW-CANARY-{:016x}{:016x}", a.finish(), b.finish())
}

struct Steps {
    core: Arc<AppCore>,
    host: Arc<AndroidHost>,
    vault: Arc<VaultStore>,
    data_dir: std::path::PathBuf,
}

impl Steps {
    fn canary(&self) -> Result<String, String> {
        std::fs::read_to_string(self.data_dir.join(CANARY_FILE)).map_err(|_| "the canary file is missing".to_owned())
    }

    fn state(&self) -> VaultState {
        self.vault.status().state
    }

    fn expect_state(&self, wanted: VaultState) -> Result<(), String> {
        if self.state() == wanted { Ok(()) } else { fail("the vault is not in the state the step expects") }
    }

    fn expect_protection(&self, wanted: Protection) -> Result<(), String> {
        if self.vault.status().protection == wanted { Ok(()) } else { fail("the vault has not the protection the step expects") }
    }

    fn expect_canary(&self) -> Result<(), String> {
        let canary = self.canary()?;
        match self.vault.get(ACCOUNT) {
            Ok(Some(got)) if got == canary => Ok(()),
            Ok(_) => fail("the saved secret is not the canary"),
            Err(_) => fail("the secret could not be read"),
        }
    }

    fn expect_locked_answer(&self) -> Result<(), String> {
        match self.vault.get(ACCOUNT) {
            Err(e) if is_locked(&e) => Ok(()),
            _ => fail("a locked vault did not answer the locked error"),
        }
    }

    fn run(&self, step: &str) -> Outcome {
        match step {
            "vault-info" => {
                let s = self.vault.status();
                Ok(format!(
                    "state={} protection={} screen_lock={} strongbox={} can_protect={}",
                    s.state.as_str(),
                    s.protection.as_str(),
                    s.has_screen_lock,
                    s.strongbox,
                    s.can_protect
                ))
            }
            // A device with no screen lock: a plain vault, made by the first save.
            "vault-plain-write" => {
                self.expect_state(VaultState::New)?;
                if self.vault.status().has_screen_lock {
                    return fail("the device has a screen lock; this step is for a device without one");
                }
                let canary = canary_text();
                std::fs::write(self.data_dir.join(CANARY_FILE), &canary).map_err(|_| "the canary file could not be written".to_owned())?;
                self.vault.set(ACCOUNT, &canary).map_err(|_| "the first save failed".to_owned())?;
                self.expect_protection(Protection::Plain)?;
                self.expect_state(VaultState::Unlocked)?;
                self.expect_canary()?;
                Ok("plain, unlocked, saved and read back".into())
            }
            // After a restart of the process: a plain vault opens by itself, with no prompt.
            "vault-plain-read" => {
                self.expect_protection(Protection::Plain)?;
                self.expect_state(VaultState::Unlocked)?;
                self.expect_canary()?;
                Ok("plain, unlocked with no prompt, read back".into())
            }
            // A device that has a screen lock and no vault yet: the first unlock makes a protected vault, and shows the prompt.
            "vault-create" => {
                self.expect_state(VaultState::New)?;
                self.vault.unlock().map_err(|e| format!("the unlock failed: {}", word(&e)))?;
                self.expect_protection(Protection::Auth)?;
                self.expect_state(VaultState::Unlocked)?;
                Ok("auth, created by the first unlock".into())
            }
            // A screen lock now exists: the plain vault can be protected, and doing so shows the system prompt.
            "vault-protect" => {
                let s = self.vault.status();
                if !s.can_protect {
                    return fail("the vault cannot be protected now");
                }
                self.vault.set_protection(Protection::Auth).map_err(|e| format!("the switch failed: {}", word(&e)))?;
                self.expect_protection(Protection::Auth)?;
                self.expect_state(VaultState::Unlocked)?;
                self.expect_canary()?;
                Ok("auth, unlocked, the secret survived the switch".into())
            }
            // After a restart: locked, the locked fault, then the prompt and the secret.
            "vault-locked-read" => {
                self.expect_protection(Protection::Auth)?;
                self.expect_state(VaultState::Locked)?;
                self.expect_locked_answer()?;
                self.vault.unlock().map_err(|e| format!("the unlock failed: {}", word(&e)))?;
                self.expect_state(VaultState::Unlocked)?;
                self.expect_canary()?;
                Ok("locked, the locked fault, unlocked by the prompt, read back".into())
            }
            // The page was hidden and the five minutes are taken as passed by a lock; the clock is the real one.
            "vault-background" => {
                self.expect_state(VaultState::Unlocked)?;
                self.vault.hidden(true);
                self.vault.lock();
                self.expect_locked_answer()?;
                self.vault.hidden(false);
                let clock = AndroidClock::new(Arc::new(PluginBridge));
                let first = clock.now_ms();
                std::thread::sleep(Duration::from_millis(1100));
                let second = clock.now_ms();
                if first == 0 || second < first + 1000 {
                    return fail("the device clock did not answer a positive, advancing value");
                }
                Ok(format!("locked after hidden, clock advanced {} ms in 1100 ms", second - first))
            }
            // The AI key goes through the same store; no provider is called.
            "vault-ai-key" => {
                if self.state() == VaultState::Locked {
                    self.vault.unlock().map_err(|e| format!("the unlock failed: {}", word(&e)))?;
                }
                self.expect_state(VaultState::Unlocked)?;
                let id = self.profile()?;
                self.core.ai().key_set(&id, KeySlot::Provider, AI_KEY.to_owned()).map_err(|_| "the key was not saved".to_owned())?;
                match self.core.ai().has_key(&id, KeySlot::Provider) {
                    Ok(true) => Ok("saved while unlocked, present".into()),
                    _ => fail("the saved key is not present"),
                }
            }
            "vault-ai-key-after-restart" => {
                self.expect_state(VaultState::Locked)?;
                // The profile is looked up, never saved again: a save would rewrite the record the key belongs to.
                let id = self.existing_profile()?;
                match self.core.ai().has_key(&id, KeySlot::Provider) {
                    Err(_) => {}
                    Ok(_) => return fail("a locked vault answered about a key"),
                }
                self.vault.unlock().map_err(|e| format!("the unlock failed: {}", word(&e)))?;
                match self.core.ai().has_key(&id, KeySlot::Provider) {
                    Ok(true) => Ok("locked answered the locked fault, present after the unlock".into()),
                    _ => fail("the key is not there after the unlock"),
                }
            }
            // The screen lock was removed by the script: the system deleted the key.
            "vault-lost" => {
                match self.vault.unlock() {
                    Err(VaultError::Lost(LostReason::KeyGone)) => {}
                    _ => return fail("the unlock did not report a lost key"),
                }
                let s = self.vault.status();
                if s.state != VaultState::Lost || s.reason != Some(LostReason::KeyGone) {
                    return fail("the status is not lost with key_gone");
                }
                self.vault.reset().map_err(|_| "the reset failed".to_owned())?;
                self.expect_state(VaultState::New)?;
                Ok("lost with key_gone, reset leaves new".into())
            }
            "vault-recents-protected" => {
                self.expect_protection(Protection::Auth)?;
                self.recents(true)
            }
            // The protection goes off on the unlocked vault (no prompt beyond the unlock already done).
            "vault-recents-plain" => {
                self.expect_state(VaultState::Unlocked)?;
                self.vault.set_protection(Protection::Plain).map_err(|e| format!("the switch failed: {}", word(&e)))?;
                self.expect_protection(Protection::Plain)?;
                self.recents(false)
            }
            _ => fail("an unknown step"),
        }
    }

    /// The profile the AI-key steps use: made once, found by its name afterwards.
    fn profile(&self) -> Result<String, String> {
        let input: ProfileInputMsg = serde_json::from_value(json!({ "name": PROFILE_NAME, "preset": "ollama" }))
            .map_err(|_| "the profile input is not understood".to_owned())?;
        self.core.ai().profile_save(input).map_err(|_| "the profile was not saved".to_owned())
    }

    /// The id of the profile an earlier step made, found by its name.
    fn existing_profile(&self) -> Result<String, String> {
        self.core
            .ai()
            .profiles()
            .profiles
            .into_iter()
            .find(|profile| profile.name == PROFILE_NAME)
            .map(|profile| profile.id)
            .ok_or_else(|| "the profile of the earlier step is missing".to_owned())
    }

    /// Wait up to three seconds for the host's watcher to have told the plugin and for the plugin's answer to match the wish,
    /// then report the applied value and the mode.
    fn recents(&self, hidden: bool) -> Outcome {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(answer) = self.host.recents_answer() {
                let consistent = match answer.mode {
                    // The framework call exists: the picture is hidden exactly when it was asked to be.
                    RecentsMode::System => answer.applied == hidden,
                    // Android 12 and 12L: nothing hides the picture, and the plugin says so.
                    RecentsMode::Unsupported => !answer.applied,
                };
                if consistent {
                    return Ok(format!("applied={} mode={}", answer.applied, answer.mode.as_str()));
                }
            }
            if Instant::now() >= deadline {
                return fail("the plugin's answer did not follow the vault within 3 seconds");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The word of a failed vault call, as the page would get it.
fn word(error: &VaultError) -> &'static str {
    match error {
        VaultError::Cancelled => "cancelled",
        VaultError::Lost(_) => "lost",
        VaultError::NoScreenLock => "no_screen_lock",
        VaultError::Failed(_) => "failed",
    }
}

/// Start the thread that takes the steps the debug hook was given and runs them. Called once from `setup`, debug builds only.
pub fn start(core: Arc<AppCore>, host: Arc<AndroidHost>) {
    let vault = host.vault_store().clone();
    let data_dir = host.private_dirs().and_then(|dirs| dirs.home.parent().map(std::path::Path::to_path_buf)).unwrap_or_default();
    let steps = Steps { core, host, vault, data_dir };
    let spawned = std::thread::Builder::new().name("hw-selftest".into()).spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let Some(handle) = plugin::handle() else { continue };
            let Ok(answer) = handle.run_mobile_plugin::<Value>("debugTake", json!({})) else { continue };
            let Some(step) = answer.get("step").and_then(Value::as_str) else { continue };
            match steps.run(step) {
                Ok(detail) => eprintln!("holzwave-selftest {step}: ok {detail}"),
                Err(reason) => eprintln!("holzwave-selftest {step}: FAIL {reason}"),
            }
        }
    });
    if let Err(e) = spawned {
        eprintln!("holzwave: the self-test thread could not start: {e}");
    }
}
