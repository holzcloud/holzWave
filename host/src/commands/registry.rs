//! The registry the server calls: the rows of the [table](super::table) as functions and as plain facts.
//!
//! [`lookup`] finds a row by the name the page sent. [`Command::call`] reads the argument text strictly (a field the
//! row does not name is refused, a field it needs but does not get is refused, the types are the types of the plain
//! function), runs the plain function and answers an [`Outcome`] that already holds the final JSON text of the answer or
//! of the error value, so no caller needs the types of the commands. [`COMMANDS`] is the same list as data: the group,
//! the scope, the mode, the cap, the arguments with the text of their types, the answer, the error and what the browser
//! build does, for the server's limits and for the tests.
//!
//! Nothing here runs a command whose row says `web: unavailable`: its function answers [`NOT_IN_BROWSER`] without
//! reading the arguments and without calling the plain function, so a browser can never make the server start a process,
//! write the server's clipboard or show a window.

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::prelude::*;
use crate::state::AppCore;

/// The most bytes of argument text of `key`, `mouse`, `wheel` and `resize`: a hot path that carries a few numbers.
pub const CAP_INPUT: usize = 4 * 1024;

/// The most bytes of argument text of `paste`, the largest message a page may send.
pub const CAP_PASTE: usize = 8 * 1024 * 1024;

/// The most bytes of argument text of a command that carries a document: `file_save`, `widget_note_save` and
/// `markdown_render`. The core takes a Markdown source of 2 MiB and a text of 8 MiB to save, and escaping in JSON can make
/// either several times longer, so a smaller cap would refuse a document the core accepts, in the browser only.
pub const CAP_LARGE: usize = 8 * 1024 * 1024;

/// The most bytes of argument text of every other command of this table.
pub const CAP_DEFAULT: usize = 64 * 1024;

/// The sentence a command answers in the browser build when the browser build does not have it. It is an error value (a
/// string), as the desktop gives for a refusal.
pub const NOT_IN_BROWSER: &str = "Not available in the browser.";

/// The most characters of an error text made from a refused argument text.
pub const ERROR_TEXT_LIMIT: usize = 200;

/// Whether a command works on the layout of the calling window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// It names a pane by its global id, or nothing.
    None,
    /// It takes the label of the calling window.
    Window,
}

/// Whether the desktop runs a command off the main thread because it waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Sync,
    Async,
}

/// What the browser build does with a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Web {
    /// The same plain function as the desktop.
    Same,
    /// The command answers [`NOT_IN_BROWSER`] and nothing runs.
    Unavailable,
    /// Another plain function with the signature of the desktop's.
    Variant,
}

/// Who writes the desktop wrapper of a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    /// The app writes it from the row.
    Generated,
    /// It stays in the app's `commands.rs` because it needs the window shell.
    Hand,
}

/// One argument of a command: its name and the text of its type as the row spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArgInfo {
    pub name: &'static str,
    pub ty: &'static str,
}

/// What a call came to. The text is final: nobody downstream parses or rewrites it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The JSON text of the answer.
    Ok(String),
    /// A raw answer, sent as bytes and not as text.
    Binary(Vec<u8>),
    /// The JSON text of the error value: a string for most commands.
    Err(String),
}

/// One row of the table as data, and the function that runs it.
#[derive(Clone, Copy)]
pub struct Command {
    /// The name the page sends.
    pub name: &'static str,
    /// The module of the plain function (`terminal`, `layout`).
    pub group: &'static str,
    pub scope: Scope,
    pub mode: Mode,
    /// The most bytes of argument text the command takes; [`Command::admits`] is the one check.
    pub cap: usize,
    pub args: &'static [ArgInfo],
    /// The text of the answer type.
    pub answer: &'static str,
    /// The text of the error type of a command that can fail.
    pub error: Option<&'static str>,
    pub web: Web,
    pub shell: Shell,
    /// Whether the answer is raw bytes ([`Outcome::Binary`]) and not JSON text. Exactly one command is: `file_range`.
    pub raw: bool,
    call: fn(&AppCore, &str, &str) -> Outcome,
}

impl Command {
    /// Whether `len` bytes of argument text are within the cap of this command.
    pub fn admits(&self, len: usize) -> bool {
        len <= self.cap
    }

    /// Run the command for the calling window `window` with the argument object `args` (JSON text). An argument text
    /// that is not exactly the object the row names is refused with a short error that says which field is wrong and
    /// never repeats a value; the plain function is not called then. The caller checks [`Command::admits`] first.
    pub fn call(&self, core: &AppCore, window: &str, args: &str) -> Outcome {
        (self.call)(core, window, args)
    }
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command").field("name", &self.name).field("web", &self.web).finish_non_exhaustive()
    }
}

/// The row named `name`, or `None` for a name the table does not have.
pub fn lookup(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|command| command.name == name)
}

// ---- answering ------------------------------------------------------------------------------------------------------

/// `text` cut to [`ERROR_TEXT_LIMIT`] characters.
fn bounded(text: &str) -> String {
    text.chars().take(ERROR_TEXT_LIMIT).collect()
}

/// The error outcome of a sentence of this module.
fn refusal(text: &str) -> Outcome {
    match serde_json::to_string(&bounded(text)) {
        Ok(json) => Outcome::Err(json),
        Err(_) => Outcome::Err(String::from("\"the call was refused\"")),
    }
}

/// The error outcome of a command the browser build does not have: [`NOT_IN_BROWSER`], whatever was sent.
pub fn unavailable() -> Outcome {
    refusal(NOT_IN_BROWSER)
}

/// The outcome of an answer: its JSON text.
fn ok_json<T: Serialize>(value: &T) -> Outcome {
    match serde_json::to_string(value) {
        Ok(json) => Outcome::Ok(json),
        Err(_) => refusal("the answer could not be written"),
    }
}

/// The outcome of an error value: its JSON text, as the desktop hands the page the same value.
fn err_json<E: Serialize>(value: &E) -> Outcome {
    match serde_json::to_string(value) {
        Ok(json) => Outcome::Err(json),
        Err(_) => refusal("the error could not be written"),
    }
}

/// What is wrong with an argument text, in a short sentence: the name of a field that is unknown, missing or repeated,
/// or what kind of value was expected (a word that is not one of the allowed words is called `unknown variant`, without
/// the word). It never repeats a value the caller sent, and it is at most
/// [`ERROR_TEXT_LIMIT`] characters.
pub fn argument_error(error: &serde_json::Error) -> String {
    use serde_json::error::Category;

    let full = error.to_string();
    // serde_json appends the place (`at line 1 column 9`); the place says nothing about which field it is.
    let message = match full.rfind(" at line ") {
        Some(at) => &full[..at],
        None => full.as_str(),
    };
    let named = ["unknown field `", "missing field `", "duplicate field `"];
    let text = if message.starts_with("unknown variant `") {
        // The word that was sent is a value of the caller, so it is not repeated; the field it was in is not known here.
        "unknown variant"
    } else if named.iter().any(|start| message.starts_with(start)) {
        // The name, and not the list of the names that would have been right.
        match message.rfind("`, expected ") {
            Some(end) => &message[..=end],
            None => message,
        }
    } else if error.classify() == Category::Data {
        // `invalid type: string "..." , expected u32`: keep what was expected, drop what was sent.
        match message.rsplit_once(", expected ") {
            Some((_, expected)) => return bounded(&format!("invalid argument, expected {expected}")),
            None => "invalid argument",
        }
    } else {
        "the arguments are not valid JSON"
    };
    bounded(text)
}

/// Read the argument text as the struct a row names; a refusal is the outcome to answer with.
fn parse<A: DeserializeOwned>(args: &str) -> Result<A, Outcome> {
    serde_json::from_str(args).map_err(|error| refusal(&argument_error(&error)))
}

// ---- the expansion of the table ---------------------------------------------------------------------------------------

macro_rules! scope_of {
    (none) => {
        Scope::None
    };
    (window) => {
        Scope::Window
    };
}

macro_rules! mode_of {
    (sync) => {
        Mode::Sync
    };
    (async) => {
        Mode::Async
    };
}

macro_rules! web_of {
    (same) => {
        Web::Same
    };
    (unavailable) => {
        Web::Unavailable
    };
    (variant) => {
        Web::Variant
    };
}

macro_rules! shell_of {
    (generated) => {
        Shell::Generated
    };
    (hand) => {
        Shell::Hand
    };
}

/// The function the browser build runs for a row: `run`, or the variant.
macro_rules! browser {
    (same; $run:path) => {
        $run
    };
    (variant, $variant:path; $run:path) => {
        $variant
    };
}

/// The call of a plain function with the fields of the parsed arguments, and the window label for a window row.
macro_rules! invoke {
    (none; $f:expr; $core:ident, $window:ident, $parsed:ident; [$($arg:ident),*]) => {
        $f($core $(, $parsed.$arg)*)
    };
    (window; $f:expr; $core:ident, $window:ident, $parsed:ident; [$($arg:ident),*]) => {
        $f($core, $window $(, $parsed.$arg)*)
    };
}

/// The module of one row: its argument struct and its `call`.
macro_rules! row_module {
    // The browser build does not have the command: the arguments are not read and nothing runs.
    (
        name: $name:ident, shell: $shell:ident, group: $group:ident, scope: $scope:ident, mode: $mode:ident,
        cap: $cap:expr,
        args: [$($arg:ident : $aty:ty),* $(,)?],
        answers: $ok:ty, $(fails: $err:ty,)?
        run: $run:path, web: unavailable
    ) => {
        pub mod $name {
            use super::*;

            #[allow(unused_variables)]
            pub fn call(core: &AppCore, window: &str, args: &str) -> Outcome {
                unavailable()
            }
        }
    };
    // A command that can fail and answers raw bytes: the plain function returns the bytes, and they are not written as JSON.
    (
        name: $name:ident, shell: $shell:ident, group: $group:ident, scope: $scope:ident, mode: $mode:ident,
        cap: $cap:expr,
        args: [$($arg:ident : $aty:ty),* $(,)?],
        answers: raw $ok:ty, fails: $err:ty,
        run: $run:path, web: $web:ident $(($variant:path))?
    ) => {
        pub mod $name {
            use super::*;

            #[derive(::serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            #[allow(dead_code)]
            pub struct Args {
                $(pub $arg: $aty,)*
            }

            #[allow(unused_variables)]
            pub fn call(core: &AppCore, window: &str, args: &str) -> Outcome {
                let parsed: Args = match parse(args) {
                    Ok(parsed) => parsed,
                    Err(refused) => return refused,
                };
                let answer: Result<Vec<u8>, $err> =
                    invoke!($scope; browser!($web $(, $variant)?; $run); core, window, parsed; [$($arg),*]);
                match answer {
                    Ok(bytes) => Outcome::Binary(bytes),
                    Err(error) => err_json(&error),
                }
            }
        }
    };
    // A command that can fail.
    (
        name: $name:ident, shell: $shell:ident, group: $group:ident, scope: $scope:ident, mode: $mode:ident,
        cap: $cap:expr,
        args: [$($arg:ident : $aty:ty),* $(,)?],
        answers: $ok:ty, fails: $err:ty,
        run: $run:path, web: $web:ident $(($variant:path))?
    ) => {
        pub mod $name {
            use super::*;

            #[derive(::serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            #[allow(dead_code)]
            pub struct Args {
                $(pub $arg: $aty,)*
            }

            #[allow(unused_variables)]
            pub fn call(core: &AppCore, window: &str, args: &str) -> Outcome {
                let parsed: Args = match parse(args) {
                    Ok(parsed) => parsed,
                    Err(refused) => return refused,
                };
                let answer: Result<$ok, $err> =
                    invoke!($scope; browser!($web $(, $variant)?; $run); core, window, parsed; [$($arg),*]);
                match answer {
                    Ok(value) => ok_json(&value),
                    Err(error) => err_json(&error),
                }
            }
        }
    };
    // A command that cannot fail.
    (
        name: $name:ident, shell: $shell:ident, group: $group:ident, scope: $scope:ident, mode: $mode:ident,
        cap: $cap:expr,
        args: [$($arg:ident : $aty:ty),* $(,)?],
        answers: $ok:ty,
        run: $run:path, web: $web:ident $(($variant:path))?
    ) => {
        pub mod $name {
            use super::*;

            #[derive(::serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            #[allow(dead_code)]
            pub struct Args {
                $(pub $arg: $aty,)*
            }

            #[allow(unused_variables)]
            pub fn call(core: &AppCore, window: &str, args: &str) -> Outcome {
                let parsed: Args = match parse(args) {
                    Ok(parsed) => parsed,
                    Err(refused) => return refused,
                };
                let answer: $ok =
                    invoke!($scope; browser!($web $(, $variant)?; $run); core, window, parsed; [$($arg),*]);
                ok_json(&answer)
            }
        }
    };
}

/// The entry of one row in [`COMMANDS`].
macro_rules! row_meta {
    (
        name: $name:ident, shell: $shell:ident, group: $group:ident, scope: $scope:ident, mode: $mode:ident,
        cap: $cap:expr,
        args: [$($arg:ident : $aty:ty),* $(,)?],
        answers: raw $ok:ty, fails: $err:ty,
        run: $run:path, web: $web:ident $(($variant:path))?
    ) => {
        Command {
            name: stringify!($name),
            group: stringify!($group),
            scope: scope_of!($scope),
            mode: mode_of!($mode),
            cap: $cap,
            args: &[$(ArgInfo { name: stringify!($arg), ty: stringify!($aty) }),*],
            answer: stringify!($ok),
            error: Some(stringify!($err)),
            web: web_of!($web),
            shell: shell_of!($shell),
            raw: true,
            call: rows::$name::call,
        }
    };
    (
        name: $name:ident, shell: $shell:ident, group: $group:ident, scope: $scope:ident, mode: $mode:ident,
        cap: $cap:expr,
        args: [$($arg:ident : $aty:ty),* $(,)?],
        answers: $ok:ty, fails: $err:ty,
        run: $run:path, web: $web:ident $(($variant:path))?
    ) => {
        Command {
            name: stringify!($name),
            group: stringify!($group),
            scope: scope_of!($scope),
            mode: mode_of!($mode),
            cap: $cap,
            args: &[$(ArgInfo { name: stringify!($arg), ty: stringify!($aty) }),*],
            answer: stringify!($ok),
            error: Some(stringify!($err)),
            web: web_of!($web),
            shell: shell_of!($shell),
            raw: false,
            call: rows::$name::call,
        }
    };
    (
        name: $name:ident, shell: $shell:ident, group: $group:ident, scope: $scope:ident, mode: $mode:ident,
        cap: $cap:expr,
        args: [$($arg:ident : $aty:ty),* $(,)?],
        answers: $ok:ty,
        run: $run:path, web: $web:ident $(($variant:path))?
    ) => {
        Command {
            name: stringify!($name),
            group: stringify!($group),
            scope: scope_of!($scope),
            mode: mode_of!($mode),
            cap: $cap,
            args: &[$(ArgInfo { name: stringify!($arg), ty: stringify!($aty) }),*],
            answer: stringify!($ok),
            error: None,
            web: web_of!($web),
            shell: shell_of!($shell),
            raw: false,
            call: rows::$name::call,
        }
    };
}

/// The callback of the table: the row modules and the slice.
macro_rules! registry_rows {
    ($({ $($row:tt)* })*) => {
        mod rows {
            use super::*;

            $(row_module! { $($row)* })*
        }

        /// Every command of the table, in the order of the table.
        pub static COMMANDS: &[Command] = &[$(row_meta! { $($row)* }),*];
    };
}

crate::host_commands!(registry_rows);
