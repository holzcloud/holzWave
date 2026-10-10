//! The holzWave app shell. A thin layer: terminals, layout and geometry live in `holzwave-core`.
//!
//! Threading: libghostty-vt objects are `!Send`, so every pane has one engine thread that owns its
//! engine and PTY writer, and everything reaches it through one channel: PTY output, keyboard input,
//! resize, scroll. The engine thread coalesces output and emits at most one frame per `FRAME_INTERVAL`.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use holzwave_core::layout::{Axis, Divider, Layout, PaneId, Rect, SplitId};
use holzwave_core::pty::Pty;
use holzwave_core::{GhosttyEngine, Screen, TerminalEngine, Width};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

const FRAME_INTERVAL: Duration = Duration::from_millis(8);
const SCROLLBACK_LINES: usize = 10_000;

enum Msg {
    Output(Vec<u8>),
    Closed,
    Input(Vec<u8>),
    Resize(u16, u16),
    Scroll(isize),
}

struct App {
    layout: Mutex<Layout>,
    sessions: Mutex<HashMap<PaneId, mpsc::Sender<Msg>>>,
    path: PathBuf,
}

impl App {
    fn load(path: PathBuf) -> Self {
        let layout = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Layout>(&s).ok())
            .unwrap_or_else(|| Layout::new().0);
        App { layout: Mutex::new(layout), sessions: Mutex::default(), path }
    }

    /// Persist after every change; written to a temporary file first so a crash cannot truncate it.
    fn save(&self, layout: &Layout) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("json.tmp");
        if let Ok(json) = serde_json::to_string_pretty(layout) {
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }
}

// ---- what the webview sees -------------------------------------------------------------------

/// `[text, cols, fg, bg, flags]`. A wide character is followed by `\0` in `text` for its spacer cell.
#[derive(Clone, Serialize)]
struct Run(String, u16, u32, u32, u8);

#[derive(Clone, Serialize)]
struct Frame {
    cols: u16,
    rows: u16,
    cursor: Option<(u16, u16)>,
    fg: u32,
    bg: u32,
    /// `[offset, total, len]` of the viewport in rows.
    scrollbar: (u64, u64, u64),
    lines: Vec<Vec<Run>>,
}

#[derive(Clone, Serialize)]
struct PaneFrame {
    pane: PaneId,
    frame: Frame,
}

#[derive(Clone, Serialize)]
struct TabView {
    title: String,
    active: bool,
}

#[derive(Clone, Serialize)]
struct WorkspaceView {
    name: String,
    active: bool,
}

#[derive(Clone, Serialize)]
struct View {
    workspaces: Vec<WorkspaceView>,
    tabs: Vec<TabView>,
    focus: PaneId,
    panes: Vec<(PaneId, Rect)>,
    dividers: Vec<Divider>,
}

fn view(l: &Layout) -> View {
    let (panes, dividers) = l.tab().geometry();
    View {
        workspaces: l
            .workspaces
            .iter()
            .enumerate()
            .map(|(i, w)| WorkspaceView { name: w.name.clone(), active: i == l.active })
            .collect(),
        tabs: l
            .workspace()
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| TabView { title: t.title.clone(), active: i == l.workspace().active })
            .collect(),
        focus: l.tab().focus,
        panes,
        dividers,
    }
}

fn rgb(c: holzwave_core::Color) -> u32 {
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

/// Run-length encode the screen by style, so a frame is a few KiB instead of one object per cell.
fn encode(screen: &Screen) -> Frame {
    let cols = screen.cols.max(1) as usize;
    let default_bg = rgb(screen.default_bg);
    let mut lines = Vec::with_capacity(screen.rows as usize);
    for row in screen.cells.chunks(cols) {
        let mut runs: Vec<Run> = Vec::new();
        for cell in row {
            let flags = u8::from(cell.bold)
                | u8::from(cell.italic) << 1
                | u8::from(cell.underline) << 2
                | u8::from(cell.inverse) << 3;
            let (fg, bg) = (rgb(cell.fg), rgb(cell.bg));
            let text = match cell.width {
                Width::Spacer => "\0",
                _ if cell.text.is_empty() => " ",
                _ => cell.text.as_str(),
            };
            match runs.last_mut() {
                Some(r) if r.2 == fg && r.3 == bg && r.4 == flags => {
                    r.0.push_str(text);
                    r.1 += 1;
                }
                _ => runs.push(Run(text.to_owned(), 1, fg, bg, flags)),
            }
        }
        // Trailing blanks in the default style carry no information.
        while matches!(runs.last(), Some(r) if r.0.chars().all(|c| c == ' ') && r.3 == default_bg && r.4 == 0) {
            runs.pop();
        }
        lines.push(runs);
    }
    let sb = screen.scrollbar;
    Frame {
        cols: screen.cols,
        rows: screen.rows,
        cursor: screen.cursor.filter(|c| c.visible).map(|c| (c.x, c.y)),
        fg: rgb(screen.default_fg),
        bg: default_bg,
        scrollbar: (sb.offset, sb.total, sb.len),
        lines,
    }
}

// ---- one pane: shell, engine, frames ----------------------------------------------------------

fn run_pane(app: AppHandle, pane: PaneId, cols: u16, rows: u16, tx: mpsc::Sender<Msg>, rx: mpsc::Receiver<Msg>) {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let spawned = Pty::spawn(&shell, &["-l"], cols, rows)
        .and_then(|pty| GhosttyEngine::new(cols, rows, SCROLLBACK_LINES).map(|e| (pty, e)));
    let (mut pty, mut engine) = match spawned {
        Ok(v) => v,
        Err(e) => {
            eprintln!("holzwave: pane {pane}: {e}");
            return pane_gone(&app, pane);
        }
    };

    if let Some(mut reader) = pty.take_reader() {
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(Msg::Output(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
            let _ = tx.send(Msg::Closed);
        });
    }

    let mut dirty = false;
    let mut last_emit = Instant::now() - FRAME_INTERVAL;
    loop {
        let wait = if dirty { FRAME_INTERVAL.saturating_sub(last_emit.elapsed()) } else { Duration::from_secs(3600) };
        let first = match rx.recv_timeout(wait) {
            Ok(m) => Some(m),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let mut closed = false;
        for msg in first.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            match msg {
                Msg::Output(b) => {
                    engine.feed(&b);
                    dirty = true;
                }
                Msg::Input(b) => {
                    // Typing returns to the live screen, like every terminal.
                    engine.scroll_to_bottom();
                    let _ = pty.write(&b);
                    dirty = true;
                }
                Msg::Resize(c, r) => {
                    if engine.resize(c, r, (0, 0)).is_ok() {
                        let _ = pty.resize(c, r);
                        dirty = true;
                    }
                }
                Msg::Scroll(rows) => {
                    engine.scroll(rows);
                    dirty = true;
                }
                Msg::Closed => closed = true,
            }
        }
        if dirty && last_emit.elapsed() >= FRAME_INTERVAL {
            if let Ok(screen) = engine.screen() {
                let _ = app.emit("screen", PaneFrame { pane, frame: encode(&screen) });
            }
            dirty = false;
            last_emit = Instant::now();
        }
        if closed {
            break;
        }
    }
    let _ = pty.wait();
    pane_gone(&app, pane);
}

/// The shell of a pane ended: drop the session and the pane. The last pane takes the app with it.
fn pane_gone(app: &AppHandle, pane: PaneId) {
    let state = app.state::<App>();
    state.sessions.lock().unwrap().remove(&pane);
    let mut layout = state.layout.lock().unwrap();
    if layout.close_pane(pane) {
        state.save(&layout);
        let _ = app.emit("layout", view(&layout));
    } else {
        drop(layout);
        app.exit(0);
    }
}

// ---- commands -------------------------------------------------------------------------------

fn send(state: &App, pane: PaneId, msg: Msg) {
    if let Some(tx) = state.sessions.lock().unwrap().get(&pane) {
        let _ = tx.send(msg);
    }
}

/// Apply a layout change, persist it and return the new view.
fn change(state: &App, f: impl FnOnce(&mut Layout)) -> View {
    let mut layout = state.layout.lock().unwrap();
    f(&mut layout);
    state.save(&layout);
    view(&layout)
}

#[tauri::command]
fn get_layout(state: State<App>) -> View {
    view(&state.layout.lock().unwrap())
}

/// Start the shell of a pane if it has none yet. The webview calls this once it knows the pane's size.
#[tauri::command]
fn start_pane(app: AppHandle, state: State<App>, pane: PaneId, cols: u16, rows: u16) {
    let mut sessions = state.sessions.lock().unwrap();
    if sessions.contains_key(&pane) {
        return;
    }
    let (tx, rx) = mpsc::channel();
    sessions.insert(pane, tx.clone());
    drop(sessions);
    std::thread::spawn(move || run_pane(app, pane, cols.max(2), rows.max(1), tx, rx));
}

#[tauri::command]
fn input(state: State<App>, pane: PaneId, data: String) {
    send(&state, pane, Msg::Input(data.into_bytes()));
}

#[tauri::command]
fn resize(state: State<App>, pane: PaneId, cols: u16, rows: u16) {
    send(&state, pane, Msg::Resize(cols.max(2), rows.max(1)));
}

#[tauri::command]
fn scroll(state: State<App>, pane: PaneId, rows: i32) {
    send(&state, pane, Msg::Scroll(rows as isize));
}

#[tauri::command]
fn split(state: State<App>, column: bool) -> View {
    change(&state, |l| {
        l.split_focused(if column { Axis::Column } else { Axis::Row });
    })
}

#[tauri::command]
fn new_tab(state: State<App>) -> View {
    change(&state, |l| {
        l.new_tab();
    })
}

#[tauri::command]
fn new_workspace(state: State<App>, name: String) -> View {
    change(&state, |l| {
        l.new_workspace(&name);
    })
}

/// Close a pane: its engine thread ends when the sender is dropped, and the shell gets SIGHUP with the PTY.
#[tauri::command]
fn close_pane(app: AppHandle, state: State<App>, pane: PaneId) -> View {
    state.sessions.lock().unwrap().remove(&pane);
    let mut layout = state.layout.lock().unwrap();
    if !layout.close_pane(pane) {
        drop(layout);
        app.exit(0);
        return View { workspaces: vec![], tabs: vec![], focus: 0, panes: vec![], dividers: vec![] };
    }
    state.save(&layout);
    view(&layout)
}

#[tauri::command]
fn focus_pane(state: State<App>, pane: PaneId) -> View {
    change(&state, |l| l.focus_pane(pane))
}

#[tauri::command]
fn cycle_focus(state: State<App>, step: i32) -> View {
    change(&state, |l| l.cycle_focus(step as isize))
}

#[tauri::command]
fn select_tab(state: State<App>, index: usize) -> View {
    change(&state, |l| l.select_tab(index))
}

#[tauri::command]
fn select_workspace(state: State<App>, index: usize) -> View {
    change(&state, |l| l.select_workspace(index))
}

#[tauri::command]
fn set_ratio(state: State<App>, split: SplitId, ratio: f32) -> View {
    change(&state, |l| l.set_ratio(split, ratio))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .runtime(tauri_runtime_wry::Wry::default())
        .setup(|app| {
            let path = app.path().app_data_dir()?.join("layout.json");
            app.manage(App::load(path));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_layout,
            start_pane,
            input,
            resize,
            scroll,
            split,
            new_tab,
            new_workspace,
            close_pane,
            focus_pane,
            cycle_focus,
            select_tab,
            select_workspace,
            set_ratio,
        ])
        .run(tauri::generate_context!())
        .expect("error while running holzWave");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("holzwave-test-{}", std::process::id()));
        let path = dir.join("layout.json");

        let app = App::load(path.clone());
        let before = {
            let mut l = app.layout.lock().unwrap();
            l.split_focused(Axis::Row);
            l.new_workspace("two");
            app.save(&l);
            l.clone()
        };

        let again = App::load(path);
        assert_eq!(*again.layout.lock().unwrap(), before);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_corrupt_layout_file_falls_back_to_a_fresh_layout() {
        let dir = std::env::temp_dir().join(format!("holzwave-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("layout.json");
        std::fs::write(&path, "{ not json").unwrap();

        let app = App::load(path);
        assert_eq!(app.layout.lock().unwrap().all_panes().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
