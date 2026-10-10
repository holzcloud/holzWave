//! The pane actor: one engine thread that owns the engine and the PTY writer, a PTY reader thread feeding it, and
//! the pacing of frames towards the shell.
//!
//! Skeleton: the types and signatures exist, the actor itself does not yet.

use std::path::PathBuf;
use std::time::Duration;

use crate::engine::{DomKey, EngineTheme, TerminalEngine};
use crate::layout::PaneId;
use crate::wire::FrameMsg;
use crate::{Error, Result};

/// The default number of scrollback lines of a pane.
pub const DEFAULT_SCROLLBACK: usize = 10_000;

/// The default time between two frames of a pane.
pub const DEFAULT_FRAME_INTERVAL: Duration = Duration::from_millis(8);

/// How a pane is started.
#[derive(Clone, Debug)]
pub struct PaneSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub cols: u16,
    pub rows: u16,
    pub cell_px: (u32, u32),
    pub scrollback: usize,
    pub option_as_alt: bool,
    pub theme: Option<EngineTheme>,
    pub frame_interval: Duration,
}

impl PaneSpec {
    pub fn new(program: impl Into<String>, cols: u16, rows: u16) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            cols,
            rows,
            cell_px: (0, 0),
            scrollback: DEFAULT_SCROLLBACK,
            option_as_alt: false,
            theme: None,
            frame_interval: DEFAULT_FRAME_INTERVAL,
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }
}

/// What a pane tells its sink.
#[derive(Clone, Debug, PartialEq)]
pub enum PaneEvent {
    Frame(FrameMsg),
    Title(String),
    Cwd(String),
    Exited { code: Option<u32> },
    Failed(String),
}

/// A running pane.
#[derive(Clone)]
pub struct PaneHandle {
    _private: (),
}

impl PaneHandle {
    pub fn spawn<E, F, S>(_pane: PaneId, _spec: PaneSpec, _make_engine: F, _sink: S) -> Result<PaneHandle>
    where
        E: TerminalEngine + 'static,
        F: FnOnce(&PaneSpec) -> Result<E> + Send + 'static,
        S: FnMut(PaneEvent) + Send + 'static,
    {
        Err(Error::Unsupported("pane actor".into()))
    }

    #[cfg(feature = "ghostty")]
    pub fn spawn_ghostty<S>(pane: PaneId, spec: PaneSpec, sink: S) -> Result<PaneHandle>
    where
        S: FnMut(PaneEvent) + Send + 'static,
    {
        Self::spawn(pane, spec, |spec| {
            crate::GhosttyEngine::new(spec.cols, spec.rows, spec.scrollback)
        }, sink)
    }

    pub fn pid(&self) -> Option<u32> {
        None
    }

    pub fn key(&self, _key: DomKey) -> Result<()> {
        Err(Error::Unsupported("pane actor".into()))
    }

    pub fn text(&self, _text: String) -> Result<()> {
        Err(Error::Unsupported("pane actor".into()))
    }

    pub fn close(&self) {}

    pub fn join(&self, _timeout: Duration) -> bool {
        true
    }
}
