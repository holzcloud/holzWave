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
use holzwave_host::commands::registry::{Shell, lookup};
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

/// The handler of the generated wrappers: the names of the rows whose shell part is `generated`, in a list that the
/// shell's handler macro takes.
macro_rules! generated_handler {
    ([$($done:ident),*];) => {
        tauri::generate_handler![$($done),*]
    };
    ([$($done:ident),*]; { name: $name:ident, shell: generated, $($more:tt)* } $($rest:tt)*) => {
        generated_handler! { [$($done,)* $name]; $($rest)* }
    };
    ([$($done:ident),*]; { name: $name:ident, shell: hand, $($more:tt)* } $($rest:tt)*) => {
        generated_handler! { [$($done),*]; $($rest)* }
    };
}

/// The callback of the table: the wrappers and the handler.
macro_rules! desktop_commands {
    ($({ $($row:tt)* })*) => {
        $(desktop_wrapper! { $($row)* })*

        /// The invoke handler of the generated wrappers. It answers `false` for a name that is not one of them.
        fn handler() -> impl Fn(Invoke<DynRuntime>) -> bool + Send + Sync + 'static {
            generated_handler! { []; $({ $($row)* })* }
        }
    };
}

holzwave_host::host_commands!(desktop_commands);

/// Whether the wrapper of `name` is generated from a row of the table.
fn generated(name: &str) -> bool {
    lookup(name).is_some_and(|command| command.shell == Shell::Generated)
}

/// One invoke handler for the generated wrappers and the hand-written ones: a call whose name is a generated row goes to
/// the generated handler and every other call to `rest`. The join stays until the hand-written list is empty.
pub fn join(
    rest: impl Fn(Invoke<DynRuntime>) -> bool + Send + Sync + 'static,
) -> impl Fn(Invoke<DynRuntime>) -> bool + Send + Sync + 'static {
    let table = handler();
    move |invoke| if generated(invoke.message.command()) { table(invoke) } else { rest(invoke) }
}
