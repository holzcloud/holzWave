//! Starting the spawn helper (Linux): the control socket is the helper's standard input, the helper's early exit is reported
//! with its status, a helper that never answers is killed and reaped, and no descriptor stays open in the server.
//!
//! The helper is a stand-in shell script: the real one needs file capabilities and runs in the image. The script's answer to
//! the supervisor is the silence or the exit, which is all these tests need.
#![cfg(target_os = "linux")]

#[path = "support/scratch.rs"]
mod scratch;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use holzwave_server::supervisor::{HelperSettings, HelperTimeouts};
use holzwave_server::supervisor::HelperClient;

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("a script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("executable");
    path
}

fn settings(program: PathBuf) -> HelperSettings {
    HelperSettings {
        program,
        worker_path: PathBuf::from("/opt/holzwave/worker"),
        home_base: PathBuf::from("/data/homes"),
        timeouts: HelperTimeouts { request: Duration::from_millis(500), start: Duration::from_millis(500) },
        ..HelperSettings::default()
    }
}

fn open_descriptors() -> usize {
    std::fs::read_dir("/proc/self/fd").expect("the descriptor list").count()
}

fn alive(pid: i32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[tokio::test(flavor = "multi_thread")]
async fn start_reports_an_early_exit_a_silence_and_the_arguments_and_leaves_nothing_open() {
    let scratch = scratch::Scratch::new("helper-start");
    let dir = scratch.path();
    let before = open_descriptors();

    // A helper that exits with status 3 at once: the error names the status and the helper's path.
    let program = script(dir, "exits", "exit 3");
    let error = HelperClient::start(&settings(program.clone())).await.err().expect("an early exit is an error");
    assert!(error.sentence().contains("status 3"), "{error}");
    assert!(error.sentence().contains(&program.display().to_string()), "{error}");

    // A helper that records what it was given and exits: the control descriptor is named and is its standard input.
    let args_file = dir.join("args.txt");
    let fd_file = dir.join("fd0.txt");
    let program = script(
        dir,
        "records",
        &format!("echo \"$@\" > '{}'; readlink /proc/self/fd/0 > '{}'; exit 5", args_file.display(), fd_file.display()),
    );
    let error = HelperClient::start(&settings(program)).await.err().expect("an exit");
    assert!(error.sentence().contains("status 5"), "{error}");
    let args = std::fs::read_to_string(&args_file).expect("the arguments");
    assert!(args.contains("--control-fd 0"), "{args}");
    assert!(args.contains("--worker-path /opt/holzwave/worker"), "{args}");
    assert!(args.contains("--home-base /data/homes"), "{args}");
    assert!(std::fs::read_to_string(&fd_file).expect("standard input").starts_with("socket:"), "standard input is the control socket");

    // A helper that never answers: the timeout error, and no child process is left, not even one to be reaped.
    let pid_file = dir.join("pid.txt");
    let program = script(dir, "silent", &format!("echo $$ > '{}'; exec sleep 30", pid_file.display()));
    let started = Instant::now();
    let error = HelperClient::start(&settings(program)).await.err().expect("a silence is an error");
    assert!(error.sentence().contains("did not answer"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    let pid: i32 = std::fs::read_to_string(&pid_file).expect("the pid").trim().parse().expect("a number");
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) {
        assert!(Instant::now() < deadline, "the stand-in helper {pid} is still there");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A program that cannot be started.
    let error = HelperClient::start(&settings(dir.join("missing"))).await.err().expect("no such program");
    assert!(error.sentence().contains("could not be started"), "{error}");

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(open_descriptors(), before, "no descriptor of the control socket stays open in the server");
}
