//! The one list of commands.
//!
//! [`host_commands!`] is a callback macro: `host_commands!(name)` calls the macro `name!` with every row below, and each
//! expansion writes what one side needs. The host's [`registry`](super::registry) writes a function that reads the
//! arguments strictly and runs the plain function, and a constant slice of plain facts. The desktop app writes the
//! window system's command wrappers and the handler for them. A row therefore cannot exist for one side only, and a
//! plain function whose signature does not match its row does not compile.
//!
//! # One row
//!
//! ```text
//! { name: paste, shell: generated, group: terminal, scope: none, mode: async, cap: CAP_PASTE,
//!   args: [pane: PaneId, text: String, allow_unsafe: bool],
//!   answers: PasteOutcomeMsg, fails: String,
//!   run: terminal::paste, web: same }
//! ```
//!
//! - `name`: the command, as the page spells it (snake_case).
//! - `shell`: `generated` when the app writes the desktop wrapper from the row, `hand` when the wrapper stays in the
//!   app's `commands.rs` because it needs the window shell (the clipboard, showing a window). The app's expansion fails
//!   the build when a `hand` row has no wrapper there.
//! - `group`: the plain-function module the row belongs to (`terminal`, `layout`).
//! - `scope`: `none`, or `window` when the command works on the layout of the calling window and so takes its label.
//! - `mode`: `sync`, or `async` when the command waits for the engine, a process or the network.
//! - `cap`: the most bytes of argument text the command takes (`CAP_INPUT`, `CAP_PASTE` or `CAP_DEFAULT`).
//! - `args`: the arguments as `name: Type`, in the order of the plain function. An `Option` argument may be left out.
//! - `answers`: the type the command answers; `fails`: the error type of a command that can fail (left out for one that
//!   cannot). The plain function returns exactly `Result<answers, fails>`, or `answers` without `fails`.
//! - `run`: the plain function `fn(core: &AppCore, [window: &str,] <args>) -> ...`. The desktop wrapper calls it; for a
//!   `hand` row it is the shared part of the wrapper, or `none` when the wrapper has no shared part.
//! - `web`: what the browser build does. `same` runs `run`; `unavailable` answers `Not available in the browser.` and
//!   never reads the arguments or runs anything; `variant(path)` runs `path`, a function with the signature of `run`.
//!
//! Every type, module and cap a row names is in [`prelude`](super::prelude); a new type goes there, so both sides import
//! the same list.

/// Calls `$callback!` with every row of the table, each in braces and without a separator. The rows are in the order of
/// [`COMMANDS`](super::registry::COMMANDS).
#[macro_export]
macro_rules! host_commands {
    ($callback:ident) => {
        $callback! {
            {
                name: get_layout, shell: generated, group: layout, scope: window, mode: sync, cap: CAP_DEFAULT,
                args: [],
                answers: View, fails: String,
                run: layout::get_layout, web: same
            }
            {
                name: paste, shell: generated, group: terminal, scope: none, mode: async, cap: CAP_PASTE,
                args: [pane: PaneId, text: String, allow_unsafe: bool],
                answers: PasteOutcomeMsg, fails: String,
                run: terminal::paste, web: same
            }
            {
                name: selection_text, shell: generated, group: terminal, scope: none, mode: async, cap: CAP_DEFAULT,
                args: [pane: PaneId],
                answers: Option<String>, fails: String,
                run: terminal::selection_text, web: same
            }
        }
    };
}
