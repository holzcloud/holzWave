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

use holzwave_core::layout::{Axis, CloseOutcome, Divider, Layout, LoadStatus, PaneId, Rect, SplitId};
use holzwave_core::persist;
use holzwave_core::pty::Pty;
use holzwave_core::{GhosttyEngine, Screen, TerminalEngine, Width};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

/// A layout file larger than this is not read: a real layout is a few KiB.
const MAX_LAYOUT_BYTES: u64 = 1 << 20;
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
    /// Read the layout file. A file that is missing gives a fresh layout. One that is damaged or inconsistent
    /// is repaired or replaced, and its original bytes are copied to `<path>.bad` before anything can
    /// overwrite them. One that is larger than `MAX_LAYOUT_BYTES` is not read; it is moved to `<path>.bad`.
    fn load(path: PathBuf) -> Self {
        let layout = match persist::read_limited(&path, MAX_LAYOUT_BYTES) {
            Ok(None) => Layout::new().0,
            Ok(Some(bytes)) => {
                let (layout, status) = match std::str::from_utf8(&bytes) {
                    Ok(text) => Layout::from_json(text),
                    Err(e) => (Layout::new().0, LoadStatus::Reset(format!("not UTF-8: {e}"))),
                };
                match &status {
                    LoadStatus::Clean => {}
                    LoadStatus::Repaired(repairs) => {
                        eprintln!("holzwave: {} repaired: {}", path.display(), repairs.join("; "));
                        keep_bad_copy(&path);
                    }
                    LoadStatus::Reset(reason) => {
                        eprintln!("holzwave: {} reset: {reason}", path.display());
                        keep_bad_copy(&path);
                    }
                }
                layout
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                eprintln!("holzwave: {e}; set aside as .bad");
                let bad = persist::bad_path(&path);
                if let Err(e) = std::fs::rename(&path, &bad) {
                    eprintln!("holzwave: cannot move {} to {}: {e}", path.display(), bad.display());
                }
                Layout::new().0
            }
            Err(e) => {
                eprintln!("holzwave: cannot read {}: {e}", path.display());
                Layout::new().0
            }
        };
        App { layout: Mutex::new(layout), sessions: Mutex::default(), path }
    }

    /// Persist after every change: serialise here, write atomically (temporary file, fsync, rename). A
    /// failure is logged, not dropped.
    fn save(&self, layout: &Layout) {
        match serde_json::to_vec_pretty(layout) {
            Ok(json) => {
                if let Err(e) = persist::write_atomic(&self.path, &json) {
                    eprintln!("holzwave: cannot save {}: {e}", self.path.display());
                }
            }
            Err(e) => eprintln!("holzwave: cannot serialise the layout: {e}"),
        }
    }
}

/// Copy a layout file that could not be used as it was to `<path>.bad`.
fn keep_bad_copy(path: &std::path::Path) {
    if let Err(e) = persist::backup_bad(path) {
        eprintln!("holzwave: cannot keep a copy of {}: {e}", path.display());
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
    if layout.close_pane(pane) == CloseOutcome::Closed {
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
    if layout.close_pane(pane) != CloseOutcome::Closed {
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
        assert_eq!(
            std::fs::read_to_string(dir.join("layout.json.bad")).unwrap(),
            "{ not json",
            "the damaged file is kept next to the layout"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_layout_file_over_one_mebibyte_is_not_read_and_keeps_a_bad_copy() {
        let dir = std::env::temp_dir().join(format!("holzwave-oversize-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("layout.json");
        // A perfectly valid layout, padded with blanks past the limit: the size alone is the reason.
        let mut text = r#"{"workspaces":[{"id":1,"name":"main","tabs":[{"id":2,"title":"shell","root":{"Pane":3},"focus":3}],"active":0}],"active":0,"next_id":4}"#.to_owned();
        text.push_str(&" ".repeat(MAX_LAYOUT_BYTES as usize));
        std::fs::write(&path, &text).unwrap();

        let app = App::load(path.clone());
        assert_eq!(*app.layout.lock().unwrap(), Layout::new().0, "an oversized file is not used");
        let bad = std::fs::metadata(dir.join("layout.json.bad")).expect("a bad copy next to the file");
        assert_eq!(bad.len(), text.len() as u64);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saving_leaves_no_temporary_file_and_creates_the_directory() {
        let dir = std::env::temp_dir().join(format!("holzwave-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("layout.json");
        let app = App::load(path.clone());
        app.save(&app.layout.lock().unwrap());
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["layout.json"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_layout_with_out_of_range_indices_loads_and_keeps_a_bad_copy() {
        let dir = std::env::temp_dir().join(format!("holzwave-badcopy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("layout.json");
        let damaged = r#"{"workspaces":[{"id":1,"name":"main","tabs":[{"id":2,"title":"shell","root":{"Pane":3},"focus":3}],"active":7}],"active":3,"next_id":4}"#;
        std::fs::write(&path, damaged).unwrap();

        let app = App::load(path.clone());
        {
            let l = app.layout.lock().unwrap();
            assert_eq!(l.active, 0);
            assert_eq!(l.workspace().active, 0);
            assert_eq!(l.all_panes(), vec![3], "the repaired layout keeps the file's pane");
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("layout.json.bad")).unwrap(),
            damaged,
            "the original bytes are kept before the first save can overwrite them"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), damaged, "loading alone does not rewrite the file");
        let _ = std::fs::remove_dir_all(dir);
    }
}
