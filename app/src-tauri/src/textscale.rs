//! The text size the operating system asks for (DESK-24, Phase 2.2): the page multiplies its UI scale by it, for the
//! chrome only, never for the terminal grid.
//!
//! macOS has no such signal and answers 100. Windows keeps the "Text size" of its accessibility settings as the
//! registry value `TextScaleFactor` (a percent, 100 to 225), read through `reg.exe`; GNOME keeps `text-scaling-factor`
//! (a factor, 1.0 and up), read through `gsettings`. Both programs are started by absolute path with a cleared
//! environment and closed standard input, wait at most [`DEADLINE`] and are read to at most [`OUTPUT_LIMIT`] bytes.
//! Whatever goes wrong (no such program, no such key, garbage, a hang) is 100: no text size is never an error.
//!
//! The decisions are functions of data: the parsers take the text a program printed, the vector builders return the
//! program and its arguments, [`read_via`] takes the OS name, the environment lookup and the runner. Only [`read`]
//! touches the process.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::state::system_root;

/// What a platform without a text-size signal, or any failure, gives: no change.
pub const NEUTRAL: u32 = 100;

/// The largest answer: the page clamps its effective scale to 300 % as well (T-2.2-24).
pub const LIMIT: u32 = 300;

/// The longest a helper program may run before it is given up.
const DEADLINE: Duration = Duration::from_millis(750);

/// The most output of a helper that is read; both print under a hundred bytes.
const OUTPUT_LIMIT: u64 = 4096;

/// A program to start and its arguments, one entry per argument.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Helper {
    program: String,
    args: Vec<String>,
}

/// `reg.exe query` of the accessibility key's `TextScaleFactor`, under `<SystemRoot>\System32`. `env` looks up a
/// variable of the process; only the Windows directory comes from it.
fn windows_helper(env: &dyn Fn(&str) -> Option<String>) -> Helper {
    Helper {
        program: format!(r"{}\System32\reg.exe", system_root(env)),
        args: ["query", r"HKCU\Software\Microsoft\Accessibility", "/v", "TextScaleFactor"]
            .map(str::to_owned)
            .into(),
    }
}

/// The percent in the output of `reg query ... /v TextScaleFactor`, whose value line reads
/// `    TextScaleFactor    REG_DWORD    0x96` (hexadecimal; a decimal value is read too). A missing key or value
/// (the tool prints an error text to the error stream and ends with status 1, so nothing is left here) and anything
/// else is 100; a value is clamped to 100 to 300.
fn parse_windows(output: &str) -> u32 {
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let (Some(name), Some(kind), Some(value), None) = (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if !name.eq_ignore_ascii_case("TextScaleFactor") || !kind.eq_ignore_ascii_case("REG_DWORD") {
            continue;
        }
        let parsed = match value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")) {
            Some(hex) => u32::from_str_radix(hex, 16),
            None => value.parse(),
        };
        return parsed.map_or(NEUTRAL, |percent| percent.clamp(NEUTRAL, LIMIT));
    }
    NEUTRAL
}

/// A command for `helper`: absolute program, no inherited environment, nothing on standard input or error.
fn command_of(helper: &Helper) -> Command {
    let mut command = Command::new(&helper.program);
    command.args(&helper.args).env_clear().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        // `reg.exe` is a console program: without this flag a window flashes up from this window application.
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}

/// Run `command` and return what it wrote to standard output (at most [`OUTPUT_LIMIT`] bytes), or `None` when it
/// could not be started or did not finish its output within `deadline`.
///
/// A child that is over time is killed and reaped on a short-lived thread, so the caller never waits for it: a
/// process in an uninterruptible call does not die on a signal, and waiting for it would be the hang this function
/// exists to avoid (the same shape as `output_within` of the core's `pty.rs`, which is private to that file).
fn output_within(mut command: Command, deadline: Duration) -> Option<Vec<u8>> {
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    let reader = thread::Builder::new().name("holzwave-textscale".into()).spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.take(OUTPUT_LIMIT).read_to_end(&mut out);
        let _ = tx.send(out);
    });
    let output = match reader {
        Ok(_) => rx.recv_timeout(deadline).ok(),
        Err(_) => None,
    };
    // After the end of its output a child is gone or about to be; one that is not is killed like an overdue one.
    if output.is_none() || !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
        let _ = thread::Builder::new().name("holzwave-reap".into()).spawn(move || {
            let _ = child.wait();
        });
    }
    output
}

/// The percent for `os` (as `std::env::consts::OS` spells it): the parsed answer of the platform's helper, run by
/// `run`, which gives `None` for a helper that did not answer. Every other platform answers 100 without running
/// anything.
fn read_via(os: &str, env: &dyn Fn(&str) -> Option<String>, run: &dyn Fn(&Helper) -> Option<Vec<u8>>) -> u32 {
    let (helper, parse): (Helper, fn(&str) -> u32) = match os {
        "windows" => (windows_helper(env), parse_windows),
        _ => return NEUTRAL,
    };
    run(&helper).map_or(NEUTRAL, |bytes| parse(&String::from_utf8_lossy(&bytes)))
}

/// The OS text size in percent, 100 to 300, 100 where the platform has none or it could not be read. Blocks for up to
/// [`DEADLINE`]: call it off the main thread.
pub fn read(os: &str) -> u32 {
    read_via(os, &|name| std::env::var(name).ok(), &|helper| output_within(command_of(helper), DEADLINE))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Windows: reg.exe -----------------------------------------------------------------------------

    /// What `reg query "HKCU\Software\Microsoft\Accessibility" /v TextScaleFactor` prints for a value of 150 %.
    const REG_150: &str = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Accessibility\r\n    TextScaleFactor    REG_DWORD    0x96\r\n\r\n";

    #[test]
    fn the_windows_value_of_the_registry_line_is_read_as_hexadecimal() {
        assert_eq!(parse_windows(REG_150), 150);
        assert_eq!(parse_windows("    TextScaleFactor    REG_DWORD    0xe1\n"), 225, "the largest the settings page offers");
        assert_eq!(parse_windows("    TextScaleFactor    REG_DWORD    0x64\n"), 100);
    }

    #[test]
    fn the_windows_value_in_decimal_form_is_read_too() {
        assert_eq!(parse_windows("    TextScaleFactor    REG_DWORD    150\n"), 150);
    }

    #[test]
    fn a_windows_value_below_100_is_100_and_above_the_clamp_is_300() {
        assert_eq!(parse_windows("    TextScaleFactor    REG_DWORD    0x32\n"), 100);
        assert_eq!(parse_windows("    TextScaleFactor    REG_DWORD    0x0\n"), 100);
        assert_eq!(parse_windows("    TextScaleFactor    REG_DWORD    0x3e8\n"), 300);
        assert_eq!(parse_windows("    TextScaleFactor    REG_DWORD    0xffffffff\n"), 300);
        assert_eq!(parse_windows("    TextScaleFactor    REG_DWORD    4000000000\n"), 300);
    }

    #[test]
    fn a_missing_windows_key_or_value_and_garbage_are_100() {
        // The tool writes these to the error stream and ends with status 1; they are tested as if they were output.
        for text in [
            "",
            "ERROR: The system was unable to find the specified registry key or value.\r\n",
            "ERROR: Invalid key name.\r\nType \"REG QUERY /?\" for usage.\r\n",
            "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Accessibility\r\n\r\n",
            "    TextScaleFactor    REG_SZ    150\n",
            "    TextScaleFactor    REG_DWORD    banana\n",
            "    TextScaleFactor    REG_DWORD    0x\n",
            "    TextScaleFactor    REG_DWORD    0x96 extra\n",
            "    TextScaleFactor    REG_DWORD    -150\n",
            "    TextScaleFactor    REG_DWORD    0x1ffffffff\n",
            "    OtherValue    REG_DWORD    0x96\n",
            "\u{0}\u{1}\u{2}",
        ] {
            assert_eq!(parse_windows(text), 100, "{text:?}");
        }
    }

    #[test]
    fn the_windows_helper_is_reg_exe_by_absolute_path_with_its_exact_arguments() {
        let none = |_: &str| None;
        let helper = windows_helper(&none);
        assert_eq!(helper.program, r"C:\Windows\System32\reg.exe", "SystemRoot defaults to C:\\Windows");
        assert_eq!(helper.args, ["query", r"HKCU\Software\Microsoft\Accessibility", "/v", "TextScaleFactor"]);
        let root = |name: &str| (name == "SystemRoot").then(|| r"D:\WinNT\".to_owned());
        assert_eq!(windows_helper(&root).program, r"D:\WinNT\System32\reg.exe");
        for hostile in [r"\\evil\share", "reg", "", r"..\..\bin"] {
            let env = |name: &str| (name == "SystemRoot").then(|| hostile.to_owned());
            assert_eq!(windows_helper(&env).program, r"C:\Windows\System32\reg.exe", "SystemRoot {hostile:?}");
        }
    }

    // ---- the reader -----------------------------------------------------------------------------------

    /// A runner that fails the test when it is called, and one that answers `text`.
    fn never(_: &Helper) -> Option<Vec<u8>> {
        panic!("a helper was started");
    }

    #[test]
    fn macos_and_every_other_platform_answer_100_without_starting_anything() {
        let none = |_: &str| None;
        for os in ["macos", "linux", "freebsd", "ios", "", "WINDOWS"] {
            assert_eq!(read_via(os, &none, &never), 100, "{os:?}");
        }
        assert_eq!(read("macos"), 100);
    }

    #[test]
    fn each_platform_runs_its_own_helper_and_reads_its_own_format() {
        let none = |_: &str| None;
        let reg = |helper: &Helper| {
            assert!(helper.program.ends_with(r"\System32\reg.exe"), "{helper:?}");
            Some(REG_150.as_bytes().to_vec())
        };
        assert_eq!(read_via("windows", &none, &reg), 150);
    }

    #[test]
    fn a_helper_that_did_not_answer_or_printed_garbage_gives_100() {
        let none = |_: &str| None;
        assert_eq!(read_via("windows", &none, &|_| None), 100);
        assert_eq!(read_via("windows", &none, &|_| Some(vec![0xff, 0xfe, 0x00])), 100);
        assert_eq!(read_via("windows", &none, &|_| Some(Vec::new())), 100);
    }

    #[test]
    fn a_missing_helper_cannot_be_started_and_answers_none() {
        let helper = Helper { program: "/nonexistent/holzwave-helper".to_owned(), args: Vec::new() };
        assert_eq!(output_within(command_of(&helper), Duration::from_secs(1)), None);
    }
}
