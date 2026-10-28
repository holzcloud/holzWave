//! The desktop's expansion of the command table (`holzwave_host::commands::table`).
//!
//! The host crate owns the one list of commands. This file feeds it to a macro that writes, for every row whose shell
//! part is `generated`, the command wrapper of the window shell and a call of the row's plain function, and a handler
//! that answers those wrappers. A row whose shell part is `hand` has its wrapper in `commands.rs`, because it needs the
//! clipboard or the window; this file only makes the build fail when that wrapper is missing.
//!
//! The wrappers are what the old hand-written ones were: the argument names stay snake_case (the page and the protocol
//! golden depend on them), a window-scoped row takes the calling window and passes its label, a row that waits is
//! `async`, and the core comes from the shell's managed state. The page therefore sees the same command, with the same
//! answer and the same error.
//!
//! The wrappers are private to this module: nothing but the handler below names them. The shell's command attribute
//! exports two helper macros for a `pub` function, and a macro that is written by a macro and exported would be reached
//! by an absolute path (a lint that is an error today); a private function exports nothing, so the question does not come
//! up. (A `pub` wrapper compiles today as well.)

use std::sync::Arc;

use holzwave_host::commands::prelude::*;
use holzwave_host::state::AppCore;
use tauri::ipc::Invoke;
use tauri::{DynRuntime, State, WebviewWindow};

/// Gives a function the shell's command attribute: `async` for a row that waits, off the main thread, and the snake_case
/// rename for every row.
macro_rules! attributed {
    (sync; $($item:tt)*) => {
        #[tauri::command(rename_all = "snake_case")]
        $($item)*
    };
    (async; $($item:tt)*) => {
        #[tauri::command(async, rename_all = "snake_case")]
        $($item)*
    };
}

/// The wrapper of one row, or the check that the hand-written one exists.
macro_rules! desktop_wrapper {
    // The wrapper stays in `commands.rs`; the build fails here when it is gone.
    (name: $name:ident, shell: hand, $($rest:tt)*) => {
        const _: () = {
            #[allow(unused_imports)]
            use crate::commands::$name;
        };
    };
    // A raw (binary) answer: the plain function returns the bytes and the wrapper wraps them as the shell's raw response,
    // which the page reads as an `ArrayBuffer`.
    (
        name: $name:ident, shell: generated, group: $group:ident, scope: none, mode: $mode:ident, cap: $cap:expr,
        args: [$($arg:ident : $aty:ty),* $(,)?],
        answers: raw $ok:ty, fails: $err:ty,
        run: $run:path, web: $($web:tt)*
    ) => {
        attributed! { $mode;
            #[allow(clippy::too_many_arguments)]
            fn $name(core: State<Arc<AppCore>> $(, $arg: $aty)*) -> Result<$ok, $err> {
                $run(&core $(, $arg)*).map(<$ok>::new)
            }
        }
    };
    (
        name: $name:ident, shell: generated, group: $group:ident, scope: window, mode: $mode:ident, cap: $cap:expr,
        args: [$($arg:ident : $aty:ty),* $(,)?],
        answers: $ok:ty, fails: $err:ty,
        run: $run:path, web: $($web:tt)*
    ) => {
        attributed! { $mode;
            #[allow(clippy::too_many_arguments)]
            fn $name(core: State<Arc<AppCore>>, window: WebviewWindow $(, $arg: $aty)*) -> Result<$ok, $err> {
                $run(&core, window.label() $(, $arg)*)
            }
        }
    };
    (
        name: $name:ident, shell: generated, group: $group:ident, scope: window, mode: $mode:ident, cap: $cap:expr,
        args: [$($arg:ident : $aty:ty),* $(,)?],
        answers: $ok:ty,
        run: $run:path, web: $($web:tt)*
    ) => {
        attributed! { $mode;
            #[allow(clippy::too_many_arguments)]
            fn $name(core: State<Arc<AppCore>>, window: WebviewWindow $(, $arg: $aty)*) -> $ok {
                $run(&core, window.label() $(, $arg)*)
            }
        }
    };
    (
        name: $name:ident, shell: generated, group: $group:ident, scope: none, mode: $mode:ident, cap: $cap:expr,
        args: [$($arg:ident : $aty:ty),* $(,)?],
        answers: $ok:ty, fails: $err:ty,
        run: $run:path, web: $($web:tt)*
    ) => {
        attributed! { $mode;
            #[allow(clippy::too_many_arguments)]
            fn $name(core: State<Arc<AppCore>> $(, $arg: $aty)*) -> Result<$ok, $err> {
                $run(&core $(, $arg)*)
            }
        }
    };
    (
        name: $name:ident, shell: generated, group: $group:ident, scope: none, mode: $mode:ident, cap: $cap:expr,
        args: [$($arg:ident : $aty:ty),* $(,)?],
        answers: $ok:ty,
        run: $run:path, web: $($web:tt)*
    ) => {
        attributed! { $mode;
            #[allow(clippy::too_many_arguments)]
            fn $name(core: State<Arc<AppCore>> $(, $arg: $aty)*) -> $ok {
                $run(&core $(, $arg)*)
            }
        }
    };
}

/// The handler of every row: the names of the rows, each with the path of its wrapper (a generated one is in this
/// module, a hand-written one in `commands.rs`), collected and then written once as the names `HANDLED` and the
/// shell's invoke handler. Nothing else lists a command.
macro_rules! table_handler {
    ([$(($name:ident, $path:path))*];) => {
        /// The names of the commands the handler answers, in the order of the table.
        #[cfg(test)]
        pub(crate) const HANDLED: &[&str] = &[$(stringify!($name)),*];

        /// The invoke handler of the app: the generated wrappers and the hand-written ones, from the table alone. It
        /// answers `false` for a name that is not a row.
        pub fn handler() -> impl Fn(Invoke<DynRuntime>) -> bool + Send + Sync + 'static {
            tauri::generate_handler![$($path),*]
        }
    };
    ([$($done:tt)*]; { name: $name:ident, shell: generated, $($more:tt)* } $($rest:tt)*) => {
        table_handler! { [$($done)* ($name, $name)]; $($rest)* }
    };
    ([$($done:tt)*]; { name: $name:ident, shell: hand, $($more:tt)* } $($rest:tt)*) => {
        table_handler! { [$($done)* ($name, crate::commands::$name)]; $($rest)* }
    };
}

/// The callback of the table: the wrappers and the handler.
macro_rules! desktop_commands {
    ($({ $($row:tt)* })*) => {
        $(desktop_wrapper! { $($row)* })*

        table_handler! { []; $({ $($row)* })* }
    };
}

holzwave_host::host_commands!(desktop_commands);

#[cfg(test)]
mod tests {
    use holzwave_host::commands::registry::COMMANDS;

    use super::HANDLED;

    #[test]
    fn the_handler_answers_every_row_of_the_table_once_and_nothing_else() {
        let rows: Vec<&str> = COMMANDS.iter().map(|row| row.name).collect();
        assert_eq!(HANDLED, rows.as_slice(), "the handler is the table, in its order");
        let mut sorted = rows.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), rows.len(), "a name twice");
    }
}
