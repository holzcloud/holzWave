//! A pane in host-spawn mode (a Flatpak session), end to end on this host with a stand-in for `flatpak-spawn`.
//!
//! The real `flatpak-spawn --host` exists only inside a Flatpak; the stand-in is a small `sh` script that records its
//! arguments, does what the real one does with them (changes into the `--directory`, sets the `--env` pairs) and runs
//! the command after `--`. What this proves: the pane starts `flatpak-spawn` with the documented line, the user's
//! program runs behind the separator in the directory that was asked for (spaces, an equals sign and an option-like
//! part included), and the child gets the terminal variables as options. What it cannot prove is the PTY behaviour of
//! the real portal (a controlling terminal, job control): that is row L10 of `docs/test-plan.md`.
//!
//! The stand-in has to be found on `PATH`, and the tests of one binary share the process environment, so the outer
//! test starts this executable again with a `PATH` that has the stand-in first and the inner test does the work (no
//! `unsafe` environment change in a test).
#![cfg(feature = "ghostty")]
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use common::{scratch_dir, wait_until};
use holzwave_core::pane::{PaneHandle, PaneSpec};

const INNER: &str = "inner_a_host_spawn_pane_starts_the_command_behind_flatpak_spawn_in_the_directory";
const WORK: &str = "HOLZWAVE_TEST_HOST_SPAWN_WORK";

/// Records its arguments, then acts like `flatpak-spawn --host`: change into `--directory`, set the `--env` pairs, run
/// what follows `--`. It reads `FAKE_LOG` (where to record) from the environment of the run.
const STAND_IN: &str = r#"#!/bin/sh
printf '%s\n' "$@" > "$FAKE_LOG"
# The real one does not forward its own environment: the host command gets only what `--env` says.
unset TERM COLORTERM TERM_PROGRAM TERM_PROGRAM_VERSION
dir=
while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do
  case "$1" in
    --directory=*) dir=${1#--directory=} ;;
    --env=*) export "${1#--env=}" ;;
  esac
  shift
done
shift
if [ -n "$dir" ]; then cd "$dir" || exit 97; fi
exec "$@"
"#;

#[test]
fn a_host_spawn_pane_runs_end_to_end_with_a_stand_in_for_flatpak_spawn() {
    let work = scratch_dir("host-spawn");
    let bin = work.join("bin");
    std::fs::create_dir(&bin).expect("create bin");
    let script = bin.join("flatpak-spawn");
    std::fs::write(&script, STAND_IN).expect("write the stand-in");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    let output = Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--exact", INNER, "--test-threads=1"])
        .env("PATH", path)
        .env(WORK, &work)
        .env("FAKE_LOG", work.join("args.txt"))
        .output()
        .expect("run the inner test");
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    // `--exact` with a name that matches nothing runs zero tests and succeeds: that must not pass.
    assert!(text.contains("1 passed"), "the inner test did not run and pass:\n{text}\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "the inner test failed:\n{text}");
    let _ = std::fs::remove_dir_all(&work);
}

/// The inner test: it needs the stand-in on `PATH`, so on its own (no `WORK` variable) it has nothing to do.
#[test]
fn inner_a_host_spawn_pane_starts_the_command_behind_flatpak_spawn_in_the_directory() {
    let Some(work) = std::env::var_os(WORK).map(PathBuf::from) else { return };
    // A directory with spaces, an equals sign and a part that looks like an option: one argument, and the stand-in
    // changes into it.
    let dir = work.join("my dir=x --env=Y=1");
    std::fs::create_dir(&dir).expect("create the directory");
    let (log, out) = (work.join("args.txt"), work.join("out.txt"));
    let script = format!(
        "pwd > '{out}'; printf '%s %s %s' \"$TERM\" \"$COLORTERM\" \"$TERM_PROGRAM\" >> '{out}'",
        out = out.display()
    );

    let mut spec = PaneSpec::new("sh", 80, 24).args(["-c", &script]).cwd(&dir);
    spec.host_spawn = true;
    spec.cwd_lookup = false;
    let pane = PaneHandle::spawn_ghostty(1, spec, |_| {}).expect("the pane starts");

    assert!(wait_until(Duration::from_secs(8), || out.exists() && std::fs::read_to_string(&out).is_ok_and(|t| t.lines().count() >= 2)), "the command did not run");
    let recorded = std::fs::read_to_string(&log).expect("the stand-in recorded its arguments");
    let lines: Vec<&str> = recorded.lines().collect();
    let separator = lines.iter().position(|l| *l == "--").expect("a separator");
    assert_eq!(&lines[..3], ["--host", "--watch-bus", &format!("--directory={}", dir.display())]);
    assert!(lines[3..separator].iter().all(|l| l.starts_with("--env=")), "only env options follow: {lines:?}");
    assert_eq!(&lines[separator + 1..separator + 3], ["sh", "-c"]);
    assert_eq!(lines[separator + 3], script, "the user's script is one argument, unchanged");

    let result = std::fs::read_to_string(&out).expect("the command wrote its result");
    let mut result_lines = result.lines();
    assert_eq!(
        std::fs::canonicalize(result_lines.next().unwrap_or_default()).expect("the shell's directory exists"),
        std::fs::canonicalize(&dir).expect("canonical"),
        "the command runs in the directory that was asked for"
    );
    assert_eq!(result_lines.next(), Some("xterm-256color truecolor holzWave"), "the terminal variables arrived as options");
    pane.close();
    assert!(pane.join(Duration::from_secs(3)));
}
