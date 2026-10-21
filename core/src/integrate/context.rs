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
//! No process is started and no tool is asked in this module: the defaults of a tool (its own current selection) are
//! stored by the service when it has read them, and this module is state, resolution and application. A profile's
//! secret values never pass through it; it reports the names only.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::args::{ArgError, ProfileId};
use super::profiles::{IntegrationProfile, IntegrationProfiles};
use super::{azure, gam, kube, talos};
use super::{CheckedSelection, IntegrationSpec, Output, SelectKind, Selection, ToolId, VerbEffect};
use crate::layout::PaneId;
use crate::persist::{backup_bad, bad_path, read_limited, write_atomic};

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
    out.selection = checked;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------------------
// The state.
// ---------------------------------------------------------------------------------------------------------------------

/// The description of a tool.
fn spec_of(tool: ToolId) -> &'static IntegrationSpec {
    match tool {
        ToolId::Kube => &kube::SPEC,
        ToolId::Talos => &talos::SPEC,
        ToolId::Azure => &azure::SPEC,
        ToolId::Gam => &gam::SPEC,
    }
}

/// Whether `view` is a verb of `tool` that reads and shows a view.
fn is_view(tool: ToolId, view: &str) -> bool {
    spec_of(tool).verb(view).is_some_and(|verb| verb.effect == VerbEffect::Read && verb.output == Output::View)
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
