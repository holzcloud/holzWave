//! The detector of the integrations' programs, on injected facts: the order of the places, the three answers, the
//! Windows extension rules, the login-shell probe and its parser against the recorded captures, the `PATH` of a child
//! and the refusals that keep a hijacked directory from being trusted.
//!
//! Nothing here reads the machine: the environment, the files, the modes and the login shell's `PATH` are a [`World`]
//! built by each test, so every rule is proven on any host for any [`Os`].

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use holzwave_core::integrate::detect::{detect, Detection, Facts, Found, Place};
use holzwave_core::integrate::{IntegrationSpec, SelectKind, ToolId, VersionProbe};
use holzwave_core::platform::{Os, ProgramCheck};

/// A version parser that never finds one.
fn no_version(_: &str) -> Option<String> {
    None
}

/// A toy tool with the shape of a real one: a Unix name, a Windows file name and the fixed directories the macOS and
/// Windows lists have.
fn spec_with(binaries: &'static [&'static str], windows: &'static [&'static str]) -> IntegrationSpec {
    IntegrationSpec {
        id: ToolId::Kube,
        label: "Toy",
        binaries,
        windows_binaries: windows,
        fixed_dirs: &["/opt/homebrew/bin", "/usr/local/bin", "$HOME/bin", "$NOPE/bin", "%LOCALAPPDATA%\\Links", "C:\\Tools"],
        version_probe: VersionProbe { argv: &["version"], parse: no_version },
        env_set: &[],
        selects: &[SelectKind::Context],
        verbs: &[],
    }
}

fn toy() -> IntegrationSpec {
    spec_with(&["kubectl"], &["kubectl.exe"])
}

/// The machine a test describes: variables, files that exist (with what kind of file), and the login shell's `PATH`.
struct World {
    os: Os,
    env: HashMap<String, String>,
    files: HashMap<String, ProgramCheck>,
    home: Option<String>,
    login: Vec<String>,
    login_calls: Cell<usize>,
}

impl World {
    fn new(os: Os) -> Self {
        Self { os, env: HashMap::new(), files: HashMap::new(), home: None, login: Vec::new(), login_calls: Cell::new(0) }
    }

    fn var(mut self, key: &str, value: &str) -> Self {
        self.env.insert(key.to_string(), value.to_string());
        self
    }

    /// An executable file. On Windows the file system ignores case, so the key is lower case there.
    fn exe(self, path: &str) -> Self {
        self.file(path, ProgramCheck::Usable)
    }

    fn file(mut self, path: &str, kind: ProgramCheck) -> Self {
        let key = self.key(path);
        self.files.insert(key, kind);
        self
    }

    fn key(&self, path: &str) -> String {
        if self.os == Os::Windows { path.to_ascii_lowercase() } else { path.to_string() }
    }

    fn home(mut self, home: &str) -> Self {
        self.home = Some(home.to_string());
        self
    }

    fn login(mut self, dirs: &[&str]) -> Self {
        self.login = dirs.iter().map(|d| d.to_string()).collect();
        self
    }

    fn detect(&self, spec: &IntegrationSpec, override_path: Option<&str>) -> Detection {
        let env = |key: &str| self.env.get(key).cloned();
        let check = |path: &str| self.files.get(&self.key(path)).copied().unwrap_or(ProgramCheck::Missing);
        let login = || {
            self.login_calls.set(self.login_calls.get() + 1);
            self.login.clone()
        };
        let facts = Facts { os: self.os, env: &env, check: &check, home: self.home.as_deref(), login_path: &login };
        detect(spec, &facts, override_path)
    }
}

fn found(detection: Detection) -> Found {
    match detection {
        Detection::Found(found) => found,
        other => panic!("expected a found tool, got {other:?}"),
    }
}

#[test]
fn the_first_place_that_has_the_tool_wins_in_the_documented_order() {
    // The process PATH: a relative entry and an empty entry are never searched, the first directory with the file wins.
    let world = World::new(Os::Linux)
        .var("PATH", "/usr/bin:/opt/tools/bin:relative/bin:")
        .exe("/opt/tools/bin/kubectl")
        .exe("relative/bin/kubectl");
    let answer = found(world.detect(&toy(), None));
    assert_eq!(
        answer,
        Found {
            path: "/opt/tools/bin/kubectl".into(),
            dir: "/opt/tools/bin".into(),
            place: Place::Path,
            batch: false,
            version: None
        }
    );
    assert_eq!(world.login_calls.get(), 0, "the login shell is not asked when the process PATH found the tool");

    // Nothing anywhere: the answer lists the absolute directories that were looked in, in order, each once.
    let world = World::new(Os::Linux).var("PATH", "/usr/bin:relative/bin::/usr/bin:/opt/tools/bin");
    match world.detect(&toy(), None) {
        Detection::NotFound { searched } => assert_eq!(searched, ["/usr/bin", "/opt/tools/bin"]),
        other => panic!("expected not found, got {other:?}"),
    }
}
