//! The commands the webview can call: thin wrappers over [`AppCore`] (D-19). The terminal, layout, window, pointer,
//! settings, connection, block, file, history, sync and local network commands are rows of the host's command table
//! (`holzwave_host::commands::table`) and their wrappers are written from it in `desktop_table.rs`; the three of them
//! that need the window shell (`copy_selection`, `clipboard_text`, `window_ready`) and the AI and integration commands
//! are written out here.
//!
//! Two conventions hold for every command, and `state.rs` has a test that reads this file and the table to keep them:
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
//! The wire op types (`holzwave_host::commands::wire`) are internally tagged with `kind` and every variant, motion and
//! mode name is snake_case, so the page sends `{kind: "begin", x, y, mode: "word"}` and `{kind: "move", motion:
//! "line_start"}`.

use std::sync::Arc;

use holzwave_core::layout::PaneId;
use tauri::{State, WebviewWindow};

use crate::clipboard;
use holzwave_host::state::AppCore;

// The wire types of the table's commands live in the host crate. The protocol golden and the tests of the shell import
// them from here.
#[allow(unused_imports)]
pub use holzwave_host::commands::wire::{
    CopyMotionMsg, CopyOpMsg, CopyStateMsg, EditActionMsg, GrantAnswer, MouseActionMsg, MouseButtonMsg, PasteOutcomeMsg,
    SearchOpMsg, SelectModeMsg, SelectOpMsg,
};

// ---- the wrappers that need the window shell ---------------------------------------------------------------------

/// Copy the core's own selection of a pane to the system clipboard: `true` when text was written, `false` when the pane
/// has no selection or an empty one. The text comes from the core and never from the page, so this command is not a
/// clipboard write primitive: a script in the page can ask for the selection to be copied, but never choose what is
/// copied. Waits for the engine's answer.
#[tauri::command(async, rename_all = "snake_case")]
pub fn copy_selection(core: State<Arc<AppCore>>, pane: PaneId) -> Result<bool, String> {
    match core.selection_text(pane)? {
        Some(text) if !text.is_empty() => {
            clipboard::write_text(&text)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The text on the system clipboard (the empty string when it holds none), for the page's paste fallback. The page calls it
/// only after the person's own paste chord, when the web view's `paste` event did not come, and the text then goes through
/// the one `paste` path (sanitised, bracketed when asked, confirmed when it spans lines). An error means this platform has
/// no native clipboard access and the page reads the web view's clipboard instead. Waits for the system.
#[tauri::command(async, rename_all = "snake_case")]
pub fn clipboard_text() -> Result<String, String> {
    clipboard::read_text()
}

/// The page of the calling window has applied its first layout: it is told the last title and directory of its
/// panes, and a window that was created hidden is shown and focused. The first half is the table's function; showing
/// the window is the shell's.
#[tauri::command(async, rename_all = "snake_case")]
pub fn window_ready(core: State<Arc<AppCore>>, window: WebviewWindow) -> Result<(), String> {
    holzwave_host::commands::layout::window_ready(&core, window.label())?;
    if !window.is_visible().map_err(|e| e.to_string())? {
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}
