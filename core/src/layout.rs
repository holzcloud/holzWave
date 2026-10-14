//! The block layout: workspaces hold tabs, a tab holds a tree of panes split side by side or stacked.
//!
//! Pure data and geometry, with no terminal and no UI in it, so it is the same on every platform and
//! easy to test. The app persists it as JSON and asks it for rectangles in normalised 0..1 units.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

mod windows;

pub use windows::{
    AttachTo, MAIN_WINDOW, MAX_WINDOWS, Moved, SetClose, SetCloseGroup, TornOff, WindowSet, valid_window_label,
};

pub type PaneId = u64;
pub type SplitId = u64;

/// How a split divides its area.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    /// Two panes next to each other (a vertical divider).
    Row,
    /// Two panes on top of each other (a horizontal divider).
    Column,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Pane(PaneId),
    Split { id: SplitId, axis: Axis, ratio: f32, a: Box<Node>, b: Box<Node> },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// A draggable divider between two panes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Divider {
    pub split: SplitId,
    pub axis: Axis,
    /// The whole span of the split; the divider sits at `ratio` along it.
    pub area: Rect,
    pub ratio: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub id: u64,
    pub title: String,
    pub root: Node,
    pub focus: PaneId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: u64,
    pub name: String,
    pub tabs: Vec<Tab>,
    pub active: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub workspaces: Vec<Workspace>,
    pub active: usize,
    next_id: u64,
    /// The names the user gave to panes. A pane without an entry shows its program title. Left out of the file
    /// while empty, so a layout written before panes had names is the same file as one without names.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pane_names: BTreeMap<PaneId, String>,
}

const MIN_RATIO: f32 = 0.1;

/// Ids stay below this. The webview receives ids as JSON numbers, and a JavaScript number holds integers
/// exactly only up to 2^53, so a larger id would come back as a different pane. A file with ids at or above
/// the limit (and with it `u64::MAX`, which `next_id` could never be raised above) is not repaired.
const ID_CEILING: u64 = 1 << 53;

/// Most panes one tab may hold.
const MAX_PANES_PER_TAB: usize = 64;
/// Most panes the whole layout may hold; each pane is a shell with a thread, so this bounds the processes a
/// damaged file can start.
const MAX_PANES: usize = 256;
/// Deepest nesting of splits in one tab. The serialiser writes two JSON levels per split and serde_json
/// refuses documents nested deeper than 128, so a chain of repeated splits would stop loading near 60. Stopping
/// well below that keeps everything the operations produce loadable.
const MAX_SPLIT_DEPTH: usize = 32;
/// Longest workspace name or tab title, in characters.
const MAX_TEXT_CHARS: usize = 128;

/// Drop control characters (terminal escapes, newlines) and cut to `MAX_TEXT_CHARS` characters.
fn clean_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(MAX_TEXT_CHARS).collect()
}

/// A name typed by the user: cleaned, cut, and without the blanks at both ends. Blank text is the empty name.
fn clean_name(text: &str) -> String {
    clean_text(text).trim().to_string()
}

/// Keep the items `keep` accepts, in order. Returns them and the new index of the item that was at `active`,
/// or `None` when that item was dropped (or `active` was out of range).
fn retain_tracking<T>(items: Vec<T>, active: usize, mut keep: impl FnMut(&mut T) -> bool) -> (Vec<T>, Option<usize>) {
    let mut out = Vec::with_capacity(items.len());
    let mut new_active = None;
    for (i, mut item) in items.into_iter().enumerate() {
        if !keep(&mut item) {
            continue;
        }
        if i == active {
            new_active = Some(out.len());
        }
        out.push(item);
    }
    (out, new_active)
}

/// Where on a pane another pane was dropped: a side splits it there, the middle swaps the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
    Center,
}

/// What `Layout::close_pane` did. The three cases need different reactions: an unknown id is a stale event
/// and is ignored, the last pane ends the app, anything else changed the layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    /// The pane was removed (and its tab or workspace with it, when it was the last pane of that).
    Closed,
    /// The pane exists but is the only one left in the whole layout; nothing was changed.
    LastPane,
    /// No tab holds this pane id; nothing was changed.
    Unknown,
}

/// What `Layout::close_tab` and `Layout::close_workspace` did: the same three cases as [`CloseOutcome`], with the
/// panes that went in the first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseGroup {
    /// The panes were removed, in the order of the tab (or of the tabs of the workspace); the tab or workspace is
    /// gone, and what else is left follows the rules of `close_pane`.
    Closed(Vec<PaneId>),
    /// The tab or workspace holds every pane of the layout, so closing it would leave nothing; nothing was changed.
    LastPane,
    /// There is no such tab or workspace; nothing was changed.
    Unknown,
}

/// How `Layout::from_json` found the text it was given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadStatus {
    /// The text was a valid layout and is returned as it was.
    Clean,
    /// The text was a layout with defects; each line says what was repaired.
    Repaired(Vec<String>),
    /// The text could not be used; a fresh layout was returned. The string says why.
    Reset(String),
}

impl Node {
    pub fn panes(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Pane(p) => out.push(*p),
            Node::Split { a, b, .. } => {
                a.panes(out);
                b.panes(out);
            }
        }
    }

    fn contains(&self, pane: PaneId) -> bool {
        match self {
            Node::Pane(p) => *p == pane,
            Node::Split { a, b, .. } => a.contains(pane) || b.contains(pane),
        }
    }

    /// Number of splits between the root and `pane`, or `None` when the pane is not in the tree.
    fn depth_of(&self, pane: PaneId) -> Option<usize> {
        match self {
            Node::Pane(p) => (*p == pane).then_some(0),
            Node::Split { a, b, .. } => a.depth_of(pane).or_else(|| b.depth_of(pane)).map(|d| d + 1),
        }
    }

    /// Longest chain of splits in the tree.
    fn depth(&self) -> usize {
        match self {
            Node::Pane(_) => 0,
            Node::Split { a, b, .. } => 1 + a.depth().max(b.depth()),
        }
    }

    /// Replace every ratio that is not finite by 0.5 and clamp the rest. Returns how many were changed.
    fn sanitize_ratios(&mut self) -> usize {
        match self {
            Node::Pane(_) => 0,
            Node::Split { ratio, a, b, .. } => {
                let fixed = if ratio.is_finite() { ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO) } else { 0.5 };
                let changed = usize::from(fixed != *ratio);
                *ratio = fixed;
                changed + a.sanitize_ratios() + b.sanitize_ratios()
            }
        }
    }

    fn split_ids(&self, out: &mut Vec<SplitId>) {
        if let Node::Split { id, a, b, .. } = self {
            out.push(*id);
            a.split_ids(out);
            b.split_ids(out);
        }
    }

    fn split_pane(&mut self, pane: PaneId, axis: Axis, split_id: SplitId, new_pane: PaneId) -> bool {
        match self {
            Node::Pane(p) if *p == pane => {
                *self = Node::Split {
                    id: split_id,
                    axis,
                    ratio: 0.5,
                    a: Box::new(Node::Pane(pane)),
                    b: Box::new(Node::Pane(new_pane)),
                };
                true
            }
            Node::Pane(_) => false,
            Node::Split { a, b, .. } => {
                a.split_pane(pane, axis, split_id, new_pane) || b.split_pane(pane, axis, split_id, new_pane)
            }
        }
    }

    /// Like `split_pane`, but the new pane is `moved` and sits first (`before`) or last in the split.
    fn split_with(&mut self, target: PaneId, axis: Axis, split_id: SplitId, moved: PaneId, before: bool) -> bool {
        match self {
            Node::Pane(p) if *p == target => {
                let (old, new) = (Box::new(Node::Pane(target)), Box::new(Node::Pane(moved)));
                let (a, b) = if before { (new, old) } else { (old, new) };
                *self = Node::Split { id: split_id, axis, ratio: 0.5, a, b };
                true
            }
            Node::Pane(_) => false,
            Node::Split { a, b, .. } => {
                a.split_with(target, axis, split_id, moved, before) || b.split_with(target, axis, split_id, moved, before)
            }
        }
    }

    /// Give the leaf `from` the id `to`.
    fn rename_leaf(&mut self, from: PaneId, to: PaneId) {
        match self {
            Node::Pane(p) if *p == from => *p = to,
            Node::Pane(_) => {}
            Node::Split { a, b, .. } => {
                a.rename_leaf(from, to);
                b.rename_leaf(from, to);
            }
        }
    }

    /// Remove `pane`; its sibling takes the parent's place. Returns false if `self` is that pane.
    fn remove(&mut self, pane: PaneId) -> bool {
        if let Node::Split { a, b, .. } = self {
            if matches!(**a, Node::Pane(p) if p == pane) {
                *self = (**b).clone();
                return true;
            }
            if matches!(**b, Node::Pane(p) if p == pane) {
                *self = (**a).clone();
                return true;
            }
            return a.remove(pane) || b.remove(pane);
        }
        false
    }

    fn set_ratio(&mut self, split: SplitId, value: f32) -> bool {
        match self {
            Node::Pane(_) => false,
            Node::Split { id, ratio, a, b, .. } => {
                if *id == split {
                    *ratio = value.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
                    true
                } else {
                    a.set_ratio(split, value) || b.set_ratio(split, value)
                }
            }
        }
    }

    fn geometry(&self, r: Rect, panes: &mut Vec<(PaneId, Rect)>, dividers: &mut Vec<Divider>) {
        match self {
            Node::Pane(p) => panes.push((*p, r)),
            Node::Split { id, axis, ratio, a, b } => {
                let (ra, rb) = match axis {
                    Axis::Row => (
                        Rect { w: r.w * ratio, ..r },
                        Rect { x: r.x + r.w * ratio, w: r.w * (1.0 - ratio), ..r },
                    ),
                    Axis::Column => (
                        Rect { h: r.h * ratio, ..r },
                        Rect { y: r.y + r.h * ratio, h: r.h * (1.0 - ratio), ..r },
                    ),
                };
                dividers.push(Divider { split: *id, axis: *axis, area: r, ratio: *ratio });
                a.geometry(ra, panes, dividers);
                b.geometry(rb, panes, dividers);
            }
        }
    }
}

impl Tab {
    pub fn panes(&self) -> Vec<PaneId> {
        let mut v = Vec::new();
        self.root.panes(&mut v);
        v
    }

    /// Pane and divider rectangles, with the tab area as the unit square.
    pub fn geometry(&self) -> (Vec<(PaneId, Rect)>, Vec<Divider>) {
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        self.root.geometry(Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }, &mut panes, &mut dividers);
        (panes, dividers)
    }
}

impl Layout {
    /// One workspace with one tab with one pane. Returns the layout and that pane.
    pub fn new() -> (Self, PaneId) {
        let mut l = Layout { workspaces: Vec::new(), active: 0, next_id: 1, pane_names: BTreeMap::new() };
        let ws = l.id();
        let tab = l.id();
        let pane = l.id();
        l.workspaces.push(Workspace {
            id: ws,
            name: "main".into(),
            tabs: vec![Tab { id: tab, title: "shell".into(), root: Node::Pane(pane), focus: pane }],
            active: 0,
        });
        (l, pane)
    }

    fn id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspaces[self.active]
    }

    fn workspace_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active]
    }

    pub fn tab(&self) -> &Tab {
        let w = self.workspace();
        &w.tabs[w.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        let w = self.workspace_mut();
        let i = w.active;
        &mut w.tabs[i]
    }

    /// Every pane in every workspace, for starting and stopping sessions.
    pub fn all_panes(&self) -> Vec<PaneId> {
        let mut v = Vec::new();
        for w in &self.workspaces {
            for t in &w.tabs {
                t.root.panes(&mut v);
            }
        }
        v
    }

    /// Split the focused pane; the new pane gets focus. Returns it.
    ///
    /// Returns `None`, and changes nothing, when the focus is not in the tree, when the tab or the whole
    /// layout is at its pane limit, or when the focused pane is already nested `MAX_SPLIT_DEPTH` splits deep.
    pub fn split_focused(&mut self, axis: Axis) -> Option<PaneId> {
        if self.all_panes().len() >= MAX_PANES {
            return None;
        }
        let tab = self.tab();
        if tab.panes().len() >= MAX_PANES_PER_TAB {
            return None;
        }
        if tab.root.depth_of(tab.focus).is_none_or(|d| d >= MAX_SPLIT_DEPTH) {
            return None;
        }
        let (split_id, new_pane) = (self.id(), self.id());
        let tab = self.tab_mut();
        let focus = tab.focus;
        if !tab.root.split_pane(focus, axis, split_id, new_pane) {
            return None;
        }
        tab.focus = new_pane;
        Some(new_pane)
    }

    /// Open a new tab in the active workspace and focus it. Returns its pane, or `None` at the pane limit.
    pub fn new_tab(&mut self) -> Option<PaneId> {
        if self.all_panes().len() >= MAX_PANES {
            return None;
        }
        let (tab_id, pane) = (self.id(), self.id());
        let w = self.workspace_mut();
        w.tabs.push(Tab { id: tab_id, title: "shell".into(), root: Node::Pane(pane), focus: pane });
        w.active = w.tabs.len() - 1;
        Some(pane)
    }

    /// Add a workspace with one tab and switch to it. Returns its pane, or `None` at the pane limit. The name
    /// is cut and stripped of control characters the same way a loaded one is.
    pub fn new_workspace(&mut self, name: &str) -> Option<PaneId> {
        if self.all_panes().len() >= MAX_PANES {
            return None;
        }
        let (ws, tab, pane) = (self.id(), self.id(), self.id());
        self.workspaces.push(Workspace {
            id: ws,
            name: clean_text(name),
            tabs: vec![Tab { id: tab, title: "shell".into(), root: Node::Pane(pane), focus: pane }],
            active: 0,
        });
        self.active = self.workspaces.len() - 1;
        Some(pane)
    }

    /// Repair everything in a deserialised layout that would make the operations panic or misbehave. Returns
    /// one human-readable line per repair, empty when nothing had to change. A layout that cannot be repaired
    /// is replaced by a fresh one and the single returned line says why.
    pub fn validate(&mut self) -> Vec<String> {
        match self.check() {
            Ok(repairs) => repairs,
            Err(reason) => {
                *self = Layout::new().0;
                vec![format!("layout reset: {reason}")]
            }
        }
    }

    /// Parse and validate a persisted layout. Never fails and never panics: a text that is not a layout, or
    /// that cannot be repaired, gives a fresh layout and `LoadStatus::Reset`.
    pub fn from_json(text: &str) -> (Layout, LoadStatus) {
        let mut layout: Layout = match serde_json::from_str(text) {
            Ok(l) => l,
            Err(e) => return (Layout::new().0, LoadStatus::Reset(format!("not a layout: {e}"))),
        };
        match layout.check() {
            Ok(repairs) if repairs.is_empty() => (layout, LoadStatus::Clean),
            Ok(repairs) => (layout, LoadStatus::Repaired(repairs)),
            Err(reason) => (Layout::new().0, LoadStatus::Reset(reason)),
        }
    }

    /// The repair pass. `Err` carries the reason when nothing usable is left; `self` is then unspecified.
    ///
    /// Order matters: first everything that cannot be kept is dropped, in file order, so that the first of
    /// two clashing tabs wins and the caps count what is really kept; then indices, focus, ratios and text
    /// of what remains are repaired; last the id counter is raised above every id that is left.
    fn check(&mut self) -> Result<Vec<String>, String> {
        use std::collections::HashSet;

        #[derive(Default)]
        struct Dropped {
            workspaces_repeated: usize,
            workspaces_empty: usize,
            tabs_repeated_ids: usize,
            tabs_too_many_panes: usize,
            tabs_over_total: usize,
            tabs_too_deep: usize,
        }

        let mut repairs = Vec::new();
        let mut dropped = Dropped::default();
        let (mut workspace_ids, mut tab_ids): (HashSet<u64>, HashSet<u64>) = (HashSet::new(), HashSet::new());
        let (mut pane_ids, mut split_ids): (HashSet<u64>, HashSet<u64>) = (HashSet::new(), HashSet::new());
        let mut total_panes = 0usize;
        let (old_active, old_len) = (self.active, self.workspaces.len());

        let (workspaces, new_active) = retain_tracking(std::mem::take(&mut self.workspaces), old_active, |w| {
            if workspace_ids.contains(&w.id) {
                dropped.workspaces_repeated += 1;
                return false;
            }
            let (old_tab_active, old_tabs) = (w.active, w.tabs.len());
            let (tabs, new_tab_active) = retain_tracking(std::mem::take(&mut w.tabs), old_tab_active, |tab| {
                let (mut panes, mut splits) = (Vec::new(), Vec::new());
                tab.root.panes(&mut panes);
                tab.root.split_ids(&mut splits);
                if tab.root.depth() > MAX_SPLIT_DEPTH {
                    dropped.tabs_too_deep += 1;
                    return false;
                }
                if panes.len() > MAX_PANES_PER_TAB {
                    dropped.tabs_too_many_panes += 1;
                    return false;
                }
                if total_panes + panes.len() > MAX_PANES {
                    dropped.tabs_over_total += 1;
                    return false;
                }
                let own_panes: HashSet<u64> = panes.iter().copied().collect();
                let own_splits: HashSet<u64> = splits.iter().copied().collect();
                if tab_ids.contains(&tab.id)
                    || own_panes.len() != panes.len()
                    || own_splits.len() != splits.len()
                    || !own_panes.is_disjoint(&pane_ids)
                    || !own_splits.is_disjoint(&split_ids)
                {
                    dropped.tabs_repeated_ids += 1;
                    return false;
                }
                tab_ids.insert(tab.id);
                pane_ids.extend(own_panes);
                split_ids.extend(own_splits);
                total_panes += panes.len();
                true
            });
            if tabs.is_empty() {
                dropped.workspaces_empty += 1;
                return false;
            }
            w.tabs = tabs;
            match new_tab_active {
                Some(a) => w.active = a,
                None => {
                    // The active tab was dropped, or the index was never valid.
                    w.active = old_tab_active.min(w.tabs.len() - 1);
                    if old_tab_active >= old_tabs {
                        repairs.push(format!("active tab {old_tab_active} is out of range, set to {}", w.active));
                    }
                }
            }
            workspace_ids.insert(w.id);
            true
        });
        self.workspaces = workspaces;

        for (count, what) in [
            (dropped.workspaces_repeated, "workspace(s) with a repeated id"),
            (dropped.workspaces_empty, "workspace(s) without a usable tab"),
            (dropped.tabs_repeated_ids, "tab(s) that repeat an id"),
            (dropped.tabs_too_many_panes, "tab(s) with more than 64 panes"),
            (dropped.tabs_over_total, "tab(s) beyond the 256 pane limit"),
            (dropped.tabs_too_deep, "tab(s) with splits nested deeper than 32"),
        ] {
            if count > 0 {
                repairs.push(format!("dropped {count} {what}"));
            }
        }
        if self.workspaces.is_empty() {
            return Err(if old_len == 0 { "no workspaces".into() } else { repairs.join("; ") });
        }

        let last = self.workspaces.len() - 1;
        match new_active {
            Some(a) => self.active = a,
            None => {
                self.active = old_active.min(last);
                if old_active >= old_len {
                    repairs.push(format!("active workspace {old_active} is out of range, set to {}", self.active));
                }
            }
        }

        for (wi, w) in self.workspaces.iter_mut().enumerate() {
            let name = clean_text(&w.name);
            if name != w.name {
                repairs.push(format!("workspace {wi}: name shortened or stripped of control characters"));
                w.name = name;
            }
            for (ti, tab) in w.tabs.iter_mut().enumerate() {
                let title = clean_text(&tab.title);
                if title != tab.title {
                    repairs.push(format!("workspace {wi} tab {ti}: title shortened or stripped of control characters"));
                    tab.title = title;
                }
                let panes = tab.panes();
                if !panes.contains(&tab.focus) {
                    repairs.push(format!(
                        "workspace {wi} tab {ti}: focus {} is not in the tab, set to {}",
                        tab.focus, panes[0]
                    ));
                    tab.focus = panes[0];
                }
                let ratios = tab.root.sanitize_ratios();
                if ratios > 0 {
                    repairs.push(format!("workspace {wi} tab {ti}: {ratios} split ratio(s) out of range"));
                }
            }
        }

        // Pane names: a name of a pane that is not there is dropped, the others are cleaned like every stored text.
        let before = self.pane_names.len();
        let mut names = std::mem::take(&mut self.pane_names);
        names.retain(|pane, _| pane_ids.contains(pane));
        if names.len() != before {
            repairs.push(format!("dropped {} name(s) of panes that are not in the layout", before - names.len()));
        }
        for (pane, name) in &mut names {
            let clean = clean_name(name);
            if clean != *name {
                repairs.push(format!("pane {pane}: name shortened or stripped of control characters"));
                *name = clean;
            }
        }
        names.retain(|_, name| !name.is_empty());
        self.pane_names = names;

        // Id counter: every id in the file must be below next_id, or a new pane could repeat an old id.
        let max_id = [&workspace_ids, &tab_ids, &pane_ids, &split_ids]
            .into_iter()
            .flat_map(|ids| ids.iter().copied())
            .max()
            .unwrap_or(0);
        if max_id >= ID_CEILING - 1 || self.next_id >= ID_CEILING {
            return Err(format!("ids are out of range (largest {max_id}, next {})", self.next_id));
        }
        if self.next_id <= max_id {
            repairs.push(format!("next id {} raised to {}", self.next_id, max_id + 1));
            self.next_id = max_id + 1;
        }

        Ok(repairs)
    }

    /// The name the user gave to `pane`, if any.
    pub fn pane_name(&self, pane: PaneId) -> Option<&str> {
        self.pane_names.get(&pane).map(String::as_str)
    }

    /// Every pane name, by pane id.
    pub fn pane_names(&self) -> &BTreeMap<PaneId, String> {
        &self.pane_names
    }

    /// Name a pane. The text is cleaned like every stored text; the empty (or blank) text removes the name and the
    /// pane shows its program title again. False for a pane that is in no tab: nothing changes.
    pub fn rename_pane(&mut self, pane: PaneId, name: &str) -> bool {
        if !self.workspaces.iter().any(|w| w.tabs.iter().any(|t| t.root.contains(pane))) {
            return false;
        }
        let name = clean_name(name);
        if name.is_empty() {
            self.pane_names.remove(&pane);
        } else {
            self.pane_names.insert(pane, name);
        }
        true
    }

    /// Name the tab at `index` of the shown workspace; the empty text restores `shell`. False when there is no such tab.
    pub fn rename_tab(&mut self, index: usize, name: &str) -> bool {
        let name = clean_name(name);
        let w = &mut self.workspaces[self.active];
        let Some(tab) = w.tabs.get_mut(index) else { return false };
        tab.title = if name.is_empty() { "shell".into() } else { name };
        true
    }

    /// Name the workspace at `index`; the empty text restores `ws N`. False when there is no such workspace.
    pub fn rename_workspace(&mut self, index: usize, name: &str) -> bool {
        let name = clean_name(name);
        let Some(w) = self.workspaces.get_mut(index) else { return false };
        w.name = if name.is_empty() { format!("ws {}", index + 1) } else { name };
        true
    }

    /// Where a pane is: `(workspace index, tab index)`.
    fn locate(&self, pane: PaneId) -> Option<(usize, usize)> {
        self.workspaces.iter().enumerate().find_map(|(wi, w)| w.tabs.iter().position(|t| t.root.contains(pane)).map(|ti| (wi, ti)))
    }

    /// Drop `pane` on `target`: on a side of it the target is split there and the pane takes that half, on the
    /// middle the two swap places. Works inside a tab and across tabs and workspaces; the pane keeps its id (its
    /// shell keeps running, its name stays). The moved pane gets focus and is shown. Nothing changes, and false
    /// comes back, for an unknown or identical id, and when the result would pass the pane or depth limits.
    pub fn move_pane(&mut self, pane: PaneId, target: PaneId, edge: Edge) -> bool {
        if pane == target || self.locate(pane).is_none() || self.locate(target).is_none() {
            return false;
        }
        if edge == Edge::Center {
            return self.swap_panes(pane, target);
        }
        let mut next = self.clone();
        if !next.detach_pane(pane) || !next.insert_pane(pane, target, edge) {
            return false;
        }
        *self = next;
        true
    }

    /// Put `pane`, which is in no tab of this layout, on a side of `target` (never the middle) and focus it. False,
    /// with the layout possibly changed by the unused split id only, when the target is not here or the tab is at
    /// its pane or depth limit; callers work on a copy.
    fn insert_pane(&mut self, pane: PaneId, target: PaneId, edge: Edge) -> bool {
        let Some((wi, ti)) = self.locate(target) else { return false };
        let (axis, before) = match edge {
            Edge::Left => (Axis::Row, true),
            Edge::Right => (Axis::Row, false),
            Edge::Top => (Axis::Column, true),
            Edge::Bottom => (Axis::Column, false),
            Edge::Center => return false,
        };
        let split_id = self.id();
        let tab = &mut self.workspaces[wi].tabs[ti];
        if tab.panes().len() >= MAX_PANES_PER_TAB || !tab.root.split_with(target, axis, split_id, pane, before) {
            return false;
        }
        if tab.root.depth() > MAX_SPLIT_DEPTH {
            return false;
        }
        self.focus_pane(pane);
        true
    }

    /// Swap the places of two panes, in one tab or in two. Both keep their ids and names; each tab's focus stays
    /// on the same place, and the pane `a` is focused and shown afterwards.
    pub fn swap_panes(&mut self, a: PaneId, b: PaneId) -> bool {
        if a == b || self.locate(a).is_none() || self.locate(b).is_none() {
            return false;
        }
        for w in &mut self.workspaces {
            for t in &mut w.tabs {
                // A temporary id outside every range in use, so the two renames cannot meet in the middle.
                t.root.rename_leaf(a, u64::MAX);
                t.root.rename_leaf(b, a);
                t.root.rename_leaf(u64::MAX, b);
                if t.focus == a {
                    t.focus = b;
                } else if t.focus == b {
                    t.focus = a;
                }
            }
        }
        self.focus_pane(a);
        true
    }

    /// Move the tab at `from` to the place `to` in the shown workspace (the other tabs shift); the moved tab stays the
    /// shown one if it was shown, and the shown tab stays shown otherwise. False when an index is out of range.
    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        let w = self.workspace_mut();
        if from >= w.tabs.len() || to >= w.tabs.len() {
            return false;
        }
        if from == to {
            return true;
        }
        let shown = w.tabs[w.active].id;
        let tab = w.tabs.remove(from);
        w.tabs.insert(to, tab);
        w.active = w.tabs.iter().position(|t| t.id == shown).unwrap_or(0);
        true
    }

    /// Move the tab at `tab` of the shown workspace to the end of workspace `to`, and show it there. A workspace
    /// this empties disappears. False for an index out of range or the shown workspace itself.
    pub fn move_tab_to_workspace(&mut self, tab: usize, to: usize) -> bool {
        if to >= self.workspaces.len() || to == self.active || tab >= self.workspace().tabs.len() {
            return false;
        }
        let target_id = self.workspaces[to].id;
        let moved = self.workspace_mut().tabs.remove(tab);
        let source = self.active;
        {
            let w = &mut self.workspaces[source];
            w.active = w.active.min(w.tabs.len().saturating_sub(1));
        }
        if self.workspaces[source].tabs.is_empty() {
            self.workspaces.remove(source);
        }
        let to = self.workspaces.iter().position(|w| w.id == target_id).unwrap_or(0);
        let w = &mut self.workspaces[to];
        w.tabs.push(moved);
        w.active = w.tabs.len() - 1;
        self.active = to;
        true
    }

    /// Take `pane` out of its tab; its sibling takes its place. A tab (or workspace) that this empties disappears,
    /// with the active indexes kept on what was shown. The pane's name is kept: the caller closes or re-inserts it.
    /// False when no tab holds the pane.
    fn detach_pane(&mut self, pane: PaneId) -> bool {
        for wi in 0..self.workspaces.len() {
            let w = &mut self.workspaces[wi];
            let Some(ti) = w.tabs.iter().position(|t| t.root.contains(pane)) else { continue };
            let tab = &mut w.tabs[ti];
            if tab.root.remove(pane) {
                if tab.focus == pane {
                    tab.focus = tab.panes()[0];
                }
            } else {
                self.remove_tab_at(wi, ti);
            }
            return true;
        }
        false
    }

    /// Take the tab `ti` of workspace `wi` out. A workspace that this empties disappears, and the active indexes stay
    /// on what was shown: when something before the shown one goes the index moves with it, and when the shown one
    /// itself goes the one that followed is shown (the new last one when it was the last). Both indexes must exist.
    /// The layout may be left without a workspace; the caller keeps that from being visible.
    fn remove_tab_at(&mut self, wi: usize, ti: usize) -> Tab {
        let w = &mut self.workspaces[wi];
        let tab = w.tabs.remove(ti);
        if ti < w.active {
            // The shown tab moved one place to the left with its siblings: it stays the shown one.
            w.active -= 1;
        } else {
            // The shown tab itself went (the one that followed slides into the index, or the new last
            // one is shown), or a tab after it went and the index is still right.
            w.active = w.active.min(w.tabs.len().saturating_sub(1));
        }
        if w.tabs.is_empty() {
            self.workspaces.remove(wi);
            if wi < self.active {
                // The shown workspace moved one place to the left with its siblings: it stays shown.
                self.active -= 1;
            } else {
                self.active = self.active.min(self.workspaces.len().saturating_sub(1));
            }
        }
        tab
    }

    /// Close a pane everywhere. Empty tabs and empty workspaces disappear; the last pane of the last
    /// workspace cannot be closed. An id that is in no tab is `Unknown`, which is checked first.
    ///
    /// What is shown stays shown: when a tab or workspace before the shown one disappears, the active index
    /// moves with it and keeps pointing at the same one, so a shell that ends in the background changes
    /// nothing the user looks at. Only when the shown one itself disappears is the one that followed it shown
    /// (the new last one when it was the last).
    pub fn close_pane(&mut self, pane: PaneId) -> CloseOutcome {
        if !self.workspaces.iter().any(|w| w.tabs.iter().any(|t| t.root.contains(pane))) {
            return CloseOutcome::Unknown;
        }
        if self.all_panes().len() <= 1 {
            return CloseOutcome::LastPane;
        }
        if self.detach_pane(pane) {
            self.pane_names.remove(&pane);
            return CloseOutcome::Closed;
        }
        CloseOutcome::Unknown
    }

    /// Close every pane of the tab at `index` of the shown workspace (the index `select_tab` and `rename_tab` use),
    /// one by one through [`Layout::close_pane`], so what is shown stays shown, names go with their panes and an empty
    /// workspace disappears. When the tab holds every pane of the layout nothing changes and the answer is
    /// [`CloseGroup::LastPane`], like `close_pane` of the last pane (the caller ends the app).
    pub fn close_tab(&mut self, index: usize) -> CloseGroup {
        let Some(tab) = self.workspaces[self.active].tabs.get(index) else { return CloseGroup::Unknown };
        let panes = tab.panes();
        self.close_all(panes)
    }

    /// Like [`Layout::close_tab`] for the workspace at `index`: every pane of every tab of it.
    pub fn close_workspace(&mut self, index: usize) -> CloseGroup {
        let Some(workspace) = self.workspaces.get(index) else { return CloseGroup::Unknown };
        let panes = workspace.tabs.iter().flat_map(Tab::panes).collect();
        self.close_all(panes)
    }

    fn close_all(&mut self, panes: Vec<PaneId>) -> CloseGroup {
        if panes.len() >= self.all_panes().len() {
            return CloseGroup::LastPane;
        }
        for pane in &panes {
            self.close_pane(*pane);
        }
        CloseGroup::Closed(panes)
    }

    pub fn focus_pane(&mut self, pane: PaneId) {
        for (wi, w) in self.workspaces.iter_mut().enumerate() {
            if let Some(ti) = w.tabs.iter().position(|t| t.root.contains(pane)) {
                w.tabs[ti].focus = pane;
                w.active = ti;
                self.active = wi;
                return;
            }
        }
    }

    /// Move focus to the next (`+1`) or previous (`-1`) pane of the tab, wrapping around.
    pub fn cycle_focus(&mut self, step: isize) {
        let tab = self.tab_mut();
        let panes = tab.panes();
        let i = panes.iter().position(|p| *p == tab.focus).unwrap_or(0) as isize;
        tab.focus = panes[(i + step).rem_euclid(panes.len() as isize) as usize];
    }

    pub fn select_tab(&mut self, index: usize) {
        let w = self.workspace_mut();
        if index < w.tabs.len() {
            w.active = index;
        }
    }

    pub fn select_workspace(&mut self, index: usize) {
        if index < self.workspaces.len() {
            self.active = index;
        }
    }

    /// Move a divider. A value that is not a number (a pointer position computed from nothing) leaves the
    /// divider where it is; any other value is clamped to `MIN_RATIO..=1 - MIN_RATIO`.
    pub fn set_ratio(&mut self, split: SplitId, ratio: f32) {
        if !ratio.is_finite() {
            return;
        }
        for w in &mut self.workspaces {
            for t in &mut w.tabs {
                if t.root.set_ratio(split, ratio) {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitting_makes_two_panes_that_cover_the_area() {
        let (mut l, first) = Layout::new();
        let second = l.split_focused(Axis::Row).unwrap();
        let (panes, dividers) = l.tab().geometry();
        assert_eq!(panes.len(), 2);
        assert_eq!(dividers.len(), 1);
        assert_eq!(l.tab().focus, second);
        let area: f32 = panes.iter().map(|(_, r)| r.w * r.h).sum();
        assert!((area - 1.0).abs() < 1e-6);
        assert!(panes.iter().any(|(p, _)| *p == first));
    }

    #[test]
    fn nested_splits_and_closing_collapse_the_tree() {
        let (mut l, a) = Layout::new();
        let b = l.split_focused(Axis::Row).unwrap();
        let c = l.split_focused(Axis::Column).unwrap();
        assert_eq!(l.tab().panes(), vec![a, b, c]);
        assert_eq!(l.close_pane(b), CloseOutcome::Closed);
        assert_eq!(l.tab().panes(), vec![a, c]);
        assert_eq!(l.close_pane(c), CloseOutcome::Closed);
        assert_eq!(l.tab().root, Node::Pane(a));
    }

    #[test]
    fn the_last_pane_cannot_be_closed() {
        let (mut l, only) = Layout::new();
        assert_eq!(l.close_pane(only), CloseOutcome::LastPane);
        assert_eq!(l.all_panes(), vec![only]);
    }

    #[test]
    fn closing_the_only_pane_of_a_tab_removes_the_tab() {
        let (mut l, first) = Layout::new();
        let second = l.new_tab().unwrap();
        assert_eq!(l.workspace().tabs.len(), 2);
        assert_eq!(l.close_pane(second), CloseOutcome::Closed);
        assert_eq!(l.workspace().tabs.len(), 1);
        assert_eq!(l.tab().focus, first);
    }

    #[test]
    fn workspaces_are_independent() {
        let (mut l, a) = Layout::new();
        let b = l.new_workspace("work").unwrap();
        assert_eq!(l.active, 1);
        assert_eq!(l.tab().panes(), vec![b]);
        l.select_workspace(0);
        assert_eq!(l.tab().panes(), vec![a]);
        l.focus_pane(b);
        assert_eq!(l.active, 1);
    }

    #[test]
    fn ratio_is_clamped_and_moves_the_divider() {
        let (mut l, _) = Layout::new();
        l.split_focused(Axis::Row);
        let split = l.tab().geometry().1[0].split;
        l.set_ratio(split, 0.0);
        assert_eq!(l.tab().geometry().1[0].ratio, MIN_RATIO);
        l.set_ratio(split, 0.7);
        let (panes, _) = l.tab().geometry();
        assert!((panes[0].1.w - 0.7).abs() < 1e-6);
    }

    #[test]
    fn focus_cycles_and_wraps() {
        let (mut l, a) = Layout::new();
        let b = l.split_focused(Axis::Row).unwrap();
        l.cycle_focus(1);
        assert_eq!(l.tab().focus, a);
        l.cycle_focus(-1);
        assert_eq!(l.tab().focus, b);
    }

    #[test]
    fn json_roundtrip_keeps_the_layout() {
        let (mut l, _) = Layout::new();
        l.split_focused(Axis::Column);
        l.new_workspace("two");
        l.split_focused(Axis::Row);
        let json = serde_json::to_string(&l).unwrap();
        let back: Layout = serde_json::from_str(&json).unwrap();
        assert_eq!(back, l);
    }

    // ---- closing: unknown and last are different outcomes ------------------------------------------

    #[test]
    fn closing_an_unknown_pane_is_unknown_even_when_only_one_pane_is_left() {
        let (mut l, only) = Layout::new();
        assert_eq!(l.close_pane(999), CloseOutcome::Unknown);
        assert_eq!(l.close_pane(only), CloseOutcome::LastPane);
        assert_eq!(l.all_panes(), vec![only]);
    }

    #[test]
    fn closing_an_unknown_pane_changes_nothing_when_several_panes_exist() {
        let (mut l, a) = Layout::new();
        let b = l.split_focused(Axis::Row).unwrap();
        let before = l.clone();
        assert_eq!(l.close_pane(999), CloseOutcome::Unknown);
        assert_eq!(l, before);
        assert_eq!(l.close_pane(a), CloseOutcome::Closed);
        assert_eq!(l.all_panes(), vec![b]);
        assert_eq!(l.close_pane(a), CloseOutcome::Unknown, "a closed pane is unknown afterwards");
    }

    // ---- closing keeps pointing at what was shown (CR-01) ------------------------------------------

    /// One workspace with the tabs A, B, C and B shown. Returns the layout and the pane of each tab.
    fn tabs_with_the_middle_one_shown() -> (Layout, PaneId, PaneId, PaneId) {
        let (mut l, a) = Layout::new();
        let b = l.new_tab().unwrap();
        let c = l.new_tab().unwrap();
        l.focus_pane(b);
        assert_eq!(l.tab().panes(), vec![b]);
        assert_eq!(l.workspace().active, 1);
        (l, a, b, c)
    }

    #[test]
    fn closing_a_tab_before_the_shown_one_keeps_the_shown_tab_shown() {
        let (mut l, a, b, c) = tabs_with_the_middle_one_shown();

        assert_eq!(l.close_pane(a), CloseOutcome::Closed);

        assert_eq!(l.tab().panes(), vec![b], "tab B is still the shown one");
        assert_eq!(l.workspace().active, 0, "the index follows B one place to the left");
        assert_eq!(l.workspace().tabs.len(), 2);
        assert_eq!(l.workspace().tabs[1].panes(), vec![c]);
    }

    #[test]
    fn closing_the_shown_tab_shows_the_one_that_followed_or_the_new_last_one() {
        let (mut l, a, b, c) = tabs_with_the_middle_one_shown();

        assert_eq!(l.close_pane(b), CloseOutcome::Closed);
        assert_eq!(l.tab().panes(), vec![c], "the tab that followed the shown one is shown");
        assert_eq!(l.workspace().active, 1);

        assert_eq!(l.close_pane(c), CloseOutcome::Closed);
        assert_eq!(l.tab().panes(), vec![a], "the shown last tab was closed: the new last tab is shown");
        assert_eq!(l.workspace().active, 0);
    }

    #[test]
    fn closing_a_tab_after_the_shown_one_keeps_the_shown_tab_shown() {
        let (mut l, a, b, c) = tabs_with_the_middle_one_shown();

        assert_eq!(l.close_pane(c), CloseOutcome::Closed);

        assert_eq!(l.tab().panes(), vec![b], "tab B is still the shown one");
        assert_eq!(l.workspace().active, 1);
        assert_eq!(l.workspace().tabs[0].panes(), vec![a]);
    }

    /// Three workspaces W0, W1, W2 with one pane each and W1 shown. Returns the layout and the three panes.
    fn workspaces_with_the_middle_one_shown() -> (Layout, PaneId, PaneId, PaneId) {
        let (mut l, a) = Layout::new();
        let b = l.new_workspace("one").unwrap();
        let c = l.new_workspace("two").unwrap();
        l.select_workspace(1);
        assert_eq!(l.tab().panes(), vec![b]);
        assert_eq!(l.active, 1);
        (l, a, b, c)
    }

    #[test]
    fn closing_a_workspace_before_the_shown_one_keeps_the_shown_workspace_shown() {
        let (mut l, a, b, c) = workspaces_with_the_middle_one_shown();
        let shown = l.workspace().id;

        assert_eq!(l.close_pane(a), CloseOutcome::Closed);

        assert_eq!(l.workspace().id, shown, "workspace W1 is still the shown one");
        assert_eq!(l.tab().panes(), vec![b]);
        assert_eq!(l.active, 0, "the index follows W1 one place to the left");
        assert_eq!(l.workspaces.len(), 2);
        assert_eq!(l.workspaces[1].tabs[0].panes(), vec![c]);
    }

    #[test]
    fn closing_the_shown_workspace_shows_the_one_that_followed_or_the_new_last_one() {
        let (mut l, a, b, c) = workspaces_with_the_middle_one_shown();

        assert_eq!(l.close_pane(b), CloseOutcome::Closed);
        assert_eq!(l.tab().panes(), vec![c], "the workspace that followed the shown one is shown");
        assert_eq!(l.active, 1);

        assert_eq!(l.close_pane(c), CloseOutcome::Closed);
        assert_eq!(l.tab().panes(), vec![a], "the shown last workspace was closed: the new last one is shown");
        assert_eq!(l.active, 0);
    }

    // ---- closing a whole tab or workspace -------------------------------------------------------------------------

    #[test]
    fn closing_a_tab_closes_all_its_panes_and_nothing_else() {
        let (mut l, a) = Layout::new();
        let b = l.split_focused(Axis::Row).unwrap();
        let c = l.split_focused(Axis::Column).unwrap();
        let d = l.new_tab().unwrap();
        l.rename_pane(b, "kept? no");
        l.rename_pane(d, "other tab");
        l.select_tab(0);

        assert_eq!(l.close_tab(0), CloseGroup::Closed(vec![a, b, c]));

        assert_eq!(l.all_panes(), vec![d]);
        assert_eq!(l.workspace().tabs.len(), 1);
        assert_eq!(l.pane_name(b), None, "the names go with the panes");
        assert_eq!(l.pane_name(d), Some("other tab"));
    }

    #[test]
    fn closing_a_tab_follows_the_rules_of_closing_a_pane_for_what_is_shown() {
        // Before the shown tab: the shown one stays shown.
        let (mut l, a, b, c) = tabs_with_the_middle_one_shown();
        assert_eq!(l.close_tab(0), CloseGroup::Closed(vec![a]));
        assert_eq!((l.tab().panes(), l.workspace().active), (vec![b], 0));
        // The shown tab itself: the one that followed it is shown, the new last one when it was the last.
        assert_eq!(l.close_tab(0), CloseGroup::Closed(vec![b]));
        assert_eq!((l.tab().panes(), l.workspace().active), (vec![c], 0));
        let (mut l, a, b, c) = tabs_with_the_middle_one_shown();
        assert_eq!(l.close_tab(2), CloseGroup::Closed(vec![c]), "after the shown one");
        assert_eq!((l.tab().panes(), l.workspace().active), (vec![b], 1));
        assert_eq!(l.close_tab(1), CloseGroup::Closed(vec![b]));
        assert_eq!((l.tab().panes(), l.workspace().active), (vec![a], 0));
    }

    #[test]
    fn closing_a_tab_that_is_not_there_or_is_everything_changes_nothing() {
        let (mut l, a) = Layout::new();
        let b = l.split_focused(Axis::Row).unwrap();
        let before = l.clone();
        assert_eq!(l.close_tab(1), CloseGroup::Unknown);
        assert_eq!(l.close_tab(usize::MAX), CloseGroup::Unknown);
        assert_eq!(l.close_tab(0), CloseGroup::LastPane, "every pane of the layout: the end of the app");
        assert_eq!(l, before);
        assert_eq!(l.all_panes(), vec![a, b]);
    }

    #[test]
    fn closing_a_tab_in_another_workspace_than_the_shown_one_is_not_possible_by_index() {
        // The index is the one of the shown workspace, as for select_tab and rename_tab.
        let (mut l, a) = Layout::new();
        let b = l.new_tab().unwrap();
        let c = l.new_workspace("two").unwrap();
        assert_eq!(l.close_tab(1), CloseGroup::Unknown, "the shown workspace has one tab");
        assert_eq!(l.close_tab(0), CloseGroup::Closed(vec![c]));
        assert_eq!((l.active, l.all_panes()), (0, vec![a, b]), "the workspace went with its only tab");
    }

    #[test]
    fn closing_a_workspace_closes_all_panes_of_all_its_tabs() {
        let (mut l, a) = Layout::new();
        let b = l.new_workspace("two").unwrap();
        let c = l.split_focused(Axis::Row).unwrap();
        let d = l.new_tab().unwrap();
        let e = l.new_workspace("three").unwrap();
        l.rename_pane(c, "gone");
        l.select_workspace(1);

        assert_eq!(l.close_workspace(1), CloseGroup::Closed(vec![b, c, d]));

        assert_eq!(l.all_panes(), vec![a, e]);
        assert_eq!(l.workspaces.len(), 2);
        assert_eq!(l.tab().panes(), vec![e], "the workspace that followed the shown one is shown");
        assert_eq!(l.pane_name(c), None);
    }

    #[test]
    fn closing_a_workspace_before_the_shown_one_keeps_the_shown_one_shown() {
        let (mut l, a, b, c) = workspaces_with_the_middle_one_shown();
        let shown = l.workspace().id;
        assert_eq!(l.close_workspace(0), CloseGroup::Closed(vec![a]));
        assert_eq!((l.workspace().id, l.active), (shown, 0));
        assert_eq!(l.close_workspace(1), CloseGroup::Closed(vec![c]), "after the shown one");
        assert_eq!((l.tab().panes(), l.active), (vec![b], 0));
    }

    #[test]
    fn closing_a_workspace_that_is_not_there_or_is_everything_changes_nothing() {
        let (mut l, _) = Layout::new();
        l.split_focused(Axis::Row);
        l.new_tab();
        let before = l.clone();
        assert_eq!(l.close_workspace(1), CloseGroup::Unknown);
        assert_eq!(l.close_workspace(0), CloseGroup::LastPane);
        assert_eq!(l, before);
    }

    #[test]
    fn every_workspace_keeps_its_own_shown_tab_when_a_tab_before_it_is_removed() {
        // W0 has the tabs T0, T1, T2 with T1 shown; W1 is the workspace the user looks at.
        let (mut l, t0) = Layout::new();
        let t1 = l.new_tab().unwrap();
        let t2 = l.new_tab().unwrap();
        l.focus_pane(t1);
        let other = l.new_workspace("other").unwrap();
        assert_eq!(l.active, 1);

        assert_eq!(l.close_pane(t0), CloseOutcome::Closed);

        assert_eq!(l.tab().panes(), vec![other], "the shown workspace did not change");
        l.select_workspace(0);
        assert_eq!(l.tab().panes(), vec![t1], "returning to W0 shows the tab it showed before");
        assert_eq!(l.workspace().tabs[1].panes(), vec![t2]);
    }

    // ---- loading: hostile and damaged files --------------------------------------------------------

    /// A layout text with one workspace of one tab of one pane (id 3), the given active indices and next_id.
    fn one_pane_json(active: u64, ws_active: u64, next_id: u64) -> String {
        format!(
            r#"{{"workspaces":[{{"id":1,"name":"main","tabs":[{{"id":2,"title":"shell","root":{{"Pane":3}},"focus":3}}],"active":{ws_active}}}],"active":{active},"next_id":{next_id}}}"#
        )
    }

    #[test]
    fn text_that_is_not_json_gives_a_fresh_layout_and_the_parse_error() {
        let (l, status) = Layout::from_json("{ not json");
        assert_eq!(l, Layout::new().0);
        match status {
            LoadStatus::Reset(reason) => assert!(!reason.is_empty(), "the reason carries the parse error"),
            other => panic!("expected Reset, got {other:?}"),
        }
    }

    #[test]
    fn json_of_the_wrong_shape_gives_a_fresh_layout() {
        for text in ["", "null", "[]", "42", r#"{"workspaces":"x"}"#, r#"{"workspaces":[]}"#] {
            let (l, status) = Layout::from_json(text);
            assert_eq!(l, Layout::new().0, "text: {text:?}");
            assert!(matches!(status, LoadStatus::Reset(_)), "text: {text:?} gave {status:?}");
        }
    }

    #[test]
    fn an_empty_workspace_list_resets_the_layout() {
        let (l, status) = Layout::from_json(r#"{"workspaces":[],"active":0,"next_id":10}"#);
        assert_eq!(l, Layout::new().0);
        assert!(matches!(status, LoadStatus::Reset(_)), "got {status:?}");
    }

    #[test]
    fn workspaces_without_tabs_are_dropped_and_none_left_resets() {
        let two = r#"{"workspaces":[
            {"id":1,"name":"empty","tabs":[],"active":0},
            {"id":4,"name":"kept","tabs":[{"id":5,"title":"shell","root":{"Pane":6},"focus":6}],"active":0}
        ],"active":1,"next_id":7}"#;
        let (l, status) = Layout::from_json(two);
        assert_eq!(l.workspaces.len(), 1);
        assert_eq!(l.workspaces[0].name, "kept");
        assert_eq!(l.active, 0, "the active index follows the drop");
        assert!(matches!(status, LoadStatus::Repaired(ref r) if !r.is_empty()), "got {status:?}");

        let none = r#"{"workspaces":[{"id":1,"name":"empty","tabs":[],"active":0}],"active":0,"next_id":2}"#;
        let (l, status) = Layout::from_json(none);
        assert_eq!(l, Layout::new().0);
        assert!(matches!(status, LoadStatus::Reset(_)), "got {status:?}");
    }

    #[test]
    fn out_of_range_active_indices_are_clamped_and_reported() {
        let (l, status) = Layout::from_json(&one_pane_json(3, 7, 4));
        assert_eq!(l.active, 0);
        assert_eq!(l.workspaces[0].active, 0);
        let LoadStatus::Repaired(repairs) = status else { panic!("expected Repaired, got {status:?}") };
        assert_eq!(repairs.len(), 2, "one line per repair: {repairs:?}");
        // The repaired layout is usable.
        assert_eq!(l.tab().panes(), vec![3]);
        assert_eq!(l.tab().focus, 3);
    }

    #[test]
    fn a_focus_outside_the_tree_moves_to_the_first_pane_of_the_tab() {
        let text = r#"{"workspaces":[{"id":1,"name":"main","tabs":[{"id":2,"title":"shell",
            "root":{"Split":{"id":3,"axis":"Row","ratio":0.5,"a":{"Pane":4},"b":{"Pane":5}}},"focus":99}],
            "active":0}],"active":0,"next_id":6}"#;
        let (l, status) = Layout::from_json(text);
        assert_eq!(l.tab().focus, 4);
        assert!(matches!(status, LoadStatus::Repaired(ref r) if r.len() == 1), "got {status:?}");
    }

    #[test]
    fn a_next_id_at_or_below_the_largest_id_is_raised_above_it() {
        // Largest id in the file is 3 (the pane); next_id 2 would hand out 2 and 3 again.
        let (mut l, status) = Layout::from_json(&one_pane_json(0, 0, 2));
        assert!(matches!(status, LoadStatus::Repaired(ref r) if r.len() == 1), "got {status:?}");
        let new_pane = l.new_tab().unwrap();
        assert!(new_pane > 3, "new pane {new_pane} must not collide with the file's ids");
        let mut all = l.all_panes();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 2, "every pane exactly once");

        // Equal to the largest id counts as too low as well.
        let (l, status) = Layout::from_json(&one_pane_json(0, 0, 3));
        assert!(matches!(status, LoadStatus::Repaired(_)), "got {status:?}");
        assert!(l.clone().new_tab().unwrap() > 3);
    }

    #[test]
    fn an_id_of_u64_max_resets_the_layout() {
        let max = u64::MAX;
        let pane_max = format!(
            r#"{{"workspaces":[{{"id":1,"name":"m","tabs":[{{"id":2,"title":"s","root":{{"Pane":{max}}},"focus":{max}}}],"active":0}}],"active":0,"next_id":5}}"#
        );
        for text in [pane_max, one_pane_json(0, 0, max)] {
            let (l, status) = Layout::from_json(&text);
            assert_eq!(l, Layout::new().0, "text: {text}");
            assert!(matches!(status, LoadStatus::Reset(_)), "got {status:?}");
        }
    }

    #[test]
    fn a_clean_file_loads_unchanged() {
        let (mut l, _) = Layout::new();
        l.split_focused(Axis::Column);
        l.new_workspace("two");
        l.split_focused(Axis::Row);
        l.new_tab();
        let text = serde_json::to_string_pretty(&l).unwrap();
        let (back, status) = Layout::from_json(&text);
        assert_eq!(status, LoadStatus::Clean);
        assert_eq!(back, l);
    }

    #[test]
    fn validate_repairs_in_place_and_a_second_pass_finds_nothing() {
        let mut l: Layout = serde_json::from_str(&one_pane_json(3, 7, 1)).unwrap();
        let first = l.validate();
        assert!(!first.is_empty());
        assert!(l.validate().is_empty(), "validate is idempotent");
        assert_eq!(Layout::new().0.validate(), Vec::<String>::new());
    }

    // ---- loading: duplicates, ratios, caps, strings ------------------------------------------------

    fn leaf_tab(id: u64, pane: u64) -> Tab {
        Tab { id, title: "shell".into(), root: Node::Pane(pane), focus: pane }
    }

    /// A balanced tree holding the given panes. A split is named after the first pane of its right half, so
    /// split ids are unique inside one tree; use bases that are far apart for different trees.
    fn balanced(panes: std::ops::Range<u64>, split_base: u64) -> Node {
        if panes.end - panes.start == 1 {
            return Node::Pane(panes.start);
        }
        let mid = panes.start + (panes.end - panes.start) / 2;
        Node::Split {
            id: split_base + mid,
            axis: Axis::Row,
            ratio: 0.5,
            a: Box::new(balanced(panes.start..mid, split_base)),
            b: Box::new(balanced(mid..panes.end, split_base)),
        }
    }

    fn big_tab(id: u64, panes: std::ops::Range<u64>, split_base: u64) -> Tab {
        let focus = panes.start;
        Tab { id, title: "shell".into(), root: balanced(panes, split_base), focus }
    }

    /// A right-leaning chain of `splits` splits (`splits + 1` panes), the shape that repeated splitting of the
    /// newest pane makes. Its JSON nests two levels per split.
    fn chain(first_pane: u64, splits: u64, split_base: u64) -> Node {
        let mut node = Node::Pane(first_pane + splits);
        for k in (0..splits).rev() {
            node = Node::Split {
                id: split_base + k,
                axis: Axis::Row,
                ratio: 0.5,
                a: Box::new(Node::Pane(first_pane + k)),
                b: Box::new(node),
            };
        }
        node
    }

    fn depth(n: &Node) -> usize {
        match n {
            Node::Pane(_) => 0,
            Node::Split { a, b, .. } => 1 + depth(a).max(depth(b)),
        }
    }

    fn workspace(id: u64, name: &str, tabs: Vec<Tab>) -> Workspace {
        Workspace { id, name: name.into(), tabs, active: 0 }
    }

    /// Serialise a hand-built layout and load it like a file from disk.
    fn load(workspaces: Vec<Workspace>) -> (Layout, LoadStatus) {
        let l = Layout { workspaces, active: 0, next_id: 1_000_000, pane_names: BTreeMap::new() };
        Layout::from_json(&serde_json::to_string(&l).unwrap())
    }

    #[test]
    fn a_pane_id_in_two_tabs_drops_the_later_tab() {
        let (l, status) = load(vec![
            workspace(1, "a", vec![leaf_tab(2, 3), leaf_tab(4, 3)]),
            workspace(5, "b", vec![leaf_tab(6, 3), leaf_tab(7, 8)]),
        ]);
        assert_eq!(l.all_panes(), vec![3, 8], "every pane exactly once");
        assert_eq!(l.workspaces[0].tabs.len(), 1);
        assert_eq!(l.workspaces[1].tabs.len(), 1);
        assert!(matches!(status, LoadStatus::Repaired(_)), "got {status:?}");
    }

    #[test]
    fn a_pane_id_twice_inside_one_tab_drops_that_tab() {
        let twice = Node::Split {
            id: 9,
            axis: Axis::Row,
            ratio: 0.5,
            a: Box::new(Node::Pane(3)),
            b: Box::new(Node::Pane(3)),
        };
        let bad = Tab { id: 4, title: "x".into(), root: twice, focus: 3 };
        let (l, _) = load(vec![workspace(1, "a", vec![leaf_tab(2, 10), bad])]);
        assert_eq!(l.all_panes(), vec![10]);
    }

    #[test]
    fn a_split_id_used_twice_drops_the_tab_that_repeats_it() {
        let nested = Node::Split {
            id: 5,
            axis: Axis::Row,
            ratio: 0.5,
            a: Box::new(Node::Pane(3)),
            b: Box::new(Node::Split {
                id: 5,
                axis: Axis::Column,
                ratio: 0.5,
                a: Box::new(Node::Pane(4)),
                b: Box::new(Node::Pane(6)),
            }),
        };
        let bad = Tab { id: 7, title: "x".into(), root: nested, focus: 3 };
        let (l, status) = load(vec![workspace(1, "a", vec![leaf_tab(2, 10), bad])]);
        assert_eq!(l.all_panes(), vec![10]);
        assert!(matches!(status, LoadStatus::Repaired(_)), "got {status:?}");

        // The same split id in two different tabs is a duplicate as well.
        let one = |tab, a, b, split| Tab {
            id: tab,
            title: "x".into(),
            root: Node::Split { id: split, axis: Axis::Row, ratio: 0.5, a: Box::new(Node::Pane(a)), b: Box::new(Node::Pane(b)) },
            focus: a,
        };
        let (l, _) = load(vec![workspace(1, "a", vec![one(2, 3, 4, 9), one(5, 6, 7, 9)])]);
        assert_eq!(l.all_panes(), vec![3, 4]);
    }

    #[test]
    fn repeated_tab_and_workspace_ids_drop_the_later_one() {
        let (l, _) = load(vec![
            workspace(1, "a", vec![leaf_tab(2, 3), leaf_tab(2, 4)]),
            workspace(1, "dup", vec![leaf_tab(8, 9)]),
        ]);
        assert_eq!(l.workspaces.len(), 1);
        assert_eq!(l.all_panes(), vec![3]);
    }

    #[test]
    fn ratios_that_are_not_finite_or_out_of_range_are_sanitised() {
        let split = |ratio: f32| Node::Split {
            id: 5,
            axis: Axis::Row,
            ratio,
            a: Box::new(Node::Pane(3)),
            b: Box::new(Node::Pane(4)),
        };
        let cases = [
            (f32::NAN, 0.5),
            (f32::INFINITY, 0.5),
            (f32::NEG_INFINITY, 0.5),
            (-3.0, MIN_RATIO),
            (0.0, MIN_RATIO),
            (7.5, 1.0 - MIN_RATIO),
            (0.7, 0.7),
        ];
        for (given, expected) in cases {
            let tab = Tab { id: 2, title: "t".into(), root: split(given), focus: 3 };
            let mut l = Layout { workspaces: vec![workspace(1, "a", vec![tab])], active: 0, next_id: 100, pane_names: BTreeMap::new() };
            let repairs = l.validate();
            let Node::Split { ratio, .. } = l.tab().root else { panic!("tree changed") };
            assert_eq!(ratio, expected, "ratio {given}");
            assert_eq!(repairs.is_empty(), given == expected, "ratio {given}: {repairs:?}");
        }
    }

    #[test]
    fn a_ratio_beyond_f32_in_the_file_is_sanitised_not_fatal() {
        let text = r#"{"workspaces":[{"id":1,"name":"a","tabs":[{"id":2,"title":"t",
            "root":{"Split":{"id":3,"axis":"Row","ratio":1e39,"a":{"Pane":4},"b":{"Pane":5}}},"focus":4}],
            "active":0}],"active":0,"next_id":6}"#;
        let (l, status) = Layout::from_json(text);
        assert!(l.tab().geometry().1.iter().all(|d| d.ratio.is_finite()));
        assert!(matches!(status, LoadStatus::Repaired(_) | LoadStatus::Reset(_)), "got {status:?}");
        assert_eq!(l.all_panes().len(), if matches!(status, LoadStatus::Reset(_)) { 1 } else { 2 });
    }

    #[test]
    fn a_tab_with_more_than_64_panes_is_dropped() {
        let (l, status) = load(vec![workspace(
            1,
            "a",
            vec![big_tab(2, 100..164, 100_000), big_tab(3, 200..265, 200_000), leaf_tab(4, 900)],
        )]);
        assert_eq!(l.workspaces[0].tabs.len(), 2, "64 panes fit, 65 do not");
        assert_eq!(l.all_panes().len(), 64 + 1);
        assert!(matches!(status, LoadStatus::Repaired(_)), "got {status:?}");
    }

    #[test]
    fn a_workspace_whose_only_tab_is_over_the_cap_is_dropped_and_none_left_resets() {
        let (l, status) = load(vec![workspace(1, "a", vec![big_tab(2, 100..170, 100_000)])]);
        assert_eq!(l, Layout::new().0);
        assert!(matches!(status, LoadStatus::Reset(_)), "got {status:?}");
    }

    #[test]
    fn once_256_panes_are_kept_later_tabs_and_workspaces_are_dropped() {
        let mut tabs = Vec::new();
        for k in 0..5u64 {
            tabs.push(big_tab(10 + k, 1000 * (k + 1)..1000 * (k + 1) + 64, 1_000_000 * (k + 1)));
        }
        tabs.push(leaf_tab(20, 50_000));
        let (l, status) = load(vec![workspace(1, "a", tabs), workspace(2, "b", vec![leaf_tab(30, 60_000)])]);
        assert_eq!(l.all_panes().len(), 256);
        assert_eq!(l.workspaces.len(), 1);
        assert_eq!(l.workspaces[0].tabs.len(), 4);
        assert!(matches!(status, LoadStatus::Repaired(_)), "got {status:?}");
    }

    #[test]
    fn names_and_titles_are_cut_and_lose_control_characters() {
        let long = "x".repeat(300);
        let nasty = format!("a\u{0}b\u{1b}[31mc\n{long}");
        let mut tab = leaf_tab(2, 3);
        tab.title = nasty.clone();
        let (l, status) = load(vec![workspace(1, &nasty, vec![tab])]);
        for text in [&l.workspaces[0].name, &l.workspaces[0].tabs[0].title] {
            assert_eq!(text.chars().count(), 128);
            assert!(text.starts_with("ab[31mc"), "{text:?}");
            assert!(!text.chars().any(char::is_control));
        }
        assert!(matches!(status, LoadStatus::Repaired(_)), "got {status:?}");
    }

    #[test]
    fn a_chain_of_splits_deeper_than_32_is_dropped_on_load() {
        let deep = Tab { id: 4, title: "deep".into(), root: chain(500, 33, 9000), focus: 500 };
        let (l, status) = load(vec![workspace(1, "a", vec![leaf_tab(2, 10), deep])]);
        assert_eq!(l.all_panes(), vec![10]);
        assert!(matches!(status, LoadStatus::Repaired(_)), "got {status:?}");

        let ok = Tab { id: 4, title: "ok".into(), root: chain(500, 32, 9000), focus: 500 };
        let (l, status) = load(vec![workspace(1, "a", vec![leaf_tab(2, 10), ok])]);
        assert_eq!(l.all_panes().len(), 1 + 33);
        assert_eq!(status, LoadStatus::Clean);
    }

    #[test]
    fn nesting_beyond_the_json_recursion_limit_is_a_reset_not_a_crash() {
        let deep = Tab { id: 4, title: "deep".into(), root: chain(500, 90, 9000), focus: 500 };
        let (l, status) = load(vec![workspace(1, "a", vec![leaf_tab(2, 10), deep])]);
        assert_eq!(l, Layout::new().0);
        assert!(matches!(status, LoadStatus::Reset(_)), "got {status:?}");

        // A hostile file far deeper than anything the serialiser would write.
        let text = format!("{}1{}", r#"{"Split":{"a":"#.repeat(100_000), "}}".repeat(100_000));
        let (l, status) = Layout::from_json(&text);
        assert_eq!(l, Layout::new().0);
        assert!(matches!(status, LoadStatus::Reset(_)), "got {status:?}");
    }

    // ---- bounded operations ------------------------------------------------------------------------

    #[test]
    fn splitting_the_newest_pane_again_and_again_stops_at_depth_32_and_still_reloads() {
        let (mut l, _) = Layout::new();
        for n in 1..=32 {
            assert!(l.split_focused(Axis::Row).is_some(), "split {n}");
        }
        assert_eq!(depth(&l.tab().root), 32);
        let before = l.clone();
        assert_eq!(l.split_focused(Axis::Row), None, "the 33rd nested split");
        assert_eq!(l, before);
        // Splitting another leaf of the same tab is allowed while the depth stays within the limit.
        l.focus_pane(l.tab().panes()[0]);
        assert!(l.split_focused(Axis::Column).is_some());

        let (back, status) = Layout::from_json(&serde_json::to_string(&l).unwrap());
        assert_eq!(status, LoadStatus::Clean, "what the operations make, the loader accepts");
        assert_eq!(back, l);
    }

    #[test]
    fn split_focused_refuses_when_the_focus_is_not_in_the_tree() {
        let (mut l, _) = Layout::new();
        l.workspaces[0].tabs[0].focus = 999;
        let before = l.clone();
        assert_eq!(l.split_focused(Axis::Row), None);
        assert_eq!(l, before, "nothing changed, not even the id counter");
    }

    /// Split every pane of the active tab once per round, so the tree grows level by level (depth 6 for 64
    /// panes) instead of as a chain, until it holds `n` panes.
    fn fill_tab(l: &mut Layout, n: usize) {
        while l.tab().panes().len() < n {
            for pane in l.tab().panes() {
                if l.tab().panes().len() >= n {
                    break;
                }
                l.focus_pane(pane);
                assert!(l.split_focused(Axis::Row).is_some());
            }
        }
    }

    #[test]
    fn a_tab_stops_at_64_panes_and_the_layout_at_256() {
        let (mut l, _) = Layout::new();
        fill_tab(&mut l, 64);
        assert_eq!(l.tab().panes().len(), 64);
        let before = l.clone();
        assert_eq!(l.split_focused(Axis::Column), None, "65th pane in a tab");
        assert_eq!(l, before);

        for _ in 0..3 {
            assert!(l.new_tab().is_some());
            fill_tab(&mut l, 64);
        }
        assert_eq!(l.all_panes().len(), 256);
        let before = l.clone();
        assert_eq!(l.new_tab(), None, "tab over the total cap");
        assert_eq!(l.new_workspace("more"), None, "workspace over the total cap");
        assert_eq!(l.split_focused(Axis::Row), None, "split over the total cap");
        assert_eq!(l, before);
        assert!(l.validate().is_empty());
    }

    #[test]
    fn set_ratio_ignores_values_that_are_not_numbers() {
        let (mut l, _) = Layout::new();
        l.split_focused(Axis::Row).unwrap();
        let split = l.tab().geometry().1[0].split;
        l.set_ratio(split, 0.3);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            l.set_ratio(split, bad);
            assert_eq!(l.tab().geometry().1[0].ratio, 0.3, "{bad} must not move the divider");
        }
        l.set_ratio(split, 40.0);
        assert_eq!(l.tab().geometry().1[0].ratio, 1.0 - MIN_RATIO);
        assert!(l.validate().is_empty());
    }

    #[test]
    fn names_given_to_new_workspaces_are_sanitised_like_loaded_ones() {
        let (mut l, _) = Layout::new();
        l.new_workspace("a\nb\u{7f}c").unwrap();
        assert_eq!(l.workspace().name, "abc");
        l.new_workspace(&"y".repeat(500)).unwrap();
        assert_eq!(l.workspace().name.chars().count(), 128);
        assert!(l.validate().is_empty());
    }

    // ---- randomized invariants ---------------------------------------------------------------------

    /// xorshift64: a reproducible sequence from a seed, so a failing seed can be replayed exactly.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// Everything a valid layout promises, checked from the outside. `deep` adds the JSON round trip and a
    /// full `validate()`, which cost a thousand times the rest; the random test runs them every 100 steps and after the last.
    fn assert_invariants(l: &Layout, context: &str, deep: bool) {
        let mut panes = l.all_panes();
        let total = panes.len();
        panes.sort_unstable();
        panes.dedup();
        assert_eq!(panes.len(), total, "{context}: a pane id appears twice");
        assert!((1..=256).contains(&total), "{context}: {total} panes");
        assert!(!l.workspaces.is_empty(), "{context}: no workspaces");
        assert!(l.active < l.workspaces.len(), "{context}: active workspace out of range");
        for w in &l.workspaces {
            assert!(!w.tabs.is_empty(), "{context}: empty workspace");
            assert!(w.active < w.tabs.len(), "{context}: active tab out of range");
            for t in &w.tabs {
                let in_tab = t.panes();
                assert!(in_tab.len() <= 64, "{context}: {} panes in a tab", in_tab.len());
                assert!(depth(&t.root) <= 32, "{context}: split depth {}", depth(&t.root));
                assert!(in_tab.contains(&t.focus), "{context}: focus {} not in its tab", t.focus);
                let (rects, dividers) = t.geometry();
                let area: f32 = rects.iter().map(|(_, r)| r.w * r.h).sum();
                assert!((area - 1.0).abs() < 1e-3, "{context}: tab area {area}");
                assert!(dividers.iter().all(|d| d.ratio.is_finite()), "{context}: ratio not finite");
            }
        }
        if deep {
            let json = serde_json::to_string(l).unwrap();
            let back: Layout = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, l, "{context}: JSON round trip differs");
            let (loaded, status) = Layout::from_json(&json);
            assert_eq!(status, LoadStatus::Clean, "{context}: the loader repairs a layout made by operations");
            assert_eq!(&loaded, l);
            assert!(l.clone().validate().is_empty(), "{context}: validate repairs a layout made by operations");
        }
    }

    /// What the user is looking at, by identity: the shown workspace and, for every workspace, its shown tab.
    struct Shown {
        workspace: u64,
        tabs: Vec<(u64, u64)>,
    }

    impl Shown {
        fn of(l: &Layout) -> Shown {
            let tabs = l.workspaces.iter().map(|w| (w.id, w.tabs[w.active].id)).collect();
            Shown { workspace: l.workspace().id, tabs }
        }

        /// What was shown before is still shown, as far as it still exists: an operation that does not select
        /// anything on purpose may remove the shown workspace or tab (the next one is shown then) but must never
        /// move the shown one to a neighbour.
        fn assert_kept(&self, l: &Layout, context: &str) {
            if l.workspaces.iter().any(|w| w.id == self.workspace) {
                assert_eq!(l.workspace().id, self.workspace, "{context}: the shown workspace changed");
            }
            for (workspace, tab) in &self.tabs {
                let Some(w) = l.workspaces.iter().find(|w| w.id == *workspace) else { continue };
                if w.tabs.iter().any(|t| t.id == *tab) {
                    assert_eq!(w.tabs[w.active].id, *tab, "{context}: workspace {workspace} shows another tab");
                }
            }
        }
    }

    #[test]
    fn random_operations_never_leave_an_invalid_layout() {
        for seed in 1..=20u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let (mut l, _) = Layout::new();
            for step in 0..2000 {
                let shown = Shown::of(&l);
                let panes = l.all_panes();
                let pick = |rng: &mut Rng| panes[rng.below(panes.len())];
                let op = rng.below(100);
                let name = match op {
                    0..=29 => {
                        let axis = if rng.below(2) == 0 { Axis::Row } else { Axis::Column };
                        let _ = l.split_focused(axis);
                        "split"
                    }
                    30..=49 => {
                        let target = pick(&mut rng);
                        let _ = l.close_pane(target);
                        "close"
                    }
                    50..=54 => {
                        // An id that is in no tab: must be a no-op.
                        let _ = l.close_pane(1_000_000 + rng.next() % 1000);
                        "close unknown"
                    }
                    55..=62 => {
                        let _ = l.new_tab();
                        "new_tab"
                    }
                    63..=66 => {
                        let _ = l.new_workspace("w\n\u{1b}");
                        "new_workspace"
                    }
                    67..=76 => {
                        let target = pick(&mut rng);
                        l.focus_pane(target);
                        "focus"
                    }
                    77..=80 => {
                        l.cycle_focus(if rng.below(2) == 0 { 1 } else { -1 });
                        "cycle"
                    }
                    81..=92 => {
                        let splits: Vec<SplitId> = l.tab().geometry().1.iter().map(|d| d.split).collect();
                        if !splits.is_empty() {
                            let split = splits[rng.below(splits.len())];
                            let value = match rng.below(6) {
                                0 => f32::NAN,
                                1 => f32::INFINITY,
                                2 => f32::NEG_INFINITY,
                                3 => -(rng.below(1000) as f32),
                                4 => rng.below(1000) as f32,
                                _ => rng.below(1000) as f32 / 1000.0,
                            };
                            l.set_ratio(split, value);
                        }
                        "set_ratio"
                    }
                    93..=96 => {
                        l.select_tab(rng.below(80));
                        "select_tab"
                    }
                    _ => {
                        l.select_workspace(rng.below(80));
                        "select_workspace"
                    }
                };
                let context = format!("seed {seed} step {step} after {name}");
                assert_invariants(&l, &context, step % 100 == 99);
                // Split, close, a close of an unknown id, cycling the focus and moving a divider must keep what is
                // shown. Select, focus, new tab and new workspace change it on purpose and are exempt.
                if matches!(name, "split" | "close" | "close unknown" | "cycle" | "set_ratio") {
                    shown.assert_kept(&l, &context);
                }
            }
        }
    }

    // ---- names (DESK-32) -----------------------------------------------------------------------------

    fn two_tab_layout() -> (Layout, PaneId, PaneId) {
        let (mut l, first) = Layout::new();
        let second = l.split_focused(Axis::Row).expect("split");
        (l, first, second)
    }

    #[test]
    fn a_name_is_cleaned_cut_and_cleared_by_the_empty_text() {
        let (mut l, first, second) = two_tab_layout();
        assert!(l.rename_pane(first, "api\u{1b}[31m server"));
        assert_eq!(l.pane_name(first), Some("api[31m server"));
        assert!(l.rename_pane(first, &"x".repeat(500)));
        assert_eq!(l.pane_name(first).map(|n| n.chars().count()), Some(MAX_TEXT_CHARS));
        assert!(l.rename_pane(first, ""));
        assert_eq!(l.pane_name(first), None, "the empty text gives the automatic title back");
        assert_eq!(l.pane_name(second), None);
    }

    #[test]
    fn tabs_and_workspaces_are_renamed_by_index_and_the_empty_text_restores_the_default() {
        let (mut l, _) = Layout::new();
        l.new_tab();
        assert!(l.rename_tab(1, "website"));
        assert_eq!(l.workspace().tabs[1].title, "website");
        assert!(l.rename_tab(1, "  "), "blank text counts as empty");
        assert_eq!(l.workspace().tabs[1].title, "shell");
        assert!(!l.rename_tab(9, "x"), "an index that is not there changes nothing");
        assert!(l.rename_workspace(0, "holzWave"));
        assert_eq!(l.workspaces[0].name, "holzWave");
        assert!(l.rename_workspace(0, ""));
        assert_eq!(l.workspaces[0].name, "ws 1");
        assert!(!l.rename_workspace(5, "x"));
    }

    #[test]
    fn an_unknown_pane_is_not_renamed_and_a_closed_pane_loses_its_name() {
        let (mut l, first, second) = two_tab_layout();
        assert!(!l.rename_pane(999, "ghost"));
        assert!(l.rename_pane(second, "tests"));
        assert_eq!(l.close_pane(second), CloseOutcome::Closed);
        assert_eq!(l.pane_name(second), None);
        assert!(l.rename_pane(first, "main"));
        assert_eq!(l.pane_name(first), Some("main"), "closing another pane keeps a name");
    }

    #[test]
    fn splitting_keeps_names_and_gives_the_new_pane_none() {
        let (mut l, first, _) = two_tab_layout();
        assert!(l.rename_pane(first, "db"));
        let third = l.split_focused(Axis::Column).expect("split");
        assert_eq!(l.pane_name(first), Some("db"));
        assert_eq!(l.pane_name(third), None);
    }

    #[test]
    fn a_layout_without_names_has_no_name_field_and_an_old_file_loads_clean() {
        let (l, _) = Layout::new();
        let json = serde_json::to_string(&l).expect("json");
        assert!(!json.contains("pane_names"), "{json}");
        let (back, status) = Layout::from_json(&json);
        assert_eq!(status, LoadStatus::Clean);
        assert_eq!(back, l);
    }

    #[test]
    fn names_survive_a_round_trip_and_damaged_names_are_repaired() {
        let (mut l, first, _) = two_tab_layout();
        l.rename_pane(first, "docs");
        let (back, status) = Layout::from_json(&serde_json::to_string(&l).expect("json"));
        assert_eq!(status, LoadStatus::Clean);
        assert_eq!(back.pane_name(first), Some("docs"));

        let mut value: serde_json::Value = serde_json::to_value(&l).expect("value");
        value["pane_names"] = serde_json::json!({ first.to_string(): "bad\u{7}name", "9999": "no such pane" });
        let (fixed, status) = Layout::from_json(&value.to_string());
        assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
        assert_eq!(fixed.pane_name(first), Some("badname"));
        assert_eq!(fixed.pane_name(9999), None, "a name of a pane that is not there is dropped");
    }

    fn sorted(mut v: Vec<PaneId>) -> Vec<PaneId> {
        v.sort_unstable();
        v
    }

    #[test]
    fn a_pane_dropped_on_a_side_splits_the_target_there() {
        let (mut l, a) = Layout::new();
        let b = l.split_focused(Axis::Row).unwrap();
        let c = l.split_focused(Axis::Row).unwrap();
        // a | (b | c): drop a on the bottom of c.
        assert!(l.move_pane(a, c, Edge::Bottom));
        assert_eq!(sorted(l.tab().panes()), sorted(vec![a, b, c]));
        assert_eq!(l.tab().focus, a, "the moved pane is focused");
        let (rects, _) = l.tab().geometry();
        let rect = |p| rects.iter().find(|(q, _)| *q == p).map(|(_, r)| *r).unwrap();
        assert!(rect(a).y > rect(c).y, "a sits below c");
        assert!((rect(a).x - rect(c).x).abs() < 1e-6);
    }

    #[test]
    fn left_and_top_put_the_moved_pane_first() {
        let (mut l, a) = Layout::new();
        let b = l.split_focused(Axis::Row).unwrap();
        assert!(l.move_pane(b, a, Edge::Left));
        assert_eq!(l.tab().panes(), vec![b, a]);
        assert!(l.move_pane(b, a, Edge::Top));
        let (rects, _) = l.tab().geometry();
        let y = |p| rects.iter().find(|(q, _)| *q == p).map(|(_, r)| r.y).unwrap();
        assert!(y(b) < y(a));
    }

    #[test]
    fn the_middle_swaps_two_panes_and_keeps_names_and_ids() {
        let (mut l, a) = Layout::new();
        let b = l.split_focused(Axis::Row).unwrap();
        l.rename_pane(a, "db");
        assert!(l.move_pane(a, b, Edge::Center));
        assert_eq!(l.tab().panes(), vec![b, a]);
        assert_eq!(l.pane_name(a), Some("db"));
        assert_eq!(l.tab().focus, a);
    }

    #[test]
    fn a_pane_moves_across_tabs_and_an_emptied_tab_goes() {
        let (mut l, a) = Layout::new();
        let b = l.new_tab().unwrap();
        l.rename_pane(b, "web");
        assert_eq!(l.workspace().tabs.len(), 2);
        assert!(l.move_pane(b, a, Edge::Right));
        assert_eq!(l.workspace().tabs.len(), 1, "the tab b left is empty and gone");
        assert_eq!(l.tab().panes(), vec![a, b]);
        assert_eq!(l.pane_name(b), Some("web"), "the name follows the pane");
        assert_eq!(l.tab().focus, b);
    }

    #[test]
    fn a_pane_moves_to_another_workspace_and_the_shown_one_follows_it() {
        let (mut l, a) = Layout::new();
        let w = l.new_workspace("work").unwrap();
        l.select_workspace(0);
        assert!(l.move_pane(a, w, Edge::Left));
        assert_eq!(l.workspaces.len(), 1, "workspace 0 had only a and is gone");
        assert_eq!(l.active, 0);
        assert_eq!(l.tab().panes(), vec![a, w]);
    }

    #[test]
    fn swapping_across_tabs_exchanges_the_places() {
        let (mut l, a) = Layout::new();
        let b = l.new_tab().unwrap();
        assert!(l.swap_panes(a, b));
        assert_eq!(l.workspace().tabs[0].root, Node::Pane(b));
        assert_eq!(l.workspace().tabs[1].root, Node::Pane(a));
        assert_eq!(l.workspace().tabs[0].focus, b);
    }

    #[test]
    fn bad_moves_change_nothing() {
        let (mut l, a) = Layout::new();
        let b = l.split_focused(Axis::Row).unwrap();
        let before = l.clone();
        assert!(!l.move_pane(a, a, Edge::Left), "onto itself");
        assert!(!l.move_pane(a, 999, Edge::Left), "unknown target");
        assert!(!l.move_pane(999, b, Edge::Left), "unknown pane");
        assert!(!l.swap_panes(a, a));
        assert_eq!(l, before);
    }

    #[test]
    fn a_move_that_passes_the_depth_limit_is_refused() {
        let (mut l, _) = Layout::new();
        let mut deepest = 0;
        while let Some(p) = l.split_focused(Axis::Row) {
            deepest = p;
        }
        let loose = l.new_tab().unwrap();
        let before = l.clone();
        assert!(!l.move_pane(loose, deepest, Edge::Right), "the target is as deep as a pane may be");
        assert_eq!(l, before, "a refused move leaves everything as it was, the loose tab included");
    }

    #[test]
    fn moves_never_lose_or_double_a_pane() {
        let (mut l, a) = Layout::new();
        let b = l.split_focused(Axis::Row).unwrap();
        let c = l.split_focused(Axis::Column).unwrap();
        let d = l.new_tab().unwrap();
        let all = sorted(vec![a, b, c, d]);
        let edges = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom, Edge::Center];
        for (i, edge) in edges.iter().cycle().take(40).enumerate() {
            let (from, to) = (all[i % 4], all[(i * 3 + 1) % 4]);
            l.move_pane(from, to, *edge);
            assert_eq!(sorted(l.all_panes()), all, "after move {i}");
            let mut copy = l.clone();
            assert!(copy.validate().is_empty(), "the layout stays valid after move {i}");
        }
    }

    #[test]
    fn a_tab_moves_within_the_workspace_and_the_shown_tab_stays_shown() {
        let (mut l, a) = Layout::new();
        let b = l.new_tab().unwrap();
        let c = l.new_tab().unwrap();
        l.select_tab(0);
        assert!(l.move_tab(0, 2));
        let order: Vec<_> = l.workspace().tabs.iter().map(|t| t.focus).collect();
        assert_eq!(order, vec![b, c, a]);
        assert_eq!(l.tab().focus, a, "the moved tab was the shown one");
        assert!(l.move_tab(0, 1));
        assert_eq!(l.tab().focus, a, "the shown tab stays shown when others move around it");
        assert!(!l.move_tab(0, 9));
    }

    #[test]
    fn a_tab_moves_to_another_workspace() {
        let (mut l, a) = Layout::new();
        let b = l.new_tab().unwrap();
        let w = l.new_workspace("work").unwrap();
        l.select_workspace(0);
        assert!(l.move_tab_to_workspace(1, 1));
        assert_eq!(l.workspaces[0].tabs.len(), 1);
        assert_eq!(l.workspaces[1].tabs.len(), 2);
        assert_eq!(l.active, 1);
        assert_eq!(l.tab().focus, b);
        assert_eq!(sorted(l.all_panes()), sorted(vec![a, b, w]));
        assert!(!l.move_tab_to_workspace(0, 1), "not onto the shown workspace");
        // The only tab of workspace 0 leaves: the workspace goes and the target keeps its identity.
        l.select_workspace(0);
        assert!(l.move_tab_to_workspace(0, 1));
        assert_eq!(l.workspaces.len(), 1);
        assert_eq!(l.workspace().tabs.len(), 3);
    }
}
