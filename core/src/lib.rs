//! The shared core of holzWave.
//!
//! Everything platform-independent lives here: the terminal engine behind the
//! [`TerminalEngine`] trait, the PTY, and (later) connections, sync and storage.
//! The app shell stays thin and calls into this crate.
//!
//! Modules:
//!
//! - [`engine`]: the [`TerminalEngine`] trait and the screen model it produces.
//! - [`layout`]: the pure tab and pane layout model.
//! - [`pty`]: spawning a shell on a pseudo terminal.
//! - [`persist`]: atomic file writes, bounded reads and the debounced saver.
//! - [`settings`]: `settings.json`, typed, versioned and validated.
//! - [`theme`]: built-in and user themes, terminal palette and chrome tokens.
//! - [`keybindings`]: action ids, chord grammar, defaults and validation of overrides.
//! - [`session`]: `session.json`, per-pane working directory and window geometry.
//! - [`queue`]: the bounded per-pane queue between the PTY reader and the engine thread.
//! - [`wire`]: the frame protocol from engine update to frame message.
//! - [`pane`]: the pane actor with engine thread, PTY reader and writer, and frame pacing.

pub mod engine;
mod error;
pub mod keybindings;
pub mod layout;
pub mod pane;
pub mod persist;
pub mod pty;
pub mod queue;
pub mod session;
pub mod settings;
pub mod theme;
pub mod wire;

#[cfg(feature = "ghostty")]
mod ghostty;

pub use engine::{Cell, Color, Cursor, Screen, Scrollbar, TerminalEngine, Width};
pub use error::{Error, Result};

#[cfg(feature = "ghostty")]
pub use ghostty::GhosttyEngine;
