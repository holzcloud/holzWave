//! The connection manager: the saved connections, their secrets, and which pane runs which connection.
//!
//! This is everything the app shell needs and nothing of Tauri. The shell holds one [`Connections`], answers the
//! page's calls with its methods and, when it starts a pane, asks [`Connections::apply`] to turn the spec into an
//! SSH, WSL or Docker pane.
//!
//! Which pane runs which connection is kept in `pane_connections.json` next to `layout.json`, so a restart opens the
//! same panes on the same hosts. The file holds pane ids and connection ids only.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::conn::ConnState;
use crate::layout::{LoadStatus, PaneId};
use crate::pane::PaneSpec;
use crate::persist::{read_limited, write_atomic};
use crate::profiles::{Kind, Profile, Profiles};
use crate::secrets::SecretStore;
use crate::{Error, Result};

pub const PANES_FILE: &str = "pane_connections.json";
const MAX_PANES_BYTES: u64 = 256 * 1024;
pub const KNOWN_HOSTS_FILE: &str = "known_hosts";

/// What the page says about a connection's secret when it saves the connection.
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SecretChange {
    /// Leave what is stored as it is.
    Keep,
    /// Store this password or passphrase.
    Set { value: String },
    /// Remove what is stored.
    Clear,
}

impl std::fmt::Debug for SecretChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretChange::Keep => f.write_str("Keep"),
            SecretChange::Set { .. } => f.write_str("Set(<secret>)"),
            SecretChange::Clear => f.write_str("Clear"),
        }
    }
}

/// A connection as the page sees it: the profile and whether a secret is saved. The secret itself never leaves.
#[derive(Clone, Debug, Serialize)]
pub struct ProfileView {
    #[serde(flatten)]
    pub profile: Profile,
    pub has_secret: bool,
}

/// The state of one pane's connection, for the pane header, the sidebar and the status line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConnView {
    pub pane: PaneId,
    pub profile: String,
    pub name: String,
    /// `connecting`, `connected`, `reconnecting`, `failed` or `closed`.
    pub state: &'static str,
    /// While reconnecting: attempt, attempts in all, seconds until the next one. Else 0.
    pub attempt: u32,
    pub of: u32,
    pub secs: u64,
}

impl ConnView {
    pub fn new(pane: PaneId, profile: &Profile, state: &ConnState) -> Option<Self> {
        let (name, attempt, of, secs) = match state {
            ConnState::Local => return None,
            ConnState::Connecting => ("connecting", 0, 0, 0),
            ConnState::Connected => ("connected", 0, 0, 0),
            ConnState::Reconnecting { attempt, of, in_secs } => ("reconnecting", *attempt, *of, *in_secs),
            ConnState::Failed => ("failed", 0, 0, 0),
            ConnState::Closed => ("closed", 0, 0, 0),
        };
        Some(Self { pane, profile: profile.id.clone(), name: profile.name.clone(), state: name, attempt, of, secs })
    }
}

pub struct Connections {
    dir: PathBuf,
    home: Option<PathBuf>,
    profiles: Mutex<Profiles>,
    secrets: Arc<dyn SecretStore>,
    panes: Mutex<HashMap<PaneId, String>>,
    /// Counts the saves and deletes, so what was made from a connection (a file source) knows when it is out of date.
    generation: AtomicU64,
}

impl Connections {
    /// Read `connections.json` and `pane_connections.json` from `dir`. `live_panes` are the panes of the layout: an
    /// assignment of any other pane, or to a connection that no longer exists, is dropped.
    pub fn load(dir: &Path, secrets: Arc<dyn SecretStore>, home: Option<PathBuf>, live_panes: &[PaneId]) -> (Self, LoadStatus) {
        let (profiles, status) = Profiles::load(dir);
        let mut panes: HashMap<PaneId, String> = match read_limited(&dir.join(PANES_FILE), MAX_PANES_BYTES) {
            Ok(Some(bytes)) => serde_json::from_slice::<HashMap<String, String>>(&bytes)
                .map(|map| map.into_iter().filter_map(|(k, v)| k.parse::<PaneId>().ok().map(|k| (k, v))).collect())
                .unwrap_or_default(),
            _ => HashMap::new(),
        };
        panes.retain(|pane, id| live_panes.contains(pane) && profiles.get(id).is_some());
        let me = Self { dir: dir.to_path_buf(), home, profiles: Mutex::new(profiles), secrets, panes: Mutex::new(panes), generation: AtomicU64::new(0) };
        (me, status)
    }

    fn profiles(&self) -> std::sync::MutexGuard<'_, Profiles> {
        self.profiles.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn panes(&self) -> std::sync::MutexGuard<'_, HashMap<PaneId, String>> {
        self.panes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn view(&self, profile: &Profile) -> ProfileView {
        // A keychain that cannot be read counts as no secret: the list must still show.
        let has_secret = self.secrets.get(&profile.secret_account()).ok().flatten().is_some();
        ProfileView { profile: profile.clone(), has_secret }
    }

    /// Whether a connection with this id exists.
    pub fn contains(&self, id: &str) -> bool {
        self.profiles().get(id).is_some()
    }

    /// Changes whenever a connection is saved or deleted.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// The kind of connection `id`: the shell checks with it that a monitor of another computer names an SSH connection.
    pub fn kind_of(&self, id: &str) -> Option<Kind> {
        self.profiles().get(id).map(|p| p.kind)
    }

    /// The name of connection `id`.
    pub fn label_of(&self, id: &str) -> Option<String> {
        self.profiles().get(id).map(|p| p.name.clone())
    }

    /// The SSH target of connection `id`, with its login from the keychain and its jump hosts. For the file sources.
    pub fn ssh_target(&self, id: &str) -> Result<crate::ssh::SshTarget> {
        let profiles = self.profiles().clone();
        profiles.ssh_target(id, self.secrets.as_ref(), self.home.as_deref(), &self.dir.join(KNOWN_HOSTS_FILE))
    }

    /// Every saved connection.
    pub fn list(&self) -> Vec<ProfileView> {
        let items = self.profiles().items.clone();
        items.iter().map(|p| self.view(p)).collect()
    }

    /// Add or change a connection and its secret. An id that is empty is made from the name. Validated as a whole;
    /// nothing is written when it does not validate. The keychain is written before the file, and rolled back when the
    /// file cannot be written.
    pub fn save(&self, mut profile: Profile, secret: SecretChange) -> Result<ProfileView> {
        let mut profiles = self.profiles();
        if profile.id.trim().is_empty() {
            profile.id = profiles.fresh_id(&profile.name);
        }
        let account = profile.secret_account();
        let previous_secret = self.secrets.get(&account).ok().flatten();
        let mut next = profiles.clone();
        next.upsert(profile.clone())?;
        match &secret {
            SecretChange::Keep => {}
            SecretChange::Set { value } if !value.is_empty() => self.secrets.set(&account, value)?,
            SecretChange::Set { .. } | SecretChange::Clear => self.secrets.delete(&account)?,
        }
        if let Err(e) = next.save(&self.dir) {
            // Put the keychain back as it was.
            let _ = match &previous_secret {
                Some(old) => self.secrets.set(&account, old),
                None => self.secrets.delete(&account),
            };
            return Err(Error::Io(e));
        }
        *profiles = next;
        drop(profiles);
        self.generation.fetch_add(1, Ordering::SeqCst);
        Ok(self.view(&profile))
    }

    /// Remove a connection, its secret and the assignment of its panes (those panes keep running; they are local
    /// shells again after a restart).
    pub fn delete(&self, id: &str) -> Result<()> {
        let mut profiles = self.profiles();
        let mut next = profiles.clone();
        if !next.remove(id) {
            return Err(Error::Unsupported(format!("no connection {id}")));
        }
        next.save(&self.dir)?;
        let account = format!("ssh:{id}");
        let _ = self.secrets.delete(&account);
        *profiles = next;
        drop(profiles);
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.panes().retain(|_, v| v != id);
        self.save_panes();
        Ok(())
    }

    /// Say that `pane` runs connection `id`. Takes effect when the pane is started.
    pub fn assign(&self, pane: PaneId, id: &str) -> Result<()> {
        if self.profiles().get(id).is_none() {
            return Err(Error::Unsupported(format!("no connection {id}")));
        }
        self.panes().insert(pane, id.to_owned());
        self.save_panes();
        Ok(())
    }

    /// A pane is gone: its assignment goes with it.
    pub fn forget(&self, pane: PaneId) {
        if self.panes().remove(&pane).is_some() {
            self.save_panes();
        }
    }

    /// The connection `pane` runs, if it runs one.
    pub fn profile_of(&self, pane: PaneId) -> Option<Profile> {
        let id = self.panes().get(&pane).cloned()?;
        self.profiles().get(&id).cloned()
    }

    /// Turn `spec` into the pane of connection `pane` is assigned to. A pane without one is left as it is. Fails when
    /// the connection cannot be made up (a jump host that is missing, a keychain that cannot be read).
    pub fn apply(&self, pane: PaneId, spec: &mut PaneSpec) -> Result<()> {
        let Some(profile) = self.profile_of(pane) else { return Ok(()) };
        let profiles = self.profiles().clone();
        match profile.kind {
            Kind::Ssh => {
                let known = self.dir.join(KNOWN_HOSTS_FILE);
                spec.ssh = Some(profiles.ssh_target(&profile.id, self.secrets.as_ref(), self.home.as_deref(), &known)?);
            }
            Kind::Wsl | Kind::Docker => {
                let command = profiles
                    .local_command(&profile.id)
                    .ok_or_else(|| Error::Unsupported(format!("{} has no command", profile.name)))?;
                spec.program = command.program;
                spec.args = command.args;
            }
        }
        Ok(())
    }

    /// The connection views of the panes that have one, from their current states.
    pub fn views(&self, states: impl IntoIterator<Item = (PaneId, ConnState)>) -> Vec<ConnView> {
        let panes = self.panes().clone();
        let profiles = self.profiles();
        let mut out: Vec<ConnView> = states
            .into_iter()
            .filter_map(|(pane, state)| {
                let profile = profiles.get(panes.get(&pane)?)?;
                ConnView::new(pane, profile, &state)
            })
            .collect();
        out.sort_by_key(|v| v.pane);
        out
    }

    fn save_panes(&self) {
        let map: HashMap<String, String> = self.panes().iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let text = serde_json::to_string_pretty(&map).unwrap_or_else(|_| "{}".into());
        if let Err(e) = fs::create_dir_all(&self.dir).and_then(|()| write_atomic(&self.dir.join(PANES_FILE), text.as_bytes())) {
            eprintln!("holzwave: cannot save {PANES_FILE}: {e}");
        }
    }
}

/// The home directory for `~/` in key paths, from the environment.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::Auth;
    use crate::secrets::MemoryStore;
    use crate::ssh::SshAuth;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("holzwave-conns-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn profile(name: &str) -> Profile {
        let mut p = Profile::new("", name, "host.example.org");
        p.user = "holz".into();
        p.auth = Auth::Password;
        p
    }

    fn manager(d: &Path, live: &[PaneId]) -> (Connections, Arc<MemoryStore>) {
        let secrets = Arc::new(MemoryStore::default());
        let (c, _) = Connections::load(d, secrets.clone(), Some(PathBuf::from("/home/holz")), live);
        (c, secrets)
    }

    #[test]
    fn a_saved_connection_gets_an_id_and_its_password_goes_to_the_keychain_only() {
        let d = dir("save");
        let (c, secrets) = manager(&d, &[]);
        let view = c.save(profile("My Server"), SecretChange::Set { value: "hunter2".into() }).unwrap();
        assert_eq!(view.profile.id, "my-server");
        assert!(view.has_secret);
        assert_eq!(secrets.get("ssh:my-server").unwrap().as_deref(), Some("hunter2"));
        assert!(!fs::read_to_string(d.join("connections.json")).unwrap().contains("hunter2"));
        // Keep leaves it, Clear removes it.
        assert!(c.save(view.profile.clone(), SecretChange::Keep).unwrap().has_secret);
        assert!(!c.save(view.profile, SecretChange::Clear).unwrap().has_secret);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn an_invalid_connection_changes_nothing() {
        let d = dir("invalid");
        let (c, secrets) = manager(&d, &[]);
        let mut bad = profile("Bad");
        bad.host = "has blank".into();
        assert!(c.save(bad, SecretChange::Set { value: "pw".into() }).is_err());
        assert!(c.list().is_empty());
        assert_eq!(secrets.get("ssh:bad").unwrap(), None, "no secret is stored for a connection that was refused");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_pane_runs_its_connection_and_it_survives_a_restart() {
        let d = dir("assign");
        let (c, _) = manager(&d, &[1, 2]);
        let id = c.save(profile("Talos"), SecretChange::Set { value: "pw".into() }).unwrap().profile.id;
        c.assign(2, &id).unwrap();
        assert!(c.assign(1, "missing").is_err());
        let mut spec = PaneSpec::new("sh", 80, 24);
        c.apply(1, &mut spec).unwrap();
        assert!(spec.ssh.is_none(), "an unassigned pane stays local");
        c.apply(2, &mut spec).unwrap();
        let target = spec.ssh.clone().unwrap();
        assert_eq!(target.host, "host.example.org");
        assert!(matches!(target.auth, SshAuth::Password(ref p) if p == "pw"));
        assert_eq!(target.known_hosts.as_deref(), Some(d.join("known_hosts").as_path()));

        // A restart: the same panes, the same hosts; a pane that is not in the layout any more is dropped.
        let secrets = Arc::new(MemoryStore::default());
        let (again, _) = Connections::load(&d, secrets, None, &[2]);
        assert_eq!(again.profile_of(2).unwrap().id, id);
        let (fewer, _) = Connections::load(&d, Arc::new(MemoryStore::default()), None, &[1]);
        assert!(fewer.profile_of(2).is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn deleting_a_connection_removes_its_secret_and_its_panes() {
        let d = dir("delete");
        let (c, secrets) = manager(&d, &[5]);
        let id = c.save(profile("Gone"), SecretChange::Set { value: "pw".into() }).unwrap().profile.id;
        c.assign(5, &id).unwrap();
        c.delete(&id).unwrap();
        assert!(c.list().is_empty());
        assert_eq!(secrets.get(&format!("ssh:{id}")).unwrap(), None);
        assert!(c.profile_of(5).is_none());
        assert!(c.delete(&id).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_views_follow_the_state_and_skip_local_panes() {
        let d = dir("views");
        let (c, _) = manager(&d, &[1, 2, 3]);
        let id = c.save(profile("Talos"), SecretChange::Keep).unwrap().profile.id;
        c.assign(2, &id).unwrap();
        c.assign(3, &id).unwrap();
        let views = c.views([
            (1, ConnState::Local),
            (2, ConnState::Reconnecting { attempt: 2, of: 5, in_secs: 3 }),
            (3, ConnState::Failed),
        ]);
        assert_eq!(views.len(), 2);
        assert_eq!((views[0].pane, views[0].state, views[0].attempt, views[0].of, views[0].secs), (2, "reconnecting", 2, 5, 3));
        assert_eq!(views[1].state, "failed");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_docker_connection_changes_the_program_not_the_transport() {
        let d = dir("docker");
        let (c, _) = manager(&d, &[7]);
        let mut p = Profile::new("", "Web", "web-1");
        p.kind = Kind::Docker;
        let id = c.save(p, SecretChange::Keep).unwrap().profile.id;
        c.assign(7, &id).unwrap();
        let mut spec = PaneSpec::new("zsh", 80, 24);
        c.apply(7, &mut spec).unwrap();
        assert!(spec.ssh.is_none());
        assert_eq!(spec.program, "docker");
        assert_eq!(spec.args, ["exec", "-it", "web-1", "sh"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_kind_of_a_connection_is_known_by_its_id() {
        let d = dir("kind");
        let (c, _) = manager(&d, &[]);
        let ssh = c.save(profile("Talos"), SecretChange::Set { value: "pw".into() }).unwrap().profile.id;
        let mut wsl = Profile::new("", "Box", "Ubuntu");
        wsl.kind = Kind::Wsl;
        let wsl = c.save(wsl, SecretChange::Keep).unwrap().profile.id;
        assert_eq!(c.kind_of(&ssh), Some(Kind::Ssh));
        assert_eq!(c.kind_of(&wsl), Some(Kind::Wsl));
        assert_eq!(c.kind_of("nope"), None);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_secret_change_never_prints_the_secret() {
        let change = SecretChange::Set { value: "hunter2".into() };
        assert!(!format!("{change:?}").contains("hunter2"));
    }
}
