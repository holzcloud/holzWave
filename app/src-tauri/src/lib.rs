//! The holzWave app shell, a thin adapter (D-19).
//!
//! Everything that matters sits in [`state::AppCore`], which has no Tauri types and is tested headless with a
//! recording sink and real shells. This file only builds the Tauri app: it creates the core with a sink that turns the
//! core's [`state::Out`] events into Tauri events, and it registers the commands of [`commands`].
//!
//! Webview contract: the events are `frame` (a frame message of the core), `layout` (the view), `title`
//! `{pane, title}`, `cwd` `{pane, cwd}`, `pane-exit` `{pane, code}`, `bell` `{pane}` (the program in that pane rang the bell; the window that has the pane), `command-end` `{pane, code}` (a command ended in that pane, `code` is the OSC 133 status or `null`; the window that has the pane), `settings` (a settings view), `ai` (one event
//! of a chat turn, `{conversation, turn, seq, kind, ...}`, which the page routes by conversation) and `ai-ask` (a request
//! of `wsh ai` that waits for the person, or is over: `{kind: "waiting", view}` for the window of the pane, `{kind:
//! "closed", ask, why}` for every window) and `history-open` `{pane}` (the shell of that pane asked for the history
//! search; it goes to the one window that has the pane), `login-prompt` `{id, connection, host, name, instructions, prompts:
//! [{text, echo}]}` (the server of a saved connection asks the person a question to log in; every window; `connection`
//! and `host` are the person's own saved connection, the rest is the server's text as text) and `login-prompt-end` `{id,
//! outcome}` (`answered`, `cancelled` or `timed_out`; every window) and `integration` `{seq, kind, tool, pane, code}` (something the
//! integrations show changed; `kind` is `tools`, `context`, `profiles` or `held`; every window, and the page asks for the data); command arguments
//! are snake_case. Every event but `settings`, `ai`, `exit`, `login-prompt`, `login-prompt-end`, `integration` and the `closed` half of `ai-ask` is emitted to one window, the one that holds the pane
//! (or whose layout changed), with `emit_str_to(label, ..)`; a page has to listen with its own label, because a
//! listener without a target receives the events of every window.
//!
//! Terminal and file content reaches a chat in four calls, and the page never holds the text: `ai_context_state` says
//! what a pane offers and where it would go, `ai_context_share` shares the pane for the chat, `ai_context_preview`
//! answers an offer (labels, sizes and what was removed), and `ai_send` names the offer and the numbers of the items it
//! picked. The three context commands take ids, a flag and kind names; the only text any of them carries is what the
//! user typed. A tool call that waits for the person is decided by `ai_decide`, which takes the chat, the call, `allow`
//! or `deny` and the hash of the card, and answers nothing.

// The handler of the generated command wrappers names its rows one macro call at a time.
#![recursion_limit = "256"]

mod ai_device;
// The Android host touches no Android API, so its rules are tested on the desktop too; the desktop app never uses it.
#[cfg(any(target_os = "android", test))]
mod android;
mod clipboard;
mod commands;
// The desktop host answers with the operating system's keychain, which Android does not have (see `android`).
#[cfg(not(target_os = "android"))]
mod desktop;
mod desktop_table;
mod files_protocol;
mod local_network;
mod menu;
#[cfg(test)]
mod state;
// The operating system's text size is read by the desktop host; Android's host does not read it yet.
#[cfg(not(target_os = "android"))]
mod textscale;
// The frame of a desktop window is decided before the window exists (see the module); a phone has one window the system makes.
#[cfg(not(target_os = "android"))]
mod windowframe;

use std::sync::Arc;

use tauri::{
    AppHandle, Emitter, LogicalPosition, Manager, PhysicalPosition, RunEvent, WebviewWindow, WebviewWindowBuilder, Window,
    WindowEvent,
};
#[cfg(not(target_os = "android"))]
use tauri::PhysicalSize;

use holzwave_core::session::WindowState;
#[cfg(not(target_os = "android"))]
use holzwave_core::session::{DEFAULT_SIZE, MonitorRect, Placement};
use holzwave_host::ai_host::AiParts;
use holzwave_host::seams::Host;
use holzwave_host::state::{AppCore, Out, ShellSpec, Sink, Target};
use holzwave_host::windows::{
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
        let window = build_window(&self.app, label, false)?;
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

/// Build the window `label` from the window of the configuration (size, minimum size, overlay title bar), the one
/// source of what every window is like; the configuration itself creates none on a desktop (see `run`). `visible` says
/// whether it shows at once: the first window does, as a configuration window always did; a later window is hidden and its
/// page shows it with `window_ready` once it has drawn, so that it never flashes empty. When the system decided an own frame
/// ([`windowframe::facts_for_this_system`], held in the app's state) the window has no system title bar and its page gets
/// the facts before it runs. A label that exists is an error. Never call it with a lock of the core held (a window event of
/// the new window takes them), nor from a synchronous command (Windows deadlocks there, wry 583).
fn build_window(app: &AppHandle, label: &str, visible: bool) -> Result<WebviewWindow, String> {
    let mut conf = app.config().app.windows.first().cloned().ok_or("the configuration has no window")?;
    conf.label = label.to_owned();
    let builder = WebviewWindowBuilder::from_config(app, &conf).map_err(|e| e.to_string())?.visible(visible);
    #[cfg(not(target_os = "android"))]
    let builder = match app.try_state::<windowframe::FrameFacts>() {
        Some(facts) => builder.decorations(false).initialization_script(facts.script()),
        None => builder,
    };
    builder.build().map_err(|e| e.to_string())
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

/// What to do with the window when the app starts. Not on Android, where the system sizes the one window.
#[cfg(not(target_os = "android"))]
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
#[cfg(not(target_os = "android"))]
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
/// is used (T-02-59). Failures only cost the restore, never the start. Not on Android: the system sizes the one window
/// there (and the window type has no `maximize` on that platform).
#[cfg(not(target_os = "android"))]
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

/// The host of this platform: the desktop's (system keychain, no private folders) or, on Android, the one that names the
/// app's private folders and whose secret store is the vault over the Android Keystore.
#[cfg(not(target_os = "android"))]
fn platform_host(_app: &tauri::App) -> Result<Arc<dyn Host>, Box<dyn std::error::Error>> {
    Ok(Arc::new(desktop::DesktopHost::new()))
}

/// See the other `platform_host`: the folders are made here, before the core is built, and a failure to make them stops
/// the start with the reason.
#[cfg(target_os = "android")]
fn platform_host(app: &tauri::App) -> Result<Arc<dyn Host>, Box<dyn std::error::Error>> {
    let data = app.path().app_data_dir()?;
    let cache = app.path().app_cache_dir()?;
    let host = Arc::new(android::host::AndroidHost::new(&data, &cache)?);
    // The watcher keeps the vault's clock rule and the app switcher picture in line with the vault; the debug self-test finds the
    // host through the app's state.
    android::host::start_watcher(host.clone());
    app.manage(host.clone());
    Ok(host)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().runtime(tauri_runtime_wry::Wry::default());
    // macOS: no default menu (it owns Cmd+W, Close Window) but the explicit one of `menu.rs`. The other platforms
    // draw their own chrome and have no native menu at all (D-01, D-14).
    #[cfg(target_os = "macos")]
    let builder = builder.enable_macos_default_menu(false).menu(menu::build);
    // Android: the in-app plugin through which Rust and the Kotlin side of the app talk (the foreground service that keeps
    // sessions alive in the background). It has no command, so the page cannot call it.
    #[cfg(target_os = "android")]
    let builder = builder.plugin(android::plugin::init());

    // The exit events can come more than once (a requested exit, then the exit itself): the shutdown runs once.
    let mut ended = false;
    // A desktop builds every window itself, the first one in `setup` (see `build_window`): the configuration stays as the
    // one description of a window, but creates none. A phone keeps the window of the configuration.
    #[cfg_attr(target_os = "android", allow(unused_mut))]
    let mut context = tauri::generate_context!();
    #[cfg(not(target_os = "android"))]
    for window in &mut context.config_mut().app.windows {
        window.create = false;
    }
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
            let host = platform_host(app)?;
            let parts = AiParts::with_secrets(host.secrets()).protecting(protected);
            let core = AppCore::new_with_host(dir, sink, ShellSpec::default(), Some(parts), None, host);
            // The on-device model (macOS only): the host learns where it comes from and asks nothing of it until a person acts.
            if let Some(source) = ai_device::system() {
                core.ai().use_device(source);
            }
            // The bundled `wsh` builds for Linux hosts are in the resource folder; without one the install says so.
            if let Ok(resources) = app.path().resource_dir() {
                core.set_resource_dir(resources);
            }
            app.manage(core.clone());
            // A phone has one window that the system sizes: no frame is restored, and no second window is built.
            #[cfg(not(target_os = "android"))]
            {
                // The frame is decided before any window exists, because it cannot be changed on a window that is built.
                // The choice the first window is built with is the one the core read at start, so the stored file, this
                // decision and the dialog's `pending` flag agree; every window shares the one decision. On a GNOME desktop
                // with the own frame this waits for the layout helper for at most its short deadline.
                let (frame, buttons) = core.window_frame().started();
                if let Some(facts) = windowframe::facts_for_this_system(frame, buttons) {
                    app.manage(facts);
                }
                // The first window is built here like every other (the configuration creates none), and visible at once as
                // a configuration window was. A failure stops the start with the reason: a process with no window is worse.
                let main = build_window(app.handle(), "main", true)?;
                restore_window(&main, &core, "main");
                // The windows of the last session other than `main`: each is built hidden with its own frame, and shown
                // when its page is ready. A window that cannot be built keeps its entry in the layout, so the next start
                // tries again.
                for label in core.restored_windows() {
                    match build_window(app.handle(), &label, false) {
                        Ok(window) => restore_window(&window, &core, &label),
                        Err(e) => eprintln!("holzwave: cannot restore the window {label}: {e}"),
                    }
                }
            }
            // `wsh` finds the app through a socket in a folder only this user can enter; every pane started from now on
            // gets its address and this run's secret. Without a socket the app works the same, `wsh` just has nothing to talk to.
            start_ipc(app.handle(), &core);
            // Android: the foreground service that keeps shells and connections alive in the background is told how many
            // there are, and its `Stop all` ends them.
            #[cfg(target_os = "android")]
            android::service::start(core.clone());
            // Android: a connection to a device on the local network asks for the system's permission when it is made (Android 17).
            #[cfg(target_os = "android")]
            android::network::install();
            // Android debug builds only: the steps of the emulator's self-test of the vault. Release builds do not contain it.
            #[cfg(all(target_os = "android", debug_assertions))]
            android::selftest::start(core.clone(), app.state::<Arc<android::host::AndroidHost>>().inner().clone());
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
        // Every command comes from the one table in the host crate: `desktop_table` expands it with
        // `tauri::generate_handler!` into the handler (the generated wrappers and the three that need the window shell),
        // and there is no hand-written list.
        .invoke_handler(desktop_table::handler())
        .build(context)
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
    match holzwave_ipc::serve(&dir, token.clone(), std::sync::Arc::new(holzwave_host::IpcHandler(core.clone()))) {
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

    /// A decision on a call that waits for nothing, made on a blocking thread of the async runtime of the app, which is
    /// where the command runs. A call there that classifies a destination is refused; a decision only locks the table, so
    /// it answers as it does anywhere else.
    #[test]
    fn a_decision_on_a_blocking_thread_of_the_async_runtime_answers_unknown_call() {
        use holzwave_core::ai::profiles::KeyVault;
        use holzwave_host::windows::{CursorSpot, Place};

        struct Quiet;

        impl Sink for Quiet {
            fn send(&self, _target: Target, _out: Out) {}

            fn close_window(&self, _label: &str) {}

            fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
                Ok(())
            }

            fn cursor_spot(&self, _source: &str) -> CursorSpot {
                CursorSpot::Unknown
            }
        }

        let dir = std::env::temp_dir().join(format!("holzwave-app-decide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).expect("scratch dir");
        let parts = AiParts { vault: KeyVault::unavailable(), clock: None, protected: Vec::new() };
        let core = AppCore::new_with_ai(dir.clone(), Arc::new(Quiet), ShellSpec::new("/bin/cat", &[]), parts);
        let chat = core.ai().conversation_new().expect("a chat id");

        let spent = tauri::async_runtime::block_on(async move {
            let core = core.clone();
            tauri::async_runtime::spawn_blocking(move || core.ai_decide(&chat, "call_1", "deny", "no-such-hash")).await
        });
        let fault = spent.expect("the blocking thread ends").expect_err("no card waits for this call");
        assert_eq!((fault.code, fault.reason), ("tool", Some("unknown_call")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
