//! [`AndroidHost`]: the platform answers of the Android app.
//!
//! - The platform is Android, so the page takes the Linux key table and the phone and tablet layouts.
//! - The private folders are two folders inside the app's own storage: `home` (the shell's home and start directory) in
//!   the data directory, and the app's cache directory (the shell's `TMPDIR`). `home` is a folder of its own beside the
//!   app's settings, layout, sync and vault files, never the data directory itself, so that a program a person starts in
//!   a pane does not find those files next to its own and a hand-over of files can be limited to it. Both folders are made
//!   before they are named, `home` with mode 0700.
//! - The secret store is the vault of the core, wrapped by a key in the Android Keystore ([`AndroidKeystore`]) and timed by
//!   the clock that counts the time the phone slept ([`AndroidClock`]). Its file, `vault.bin`, is in the data directory
//!   beside the app's own files and outside `home`, so the shell can read it but finds only ciphertext. The same object is
//!   the host's `secrets()` (what the AI chat, the saved connections and the sync read) and its `vault()` (what the page's
//!   vault commands drive).
//! - While the vault is protected, the picture of the app in the app switcher is hidden ([`AndroidHost::sync_recents`]):
//!   the host follows the vault's protection and tells the plugin when it changes. See [`holzwave_host::keystore_wire::recents_hidden`].
//! - Everything else is what a platform without the feature answers: no permission source for the local network, a text
//!   size of 100, no limits, and the address of the file scheme from `files_protocol` (on Android the `http://holzfile.localhost/`
//!   form).

use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use holzwave_core::secrets::SecretStore;
use holzwave_core::vault::VaultStore;
use holzwave_host::keystore_wire::{RecentsAnswer, recents_from_wire, recents_hidden, recents_to_wire};
use holzwave_host::local_network::{Source, Unsupported};
use holzwave_host::seams::{Host, HostLimits, Platform, PrivateDirs};

use crate::android::keystore::{AndroidClock, AndroidKeystore, Bridge};

/// The name of the shell's own folder inside the app's data directory.
pub const HOME_FOLDER: &str = "home";

/// The vault's file, in the data directory (not in `home`).
pub const VAULT_FILE: &str = "vault.bin";

/// The command of the Kotlin plugin that hides or shows the app switcher picture.
pub const RECENTS: &str = "recentsHidden";

/// What the host knows about the app switcher picture.
#[derive(Default)]
struct Recents {
    /// The value the plugin took last; `None` until a call succeeded. A failed call leaves it as it was, so the next round
    /// sends again.
    sent: Option<bool>,
    /// The plugin's last answer, for the debug self-test and the tests.
    #[cfg_attr(not(any(test, all(target_os = "android", debug_assertions))), allow(dead_code))]
    answer: Option<RecentsAnswer>,
}

/// The host of the Android app.
pub struct AndroidHost {
    dirs: PrivateDirs,
    vault: Arc<VaultStore>,
    bridge: Arc<dyn Bridge>,
    recents: Mutex<Recents>,
}

impl AndroidHost {
    /// The host for an app whose data directory is `data_dir` and whose cache directory is `cache_dir`. Both folders it
    /// names exist when this returns: `data_dir/home` (mode 0700) and `cache_dir`. Fails when one cannot be made, and
    /// the app does not start without them. The vault is the Android Keystore's, through the app's own plugin.
    #[cfg(target_os = "android")]
    pub fn new(data_dir: &Path, cache_dir: &Path) -> io::Result<AndroidHost> {
        Self::with_bridge(data_dir, cache_dir, Arc::new(crate::android::keystore::PluginBridge))
    }

    /// [`new`](AndroidHost::new) over another bridge to the platform: the vault is the real one over a keystore and a clock
    /// that call `bridge`.
    pub fn with_bridge(data_dir: &Path, cache_dir: &Path, bridge: Arc<dyn Bridge>) -> io::Result<AndroidHost> {
        let keystore = Arc::new(AndroidKeystore::new(bridge.clone()));
        let clock = Arc::new(AndroidClock::new(bridge.clone()));
        let vault = Arc::new(VaultStore::open(data_dir.join(VAULT_FILE), keystore, clock));
        Self::with_vault(data_dir, cache_dir, bridge, vault)
    }

    /// The host over a vault that is already open.
    pub fn with_vault(
        data_dir: &Path,
        cache_dir: &Path,
        bridge: Arc<dyn Bridge>,
        vault: Arc<VaultStore>,
    ) -> io::Result<AndroidHost> {
        for dir in [data_dir, cache_dir] {
            if !dir.is_absolute() {
                return Err(io::Error::other(format!("the private folder {} is not an absolute path", dir.display())));
            }
        }
        let home = data_dir.join(HOME_FOLDER);
        make_private_dir(&home)?;
        std::fs::create_dir_all(cache_dir)?;
        Ok(AndroidHost {
            dirs: PrivateDirs { home, cache: cache_dir.to_owned() },
            vault,
            bridge,
            recents: Mutex::new(Recents::default()),
        })
    }

    /// Bring the app switcher picture in line with the vault: hidden while the protection is `auth` (locked or unlocked),
    /// shown otherwise. The plugin is called only when the wanted value differs from the one it took last, or when `resend`
    /// is set (a recreated activity forgets the flag). Reads the vault's status, which never prompts and never waits.
    /// A failed call changes nothing here, so the next round tries again. Answers whether a call was made and succeeded.
    pub fn sync_recents(&self, resend: bool) -> bool {
        let want = recents_hidden(&self.vault.status());
        {
            let state = self.recents.lock().unwrap_or_else(PoisonError::into_inner);
            if !resend && state.sent == Some(want) {
                return false;
            }
        }
        let answer = self.bridge.call(RECENTS, &recents_to_wire(want)).ok().and_then(|value| recents_from_wire(&value));
        let Some(answer) = answer else { return false };
        let mut state = self.recents.lock().unwrap_or_else(PoisonError::into_inner);
        state.sent = Some(want);
        state.answer = Some(answer);
        true
    }

    /// The plugin's last answer about the app switcher picture, for the debug self-test. A release build does not use it.
    #[cfg_attr(not(any(test, all(target_os = "android", debug_assertions))), allow(dead_code))]
    pub fn recents_answer(&self) -> Option<RecentsAnswer> {
        self.recents.lock().unwrap_or_else(PoisonError::into_inner).answer
    }

    /// The vault, for the debug self-test.
    #[cfg(all(target_os = "android", debug_assertions))]
    pub fn vault_store(&self) -> &Arc<VaultStore> {
        &self.vault
    }
}

/// The rounds of the watcher: one per second. Every thirtieth round the vault's clock rule is applied (`tick`), which locks a
/// vault whose five minutes have passed while the page stays hidden, and the app switcher value is sent again whether it
/// changed or not.
#[derive(Debug, Default)]
pub struct Rounds {
    seen: u32,
}

/// What a round of the watcher does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Round {
    pub tick: bool,
    pub resend: bool,
}

/// The seconds between two rounds, and how many rounds make the long one.
pub const ROUND_SECONDS: u64 = 1;
pub const LONG_ROUND: u32 = 30;

impl Rounds {
    pub fn next(&mut self) -> Round {
        self.seen = self.seen.wrapping_add(1);
        let long = self.seen % LONG_ROUND == 0;
        Round { tick: long, resend: long }
    }
}

/// Start the watcher: one thread that applies the app switcher value once at start and then every second, and every thirtieth
/// round also applies the vault's clock rule. Called once from `platform_host`, so a vault that is hidden for five minutes
/// locks without any call from the page.
#[cfg(target_os = "android")]
pub fn start_watcher(host: Arc<AndroidHost>) {
    let spawned = std::thread::Builder::new().name("hw-vault".into()).spawn(move || {
        host.sync_recents(true);
        let mut rounds = Rounds::default();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(ROUND_SECONDS));
            let round = rounds.next();
            if round.tick {
                host.vault.tick();
            }
            host.sync_recents(round.resend);
        }
    });
    if let Err(e) = spawned {
        eprintln!("holzwave: the vault watcher could not start its thread: {e}");
    }
}

/// Make `dir` and its missing parents, and set the mode of `dir` itself to 0700 (owner only) on Unix.
fn make_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

impl Host for AndroidHost {
    fn platform(&self) -> Platform {
        Platform::Android
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        self.vault.clone()
    }

    fn vault(&self) -> Option<Arc<VaultStore>> {
        Some(self.vault.clone())
    }

    fn local_network(&self) -> Arc<dyn Source> {
        crate::local_network::system().unwrap_or_else(|| Arc::new(Unsupported))
    }

    fn text_scale(&self) -> u32 {
        100
    }

    fn limits(&self) -> HostLimits {
        HostLimits::default()
    }

    /// The address of the app's own file scheme, which Android serves as `http://<scheme>.localhost/` like Windows.
    fn file_url(&self, token: &str) -> String {
        crate::files_protocol::url_of(token)
    }

    fn private_dirs(&self) -> Option<PrivateDirs> {
        Some(self.dirs.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::android::keystore::tests::Recorder;
    use holzwave_core::vault::{FakeClock, FakeKeystore, LOCKED_TEXT, MemoryFiles, Protection, VaultState};
    use holzwave_host::keystore_wire::RecentsMode;
    use holzwave_host::locked::is_locked;
    use serde_json::{Value, json};

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("holzwave-android-host-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn host_in(scratch: &Scratch) -> (AndroidHost, Arc<Recorder>) {
        let recorder = Arc::new(Recorder::default());
        let host = AndroidHost::with_bridge(&scratch.0.join("files"), &scratch.0.join("cache"), recorder.clone()).expect("a host");
        (host, recorder)
    }

    /// A host whose vault is the real one over a fake keystore, with a recorder for the plugin's other calls.
    fn host_with_fake_vault(scratch: &Scratch, keystore: Arc<FakeKeystore>) -> (AndroidHost, Arc<Recorder>) {
        let recorder = Arc::new(Recorder::default());
        let vault = Arc::new(VaultStore::open_with_files(
            scratch.0.join("files").join(VAULT_FILE),
            keystore,
            Arc::new(FakeClock::new(1)),
            Arc::new(MemoryFiles::default()),
        ));
        let host = AndroidHost::with_vault(&scratch.0.join("files"), &scratch.0.join("cache"), recorder.clone(), vault).expect("a host");
        (host, recorder)
    }

    #[test]
    fn the_host_is_android_and_names_the_folders_it_made() {
        let scratch = Scratch::new("folders");
        let data = scratch.0.join("files");
        let cache = scratch.0.join("cache");
        let recorder = Arc::new(Recorder::default());
        let host = AndroidHost::with_bridge(&data, &cache, recorder.clone()).expect("a host");
        assert_eq!(host.platform(), Platform::Android);
        let dirs = host.private_dirs().expect("the folders");
        assert_eq!(dirs.home, data.join("home"));
        assert_eq!(dirs.cache, cache);
        assert!(dirs.home.is_dir() && dirs.cache.is_dir(), "both exist before they are named");
        assert_ne!(dirs.home, data, "the shell's home is not the folder of the app's own files");
        assert!(dirs.home.starts_with(&data) && dirs.home != data);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dirs.home).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "the home folder is the owner's alone");
        }
        // Starting again over the same folders changes nothing and fails nowhere.
        let again = AndroidHost::with_bridge(&data, &cache, recorder).expect("a second host");
        assert_eq!(again.private_dirs(), host.private_dirs());
    }

    #[test]
    fn a_relative_folder_is_refused() {
        let scratch = Scratch::new("relative");
        let recorder = Arc::new(Recorder::default());
        assert!(AndroidHost::with_bridge(Path::new("files"), &scratch.0.join("cache"), recorder.clone()).is_err());
        assert!(AndroidHost::with_bridge(&scratch.0.join("files"), Path::new("cache"), recorder).is_err());
        assert!(!Path::new("files").exists() && !Path::new("cache").exists(), "nothing was made for a bad path");
    }

    #[test]
    fn the_hosts_secrets_and_vault_are_one_object_and_a_device_that_does_not_answer_keeps_nothing() {
        let scratch = Scratch::new("store");
        let (host, _recorder) = host_in(&scratch);
        let vault = host.vault().expect("a vault on Android");
        // The one store: whatever the page's vault commands see is what the AI chat and the connections read.
        assert_eq!(Arc::as_ptr(&vault) as *const (), Arc::as_ptr(&host.secrets()) as *const ());
        // The recorder has no answer for any command, which is a device that fails every call: no screen lock, so a plain
        // vault would be made, but its key cannot be, so the store says so and holds nothing.
        let refused = host.secrets().set("ssh:a", "canary-secret").expect_err("no key could be made");
        assert!(!refused.to_string().contains("canary-secret"));
        assert!(!scratch.0.join("files").join(VAULT_FILE).exists(), "no file without a key");
        assert_eq!(vault.state(), VaultState::New);
        assert_eq!(host.text_scale(), 100);
        assert_eq!(host.limits(), HostLimits::default());
        assert!(host.local_processes());
        assert_eq!(host.file_url("abc"), crate::files_protocol::url_of("abc"), "the one definition of the address");
        if cfg!(target_os = "android") {
            assert_eq!(host.file_url("abc"), "http://holzfile.localhost/abc");
        }
    }

    #[test]
    fn a_locked_vault_answers_the_locked_error_the_page_matches() {
        let scratch = Scratch::new("locked");
        let keystore = Arc::new(FakeKeystore::new());
        let (host, _recorder) = host_with_fake_vault(&scratch, keystore);
        let vault = host.vault().expect("a vault");
        vault.unlock().expect("a vault is made");
        vault.set("ssh:a", "x").expect("set");
        vault.lock();
        let error = host.secrets().get("ssh:a").expect_err("locked");
        assert!(is_locked(&error), "{error}");
        assert_eq!(error.to_string(), LOCKED_TEXT);
    }

    fn scripted_recents(recorder: &Recorder, applied: bool, mode: &str) {
        recorder.answer(RECENTS, Ok(json!({ "applied": applied, "mode": mode })));
    }

    fn recents_calls(recorder: &Recorder) -> Vec<Value> {
        recorder.payloads(RECENTS)
    }

    #[test]
    fn the_picture_is_hidden_exactly_while_the_vault_is_protected_and_the_plugin_is_told_only_when_that_changes() {
        let scratch = Scratch::new("recents");
        let keystore = Arc::new(FakeKeystore::new());
        let (host, recorder) = host_with_fake_vault(&scratch, keystore);
        scripted_recents(&recorder, true, "system");
        let vault = host.vault().expect("a vault").clone();
        // A new vault on a device with a screen lock will be protected: the picture is hidden from the start.
        assert!(host.sync_recents(false), "the first round tells the plugin");
        assert_eq!(recents_calls(&recorder), vec![json!({ "hidden": true })]);
        assert!(!host.sync_recents(false), "nothing changed: no call");
        assert!(!host.sync_recents(false));
        assert_eq!(recents_calls(&recorder).len(), 1);
        assert_eq!(host.recents_answer(), Some(RecentsAnswer { applied: true, mode: RecentsMode::System }));
        // Unlocked and locked change nothing: only the protection decides.
        vault.unlock().expect("unlock");
        assert!(!host.sync_recents(false));
        vault.lock();
        assert!(!host.sync_recents(false));
        assert_eq!(recents_calls(&recorder).len(), 1);
        // Plain mode shows it again, and says so once.
        vault.unlock().expect("unlock");
        vault.set_protection(Protection::Plain).expect("plain");
        scripted_recents(&recorder, false, "system");
        assert!(host.sync_recents(false));
        assert_eq!(recents_calls(&recorder).last(), Some(&json!({ "hidden": false })));
        assert_eq!(host.recents_answer(), Some(RecentsAnswer { applied: false, mode: RecentsMode::System }));
        assert!(!host.sync_recents(false));
        // Back to protected.
        vault.set_protection(Protection::Auth).expect("auth");
        scripted_recents(&recorder, true, "system");
        assert!(host.sync_recents(false));
        assert_eq!(recents_calls(&recorder).last(), Some(&json!({ "hidden": true })));
        assert_eq!(recents_calls(&recorder).len(), 3);
    }

    #[test]
    fn a_failed_call_is_tried_again_and_a_resend_repeats_the_value() {
        let scratch = Scratch::new("recents-retry");
        let (host, recorder) = host_with_fake_vault(&scratch, Arc::new(FakeKeystore::new()));
        // The plugin fails: nothing is recorded as sent, so the next round calls again.
        assert!(!host.sync_recents(false));
        assert!(!host.sync_recents(false));
        assert_eq!(recents_calls(&recorder).len(), 2, "a failure is retried at every round");
        assert_eq!(host.recents_answer(), None);
        // An answer that cannot be read is a failure too.
        recorder.answer(RECENTS, Ok(json!({ "applied": "yes" })));
        assert!(!host.sync_recents(false));
        assert_eq!(host.recents_answer(), None);
        scripted_recents(&recorder, true, "system");
        assert!(host.sync_recents(false));
        assert!(!host.sync_recents(false));
        let calls = recents_calls(&recorder).len();
        assert!(host.sync_recents(true), "a resend calls even when nothing changed");
        assert_eq!(recents_calls(&recorder).len(), calls + 1);
        assert_eq!(recents_calls(&recorder).last(), Some(&json!({ "hidden": true })));
    }

    #[test]
    fn android_12_answers_unsupported_and_the_host_keeps_that_as_it_is() {
        let scratch = Scratch::new("recents-12");
        let (host, recorder) = host_with_fake_vault(&scratch, Arc::new(FakeKeystore::new()));
        scripted_recents(&recorder, false, "unsupported");
        assert!(host.sync_recents(false));
        assert_eq!(host.recents_answer(), Some(RecentsAnswer { applied: false, mode: RecentsMode::Unsupported }));
        assert!(!host.sync_recents(false), "an unsupported system is asked once, not every second");
    }

    #[test]
    fn a_device_without_a_screen_lock_has_a_plain_vault_and_shows_the_picture() {
        let scratch = Scratch::new("recents-nolock");
        let (host, recorder) = host_with_fake_vault(&scratch, Arc::new(FakeKeystore::without_screen_lock()));
        scripted_recents(&recorder, false, "system");
        assert!(host.sync_recents(false));
        assert_eq!(recents_calls(&recorder), vec![json!({ "hidden": false })]);
    }

    #[test]
    fn the_watcher_rounds_apply_the_clock_rule_and_resend_every_thirtieth_second() {
        let mut rounds = Rounds::default();
        let mut long = Vec::new();
        for n in 1..=90u32 {
            let round = rounds.next();
            assert_eq!(round.tick, round.resend);
            if round.tick {
                long.push(n);
            }
        }
        assert_eq!(long, vec![30, 60, 90]);
        assert_eq!((ROUND_SECONDS, LONG_ROUND), (1, 30));
    }
}
