//! Finding a tool on this computer: the override the person set, the process `PATH`, the `PATH` of their login shell,
//! a short list of fixed directories per operating system, and a version probe of what was found.
//!
//! The order of [`detect`], and the place that won is named in [`Found::place`]: the path the person set; the absolute
//! directories of the process `PATH`; the directories of the login shell's `PATH` (asked for lazily, at most once, only
//! when the first two found nothing, never on Windows); the fixed directories of the spec. A relative directory, the
//! current directory, an empty entry and on Windows a network or device path are never searched.
//! The answer is [`Found`], [`Detection::NotFound`] (with the directories that were looked in) or
//! [`Detection::Unusable`] (a path that is there or was set and cannot be used); [`recheck`] looks again right before a run.
//! [`child_path`] builds the `PATH` of the child: the tool's directory, then the process directories, then the login
//! shell's. Nothing here starts a process: the probes are described as a program and arguments plus a parser.

use std::fmt;
use std::time::Duration;

use super::IntegrationSpec;
use crate::platform::{self, Os, ProgramCheck};

/// The longest list of directories kept from the login shell and the longest `PATH` built for a child.
const MAX_DIRS: usize = 64;

/// The extensions of `PATHEXT` that are tried, in the order of the default `PATHEXT`: the ones a program can be started
/// with (`.exe`, `.com`) or a batch file the runner starts with the strict argument charset (`.cmd`, `.bat`). The others
/// (a script host's `.vbs`, `.js`) are never a tool.
const PATHEXT_PROGRAMS: [&str; 4] = [".exe", ".cmd", ".bat", ".com"];

/// The login shell is given this long; a longer one is ended and the answer is an empty list, not an error.
pub const LOGIN_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The line the probe prints before the value of `PATH`.
pub const LOGIN_PROBE_BEGIN: &str = "@@hw-path-begin";

/// The line the probe prints after the value of `PATH`.
pub const LOGIN_PROBE_END: &str = "@@hw-path-end";

/// The one script of the login-shell probe, a constant with nothing interpolated: the begin marker line, the value of
/// `PATH`, the end marker line. `command` so that an alias or function named `printenv` cannot win, and `printenv`
/// because fish prints `$PATH` as a list.
pub const LOGIN_PROBE_SCRIPT: &str = "printf '%s\\n' @@hw-path-begin; command printenv PATH; printf '%s\\n' @@hw-path-end";

/// The `PATH` the probe's environment holds: the caller starts the probe with an environment of `HOME`, `USER`, `SHELL`
/// and this, nothing else, standard input closed, in its own process group, and ends it after [`LOGIN_PROBE_TIMEOUT`].
pub const LOGIN_PROBE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// The file names of the shells that are probed.
const LOGIN_SHELLS: [&str; 6] = ["zsh", "bash", "fish", "sh", "dash", "ksh"];

/// The facts the detector reads, injected so that the whole file runs on any host for any [`Os`].
pub struct Facts<'a> {
    /// The operating system whose rules apply.
    pub os: Os,
    /// A variable of the process environment.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// What a look at a candidate file on disk finds.
    pub check: &'a dyn Fn(&str) -> ProgramCheck,
    /// The person's home directory, which `$HOME/` in a fixed directory stands for.
    pub home: Option<&'a str>,
    /// The directories of the login shell's `PATH`, asked for lazily and at most once per call.
    pub login_path: &'a dyn Fn() -> Vec<String>,
}

impl fmt::Debug for Facts<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Facts").field("os", &self.os).field("home", &self.home).finish_non_exhaustive()
    }
}

/// The place that supplied the file that was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Place {
    /// The path the person set.
    Override,
    /// A directory of the process `PATH`.
    Path,
    /// A directory of the login shell's `PATH`.
    LoginPath,
    /// One of the fixed directories of the operating system.
    FixedDir,
}

/// A tool that was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The absolute path of the file.
    pub path: String,
    /// The directory that holds it.
    pub dir: String,
    /// The place that won.
    pub place: Place,
    /// Whether it is a Windows batch file (`.cmd`, `.bat`), which the runner starts with the strict argument charset.
    pub batch: bool,
    /// The version, once a version probe has run.
    pub version: Option<String>,
}

/// Why a path cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnusableReason {
    /// Not an absolute path (or one with a control character): it is never resolved against the current directory.
    NotAbsolute,
    /// On Windows, a network (`\\host\share`) or device (`\\?\`, `\\.\`) path.
    NetworkPath,
    /// Nothing is there.
    Missing,
    /// Something is there, but it is not a file.
    NotAFile,
    /// A file that cannot be executed.
    NotExecutable,
}

impl UnusableReason {
    /// One plain sentence fragment for the person: what is wrong with the path.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotAbsolute => "not an absolute path",
            Self::NetworkPath => "a network or device path",
            Self::Missing => "not found",
            Self::NotAFile => "not a file",
            Self::NotExecutable => "not executable",
        }
    }
}

impl fmt::Display for UnusableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the detector found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    /// The tool, usable.
    Found(Found),
    /// Nothing was found; the directories that were searched, each once, in order.
    NotFound { searched: Vec<String> },
    /// A path that is there or was set, and cannot be used.
    Unusable { path: String, reason: UnusableReason },
}

/// A program and its arguments, described and not run: the caller starts it through the runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The absolute path of the program.
    pub program: String,
    /// The arguments after it.
    pub args: Vec<String>,
}

/// The login-shell probe for `shell` (the value of `$SHELL`), or `None` when there is none to run: never on Windows, and
/// only for an absolute path that `exists` and whose file name is `zsh`, `bash`, `fish`, `sh`, `dash` or `ksh`. Any other
/// program (nushell among them) is not probed: `$SHELL` is another party's text, and the probe starts it.
pub fn login_probe(os: Os, shell: Option<&str>, exists: &dyn Fn(&str) -> bool) -> Option<Probe> {
    if os == Os::Windows {
        return None;
    }
    let shell = shell?;
    let name = shell.rsplit('/').next().unwrap_or("");
    let plain = shell.starts_with('/') && !shell.chars().any(char::is_control) && LOGIN_SHELLS.contains(&name);
    if !plain || !exists(shell) {
        return None;
    }
    Some(Probe { program: shell.to_string(), args: vec!["-l".into(), "-c".into(), LOGIN_PROBE_SCRIPT.into()] })
}

/// The directories in the output of the login-shell probe. Only the text between marker lines counts: the last begin
/// marker wins (a banner may print the same line), the value is exactly one line, and a begin marker without an end
/// marker after it, or any other number of lines, gives an empty list. The value is split with the separator of `os`; an
/// entry is kept when it is an absolute path without a control character that `exists` accepts, exactly as written;
/// a repeated entry is dropped (the first wins) and the list ends after 64.
pub fn parse_login_path(os: Os, output: &[u8], exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let text = String::from_utf8_lossy(output);
    let lines: Vec<&str> = text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)).collect();
    let Some(begin) = lines.iter().rposition(|line| *line == LOGIN_PROBE_BEGIN) else {
        return Vec::new();
    };
    let Some(end) = lines[begin + 1..].iter().position(|line| *line == LOGIN_PROBE_END).map(|at| begin + 1 + at) else {
        return Vec::new();
    };
    if end - begin != 2 {
        return Vec::new();
    }
    let mut kept = Dirs::new(os);
    for entry in lines[begin + 1].split(separator(os)) {
        if kept.len() == MAX_DIRS {
            break;
        }
        if platform::is_absolute_path(os, entry) && exists(entry) {
            kept.push(entry);
        }
    }
    kept.into_vec()
}

/// The separator of the entries of a `PATH`.
fn separator(os: Os) -> char {
    if os == Os::Windows { ';' } else { ':' }
}

/// A list of directories that keeps only absolute ones without a control character, each once (by text, ignoring case on
/// Windows), in the order they were offered.
struct Dirs {
    os: Os,
    seen: std::collections::HashSet<String>,
    list: Vec<String>,
}

impl Dirs {
    fn new(os: Os) -> Self {
        Self { os, seen: std::collections::HashSet::new(), list: Vec::new() }
    }

    fn len(&self) -> usize {
        self.list.len()
    }

    fn key(&self, dir: &str) -> String {
        if self.os == Os::Windows { dir.trim_end_matches(['\\', '/']).to_ascii_lowercase() } else { dir.to_string() }
    }

    /// Adds `dir` unless it is relative, holds a control character or is already there. True when it was added.
    fn push(&mut self, dir: &str) -> bool {
        if !platform::is_absolute_path(self.os, dir) || dir.chars().any(char::is_control) {
            return false;
        }
        if !self.seen.insert(self.key(dir)) {
            return false;
        }
        self.list.push(dir.to_string());
        true
    }

    fn into_vec(self) -> Vec<String> {
        self.list
    }
}

/// Finds the program of `spec`: the order is in the module comment. `override_path` is the path the person set (an empty
/// one counts as none): when it is set it is the only place looked at, and a set path that cannot be used is
/// [`Detection::Unusable`], never silently replaced by another one.
pub fn detect(spec: &IntegrationSpec, facts: &Facts<'_>, override_path: Option<&str>) -> Detection {
    if let Some(set) = override_path.filter(|set| !set.is_empty()) {
        return match vet(facts, set) {
            Ok(()) => Detection::Found(found_at(facts.os, set, Place::Override)),
            Err(reason) => Detection::Unusable { path: set.to_string(), reason },
        };
    }
    let os = facts.os;
    let names: Vec<&str> = if os == Os::Windows { spec.windows_binaries } else { spec.binaries }
        .iter()
        .copied()
        .filter(|name| is_plain_name(name))
        .collect();
    let mut search = Search { facts, names, seen: Dirs::new(os), searched: Vec::new(), blocked: None };

    let path_dirs = platform::search_dirs(os, facts.env);
    if let Some(found) = search.scan(&path_dirs, Place::Path) {
        return Detection::Found(found);
    }
    if os != Os::Windows {
        let login = (facts.login_path)();
        if let Some(found) = search.scan(&login, Place::LoginPath) {
            return Detection::Found(found);
        }
    }
    let fixed: Vec<String> = spec.fixed_dirs.iter().filter_map(|entry| expand_fixed(facts, entry)).collect();
    if let Some(found) = search.scan(&fixed, Place::FixedDir) {
        return Detection::Found(found);
    }
    match search.blocked {
        Some((path, reason)) => Detection::Unusable { path, reason },
        None => Detection::NotFound { searched: search.searched },
    }
}

/// The directories searched so far and the first file that was there but could not be used.
struct Search<'a, 'b> {
    facts: &'a Facts<'b>,
    names: Vec<&'a str>,
    seen: Dirs,
    searched: Vec<String>,
    blocked: Option<(String, UnusableReason)>,
}

impl Search<'_, '_> {
    /// Looks in each new absolute directory of `dirs`, in order, for each name; the first usable file wins. A file that
    /// is there but unusable is remembered (the first one) and the search goes on: a later usable file wins over it.
    fn scan(&mut self, dirs: &[String], place: Place) -> Option<Found> {
        let os = self.facts.os;
        let pathext = (os == Os::Windows).then(|| pathext(self.facts.env));
        for dir in dirs {
            if !self.seen.push(dir) {
                continue;
            }
            self.searched.push(dir.clone());
            for name in &self.names {
                for candidate in platform::candidate_names(os, name, pathext.as_deref()) {
                    let path = platform::join_path(os, dir, &candidate);
                    match vet(self.facts, &path) {
                        Ok(()) => return Some(found_at(os, &path, place)),
                        Err(UnusableReason::Missing) => {}
                        Err(reason) => {
                            if self.blocked.is_none() {
                                self.blocked = Some((path, reason));
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

/// A program name that is a plain file name: not empty, no separator, no control character.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\']) && !name.chars().any(char::is_control)
}

/// The extensions to try on Windows: those of `PATHEXT` in its order that are programs ([`PATHEXT_PROGRAMS`]), in lower
/// case, joined by `;`; the default order when `PATHEXT` is unset, empty or holds none of them.
fn pathext(env: &dyn Fn(&str) -> Option<String>) -> String {
    let kept: Vec<String> = env("PATHEXT")
        .unwrap_or_default()
        .split(';')
        .map(|ext| ext.trim().to_ascii_lowercase())
        .filter(|ext| PATHEXT_PROGRAMS.contains(&ext.as_str()))
        .collect();
    if kept.is_empty() { PATHEXT_PROGRAMS.join(";") } else { kept.join(";") }
}

/// Whether the file at `path` can be the tool: absolute (and on Windows not a network or device path) and `check` finds
/// it usable.
fn vet(facts: &Facts<'_>, path: &str) -> Result<(), UnusableReason> {
    if facts.os == Os::Windows && (path.starts_with(r"\\") || path.starts_with("//")) {
        return Err(UnusableReason::NetworkPath);
    }
    if !platform::is_absolute_path(facts.os, path) || path.chars().any(char::is_control) {
        return Err(UnusableReason::NotAbsolute);
    }
    match (facts.check)(path) {
        ProgramCheck::Usable => Ok(()),
        ProgramCheck::Missing => Err(UnusableReason::Missing),
        ProgramCheck::NotAFile => Err(UnusableReason::NotAFile),
        ProgramCheck::NotExecutable => Err(UnusableReason::NotExecutable),
    }
}

/// The directory of an absolute `path` (the whole text up to its last separator; `/` for a file at the root).
fn parent_dir(os: Os, path: &str) -> String {
    let at = if os == Os::Windows { path.rfind(['\\', '/']) } else { path.rfind('/') };
    match at {
        Some(0) => path[..1].to_string(),
        Some(at) => path[..at].to_string(),
        None => String::new(),
    }
}

/// Whether the file is a batch file that only a Windows command interpreter runs.
fn is_batch(os: Os, path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    os == Os::Windows && (lower.ends_with(".cmd") || lower.ends_with(".bat"))
}

fn found_at(os: Os, path: &str, place: Place) -> Found {
    Found { path: path.to_string(), dir: parent_dir(os, path), place, batch: is_batch(os, path), version: None }
}

/// A fixed directory of a spec with its variables expanded: `$HOME/` or `~/` at the start from the injected home, and
/// `%NAME%` from the environment. An entry with a variable that is not set (or is empty), another `$` or `~` form, an
/// unclosed `%`, or a result that is not an absolute path of this system, is left out.
fn expand_fixed(facts: &Facts<'_>, entry: &str) -> Option<String> {
    let os = facts.os;
    let mut out = String::new();
    let mut rest = entry;
    if let Some(tail) = entry.strip_prefix("$HOME/").or_else(|| entry.strip_prefix("~/")) {
        let home = facts.home.filter(|home| !home.is_empty())?;
        out.push_str(home.trim_end_matches(['/', '\\']));
        out.push('/');
        rest = tail;
    } else if entry.starts_with('$') || entry.starts_with('~') {
        return None;
    }
    while let Some(open) = rest.find('%') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after.find('%')?;
        let name = &after[..close];
        let valid = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return None;
        }
        out.push_str(&(facts.env)(name).filter(|value| !value.is_empty())?);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    if os == Os::Windows {
        out = out.replace('/', "\\");
    }
    (platform::is_absolute_path(os, &out) && !out.chars().any(char::is_control)).then_some(out)
}
