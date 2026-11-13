//! The sync of the app: the account (`sync.json`; the app password and the passphrase are in the OS keychain), the runs,
//! and the state the page shows. The format, the transport and the merge are in `holzwave_core::sync`.
//!
//! Two transports carry the same encrypted files with the same passphrase: WebDAV (an address, a user and an app password) and
//! the holzWave server this device is signed in to ([`crate::server`]). Changing the transport copies nothing between the two
//! targets and removes no file of the person: it only forgets what the engine recorded about the old target, so the first run
//! against the new one treats it as new and never reads an empty target as the deletion of local files.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use holzwave_core::persist::{read_limited, write_atomic};
use holzwave_core::secrets::SecretStore;
use holzwave_core::server_link::Fault as LinkFault;
use holzwave_core::sync::engine::{ConfigSource, Promoted, Report, Syncer, is_unowned};
use holzwave_core::sync::server::ServerBackend;
use holzwave_core::sync::webdav::{Backend, WebDav};
use serde::{Deserialize, Serialize};

use crate::locked::{SECRETS_LOCKED, is_locked};
use crate::server::{KEYCHAIN, ServerRuntime, Unavailable};

const FILE: &str = "sync.json";
/// What the engine records about the target it last synced with.
const STATE_FILE: &str = "sync_state.json";
/// Where the record of the old target waits while the first run against a new one is made.
const STATE_PARKED: &str = "sync_state.json.previous";
/// What a run says when the sync was set up with another server than the one this device is signed in to now.
pub const OTHER_TARGET: &str = "The sync was set up with another server. Turn it on again for this one.";

/// Whether two addresses name the same server: compared as origins, so a host written in another case (a set-up stored
/// before addresses were kept in lower case) is the same server. An address that is not an origin is compared as it is.
fn same_server(a: &str, b: &str) -> bool {
    use holzwave_core::server_link::Origin;
    match (Origin::parse(a), Origin::parse(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
const PASSWORD: &str = "sync:password";
const PASSPHRASE: &str = "sync:passphrase";
/// The shortest passphrase accepted. It is stretched by Argon2id, but it is all that protects the data on the server.
pub const MIN_PASSPHRASE: usize = 10;

/// Which target the sync talks to.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// A WebDAV folder (Nextcloud and the like). What every `sync.json` of an older build means.
    #[default]
    Webdav,
    /// The holzWave server this device is signed in to.
    Server,
}

/// The account: for WebDAV the address and the user; for the server the address of the server the sync was set up with (a run
/// refuses another one, see [`OTHER_TARGET`]).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Account {
    endpoint: String,
    user: String,
    #[serde(default)]
    transport: Transport,
}

/// What the shell does after a run, with the files the running app does not own: moves them into place. Set once.
pub type Applier = Arc<dyn Fn() -> Promoted + Send + Sync>;

/// What the page shows.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct SyncView {
    pub configured: bool,
    /// Which target the sync uses.
    pub transport: Transport,
    /// The WebDAV address, or the address of the server.
    pub endpoint: String,
    pub user: String,
    /// `off`, `idle`, `syncing`, `paused`, `signed_out` or `error`. `paused` is a locked secret store: the app password and
    /// the passphrase cannot be read until the person unlocks it, no run is made and there is no error text; the first
    /// unlock starts one run at once. `signed_out` is a server that refused the token of this device (it was revoked or it
    /// ran out): no run is made until the person signs in again, and there is no error text.
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

/// Why a run could not be made.
enum Fault {
    /// The secret store is locked: the run waits for the unlock and is not an error.
    Locked,
    /// The server refused the token of this device: the run waits for a new sign-in and is not a generic error.
    SignedOut,
    /// Anything else, as the text the page shows.
    Failed(String),
}

/// Whether `text` is the sentence of a token the server refused.
fn is_signed_out(text: &str) -> bool {
    text == LinkFault::SignedOut.sentence()
}

impl Fault {
    /// The fault of a failure of the core: a locked store and a refused token are their own cases, everything else is its text.
    fn of(error: impl Into<holzwave_core::Error>) -> Fault {
        let error = error.into();
        if is_locked(&error) {
            Fault::Locked
        } else if is_signed_out(&error.to_string()) {
            Fault::SignedOut
        } else {
            Fault::Failed(error.to_string())
        }
    }
}

impl From<&str> for Fault {
    fn from(text: &str) -> Fault {
        Fault::Failed(text.to_owned())
    }
}

/// What a change of the account puts back when its first run fails.
struct Before {
    password: Option<String>,
    passphrase: Option<String>,
    account: Option<Account>,
    /// The bytes of the account file as they were, written back as they were.
    account_bytes: Option<Vec<u8>>,
    /// The record of the old target was set aside (see [`SyncRuntime::park_state`]).
    parked: bool,
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
    /// The link to the server, for the server transport.
    server: Arc<ServerRuntime>,
}

impl SyncRuntime {
    pub fn new(dir: PathBuf, secrets: Arc<dyn SecretStore>, server: Arc<ServerRuntime>) -> Self {
        let account = Self::read_account(&dir);
        let view = SyncView {
            configured: account.is_some(),
            transport: account.as_ref().map(|a| a.transport).unwrap_or_default(),
            endpoint: account.as_ref().map(|a| a.endpoint.clone()).unwrap_or_default(),
            user: account.map(|a| a.user).unwrap_or_default(),
            state: "off",
            ..SyncView::default()
        };
        let view = SyncView { state: if view.configured { "idle" } else { "off" }, ..view };
        Self { dir, secrets, view: Mutex::new(view), running: AtomicBool::new(false), applier: OnceLock::new(), server }
    }

    /// Set the step that applies the downloads of the files the app does not own. Once; a second call is ignored. The
    /// closure should hold a weak reference to the core, so the runtime never keeps the core alive.
    pub fn bind_applier(&self, applier: Applier) {
        let _ = self.applier.set(applier);
    }

    fn read_account(dir: &std::path::Path) -> Option<Account> {
        let bytes = read_limited(&dir.join(FILE), 64 * 1024).ok()??;
        let account: Account = serde_json::from_slice(&bytes).ok()?;
        // A WebDAV account has an address; a server account has the address of its server, and is nothing without one.
        (!account.endpoint.is_empty()).then_some(account)
    }

    pub fn view(&self) -> SyncView {
        lock(&self.view).clone()
    }

    fn set(&self, f: impl FnOnce(&mut SyncView)) {
        f(&mut lock(&self.view));
    }

    /// Everything a change of the account could lose, taken before it is made, so that a run that fails puts it all back.
    fn before(&self) -> Before {
        Before {
            password: self.secrets.get(PASSWORD).ok().flatten(),
            passphrase: self.secrets.get(PASSPHRASE).ok().flatten(),
            account: Self::read_account(&self.dir),
            account_bytes: read_limited(&self.dir.join(FILE), 64 * 1024).ok().flatten(),
            parked: false,
        }
    }

    /// Sets the record of the old target aside, so the first run against a new target treats it as new. Nothing else is touched:
    /// no file of the person, no copy between targets. `true` when there was a record to set aside.
    fn park_state(&self) -> bool {
        std::fs::rename(self.dir.join(STATE_FILE), self.dir.join(STATE_PARKED)).is_ok()
    }

    /// A change of the account ended well: the record of the old target is not needed any more.
    fn conclude(&self, before: &Before) {
        if before.parked {
            let _ = std::fs::remove_file(self.dir.join(STATE_PARKED));
        }
    }

    /// A first run failed: what was there before is back, so a typo does not replace a working account.
    fn put_back(&self, before: Before) {
        let restore = |account: &str, value: Option<String>| match value {
            Some(v) => self.secrets.set(account, &v),
            None => self.secrets.delete(account),
        };
        let _ = restore(PASSWORD, before.password);
        let _ = restore(PASSPHRASE, before.passphrase);
        if before.parked {
            let _ = std::fs::rename(self.dir.join(STATE_PARKED), self.dir.join(STATE_FILE));
        }
        match before.account {
            Some(old) => {
                let bytes = before.account_bytes.unwrap_or_else(|| serde_json::to_vec_pretty(&old).unwrap_or_default());
                let _ = write_atomic(&self.dir.join(FILE), &bytes);
                self.set(|v| {
                    v.transport = old.transport;
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
        // A locked store is said before anything is written: no account file, no secret, no state change.
        if self.locked() {
            return Err(SECRETS_LOCKED.to_owned());
        }
        let mut before = self.before();
        // A sync that was on the server has a record of that target: set it aside, so WebDAV is a new target.
        before.parked = matches!(before.account, Some(Account { transport: Transport::Server, .. })) && self.park_state();
        self.secrets.set(PASSWORD, password).map_err(|e| e.to_string())?;
        if let Err(e) = self.secrets.set(PASSPHRASE, passphrase) {
            // The first write went through and the second did not (the store locked in between): put the first back.
            let _ = match &before.password {
                Some(old) => self.secrets.set(PASSWORD, old),
                None => self.secrets.delete(PASSWORD),
            };
            if before.parked {
                let _ = std::fs::rename(self.dir.join(STATE_PARKED), self.dir.join(STATE_FILE));
            }
            return Err(e.to_string());
        }
        let account = Account { endpoint: endpoint.to_owned(), user: user.to_owned(), transport: Transport::Webdav };
        if let Err(e) = write_atomic(&self.dir.join(FILE), &serde_json::to_vec_pretty(&account).unwrap_or_default()) {
            self.put_back(before);
            return Err(e.to_string());
        }
        self.set(|v| {
            v.configured = true;
            v.transport = Transport::Webdav;
            v.endpoint.clone_from(&account.endpoint);
            v.user.clone_from(&account.user);
            v.state = "idle";
        });
        match self.run() {
            Ok(view) => {
                self.conclude(&before);
                Ok(view)
            }
            Err(e) => {
                self.put_back(before);
                Err(e)
            }
        }
    }

    /// Use the server this device is signed in to as the transport of the sync, with the person's passphrase, and run it once.
    /// The same passphrase and the same encrypted files as WebDAV; a passphrase that does not open the folder on the server is
    /// refused and nothing is kept. Changing the target copies nothing and removes no file: the engine's record of the old target
    /// is set aside, and the first run treats the server as new. A failed first run puts everything back as [`SyncRuntime::save`]
    /// does.
    pub fn use_server(&self, passphrase: &str) -> Result<SyncView, String> {
        if passphrase.chars().count() < MIN_PASSPHRASE {
            return Err(format!("The passphrase needs at least {MIN_PASSPHRASE} characters."));
        }
        let (token, origin) = self.server.token_and_origin().map_err(|unavailable| unavailable.sentence().to_owned())?;
        ServerBackend::new(&origin, &token).map_err(|e| e.to_string())?;
        drop(token);
        // A locked store is said before anything is written.
        if self.locked_for(Transport::Server) {
            return Err(SECRETS_LOCKED.to_owned());
        }
        let mut before = self.before();
        let same_target = matches!(&before.account, Some(Account { transport: Transport::Server, endpoint, .. }) if same_server(endpoint, &origin));
        before.parked = !same_target && self.park_state();
        if let Err(e) = self.secrets.set(PASSPHRASE, passphrase) {
            if before.parked {
                let _ = std::fs::rename(self.dir.join(STATE_PARKED), self.dir.join(STATE_FILE));
            }
            return Err(e.to_string());
        }
        let account = Account { endpoint: origin, user: String::new(), transport: Transport::Server };
        if let Err(e) = write_atomic(&self.dir.join(FILE), &serde_json::to_vec_pretty(&account).unwrap_or_default()) {
            self.put_back(before);
            return Err(e.to_string());
        }
        self.set(|v| {
            v.configured = true;
            v.transport = Transport::Server;
            v.endpoint.clone_from(&account.endpoint);
            v.user.clear();
            v.state = "idle";
        });
        match self.run() {
            // The server refused the token in the first run: nothing was synced, so nothing is kept.
            Ok(view) if view.state == "signed_out" => {
                self.put_back(before);
                Err(LinkFault::SignedOut.sentence().to_owned())
            }
            Ok(view) => {
                // The app password of the old WebDAV account is of no use any more.
                if matches!(before.account, Some(Account { transport: Transport::Webdav, .. })) {
                    let _ = self.secrets.delete(PASSWORD);
                }
                self.conclude(&before);
                Ok(view)
            }
            Err(e) => {
                self.put_back(before);
                Err(e)
            }
        }
    }

    /// Turn the sync off and forget the account. The files on the server and on this computer stay.
    pub fn off(&self) -> SyncView {
        let _ = self.secrets.delete(PASSWORD);
        let _ = self.secrets.delete(PASSPHRASE);
        let _ = std::fs::remove_file(self.dir.join(FILE));
        let _ = std::fs::remove_file(self.dir.join(STATE_FILE));
        let _ = std::fs::remove_file(self.dir.join(STATE_PARKED));
        self.set(|v| *v = SyncView { state: "off", ..SyncView::default() });
        self.view()
    }

    /// One run now. Blocks; call it off the main thread. A run that is already going is not doubled.
    pub fn run(&self) -> Result<SyncView, String> {
        let account = Self::read_account(&self.dir).ok_or("Sync is not set up.")?;
        // A locked store pauses the sync without a run, a network call or a state that flickers through `syncing`.
        if self.locked_for(account.transport) {
            self.pause();
            return Ok(self.view());
        }
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
            // The store locked between the check above and the read of the password: the same pause, not an error.
            Err(Fault::Locked) => {
                self.pause();
                Ok(self.view())
            }
            // The server refused the token: the run waits for a new sign-in. A state of its own, not an error.
            Err(Fault::SignedOut) => {
                self.set(|v| {
                    v.state = "signed_out";
                    v.error = None;
                });
                Ok(self.view())
            }
            Err(Fault::Failed(e)) => {
                self.set(|v| {
                    v.state = "error";
                    v.error = Some(e.clone());
                });
                Err(e)
            }
        };
        result
    }

    /// Whether the secret store is locked now: what the transport of the sync needs from it cannot be read because of it.
    fn locked(&self) -> bool {
        let transport = lock(&self.view).transport;
        self.locked_for(transport)
    }

    /// [`SyncRuntime::locked`] for a given transport: WebDAV reads the app password, the server the passphrase.
    fn locked_for(&self, transport: Transport) -> bool {
        let account = match transport {
            Transport::Webdav => PASSWORD,
            Transport::Server => PASSPHRASE,
        };
        matches!(self.secrets.get(account), Err(error) if is_locked(&error))
    }

    /// The state of a sync that waits for the unlock: no error text, and what the last run reported stays as it was.
    fn pause(&self) {
        self.set(|v| {
            v.state = "paused";
            v.error = None;
        });
    }

    /// Whether a run is due now because the store was unlocked: sync is set up and its state is `paused`.
    pub fn resume_due(&self) -> bool {
        let view = lock(&self.view);
        view.configured && view.state == "paused"
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
            // Every file that failed failed because the store locked during the run: the run waits for the unlock.
            if !report.errors.is_empty() && report.errors.iter().all(|(_, why)| why == SECRETS_LOCKED) {
                v.state = "paused";
                v.error = None;
            }
            // Every file that failed failed because the server refused the token during the run: the run waits for a sign-in.
            if !report.errors.is_empty() && report.errors.iter().all(|(_, why)| is_signed_out(why)) {
                v.state = "signed_out";
                v.error = None;
            }
        });
    }

    fn once(&self, account: &Account) -> Result<Report, Fault> {
        #[cfg(target_os = "android")]
        crate::tls_ready::require().map_err(Fault::from)?;
        match account.transport {
            Transport::Webdav => {
                let password = self.secrets.get(PASSWORD).map_err(Fault::of)?.ok_or("The app password is missing from the keychain.")?;
                let passphrase = self.secrets.get(PASSPHRASE).map_err(Fault::of)?.ok_or("The passphrase is missing from the keychain.")?;
                let backend = WebDav::new(&account.endpoint, &account.user, &password).map_err(Fault::of)?;
                self.sync_with(backend, &passphrase)
            }
            Transport::Server => {
                let passphrase = self.secrets.get(PASSPHRASE).map_err(Fault::of)?.ok_or("The passphrase is missing from the keychain.")?;
                let (token, origin) = self.server.token_and_origin().map_err(|unavailable| match unavailable {
                    Unavailable::Locked => Fault::Locked,
                    Unavailable::SignedOut => Fault::SignedOut,
                    Unavailable::Keychain => Fault::Failed(KEYCHAIN.to_owned()),
                })?;
                // The record of the engine belongs to the server the sync was set up with: another one is a new target, and a run
                // against it would read its empty folder as the deletion of every file.
                if !same_server(&origin, &account.endpoint) {
                    return Err(Fault::Failed(OTHER_TARGET.to_owned()));
                }
                let backend = ServerBackend::new(&origin, &token).map_err(Fault::of)?;
                let outcome = self.sync_with(backend, &passphrase);
                if matches!(outcome, Err(Fault::SignedOut)) {
                    self.server.token_refused(&token);
                }
                outcome
            }
        }
    }

    /// One run over a backend: open the folder with the passphrase, then merge.
    fn sync_with<B: Backend + 'static>(&self, backend: B, passphrase: &str) -> Result<Report, Fault> {
        let source = ConfigSource::new(&self.dir, self.secrets.clone());
        let mut syncer = Syncer::connect(backend, source, &self.dir, passphrase).map_err(Fault::of)?;
        syncer.sync_once().map_err(Fault::of)
    }

    /// The scheduled run: nothing when sync is off, and no complaint beyond the state the page shows. While the sync is
    /// paused and the store still answers locked it does nothing at all, so the state does not move.
    pub fn background(&self) {
        let view = self.view();
        if !view.configured || (view.state == "paused" && self.locked()) {
            return;
        }
        // A server that refused the token is asked again only after a new sign-in.
        if view.state == "signed_out" && self.server.token_and_origin().is_err() {
            return;
        }
        let _ = self.run();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holzwave_core::secrets::MemoryStore;
    use std::sync::atomic::Ordering;

    #[test]
    fn a_server_address_stored_in_another_case_is_the_same_server_and_another_host_is_not() {
        assert!(same_server("https://Sync.Example", "https://sync.example"));
        assert!(same_server("https://sync.example:443", "https://sync.example"));
        assert!(!same_server("https://sync.example", "https://other.example"));
        assert!(!same_server("https://sync.example", "https://sync.example:8443"));
        assert!(!same_server("not an address", "https://sync.example"));
    }

    fn runtime(tag: &str) -> (SyncRuntime, Arc<MemoryStore>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("hw-app-sync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let secrets = Arc::new(MemoryStore::default());
        let server = Arc::new(ServerRuntime::new(dir.clone(), secrets.clone()));
        (SyncRuntime::new(dir.clone(), secrets.clone(), server), secrets, dir)
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
        let again = SyncRuntime::new(dir.clone(), secrets.clone(), Arc::new(ServerRuntime::new(dir.clone(), secrets.clone())));
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
    fn a_run_whose_files_all_failed_because_the_store_locked_is_paused_and_any_other_failure_is_an_error() {
        let (rt, _calls, _dir) = applying("lk", Promoted::default());
        let failed = |pairs: &[(&str, &str)]| Report {
            errors: pairs.iter().map(|(name, why)| ((*name).to_owned(), (*why).to_owned())).collect(),
            ..Report::default()
        };
        let view = rt.complete(&failed(&[("secrets.json", SECRETS_LOCKED)]));
        assert_eq!((view.state, view.error.as_deref()), ("paused", None));
        // One file that failed for another reason makes the run an error, and the text is the first failure's.
        let view = rt.complete(&failed(&[("secrets.json", SECRETS_LOCKED), ("themes/a.json", "no space left")]));
        assert_eq!((view.state, view.error.as_deref()), ("error", Some("secrets.json: secrets are locked")));
        // A run with no failure is idle again.
        assert_eq!(rt.complete(&Report::default()).state, "idle");
    }

    #[test]
    fn only_a_configured_sync_in_the_paused_state_is_due_for_a_run_at_the_unlock() {
        let (rt, secrets, dir) = runtime("due");
        assert!(!rt.resume_due(), "a sync that is off has nothing to resume");
        write_atomic(&dir.join(FILE), br#"{"endpoint":"http://127.0.0.1:1/dav","user":"u"}"#).unwrap();
        let rt = SyncRuntime::new(dir.clone(), secrets.clone(), Arc::new(ServerRuntime::new(dir.clone(), secrets)));
        assert!(!rt.resume_due(), "idle is not paused");
        rt.pause();
        assert!(rt.resume_due());
        assert_eq!((rt.view().state, rt.view().error), ("paused", None));
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

    // ---- the server as the transport ----

    use crate::server::fake::{Fake, TOKEN};
    use holzwave_core::server_link::{ServerLinkFile, token_save};

    /// A runtime signed in at `fake` (the link file and the token are written first, as a finished sign-in leaves them), with
    /// its own directory and store.
    fn on_server(tag: &str, fake: &Fake) -> (SyncRuntime, Arc<ServerRuntime>, Arc<MemoryStore>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("hw-app-sync-srv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(MemoryStore::default());
        ServerLinkFile::new_with(&fake.origin, "17", "laptop", true).unwrap().save(&dir).unwrap();
        token_save(&*store, &holzwave_core::server_link::Token::parse(TOKEN).unwrap()).unwrap();
        let server = Arc::new(ServerRuntime::new_for_loopback(dir.clone(), store.clone()));
        (SyncRuntime::new(dir.clone(), store.clone(), server.clone()), server, store, dir)
    }

    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).unwrap().flatten() {
                if entry.path().is_dir() {
                    stack.push(entry.path());
                } else {
                    found.push(entry.path().strip_prefix(dir).unwrap().display().to_string());
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn an_old_account_without_a_transport_reads_as_webdav() {
        let (_rt, _secrets, dir) = runtime("old");
        write_atomic(&dir.join(FILE), br#"{"endpoint":"https://cloud.example/dav","user":"holz"}"#).unwrap();
        let secrets = Arc::new(MemoryStore::default());
        let view = SyncRuntime::new(dir.clone(), secrets.clone(), Arc::new(ServerRuntime::new(dir.clone(), secrets))).view();
        assert_eq!((view.configured, view.transport, view.endpoint.as_str(), view.state), (true, Transport::Webdav, "https://cloud.example/dav", "idle"));
        assert_eq!(serde_json::to_value(&view).unwrap()["transport"], "webdav");
    }

    #[test]
    fn using_the_server_needs_a_sign_in_and_a_long_enough_passphrase_and_writes_nothing_otherwise() {
        let (rt, secrets, dir) = runtime("srv-refuse");
        assert!(rt.use_server("short").unwrap_err().contains("at least"));
        assert_eq!(rt.use_server("long enough pass").unwrap_err(), "Sign in to the server first.");
        assert!(secrets.get(PASSPHRASE).unwrap().is_none() && !dir.join(FILE).exists());
        assert_eq!(rt.view().state, "off");
    }

    #[test]
    fn using_the_server_writes_the_account_stores_the_passphrase_and_runs_once() {
        let fake = Fake::start();
        let (rt, _server, store, dir) = on_server("use", &fake);
        std::fs::write(dir.join("settings.json"), br#"{"font_size":14}"#).unwrap();
        let view = rt.use_server("long enough pass").expect("the first run");
        assert_eq!((view.transport, view.state, view.endpoint.as_str(), view.user.as_str()), (Transport::Server, "idle", fake.origin.as_str(), ""));
        assert!(view.configured && view.files >= 1, "{view:?}");
        assert_eq!(serde_json::to_value(&view).unwrap()["transport"], "server");
        let account: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(FILE)).unwrap()).unwrap();
        assert_eq!(account["transport"], "server");
        assert_eq!(store.get(PASSPHRASE).unwrap().as_deref(), Some("long enough pass"));
        assert!(fake.count("POST /sync/v1/ensure/") >= 1);
        let files = fake.files.lock().unwrap();
        assert!(files.contains_key("meta.json") && files.keys().any(|k| k.starts_with("files/")), "{:?}", files.keys().collect::<Vec<_>>());
        // No byte of the token, the passphrase or a file's content is in anything the sync wrote here.
        drop(files);
        for name in names(&dir) {
            let text = String::from_utf8_lossy(&std::fs::read(dir.join(&name)).unwrap()).into_owned();
            assert!(!text.contains(TOKEN) && !text.contains("long enough pass") && !text.contains("hwd_"), "{name}");
        }
        // The next start reads the same.
        let again = SyncRuntime::new(dir.clone(), store.clone(), Arc::new(ServerRuntime::new_for_loopback(dir.clone(), store.clone())));
        assert_eq!((again.view().transport, again.view().configured), (Transport::Server, true));
    }

    #[test]
    fn a_failed_first_run_against_the_server_puts_the_previous_account_back_exactly() {
        let fake = Fake::start();
        let (_first, server, store, dir) = on_server("restore", &fake);
        // A WebDAV account as it was, with its secrets and the engine's record of that target.
        let old = br#"{"endpoint":"https://cloud.example/dav","user":"holz","transport":"webdav"}"#;
        write_atomic(&dir.join(FILE), old).unwrap();
        store.set(PASSWORD, "old-app-password").unwrap();
        store.set(PASSPHRASE, "old passphrase here").unwrap();
        std::fs::write(dir.join(STATE_FILE), br#"{"files":{"settings.json":{"counter":3}}}"#).unwrap();
        let rt = SyncRuntime::new(dir.clone(), store.clone(), server);
        let before = rt.view();
        // The server has the sync target off: the first run fails.
        fake.sync_on.store(false, Ordering::SeqCst);
        let refused = rt.use_server("another long passphrase").unwrap_err();
        assert!(!refused.is_empty());
        assert_eq!(std::fs::read(dir.join(FILE)).unwrap(), old, "the account file is as it was");
        assert_eq!(store.get(PASSWORD).unwrap().as_deref(), Some("old-app-password"));
        assert_eq!(store.get(PASSPHRASE).unwrap().as_deref(), Some("old passphrase here"));
        assert_eq!(std::fs::read(dir.join(STATE_FILE)).unwrap(), br#"{"files":{"settings.json":{"counter":3}}}"#, "the record of the old target is back");
        assert!(!dir.join(STATE_PARKED).exists());
        assert_eq!(rt.view().transport, before.transport);
        assert_eq!(rt.view().endpoint, before.endpoint);
        assert_eq!(rt.view().state, "idle");
    }

    #[test]
    fn changing_the_transport_removes_only_the_record_of_the_old_target_and_loses_no_local_file() {
        let fake = Fake::start();
        let (_rt, server, store, dir) = on_server("switch", &fake);
        std::fs::write(dir.join("settings.json"), br#"{"font_size":14}"#).unwrap();
        std::fs::write(dir.join("layout.json"), br#"{"version":1}"#).unwrap();
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        std::fs::write(dir.join("themes/mine.json"), br#"{"name":"mine"}"#).unwrap();
        // A WebDAV account whose engine already recorded these files.
        write_atomic(&dir.join(FILE), br#"{"endpoint":"https://cloud.example/dav","user":"holz","transport":"webdav"}"#).unwrap();
        store.set(PASSWORD, "pw").unwrap();
        std::fs::write(
            dir.join(STATE_FILE),
            br#"{"files":{"settings.json":{"counter":3},"layout.json":{"counter":2},"themes/mine.json":{"counter":1}}}"#,
        )
        .unwrap();
        let rt = SyncRuntime::new(dir.clone(), store.clone(), server);
        let local = ["settings.json", "layout.json", "themes/mine.json"];
        let contents: Vec<Vec<u8>> = local.iter().map(|n| std::fs::read(dir.join(n)).unwrap()).collect();

        // The new target is an empty server: the first run must treat it as new, not as the deletion of everything.
        let view = rt.use_server("long enough pass").expect("the run against the empty server");
        assert_eq!(view.transport, Transport::Server);
        for (name, content) in local.iter().zip(&contents) {
            assert_eq!(&std::fs::read(dir.join(name)).unwrap(), content, "{name} is still here, unchanged");
        }
        assert_eq!(view.removed, 0, "{view:?}");
        let sent = fake.files.lock().unwrap().keys().cloned().collect::<Vec<_>>();
        assert!(sent.len() > local.len() && sent.iter().any(|k| k == "meta.json"), "meta.json and a file for each of ours were sent: {sent:?}");
        assert!(!dir.join(STATE_PARKED).exists(), "the old record was dropped after the run");
        let state = std::fs::read_to_string(dir.join(STATE_FILE)).unwrap();
        assert!(state.contains("settings.json"), "the new record is of the new target");
        // The app password of the old account is of no use any more.
        assert_eq!(store.get(PASSWORD).unwrap(), None);

        // A sync that was set up with this server is refused at another one: it would read an empty folder as a deletion.
        let other = Fake::start();
        let (_unused, other_server, _store, other_dir) = on_server("switch-other", &other);
        let elsewhere = SyncRuntime::new(dir.clone(), store.clone(), other_server);
        assert_eq!(elsewhere.run().unwrap_err(), OTHER_TARGET);
        assert_eq!(other.count("POST /sync/v1/"), 0, "nothing was asked of the other server");
        for (name, content) in local.iter().zip(&contents) {
            assert_eq!(&std::fs::read(dir.join(name)).unwrap(), content);
        }
        let _ = std::fs::remove_dir_all(&other_dir);
    }

    #[test]
    fn a_second_device_with_its_own_token_and_the_same_passphrase_ends_with_the_same_files() {
        let fake = Fake::start();
        let (first, _server, _store, dir) = on_server("dev-a", &fake);
        std::fs::write(dir.join("settings.json"), br#"{"font_size":14}"#).unwrap();
        first.use_server("long enough pass").expect("the first device");
        let (second, _server, _store, dir2) = on_server("dev-b", &fake);
        let view = second.use_server("long enough pass").expect("the second device");
        assert!(view.state == "idle" && view.error.is_none(), "{view:?}");
        // The file is downloaded and staged for the next start: the app owns it, so it does not replace it while it runs.
        assert!(view.files >= 1 && view.restart, "{view:?}");
        let staged = std::fs::read_dir(dir2.join("sync-incoming")).map(|d| d.count()).unwrap_or(0);
        assert!(staged >= 1, "a staged download waits for the next start");
        // A wrong passphrase on a third device is refused and writes nothing.
        let (third, _server, store3, dir3) = on_server("dev-c", &fake);
        let refused = third.use_server("not the passphrase").unwrap_err();
        assert!(!refused.is_empty());
        assert!(!dir3.join(FILE).exists() && !dir3.join("settings.json").exists());
        assert_eq!(store3.get(PASSPHRASE).unwrap(), None);
    }

    #[test]
    fn a_token_the_server_refuses_ends_the_run_as_signed_out_and_is_forgotten() {
        let fake = Fake::start();
        let (rt, server, store, dir) = on_server("revoked", &fake);
        rt.use_server("long enough pass").expect("the first run");
        assert_eq!(rt.run().unwrap().state, "idle");
        // The device is revoked at the server.
        fake.accepts.store(false, Ordering::SeqCst);
        let view = rt.run().expect("a refused token is a state, not an error");
        assert_eq!((view.state, view.error), ("signed_out", None));
        assert_eq!(store.get("server:token").unwrap(), None, "the refused token is forgotten");
        assert_eq!(server.view().state, "signed_out");
        assert!(dir.join("server_link.json").exists(), "the address stays");
        // The scheduled run does not ask again until a sign-in happens.
        let before = fake.total();
        rt.background();
        assert_eq!(fake.total(), before);
        // The account stays: the sync is configured for the server and waits.
        assert!(rt.view().configured);
    }

    #[test]
    fn a_locked_store_pauses_the_server_sync_without_a_request() {
        let fake = Fake::start();
        let (rt, _server, store, _dir) = on_server("paused", &fake);
        rt.use_server("long enough pass").expect("the first run");
        // A store that answers locked for the passphrase.
        struct Locked;
        impl SecretStore for Locked {
            fn get(&self, _: &str) -> holzwave_core::Result<Option<String>> {
                Err(holzwave_core::Error::Io(std::io::Error::other(SECRETS_LOCKED)))
            }
            fn set(&self, _: &str, _: &str) -> holzwave_core::Result<()> {
                Ok(())
            }
            fn delete(&self, _: &str) -> holzwave_core::Result<()> {
                Ok(())
            }
        }
        let dir = std::env::temp_dir().join(format!("hw-app-sync-srv-locked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _ = store;
        write_atomic(&dir.join(FILE), format!(r#"{{"endpoint":"{}","user":"","transport":"server"}}"#, fake.origin).as_bytes()).unwrap();
        let locked: Arc<dyn SecretStore> = Arc::new(Locked);
        let rt = SyncRuntime::new(dir.clone(), locked.clone(), Arc::new(ServerRuntime::new_for_loopback(dir.clone(), locked)));
        let before = fake.total();
        let view = rt.run().unwrap();
        assert_eq!((view.state, view.error), ("paused", None));
        assert_eq!(fake.total(), before);
    }

    // ---- switching between WebDAV and the server ----

    /// A WebDAV folder on the loopback address that keeps its files and records every request as `METHOD path`.
    struct Dav {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl Dav {
        fn start() -> Dav {
            use std::collections::{BTreeMap, BTreeSet};
            use std::io::{BufRead, BufReader, Read, Write};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://127.0.0.1:{}/dav", listener.local_addr().unwrap().port());
            let requests = Arc::new(Mutex::new(Vec::new()));
            type Files = BTreeMap<String, (Vec<u8>, u64)>;
            let store: Arc<Mutex<(Files, BTreeSet<String>, u64)>> = Arc::default();
            let seen = requests.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (seen, store) = (seen.clone(), store.clone());
                    std::thread::spawn(move || {
                        let mut out = stream.try_clone().unwrap();
                        let mut reader = BufReader::new(stream);
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                                return;
                            }
                            let mut parts = line.split_whitespace();
                            let (method, path) = (parts.next().unwrap_or("").to_owned(), parts.next().unwrap_or("").to_owned());
                            let mut headers = std::collections::HashMap::new();
                            loop {
                                let mut h = String::new();
                                if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                                    break;
                                }
                                if let Some((k, v)) = h.split_once(':') {
                                    headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
                                }
                            }
                            let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                            let mut body = vec![0u8; len];
                            if reader.read_exact(&mut body).is_err() {
                                return;
                            }
                            seen.lock().unwrap().push(format!("{method} {path}"));
                            let rel = path.trim_start_matches("/dav").trim_matches('/').to_owned();
                            let mut guard = store.lock().unwrap();
                            let (files, dirs, next) = &mut *guard;
                            let (status, extra, payload): (u16, String, Vec<u8>) = match method.as_str() {
                                "MKCOL" => {
                                    if dirs.insert(rel) { (201, String::new(), vec![]) } else { (405, String::new(), vec![]) }
                                }
                                "GET" => match files.get(&rel) {
                                    Some((b, e)) => (200, format!("ETag: \"v{e}\"\r\n"), b.clone()),
                                    None => (404, String::new(), vec![]),
                                },
                                "PUT" => {
                                    let current = files.get(&rel).map(|(_, e)| format!("\"v{e}\""));
                                    let ok = match (headers.get("if-none-match"), headers.get("if-match")) {
                                        (Some(_), _) => current.is_none(),
                                        (_, Some(m)) => current.as_deref() == Some(m.as_str()),
                                        _ => true,
                                    };
                                    if ok {
                                        *next += 1;
                                        let created = current.is_none();
                                        files.insert(rel, (body, *next));
                                        (if created { 201 } else { 204 }, format!("ETag: \"v{next}\"\r\n"), vec![])
                                    } else {
                                        (412, String::new(), vec![])
                                    }
                                }
                                "PROPFIND" if !rel.is_empty() && !dirs.contains(&rel) => (404, String::new(), vec![]),
                                "PROPFIND" => {
                                    let prefix = if rel.is_empty() { String::new() } else { format!("{rel}/") };
                                    let mut xml = String::from(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">"#);
                                    xml.push_str(&format!("<d:response><d:href>/dav/{prefix}</d:href><d:propstat><d:prop><d:getetag>\"d\"</d:getetag><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>"));
                                    for (p, (_, e)) in files.iter() {
                                        if let Some(name) = p.strip_prefix(&prefix) {
                                            if !name.contains('/') {
                                                xml.push_str(&format!("<d:response><d:href>/dav/{p}</d:href><d:propstat><d:prop><d:getetag>&quot;v{e}&quot;</d:getetag><d:resourcetype/></d:prop></d:propstat></d:response>"));
                                            }
                                        }
                                    }
                                    xml.push_str("</d:multistatus>");
                                    (207, "Content-Type: application/xml\r\n".into(), xml.into_bytes())
                                }
                                _ => (405, String::new(), vec![]),
                            };
                            drop(guard);
                            let head = format!("HTTP/1.1 {status} X\r\n{extra}Content-Length: {}\r\nConnection: keep-alive\r\n\r\n", payload.len());
                            if out.write_all(head.as_bytes()).and_then(|()| out.write_all(&payload)).is_err() {
                                return;
                            }
                        }
                    });
                }
            });
            Dav { url, requests }
        }

        fn total(&self) -> usize {
            self.requests.lock().unwrap().len()
        }

        fn all(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn some_local_files(dir: &std::path::Path) -> Vec<(&'static str, Vec<u8>)> {
        let local: [(&str, &[u8]); 3] =
            [("settings.json", br#"{"font_size":14}"#), ("layout.json", br#"{"version":1}"#), ("themes/mine.json", br#"{"name":"mine"}"#)];
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        local
            .iter()
            .map(|(name, bytes)| {
                std::fs::write(dir.join(name), bytes).unwrap();
                (*name, bytes.to_vec())
            })
            .collect()
    }

    #[test]
    fn switching_from_webdav_to_the_server_asks_nothing_more_of_the_old_webdav_folder() {
        let dav = Dav::start();
        let fake = Fake::start();
        let (rt, _server, store, dir) = on_server("dav-to-srv", &fake);
        let local = some_local_files(&dir);
        let view = rt.save(&dav.url, "holz", "pw", "long enough pass").expect("the first run against WebDAV");
        assert_eq!(view.transport, Transport::Webdav);
        assert!(dav.all().iter().any(|r| r.starts_with("PUT /dav/files/")), "the files went to WebDAV: {:?}", dav.all());
        let before = dav.total();

        let view = rt.use_server("long enough pass").expect("the switch to the server");
        assert_eq!(view.transport, Transport::Server);
        assert_eq!(view.removed, 0, "{view:?}");
        assert_eq!(dav.total(), before, "the old WebDAV folder was asked again: {:?}", &dav.all()[before..]);
        for (name, bytes) in &local {
            assert_eq!(&std::fs::read(dir.join(name)).unwrap(), bytes, "{name} is unchanged");
        }
        assert_eq!(store.get(PASSWORD).unwrap(), None, "the app password of WebDAV is forgotten");
        // A later run goes to the server only.
        rt.run().expect("a run on the server");
        assert_eq!(dav.total(), before, "a run after the switch went to WebDAV");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn switching_from_the_server_back_to_webdav_loses_no_file_and_leaves_the_server_sign_in_alone() {
        let dav = Dav::start();
        let fake = Fake::start();
        let (rt, server, store, dir) = on_server("srv-to-dav", &fake);
        let local = some_local_files(&dir);
        let view = rt.use_server("long enough pass").expect("the first run against the server");
        assert_eq!(view.transport, Transport::Server);
        assert!(fake.count("PUT /sync/v1/") + fake.count("POST /sync/v1/") > 0, "the server was used");
        let on_server_before = fake.requests.lock().unwrap().iter().filter(|l| l.contains("/sync/v1/")).count();

        let view = rt.save(&dav.url, "holz", "pw", "long enough pass").expect("the switch back to WebDAV");
        assert_eq!(view.transport, Transport::Webdav);
        assert_eq!(view.removed, 0, "{view:?}");
        for (name, bytes) in &local {
            assert_eq!(&std::fs::read(dir.join(name)).unwrap(), bytes, "{name} is unchanged");
        }
        rt.run().expect("a run on WebDAV");
        let after = fake.requests.lock().unwrap().iter().filter(|l| l.contains("/sync/v1/")).count();
        assert_eq!(after, on_server_before, "the server's sync was asked after the switch");
        let gets: Vec<String> = dav.all().into_iter().filter(|r| r.starts_with("GET /dav/files/")).collect();
        assert!(gets.is_empty(), "WebDAV was asked for blobs it never had: {gets:?}");
        assert!(dav.all().iter().any(|r| r.starts_with("PUT /dav/files/")), "the local files went to the new folder");
        // The sign-in at the server is untouched.
        assert_eq!(store.get("server:token").unwrap().as_deref(), Some(TOKEN));
        assert_ne!(server.view().state, "signed_out");
        assert!(dir.join("server_link.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
