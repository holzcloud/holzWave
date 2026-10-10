//! The shared core of holzWave.
//!
//! Everything platform-independent lives here: the terminal engine behind the
//! [`TerminalEngine`] trait, the PTY, and (later) connections, sync and storage.
//! The app shell stays thin and calls into this crate.

mod engine;
mod error;
pub mod layout;
pub mod pty;

#[cfg(feature = "ghostty")]
mod ghostty;

pub use engine::{Cell, Color, Cursor, Screen, Scrollbar, TerminalEngine, Width};
pub use error::{Error, Result};

#[cfg(feature = "ghostty")]
pub use ghostty::GhosttyEngine;
