//! Build script of the app shell: it generates the Tauri context and the permission set of the app.
//!
//! The app manifest makes every command deny-by-default (T-02-55): once it is declared, a command is callable only
//! when a capability grants its `allow-<command>` permission. The list below is the single source of the command
//! names; `tests/acl.rs` fails when the invoke handler of `src/lib.rs` or the permissions of
//! `capabilities/default.json` disagree with it. Do not add a command to one of the three without the others.

fn main() {
  let manifest = tauri_build::AppManifest::new().commands(&[
    "get_layout",
    "start_pane",
    "key",
    "send_edit",
    "text",
    "paste",
    "resize",
    "scroll",
    "scroll_to_bottom",
    "request_full",
    "select",
    "selection_text",
    "search",
    "copy_mode",
    "split",
    "new_tab",
    "new_workspace",
    "close_pane",
    "close_tab",
    "close_workspace",
    "focus_pane",
    "cycle_focus",
    "select_tab",
    "select_workspace",
    "rename_pane",
    "rename_tab",
    "rename_workspace",
    "move_pane",
    "move_tab",
    "move_tab_to_workspace",
    "set_ratio",
    "tear_off_pane",
    "tear_off_tab",
    "release_outside",
    "attach_pane",
    "attach_tab",
    "window_ready",
    "get_settings",
    "set_setting",
    "reset_setting",
    "reload_settings",
    "app_info",
    "os_text_scale",
    "open_url",
    "reveal_path",
    "mouse",
    "wheel",
  ]);
  tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest)).expect("tauri build failed");
}
