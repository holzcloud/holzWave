//! The shared core of holzWave.
//!
//! Everything platform-independent lives here: the terminal engine behind the
//! [`TerminalEngine`] trait, the PTY, and (later) connections, sync and storage.
//! The app shell stays thin and calls into this crate.
//!
//! Modules:
//!
//! - [`ai`]: the AI chat core; [`ai::http`] builds the one HTTP client that reaches a provider.
//! - [`engine`]: the [`TerminalEngine`] trait and the screen model it produces.
//! - [`layout`]: the pure tab and pane layout model.
//! - [`platform`]: the per-OS decisions (default shell, locale, polling) as pure functions of an `Os`.
//! - [`pty`]: spawning a shell on a pseudo terminal.
//! - [`blocks`]: panes that show a file browser or a preview instead of a terminal.
//! - [`conn`]: the [`conn::Link`] trait a pane talks to, and the state of a connection.
//! - [`profiles`]: `connections.json`, the saved SSH, WSL and Docker connections.
//! - [`secrets`]: the [`secrets::SecretStore`] trait over the OS keychain.
//! - [`connections`]: the connection manager the app shell holds: profiles, secrets, which pane runs which.
//! - [`files`]: the file browser's source of files, local and over SFTP, with bounded reads and atomic writes.
//! - [`preview`]: the kind of a file and Markdown as sanitised HTML.
//! - [`ssh`]: a shell on another machine over SSH, with reconnect.
//! - [`persist`]: atomic file writes, bounded reads and the debounced saver.
//! - [`settings`]: `settings.json`, typed, versioned and validated.
//! - [`theme`]: built-in and user themes, terminal palette and chrome tokens.
//! - [`keybindings`]: action ids, chord grammar, defaults and validation of overrides.
//! - [`session`]: `session.json`, per-pane working directory and window geometry.
//! - [`queue`]: the bounded per-pane queue between the PTY reader and the engine thread.
//! - [`wire`]: the frame protocol from engine update to frame message.
//! - [`pane`]: the pane actor with engine thread, PTY reader and writer, and frame pacing.
//! - [`winshell`]: the PowerShell prompt wrapper that announces the directory, and the Windows reading of its OSC 7.

pub mod ai;
pub mod blocks;
pub mod conn;
pub mod connections;
pub mod engine;
mod error;
pub mod files;
pub mod history;
pub mod keybindings;
pub mod monitor;
pub mod layout;
pub mod pane;
pub mod persist;
pub mod platform;
pub mod preview;
pub mod profiles;
pub mod pty;
pub mod queue;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod ssh;
pub mod sync;
pub mod theme;
pub mod widgets;
pub mod wire;
pub mod winshell;

#[cfg(feature = "ghostty")]
mod ghostty;

pub use engine::{Cell, Color, Cursor, Screen, Scrollbar, TerminalEngine, Width};
pub use error::{Error, Result};

#[cfg(feature = "ghostty")]
pub use ghostty::GhosttyEngine;
