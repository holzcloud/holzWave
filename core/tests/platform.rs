//! Tests for the per-OS decisions of the core: the default shell and its arguments, the child's locale variable, the
//! exit-poll interval and the number of rename retries.
//!
//! Every test passes an `Os` and fake environment and file-existence closures. Nothing here reads the machine the
//! test runs on, so the Windows and Linux rules are proven on any host.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use holzwave_core::platform::{Os, ShellLaunch, exit_poll, rename_retries, shell_launch, utf8_locale};
use holzwave_core::pty::SpawnOptions;

/// A fake environment: the variable named `key`, as `pairs` define it.
fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    move |key| map.get(key).cloned()
}

/// A fake file system: only the listed paths exist.
fn files(paths: &[&str]) -> impl Fn(&str) -> bool {
    let set: HashSet<String> = paths.iter().map(|p| p.to_string()).collect();
    move |path| set.contains(path)
}

fn launch(os: Os, configured: &str, vars: &[(&str, &str)], existing: &[&str]) -> ShellLaunch {
    shell_launch(os, configured, &env(vars), &files(existing))
}

fn expect(program: &str, args: &[&str]) -> ShellLaunch {
    ShellLaunch { program: program.to_string(), args: args.iter().map(|a| a.to_string()).collect() }
}

const PWSH7: &str = r"C:\Program Files\PowerShell\7\pwsh.exe";
const WINPS: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";

/// The variables of an ordinary Windows session.
const WINDOWS_VARS: &[(&str, &str)] = &[
    ("ProgramFiles", r"C:\Program Files"),
    ("SystemRoot", r"C:\Windows"),
    ("ComSpec", r"C:\Windows\System32\cmd.exe"),
    ("PATH", r"C:\Windows\System32;C:\Tools"),
];

// ---- Os ----------------------------------------------------------------------------------------------

#[test]
fn os_names_round_trip_and_unknown_names_read_as_linux() {
    for os in [Os::Macos, Os::Windows, Os::Linux] {
        assert_eq!(Os::from_name(os.name()), os);
    }
    assert_eq!(Os::from_name("macos"), Os::Macos);
    assert_eq!(Os::from_name("windows"), Os::Windows);
    assert_eq!(Os::from_name("linux"), Os::Linux);
    // The names of std::env::consts::OS that are Unix and not macOS take the Linux rules.
    for name in ["freebsd", "openbsd", "android", ""] {
        assert_eq!(Os::from_name(name), Os::Linux, "{name:?}");
    }
}

#[test]
fn the_current_os_is_the_one_this_build_targets() {
    let expected = if cfg!(target_os = "macos") {
        Os::Macos
    } else if cfg!(windows) {
        Os::Windows
    } else {
        Os::Linux
    };
    assert_eq!(Os::current(), expected);
    assert_eq!(Os::from_name(std::env::consts::OS), expected, "std's name for this OS is understood");
}

// ---- the default shell, Windows ----------------------------------------------------------------------

#[test]
fn windows_default_shell_prefers_pwsh_then_windows_powershell_then_cmd() {
    // 1. pwsh 7 in its standard folder
    assert_eq!(launch(Os::Windows, "", WINDOWS_VARS, &[PWSH7, WINPS]), expect(PWSH7, &["-NoLogo"]));
    // 2. pwsh.exe in an absolute directory of PATH, when the standard folder has none
    let vars = [("ProgramFiles", r"C:\Program Files"), ("SystemRoot", r"C:\Windows"), ("PATH", r"C:\Tools;D:\bin")];
    assert_eq!(
        launch(Os::Windows, "", &vars, &[r"D:\bin\pwsh.exe", WINPS]),
        expect(r"D:\bin\pwsh.exe", &["-NoLogo"])
    );
    // 3. Windows PowerShell under SystemRoot
    assert_eq!(launch(Os::Windows, "", WINDOWS_VARS, &[WINPS]), expect(WINPS, &["-NoLogo"]));
    // 4. %ComSpec%, without arguments
    assert_eq!(launch(Os::Windows, "", WINDOWS_VARS, &[]), expect(r"C:\Windows\System32\cmd.exe", &[]));
    // 5. nothing set: the bare cmd.exe
    assert_eq!(launch(Os::Windows, "", &[], &[]), expect("cmd.exe", &[]));
}

#[test]
fn the_standard_pwsh_folder_beats_a_pwsh_on_path() {
    let vars = [("ProgramFiles", r"C:\Program Files"), ("PATH", r"C:\Tools")];
    let launched = launch(Os::Windows, "", &vars, &[PWSH7, r"C:\Tools\pwsh.exe"]);
    assert_eq!(launched, expect(PWSH7, &["-NoLogo"]));
}

#[test]
fn a_pwsh_on_path_beats_windows_powershell() {
    let vars = [("SystemRoot", r"C:\Windows"), ("PATH", r"C:\Tools")];
    let launched = launch(Os::Windows, "", &vars, &[r"C:\Tools\pwsh.exe", WINPS]);
    assert_eq!(launched, expect(r"C:\Tools\pwsh.exe", &["-NoLogo"]));
}

#[test]
fn a_relative_path_entry_is_never_searched() {
    // `pwsh.exe` exists next to the current directory, in a relative entry and in an empty one.
    let existing = ["pwsh.exe", r".\pwsh.exe", r"tools\pwsh.exe", r"\pwsh.exe", r"/pwsh.exe", r"bin\pwsh.exe"];
    for path in ["", ";", r".;tools;;", r"\;/;bin", r"..\..;."] {
        let vars = [("PATH", path)];
        assert_eq!(launch(Os::Windows, "", &vars, &existing), expect("cmd.exe", &[]), "PATH {path:?}");
    }
    // An absolute entry after a relative one is still found.
    let vars = [("PATH", r".;C:\Tools")];
    assert_eq!(
        launch(Os::Windows, "", &vars, &[r"C:\Tools\pwsh.exe", "pwsh.exe"]),
        expect(r"C:\Tools\pwsh.exe", &["-NoLogo"])
    );
}

#[test]
fn a_network_path_entry_is_never_searched() {
    // A UNC directory would send the user's credentials to a host the PATH names.
    let vars = [("PATH", r"\\evil\share;//evil/share")];
    let existing = [r"\\evil\share\pwsh.exe", "//evil/share/pwsh.exe"];
    assert_eq!(launch(Os::Windows, "", &vars, &existing), expect("cmd.exe", &[]));
}

#[test]
fn a_relative_install_folder_is_not_used() {
    let vars = [("ProgramFiles", "Program Files"), ("SystemRoot", "Windows"), ("ComSpec", "cmd")];
    let existing = [r"Program Files\PowerShell\7\pwsh.exe", r"Windows\System32\WindowsPowerShell\v1.0\powershell.exe"];
    assert_eq!(launch(Os::Windows, "", &vars, &existing), expect("cmd.exe", &[]));
}

#[test]
fn quoted_and_slashed_path_entries_are_read_like_windows_reads_them() {
    let vars = [("PATH", r#""C:\Program Files\Tools\";D:/bin/"#)];
    assert_eq!(
        launch(Os::Windows, "", &vars, &[r"C:\Program Files\Tools\pwsh.exe"]),
        expect(r"C:\Program Files\Tools\pwsh.exe", &["-NoLogo"])
    );
    assert_eq!(
        launch(Os::Windows, "", &vars, &[r"D:/bin\pwsh.exe"]),
        expect(r"D:/bin\pwsh.exe", &["-NoLogo"])
    );
}

#[test]
fn empty_windows_variables_count_as_unset() {
    let vars = [("ProgramFiles", ""), ("SystemRoot", ""), ("ComSpec", ""), ("PATH", "")];
    assert_eq!(launch(Os::Windows, "", &vars, &[PWSH7, WINPS]), expect("cmd.exe", &[]));
}

#[test]
fn a_comspec_that_is_not_cmd_gets_the_arguments_of_its_own_file() {
    let vars = [("ComSpec", r"C:\Tools\NuShell\nu.exe")];
    assert_eq!(launch(Os::Windows, "", &vars, &[]), expect(r"C:\Tools\NuShell\nu.exe", &["-l"]));
}

// ---- the default shell, Linux and macOS --------------------------------------------------------------

#[test]
fn unix_uses_shell_with_a_login_flag() {
    for os in [Os::Linux, Os::Macos] {
        assert_eq!(launch(os, "", &[("SHELL", "/usr/bin/fish")], &[]), expect("/usr/bin/fish", &["-l"]), "{os:?}");
    }
}

#[test]
fn linux_falls_back_to_bin_sh() {
    assert_eq!(launch(Os::Linux, "", &[], &[]), expect("/bin/sh", &["-l"]));
}

#[test]
fn macos_falls_back_to_bin_zsh() {
    assert_eq!(launch(Os::Macos, "", &[], &[]), expect("/bin/zsh", &["-l"]));
}

#[test]
fn an_empty_shell_variable_counts_as_unset() {
    assert_eq!(launch(Os::Linux, "", &[("SHELL", "")], &[]), expect("/bin/sh", &["-l"]));
    assert_eq!(launch(Os::Macos, "", &[("SHELL", "")], &[]), expect("/bin/zsh", &["-l"]));
}

#[test]
fn unix_ignores_the_windows_variables() {
    let launched = launch(Os::Linux, "", WINDOWS_VARS, &[PWSH7, WINPS]);
    assert_eq!(launched, expect("/bin/sh", &["-l"]));
}

#[test]
fn windows_ignores_the_shell_variable() {
    // An msys or git-bash parent exports SHELL; a Windows pane still starts the Windows default.
    let mut vars = WINDOWS_VARS.to_vec();
    vars.push(("SHELL", "/usr/bin/bash"));
    assert_eq!(launch(Os::Windows, "", &vars, &[PWSH7]), expect(PWSH7, &["-NoLogo"]));
}

// ---- a configured shell ------------------------------------------------------------------------------

#[test]
fn a_configured_shell_replaces_the_default_on_every_os() {
    assert_eq!(launch(Os::Linux, "/usr/bin/zsh", &[("SHELL", "/bin/bash")], &[]), expect("/usr/bin/zsh", &["-l"]));
    assert_eq!(launch(Os::Macos, "/opt/homebrew/bin/fish", &[], &[]), expect("/opt/homebrew/bin/fish", &["-l"]));
    assert_eq!(
        launch(Os::Windows, r"C:\Program Files\Git\bin\bash.exe", WINDOWS_VARS, &[PWSH7]),
        expect(r"C:\Program Files\Git\bin\bash.exe", &["-l"])
    );
}

#[test]
fn a_configured_shell_is_one_program_and_never_split() {
    let program = r"C:\Program Files\PowerShell\7-preview\pwsh.exe --login";
    let launched = launch(Os::Windows, program, &[], &[]);
    assert_eq!(launched.program, program);
    assert!(launched.args.iter().all(|a| !a.contains(' ')), "{:?}", launched.args);
}

#[test]
fn shell_arguments_follow_the_file_stem() {
    let args = |program: &str| launch(Os::Windows, program, &[], &[]).args;
    for program in [r"C:\x\pwsh.exe", r"C:\x\powershell.exe", "pwsh", "powershell"] {
        assert_eq!(args(program), ["-NoLogo"], "{program}");
    }
    for program in [r"C:\Windows\System32\cmd.exe", "cmd"] {
        assert!(args(program).is_empty(), "{program}");
    }
    for program in ["bash", "zsh", "fish", "sh", "nu", r"C:\Program Files\Git\bin\bash.exe", "/usr/bin/zsh"] {
        assert_eq!(args(program), ["-l"], "{program}");
    }
    for program in ["wsl.exe", r"C:\x\ssh.exe", "python", r"C:\x\bashful.exe"] {
        assert!(args(program).is_empty(), "{program}");
    }
}

#[test]
fn the_file_stem_is_read_case_insensitively_and_without_exe() {
    let args = |program: &str| launch(Os::Windows, program, &[], &[]).args;
    assert_eq!(args(r"C:\X\PWSH.EXE"), ["-NoLogo"]);
    assert_eq!(args(r"C:\X\PowerShell.Exe"), ["-NoLogo"]);
    assert_eq!(args("PwSh"), ["-NoLogo"]);
    assert!(args(r"C:\X\CMD.EXE").is_empty());
    assert_eq!(args(r"C:\X\Bash.EXE"), ["-l"]);
    // Both separators end a directory name; only a trailing `.exe` is dropped.
    assert_eq!(args("C:/X/pwsh.exe"), ["-NoLogo"]);
    assert!(args(r"C:\pwsh.exe\other").is_empty());
    assert!(args("pwsh.exe.bak").is_empty());
}

#[test]
fn unix_gives_a_configured_shell_a_login_flag_whatever_its_name() {
    for os in [Os::Linux, Os::Macos] {
        for program in ["/bin/sh", "/usr/local/bin/nu", "/opt/odd/tool", "/usr/bin/pwsh"] {
            assert_eq!(launch(os, program, &[], &[]).args, ["-l"], "{os:?} {program}");
        }
    }
}

// ---- timing ------------------------------------------------------------------------------------------

#[test]
fn exit_poll_exists_only_on_windows() {
    assert_eq!(exit_poll(Os::Windows), Some(Duration::from_millis(250)));
    assert_eq!(exit_poll(Os::Macos), None);
    assert_eq!(exit_poll(Os::Linux), None);
}

#[test]
fn rename_retries_exist_only_on_windows() {
    assert_eq!(rename_retries(Os::Windows), 3);
    assert_eq!(rename_retries(Os::Macos), 0);
    assert_eq!(rename_retries(Os::Linux), 0);
}

// ---- the child's environment -------------------------------------------------------------------------

fn child_env(os: Os, parent: &[(&str, &str)]) -> Vec<(String, String)> {
    SpawnOptions::new("shell", 80, 24).child_env_on(os, env(parent))
}

fn value(env: &[(String, String)], key: &str) -> Option<String> {
    env.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

#[test]
fn the_utf8_fallback_locale_depends_on_the_os() {
    assert_eq!(utf8_locale(Os::Macos), Some("en_US.UTF-8"));
    assert_eq!(utf8_locale(Os::Linux), Some("C.UTF-8"));
    assert_eq!(utf8_locale(Os::Windows), None);
}

#[test]
fn windows_children_get_no_locale_variable() {
    assert_eq!(value(&child_env(Os::Windows, &[]), "LC_CTYPE"), None);
}

#[test]
fn linux_and_macos_children_get_their_fallback_locale() {
    assert_eq!(value(&child_env(Os::Linux, &[]), "LC_CTYPE").as_deref(), Some("C.UTF-8"));
    assert_eq!(value(&child_env(Os::Macos, &[]), "LC_CTYPE").as_deref(), Some("en_US.UTF-8"));
}

#[test]
fn a_parent_locale_is_left_alone_on_every_os() {
    for os in [Os::Macos, Os::Windows, Os::Linux] {
        for key in ["LANG", "LC_ALL", "LC_CTYPE"] {
            let vars = child_env(os, &[(key, "de_CH.UTF-8")]);
            assert_eq!(value(&vars, "LC_CTYPE"), None, "{os:?} with {key}");
        }
        // Empty values count as unset (Windows has no fallback to add either way).
        let empty = child_env(os, &[("LANG", ""), ("LC_ALL", ""), ("LC_CTYPE", "")]);
        assert_eq!(value(&empty, "LC_CTYPE").as_deref(), utf8_locale(os), "{os:?}");
    }
}

#[test]
fn the_fixed_variables_and_the_callers_pairs_are_the_same_on_every_os() {
    for os in [Os::Macos, Os::Windows, Os::Linux] {
        let options = SpawnOptions::new("shell", 80, 24).env("A", "1").env("TERM", "dumb");
        let vars = options.child_env_on(os, env(&[("LANG", "en_US.UTF-8")]));
        let names: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["TERM", "COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "A", "TERM"], "{os:?}");
        assert_eq!(value(&vars, "COLORTERM").as_deref(), Some("truecolor"));
        assert_eq!(value(&vars, "TERM").as_deref(), Some("dumb"), "the caller's pair wins");
        assert_eq!(value(&vars, "TERM_PROGRAM_VERSION").as_deref(), Some(env!("CARGO_PKG_VERSION")));
    }
}

#[test]
fn child_env_is_child_env_on_for_the_current_os() {
    let options = SpawnOptions::new("shell", 80, 24).env("A", "1");
    let parent = [("HOME", "/h")];
    assert_eq!(options.child_env(env(&parent)), options.child_env_on(Os::current(), env(&parent)));
}
