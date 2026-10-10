//! The block layout: workspaces hold tabs, a tab holds a tree of panes split side by side or stacked.
//!
//! Pure data and geometry, with no terminal and no UI in it, so it is the same on every platform and
//! easy to test. The app persists it as JSON and asks it for rectangles in normalised 0..1 units.

use serde::{Deserialize, Serialize};

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
}

const MIN_RATIO: f32 = 0.1;

/// Ids stay below this. The webview receives ids as JSON numbers, and a JavaScript number holds integers
/// exactly only up to 2^53, so a larger id would come back as a different pane. A file with ids at or above
/// the limit (and with it `u64::MAX`, which `next_id` could never be raised above) is not repaired.
const ID_CEILING: u64 = 1 << 53;

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
        let mut l = Layout { workspaces: Vec::new(), active: 0, next_id: 1 };
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
    pub fn split_focused(&mut self, axis: Axis) -> Option<PaneId> {
        let (split_id, new_pane) = (self.id(), self.id());
        let tab = self.tab_mut();
        let focus = tab.focus;
        tab.root.split_pane(focus, axis, split_id, new_pane);
        tab.focus = new_pane;
        Some(new_pane)
    }

    /// Open a new tab in the active workspace and focus it. Returns its pane.
    pub fn new_tab(&mut self) -> Option<PaneId> {
        let (tab_id, pane) = (self.id(), self.id());
        let w = self.workspace_mut();
        w.tabs.push(Tab { id: tab_id, title: "shell".into(), root: Node::Pane(pane), focus: pane });
        w.active = w.tabs.len() - 1;
        Some(pane)
    }

    /// Add a workspace with one tab and switch to it. Returns its pane.
    pub fn new_workspace(&mut self, name: &str) -> Option<PaneId> {
        let (ws, tab, pane) = (self.id(), self.id(), self.id());
        self.workspaces.push(Workspace {
            id: ws,
            name: name.into(),
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
    fn check(&mut self) -> Result<Vec<String>, String> {
        let mut repairs = Vec::new();

        // Workspaces without tabs cannot be shown; the active index follows the workspace it pointed at.
        let (old_active, old_len) = (self.active, self.workspaces.len());
        let mut kept = Vec::with_capacity(old_len);
        let mut new_active = None;
        for (i, w) in std::mem::take(&mut self.workspaces).into_iter().enumerate() {
            if w.tabs.is_empty() {
                continue;
            }
            if i == old_active {
                new_active = Some(kept.len());
            }
            kept.push(w);
        }
        self.workspaces = kept;
        if self.workspaces.is_empty() {
            return Err("no workspace with a tab".into());
        }
        if self.workspaces.len() < old_len {
            repairs.push(format!("dropped {} workspace(s) without tabs", old_len - self.workspaces.len()));
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
            if w.active >= w.tabs.len() {
                let fixed = w.tabs.len() - 1;
                repairs.push(format!("workspace {wi}: active tab {} is out of range, set to {fixed}", w.active));
                w.active = fixed;
            }
            for (ti, tab) in w.tabs.iter_mut().enumerate() {
                let panes = tab.panes();
                if !panes.contains(&tab.focus) {
                    repairs.push(format!(
                        "workspace {wi} tab {ti}: focus {} is not in the tab, set to {}",
                        tab.focus, panes[0]
                    ));
                    tab.focus = panes[0];
                }
            }
        }

        // Id counter: every id in the file must be below next_id, or a new pane could repeat an old id.
        let mut max_id = 0;
        for w in &self.workspaces {
            max_id = max_id.max(w.id);
            for t in &w.tabs {
                max_id = max_id.max(t.id);
                let (mut panes, mut splits) = (Vec::new(), Vec::new());
                t.root.panes(&mut panes);
                t.root.split_ids(&mut splits);
                max_id = panes.into_iter().chain(splits).fold(max_id, u64::max);
            }
        }
        if max_id >= ID_CEILING - 1 || self.next_id >= ID_CEILING {
            return Err(format!("ids are out of range (largest {max_id}, next {})", self.next_id));
        }
        if self.next_id <= max_id {
            repairs.push(format!("next id {} raised to {}", self.next_id, max_id + 1));
            self.next_id = max_id + 1;
        }

        Ok(repairs)
    }

    /// Close a pane everywhere. Empty tabs and empty workspaces disappear; the last pane of the last
    /// workspace cannot be closed. An id that is in no tab is `Unknown`, which is checked first.
    pub fn close_pane(&mut self, pane: PaneId) -> CloseOutcome {
        if !self.workspaces.iter().any(|w| w.tabs.iter().any(|t| t.root.contains(pane))) {
            return CloseOutcome::Unknown;
        }
        if self.all_panes().len() <= 1 {
            return CloseOutcome::LastPane;
        }
        for wi in 0..self.workspaces.len() {
            let w = &mut self.workspaces[wi];
            let Some(ti) = w.tabs.iter().position(|t| t.root.contains(pane)) else { continue };
            let tab = &mut w.tabs[ti];
            if tab.root.remove(pane) {
                if tab.focus == pane {
                    tab.focus = tab.panes()[0];
                }
            } else {
                w.tabs.remove(ti);
                w.active = w.active.min(w.tabs.len().saturating_sub(1));
                if w.tabs.is_empty() {
                    self.workspaces.remove(wi);
                    self.active = self.active.min(self.workspaces.len() - 1);
                }
            }
            return CloseOutcome::Closed;
        }
        CloseOutcome::Unknown
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

    pub fn set_ratio(&mut self, split: SplitId, ratio: f32) {
        for w in &mut self.workspaces {
            for t in &mut w.tabs {
                if t.root.set_ratio(split, ratio) {
                    return;
                }
            }
        }
    }

    pub fn set_title(&mut self, pane: PaneId, title: &str) {
        for w in &mut self.workspaces {
            for t in &mut w.tabs {
                if t.root.contains(pane) && t.focus == pane {
                    t.title = title.into();
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
        let l = Layout { workspaces, active: 0, next_id: 1_000_000 };
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
            let mut l = Layout { workspaces: vec![workspace(1, "a", vec![tab])], active: 0, next_id: 100 };
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

    #[test]
    fn a_tab_stops_at_64_panes_and_the_layout_at_256() {
        let (mut l, _) = Layout::new();
        for _ in 0..63 {
            assert!(l.split_focused(Axis::Row).is_some());
        }
        assert_eq!(l.tab().panes().len(), 64);
        let before = l.clone();
        assert_eq!(l.split_focused(Axis::Column), None, "65th pane in a tab");
        assert_eq!(l, before);

        for _ in 0..3 {
            assert!(l.new_tab().is_some());
            for _ in 0..63 {
                assert!(l.split_focused(Axis::Row).is_some());
            }
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

    /// Everything a valid layout promises, checked from the outside.
    fn assert_invariants(l: &Layout, context: &str) {
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
        let json = serde_json::to_string(l).unwrap();
        let back: Layout = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, l, "{context}: JSON round trip differs");
        assert!(l.clone().validate().is_empty(), "{context}: validate repairs a layout made by operations");
    }

    #[test]
    fn random_operations_never_leave_an_invalid_layout() {
        for seed in 1..=20u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let (mut l, _) = Layout::new();
            for step in 0..2000 {
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
                assert_invariants(&l, &format!("seed {seed} step {step} after {name}"));
            }
        }
    }
}
