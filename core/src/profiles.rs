//! `connections.json`: the saved connections (SSH hosts, WSL distributions, Docker containers), typed and validated.
//!
//! A profile holds everything that is not secret: the host, the user, the path of a key file, the jump host, the
//! reconnect policy. The password or key passphrase is in the OS keychain ([`crate::secrets`]) under the account
//! [`Profile::secret_account`] and is never in this file, so the file can be synced and shared without leaking a login.
//!
//! The file is read with a size limit and written atomically like the other documents ([`crate::persist`]). A
//! profile that does not validate is dropped on load with a note, and the file is kept as `.bad` first.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::layout::LoadStatus;
use crate::persist::{backup_bad, read_limited, write_atomic};
use crate::secrets::SecretStore;
use crate::ssh::{HostKeyPolicy, Reconnect, SshAuth, SshTarget};
use crate::{Error, Result};

pub const FILE: &str = "connections.json";
pub const MAX_BYTES: u64 = 1024 * 1024;
pub const MAX_PROFILES: usize = 256;
/// The most jump hosts in a chain.
pub const MAX_JUMPS: usize = 3;
const MAX_TEXT: usize = 256;
const VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A shell on another machine over SSH.
    #[default]
    Ssh,
    /// A WSL distribution on this machine (Windows).
    Wsl,
    /// A shell in a running Docker container.
    Docker,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Auth {
    /// A private key file, with a passphrase from the keychain if it has one.
    #[default]
    Key,
    /// A password from the keychain.
    Password,
}

fn default_port() -> u16 {
    22
}
fn default_attempts() -> u32 {
    5
}
fn default_delay() -> u32 {
    3
}
fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// A stable id: lowercase letters, digits, `-` and `_`.
    pub id: String,
    /// What the user calls it.
    pub name: String,
    #[serde(default)]
    pub kind: Kind,
    /// SSH: the host name or address. WSL: the distribution. Docker: the container name or id.
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub auth: Auth,
    /// The private key file (SSH with a key). A `~/` start is the home directory.
    #[serde(default)]
    pub key_path: String,
    /// The id of the profile to go through first.
    #[serde(default)]
    pub jump: Option<String>,
    /// A line typed into the shell once it runs.
    #[serde(default)]
    pub startup: String,
    #[serde(default = "default_attempts")]
    pub reconnect_attempts: u32,
    #[serde(default = "default_delay")]
    pub reconnect_delay_secs: u32,
    /// Docker: the shell to start in the container; empty is `sh`.
    #[serde(default)]
    pub shell: String,
    /// SSH: the person allows the `wsh` tunnel on this host. Off unless ticked, and left out of the file when off, so
    /// an older file and its sync read the same as before.
    #[serde(default, skip_serializing_if = "is_false")]
    pub wsh: bool,
}

impl Profile {
    pub fn new(id: impl Into<String>, name: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind: Kind::Ssh,
            host: host.into(),
            port: 22,
            user: String::new(),
            auth: Auth::Key,
            key_path: String::new(),
            jump: None,
            startup: String::new(),
            reconnect_attempts: 5,
            reconnect_delay_secs: 3,
            shell: String::new(),
            wsh: false,
        }
    }

    /// The keychain account of this profile's password or passphrase.
    pub fn secret_account(&self) -> String {
        format!("ssh:{}", self.id)
    }

    /// Why this profile cannot be used, or `None`. Does not look at other profiles (see [`Profiles::validate`]).
    pub fn problem(&self) -> Option<String> {
        let text = |label: &str, value: &str, required: bool| -> Option<String> {
            if required && value.trim().is_empty() {
                return Some(format!("{label} is empty"));
            }
            if value.len() > MAX_TEXT || value.chars().any(char::is_control) {
                return Some(format!("{label} is longer than {MAX_TEXT} bytes or holds a control character"));
            }
            None
        };
        if self.id.is_empty()
            || self.id.len() > 64
            || !self.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return Some("the id must be 1 to 64 characters of a-z, 0-9, - and _".into());
        }
        if let Some(p) = text("the name", &self.name, true) {
            return Some(p);
        }
        if let Some(p) = text("the host", &self.host, true) {
            return Some(p);
        }
        if self.host.chars().any(char::is_whitespace) || self.host.starts_with('-') {
            return Some("the host has a blank or starts with a dash".into());
        }
        for (label, value) in [("the user", &self.user), ("the key path", &self.key_path), ("the startup line", &self.startup), ("the shell", &self.shell)] {
            if let Some(p) = text(label, value, false) {
                return Some(p);
            }
        }
        if self.wsh && self.kind != Kind::Ssh {
            return Some("wsh only applies to SSH".into());
        }
        match self.kind {
            Kind::Ssh => {
                if self.port == 0 {
                    return Some("the port is 0".into());
                }
                if self.user.trim().is_empty() {
                    return Some("the user is empty".into());
                }
                if self.auth == Auth::Key && self.key_path.trim().is_empty() {
                    return Some("a login with a key needs the key file".into());
                }
                if self.reconnect_attempts > 100 || self.reconnect_delay_secs > 3600 {
                    return Some("the reconnect policy is out of range (100 attempts, 3600 seconds)".into());
                }
            }
            Kind::Wsl | Kind::Docker => {
                if self.jump.is_some() {
                    return Some("a jump host only applies to SSH".into());
                }
            }
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profiles {
    pub version: u32,
    pub items: Vec<Profile>,
}

impl Default for Profiles {
    fn default() -> Self {
        Self { version: VERSION, items: Vec::new() }
    }
}

/// The program and arguments that open a WSL or Docker profile in a local PTY, or `None` for SSH.
pub struct LocalCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl Profiles {
    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.items.iter().find(|p| p.id == id)
    }

    /// Why the list cannot be used, or `None`: every profile valid, ids unique, jumps existing, no loop, not too deep.
    pub fn validate(&self) -> Option<String> {
        if self.items.len() > MAX_PROFILES {
            return Some(format!("more than {MAX_PROFILES} connections"));
        }
        let mut seen = HashSet::new();
        for p in &self.items {
            if let Some(why) = p.problem() {
                return Some(format!("{}: {why}", p.id));
            }
            if !seen.insert(p.id.as_str()) {
                return Some(format!("{}: the id is used twice", p.id));
            }
        }
        for p in &self.items {
            let mut depth = 0;
            let mut at = p;
            while let Some(next) = &at.jump {
                depth += 1;
                if depth > MAX_JUMPS {
                    return Some(format!("{}: more than {MAX_JUMPS} jump hosts, or a loop", p.id));
                }
                match self.get(next) {
                    Some(n) if n.kind == Kind::Ssh => at = n,
                    Some(_) => return Some(format!("{}: the jump host {next} is not an SSH connection", p.id)),
                    None => return Some(format!("{}: the jump host {next} does not exist", p.id)),
                }
            }
        }
        None
    }

    /// Add or replace a profile. The list is left as it was when the result would not validate.
    pub fn upsert(&mut self, profile: Profile) -> Result<()> {
        let before = self.clone();
        match self.items.iter_mut().find(|p| p.id == profile.id) {
            Some(slot) => *slot = profile,
            None => self.items.push(profile),
        }
        match self.validate() {
            None => Ok(()),
            Some(why) => {
                *self = before;
                Err(Error::Unsupported(why))
            }
        }
    }

    /// Remove a profile. Profiles that use it as a jump host lose that jump; it is the caller's call to confirm first.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|p| p.id != id);
        for p in &mut self.items {
            if p.jump.as_deref() == Some(id) {
                p.jump = None;
            }
        }
        self.items.len() != before
    }

    /// An id for a new profile made from `name`: lowercase, dashes, and a number if it is taken.
    pub fn fresh_id(&self, name: &str) -> String {
        let mut base: String = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' })
            .collect();
        base = base.trim_matches('-').chars().take(48).collect();
        if base.is_empty() {
            base = "host".into();
        }
        let mut id = base.clone();
        let mut n = 2;
        while self.get(&id).is_some() {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }

    /// The SSH target of profile `id`, with its login resolved through `secrets` and its jump hosts, or the reason it
    /// cannot be made. `home` is what a `~/` at the start of a key path means; `known_hosts` is the file of host keys.
    pub fn ssh_target(
        &self,
        id: &str,
        secrets: &dyn SecretStore,
        home: Option<&Path>,
        known_hosts: &Path,
    ) -> Result<SshTarget> {
        let profile = self.get(id).ok_or_else(|| Error::Unsupported(format!("no connection {id}")))?;
        if profile.kind != Kind::Ssh {
            return Err(Error::Unsupported(format!("{} is not an SSH connection", profile.name)));
        }
        let secret = secrets.get(&profile.secret_account())?;
        let auth = match profile.auth {
            Auth::Password => SshAuth::Password(
                secret.ok_or_else(|| Error::Unsupported(format!("no password is saved for {}", profile.name)))?,
            ),
            Auth::Key => SshAuth::Key { path: expand_home(&profile.key_path, home), passphrase: secret },
        };
        let mut target = SshTarget::new(profile.host.clone(), profile.user.clone(), auth);
        target.port = profile.port;
        target.startup = (!profile.startup.trim().is_empty()).then(|| profile.startup.clone());
        target.reconnect = Reconnect {
            attempts: profile.reconnect_attempts,
            delay: Duration::from_secs(u64::from(profile.reconnect_delay_secs)),
        };
        target.known_hosts = Some(known_hosts.to_path_buf());
        target.host_key = HostKeyPolicy::AcceptNew;
        if let Some(jump) = &profile.jump {
            // A jump host that fails to connect is not retried on its own: the pane's reconnect covers the chain.
            target.jump = Some(Box::new(self.ssh_target(jump, secrets, home, known_hosts)?));
        }
        Ok(target)
    }

    /// The local command for a WSL or Docker profile, `None` for SSH.
    pub fn local_command(&self, id: &str) -> Option<LocalCommand> {
        let p = self.get(id)?;
        match p.kind {
            Kind::Ssh => None,
            Kind::Wsl => Some(LocalCommand { program: "wsl.exe".into(), args: vec!["-d".into(), p.host.clone()] }),
            Kind::Docker => Some(LocalCommand {
                program: "docker".into(),
                args: vec![
                    "exec".into(),
                    "-it".into(),
                    p.host.clone(),
                    if p.shell.trim().is_empty() { "sh".into() } else { p.shell.clone() },
                ],
            }),
        }
    }

    /// Parse `text`. Profiles that do not validate are dropped, each with a line saying why.
    pub fn from_json(text: &str) -> (Profiles, LoadStatus) {
        let parsed: std::result::Result<Profiles, _> = serde_json::from_str(text);
        let Ok(mut profiles) = parsed else {
            return (Profiles::default(), LoadStatus::Reset("connections.json is not valid JSON of the expected shape".into()));
        };
        if profiles.version != VERSION {
            return (Profiles::default(), LoadStatus::Reset(format!("connections.json has version {}, this build reads {VERSION}", profiles.version)));
        }
        let mut notes = Vec::new();
        let mut ids = HashSet::new();
        profiles.items.retain(|p| match p.problem() {
            Some(why) => {
                notes.push(format!("dropped {}: {why}", p.id));
                false
            }
            None if !ids.insert(p.id.clone()) => {
                notes.push(format!("dropped a second connection with the id {}", p.id));
                false
            }
            None => true,
        });
        // A jump that points nowhere is cleared; a chain too deep or looping is cut at the profile that starts it.
        let known: HashSet<String> = profiles.items.iter().map(|p| p.id.clone()).collect();
        for p in &mut profiles.items {
            if p.jump.as_ref().is_some_and(|j| !known.contains(j)) {
                notes.push(format!("{}: the jump host does not exist; it was removed", p.id));
                p.jump = None;
            }
        }
        while let Some(why) = profiles.validate() {
            let Some((id, _)) = why.split_once(": ") else { break };
            let id = id.to_owned();
            notes.push(format!("dropped {id}: {why}"));
            profiles.items.retain(|p| p.id != id);
            for p in &mut profiles.items {
                if p.jump.as_deref() == Some(id.as_str()) {
                    p.jump = None;
                }
            }
        }
        let status = if notes.is_empty() { LoadStatus::Clean } else { LoadStatus::Repaired(notes) };
        (profiles, status)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{\"version\":1,\"items\":[]}".into())
    }

    /// Read `<dir>/connections.json`. A file that cannot be used is kept as `.bad` and the list is empty.
    pub fn load(dir: &Path) -> (Profiles, LoadStatus) {
        let path = dir.join(FILE);
        let bytes = match read_limited(&path, MAX_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return (Profiles::default(), LoadStatus::Clean),
            Err(e) => return (Profiles::default(), LoadStatus::Reset(format!("cannot read {FILE}: {e}"))),
        };
        let (profiles, status) = match String::from_utf8(bytes) {
            Ok(text) => Profiles::from_json(&text),
            Err(_) => (Profiles::default(), LoadStatus::Reset(format!("{FILE} is not UTF-8 text"))),
        };
        match status {
            LoadStatus::Clean => (profiles, LoadStatus::Clean),
            other => {
                // Whatever was changed, the original stays next to the file.
                let _ = backup_bad(&path);
                (profiles, other)
            }
        }
    }

    /// Write `<dir>/connections.json` atomically.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        write_atomic(&dir.join(FILE), self.to_json().as_bytes())
    }
}

fn expand_home(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

    fn ssh(id: &str) -> Profile {
        let mut p = Profile::new(id, id.to_uppercase(), format!("{id}.example.org"));
        p.user = "holz".into();
        p.key_path = "~/.ssh/id_ed25519".into();
        p
    }

    #[test]
    fn a_valid_list_round_trips() {
        let mut list = Profiles::default();
        list.upsert(ssh("talos")).unwrap();
        let mut b = ssh("nas");
        b.jump = Some("talos".into());
        list.upsert(b).unwrap();
        let (back, status) = Profiles::from_json(&list.to_json());
        assert_eq!(status, LoadStatus::Clean);
        assert_eq!(back, list);
    }

    #[test]
    fn a_profile_that_does_not_validate_is_refused_and_the_list_is_unchanged() {
        let mut list = Profiles::default();
        list.upsert(ssh("a")).unwrap();
        let mut bad = ssh("b");
        bad.host = "has blank".into();
        assert!(list.upsert(bad).is_err());
        let mut dash = ssh("c");
        dash.host = "-oProxyCommand=x".into();
        assert!(list.upsert(dash).is_err());
        let mut noid = ssh("D");
        noid.id = "Not Allowed".into();
        assert!(list.upsert(noid).is_err());
        assert_eq!(list.items.len(), 1);
    }

    #[test]
    fn jump_hosts_must_exist_and_may_not_loop() {
        let mut list = Profiles::default();
        let mut a = ssh("a");
        a.jump = Some("missing".into());
        assert!(list.upsert(a).is_err());
        list.upsert(ssh("a")).unwrap();
        let mut b = ssh("b");
        b.jump = Some("a".into());
        list.upsert(b).unwrap();
        let mut a2 = ssh("a");
        a2.jump = Some("b".into());
        assert!(list.upsert(a2).is_err(), "a loop");
        // Removing a jump host clears the jump of the profiles that used it.
        assert!(list.remove("a"));
        assert_eq!(list.get("b").unwrap().jump, None);
    }

    #[test]
    fn a_damaged_list_loses_only_the_bad_profiles() {
        let text = r#"{"version":1,"items":[
            {"id":"ok","name":"Ok","host":"h","user":"u","key_path":"k"},
            {"id":"BAD ID","name":"x","host":"h","user":"u","key_path":"k"},
            {"id":"jump","name":"J","host":"h","user":"u","key_path":"k","jump":"gone"}]}"#;
        let (list, status) = Profiles::from_json(text);
        assert_eq!(list.items.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["ok", "jump"]);
        assert_eq!(list.get("jump").unwrap().jump, None);
        let LoadStatus::Repaired(notes) = status else { panic!("not repaired") };
        assert_eq!(notes.len(), 2, "{notes:?}");
        let (empty, status) = Profiles::from_json("not json");
        assert!(empty.items.is_empty());
        assert!(matches!(status, LoadStatus::Reset(_)));
    }

    #[test]
    fn fresh_ids_are_clean_and_unique() {
        let mut list = Profiles::default();
        assert_eq!(list.fresh_id("My Server!"), "my-server");
        list.upsert(ssh("my-server")).unwrap();
        assert_eq!(list.fresh_id("My Server!"), "my-server-2");
        assert_eq!(list.fresh_id("???"), "host");
    }

    #[test]
    fn a_target_takes_its_secret_from_the_keychain_and_its_jump_with_it() {
        let secrets = MemoryStore::default();
        let mut list = Profiles::default();
        list.upsert(ssh("bastion")).unwrap();
        let mut inner = ssh("inner");
        inner.auth = Auth::Password;
        inner.jump = Some("bastion".into());
        inner.startup = "tmux attach -t main".into();
        list.upsert(inner).unwrap();
        let known = Path::new("/tmp/known");
        let missing = list.ssh_target("inner", &secrets, None, known);
        assert!(missing.is_err(), "a password login needs a saved password");
        secrets.set("ssh:inner", "pw").unwrap();
        secrets.set("ssh:bastion", "phrase").unwrap();
        let t = list.ssh_target("inner", &secrets, Some(Path::new("/home/holz")), known).unwrap();
        assert!(matches!(&t.auth, SshAuth::Password(p) if p == "pw"));
        assert_eq!(t.startup.as_deref(), Some("tmux attach -t main"));
        let jump = t.jump.unwrap();
        match &jump.auth {
            SshAuth::Key { path, passphrase } => {
                assert_eq!(path, Path::new("/home/holz/.ssh/id_ed25519"), "~/ is the home directory");
                assert_eq!(passphrase.as_deref(), Some("phrase"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wsl_and_docker_open_as_local_commands() {
        let mut list = Profiles::default();
        let mut w = Profile::new("ubuntu", "Ubuntu", "Ubuntu-24.04");
        w.kind = Kind::Wsl;
        list.upsert(w).unwrap();
        let mut d = Profile::new("web", "Web", "web-1");
        d.kind = Kind::Docker;
        d.shell = "bash".into();
        list.upsert(d).unwrap();
        let w = list.local_command("ubuntu").unwrap();
        assert_eq!((w.program.as_str(), w.args.as_slice()), ("wsl.exe", &["-d".to_string(), "Ubuntu-24.04".to_string()][..]));
        let d = list.local_command("web").unwrap();
        assert_eq!(d.args, ["exec", "-it", "web-1", "bash"]);
        list.upsert(ssh("a")).unwrap();
        assert!(list.local_command("a").is_none());
    }

    #[test]
    fn an_old_profile_without_wsh_loads_as_false_and_a_false_one_is_not_written() {
        let old = r#"{"version":1,"items":[{"id":"talos","name":"Talos","host":"h","user":"u","key_path":"k"}]}"#;
        let (list, status) = Profiles::from_json(old);
        assert_eq!(status, LoadStatus::Clean);
        assert!(!list.get("talos").unwrap().wsh, "a file written before the field means false");
        assert!(!list.to_json().contains("wsh"), "a false value is not written");

        let mut on = ssh("web");
        on.wsh = true;
        let mut both = Profiles::default();
        both.upsert(ssh("off")).unwrap();
        both.upsert(on).unwrap();
        let text = both.to_json();
        assert_eq!(text.matches("\"wsh\": true").count(), 1, "only the ticked profile writes it: {text}");
        assert_eq!(text.matches("wsh").count(), 1, "{text}");
        let (back, status) = Profiles::from_json(&text);
        assert_eq!(status, LoadStatus::Clean);
        assert!(back.get("web").unwrap().wsh && !back.get("off").unwrap().wsh);
        assert_eq!(back, both);
    }

    #[test]
    fn wsh_only_applies_to_ssh() {
        let mut ok = ssh("a");
        ok.wsh = true;
        assert_eq!(ok.problem(), None);
        for kind in [Kind::Wsl, Kind::Docker] {
            let mut p = Profile::new("b", "B", "box");
            p.kind = kind;
            p.wsh = true;
            assert_eq!(p.problem().as_deref(), Some("wsh only applies to SSH"), "{kind:?}");
            p.wsh = false;
            assert_eq!(p.problem(), None, "{kind:?} without it is fine");
        }
        let mut list = Profiles::default();
        let mut w = Profile::new("w", "W", "Ubuntu");
        w.kind = Kind::Wsl;
        w.wsh = true;
        assert!(list.upsert(w).is_err() && list.items.is_empty(), "a save is refused and changes nothing");
        // A file that carries it on a WSL profile loses that profile with a note, and keeps the valid one.
        let text = r#"{"version":1,"items":[
            {"id":"w","name":"W","kind":"wsl","host":"Ubuntu","wsh":true},
            {"id":"s","name":"S","host":"h","user":"u","key_path":"k","wsh":true}]}"#;
        let (list, status) = Profiles::from_json(text);
        assert_eq!(list.items.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["s"]);
        let LoadStatus::Repaired(notes) = status else { panic!("not repaired") };
        assert!(notes.iter().any(|n| n.contains("wsh only applies to SSH")), "{notes:?}");
        assert!(list.get("s").unwrap().wsh);
    }

    #[test]
    fn the_field_travels_in_the_synced_connection_list() {
        // The sync moves `connections.json` as bytes, so the bytes of one device must read the same on another. A
        // list from a build that knows no `wsh` has every profile false; one with it keeps it through a file.
        let dir = std::env::temp_dir().join(format!("holzwave-profiles-wsh-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut list = Profiles::default();
        let mut ticked = ssh("ticked");
        ticked.wsh = true;
        list.upsert(ticked).unwrap();
        list.upsert(ssh("plain")).unwrap();
        list.save(&dir).unwrap();
        let bytes = fs::read(dir.join(FILE)).unwrap();
        let other = std::env::temp_dir().join(format!("holzwave-profiles-wsh-b-{}", std::process::id()));
        let _ = fs::remove_dir_all(&other);
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join(FILE), &bytes).unwrap();
        let (b, status) = Profiles::load(&other);
        assert_eq!(status, LoadStatus::Clean);
        assert!(b.get("ticked").unwrap().wsh && !b.get("plain").unwrap().wsh);
        assert_eq!(b, list);
        let old = r#"{"version":1,"items":[{"id":"a","name":"A","host":"h","user":"u","key_path":"k"},{"id":"b","name":"B","host":"h","user":"u","key_path":"k"}]}"#;
        let (c, _) = Profiles::from_json(old);
        assert!(c.items.iter().all(|p| !p.wsh));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&other);
    }

    #[test]
    fn saving_a_connection_with_wsh_changes_no_secret() {
        use crate::connections::{Connections, SecretChange};
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!("holzwave-profiles-wsh-save-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let secrets = Arc::new(MemoryStore::default());
        let (c, _) = Connections::load(&dir, secrets.clone(), Some(PathBuf::from("/home/holz")), &[]);
        let mut p = Profile::new("", "Talos", "host.example.org");
        p.user = "holz".into();
        p.auth = Auth::Password;
        let saved = c.save(p, SecretChange::Set { value: "hunter2".into() }).unwrap();
        assert!(!saved.profile.wsh);
        let before = c.generation();
        let mut flipped = saved.profile.clone();
        flipped.wsh = true;
        let view = c.save(flipped, SecretChange::Keep).unwrap();
        assert!(view.profile.wsh && view.has_secret);
        assert_eq!(secrets.get("ssh:talos").unwrap().as_deref(), Some("hunter2"), "the keychain entry is as it was");
        assert_eq!(c.generation(), before + 1, "one save, one generation");
        assert!(c.list()[0].profile.wsh);
        let file = fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(file.contains("\"wsh\": true") && !file.contains("hunter2"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_view_for_the_page_lists_wsh_only_when_true() {
        use crate::connections::ProfileView;
        let off = serde_json::to_value(ProfileView { profile: ssh("a"), has_secret: false }).unwrap();
        assert!(off.get("wsh").is_none(), "{off}");
        let mut p = ssh("b");
        p.wsh = true;
        let on = serde_json::to_value(ProfileView { profile: p, has_secret: true }).unwrap();
        assert_eq!(on.get("wsh"), Some(&serde_json::Value::Bool(true)), "{on}");
    }

    #[test]
    fn the_file_is_written_and_read_back() {
        let dir = std::env::temp_dir().join(format!("holzwave-profiles-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut list = Profiles::default();
        list.upsert(ssh("talos")).unwrap();
        list.save(&dir).unwrap();
        let (back, status) = Profiles::load(&dir);
        assert_eq!(status, LoadStatus::Clean);
        assert_eq!(back, list);
        assert!(!fs::read_to_string(dir.join(FILE)).unwrap().contains("passphrase"), "no secret in the file");
        fs::write(dir.join(FILE), "garbage").unwrap();
        let (empty, status) = Profiles::load(&dir);
        assert!(empty.items.is_empty() && matches!(status, LoadStatus::Reset(_)));
        assert!(dir.join("connections.json.bad").exists(), "the damaged file is kept");
        let _ = fs::remove_dir_all(&dir);
    }
}
