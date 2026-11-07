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
//!
//! The steps `handover-in`, `handover-out` and `clipboard-roundtrip` run the single-file hand-over with the real document dialog of
//! the system (the script of the emulator taps the dialog) and the real system clipboard. They log the outcome kind, a cleaned file
//! name and a size, a fixed test pattern's own path, or the fixed words of the clipboard steps; never the bytes of a file and
//! never a text that came from the clipboard.
//!
//! The steps `tls-public`, `tls-untrusted`, `sync-save`, `sync-paused` and `sync-resume` run the two real network clients of the
//! core (the WebDAV client of the sync and the client of the AI chat) against a public site and against a server with a
//! self-signed certificate that the emulator script starts on the Mac and forwards to the emulator's loopback address with `adb
//! reverse`. They show
//! that the platform certificate verifier judges certificates, and that a locked vault pauses the sync and lets it run at the
//! unlock. What they print about a failed request is the text of the client's own error (a fixed phrase or the transport's
//! message, which holds no header, body, certificate or secret) cleaned to printable ASCII and cut short.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use holzwave_core::ai::http::{Call, Framing, Http, HttpConfig, NetworkPolicy, Target};
use holzwave_core::ai::profiles::KeySlot;
use holzwave_core::ai::provider::{AiError, CancelToken, Flow};
use holzwave_core::secrets::SecretStore;
use holzwave_core::sync::webdav::{Backend, WebDav};
use holzwave_core::vault::{Clock, LostReason, Protection, VaultError, VaultState, VaultStore};
use holzwave_host::ai_host::ProfileInputMsg;
use holzwave_host::commands::handover::{HandoverOutcomeMsg, handover_in, handover_out};
use holzwave_host::commands::vault::vault_unlock;
use holzwave_host::keystore_wire::RecentsMode;
use holzwave_host::locked::is_locked;
use holzwave_host::seams::Host;
use holzwave_host::state::AppCore;
use serde_json::{Value, json};

use crate::android::host::AndroidHost;
use crate::clipboard;
use crate::android::keystore::{AndroidClock, PluginBridge};
use crate::android::plugin;

/// The account the steps use.
const ACCOUNT: &str = "ssh:selftest";
/// The file of the canary, in the app's private folder.
const CANARY_FILE: &str = "selftest-canary";
/// The file `handover-out` writes in the shell's folder and then copies out through the system's dialog: a fixed non-secret pattern
/// of 4 KiB (the bytes 0 to 255, sixteen times).
const OUT_FILE: &str = "hw-out-test.bin";
/// The text the clipboard step writes and reads back, and the text over the bound it must refuse: neither is a secret.
const CLIPBOARD_TEXT: &str = "holzwave clipboard test";
const CLIPBOARD_BIG: usize = 600_000;
/// The name of the profile the AI-key steps make.
const PROFILE_NAME: &str = "Selftest";
/// The key the AI-key steps save. It is not a real key and it is not the canary.
const AI_KEY: &str = "HW-SELFTEST-AI-KEY";

/// A public site every network has, for the positive check of the verifier. The address is a constant of this debug module and
/// not a setting. Nothing but a request without a body or a secret is sent to it.
const PUBLIC_URL: &str = "https://example.com/";
/// The loopback address of the emulator, where `adb reverse` forwards the port of the TLS server with a self-signed certificate that
/// the script runs on the Mac. (The emulator's own name for the Mac, 10.0.2.2, is a local-network address, which an app that
/// targets Android 17 may not reach without the local-network permission; the loopback address needs none.)
const UNTRUSTED_URL: &str = "https://127.0.0.1:8443/";
/// The sync account the steps save: the same server, a folder below it.
const SYNC_ENDPOINT: &str = "https://127.0.0.1:8443/dav";
const SYNC_USER: &str = "selftest";
/// What the clients send to a server that is not the app's own: a dummy account, never a secret.
const DUMMY_USER: &str = "selftest";
const DUMMY_PASSWORD: &str = "selftest-dummy";
/// The names of the sync's secrets and the file of its account, as `host/src/sync.rs` writes them (the host's own tests spell
/// them the same way): the steps put an account in place the way a successful save would have, because a save against a server
/// whose certificate is refused rolls itself back.
const SYNC_PASSWORD_ACCOUNT: &str = "sync:password";
const SYNC_PASSPHRASE_ACCOUNT: &str = "sync:passphrase";
const SYNC_FILE: &str = "sync.json";

type Outcome = Result<String, String>;

/// How a request of one of the network clients ended.
enum Reach {
    /// The server answered at the HTTP level (any status): the handshake succeeded, so the certificate was accepted.
    Answered(String),
    /// The certificate was refused.
    Certificate(String),
    /// Anything else: no route, a refused connection, a time-out, the verifier not set up.
    Other(String),
}

impl Reach {
    fn is_answer(&self) -> bool {
        matches!(self, Reach::Answered(_))
    }

    fn is_certificate(&self) -> bool {
        matches!(self, Reach::Certificate(_))
    }

    fn text(&self) -> &str {
        match self {
            Reach::Answered(t) | Reach::Certificate(t) | Reach::Other(t) => t,
        }
    }
}

/// Printable ASCII only and at most 160 characters, so a client's text can go to the log as one line.
fn clean(text: &str) -> String {
    text.chars().map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '?' }).take(160).collect()
}

/// The text of a failure, sorted: a refused certificate, an answer of the server (a status the client reports as an error), or
/// another failure.
fn classify(text: &str) -> Reach {
    let lower = text.to_ascii_lowercase();
    if lower.contains("certificate") {
        Reach::Certificate(clean(text))
    } else if lower.contains("the server answered") || lower.contains("the server refused the user name") {
        Reach::Answered(clean(text))
    } else {
        Reach::Other(clean(text))
    }
}

/// The sync client against `url`: a PROPFIND of the folder, the cheapest call the client has.
fn sync_reach(url: &str) -> Reach {
    let Ok(backend) = WebDav::new(url, DUMMY_USER, DUMMY_PASSWORD) else { return Reach::Other("the client was refused its address".into()) };
    match backend.list("") {
        Ok(_) => Reach::Answered("the folder was listed".into()),
        Err(e) => classify(&e.to_string()),
    }
}

/// The AI client against `url`: a POST of an empty JSON object to the root, which no provider answers with anything but a
/// status. The body of an answer is read and dropped; nothing of it is kept or printed.
fn ai_reach(url: &str) -> Reach {
    let Ok(target) = Target::parse(url, false) else { return Reach::Other("the address was refused".into()) };
    let http = Http::new(HttpConfig::standard());
    let call = Call::new(&target, NetworkPolicy::new(false, false), "/", b"{}", Framing::Sse);
    match http.post_stream(&call, &mut |_frame| Flow::Continue, &CancelToken::new()) {
        Ok(_) => Reach::Answered("answered".into()),
        // Errors that exist only after the server answered.
        Err(
            AiError::Auth(_)
            | AiError::RateLimited { .. }
            | AiError::Overloaded
            | AiError::ContextLength
            | AiError::ContentFilter
            | AiError::InvalidRequest(_)
            | AiError::Protocol(_)
            | AiError::Server { .. },
        ) => Reach::Answered("answered with an error status or a body the adapter cannot read".into()),
        Err(e) => {
            let text = e.to_string();
            if text.to_ascii_lowercase().contains("the server answered") { Reach::Answered(clean(&text)) } else { classify(&text) }
        }
    }
}

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
            // The platform verifier accepts what the system trusts: both clients reach a public site.
            "tls-public" => {
                let (sync, ai) = (sync_reach(PUBLIC_URL), ai_reach(PUBLIC_URL));
                if !sync.is_answer() {
                    return Err(format!("the sync client got no answer from the public site: {}", sync.text()));
                }
                if !ai.is_answer() {
                    return Err(format!("the AI client got no answer from the public site: {}", ai.text()));
                }
                Ok(format!("sync client: {}; AI client: {}", sync.text(), ai.text()))
            }
            // And it refuses what the system does not trust: both clients refuse the self-signed certificate.
            "tls-untrusted" => {
                let (sync, ai) = (sync_reach(UNTRUSTED_URL), ai_reach(UNTRUSTED_URL));
                if sync.is_answer() || ai.is_answer() {
                    return fail("a client accepted a self-signed certificate");
                }
                if !sync.is_certificate() {
                    return Err(format!("the sync client failed, but not as a refused certificate: {}", sync.text()));
                }
                if !ai.is_certificate() {
                    return Err(format!("the AI client failed, but not as a refused certificate: {}", ai.text()));
                }
                Ok(format!("sync client: {}; AI client: {}", sync.text(), ai.text()))
            }
            // The vault is unlocked. A save through the real path is refused for the certificate and keeps nothing; then the
            // account a successful save would have left is put in place, for the restart that follows.
            "sync-save" => self.sync_save(),
            // After a restart with the vault locked: the sync is paused, with no error text.
            "sync-paused" => self.sync_paused(),
            // The unlock (the system prompt, answered by the script) starts one run, which reaches the untrusted server.
            "sync-resume" => self.sync_resume(),
            // The system's open dialog, driven by the script: the outcome kind and, for a copy, the cleaned name and the size.
            "handover-in" => handover_line(&handover_in(&self.core)),
            // A fixed pattern is written to the shell's folder and copied out through the system's create dialog.
            "handover-out" => {
                let home = self.host.private_dirs().map(|dirs| dirs.home).ok_or_else(|| "the host has no shell folder".to_owned())?;
                let pattern: Vec<u8> = (0..4096u32).map(|i| (i % 256) as u8).collect();
                std::fs::write(home.join(OUT_FILE), pattern).map_err(|_| "the test pattern could not be written".to_owned())?;
                handover_line(&handover_out(&self.core, OUT_FILE.to_owned()))
            }
            // The clipboard written and read back through the plugin, the bound, and the sensitive mark.
            "clipboard-roundtrip" => clipboard_roundtrip(),
            _ => fail("an unknown step"),
        }
    }

    /// The account of the sync steps: the password and the passphrase are the canary, so the canary scan covers them too.
    fn sync_save(&self) -> Outcome {
        self.expect_state(VaultState::Unlocked)?;
        let canary = match self.canary() {
            Ok(text) => text,
            Err(_) => {
                // The first step that needs it makes it, the same file the vault steps use.
                let text = canary_text();
                std::fs::write(self.data_dir.join(CANARY_FILE), &text).map_err(|_| "the canary file could not be written".to_owned())?;
                text
            }
        };
        match self.core.sync_save(SYNC_ENDPOINT, SYNC_USER, &canary, &canary) {
            Ok(_) => return fail("a save against a server with a refused certificate was accepted"),
            Err(text) if text.to_ascii_lowercase().contains("certificate") => {}
            Err(text) => return Err(format!("the save failed, but not as a refused certificate: {}", clean(&text.replace(&canary, "<canary>")))),
        }
        if self.core.sync_status().configured {
            return fail("a refused save left an account behind");
        }
        self.vault.set(SYNC_PASSWORD_ACCOUNT, &canary).map_err(|_| "the password could not be saved".to_owned())?;
        self.vault.set(SYNC_PASSPHRASE_ACCOUNT, &canary).map_err(|_| "the passphrase could not be saved".to_owned())?;
        let account = json!({ "endpoint": SYNC_ENDPOINT, "user": SYNC_USER });
        std::fs::write(self.data_dir.join(SYNC_FILE), account.to_string()).map_err(|_| "the account file could not be written".to_owned())?;
        Ok("the save was refused as a certificate error and kept nothing; the account is in place for the restart".into())
    }

    fn sync_paused(&self) -> Outcome {
        self.expect_state(VaultState::Locked)?;
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            let view = self.core.sync_status();
            if !view.configured {
                return fail("the account was not found after the restart");
            }
            if view.state == "paused" {
                return if view.error.is_none() { Ok("state=paused, no error text".into()) } else { fail("paused, but with an error text") };
            }
            if view.state == "error" || view.state == "syncing" {
                return Err(format!("the sync ran while the vault was locked (state={})", view.state));
            }
            if Instant::now() >= deadline {
                return fail("the sync did not become paused within 40 seconds");
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn sync_resume(&self) -> Outcome {
        if self.core.sync_status().state != "paused" {
            return fail("the sync was not paused before the unlock");
        }
        vault_unlock(&self.core).map_err(|e| format!("the unlock failed: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let view = self.core.sync_status();
            match view.state {
                "paused" | "syncing" => {}
                "error" => {
                    let text = view.error.unwrap_or_default();
                    let lower = text.to_ascii_lowercase();
                    return if lower.contains("certificate") {
                        Ok(format!("one run started at the unlock and the server's certificate was refused: {}", clean(&text)))
                    } else if lower.contains("secret") || lower.contains("keychain") || lower.contains("locked") || lower.contains("missing") {
                        Err(format!("the run failed for a missing secret: {}", clean(&text)))
                    } else {
                        Err(format!("the run failed, but not as a refused certificate: {}", clean(&text)))
                    };
                }
                "idle" => return fail("the run ended as a success: the certificate was accepted"),
                _ => return fail("the sync is in an unexpected state"),
            }
            if Instant::now() >= deadline {
                return fail("the sync stayed paused or syncing for 20 seconds after the unlock");
            }
            std::thread::sleep(Duration::from_millis(250));
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

/// The log line of a hand-over: the kind, and for a finished copy the cleaned name and the size. `failed` and `unavailable` are
/// failures of the step; `cancelled` and `too_large` are outcomes the script asks for.
fn handover_line(outcome: &HandoverOutcomeMsg) -> Outcome {
    match outcome {
        HandoverOutcomeMsg::Done { name, size, shown, renamed } => {
            Ok(format!("done name={name:?} size={size} shown={shown:?} renamed={renamed}"))
        }
        HandoverOutcomeMsg::Cancelled => Ok("cancelled".to_owned()),
        HandoverOutcomeMsg::TooLarge { name, size } => Ok(format!("too_large name={name:?} size={}", size.map_or("unknown".to_owned(), |n| n.to_string()))),
        HandoverOutcomeMsg::Failed => fail("the hand-over failed"),
        HandoverOutcomeMsg::Unavailable => fail("the build has no document dialog"),
    }
}

/// A write, a read back and a refusal. A read right after the app came to the front may find the system's clipboard closed to an
/// app that has no focus yet (it answers an empty clip), so the read is repeated for a few seconds; it is the text of this step
/// that is compared, and only ok or a fixed reason is logged.
fn clipboard_roundtrip() -> Outcome {
    let marked = clipboard::write_text_marked(CLIPBOARD_TEXT).map_err(|_| "the clipboard write failed".to_owned())?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match clipboard::read_text() {
            Ok(text) if text == CLIPBOARD_TEXT => break,
            Ok(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(250)),
            Ok(_) => return fail("the clipboard did not give back the text that was written"),
            Err(_) => return fail("the clipboard read failed"),
        }
    }
    let big = "x".repeat(CLIPBOARD_BIG);
    match clipboard::write_text(&big) {
        Err(error) if error.contains("the limit is 524288 bytes") => {}
        Err(_) => return fail("the text over the bound was refused, but not with the fixed words"),
        Ok(()) => return fail("a text over the bound was written"),
    }
    match clipboard::read_text() {
        Ok(text) if text == CLIPBOARD_TEXT => {}
        _ => return fail("the clipboard no longer holds the first text after the refused write"),
    }
    Ok(format!("written, read back, the oversized text refused, the first text kept, sensitive={marked}"))
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
