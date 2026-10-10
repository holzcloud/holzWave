//! `session.json`: what is restored on the next start, per pane working directory and the window geometry.
//!
//! The file is machine-local and kept apart from `layout.json` on purpose: the layout is what sync will carry
//! between machines (Phase 4), a window position and a directory name are not. Everything in it is a hint. A
//! damaged or hostile file costs the hint and never the start: [`Session::load`] has no error case, a window
//! that would be off screen is not restored, and a stored directory is only ever used as the working directory
//! argument of a spawn (never as part of a command line), and only while it is still an absolute directory.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::layout::{LoadStatus, PaneId};
use crate::persist::{backup_bad, bad_path, read_limited, write_atomic};

/// The file name inside the app data directory.
pub const SESSION_FILE: &str = "session.json";
/// The format version this build writes and reads.
pub const SESSION_VERSION: u32 = 1;
/// The largest `session.json` that is read.
pub const MAX_SESSION_BYTES: u64 = 1024 * 1024;
/// The longest stored working directory, in bytes.
pub const MAX_CWD_BYTES: usize = 4096;
/// The most working directories kept: as many as the layout holds panes.
pub const MAX_CWDS: usize = 256;
/// The accepted window width and height, in pixels.
pub const WINDOW_SIZE: (u32, u32) = (200, 16384);
/// The accepted window position on either axis, in pixels.
pub const WINDOW_POSITION: (i32, i32) = (-32768, 32768);
/// The size used when nothing was saved; the same as `app/src-tauri/tauri.conf.json`.
pub const DEFAULT_SIZE: (u32, u32) = (1000, 640);
/// How much of a window (width, height) must be on one monitor for its position to be restored.
pub const MIN_VISIBLE: (u32, u32) = (100, 50);

/// The window geometry that was saved, in pixels. Position and size are in the same unit as [`MonitorRect`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

/// One monitor, in the same pixel space as [`WindowState`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Where and how large to open the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub size: (u32, u32),
    /// `None` leaves the position to the window system.
    pub position: Option<(i32, i32)>,
    pub maximized: bool,
}

/// The machine-local state restored on the next start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Session {
    pub version: u32,
    pub window: Option<WindowState>,
    pub cwds: BTreeMap<PaneId, String>,
}

impl Default for Session {
    fn default() -> Self {
        Session { version: SESSION_VERSION, window: None, cwds: BTreeMap::new() }
    }
}

impl WindowState {
    /// The same window with size and position inside [`WINDOW_SIZE`] and [`WINDOW_POSITION`].
    pub fn clamped(self) -> WindowState {
        WindowState {
            x: self.x.clamp(WINDOW_POSITION.0, WINDOW_POSITION.1),
            y: self.y.clamp(WINDOW_POSITION.0, WINDOW_POSITION.1),
            width: self.width.clamp(WINDOW_SIZE.0, WINDOW_SIZE.1),
            height: self.height.clamp(WINDOW_SIZE.0, WINDOW_SIZE.1),
            maximized: self.maximized,
        }
    }

    /// Whether at least [`MIN_VISIBLE`] of this window lies inside `monitor`.
    fn is_visible_on(&self, monitor: &MonitorRect) -> bool {
        // i64: a position near the clamp plus a size cannot overflow, and neither can a monitor of any i32/u32.
        let overlap = |start: i32, len: u32, m_start: i32, m_len: u32| {
            let low = i64::from(start).max(i64::from(m_start));
            let high = (i64::from(start) + i64::from(len)).min(i64::from(m_start) + i64::from(m_len));
            high - low
        };
        overlap(self.x, self.width, monitor.x, monitor.width) >= i64::from(MIN_VISIBLE.0)
            && overlap(self.y, self.height, monitor.y, monitor.height) >= i64::from(MIN_VISIBLE.1)
    }
}

/// Whether `text` may be stored and later handed to a spawn as the working directory: not empty, at most
/// [`MAX_CWD_BYTES`], no control character (a terminal escape or a newline), and an absolute path.
fn valid_cwd(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_CWD_BYTES
        && !text.chars().any(char::is_control)
        && Path::new(text).is_absolute()
}

/// A number from the file as a window value. Fractions are rounded; a value beyond the field type saturates at
/// its bound (a float to integer cast in Rust saturates), and [`Session::sanitise`] then clamps it into range and
/// says so.
fn window_number(value: &Value) -> Option<f64> {
    Some(value.as_f64()?.round())
}

/// The `window` entry of the file. `None` when it is not an object with the four numbers and, if present, a
/// boolean `maximized`.
fn read_window(value: &Value) -> Option<WindowState> {
    let object = value.as_object()?;
    let maximized = match object.get("maximized") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return None,
    };
    Some(WindowState {
        x: window_number(object.get("x")?)? as i32,
        y: window_number(object.get("y")?)? as i32,
        width: window_number(object.get("width")?)? as u32,
        height: window_number(object.get("height")?)? as u32,
        maximized,
    })
}

impl Session {
    /// Read a session document.
    ///
    /// Text that is not a JSON object is an empty session and `Reset`. Otherwise every entry is read on its own:
    /// one wrong entry is dropped and the rest is kept, numbers are clamped, and the result is `Repaired` with one
    /// note per kind of repair. Unknown keys are ignored. The rules of [`Session::sanitise`] that do not need to
    /// know the live panes are applied here.
    pub fn from_json(text: &str) -> (Session, LoadStatus) {
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(e) => return (Session::default(), LoadStatus::Reset(format!("not a session: {e}"))),
        };
        let Value::Object(object) = value else {
            return (Session::default(), LoadStatus::Reset("not a session: the top level is not an object".into()));
        };
        let mut session = Session::default();
        let mut notes = Vec::new();
        if let Some(version) = object.get("version") {
            session.version = version.as_u64().and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        }
        match object.get("window") {
            None | Some(Value::Null) => {}
            Some(value) => match read_window(value) {
                Some(window) => session.window = Some(window),
                None => notes.push("the window entry is not usable and was dropped".to_string()),
            },
        }
        match object.get("cwds") {
            None | Some(Value::Null) => {}
            Some(Value::Object(map)) => {
                let mut unusable = 0usize;
                for (key, value) in map {
                    match (key.parse::<PaneId>(), value) {
                        (Ok(pane), Value::String(cwd)) => {
                            session.cwds.insert(pane, cwd.clone());
                        }
                        _ => unusable += 1,
                    }
                }
                if unusable > 0 {
                    notes.push(format!(
                        "{unusable} working directory entries were dropped: the key is not a pane id or the value \
                         is not text"
                    ));
                }
            }
            Some(_) => notes.push("the working directory table is not an object and was dropped".to_string()),
        }
        notes.extend(session.repair());
        let status = if notes.is_empty() { LoadStatus::Clean } else { LoadStatus::Repaired(notes) };
        (session, status)
    }

    /// The document as pretty printed JSON with a trailing newline.
    pub fn to_json(&self) -> String {
        // A struct of numbers, strings and a map with integer keys cannot fail to serialise; the fallback only
        // keeps the function total.
        let mut text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        text.push('\n');
        text
    }

    /// Read `<dir>/session.json`.
    ///
    /// There is no error case. A missing file is the default and `Clean`. A file that cannot be used is the
    /// default and `Reset`, and its bytes are copied to `session.json.bad` first so that the next save does not
    /// destroy them; an oversized file is moved there without being read. A valid file is never written to.
    pub fn load(dir: &Path) -> (Session, LoadStatus) {
        let path = dir.join(SESSION_FILE);
        let bytes = match read_limited(&path, MAX_SESSION_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return (Session::default(), LoadStatus::Clean),
            Err(e) => {
                let mut reason = format!("cannot read {SESSION_FILE}: {e}");
                if e.kind() == io::ErrorKind::InvalidData {
                    match fs::rename(&path, bad_path(&path)) {
                        Ok(()) => reason.push_str("; the file was moved to session.json.bad"),
                        Err(e) => reason.push_str(&format!("; and it could not be kept: {e}")),
                    }
                }
                return (Session::default(), LoadStatus::Reset(reason));
            }
        };
        let (session, status) = match String::from_utf8(bytes) {
            Ok(text) => Session::from_json(&text),
            Err(_) => (Session::default(), LoadStatus::Reset(format!("{SESSION_FILE} is not UTF-8 text"))),
        };
        match status {
            LoadStatus::Reset(reason) => {
                let reason = match backup_bad(&path) {
                    Ok(_) => reason,
                    Err(e) => format!("{reason} (and the damaged file could not be kept: {e})"),
                };
                (session, LoadStatus::Reset(reason))
            }
            other => (session, other),
        }
    }

    /// Write `<dir>/session.json` atomically.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        write_atomic(&dir.join(SESSION_FILE), self.to_json().as_bytes())
    }

    /// Bring the session into shape for the panes that are alive now, and say what changed.
    ///
    /// Directories of panes that are not in `live_panes` are dropped (a closed pane must not leave its
    /// directory behind), then the rules of [`Session::from_json`] are applied again: the version, the window
    /// clamp, and the content and count rules for directories. Call it before a save.
    pub fn sanitise(&mut self, live_panes: &[PaneId]) -> Vec<String> {
        let mut notes = Vec::new();
        let before = self.cwds.len();
        self.cwds.retain(|pane, _| live_panes.contains(pane));
        let gone = before - self.cwds.len();
        if gone > 0 {
            notes.push(format!("{gone} working directories of panes that no longer exist were dropped"));
        }
        notes.extend(self.repair());
        notes
    }

    /// The rules that need no knowledge of the live panes.
    fn repair(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.version != SESSION_VERSION {
            notes.push(format!("version {} is not {SESSION_VERSION}; read as version {SESSION_VERSION}", self.version));
            self.version = SESSION_VERSION;
        }
        if let Some(window) = self.window {
            let clamped = window.clamped();
            if clamped != window {
                notes.push(format!(
                    "the window geometry was outside {}..={} for size and {}..={} for position and was clamped",
                    WINDOW_SIZE.0, WINDOW_SIZE.1, WINDOW_POSITION.0, WINDOW_POSITION.1
                ));
                self.window = Some(clamped);
            }
        }
        let before = self.cwds.len();
        self.cwds.retain(|_, cwd| valid_cwd(cwd));
        let invalid = before - self.cwds.len();
        if invalid > 0 {
            notes.push(format!(
                "{invalid} working directories were dropped: a path must be absolute, at most {MAX_CWD_BYTES} \
                 bytes and free of control characters"
            ));
        }
        if self.cwds.len() > MAX_CWDS {
            let surplus = self.cwds.len() - MAX_CWDS;
            while self.cwds.len() > MAX_CWDS {
                self.cwds.pop_last();
            }
            notes.push(format!("{surplus} working directories beyond the first {MAX_CWDS} were dropped"));
        }
        notes
    }

    /// Remember the working directory of `pane`. Returns false and changes nothing when `cwd` fails the rules of
    /// [`Session::sanitise`] or the table is full.
    pub fn set_cwd(&mut self, pane: PaneId, cwd: &str) -> bool {
        if !valid_cwd(cwd) || (self.cwds.len() >= MAX_CWDS && !self.cwds.contains_key(&pane)) {
            return false;
        }
        self.cwds.insert(pane, cwd.to_owned());
        true
    }

    /// Remember the window geometry, clamped into range.
    pub fn set_window(&mut self, window: WindowState) {
        self.window = Some(window.clamped());
    }

    /// The directory a new shell of `pane` should start in: the stored path, only when it is a valid path (the
    /// rules of [`Session::sanitise`], checked again because the table is public) and is an existing directory
    /// right now. `None` means "start in the default directory".
    pub fn cwd_for(&self, pane: PaneId) -> Option<PathBuf> {
        let text = self.cwds.get(&pane)?;
        if !valid_cwd(text) {
            return None;
        }
        let path = PathBuf::from(text);
        path.is_dir().then_some(path)
    }

    /// Where and how large to open the window, given the monitors that are connected now.
    ///
    /// The size is always kept (clamped). The position is kept only when at least [`MIN_VISIBLE`] of the window
    /// is inside one monitor; a window that was on a monitor that has been unplugged, or any window when no
    /// monitor is known, is left to the window system. With nothing saved the result is [`DEFAULT_SIZE`].
    pub fn placement(&self, monitors: &[MonitorRect]) -> Placement {
        let Some(window) = self.window.map(WindowState::clamped) else {
            return Placement { size: DEFAULT_SIZE, position: None, maximized: false };
        };
        let visible = monitors.iter().any(|m| window.is_visible_on(m));
        Placement {
            size: (window.width, window.height),
            position: visible.then_some((window.x, window.y)),
            maximized: window.maximized,
        }
    }
}
