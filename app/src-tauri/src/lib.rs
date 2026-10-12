//! The holzWave app shell, a thin adapter (D-19).
//!
//! Everything that matters sits in [`state::AppCore`], which has no Tauri types and is tested headless with a
//! recording sink and real shells. This file only builds the Tauri app: it creates the core with a sink that turns the
//! core's [`state::Out`] events into Tauri events, and it registers the commands of [`commands`].
//!
//! Webview contract: the events are `frame` (a frame message of the core), `layout` (the view), `title`
//! `{pane, title}`, `cwd` `{pane, cwd}`, `pane-exit` `{pane, code}` and `settings` (a settings view); command
//! arguments are snake_case.

mod commands;
mod menu;
mod state;

use std::sync::{Arc, Mutex, PoisonError};

use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, RunEvent, WebviewWindow, Window, WindowEvent};

use holzwave_core::session::{DEFAULT_SIZE, MonitorRect, Placement, WindowState};
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

// ---- the window frame (DESK-22) ----------------------------------------------------------------------------------
//
// Pixels are physical pixels throughout: what the window system reports for the window and for the monitors, and what
// the setters take, so the numbers compare without a scale factor.

/// What to do with the window when the app starts.
#[derive(Debug, PartialEq, Eq)]
struct Startup {
    /// `None` leaves the size of the configuration (logical pixels, so right on every display).
    size: Option<(u32, u32)>,
    /// `None` leaves the position to the window system.
    position: Option<(i32, i32)>,
    maximize: bool,
}

/// The startup actions for a placement of the core.
///
/// With nothing saved the core answers the default size, and then the size of the configuration is left alone: it is
/// in logical pixels and the saved sizes are physical ones, so applying the default as a physical size would shrink
/// the window on a high density display. (A saved window of exactly the default size, off every monitor, is treated
/// the same way; its size differs from the default only on such a display.)
fn startup_for(placement: Placement) -> Startup {
    let untouched = placement.size == DEFAULT_SIZE && placement.position.is_none() && !placement.maximized;
    Startup {
        size: (!untouched).then_some(placement.size),
        position: placement.position,
        maximize: placement.maximized,
    }
}

/// What the shell reads from the window after it moved or was resized.
#[derive(Clone, Copy, Debug)]
struct WindowFacts {
    position: (i32, i32),
    size: (u32, u32),
    maximized: bool,
    minimized: bool,
    fullscreen: bool,
}

/// The last position and size of the window while it was neither maximised nor full screen.
type Normal = (i32, i32, u32, u32);

/// The window state to remember for `facts`, or `None` when the window is in a state that says nothing about where
/// it lives (minimised, full screen). A maximised window keeps the geometry it had before, so that leaving the
/// maximised state after a restart does not give back a window as large as the screen.
fn state_to_note(facts: WindowFacts, normal: &mut Option<Normal>) -> Option<WindowState> {
    if facts.minimized || facts.fullscreen {
        return None;
    }
    let current = (facts.position.0, facts.position.1, facts.size.0, facts.size.1);
    let (x, y, width, height) = if facts.maximized {
        normal.unwrap_or(current)
    } else {
        *normal = Some(current);
        current
    };
    Some(WindowState { x, y, width, height, maximized: facts.maximized })
}

/// The geometry the window had while it was normal, kept between the window events of the one window.
static NORMAL: Mutex<Option<Normal>> = Mutex::new(None);

/// Apply the saved frame to the main window: the monitors that are connected now decide whether the saved position
/// is used (T-02-59). Failures only cost the restore, never the start.
fn restore_window(window: &WebviewWindow, core: &AppCore) {
    let monitors: Vec<MonitorRect> = match window.available_monitors() {
        Ok(list) => list
            .iter()
            .map(|m| MonitorRect { x: m.position().x, y: m.position().y, width: m.size().width, height: m.size().height })
            .collect(),
        Err(e) => {
            eprintln!("holzwave: cannot list the monitors: {e}");
            Vec::new()
        }
    };
    let startup = startup_for(core.placement(&monitors));
    if let Some((width, height)) = startup.size
        && let Err(e) = window.set_size(PhysicalSize::new(width, height))
    {
        eprintln!("holzwave: cannot restore the window size: {e}");
    }
    if let Some((x, y)) = startup.position
        && let Err(e) = window.set_position(PhysicalPosition::new(x, y))
    {
        eprintln!("holzwave: cannot restore the window position: {e}");
    }
    if startup.maximize
        && let Err(e) = window.maximize()
    {
        eprintln!("holzwave: cannot maximise the window: {e}");
    }
}

/// The main window moved or changed size: hand its frame to the core, which only touches a debounced saver, so
/// this is cheap on the thread of the window events.
fn note_window(window: &Window) {
    let Some(core) = window.app_handle().try_state::<Arc<AppCore>>() else {
        return;
    };
    let (Ok(position), Ok(size), Ok(maximized), Ok(minimized), Ok(fullscreen)) = (
        window.outer_position(),
        window.inner_size(),
        window.is_maximized(),
        window.is_minimized(),
        window.is_fullscreen(),
    ) else {
        return;
    };
    let facts = WindowFacts {
        position: (position.x, position.y),
        size: (size.width, size.height),
        maximized,
        minimized,
        fullscreen,
    };
    let noted = state_to_note(facts, &mut NORMAL.lock().unwrap_or_else(PoisonError::into_inner));
    if let Some(state) = noted {
        core.note_window(state);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().runtime(tauri_runtime_wry::Wry::default());
    // macOS: no default menu (it owns Cmd+W, Close Window) but the explicit one of `menu.rs`. The other platforms
    // draw their own chrome and have no native menu at all (D-01, D-14).
    #[cfg(target_os = "macos")]
    let builder = builder.enable_macos_default_menu(false).menu(menu::build);

    // The exit events can come more than once (a requested exit, then the exit itself): the shutdown runs once.
    let mut ended = false;
    builder
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let sink = Arc::new(TauriSink { app: app.handle().clone() });
            let core = AppCore::new(dir, sink, ShellSpec::default());
            app.manage(core.clone());
            if let Some(window) = app.get_webview_window("main") {
                restore_window(&window, &core);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" && matches!(event, WindowEvent::Resized(_) | WindowEvent::Moved(_)) {
                note_window(window);
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_layout,
            commands::start_pane,
            commands::key,
            commands::send_edit,
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
            commands::split,
            commands::new_tab,
            commands::new_workspace,
            commands::close_pane,
            commands::focus_pane,
            commands::cycle_focus,
            commands::select_tab,
            commands::select_workspace,
            commands::set_ratio,
            commands::get_settings,
            commands::set_setting,
            commands::reset_setting,
            commands::reload_settings,
            commands::app_info,
            commands::open_url,
            commands::reveal_path,
            commands::mouse,
            commands::wheel,
        ])
        .build(tauri::generate_context!())
        .expect("error while building holzWave")
        .run(move |app, event| {
            // The pending layout and session go to disk and every shell ends before the process does (DESK-16,
            // DESK-14). Closing the last window and Cmd+Q arrive here too.
            if !ended
                && matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit)
                && let Some(core) = app.try_state::<Arc<AppCore>>()
            {
                ended = true;
                core.shutdown();
            }
        });
}

#[cfg(test)]
mod tests {
    use holzwave_core::session::Session;

    use super::*;

    const SCREEN: MonitorRect = MonitorRect { x: 0, y: 0, width: 2560, height: 1440 };

    fn saved(window: WindowState) -> Placement {
        let mut session = Session::default();
        session.set_window(window);
        session.placement(&[SCREEN])
    }

    fn state(x: i32, y: i32, width: u32, height: u32, maximized: bool) -> WindowState {
        WindowState { x, y, width, height, maximized }
    }

    #[test]
    fn with_nothing_saved_the_window_keeps_the_size_of_the_configuration() {
        let placement = Session::default().placement(&[SCREEN]);
        assert_eq!(startup_for(placement), Startup { size: None, position: None, maximize: false });
    }

    #[test]
    fn a_saved_window_on_a_connected_monitor_gets_size_and_position() {
        let startup = startup_for(saved(state(120, 80, 1200, 800, false)));
        assert_eq!(startup, Startup { size: Some((1200, 800)), position: Some((120, 80)), maximize: false });
    }

    #[test]
    fn an_off_screen_position_is_not_applied_but_the_size_is() {
        let startup = startup_for(saved(state(9000, 80, 1200, 800, false)));
        assert_eq!(startup, Startup { size: Some((1200, 800)), position: None, maximize: false });
    }

    #[test]
    fn a_maximised_window_is_maximised_again() {
        let startup = startup_for(saved(state(120, 80, 1200, 800, true)));
        assert!(startup.maximize);
        assert_eq!(startup.position, Some((120, 80)));
    }

    fn facts(position: (i32, i32), size: (u32, u32)) -> WindowFacts {
        WindowFacts { position, size, maximized: false, minimized: false, fullscreen: false }
    }

    #[test]
    fn a_normal_window_is_noted_as_it_is_and_remembered() {
        let mut normal = None;
        let noted = state_to_note(facts((10, 20), (900, 600)), &mut normal);
        assert_eq!(noted, Some(state(10, 20, 900, 600, false)));
        assert_eq!(normal, Some((10, 20, 900, 600)));
    }

    #[test]
    fn a_maximised_window_keeps_the_last_normal_geometry() {
        let mut normal = None;
        state_to_note(facts((10, 20), (900, 600)), &mut normal);
        let maximised = WindowFacts { maximized: true, ..facts((0, 0), (2560, 1400)) };
        assert_eq!(state_to_note(maximised, &mut normal), Some(state(10, 20, 900, 600, true)));
        assert_eq!(normal, Some((10, 20, 900, 600)), "the maximised geometry is not remembered as the normal one");
    }

    #[test]
    fn a_window_that_started_maximised_is_noted_with_its_current_geometry() {
        let mut normal = None;
        let maximised = WindowFacts { maximized: true, ..facts((0, 0), (2560, 1400)) };
        assert_eq!(state_to_note(maximised, &mut normal), Some(state(0, 0, 2560, 1400, true)));
    }

    #[test]
    fn a_minimised_or_full_screen_window_changes_nothing() {
        let mut normal = Some((10, 20, 900, 600));
        let minimised = WindowFacts { minimized: true, ..facts((-32000, -32000), (0, 0)) };
        assert_eq!(state_to_note(minimised, &mut normal), None);
        let full = WindowFacts { fullscreen: true, ..facts((0, 0), (2560, 1440)) };
        assert_eq!(state_to_note(full, &mut normal), None);
        assert_eq!(normal, Some((10, 20, 900, 600)));
    }
}
