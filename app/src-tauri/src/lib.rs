//! The holzWave app shell, a thin adapter (D-19).
//!
//! Everything that matters sits in [`state::AppCore`], which has no Tauri types and is tested headless with a
//! recording sink and real shells. This file only builds the Tauri app: it creates the core with a sink that turns the
//! core's [`state::Out`] events into Tauri events, and it registers the commands of [`commands`].
//!
//! Webview contract: the events are `frame` (a frame message of the core), `layout` (the view), `title`
//! `{pane, title}`, `cwd` `{pane, cwd}`, `pane-exit` `{pane, code}`, `settings` (a settings view) and `ai` (one event
//! of a chat turn, `{conversation, turn, seq, kind, ...}`, which the page routes by conversation); command arguments
//! are snake_case. Every event but `settings`, `ai` and `exit` is emitted to one window, the one that holds the pane
//! (or whose layout changed), with `emit_str_to(label, ..)`; a page has to listen with its own label, because a
//! listener without a target receives the events of every window.
//!
//! Terminal and file content reaches a chat in four calls, and the page never holds the text: `ai_context_state` says
//! what a pane offers and where it would go, `ai_context_share` shares the pane for the chat, `ai_context_preview`
//! answers an offer (labels, sizes and what was removed), and `ai_send` names the offer and the numbers of the items it
//! picked. The three context commands take ids, a flag and kind names; the only text any of them carries is what the
//! user typed. A tool call that waits for the person is decided by `ai_decide`, which takes the chat, the call, `allow`
//! or `deny` and the hash of the card, and answers nothing.

mod ai_host;
mod ai_tools;
mod commands;
mod files_protocol;
mod menu;
mod state;
mod sync;
mod textscale;
mod windows;

use std::sync::Arc;

use tauri::{
    AppHandle, Emitter, LogicalPosition, Manager, PhysicalPosition, PhysicalSize, RunEvent, WebviewWindow,
    WebviewWindowBuilder, Window, WindowEvent,
};

use holzwave_core::session::{DEFAULT_SIZE, MonitorRect, Placement, WindowState};
use ai_host::AiParts;
use state::{AppCore, Out, ShellSpec, Sink, Target};
use windows::{
    CursorSpot, Frame, Normal, Place, WindowTracker, physical_pixels, pointer_is_global, position_beside,
    position_under_cursor, shared_divisor, units_per_point, window_under,
};

/// The platform, as `std::env::consts::OS` and the page's `os-<name>` class spell it.
const OS: &str = std::env::consts::OS;

/// Sends the core's events to the webview they are for, and does what the core asks of the window system. Frames are
/// events and are measured (D-16).
struct TauriSink {
    app: AppHandle,
    tracker: Arc<WindowTracker>,
}

impl TauriSink {
    /// The units per point of the window a drag gesture started in: the one that was focused last, since a drag
    /// focuses it.
    fn gesture_units(&self) -> f64 {
        let window = self.tracker.order().iter().find_map(|label| self.app.get_webview_window(label));
        window.map_or(1.0, |window| window_units(&window))
    }
}

impl Sink for TauriSink {
    fn send(&self, target: Target, out: Out) {
        if matches!(out, Out::Exit) {
            self.app.exit(0);
            return;
        }
        match serde_json::to_string(&out) {
            Ok(json) => {
                let sent = match &target {
                    Target::All => self.app.emit_str(out.name(), json),
                    Target::Window(label) => self.app.emit_str_to(&**label, out.name(), json),
                };
                if let Err(e) = sent {
                    eprintln!("holzwave: cannot emit {}: {e}", out.name());
                }
            }
            Err(e) => eprintln!("holzwave: cannot serialise {}: {e}", out.name()),
        }
    }

    fn close_window(&self, label: &str) {
        // `destroy`, not `close`: the window is out of the set already and must not ask again.
        if let Some(window) = self.app.get_webview_window(label)
            && let Err(e) = window.destroy()
        {
            eprintln!("holzwave: cannot close the window {label}: {e}");
        }
    }

    fn open_window(&self, label: &str, place: &Place) -> Result<(), String> {
        let window = build_window(&self.app, label)?;
        // A position is in the shared space of `windows.rs` (points on macOS, physical pixels elsewhere), and the
        // offsets that are given in points are scaled into it. Where the window system does not say where the
        // pointer or a window is (Wayland) there is nothing to compute, and the window opens where it puts it.
        let at = match place {
            Place::Cursor => {
                pointer_in_points(&self.app).map(|cursor| position_under_cursor(cursor, self.gesture_units()))
            }
            Place::Beside(origin) if pointer_known() => self
                .app
                .get_webview_window(origin)
                .and_then(|origin| Some((origin.outer_position().ok()?, own_scale(&origin), window_units(&origin))))
                .map(|(position, scale, units)| {
                    position_beside((f64::from(position.x) / scale, f64::from(position.y) / scale), units)
                }),
            Place::Beside(_) => None,
        };
        if let Some(at) = at
            && let Err(e) = place_window(&window, at)
        {
            // The window opens where the window system puts it; nothing is lost.
            eprintln!("holzwave: cannot place the window {label}: {e}");
        }
        Ok(())
    }

    fn cursor_spot(&self, source: &str) -> CursorSpot {
        let Some(cursor) = pointer_in_points(&self.app) else {
            return CursorSpot::Unknown;
        };
        let frames: Vec<Frame> =
            self.app.webview_windows().iter().filter_map(|(label, window)| frame_of(label, window)).collect();
        match window_under(cursor, &frames, &self.tracker.order(), source) {
            Some(frame) => {
                let (x, y) = frame.client(cursor);
                CursorSpot::Window { label: frame.label.clone(), x, y }
            }
            None => CursorSpot::Nothing,
        }
    }
}

// ---- windows: building them, and finding them under the pointer ---------------------------------------------------

/// Build the window `label` like the one of the configuration (size, minimum size, overlay title bar), hidden: its page
/// shows it with `window_ready` once it has drawn, so that it never flashes empty. A label that exists is an error.
/// Never call it with a lock of the core held (a window event of the new window takes them), nor from a synchronous
/// command (Windows deadlocks there, wry 583).
fn build_window(app: &AppHandle, label: &str) -> Result<WebviewWindow, String> {
    let mut conf = app.config().app.windows.first().cloned().ok_or("the configuration has no window")?;
    conf.label = label.to_owned();
    WebviewWindowBuilder::from_config(app, &conf)
        .map_err(|e| e.to_string())?
        .visible(false)
        .build()
        .map_err(|e| e.to_string())
}

/// What the pixels of a window's own coordinates are divided by to reach the shared space of `windows.rs`: on macOS
/// the position and size of a window are in the scale of the monitor it is on, elsewhere the numbers of all windows
/// and of the pointer are physical and share one space already.
fn own_scale(window: &WebviewWindow) -> f64 {
    shared_divisor(OS, window.scale_factor().unwrap_or(1.0))
}

/// How many units of the shared space one point (one CSS pixel) of the window is.
fn window_units(window: &WebviewWindow) -> f64 {
    units_per_point(OS, window.scale_factor().unwrap_or(1.0))
}

/// Whether the window system says where the pointer and the windows are (not under Wayland, see
/// [`pointer_is_global`]).
fn pointer_known() -> bool {
    pointer_is_global(OS, &|name| std::env::var(name).ok())
}

/// The pointer in the shared space of `windows.rs`, `None` when the window system does not say (Wayland) or refuses.
/// macOS reports it relative to the primary monitor and in the primary monitor's scale, whatever monitor it is over,
/// so it is divided by that scale (in points) and not by the scale of the window under it; elsewhere it is physical
/// like the windows and nothing is divided.
fn pointer_in_points(app: &AppHandle) -> Option<(f64, f64)> {
    if !pointer_known() {
        return None;
    }
    let cursor = app.cursor_position().ok()?;
    let scale = if cfg!(target_os = "macos") {
        shared_divisor(OS, app.primary_monitor().ok()??.scale_factor())
    } else {
        1.0
    };
    Some((cursor.x / scale, cursor.y / scale))
}

/// Move `window` to a point of the shared space: a logical position on macOS (points), physical pixels elsewhere.
fn place_window(window: &WebviewWindow, (x, y): (f64, f64)) -> tauri::Result<()> {
    if cfg!(target_os = "macos") {
        window.set_position(LogicalPosition::new(x, y))
    } else {
        let (x, y) = physical_pixels((x, y));
        window.set_position(PhysicalPosition::new(x, y))
    }
}

/// The frame of a window that can be dropped on, in the shared space; `None` for a window that is hidden or minimised.
fn frame_of(label: &str, window: &WebviewWindow) -> Option<Frame> {
    if !window.is_visible().ok()? || window.is_minimized().ok()? {
        return None;
    }
    let scale = own_scale(window);
    let (outer, size, inner) = (window.outer_position().ok()?, window.outer_size().ok()?, window.inner_position().ok()?);
    let points = |v: f64| v / scale;
    Some(Frame {
        label: label.to_owned(),
        x: points(f64::from(outer.x)),
        y: points(f64::from(outer.y)),
        width: points(f64::from(size.width)),
        height: points(f64::from(size.height)),
        inner_x: points(f64::from(inner.x)),
        inner_y: points(f64::from(inner.y)),
        scale: window_units(window),
    })
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

/// Apply the saved frame to the window `label`: the monitors that are connected now decide whether the saved position
/// is used (T-02-59). Failures only cost the restore, never the start.
fn restore_window(window: &WebviewWindow, core: &AppCore, label: &str) {
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
    let startup = startup_for(core.placement(label, &monitors));
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

/// A window moved or changed size: hand its frame to the core, which only touches a debounced saver, so this is
/// cheap on the thread of the window events. The geometry it had while normal is kept per window.
fn note_window(window: &Window) {
    let app = window.app_handle();
    let (Some(core), Some(tracker)) = (app.try_state::<Arc<AppCore>>(), app.try_state::<Arc<WindowTracker>>()) else {
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
    let label = window.label();
    let mut normal = tracker.normal(label);
    let noted = state_to_note(facts, &mut normal);
    tracker.set_normal(label, normal);
    if let Some(state) = noted {
        core.note_window(label, state);
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
            let tracker = Arc::new(WindowTracker::default());
            app.manage(tracker.clone());
            let sink = Arc::new(TauriSink { app: app.handle().clone(), tracker });
            // No tool may touch the data directory, the configuration directory or the folder of the local socket.
            let mut protected = Vec::new();
            protected.extend(app.path().app_config_dir().ok());
            protected.extend(ipc_dir(app.handle()));
            let parts = AiParts::system().protecting(protected);
            let core = AppCore::new_with_ai(dir, sink, ShellSpec::default(), parts);
            app.manage(core.clone());
            if let Some(window) = app.get_webview_window("main") {
                restore_window(&window, &core, "main");
            }
            // The windows of the last session other than `main` (which the configuration makes): each is built
            // hidden with its own frame, and shown when its page is ready. A window that cannot be built keeps its
            // entry in the layout, so the next start tries again.
            for label in core.restored_windows() {
                match build_window(app.handle(), &label) {
                    Ok(window) => restore_window(&window, &core, &label),
                    Err(e) => eprintln!("holzwave: cannot restore the window {label}: {e}"),
                }
            }
            // `wsh` finds the app through a socket in a folder only this user can enter; every pane started from now on
            // gets its address and this run's secret. Without a socket the app works the same, `wsh` just has nothing to talk to.
            start_ipc(app.handle(), &core);
            // The scheduled sync: a first run shortly after the start, then every five minutes. It does nothing while sync
            // is off, and a failure is only the state the page shows.
            let background = core.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(10));
                loop {
                    background.sync_background();
                    std::thread::sleep(std::time::Duration::from_secs(300));
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::Resized(_) | WindowEvent::Moved(_) => note_window(window),
            WindowEvent::Focused(true) => {
                if let Some(tracker) = window.app_handle().try_state::<Arc<WindowTracker>>() {
                    tracker.focused(window.label());
                }
            }
            WindowEvent::Destroyed => {
                if let Some(tracker) = window.app_handle().try_state::<Arc<WindowTracker>>() {
                    tracker.forget(window.label());
                }
                // The red button of a window, or the destroy of the sink. See `AppCore::window_closed` for what
                // closing the last window and quitting leave alone.
                if let Some(core) = window.app_handle().try_state::<Arc<AppCore>>() {
                    core.window_closed(window.label());
                }
            }
            _ => {}
        })
        // Images, video, audio and PDFs are shown from this scheme, one granted file per token (files_protocol.rs).
        .register_asynchronous_uri_scheme_protocol(files_protocol::SCHEME, |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                let response = match app.try_state::<Arc<AppCore>>() {
                    Some(core) => files_protocol::serve(&core, &request),
                    None => tauri::http::Response::builder()
                        .status(tauri::http::StatusCode::SERVICE_UNAVAILABLE)
                        .body(Vec::new())
                        .expect("a 503 response"),
                };
                responder.respond(response);
            });
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
            commands::close_tab,
            commands::close_workspace,
            commands::focus_pane,
            commands::cycle_focus,
            commands::select_tab,
            commands::select_workspace,
            commands::rename_pane,
            commands::rename_tab,
            commands::rename_workspace,
            commands::move_pane,
            commands::move_pane_to_workspace,
            commands::move_tab,
            commands::move_tab_to_workspace,
            commands::set_ratio,
            commands::tear_off_pane,
            commands::tear_off_tab,
            commands::release_outside,
            commands::drag_over,
            commands::drag_end,
            commands::attach_pane,
            commands::attach_tab,
            commands::window_ready,
            commands::get_settings,
            commands::set_setting,
            commands::reset_setting,
            commands::reload_settings,
            commands::app_info,
            commands::os_text_scale,
            commands::open_url,
            commands::reveal_path,
            commands::mouse,
            commands::wheel,
            commands::connections_list,
            commands::connection_save,
            commands::connection_delete,
            commands::connect,
            commands::pane_connections,
            commands::block_open,
            commands::pane_blocks,
            commands::block_update,
            commands::files_list,
            commands::file_open,
            commands::file_stat,
            commands::file_grant,
            commands::file_revoke,
            commands::file_range,
            commands::file_save,
            commands::sync_status,
            commands::sync_save,
            commands::sync_now,
            commands::sync_off,
            commands::monitor_sample,
            commands::history_search,
            commands::history_delete,
            commands::history_clear,
            commands::widgets_list,
            commands::widget_note_save,
            commands::ai_send,
            commands::ai_turn_snapshot,
            commands::ai_conversation_new,
            commands::ai_conversation_list,
            commands::ai_conversation_delete,
            commands::ai_cancel,
            commands::ai_profiles,
            commands::ai_profile_save,
            commands::ai_profile_remove,
            commands::ai_key_set,
            commands::ai_key_clear,
            commands::ai_has_key,
            commands::ai_context_state,
            commands::ai_context_share,
            commands::ai_context_preview,
            commands::ai_decide,
            commands::markdown_render,
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

/// The app's side of `wsh`.
struct IpcHandler(std::sync::Arc<AppCore>);

impl holzwave_ipc::Handler for IpcHandler {
    fn handle(&self, pane: Option<u64>, cmd: holzwave_ipc::Cmd) -> Result<String, String> {
        self.0.ipc_command(pane, cmd)
    }
}

/// The folder of the local socket: `holzwave` in the runtime directory when there is one, `ipc` in the data directory
/// otherwise, and none when neither is known. The one definition of the place: the code that creates the socket and the
/// deny list of the agent both get it from here.
fn ipc_dir_from(runtime: Option<std::ffi::OsString>, data: Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    runtime.map(|dir| std::path::PathBuf::from(dir).join("holzwave")).or_else(|| data.map(|dir| dir.join("ipc")))
}

/// [`ipc_dir_from`] for this run: the runtime directory variable and the data directory of the app.
fn ipc_dir(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    ipc_dir_from(std::env::var_os("XDG_RUNTIME_DIR"), app.path().app_data_dir().ok())
}

fn start_ipc(app: &tauri::AppHandle, core: &std::sync::Arc<AppCore>) {
    let dir = ipc_dir(app);
    let (Some(dir), Ok(token)) = (dir, holzwave_ipc::new_token()) else { return };
    match holzwave_ipc::serve(&dir, token.clone(), std::sync::Arc::new(IpcHandler(core.clone()))) {
        Ok(server) => {
            core.set_ipc(dir, token);
            // Lives as long as the app.
            std::mem::forget(server);
        }
        Err(e) => eprintln!("holzwave: wsh cannot connect to this app: {e}"),
    }
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

    #[test]
    fn the_socket_directory_has_one_definition_and_the_agent_protects_it() {
        use std::ffi::OsString;
        use std::path::PathBuf;

        // One pure function names the folder.
        assert_eq!(
            ipc_dir_from(Some(OsString::from("/run/user/7")), Some(PathBuf::from("/data"))),
            Some(PathBuf::from("/run/user/7/holzwave")),
            "the runtime directory wins"
        );
        assert_eq!(ipc_dir_from(None, Some(PathBuf::from("/data"))), Some(PathBuf::from("/data/ipc")));
        assert_eq!(ipc_dir_from(None, None), None);

        // The code that creates the socket and the code that builds the agent's deny list ask the same helper, and
        // nothing else builds the folder.
        let whole = include_str!("lib.rs");
        let shipped = whole.split("\n#[cfg(test)]\nmod tests").next().expect("a first part");
        let mut code: String =
            shipped.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        code.push('\n');
        assert_eq!(code.matches("join(\"holzwave\")").count(), 1, "the folder is built in one place");
        let body_of = |signature: &str| -> String {
            let rest = &code[code.find(signature).unwrap_or_else(|| panic!("{signature} is gone"))..];
            rest[..rest.find("\n}\n").expect("a closing brace")].to_owned()
        };
        assert!(body_of("fn start_ipc(").contains("ipc_dir(app)"), "the socket is made in the folder of the helper");
        assert!(body_of("pub fn run(").contains("ipc_dir(app.handle())"), "the deny list is told the same folder");
        assert!(code.contains("protecting("), "the core is built with the places the agent may not touch");
        assert!(body_of("pub fn run(").contains("app_config_dir"), "and with the configuration directory");
    }
}
