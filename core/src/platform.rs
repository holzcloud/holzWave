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
//! - [`in_flatpak`], [`host_shell_launch`] and [`host_command`]: inside a Flatpak the shell runs on the host.
//! - [`search_dirs`], [`candidate_names`], [`is_absolute_path`] and [`join_path`]: the lookup of a program by name, shared
//!   by the shell lookup and the detector of the integrations.

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

/// What a look at one candidate program on disk found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramCheck {
    /// A file a pane can start.
    Usable,
    /// Nothing is there.
    Missing,
    /// Something is there, but it is not a file (a directory).
    NotAFile,
    /// A file without the permission to execute it.
    NotExecutable,
}

/// The shell a pane starts and, when the `shell` setting named one that cannot be started, why the default is used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellResolved {
    pub launch: ShellLaunch,
    /// One plain sentence naming the setting's value (cut at 40 characters, control characters replaced) and the
    /// reason, and the program that is used instead. `None` when the setting is empty or usable.
    pub unusable: Option<String>,
}

/// [`shell_launch`] for a setting that may be wrong: a `configured` program that cannot be started never leaves a
/// pane without a shell. It falls back to the platform default (`shell_launch` with an empty setting) and says why in
/// [`ShellResolved::unusable`].
///
/// `configured` is usable when it is an absolute path (a drive path on Windows) that `check` finds usable, or a bare
/// name that `check` finds usable in an absolute directory of `PATH` (and, on Windows, in `%SystemRoot%\System32`,
/// with `.exe` tried when the name has no extension). A relative path such as `./sh` or `~/sh` is never looked up: it
/// would depend on a directory nobody chose. A name found on `PATH` is started by its full path.
pub fn resolve_shell(
    os: Os,
    configured: &str,
    env: &dyn Fn(&str) -> Option<String>,
    exists: &dyn Fn(&str) -> bool,
    check: &dyn Fn(&str) -> ProgramCheck,
) -> ShellResolved {
    if configured.is_empty() {
        return ShellResolved { launch: shell_launch(os, "", env, exists), unusable: None };
    }
    match locate_program(os, configured, env, check) {
        Ok(program) => ShellResolved { launch: shell_launch(os, &program, env, exists), unusable: None },
        Err(why) => {
            let launch = shell_launch(os, "", env, exists);
            let shown: String =
                configured.chars().take(40).map(|c| if c.is_control() { '?' } else { c }).collect();
            let cut = if configured.chars().count() > 40 { "..." } else { "" };
            let unusable = format!(
                "the shell setting \"{shown}{cut}\" {}; {} is used instead",
                match why {
                    ProgramCheck::Usable | ProgramCheck::Missing => "was not found",
                    ProgramCheck::NotAFile => "is not a file",
                    ProgramCheck::NotExecutable => "is not executable",
                },
                launch.program
            );
            ShellResolved { launch, unusable: Some(unusable) }
        }
    }
}

/// The program `name` stands for, or what is wrong with it.
fn locate_program(
    os: Os,
    name: &str,
    env: &dyn Fn(&str) -> Option<String>,
    check: &dyn Fn(&str) -> ProgramCheck,
) -> Result<String, ProgramCheck> {
    let has_separator = name.contains('/') || (os == Os::Windows && name.contains('\\'));
    if has_separator {
        // A path is checked as it is; a relative one is not looked up.
        return if is_absolute_path(os, name) {
            match check(name) {
                ProgramCheck::Usable => Ok(name.to_string()),
                other => Err(other),
            }
        } else {
            Err(ProgramCheck::Missing)
        };
    }
    // The name of a program, searched like the shell's own lookup, in absolute directories only.
    let dirs = search_dirs(os, env);
    let candidates = candidate_names(os, name, None);
    let mut worst = ProgramCheck::Missing;
    for dir in &dirs {
        for candidate in &candidates {
            let path = join_path(os, dir, candidate);
            match check(&path) {
                ProgramCheck::Usable => return Ok(path),
                ProgramCheck::Missing => {}
                // A file that is there but cannot run is a better answer than "not found", but a later usable one wins.
                other => worst = other,
            }
        }
    }
    Err(worst)
}

/// The directories a program name is looked up in, in order: the absolute entries of `PATH` (split with the separator
/// of `os`, quotes trimmed), and on Windows `%SystemRoot%\System32` last. An empty entry, a relative entry and on
/// Windows a network or device path are dropped: an empty or relative entry would start a program out of the current
/// directory, and a network path would hand the user's credentials to a foreign host. Duplicates are kept, as the
/// shell's own lookup keeps them.
pub fn search_dirs(os: Os, env: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    let windows = os == Os::Windows;
    let mut dirs: Vec<String> = Vec::new();
    if let Some(path) = non_empty(env("PATH")) {
        let separator = if windows { ';' } else { ':' };
        dirs.extend(
            path.split(separator).map(|dir| dir.trim_matches('"').to_string()).filter(|dir| is_absolute_path(os, dir)),
        );
    }
    if windows {
        if let Some(root) = non_empty(env("SystemRoot")).filter(|root| is_drive_absolute(root)) {
            dirs.push(join_windows(&root, "System32"));
        }
    }
    dirs
}

/// The file names to try for the program `name`, in order. Unix: the name. Windows: a name that has a dot is tried as
/// it is; one without is tried with each extension of `pathext` (the value of `PATHEXT`, `;` separated, in order) and,
/// when `pathext` is `None`, as the name itself and then with `.exe` (the rule of the shell lookup).
pub fn candidate_names(os: Os, name: &str, pathext: Option<&str>) -> Vec<String> {
    if os != Os::Windows || name.contains('.') {
        return vec![name.to_string()];
    }
    match pathext {
        None => vec![name.to_string(), format!("{name}.exe")],
        Some(list) => list
            .split(';')
            .map(str::trim)
            .filter(|ext| !ext.is_empty())
            .map(|ext| if ext.starts_with('.') { format!("{name}{ext}") } else { format!("{name}.{ext}") })
            .collect(),
    }
}

/// Whether `path` is an absolute path of `os`: it starts with `/` on Unix, with a drive letter and a separator on
/// Windows (a drive-less, network or device path is not).
pub fn is_absolute_path(os: Os, path: &str) -> bool {
    if os == Os::Windows { is_drive_absolute(path) } else { path.starts_with('/') }
}

/// `name` inside the directory `dir`, with exactly one separator of `os` between them.
pub fn join_path(os: Os, dir: &str, name: &str) -> String {
    if os == Os::Windows { join_windows(dir, name) } else { format!("{}/{name}", dir.trim_end_matches('/')) }
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

/// Whether this process runs inside a Flatpak sandbox: `/.flatpak-info` exists or `FLATPAK_ID` is set and not empty.
/// The facts are injected like everywhere in this module, so the answer is the same on every OS and a test needs no
/// Flatpak.
pub fn in_flatpak(env: &dyn Fn(&str) -> Option<String>, exists: &dyn Fn(&str) -> bool) -> bool {
    non_empty(env("FLATPAK_ID")).is_some() || exists("/.flatpak-info")
}

/// The one script that finds the user's login shell on the host and starts it. It is run as `sh -c <script>` on the
/// host, asks `getent passwd` for the shell of the user, falls back to `/bin/sh` when there is none or it cannot be
/// run, and execs it as a login shell (`-l`, like every Unix pane). A constant: nothing is interpolated into it.
pub const HOST_LOGIN_SHELL_SCRIPT: &str = "u=$(id -un); s=$(getent passwd \"$u\" 2>/dev/null | cut -d: -f7); \
if [ -z \"$s\" ] || [ ! -x \"$s\" ]; then s=/bin/sh; fi; exec \"$s\" -l";

/// The shell a pane starts on the host, for a Flatpak session. `configured` is the `shell` setting: when it is not
/// empty, that program (never split, not looked up: it names a program of the host, which the sandbox cannot see) with
/// the arguments of its file stem; otherwise `sh -c` [`HOST_LOGIN_SHELL_SCRIPT`].
pub fn host_shell_launch(configured: &str) -> ShellLaunch {
    if configured.is_empty() {
        return ShellLaunch { program: "sh".to_string(), args: vec!["-c".to_string(), HOST_LOGIN_SHELL_SCRIPT.to_string()] };
    }
    ShellLaunch { program: configured.to_string(), args: shell_args(Os::Linux, configured) }
}

/// A shell launch as the command line that runs it on the host from inside a Flatpak:
/// `flatpak-spawn --host --watch-bus [--directory=<cwd>] [--env=NAME=VALUE]... -- <program> <args>`.
///
/// The options come only from constants and from the two validated inputs, each inside ONE argument:
///
/// - `cwd` becomes `--directory=<cwd>`, so a directory that looks like an option, or holds spaces or an equals sign,
///   is still the value of that one option. A directory with a control character (NUL included) is refused and the
///   pane starts without one, as does an empty one.
/// - each pair of `env` becomes `--env=NAME=VALUE`; a pair whose name is not `[A-Za-z_][A-Za-z0-9_]*` or whose value
///   has a control character is dropped.
///
/// The program and the arguments of `launch` follow the one `--` as separate arguments, unchanged and never joined into
/// a string, so nothing of them can become an option of `flatpak-spawn`. `--watch-bus` ends the host process when
/// the app's bus connection closes.
///
/// Whether a shell started this way gets a controlling terminal (job control, `sudo`) is an assumption until row L10
/// of `docs/test-plan.md` passes on a real Flatpak.
pub fn host_command(launch: &ShellLaunch, cwd: Option<&str>, env: &[(String, String)]) -> ShellLaunch {
    let mut args = vec!["--host".to_string(), "--watch-bus".to_string()];
    if let Some(dir) = cwd.filter(|dir| !dir.is_empty() && !dir.chars().any(char::is_control)) {
        args.push(format!("--directory={dir}"));
    }
    for (name, value) in env {
        if is_env_name(name) && !value.chars().any(char::is_control) {
            args.push(format!("--env={name}={value}"));
        }
    }
    args.push("--".to_string());
    args.push(launch.program.clone());
    args.extend(launch.args.iter().cloned());
    ShellLaunch { program: "flatpak-spawn".to_string(), args }
}

/// A name of an environment variable the way a shell reads it: a letter or underscore, then letters, digits, underscores.
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
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
