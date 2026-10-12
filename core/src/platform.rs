//! The per-OS decisions of the core, as pure functions of an [`Os`] and injected facts.
//!
//! Nothing here reads the machine it runs on: the environment and the file system are passed in as closures, so
//! the rules for Windows and Linux are unit-tested on a Mac (`core/tests/platform.rs`) and the call sites pass
//! [`Os::current`] and the real environment. A decision that only one host could compile and test does not belong
//! in a `cfg!` body: it belongs here with the OS as a parameter.
//!
//! - [`shell_launch`]: the program and arguments of a pane's shell.
//! - [`utf8_locale`]: the `LC_CTYPE` a child gets when its parent has no locale.
//! - [`exit_poll`] and [`rename_retries`]: the two numbers the pane actor and the atomic file writer read.

use std::time::Duration;

use crate::winshell::powershell_integration_args;

/// An operating system, as far as the core's behaviour differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    Macos,
    Windows,
    Linux,
}

impl Os {
    /// The OS this build runs on. Every Unix that is not macOS takes the Linux rules.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    /// The OS of a name as `std::env::consts::OS` and the app's `app_info` spell it. `macos` and `windows` are
    /// known; every other name is read as Linux.
    pub fn from_name(name: &str) -> Self {
        match name {
            "macos" => Self::Macos,
            "windows" => Self::Windows,
            _ => Self::Linux,
        }
    }

    /// The name [`from_name`](Self::from_name) reads back.
    pub fn name(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Windows => "windows",
            Self::Linux => "linux",
        }
    }
}

/// The program of a pane's shell and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellLaunch {
    pub program: String,
    pub args: Vec<String>,
}

/// The shell a pane starts.
///
/// `configured` is the `shell` setting: one program, never split and never run through another shell. A non-empty
/// one replaces the default on every OS. `env` looks up a variable of the parent process and `exists` says whether
/// a file exists; both are injected so that no test depends on the host.
///
/// The defaults:
///
/// - macOS and Linux: `$SHELL` (unset or empty counts as unset), else `/bin/zsh` on macOS and `/bin/sh` on Linux.
/// - Windows, in this order: `%ProgramFiles%\PowerShell\7\pwsh.exe`, a `pwsh.exe` in a directory of `PATH`, Windows
///   PowerShell under `%SystemRoot%\System32`, `%ComSpec%`, and last the bare `cmd.exe`. Only files that exist
///   are taken, and only absolute directories are searched: an empty or relative `PATH` entry would start a
///   program out of the current directory, and a network path would hand the user's credentials to a foreign host.
///
/// The arguments follow from the program's file stem, see [`shell_args`].
pub fn shell_launch(
    os: Os,
    configured: &str,
    env: &dyn Fn(&str) -> Option<String>,
    exists: &dyn Fn(&str) -> bool,
) -> ShellLaunch {
    let program = if !configured.is_empty() {
        configured.to_string()
    } else if os == Os::Windows {
        windows_shell(env, exists)
    } else {
        let fallback = if os == Os::Macos { "/bin/zsh" } else { "/bin/sh" };
        non_empty(env("SHELL")).unwrap_or_else(|| fallback.to_string())
    };
    let args = shell_args(os, &program);
    ShellLaunch { program, args }
}

/// The Windows default, see [`shell_launch`]. `cmd.exe` is the one program that is not a path: it is resolved
/// by the spawn itself, never relative to the current directory.
fn windows_shell(env: &dyn Fn(&str) -> Option<String>, exists: &dyn Fn(&str) -> bool) -> String {
    let var = |key: &str| non_empty(env(key)).filter(|value| is_drive_absolute(value));
    if let Some(dir) = var("ProgramFiles") {
        let pwsh = join_windows(&dir, r"PowerShell\7\pwsh.exe");
        if exists(&pwsh) {
            return pwsh;
        }
    }
    if let Some(path) = non_empty(env("PATH")) {
        for entry in path.split(';') {
            let dir = entry.trim_matches('"');
            if !is_drive_absolute(dir) {
                continue;
            }
            let pwsh = join_windows(dir, "pwsh.exe");
            if exists(&pwsh) {
                return pwsh;
            }
        }
    }
    if let Some(root) = var("SystemRoot") {
        let powershell = join_windows(&root, r"System32\WindowsPowerShell\v1.0\powershell.exe");
        if exists(&powershell) {
            return powershell;
        }
    }
    var("ComSpec").unwrap_or_else(|| "cmd.exe".to_string())
}

/// The arguments of the shell `program` for `os`: the one place that knows what a shell wants.
///
/// Unix shells always get `-l`, so a pane is a login shell. On Windows the file stem decides, read without case
/// and without a `.exe` suffix: both PowerShell flavours get `-NoLogo` (no banner; `-l` would be an error in
/// Windows PowerShell) and then the prompt wrapper that announces the directory
/// ([`powershell_integration_args`]; this is the only place that adds it), `cmd` gets none, the Unix shells that run on Windows (`bash`, `zsh`, `fish`, `sh`, `nu`)
/// get `-l`, and any other program gets none.
fn shell_args(os: Os, program: &str) -> Vec<String> {
    if os != Os::Windows {
        return vec!["-l".to_string()];
    }
    let file = program.rsplit(['\\', '/']).next().unwrap_or(program).to_ascii_lowercase();
    let stem = file.strip_suffix(".exe").unwrap_or(&file);
    match stem {
        "pwsh" | "powershell" => {
            let mut args = vec!["-NoLogo".to_string()];
            args.extend(powershell_integration_args());
            args
        }
        "bash" | "zsh" | "fish" | "sh" | "nu" => vec!["-l".to_string()],
        _ => Vec::new(),
    }
}

/// The locale a child gets when its parent has none, as the value of `LC_CTYPE`: a Finder-launched macOS app
/// has no `LANG`, and a shell without a UTF-8 locale mangles every non-ASCII character. Windows programs do not
/// read `LC_CTYPE`, so Windows gets none.
pub fn utf8_locale(os: Os) -> Option<&'static str> {
    match os {
        Os::Macos => Some("en_US.UTF-8"),
        Os::Linux => Some("C.UTF-8"),
        Os::Windows => None,
    }
}

/// How often the pane actor asks the child whether it has exited, besides waiting for the end of its output.
/// Windows needs it: before Windows 11 24H2 the output pipe of a pseudo console does not end when the shell exits.
/// Elsewhere the reader sees the end of the PTY and no polling is needed.
pub fn exit_poll(os: Os) -> Option<Duration> {
    match os {
        Os::Windows => Some(Duration::from_millis(250)),
        Os::Macos | Os::Linux => None,
    }
}

/// How often a failed rename of an atomic file write is retried. On Windows a rename over a file that another
/// process (antivirus, an indexer, our own reload) has open fails for a moment with a sharing violation.
pub fn rename_retries(os: Os) -> u32 {
    match os {
        Os::Windows => 3,
        Os::Macos | Os::Linux => 0,
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

/// Whether `path` starts with a drive letter and a separator (`C:\` or `C:/`). A path without a drive is relative
/// to the current directory or to the current drive, and `\\host\share` is a network path: neither is a place to
/// look for a program.
fn is_drive_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/')
}

/// `rest` inside the directory `dir`, with exactly one backslash between them.
fn join_windows(dir: &str, rest: &str) -> String {
    format!(r"{}\{rest}", dir.trim_end_matches(['\\', '/']))
}
