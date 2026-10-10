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
    pub fn split_focused(&mut self, axis: Axis) -> PaneId {
        let (split_id, new_pane) = (self.id(), self.id());
        let tab = self.tab_mut();
        let focus = tab.focus;
        tab.root.split_pane(focus, axis, split_id, new_pane);
        tab.focus = new_pane;
        new_pane
    }

    /// Open a new tab in the active workspace and focus it. Returns its pane.
    pub fn new_tab(&mut self) -> PaneId {
        let (tab_id, pane) = (self.id(), self.id());
        let w = self.workspace_mut();
        w.tabs.push(Tab { id: tab_id, title: "shell".into(), root: Node::Pane(pane), focus: pane });
        w.active = w.tabs.len() - 1;
        pane
    }

    /// Add a workspace with one tab and switch to it. Returns its pane.
    pub fn new_workspace(&mut self, name: &str) -> PaneId {
        let (ws, tab, pane) = (self.id(), self.id(), self.id());
        self.workspaces.push(Workspace {
            id: ws,
            name: name.into(),
            tabs: vec![Tab { id: tab, title: "shell".into(), root: Node::Pane(pane), focus: pane }],
            active: 0,
        });
        self.active = self.workspaces.len() - 1;
        pane
    }

    /// Close a pane everywhere. Empty tabs and empty workspaces disappear; the last pane of the last
    /// workspace cannot be closed and the call returns `false`.
    pub fn close_pane(&mut self, pane: PaneId) -> bool {
        if self.all_panes().len() <= 1 {
            return false;
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
            return true;
        }
        false
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
        let second = l.split_focused(Axis::Row);
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
        let b = l.split_focused(Axis::Row);
        let c = l.split_focused(Axis::Column);
        assert_eq!(l.tab().panes(), vec![a, b, c]);
        assert!(l.close_pane(b));
        assert_eq!(l.tab().panes(), vec![a, c]);
        assert!(l.close_pane(c));
        assert_eq!(l.tab().root, Node::Pane(a));
    }

    #[test]
    fn the_last_pane_cannot_be_closed() {
        let (mut l, only) = Layout::new();
        assert!(!l.close_pane(only));
        assert_eq!(l.all_panes(), vec![only]);
    }

    #[test]
    fn closing_the_only_pane_of_a_tab_removes_the_tab() {
        let (mut l, first) = Layout::new();
        let second = l.new_tab();
        assert_eq!(l.workspace().tabs.len(), 2);
        assert!(l.close_pane(second));
        assert_eq!(l.workspace().tabs.len(), 1);
        assert_eq!(l.tab().focus, first);
    }

    #[test]
    fn workspaces_are_independent() {
        let (mut l, a) = Layout::new();
        let b = l.new_workspace("work");
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
        let b = l.split_focused(Axis::Row);
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
}
