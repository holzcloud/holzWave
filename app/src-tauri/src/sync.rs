//! The sync of the app: the account (`sync.json`; the app password and the passphrase are in the OS keychain), the runs,
//! and the state the page shows. The format, the transport and the merge are in `holzwave_core::sync`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use holzwave_core::persist::{read_limited, write_atomic};
use holzwave_core::secrets::SecretStore;
use holzwave_core::sync::engine::{ConfigSource, Promoted, Report, Syncer, is_unowned};
use holzwave_core::sync::webdav::WebDav;
use serde::{Deserialize, Serialize};

const FILE: &str = "sync.json";
const PASSWORD: &str = "sync:password";
const PASSPHRASE: &str = "sync:passphrase";
/// The shortest passphrase accepted. It is stretched by Argon2id, but it is all that protects the data on the server.
pub const MIN_PASSPHRASE: usize = 10;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Account {
    endpoint: String,
    user: String,
}

/// What the shell does after a run, with the files the running app does not own: moves them into place. Set once.
pub type Applier = Arc<dyn Fn() -> Promoted + Send + Sync>;

/// What the page shows.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct SyncView {
    pub configured: bool,
    pub endpoint: String,
    pub user: String,
    /// `off`, `idle`, `syncing` or `error`.
    pub state: &'static str,
    /// Seconds since the epoch of the last successful run.
    pub last: Option<i64>,
    /// Files sent and received by the last run.
    pub files: usize,
    /// Conflicts in the last run (a copy of the losing version is kept).
    pub conflicts: usize,
    pub error: Option<String>,
    /// A downloaded file still waits for the next start (the files the running app owns, or one that could not be
    /// applied at once).
    pub restart: bool,
    /// Files another computer removed in the last run (a copy of each is kept in `sync-conflicts`).
    pub removed: usize,
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Clears the flag of a run in progress when it goes out of scope.
struct Running<'a>(&'a AtomicBool);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub struct SyncRuntime {
    dir: PathBuf,
    secrets: Arc<dyn SecretStore>,
    view: Mutex<SyncView>,
    running: AtomicBool,
    /// Applies the downloads the app does not own right after a run; bound by the core that owns this runtime.
    applier: OnceLock<Applier>,
}

impl SyncRuntime {
    pub fn new(dir: PathBuf, secrets: Arc<dyn SecretStore>) -> Self {
        let account = Self::read_account(&dir);
        let view = SyncView {
            configured: account.is_some(),
            endpoint: account.as_ref().map(|a| a.endpoint.clone()).unwrap_or_default(),
            user: account.map(|a| a.user).unwrap_or_default(),
            state: "off",
            ..SyncView::default()
        };
        let view = SyncView { state: if view.configured { "idle" } else { "off" }, ..view };
        Self { dir, secrets, view: Mutex::new(view), running: AtomicBool::new(false), applier: OnceLock::new() }
    }

    /// Set the step that applies the downloads of the files the app does not own. Once; a second call is ignored. The
    /// closure should hold a weak reference to the core, so the runtime never keeps the core alive.
    pub fn bind_applier(&self, applier: Applier) {
        let _ = self.applier.set(applier);
    }

    fn read_account(dir: &std::path::Path) -> Option<Account> {
        let bytes = read_limited(&dir.join(FILE), 64 * 1024).ok()??;
        let account: Account = serde_json::from_slice(&bytes).ok()?;
        (!account.endpoint.is_empty()).then_some(account)
    }

    pub fn view(&self) -> SyncView {
        lock(&self.view).clone()
    }

    fn set(&self, f: impl FnOnce(&mut SyncView)) {
        f(&mut lock(&self.view));
    }

    /// Turn the sync on for an account and run it once. A wrong address, password or passphrase is refused and nothing is
    /// kept. The first run also makes the folder on a new server, with this passphrase.
    pub fn save(&self, endpoint: &str, user: &str, password: &str, passphrase: &str) -> Result<SyncView, String> {
        let endpoint = endpoint.trim();
        let user = user.trim();
        if passphrase.chars().count() < MIN_PASSPHRASE {
            return Err(format!("The passphrase needs at least {MIN_PASSPHRASE} characters."));
        }
        if password.is_empty() {
            return Err("The app password is empty.".into());
        }
        WebDav::new(endpoint, user, password).map_err(|e| e.to_string())?;
        let before = (self.secrets.get(PASSWORD).ok().flatten(), self.secrets.get(PASSPHRASE).ok().flatten(), Self::read_account(&self.dir));
        self.secrets.set(PASSWORD, password).map_err(|e| e.to_string())?;
        self.secrets.set(PASSPHRASE, passphrase).map_err(|e| e.to_string())?;
        let account = Account { endpoint: endpoint.to_owned(), user: user.to_owned() };
        write_atomic(&self.dir.join(FILE), &serde_json::to_vec_pretty(&account).unwrap_or_default()).map_err(|e| e.to_string())?;
        self.set(|v| {
            v.configured = true;
            v.endpoint.clone_from(&account.endpoint);
            v.user.clone_from(&account.user);
            v.state = "idle";
        });
        match self.run() {
            Ok(view) => Ok(view),
            Err(e) => {
                // Put back what was there before, so a typo does not replace a working account.
                let restore = |account: &str, value: Option<String>| match value {
                    Some(v) => self.secrets.set(account, &v),
                    None => self.secrets.delete(account),
                };
                let _ = restore(PASSWORD, before.0);
                let _ = restore(PASSPHRASE, before.1);
                match before.2 {
                    Some(old) => {
                        let _ = write_atomic(&self.dir.join(FILE), &serde_json::to_vec_pretty(&old).unwrap_or_default());
                        self.set(|v| {
                            v.endpoint = old.endpoint;
                            v.user = old.user;
                            v.state = "idle";
                            v.error = None;
                        });
                    }
                    None => {
                        let _ = std::fs::remove_file(self.dir.join(FILE));
                        self.set(|v| *v = SyncView { state: "off", ..SyncView::default() });
                    }
                }
                Err(e)
            }
        }
    }

    /// Turn the sync off and forget the account. The files on the server and on this computer stay.
    pub fn off(&self) -> SyncView {
        let _ = self.secrets.delete(PASSWORD);
        let _ = self.secrets.delete(PASSPHRASE);
        let _ = std::fs::remove_file(self.dir.join(FILE));
        let _ = std::fs::remove_file(self.dir.join("sync_state.json"));
        self.set(|v| *v = SyncView { state: "off", ..SyncView::default() });
        self.view()
    }

    /// One run now. Blocks; call it off the main thread. A run that is already going is not doubled.
    pub fn run(&self) -> Result<SyncView, String> {
        let account = Self::read_account(&self.dir).ok_or("Sync is not set up.")?;
        if self.running.swap(true, Ordering::AcqRel) {
            return Ok(self.view());
        }
        // Cleared when this function ends, however it ends.
        let _running = Running(&self.running);
        self.set(|v| v.state = "syncing");
        let outcome = self.once(&account);
        let result = match outcome {
            // `running` stays set until the apply is done, so no second run stages a file while this one moves them.
            Ok(report) => Ok(self.complete(&report)),
            Err(e) => {
                self.set(|v| {
                    v.state = "error";
                    v.error = Some(e.clone());
                });
                Err(e)
            }
        };
        result
    }

    /// After a successful run: what the running app does not own is moved into place now, the rest stays staged for
    /// the next start; then the view is finished. The applier takes no lock of this runtime.
    fn complete(&self, report: &Report) -> SyncView {
        let promoted = self.applier.get().map(|apply| apply()).unwrap_or_default();
        self.finish(report, &promoted);
        self.view()
    }

    /// The view after a successful run: the run's own report plus what was applied at once. A copy kept before a
    /// download replaced an edit counts as a conflict; a download of a file the app owns, or a staged file that could not
    /// be applied, asks for a restart.
    fn finish(&self, report: &Report, promoted: &Promoted) {
        self.set(|v| {
            v.state = "idle";
            v.last = Some(now());
            v.files = report.uploaded.len() + report.downloaded.len();
            v.conflicts = report.conflicts.len() + promoted.kept.len();
            v.removed = report.removed.len();
            v.restart |= report.downloaded.iter().any(|name| !is_unowned(name)) || !promoted.skipped.is_empty();
            v.error = report.errors.first().map(|(name, why)| format!("{name}: {why}"));
            if v.error.is_some() {
                v.state = "error";
            }
        });
    }

    fn once(&self, account: &Account) -> Result<Report, String> {
        let password = self.secrets.get(PASSWORD).map_err(|e| e.to_string())?.ok_or("The app password is missing from the keychain.")?;
        let passphrase = self.secrets.get(PASSPHRASE).map_err(|e| e.to_string())?.ok_or("The passphrase is missing from the keychain.")?;
        let backend = WebDav::new(&account.endpoint, &account.user, &password).map_err(|e| e.to_string())?;
        let source = ConfigSource::new(&self.dir, self.secrets.clone());
        let mut syncer = Syncer::connect(backend, source, &self.dir, &passphrase).map_err(|e| e.to_string())?;
        syncer.sync_once().map_err(|e| e.to_string())
    }

    /// The scheduled run: nothing when sync is off, and no complaint beyond the state the page shows.
    pub fn background(&self) {
        if self.view().configured {
            let _ = self.run();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holzwave_core::secrets::MemoryStore;

    fn runtime(tag: &str) -> (SyncRuntime, Arc<MemoryStore>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("hw-app-sync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let secrets = Arc::new(MemoryStore::default());
        (SyncRuntime::new(dir.clone(), secrets.clone()), secrets, dir)
    }

    #[test]
    fn a_new_install_is_off_and_a_bad_account_is_refused_without_keeping_anything() {
        let (rt, secrets, dir) = runtime("a");
        assert_eq!(rt.view().state, "off");
        assert!(rt.run().is_err(), "nothing to run");
        let short = rt.save("https://cloud.example/dav", "u", "p", "short").unwrap_err();
        assert!(short.contains("at least"), "{short}");
        assert!(rt.save("http://cloud.example/dav", "u", "p", "long enough pass").unwrap_err().contains("https"));
        assert!(rt.save("https://cloud.example/dav", "u", "", "long enough pass").unwrap_err().contains("empty"));
        assert!(secrets.get(PASSWORD).unwrap().is_none() && !dir.join(FILE).exists());
    }

    #[test]
    fn a_failed_first_run_leaves_no_account_behind() {
        let (rt, secrets, dir) = runtime("b");
        // A port nobody listens on: the run fails, and so does the save.
        let e = rt.save("http://127.0.0.1:1/dav", "u", "p", "long enough pass").unwrap_err();
        assert!(!e.is_empty());
        assert!(secrets.get(PASSWORD).unwrap().is_none() && secrets.get(PASSPHRASE).unwrap().is_none());
        assert!(!dir.join(FILE).exists());
        assert_eq!(rt.view().state, "off");
        assert!(!rt.view().configured);
    }

    #[test]
    fn the_account_is_read_again_at_the_next_start_and_off_forgets_it() {
        let (rt, secrets, dir) = runtime("c");
        write_atomic(&dir.join(FILE), br#"{"endpoint":"https://cloud.example/dav","user":"holz"}"#).unwrap();
        secrets.set(PASSWORD, "pw").unwrap();
        let again = SyncRuntime::new(dir.clone(), secrets.clone());
        let v = again.view();
        assert!(v.configured && v.state == "idle" && v.user == "holz");
        assert_eq!(rt.view().state, "off", "the first runtime did not see the file");
        let v = again.off();
        assert!(!v.configured && v.state == "off");
        assert!(secrets.get(PASSWORD).unwrap().is_none() && !dir.join(FILE).exists());
    }

    fn report(uploaded: &[&str], downloaded: &[&str], conflicts: &[&str]) -> Report {
        let own = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        Report { uploaded: own(uploaded), downloaded: own(downloaded), conflicts: own(conflicts), errors: Vec::new(), ..Report::default() }
    }

    fn applied(names: &[&str], kept: &[&str], skipped: &[&str]) -> Promoted {
        let own = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        Promoted { applied: own(names), kept: own(kept), skipped: own(skipped), ..Promoted::default() }
    }

    /// A runtime whose applier answers `promoted` and counts its calls.
    fn applying(tag: &str, promoted: Promoted) -> (SyncRuntime, Arc<std::sync::atomic::AtomicUsize>, PathBuf) {
        let (rt, _secrets, dir) = runtime(tag);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        let promoted = Arc::new(Mutex::new(promoted));
        rt.bind_applier(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            std::mem::take(&mut *lock(&promoted))
        }));
        (rt, calls, dir)
    }

    #[test]
    fn the_sync_run_applies_what_it_does_not_own() {
        let (rt, calls, _dir) = applying("d", applied(&["themes/mine.json"], &[], &[]));
        // A theme alone is applied at once: nothing waits, so the popover does not ask for a restart.
        let view = rt.complete(&report(&[], &["themes/mine.json"], &[]));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the applier runs once after the run");
        assert!(!view.restart, "{view:?}");
        assert_eq!(view.files, 1);
        // A download of a file the app owns waits for a start.
        let view = rt.complete(&report(&[], &["themes/mine.json", "settings.json"], &[]));
        assert!(view.restart, "{view:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // The flag stays set until the process ends, as before.
        let view = rt.complete(&report(&[], &["themes/mine.json"], &[]));
        assert!(view.restart);
    }

    #[test]
    fn the_view_counts_what_another_computer_removed() {
        let (rt, _calls, _dir) = applying("rm", Promoted::default());
        let gone = Report { removed: vec!["themes/a.json".into(), "widgets/b.json".into()], ..Report::default() };
        assert_eq!(rt.complete(&gone).removed, 2);
        // The count is of the last run, not a running total.
        assert_eq!(rt.complete(&Report::default()).removed, 0);
        let one = Report { removed: vec!["themes/a.json".into()], ..Report::default() };
        assert_eq!(rt.complete(&one).removed, 1);
        assert_eq!(rt.complete(&one).removed, 1, "not cumulative");
        assert_eq!(rt.off().removed, 0, "turned off: nothing to say");
    }

    #[test]
    fn each_file_the_app_owns_asks_for_a_restart_and_each_other_one_does_not() {
        for owned in ["layout.json", "settings.json", "connections.json"] {
            let (rt, _calls, _dir) = applying(&format!("o-{}", owned.trim_end_matches(".json")), Promoted::default());
            assert!(rt.complete(&report(&[], &[owned], &[])).restart, "{owned}");
        }
        for free in ["themes/a.json", "widgets/b.json", "widget_notes.json"] {
            let (rt, _calls, _dir) = applying(&format!("f-{}", free.replace(['/', '.'], "-")), Promoted::default());
            assert!(!rt.complete(&report(&[], &[free], &[])).restart, "{free}");
        }
    }

    #[test]
    fn a_staged_file_that_cannot_be_applied_asks_for_a_restart() {
        let (rt, _calls, _dir) = applying("s", applied(&[], &[], &["widgets/deploy.json"]));
        let view = rt.complete(&report(&[], &["widgets/deploy.json"], &[]));
        assert!(view.restart, "{view:?}");
    }

    #[test]
    fn a_copy_kept_before_a_download_replaced_an_edit_counts_as_a_conflict() {
        let (rt, _calls, _dir) = applying("k", applied(&["widgets/deploy.json"], &["widgets/deploy.json"], &[]));
        let view = rt.complete(&report(&["a"], &["widgets/deploy.json", "themes/x.json"], &["layout.json"]));
        assert_eq!(view.conflicts, 2, "the run's conflict and the kept copy: {view:?}");
        assert_eq!(view.files, 3, "the apply does not change the count of files");
        assert!(!view.restart);
    }

    #[test]
    fn without_an_applier_a_run_finishes_as_before_and_a_failed_run_calls_none() {
        let (rt, _secrets, _dir) = runtime("n");
        let view = rt.complete(&report(&[], &["settings.json"], &[]));
        assert!(view.restart && view.state == "idle");
        let (rt, calls, dir) = applying("m", Promoted::default());
        write_atomic(&dir.join(FILE), br#"{"endpoint":"http://127.0.0.1:1/dav","user":"u"}"#).unwrap();
        assert!(rt.run().is_err(), "no password in the keychain");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "a failed run applies nothing");
        assert_eq!(rt.view().state, "error");
        assert!(!rt.running.load(Ordering::SeqCst), "the flag of a run in progress is cleared");
    }
}
