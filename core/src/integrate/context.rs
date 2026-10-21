//! The selection of a context, namespace, subscription, section or node for a pane, and how it is applied to a run.
//!
//! There are three tiers of context, and this module is the first two:
//!
//! 1. **Flags per command.** Every command carries its own selection in its arguments (`--context=...`,
//!    `--namespace=...`, `--subscription=...`, `select <section>`), so two panes can talk to two clusters at the same
//!    time and a selection changes nothing on disk. [`resolve`] turns what a pane selected, plus the pins of a profile,
//!    into the validated selection the argument builders take.
//! 2. **An overlay per pane.** A shell pane cannot carry flags, so it gets a small credential-free file that names the
//!    current context and nothing else, and a variable that lists it first ([`apply_env`]).
//! 3. **An explicit Local verb** changes the tool's own configuration, behind a card. No function here calls one, and
//!    none writes below a tool's own directory.
//!
//! Three facts belong with the application of a selection ([`apply_env`]). The overlay's lifetime is the pane's: it is
//! written when a shell pane starts, replaced when the pane's selection changes, and removed by the sweep of the shell
//! plan when the pane is gone ([`kube::sweep_overlays`]); this module never removes one. A shell's start-up file can
//! export `KUBECONFIG` again and replace the value, so what a pane shows is what holzWave set and not a promise about
//! what the shell then did. And isolation means that the person signs in again inside the private directory of a
//! profile: Azure and GAM keep their tokens in the directory the variable names, which is empty the first time.
//!
//! No process is started and no tool is asked in this module: the defaults of a tool (its own current selection) are
//! stored by the service when it has read them, and this module is state, resolution and application. A profile's
//! secret values never pass through it; it reports the names only.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::args::{ArgError, ProfileId};
use super::profiles::{IntegrationProfile, IntegrationProfiles};
use super::kube::ContextEntry;
use super::profiles::LocalFile;
use super::run::EnvPolicy;
use super::kube;
use super::{CheckedSelection, SelectKind, Selection, ToolId};
use crate::layout::PaneId;
use crate::persist::{backup_bad, bad_path, read_limited, write_atomic};
use crate::platform::{self, Os};

/// The machine-local file of pane selections. It is not in the sync file list.
pub const PANES_FILE: &str = "pane_integrations.json";
/// The most panes the file holds.
pub const MAX_PANES: usize = 256;
/// The most bytes the file may have; a larger file is not read.
pub const MAX_BYTES: u64 = 64 * 1024;
/// The version of the file.
pub const VERSION: u32 = 1;
/// The most bytes of a damaged file that are copied to `.bad`; a larger one is renamed instead.
const MAX_BAD_COPY: u64 = 4 * 1024 * 1024;

/// Why a selection could not be resolved. Each variant is a fixed sentence that holds no byte of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextError {
    /// The selection names a profile that does not exist.
    UnknownProfile,
    /// The profile belongs to another tool.
    WrongTool,
    /// A field of the selection was refused by its validator, or is not a field of the tool.
    Selection(ArgError),
    /// The view is not a verb of the tool that reads and shows a view.
    UnknownView,
    /// Another pane would not fit in the file of pane selections.
    TooMany,
}

impl ContextError {
    /// The fixed sentence of the refusal.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownProfile => "the profile does not exist",
            Self::WrongTool => "the profile belongs to another tool",
            Self::Selection(error) => error.as_str(),
            Self::UnknownView => "the view is not a read view of the tool",
            Self::TooMany => "there are more pane selections than are kept",
        }
    }
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for ContextError {}

impl From<ArgError> for ContextError {
    fn from(error: ArgError) -> Self {
        Self::Selection(error)
    }
}

/// A selection turned into what a run needs.
#[derive(Clone)]
pub struct Resolved {
    /// The tool.
    pub tool: ToolId,
    /// The selection after the pins of the profile filled the fields it left empty. An explicit field wins.
    pub selection: CheckedSelection,
    /// The profile the selection named, if any.
    pub profile: Option<ProfileId>,
    /// The display name of the profile that makes the selected context production, or `None` when it is not.
    pub production: Option<String>,
    /// The plain environment of the profile, in name order. Values are the person's own and not secret.
    pub env_plain: Vec<(String, String)>,
    /// The names, never the values, of the profile's secret variables. The service resolves the values for a run.
    pub secret_names: Vec<String>,
    /// Whether the profile asks for a configuration directory of its own (Azure and GAM).
    pub isolated: bool,
}

impl fmt::Debug for Resolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolved")
            .field("tool", &self.tool)
            .field("selection", &self.selection)
            .field("profile", &self.profile.as_ref().map(ProfileId::as_str))
            .field("production", &self.production.is_some())
            .field("env_plain", &self.env_plain.len())
            .field("secret_names", &self.secret_names.len())
            .field("isolated", &self.isolated)
            .finish()
    }
}

/// The selection fields of `profile` that `tool` takes, as a selection of their own.
fn pins(tool: ToolId, profile: &IntegrationProfile) -> CheckedSelection {
    let pinned = profile.selection();
    let takes = |kind: SelectKind| tool.selects().contains(&kind);
    CheckedSelection {
        profile: pinned.profile,
        context: pinned.context.filter(|_| takes(SelectKind::Context)),
        namespace: pinned.namespace.filter(|_| takes(SelectKind::Namespace)),
        subscription: pinned.subscription.filter(|_| takes(SelectKind::Subscription)),
        section: pinned.section.filter(|_| takes(SelectKind::Section)),
        node: pinned.node.filter(|_| takes(SelectKind::Node)),
    }
}

/// Resolves `selection` for `tool` against `profiles`.
///
/// Every field that is set is validated first (a field the tool does not select is refused). When the selection names
/// a profile, that profile must exist and be one of this tool's, and the fields the selection leaves empty are filled
/// from what the profile pins; a field the selection sets itself always wins, because the person's click is the more
/// recent intention. Nothing is written and no process is started.
///
/// # Errors
///
/// A fixed sentence: a field that failed its validator, a profile that does not exist, or a profile of another tool.
pub fn resolve(profiles: &IntegrationProfiles, tool: ToolId, selection: &Selection) -> Result<Resolved, ContextError> {
    let mut checked = selection.check(tool)?;
    let mut out = Resolved {
        tool,
        selection: CheckedSelection::default(),
        profile: None,
        production: None,
        env_plain: Vec::new(),
        secret_names: Vec::new(),
        isolated: false,
    };
    if let Some(id) = checked.profile.clone() {
        let profile = profiles.get(id.as_str()).ok_or(ContextError::UnknownProfile)?;
        if profile.tool != tool {
            return Err(ContextError::WrongTool);
        }
        let pinned = pins(tool, profile);
        checked.context = checked.context.or(pinned.context);
        checked.namespace = checked.namespace.or(pinned.namespace);
        checked.subscription = checked.subscription.or(pinned.subscription);
        checked.section = checked.section.or(pinned.section);
        checked.node = checked.node.or(pinned.node);
        out.profile = Some(id);
        out.env_plain = profile.env.iter().map(|(name, value)| (name.clone(), value.clone())).collect();
        out.secret_names = profile.secret_env.clone();
        out.isolated = profile.isolated;
    }
    out.production = production_name(profiles, tool, &checked);
    out.selection = checked;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------------------
// The production rule.
// ---------------------------------------------------------------------------------------------------------------------

/// The value of a selection that decides whether it is production: the context of Kubernetes and Talos, the
/// subscription of Azure, the section of GAM.
fn deciding(tool: ToolId, selection: &CheckedSelection) -> Option<&str> {
    match tool {
        ToolId::Kube | ToolId::Talos => selection.context.as_ref().map(|name| name.as_str()),
        ToolId::Azure => selection.subscription.as_ref().map(|name| name.as_str()),
        ToolId::Gam => selection.section.as_ref().map(|name| name.as_str()),
    }
}

/// The same value as a profile pins it.
fn pinned(profile: &IntegrationProfile) -> Option<&str> {
    match profile.tool {
        ToolId::Kube | ToolId::Talos => profile.context.as_ref().map(|name| name.as_str()),
        ToolId::Azure => profile.subscription.as_ref().map(|name| name.as_str()),
        ToolId::Gam => profile.section.as_ref().map(|name| name.as_str()),
    }
}

/// The one production rule: a selection is production when any profile of the same tool that pins the same context
/// (subscription, section) says `production`, however the selection was made (through that profile, through another
/// one, or by a direct pick of the name). The answer is the display name of the first such profile in id order. A
/// profile without the flag pins nothing production, and a selection with no deciding value is not production.
/// [`resolve`] and [`ContextState::summary`] both call this function, so the card, the header and the status line
/// cannot disagree.
fn production_name(profiles: &IntegrationProfiles, tool: ToolId, selection: &CheckedSelection) -> Option<String> {
    let wanted = deciding(tool, selection)?;
    profiles
        .items
        .iter()
        .filter(|profile| profile.tool == tool && profile.production && pinned(profile) == Some(wanted))
        .min_by(|a, b| a.id.as_str().cmp(b.id.as_str()))
        .map(|profile| profile.name.clone())
}

// ---------------------------------------------------------------------------------------------------------------------
// The state.
// ---------------------------------------------------------------------------------------------------------------------

/// Whether `view` is a verb of `tool` that reads and shows a view.
fn is_view(tool: ToolId, view: &str) -> bool {
    super::is_view_verb(tool, view)
}

/// What one pane shows and has selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSelection {
    /// The tool of the pane.
    pub tool: ToolId,
    /// The view verb the pane shows: a read verb of the tool with a view for its output.
    pub view: String,
    /// What the pane selected. It passed [`Selection::check`] for the tool when it was stored.
    pub selection: Selection,
}

/// The tool's own current selection as last read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolDefault {
    selection: Selection,
    stale: bool,
}

#[derive(Default)]
struct Inner {
    panes: BTreeMap<PaneId, PaneSelection>,
    picks: BTreeMap<u8, Selection>,
    defaults: BTreeMap<u8, ToolDefault>,
    seq: u64,
}

/// A small stable key for a tool in the maps of the state.
fn slot(tool: ToolId) -> u8 {
    match tool {
        ToolId::Kube => 0,
        ToolId::Talos => 1,
        ToolId::Azure => 2,
        ToolId::Gam => 3,
    }
}

/// What is selected where: the selection of each integration pane, one pick per tool that new blocks start from, and
/// the tool's own current selection as last read (a default, with a stale flag).
///
/// The state is thread-safe. [`ContextState::seq`] moves by one on every real change and not on a store of an equal
/// value, so a reader can tell whether anything moved. A change of one pane never changes another pane's selection, the
/// pick or any file of a tool. Only the pane selections are persisted ([`ContextState::save`]); a pick and a default
/// are in memory, because a new window starts from the tool's own current selection.
#[derive(Default)]
pub struct ContextState {
    inner: Mutex<Inner>,
}

impl fmt::Debug for ContextState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.lock();
        write!(f, "ContextState({} panes, seq {})", inner.panes.len(), inner.seq)
    }
}

impl ContextState {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// The count of changes so far: it grows by one for every change that stored a different value.
    pub fn seq(&self) -> u64 {
        self.lock().seq
    }

    /// Stores what `pane` shows and has selected. The selection must pass [`Selection::check`] for the tool and `view`
    /// must be a read verb of the tool that shows a view. Says whether the stored value changed. A new pane that would
    /// make the file larger than [`MAX_BYTES`] or hold more than [`MAX_PANES`] panes is refused, so that what is saved
    /// always loads again.
    ///
    /// # Errors
    ///
    /// A fixed sentence: the selection is not valid for the tool, the view is not one of its views, or the file is full.
    pub fn set_pane(&self, pane: PaneId, tool: ToolId, view: &str, selection: &Selection) -> Result<bool, ContextError> {
        selection.check(tool)?;
        if !is_view(tool, view) {
            return Err(ContextError::UnknownView);
        }
        let entry = PaneSelection { tool, view: view.to_owned(), selection: selection.clone() };
        let mut inner = self.lock();
        if inner.panes.get(&pane) == Some(&entry) {
            return Ok(false);
        }
        let before = inner.panes.insert(pane, entry);
        if (before.is_none() && inner.panes.len() > MAX_PANES) || file_text(&inner.panes).len() as u64 > MAX_BYTES {
            match before {
                Some(old) => drop(inner.panes.insert(pane, old)),
                None => drop(inner.panes.remove(&pane)),
            }
            return Err(ContextError::TooMany);
        }
        inner.seq += 1;
        Ok(true)
    }

    /// Removes the entry of `pane`; says whether it had one.
    pub fn forget_pane(&self, pane: PaneId) -> bool {
        let mut inner = self.lock();
        let had = inner.panes.remove(&pane).is_some();
        if had {
            inner.seq += 1;
        }
        had
    }

    /// What `pane` shows and has selected, when it has an entry.
    pub fn pane(&self, pane: PaneId) -> Option<PaneSelection> {
        self.lock().panes.get(&pane).cloned()
    }

    /// The panes that have an entry for `tool`, in order.
    pub fn panes_of(&self, tool: ToolId) -> Vec<PaneId> {
        self.lock().panes.iter().filter(|(_, entry)| entry.tool == tool).map(|(pane, _)| *pane).collect()
    }

    /// The selection that applies to `pane` for `tool`: the pane's own, when it has an entry for this tool; else the
    /// pick of the tool; else the tool's own current selection as last read; else an empty selection. With no pane it
    /// is the pick, else the default, else empty.
    pub fn selection_for(&self, tool: ToolId, pane: Option<PaneId>) -> Selection {
        self.lock().chosen(tool, pane).0
    }

    /// Stores the pick of `tool`, which new blocks of the tool start from. Says whether it changed.
    ///
    /// # Errors
    ///
    /// A fixed sentence when the selection is not valid for the tool.
    pub fn set_pick(&self, tool: ToolId, selection: &Selection) -> Result<bool, ContextError> {
        selection.check(tool)?;
        let mut inner = self.lock();
        if inner.picks.get(&slot(tool)) == Some(selection) {
            return Ok(false);
        }
        inner.picks.insert(slot(tool), selection.clone());
        inner.seq += 1;
        Ok(true)
    }

    /// The pick of `tool`, if the person made one.
    pub fn pick(&self, tool: ToolId) -> Option<Selection> {
        self.lock().picks.get(&slot(tool)).cloned()
    }

    /// Writes `<dir>/pane_integrations.json` atomically.
    ///
    /// # Errors
    ///
    /// The file system's error, or `InvalidData` when the state would not fit the file (it cannot, see
    /// [`ContextState::set_pane`]).
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        let text = file_text(&self.lock().panes);
        if text.len() as u64 > MAX_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the pane selections are larger than the file may be"));
        }
        fs::create_dir_all(dir)?;
        write_atomic(&dir.join(PANES_FILE), text.as_bytes())
    }

    /// Reads `<dir>/pane_integrations.json` and keeps the entries of the panes in `live_panes`. A missing or empty file
    /// is an empty state with no note. An entry that is not well formed, names a tool or a view that does not exist, or
    /// whose selection fails the tool's check is dropped with a note; the notes name a pane and a reason and never a
    /// value of the file. A file that cannot be used as a whole (too large, not JSON, more than [`MAX_PANES`] panes, an
    /// unknown member, another version) is copied to `.bad`, and the state starts empty with one note.
    ///
    /// Nothing is applied: no file is written and no process is started.
    pub fn load(dir: &Path, live_panes: &[PaneId]) -> (Self, Vec<String>) {
        let path = dir.join(PANES_FILE);
        let bytes = match read_limited(&path, MAX_BYTES) {
            Ok(None) => return (Self::new(), Vec::new()),
            Ok(Some(bytes)) if bytes.is_empty() => return (Self::new(), Vec::new()),
            Ok(Some(bytes)) => bytes,
            Err(error) => {
                return match error.kind() {
                    io::ErrorKind::InvalidData => {
                        (Self::new(), quarantine(&path, format!("{PANES_FILE} is larger than {MAX_BYTES} bytes")))
                    }
                    io::ErrorKind::InvalidInput => (Self::new(), vec![format!("{PANES_FILE} is not a regular file")]),
                    _ => (Self::new(), vec![format!("{PANES_FILE} cannot be read")]),
                };
            }
        };
        match parse(&bytes, live_panes) {
            Ok((panes, notes)) => (Self { inner: Mutex::new(Inner { panes, ..Inner::default() }) }, notes),
            Err(note) => (Self::new(), quarantine(&path, note)),
        }
    }
}

impl Inner {
    /// The selection that applies, and whether it is the default's.
    fn chosen(&self, tool: ToolId, pane: Option<PaneId>) -> (Selection, bool) {
        if let Some(entry) = pane.and_then(|pane| self.panes.get(&pane)).filter(|entry| entry.tool == tool) {
            return (entry.selection.clone(), false);
        }
        if let Some(pick) = self.picks.get(&slot(tool)) {
            return (pick.clone(), false);
        }
        match self.defaults.get(&slot(tool)) {
            Some(default) => (default.selection.clone(), true),
            None => (Selection::default(), false),
        }
    }
}

/// The text of the file for these panes: pretty JSON with a newline at the end. A selection writes only the members
/// that are set.
fn file_text(panes: &BTreeMap<PaneId, PaneSelection>) -> String {
    let mut out = Map::new();
    for (pane, entry) in panes {
        let mut selection = Map::new();
        let fields = [
            ("profile", &entry.selection.profile),
            ("context", &entry.selection.context),
            ("namespace", &entry.selection.namespace),
            ("subscription", &entry.selection.subscription),
            ("section", &entry.selection.section),
            ("node", &entry.selection.node),
        ];
        for (name, value) in fields {
            if let Some(value) = value {
                selection.insert(name.to_owned(), Value::String(value.clone()));
            }
        }
        out.insert(
            pane.to_string(),
            json!({ "tool": entry.tool.as_str(), "view": entry.view, "selection": Value::Object(selection) }),
        );
    }
    let mut text = serde_json::to_string_pretty(&json!({ "version": VERSION, "panes": Value::Object(out) }))
        .unwrap_or_else(|_| format!("{{\"version\":{VERSION}}}"));
    text.push('\n');
    text
}

/// Keeps what could not be used next to the file and returns the notes. A file over [`MAX_BAD_COPY`] is renamed, not
/// copied, so a hostile file cannot fill the disk twice.
fn quarantine(path: &Path, note: String) -> Vec<String> {
    let kept = match fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_BAD_COPY => backup_bad(path).is_ok(),
        Ok(meta) if meta.is_file() => fs::rename(path, bad_path(path)).is_ok(),
        _ => false,
    };
    let mut notes = vec![note];
    if !kept {
        notes.push(format!("a copy of {PANES_FILE} could not be kept"));
    }
    notes
}

/// Reads the text of the file. `Err` is the note for a file that is damaged as a whole; `Ok` holds the entries of live
/// panes that are well formed and the notes of those that are not.
fn parse(bytes: &[u8], live: &[PaneId]) -> Result<(BTreeMap<PaneId, PaneSelection>, Vec<String>), String> {
    let Ok(Value::Object(top)) = serde_json::from_slice::<Value>(bytes) else {
        return Err(format!("{PANES_FILE} is not a JSON object"));
    };
    if top.keys().any(|key| key != "version" && key != "panes") {
        return Err(format!("{PANES_FILE} has a member that is not allowed"));
    }
    if top.get("version") != Some(&json!(VERSION)) {
        return Err(format!("{PANES_FILE} does not have the version {VERSION}"));
    }
    let Some(Value::Object(entries)) = top.get("panes") else {
        return Err(format!("{PANES_FILE} has no list of panes"));
    };
    if entries.len() > MAX_PANES {
        return Err(format!("{PANES_FILE} holds more than {MAX_PANES} panes"));
    }
    let mut panes = BTreeMap::new();
    let mut notes = Vec::new();
    for (key, value) in entries {
        let Some(pane) = key.parse::<PaneId>().ok().filter(|pane| pane.to_string() == *key) else {
            notes.push(format!("{PANES_FILE}: an entry has a pane id that is not a number"));
            continue;
        };
        if !live.contains(&pane) {
            continue;
        }
        match entry(value) {
            Ok(entry) => drop(panes.insert(pane, entry)),
            Err(why) => notes.push(format!("{PANES_FILE}: pane {pane} {why}")),
        }
    }
    Ok((panes, notes))
}

/// One entry of the file, validated again: the tool, a view it has and a selection that passes its check.
fn entry(value: &Value) -> Result<PaneSelection, &'static str> {
    let Value::Object(members) = value else { return Err("is not an object") };
    if members.keys().any(|key| !matches!(key.as_str(), "tool" | "view" | "selection")) {
        return Err("has a member that is not allowed");
    }
    let tool = members
        .get("tool")
        .and_then(Value::as_str)
        .and_then(|name| ToolId::parse(name).ok())
        .ok_or("names a tool that does not exist")?;
    let view = members.get("view").and_then(Value::as_str).filter(|view| is_view(tool, view));
    let view = view.ok_or("names a view the tool does not have")?;
    let selection = members.get("selection").ok_or("has no selection")?;
    let selection = Selection::deserialize(selection).map_err(|_| "has a selection that is not well formed")?;
    selection.check(tool).map_err(|_| "has a selection that fails the check of the tool")?;
    Ok(PaneSelection { tool, view: view.to_owned(), selection })
}

// ---------------------------------------------------------------------------------------------------------------------
// The tool's own selection and the summary.
// ---------------------------------------------------------------------------------------------------------------------

/// What the detector says about the tool, as the service fills it. This module has its own small type so that it does
/// not depend on the detector's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detection {
    /// The tool was found on this computer.
    Found,
    /// The tool was not found.
    NotFound,
    /// The tool is there and the person is not signed in (Azure).
    SignedOut,
    /// Nothing is known yet.
    Unknown,
}

/// The state word of a [`ContextRow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowState {
    /// The tool is found, or something is selected.
    Found,
    /// The tool is not found.
    NotFound,
    /// The person is signed out.
    SignedOut,
    /// Nothing is known.
    Unknown,
}

/// What the sidebar section, the status segment and the pane chip show for a tool, derived once. Only the fields the
/// tool selects are filled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextRow {
    /// The tool.
    pub tool: ToolId,
    /// Found, not found, signed out or unknown.
    pub state: RowState,
    /// The context (Kubernetes, Talos).
    pub context: Option<String>,
    /// The namespace (Kubernetes).
    pub namespace: Option<String>,
    /// The subscription (Azure).
    pub subscription: Option<String>,
    /// The section (GAM).
    pub section: Option<String>,
    /// The node (Talos).
    pub node: Option<String>,
    /// The display name of the profile that makes this production, or `None`.
    pub production: Option<String>,
    /// Whether the value shown is the tool's own selection as read a while ago.
    pub stale: bool,
}

impl ContextState {
    /// Stores the tool's own current selection as the service read it, with `stale` false. It never names a profile
    /// (any profile in `selection` is left out). Says whether anything changed: a different value, or the same value
    /// that had been marked stale.
    ///
    /// # Errors
    ///
    /// A fixed sentence when the selection is not valid for the tool.
    pub fn set_default(&self, tool: ToolId, selection: &Selection) -> Result<bool, ContextError> {
        selection.check(tool)?;
        let stored = ToolDefault { selection: Selection { profile: None, ..selection.clone() }, stale: false };
        let mut inner = self.lock();
        if inner.defaults.get(&slot(tool)) == Some(&stored) {
            return Ok(false);
        }
        inner.defaults.insert(slot(tool), stored);
        inner.seq += 1;
        Ok(true)
    }

    /// Marks the tool's own selection as possibly out of date. The value is kept. Says whether it changed.
    pub fn mark_stale(&self, tool: ToolId) -> bool {
        let mut inner = self.lock();
        match inner.defaults.get_mut(&slot(tool)) {
            Some(default) if !default.stale => {
                default.stale = true;
                inner.seq += 1;
                true
            }
            _ => false,
        }
    }

    /// The tool's own current selection as last read, and whether it is stale.
    pub fn default_of(&self, tool: ToolId) -> Option<(Selection, bool)> {
        self.lock().defaults.get(&slot(tool)).map(|default| (default.selection.clone(), default.stale))
    }

    /// The row for `tool`, for `pane` when given: the pane's selection when it has one, else the pick, else the
    /// tool's own current selection. `detection` is what the service knows of the tool. The state is `not_found` or
    /// `signed_out` as the detection says, `found` when the tool is found or something is selected, and `unknown`
    /// otherwise. `production` comes from the same rule as [`resolve`]; `stale` is true only when the value shown is
    /// the default's and the default is stale. A profile that no longer exists does not hide the fields.
    pub fn summary(
        &self,
        profiles: &IntegrationProfiles,
        tool: ToolId,
        pane: Option<PaneId>,
        detection: Detection,
    ) -> ContextRow {
        let (chosen, from_default, default_stale) = {
            let inner = self.lock();
            let (chosen, from_default) = inner.chosen(tool, pane);
            let stale = from_default && inner.defaults.get(&slot(tool)).is_some_and(|default| default.stale);
            (chosen, from_default, stale)
        };
        let resolved = resolve(profiles, tool, &chosen).or_else(|_| {
            // The profile was deleted, or belongs to another tool: show what the selection itself holds.
            resolve(profiles, tool, &Selection { profile: None, ..chosen })
        });
        let (selection, production) = match resolved {
            Ok(resolved) => (resolved.selection, resolved.production),
            Err(_) => (CheckedSelection::default(), None),
        };
        let text = |value: Option<&str>| value.map(str::to_owned);
        let row = ContextRow {
            tool,
            state: RowState::Unknown,
            context: text(selection.context.as_ref().map(|name| name.as_str())),
            namespace: text(selection.namespace.as_ref().map(|name| name.as_str())),
            subscription: text(selection.subscription.as_ref().map(|name| name.as_str())),
            section: text(selection.section.as_ref().map(|name| name.as_str())),
            node: text(selection.node.as_ref().map(|name| name.as_str())),
            production,
            stale: from_default && default_stale,
        };
        let something =
            [&row.context, &row.namespace, &row.subscription, &row.section, &row.node].iter().any(|field| field.is_some());
        let state = match detection {
            Detection::NotFound => RowState::NotFound,
            Detection::SignedOut => RowState::SignedOut,
            Detection::Found => RowState::Found,
            Detection::Unknown if something => RowState::Found,
            Detection::Unknown => RowState::Unknown,
        };
        ContextRow { state, ..row }
    }
}

/// [`ContextState::summary`] as a function, for a caller that holds the state by reference.
pub fn summary(
    state: &ContextState,
    profiles: &IntegrationProfiles,
    tool: ToolId,
    pane: Option<PaneId>,
    detection: Detection,
) -> ContextRow {
    state.summary(profiles, tool, pane, detection)
}

// ---------------------------------------------------------------------------------------------------------------------
// Application: the variables a run needs and the directories they name.
// ---------------------------------------------------------------------------------------------------------------------

/// The longest path that becomes the value of a variable.
const PATH_MAX: usize = 4096;

/// Why a path of the machine-local file cannot become the value of a variable for `os`, as a fixed phrase that never
/// holds the path: it is not text, is empty or too long, holds a control character, is a network or device path (a
/// program on another computer or a device is not a configuration of this one), or is not an absolute path of the
/// system. On Windows only a path that starts with a drive letter, a colon and a separator is absolute.
pub fn unsafe_path_reason(os: Os, path: &Path) -> Option<&'static str> {
    let Some(text) = path.to_str() else { return Some("is not text") };
    if text.is_empty() {
        Some("is empty")
    } else if text.len() > PATH_MAX {
        Some("is longer than the limit")
    } else if text.chars().any(char::is_control) {
        Some("has a control character")
    } else if text.starts_with("\\\\") || text.starts_with("//") {
        Some("is a network or device path")
    } else if !platform::is_absolute_path(os, text) {
        Some("is not an absolute path")
    } else {
        None
    }
}

/// Where and for whom a selection is applied.
pub struct AppliedFor<'a> {
    /// The system the run is for.
    pub os: Os,
    /// The person's home directory, for the default kubeconfig that follows an overlay.
    pub home: &'a str,
    /// The data directory of the app: the only place anything is written, below `run` and `integrations`.
    pub data_dir: &'a Path,
    /// The pane the run is for. An overlay needs one.
    pub pane: Option<PaneId>,
    /// The environment of the person, read for `KUBECONFIG` only.
    pub user_env: &'a dyn Fn(&str) -> Option<String>,
    /// The contexts of the person's kubeconfig as read, to find the cluster and user names of the selected context.
    pub contexts: &'a [ContextEntry],
}

/// What to set for a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// The variables, as name and value. No name is one the runner denies.
    pub env: Vec<(String, String)>,
    /// Fixed sentences about what could not be applied. They hold no name, no path and no value.
    pub notes: Vec<String>,
    /// Isolation was asked for and could not be set up: the caller must not start the run, because a run without its
    /// own directory would use the person's default account.
    pub failed: bool,
}

/// What a selection needs in the environment of a run, per tool, and the directories those variables name.
///
/// - **Kubernetes** with `want_overlay` and a pane: the overlay of the selected context is written at
///   `<data>/run/kube/<pane>.json` (the cluster and user names come from `at.contexts`; the pane's namespace replaces
///   the one the context names) and `KUBECONFIG` is the overlay, then the person's files. Those are the configuration
///   paths of the machine-local file, else the person's own `KUBECONFIG`, else `<home>/.kube/config`. A context that
///   is not in `at.contexts` gets no overlay and a note. Without `want_overlay`, `KUBECONFIG` is the configuration
///   paths of the machine-local file joined, and no variable at all when it lists none.
/// - **Talos**: `TALOSCONFIG` is the first configuration path of the machine-local file, else nothing.
/// - **Azure** and **GAM**: with `isolated` and a profile, `AZURE_CONFIG_DIR` or `GAMCFGDIR` is
///   `<data>/integrations/<azure|gam>/<profile id>`, a directory of mode 0700 made here and never followed through a
///   link; if it cannot be made, `failed` is set and no variable is. Not isolated, the first configuration path of
///   the machine-local file, else nothing.
///
/// A path that is relative, empty, too long, has a control character or (on Windows) is a UNC or device path is
/// dropped with a note and no variable is set from it. Nothing is written outside `<data>/run` and
/// `<data>/integrations`, and nothing below a tool's own directory. Starting no process, reading no secret and
/// calling no Local verb is part of the contract.
pub fn apply_env(tool: ToolId, resolved: &Resolved, local: &LocalFile, at: &AppliedFor<'_>, want_overlay: bool) -> Applied {
    let mut out = Applied::default();
    match tool {
        ToolId::Kube => kube_env(resolved, local, at, want_overlay, &mut out),
        ToolId::Talos => {
            if let Some(path) = first_path(tool, local, at.os, &mut out.notes) {
                out.env.push(("TALOSCONFIG".to_owned(), path));
            }
        }
        ToolId::Azure => directory_env("AZURE_CONFIG_DIR", "azure", resolved, local, at, &mut out),
        ToolId::Gam => directory_env("GAMCFGDIR", "gam", resolved, local, at, &mut out),
    }
    // The runner refuses these names anyway; none of the variables above is one of them.
    out.env.retain(|(name, _)| !EnvPolicy::is_denied(name, at.os));
    out
}

const NOTE_PATH: &str = "a configuration path of the machine-local file cannot be used and is left out";
const NOTE_OVERLAY_NO_PANE: &str = "an overlay needs a pane, so none was written";
const NOTE_OVERLAY_NO_CONTEXT_LISTED: &str =
    "the selected context is not in the list that was read, so no overlay was written";
const NOTE_OVERLAY_FAILED: &str = "the overlay could not be written";
const NOTE_DATA_DIR: &str = "the data directory is not an absolute path of this system, so nothing was written there";
const NOTE_ISOLATION_NO_PROFILE: &str = "an isolated configuration needs a profile";
const NOTE_ISOLATION_FAILED: &str = "the private configuration directory could not be made, so the run is not started";

/// The configuration paths of the machine-local file for `tool` that pass [`unsafe_path_reason`] as text; the others
/// leave one note each.
fn local_paths(tool: ToolId, local: &LocalFile, os: Os, notes: &mut Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for path in local.config_paths(tool) {
        match (unsafe_path_reason(os, path), path.to_str()) {
            (None, Some(text)) => out.push(text.to_owned()),
            _ => notes.push(format!("{}: {NOTE_PATH}", tool.as_str())),
        }
    }
    out
}

fn first_path(tool: ToolId, local: &LocalFile, os: Os, notes: &mut Vec<String>) -> Option<String> {
    local_paths(tool, local, os, notes).into_iter().next()
}

fn kube_env(resolved: &Resolved, local: &LocalFile, at: &AppliedFor<'_>, want_overlay: bool, out: &mut Applied) {
    let separator = if at.os == Os::Windows { ";" } else { ":" };
    let paths = local_paths(ToolId::Kube, local, at.os, &mut out.notes);
    let own_list = if paths.is_empty() { None } else { Some(paths.join(separator)) };

    if want_overlay {
        if let Some(value) = overlay_value(resolved, at, own_list.as_deref(), &mut out.notes) {
            out.env.push(("KUBECONFIG".to_owned(), value));
            return;
        }
    }
    if let Some(list) = own_list {
        out.env.push(("KUBECONFIG".to_owned(), list));
    }
}

/// Writes the overlay and returns the value of `KUBECONFIG` that lists it first, or `None` with a note.
fn overlay_value(resolved: &Resolved, at: &AppliedFor<'_>, own_list: Option<&str>, notes: &mut Vec<String>) -> Option<String> {
    let Some(pane) = at.pane else {
        notes.push(NOTE_OVERLAY_NO_PANE.to_owned());
        return None;
    };
    // With no context selected the tool's own current context applies, and there is nothing to overlay.
    let context = resolved.selection.context.as_ref()?;
    let Some(entry) = at.contexts.iter().find(|entry| entry.name() == context.as_str()) else {
        notes.push(NOTE_OVERLAY_NO_CONTEXT_LISTED.to_owned());
        return None;
    };
    if unsafe_path_reason(at.os, at.data_dir).is_some() {
        notes.push(NOTE_DATA_DIR.to_owned());
        return None;
    }
    let namespace = resolved.selection.namespace.as_ref().map(|name| name.as_str()).or(entry.namespace());
    let wanted = ContextEntry::new(entry.name(), entry.cluster(), entry.user(), namespace);
    let path = match kube::write_overlay(at.data_dir, pane, &wanted) {
        Ok(path) => path,
        Err(_) => {
            notes.push(NOTE_OVERLAY_FAILED.to_owned());
            return None;
        }
    };
    let user_list = own_list.map(str::to_owned).or_else(|| (at.user_env)("KUBECONFIG")).filter(|list| !list.is_empty());
    let value = path
        .to_str()
        .and_then(|overlay| kube::kubeconfig_value(overlay, at.os, user_list.as_deref(), at.home).ok());
    if value.is_none() {
        notes.push(NOTE_OVERLAY_FAILED.to_owned());
    }
    value
}

fn directory_env(
    variable: &str,
    directory: &str,
    resolved: &Resolved,
    local: &LocalFile,
    at: &AppliedFor<'_>,
    out: &mut Applied,
) {
    if !resolved.isolated {
        let tool = resolved.tool;
        if let Some(path) = first_path(tool, local, at.os, &mut out.notes) {
            out.env.push((variable.to_owned(), path));
        }
        return;
    }
    let Some(profile) = &resolved.profile else {
        out.notes.push(NOTE_ISOLATION_NO_PROFILE.to_owned());
        out.failed = true;
        return;
    };
    if unsafe_path_reason(at.os, at.data_dir).is_some() {
        out.notes.push(NOTE_DATA_DIR.to_owned());
        out.failed = true;
        return;
    }
    // The profile id is `[a-z0-9_-]`, so it is one safe path component.
    let leaf = at.data_dir.join("integrations").join(directory).join(profile.as_str());
    match make_private(at.data_dir, &leaf).ok().and_then(|()| leaf.to_str().map(str::to_owned)) {
        Some(path) => out.env.push((variable.to_owned(), path)),
        None => {
            out.notes.push(NOTE_ISOLATION_FAILED.to_owned());
            out.failed = true;
        }
    }
}

/// Makes `leaf` and every directory between the data directory and it, each for its owner only. A link, or anything
/// that is not a directory, is refused: nothing is made or written through it.
fn make_private(data_dir: &Path, leaf: &Path) -> Result<(), ()> {
    fs::create_dir_all(data_dir).map_err(|_| ())?;
    let mut path = data_dir.to_path_buf();
    for component in leaf.strip_prefix(data_dir).map_err(|_| ())?.components() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => create_private(&path).map_err(|_| ())?,
            Err(_) => return Err(()),
        }
        narrow(&path)?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_private(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

/// Sets the mode to 0700 whatever the umask made of it, after looking again that it is a directory and no link.
#[cfg(unix)]
fn narrow(path: &Path) -> Result<(), ()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::symlink_metadata(path).map_err(|_| ())?;
    if !meta.is_dir() {
        return Err(());
    }
    if meta.permissions().mode() & 0o7777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| ())?;
    }
    Ok(())
}

/// The profile of a user on Windows decides who can read a directory below it; there is no mode to set.
#[cfg(not(unix))]
fn narrow(path: &Path) -> Result<(), ()> {
    if fs::symlink_metadata(path).map_err(|_| ())?.is_dir() { Ok(()) } else { Err(()) }
}
