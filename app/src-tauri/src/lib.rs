//! The holzWave app shell. A thin layer: the terminal lives in `holzwave-core`.
//!
//! Threading: libghostty-vt objects are `!Send`, so one engine thread owns the engine and the
//! PTY writer, and everything reaches it through one channel: PTY output, keyboard input, resize.
//! The engine thread coalesces output and emits at most one frame per `FRAME_INTERVAL`.

use std::io::Read;
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use holzwave_core::pty::Pty;
use holzwave_core::{GhosttyEngine, Screen, TerminalEngine, Width};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

const FRAME_INTERVAL: Duration = Duration::from_millis(8);
const SCROLLBACK_LINES: usize = 10_000;

enum Msg {
    Output(Vec<u8>),
    Closed,
    Input(Vec<u8>),
    Resize(u16, u16),
}

#[derive(Default)]
struct Session(Mutex<Option<mpsc::Sender<Msg>>>);

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
    lines: Vec<Vec<Run>>,
}

fn rgb(c: holzwave_core::Color) -> u32 {
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

/// Run-length encode the screen by style, so a frame is a few KiB instead of one object per cell.
fn encode(screen: &Screen) -> Frame {
    let cols = screen.cols.max(1) as usize;
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
        while matches!(runs.last(), Some(r) if r.0.chars().all(|c| c == ' ') && r.3 == rgb(screen.default_bg) && r.4 == 0) {
            runs.pop();
        }
        lines.push(runs);
    }
    Frame {
        cols: screen.cols,
        rows: screen.rows,
        cursor: screen.cursor.filter(|c| c.visible).map(|c| (c.x, c.y)),
        fg: rgb(screen.default_fg),
        bg: rgb(screen.default_bg),
        lines,
    }
}

fn run_session(app: AppHandle, cols: u16, rows: u16, tx: mpsc::Sender<Msg>, rx: mpsc::Receiver<Msg>) {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut pty = match Pty::spawn(&shell, &["-l"], cols, rows) {
        Ok(p) => p,
        Err(e) => return eprintln!("holzwave: cannot start {shell}: {e}"),
    };
    let mut engine = match GhosttyEngine::new(cols, rows, SCROLLBACK_LINES) {
        Ok(e) => e,
        Err(e) => return eprintln!("holzwave: engine: {e}"),
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
        // Wait for input; when a frame is due, wake up for it.
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
                    let _ = pty.write(&b);
                }
                Msg::Resize(c, r) => {
                    if engine.resize(c, r, (0, 0)).is_ok() {
                        let _ = pty.resize(c, r);
                        dirty = true;
                    }
                }
                Msg::Closed => closed = true,
            }
        }
        if dirty && last_emit.elapsed() >= FRAME_INTERVAL {
            if let Ok(screen) = engine.screen() {
                let _ = app.emit("screen", encode(&screen));
            }
            dirty = false;
            last_emit = Instant::now();
        }
        if closed {
            break;
        }
    }
    let _ = pty.wait();
    app.exit(0);
}

#[tauri::command]
fn start(app: AppHandle, session: State<Session>, cols: u16, rows: u16) {
    let mut slot = session.0.lock().unwrap();
    if slot.is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel();
    *slot = Some(tx.clone());
    std::thread::spawn(move || run_session(app, cols.max(2), rows.max(1), tx, rx));
}

#[tauri::command]
fn input(session: State<Session>, data: String) {
    if let Some(tx) = session.0.lock().unwrap().as_ref() {
        let _ = tx.send(Msg::Input(data.into_bytes()));
    }
}

#[tauri::command]
fn resize(session: State<Session>, cols: u16, rows: u16) {
    if let Some(tx) = session.0.lock().unwrap().as_ref() {
        let _ = tx.send(Msg::Resize(cols.max(2), rows.max(1)));
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .runtime(tauri_runtime_wry::Wry::default())
        .manage(Session::default())
        .invoke_handler(tauri::generate_handler![start, input, resize])
        .run(tauri::generate_context!())
        .expect("error while running holzWave");
}
