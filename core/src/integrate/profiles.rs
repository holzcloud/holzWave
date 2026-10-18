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
use std::path::Path;

use serde::de::value::MapAccessDeserializer;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

use super::args::{ContextName, Namespace, NodeName, ProfileId, SectionName, SubscriptionId};
use super::{CheckedSelection, SelectKind, ToolId};
use crate::persist::{backup_bad, bad_path, read_limited, write_atomic};

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

/// What a read error says: where, and never what. The text of a `serde_json` error holds the offending value.
fn read_note(error: &serde_json::Error) -> String {
    let what = if error.is_data() { "has a member or a value that is not allowed" } else { "is not valid JSON" };
    format!("{FILE} {what} (line {}, column {})", error.line(), error.column())
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

/// Whether a tool takes a selection field.
fn belongs(tool: ToolId, kind: SelectKind) -> bool {
    tool.selects().contains(&kind)
}

/// A raw profile as a typed one. A note names the profile and the field and never a value; the id is named only when
/// it is valid, otherwise the position stands in for it.
fn convert(raw: RawProfile, nth: usize) -> Result<IntegrationProfile, String> {
    let id = ProfileId::parse(&raw.id).map_err(|_| format!("profile {nth}: the id is not valid"))?;
    let tool = raw.tool;
    let fields = [
        (SelectKind::Context, "context", &raw.context),
        (SelectKind::Namespace, "namespace", &raw.namespace),
        (SelectKind::Subscription, "subscription", &raw.subscription),
        (SelectKind::Section, "section", &raw.section),
        (SelectKind::Node, "node", &raw.node),
    ];
    for (kind, field, value) in fields {
        if value.is_some() && !belongs(tool, kind) {
            return Err(format!("profile {}: {field} does not belong to a {} profile", id.as_str(), tool.as_str()));
        }
    }
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

    /// Why the set cannot be used, or `Ok`. The note names profile ids and fields and never a value.
    pub fn validate(&self) -> Result<(), String> {
        Ok(())
    }

    /// Reads the text of a file. `Err` is the note: a fixed sentence built from a position, an id and a field name.
    pub fn parse(text: &str) -> Result<Self, String> {
        let probe: Object<Probe> = serde_json::from_str(text).map_err(|e| read_note(&e))?;
        if probe.0.version != Some(serde_json::json!(VERSION)) {
            return Err(format!("{FILE} does not have the version {VERSION}"));
        }
        let Object(raw): Object<RawFile> = serde_json::from_str(text).map_err(|e| read_note(&e))?;
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
        let path = dir.join(FILE);
        let note = match read_limited(&path, MAX_BYTES) {
            Ok(None) => return (Self::default(), Vec::new()),
            Ok(Some(bytes)) => match String::from_utf8(bytes) {
                Ok(text) => match Self::parse(&text) {
                    Ok(set) => return (set, Vec::new()),
                    Err(note) => note,
                },
                Err(_) => format!("{FILE} is not UTF-8 text"),
            },
            Err(e) => match e.kind() {
                io::ErrorKind::InvalidData => format!("{FILE} is larger than {MAX_BYTES} bytes"),
                io::ErrorKind::InvalidInput => format!("{FILE} is not a regular file"),
                _ => return (Self::default(), vec![format!("{FILE} cannot be read")]),
            },
        };
        let mut notes = vec![note];
        if !keep_bad(&path) {
            notes.push(format!("a copy of {FILE} could not be kept"));
        }
        (Self::default(), notes)
    }

    /// Writes `<dir>/integrations.json` atomically. A set that would not load again is not written.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        if self.validate().is_err() {
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
        self.validate().inspect_err(|_| self.items = before)
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

/// Keeps what could not be used next to the file. A file over [`MAX_BAD_COPY`] is renamed, not copied. Says whether
/// the damaged content is still on disk.
fn keep_bad(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_BAD_COPY => backup_bad(path).is_ok(),
        Ok(meta) if meta.is_file() => fs::rename(path, bad_path(path)).is_ok(),
        _ => false,
    }
}
