//! The detector of the integrations' programs, on injected facts: the order of the places, the three answers, the
//! Windows extension rules, the login-shell probe and its parser against the recorded captures, the `PATH` of a child
//! and the refusals that keep a hijacked directory from being trusted.
//!
//! Nothing here reads the machine: the environment, the files, the modes and the login shell's `PATH` are a [`World`]
//! built by each test, so every rule is proven on any host for any [`Os`].

use std::cell::Cell;
use std::collections::HashMap;

use holzwave_core::integrate::detect::{
    detect, login_probe, parse_login_path, Detection, Facts, Found, Place, UnusableReason, LOGIN_PROBE_BEGIN,
    LOGIN_PROBE_END, LOGIN_PROBE_SCRIPT, LOGIN_PROBE_TIMEOUT,
};
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

    // Nothing anywhere: the answer lists the absolute directories that were looked in, in order, each once; the fixed
    // directories of the tool come after the PATH.
    let world = World::new(Os::Linux).var("PATH", "/usr/bin:relative/bin::/usr/bin:/opt/tools/bin");
    match world.detect(&toy(), None) {
        Detection::NotFound { searched } => {
            assert_eq!(searched, ["/usr/bin", "/opt/tools/bin", "/opt/homebrew/bin", "/usr/local/bin"])
        }
        other => panic!("expected not found, got {other:?}"),
    }
}

fn not_found(detection: Detection) -> Vec<String> {
    match detection {
        Detection::NotFound { searched } => searched,
        other => panic!("expected not found, got {other:?}"),
    }
}

fn unusable(detection: Detection) -> (String, UnusableReason) {
    match detection {
        Detection::Unusable { path, reason } => (path, reason),
        other => panic!("expected an unusable path, got {other:?}"),
    }
}

// ---- the login-shell probe: its command and its parser ---------------------------------------------------------------

fn shell_exists(path: &str) -> bool {
    matches!(path, "/bin/zsh" | "/bin/bash" | "/usr/bin/fish" | "/bin/sh" | "/usr/bin/dash" | "/bin/ksh" | "/usr/bin/nu")
}

#[test]
fn the_login_probe_is_built_only_for_a_known_shell() {
    let probe = login_probe(Os::Macos, Some("/bin/zsh"), &shell_exists).expect("zsh is a known shell");
    assert_eq!(probe.program, "/bin/zsh");
    assert_eq!(probe.args, ["-l", "-c", LOGIN_PROBE_SCRIPT]);

    // The script is one constant: the two marker lines around `command printenv PATH`, each printed with printf.
    assert_eq!(
        LOGIN_PROBE_SCRIPT,
        format!("printf '%s\\n' {LOGIN_PROBE_BEGIN}; command printenv PATH; printf '%s\\n' {LOGIN_PROBE_END}")
    );
    assert_eq!(LOGIN_PROBE_TIMEOUT.as_secs(), 3);

    for (shell, os) in [
        ("/bin/bash", Os::Linux),
        ("/usr/bin/fish", Os::Macos),
        ("/bin/sh", Os::Linux),
        ("/usr/bin/dash", Os::Linux),
        ("/bin/ksh", Os::Macos),
    ] {
        assert!(login_probe(os, Some(shell), &shell_exists).is_some(), "{shell} is probed");
    }
    assert!(login_probe(Os::Macos, Some("zsh"), &shell_exists).is_none(), "a name that is not a path");
    assert!(login_probe(Os::Macos, Some("./zsh"), &|_| true).is_none(), "a relative path");
    assert!(login_probe(Os::Macos, Some("/usr/bin/nu"), &shell_exists).is_none(), "a shell that is not on the list");
    assert!(login_probe(Os::Linux, Some("/tmp/evil/curl"), &|_| true).is_none(), "an attacker's program");
    assert!(login_probe(Os::Linux, Some("/bin/zsh/"), &|_| true).is_none(), "a directory-like path has no file name");
    assert!(login_probe(Os::Linux, Some("/bin/zsh\n-x"), &|_| true).is_none(), "a control character");
    assert!(login_probe(Os::Linux, Some(""), &|_| true).is_none(), "an empty value");
    assert!(login_probe(Os::Macos, None, &shell_exists).is_none(), "no $SHELL");
    assert!(login_probe(Os::Macos, Some("/bin/zsh"), &|_| false).is_none(), "a shell that is not there");
    assert!(login_probe(Os::Windows, Some("/bin/zsh"), &|_| true).is_none(), "never on Windows");
    assert!(login_probe(Os::Windows, Some(r"C:\msys\bin\bash.exe"), &|_| true).is_none(), "never on Windows");
}

/// A directory exists in the fixtures unless its name holds the word `missing`.
fn fixture_exists(path: &str) -> bool {
    !path.contains("missing")
}

#[test]
fn every_capture_of_the_probe_gives_its_expected_list() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/path_probe");
    let mut checked = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the capture directory").flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("txt") || path.file_name().is_some_and(|n| n == "SOURCES.md") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let capture = std::fs::read(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let expected_text = std::fs::read_to_string(path.with_extension("expected"))
            .unwrap_or_else(|e| panic!("{name} has no expected list: {e}"));
        let expected: Vec<&str> = expected_text.lines().collect();
        let got = parse_login_path(Os::Macos, &capture, &fixture_exists);
        assert_eq!(got, expected, "the capture {name} gives another list than its expected file");
        checked.push(name);
    }
    assert!(checked.len() >= 7, "the captures of the probe: {checked:?}");
}

#[test]
fn the_parser_takes_one_line_between_the_last_begin_marker_and_the_next_end_marker() {
    let parse = |text: &str| parse_login_path(Os::Linux, text.as_bytes(), &fixture_exists);
    let b = LOGIN_PROBE_BEGIN;
    let e = LOGIN_PROBE_END;
    assert!(parse("").is_empty(), "nothing");
    assert!(parse("/usr/bin:/bin\n").is_empty(), "a value without markers is not trusted");
    assert!(parse(&format!("{b}\n/usr/bin\n")).is_empty(), "a begin marker without its end marker");
    assert!(parse(&format!("/usr/bin\n{e}\n")).is_empty(), "an end marker without its begin marker");
    assert_eq!(parse(&format!("{b}\n/usr/bin:/bin\n{e}\n")), ["/usr/bin", "/bin"]);
    assert_eq!(parse(&format!("{b}\r\n/usr/bin\r\n{e}\r\n")), ["/usr/bin"], "a carriage return before the newline");
    assert_eq!(
        parse(&format!("{b}\n/fake\n{b}\n/usr/bin\n{e}\n")),
        ["/usr/bin"],
        "a banner that repeats the begin marker: the last one wins"
    );
    assert!(parse(&format!("{b}\n/usr/bin\n/bin\n{e}\n")).is_empty(), "two lines are not one value");
    assert!(parse(&format!("{b}\n{e}\n")).is_empty(), "no line is not one value");
    assert!(parse(&format!("x {b}\n/usr/bin\n{e}\n")).is_empty(), "a marker is a line of its own");
    assert_eq!(parse(&format!("{b}\n/usr/bin\n{e}\n/late\n{e}\n")), ["/usr/bin"], "the first end marker closes it");
    assert_eq!(
        parse(&format!("{b}\n/usr/bin:/usr/bin:/bin:/missing1:bin:.::/a\u{7}b:/c\n{e}\n")),
        ["/usr/bin", "/bin", "/c"],
        "duplicates, a missing directory, relative and empty entries and a control character are dropped"
    );
    let many: Vec<String> = (0..100).map(|n| format!("/d/{n}")).collect();
    assert_eq!(parse(&format!("{b}\n{}\n{e}\n", many.join(":"))).len(), 64, "at most 64 directories");
    assert_eq!(parse_login_path(Os::Linux, &[0xff, 0xfe, b'\n'], &|_| true), Vec::<String>::new(), "bytes that are not text");
}

// ---- the set path, the fixed directories, the lazy login shell --------------------------------------------------------

#[test]
fn a_set_path_wins_or_is_unusable() {
    let world = World::new(Os::Linux)
        .var("PATH", "/usr/bin:/opt/tools/bin")
        .exe("/opt/tools/bin/kubectl")
        .exe("/srv/mine/kubectl")
        .file("/srv/dir", ProgramCheck::NotAFile)
        .file("/srv/plain/kubectl", ProgramCheck::NotExecutable);
    let answer = found(world.detect(&toy(), Some("/srv/mine/kubectl")));
    assert_eq!(answer.place, Place::Override);
    assert_eq!(answer.path, "/srv/mine/kubectl");
    assert_eq!(answer.dir, "/srv/mine");
    assert_eq!(world.login_calls.get(), 0, "a set path ends the search");

    // The set path wins over a tool on the PATH, and a bad one does not fall back to the PATH silently.
    for (set, reason) in [
        ("kubectl", UnusableReason::NotAbsolute),
        ("./kubectl", UnusableReason::NotAbsolute),
        ("bin/kubectl", UnusableReason::NotAbsolute),
        ("~/kubectl", UnusableReason::NotAbsolute),
        ("/srv/mine/kubectl\nx", UnusableReason::NotAbsolute),
        ("/srv/absent/kubectl", UnusableReason::Missing),
        ("/srv/dir", UnusableReason::NotAFile),
        ("/srv/plain/kubectl", UnusableReason::NotExecutable),
    ] {
        let (path, got) = unusable(world.detect(&toy(), Some(set)));
        assert_eq!((path.as_str(), got), (set, reason), "the set path {set:?}");
    }
    assert_eq!(world.login_calls.get(), 0, "the login shell is not asked about a bad set path either");

    // An empty field is no path at all.
    assert_eq!(found(world.detect(&toy(), Some(""))).place, Place::Path);
}

#[test]
fn fixed_directories_come_last_and_expand_the_home() {
    let world = World::new(Os::Macos).var("PATH", "/usr/bin:/bin").home("/Users/me");
    // Nothing found: the fixed directories are listed after the PATH, `$HOME/` expanded, an unknown variable and the
    // entries of another system left out.
    assert_eq!(
        not_found(world.detect(&toy(), None)),
        ["/usr/bin", "/bin", "/opt/homebrew/bin", "/usr/local/bin", "/Users/me/bin"]
    );
    let no_home = World::new(Os::Macos).var("PATH", "/usr/bin");
    assert_eq!(not_found(no_home.detect(&toy(), None)), ["/usr/bin", "/opt/homebrew/bin", "/usr/local/bin"], "no home, no $HOME/");

    let world = world.exe("/Users/me/bin/kubectl");
    let answer = found(world.detect(&toy(), None));
    assert_eq!((answer.place, answer.path.as_str(), answer.dir.as_str()), (Place::FixedDir, "/Users/me/bin/kubectl", "/Users/me/bin"));

    // The fixed list is in its own order, and a directory of the login shell's PATH comes before it.
    let world = world.exe("/usr/local/bin/kubectl");
    assert_eq!(found(world.detect(&toy(), None)).path, "/usr/local/bin/kubectl");
    let world = world.login(&["/Users/me/tools"]).exe("/Users/me/tools/kubectl");
    let answer = found(world.detect(&toy(), None));
    assert_eq!((answer.place, answer.path.as_str()), (Place::LoginPath, "/Users/me/tools/kubectl"));

    // A home with a trailing slash does not double it.
    let world = World::new(Os::Linux).home("/home/me/").exe("/home/me/bin/kubectl");
    assert_eq!(found(world.detect(&toy(), None)).path, "/home/me/bin/kubectl");
}

#[test]
fn the_login_path_is_asked_for_only_when_needed() {
    // The process PATH has it: not asked.
    let world = World::new(Os::Macos).var("PATH", "/usr/bin:/opt/x").exe("/opt/x/kubectl").login(&["/opt/y"]);
    assert_eq!(found(world.detect(&toy(), None)).place, Place::Path);
    assert_eq!(world.login_calls.get(), 0);

    // The PATH has not: asked once, even though several directories and names are searched.
    let world = World::new(Os::Macos).var("PATH", "/usr/bin").exe("/opt/y/kubectl").login(&["/opt/z", "/opt/y"]);
    let two = spec_with(&["kubectl", "kubectl2"], &["kubectl.exe"]);
    assert_eq!(found(world.detect(&two, None)).place, Place::LoginPath);
    assert_eq!(world.login_calls.get(), 1, "asked once");

    // Nothing anywhere: asked once, and its directories are listed once each, after the PATH, without repeating one.
    let world = World::new(Os::Macos).var("PATH", "/usr/bin").login(&["/usr/bin", "/opt/z", "rel", "", "/opt/z"]);
    assert_eq!(
        not_found(world.detect(&toy(), None)),
        ["/usr/bin", "/opt/z", "/opt/homebrew/bin", "/usr/local/bin"]
    );
    assert_eq!(world.login_calls.get(), 1);

    // Never on Windows: nothing is probed there.
    let world = World::new(Os::Windows).var("PATH", r"C:\Tools").login(&[r"C:\Probe"]).exe(r"C:\Probe\kubectl.exe");
    assert!(matches!(world.detect(&toy(), None), Detection::NotFound { .. }));
    assert_eq!(world.login_calls.get(), 0);

    // A hostile answer is filtered again by the detector: relative entries are not searched.
    let world = World::new(Os::Linux).login(&["relative/bin", "."]).exe("relative/bin/kubectl").exe("./kubectl");
    assert!(matches!(world.detect(&toy(), None), Detection::NotFound { .. }));
}

// ---- Windows ----------------------------------------------------------------------------------------------------------

#[test]
fn windows_tries_pathext_in_order_and_marks_batch_files() {
    let az = spec_with(&["az"], &["az"]);
    let base = || World::new(Os::Windows).var("PATH", r"C:\Tools;C:\Other").var("PATHEXT", ".COM;.EXE;.BAT;.CMD");

    // The extensions are tried in the order of PATHEXT, in each directory before the next.
    let world = base().exe(r"C:\Tools\az.cmd").exe(r"C:\Tools\az.bat").exe(r"C:\Other\az.exe");
    let answer = found(world.detect(&az, None));
    assert_eq!((answer.path.as_str(), answer.batch), (r"C:\Tools\az.bat", true), ".BAT comes before .CMD");
    let world = base().exe(r"C:\Tools\az.cmd").exe(r"C:\Tools\az.exe");
    assert_eq!(found(world.detect(&az, None)).path, r"C:\Tools\az.exe", ".EXE comes before .CMD");
    assert!(!found(world.detect(&az, None)).batch, "an executable is not a batch file");
    let world = base().exe(r"C:\Tools\az.com").exe(r"C:\Tools\az.exe");
    assert_eq!(found(world.detect(&az, None)).path, r"C:\Tools\az.com", "the order is the one of PATHEXT");

    // The default order when PATHEXT is not set: .exe, .cmd, .bat, .com.
    let world = World::new(Os::Windows).var("PATH", r"C:\Tools").exe(r"C:\Tools\az.com").exe(r"C:\Tools\az.bat");
    assert_eq!(found(world.detect(&az, None)).path, r"C:\Tools\az.bat");
    let world = World::new(Os::Windows).var("PATH", r"C:\Tools").var("PATHEXT", "").exe(r"C:\Tools\az.cmd").exe(r"C:\Tools\az.exe");
    assert_eq!(found(world.detect(&az, None)).path, r"C:\Tools\az.exe", "an empty PATHEXT is the default");

    // `az.cmd` found by its name and marked as a batch file; other extensions of PATHEXT are not programs.
    let world = base().exe(r"C:\Tools\AZ.CMD");
    let answer = found(world.detect(&az, None));
    assert_eq!((answer.path.as_str(), answer.dir.as_str(), answer.batch, answer.place), (r"C:\Tools\az.cmd", r"C:\Tools", true, Place::Path));
    let world = World::new(Os::Windows)
        .var("PATH", r"C:\Tools")
        .var("PATHEXT", ".VBS;.JS;.CMD")
        .exe(r"C:\Tools\az.vbs")
        .exe(r"C:\Tools\az.js");
    assert!(matches!(world.detect(&az, None), Detection::NotFound { .. }), "a script host extension is never tried");

    // A name that already has an extension is tried as it is; case does not matter.
    let world = base().exe(r"C:\Tools\KUBECTL.EXE");
    let answer = found(world.detect(&toy(), None));
    assert_eq!((answer.path.as_str(), answer.batch), (r"C:\Tools\kubectl.exe", false));
    let world = base().exe(r"C:\Tools\kubectl.exe.cmd");
    assert!(matches!(world.detect(&toy(), None), Detection::NotFound { .. }), "no extension is added to a name that has one");
    let world = base().exe(r"C:\Tools\az.cmd");
    assert!(
        matches!(world.detect(&spec_with(&["az"], &["az.cmd"]), None), Detection::Found(Found { batch: true, .. })),
        "a batch file named with its extension"
    );

    // The same directory spelled twice in another case is searched once.
    let world = World::new(Os::Windows).var("PATH", r"C:\Tools;c:\tools\;C:\TOOLS");
    assert_eq!(not_found(world.detect(&az, None))[0], r"C:\Tools");
    assert_eq!(not_found(world.detect(&az, None)).len(), 1);

    // The fixed directories expand %NAME% from the environment and `$HOME/` from the home; a Unix entry is left out.
    let world = World::new(Os::Windows)
        .var("PATH", r"C:\Tools")
        .var("LOCALAPPDATA", r"C:\Users\me\AppData\Local")
        .exe(r"C:\Users\me\AppData\Local\Links\kubectl.exe");
    let answer = found(world.detect(&toy(), None));
    assert_eq!((answer.place, answer.path.as_str()), (Place::FixedDir, r"C:\Users\me\AppData\Local\Links\kubectl.exe"));
    let world = World::new(Os::Windows).home(r"C:\Users\me").exe(r"C:\Users\me\bin\kubectl.exe");
    assert_eq!(found(world.detect(&toy(), None)).path, r"C:\Users\me\bin\kubectl.exe");
    let listed = not_found(World::new(Os::Windows).detect(&toy(), None));
    assert_eq!(listed, [r"C:\Tools"], "the entries of another system and an unset variable are left out: {listed:?}");
}

#[test]
fn windows_refuses_unc_device_and_relative_entries() {
    let az = spec_with(&["az"], &["az.cmd", "az.exe"]);
    let path = [r"\\server\share", r"\\?\C:\x", r"//server/share", r"\\.\pipe\x", "rel", r"rel\bin", ".", "", r"\bin", r"D:bin", r"C:\Ok"]
        .join(";");
    let world = World::new(Os::Windows)
        .var("PATH", &path)
        .exe(r"\\server\share\az.cmd")
        .exe(r"\\?\C:\x\az.cmd")
        .exe(r"//server/share/az.cmd")
        .exe(r"\\.\pipe\x\az.cmd")
        .exe(r"rel\az.cmd")
        .exe(r"rel\bin\az.cmd")
        .exe(r".\az.cmd")
        .exe(r"\bin\az.cmd")
        .exe(r"D:bin\az.cmd");
    assert_eq!(
        not_found(world.detect(&az, None)),
        [r"C:\Ok", r"C:\Tools"],
        "only the absolute drive directory of the PATH is searched, then the fixed one"
    );

    // A set path of that form is unusable, whatever is there.
    for set in [r"\\server\share\az.cmd", r"\\?\C:\x\az.cmd", r"//server/share/az.cmd", r"\\.\pipe\az.exe"] {
        let (shown, reason) = unusable(world.detect(&az, Some(set)));
        assert_eq!((shown.as_str(), reason), (set, UnusableReason::NetworkPath), "{set}");
    }
    for set in ["az.cmd", r".\az.cmd", r"rel\az.cmd", r"\bin\az.cmd", r"D:az.cmd"] {
        let (_, reason) = unusable(world.detect(&az, Some(set)));
        assert_eq!(reason, UnusableReason::NotAbsolute, "{set}");
    }
    let world = world.exe(r"C:\Ok\az.cmd");
    let answer = found(world.detect(&az, Some(r"C:\Ok\az.cmd")));
    assert_eq!((answer.place, answer.batch), (Place::Override, true), "a drive path that is there");

    // Fixed directories do not reach a network path either: an environment variable that holds one is left out.
    let world = World::new(Os::Windows).var("LOCALAPPDATA", r"\\server\share").exe(r"\\server\share\Links\kubectl.exe");
    assert!(matches!(world.detect(&toy(), None), Detection::NotFound { .. }));
}

#[test]
fn a_relative_or_empty_path_entry_is_never_searched_on_any_system() {
    for os in [Os::Macos, Os::Linux] {
        let world = World::new(os)
            .var("PATH", ".:..:bin::./bin:~/bin:$HOME/bin")
            .exe("./kubectl")
            .exe("../kubectl")
            .exe("bin/kubectl")
            .exe("kubectl")
            .exe("/kubectl")
            .exe("~/bin/kubectl");
        assert_eq!(not_found(world.detect(&toy(), None)), ["/opt/homebrew/bin", "/usr/local/bin"], "{os:?}: only the fixed directories");
    }
}
