//! The commands the webview can call: thin wrappers over [`AppCore`] (D-19) and the wire types of their arguments
//! and answers.
//!
//! Two conventions hold for every command here, and `state.rs` has a test that reads this file to keep them:
//!
//! - **snake_case argument names.** The command macro of the pinned tauri-macros turns `allow_unsafe` into
//!   `allowUnsafe` for the page unless `rename_all = "snake_case"` says otherwise, which would make `allow_unsafe`,
//!   `cell_w` and `cell_h` silently disappear. Every command carries the rename.
//! - **no waiting on the main thread.** A command without `async` runs on the main thread (RESEARCH Pitfall 10), so
//!   those only lock, mutate and push to a queue. A command that waits for the engine thread's answer (up to two
//!   seconds) or for a shell to start is `async`, which runs it off the main thread.
//!
//! The wire op types are internally tagged with `kind` and every variant, motion and mode name is snake_case, so the
//! page sends `{kind: "begin", x, y, mode: "word"}` and `{kind: "move", motion: "line_start"}`.

use std::sync::Arc;

use holzwave_core::engine::{
    CopyMotion, CopyOp, CopyState, DomKey, Mods, MouseAction, MouseButton, PasteOutcome, SearchOp, SelectMode,
    SelectOp,
};
use holzwave_core::layout::{PaneId, SplitId};
use holzwave_core::pane::EditAction;
use holzwave_core::wire::SearchMsg;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;

use crate::state::{AppCore, AppInfo, SettingsView, View};

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
pub fn get_layout(core: State<Arc<AppCore>>) -> View {
    core.view()
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
pub fn split(core: State<Arc<AppCore>>, column: bool) -> View {
    core.split(column)
}

#[tauri::command(rename_all = "snake_case")]
pub fn new_tab(core: State<Arc<AppCore>>) -> View {
    core.new_tab()
}

#[tauri::command(rename_all = "snake_case")]
pub fn new_workspace(core: State<Arc<AppCore>>, name: String) -> View {
    core.new_workspace(&name)
}

/// Close a pane and kill its shell. Only closing the last pane ends the app. Async because that exit path writes
/// the layout.
#[tauri::command(async, rename_all = "snake_case")]
pub fn close_pane(core: State<Arc<AppCore>>, pane: PaneId) -> View {
    core.close_pane(pane)
}

#[tauri::command(rename_all = "snake_case")]
pub fn focus_pane(core: State<Arc<AppCore>>, pane: PaneId) -> View {
    core.focus_pane(pane)
}

#[tauri::command(rename_all = "snake_case")]
pub fn cycle_focus(core: State<Arc<AppCore>>, step: i32) -> View {
    core.cycle_focus(step)
}

#[tauri::command(rename_all = "snake_case")]
pub fn select_tab(core: State<Arc<AppCore>>, index: usize) -> View {
    core.select_tab(index)
}

#[tauri::command(rename_all = "snake_case")]
pub fn select_workspace(core: State<Arc<AppCore>>, index: usize) -> View {
    core.select_workspace(index)
}

/// Move a divider. Only marks the layout dirty: a debounced saver writes it, so a drag costs one write per window.
/// Name a pane. The empty text gives back the program title; the core cleans and cuts the text.
#[tauri::command(rename_all = "snake_case")]
pub fn rename_pane(core: State<Arc<AppCore>>, pane: PaneId, name: String) -> View {
    core.rename_pane(pane, &name)
}

/// Name the tab at `index` of the shown workspace. The empty text gives back `shell`.
#[tauri::command(rename_all = "snake_case")]
pub fn rename_tab(core: State<Arc<AppCore>>, index: usize, name: String) -> View {
    core.rename_tab(index, &name)
}

/// Name the workspace at `index`. The empty text gives back `ws N`.
#[tauri::command(rename_all = "snake_case")]
pub fn rename_workspace(core: State<Arc<AppCore>>, index: usize, name: String) -> View {
    core.rename_workspace(index, &name)
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_ratio(core: State<Arc<AppCore>>, split: SplitId, ratio: f32) -> View {
    core.set_ratio(split, ratio)
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
