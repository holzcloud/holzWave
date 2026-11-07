//! The shared core of holzWave.
//!
//! Everything platform-independent lives here: the terminal engine behind the
//! [`TerminalEngine`] trait, the PTY, and (later) connections, sync and storage.
//! The app shell stays thin and calls into this crate.
//!
//! Modules:
//!
//! - [`attention`]: the flood gate of the events that mark a tab or a workspace (the bell).
//! - [`ai`]: the AI chat core; [`ai::http`] builds the one HTTP client that reaches a provider.
//! - [`engine`]: the [`TerminalEngine`] trait and the screen model it produces.
//! - `images`: the limits of the Kitty graphics protocol and the bounded PNG decoder (with the `ghostty` feature).
//! - [`layout`]: the pure tab and pane layout model.
//! - [`links`]: hyperlinks (OSC 8) as bounded, cleaned spans, and the one function that decides what an address may be.
//! - [`platform`]: the per-OS decisions (default shell, locale, polling) as pure functions of an `Os`.
//! - [`pty`]: spawning a shell on a pseudo terminal.
//! - [`blocks`]: panes that show a file browser or a preview instead of a terminal.
//! - [`conn`]: the [`conn::Link`] trait a pane talks to, and the state of a connection.
//! - [`profiles`]: `connections.json`, the saved SSH, WSL and Docker connections.
//! - [`secrets`]: the [`secrets::SecretStore`] trait over the OS keychain.
//! - [`vault`]: the secret store of Android: one encrypted file, its data key wrapped by a device key behind the [`vault::Keystore`] trait.
//! - [`connections`]: the connection manager the app shell holds: profiles, secrets, which pane runs which.
//! - [`integrate`]: the person's own command-line tools (Kubernetes, Talos, Azure, GAM), with names validated by kind.
//! - [`files`]: the file browser's source of files, local and over SFTP, with bounded reads and atomic writes.
//! - [`preview`]: the kind of a file and Markdown as sanitised HTML.
//! - [`ssh`]: a shell on another machine over SSH, with reconnect.
//! - [`pool`]: one SSH connection per saved connection, shared by the file browser and the command runner.
//! - [`persist`]: atomic file writes, bounded reads and the debounced saver.
//! - [`settings`]: `settings.json`, typed, versioned and validated.
//! - [`theme`]: built-in and user themes, terminal palette and chrome tokens.
//! - [`keybindings`]: action ids, chord grammar, defaults and validation of overrides.
//! - [`session`]: `session.json`, per-pane working directory and window geometry.
//! - [`queue`]: the bounded per-pane queue between the PTY reader and the engine thread.
//! - [`wire`]: the frame protocol from engine update to frame message.
//! - [`pane`]: the pane actor with engine thread, PTY reader and writer, and frame pacing.
//! - [`textread`]: reading a region of a terminal's text, with the byte ranges of the text the user could not see.
//! - [`exec`]: the local runner of the agent's command tool: a separate process, a scrubbed environment, bounded output.
//! - [`winshell`]: the PowerShell prompt wrapper that announces the directory, and the Windows reading of its OSC 7.
//! - [`wshinstall`]: the one explicit act that puts the bundled `wsh` on a Linux host reached over SSH, and verifies it.
//! - `platform_tls` (Android only): the one-time set-up of the certificate verifier both network clients use.

pub mod ai;
pub mod attention;
pub mod blocks;
pub mod conn;
pub mod connections;
pub mod engine;
mod error;
pub mod exec;
pub mod files;
pub mod history;
pub mod host_shell;
pub mod integrate;
pub mod keybindings;
pub mod monitor;
pub mod layout;
pub mod links;
pub mod local_network;
pub mod pane;
pub mod persist;
pub mod platform;
#[cfg(target_os = "android")]
pub mod platform_tls;
pub mod pool;
pub mod preview;
pub mod profiles;
pub mod pty;
pub mod queue;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod ssh;
pub mod sync;
pub mod textread;
pub mod theme;
pub mod vault;
pub mod widgets;
pub mod wire;
pub mod winshell;
pub mod wshinstall;

#[cfg(feature = "ghostty")]
mod ghostty;
#[cfg(feature = "ghostty")]
pub mod images;

pub use engine::{Cell, Color, Cursor, Screen, Scrollbar, TerminalEngine, Width};
pub use error::{Error, Result};
pub use links::LinkSpan;

#[cfg(feature = "ghostty")]
pub use ghostty::{GhosttyEngine, ImageLimits};
