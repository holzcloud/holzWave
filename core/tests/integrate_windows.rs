//! The Windows rules of the native integrations, run against the real Windows: real files in a scratch directory, the
//! real detector with the facts of this machine's environment, the real runner with batch-file fakes, the real overlay
//! writer and a real ConPTY pane. This is the checklist of the owner's Windows rows; the same rules are also proven on
//! any host with injected facts (`integrate_detect`, `integrate_argv`, `integrate_kube`), but only a Windows machine can
//! show that the real batch interpreter, the real `PATHEXT`, the real path rules and the real pseudo console agree.
//!
//! The owner runs it on Windows with
//!
//! ```text
//! cargo test -p holzwave-core --test integrate_windows -- --nocapture
//! ```
//!
//! and writes into the Result column of the matching row of `docs/test-plan.md` how many of the tests passed, the Windows
//! build and, for a failure, the line the test printed. On macOS and Linux the file is empty (`running 0 tests`);
//! `sh scripts/cross-check.sh core` compiles it for `x86_64-pc-windows-gnu`, which is the only proof that can be had
//! without Windows. That compile shows that the file builds, not that any test passes.
//!
//! | Test                                                                      | Row    | What a pass means                                              |
//! |---------------------------------------------------------------------------|--------|----------------------------------------------------------------|
//! | `a_cmd_in_a_path_directory_is_found_through_pathext_and_marked_batch`     | INT-W1 | a `.cmd` file in a `PATH` directory is the tool, marked batch  |
//! | `gam_exe_in_a_fixed_directory_is_found`                                   | INT-W1 | the `%LOCALAPPDATA%\GAM7` entry of the fixed list is searched  |
//! | `an_az_cmd_runs_with_safe_arguments_and_refuses_unsafe_ones_before_a_process` | INT-W2 | the batch charset holds with the real command interpreter  |
//! | `an_exec_launch_runs_in_a_conpty_pane_and_is_held_after_a_failure`        | INT-W3 | a failing tool stays on screen until Enter, under ConPTY       |
//! | `the_overlay_kubeconfig_value_uses_semicolons_and_refuses_unc_and_device_paths` | INT-W4 | `;` joins the list, UNC and device paths are never used  |
//!
//! Every test uses its own scratch directory and ignores every tool that is installed on the machine: the detector is
//! asked about files inside the scratch directory only, so a real `az` or `gam` on the owner's computer changes nothing.
//! Every wait is bounded and every pane is closed at the end of its test.
#![cfg(windows)]

use std::cell::Cell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use holzwave_core::integrate::detect::{child_path, detect, Detection, Facts, Place, UnusableReason};
use holzwave_core::integrate::kube::{kubeconfig_value, write_overlay, ContextEntry, OverlayError};
use holzwave_core::integrate::run::{
    EnvExtra, EnvPolicy, RunCancel, RunEnd, RunError, RunLimits, Runner,
};
use holzwave_core::integrate::{azure, gam, IntegrationSpec, ToolId, VersionProbe};
use holzwave_core::platform::{Os, ProgramCheck};

/// A scratch directory that is removed when the test ends, even when it fails.
struct Scratch(PathBuf);

impl Scratch {
    /// A fresh, empty directory `holzwave-{tag}-{pid}-{n}` in the temporary directory. It is made with `create_dir`, so
    /// a path that exists already fails the test instead of being used.
    fn new(tag: &str) -> Scratch {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("holzwave-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir(&path).unwrap_or_else(|e| panic!("scratch dir {} must not exist yet: {e}", path.display()));
        Scratch(path)
    }

    /// A directory below the scratch directory.
    fn dir(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir(&path).unwrap_or_else(|e| panic!("{} was not created: {e}", path.display()));
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The path as text, which every path of a scratch directory is on Windows (the temporary directory of an account is
/// Unicode, and a name outside Unicode cannot be handed to the detector at all).
fn text(path: &Path) -> String {
    path.to_str().expect("the scratch path is Unicode").to_owned()
}

/// Whether `path` is inside `root`, without regard to case or to the direction of the separators.
fn inside(root: &Path, path: &str) -> bool {
    let plain = |value: &str| value.replace('/', "\\").to_ascii_lowercase();
    let root = plain(&text(root));
    plain(path).starts_with(&format!("{}\\", root.trim_end_matches('\\')))
}

/// A tool that does nothing to find a version.
fn no_version(_: &str) -> Option<String> {
    None
}

/// A tool whose Windows name has no dot, so that the extensions of `PATHEXT` decide what is searched (every shipped tool
/// names its Windows file with an extension, so none of them reaches `PATHEXT`).
fn dotless() -> IntegrationSpec {
    IntegrationSpec {
        id: ToolId::Kube,
        label: "Toy",
        binaries: &["kubectl"],
        windows_binaries: &["kubectl"],
        fixed_dirs: &[],
        version_probe: VersionProbe { argv: &["version"], parse: no_version },
        env_set: &[],
        selects: &[],
        verbs: &[],
    }
}

/// Asks the real detector about this machine's environment, with the answers for `overrides` replaced and every file
/// outside `root` reported as missing. Answers the detection and how many files the detector looked at.
fn detect_in(
    spec: &IntegrationSpec,
    root: &Path,
    overrides: &[(&str, &str)],
    override_path: Option<&str>,
) -> (Detection, usize) {
    let looked = Cell::new(0usize);
    let env = |name: &str| -> Option<String> {
        match overrides.iter().find(|(own, _)| own.eq_ignore_ascii_case(name)) {
            Some((_, value)) => Some((*value).to_owned()),
            None => std::env::var(name).ok(),
        }
    };
    let check = |path: &str| -> ProgramCheck {
        looked.set(looked.get() + 1);
        if !inside(root, path) {
            return ProgramCheck::Missing;
        }
        let Ok(meta) = std::fs::metadata(path) else { return ProgramCheck::Missing };
        if !meta.is_file() {
            return ProgramCheck::NotAFile;
        }
        let lower = path.to_ascii_lowercase();
        if [".exe", ".cmd", ".bat", ".com"].iter().any(|ext| lower.ends_with(ext)) {
            ProgramCheck::Usable
        } else {
            ProgramCheck::NotExecutable
        }
    };
    let home = text(root);
    let login = || -> Vec<String> { Vec::new() };
    let mode = |_: &str| -> Option<u32> { None };
    let facts = Facts {
        os: Os::Windows,
        env: &env,
        check: &check,
        home: Some(home.as_str()),
        login_path: &login,
        mode_of: &mode,
    };
    let found = detect(spec, &facts, override_path);
    (found, looked.get())
}

// ---- INT-W1: the detector ---------------------------------------------------------------------------------------

/// Row INT-W1. A pass: a file `kubectl.cmd` in a directory of `PATH` is found as the tool through the real `PATHEXT` of
/// this machine and is marked as a batch file (the runner then applies the strict argument charset); the shipped Azure
/// description finds `az.cmd` the same way, and a network entry in `PATH` is never looked into.
#[test]
fn a_cmd_in_a_path_directory_is_found_through_pathext_and_marked_batch() {
    let root = Scratch::new("integrate_windows_pathext");
    let bin = root.dir("bin");
    std::fs::write(bin.join("kubectl.cmd"), "@echo off\r\necho fake\r\n").unwrap();
    let pathext = std::env::var("PATHEXT").unwrap_or_default().to_ascii_lowercase();
    assert!(pathext.split(';').any(|ext| ext == ".cmd"), "this machine's PATHEXT has no .CMD: {pathext:?}");

    // A network entry first: it is dropped before any file is looked at.
    let path = format!(r"\\no-such-host.invalid\share;{}", text(&bin));
    let (detection, _) = detect_in(&dotless(), &root.0, &[("PATH", &path)], None);
    let Detection::Found(found) = detection else { panic!("kubectl.cmd was not found: {detection:?}") };
    assert!(found.path.to_ascii_lowercase().ends_with(r"\kubectl.cmd"), "found {}", found.path);
    assert!(inside(&root.0, &found.path));
    assert!(found.batch, "a .cmd file is a batch file");
    assert_eq!(found.place, Place::Path);
    assert!(found.dir.eq_ignore_ascii_case(&text(&bin)), "the directory is {:?}", found.dir);

    // The shipped description of Azure names `az.cmd` with its extension.
    let az_dir = root.dir("azbin");
    std::fs::write(az_dir.join("az.cmd"), "@echo off\r\n").unwrap();
    let az_path = text(&az_dir);
    let (detection, _) = detect_in(&azure::SPEC, &root.0, &[("PATH", &az_path)], None);
    let Detection::Found(found) = detection else { panic!("az.cmd was not found: {detection:?}") };
    assert!(found.batch);
    assert!(found.path.to_ascii_lowercase().ends_with(r"\az.cmd"), "found {}", found.path);

    // A directory of only network entries finds nothing and searches nothing.
    let nowhere = r"\\no-such-host.invalid\share;\\?\C:\Windows;\\.\pipe\x";
    let (detection, _) = detect_in(&dotless(), &root.0, &[("PATH", nowhere)], None);
    match detection {
        Detection::NotFound { searched } => {
            assert!(searched.iter().all(|dir| !dir.starts_with(r"\\")), "a network or device path was searched: {searched:?}")
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

/// Row INT-W1. A pass: `gam.exe` in `%LOCALAPPDATA%\GAM7`, an entry of the fixed list of the GAM description, is found
/// when the `PATH` has no `gam`. The file is empty: only the extension rule of the detector is shown, not a running GAM.
#[test]
fn gam_exe_in_a_fixed_directory_is_found() {
    let root = Scratch::new("integrate_windows_gam");
    let empty_path = root.dir("empty");
    let gam_dir = root.dir("GAM7");
    std::fs::write(gam_dir.join("gam.exe"), b"").unwrap();

    let local = text(&root.0);
    let (detection, _) =
        detect_in(&gam::SPEC, &root.0, &[("PATH", &text(&empty_path)), ("LOCALAPPDATA", &local)], None);
    let Detection::Found(found) = detection else { panic!("gam.exe was not found: {detection:?}") };
    let want = format!(r"{}\GAM7\gam.exe", local.trim_end_matches('\\'));
    assert!(found.path.eq_ignore_ascii_case(&want), "found {} and wanted {want}", found.path);
    assert_eq!(found.place, Place::FixedDir);
    assert!(!found.batch, "gam.exe is a program");

    // Without the file the answer is "not found" with the directories that were looked in.
    std::fs::remove_file(gam_dir.join("gam.exe")).unwrap();
    let (detection, _) =
        detect_in(&gam::SPEC, &root.0, &[("PATH", &text(&empty_path)), ("LOCALAPPDATA", &local)], None);
    assert!(matches!(detection, Detection::NotFound { .. }), "expected NotFound, got {detection:?}");
}

// ---- INT-W2: the batch charset ----------------------------------------------------------------------------------

/// A fake `az.cmd`: it prints a marker with its arguments and appends a line to `runs.log` next to it, so that a run
/// that happened is visible twice.
const FAKE_AZ: &str = "@echo off\r\necho hw-az-ran %*\r\n>>\"%~dp0runs.log\" echo ran\r\nexit /b 0\r\n";

fn lines_in(path: &Path) -> usize {
    std::fs::read_to_string(path).map(|text| text.lines().count()).unwrap_or(0)
}

/// Row INT-W2. A pass: with the real command interpreter a fake `az.cmd` runs with arguments inside the charset, and an
/// argument with `&`, `%`, `^`, `|`, `<`, `>`, a double quote, a line feed, a space or a semicolon (or an empty one) is
/// refused with the fixed sentence before a process exists: the log of the fake does not grow and nothing is printed.
#[test]
fn an_az_cmd_runs_with_safe_arguments_and_refuses_unsafe_ones_before_a_process() {
    let root = Scratch::new("integrate_windows_az");
    let bin = root.dir("bin");
    let az = bin.join("az.cmd");
    std::fs::write(&az, FAKE_AZ).unwrap();
    let log = bin.join("runs.log");

    let runner = Runner::new(true);
    let process_path = std::env::var("PATH").unwrap_or_default();
    let env = || {
        let path = child_path(Os::Windows, &text(&bin), &process_path, &[]);
        EnvPolicy::build(std::env::vars_os(), Os::Windows, path, EnvExtra::default()).expect("the environment builds")
    };
    let limits = RunLimits { timeout: std::time::Duration::from_secs(30), ..RunLimits::default() };
    let args = |list: &[&str]| -> Vec<OsString> { list.iter().map(OsString::from).collect() };

    // Inside the charset: [A-Za-z0-9._:/@=+,-].
    let safe = ["version", "-o", "json", "--subscription=00000000-1111-2222-3333-444444444444", "a.b_c:d/e@f=g+h,i-j"];
    let result = runner
        .run_argv(&az, &args(&safe), env(), &limits, &RunCancel::default())
        .unwrap_or_else(|e| panic!("a safe argument list was refused: {e}"));
    let out = String::from_utf8_lossy(&result.stdout).replace('"', "");
    assert_eq!(result.end, RunEnd::Finished, "stdout {out:?} stderr {:?}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(result.status, Some(0));
    assert!(out.contains("hw-az-ran"), "the marker is missing: {out:?}");
    for arg in safe {
        assert!(out.contains(arg), "{arg:?} did not reach the batch file: {out:?}");
    }
    assert_eq!(lines_in(&log), 1, "the fake ran once");

    // Outside it: every one is refused and no process starts, so the log stays as it is.
    let unsafe_args = [
        "a&b", "&calc", "a%b", "%PATH%", "a^b", "a|b", "a<b", "a>b", "a\"b", "a\nb", "a\r\nb", "a b", "a;b", "a(b)",
        "a!b", "a`b", "a'b", "a$b", "a*b", "a?b", "a~b", "a\\b", "",
    ];
    for bad in unsafe_args {
        let outcome = runner.run_argv(&az, &args(&["version", bad]), env(), &limits, &RunCancel::default());
        assert!(matches!(outcome, Err(RunError::BatchArgument)), "{bad:?} was not refused before a process: {:?}", outcome.map(|r| r.end));
    }
    assert_eq!(lines_in(&log), 1, "an unsafe argument started a process");

    // The same program name with the other batch extension and in capitals is held to the same rule.
    let bat = bin.join("AZ.BAT");
    std::fs::write(&bat, FAKE_AZ).unwrap();
    let outcome = runner.run_argv(&bat, &args(&["a&b"]), env(), &limits, &RunCancel::default());
    assert!(matches!(outcome, Err(RunError::BatchArgument)), "AZ.BAT was not held to the charset");
    assert_eq!(lines_in(&log), 1);
}

// ---- INT-W3: an Exec launch under ConPTY ------------------------------------------------------------------------

#[cfg(feature = "ghostty")]
mod conpty_exec {
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use holzwave_core::engine::{DomKey, Mods};
    use holzwave_core::integrate::detect::child_path;
    use holzwave_core::integrate::run::{EnvExtra, EnvPolicy, Launch};
    use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
    use holzwave_core::platform::Os;
    use holzwave_core::wire::RowMsg;

    use super::{text, Scratch};

    /// Starting a console host and a batch file takes a moment on a cold machine.
    const WAIT: Duration = Duration::from_secs(20);

    type Log = Arc<Mutex<Vec<PaneEvent>>>;

    fn events(log: &Log) -> Vec<PaneEvent> {
        log.lock().unwrap().clone()
    }

    fn row_text(row: &RowMsg) -> String {
        row.runs.iter().flat_map(|run| run.0.chars()).filter(|c| *c != '\0').collect()
    }

    fn has_text(log: &Log, needle: &str) -> bool {
        events(log).iter().any(|event| match event {
            PaneEvent::Frame(frame) => frame.changed.iter().any(|row| row_text(row).contains(needle)),
            _ => false,
        })
    }

    fn helds(log: &Log) -> Vec<Option<u32>> {
        events(log)
            .into_iter()
            .filter_map(|event| match event {
                PaneEvent::Held { code } => Some(code),
                _ => None,
            })
            .collect()
    }

    fn exits(log: &Log) -> Vec<Option<u32>> {
        events(log)
            .into_iter()
            .filter_map(|event| match event {
                PaneEvent::Exited { code } => Some(code),
                _ => None,
            })
            .collect()
    }

    fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if f() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn enter() -> DomKey {
        DomKey { code: "Enter".into(), key: "Enter".into(), mods: Mods::NONE, ..DomKey::default() }
    }

    /// Row INT-W3. A pass: a terminal launch of a fake `kubectl.cmd` that prints an error line and exits with status 1
    /// runs in a real ConPTY pane, the error line and the notice `[holzWave] exited with status 1. Press Enter to close.`
    /// are on the screen, the pane is held (one `Held` with status 1, no `Exited`) through a wait and a letter, and Enter
    /// ends it with one `Exited` carrying status 1.
    #[test]
    fn an_exec_launch_runs_in_a_conpty_pane_and_is_held_after_a_failure() {
        let root = Scratch::new("integrate_windows_exec");
        let bin = root.dir("bin");
        let tool = bin.join("kubectl.cmd");
        std::fs::write(&tool, "@echo off\r\necho Error from server (NotFound): pods \"x\" not found\r\nexit /b 1\r\n")
            .unwrap();

        // The launch is built the way the shell builds it: an environment from nothing, the names the pane inherits.
        let process_path = std::env::var("PATH").unwrap_or_default();
        let path = child_path(Os::Windows, &text(&bin), &process_path, &[]);
        let env = EnvPolicy::build(std::iter::empty::<(OsString, OsString)>(), Os::Windows, path, EnvExtra::default())
            .expect("the environment builds");
        let inherited: Vec<OsString> = std::env::vars_os().map(|(name, _)| name).collect();
        let launch = Launch::new(tool.clone(), Vec::new(), env, inherited, Os::Windows, None, "kubectl".into());
        let mut spec = PaneSpec::new(text(&tool), 100, 24);
        launch.apply_to(&mut spec);
        assert!(spec.linger.is_some(), "a launch lingers");

        let log = Log::default();
        let sink = {
            let log = log.clone();
            move |event| log.lock().unwrap().push(event)
        };
        let pane = PaneHandle::spawn_ghostty(1, spec, sink).expect("the pane starts");

        assert!(wait_until(WAIT, || !helds(&log).is_empty()), "no Held within {WAIT:?}: {:?}", events(&log).len());
        assert!(has_text(&log, "Error from server (NotFound): pods \"x\" not found"), "the tool's line is not on screen");
        assert!(
            wait_until(WAIT, || has_text(&log, "[holzWave] exited with status 1. Press Enter to close.")),
            "the notice line is not on screen"
        );
        assert_eq!(helds(&log), [Some(1)], "one Held with the tool's status");
        assert!(exits(&log).is_empty(), "no Exited yet");

        // Neither waiting nor a letter ends it.
        std::thread::sleep(Duration::from_secs(1));
        let letter = DomKey { code: "KeyA".into(), key: "a".into(), mods: Mods::NONE, ..DomKey::default() };
        let _ = pane.key(letter);
        std::thread::sleep(Duration::from_millis(500));
        assert!(exits(&log).is_empty(), "the pane ended without Enter");

        pane.key(enter()).expect("the held pane takes a key");
        assert!(wait_until(WAIT, || !exits(&log).is_empty()), "Enter did not end the pane");
        assert!(pane.join(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(exits(&log), [Some(1)], "exactly one Exited, with the tool's status");
    }
}

// ---- INT-W4: the KUBECONFIG list and the path rules --------------------------------------------------------------

/// Row INT-W4. A pass: the overlay the app wrote is first in `KUBECONFIG`, the person's own files follow it, and the
/// separator is `;` (a drive letter's `:` is no separator); a network path, a device path, a path without a drive, an
/// empty entry and an entry with a control character are dropped from the list; an overlay that is such a path, or has
/// a `;` in it, is refused; and the detector refuses a network or device path set as the tool's path without looking
/// at the file at all (a look at a network path would contact the host).
#[test]
fn the_overlay_kubeconfig_value_uses_semicolons_and_refuses_unc_and_device_paths() {
    let root = Scratch::new("integrate_windows_kubeconfig");
    let entry = ContextEntry::new("prod-eu", "prod-eu", "admin", Some("default"));
    let overlay_path = write_overlay(&root.0, 41, &entry).expect("the overlay is written");
    assert!(overlay_path.is_file(), "the overlay file is not there: {}", overlay_path.display());
    let overlay = text(&overlay_path);
    let home = r"C:\Users\someone";

    // The person's own list follows the overlay, joined with ';'.
    let own = r"C:\kube\a.yaml;D:\kube\b.yaml";
    assert_eq!(kubeconfig_value(&overlay, Os::Windows, Some(own), home).unwrap(), format!("{overlay};{own}"));

    // No list of their own: the default file of the home directory.
    let default = format!(r"{overlay};{home}\.kube\config");
    assert_eq!(kubeconfig_value(&overlay, Os::Windows, None, home).unwrap(), default);
    assert_eq!(kubeconfig_value(&overlay, Os::Windows, Some(""), home).unwrap(), default);

    // Entries that must never be listed are dropped one by one; the good one stays.
    let hostile = [
        r"\\evil-host\share\config",
        r"\\?\C:\x\config",
        r"\\.\pipe\x",
        "//evil-host/share/config",
        r"relative\config",
        r"C:drive-relative",
        r"\rooted\config",
        "",
        "C:\\with\u{7}control",
    ];
    let list = format!("{};C:\\ok\\config", hostile.join(";"));
    assert_eq!(kubeconfig_value(&overlay, Os::Windows, Some(&list), home).unwrap(), format!("{overlay};C:\\ok\\config"));
    // Only hostile entries: the list falls back to the default, it is never empty and never hostile.
    let only = hostile.join(";");
    assert_eq!(kubeconfig_value(&overlay, Os::Windows, Some(&only), home).unwrap(), default);

    // An overlay that is not an absolute drive path, or holds the separator, is refused.
    for bad in [r"\\evil-host\share\run\kube\1.json", r"\\?\C:\x\1.json", r"\\.\pipe\x", r"relative\1.json", r"C:\a;C:\b\1.json", ""] {
        assert_eq!(kubeconfig_value(bad, Os::Windows, Some(own), home), Err(OverlayError::Path), "{bad:?} was accepted");
    }

    // The detector: a tool path that is a network or device path is unusable and the file is not even looked at.
    for path in [r"\\evil-host\share\kubectl.exe", "//evil-host/share/kubectl.exe", r"\\?\C:\x\kubectl.exe", r"\\.\pipe\kubectl.exe"] {
        let (detection, looked) = detect_in(&dotless(), &root.0, &[("PATH", "")], Some(path));
        match detection {
            Detection::Unusable { reason: UnusableReason::NetworkPath, .. } => {}
            other => panic!("{path:?} gave {other:?}"),
        }
        assert_eq!(looked, 0, "{path:?} was looked at");
    }
}
