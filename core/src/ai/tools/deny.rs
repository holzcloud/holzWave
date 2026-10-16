//! The deny list: which locations the agent's file tools may never read or write.
//!
//! The check is lexical and pure ([`DenyPolicy::check`]) and, for this computer, followed by a check of the path after
//! the file system resolved it ([`DenyPolicy::check_local`]). The lexical part cannot see symbolic links, so a local
//! path is never judged by its spelling alone.

use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

/// How a path is spelled: with `/` only, or with `/` and `\` and the quirks of Windows names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Components are separated by `/`.
    Posix,
    /// Components are separated by `/` or `\`.
    Windows,
}

impl Style {
    /// The style of the system this program runs on.
    pub fn native() -> Self {
        if cfg!(windows) { Self::Windows } else { Self::Posix }
    }
}

/// What a call does with a path. A path denied for `Read` is denied for both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// The call reads the path.
    Read,
    /// The call changes the path.
    Write,
}

/// Whose file system a path belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsKind {
    /// This computer.
    Local,
    /// Another machine; the directories the shell protects are not on it.
    Remote,
}

/// A refusal, with the name of the rule that made it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Denied {
    rule: &'static str,
}

impl Denied {
    fn new(rule: &'static str) -> Self {
        Self { rule }
    }

    /// The stable name of the rule: `secret_location`, `env_file`, `start_file`, `git_internals`, `editor_config`,
    /// `proc_environ`, `protected_dir` or `unresolvable_path`.
    pub fn rule(&self) -> &'static str {
        self.rule
    }
}

impl fmt::Debug for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Denied({})", self.rule)
    }
}

impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("this location is protected")
    }
}

impl std::error::Error for Denied {}

/// How a row finds its place in a path.
#[derive(Clone, Copy)]
enum Matcher {
    /// The final name starts with this prefix.
    NamePrefix(&'static str),
}

/// Which access a row refuses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Refuses {
    /// Reading and writing.
    Both,
}

/// One rule of the list.
struct Row {
    rule: &'static str,
    matcher: Matcher,
    refuses: Refuses,
}

/// The rows, checked in order. Over-matching is intended: a refusal that is too wide costs the user a message, one
/// that is too narrow costs a secret.
const ROWS: &[Row] = &[
    // `.env` files hold the keys of the project they sit in; the agent must not read them or plant its own.
    Row { rule: "env_file", matcher: Matcher::NamePrefix(".env"), refuses: Refuses::Both },
];

/// The deny list, with the directories the shell protects.
#[derive(Clone, Debug)]
pub struct DenyPolicy {
    protected: Vec<Vec<String>>,
}

impl DenyPolicy {
    /// A policy that also refuses everything below `protected`: the data directory, the configuration directory and
    /// the directory of the `wsh` socket. Each directory is kept as given and, when it exists, in its canonical form.
    pub fn new(protected: Vec<PathBuf>) -> Self {
        let mut kept: Vec<Vec<String>> = Vec::new();
        let mut add = |path: &Path| {
            let parts = components(Style::native(), &path.to_string_lossy());
            if !parts.is_empty() && !kept.contains(&parts) {
                kept.push(parts);
            }
        };
        for dir in &protected {
            add(dir);
            if let Ok(canonical) = std::fs::canonicalize(dir) {
                add(&canonical);
            }
        }
        Self { protected: kept }
    }

    /// Judges a path by its spelling.
    ///
    /// `..` segments, repeated separators and `.` segments are applied first, and names compare without regard to
    /// ASCII case. The protected directories only apply to [`FsKind::Local`].
    pub fn check(&self, style: Style, fs: FsKind, path: &str, access: Access) -> Result<(), Denied> {
        let parts = components(style, path);
        if fs == FsKind::Local {
            for dir in &self.protected {
                if parts.len() >= dir.len() && parts.iter().zip(dir).all(|(a, b)| a.eq_ignore_ascii_case(b)) {
                    return Err(Denied::new("protected_dir"));
                }
            }
        }
        for row in ROWS {
            if row.matches(&parts) && row.refuses_for(access) {
                return Err(Denied::new(row.rule));
            }
        }
        Ok(())
    }

    /// Judges a path on this computer: the spelling, then the path after the file system resolved it.
    ///
    /// Returns the resolved path, which is what the host must open. A path that is not absolute, not valid text or not
    /// resolvable is refused as `unresolvable_path`.
    pub fn check_local(&self, path: &Path, access: Access) -> Result<PathBuf, Denied> {
        let unresolvable = || Denied::new("unresolvable_path");
        let text = path.to_str().ok_or_else(unresolvable)?;
        if !path.is_absolute() {
            return Err(unresolvable());
        }
        self.check(Style::native(), FsKind::Local, text, access)?;
        let resolved = canonical_local(path).map_err(|_| unresolvable())?;
        let resolved_text = resolved.to_str().ok_or_else(unresolvable)?;
        self.check(Style::native(), FsKind::Local, resolved_text, access)?;
        Ok(resolved)
    }
}

impl Row {
    fn matches(&self, parts: &[String]) -> bool {
        match self.matcher {
            Matcher::NamePrefix(prefix) => parts.last().is_some_and(|name| {
                name.len() >= prefix.len() && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
            }),
        }
    }

    fn refuses_for(&self, _access: Access) -> bool {
        match self.refuses {
            Refuses::Both => true,
        }
    }
}

/// The names of a path with `.`, empty parts and `..` applied. A `..` at the root stays at the root.
fn components(style: Style, path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in path.split(|c| c == '/' || (style == Style::Windows && c == '\\')) {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            name => out.push(name.to_owned()),
        }
    }
    out
}

/// The path with every symbolic link resolved, also when the last parts of it do not exist yet.
///
/// The longest existing prefix is canonicalised and the missing names are appended. A `..` after a missing name cannot
/// be resolved and is an error.
pub fn canonical_local(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "the path is not absolute"));
    }
    match std::fs::canonicalize(path) {
        Ok(resolved) => return Ok(resolved),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(resolved) => {
                existing = resolved;
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name().map(|n| n.to_owned()) else {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "the path has no existing part"));
                };
                tail.push(name);
                existing.pop();
            }
            Err(e) => return Err(e),
        }
    }
    for name in tail.into_iter().rev() {
        let part = Path::new(&name);
        if !matches!(part.components().next(), Some(Component::Normal(_))) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "a missing part is followed by `..`"));
        }
        existing.push(name);
    }
    Ok(existing)
}
