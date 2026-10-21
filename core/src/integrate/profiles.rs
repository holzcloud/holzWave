//! Integration profiles: the synced file that pins a context, a subscription or a section, marks it production and
//! names the environment of a connection, and the machine-local file that holds the paths of one computer.
//!
//! Three layers, because the tools keep their own credentials (a kubeconfig, a talosconfig, `~/.azure`, `~/.gam`) and
//! holzWave references them and never copies them: `integrations.json` holds names and flags and is synced;
//! `integration_local.json` holds binary and configuration paths and never leaves the computer; and a handful of small
//! `secret_env` values live in the operating system's keychain, under `int:<profile id>:<NAME>`, and travel only inside
//! the already encrypted secrets file. A value is at most 1 KiB because the Windows credential store refuses a blob
//! over 2560 bytes. Deleting a secret on one device is not synced.
//!
//! A synced file that sets an environment variable is an attack on every device that receives it, so the loader is the
//! gate: it validates every field and refuses by the strictest platform's rule. A file that fails any rule is copied
//! to its `.bad` sibling and the app starts without profiles; it is never half applied.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use serde::de::value::MapAccessDeserializer;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

use super::args::{ContextName, EnvName, Namespace, NodeName, ProfileId, SectionName, SubscriptionId};
use zeroize::Zeroizing;

use super::run::EnvPolicy;
use super::{CheckedSelection, SelectKind, ToolId};
use crate::ai::tools::is_hidden;
use crate::persist::{backup_bad, bad_path, read_limited, write_atomic};
use crate::platform::Os;
use crate::secrets::SecretStore;

/// The synced file of profiles.
pub const FILE: &str = "integrations.json";
/// The machine-local file of paths. It is not in the sync file list.
pub const LOCAL_FILE: &str = "integration_local.json";
/// The most bytes either file may have; a larger file is not read.
pub const MAX_BYTES: u64 = 256 * 1024;
/// The most profiles in the file.
pub const MAX_PROFILES: usize = 64;
/// The version of both files.
pub const VERSION: u32 = 1;
/// The most characters in the display name of a profile.
pub const MAX_NAME: usize = 64;
/// The most plain environment variables of a profile.
pub const MAX_ENV: usize = 32;
/// The most secret variable names of a profile.
pub const MAX_SECRET_NAMES: usize = 8;
/// The most bytes of a plain environment value.
pub const MAX_VALUE: usize = 1024;
/// The most paths one tool may list as configuration files in the machine-local file.
pub const MAX_PATHS: usize = 8;
/// The most bytes of one path in the machine-local file.
pub const MAX_PATH: usize = 4096;
/// The most bytes of a secret variable. The Windows credential store refuses a blob over 2560 bytes and
/// [`crate::secrets::MAX_LEN`] allows 4096, so this module enforces the lower bound on every platform.
pub const MAX_SECRET: usize = 1024;

/// Names that a synced profile may not set because they make a tool, a pager or the shell of a pane start another
/// program, read another start-up file or load another piece of code (a tool opens the editor for a diff, a pager
/// runs a command, the shell reads the file `ZDOTDIR` names, `AZURE_EXTENSION_DIR` is Python code the Azure tool
/// imports, `SSLKEYLOGFILE` writes the keys of every connection to a file). They are refused in the values and the
/// secret names of a synced profile only: the runner's own deny rule ([`EnvPolicy::is_denied`]) leaves them alone, so
/// the person's own `EDITOR` or `HOME` still reaches the child from the inherited environment.
pub const PROGRAM_NAMES: [&str; 15] = [
    "BROWSER",
    "EDITOR",
    "VISUAL",
    "PAGER",
    "SHELL",
    "SHELLOPTS",
    "BASHOPTS",
    "PROMPT_COMMAND",
    "ZDOTDIR",
    "HOME",
    "XDG_CONFIG_HOME",
    "KUBE_EDITOR",
    "KUBECTL_EXTERNAL_DIFF",
    "AZURE_EXTENSION_DIR",
    "SSLKEYLOGFILE",
];

/// The names the app sets itself from its own state (the isolation of a pane, the pinned selection). The runner refuses
/// them in a profile too; the runner keeps its own copy, and a test holds the two lists equal.
pub const APP_SET_NAMES: [&str; 5] = ["KUBECONFIG", "TALOSCONFIG", "AZURE_CONFIG_DIR", "GAMCFGDIR", "GAMCFGSECTION"];

/// The most bytes of a damaged file that are copied to `.bad`; a larger one is renamed instead of copied, so that a
/// hostile file cannot fill the disk twice.
const MAX_BAD_COPY: u64 = 4 * 1024 * 1024;

// ---------------------------------------------------------------------------------------------------------------------
// Reading a struct from a JSON object only.
// ---------------------------------------------------------------------------------------------------------------------

/// Reads a struct from a JSON object only. A derived `Deserialize` also reads an array of the member values, in order,
/// which the schema does not, so every struct of the files is read through this.
fn object_only<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<T, D::Error> {
    struct Members<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for Members<T> {
        type Value = T;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an object")
        }

        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
            T::deserialize(MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_map(Members(PhantomData))
}

/// A struct that is read from an object only.
struct Object<T>(T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        object_only(deserializer).map(Object)
    }
}

/// The first look at a file: only its version, so that a file of another version is refused for that reason and not
/// for the members it may have.
#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    version: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    // Read as a number so that `1.0` and `"1"` are refused; compared by the probe.
    #[allow(dead_code)]
    version: u32,
    profiles: Vec<Object<RawProfile>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    id: String,
    tool: ToolId,
    name: String,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    subscription: Option<String>,
    #[serde(default)]
    section: Option<String>,
    #[serde(default)]
    node: Option<String>,
    #[serde(default)]
    production: bool,
    #[serde(default)]
    isolated: bool,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    secret_env: Vec<String>,
}

/// The first look at the text of a file: not too large, a JSON object, and exactly the version this code reads.
fn check_version(file: &str, text: &str) -> Result<(), String> {
    if text.len() as u64 > MAX_BYTES {
        return Err(format!("{file} is larger than {MAX_BYTES} bytes"));
    }
    let probe: Object<Probe> = serde_json::from_str(text).map_err(|e| read_note(file, &e))?;
    if probe.0.version != Some(serde_json::json!(VERSION)) {
        return Err(format!("{file} does not have the version {VERSION}"));
    }
    Ok(())
}

/// What a read error says: where, and never what. The text of a `serde_json` error holds the offending value.
fn read_note(file: &str, error: &serde_json::Error) -> String {
    let what = if error.is_data() { "has a member or a value that is not allowed" } else { "is not valid JSON" };
    format!("{file} {what} (line {}, column {})", error.line(), error.column())
}

// ---------------------------------------------------------------------------------------------------------------------
// The profile.
// ---------------------------------------------------------------------------------------------------------------------

/// What holzWave remembers about one connection: names and flags, never a credential. The fields are public so that
/// the page's dialog can build one, and every way into a set ([`IntegrationProfiles::upsert`], [`IntegrationProfiles::parse`],
/// [`IntegrationProfiles::load`]) validates it again.
#[derive(Clone, PartialEq, Eq)]
pub struct IntegrationProfile {
    /// The id, `[a-z0-9_-]`, at most 32 characters.
    pub id: ProfileId,
    /// The tool the profile is for.
    pub tool: ToolId,
    /// What the person calls it, 1 to 64 characters.
    pub name: String,
    /// The context it pins (Kubernetes and Talos).
    pub context: Option<ContextName>,
    /// The namespace it pins (Kubernetes).
    pub namespace: Option<Namespace>,
    /// The subscription it pins (Azure).
    pub subscription: Option<SubscriptionId>,
    /// The configuration section it pins (GAM).
    pub section: Option<SectionName>,
    /// The node it pins (Talos).
    pub node: Option<NodeName>,
    /// Whether the pinned context is production: a write there needs a second press.
    pub production: bool,
    /// Whether the tool runs with a configuration directory of its own for this profile (Azure and GAM only).
    pub isolated: bool,
    /// Plain environment variables for a run of the tool.
    pub env: BTreeMap<String, String>,
    /// The names, never the values, of the variables whose values are in the keychain.
    pub secret_env: Vec<String>,
}

impl IntegrationProfile {
    /// A profile with nothing pinned, no flag and no variable.
    pub fn new(id: ProfileId, tool: ToolId, name: &str) -> Self {
        Self {
            id,
            tool,
            name: name.to_owned(),
            context: None,
            namespace: None,
            subscription: None,
            section: None,
            node: None,
            production: false,
            isolated: false,
            env: BTreeMap::new(),
            secret_env: Vec::new(),
        }
    }

    /// The selection a pane that uses this profile starts with: the profile and what it pins.
    pub fn selection(&self) -> CheckedSelection {
        CheckedSelection {
            profile: Some(self.id.clone()),
            context: self.context.clone(),
            namespace: self.namespace.clone(),
            subscription: self.subscription.clone(),
            section: self.section.clone(),
            node: self.node.clone(),
        }
    }
}

impl fmt::Debug for IntegrationProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IntegrationProfile")
            .field("id", &self.id.as_str())
            .field("tool", &self.tool)
            .field("production", &self.production)
            .field("env", &self.env.len())
            .field("secret_env", &self.secret_env.len())
            .finish_non_exhaustive()
    }
}

/// A raw profile as a typed one: every value goes through the validator of its kind. A note names the profile and the
/// field and never a value; the id is named only when it is valid, otherwise the position stands in for it. The rules
/// that look at the profile as a whole are [`check_profile`]'s.
fn convert(raw: RawProfile, nth: usize) -> Result<IntegrationProfile, String> {
    let id = ProfileId::parse(&raw.id).map_err(|_| format!("profile {nth}: the id is not valid"))?;
    let tool = raw.tool;
    let bad = |field: &str| format!("profile {}: {field} is not valid", id.as_str());
    Ok(IntegrationProfile {
        context: raw.context.as_deref().map(ContextName::parse).transpose().map_err(|_| bad("context"))?,
        namespace: raw.namespace.as_deref().map(Namespace::parse).transpose().map_err(|_| bad("namespace"))?,
        subscription: raw
            .subscription
            .as_deref()
            .map(SubscriptionId::parse)
            .transpose()
            .map_err(|_| bad("subscription"))?,
        section: raw.section.as_deref().map(SectionName::parse).transpose().map_err(|_| bad("section"))?,
        node: raw.node.as_deref().map(NodeName::parse).transpose().map_err(|_| bad("node"))?,
        id,
        tool,
        name: raw.name,
        production: raw.production,
        isolated: raw.isolated,
        env: raw.env,
        secret_env: raw.secret_env,
    })
}

impl<'de> Deserialize<'de> for IntegrationProfile {
    /// Reads one profile from an object (the page sends one). The types are checked, not the rules of a set:
    /// [`IntegrationProfiles::upsert`] checks those.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let Object(raw) = Object::<RawProfile>::deserialize(deserializer)?;
        convert(raw, 1).map_err(de::Error::custom)
    }
}

/// The wire form of a profile: every member is written, so a reader of another version sees all of them.
#[derive(Serialize)]
struct WireProfile<'a> {
    id: &'a str,
    tool: ToolId,
    name: &'a str,
    context: Option<&'a str>,
    namespace: Option<&'a str>,
    subscription: Option<&'a str>,
    section: Option<&'a str>,
    node: Option<&'a str>,
    production: bool,
    isolated: bool,
    env: &'a BTreeMap<String, String>,
    secret_env: &'a [String],
}

impl Serialize for IntegrationProfile {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        WireProfile {
            id: self.id.as_str(),
            tool: self.tool,
            name: &self.name,
            context: self.context.as_ref().map(ContextName::as_str),
            namespace: self.namespace.as_ref().map(Namespace::as_str),
            subscription: self.subscription.as_ref().map(SubscriptionId::as_str),
            section: self.section.as_ref().map(SectionName::as_str),
            node: self.node.as_ref().map(NodeName::as_str),
            production: self.production,
            isolated: self.isolated,
            env: &self.env,
            secret_env: &self.secret_env,
        }
        .serialize(serializer)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The set.
// ---------------------------------------------------------------------------------------------------------------------

/// The profiles of one person: the content of `integrations.json`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct IntegrationProfiles {
    /// The profiles, in the order the person sees them.
    pub items: Vec<IntegrationProfile>,
}

impl fmt::Debug for IntegrationProfiles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IntegrationProfiles({} profiles)", self.items.len())
    }
}

#[derive(Serialize)]
struct WireFile<'a> {
    version: u32,
    profiles: &'a [IntegrationProfile],
}

impl IntegrationProfiles {
    /// The profile with exactly this id.
    pub fn get(&self, id: &str) -> Option<&IntegrationProfile> {
        self.items.iter().find(|profile| profile.id.as_str() == id)
    }

    /// Why the set cannot be used, or `Ok`: at most 64 profiles, every id once, and every profile by
    /// [`check_profile`]. The note names profile ids and fields and never a value.
    pub fn validate(&self) -> Result<(), String> {
        if self.items.len() > MAX_PROFILES {
            return Err(format!("the file holds more than {MAX_PROFILES} profiles"));
        }
        for (at, profile) in self.items.iter().enumerate() {
            if self.items[..at].iter().any(|earlier| earlier.id == profile.id) {
                return Err(format!("profile {}: the id is used twice", profile.id.as_str()));
            }
            check_profile(profile)?;
        }
        Ok(())
    }

    /// Whether the file of this set stays within [`MAX_BYTES`]. Every field is bounded, and a set that is within every
    /// bound can still be larger than the file may be, so a set that does not fit is never written: the next start
    /// would refuse the whole file.
    fn fits(&self) -> Result<(), String> {
        if self.to_json().len() as u64 > MAX_BYTES {
            return Err(format!("the profiles would be larger than {MAX_BYTES} bytes"));
        }
        Ok(())
    }

    /// Reads the text of a file. `Err` is the note: a fixed sentence built from a position, an id and a field name.
    pub fn parse(text: &str) -> Result<Self, String> {
        check_version(FILE, text)?;
        let Object(raw): Object<RawFile> = serde_json::from_str(text).map_err(|e| read_note(FILE, &e))?;
        if raw.profiles.len() > MAX_PROFILES {
            return Err(format!("the file holds more than {MAX_PROFILES} profiles"));
        }
        let mut items = Vec::with_capacity(raw.profiles.len());
        for (at, Object(profile)) in raw.profiles.into_iter().enumerate() {
            items.push(convert(profile, at + 1)?);
        }
        let set = Self { items };
        set.validate()?;
        Ok(set)
    }

    /// The text of the file: pretty JSON with a newline at the end.
    pub fn to_json(&self) -> String {
        let file = WireFile { version: VERSION, profiles: &self.items };
        let mut text = serde_json::to_string_pretty(&file).unwrap_or_else(|_| format!("{{\"version\":{VERSION}}}"));
        text.push('\n');
        text
    }

    /// Reads `<dir>/integrations.json`. A missing file is an empty set with no note. A file that cannot be read or
    /// fails any rule is copied to `.bad`, the set is empty and the one note says why, without a value.
    pub fn load(dir: &Path) -> (Self, Vec<String>) {
        load_file(dir, FILE, Self::parse)
    }

    /// Writes `<dir>/integrations.json` atomically. A set that would not load again is not written.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        if self.validate().and_then(|()| self.fits()).is_err() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the profiles are not valid and are not written"));
        }
        fs::create_dir_all(dir)?;
        write_atomic(&dir.join(FILE), self.to_json().as_bytes())
    }

    /// Adds a profile or replaces the one with the same id. The set is left as it was when the result is not valid.
    pub fn upsert(&mut self, profile: IntegrationProfile) -> Result<(), String> {
        let before = self.items.clone();
        match self.items.iter_mut().find(|item| item.id == profile.id) {
            Some(slot) => *slot = profile,
            None => self.items.push(profile),
        }
        self.validate().and_then(|()| self.fits()).inspect_err(|_| self.items = before)
    }

    /// Removes the profile with this id; says whether there was one.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.id.as_str() != id);
        self.items.len() != before
    }

    /// Whether a context of a tool is production: when any profile that pins it says so.
    pub fn production_for(&self, tool: ToolId, context: &ContextName) -> bool {
        self.items
            .iter()
            .any(|item| item.tool == tool && item.production && item.context.as_ref() == Some(context))
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Secret variables: values only in the keychain.
// ---------------------------------------------------------------------------------------------------------------------

/// The account of a secret variable in the keychain: `int:<profile id>:<NAME>`. Both parts are validated types, so the
/// account holds no control character and no separator of its own.
pub fn secret_account(id: &ProfileId, name: &EnvName) -> String {
    format!("int:{}:{}", id.as_str(), name.as_str())
}

/// What a store failure says. The text of the store's own error is dropped: a binding may repeat what it was given.
const KEYCHAIN_FAILED: &str = "the keychain could not be used";

impl IntegrationProfiles {
    /// The profile with this id, or the note that it is not there.
    fn known(&self, id: &ProfileId) -> Result<&IntegrationProfile, String> {
        self.get(id.as_str()).ok_or_else(|| format!("profile {}: there is no such profile", id.as_str()))
    }

    /// Stores the value of one secret variable in the keychain, under [`secret_account`]. The profile must exist and
    /// list the name in `secret_env`; the value is 1 to [`MAX_SECRET`] bytes without a NUL. A refused value changes
    /// nothing. The error is a fixed sentence with the profile id and never the value.
    pub fn set_secret(
        &self,
        store: &dyn SecretStore,
        id: &ProfileId,
        name: &EnvName,
        value: &str,
    ) -> Result<(), String> {
        let profile = self.known(id)?;
        let at = id.as_str();
        if !profile.secret_env.iter().any(|listed| listed == name.as_str()) {
            return Err(format!("profile {at}: the name is not one of its secret variables"));
        }
        if value.is_empty() || value.len() > MAX_SECRET || value.contains('\0') {
            return Err(format!("profile {at}: a secret value is 1 to {MAX_SECRET} bytes without a NUL"));
        }
        store.set(&secret_account(id, name), value).map_err(|_| format!("profile {at}: {KEYCHAIN_FAILED}"))
    }

    /// Removes the value of one secret variable. Clearing one that is not there is not an error, and the name need not
    /// be listed any more, so that a name that was just taken out of `secret_env` can still be wiped.
    pub fn clear_secret(&self, store: &dyn SecretStore, id: &ProfileId, name: &EnvName) -> Result<(), String> {
        self.known(id)?;
        store.delete(&secret_account(id, name)).map_err(|_| format!("profile {}: {KEYCHAIN_FAILED}", id.as_str()))
    }

    /// Whether a listed secret variable of a profile has a value: a yes or a no. The value is not kept, and a store
    /// that fails answers no.
    pub fn has_secret(&self, store: &dyn SecretStore, id: &ProfileId, name: &EnvName) -> bool {
        let listed = self.get(id.as_str()).is_some_and(|p| p.secret_env.iter().any(|n| n == name.as_str()));
        listed
            && store
                .get(&secret_account(id, name))
                .ok()
                .flatten()
                .map(Zeroizing::new)
                .is_some_and(|value| !value.is_empty())
    }

    /// The secret variables of a profile for the moment of one run, in the profile's order: only the names that have a
    /// value, as wiping strings that the caller drops when the child is started. A name the profile does not list is
    /// never read. A store that fails is an error, so that a run never starts without the credentials that were asked
    /// for.
    pub fn resolve_secrets(
        &self,
        store: &dyn SecretStore,
        id: &ProfileId,
    ) -> Result<Vec<(String, Zeroizing<String>)>, String> {
        let profile = self.known(id)?;
        let mut out = Vec::with_capacity(profile.secret_env.len());
        for listed in &profile.secret_env {
            let name = EnvName::parse(listed)
                .map_err(|_| format!("profile {}: a secret name is not valid", id.as_str()))?;
            let found = store
                .get(&secret_account(id, &name))
                .map_err(|_| format!("profile {}: {KEYCHAIN_FAILED}", id.as_str()))?;
            if let Some(value) = found.map(Zeroizing::new).filter(|value| !value.is_empty()) {
                out.push((listed.clone(), value));
            }
        }
        Ok(out)
    }

    /// Removes a profile and every secret it lists. Says whether there was a profile. When a secret cannot be deleted
    /// the profile is kept, so that the names are still known when the person tries again.
    pub fn remove_with_secrets(&mut self, store: &dyn SecretStore, id: &ProfileId) -> Result<bool, String> {
        let Some(profile) = self.get(id.as_str()) else { return Ok(false) };
        let mut failed = false;
        for listed in &profile.secret_env {
            match EnvName::parse(listed) {
                Ok(name) => failed |= store.delete(&secret_account(id, &name)).is_err(),
                Err(_) => failed = true,
            }
        }
        if failed {
            return Err(format!("profile {}: {KEYCHAIN_FAILED}; the profile is kept", id.as_str()));
        }
        Ok(self.remove(id.as_str()))
    }
}

/// Reads one of the two files of this module: absent or empty is the default with no note; a file that cannot be read,
/// is too large or fails `parse` is kept as `.bad` and gives the default and one note that holds no value of the file.
fn load_file<T: Default>(dir: &Path, file: &str, parse: fn(&str) -> Result<T, String>) -> (T, Vec<String>) {
    let path = dir.join(file);
    let note = match read_limited(&path, MAX_BYTES) {
        Ok(None) => return (T::default(), Vec::new()),
        Ok(Some(bytes)) if bytes.is_empty() => return (T::default(), Vec::new()),
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(text) => match parse(&text) {
                Ok(value) => return (value, Vec::new()),
                Err(note) => note,
            },
            Err(_) => format!("{file} is not UTF-8 text"),
        },
        Err(e) => match e.kind() {
            io::ErrorKind::InvalidData => format!("{file} is larger than {MAX_BYTES} bytes"),
            io::ErrorKind::InvalidInput => return (T::default(), vec![format!("{file} is not a regular file")]),
            _ => return (T::default(), vec![format!("{file} cannot be read")]),
        },
    };
    let mut notes = vec![note];
    if !keep_bad(&path) {
        notes.push(format!("a copy of {file} could not be kept"));
    }
    (T::default(), notes)
}

/// Keeps what could not be used next to the file. A file over [`MAX_BAD_COPY`] is renamed, not copied. Says whether
/// the damaged content is still on disk.
fn keep_bad(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_BAD_COPY => backup_bad(path).is_ok(),
        Ok(meta) if meta.is_file() => fs::rename(path, bad_path(path)).is_ok(),
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The rules of one profile.
// ---------------------------------------------------------------------------------------------------------------------

/// Whether a synced profile may not use `name` for a variable: not a variable name, denied by the runner's rule for
/// either operating-system family (the file is valid on every platform or on none, and Windows reads names without
/// regard to case), one the app sets itself, or one that starts another program. The deny rule is asked before the
/// shape, so that a name Windows would fold onto a denied one is refused for that reason too.
fn name_refused(name: &str) -> bool {
    EnvPolicy::is_denied(name, Os::Windows)
        || EnvPolicy::is_denied(name, Os::Macos)
        || APP_SET_NAMES.contains(&name)
        || PROGRAM_NAMES.contains(&name)
        || EnvName::parse(name).is_err()
}

/// How a variable is named in a note: by its name when that is a valid variable name (which holds no hostile text),
/// otherwise as "an env name".
fn named(label: &str, name: &str) -> String {
    if EnvName::parse(name).is_ok() { format!("{label} {name}") } else { "an env name".to_owned() }
}

/// Whether a value looks like the path of this computer or another: it starts with `/`, `~/`, two backslashes, or a
/// drive letter, a colon and a separator. Paths belong in the machine-local file and never travel.
fn looks_like_a_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with('/')
        || value.starts_with("~/")
        || value.starts_with("\\\\")
        || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\'))
}

/// What is wrong with a plain value, as the end of a note.
fn value_problem(value: &str) -> Option<&'static str> {
    if value.len() > MAX_VALUE {
        Some("a value that is too long")
    } else if value.chars().any(is_hidden) {
        Some("a value with a control or invisible character")
    } else if looks_like_a_path(value) {
        Some("a value that looks like a path")
    } else {
        None
    }
}

/// The rules of a profile on its own, in this order: the name, the selection fields the tool does not take, `isolated`,
/// the plain variables, the secret names. The note names the profile and the field and never a value.
fn check_profile(profile: &IntegrationProfile) -> Result<(), String> {
    let at = profile.id.as_str();
    let fail = |what: String| Err(format!("profile {at}: {what}"));
    let tool = profile.tool;

    let name_chars = profile.name.chars().count();
    if name_chars == 0 || name_chars > MAX_NAME || profile.name.chars().any(is_hidden) {
        return fail("name is not valid".into());
    }
    let selection = [
        (SelectKind::Context, "context", profile.context.is_some()),
        (SelectKind::Namespace, "namespace", profile.namespace.is_some()),
        (SelectKind::Subscription, "subscription", profile.subscription.is_some()),
        (SelectKind::Section, "section", profile.section.is_some()),
        (SelectKind::Node, "node", profile.node.is_some()),
    ];
    for (kind, field, set) in selection {
        if set && !tool.selects().contains(&kind) {
            return fail(format!("{field} is not used by {} profiles", tool.as_str()));
        }
    }
    if profile.isolated && !matches!(tool, ToolId::Azure | ToolId::Gam) {
        return fail("isolated is only for azure and gam profiles".into());
    }
    if profile.env.len() > MAX_ENV {
        return fail(format!("env has more than {MAX_ENV} variables"));
    }
    for (name, value) in &profile.env {
        if name_refused(name) {
            return fail(format!("{} is not allowed", named("env", name)));
        }
        if let Some(problem) = value_problem(value) {
            return fail(format!("env {name} has {problem}"));
        }
    }
    if profile.secret_env.len() > MAX_SECRET_NAMES {
        return fail(format!("secret_env has more than {MAX_SECRET_NAMES} names"));
    }
    for (index, name) in profile.secret_env.iter().enumerate() {
        if name_refused(name) {
            return fail(format!("{} is not allowed", named("secret_env", name)));
        }
        if profile.secret_env[..index].contains(name) {
            return fail(format!("secret_env {name} is listed twice"));
        }
        if profile.env.contains_key(name) {
            return fail(format!("secret_env {name} is also a plain variable"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// The machine-local file.
// ---------------------------------------------------------------------------------------------------------------------

/// Why a path cannot be kept in the machine-local file, as the end of a note. The path itself is never named.
fn path_problem(path: &Path) -> Option<&'static str> {
    let Some(text) = path.to_str() else { return Some("is not text") };
    if text.len() > MAX_PATH {
        Some("is longer than the limit")
    } else if text.chars().any(char::is_control) {
        Some("has a control character")
    } else if text.starts_with("\\\\") || text.starts_with("//") {
        // A UNC path (`\\server\share`), a device path (`\\?\C:\x`, `\\.\COM1`) and their slash forms: a program on
        // another computer or a device is not a binary of this one.
        Some("is a network or device path")
    } else if !path.is_absolute() {
        Some("is not an absolute path")
    } else {
        None
    }
}

/// The machine-specific values of one computer: where each tool's program is and which configuration files it reads.
/// The file is read by this computer only and is not in the sync file list; a synced profile never holds a path.
/// Paths are validated by this computer's own rule for an absolute path.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct LocalFile {
    /// The program of each tool, by the tool's name.
    binaries: BTreeMap<String, PathBuf>,
    /// The configuration files of each tool, by the tool's name.
    config_paths: BTreeMap<String, Vec<PathBuf>>,
}

impl fmt::Debug for LocalFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LocalFile({} binaries, {} configuration lists)", self.binaries.len(), self.config_paths.len())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLocal {
    #[allow(dead_code)]
    version: u32,
    #[serde(default)]
    binaries: BTreeMap<String, String>,
    #[serde(default)]
    config_paths: BTreeMap<String, Vec<String>>,
}

#[derive(Serialize)]
struct WireLocal<'a> {
    version: u32,
    binaries: &'a BTreeMap<String, PathBuf>,
    config_paths: &'a BTreeMap<String, Vec<PathBuf>>,
}

impl LocalFile {
    /// The note for a path that is not allowed: the tool and the member, never the path.
    fn refuse(tool: &str, member: &str, problem: &str) -> String {
        format!("{LOCAL_FILE}: the {member} of {tool} {problem}")
    }

    /// Why the file cannot be used, or `Ok`: every tool is one of the four, every path passes [`path_problem`], at most
    /// [`MAX_PATHS`] configuration paths per tool.
    fn validate(&self) -> Result<(), String> {
        for (name, path) in &self.binaries {
            let tool = ToolId::parse(name).map_err(|_| format!("{LOCAL_FILE} names a tool that does not exist"))?;
            if let Some(problem) = path_problem(path) {
                return Err(Self::refuse(tool.as_str(), "binary path", problem));
            }
        }
        for (name, paths) in &self.config_paths {
            let tool = ToolId::parse(name).map_err(|_| format!("{LOCAL_FILE} names a tool that does not exist"))?;
            if paths.len() > MAX_PATHS {
                return Err(Self::refuse(tool.as_str(), "configuration paths", "are more than the limit"));
            }
            if let Some(problem) = paths.iter().find_map(|path| path_problem(path)) {
                return Err(Self::refuse(tool.as_str(), "configuration path", problem));
            }
        }
        Ok(())
    }

    /// Whether the file of this set stays within [`MAX_BYTES`] (escaping can double a Windows path).
    fn fits(&self) -> Result<(), String> {
        if self.to_json().len() as u64 > MAX_BYTES {
            return Err(format!("{LOCAL_FILE} would be larger than {MAX_BYTES} bytes"));
        }
        Ok(())
    }

    /// Reads the text of a file. `Err` is the note: a fixed sentence built from a position, a tool and a member name.
    pub fn parse(text: &str) -> Result<Self, String> {
        check_version(LOCAL_FILE, text)?;
        let Object(raw): Object<RawLocal> = serde_json::from_str(text).map_err(|e| read_note(LOCAL_FILE, &e))?;
        let set = Self {
            binaries: raw.binaries.into_iter().map(|(tool, path)| (tool, PathBuf::from(path))).collect(),
            config_paths: raw
                .config_paths
                .into_iter()
                .map(|(tool, paths)| (tool, paths.into_iter().map(PathBuf::from).collect()))
                .collect(),
        };
        set.validate()?;
        Ok(set)
    }

    /// The text of the file: pretty JSON with a newline at the end.
    pub fn to_json(&self) -> String {
        let file = WireLocal { version: VERSION, binaries: &self.binaries, config_paths: &self.config_paths };
        let mut text = serde_json::to_string_pretty(&file).unwrap_or_else(|_| format!("{{\"version\":{VERSION}}}"));
        text.push('\n');
        text
    }

    /// Reads `<dir>/integration_local.json`. A missing file is an empty set with no note. A file that cannot be read
    /// or fails any rule is copied to `.bad`, the set is empty and the one note says why, without a path.
    pub fn load(dir: &Path) -> (Self, Vec<String>) {
        load_file(dir, LOCAL_FILE, Self::parse)
    }

    /// Writes `<dir>/integration_local.json` atomically. A set that would not load again is not written.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        if self.validate().and_then(|()| self.fits()).is_err() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the local paths are not valid and are not written"));
        }
        fs::create_dir_all(dir)?;
        write_atomic(&dir.join(LOCAL_FILE), self.to_json().as_bytes())
    }

    /// Remembers where the program of a tool is. The path must be absolute on this computer; the set is left as it was
    /// when it is not.
    pub fn set_binary(&mut self, tool: ToolId, path: &Path) -> Result<(), String> {
        if let Some(problem) = path_problem(path) {
            return Err(Self::refuse(tool.as_str(), "binary path", problem));
        }
        let before = self.binaries.insert(tool.as_str().to_owned(), path.to_path_buf());
        self.fits().inspect_err(|_| match before {
            Some(old) => drop(self.binaries.insert(tool.as_str().to_owned(), old)),
            None => drop(self.binaries.remove(tool.as_str())),
        })
    }

    /// Forgets the program of a tool; says whether there was one.
    pub fn clear_binary(&mut self, tool: ToolId) -> bool {
        self.binaries.remove(tool.as_str()).is_some()
    }

    /// The program of a tool on this computer, if the person pinned one.
    pub fn binary(&self, tool: ToolId) -> Option<&Path> {
        self.binaries.get(tool.as_str()).map(PathBuf::as_path)
    }

    /// Remembers the configuration files of a tool (a kubeconfig, a talosconfig): at most [`MAX_PATHS`], each absolute
    /// on this computer. An empty list forgets them. The set is left as it was when the list is not valid.
    pub fn set_config_paths(&mut self, tool: ToolId, paths: &[PathBuf]) -> Result<(), String> {
        if paths.len() > MAX_PATHS {
            return Err(Self::refuse(tool.as_str(), "configuration paths", "are more than the limit"));
        }
        if let Some(problem) = paths.iter().find_map(|path| path_problem(path)) {
            return Err(Self::refuse(tool.as_str(), "configuration path", problem));
        }
        let before = if paths.is_empty() {
            self.config_paths.remove(tool.as_str())
        } else {
            self.config_paths.insert(tool.as_str().to_owned(), paths.to_vec())
        };
        self.fits().inspect_err(|_| match before {
            Some(old) => drop(self.config_paths.insert(tool.as_str().to_owned(), old)),
            None => drop(self.config_paths.remove(tool.as_str())),
        })
    }

    /// The configuration files of a tool on this computer; empty when none is pinned.
    pub fn config_paths(&self, tool: ToolId) -> &[PathBuf] {
        self.config_paths.get(tool.as_str()).map_or(&[], Vec::as_slice)
    }
}
