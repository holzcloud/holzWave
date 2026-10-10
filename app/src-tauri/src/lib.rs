//! The holzWave app shell, a thin adapter (D-19).
//!
//! Everything that matters sits in [`state::AppCore`], which has no Tauri types and is tested headless with a
//! recording sink and real shells. This file only builds the Tauri app: it creates the core with a sink that turns the
//! core's [`state::Out`] events into Tauri events, and it registers the commands of [`commands`].
//!
//! Webview contract: the events are `frame` (a frame message of the core), `layout` (the view), `title`
//! `{pane, title}`, `cwd` `{pane, cwd}` and `pane-exit` `{pane, code}`; command arguments are snake_case.

mod commands;
mod state;

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};

use state::{AppCore, Out, ShellSpec, Sink};

/// Sends the core's events to every webview. Frames are events and are measured (D-16).
struct TauriSink {
    app: AppHandle,
}

impl Sink for TauriSink {
    fn send(&self, out: Out) {
        if matches!(out, Out::Exit) {
            self.app.exit(0);
            return;
        }
        match serde_json::to_string(&out) {
            Ok(json) => {
                if let Err(e) = self.app.emit_str(out.name(), json) {
                    eprintln!("holzwave: cannot emit {}: {e}", out.name());
                }
            }
            Err(e) => eprintln!("holzwave: cannot serialise {}: {e}", out.name()),
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .runtime(tauri_runtime_wry::Wry::default())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let sink = Arc::new(TauriSink { app: app.handle().clone() });
            app.manage(AppCore::new(dir, sink, ShellSpec::default()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_layout,
            commands::start_pane,
            commands::key,
            commands::text,
            commands::paste,
            commands::resize,
            commands::scroll,
            commands::scroll_to_bottom,
            commands::request_full,
            commands::select,
            commands::selection_text,
            commands::search,
            commands::copy_mode,
        ])
        .run(tauri::generate_context!())
        .expect("error while running holzWave");
}
