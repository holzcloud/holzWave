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
//!   cannot). The plain function returns exactly `Result<answers, fails>`, or `answers` without `fails`. A raw (binary)
//!   answer is written `answers: raw <shell type>, fails: E`: the plain function returns `Result<Vec<u8>, E>`, the
//!   registry answers the bytes as they are (`Outcome::Binary`, never JSON text), and the desktop wrapper wraps them as
//!   the shell type, which is also the type text the recorded surface holds. Only a command that can fail may be raw.
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
                name: image_data, shell: generated, group: terminal, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId, image: u32, generation: u64, offset: u64],
                answers: raw tauri::ipc::Response, fails: String,
                run: terminal::image_data, web: same
            }
            {
                name: set_image_animation, shell: generated, group: terminal, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [enabled: bool],
                answers: (),
                run: terminal::set_image_animation, web: same
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
            {
                name: file_grant, shell: generated, group: files, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [source: String, path: String],
                answers: GrantAnswer, fails: String,
                run: files::file_grant, web: same
            }
            {
                name: file_grant_why, shell: generated, group: files, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [source: String, path: String],
                answers: GrantWhy,
                run: files::file_grant_why, web: same
            }
            {
                name: file_range, shell: generated, group: files, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [token: String, offset: u64, len: u64],
                answers: raw tauri::ipc::Response, fails: String,
                run: files::file_range, web: same
            }
            {
                name: file_revoke, shell: generated, group: files, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [token: String],
                answers: (),
                run: files::file_revoke, web: same
            }
            {
                name: get_settings, shell: generated, group: settings, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [],
                answers: SettingsView,
                run: settings::get_settings, web: same
            }
            {
                name: set_setting, shell: generated, group: settings, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [key: String, value: Value],
                answers: SettingsView, fails: String,
                run: settings::set_setting, web: same
            }
            {
                name: reset_setting, shell: generated, group: settings, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [key: String],
                answers: SettingsView, fails: String,
                run: settings::reset_setting, web: same
            }
            {
                name: reload_settings, shell: generated, group: settings, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: SettingsView,
                run: settings::reload_settings, web: same
            }
            {
                name: os_text_scale, shell: generated, group: settings, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: u32,
                run: settings::os_text_scale, web: same
            }
            {
                name: app_info, shell: generated, group: settings, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: AppInfo,
                run: settings::app_info, web: same
            }
            {
                name: markdown_render, shell: generated, group: settings, scope: none, mode: async, cap: CAP_LARGE,
                args: [text: String],
                answers: Option<String>,
                run: settings::markdown_render, web: same
            }
            {
                name: open_url, shell: generated, group: settings, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [url: String, pane: Option<PaneId>],
                answers: (), fails: String,
                run: settings::open_url, web: unavailable
            }
            {
                name: reveal_path, shell: generated, group: settings, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [path: String, base: Option<String>],
                answers: (), fails: String,
                run: settings::reveal_path, web: unavailable
            }
            {
                name: connections_list, shell: generated, group: connections, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: Vec<ProfileView>,
                run: connections::connections_list, web: same
            }
            {
                name: connection_save, shell: generated, group: connections, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [profile: Profile, secret: SecretChange],
                answers: ProfileView, fails: String,
                run: connections::connection_save, web: same
            }
            {
                name: connection_delete, shell: generated, group: connections, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [id: String],
                answers: (), fails: String,
                run: connections::connection_delete, web: same
            }
            {
                name: connect, shell: generated, group: connections, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [id: String],
                answers: View, fails: String,
                run: connections::connect, web: same
            }
            {
                name: pane_connections, shell: generated, group: connections, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [],
                answers: Vec<ConnView>,
                run: connections::pane_connections, web: same
            }
            {
                name: login_prompt_answer, shell: generated, group: connections, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [id: String, answers: Vec<String>],
                answers: bool,
                run: connections::login_prompt_answer, web: same
            }
            {
                name: login_prompt_cancel, shell: generated, group: connections, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [id: String],
                answers: bool,
                run: connections::login_prompt_cancel, web: same
            }
            {
                name: wsh_install, shell: generated, group: connections, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId],
                answers: String, fails: String,
                run: connections::wsh_install, web: same
            }
            {
                name: block_open, shell: generated, group: blocks, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [kind: BlockKind, source: String, path: String, placement: String],
                answers: View, fails: String,
                run: blocks::block_open, web: same
            }
            {
                name: block_update, shell: generated, group: blocks, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [pane: PaneId, path: String],
                answers: (), fails: String,
                run: blocks::block_update, web: same
            }
            {
                name: pane_blocks, shell: generated, group: blocks, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [],
                answers: Vec<BlockView>,
                run: blocks::pane_blocks, web: same
            }
            {
                name: widgets_list, shell: generated, group: blocks, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: holzwave_host::state::WidgetsView,
                run: blocks::widgets_list, web: same
            }
            {
                name: widget_note_save, shell: generated, group: blocks, scope: none, mode: async, cap: CAP_LARGE,
                args: [id: String, text: String],
                answers: (), fails: String,
                run: blocks::widget_note_save, web: same
            }
            {
                name: monitor_sample, shell: generated, group: blocks, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId],
                answers: holzwave_core::monitor::Reading, fails: String,
                run: blocks::monitor_sample, web: same
            }
            {
                name: files_list, shell: generated, group: files, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [source: String, path: String],
                answers: Listing, fails: String,
                run: files::files_list, web: same
            }
            {
                name: file_open, shell: generated, group: files, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [source: String, path: String],
                answers: Opened, fails: String,
                run: files::file_open, web: same
            }
            {
                name: file_stat, shell: generated, group: files, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [source: String, path: String],
                answers: Entry, fails: String,
                run: files::file_stat, web: same
            }
            {
                name: file_save, shell: generated, group: files, scope: none, mode: async, cap: CAP_LARGE,
                args: [source: String, path: String, text: String, expected_modified: Option<i64>, expected_digest: Option<String>],
                answers: Saved, fails: String,
                run: files::file_save, web: same
            }
            {
                name: history_search, shell: generated, group: history_sync, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [query: String, limit: usize],
                answers: Vec<holzwave_core::history::Hit>,
                run: history_sync::history_search, web: same
            }
            {
                name: history_delete, shell: generated, group: history_sync, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [cmd: String],
                answers: usize, fails: String,
                run: history_sync::history_delete, web: same
            }
            {
                name: history_clear, shell: generated, group: history_sync, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: (), fails: String,
                run: history_sync::history_clear, web: same
            }
            {
                name: sync_status, shell: generated, group: history_sync, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: holzwave_host::sync::SyncView,
                run: history_sync::sync_status, web: same
            }
            {
                name: sync_save, shell: generated, group: history_sync, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [endpoint: String, user: String, password: String, passphrase: String],
                answers: holzwave_host::sync::SyncView, fails: String,
                run: history_sync::sync_save, web: same
            }
            {
                name: sync_now, shell: generated, group: history_sync, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: holzwave_host::sync::SyncView, fails: String,
                run: history_sync::sync_now, web: same
            }
            {
                name: sync_off, shell: generated, group: history_sync, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: holzwave_host::sync::SyncView,
                run: history_sync::sync_off, web: same
            }
            {
                name: local_network_status, shell: generated, group: history_sync, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: holzwave_host::local_network::LocalNetworkView,
                run: history_sync::local_network_status, web: unavailable
            }
            {
                name: local_network_answer, shell: generated, group: history_sync, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [allow: bool],
                answers: holzwave_host::local_network::LocalNetworkView, fails: String,
                run: history_sync::local_network_answer, web: unavailable
            }
            {
                name: local_network_open_settings, shell: generated, group: history_sync, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: (), fails: String,
                run: history_sync::local_network_open_settings, web: unavailable
            }
            {
                name: host_shell_status, shell: generated, group: history_sync, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: holzwave_host::host_shell::HostShellView,
                run: history_sync::host_shell_status, web: unavailable
            }
            {
                name: host_shell_set, shell: generated, group: history_sync, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [enabled: bool],
                answers: holzwave_host::host_shell::HostShellView, fails: String,
                run: history_sync::host_shell_set, web: unavailable
            }
            {
                name: ai_turn_snapshot, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [conversation: String],
                answers: ChatState, fails: AiFault,
                run: ai::ai_turn_snapshot, web: same
            }
            {
                name: ai_profiles, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: AiProfiles,
                run: ai::ai_profiles, web: same
            }
            {
                name: ai_key_set, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [profile: String, slot: KeySlot, api_key: String],
                answers: (), fails: AiFault,
                run: ai::ai_key_set, web: same
            }
            {
                name: ai_has_key, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [profile: String, slot: KeySlot],
                answers: bool, fails: AiFault,
                run: ai::ai_has_key, web: same
            }
            {
                name: integration_tools, shell: generated, group: integrations, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: Vec<ToolView>,
                run: integrations::integration_tools, web: same
            }
            {
                name: ai_send, shell: generated, group: ai, scope: none, mode: async, cap: CAP_LARGE,
                args: [conversation: String, text: String, pane: PaneId, profile: Option<String>, model: Option<String>, offer: Option<String>, picked: Option<Vec<u32>>],
                answers: TicketView, fails: AiFault,
                run: ai::ai_send, web: same
            }
            {
                name: ai_context_state, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [conversation: String, pane: PaneId, profile: Option<String>, model: Option<String>],
                answers: ContextState, fails: AiFault,
                run: ai::ai_context_state, web: same
            }
            {
                name: ai_context_share, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [conversation: String, pane: PaneId, profile: Option<String>, model: Option<String>, shared: bool],
                answers: ContextState, fails: AiFault,
                run: ai::ai_context_share, web: same
            }
            {
                name: ai_context_preview, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [conversation: String, pane: PaneId, profile: Option<String>, model: Option<String>, kinds: Vec<ContextKind>],
                answers: OfferView, fails: AiFault,
                run: ai::ai_context_preview, web: same
            }
            {
                name: ai_conversation_new, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: String, fails: AiFault,
                run: ai::ai_conversation_new, web: same
            }
            {
                name: ai_conversation_list, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: ConversationList, fails: AiFault,
                run: ai::ai_conversation_list, web: same
            }
            {
                name: ai_conversation_delete, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [conversation: String],
                answers: bool, fails: AiFault,
                run: ai::ai_conversation_delete, web: same
            }
            {
                name: ai_cancel, shell: generated, group: ai, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [conversation: String],
                answers: bool, fails: AiFault,
                run: ai::ai_cancel, web: same
            }
            {
                name: ai_decide, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [conversation: String, call_id: String, decision: String, hash: String],
                answers: (), fails: AiFault,
                run: ai::ai_decide, web: same
            }
            {
                name: ai_undo, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [conversation: String, call_id: String],
                answers: (), fails: AiFault,
                run: ai::ai_undo, web: same
            }
            {
                name: ai_ask_pending, shell: generated, group: ai, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: Vec<AskView>, fails: AiFault,
                run: ai::ai_ask_pending, web: same
            }
            {
                name: ai_ask_answer, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [ask: String, verdict: String, seal: String],
                answers: (), fails: AiFault,
                run: ai::ai_ask_answer, web: same
            }
            {
                name: ai_profile_save, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [input: ProfileInputMsg],
                answers: String, fails: AiFault,
                run: ai::ai_profile_save, web: same
            }
            {
                name: ai_profile_remove, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [profile: String],
                answers: RemovalView, fails: AiFault,
                run: ai::ai_profile_remove, web: same
            }
            {
                name: ai_key_clear, shell: generated, group: ai, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [profile: String, slot: KeySlot],
                answers: (), fails: AiFault,
                run: ai::ai_key_clear, web: same
            }
            {
                name: integration_run, shell: generated, group: integrations, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [tool: String, verb: String, pane: Option<PaneId>, args: BTreeMap<String, String>],
                answers: RunAnswer, fails: IntegrationFault,
                run: integrations::integration_run, web: same
            }
            {
                name: integration_select, shell: generated, group: integrations, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [tool: String, pane: Option<PaneId>, selection: Selection],
                answers: SelectionView, fails: IntegrationFault,
                run: integrations::integration_select, web: same
            }
            {
                name: integration_context, shell: generated, group: integrations, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [pane: Option<PaneId>, refresh: bool],
                answers: ContextAnswer,
                run: integrations::integration_context, web: same
            }
            {
                name: integration_prepare, shell: generated, group: integrations, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [tool: String, verb: String, pane: Option<PaneId>, args: BTreeMap<String, String>],
                answers: Card, fails: IntegrationFault,
                run: integrations::integration_prepare, web: same
            }
            {
                name: integration_confirm, shell: generated, group: integrations, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [ticket: String, hash: String],
                answers: ConfirmAnswer, fails: IntegrationFault,
                run: integrations::integration_confirm, web: same
            }
            {
                name: integration_cancel, shell: generated, group: integrations, scope: none, mode: sync, cap: CAP_DEFAULT,
                args: [ticket: String],
                answers: (), fails: IntegrationFault,
                run: integrations::integration_cancel, web: same
            }
            {
                name: integration_launch, shell: generated, group: integrations, scope: window, mode: async, cap: CAP_DEFAULT,
                args: [tool: String, verb: String, pane: Option<PaneId>, args: BTreeMap<String, String>, placement: String],
                answers: LaunchAnswer, fails: IntegrationFault,
                run: integrations::integration_launch, web: same
            }
            {
                name: integration_binary_set, shell: generated, group: integrations, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [tool: String, path: String],
                answers: Vec<ToolView>, fails: IntegrationFault,
                run: integrations::integration_binary_set, web: same
            }
            {
                name: integration_binary_clear, shell: generated, group: integrations, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [tool: String],
                answers: Vec<ToolView>, fails: IntegrationFault,
                run: integrations::integration_binary_clear, web: same
            }
            {
                name: integration_profiles, shell: generated, group: integrations, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [],
                answers: ProfilesView,
                run: integrations::integration_profiles, web: same
            }
            {
                name: integration_profile_save, shell: generated, group: integrations, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [profile: Value],
                answers: ProfilesView, fails: IntegrationFault,
                run: integrations::integration_profile_save, web: same
            }
            {
                name: integration_profile_remove, shell: generated, group: integrations, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [id: String],
                answers: ProfilesView, fails: IntegrationFault,
                run: integrations::integration_profile_remove, web: same
            }
            {
                name: integration_secret_set, shell: generated, group: integrations, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [profile: String, name: String, value: String],
                answers: (), fails: IntegrationFault,
                run: integrations::integration_secret_set, web: same
            }
            {
                name: integration_secret_clear, shell: generated, group: integrations, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [profile: String, name: String],
                answers: (), fails: IntegrationFault,
                run: integrations::integration_secret_clear, web: same
            }
        }
    };
}
