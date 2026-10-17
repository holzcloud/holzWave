//! The commands the webview can call: thin wrappers over [`AppCore`] (D-19) and the wire types of their arguments
//! and answers.
//!
//! Two conventions hold for every command here, and `state.rs` has a test that reads this file to keep them:
//!
//! - **snake_case argument names.** The command macro of the pinned tauri-macros turns `allow_unsafe` into
//!   `allowUnsafe` for the page unless `rename_all = "snake_case"` says otherwise, which would make `allow_unsafe`,
//!   `cell_w` and `cell_h` silently disappear. Every command carries the rename.
//! - **the calling window.** A command that works on a layout takes `window: WebviewWindow`, which Tauri fills in
//!   with the window that made the call (the page does not send it, so it is no argument of the protocol), and
//!   answers with that window's view. A command that names a pane by id (`key`, `resize`, ...) needs no window: pane
//!   ids are global. The label of a window is the key of its layout; an unknown one is the error `no such window`.
//! - **no waiting on the main thread.** A command without `async` runs on the main thread (RESEARCH Pitfall 10), so
//!   those only lock, mutate and push to a queue. A command that waits for the engine thread's answer (up to two
//!   seconds) or for a shell to start is `async`, which runs it off the main thread.
//!
//! The wire op types are internally tagged with `kind` and every variant, motion and mode name is snake_case, so the
//! page sends `{kind: "begin", x, y, mode: "word"}` and `{kind: "move", motion: "line_start"}`.

use std::sync::Arc;

use holzwave_core::ai::profiles::KeySlot;
use holzwave_core::ai::service::events::ChatState;
use holzwave_core::ai::service::{AskView, ContextState, OfferView};
use holzwave_core::ai::types::ContextKind;
use holzwave_core::blocks::{BlockKind, BlockView, Grant, Opened};
use holzwave_core::connections::{ConnView, ProfileView, SecretChange};
use holzwave_core::files::{Entry, Listing};
use holzwave_core::engine::{
    CopyMotion, CopyOp, CopyState, DomKey, Mods, MouseAction, MouseButton, PasteOutcome, SearchOp, SelectMode,
    SelectOp,
};
use holzwave_core::layout::{Edge, PaneId, SplitId};
use holzwave_core::pane::EditAction;
use holzwave_core::profiles::Profile;
use holzwave_core::wire::SearchMsg;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{State, WebviewWindow};

use crate::ai_host::{AiFault, AiProfiles, ConversationList, ProfileInputMsg, RemovalView, TicketView};
use crate::state::{AppCore, AppInfo, Released, SettingsView, View};
use crate::windows::DropKind;

// ---- wire types ------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectModeMsg {
    Cell,
    Word,
    Line,
}

impl From<SelectModeMsg> for SelectMode {
    fn from(mode: SelectModeMsg) -> Self {
        match mode {
            SelectModeMsg::Cell => SelectMode::Cell,
            SelectModeMsg::Word => SelectMode::Word,
            SelectModeMsg::Line => SelectMode::Line,
        }
    }
}

/// The line-editing steps the page can ask for by name: `"word_back"`, `"word_forward"`, `"line_start"`, `"line_end"`,
/// `"delete_word_back"`, `"delete_to_line_start"`. The core owns the bytes; an unknown name is refused by serde.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditActionMsg {
    WordBack,
    WordForward,
    LineStart,
    LineEnd,
    DeleteWordBack,
    DeleteToLineStart,
}

impl From<EditActionMsg> for EditAction {
    fn from(action: EditActionMsg) -> Self {
        match action {
            EditActionMsg::WordBack => EditAction::WordBack,
            EditActionMsg::WordForward => EditAction::WordForward,
            EditActionMsg::LineStart => EditAction::LineStart,
            EditActionMsg::LineEnd => EditAction::LineEnd,
            EditActionMsg::DeleteWordBack => EditAction::DeleteWordBack,
            EditActionMsg::DeleteToLineStart => EditAction::DeleteToLineStart,
        }
    }
}

/// `{kind: "begin", x, y, mode}`, `{kind: "extend", x, y}`, `{kind: "all"}`, `{kind: "clear"}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SelectOpMsg {
    Begin { x: u16, y: u16, mode: SelectModeMsg },
    Extend { x: u16, y: u16 },
    All,
    Clear,
}

impl From<SelectOpMsg> for SelectOp {
    fn from(op: SelectOpMsg) -> Self {
        match op {
            SelectOpMsg::Begin { x, y, mode } => SelectOp::Begin { x, y, mode: mode.into() },
            SelectOpMsg::Extend { x, y } => SelectOp::Extend { x, y },
            SelectOpMsg::All => SelectOp::All,
            SelectOpMsg::Clear => SelectOp::Clear,
        }
    }
}

/// `{kind: "set", needle}`, `{kind: "next"}`, `{kind: "prev"}`, `{kind: "clear"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchOpMsg {
    Set { needle: String },
    Next,
    Prev,
    Clear,
}

impl From<SearchOpMsg> for SearchOp {
    fn from(op: SearchOpMsg) -> Self {
        match op {
            SearchOpMsg::Set { needle } => SearchOp::Set(needle),
            SearchOpMsg::Next => SearchOp::Next,
            SearchOpMsg::Prev => SearchOp::Prev,
            SearchOpMsg::Clear => SearchOp::Clear,
        }
    }
}

/// `left|right|up|down|line_start|line_end|top|bottom|page_up|page_down|word_forward|word_back`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyMotionMsg {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
    Top,
    Bottom,
    PageUp,
    PageDown,
    WordForward,
    WordBack,
}

impl From<CopyMotionMsg> for CopyMotion {
    fn from(motion: CopyMotionMsg) -> Self {
        match motion {
            CopyMotionMsg::Left => CopyMotion::Left,
            CopyMotionMsg::Right => CopyMotion::Right,
            CopyMotionMsg::Up => CopyMotion::Up,
            CopyMotionMsg::Down => CopyMotion::Down,
            CopyMotionMsg::LineStart => CopyMotion::LineStart,
            CopyMotionMsg::LineEnd => CopyMotion::LineEnd,
            CopyMotionMsg::Top => CopyMotion::Top,
            CopyMotionMsg::Bottom => CopyMotion::Bottom,
            CopyMotionMsg::PageUp => CopyMotion::PageUp,
            CopyMotionMsg::PageDown => CopyMotion::PageDown,
            CopyMotionMsg::WordForward => CopyMotion::WordForward,
            CopyMotionMsg::WordBack => CopyMotion::WordBack,
        }
    }
}

/// `{kind: "enter"}`, `{kind: "exit"}`, `{kind: "toggle"}`, `{kind: "yank"}`, `{kind: "move", motion}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CopyOpMsg {
    Enter,
    Exit,
    Toggle,
    Yank,
    Move { motion: CopyMotionMsg },
}

impl From<CopyOpMsg> for CopyOp {
    fn from(op: CopyOpMsg) -> Self {
        match op {
            CopyOpMsg::Enter => CopyOp::Enter,
            CopyOpMsg::Exit => CopyOp::Exit,
            CopyOpMsg::Toggle => CopyOp::ToggleSelection,
            CopyOpMsg::Yank => CopyOp::Yank,
            CopyOpMsg::Move { motion } => CopyOp::Move(motion.into()),
        }
    }
}

/// What a paste did: `written`, `empty` or `rejected`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteOutcomeMsg {
    Written,
    Empty,
    Rejected,
}

impl From<PasteOutcome> for PasteOutcomeMsg {
    fn from(outcome: PasteOutcome) -> Self {
        match outcome {
            PasteOutcome::Written => PasteOutcomeMsg::Written,
            PasteOutcome::Empty => PasteOutcomeMsg::Empty,
            PasteOutcome::Rejected => PasteOutcomeMsg::Rejected,
        }
    }
}

/// What a pointer button did: `press`, `release` or `motion`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseActionMsg {
    Press,
    Release,
    Motion,
}

impl From<MouseActionMsg> for MouseAction {
    fn from(action: MouseActionMsg) -> Self {
        match action {
            MouseActionMsg::Press => MouseAction::Press,
            MouseActionMsg::Release => MouseAction::Release,
            MouseActionMsg::Motion => MouseAction::Motion,
        }
    }
}

/// Which button: `left`, `middle`, `right`, or `none` for a motion with nothing held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButtonMsg {
    Left,
    Middle,
    Right,
    None,
}

impl From<MouseButtonMsg> for MouseButton {
    fn from(button: MouseButtonMsg) -> Self {
        match button {
            MouseButtonMsg::Left => MouseButton::Left,
            MouseButtonMsg::Middle => MouseButton::Middle,
            MouseButtonMsg::Right => MouseButton::Right,
            MouseButtonMsg::None => MouseButton::None,
        }
    }
}

/// The state of copy mode after an operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CopyStateMsg {
    pub active: bool,
    pub selecting: bool,
    /// The text handed back by `yank`, `null` otherwise or when the selection held no text.
    pub yanked: Option<String>,
}

impl From<CopyState> for CopyStateMsg {
    fn from(state: CopyState) -> Self {
        CopyStateMsg { active: state.active, selecting: state.selecting, yanked: state.yanked }
    }
}

// ---- commands --------------------------------------------------------------------------------------------------

#[tauri::command(rename_all = "snake_case")]
pub fn get_layout(core: State<Arc<AppCore>>, window: WebviewWindow) -> Result<View, String> {
    core.view_in(window.label())
}

/// Start the shell of a pane if it has none yet. Waits for the actor's startup handshake.
#[tauri::command(async, rename_all = "snake_case")]
pub fn start_pane(
    core: State<Arc<AppCore>>,
    pane: PaneId,
    cols: u32,
    rows: u32,
    cell_w: Option<u32>,
    cell_h: Option<u32>,
) -> Result<(), String> {
    core.start_pane(pane, cols, rows, cell_w.unwrap_or(0), cell_h.unwrap_or(0))
}

/// A key press or release as the page saw it (`mods` is SHIFT 1, ALT 2, CTRL 4, SUPER 8). The engine encodes it.
#[allow(clippy::too_many_arguments)]
#[tauri::command(rename_all = "snake_case")]
pub fn key(
    core: State<Arc<AppCore>>,
    pane: PaneId,
    code: String,
    key: String,
    mods: u32,
    repeat: bool,
    release: bool,
    composing: bool,
) -> Result<(), String> {
    let dom = DomKey { code, key, mods: Mods::from_bits((mods & 0xFF) as u8), repeat, release, composing };
    core.key(pane, dom)
}

/// A line-editing step (word or line jump, delete word or line) for the shell's line editor. The page sends the
/// name of the step, never bytes. It asks for this only on the normal screen; a program on the alternate screen
/// gets the key through `key`.
#[tauri::command(rename_all = "snake_case")]
pub fn send_edit(core: State<Arc<AppCore>>, pane: PaneId, action: EditActionMsg) -> Result<(), String> {
    core.edit(pane, action.into())
}

/// Committed text (an IME commit, a dead-key result) as input.
#[tauri::command(rename_all = "snake_case")]
pub fn text(core: State<Arc<AppCore>>, pane: PaneId, text: String) -> Result<(), String> {
    core.text(pane, text)
}

/// Paste clipboard text. Waits for the engine's answer.
#[tauri::command(async, rename_all = "snake_case")]
pub fn paste(
    core: State<Arc<AppCore>>,
    pane: PaneId,
    text: String,
    allow_unsafe: bool,
) -> Result<PasteOutcomeMsg, String> {
    core.paste(pane, text, allow_unsafe).map(PasteOutcomeMsg::from)
}

#[tauri::command(rename_all = "snake_case")]
pub fn resize(
    core: State<Arc<AppCore>>,
    pane: PaneId,
    cols: u32,
    rows: u32,
    cell_w: Option<u32>,
    cell_h: Option<u32>,
) -> Result<(), String> {
    core.resize(pane, cols, rows, cell_w.unwrap_or(0), cell_h.unwrap_or(0))
}

#[tauri::command(rename_all = "snake_case")]
pub fn scroll(core: State<Arc<AppCore>>, pane: PaneId, rows: i32) -> Result<(), String> {
    core.scroll(pane, rows)
}

#[tauri::command(rename_all = "snake_case")]
pub fn scroll_to_bottom(core: State<Arc<AppCore>>, pane: PaneId) -> Result<(), String> {
    core.scroll_to_bottom(pane)
}

/// Make the next frame of the pane a full one (the page reconnected or saw a gap in the sequence numbers).
#[tauri::command(rename_all = "snake_case")]
pub fn request_full(core: State<Arc<AppCore>>, pane: PaneId) -> Result<(), String> {
    core.request_full(pane)
}

#[tauri::command(rename_all = "snake_case")]
pub fn select(core: State<Arc<AppCore>>, pane: PaneId, op: SelectOpMsg) -> Result<(), String> {
    core.select(pane, op.into())
}

/// The selected text, `null` when nothing is selected. Waits for the engine's answer.
#[tauri::command(async, rename_all = "snake_case")]
pub fn selection_text(core: State<Arc<AppCore>>, pane: PaneId) -> Result<Option<String>, String> {
    core.selection_text(pane)
}

/// Change the search and answer with its state. Waits for the engine's answer.
#[tauri::command(async, rename_all = "snake_case")]
pub fn search(core: State<Arc<AppCore>>, pane: PaneId, op: SearchOpMsg) -> Result<SearchMsg, String> {
    let state = core.search(pane, op.into())?;
    Ok(SearchMsg { total: state.total, current: state.current })
}

/// Run a copy-mode operation and answer with its state. Waits for the engine's answer.
#[tauri::command(async, rename_all = "snake_case")]
pub fn copy_mode(core: State<Arc<AppCore>>, pane: PaneId, op: CopyOpMsg) -> Result<CopyStateMsg, String> {
    core.copy_mode(pane, op.into()).map(CopyStateMsg::from)
}

// ---- layout commands: each answers with the new view -----------------------------------------------------------

/// Split the focused pane; `column` stacks the panes, otherwise they sit side by side.
#[tauri::command(rename_all = "snake_case")]
pub fn split(core: State<Arc<AppCore>>, window: WebviewWindow, column: bool) -> Result<View, String> {
    core.split_in(window.label(), column)
}

#[tauri::command(rename_all = "snake_case")]
pub fn new_tab(core: State<Arc<AppCore>>, window: WebviewWindow) -> Result<View, String> {
    core.new_tab_in(window.label())
}

#[tauri::command(rename_all = "snake_case")]
pub fn new_workspace(core: State<Arc<AppCore>>, window: WebviewWindow, name: String) -> Result<View, String> {
    core.new_workspace_in(window.label(), &name)
}

/// Close a pane of the calling window and kill its shell. The last pane of a window closes the window, and only the
/// last pane of the last window ends the app. Async because that exit path writes the layout.
#[tauri::command(async, rename_all = "snake_case")]
pub fn close_pane(core: State<Arc<AppCore>>, window: WebviewWindow, pane: PaneId) -> Result<View, String> {
    core.close_pane_in(window.label(), pane)
}

/// Close every pane of the tab at `index` of the shown workspace of the calling window (the × of a tab) and kill their
/// shells. A tab that is all of its window closes the window; one that is the whole layout ends the app. Async for
/// the same reasons as `close_pane`.
#[tauri::command(async, rename_all = "snake_case")]
pub fn close_tab(core: State<Arc<AppCore>>, window: WebviewWindow, index: usize) -> Result<View, String> {
    core.close_tab_in(window.label(), index)
}

/// Like `close_tab` for the workspace at `index` of the calling window (the × of a workspace chip).
#[tauri::command(async, rename_all = "snake_case")]
pub fn close_workspace(core: State<Arc<AppCore>>, window: WebviewWindow, index: usize) -> Result<View, String> {
    core.close_workspace_in(window.label(), index)
}

#[tauri::command(rename_all = "snake_case")]
pub fn focus_pane(core: State<Arc<AppCore>>, window: WebviewWindow, pane: PaneId) -> Result<View, String> {
    core.focus_pane_in(window.label(), pane)
}

#[tauri::command(rename_all = "snake_case")]
pub fn cycle_focus(core: State<Arc<AppCore>>, window: WebviewWindow, step: i32) -> Result<View, String> {
    core.cycle_focus_in(window.label(), step)
}

#[tauri::command(rename_all = "snake_case")]
pub fn select_tab(core: State<Arc<AppCore>>, window: WebviewWindow, index: usize) -> Result<View, String> {
    core.select_tab_in(window.label(), index)
}

#[tauri::command(rename_all = "snake_case")]
pub fn select_workspace(core: State<Arc<AppCore>>, window: WebviewWindow, index: usize) -> Result<View, String> {
    core.select_workspace_in(window.label(), index)
}

/// Move a divider. Only marks the layout dirty: a debounced saver writes it, so a drag costs one write per window.
/// Name a pane. The empty text gives back the program title; the core cleans and cuts the text.
#[tauri::command(rename_all = "snake_case")]
pub fn rename_pane(core: State<Arc<AppCore>>, window: WebviewWindow, pane: PaneId, name: String) -> Result<View, String> {
    core.rename_pane_in(window.label(), pane, &name)
}

/// Name the tab at `index` of the shown workspace. The empty text gives back `shell`.
#[tauri::command(rename_all = "snake_case")]
pub fn rename_tab(core: State<Arc<AppCore>>, window: WebviewWindow, index: usize, name: String) -> Result<View, String> {
    core.rename_tab_in(window.label(), index, &name)
}

/// Name the workspace at `index`. The empty text gives back `ws N`.
#[tauri::command(rename_all = "snake_case")]
pub fn rename_workspace(
    core: State<Arc<AppCore>>,
    window: WebviewWindow,
    index: usize,
    name: String,
) -> Result<View, String> {
    core.rename_workspace_in(window.label(), index, &name)
}

/// Drop a pane on the side or the middle of another in the calling window (DESK-20). The core checks both ids and
/// the limits.
#[tauri::command(rename_all = "snake_case")]
pub fn move_pane(
    core: State<Arc<AppCore>>,
    window: WebviewWindow,
    pane: PaneId,
    target: PaneId,
    edge: Edge,
) -> Result<View, String> {
    core.move_pane_in(window.label(), pane, target, edge)
}

/// Drop a pane on the chip of a workspace of the calling window: it goes next to the focus pane of that workspace's
/// shown tab. The core checks the pane, the workspace and the limits.
#[tauri::command(rename_all = "snake_case")]
pub fn move_pane_to_workspace(
    core: State<Arc<AppCore>>,
    window: WebviewWindow,
    pane: PaneId,
    workspace: usize,
) -> Result<View, String> {
    core.move_pane_to_workspace_in(window.label(), pane, workspace)
}

/// Move a tab to another place in the shown workspace.
#[tauri::command(rename_all = "snake_case")]
pub fn move_tab(core: State<Arc<AppCore>>, window: WebviewWindow, from: usize, to: usize) -> Result<View, String> {
    core.move_tab_in(window.label(), from, to)
}

/// Move a tab of the shown workspace into another workspace.
#[tauri::command(rename_all = "snake_case")]
pub fn move_tab_to_workspace(
    core: State<Arc<AppCore>>,
    window: WebviewWindow,
    tab: usize,
    workspace: usize,
) -> Result<View, String> {
    core.move_tab_to_workspace_in(window.label(), tab, workspace)
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_ratio(core: State<Arc<AppCore>>, window: WebviewWindow, split: SplitId, ratio: f32) -> Result<View, String> {
    core.set_ratio_in(window.label(), split, ratio)
}

// ---- panes and tabs between windows (DESK-29 part 2) ---------------------------------------------------------------
//
// All of them are async: they build, show or destroy a window, which waits for the main thread. The page never says
// where the pointer is; the shell reads it.

/// Give a pane of the calling window a window of its own: under the pointer (`at_cursor`), or beside this window
/// (the palette action). A pane that is alone in its window is refused. Answers the view of the calling window.
#[tauri::command(async, rename_all = "snake_case")]
pub fn tear_off_pane(
    core: State<Arc<AppCore>>,
    window: WebviewWindow,
    pane: PaneId,
    at_cursor: bool,
) -> Result<View, String> {
    core.tear_off_pane_in(window.label(), pane, at_cursor)
}

/// Like `tear_off_pane` for the whole tab the pane is in. The tab is named by any of its panes.
#[tauri::command(async, rename_all = "snake_case")]
pub fn tear_off_tab(
    core: State<Arc<AppCore>>,
    window: WebviewWindow,
    pane: PaneId,
    at_cursor: bool,
) -> Result<View, String> {
    core.tear_off_tab_in(window.label(), pane, at_cursor)
}

/// A pane (`kind` `pane`) or the tab of a pane (`kind` `tab`) was released outside the calling window. The shell
/// reads the pointer: over another window of the app that window is told with `incoming-drop`, over nothing the pane
/// or tab gets a window under the pointer. Answers `{outcome: "torn_off", label}`, `{outcome: "handed", to}` or
/// `{outcome: "cancelled"}`.
#[tauri::command(async, rename_all = "snake_case")]
pub fn release_outside(
    core: State<Arc<AppCore>>,
    window: WebviewWindow,
    pane: PaneId,
    kind: DropKind,
) -> Result<Released, String> {
    core.release_outside_in(window.label(), pane, kind)
}

/// The calling window is dragging a pane (`kind` `pane`) or the tab of a pane (`kind` `tab`) and the pointer is
/// outside it. The shell reads the pointer and tells the window under it to show the outline of the zone it would
/// land in (`drag-hover`); a window the drag left is told `drag-end`. Moves nothing.
#[tauri::command(async, rename_all = "snake_case")]
pub fn drag_over(core: State<Arc<AppCore>>, window: WebviewWindow, pane: PaneId, kind: DropKind) -> Result<(), String> {
    core.drag_over_in(window.label(), pane, kind)
}

/// The drag of the calling window ended (released anywhere, Escape, the pointer came back inside): the window it was
/// over takes its outline away. Nothing to tell is no error.
#[tauri::command(async, rename_all = "snake_case")]
pub fn drag_end(core: State<Arc<AppCore>>, window: WebviewWindow) -> Result<(), String> {
    core.drag_end_in(window.label())
}

/// The calling window answers an `incoming-drop` of a pane: onto the pane `target` at `edge`, or as a tab of
/// `workspace`. Only the drop that is in flight for this pane counts. Answers the view of the calling window.
#[tauri::command(async, rename_all = "snake_case")]
pub fn attach_pane(
    core: State<Arc<AppCore>>,
    window: WebviewWindow,
    pane: PaneId,
    target: Option<PaneId>,
    edge: Option<Edge>,
    workspace: Option<usize>,
) -> Result<View, String> {
    core.attach_pane_in(window.label(), pane, target, edge, workspace)
}

/// The calling window answers an `incoming-drop` of a tab: as the last tab of `workspace`. Answers the view of the
/// calling window.
#[tauri::command(async, rename_all = "snake_case")]
pub fn attach_tab(
    core: State<Arc<AppCore>>,
    window: WebviewWindow,
    pane: PaneId,
    workspace: usize,
) -> Result<View, String> {
    core.attach_tab_in(window.label(), pane, workspace)
}

/// The page of the calling window has applied its first layout: it is told the last title and directory of its
/// panes, and a window that was created hidden is shown and focused.
#[tauri::command(async, rename_all = "snake_case")]
pub fn window_ready(core: State<Arc<AppCore>>, window: WebviewWindow) -> Result<(), String> {
    core.window_ready_in(window.label())?;
    if !window.is_visible().map_err(|e| e.to_string())? {
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---- settings, themes and key bindings ----------------------------------------------------------------------------

/// The settings, the themes, the theme in force and the key bindings. Reads memory only.
#[tauri::command(rename_all = "snake_case")]
pub fn get_settings(core: State<Arc<AppCore>>) -> SettingsView {
    core.get_settings()
}

/// Set one setting (`key` is a field of `settings.json`, `value` its JSON value). Validated, written atomically,
/// pushed to every live pane and announced as the `settings` event; the answer is the new view, or the reason why
/// nothing changed. Writes a file, so it is async.
#[tauri::command(async, rename_all = "snake_case")]
pub fn set_setting(core: State<Arc<AppCore>>, key: String, value: Value) -> Result<SettingsView, String> {
    core.set_setting(&key, value)
}

/// Restore the default of one setting. Async for the same reason as `set_setting`.
#[tauri::command(async, rename_all = "snake_case")]
pub fn reset_setting(core: State<Arc<AppCore>>, key: String) -> Result<SettingsView, String> {
    core.reset_setting(&key)
}

/// Re-read `settings.json` and `themes/` when a file changed; does nothing otherwise. Async: it inspects files.
#[tauri::command(async, rename_all = "snake_case")]
pub fn reload_settings(core: State<Arc<AppCore>>) -> SettingsView {
    core.reload_settings()
}

// ---- links, paths, app info and the pointer -----------------------------------------------------------------------

/// Open a link with the platform opener. Only http, https and mailto links up to 2048 bytes are opened. Async: it
/// starts a process.
#[tauri::command(async, rename_all = "snake_case")]
pub fn open_url(core: State<Arc<AppCore>>, url: String) -> Result<(), String> {
    core.open_url(&url)
}

/// Reveal a file or folder in the file manager and never run it. `path` is absolute, `~/...` or relative to `base`
/// (the pane's working directory); it has to exist. Async: it starts a process.
#[tauri::command(async, rename_all = "snake_case")]
pub fn reveal_path(core: State<Arc<AppCore>>, path: String, base: Option<String>) -> Result<(), String> {
    core.reveal_path(&path, base.as_deref())
}

/// The version, the operating system and the host name. Async: it may run a program.
#[tauri::command(async, rename_all = "snake_case")]
pub fn app_info(core: State<Arc<AppCore>>) -> AppInfo {
    core.app_info()
}

/// The text size the operating system asks for, in percent (100 to 300; 100 where the platform has no such setting or
/// it cannot be read). Async: it runs a program, for at most a moment. The page multiplies its UI scale by it.
#[tauri::command(async, rename_all = "snake_case")]
pub fn os_text_scale() -> u32 {
    crate::textscale::read(std::env::consts::OS)
}

/// A pointer press, release or motion at `x`, `y` device pixels from the origin of the pane's grid; `mods` is
/// SHIFT 1, ALT 2, CTRL 4, SUPER 8. Hot path: bounds the numbers and pushes to the pane's queue, nothing else, so
/// it is synchronous and never waits.
#[tauri::command(rename_all = "snake_case")]
pub fn mouse(
    core: State<Arc<AppCore>>,
    pane: PaneId,
    action: MouseActionMsg,
    button: MouseButtonMsg,
    x: f64,
    y: f64,
    mods: u32,
) {
    core.mouse(pane, action.into(), button.into(), x, y, mods);
}

/// A wheel event of `rows` rows (negative towards older output) at `x`, `y`. Hot path like `mouse`.
#[tauri::command(rename_all = "snake_case")]
pub fn wheel(core: State<Arc<AppCore>>, pane: PaneId, rows: i32, x: f64, y: f64, mods: u32) {
    core.wheel(pane, rows, x, y, mods);
}

// ---- connections ------------------------------------------------------------------------------------------------

/// The saved connections (SSH, WSL, Docker), each with whether a secret is saved for it. Asks the keychain, so async.
#[tauri::command(async, rename_all = "snake_case")]
pub fn connections_list(core: State<Arc<AppCore>>) -> Vec<ProfileView> {
    core.connections_list()
}

/// Add or change a connection. `secret` says what to do with its password or key passphrase: `{kind: "keep"}`,
/// `{kind: "set", value}` or `{kind: "clear"}`. The answer is the connection as saved, or why it was refused.
#[tauri::command(async, rename_all = "snake_case")]
pub fn connection_save(core: State<Arc<AppCore>>, profile: Profile, secret: SecretChange) -> Result<ProfileView, String> {
    core.connection_save(profile, secret)
}

/// Remove a connection and its secret.
#[tauri::command(async, rename_all = "snake_case")]
pub fn connection_delete(core: State<Arc<AppCore>>, id: String) -> Result<(), String> {
    core.connection_delete(&id)
}

/// Open connection `id` in a new tab; the answer is the view with that tab shown.
#[tauri::command(async, rename_all = "snake_case")]
pub fn connect(core: State<Arc<AppCore>>, window: WebviewWindow, id: String) -> Result<View, String> {
    core.connect_in(window.label(), &id)
}

/// The connection state of every running pane that has a connection. Reads memory only.
#[tauri::command(rename_all = "snake_case")]
pub fn pane_connections(core: State<Arc<AppCore>>) -> Vec<ConnView> {
    core.pane_connections()
}

// ---- files and previews -----------------------------------------------------------------------------------------

/// Open a file browser or a preview: `kind` is `files` or `preview`, `source` is `local` or a connection id, `placement`
/// is `tab`, `right` or `down`. An empty `path` is the start directory of the source (for a browser). The answer is the
/// view with the new pane focused. Reads a directory over the network, so async.
#[tauri::command(async, rename_all = "snake_case")]
pub fn block_open(
    core: State<Arc<AppCore>>,
    window: WebviewWindow,
    kind: BlockKind,
    source: String,
    path: String,
    placement: String,
) -> Result<View, String> {
    core.block_open_in(window.label(), &placement, kind, &source, &path)
}

/// Every pane that shows a file browser or a preview. Reads memory only.
#[tauri::command(rename_all = "snake_case")]
pub fn pane_blocks(core: State<Arc<AppCore>>) -> Vec<BlockView> {
    core.blocks_list()
}

/// The pane's block now shows `path`.
#[tauri::command(rename_all = "snake_case")]
pub fn block_update(core: State<Arc<AppCore>>, pane: PaneId, path: String) -> Result<(), String> {
    core.block_update(pane, &path)
}

/// The entries of a directory, folders first. Over SFTP this waits for the network, so async.
#[tauri::command(async, rename_all = "snake_case")]
pub fn files_list(core: State<Arc<AppCore>>, source: String, path: String) -> Result<Listing, String> {
    core.files_list(&source, &path)
}

/// What a file is: its kind, size, and for Markdown the sanitised HTML, for small text the content.
#[tauri::command(async, rename_all = "snake_case")]
pub fn file_open(core: State<Arc<AppCore>>, source: String, path: String) -> Result<Opened, String> {
    core.file_open(&source, &path)
}

/// One entry, following links.
#[tauri::command(async, rename_all = "snake_case")]
pub fn file_stat(core: State<Arc<AppCore>>, source: String, path: String) -> Result<Entry, String> {
    core.file_stat(&source, &path)
}

/// Permission to show one image, video, audio or PDF: the answer has a `token`, the media type and the size, and
/// `url` is where the `holzfile` scheme serves the file from (with byte ranges, so a video seeks and nothing is loaded
/// whole). The grant names that one file and ends with `file_revoke`.
#[tauri::command(async, rename_all = "snake_case")]
pub fn file_grant(core: State<Arc<AppCore>>, source: String, path: String) -> Result<GrantAnswer, String> {
    core.file_grant(&source, &path).map(|g| GrantAnswer { url: crate::files_protocol::url_of(&g.token), grant: g })
}

/// `len` bytes of a granted file from `offset` (at most 8 MiB), as raw bytes: what the PDF viewer reads pages with, so a
/// large document is read in pieces and never whole. Fewer bytes come back at the end of the file.
#[tauri::command(async, rename_all = "snake_case")]
pub fn file_range(core: State<Arc<AppCore>>, token: String, offset: u64, len: u64) -> Result<tauri::ipc::Response, String> {
    let len = len.min(crate::files_protocol::CHUNK);
    match core.read_granted(&token, offset, len) {
        Some(Ok(bytes)) => Ok(tauri::ipc::Response::new(bytes)),
        Some(Err(why)) => Err(why),
        None => Err("no such grant".into()),
    }
}

/// End a grant.
#[tauri::command(rename_all = "snake_case")]
pub fn file_revoke(core: State<Arc<AppCore>>, token: String) {
    core.file_revoke(&token);
}

/// The answer of `file_grant`.
#[derive(Serialize)]
pub struct GrantAnswer {
    pub url: String,
    #[serde(flatten)]
    pub grant: Grant,
}

/// Save the editor's text over a file (written aside and renamed over it). `expected_modified` is the change time the
/// file was opened with: a file that changed meanwhile is not overwritten (`null` overwrites). The answer is the new
/// change time.
#[tauri::command(async, rename_all = "snake_case")]
pub fn file_save(
    core: State<Arc<AppCore>>,
    source: String,
    path: String,
    text: String,
    expected_modified: Option<i64>,
) -> Result<Option<i64>, String> {
    core.file_save(&source, &path, &text, expected_modified)
}

/// The state of the sync.
#[tauri::command(async, rename_all = "snake_case")]
pub fn sync_status(core: State<Arc<AppCore>>) -> crate::sync::SyncView {
    core.sync_status()
}

/// Set up the sync for an account (the first run makes the folder on a new server) or why it was refused.
#[tauri::command(async, rename_all = "snake_case")]
pub fn sync_save(
    core: State<Arc<AppCore>>,
    endpoint: String,
    user: String,
    password: String,
    passphrase: String,
) -> Result<crate::sync::SyncView, String> {
    core.sync_save(&endpoint, &user, &password, &passphrase)
}

/// Run the sync now.
#[tauri::command(async, rename_all = "snake_case")]
pub fn sync_now(core: State<Arc<AppCore>>) -> Result<crate::sync::SyncView, String> {
    core.sync_now()
}

/// Turn the sync off and forget the account.
#[tauri::command(async, rename_all = "snake_case")]
pub fn sync_off(core: State<Arc<AppCore>>) -> crate::sync::SyncView {
    core.sync_off()
}

/// What the computer of a monitor pane is doing: CPU, memory, network and the busiest processes, or the word that it is
/// not a computer this monitor can read. The page names a pane and never an address: the pane's block names the source.
#[tauri::command(async, rename_all = "snake_case")]
pub fn monitor_sample(core: State<Arc<AppCore>>, pane: PaneId) -> Result<holzwave_core::monitor::Reading, String> {
    core.monitor_sample(pane)
}

/// Typed commands that match `query` (fuzzy), newest and best first; an empty query lists the newest. The search is the
/// history of the host the focused pane of the calling window runs on: the page sends no host.
#[tauri::command(async, rename_all = "snake_case")]
pub fn history_search(
    core: State<Arc<AppCore>>,
    window: WebviewWindow,
    query: String,
    limit: usize,
) -> Vec<holzwave_core::history::Hit> {
    core.history_search_in(window.label(), &query, limit)
}

/// Forget every use of one command.
#[tauri::command(async, rename_all = "snake_case")]
pub fn history_delete(core: State<Arc<AppCore>>, cmd: String) -> Result<usize, String> {
    core.history_delete(&cmd)
}

/// Forget the whole history.
#[tauri::command(async, rename_all = "snake_case")]
pub fn history_clear(core: State<Arc<AppCore>>) -> Result<(), String> {
    core.history_clear()
}

/// The widgets of the sidebar, their saved notes and what was wrong with a widget file.
#[tauri::command(async, rename_all = "snake_case")]
pub fn widgets_list(core: State<Arc<AppCore>>) -> crate::state::WidgetsView {
    core.widgets_list()
}

/// Save the text of a note widget (an empty text forgets it).
#[tauri::command(async, rename_all = "snake_case")]
pub fn widget_note_save(core: State<Arc<AppCore>>, id: String, text: String) -> Result<(), String> {
    core.widget_note_save(&id, &text)
}

// ---- the AI chat -------------------------------------------------------------------------------------------------
//
// A refusal of these commands reaches the page as the object `{code, reason, text}` (`AiFault`), not as a string. The
// events of a turn arrive as the event `ai` for every window; the page routes them by conversation.

/// Send the typed `text` to the chat `conversation`, as a message of the user in `pane`. The answer is the ticket of
/// the turn, at once; the reply arrives as `ai` events, and anything that goes wrong later arrives as the end of the
/// turn. `profile` and `model` may be null or empty: the chat then uses the provider and model it is pinned to. Only
/// the typed text leaves: the command has no argument that could carry terminal content or a decision. A pane that is
/// not open is refused as `no_such_pane`.
///
/// `offer` and `picked` are both given or both left out. With both, the message carries the items of an offer that
/// `ai_context_preview` made for this chat, this pane and this route: the page names the offer and the numbers of the
/// items it picked, never their text. With one of them only, the call is refused as `context` / `bad_pick` and nothing
/// is stored.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_send(
    core: State<Arc<AppCore>>,
    conversation: String,
    text: String,
    pane: PaneId,
    profile: Option<String>,
    model: Option<String>,
    offer: Option<String>,
    picked: Option<Vec<u32>>,
) -> Result<TicketView, AiFault> {
    core.ai_send_with(&conversation, text, pane, profile, model, offer, picked)
}

/// What `pane` offers for the chat and where it would go: the profile and model of the call (the chat's own when
/// they are null or empty), the host and where it is (`on_device`, `loopback`, `lan` or `cloud`), whether the pane
/// must be shared first (`required`), whether it is (`shared`), the kinds the pane offers now and whether the chat
/// file keeps the text of a context. Nothing is read from the pane, and there is no text in the answer. This looks up
/// the name of a host once, so call it for an act of the user and not on a mount. A refusal is the fault object with
/// the code `context`.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_context_state(
    core: State<Arc<AppCore>>,
    conversation: String,
    pane: PaneId,
    profile: Option<String>,
    model: Option<String>,
) -> Result<ContextState, AiFault> {
    core.ai_context_state(&conversation, pane, profile, model)
}

/// Share `pane` for the chat on this route (`shared` true), or end the share (false). A share is the permission to
/// read the pane into an offer; nothing is read and nothing is attached to a later message. A change of provider,
/// model or address, and the end of the chat or of the pane, end the share. The answer is the state, as for
/// `ai_context_state`.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_context_share(
    core: State<Arc<AppCore>>,
    conversation: String,
    pane: PaneId,
    profile: Option<String>,
    model: Option<String>,
    shared: bool,
) -> Result<ContextState, AiFault> {
    core.ai_context_share(&conversation, pane, profile, model, shared)
}

/// Read the `kinds` (`selection`, `screen`, `last_command`, `file`, `preview`) of a shared `pane` into an offer. The
/// answer carries the id of the offer, where it would go, and for each item its number, kind, label, size and what the
/// cleaner removed: labels, sizes and findings, never text. The chat holds one offer at a time and it lives five
/// minutes. A refusal is the fault object with the code `context` and one of its stable reasons.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_context_preview(
    core: State<Arc<AppCore>>,
    conversation: String,
    pane: PaneId,
    profile: Option<String>,
    model: Option<String>,
    kinds: Vec<ContextKind>,
) -> Result<OfferView, AiFault> {
    core.ai_context_preview(&conversation, pane, profile, model, kinds)
}

/// The state of a chat right now: what a page that saw every `ai` event of it holds. The page asks for it when it
/// opens a chat and when it misses an event.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_turn_snapshot(core: State<Arc<AppCore>>, conversation: String) -> Result<ChatState, AiFault> {
    core.ai().snapshot(&conversation)
}

/// A fresh chat id. No file is made until the first message is sent.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_conversation_new(core: State<Arc<AppCore>>) -> Result<String, AiFault> {
    core.ai().conversation_new()
}

/// The saved chats, newest first, and one note for each file that could not be listed.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_conversation_list(core: State<Arc<AppCore>>) -> Result<ConversationList, AiFault> {
    core.ai().conversation_list()
}

/// Delete a chat and stop its turn. Answers whether a file was removed.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_conversation_delete(core: State<Arc<AppCore>>, conversation: String) -> Result<bool, AiFault> {
    core.ai().conversation_delete(&conversation)
}

/// Stop the turn of a chat; it keeps the text it had. Answers whether the chat had a turn.
#[tauri::command(rename_all = "snake_case")]
pub fn ai_cancel(core: State<Arc<AppCore>>, conversation: String) -> Result<bool, AiFault> {
    core.ai().cancel(&conversation)
}

/// The person's verdict on one approval card: the chat, the call, `allow` or `deny`, and the hash of the card they were
/// shown. It answers nothing; what follows arrives as `ai` events (the result of the call, or the end of the turn). A
/// refusal is the fault object with the code `tool` and the stable code of the error as the reason (`hash_mismatch`,
/// `unknown_call`, `not_pending`, ...), or `invalid_decision` for a verdict that is neither word. A hash that does not
/// match leaves the card waiting.
///
/// This is the one command that decides a call. It exists for the webview only: the local socket of `wsh` has no
/// counterpart, and nothing else reaches the approval table.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_decide(
    core: State<Arc<AppCore>>,
    conversation: String,
    call_id: String,
    decision: String,
    hash: String,
) -> Result<(), AiFault> {
    core.ai_decide(&conversation, &call_id, &decision, &hash)
}

/// The person's press on `Undo` for one finished write: the chat and the call. It answers nothing. The core puts the old text
/// of the file back only while the file still holds exactly what the assistant wrote, and outside the deny list; a refusal
/// is the fault object with the code `undo` and a stable reason (`unknown_change`, `created`, `file_changed`, `denied`,
/// `unreadable`) and a fixed sentence.
///
/// This command exists for the webview only and is not a verdict on a tool call: it never reaches the approval table, and it
/// carries two ids and no text.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_undo(core: State<Arc<AppCore>>, conversation: String, call_id: String) -> Result<(), AiFault> {
    core.ai_undo(&conversation, &call_id)
}

/// The requests of `wsh ai` that wait for an answer in the calling window, oldest first: a page that loaded after the
/// request was made (a reload, a window that opened late) asks for the cards it missed. It reads an in-memory table and
/// touches no keychain, file or network, and it holds no text but the prompt each card shows.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_ask_pending(core: State<Arc<AppCore>>, window: WebviewWindow) -> Result<Vec<AskView>, AiFault> {
    Ok(core.ai_ask_pending(window.label()))
}

/// The person's press on a card of `wsh ai`: the request, `send` or `refuse`, and the seal the card carried. It answers
/// nothing; the answer of the model streams back to the program that asked. A refusal is the fault object with the code
/// `ask` and a stable reason (`unknown_ask`, `seal_mismatch`, `bad_verdict`) and a fixed sentence.
///
/// This command exists for the webview only: it releases one waiting request, for its own seal, and the local socket of
/// `wsh` has no way to reach it.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_ask_answer(core: State<Arc<AppCore>>, ask: String, verdict: String, seal: String) -> Result<(), AiFault> {
    core.ai_ask_answer(&ask, &verdict, &seal)
}

/// The profiles with the state of each key (never a key). This reads the keychain, so the settings dialog calls it
/// when it opens and not before.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_profiles(core: State<Arc<AppCore>>) -> AiProfiles {
    core.ai().profiles()
}

/// Make a profile or change one. Answers its id. The input has no member for a key.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_profile_save(core: State<Arc<AppCore>>, input: ProfileInputMsg) -> Result<String, AiFault> {
    core.ai().profile_save(input)
}

/// Remove a profile and its keys, and stop the turns that use it.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_profile_remove(core: State<Arc<AppCore>>, profile: String) -> Result<RemovalView, AiFault> {
    core.ai().profile_remove(&profile)
}

/// Save a key in the system keychain: it is written and never read back. A profile's address is part of the saved
/// record, so a key is used only for the address it was saved for. Answers nothing.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_key_set(
    core: State<Arc<AppCore>>,
    profile: String,
    slot: KeySlot,
    api_key: String,
) -> Result<(), AiFault> {
    core.ai().key_set(&profile, slot, api_key)
}

/// Remove a key from the system keychain and stop the turns that use the profile. Answers nothing.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_key_clear(core: State<Arc<AppCore>>, profile: String, slot: KeySlot) -> Result<(), AiFault> {
    core.ai().key_clear(&profile, slot)
}

/// Whether a usable key is saved for the profile. Answers a flag, never the key.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_has_key(core: State<Arc<AppCore>>, profile: String, slot: KeySlot) -> Result<bool, AiFault> {
    core.ai().has_key(&profile, slot)
}

/// Render the Markdown of a chat reply as HTML, with the sanitiser the previews use: an allow-list of tags and of link
/// schemes, no images, no styles, no scripts. Answers nothing when the text is larger than the renderer takes. The page
/// copies the answer through an allow-list of its own a second time and never trusts this one alone. It holds no state,
/// so it needs no handle.
#[tauri::command(async, rename_all = "snake_case")]
pub fn markdown_render(text: String) -> Option<String> {
    holzwave_core::preview::render_markdown(&text)
}
