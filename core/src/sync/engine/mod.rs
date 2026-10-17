//! The sync itself: which local files go to the server and back, and what happens when both sides changed.
//!
//! One run looks at every synced name. A side has *changed* when it differs from what the last run left behind (the local
//! content by its SHA-256, the server file by its ETag). Only one side changed: it is copied to the other. Both changed:
//! the higher version stamp wins (counter first, the writer's clock only to break a tie), and the version that lost is
//! kept: a lost server version in `backups/` on the server, a lost local version in `sync-conflicts/` next to the files
//! (never for the secrets, which are never written to a file). A download is staged and applied at the next start; a
//! live file the person changed after the download was staged is copied to `sync-conflicts/` first, and the first run
//! after that start lists it among the conflicts.
//!
//! A deletion is a version like any other: the device that deleted a theme or a widget uploads a *tombstone*, the sealed
//! payload [`TOMBSTONE`], and a device that finds one over a file it has not changed copies the file to `sync-conflicts/`
//! and removes it. Tombstones exist for the folders that hold one file per item (`themes/`, `widgets/`) and nowhere else;
//! an edit beats a delete on either side; a file that is merely not found counts as deleted, an error or a missing
//! folder never does; and a run sends at most [`MAX_DELETES_SENT`] and applies at most [`MAX_DELETES_APPLIED`].
//!
//! The name `layout.json` carries a *layout unit*: the layout together with the pane assignments that make sense on
//! another computer (see [`Unit`]). Pane ids are small per-device counters, so a layout adopted without the assignments
//! that belong to it would meet the other device's own pane files; an adoption replaces them as one step.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::crypto::{self, Kdf, Key, Stamp};
use super::webdav::{Backend, CONFLICT, Cond, safe_segment};
use crate::blocks::{self, BLOCKS_FILE, Block};
use crate::connections::PANES_FILE;
use crate::layout::PaneId;
use crate::persist::{read_limited, write_atomic};
use crate::profiles::Profiles;
use crate::secrets::SecretStore;
use crate::{Error, Result};

/// The folder on the server holding the encrypted files, and the one holding replaced versions.
const FILES: &str = "files";
const BACKUPS: &str = "backups";
const META: &str = "meta.json";
/// The most a synced file may weigh.
const MAX_FILE: u64 = 4 * 1024 * 1024;
/// The name of the virtual file that carries the connection secrets.
pub const SECRETS: &str = "secrets.json";
const FIXED: [&str; 5] = ["layout.json", "settings.json", "connections.json", "widget_notes.json", SECRETS];
/// The folders whose `<name>.json` files are synced one by one.
const DIRS: [&str; 2] = ["themes", "widgets"];
const STATE: &str = "sync_state.json";
/// The synced name whose payload is a layout unit.
const LAYOUT: &str = "layout.json";
/// The version of the layout unit, and the most entries one of its two maps may hold.
const UNIT_VERSION: u32 = 1;
const MAX_UNIT_ENTRIES: usize = 4096;
/// The most a pane file is read at, as everywhere else.
const PANE_FILE_LIMIT: u64 = 256 * 1024;
/// The longest connection id or block source a unit may name, and the longest block path.
const MAX_ID: usize = 64;
const MAX_BLOCK_PATH: usize = 4096;
/// The plaintext of a deleted file: a NUL byte and a fixed word. It is sealed like any file, so the blob format does not
/// change, and it is not valid JSON, so a client that does not know it sees an invalid theme or widget file and ignores it.
pub const TOMBSTONE: &[u8] = b"\0holzwave:deleted";
/// The most tombstones one run sends, and the most files one run removes because another device deleted them.
pub const MAX_DELETES_SENT: usize = 5;
pub const MAX_DELETES_APPLIED: usize = 20;

/// Whether a payload is the marker of a deleted file.
fn is_tombstone(plain: &[u8]) -> bool {
    plain == TOMBSTONE
}

/// Whether a deletion can be synced for `name`: only the folders that hold one file per item.
fn deletable(name: &str) -> bool {
    allowed(name) && name.split_once('/').is_some_and(|(dir, _)| DIRS.contains(&dir))
}

/// The folder (`themes` or `widgets`) of a deletable name.
fn folder_of(name: &str) -> &str {
    name.split_once('/').map_or(name, |(dir, _)| dir)
}

fn err(m: impl Into<String>) -> Error {
    Error::Io(io::Error::other(m.into()))
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

// ---- names -------------------------------------------------------------------------------------------------------

/// Whether `name` is a file the sync handles: a fixed one, or `themes/<safe>.json`.
pub fn allowed(name: &str) -> bool {
    FIXED.contains(&name)
        || name.split_once('/').is_some_and(|(dir, file)| {
            DIRS.contains(&dir) && file.strip_suffix(".json").is_some_and(|stem| safe_segment(stem) && !stem.contains('.'))
        })
}

/// The server's file name for a logical name: `themes/dark.json` is `themes.dark.json.hwx`, `widgets/a.json` is
/// `widgets.a.json.hwx`.
fn remote_file(name: &str) -> String {
    format!("{}.hwx", name.replace('/', "."))
}

fn logical_name(remote: &str) -> Option<String> {
    let stem = remote.strip_suffix(".hwx")?;
    let name = match DIRS.iter().find_map(|d| stem.strip_prefix(*d).and_then(|r| r.strip_prefix('.')).map(|rest| format!("{d}/{rest}"))) {
        Some(n) => n,
        None => stem.to_owned(),
    };
    allowed(&name).then_some(name)
}

// ---- the local side ----------------------------------------------------------------------------------------------

/// The local files of the sync.
pub trait Source: Send {
    /// The synced names that exist now.
    fn names(&self) -> Result<Vec<String>>;
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>>;
    fn write(&self, name: &str, bytes: &[u8]) -> Result<()>;
    /// Remove the local file of `name` (a name under `themes/` or `widgets/`) after a copy of it was kept; fails without
    /// removing anything when the copy cannot be written.
    fn delete(&self, name: &str) -> Result<()>;
    /// Whether the folder `folder` (`themes` or `widgets`) exists here. A missing folder is not a deletion of its files.
    fn folder_exists(&self, folder: &str) -> bool;
}

/// Where downloaded files wait until the next start. The running app owns its files (it rewrites the layout, the settings
/// and the connection list from memory), so a download must not land in them: [`promote_incoming`] moves it into place
/// before anything is read.
const INCOMING: &str = "sync-incoming";

/// The live hashes recorded when a download was first staged (inside `sync-incoming/`), and the names kept at the last
/// start (inside the data folder). Both are local state of the sync, read bounded; damaged content counts as empty.
const STAGE_BASE: &str = ".live-at-stage.json";
const KEPT: &str = "sync-kept.json";
const NOTE_LIMIT: u64 = 256 * 1024;
const NOTE_NAMES: usize = 256;

/// What [`promote_incoming_with_report`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Promoted {
    /// The names moved into place, sorted.
    pub applied: Vec<String>,
    /// The applied names whose live file had been changed since the download was staged and was copied first.
    pub kept: Vec<String>,
    /// The names left staged because the copy of the live file could not be written; the next start tries again.
    pub skipped: Vec<String>,
}

/// Move what the sync downloaded into place. Called once at start, before the configuration is read. Returns the names
/// that were applied. A file that is not a synced name is removed, not applied. A live file the person changed after
/// the download was staged is copied to `sync-conflicts/` first and comes back as a conflict of the next run; see
/// [`promote_incoming_with_report`].
pub fn promote_incoming(dir: &Path) -> Vec<String> {
    promote_incoming_with_report(dir).applied
}

/// [`promote_incoming`] with what else happened: every staged name, see [`promote_where`].
pub fn promote_incoming_with_report(dir: &Path) -> Promoted {
    promote_where(dir, &|_| true)
}

/// Whether the running app leaves `name` alone: the themes and the widgets (it only reads them) and the note texts (one
/// writer, which shares a lock with the promote). The layout, the settings and the connections are rewritten from memory
/// by the running app, so they wait for the next start.
pub fn is_unowned(name: &str) -> bool {
    allowed(name) && (name == "widget_notes.json" || name.split_once('/').is_some_and(|(dir, _)| dir == "themes" || dir == "widgets"))
}

/// Move the staged files `wanted` accepts into place; every other staged file stays staged, with its record. For each
/// staged name the live file is compared with the hash recorded when the download was staged: a live file that changed
/// since (or that did not exist then) is written to `sync-conflicts/<name with / as .>.<time>` before the download
/// replaces it. When that copy cannot be written the name is skipped: the staged file and the live file stay as they
/// are. The kept names are noted in `sync-kept.json` for the next run. The staging folder is removed last, and only when
/// no staged name is left and nothing was skipped. `wanted` only ever sees synced names other than the secrets.
pub fn promote_where(dir: &Path, wanted: &dyn Fn(&str) -> bool) -> Promoted {
    let root = dir.join(INCOMING);
    let mut staged = Vec::new();
    let mut stack = vec![(root.clone(), String::new())];
    while let Some((path, prefix)) = stack.pop() {
        let Ok(read) = fs::read_dir(&path) else { continue };
        for entry in read.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            let name = format!("{prefix}{file}");
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() && prefix.is_empty() && DIRS.contains(&file.as_str()) {
                stack.push((entry.path(), format!("{file}/")));
            } else if kind.is_file() && allowed(&name) && name != SECRETS {
                staged.push((name, entry.path()));
            }
        }
    }
    staged.sort();
    let bases = load_bases(&root);
    let mut out = Promoted::default();
    let mut waiting = Vec::new();
    for (name, path) in staged {
        if !wanted(&name) {
            waiting.push(name);
            continue;
        }
        if name == LAYOUT {
            match adopt_layout(dir, &path, bases.get(&name)) {
                Adopted::Applied { edited } => {
                    if edited {
                        out.kept.push(name.clone());
                    }
                    out.applied.push(name);
                }
                Adopted::Skipped => out.skipped.push(name),
                Adopted::Refused => {
                    let _ = fs::remove_file(&path);
                }
            }
            continue;
        }
        let live_path = dir.join(&name);
        let incoming = read_limited(&path, MAX_FILE).ok().flatten().map(|b| digest(&b));
        let edited = match read_limited(&live_path, MAX_FILE) {
            Ok(None) => false,
            Ok(Some(live)) => {
                let hash = digest(&live);
                let changed = incoming.as_deref() != Some(hash.as_str())
                    && !matches!(bases.get(&name), Some(Some(base)) if *base == hash);
                if changed && !copy_edit(dir, &name, &live) {
                    out.skipped.push(name);
                    continue;
                }
                changed
            }
            // Present but unreadable (too large, not a file): it cannot be copied, so it is not replaced.
            Err(_) => {
                out.skipped.push(name);
                continue;
            }
        };
        if let Some(parent) = live_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::rename(&path, &live_path).is_ok() {
            if edited {
                out.kept.push(name.clone());
            }
            out.applied.push(name);
        } else {
            out.skipped.push(name);
        }
    }
    if !out.kept.is_empty() {
        let mut names = load_kept(dir);
        names.extend(out.kept.iter().cloned());
        names.sort();
        names.dedup();
        names.truncate(NOTE_NAMES);
        let note = serde_json::json!({ "kept": names });
        let _ = write_atomic(&dir.join(KEPT), &serde_json::to_vec_pretty(&note).unwrap_or_default());
    }
    if out.skipped.is_empty() && waiting.is_empty() {
        let _ = fs::remove_dir_all(&root);
    } else {
        // The staged files that wait keep their own base; the entries of what was applied go.
        let mut left = bases;
        left.retain(|name, _| out.skipped.contains(name) || waiting.contains(name));
        save_bases(&root, &left);
    }
    out
}

/// Write the live file `bytes` of `name` to `sync-conflicts/` before it is replaced. Whether that worked.
fn copy_edit(dir: &Path, name: &str, bytes: &[u8]) -> bool {
    let copy = dir.join("sync-conflicts").join(format!("{}.{}", name.replace('/', "."), now()));
    write_atomic(&copy, bytes).is_ok()
}

fn load_bases(root: &Path) -> BTreeMap<String, Option<String>> {
    let read = read_limited(&root.join(STAGE_BASE), NOTE_LIMIT).ok().flatten();
    let mut bases: BTreeMap<String, Option<String>> = read.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    bases.retain(|name, _| allowed(name) && name != SECRETS);
    while bases.len() > NOTE_NAMES {
        bases.pop_last();
    }
    bases
}

fn save_bases(root: &Path, bases: &BTreeMap<String, Option<String>>) {
    let _ = write_atomic(&root.join(STAGE_BASE), &serde_json::to_vec(bases).unwrap_or_default());
}

/// The names `sync-kept.json` holds: only allowed names, no duplicates, at most [`NOTE_NAMES`].
fn load_kept(dir: &Path) -> Vec<String> {
    #[derive(Deserialize)]
    struct Note {
        kept: Vec<String>,
    }
    let read = read_limited(&dir.join(KEPT), NOTE_LIMIT).ok().flatten();
    let mut names = read.and_then(|b| serde_json::from_slice::<Note>(&b).ok()).map(|n| n.kept).unwrap_or_default();
    names.retain(|n| allowed(n) && n != SECRETS);
    names.sort();
    names.dedup();
    names.truncate(NOTE_NAMES);
    names
}

/// The synced names that are staged in `root`.
fn promote_names(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for fixed in FIXED.iter().filter(|n| **n != SECRETS) {
        if root.join(fixed).is_file() {
            out.push((*fixed).to_owned());
        }
    }
    for d in DIRS {
        if let Ok(read) = fs::read_dir(root.join(d)) {
            for entry in read.flatten() {
                let name = format!("{d}/{}", entry.file_name().to_string_lossy());
                if allowed(&name) && entry.path().is_file() {
                    out.push(name);
                }
            }
        }
    }
    out
}

// ---- the layout unit ---------------------------------------------------------------------------------------------

/// The payload of the name `layout.json`. A layout names panes by small per-device counters, and what runs in a pane
/// (a saved connection, a file browser, the monitor) is kept in two machine-local files keyed by the same numbers. So the
/// layout travels with the part of those files that makes sense on another computer, and an adoption replaces both as
/// one step. The unit is built from the live files without changing them.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Unit {
    unit: u32,
    layout: serde_json::Value,
    /// Pane id to the id of a saved connection.
    #[serde(default)]
    connections: BTreeMap<String, String>,
    /// Pane id to its block; only blocks that [`blocks::portable`] accepts.
    #[serde(default)]
    blocks: BTreeMap<String, Block>,
}

/// What a downloaded `layout.json` is: a unit, or the plain layout an older build uploaded.
enum Download {
    Unit(Unit),
    Plain(serde_json::Value),
}

/// A connection id or a block source that names a saved connection.
fn named_id(s: &str) -> bool {
    s.len() <= MAX_ID && safe_segment(s)
}

/// A block the unit may hold: a source (this computer or a saved connection) and a bounded path.
fn block_fits(block: &Block) -> bool {
    !block.source.is_empty()
        && (block.source == blocks::LOCAL || named_id(&block.source))
        && block.path.len() <= MAX_BLOCK_PATH
        && !block.path.contains('\0')
}

/// A pane id as the app writes it: decimal digits without a sign or a leading zero, so one pane has one spelling.
fn is_pane_key(key: &str) -> bool {
    key.parse::<PaneId>().is_ok_and(|pane| pane.to_string() == key)
}

/// The value with every object's keys in sorted order, so the same content always gives the same bytes.
fn sorted(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let ordered: BTreeMap<String, Value> = map.into_iter().map(|(k, v)| (k, sorted(v))).collect();
            Value::Object(ordered.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// The two pane files of the data folder, each as the entries that are fit for a unit: a numeric pane key and a value
/// that passes the bounds. A missing or unreadable file is empty; one entry that does not fit is left out.
fn live_assignments(dir: &Path) -> (BTreeMap<String, String>, BTreeMap<String, Block>) {
    let read = |file: &str| -> serde_json::Map<String, serde_json::Value> {
        match read_limited(&dir.join(file), PANE_FILE_LIMIT).ok().flatten().and_then(|b| serde_json::from_slice(&b).ok()) {
            Some(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        }
    };
    let mut connections = BTreeMap::new();
    for (key, value) in read(PANES_FILE) {
        let (true, Some(id)) = (is_pane_key(&key), value.as_str().filter(|id| named_id(id))) else { continue };
        if connections.len() < MAX_UNIT_ENTRIES {
            connections.insert(key, id.to_owned());
        }
    }
    let mut shared = BTreeMap::new();
    for (key, value) in read(BLOCKS_FILE) {
        let (true, Ok(block)) = (is_pane_key(&key), serde_json::from_value::<Block>(value)) else { continue };
        if blocks::portable(&block) && block_fits(&block) && shared.len() < MAX_UNIT_ENTRIES {
            shared.insert(key, block);
        }
    }
    (connections, shared)
}

/// The unit of this device, as its canonical bytes (sorted keys, no whitespace), built from `layout.json` and the two
/// pane files. `None` when there is no layout. A layout that is not a JSON object is an error that names it: it would
/// be refused by every other device.
fn live_unit(dir: &Path) -> Result<Option<Vec<u8>>> {
    let Some(bytes) = read_limited(&dir.join(LAYOUT), MAX_FILE)? else { return Ok(None) };
    let layout: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| err(format!("{LAYOUT} is not valid JSON: {e}")))?;
    if !layout.is_object() {
        return Err(err(format!("{LAYOUT} is not a layout (not a JSON object)")));
    }
    let (connections, blocks) = live_assignments(dir);
    let unit = Unit { unit: UNIT_VERSION, layout: sorted(layout), connections, blocks };
    Ok(Some(serde_json::to_vec(&unit).map_err(|e| err(e.to_string()))?))
}

/// Check the bytes of a downloaded `layout.json` before anything is staged or applied: a unit of this version with
/// nothing but the members it names, a layout that is an object, numeric pane keys, safe connection ids and bounded
/// maps; or a plain layout object (no `unit` member). Anything else is refused.
fn check_download(bytes: &[u8]) -> Result<Download> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(err("the synced layout is too large"));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| err(format!("the synced layout is not valid JSON: {e}")))?;
    let Some(object) = value.as_object() else { return Err(err("the synced layout is not a JSON object")) };
    if !object.contains_key("unit") {
        return Ok(Download::Plain(value));
    }
    let unit: Unit = serde_json::from_value(value).map_err(|e| err(format!("the synced layout unit is not valid: {e}")))?;
    if unit.unit != UNIT_VERSION {
        return Err(err("the synced layout unit is of another version; update holzWave"));
    }
    if !unit.layout.is_object() {
        return Err(err("the synced layout unit holds a layout that is not an object"));
    }
    if unit.connections.len() > MAX_UNIT_ENTRIES || unit.blocks.len() > MAX_UNIT_ENTRIES {
        return Err(err("the synced layout unit holds too many pane assignments"));
    }
    for (pane, id) in &unit.connections {
        if !is_pane_key(pane) || !named_id(id) {
            return Err(err("the synced layout unit names a pane or a connection that is not valid"));
        }
    }
    for (pane, block) in &unit.blocks {
        if !is_pane_key(pane) || !block_fits(block) {
            return Err(err("the synced layout unit holds a block that is not valid"));
        }
    }
    Ok(Download::Unit(unit))
}

/// How the promote of `layout.json` ended.
enum Adopted {
    /// Applied; whether the live unit had been changed since the download was staged (its layout was copied first).
    Applied { edited: bool },
    /// Left staged, nothing live touched: a backup copy could not be written, or a write failed.
    Skipped,
    /// Not a unit or layout (a planted file): removed, not applied.
    Refused,
}

/// Whether the live pane file `file` holds something a replacement would lose: it exists, is not an empty object and
/// does not say what `target` says. `Err` when it cannot be read (so it cannot be copied either).
fn pane_file_needs_copy(dir: &Path, file: &str, target: &serde_json::Value) -> io::Result<Option<Vec<u8>>> {
    let Some(live) = read_limited(&dir.join(file), PANE_FILE_LIMIT)? else { return Ok(None) };
    let same = serde_json::from_slice::<serde_json::Value>(&live).is_ok_and(|v| v == *target || v.as_object().is_some_and(serde_json::Map::is_empty));
    Ok((!same).then_some(live))
}

/// Adopt the staged `layout.json` at `staged` as one step: copy what is replaced to `sync-conflicts/`, write the two pane
/// files, then the layout, then remove the staged file. The order makes a crash in between repeat the whole step at the
/// next start. A backup that cannot be written stops the adoption before anything live is touched.
fn adopt_layout(dir: &Path, staged: &Path, base: Option<&Option<String>>) -> Adopted {
    let Ok(Some(bytes)) = read_limited(staged, MAX_FILE) else { return Adopted::Refused };
    let Ok(download) = check_download(&bytes) else { return Adopted::Refused };
    let (layout, connections, blocks) = match download {
        Download::Unit(unit) => {
            // What a unit shares is what `portable` accepts, whoever built it.
            let shared: BTreeMap<String, Block> = unit.blocks.into_iter().filter(|(_, b)| blocks::portable(b)).collect();
            (unit.layout, unit.connections, shared)
        }
        Download::Plain(layout) => (layout, BTreeMap::new(), BTreeMap::new()),
    };
    let (Ok(connections_target), Ok(blocks_target)) = (serde_json::to_value(&connections), serde_json::to_value(&blocks)) else {
        return Adopted::Skipped;
    };
    // The live layout counts as edited like any synced file (see `promote_where`), judged on the unit it makes.
    let live_layout = match read_limited(&dir.join(LAYOUT), MAX_FILE) {
        Ok(live) => live,
        Err(_) => return Adopted::Skipped,
    };
    let live_hash = match live_layout {
        Some(_) => live_unit(dir).ok().flatten().map(|u| digest(&u)),
        None => None,
    };
    let edited = live_hash.as_deref().is_some_and(|hash| {
        hash != digest(&bytes) && !matches!(base, Some(Some(recorded)) if recorded == hash)
    });
    // Backups first: nothing live changes unless every copy is written.
    let mut copies: Vec<(String, Vec<u8>)> = Vec::new();
    for (file, target) in [(PANES_FILE, &connections_target), (BLOCKS_FILE, &blocks_target)] {
        match pane_file_needs_copy(dir, file, target) {
            Ok(Some(live)) => copies.push((file.to_owned(), live)),
            Ok(None) => {}
            Err(_) => return Adopted::Skipped,
        }
    }
    if edited {
        if let Some(live) = live_layout {
            copies.push((LAYOUT.to_owned(), live));
        }
    }
    for (name, live) in &copies {
        if !copy_edit(dir, name, live) {
            return Adopted::Skipped;
        }
    }
    let pretty = |v: &serde_json::Value| serde_json::to_vec_pretty(v).unwrap_or_default();
    let written = write_atomic(&dir.join(PANES_FILE), &pretty(&connections_target))
        .and_then(|()| write_atomic(&dir.join(BLOCKS_FILE), &pretty(&blocks_target)))
        .and_then(|()| write_atomic(&dir.join(LAYOUT), &pretty(&layout)))
        .and_then(|()| fs::remove_file(staged));
    if written.is_ok() { Adopted::Applied { edited } } else { Adopted::Skipped }
}

/// The configuration folder, plus the connection secrets as the virtual file [`SECRETS`]. Downloads are staged, see
/// [`promote_incoming`]; what is staged counts as the local version, and the live hash at staging is recorded.
pub struct ConfigSource {
    dir: PathBuf,
    secrets: Arc<dyn SecretStore>,
}

impl ConfigSource {
    pub fn new(dir: impl Into<PathBuf>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { dir: dir.into(), secrets }
    }

    /// Note the live hash of `name` the first time a download is staged for it (a second download before the start
    /// keeps the first value). No entry, or no room for one, means the next promote treats a differing live file as
    /// changed, which only ever costs a copy.
    fn record_live(&self, name: &str) {
        let root = self.dir.join(INCOMING);
        let mut bases = load_bases(&root);
        if bases.contains_key(name) || bases.len() >= NOTE_NAMES {
            return;
        }
        // The layout is judged as the unit it makes with the pane files, like everything the sync reads of it.
        let live = if name == LAYOUT {
            live_unit(&self.dir).ok().flatten()
        } else {
            read_limited(&self.dir.join(name), MAX_FILE).ok().flatten()
        };
        bases.insert(name.to_owned(), live.as_deref().map(digest));
        save_bases(&root, &bases);
    }

    fn accounts(&self) -> Vec<String> {
        Profiles::load(&self.dir).0.items.iter().map(|p| p.secret_account()).collect()
    }
}

impl Source for ConfigSource {
    fn names(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for name in FIXED.iter().filter(|n| **n != SECRETS) {
            if self.dir.join(name).is_file() {
                out.push((*name).to_owned());
            }
        }
        for d in DIRS {
            if let Ok(read) = fs::read_dir(self.dir.join(d)) {
                for entry in read.flatten() {
                    let name = format!("{d}/{}", entry.file_name().to_string_lossy());
                    if allowed(&name) && entry.path().is_file() {
                        out.push(name);
                    }
                }
            }
        }
        for staged in promote_names(&self.dir.join(INCOMING)) {
            if !out.contains(&staged) {
                out.push(staged);
            }
        }
        if !self.accounts().is_empty() {
            out.push(SECRETS.to_owned());
        }
        out.sort();
        Ok(out)
    }

    fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        if !allowed(name) {
            return Err(err("not a synced file"));
        }
        if name == SECRETS {
            let mut map = BTreeMap::new();
            for account in self.accounts() {
                if let Some(secret) = self.secrets.get(&account)? {
                    map.insert(account, secret);
                }
            }
            return Ok((!map.is_empty()).then(|| serde_json::to_vec(&map).unwrap_or_default()));
        }
        if let Some(staged) = read_limited(&self.dir.join(INCOMING).join(name), MAX_FILE)? {
            return Ok(Some(staged));
        }
        if name == LAYOUT {
            return live_unit(&self.dir);
        }
        Ok(read_limited(&self.dir.join(name), MAX_FILE)?)
    }

    fn write(&self, name: &str, bytes: &[u8]) -> Result<()> {
        if !allowed(name) {
            return Err(err("not a synced file"));
        }
        if name == SECRETS {
            let map: BTreeMap<String, String> =
                serde_json::from_slice(bytes).map_err(|e| err(format!("the synced secrets are damaged: {e}")))?;
            for (account, secret) in map {
                // Only the accounts of connection profiles: a hostile payload cannot reach any other entry.
                let id = account.strip_prefix("ssh:").filter(|id| safe_segment(id));
                if id.is_none() {
                    return Err(err("the synced secrets name an account that is not a connection"));
                }
                self.secrets.set(&account, &secret)?;
            }
            return Ok(());
        }
        if name == LAYOUT {
            // Staged verbatim, so it counts as the local version; refused before anything is staged when it is not a layout.
            check_download(bytes)?;
        }
        self.record_live(name);
        Ok(write_atomic(&self.dir.join(INCOMING).join(name), bytes)?)
    }

    fn delete(&self, name: &str) -> Result<()> {
        if !deletable(name) {
            return Err(err("the sync never deletes this file"));
        }
        let live = self.dir.join(name);
        // The copy comes first: when it cannot be written nothing is removed.
        if let Some(bytes) = read_limited(&live, MAX_FILE)? {
            if !copy_edit(&self.dir, name, &bytes) {
                return Err(err("a copy of the file could not be kept in sync-conflicts, so it was not removed"));
            }
        }
        match fs::remove_file(&live) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let root = self.dir.join(INCOMING);
        match fs::remove_file(root.join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut bases = load_bases(&root);
        if bases.remove(name).is_some() {
            save_bases(&root, &bases);
        }
        Ok(())
    }

    fn folder_exists(&self, folder: &str) -> bool {
        DIRS.contains(&folder) && fs::metadata(self.dir.join(folder)).is_ok_and(|m| m.is_dir())
    }
}

// ---- what the last run left behind -------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Seen {
    counter: u64,
    etag: String,
    /// The hash of the content last in agreement; empty for a deleted name.
    hash: String,
    /// The name was deleted (the server holds a tombstone) and the local file is gone.
    #[serde(default, skip_serializing_if = "is_false")]
    deleted: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// This device, hex of 16 random bytes.
    device: String,
    files: BTreeMap<String, Seen>,
}

impl State {
    fn load(dir: &Path) -> Result<State> {
        let mut state: State = match read_limited(&dir.join(STATE), 1 << 20)? {
            Some(b) => serde_json::from_slice(&b).unwrap_or_default(),
            None => State::default(),
        };
        if unhex(&state.device).is_none_or(|d| d.len() != 16) {
            state.device = hex(&crypto::random::<16>()?);
        }
        Ok(state)
    }

    fn save(&self, dir: &Path) -> Result<()> {
        Ok(write_atomic(&dir.join(STATE), &serde_json::to_vec_pretty(self).unwrap_or_default())?)
    }

    fn device(&self) -> [u8; 16] {
        let mut d = [0u8; 16];
        if let Some(b) = unhex(&self.device) {
            d.copy_from_slice(&b);
        }
        d
    }
}

// ---- the server's meta file --------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct Meta {
    format: u8,
    m_kib: u32,
    t: u32,
    p: u32,
    salt: String,
    /// A known text sealed with the key, so a wrong passphrase is told at the start and not file by file.
    check: String,
}

const CHECK_TEXT: &[u8] = b"holzwave sync";

// ---- a run -------------------------------------------------------------------------------------------------------

/// What one run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub uploaded: Vec<String>,
    pub downloaded: Vec<String>,
    /// Names where both sides had changed; the version that lost is kept.
    pub conflicts: Vec<String>,
    /// A name and why it could not be synced; the others were.
    pub errors: Vec<(String, String)>,
    /// Files this device removed because another device deleted them (a copy is in `sync-conflicts/`).
    pub removed: Vec<String>,
    /// Files this device deleted, whose tombstones were sent.
    pub deleted: Vec<String>,
}

pub struct Syncer<B: Backend, S: Source> {
    backend: B,
    source: S,
    key: Key,
    dir: PathBuf,
    state: State,
    /// The directory for the copies of local versions that lost (`None`: keep none).
    conflicts_dir: Option<PathBuf>,
}

impl<B: Backend, S: Source> Syncer<B, S> {
    /// Open the sync folder on the server (creating it with a new key on the first use) and check the passphrase.
    /// `dir` is where `sync_state.json` lives.
    pub fn connect(backend: B, source: S, dir: &Path, passphrase: &str) -> Result<Self> {
        Self::connect_with(backend, source, dir, passphrase, Kdf::default())
    }

    /// [`Self::connect`] with the key derivation parameters a new folder is made with (an existing folder has its own).
    pub fn connect_with(backend: B, source: S, dir: &Path, passphrase: &str, kdf: Kdf) -> Result<Self> {
        backend.ensure("")?;
        backend.ensure(FILES)?;
        backend.ensure(BACKUPS)?;
        let state = State::load(dir)?;
        let key = match backend.get(META)? {
            Some((bytes, _)) => Self::open_meta(&bytes, passphrase)?,
            None => match Self::create_meta(&backend, passphrase, kdf)? {
                Some(key) => key,
                // Another device made it a moment ago.
                None => {
                    let (bytes, _) = backend.get(META)?.ok_or_else(|| err("the sync folder disappeared"))?;
                    Self::open_meta(&bytes, passphrase)?
                }
            },
        };
        Ok(Self { backend, source, key, dir: dir.to_path_buf(), state, conflicts_dir: Some(dir.join("sync-conflicts")) })
    }

    fn open_meta(bytes: &[u8], passphrase: &str) -> Result<Key> {
        let meta: Meta = serde_json::from_slice(bytes).map_err(|_| err("the sync folder has no valid meta.json"))?;
        if meta.format != crypto::FORMAT {
            return Err(err("the sync folder is of a newer format; update holzWave"));
        }
        let salt = unhex(&meta.salt).ok_or_else(|| err("the sync folder has a damaged salt"))?;
        let key = Key::derive(passphrase, &salt, Kdf { m_kib: meta.m_kib, t: meta.t, p: meta.p })?;
        let check = unhex(&meta.check).ok_or_else(|| err("the sync folder has a damaged check"))?;
        match key.open(META, &check) {
            Ok((_, plain)) if *plain == CHECK_TEXT => Ok(key),
            _ => Err(err("the passphrase is wrong for this sync folder")),
        }
    }

    /// `None` when another device created the folder first.
    fn create_meta(backend: &B, passphrase: &str, kdf: Kdf) -> Result<Option<Key>> {
        let salt: [u8; 16] = crypto::random()?;
        let key = Key::derive(passphrase, &salt, kdf)?;
        let stamp = Stamp { counter: 0, time: now(), device: [0; 16] };
        let check = key.seal(META, stamp, CHECK_TEXT)?;
        let meta = Meta { format: crypto::FORMAT, m_kib: kdf.m_kib, t: kdf.t, p: kdf.p, salt: hex(&salt), check: hex(&check) };
        match backend.put(META, &serde_json::to_vec(&meta).unwrap_or_default(), &Cond::Absent) {
            Ok(_) => Ok(Some(key)),
            Err(e) if e.to_string().contains(CONFLICT) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Bring both sides level.
    pub fn sync_once(&mut self) -> Result<Report> {
        let remote: BTreeMap<String, String> = self
            .backend
            .list(FILES)?
            .into_iter()
            .filter_map(|e| logical_name(&e.name).map(|n| (n, e.etag)))
            .collect();
        let mut names: Vec<String> = self.source.names()?;
        names.extend(remote.keys().cloned());
        // A local deletion is noticed even when the name is no longer among the local files.
        names.extend(self.state.files.iter().filter(|(n, s)| !s.deleted && deletable(n)).map(|(n, _)| n.clone()));
        names.sort();
        names.dedup();
        let kept = load_kept(&self.dir);
        let mut report = Report::default();
        for name in names {
            if let Err(e) = self.one(&name, remote.get(&name).cloned(), &mut report) {
                report.errors.push((name, e.to_string()));
            }
        }
        self.state.save(&self.dir)?;
        // The names a start kept before replacing them come back as conflicts, once.
        for name in kept {
            if !report.conflicts.contains(&name) {
                report.conflicts.push(name);
            }
        }
        let _ = fs::remove_file(self.dir.join(KEPT));
        Ok(report)
    }

    fn one(&mut self, name: &str, mut remote_etag: Option<String>, report: &mut Report) -> Result<()> {
        let seen = self.state.files.get(name).cloned();
        let local = self.source.read(name)?;
        let local_hash = local.as_deref().map(digest);
        let local_changed = local_hash.is_some() && local_hash.as_deref() != seen.as_ref().map(|s| s.hash.as_str());
        for _ in 0..3 {
            let remote_changed = remote_etag.is_some() && remote_etag.as_deref() != seen.as_ref().map(|s| s.etag.as_str());
            // The file is not found here, it was in agreement before and nobody changed it on the server: a deletion.
            if local.is_none() && !remote_changed && deletable(name) && seen.as_ref().is_some_and(|s| !s.deleted) {
                let Some(etag) = remote_etag.clone() else {
                    // The server has no such file any more either: nothing to keep in step.
                    self.state.files.remove(name);
                    return Ok(());
                };
                let base = seen.as_ref().map_or(0, |s| s.counter);
                match self.upload(name, TOMBSTONE, base + 1, Some(&etag)) {
                    Ok(new_etag) => {
                        self.remember_deleted(name, base + 1, &new_etag);
                        report.deleted.push(name.to_owned());
                        report.uploaded.push(name.to_owned());
                        return Ok(());
                    }
                    Err(e) if e.to_string().contains(CONFLICT) => {
                        remote_etag = self.fresh_etag(name)?;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }
            if !local_changed && !remote_changed {
                return Ok(());
            }
            // Fetch the server's version when it is needed to decide or to apply.
            let fetched = if remote_etag.is_some() && (remote_changed || local.is_none()) {
                match self.backend.get(&format!("{FILES}/{}", remote_file(name)))? {
                    Some((blob, etag)) => {
                        let (stamp, plain) = self.key.open(name, &blob)?;
                        Some((blob, etag, stamp, plain))
                    }
                    None => None,
                }
            } else {
                None
            };
            if remote_changed && fetched.is_none() {
                remote_etag = None; // gone between the listing and now: treat as absent
                continue;
            }
            let base = seen.as_ref().map_or(0, |s| s.counter);
            if let Some((_, etag, stamp, plain)) = &fetched {
                if is_tombstone(plain) {
                    // Another device deleted the file. A device that has none records it and creates nothing; one that has
                    // an unchanged file removes it after a copy.
                    if !deletable(name) {
                        return Err(err("a deletion for a file the sync never deletes"));
                    }
                    if local.is_some() && !local_changed {
                        self.source.delete(name)?;
                        report.removed.push(name.to_owned());
                    } else if local.is_some() {
                        return Err(err("a deletion met a changed file"));
                    }
                    self.remember_deleted(name, stamp.counter, etag);
                    return Ok(());
                }
            }
            match (&local, &fetched, local_changed) {
                // Only the server changed (or the local file is gone): take its version.
                (_, Some((_, etag, stamp, plain)), false) | (None, Some((_, etag, stamp, plain)), _) => {
                    self.source.write(name, plain)?;
                    self.remember(name, stamp.counter, etag, &digest(plain));
                    report.downloaded.push(name.to_owned());
                    return Ok(());
                }
                // Only the local file changed: send it.
                (Some(bytes), None, true) => {
                    match self.upload(name, bytes, base + 1, remote_etag.as_deref()) {
                        Ok(etag) => {
                            self.remember(name, base + 1, &etag, &digest(bytes));
                            report.uploaded.push(name.to_owned());
                            return Ok(());
                        }
                        Err(e) if e.to_string().contains(CONFLICT) => {
                            // Somebody wrote in between: look at the server again.
                            remote_etag = self.fresh_etag(name)?;
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
                // Both changed: the higher stamp wins, the other version is kept.
                (Some(bytes), Some((_, etag, stamp, plain)), true) if *bytes == **plain => {
                    // Both changed to the very same content (two devices that moved to the same unit): nothing to keep.
                    self.remember(name, stamp.counter, etag, &digest(plain));
                    return Ok(());
                }
                (Some(bytes), Some((blob, etag, stamp, plain)), true) => {
                    let mine = Stamp { counter: base + 1, time: now(), device: self.state.device() };
                    let theirs_wins = (stamp.counter, stamp.time) > (mine.counter, mine.time);
                    report.conflicts.push(name.to_owned());
                    if theirs_wins {
                        self.keep_local(name, bytes);
                        self.source.write(name, plain)?;
                        self.remember(name, stamp.counter, etag, &digest(plain));
                        report.downloaded.push(name.to_owned());
                        return Ok(());
                    }
                    self.backend.put(&format!("{BACKUPS}/{}", backup_file(name, stamp)), blob, &Cond::Any)?;
                    let counter = stamp.counter.max(base) + 1;
                    match self.upload(name, bytes, counter, Some(etag)) {
                        Ok(new_etag) => {
                            self.remember(name, counter, &new_etag, &digest(bytes));
                            report.uploaded.push(name.to_owned());
                            return Ok(());
                        }
                        Err(e) if e.to_string().contains(CONFLICT) => {
                            remote_etag = self.fresh_etag(name)?;
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
                _ => return Ok(()),
            }
        }
        Err(err("the server kept changing while syncing; try again"))
    }

    fn upload(&self, name: &str, bytes: &[u8], counter: u64, etag: Option<&str>) -> Result<String> {
        let stamp = Stamp { counter, time: now(), device: self.state.device() };
        let blob = self.key.seal(name, stamp, bytes)?;
        let cond = etag.map_or(Cond::Absent, |e| Cond::Match(e.to_owned()));
        self.backend.put(&format!("{FILES}/{}", remote_file(name)), &blob, &cond)
    }

    fn fresh_etag(&self, name: &str) -> Result<Option<String>> {
        let file = remote_file(name);
        Ok(self.backend.list(FILES)?.into_iter().find(|e| e.name == file).map(|e| e.etag))
    }

    fn remember(&mut self, name: &str, counter: u64, etag: &str, hash: &str) {
        self.state.files.insert(name.to_owned(), Seen { counter, etag: etag.to_owned(), hash: hash.to_owned(), deleted: false });
    }

    /// Note that `name` is deleted at the version `counter`/`etag`: no hash, the local file is gone.
    fn remember_deleted(&mut self, name: &str, counter: u64, etag: &str) {
        self.state.files.insert(name.to_owned(), Seen { counter, etag: etag.to_owned(), hash: String::new(), deleted: true });
    }

    /// Keep a local version that lost. Never for the secrets: they must not be written to a file.
    fn keep_local(&self, name: &str, bytes: &[u8]) {
        if name == SECRETS {
            return;
        }
        if let Some(dir) = &self.conflicts_dir {
            let _ = write_atomic(&dir.join(format!("{}.{}", name.replace('/', "."), now())), bytes);
        }
    }
}

fn backup_file(name: &str, stamp: &Stamp) -> String {
    format!("{}.{}.{}.hwx", name.replace('/', "."), stamp.counter, hex(&stamp.device[..4]))
}

#[cfg(test)]
mod tests;
