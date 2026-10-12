//! Tests for the per-OS decisions of the core: the default shell and its arguments, the child's locale variable, the
//! exit-poll interval and the number of rename retries.
//!
//! Every test passes an `Os` and fake environment and file-existence closures. Nothing here reads the machine the
//! test runs on, so the Windows and Linux rules are proven on any host.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use holzwave_core::platform::{
    HOST_LOGIN_SHELL_SCRIPT, Os, ProgramCheck, ShellLaunch, exit_poll, host_command, host_shell_launch, in_flatpak,
    rename_retries, resolve_shell, shell_launch, utf8_locale,
};
use holzwave_core::pty::SpawnOptions;
use holzwave_core::winshell::{POWERSHELL_PROMPT_SCRIPT, encode_command, powershell_integration_args};

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

/// What a PowerShell pane on Windows is started with: no banner, then the prompt wrapper that announces the
/// directory (`-NoExit -EncodedCommand <the constant script>`). The encoding itself is proven in `winshell.rs`.
fn powershell_args() -> Vec<String> {
    let mut args = vec!["-NoLogo".to_string()];
    args.extend(powershell_integration_args());
    args
}

fn powershell(program: &str) -> ShellLaunch {
    ShellLaunch { program: program.to_string(), args: powershell_args() }
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
    assert_eq!(launch(Os::Windows, "", WINDOWS_VARS, &[PWSH7, WINPS]), powershell(PWSH7));
    // 2. pwsh.exe in an absolute directory of PATH, when the standard folder has none
    let vars = [("ProgramFiles", r"C:\Program Files"), ("SystemRoot", r"C:\Windows"), ("PATH", r"C:\Tools;D:\bin")];
    assert_eq!(
        launch(Os::Windows, "", &vars, &[r"D:\bin\pwsh.exe", WINPS]),
        powershell(r"D:\bin\pwsh.exe")
    );
    // 3. Windows PowerShell under SystemRoot
    assert_eq!(launch(Os::Windows, "", WINDOWS_VARS, &[WINPS]), powershell(WINPS));
    // 4. %ComSpec%, without arguments
    assert_eq!(launch(Os::Windows, "", WINDOWS_VARS, &[]), expect(r"C:\Windows\System32\cmd.exe", &[]));
    // 5. nothing set: the bare cmd.exe
    assert_eq!(launch(Os::Windows, "", &[], &[]), expect("cmd.exe", &[]));
}

#[test]
fn the_standard_pwsh_folder_beats_a_pwsh_on_path() {
    let vars = [("ProgramFiles", r"C:\Program Files"), ("PATH", r"C:\Tools")];
    let launched = launch(Os::Windows, "", &vars, &[PWSH7, r"C:\Tools\pwsh.exe"]);
    assert_eq!(launched, powershell(PWSH7));
}

#[test]
fn a_pwsh_on_path_beats_windows_powershell() {
    let vars = [("SystemRoot", r"C:\Windows"), ("PATH", r"C:\Tools")];
    let launched = launch(Os::Windows, "", &vars, &[r"C:\Tools\pwsh.exe", WINPS]);
    assert_eq!(launched, powershell(r"C:\Tools\pwsh.exe"));
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
        powershell(r"C:\Tools\pwsh.exe")
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
        powershell(r"C:\Program Files\Tools\pwsh.exe")
    );
    assert_eq!(
        launch(Os::Windows, "", &vars, &[r"D:/bin\pwsh.exe"]),
        powershell(r"D:/bin\pwsh.exe")
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
    assert_eq!(launch(Os::Windows, "", &vars, &[PWSH7]), powershell(PWSH7));
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
        assert_eq!(args(program), powershell_args(), "{program}");
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
    assert_eq!(args(r"C:\X\PWSH.EXE"), powershell_args());
    assert_eq!(args(r"C:\X\PowerShell.Exe"), powershell_args());
    assert_eq!(args("PwSh"), powershell_args());
    assert!(args(r"C:\X\CMD.EXE").is_empty());
    assert_eq!(args(r"C:\X\Bash.EXE"), ["-l"]);
    // Both separators end a directory name; only a trailing `.exe` is dropped.
    assert_eq!(args("C:/X/pwsh.exe"), powershell_args());
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

#[test]
fn a_powershell_pane_on_windows_starts_the_prompt_wrapper_in_one_place() {
    // A default and a configured program alike: `-NoLogo -NoExit -EncodedCommand <base64 of the script>`.
    let launched = launch(Os::Windows, "", WINDOWS_VARS, &[PWSH7]);
    assert_eq!(launched.args.len(), 4);
    assert_eq!(&launched.args[..3], ["-NoLogo", "-NoExit", "-EncodedCommand"]);
    assert_eq!(launched.args[3], encode_command(POWERSHELL_PROMPT_SCRIPT));
    for program in [WINPS, "pwsh", "powershell.exe", r"C:\Tools\PWSH.EXE"] {
        assert_eq!(launch(Os::Windows, program, &[], &[]).args, launched.args, "{program}");
    }
}

#[test]
fn only_powershell_gets_the_prompt_wrapper() {
    for program in [r"C:\Windows\System32\cmd.exe", "cmd", "bash", r"C:\Program Files\Gitinash.exe", "wsl.exe", "nu"] {
        let args = launch(Os::Windows, program, &[], &[]).args;
        assert!(!args.iter().any(|a| a == "-EncodedCommand" || a == "-NoExit"), "{program}: {args:?}");
    }
}

#[test]
fn no_unix_platform_gets_the_prompt_wrapper_whatever_the_program_is_called() {
    for os in [Os::Linux, Os::Macos] {
        for program in ["pwsh", "powershell", "/usr/bin/pwsh", "/opt/microsoft/powershell/7/pwsh", "pwsh.exe"] {
            assert_eq!(launch(os, program, &[], &[]).args, ["-l"], "{os:?} {program}");
        }
        assert_eq!(launch(os, "", &[("SHELL", "/usr/bin/pwsh")], &[]).args, ["-l"], "{os:?} through $SHELL");
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

// ---- resolve_shell: an unusable `shell` setting never leaves a pane without a shell -----------------------------------

/// The checks of a fake disk: the listed paths are usable, `kinds` say what else is there, everything else is missing.
fn disk<'a>(usable: &'a [&'a str], kinds: &'a [(&'a str, ProgramCheck)]) -> impl Fn(&str) -> ProgramCheck + 'a {
    move |path| {
        if usable.contains(&path) {
            ProgramCheck::Usable
        } else {
            kinds.iter().find(|(p, _)| *p == path).map_or(ProgramCheck::Missing, |(_, kind)| *kind)
        }
    }
}

fn resolved(
    os: Os,
    configured: &str,
    vars: &[(&str, &str)],
    existing: &[&str],
    usable: &[&str],
    kinds: &[(&str, ProgramCheck)],
) -> (ShellLaunch, Option<String>) {
    let out = resolve_shell(os, configured, &env(vars), &files(existing), &disk(usable, kinds));
    (out.launch, out.unusable)
}

#[test]
fn a_usable_setting_is_started_as_it_is_and_an_empty_one_is_the_default() {
    let (launch, note) = resolved(Os::Linux, "/usr/bin/fish", &[("SHELL", "/bin/bash")], &[], &["/usr/bin/fish"], &[]);
    assert_eq!((launch, note), (expect("/usr/bin/fish", &["-l"]), None));
    let (launch, note) = resolved(Os::Linux, "", &[("SHELL", "/bin/bash")], &[], &[], &[]);
    assert_eq!((launch, note), (expect("/bin/bash", &["-l"]), None), "the empty setting is not checked");
}

#[test]
fn a_missing_program_falls_back_to_the_login_shell_or_bin_sh_with_one_sentence() {
    let (launch, note) = resolved(Os::Linux, "/opt/none/fish", &[("SHELL", "/bin/bash")], &[], &[], &[]);
    assert_eq!(launch, expect("/bin/bash", &["-l"]));
    let note = note.expect("a sentence");
    assert!(note.contains("/opt/none/fish") && note.contains("was not found") && note.contains("/bin/bash"), "{note}");

    assert_eq!(resolved(Os::Linux, "/opt/none/fish", &[], &[], &[], &[]).0, expect("/bin/sh", &["-l"]));
    assert_eq!(resolved(Os::Macos, "/opt/none/fish", &[], &[], &[], &[]).0, expect("/bin/zsh", &["-l"]));
}

#[test]
fn a_directory_and_a_file_that_cannot_run_are_named() {
    let kinds = [("/etc", ProgramCheck::NotAFile), ("/etc/hosts", ProgramCheck::NotExecutable)];
    let (_, dir) = resolved(Os::Linux, "/etc", &[], &[], &[], &kinds);
    assert!(dir.expect("a sentence").contains("is not a file"));
    let (launch, plain) = resolved(Os::Linux, "/etc/hosts", &[], &[], &[], &kinds);
    assert!(plain.expect("a sentence").contains("is not executable"));
    assert_eq!(launch.program, "/bin/sh");
}

#[test]
fn on_windows_the_default_order_is_pwsh_then_windows_powershell_then_cmd() {
    let gone = r"D:\gone\shell.exe";
    let (launch, note) = resolved(Os::Windows, gone, WINDOWS_VARS, &[PWSH7, WINPS], &[], &[]);
    assert_eq!(launch, powershell(PWSH7));
    assert!(note.is_some());
    assert_eq!(resolved(Os::Windows, gone, WINDOWS_VARS, &[WINPS], &[], &[]).0, powershell(WINPS));
    assert_eq!(resolved(Os::Windows, gone, WINDOWS_VARS, &[], &[], &[]).0, expect(r"C:\Windows\System32\cmd.exe", &[]));
}

#[test]
fn a_bare_name_is_looked_up_in_the_absolute_directories_of_path_and_started_by_its_full_path() {
    let vars = [("SHELL", "/bin/bash"), ("PATH", "relative/bin:/opt/x/bin::/usr/local/bin")];
    let (launch, note) = resolved(Os::Linux, "fish", &vars, &[], &["/usr/local/bin/fish"], &[]);
    assert_eq!((launch, note), (expect("/usr/local/bin/fish", &["-l"]), None));
    // A relative directory of PATH and the current directory are never searched.
    let (launch, note) = resolved(Os::Linux, "fish", &vars, &[], &["relative/bin/fish", "fish", "/fish"], &[]);
    assert_eq!(launch.program, "/bin/bash");
    assert!(note.expect("a sentence").contains("was not found"));
    // A name that is there but cannot run is named, a later usable one still wins.
    let kinds = [("/opt/x/bin/fish", ProgramCheck::NotExecutable)];
    assert!(resolved(Os::Linux, "fish", &vars, &[], &[], &kinds).1.expect("a sentence").contains("is not executable"));
    assert_eq!(resolved(Os::Linux, "fish", &vars, &[], &["/usr/local/bin/fish"], &kinds).1, None);
    // Windows: the system directory is searched too and `.exe` is tried.
    let cmd = r"C:\Windows\System32\cmd.exe";
    let (launch, note) = resolved(Os::Windows, "cmd", WINDOWS_VARS, &[], &[cmd], &[]);
    assert_eq!((launch, note), (expect(cmd, &[]), None));
}

#[test]
fn a_relative_path_is_never_looked_up() {
    for relative in ["./sh", "../bin/sh", "bin/sh", "~/bin/sh"] {
        let (launch, note) = resolved(Os::Linux, relative, &[("SHELL", "/bin/bash")], &[], &[relative, "/bin/sh"], &[]);
        assert_eq!(launch.program, "/bin/bash", "{relative}");
        assert!(note.is_some(), "{relative}");
    }
    let (_, note) = resolved(Os::Windows, r"tools\pwsh.exe", WINDOWS_VARS, &[], &[r"tools\pwsh.exe"], &[]);
    assert!(note.is_some(), "a Windows path without a drive");
    let (_, note) = resolved(Os::Windows, r"\\host\share\pwsh.exe", WINDOWS_VARS, &[], &[r"\\host\share\pwsh.exe"], &[]);
    assert!(note.is_some(), "a network path");
}

#[test]
fn the_sentence_cuts_a_long_value_and_never_carries_a_control_character() {
    let long = format!("/{}", "y".repeat(300));
    let (_, note) = resolved(Os::Linux, &long, &[], &[], &[], &[]);
    let note = note.expect("a sentence");
    assert!(note.len() < 200 && note.contains("..."), "{note}");
    let (_, note) = resolved(Os::Linux, "/bin/s\x1b[31mh", &[], &[], &[], &[]);
    assert!(!note.expect("a sentence").chars().any(char::is_control));
}

// ---- Flatpak: the shell of a pane runs on the host ----------------------------------------------------------------
//
// PTY semantics of `flatpak-spawn --host` (a controlling terminal, job control, `sudo`) are an assumption until row
// L10 of docs/test-plan.md passes on a real Flatpak; what is proven here is the command line and its refusals.

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn flatpak_is_detected_by_the_info_file_or_the_id() {
    // On every OS the facts are the same, because the function reads nothing but what it is given.
    assert!(in_flatpak(&env(&[]), &files(&["/.flatpak-info"])), "the info file alone");
    assert!(in_flatpak(&env(&[("FLATPAK_ID", "ch.holzcloud.holzwave")]), &files(&[])), "the id alone");
    assert!(in_flatpak(&env(&[("FLATPAK_ID", "x")]), &files(&["/.flatpak-info"])), "both");
    assert!(!in_flatpak(&env(&[]), &files(&[])), "neither");
    assert!(!in_flatpak(&env(&[("FLATPAK_ID", "")]), &files(&[])), "an empty id is no id");
    assert!(!in_flatpak(&env(&[("HOME", "/home/a")]), &files(&["/etc/os-release", "/flatpak-info", "/app"])), "look-alikes");
}

#[test]
fn the_host_command_line_is_the_constants_the_directory_the_env_and_the_program_after_the_separator() {
    let shell = expect("/usr/bin/fish", &["-l"]);
    let line = host_command(&shell, Some("/home/a/src"), &pairs(&[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]));
    assert_eq!(
        line,
        expect(
            "flatpak-spawn",
            &[
                "--host",
                "--watch-bus",
                "--directory=/home/a/src",
                "--env=TERM=xterm-256color",
                "--env=COLORTERM=truecolor",
                "--",
                "/usr/bin/fish",
                "-l"
            ]
        )
    );
    // No directory: no --directory argument at all, and no env: none either.
    assert_eq!(host_command(&shell, None, &[]), expect("flatpak-spawn", &["--host", "--watch-bus", "--", "/usr/bin/fish", "-l"]));
    assert_eq!(
        host_command(&shell, Some(""), &[]),
        host_command(&shell, None, &[]),
        "an empty directory is no directory"
    );
}

#[test]
fn a_directory_that_looks_like_an_option_stays_one_argument() {
    let shell = expect("/bin/sh", &["-l"]);
    for dir in ["--env=X=1", "--host", "--", "-x", "--talk-name=org.freedesktop.Flatpak", "--directory=/etc"] {
        let line = host_command(&shell, Some(dir), &[]);
        assert_eq!(
            line.args,
            strings(&["--host", "--watch-bus", &format!("--directory={dir}"), "--", "/bin/sh", "-l"]),
            "{dir}"
        );
        // Exactly one argument is a directory option, and it carries the whole value.
        assert_eq!(line.args.iter().filter(|a| a.starts_with("--directory=")).count(), 1, "{dir}");
    }
}

#[test]
fn a_directory_with_spaces_and_an_equals_sign_stays_one_argument() {
    let shell = expect("/bin/sh", &["-l"]);
    for dir in ["/home/a/my projects/a=b", "/tmp/a b/--env=X=1", "/tmp/=", "/tmp/ä ö ü=1 2", "/tmp/a;b&c|d$(x)`y`'z\""] {
        let line = host_command(&shell, Some(dir), &[]);
        assert_eq!(
            line.args,
            strings(&["--host", "--watch-bus", &format!("--directory={dir}"), "--", "/bin/sh", "-l"]),
            "{dir}"
        );
    }
}

#[test]
fn a_directory_with_a_control_character_or_nul_is_refused_and_the_pane_starts_without_one() {
    let shell = expect("/bin/sh", &["-l"]);
    let without = host_command(&shell, None, &[]);
    for dir in ["/tmp/a\nb", "/tmp/a\0b", "/tmp/a\x1b[31m", "/tmp/a\rb", "\u{7f}", "/tmp/\tx", "/tmp/a\u{85}b"] {
        assert_eq!(host_command(&shell, Some(dir), &[]), without, "{dir:?}");
    }
}

#[test]
fn nothing_after_the_separator_is_an_option() {
    // Whatever the program and its arguments look like, they are after the one `--` and unchanged, and the options
    // before it are the constants plus the validated ones.
    let hostile = expect("--host", &["--env=A=1", "--directory=/", "--", "-l", "--talk-name=x", "--forward-fd=3"]);
    let line = host_command(&hostile, Some("/home/a"), &pairs(&[("TERM", "xterm")]));
    let separator = line.args.iter().position(|a| a == "--").expect("a separator");
    assert_eq!(line.args.iter().filter(|a| *a == "--").count(), 2, "the hostile list holds a second `--` as data");
    assert_eq!(separator, 4, "the first `--` ends the options: host, watch-bus, directory, one env");
    assert_eq!(&line.args[..separator], strings(&["--host", "--watch-bus", "--directory=/home/a", "--env=TERM=xterm"]).as_slice());
    let mut after = vec![hostile.program.clone()];
    after.extend(hostile.args.clone());
    assert_eq!(&line.args[separator + 1..], after.as_slice(), "program and arguments are passed through unchanged");
    assert_eq!(line.program, "flatpak-spawn", "the program of the line is the constant, never the user's");
}

#[test]
fn an_env_pair_with_a_bad_name_or_a_value_with_a_control_character_is_dropped() {
    let shell = expect("/bin/sh", &["-l"]);
    let line = host_command(
        &shell,
        None,
        &pairs(&[
            ("TERM", "xterm-256color"),
            ("", "x"),
            ("A=B", "x"),
            ("--host", "x"),
            ("1A", "x"),
            ("A B", "x"),
            ("OK_1", "a=b c"),
            ("NL", "a\nb"),
            ("NUL", "a\0b"),
        ]),
    );
    assert_eq!(
        line.args,
        strings(&["--host", "--watch-bus", "--env=TERM=xterm-256color", "--env=OK_1=a=b c", "--", "/bin/sh", "-l"])
    );
}

#[test]
fn without_the_shell_setting_the_host_login_shell_comes_from_one_constant_script() {
    let default = host_shell_launch("");
    assert_eq!(default, expect("sh", &["-c", HOST_LOGIN_SHELL_SCRIPT]));
    assert!(HOST_LOGIN_SHELL_SCRIPT.contains("getent passwd"), "the user's shell is asked of the host's passwd database");
    assert!(HOST_LOGIN_SHELL_SCRIPT.contains("/bin/sh"), "and falls back to /bin/sh");
    assert!(HOST_LOGIN_SHELL_SCRIPT.contains("exec \"$s\" -l"), "and is exec'd as a login shell");
    // One constant: nothing of the user can be in it, and it is not built from parts.
    assert!(!HOST_LOGIN_SHELL_SCRIPT.contains('{') && !HOST_LOGIN_SHELL_SCRIPT.contains("%s"), "no template slot");
    assert!(!HOST_LOGIN_SHELL_SCRIPT.contains('\n'), "one line");
}

#[test]
fn with_the_shell_setting_the_program_of_the_setting_is_used_with_the_arguments_of_its_stem() {
    assert_eq!(host_shell_launch("/usr/bin/fish"), expect("/usr/bin/fish", &["-l"]));
    assert_eq!(host_shell_launch("zsh"), expect("zsh", &["-l"]));
    // The setting is one program, never split: spaces stay inside it.
    assert_eq!(host_shell_launch("/opt/my shell/sh"), expect("/opt/my shell/sh", &["-l"]));
}

#[test]
fn the_default_host_launch_wrapped_is_what_the_flatpak_runs() {
    let line = host_command(&host_shell_launch(""), Some("/home/a"), &pairs(&[("TERM", "xterm-256color")]));
    assert_eq!(
        line.args,
        strings(&[
            "--host",
            "--watch-bus",
            "--directory=/home/a",
            "--env=TERM=xterm-256color",
            "--",
            "sh",
            "-c",
            HOST_LOGIN_SHELL_SCRIPT
        ])
    );
}
