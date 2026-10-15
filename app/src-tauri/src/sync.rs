//! The sync of the app: the account (`sync.json`; the app password and the passphrase are in the OS keychain), the runs,
//! and the state the page shows. The format, the transport and the merge are in `holzwave_core::sync`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use holzwave_core::persist::{read_limited, write_atomic};
use holzwave_core::secrets::SecretStore;
use holzwave_core::sync::engine::{ConfigSource, Report, Syncer};
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
    /// Something was downloaded since the start; it is applied at the next start.
    pub restart: bool,
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct SyncRuntime {
    dir: PathBuf,
    secrets: Arc<dyn SecretStore>,
    view: Mutex<SyncView>,
    running: AtomicBool,
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
        Self { dir, secrets, view: Mutex::new(view), running: AtomicBool::new(false) }
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
        self.set(|v| v.state = "syncing");
        let outcome = self.once(&account);
        self.running.store(false, Ordering::Release);
        match outcome {
            Ok(report) => {
                self.set(|v| {
                    v.state = "idle";
                    v.last = Some(now());
                    v.files = report.uploaded.len() + report.downloaded.len();
                    v.conflicts = report.conflicts.len();
                    v.restart |= !report.downloaded.is_empty();
                    v.error = report.errors.first().map(|(name, why)| format!("{name}: {why}"));
                    if v.error.is_some() {
                        v.state = "error";
                    }
                });
                Ok(self.view())
            }
            Err(e) => {
                self.set(|v| {
                    v.state = "error";
                    v.error = Some(e.clone());
                });
                Err(e)
            }
        }
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
}
