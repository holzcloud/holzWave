//! The runner of the integrations: one program by its absolute path with an argument list and nothing else, an
//! environment built by a policy, output with a cap that fails, a kill of everything the call started, and a limit on
//! how many children run at once.
//!
//! The tests that start a program use fake programs from the rig (POSIX shell scripts in a scratch directory) and
//! are Unix only. The pure tests (the environment policy, the batch-file charset, the spawn gate) run on every host.

mod common;
#[cfg(unix)]
#[path = "common/fake_cli.rs"]
mod fake_cli;

#[cfg(unix)]
mod unix {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;
    use std::sync::Barrier;
    use std::time::{Duration, Instant};

    use holzwave_core::integrate::run::{Env, RunCancel, RunEnd, RunError, RunLimits, Runner};

    use crate::common::{scratch_dir, wait_until};
    use crate::fake_cli::{pattern, process_exists, FakeCli};

    /// A scratch directory that is removed when the test ends, even when it fails.
    pub struct Scratch(pub PathBuf);

    impl Scratch {
        pub fn new() -> Scratch {
            Scratch(scratch_dir("fake_cli"))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The environment of a test run: the system directories on the `PATH` so that the fake's helpers are found.
    pub fn plain_env() -> Env {
        Env::from_pairs(vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())])
    }

    pub fn os(text: &str) -> OsString {
        OsString::from(text)
    }

    #[test]
    fn arguments_reach_the_program_exactly_as_given() {
        let dir = Scratch::new();
        let fake = FakeCli::new(&dir.0, "kubectl").prints("ok");
        let args: Vec<OsString> = [
            "get",
            "pods",
            "--context=a b",
            "line\nbreak",
            "$HOME",
            "*",
            "it's \"quoted\"",
            "`id`",
            "a;b|c&d",
            "",
        ]
        .iter()
        .map(|a| os(a))
        .collect();
        let result = Runner::new()
            .run_argv(&fake.path(), &args, plain_env(), &limits(MIB, 60_000), &RunCancel::default())
            .expect("the fake runs");
        assert_eq!(result.end, RunEnd::Finished);
        assert_eq!(result.status, Some(0));
        assert_eq!(result.stdout, b"ok");
        let recorded = fake.argv();
        let sent: Vec<Vec<u8>> = args.iter().map(|a| a.clone().into_vec()).collect();
        assert_eq!(recorded, sent, "every argument arrives byte for byte, none expanded or split");
    }

    #[test]
    fn standard_input_is_closed() {
        let dir = Scratch::new();
        let fake = FakeCli::new(&dir.0, "tool").reads_stdin();
        // A `cat` that inherited a terminal or a pipe would wait for input until the time limit ends it.
        let result = Runner::new()
            .run_argv(&fake.path(), &[], plain_env(), &limits(MIB, 30_000), &RunCancel::default())
            .expect("the fake runs");
        assert_eq!(result.end, RunEnd::Finished, "cat on a closed input returns at once");
        assert!(result.stdout.is_empty(), "nothing came in, so nothing came out");
    }

    #[test]
    fn the_child_leads_its_own_group_and_has_no_shell_in_between() {
        let dir = Scratch::new();
        let fake = FakeCli::new(&dir.0, "tool");
        let result = Runner::new()
            .run_argv(&fake.path(), &[], plain_env(), &limits(MIB, 60_000), &RunCancel::default())
            .expect("the fake runs");
        assert_eq!(result.end, RunEnd::Finished);
        assert_eq!(fake.parent_pid(), Some(std::process::id()), "the fake's parent is this process: no shell between");
        let own_group = std::process::Command::new("/bin/ps")
            .args(["-o", "pgid=", "-p", &std::process::id().to_string()])
            .output()
            .expect("ps");
        let own_group: u32 = String::from_utf8_lossy(&own_group.stdout).trim().parse().expect("a group id");
        let child_group = fake.group_id().expect("the fake recorded its group");
        assert_ne!(child_group, own_group, "the child leads a group of its own");
    }

    #[test]
    fn a_program_that_is_not_an_absolute_path_is_refused_before_it_starts() {
        let result = Runner::new().run_argv(
            std::path::Path::new("kubectl"),
            &[],
            plain_env(),
            &RunLimits::default(),
            &RunCancel::default(),
        );
        assert!(result.is_err(), "a bare name is looked up on a PATH anything can write to: refused");
    }

    /// Whether any process of the group `pgid` exists (signal 0 to the group through the system `kill`).
    fn group_exists(pgid: u32) -> bool {
        std::process::Command::new("/bin/sh")
            .args(["-c", "kill -0 -- \"-$1\"", "sh", &pgid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn limits(max_stdout: usize, timeout_ms: u64) -> RunLimits {
        RunLimits { max_stdout, timeout: Duration::from_millis(timeout_ms) }
    }

    const MIB: usize = 1024 * 1024;

    #[test]
    fn output_over_the_cap_fails_and_keeps_the_head() {
        let dir = Scratch::new();
        // The fake sleeps after it printed: a runner that did not kill the group would wait for the sleep.
        let fake = FakeCli::new(&dir.0, "tool").prints_bytes(20 * MIB).sleeps(30);
        let started = Instant::now();
        let result = Runner::new()
            .run_argv(&fake.path(), &[], plain_env(), &limits(MIB, 60_000), &RunCancel::default())
            .expect("the fake runs");
        assert!(started.elapsed() < Duration::from_secs(10), "the call did not wait for the sleep");
        assert_eq!(result.end, RunEnd::Overflow);
        assert!(result.stdout_dropped);
        assert_eq!(result.stdout.len(), MIB, "exactly the cap is kept");
        assert!(result.stdout == pattern(MIB), "the kept bytes are the head of the output, not the tail");
        let group = fake.group_id().expect("the fake recorded its group");
        assert!(wait_until(Duration::from_secs(3), || !group_exists(group)), "the process group is gone");
    }

    #[test]
    fn stderr_keeps_its_start() {
        let dir = Scratch::new();
        let fake = FakeCli::new(&dir.0, "tool").prints("fine").prints_stderr_bytes(MIB);
        let result = Runner::new()
            .run_argv(&fake.path(), &[], plain_env(), &limits(MIB, 60_000), &RunCancel::default())
            .expect("the fake runs");
        assert_eq!(result.end, RunEnd::Finished, "a long stderr is no failure");
        assert_eq!(result.status, Some(0));
        assert_eq!(result.stdout, b"fine");
        assert_eq!(result.stderr.len(), 64 * 1024);
        assert!(result.stderr == pattern(64 * 1024), "stderr keeps its first 64 KiB");
        assert!(!result.stdout_dropped);
    }

    #[test]
    fn a_timeout_kills_the_whole_process_group() {
        let dir = Scratch::new();
        let fake = FakeCli::new(&dir.0, "tool").sleeps(30);
        let started = Instant::now();
        let result = Runner::new()
            .run_argv(&fake.path(), &[], plain_env(), &limits(MIB, 1_500), &RunCancel::default())
            .expect("the fake runs");
        assert_eq!(result.end, RunEnd::TimedOut);
        assert_eq!(result.status, None);
        assert!(started.elapsed() < Duration::from_secs(10), "the call returned long before the sleep ended");
        let group = fake.group_id().expect("the fake recorded its group");
        assert!(wait_until(Duration::from_secs(3), || !group_exists(group)), "nothing of the group is left");

        // A program that started a child of its own leaves no child behind.
        let parent = FakeCli::new(&dir.0, "parent").spawns_a_sleeping_child(30);
        let result = Runner::new()
            .run_argv(&parent.path(), &[], plain_env(), &limits(MIB, 2_500), &RunCancel::default())
            .expect("the fake runs");
        assert_eq!(result.end, RunEnd::TimedOut);
        let child = parent.child_pid().expect("the fake started its child before the limit");
        assert!(wait_until(Duration::from_secs(3), || !process_exists(child)), "the sleeping child is gone");
    }

    #[test]
    fn a_cancel_stops_the_run_and_its_children() {
        let dir = Scratch::new();
        let parent = FakeCli::new(&dir.0, "parent").spawns_a_sleeping_child(30);
        let cancel = RunCancel::default();
        let raised = Instant::now();
        let result = std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                Runner::new().run_argv(&parent.path(), &[], plain_env(), &limits(MIB, 60_000), &cancel)
            });
            assert!(wait_until(Duration::from_secs(30), || parent.child_pid().is_some()), "the fake started its child");
            cancel.cancel();
            handle.join().expect("the run did not panic")
        })
        .expect("the fake runs");
        assert_eq!(result.end, RunEnd::Cancelled);
        assert_eq!(result.status, None);
        assert!(raised.elapsed() < Duration::from_secs(40));
        let child = parent.child_pid().expect("the child pid");
        assert!(wait_until(Duration::from_secs(3), || !process_exists(child)), "the child is gone after a cancel");

        // A switch raised before the call: no process is started.
        let fake = FakeCli::new(&dir.0, "never");
        let early = RunCancel::default();
        early.cancel();
        let result = Runner::new()
            .run_argv(&fake.path(), &[], plain_env(), &RunLimits::default(), &early)
            .expect("an answer, not an error");
        assert_eq!(result.end, RunEnd::Cancelled);
        assert!(!fake.started(), "nothing was started");
    }

    #[test]
    fn at_most_four_children_run_at_once() {
        let dir = Scratch::new();
        let fake = FakeCli::new(&dir.0, "tool").sleeps(2);
        let runner = Runner::new();
        let barrier = Barrier::new(5);
        let calls: Vec<(Result<RunEnd, RunError>, Instant, Duration)> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..5)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        let started = Instant::now();
                        let answer = runner
                            .run_argv(&fake.path(), &[], plain_env(), &limits(MIB, 60_000), &RunCancel::default())
                            .map(|result| result.end);
                        (answer, Instant::now(), started.elapsed())
                    })
                })
                .collect();
            handles.into_iter().map(|handle| handle.join().expect("no panic")).collect()
        });
        let busy: Vec<_> = calls.iter().filter(|(answer, _, _)| *answer == Err(RunError::Busy)).collect();
        let ran: Vec<_> = calls.iter().filter(|(answer, _, _)| *answer == Ok(RunEnd::Finished)).collect();
        assert_eq!(busy.len(), 1, "one call is refused: {calls:?}");
        assert_eq!(ran.len(), 4, "four run: {calls:?}");
        assert!(busy[0].2 >= Duration::from_millis(900), "the fifth waited for a slot for about a second");
        let first_end = ran.iter().map(|(_, end, _)| *end).min().expect("four ended");
        assert!(busy[0].1 < first_end, "the refusal came while the four were still running");
        // The slots are free again: a sixth call runs.
        let again = runner
            .run_argv(&fake.path(), &[], plain_env(), &limits(MIB, 60_000), &RunCancel::default())
            .expect("a slot is free");
        assert_eq!(again.end, RunEnd::Finished);
    }
}
