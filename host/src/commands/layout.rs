//! The plain functions of the layout, window, tab and workspace commands. Each takes the core, the label of the calling
//! window (the key of its layout; the page never sends it) and the arguments of the command, and answers the new view.

use holzwave_core::layout::{Edge, PaneId, SplitId};

use crate::state::{AppCore, Released, View};
use crate::windows::DropKind;

/// The layout of the calling window. An unknown window is the error `no such window`.
pub fn get_layout(core: &AppCore, window: &str) -> Result<View, String> {
    core.view_in(window)
}

/// Split the focused pane; `column` stacks the panes, otherwise they sit side by side.
pub fn split(core: &AppCore, window: &str, column: bool) -> Result<View, String> {
    core.split_in(window, column)
}

pub fn new_tab(core: &AppCore, window: &str) -> Result<View, String> {
    core.new_tab_in(window)
}

pub fn new_workspace(core: &AppCore, window: &str, name: String) -> Result<View, String> {
    core.new_workspace_in(window, &name)
}

/// Close a pane of the calling window and kill its shell. The last pane of a window closes the window, and only the
/// last pane of the last window ends the app. Async because that exit path writes the layout.
pub fn close_pane(core: &AppCore, window: &str, pane: PaneId) -> Result<View, String> {
    core.close_pane_in(window, pane)
}

/// Close every pane of the tab at `index` of the shown workspace of the calling window (the × of a tab) and kill their
/// shells. A tab that is all of its window closes the window; one that is the whole layout ends the app. Async for
/// the same reasons as `close_pane`.
pub fn close_tab(core: &AppCore, window: &str, index: usize) -> Result<View, String> {
    core.close_tab_in(window, index)
}

/// Like `close_tab` for the workspace at `index` of the calling window (the × of a workspace chip).
pub fn close_workspace(core: &AppCore, window: &str, index: usize) -> Result<View, String> {
    core.close_workspace_in(window, index)
}

pub fn focus_pane(core: &AppCore, window: &str, pane: PaneId) -> Result<View, String> {
    core.focus_pane_in(window, pane)
}

pub fn cycle_focus(core: &AppCore, window: &str, step: i32) -> Result<View, String> {
    core.cycle_focus_in(window, step)
}

pub fn select_tab(core: &AppCore, window: &str, index: usize) -> Result<View, String> {
    core.select_tab_in(window, index)
}

pub fn select_workspace(core: &AppCore, window: &str, index: usize) -> Result<View, String> {
    core.select_workspace_in(window, index)
}

/// Name a pane. The empty text gives back the program title; the core cleans and cuts the text.
pub fn rename_pane(core: &AppCore, window: &str, pane: PaneId, name: String) -> Result<View, String> {
    core.rename_pane_in(window, pane, &name)
}

/// Name the tab at `index` of the shown workspace. The empty text gives back `shell`.
pub fn rename_tab(core: &AppCore, window: &str, index: usize, name: String) -> Result<View, String> {
    core.rename_tab_in(window, index, &name)
}

/// Name the workspace at `index`. The empty text gives back `ws N`.
pub fn rename_workspace(core: &AppCore, window: &str, index: usize, name: String) -> Result<View, String> {
    core.rename_workspace_in(window, index, &name)
}

/// Drop a pane on the side or the middle of another in the calling window (DESK-20). The core checks both ids and
/// the limits.
pub fn move_pane(core: &AppCore, window: &str, pane: PaneId, target: PaneId, edge: Edge) -> Result<View, String> {
    core.move_pane_in(window, pane, target, edge)
}

/// Drop a pane on the chip of a workspace of the calling window: it goes next to the focus pane of that workspace's
/// shown tab. The core checks the pane, the workspace and the limits.
pub fn move_pane_to_workspace(core: &AppCore, window: &str, pane: PaneId, workspace: usize) -> Result<View, String> {
    core.move_pane_to_workspace_in(window, pane, workspace)
}

/// Move a tab to another place in the shown workspace.
pub fn move_tab(core: &AppCore, window: &str, from: usize, to: usize) -> Result<View, String> {
    core.move_tab_in(window, from, to)
}

/// Move a tab of the shown workspace into another workspace.
pub fn move_tab_to_workspace(core: &AppCore, window: &str, tab: usize, workspace: usize) -> Result<View, String> {
    core.move_tab_to_workspace_in(window, tab, workspace)
}

/// Move a divider. Only marks the layout dirty: a debounced saver writes it, so a drag costs one write per window.
pub fn set_ratio(core: &AppCore, window: &str, split: SplitId, ratio: f32) -> Result<View, String> {
    core.set_ratio_in(window, split, ratio)
}

/// Give a pane of the calling window a window of its own: under the pointer (`at_cursor`), or beside this window
/// (the palette action). A pane that is alone in its window is refused. Answers the view of the calling window.
pub fn tear_off_pane(core: &AppCore, window: &str, pane: PaneId, at_cursor: bool) -> Result<View, String> {
    core.tear_off_pane_in(window, pane, at_cursor)
}

/// Like `tear_off_pane` for the whole tab the pane is in. The tab is named by any of its panes.
pub fn tear_off_tab(core: &AppCore, window: &str, pane: PaneId, at_cursor: bool) -> Result<View, String> {
    core.tear_off_tab_in(window, pane, at_cursor)
}

/// A pane (`kind` `pane`) or the tab of a pane (`kind` `tab`) was released outside the calling window. The shell
/// reads the pointer: over another window of the app that window is told with `incoming-drop`, over nothing the pane
/// or tab gets a window under the pointer. Answers `{outcome: "torn_off", label}`, `{outcome: "handed", to}` or
/// `{outcome: "cancelled"}`.
pub fn release_outside(core: &AppCore, window: &str, pane: PaneId, kind: DropKind) -> Result<Released, String> {
    core.release_outside_in(window, pane, kind)
}

/// The calling window is dragging a pane (`kind` `pane`) or the tab of a pane (`kind` `tab`) and the pointer is
/// outside it. The shell reads the pointer and tells the window under it to show the outline of the zone it would
/// land in (`drag-hover`); a window the drag left is told `drag-end`. Moves nothing.
pub fn drag_over(core: &AppCore, window: &str, pane: PaneId, kind: DropKind) -> Result<(), String> {
    core.drag_over_in(window, pane, kind)
}

/// The drag of the calling window ended (released anywhere, Escape, the pointer came back inside): the window it was
/// over takes its outline away. Nothing to tell is no error.
pub fn drag_end(core: &AppCore, window: &str) -> Result<(), String> {
    core.drag_end_in(window)
}

/// The calling window answers an `incoming-drop` of a pane: onto the pane `target` at `edge`, or as a tab of
/// `workspace`. Only the drop that is in flight for this pane counts. Answers the view of the calling window.
pub fn attach_pane(
    core: &AppCore,
    window: &str,
    pane: PaneId,
    target: Option<PaneId>,
    edge: Option<Edge>,
    workspace: Option<usize>,
) -> Result<View, String> {
    core.attach_pane_in(window, pane, target, edge, workspace)
}

/// The calling window answers an `incoming-drop` of a tab: as the last tab of `workspace`. Answers the view of the
/// calling window.
pub fn attach_tab(core: &AppCore, window: &str, pane: PaneId, workspace: usize) -> Result<View, String> {
    core.attach_tab_in(window, pane, workspace)
}

/// The page of the calling window has applied its first layout: it is told the last title and directory of its
/// panes. The desktop shows and focuses a window that was created hidden after this; that part needs the window and
/// stays with the desktop's command.
pub fn window_ready(core: &AppCore, window: &str) -> Result<(), String> {
    core.window_ready_in(window)
}
