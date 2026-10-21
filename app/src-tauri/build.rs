//! Build script of the app shell: it generates the Tauri context and the permission set of the app.
//!
//! The app manifest makes every command deny-by-default (T-02-55): once it is declared, a command is callable only
//! when a capability grants its `allow-<command>` permission. The list below is the single source of the command
//! names; `tests/acl.rs` fails when the invoke handler of `src/lib.rs` or the permissions of
//! `capabilities/default.json` disagree with it. Do not add a command to one of the three without the others.

/// Repeats a link argument of the Apple bridge for this crate.
///
/// A link argument that a library crate prints does not reach the crates that depend on it. The bridge hands the two it
/// needs over as build metadata (cargo shows it to a dependent's build script as `DEP_<links>_<key>`), and this script
/// prints them again for the app. The variables exist only when the bridge is a dependency, which it is only on macOS.
///
/// - `WEAK_FRAMEWORK` names Apple's model framework. It is linked weakly: the app starts on a macOS that has no such
///   framework (below macOS 26), and the framework is used only after the system says it is there.
/// - `SWIFT_RPATH` is the run path of the Swift runtime libraries, which the operating system keeps in `/usr/lib/swift`;
///   without it the process dies at start with `Library not loaded: @rpath/libswift_Concurrency.dylib`.
///
/// A value that is not a plain word (a framework) or a plain absolute path stops the build, so metadata can never
/// smuggle another argument to the linker.
fn repeat_bridge_link_arguments() {
  if let Ok(framework) = std::env::var("DEP_HOLZWAVE_APPLE_AI_WEAK_FRAMEWORK") {
    if framework.is_empty() || !framework.chars().all(|c| c.is_ascii_alphanumeric()) {
      panic!("DEP_HOLZWAVE_APPLE_AI_WEAK_FRAMEWORK is not a framework name");
    }
    println!("cargo:rustc-link-arg=-Wl,-weak_framework,{framework}");
  }
  if let Ok(path) = std::env::var("DEP_HOLZWAVE_APPLE_AI_SWIFT_RPATH") {
    let plain = path.starts_with('/') && path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'));
    if !plain {
      panic!("DEP_HOLZWAVE_APPLE_AI_SWIFT_RPATH is not a plain absolute path");
    }
    println!("cargo:rustc-link-arg=-Wl,-rpath,{path}");
  }
}

fn main() {
  repeat_bridge_link_arguments();
  let manifest = tauri_build::AppManifest::new().commands(&[
    "get_layout",
    "start_pane",
    "key",
    "send_edit",
    "text",
    "paste",
    "paste_cue",
    "resize",
    "scroll",
    "scroll_to_bottom",
    "request_full",
    "select",
    "selection_text",
    "copy_selection",
    "clipboard_text",
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
    "move_pane_to_workspace",
    "move_tab",
    "move_tab_to_workspace",
    "set_ratio",
    "tear_off_pane",
    "tear_off_tab",
    "release_outside",
    "drag_over",
    "drag_end",
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
    "connections_list",
    "sync_status",
    "sync_save",
    "sync_now",
    "sync_off",
    "local_network_status",
    "local_network_answer",
    "local_network_open_settings",
    "monitor_sample",
    "wsh_install",
    "history_search",
    "history_delete",
    "history_clear",
    "login_prompt_answer",
    "login_prompt_cancel",
    "widgets_list",
    "widget_note_save",
    "connection_save",
    "connection_delete",
    "connect",
    "pane_connections",
    "block_open",
    "pane_blocks",
    "block_update",
    "files_list",
    "file_open",
    "file_stat",
    "file_grant",
    "file_revoke",
    "file_range",
    "file_save",
    "ai_send",
    "ai_turn_snapshot",
    "ai_conversation_new",
    "ai_conversation_list",
    "ai_conversation_delete",
    "ai_cancel",
    "ai_profiles",
    "ai_profile_save",
    "ai_profile_remove",
    "ai_key_set",
    "ai_key_clear",
    "ai_has_key",
    "ai_context_state",
    "ai_context_share",
    "ai_context_preview",
    "ai_decide",
    "ai_undo",
    "ai_ask_pending",
    "ai_ask_answer",
    "markdown_render",
    "integration_prepare",
    "integration_confirm",
    "integration_cancel",
    "integration_launch",
    "integration_select",
    "integration_context",
    "integration_tools",
    "integration_run",
  ]);
  tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest)).expect("tauri build failed");
}
