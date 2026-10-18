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

    use holzwave_core::integrate::run::{Env, RunCancel, RunEnd, RunLimits, Runner};

    use crate::common::scratch_dir;
    use crate::fake_cli::FakeCli;

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
            .run_argv(&fake.path(), &args, plain_env(), &RunLimits::default(), &RunCancel::default())
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
        let started = std::time::Instant::now();
        let result = Runner::new()
            .run_argv(&fake.path(), &[], plain_env(), &RunLimits::default(), &RunCancel::default())
            .expect("the fake runs");
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "cat on a closed input returns at once");
        assert_eq!(result.end, RunEnd::Finished);
        assert!(result.stdout.is_empty(), "nothing came in, so nothing came out");
    }

    #[test]
    fn the_child_leads_its_own_group_and_has_no_shell_in_between() {
        let dir = Scratch::new();
        let fake = FakeCli::new(&dir.0, "tool");
        let result = Runner::new()
            .run_argv(&fake.path(), &[], plain_env(), &RunLimits::default(), &RunCancel::default())
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
}
