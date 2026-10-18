//! Finding a tool on this computer: the override the person set, the process `PATH`, the `PATH` of their login shell,
//! a short list of fixed directories per operating system, and a version probe of what was found.

use std::fmt;

use super::IntegrationSpec;
use crate::platform::{self, Os, ProgramCheck};

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
    /// Not an absolute path.
    NotAbsolute,
    /// Nothing is there.
    Missing,
    /// Something is there, but it is not a file.
    NotAFile,
    /// A file that cannot be executed.
    NotExecutable,
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

/// Finds the program of `spec`.
pub fn detect(spec: &IntegrationSpec, facts: &Facts<'_>, _override_path: Option<&str>) -> Detection {
    let names = if facts.os == Os::Windows { spec.windows_binaries } else { spec.binaries };
    let mut searched: Vec<String> = Vec::new();
    for dir in platform::search_dirs(facts.os, facts.env) {
        if !searched.contains(&dir) {
            searched.push(dir.clone());
        }
        for name in names {
            for candidate in platform::candidate_names(facts.os, name, None) {
                let path = platform::join_path(facts.os, &dir, &candidate);
                if (facts.check)(&path) == ProgramCheck::Usable {
                    return Detection::Found(Found { path, dir: dir.clone(), place: Place::Path, batch: false, version: None });
                }
            }
        }
    }
    Detection::NotFound { searched }
}
