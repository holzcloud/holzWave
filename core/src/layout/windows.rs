//! Several windows: one [`Layout`] per window label under one set, and panes that travel between them.
//!
//! The terminal behind a pane does not know which window shows it, so a move between windows is a change of the
//! layouts only: the pane id stays, and with it the shell, the name and the scrollback. The set keeps three
//! promises, and [`WindowSet::check_invariants`] states them for the tests:
//!
//! - **A pane lives in exactly one window.** Pane ids (and with them tab, workspace and split ids) come from one
//!   counter that the set shares with its layouts, so a pane id is never handed out twice, in one window or in two.
//! - **A window is never empty.** The last pane of a window that leaves or closes takes the window with it. The last
//!   pane of the last window is the end of the app and is never removed here.
//! - **A damaged document never panics.** [`WindowSet::from_json`] repairs what it can, drops what it cannot, and
//!   answers a fresh set (and `Reset`) when nothing is left. A layout file from before windows is one window.
//!
//! Every operation that moves something works on copies and replaces the originals only when the whole move
//! succeeded, so a refused move (a limit, an unknown id) changes nothing.
//!
//! Window labels are `main` and `window-<n>`: the characters the capability pattern `window-*` of the shell allows,
//! and nothing a file can use to widen it. `<n>` comes from the shared id counter, so a label is never used twice
//! (the shell may still be tearing down the window that had it).

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use serde_json::Value;

use super::{
    Axis, CloseGroup, CloseOutcome, Edge, ID_CEILING, Layout, LoadStatus, MAX_PANES, Node, PaneId, Tab, Workspace,
};

/// The label of the window the app starts with: the one of the window configuration of the shell.
pub const MAIN_WINDOW: &str = "main";
/// Most windows at once. Each one is a web view of its own, so this bounds what a damaged file can open.
pub const MAX_WINDOWS: usize = 16;
/// The prefix of every label but the first.
const LABEL_PREFIX: &str = "window-";
/// Room left below the id ceiling for what one operation may allocate; an operation past it is refused.
const ID_HEADROOM: u64 = 64;

/// Whether `label` may name a window: `main`, or `window-` and one to sixteen digits.
pub fn valid_window_label(label: &str) -> bool {
    label == MAIN_WINDOW
        || label
            .strip_prefix(LABEL_PREFIX)
            .is_some_and(|n| (1..=16).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Where a pane or a tab that comes from another window is put.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachTo {
    /// Onto a pane of the target window: a side splits it, the middle swaps the two panes.
    Pane { target: PaneId, edge: Edge },
    /// As the last tab of a workspace of the target window (the tab strip, or a workspace chip), shown afterwards.
    Tab { workspace: usize },
}

/// What a move between windows did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moved {
    /// The window the pane or tab came from.
    pub from: String,
    /// The window it is in now.
    pub to: String,
    /// The source window had nothing else and is gone.
    pub source_closed: bool,
}

/// A pane or a tab that now has a window of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TornOff {
    /// The label of the new window.
    pub label: String,
    /// The window it came from, which keeps the rest.
    pub from: String,
}

/// What [`WindowSet::close_pane`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetClose {
    /// The pane is gone from its window, which has other panes.
    Closed,
    /// The pane was the last of its window and the window is gone with it (there are other windows).
    WindowClosed(String),
    /// The pane is the only one of the only window; nothing changed. This is the end of the app.
    LastPane,
    /// No window holds this pane id; nothing changed (a stale event).
    Unknown,
}

/// What [`WindowSet::close_tab`] and [`WindowSet::close_workspace`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetCloseGroup {
    /// The panes are gone from their window, which has other panes.
    Closed(Vec<PaneId>),
    /// The tab or workspace held every pane of its window and the window is gone with them (there are other windows).
    WindowClosed(String, Vec<PaneId>),
    /// It holds every pane of the only window; nothing changed. This is the end of the app.
    LastPane,
    /// No such window, tab or workspace; nothing changed.
    Unknown,
}

/// A pane or a whole tab taken out of a layout, with the names of its panes.
struct Piece {
    tab: Tab,
    names: BTreeMap<PaneId, String>,
}

/// The layouts of all windows, in the order the windows were opened.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowSet {
    windows: Vec<(String, Layout)>,
    /// The next id for everything. Every layout of the set carries the same value (see `sync`).
    next_id: u64,
}

impl Layout {
    fn pane_count(&self) -> usize {
        self.workspaces.iter().flat_map(|w| &w.tabs).map(|t| t.panes().len()).sum()
    }

    /// Take `pane` out as a one-pane tab. The layout can be left without a workspace; callers work on a copy and
    /// look at it.
    fn extract_pane(&mut self, pane: PaneId) -> Option<Piece> {
        self.locate(pane)?;
        let tab_id = self.id();
        if !self.detach_pane(pane) {
            return None;
        }
        let names = self.pane_names.remove(&pane).map(|name| BTreeMap::from([(pane, name)])).unwrap_or_default();
        Some(Piece { tab: Tab { id: tab_id, title: "shell".into(), root: Node::Pane(pane), focus: pane }, names })
    }

    /// Take the tab `index` of the shown workspace out, with its panes' names. Same caveat as `extract_pane`.
    fn extract_tab(&mut self, index: usize) -> Option<Piece> {
        if index >= self.workspaces.get(self.active)?.tabs.len() {
            return None;
        }
        let tab = self.remove_tab_at(self.active, index);
        let mut names = BTreeMap::new();
        for pane in tab.panes() {
            if let Some(name) = self.pane_names.remove(&pane) {
                names.insert(pane, name);
            }
        }
        Some(Piece { tab, names })
    }

    /// Append the piece as the last tab of workspace `workspace` and show it. False when there is no such workspace.
    fn attach_tab(&mut self, workspace: usize, piece: Piece) -> bool {
        let Some(w) = self.workspaces.get_mut(workspace) else { return false };
        w.tabs.push(piece.tab);
        w.active = w.tabs.len() - 1;
        self.active = workspace;
        self.pane_names.extend(piece.names);
        true
    }

    /// Put the pane `new` where `old` is, in every tab that has it (the focus follows).
    fn replace_pane(&mut self, old: PaneId, new: PaneId) {
        for w in &mut self.workspaces {
            for t in &mut w.tabs {
                t.root.rename_leaf(old, new);
                if t.focus == old {
                    t.focus = new;
                }
            }
        }
    }
}

impl WindowSet {
    /// One window, `main`, with a fresh one-pane layout. Returns the set and that pane.
    pub fn new() -> (Self, PaneId) {
        let (layout, pane) = Layout::new();
        (WindowSet::from_layout(layout), pane)
    }

    /// A set of one window, `main`, holding `layout`: the layout file of before windows existed.
    pub fn from_layout(layout: Layout) -> Self {
        let next_id = layout.next_id;
        WindowSet { windows: vec![(MAIN_WINDOW.to_string(), layout)], next_id }
    }

    // ---- reading ---------------------------------------------------------------------------------------------

    /// How many windows there are. Never zero.
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    /// Always false: a set keeps at least one window. It exists for the convention that goes with `len`.
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// The labels, in the order the windows were opened.
    pub fn labels(&self) -> Vec<&str> {
        self.windows.iter().map(|(label, _)| label.as_str()).collect()
    }

    /// The layout of a window.
    pub fn layout(&self, label: &str) -> Option<&Layout> {
        self.windows.iter().find(|(l, _)| l == label).map(|(_, layout)| layout)
    }

    /// The layouts with their labels, in order.
    pub fn windows(&self) -> impl Iterator<Item = (&str, &Layout)> {
        self.windows.iter().map(|(label, layout)| (label.as_str(), layout))
    }

    /// The label of the window that holds `pane`.
    pub fn window_of(&self, pane: PaneId) -> Option<&str> {
        self.windows.iter().find(|(_, layout)| layout.locate(pane).is_some()).map(|(label, _)| label.as_str())
    }

    /// Where a pane is: its window and the indexes of its workspace and tab there. The page names a tab by any pane of
    /// it, so a gesture between windows carries only pane ids; this turns one into the indexes the tab moves use.
    pub fn locate(&self, pane: PaneId) -> Option<(&str, usize, usize)> {
        self.windows
            .iter()
            .find_map(|(label, layout)| layout.locate(pane).map(|(workspace, tab)| (label.as_str(), workspace, tab)))
    }

    /// Every pane of every window, window by window.
    pub fn all_panes(&self) -> Vec<PaneId> {
        self.windows.iter().flat_map(|(_, layout)| layout.all_panes()).collect()
    }

    /// How many panes there are in all windows together.
    pub fn pane_count(&self) -> usize {
        self.windows.iter().map(|(_, layout)| layout.pane_count()).sum()
    }

    fn position(&self, label: &str) -> Option<usize> {
        self.windows.iter().position(|(l, _)| l == label)
    }

    fn index_of(&self, pane: PaneId) -> Option<usize> {
        self.windows.iter().position(|(_, layout)| layout.locate(pane).is_some())
    }

    // ---- editing one window ----------------------------------------------------------------------------------

    /// Change the layout of one window with `f`; the shared id counter is kept in step. `None` for an unknown label.
    ///
    /// For everything that stays inside one window (focus, rename, divider, tab moves, `Layout::move_pane`).
    /// Do not close panes through it: the last pane of a window is [`WindowSet::close_pane`]'s business, and the
    /// operations that add panes have the set-wide limit in the methods below.
    pub fn edit<T>(&mut self, label: &str, f: impl FnOnce(&mut Layout) -> T) -> Option<T> {
        let index = self.position(label)?;
        let result = f(&mut self.windows[index].1);
        self.sync();
        Some(result)
    }

    /// Split the focused pane of a window, as `Layout::split_focused`, with the pane limit counted over all windows.
    pub fn split_focused(&mut self, label: &str, axis: Axis) -> Option<PaneId> {
        if self.pane_count() >= MAX_PANES {
            return None;
        }
        self.edit(label, |layout| layout.split_focused(axis)).flatten()
    }

    /// A new tab in the shown workspace of a window, as `Layout::new_tab`, with the pane limit over all windows.
    pub fn new_tab(&mut self, label: &str) -> Option<PaneId> {
        if self.pane_count() >= MAX_PANES {
            return None;
        }
        self.edit(label, Layout::new_tab).flatten()
    }

    /// A new workspace in a window, as `Layout::new_workspace`, with the pane limit over all windows.
    pub fn new_workspace(&mut self, label: &str, name: &str) -> Option<PaneId> {
        if self.pane_count() >= MAX_PANES {
            return None;
        }
        self.edit(label, |layout| layout.new_workspace(name)).flatten()
    }

    // ---- closing ---------------------------------------------------------------------------------------------

    /// Close a pane in whichever window holds it. The last pane of a window closes the window, unless it is the
    /// last window: then nothing changes and the answer is [`SetClose::LastPane`] (the end of the app).
    pub fn close_pane(&mut self, pane: PaneId) -> SetClose {
        let Some(index) = self.index_of(pane) else { return SetClose::Unknown };
        let outcome = self.windows[index].1.close_pane(pane);
        self.sync();
        match outcome {
            CloseOutcome::Closed => SetClose::Closed,
            CloseOutcome::Unknown => SetClose::Unknown,
            CloseOutcome::LastPane if self.windows.len() > 1 => {
                let (label, _) = self.windows.remove(index);
                SetClose::WindowClosed(label)
            }
            CloseOutcome::LastPane => SetClose::LastPane,
        }
    }

    /// Close the tab at `index` of the shown workspace of a window, with all its panes: what [`WindowSet::close_pane`]
    /// does for each of them, but all or nothing. A tab that holds every pane of its window closes the window, unless
    /// it is the last window: then nothing changes and the answer is [`SetCloseGroup::LastPane`].
    pub fn close_tab(&mut self, label: &str, index: usize) -> SetCloseGroup {
        self.close_group(label, |layout| layout.close_tab(index))
    }

    /// Like [`WindowSet::close_tab`] for the workspace at `index` of a window.
    pub fn close_workspace(&mut self, label: &str, index: usize) -> SetCloseGroup {
        self.close_group(label, |layout| layout.close_workspace(index))
    }

    fn close_group(&mut self, label: &str, close: impl FnOnce(&mut Layout) -> CloseGroup) -> SetCloseGroup {
        let Some(index) = self.position(label) else { return SetCloseGroup::Unknown };
        let panes = self.windows[index].1.all_panes();
        let outcome = close(&mut self.windows[index].1);
        self.sync();
        match outcome {
            CloseGroup::Closed(panes) => SetCloseGroup::Closed(panes),
            CloseGroup::Unknown => SetCloseGroup::Unknown,
            CloseGroup::LastPane if self.windows.len() > 1 => {
                let (label, _) = self.windows.remove(index);
                SetCloseGroup::WindowClosed(label, panes)
            }
            CloseGroup::LastPane => SetCloseGroup::LastPane,
        }
    }

    /// Take a window away with all its panes and answer them (the caller ends their shells). `None`, and nothing
    /// changes, for an unknown label and for the last window.
    pub fn close_window(&mut self, label: &str) -> Option<Vec<PaneId>> {
        if self.windows.len() <= 1 {
            return None;
        }
        let index = self.position(label)?;
        let (_, layout) = self.windows.remove(index);
        Some(layout.all_panes())
    }

    // ---- moving between windows ------------------------------------------------------------------------------

    /// Give `pane` a window of its own (it becomes the one pane of a new window with one workspace and one tab).
    ///
    /// `None`, and nothing changes, for an unknown pane, for a pane that is alone in its window already (the result
    /// would be the same window under another label), and at [`MAX_WINDOWS`].
    pub fn tear_off_pane(&mut self, pane: PaneId) -> Option<TornOff> {
        if self.windows.len() >= MAX_WINDOWS || self.next_id >= ID_CEILING - ID_HEADROOM {
            return None;
        }
        let from = self.index_of(pane)?;
        let mut source = self.windows[from].1.clone();
        let piece = source.extract_pane(pane)?;
        if source.workspaces.is_empty() {
            return None;
        }
        Some(self.open_window(from, source, piece))
    }

    /// Give the tab `tab` of the shown workspace of `label` a window of its own, with its split tree, title and
    /// pane names. `None`, and nothing changes, for an unknown window or tab, for the only tab of the only
    /// workspace of the window, and at [`MAX_WINDOWS`].
    pub fn tear_off_tab(&mut self, label: &str, tab: usize) -> Option<TornOff> {
        if self.windows.len() >= MAX_WINDOWS || self.next_id >= ID_CEILING - ID_HEADROOM {
            return None;
        }
        let from = self.position(label)?;
        let mut source = self.windows[from].1.clone();
        let piece = source.extract_tab(tab)?;
        if source.workspaces.is_empty() {
            return None;
        }
        Some(self.open_window(from, source, piece))
    }

    /// Commit a tear off: `source` replaces the layout at `from` and a new window holds `piece`.
    fn open_window(&mut self, from: usize, mut source: Layout, piece: Piece) -> TornOff {
        let mut counter = source.next_id;
        let mut take = || {
            let id = counter;
            counter += 1;
            id
        };
        let (label_id, workspace_id) = (take(), take());
        let label = format!("{LABEL_PREFIX}{label_id}");
        let layout = Layout {
            workspaces: vec![Workspace { id: workspace_id, name: "main".into(), tabs: vec![piece.tab], active: 0 }],
            active: 0,
            next_id: counter,
            pane_names: piece.names,
        };
        source.next_id = counter;
        let origin = self.windows[from].0.clone();
        self.windows[from].1 = source;
        self.windows.push((label.clone(), layout));
        self.sync();
        TornOff { label, from: origin }
    }

    /// Move `pane` to another window: onto a pane of it (a side splits, the middle swaps the two panes) or as a tab.
    /// The moved pane is focused and shown there. The pane keeps its id, so its shell, name and scrollback go with
    /// it. A source window left without a pane closes ([`Moved::source_closed`]).
    ///
    /// `None`, and nothing changes, for an unknown pane or window, for a pane that is in `to` already (inside one
    /// window use `Layout::move_pane`), for a target that is not in `to`, and when the result would pass the pane
    /// or depth limit of the tab.
    pub fn move_pane_to(&mut self, pane: PaneId, to: &str, at: AttachTo) -> Option<Moved> {
        let from = self.index_of(pane)?;
        let into = self.position(to)?;
        if from == into || self.next_id >= ID_CEILING - ID_HEADROOM {
            return None;
        }
        match at {
            AttachTo::Pane { target, .. } if self.windows[into].1.locate(target).is_none() => return None,
            AttachTo::Tab { workspace } if workspace >= self.windows[into].1.workspaces.len() => return None,
            AttachTo::Pane { target, edge: Edge::Center } => return self.swap_across(from, into, pane, target),
            _ => {}
        }
        let (mut source, mut dest) = (self.windows[from].1.clone(), self.windows[into].1.clone());
        let Piece { tab, names } = source.extract_pane(pane)?;
        dest.next_id = dest.next_id.max(source.next_id);
        let placed = match at {
            AttachTo::Pane { target, edge } => {
                let placed = dest.insert_pane(pane, target, edge);
                dest.pane_names.extend(names);
                placed
            }
            AttachTo::Tab { workspace } => dest.attach_tab(workspace, Piece { tab, names }),
        };
        placed.then(|| self.commit_move(from, into, source, dest))
    }

    /// Move the tab `tab` of the shown workspace of `from` to the end of workspace `workspace` of `to` and show it
    /// there, with its split tree, title and pane names. A source window left without a pane closes.
    ///
    /// `None`, and nothing changes, for an unknown window, tab or workspace and for the same window twice (inside one
    /// window use `Layout::move_tab_to_workspace`).
    pub fn move_tab_to(&mut self, from: &str, tab: usize, to: &str, workspace: usize) -> Option<Moved> {
        let (source_at, dest_at) = (self.position(from)?, self.position(to)?);
        if source_at == dest_at
            || workspace >= self.windows[dest_at].1.workspaces.len()
            || self.next_id >= ID_CEILING - ID_HEADROOM
        {
            return None;
        }
        let (mut source, mut dest) = (self.windows[source_at].1.clone(), self.windows[dest_at].1.clone());
        let piece = source.extract_tab(tab)?;
        dest.next_id = dest.next_id.max(source.next_id);
        dest.attach_tab(workspace, piece).then(|| self.commit_move(source_at, dest_at, source, dest))
    }

    /// Two panes of two windows trade places; both keep their ids and names, the dragged pane `a` is focused in the
    /// window of `b` and `b` in the window of `a`. Neither window can end up empty.
    fn swap_across(&mut self, from: usize, into: usize, a: PaneId, b: PaneId) -> Option<Moved> {
        let (mut source, mut dest) = (self.windows[from].1.clone(), self.windows[into].1.clone());
        let (name_a, name_b) = (source.pane_names.remove(&a), dest.pane_names.remove(&b));
        source.replace_pane(a, b);
        dest.replace_pane(b, a);
        if let Some(name) = name_b {
            source.pane_names.insert(b, name);
        }
        if let Some(name) = name_a {
            dest.pane_names.insert(a, name);
        }
        source.focus_pane(b);
        dest.focus_pane(a);
        Some(self.commit_move(from, into, source, dest))
    }

    /// Replace the two layouts of a finished move; a source without a workspace is removed.
    fn commit_move(&mut self, from: usize, into: usize, source: Layout, dest: Layout) -> Moved {
        let (from_label, to_label) = (self.windows[from].0.clone(), self.windows[into].0.clone());
        self.windows[into].1 = dest;
        let source_closed = source.workspaces.is_empty();
        if source_closed {
            self.windows.remove(from);
        } else {
            self.windows[from].1 = source;
        }
        self.sync();
        Moved { from: from_label, to: to_label, source_closed }
    }

    /// Bring the counter of the set and of every layout to the largest of them.
    fn sync(&mut self) {
        let largest = self.windows.iter().map(|(_, layout)| layout.next_id).fold(self.next_id, u64::max);
        self.next_id = largest;
        for (_, layout) in &mut self.windows {
            layout.next_id = largest;
        }
    }

    // ---- the document ----------------------------------------------------------------------------------------

    /// The document as pretty printed JSON.
    ///
    /// A set of the one window `main` is written as the plain layout, byte for byte what `serde_json::to_vec_pretty`
    /// makes of that layout, so that a build without windows reads it and nothing changes for a user who never opens
    /// a second one. Any other set is `{"version": 2, "windows": [{"label": ..., "layout": ...}, ...]}`.
    pub fn to_json(&self) -> serde_json::Result<String> {
        if let [(label, layout)] = self.windows.as_slice()
            && label == MAIN_WINDOW
        {
            return serde_json::to_string_pretty(layout);
        }
        #[derive(Serialize)]
        struct Entry<'a> {
            label: &'a str,
            layout: &'a Layout,
        }
        #[derive(Serialize)]
        struct Document<'a> {
            version: u32,
            windows: Vec<Entry<'a>>,
        }
        let windows = self.windows.iter().map(|(label, layout)| Entry { label, layout }).collect();
        serde_json::to_string_pretty(&Document { version: 2, windows })
    }

    /// Read a persisted document: a layout of before windows (one window, `main`) or a version 2 document.
    ///
    /// Never fails and never panics. Text that is not JSON, or is a document with nothing usable in it, gives a fresh
    /// set and `Reset`. Otherwise each window is read on its own: one that is not a layout, cannot be repaired,
    /// repeats a pane of an earlier window, or is beyond [`MAX_WINDOWS`] or the pane limit is dropped, a label that
    /// is not valid or is used twice is replaced by a fresh one, and the layout rules of `Layout::from_json` apply
    /// to each layout. Everything that was done is a note of `Repaired`. A set without a window called `main` makes
    /// its first window `main`: the shell starts with the window of its configuration, and no note is made of it.
    pub fn from_json(text: &str) -> (WindowSet, LoadStatus) {
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(e) => return (WindowSet::new().0, LoadStatus::Reset(format!("not a layout: {e}"))),
        };
        let Some(list) = value.get("windows") else {
            let (layout, status) = Layout::from_json(text);
            return (WindowSet::from_layout(layout), status);
        };
        let Some(list) = list.as_array() else {
            return (WindowSet::new().0, LoadStatus::Reset("not a window list: windows is not a list".into()));
        };

        let mut notes = Vec::new();
        let mut kept: Vec<(String, Layout)> = Vec::new();
        let mut seen: HashSet<PaneId> = HashSet::new();
        let mut beyond = 0usize;
        for (i, entry) in list.iter().enumerate() {
            if kept.len() >= MAX_WINDOWS {
                beyond += 1;
                continue;
            }
            let Some(entry) = entry.as_object() else {
                notes.push(format!("window {i}: not an object, dropped"));
                continue;
            };
            let label = entry.get("label").and_then(Value::as_str).unwrap_or_default().to_string();
            let Some(layout) = entry.get("layout") else {
                notes.push(format!("window {i}: has no layout, dropped"));
                continue;
            };
            let mut layout: Layout = match serde_json::from_value(layout.clone()) {
                Ok(layout) => layout,
                Err(e) => {
                    notes.push(format!("window {i}: not a layout ({e}), dropped"));
                    continue;
                }
            };
            match layout.check() {
                Ok(repairs) => notes.extend(repairs.into_iter().map(|r| format!("window {i}: {r}"))),
                Err(reason) => {
                    notes.push(format!("window {i}: dropped, {reason}"));
                    continue;
                }
            }
            let panes = layout.all_panes();
            if panes.iter().any(|p| seen.contains(p)) {
                notes.push(format!("window {i}: dropped, it repeats a pane of an earlier window"));
                continue;
            }
            if seen.len() + panes.len() > MAX_PANES {
                notes.push(format!("window {i}: dropped, more than {MAX_PANES} panes in all windows"));
                continue;
            }
            seen.extend(panes);
            kept.push((label, layout));
        }
        if beyond > 0 {
            notes.push(format!("{beyond} window(s) beyond the {MAX_WINDOWS} window limit dropped"));
        }
        if kept.is_empty() {
            let reason = if notes.is_empty() { "no windows".to_string() } else { notes.join("; ") };
            return (WindowSet::new().0, LoadStatus::Reset(reason));
        }

        let mut set = WindowSet { windows: kept, next_id: 0 };
        set.sync();
        set.repair_labels(&mut notes);
        let status = if notes.is_empty() { LoadStatus::Clean } else { LoadStatus::Repaired(notes) };
        (set, status)
    }

    /// Labels: unusable ones (not valid, or the second use of one) are cleared, `main` goes to the first window when
    /// nobody has it, and every cleared one gets a fresh label.
    fn repair_labels(&mut self, notes: &mut Vec<String>) {
        let mut used: HashSet<String> = HashSet::new();
        let mut cleared = 0usize;
        for (label, _) in &mut self.windows {
            if !valid_window_label(label) || !used.insert(label.clone()) {
                label.clear();
                cleared += 1;
            }
        }
        if cleared > 0 {
            notes.push(format!("{cleared} window label(s) not valid or used twice were replaced"));
        }
        if !used.contains(MAIN_WINDOW) {
            // The first window takes the name; whatever it had before is free again.
            let first = &mut self.windows[0].0;
            used.remove(first.as_str());
            *first = MAIN_WINDOW.to_string();
            used.insert(MAIN_WINDOW.to_string());
        }
        for index in 0..self.windows.len() {
            if self.windows[index].0.is_empty() {
                let label = format!("{LABEL_PREFIX}{}", self.next_id);
                self.next_id += 1;
                self.windows[index].0 = label;
            }
        }
        self.sync();
    }

    /// Check what the set promises (see the module documentation); the reason on failure. For tests and for the
    /// shell's debug assertions, not for a hot path.
    pub fn check_invariants(&self) -> Result<(), String> {
        if self.windows.is_empty() || self.windows.len() > MAX_WINDOWS {
            return Err(format!("{} windows", self.windows.len()));
        }
        let mut labels = HashSet::new();
        let mut panes = HashSet::new();
        let mut total = 0usize;
        for (label, layout) in &self.windows {
            if !valid_window_label(label) || !labels.insert(label.as_str()) {
                return Err(format!("label {label:?} is not valid or is used twice"));
            }
            let repairs = layout.clone().check().map_err(|reason| format!("window {label}: {reason}"))?;
            if !repairs.is_empty() {
                return Err(format!("window {label} needs repairs: {}", repairs.join("; ")));
            }
            if layout.next_id != self.next_id {
                return Err(format!("window {label}: counter {} is not the set's {}", layout.next_id, self.next_id));
            }
            for pane in layout.all_panes() {
                total += 1;
                if !panes.insert(pane) {
                    return Err(format!("pane {pane} is in more than one place"));
                }
            }
        }
        if total > MAX_PANES {
            return Err(format!("{total} panes"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut panes: Vec<PaneId>) -> Vec<PaneId> {
        panes.sort_unstable();
        panes
    }

    fn ok(set: &WindowSet) {
        if let Err(reason) = set.check_invariants() {
            panic!("invariant broken: {reason}\n{set:#?}");
        }
    }

    /// A set with a `main` window of the panes `[1, 2, 3]` (one tab, two splits) and the pane of the second tab.
    fn busy() -> (WindowSet, Vec<PaneId>) {
        let (mut set, first) = WindowSet::new();
        let second = set.split_focused(MAIN_WINDOW, Axis::Row).unwrap();
        let third = set.split_focused(MAIN_WINDOW, Axis::Column).unwrap();
        let fourth = set.new_tab(MAIN_WINDOW).unwrap();
        (set, vec![first, second, third, fourth])
    }

    #[test]
    fn labels_are_main_or_window_and_digits_and_nothing_else() {
        for good in ["main", "window-1", "window-0000000000000042", "window-9999999999999999"] {
            assert!(valid_window_label(good), "{good}");
        }
        for bad in [
            "", "Main", "main ", "main-2", "window", "window-", "window-x", "window-1x", "window--1", "window-17000000000000000",
            "window-*", "*", "windows-1", "window-١٢", "main\n", "../main", "window-1/2",
        ] {
            assert!(!valid_window_label(bad), "{bad:?}");
        }
    }

    #[test]
    fn locate_names_the_window_workspace_and_tab_of_a_pane() {
        let (mut set, panes) = busy();
        set.edit(MAIN_WINDOW, |l| l.new_workspace("two"));
        let torn = set.tear_off_pane(panes[3]).unwrap();
        assert_eq!(set.locate(panes[0]), Some((MAIN_WINDOW, 0, 0)));
        assert_eq!(set.locate(panes[2]), Some((MAIN_WINDOW, 0, 0)), "any pane of a tab names the tab");
        assert_eq!(set.locate(panes[3]), Some((torn.label.as_str(), 0, 0)));
        assert_eq!(set.locate(9999), None);
        let extra = set.layout(MAIN_WINDOW).unwrap().workspaces[1].tabs[0].panes()[0];
        assert_eq!(set.locate(extra), Some((MAIN_WINDOW, 1, 0)));
    }

    #[test]
    fn a_new_set_is_one_main_window_with_one_pane() {
        let (set, pane) = WindowSet::new();
        assert_eq!(set.labels(), [MAIN_WINDOW]);
        assert_eq!(set.all_panes(), vec![pane]);
        assert_eq!(set.window_of(pane), Some(MAIN_WINDOW));
        assert_eq!(set.window_of(pane + 99), None);
        ok(&set);
    }

    // ---- tearing off -----------------------------------------------------------------------------------------

    #[test]
    fn tearing_a_pane_off_makes_a_window_of_its_own_and_the_pane_is_the_same_one() {
        let (mut set, panes) = busy();
        set.edit(MAIN_WINDOW, |l| l.rename_pane(panes[1], "build"));
        let before = sorted(set.all_panes());

        let torn = set.tear_off_pane(panes[1]).unwrap();
        ok(&set);
        assert_eq!(torn.from, MAIN_WINDOW);
        assert!(valid_window_label(&torn.label) && torn.label != MAIN_WINDOW, "{}", torn.label);
        assert_eq!(set.labels(), [MAIN_WINDOW, torn.label.as_str()]);
        assert_eq!(set.window_of(panes[1]), Some(torn.label.as_str()));
        assert_eq!(sorted(set.all_panes()), before, "no pane lost, none doubled");

        let new = set.layout(&torn.label).unwrap();
        assert_eq!(new.all_panes(), vec![panes[1]]);
        assert_eq!((new.workspaces.len(), new.workspace().tabs.len()), (1, 1));
        assert_eq!(new.pane_name(panes[1]), Some("build"), "the name goes with the pane");
        let old = set.layout(MAIN_WINDOW).unwrap();
        assert_eq!(old.pane_name(panes[1]), None);
        assert!(!old.all_panes().contains(&panes[1]));
    }

    #[test]
    fn a_pane_that_is_alone_in_its_window_is_not_torn_off() {
        let (mut set, pane) = WindowSet::new();
        assert_eq!(set.tear_off_pane(pane), None);
        let (mut set, _) = busy();
        let torn = set.tear_off_pane(set.layout(MAIN_WINDOW).unwrap().all_panes()[0]).unwrap();
        let before = set.clone();
        let alone = set.layout(&torn.label).unwrap().all_panes()[0];
        assert_eq!(set.tear_off_pane(alone), None);
        assert_eq!(set, before, "a refused tear off changes nothing");
    }

    #[test]
    fn an_unknown_pane_is_not_torn_off() {
        let (mut set, _) = busy();
        let before = set.clone();
        assert_eq!(set.tear_off_pane(9999), None);
        assert_eq!(set.tear_off_tab("window-77", 0), None);
        assert_eq!(set, before);
    }

    #[test]
    fn there_are_at_most_max_windows_windows() {
        let (mut set, _) = WindowSet::new();
        for _ in 1..MAX_WINDOWS {
            let pane = set.new_tab(MAIN_WINDOW).unwrap();
            set.tear_off_pane(pane).unwrap();
        }
        assert_eq!(set.len(), MAX_WINDOWS);
        ok(&set);
        let extra = set.new_tab(MAIN_WINDOW).unwrap();
        let before = set.clone();
        assert_eq!(set.tear_off_pane(extra), None);
        assert_eq!(set, before);
    }

    #[test]
    fn tearing_a_tab_off_keeps_its_splits_title_and_names() {
        let (mut set, panes) = busy();
        // The first tab holds three panes. Show it, name it and a pane of it.
        set.edit(MAIN_WINDOW, |l| {
            l.select_tab(0);
            l.rename_tab(0, "work");
            l.rename_pane(panes[2], "logs");
        });
        let tab = set.layout(MAIN_WINDOW).unwrap().tab().clone();
        let torn = set.tear_off_tab(MAIN_WINDOW, 0).unwrap();
        ok(&set);
        let new = set.layout(&torn.label).unwrap();
        assert_eq!(new.tab(), &tab, "the same tab: tree, ratios, focus, title");
        assert_eq!(new.pane_name(panes[2]), Some("logs"));
        assert_eq!(sorted(new.all_panes()), sorted(vec![panes[0], panes[1], panes[2]]));
        assert_eq!(set.layout(MAIN_WINDOW).unwrap().all_panes(), vec![panes[3]]);
        assert_eq!(set.layout(MAIN_WINDOW).unwrap().pane_name(panes[2]), None);
    }

    #[test]
    fn the_only_tab_of_a_window_is_not_torn_off() {
        let (mut set, _) = WindowSet::new();
        assert_eq!(set.tear_off_tab(MAIN_WINDOW, 0), None);
        let (mut set, _) = busy();
        let before = set.clone();
        assert_eq!(set.tear_off_tab(MAIN_WINDOW, 5), None, "no such tab");
        assert_eq!(set, before);
    }

    #[test]
    fn the_last_tab_of_a_workspace_takes_the_workspace_along_but_not_the_window() {
        let (mut set, _) = WindowSet::new();
        let extra = set.new_workspace(MAIN_WINDOW, "two").unwrap();
        let torn = set.tear_off_tab(MAIN_WINDOW, 0).unwrap();
        ok(&set);
        assert_eq!(set.layout(MAIN_WINDOW).unwrap().workspaces.len(), 1);
        assert_eq!(set.layout(MAIN_WINDOW).unwrap().all_panes().len(), 1);
        assert_eq!(set.layout(&torn.label).unwrap().all_panes().len(), 1);
        assert_ne!(set.window_of(extra), None);
    }

    // ---- moving between windows ------------------------------------------------------------------------------

    /// Two windows: `main` with panes `a, b` (and a second tab `c`), and a torn off window with `d`.
    fn two_windows() -> (WindowSet, [PaneId; 4], String) {
        let (mut set, panes) = busy();
        let torn = set.tear_off_pane(panes[3]).unwrap();
        (set, [panes[0], panes[1], panes[2], panes[3]], torn.label)
    }

    #[test]
    fn a_pane_dropped_on_a_side_of_a_pane_of_another_window_splits_it_there() {
        for (edge, row, first) in
            [(Edge::Left, true, true), (Edge::Right, true, false), (Edge::Top, false, true), (Edge::Bottom, false, false)]
        {
            let (mut set, [a, b, c, d], other) = two_windows();
            set.edit(MAIN_WINDOW, |l| l.rename_pane(a, "moved"));
            let before = sorted(set.all_panes());
            let moved = set.move_pane_to(a, &other, AttachTo::Pane { target: d, edge }).unwrap();
            ok(&set);
            assert_eq!(moved, Moved { from: MAIN_WINDOW.into(), to: other.clone(), source_closed: false });
            assert_eq!(set.window_of(a), Some(other.as_str()));
            assert_eq!(sorted(set.all_panes()), before, "{edge:?}: no pane lost, none doubled");
            let dest = set.layout(&other).unwrap();
            assert_eq!(dest.tab().focus, a, "the moved pane is focused");
            assert_eq!(dest.pane_name(a), Some("moved"));
            let Node::Split { axis, a: x, b: y, .. } = &dest.tab().root else { panic!("split expected") };
            assert_eq!(*axis == Axis::Row, row);
            let (head, tail) = if first { (a, d) } else { (d, a) };
            assert!(matches!(**x, Node::Pane(p) if p == head) && matches!(**y, Node::Pane(p) if p == tail), "{edge:?}");
            assert_eq!(set.layout(MAIN_WINDOW).unwrap().pane_name(a), None);
            assert_eq!(sorted(set.layout(MAIN_WINDOW).unwrap().all_panes()), sorted(vec![b, c]));
        }
    }

    #[test]
    fn a_pane_dropped_on_the_middle_of_a_pane_of_another_window_swaps_the_two() {
        let (mut set, [a, b, c, d], other) = two_windows();
        set.edit(MAIN_WINDOW, |l| l.rename_pane(a, "alpha"));
        set.edit(&other, |l| l.rename_pane(d, "delta"));
        let before = sorted(set.all_panes());
        let moved = set.move_pane_to(a, &other, AttachTo::Pane { target: d, edge: Edge::Center }).unwrap();
        ok(&set);
        assert!(!moved.source_closed);
        assert_eq!(sorted(set.all_panes()), before);
        assert_eq!(set.window_of(a), Some(other.as_str()));
        assert_eq!(set.window_of(d), Some(MAIN_WINDOW));
        assert_eq!(set.layout(&other).unwrap().all_panes(), vec![a]);
        assert_eq!(sorted(set.layout(MAIN_WINDOW).unwrap().all_panes()), sorted(vec![d, b, c]));
        assert_eq!(set.layout(&other).unwrap().pane_name(a), Some("alpha"));
        assert_eq!(set.layout(MAIN_WINDOW).unwrap().pane_name(d), Some("delta"));
        assert_eq!(set.layout(MAIN_WINDOW).unwrap().pane_name(a), None);
        assert_eq!(set.layout(&other).unwrap().tab().focus, a);
        assert_eq!(set.layout(MAIN_WINDOW).unwrap().tab().focus, d);
    }

    #[test]
    fn a_pane_dropped_on_a_tab_strip_or_a_workspace_chip_becomes_a_shown_tab_there() {
        let (mut set, [a, _, _, d], other) = two_windows();
        let extra_ws = set.new_workspace(&other, "second").unwrap();
        set.edit(&other, |l| l.select_workspace(0));
        // The workspace chip of the second workspace.
        let moved = set.move_pane_to(a, &other, AttachTo::Tab { workspace: 1 }).unwrap();
        ok(&set);
        assert!(!moved.source_closed);
        let dest = set.layout(&other).unwrap();
        assert_eq!(dest.active, 1, "the workspace of the chip is shown");
        assert_eq!(dest.workspace().tabs.len(), 2);
        assert_eq!(dest.tab().panes(), vec![a], "the new tab is shown");
        assert_eq!(dest.workspaces[1].tabs[0].panes(), vec![extra_ws]);
        assert_eq!(dest.workspaces[0].tabs[0].panes(), vec![d]);
        // The tab strip of the shown workspace.
        let (mut set, [a, _, _, d], other) = two_windows();
        set.move_pane_to(a, &other, AttachTo::Tab { workspace: 0 }).unwrap();
        ok(&set);
        assert_eq!(set.layout(&other).unwrap().workspace().tabs.len(), 2);
        assert_eq!(set.layout(&other).unwrap().all_panes(), vec![d, a]);
    }

    #[test]
    fn moving_the_last_pane_of_a_window_closes_that_window() {
        let (mut set, [a, b, c, d], other) = two_windows();
        let before = sorted(set.all_panes());
        let moved = set.move_pane_to(d, MAIN_WINDOW, AttachTo::Pane { target: a, edge: Edge::Right }).unwrap();
        ok(&set);
        assert_eq!(moved, Moved { from: other.clone(), to: MAIN_WINDOW.into(), source_closed: true });
        assert_eq!(set.labels(), [MAIN_WINDOW]);
        assert_eq!(sorted(set.all_panes()), before);
        assert_eq!(set.window_of(d), Some(MAIN_WINDOW));
        assert_eq!(set.layout(MAIN_WINDOW).unwrap().tab().focus, d);
        let _ = (b, c);
    }

    #[test]
    fn the_last_window_of_the_set_can_be_the_destination_or_the_one_that_gets_the_last_tab_of_another() {
        let (mut set, [a, b, c, _], other) = two_windows();
        // Move every pane of main into the other window, one by one: main closes with the last of them.
        let target = set.layout(&other).unwrap().all_panes()[0];
        set.move_pane_to(a, &other, AttachTo::Pane { target, edge: Edge::Right }).unwrap();
        set.move_pane_to(b, &other, AttachTo::Tab { workspace: 0 }).unwrap();
        let last = set.move_pane_to(c, &other, AttachTo::Tab { workspace: 0 }).unwrap();
        ok(&set);
        assert!(last.source_closed);
        assert_eq!(set.labels(), [other.as_str()], "the surviving window is not called main, which is fine at run time");
        assert_eq!(set.all_panes().len(), 4);
    }

    #[test]
    fn a_refused_move_changes_nothing() {
        let (mut set, [a, _, _, d], other) = two_windows();
        let before = set.clone();
        assert_eq!(set.move_pane_to(9999, &other, AttachTo::Tab { workspace: 0 }), None, "unknown pane");
        assert_eq!(set.move_pane_to(a, "window-4242", AttachTo::Tab { workspace: 0 }), None, "unknown window");
        assert_eq!(set.move_pane_to(a, MAIN_WINDOW, AttachTo::Tab { workspace: 0 }), None, "the same window");
        assert_eq!(set.move_pane_to(a, &other, AttachTo::Tab { workspace: 3 }), None, "no such workspace");
        assert_eq!(set.move_pane_to(a, &other, AttachTo::Pane { target: 9999, edge: Edge::Left }), None, "no target");
        assert_eq!(
            set.move_pane_to(a, &other, AttachTo::Pane { target: a, edge: Edge::Left }),
            None,
            "a target that is in the source window"
        );
        assert_eq!(
            set.move_pane_to(a, &other, AttachTo::Pane { target: 9999, edge: Edge::Center }),
            None,
            "a swap with nothing"
        );
        assert_eq!(set.move_pane_to(d, MAIN_WINDOW, AttachTo::Pane { target: d, edge: Edge::Left }), None);
        assert_eq!(set, before);
    }

    #[test]
    fn a_move_that_would_pass_the_depth_limit_changes_nothing() {
        let (mut set, [a, ..], other) = two_windows();
        // A chain of splits as deep as the limit allows in the other window.
        let mut deepest = set.layout(&other).unwrap().all_panes()[0];
        while let Some(next) = set.split_focused(&other, Axis::Row) {
            deepest = next;
        }
        ok(&set);
        let before = set.clone();
        assert_eq!(set.move_pane_to(a, &other, AttachTo::Pane { target: deepest, edge: Edge::Left }), None);
        assert_eq!(set, before, "the source keeps the pane, the target has no extra split id taken");
        // A swap needs no room.
        assert!(set.move_pane_to(a, &other, AttachTo::Pane { target: deepest, edge: Edge::Center }).is_some());
        ok(&set);
    }

    #[test]
    fn a_tab_moves_to_another_window_with_its_tree_and_names() {
        let (mut set, [a, b, c, d], other) = two_windows();
        let e = set.new_tab(MAIN_WINDOW).unwrap();
        set.edit(MAIN_WINDOW, |l| {
            l.select_tab(0);
            l.rename_tab(0, "work");
            l.rename_pane(b, "logs");
        });
        let tab = set.layout(MAIN_WINDOW).unwrap().tab().clone();
        let before = sorted(set.all_panes());
        let moved = set.move_tab_to(MAIN_WINDOW, 0, &other, 0).unwrap();
        ok(&set);
        assert_eq!(moved, Moved { from: MAIN_WINDOW.into(), to: other.clone(), source_closed: false });
        let dest = set.layout(&other).unwrap();
        assert_eq!(dest.tab(), &tab);
        assert_eq!(dest.workspace().tabs.len(), 2);
        assert_eq!(dest.pane_name(b), Some("logs"));
        assert_eq!(set.layout(MAIN_WINDOW).unwrap().pane_name(b), None);
        assert_eq!(set.layout(MAIN_WINDOW).unwrap().all_panes(), vec![e]);
        assert_eq!(sorted(set.all_panes()), before);
        let _ = (a, c, d);
    }

    #[test]
    fn moving_the_only_tab_of_a_window_closes_that_window() {
        let (mut set, [.., d], other) = two_windows();
        let moved = set.move_tab_to(&other, 0, MAIN_WINDOW, 0).unwrap();
        ok(&set);
        assert!(moved.source_closed);
        assert_eq!(set.labels(), [MAIN_WINDOW]);
        assert_eq!(set.window_of(d), Some(MAIN_WINDOW));
        assert_eq!(set.layout(MAIN_WINDOW).unwrap().tab().panes(), vec![d]);
    }

    #[test]
    fn a_refused_tab_move_changes_nothing() {
        let (mut set, _, other) = two_windows();
        let before = set.clone();
        assert_eq!(set.move_tab_to(MAIN_WINDOW, 9, &other, 0), None);
        assert_eq!(set.move_tab_to(MAIN_WINDOW, 0, &other, 5), None);
        assert_eq!(set.move_tab_to(MAIN_WINDOW, 0, MAIN_WINDOW, 0), None);
        assert_eq!(set.move_tab_to("window-4242", 0, &other, 0), None);
        assert_eq!(set.move_tab_to(MAIN_WINDOW, 0, "window-4242", 0), None);
        assert_eq!(set, before);
    }

    // ---- ids -------------------------------------------------------------------------------------------------

    #[test]
    fn a_pane_created_after_a_tear_off_never_repeats_an_id_of_either_window() {
        let (mut set, [a, ..], other) = two_windows();
        let mut known: HashSet<PaneId> = set.all_panes().into_iter().collect();
        for round in 0..20 {
            let label = if round % 2 == 0 { MAIN_WINDOW } else { other.as_str() };
            let pane = set.new_tab(label).unwrap();
            assert!(known.insert(pane), "pane {pane} repeats an id");
            if round % 5 == 0 {
                let torn = set.tear_off_pane(pane).unwrap();
                assert!(valid_window_label(&torn.label));
            }
        }
        ok(&set);
        assert_eq!(known.len(), set.pane_count());
        assert!(set.window_of(a).is_some());
    }

    #[test]
    fn window_labels_are_not_reused_while_ids_only_go_up() {
        let (mut set, panes) = busy();
        let first = set.tear_off_pane(panes[1]).unwrap().label;
        let only = set.layout(&first).unwrap().all_panes()[0];
        // Closing that window by moving its pane back, then tearing it off again, gives another label.
        set.move_pane_to(only, MAIN_WINDOW, AttachTo::Tab { workspace: 0 }).unwrap();
        let second = set.tear_off_pane(only).unwrap().label;
        assert_ne!(first, second);
    }

    // ---- closing ---------------------------------------------------------------------------------------------

    #[test]
    fn closing_a_pane_of_a_window_with_others_changes_that_window_only() {
        let (mut set, [a, b, c, d], other) = two_windows();
        assert_eq!(set.close_pane(b), SetClose::Closed);
        ok(&set);
        assert_eq!(sorted(set.layout(MAIN_WINDOW).unwrap().all_panes()), sorted(vec![a, c]));
        assert_eq!(set.layout(&other).unwrap().all_panes(), vec![d]);
    }

    #[test]
    fn closing_the_last_pane_of_a_window_closes_the_window_and_no_other() {
        let (mut set, [a, b, c, d], other) = two_windows();
        assert_eq!(set.close_pane(d), SetClose::WindowClosed(other.clone()));
        ok(&set);
        assert_eq!(set.labels(), [MAIN_WINDOW]);
        assert_eq!(sorted(set.all_panes()), sorted(vec![a, b, c]), "the panes of the other window are untouched");
    }

    #[test]
    fn the_last_pane_of_the_last_window_stays_and_is_reported() {
        let (mut set, pane) = WindowSet::new();
        let before = set.clone();
        assert_eq!(set.close_pane(pane), SetClose::LastPane);
        assert_eq!(set, before);
        assert_eq!(set.close_pane(pane + 100), SetClose::Unknown);
    }

    #[test]
    fn a_pane_that_is_in_no_window_is_unknown_and_changes_nothing() {
        let (mut set, ..) = two_windows();
        let before = set.clone();
        assert_eq!(set.close_pane(424_242), SetClose::Unknown);
        assert_eq!(set, before);
    }

    #[test]
    fn closing_a_tab_of_a_window_changes_that_window_only() {
        let (mut set, [a, b, c, d], other) = two_windows();
        // main: its one tab holds a, b, c; a second tab keeps the window alive.
        let extra = set.new_tab(MAIN_WINDOW).unwrap();
        assert_eq!(set.close_tab(MAIN_WINDOW, 0), SetCloseGroup::Closed(vec![a, b, c]));
        ok(&set);
        assert_eq!(set.layout(MAIN_WINDOW).unwrap().all_panes(), vec![extra]);
        assert_eq!(set.layout(&other).unwrap().all_panes(), vec![d], "the other window is untouched");
    }

    #[test]
    fn closing_the_only_tab_of_a_window_closes_the_window_when_there_are_others() {
        let (mut set, [a, b, c, d], other) = two_windows();
        assert_eq!(set.close_tab(&other, 0), SetCloseGroup::WindowClosed(other.clone(), vec![d]));
        ok(&set);
        assert_eq!(set.labels(), [MAIN_WINDOW]);
        assert_eq!(sorted(set.all_panes()), sorted(vec![a, b, c]));
        // And the main window now, with all its panes, is the end of the app.
        let before = set.clone();
        assert_eq!(set.close_tab(MAIN_WINDOW, 0), SetCloseGroup::LastPane);
        assert_eq!(set, before);
    }

    #[test]
    fn closing_a_workspace_of_a_window_closes_the_window_when_it_was_all_of_it() {
        let (mut set, [a, b, c, d], other) = two_windows();
        let extra = set.new_workspace(MAIN_WINDOW, "two").unwrap();
        assert_eq!(set.close_workspace(MAIN_WINDOW, 0), SetCloseGroup::Closed(vec![a, b, c]));
        ok(&set);
        assert_eq!(set.layout(MAIN_WINDOW).unwrap().all_panes(), vec![extra]);
        assert_eq!(set.close_workspace(&other, 0), SetCloseGroup::WindowClosed(other.clone(), vec![d]));
        ok(&set);
        assert_eq!(set.close_workspace(MAIN_WINDOW, 0), SetCloseGroup::LastPane);
    }

    #[test]
    fn a_tab_or_workspace_that_is_not_there_closes_nothing() {
        let (mut set, ..) = two_windows();
        let before = set.clone();
        assert_eq!(set.close_tab("window-4242", 0), SetCloseGroup::Unknown);
        assert_eq!(set.close_tab(MAIN_WINDOW, 9), SetCloseGroup::Unknown);
        assert_eq!(set.close_workspace("window-4242", 0), SetCloseGroup::Unknown);
        assert_eq!(set.close_workspace(MAIN_WINDOW, 9), SetCloseGroup::Unknown);
        assert_eq!(set, before);
    }

    #[test]
    fn closing_a_window_answers_its_panes_and_never_closes_the_last_window() {
        let (mut set, [a, b, c, d], other) = two_windows();
        assert_eq!(set.close_window("window-4242"), None);
        assert_eq!(set.close_window(&other), Some(vec![d]));
        ok(&set);
        assert_eq!(sorted(set.all_panes()), sorted(vec![a, b, c]));
        assert_eq!(set.close_window(MAIN_WINDOW), None, "the last window is the end of the app, not a window close");
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn the_pane_limit_counts_all_windows_together() {
        let (mut set, _) = WindowSet::new();
        let mut label = MAIN_WINDOW.to_string();
        let mut windows = 1;
        while set.pane_count() < MAX_PANES {
            // Fill one tab after the other; every 8th pane goes to a window of its own to spread the panes.
            let pane = set.new_tab(&label).expect("below the limit");
            if set.pane_count() % 32 == 0 && windows < MAX_WINDOWS {
                label = set.tear_off_pane(pane).unwrap().label;
                windows += 1;
            }
        }
        ok(&set);
        assert_eq!(set.pane_count(), MAX_PANES);
        assert_eq!(set.new_tab(MAIN_WINDOW), None);
        assert_eq!(set.new_workspace(&label, "x"), None);
        assert_eq!(set.split_focused(&label, Axis::Row), None);
        assert_eq!(set.new_tab("window-4242"), None);
        assert_eq!(set.pane_count(), MAX_PANES);
    }

    // ---- an invariant loop -----------------------------------------------------------------------------------

    /// Small deterministic generator (no dependency): xorshift.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self, below: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % below as u64) as usize
        }
    }

    #[test]
    fn a_long_random_sequence_never_loses_or_doubles_a_pane_and_keeps_every_invariant() {
        let (mut torn, mut moved, mut window_closes) = (0usize, 0usize, 0usize);
        for seed in 1..=8u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let (mut set, first) = WindowSet::new();
            let mut model: HashSet<PaneId> = HashSet::from([first]);
            for step in 0..400 {
                let labels: Vec<String> = set.labels().iter().map(|l| l.to_string()).collect();
                let label = labels[rng.next(labels.len())].clone();
                let panes = set.all_panes();
                let pane = panes[rng.next(panes.len())];
                let other_label = labels[rng.next(labels.len())].clone();
                let edge = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom, Edge::Center][rng.next(5)];
                let target = panes[rng.next(panes.len())];
                match rng.next(11) {
                    0 => {
                        if let Some(p) = set.split_focused(&label, [Axis::Row, Axis::Column][rng.next(2)]) {
                            assert!(model.insert(p), "step {step}: pane {p} repeats an id");
                        }
                    }
                    1 => {
                        if let Some(p) = set.new_tab(&label) {
                            assert!(model.insert(p), "step {step}: pane {p} repeats an id");
                        }
                    }
                    2 => {
                        if let Some(p) = set.new_workspace(&label, "w") {
                            assert!(model.insert(p), "step {step}: pane {p} repeats an id");
                        }
                    }
                    3 => torn += usize::from(set.tear_off_pane(pane).is_some()),
                    4 => {
                        let tabs = set.layout(&label).unwrap().workspace().tabs.len();
                        torn += usize::from(set.tear_off_tab(&label, rng.next(tabs + 1)).is_some());
                    }
                    5 | 6 => {
                        moved += usize::from(
                            set.move_pane_to(pane, &other_label, AttachTo::Pane { target, edge }).is_some(),
                        );
                    }
                    7 => {
                        let workspaces = set.layout(&other_label).unwrap().workspaces.len();
                        let at = AttachTo::Tab { workspace: rng.next(workspaces + 1) };
                        moved += usize::from(set.move_pane_to(pane, &other_label, at).is_some());
                    }
                    8 => {
                        let tabs = set.layout(&label).unwrap().workspace().tabs.len();
                        let workspaces = set.layout(&other_label).unwrap().workspaces.len();
                        let done = set.move_tab_to(&label, rng.next(tabs + 1), &other_label, rng.next(workspaces + 1));
                        moved += usize::from(done.is_some());
                    }
                    9 => {
                        let outcome = set.close_pane(pane);
                        if matches!(outcome, SetClose::Closed | SetClose::WindowClosed(_)) {
                            assert!(model.remove(&pane));
                        }
                        window_closes += usize::from(matches!(outcome, SetClose::WindowClosed(_)));
                    }
                    _ => {
                        set.edit(&label, |l| {
                            l.focus_pane(pane);
                            l.rename_pane(pane, "n");
                            l.move_pane(pane, target, edge);
                        });
                    }
                }
                if let Err(reason) = set.check_invariants() {
                    panic!("seed {seed} step {step}: {reason}");
                }
                assert_eq!(
                    sorted(set.all_panes()),
                    sorted(model.iter().copied().collect()),
                    "seed {seed} step {step}: the panes of the windows are not the panes that exist"
                );
                // Names never outlive their pane and never sit in a window that does not hold the pane.
                for (_, layout) in set.windows() {
                    for pane in layout.pane_names().keys() {
                        assert!(layout.locate(*pane).is_some(), "seed {seed} step {step}: name of a foreign pane");
                    }
                }
                // What was written and read again is the same set.
                if step % 25 == 0 {
                    let (back, status) = WindowSet::from_json(&set.to_json().unwrap());
                    assert_eq!(status, LoadStatus::Clean, "seed {seed} step {step}");
                    assert_eq!(sorted(back.all_panes()), sorted(set.all_panes()));
                }
            }
        }
        // The loop is not vacuous: it tore off, moved between windows and closed windows many times.
        assert!(torn > 50 && moved > 100 && window_closes > 5, "{torn} tear offs, {moved} moves, {window_closes} window closes");
    }

    // ---- the document ----------------------------------------------------------------------------------------

    #[test]
    fn a_set_of_one_main_window_is_written_as_the_plain_layout() {
        let (set, _) = busy();
        let layout = set.layout(MAIN_WINDOW).unwrap();
        assert_eq!(set.to_json().unwrap().as_bytes(), serde_json::to_vec_pretty(layout).unwrap().as_slice());
    }

    #[test]
    fn a_layout_file_from_before_windows_loads_as_one_main_window_with_the_same_status() {
        let (mut layout, _) = Layout::new();
        layout.split_focused(Axis::Row);
        layout.new_tab();
        layout.rename_pane(1, "kept");
        let text = serde_json::to_string_pretty(&layout).unwrap();
        let (set, status) = WindowSet::from_json(&text);
        assert_eq!(status, LoadStatus::Clean);
        assert_eq!(set.labels(), [MAIN_WINDOW]);
        assert_eq!(set.layout(MAIN_WINDOW), Some(&layout));
        ok(&set);

        // The repairs of the old loader are reported the same way: a focus that is not in the tab.
        let broken = text.replacen("\"focus\": 5", "\"focus\": 77", 1);
        assert_ne!(broken, text);
        let (set, status) = WindowSet::from_json(&broken);
        assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
        assert_eq!(status, Layout::from_json(&broken).1);
        ok(&set);
    }

    #[test]
    fn old_files_that_are_damaged_load_as_a_fresh_set() {
        for text in ["", "{", "[]", "null", "42", "\"windows\"", "{\"workspaces\": []}", "{\"workspaces\": 3}"] {
            let (set, status) = WindowSet::from_json(text);
            assert!(matches!(status, LoadStatus::Reset(_)), "{text:?}: {status:?}");
            assert_eq!(set.len(), 1);
            ok(&set);
        }
    }

    #[test]
    fn several_windows_round_trip_in_order_with_labels_and_layouts() {
        let (mut set, [a, b, ..], other) = two_windows();
        set.edit(MAIN_WINDOW, |l| l.rename_pane(a, "kept"));
        let third = set.tear_off_pane(b).unwrap().label;
        let text = set.to_json().unwrap();
        assert!(text.contains("\"version\": 2"), "{text}");
        let (back, status) = WindowSet::from_json(&text);
        assert_eq!(status, LoadStatus::Clean);
        assert_eq!(back, set);
        assert_eq!(back.labels(), [MAIN_WINDOW, other.as_str(), third.as_str()]);
        ok(&back);
    }

    #[test]
    fn a_window_set_without_main_gives_main_to_its_first_window_without_a_note() {
        let (mut set, [.., d], other) = two_windows();
        set.move_pane_to(set.layout(MAIN_WINDOW).unwrap().all_panes()[0], &other, AttachTo::Tab { workspace: 0 });
        // Close main by moving everything out of it.
        for pane in set.layout(MAIN_WINDOW).map(Layout::all_panes).unwrap_or_default() {
            set.move_pane_to(pane, &other, AttachTo::Tab { workspace: 0 });
        }
        assert_eq!(set.labels(), [other.as_str()]);
        let third = {
            let p = set.new_tab(&other).unwrap();
            set.tear_off_pane(p).unwrap().label
        };
        let (back, status) = WindowSet::from_json(&set.to_json().unwrap());
        assert_eq!(status, LoadStatus::Clean);
        assert_eq!(back.labels(), [MAIN_WINDOW, third.as_str()]);
        assert_eq!(back.window_of(d), Some(MAIN_WINDOW));
        ok(&back);
    }

    #[test]
    fn a_single_window_that_is_not_called_main_is_written_as_a_document_and_read_as_main() {
        let (mut set, [.., d], other) = two_windows();
        set.close_window(MAIN_WINDOW);
        assert_eq!(set.labels(), [other.as_str()]);
        let text = set.to_json().unwrap();
        assert!(text.contains("\"windows\""), "a lone window with another name is a document");
        let (back, status) = WindowSet::from_json(&text);
        assert_eq!(status, LoadStatus::Clean);
        assert_eq!(back.labels(), [MAIN_WINDOW]);
        assert_eq!(back.window_of(d), Some(MAIN_WINDOW));
    }

    fn document(windows: &str) -> String {
        format!("{{\"version\": 2, \"windows\": {windows}}}")
    }

    fn layout_json(layout: &Layout) -> String {
        serde_json::to_string(layout).unwrap()
    }

    fn entry(label: &str, layout: &Layout) -> String {
        format!("{{\"label\": {}, \"layout\": {}}}", serde_json::to_string(label).unwrap(), layout_json(layout))
    }

    #[test]
    fn a_document_that_is_not_a_window_list_is_reset() {
        for text in [
            document("3"),
            document("{}"),
            document("null"),
            document("\"x\""),
            document("[]"),
            document("[3, null, \"x\", [], {}]"),
            document("[{\"label\": \"main\"}]"),
            document("[{\"label\": \"main\", \"layout\": 5}]"),
            document("[{\"label\": \"main\", \"layout\": {\"workspaces\": []}}]"),
        ] {
            let (set, status) = WindowSet::from_json(&text);
            assert!(matches!(status, LoadStatus::Reset(_)), "{text}: {status:?}");
            assert_eq!((set.len(), set.pane_count()), (1, 1));
            ok(&set);
        }
    }

    #[test]
    fn a_window_that_cannot_be_used_is_dropped_and_the_others_are_kept() {
        let (good, _) = Layout::new();
        let text = document(&format!("[7, {}, {{\"label\": \"window-9\"}}, {}]", entry("main", &good), "{\"label\": \"window-8\", \"layout\": {}}"));
        let (set, status) = WindowSet::from_json(&text);
        let LoadStatus::Repaired(notes) = status else { panic!("repaired expected") };
        assert!(notes.len() >= 3, "{notes:?}");
        assert_eq!(set.labels(), [MAIN_WINDOW]);
        ok(&set);
    }

    #[test]
    fn a_window_that_repeats_a_pane_of_an_earlier_window_is_dropped() {
        let (mut shared, _) = Layout::new();
        shared.split_focused(Axis::Row);
        let (also, _) = Layout::new(); // the same ids as `shared`: pane 3 is in both
        let text = document(&format!("[{}, {}]", entry("main", &shared), entry("window-7", &also)));
        let (set, status) = WindowSet::from_json(&text);
        assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
        assert_eq!(set.labels(), [MAIN_WINDOW]);
        assert_eq!(set.all_panes(), shared.all_panes());
        ok(&set);
    }

    #[test]
    fn labels_that_are_not_valid_or_repeat_are_replaced_with_fresh_ones() {
        let mut set = WindowSet::new().0;
        let a = set.new_tab(MAIN_WINDOW).unwrap();
        let b = set.new_tab(MAIN_WINDOW).unwrap();
        let wa = set.tear_off_pane(a).unwrap().label;
        let wb = set.tear_off_pane(b).unwrap().label;
        let text = set.to_json().unwrap().replace(&wa, "window-*").replace(&wb, "main");
        let (back, status) = WindowSet::from_json(&text);
        let LoadStatus::Repaired(notes) = status else { panic!("repaired expected") };
        assert!(notes.iter().any(|n| n.contains("label")), "{notes:?}");
        ok(&back);
        assert_eq!(back.len(), 3);
        assert_eq!(back.labels()[0], MAIN_WINDOW);
        assert!(back.labels().iter().all(|l| valid_window_label(l)));
        assert_eq!(back.pane_count(), set.pane_count());
        // A label nobody checked is never used to address a window: the wildcard is gone.
        assert!(!back.labels().contains(&"window-*"));
    }

    #[test]
    fn more_than_the_window_limit_and_more_than_the_pane_limit_are_cut_with_a_note() {
        // 20 windows with distinct panes: ids are made distinct by building one set and writing it.
        let (mut set, _) = WindowSet::new();
        for _ in 1..MAX_WINDOWS {
            let pane = set.new_tab(MAIN_WINDOW).unwrap();
            set.tear_off_pane(pane).unwrap();
        }
        let mut entries: Vec<String> = set.windows().map(|(l, layout)| entry(l, layout)).collect();
        // A seventeenth window with panes of its own.
        let mut extra = Layout::new().0;
        extra.next_id = 10_000;
        extra.workspaces[0].tabs[0].root = Node::Pane(9_000);
        extra.workspaces[0].tabs[0].focus = 9_000;
        extra.workspaces[0].id = 9_001;
        extra.workspaces[0].tabs[0].id = 9_002;
        entries.push(entry("window-9999", &extra));
        let (back, status) = WindowSet::from_json(&document(&format!("[{}]", entries.join(","))));
        assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
        assert_eq!(back.len(), MAX_WINDOWS);
        ok(&back);
        assert!(!back.all_panes().contains(&9_000));
    }

    #[test]
    fn ids_at_the_ceiling_are_not_loaded() {
        let (mut layout, _) = Layout::new();
        layout.next_id = ID_CEILING;
        let (set, status) = WindowSet::from_json(&document(&format!("[{}]", entry("main", &layout))));
        assert!(matches!(status, LoadStatus::Reset(_)), "{status:?}");
        ok(&set);
    }

    #[test]
    fn every_window_counter_is_raised_to_the_largest_so_that_a_new_id_is_new_everywhere() {
        let (mut one, _) = Layout::new();
        one.next_id = 500;
        let mut two = Layout::new().0;
        two.next_id = 20;
        two.workspaces[0].id = 1_000;
        two.workspaces[0].tabs[0].id = 1_001;
        two.workspaces[0].tabs[0].root = Node::Pane(1_002);
        two.workspaces[0].tabs[0].focus = 1_002;
        // `check` raises the second counter above its own largest id (1002); the set raises both above 1002.
        let (mut set, status) =
            WindowSet::from_json(&document(&format!("[{}, {}]", entry("main", &one), entry("window-3", &two))));
        assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
        ok(&set);
        let pane = set.new_tab("window-3").unwrap();
        assert!(pane > 1_002, "{pane}");
        let pane = set.new_tab(MAIN_WINDOW).unwrap();
        assert!(pane > 1_002, "{pane}");
        ok(&set);
    }

    #[test]
    fn damaged_text_never_panics_and_always_gives_a_set_that_holds_its_promises() {
        let (set, [a, ..], _) = two_windows();
        let mut set = set;
        set.edit(MAIN_WINDOW, |l| l.rename_pane(a, "name with \"quotes\" and \u{1b}[31m escapes"));
        let text = set.to_json().unwrap();
        // Every prefix of the document, cut at a character boundary.
        for end in (0..text.len()).filter(|e| text.is_char_boundary(*e)) {
            let (back, _) = WindowSet::from_json(&text[..end]);
            ok(&back);
        }
        // Every single character of the document replaced by one that breaks something.
        let chars: Vec<char> = text.chars().collect();
        for i in (0..chars.len()).step_by(3) {
            for bad in ['}', '0', '-', '"', '9', '[', 'x'] {
                let mut copy = chars.clone();
                copy[i] = bad;
                let (back, _) = WindowSet::from_json(&copy.iter().collect::<String>());
                ok(&back);
            }
        }
        // Numbers that are too large or negative where ids and indexes are.
        for (from, to) in [("\"id\": ", "\"id\": 9007199254740993"), ("\"active\": 0", "\"active\": 99"), ("\"focus\": ", "\"focus\": -1")] {
            let (back, _) = WindowSet::from_json(&text.replace(from, to));
            ok(&back);
        }
        // Nested too deep for the parser, and a very long label.
        let deep = format!("{}1{}", "[".repeat(1000), "]".repeat(1000));
        assert!(matches!(WindowSet::from_json(&deep).1, LoadStatus::Reset(_)));
        let long = text.replacen("\"window-", &format!("\"{}", "w".repeat(100_000)), 1);
        ok(&WindowSet::from_json(&long).0);
    }
}
