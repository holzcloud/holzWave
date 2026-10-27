//! The one list of commands.
//!
//! [`host_commands!`] is a callback macro: `host_commands!(name)` calls the macro `name!` with every row below, and each
//! expansion writes what one side needs. The host's [`registry`](super::registry) writes a function that reads the
//! arguments strictly and runs the plain function, and a constant slice of plain facts. The desktop app writes the
//! window system's command wrappers and the handler for them. A row therefore cannot exist for one side only, and a
//! plain function whose signature does not match its row does not compile.
//!
//! # One row
//!
//! ```text
//! { name: paste, shell: generated, group: terminal, scope: none, mode: async, cap: CAP_PASTE,
//!   args: [pane: PaneId, text: String, allow_unsafe: bool],
//!   answers: PasteOutcomeMsg, fails: String,
//!   run: terminal::paste, web: same }
//! ```
//!
//! - `name`: the command, as the page spells it (snake_case).
//! - `shell`: `generated` when the app writes the desktop wrapper from the row, `hand` when the wrapper stays in the
//!   app's `commands.rs` because it needs the window shell (the clipboard, showing a window). The app's expansion fails
//!   the build when a `hand` row has no wrapper there.
//! - `group`: the plain-function module the row belongs to (`terminal`, `layout`).
//! - `scope`: `none`, or `window` when the command works on the layout of the calling window and so takes its label.
//! - `mode`: `sync`, or `async` when the command waits for the engine, a process or the network.
//! - `cap`: the most bytes of argument text the command takes (`CAP_INPUT`, `CAP_PASTE` or `CAP_DEFAULT`).
//! - `args`: the arguments as `name: Type`, in the order of the plain function. An `Option` argument may be left out.
//! - `answers`: the type the command answers; `fails`: the error type of a command that can fail (left out for one that
//!   cannot). The plain function returns exactly `Result<answers, fails>`, or `answers` without `fails`.
//! - `run`: the plain function `fn(core: &AppCore, [window: &str,] <args>) -> ...`. The desktop wrapper calls it; for a
//!   `hand` row it is the shared part of the wrapper, or `none` when the wrapper has no shared part.
//! - `web`: what the browser build does. `same` runs `run`; `unavailable` answers `Not available in the browser.` and
//!   never reads the arguments or runs anything; `variant(path)` runs `path`, a function with the signature of `run`.
//!
//! Every type, module and cap a row names is in [`prelude`](super::prelude); a new type goes there, so both sides import
//! the same list.

/// Calls `$callback!` with every row of the table, each in braces and without a separator. The rows are in the order of
/// [`COMMANDS`](super::registry::COMMANDS).
#[macro_export]
macro_rules! host_commands {
    ($callback:ident) => {
        $callback! {
            {
                name: get_layout, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [],
                answers: View, fails: String,
                run: layout::get_layout, web: same
            }
            {
                name: start_pane, shell: generated, group: terminal, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId, cols: u32, rows: u32, cell_w: Option<u32>, cell_h: Option<u32>],
                answers: (), fails: String,
                run: terminal::start_pane, web: same
            }
            {
                name: key, shell: generated, group: terminal, scope: none, mode: sync, cap: CAP_INPUT,
                args: [pane: PaneId, code: String, key: String, mods: u32, repeat: bool, release: bool, composing: bool],
                answers: (), fails: String,
                run: terminal::key, web: same
            }
            {
                name: send_edit, shell: generated, group: terminal, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [pane: PaneId, action: EditActionMsg],
                answers: (), fails: String,
                run: terminal::send_edit, web: same
            }
            {
                name: text, shell: generated, group: terminal, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [pane: PaneId, text: String],
                answers: (), fails: String,
                run: terminal::text, web: same
            }
            {
                name: paste, shell: generated, group: terminal, scope: none, mode: async, cap: CAP_PASTE,
                args: [pane: PaneId, text: String, allow_unsafe: bool],
                answers: PasteOutcomeMsg, fails: String,
                run: terminal::paste, web: same
            }
            {
                name: paste_cue, shell: generated, group: terminal, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId],
                answers: PasteOutcomeMsg, fails: String,
                run: terminal::paste_cue, web: same
            }
            {
                name: resize, shell: generated, group: terminal, scope: none, mode: sync, cap: CAP_INPUT,
                args: [pane: PaneId, cols: u32, rows: u32, cell_w: Option<u32>, cell_h: Option<u32>],
                answers: (), fails: String,
                run: terminal::resize, web: same
            }
            {
                name: scroll, shell: generated, group: terminal, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [pane: PaneId, rows: i32],
                answers: (), fails: String,
                run: terminal::scroll, web: same
            }
            {
                name: scroll_to_bottom, shell: generated, group: terminal, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [pane: PaneId],
                answers: (), fails: String,
                run: terminal::scroll_to_bottom, web: same
            }
            {
                name: request_full, shell: generated, group: terminal, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [pane: PaneId],
                answers: (), fails: String,
                run: terminal::request_full, web: same
            }
            {
                name: select, shell: generated, group: terminal, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [pane: PaneId, op: SelectOpMsg],
                answers: (), fails: String,
                run: terminal::select, web: same
            }
            {
                name: selection_text, shell: generated, group: terminal, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId],
                answers: Option<String>, fails: String,
                run: terminal::selection_text, web: same
            }
            {
                name: copy_selection, shell: hand, group: terminal, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId],
                answers: bool, fails: String,
                run: none, web: variant(terminal::copy_selection_web)
            }
            {
                name: clipboard_text, shell: hand, group: terminal, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: String, fails: String,
                run: none, web: unavailable
            }
            {
                name: search, shell: generated, group: terminal, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId, op: SearchOpMsg],
                answers: SearchMsg, fails: String,
                run: terminal::search, web: same
            }
            {
                name: copy_mode, shell: generated, group: terminal, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId, op: CopyOpMsg],
                answers: CopyStateMsg, fails: String,
                run: terminal::copy_mode, web: same
            }
            {
                name: split, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [column: bool],
                answers: View, fails: String,
                run: layout::split, web: same
            }
            {
                name: new_tab, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [],
                answers: View, fails: String,
                run: layout::new_tab, web: same
            }
            {
                name: new_workspace, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [name: String],
                answers: View, fails: String,
                run: layout::new_workspace, web: same
            }
            {
                name: close_pane, shell: generated, group: layout, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId],
                answers: View, fails: String,
                run: layout::close_pane, web: same
            }
            {
                name: close_tab, shell: generated, group: layout, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [index: usize],
                answers: View, fails: String,
                run: layout::close_tab, web: same
            }
            {
                name: close_workspace, shell: generated, group: layout, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [index: usize],
                answers: View, fails: String,
                run: layout::close_workspace, web: same
            }
            {
                name: focus_pane, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [pane: PaneId],
                answers: View, fails: String,
                run: layout::focus_pane, web: same
            }
            {
                name: cycle_focus, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [step: i32],
                answers: View, fails: String,
                run: layout::cycle_focus, web: same
            }
            {
                name: select_tab, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [index: usize],
                answers: View, fails: String,
                run: layout::select_tab, web: same
            }
            {
                name: select_workspace, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [index: usize],
                answers: View, fails: String,
                run: layout::select_workspace, web: same
            }
            {
                name: rename_pane, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [pane: PaneId, name: String],
                answers: View, fails: String,
                run: layout::rename_pane, web: same
            }
            {
                name: rename_tab, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [index: usize, name: String],
                answers: View, fails: String,
                run: layout::rename_tab, web: same
            }
            {
                name: rename_workspace, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [index: usize, name: String],
                answers: View, fails: String,
                run: layout::rename_workspace, web: same
            }
            {
                name: move_pane, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [pane: PaneId, target: PaneId, edge: Edge],
                answers: View, fails: String,
                run: layout::move_pane, web: same
            }
            {
                name: move_pane_to_workspace, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [pane: PaneId, workspace: usize],
                answers: View, fails: String,
                run: layout::move_pane_to_workspace, web: same
            }
            {
                name: move_tab, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [from: usize, to: usize],
                answers: View, fails: String,
                run: layout::move_tab, web: same
            }
            {
                name: move_tab_to_workspace, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [tab: usize, workspace: usize],
                answers: View, fails: String,
                run: layout::move_tab_to_workspace, web: same
            }
            {
                name: set_ratio, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [split: SplitId, ratio: f32],
                answers: View, fails: String,
                run: layout::set_ratio, web: same
            }
            {
                name: tear_off_pane, shell: generated, group: layout, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId, at_cursor: bool],
                answers: View, fails: String,
                run: layout::tear_off_pane, web: unavailable
            }
            {
                name: tear_off_tab, shell: generated, group: layout, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId, at_cursor: bool],
                answers: View, fails: String,
                run: layout::tear_off_tab, web: unavailable
            }
            {
                name: release_outside, shell: generated, group: layout, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId, kind: DropKind],
                answers: Released, fails: String,
                run: layout::release_outside, web: unavailable
            }
            {
                name: drag_over, shell: generated, group: layout, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId, kind: DropKind],
                answers: (), fails: String,
                run: layout::drag_over, web: unavailable
            }
            {
                name: drag_end, shell: generated, group: layout, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: (), fails: String,
                run: layout::drag_end, web: unavailable
            }
            {
                name: attach_pane, shell: generated, group: layout, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId, target: Option<PaneId>, edge: Option<Edge>, workspace: Option<usize>],
                answers: View, fails: String,
                run: layout::attach_pane, web: unavailable
            }
            {
                name: attach_tab, shell: generated, group: layout, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId, workspace: usize],
                answers: View, fails: String,
                run: layout::attach_tab, web: unavailable
            }
            {
                name: window_ready, shell: hand, group: layout, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: (), fails: String,
                run: layout::window_ready, web: same
            }
            {
                name: mouse, shell: generated, group: terminal, scope: none, mode: sync, cap: CAP_INPUT,
                args: [pane: PaneId, action: MouseActionMsg, button: MouseButtonMsg, x: f64, y: f64, mods: u32],
                answers: (),
                run: terminal::mouse, web: same
            }
            {
                name: wheel, shell: generated, group: terminal, scope: none, mode: sync, cap: CAP_INPUT,
                args: [pane: PaneId, rows: i32, x: f64, y: f64, mods: u32],
                answers: (),
                run: terminal::wheel, web: same
            }
        }
    };
}
