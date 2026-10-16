//! The deny list: which locations the agent's file tools may never read or write.
//!
//! The check is lexical and pure ([`DenyPolicy::check`]) and, for this computer, followed by a check of the path after
//! the file system resolved it ([`DenyPolicy::check_local`]). The lexical part cannot see symbolic links, so a local
//! path is never judged by its spelling alone.
//!
//! The list protects three things: the user's secrets (keys, credentials, `.env` files), the places that hold the
//! agent's own gate (shell start files, git hooks, editor configuration, and every directory the shell protects), and
//! the process environment of other processes. Each row says which attack it stops. A refusal that is too wide costs
//! the user a message; one that is too narrow costs a secret, so the rows over-match on purpose.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

/// How many symbolic links one path may pass through before it is given up.
const MAX_LINK_HOPS: usize = 40;

/// How a path is spelled: with `/` only, or with `/` and `\` and the quirks of Windows names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Components are separated by `/`.
    Posix,
    /// Components are separated by `/` or `\`. A drive letter and a verbatim prefix are dropped, trailing dots and
    /// spaces of a name do not count, and a name ends at its first `:` (an alternate data stream).
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
    /// Any part of the path has this name, at any depth.
    Component(&'static str),
    /// Two parts in a row have these names.
    Pair(&'static str, &'static str),
    /// The final name is this one.
    Name(&'static str),
    /// The final name starts with this prefix.
    NamePrefix(&'static str),
    /// `/proc/<anything>/environ`: the environment of another process.
    ProcEnviron,
}

/// Which access a row refuses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Refuses {
    /// Reading and writing.
    Both,
    /// Writing only; the file may still be read.
    Write,
}

/// One rule of the list.
struct Row {
    rule: &'static str,
    matcher: Matcher,
    refuses: Refuses,
}

const fn row(rule: &'static str, matcher: Matcher, refuses: Refuses) -> Row {
    Row { rule, matcher, refuses }
}

/// The rows, checked in order; the first one that matches names the refusal.
const ROWS: &[Row] = &[
    // Secret locations. Reading them leaks the user's keys; writing them plants the agent's own key (an
    // `authorized_keys` line) or replaces a credential helper. Neither is ever the user's intent for an agent.
    row("secret_location", Matcher::Component(".ssh"), Refuses::Both),
    row("secret_location", Matcher::Component(".gnupg"), Refuses::Both),
    row("secret_location", Matcher::Component(".aws"), Refuses::Both),
    row("secret_location", Matcher::Component(".kube"), Refuses::Both),
    row("secret_location", Matcher::Component(".azure"), Refuses::Both),
    row("secret_location", Matcher::Pair(".config", "gcloud"), Refuses::Both),
    row("secret_location", Matcher::Pair(".docker", "config.json"), Refuses::Both),
    row("secret_location", Matcher::Name(".netrc"), Refuses::Both),
    row("secret_location", Matcher::Name("_netrc"), Refuses::Both),
    row("secret_location", Matcher::Name(".pgpass"), Refuses::Both),
    row("secret_location", Matcher::Name(".git-credentials"), Refuses::Both),
    row("secret_location", Matcher::Name(".npmrc"), Refuses::Both),
    row("secret_location", Matcher::Name(".pypirc"), Refuses::Both),
    // Private keys keep their default names wherever they were copied to.
    row("secret_location", Matcher::Name("id_rsa"), Refuses::Both),
    row("secret_location", Matcher::Name("id_dsa"), Refuses::Both),
    row("secret_location", Matcher::Name("id_ecdsa"), Refuses::Both),
    row("secret_location", Matcher::Name("id_ed25519"), Refuses::Both),
    // `.env*` files hold the keys of the project they sit in; the agent must not read them or plant its own.
    row("env_file", Matcher::NamePrefix(".env"), Refuses::Both),
    // The environment of another process holds its secrets in clear text.
    row("proc_environ", Matcher::ProcEnviron, Refuses::Both),
    // Shell start files run on the next terminal: a write is a command that runs without a card. They stay readable,
    // because users ask for help with their prompt setup.
    row("start_file", Matcher::Name(".bashrc"), Refuses::Write),
    row("start_file", Matcher::Name(".bash_profile"), Refuses::Write),
    row("start_file", Matcher::Name(".bash_login"), Refuses::Write),
    row("start_file", Matcher::Name(".bash_logout"), Refuses::Write),
    row("start_file", Matcher::Name(".profile"), Refuses::Write),
    row("start_file", Matcher::Name(".zshrc"), Refuses::Write),
    row("start_file", Matcher::Name(".zshenv"), Refuses::Write),
    row("start_file", Matcher::Name(".zprofile"), Refuses::Write),
    row("start_file", Matcher::Name(".zlogin"), Refuses::Write),
    row("start_file", Matcher::Name(".zlogout"), Refuses::Write),
    row("start_file", Matcher::Name("config.fish"), Refuses::Write),
    row("start_file", Matcher::Pair(".config", "fish"), Refuses::Write),
    row("start_file", Matcher::Name("Microsoft.PowerShell_profile.ps1"), Refuses::Write),
    row("start_file", Matcher::Name("profile.ps1"), Refuses::Write),
    // Git hooks and configuration run programs the next time git runs.
    row("git_internals", Matcher::Pair(".git", "hooks"), Refuses::Write),
    row("git_internals", Matcher::Pair(".git", "config"), Refuses::Write),
    row("git_internals", Matcher::Name(".gitconfig"), Refuses::Write),
    // Editor configuration can define tasks that run when a folder is opened.
    row("editor_config", Matcher::Component(".vscode"), Refuses::Write),
    row("editor_config", Matcher::Component(".idea"), Refuses::Write),
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
            if row.refuses_for(access) && row.matches(style, &parts) {
                return Err(Denied::new(row.rule));
            }
        }
        Ok(())
    }

    /// Judges a path on this computer: the spelling, then the path after the file system resolved it.
    ///
    /// Symbolic links are followed first, also when the last parts of the path do not exist yet (a file about to be
    /// written) and when a link dangles. Returns the resolved path, which is what the host must open. A path that is
    /// not absolute, not valid text or not resolvable is refused as `unresolvable_path`.
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
    fn refuses_for(&self, access: Access) -> bool {
        match self.refuses {
            Refuses::Both => true,
            Refuses::Write => access == Access::Write,
        }
    }

    fn matches(&self, style: Style, parts: &[String]) -> bool {
        let is = |part: &String, name: &str| part.eq_ignore_ascii_case(name);
        match self.matcher {
            Matcher::Component(name) => parts.iter().any(|part| is(part, name)),
            Matcher::Pair(first, second) => parts.windows(2).any(|pair| is(&pair[0], first) && is(&pair[1], second)),
            Matcher::Name(name) => parts.last().is_some_and(|last| is(last, name)),
            Matcher::NamePrefix(prefix) => parts.last().is_some_and(|last| {
                last.len() >= prefix.len() && last.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
            }),
            Matcher::ProcEnviron => {
                style == Style::Posix
                    && parts.first().is_some_and(|first| is(first, "proc"))
                    && parts[1..].iter().any(|part| is(part, "environ"))
            }
        }
    }
}

/// The names of a path with `.`, empty parts and `..` applied. A `..` at the root stays at the root.
///
/// For [`Style::Windows`] the verbatim prefix and the drive letter are dropped, a name ends at its first `:`, and
/// trailing dots and spaces are not part of a name, because Windows ignores them when it opens the file.
fn components(style: Style, path: &str) -> Vec<String> {
    let windows = style == Style::Windows;
    let mut text = path;
    if windows {
        for prefix in [r"\\?\", r"\\.\", "//?/", "//./"] {
            if let Some(rest) = text.strip_prefix(prefix) {
                text = rest;
                break;
            }
        }
    }
    let mut out: Vec<String> = Vec::new();
    for (index, raw) in text.split(|c| c == '/' || (windows && c == '\\')).enumerate() {
        let mut part = raw;
        if windows && index == 0 {
            let bytes = part.as_bytes();
            if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
                part = &part[2..];
            }
        }
        match part {
            "" | "." => continue,
            ".." => {
                out.pop();
                continue;
            }
            _ => {}
        }
        if windows {
            let name = part.split(':').next().unwrap_or("").trim_end_matches(['.', ' ']);
            if !name.is_empty() {
                out.push(name.to_owned());
            }
        } else {
            out.push(part.to_owned());
        }
    }
    out
}

/// A step of a path that is being resolved.
enum Part {
    Prefix(OsString),
    Root,
    Parent,
    Name(OsString),
}

fn parts_of(path: &Path) -> VecDeque<Part> {
    path.components()
        .filter_map(|component| match component {
            Component::Prefix(prefix) => Some(Part::Prefix(prefix.as_os_str().to_owned())),
            Component::RootDir => Some(Part::Root),
            Component::CurDir => None,
            Component::ParentDir => Some(Part::Parent),
            Component::Normal(name) => Some(Part::Name(name.to_owned())),
        })
        .collect()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// The path with every symbolic link resolved, also when the last parts of it do not exist yet.
///
/// The longest existing prefix is canonicalised. A missing name is checked once more: when it is a symbolic link, which
/// dangles, its target is followed (at most 40 links), otherwise the name is kept as it is. A `..` after a missing name
/// cannot be resolved and is an error, because the file system has not said what the name stands for.
pub fn canonical_local(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(invalid("the path is not absolute"));
    }
    match std::fs::canonicalize(path) {
        Ok(resolved) => return Ok(resolved),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut work = parts_of(path);
    let mut current = PathBuf::new();
    let mut missing = false;
    let mut hops = 0;
    while let Some(part) = work.pop_front() {
        match part {
            Part::Prefix(prefix) => current = PathBuf::from(prefix),
            Part::Root => {
                current.push(Component::RootDir.as_os_str());
                current = std::fs::canonicalize(&current)?;
                missing = false;
            }
            Part::Parent => {
                if missing {
                    return Err(invalid("a missing name is followed by `..`"));
                }
                current.pop();
            }
            Part::Name(name) => {
                let next = current.join(&name);
                if missing {
                    current = next;
                    continue;
                }
                match std::fs::symlink_metadata(&next) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        hops += 1;
                        if hops > MAX_LINK_HOPS {
                            return Err(invalid("too many symbolic links"));
                        }
                        let target = std::fs::read_link(&next)?;
                        for step in parts_of(&target).into_iter().rev() {
                            work.push_front(step);
                        }
                    }
                    Ok(_) => current = std::fs::canonicalize(&next)?,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        missing = true;
                        current = next;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
    Ok(current)
}
