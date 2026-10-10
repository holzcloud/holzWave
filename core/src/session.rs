//! `session.json`: what is restored on the next start, per pane working directory and the window geometry.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::layout::{LoadStatus, PaneId};

/// The file name inside the app data directory.
pub const SESSION_FILE: &str = "session.json";
/// The format version this build writes and reads.
pub const SESSION_VERSION: u32 = 1;
/// The largest `session.json` that is read.
pub const MAX_SESSION_BYTES: u64 = 1024 * 1024;
/// The longest stored working directory, in bytes.
pub const MAX_CWD_BYTES: usize = 4096;
/// The most working directories kept.
pub const MAX_CWDS: usize = 256;
/// The accepted window width and height, in pixels.
pub const WINDOW_SIZE: (u32, u32) = (200, 16384);
/// The accepted window position on either axis, in pixels.
pub const WINDOW_POSITION: (i32, i32) = (-32768, 32768);
/// The size used when nothing was saved; the same as `app/src-tauri/tauri.conf.json`.
pub const DEFAULT_SIZE: (u32, u32) = (1000, 640);
/// How much of a window must be on one monitor for its position to be restored.
pub const MIN_VISIBLE: (u32, u32) = (100, 50);

/// The window geometry that was saved.
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

impl Session {
    pub fn from_json(_text: &str) -> (Session, LoadStatus) {
        (Session::default(), LoadStatus::Clean)
    }

    pub fn to_json(&self) -> String {
        String::new()
    }

    pub fn load(_dir: &Path) -> (Session, LoadStatus) {
        (Session::default(), LoadStatus::Clean)
    }

    pub fn save(&self, _dir: &Path) -> io::Result<()> {
        Ok(())
    }

    pub fn sanitise(&mut self, _live_panes: &[PaneId]) -> Vec<String> {
        Vec::new()
    }

    pub fn set_cwd(&mut self, _pane: PaneId, _cwd: &str) -> bool {
        false
    }

    pub fn set_window(&mut self, _window: WindowState) {}

    pub fn cwd_for(&self, _pane: PaneId) -> Option<PathBuf> {
        None
    }

    pub fn placement(&self, _monitors: &[MonitorRect]) -> Placement {
        Placement { size: (0, 0), position: None, maximized: false }
    }
}
