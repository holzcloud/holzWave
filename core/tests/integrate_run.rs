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

    use zeroize::Zeroizing;

    use holzwave_core::integrate::run::{Env, EnvExtra, EnvPolicy, RunCancel, RunEnd, RunError, RunLimits, Runner};
    use holzwave_core::platform::Os;

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
        EnvPolicy::build(std::iter::empty(), Os::current(), "/usr/bin:/bin".to_owned(), EnvExtra::default())
            .expect("an environment of nothing but a PATH")
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

    /// The secret of the canary tests: a value that must be found in the child's environment and nowhere else.
    const CANARY: &str = "hw-canary-secret-value-0001";

    #[test]
    fn a_secret_reaches_the_child_environment_only() {
        let dir = Scratch::new();
        let fake = FakeCli::new(&dir.0, "tool");
        let extra = EnvExtra {
            secrets: vec![("AWS_SECRET_ACCESS_KEY".to_owned(), Zeroizing::new(CANARY.to_owned()))],
            ..EnvExtra::default()
        };
        assert!(!format!("{extra:?}").contains(CANARY), "the debug text of the extras holds no value");
        let inherited = vec![
            (os("HOLZWAVE_TOKEN"), os("hw-canary-token")),
            (os("LD_PRELOAD"), os("/tmp/evil.so")),
            (os("GIT_SSH_COMMAND"), os("evil")),
            (os("HOME"), os("/home/user")),
            (os("AZURE_CLIENT_SECRET"), os("hw-canary-azure")),
        ];
        let env = EnvPolicy::build(inherited, Os::Linux, "/usr/bin:/bin".to_owned(), extra).expect("a valid environment");
        assert!(!format!("{env:?}").contains(CANARY), "the debug text of an environment holds no value");
        let args = [os("get"), os("pods")];
        let result = Runner::new()
            .run_argv(&fake.path(), &args, env, &limits(MIB, 60_000), &RunCancel::default())
            .expect("the fake runs");
        assert_eq!(result.end, RunEnd::Finished);
        assert!(!format!("{result:?}").contains(CANARY));
        let environment = fake.env();
        assert_eq!(environment.get("AWS_SECRET_ACCESS_KEY").map(String::as_str), Some(CANARY), "the secret is in the child");
        assert_eq!(environment.get("AZURE_CLIENT_SECRET").map(String::as_str), Some("hw-canary-azure"));
        assert_eq!(environment.get("HOME").map(String::as_str), Some("/home/user"));
        for denied in ["HOLZWAVE_TOKEN", "LD_PRELOAD", "GIT_SSH_COMMAND"] {
            assert!(!environment.contains_key(denied), "{denied} never reaches the child");
        }
        assert_eq!(environment.get("PATH").map(String::as_str), Some("/usr/bin:/bin"), "the PATH is the rebuilt one");
        for argument in fake.argv() {
            assert!(!String::from_utf8_lossy(&argument).contains(CANARY), "the value is in no argument");
        }
        assert!(!String::from_utf8_lossy(&result.stdout).contains(CANARY));
        assert!(!String::from_utf8_lossy(&result.stderr).contains(CANARY));
    }

    #[test]
    fn a_refused_environment_says_nothing_of_its_input() {
        let secret = |name: &str| EnvExtra {
            secrets: vec![(name.to_owned(), Zeroizing::new(CANARY.to_owned()))],
            ..EnvExtra::default()
        };
        for name in ["LD_PRELOAD", "lower", "A=B", "", "KUBECONFIG"] {
            let error = EnvPolicy::build(std::iter::empty(), Os::Linux, "/bin".to_owned(), secret(name))
                .expect_err("a secret may not take this name");
            let text = format!("{error} {error:?}");
            assert!(!text.contains(CANARY), "the refusal holds the value");
            if !name.is_empty() {
                assert!(!text.contains(name), "the refusal holds the name {name}");
            }
        }
    }
}

// ---- the pure tests: they run on every host --------------------------------------------------------------------------

mod pure {
    use std::ffi::OsString;
    use std::path::Path;

    use holzwave_core::integrate::run::{check_batch_args, EnvExtra, EnvPolicy, RunError};
    use holzwave_core::platform::Os;
    use zeroize::Zeroizing;

    fn os(text: &str) -> OsString {
        OsString::from(text)
    }

    fn pairs(names: &[&str]) -> Vec<(OsString, OsString)> {
        names.iter().map(|name| (os(name), os(&format!("value-of-{name}")))).collect()
    }

    const DROPPED: [&str; 10] = [
        "HOLZWAVE_IPC",
        "HOLZWAVE_TOKEN",
        "LD_PRELOAD",
        "DYLD_INSERT_LIBRARIES",
        "PYTHONSTARTUP",
        "NODE_OPTIONS",
        "GIT_SSH_COMMAND",
        "BASH_ENV",
        "ENV",
        "IFS",
    ];
    const KEPT: [&str; 6] =
        ["AWS_SECRET_ACCESS_KEY", "AZURE_CLIENT_SECRET", "GOOGLE_APPLICATION_CREDENTIALS", "KUBECONFIG", "HOME", "LANG"];

    #[test]
    fn the_environment_policy_keeps_credential_variables_and_drops_loader_and_app_variables() {
        let mut inherited = pairs(&DROPPED);
        inherited.extend(pairs(&["PATH"]));
        inherited.extend(pairs(&KEPT));
        for os_name in [Os::Linux, Os::Macos, Os::Windows] {
            let env = EnvPolicy::build(inherited.clone(), os_name, "/rebuilt:/path".to_owned(), EnvExtra::default())
                .expect("a valid environment");
            let mut expected: Vec<&str> = KEPT.to_vec();
            expected.push("PATH");
            expected.sort_unstable();
            assert_eq!(env.names(), expected, "the kept names on {os_name:?}");
            assert_eq!(env.value("PATH"), Some("/rebuilt:/path"), "PATH is the one given in the call");
            assert_eq!(env.value("AWS_SECRET_ACCESS_KEY"), Some("value-of-AWS_SECRET_ACCESS_KEY"));
            assert_eq!(env.value("GOOGLE_APPLICATION_CREDENTIALS"), Some("value-of-GOOGLE_APPLICATION_CREDENTIALS"));
        }
    }

    #[test]
    fn on_windows_the_same_names_in_other_cases_are_dropped_too_and_on_unix_they_are_not() {
        let other_case = ["ld_preload", "Holzwave_Token", "dyld_insert_libraries", "pythonpath", "Node_Options", "git_dir", "Bash_Env", "env", "Ifs", "Path"];
        let windows = EnvPolicy::build(pairs(&other_case), Os::Windows, "C:\\tool".to_owned(), EnvExtra::default())
            .expect("valid");
        assert_eq!(windows.names(), ["PATH"], "only the rebuilt PATH is left on Windows");
        assert_eq!(windows.value("PATH"), Some("C:\\tool"));
        let linux = EnvPolicy::build(pairs(&other_case), Os::Linux, "/bin".to_owned(), EnvExtra::default()).expect("valid");
        assert_eq!(linux.names().len(), other_case.len() + 1, "names are case-sensitive on Unix: ten kept and the PATH");
        assert_eq!(linux.value("PATH"), Some("/bin"), "the rebuilt PATH, not the inherited Path");
    }

    #[test]
    fn malformed_inherited_entries_are_dropped_and_the_count_is_bounded() {
        let mut inherited = vec![
            (os(""), os("x")),
            (os("A=B"), os("x")),
            (os("A\0B"), os("x")),
            (os("GOOD"), os("a\0b")),
            (os("FINE"), os("yes")),
        ];
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            inherited.push((OsString::from_vec(vec![b'B', 0xff]), os("x")));
            inherited.push((os("BADVALUE"), OsString::from_vec(vec![0xff, 0xfe])));
        }
        let env = EnvPolicy::build(inherited, Os::Linux, "/bin".to_owned(), EnvExtra::default()).expect("valid");
        assert_eq!(env.names(), ["FINE", "PATH"]);
        let many: Vec<(OsString, OsString)> = (0..3000).map(|n| (os(&format!("VAR_{n}")), os("v"))).collect();
        let env = EnvPolicy::build(many, Os::Linux, "/bin".to_owned(), EnvExtra::default()).expect("valid");
        assert!(env.len() <= 1024, "at most 1024 pairs, got {}", env.len());
        assert_eq!(env.value("PATH"), Some("/bin"), "the PATH is always there");
    }

    #[test]
    fn extras_win_over_inherited_variables_and_cannot_name_a_denied_variable_or_path() {
        let extra = EnvExtra {
            plain: vec![("HOME".to_owned(), "/profile".to_owned()), ("AWS_REGION".to_owned(), "eu-1".to_owned())],
            fixed: &[("AZURE_CORE_COLLECT_TELEMETRY", "false")],
            isolation: vec![("KUBECONFIG".to_owned(), "/overlay".to_owned())],
            secrets: Vec::new(),
        };
        let env = EnvPolicy::build(pairs(&["HOME", "KUBECONFIG", "LANG"]), Os::Linux, "/bin".to_owned(), extra).expect("valid");
        assert_eq!(env.value("HOME"), Some("/profile"), "the profile's variable wins");
        assert_eq!(env.value("KUBECONFIG"), Some("/overlay"), "the isolation variable wins");
        assert_eq!(env.value("AZURE_CORE_COLLECT_TELEMETRY"), Some("false"));
        assert_eq!(env.value("LANG"), Some("value-of-LANG"));

        let build = |extra: EnvExtra| EnvPolicy::build(std::iter::empty(), Os::Linux, "/bin".to_owned(), extra);
        let plain = |name: &str| EnvExtra { plain: vec![(name.to_owned(), "v".to_owned())], ..EnvExtra::default() };
        for name in [
            "HOLZWAVE_X", "LD_PRELOAD", "DYLD_FALLBACK_LIBRARY_PATH", "PYTHONPATH", "NODE_OPTIONS", "GIT_ASKPASS", "BASH_ENV", "ENV", "IFS", "PATH",
            "KUBECONFIG", "TALOSCONFIG", "AZURE_CONFIG_DIR", "GAMCFGDIR", "GAMCFGSECTION",
        ] {
            assert_eq!(build(plain(name)).expect_err(name), RunError::EnvName, "a profile may not set {name}");
        }
        for bad in ["lower", "A=B", "", "1X", "A B", "A\0B"] {
            assert_eq!(build(plain(bad)).expect_err(bad), RunError::EnvName, "{bad:?} is no variable name");
        }
        let nul = EnvExtra { plain: vec![("OK".to_owned(), "a\0b".to_owned())], ..EnvExtra::default() };
        assert_eq!(build(nul).expect_err("a NUL in a value"), RunError::EnvValue);
        let long = EnvExtra { plain: vec![("OK".to_owned(), "x".repeat(20_000))], ..EnvExtra::default() };
        assert_eq!(build(long).expect_err("a value over the bound"), RunError::EnvValue);
        let isolation = EnvExtra { isolation: vec![("LD_PRELOAD".to_owned(), "x".to_owned())], ..EnvExtra::default() };
        assert_eq!(build(isolation).expect_err("isolation is no way around the deny list"), RunError::EnvName);
        static EVIL: [(&str, &str); 1] = [("LD_PRELOAD", "x")];
        let fixed = EnvExtra { fixed: &EVIL, ..EnvExtra::default() };
        assert_eq!(build(fixed).expect_err("a fixed variable is no way around it either"), RunError::EnvName);
        let secret = EnvExtra { secrets: vec![("PATH".to_owned(), Zeroizing::new("x".to_owned()))], ..EnvExtra::default() };
        assert_eq!(build(secret).expect_err("a secret named PATH"), RunError::EnvName);
        let many = EnvExtra { plain: (0..300).map(|n| (format!("V_{n}"), "v".to_owned())).collect(), ..EnvExtra::default() };
        assert_eq!(build(many).expect_err("too many extras"), RunError::TooManyVariables);
        assert_eq!(
            EnvPolicy::build(std::iter::empty(), Os::Linux, "a\0b".to_owned(), EnvExtra::default()).expect_err("PATH"),
            RunError::EnvValue
        );
    }

    #[test]
    fn the_names_a_pane_has_removed_are_the_denied_ones_that_are_present() {
        let inherited: Vec<OsString> =
            ["HOLZWAVE_TOKEN", "HOME", "LD_PRELOAD", "PATH", "GIT_DIR", "LD_PRELOAD", "A=B", "", "NODE_OPTIONS"].iter().map(|n| os(n)).collect();
        let removals = EnvPolicy::removals(inherited.clone(), Os::Linux);
        assert_eq!(removals, ["GIT_DIR", "HOLZWAVE_TOKEN", "LD_PRELOAD", "NODE_OPTIONS"], "sorted, once, no PATH, no HOME");
        let windows = EnvPolicy::removals([os("Ld_Preload"), os("Path"), os("Holzwave_Ipc")], Os::Windows);
        assert_eq!(windows, ["Holzwave_Ipc", "Ld_Preload"]);
        assert!(EnvPolicy::removals([os("ld_preload")], Os::Linux).is_empty(), "case-sensitive on Unix");
    }

    #[test]
    fn batch_files_get_a_strict_argument_charset() {
        let args = |list: &[&str]| -> Vec<OsString> { list.iter().map(|a| os(a)).collect() };
        let fine = args(&["account", "show", "-o", "json", "--subscription=0f0f0f0f-0f0f-4f0f-8f0f-0f0f0f0f0f0f"]);
        assert!(check_batch_args("C:\\Tools\\az.cmd", &fine, Os::Windows).is_ok());
        assert!(check_batch_args("C:\\Tools\\AZ.CMD", &fine, Os::Windows).is_ok());
        assert!(check_batch_args("c:\\tools\\gam.Bat", &fine, Os::Windows).is_ok());
        assert!(check_batch_args("x.cmd", &args(&["a.b:c/d@e=f+g,h_i-j"]), Os::Windows).is_ok(), "the whole charset");
        for bad in [
            "a b", "a%b", "a^b", "a&b", "a|b", "a<b", "a>b", "a\"b", "a\nb", "a\rb", "caf\u{e9}", "", "a\tb", "a;b", "a(b", "a)b", "a!b", "a`b", "a$b", "a\\b", "a'b", "a*b", "a?b", "a~b", "a#b", "a\0b",
        ] {
            assert_eq!(check_batch_args("az.cmd", &args(&[bad]), Os::Windows), Err(RunError::BatchArgument), "{bad:?}");
        }
        assert_eq!(check_batch_args("az.cmd", &args(&["ok", "not ok"]), Os::Windows), Err(RunError::BatchArgument));
        // Only a batch file on Windows is checked.
        let hostile = args(&["a b", "%PATH%", "&calc", ""]);
        assert!(check_batch_args("az.exe", &hostile, Os::Windows).is_ok(), "a real executable gets any argument");
        assert!(check_batch_args("az.cmd", &hostile, Os::Linux).is_ok(), "no batch interpreter off Windows");
        assert!(check_batch_args("az.cmd", &hostile, Os::Macos).is_ok());
        assert!(check_batch_args("az.cmd.exe", &hostile, Os::Windows).is_ok(), "the end of the name decides");
        let sentence = RunError::BatchArgument.to_string();
        assert!(!sentence.contains("%PATH%") && !sentence.contains("calc"), "{sentence}");
    }

    #[test]
    fn no_secret_or_environment_value_is_printed_by_the_runner_source() {
        let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/integrate/run.rs"))
            .expect("the runner source");
        let production = source.split("\n#[cfg(test)]\nmod ").next().expect("a production part");
        let code: String = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "println!", "print!", "eprintln!", "eprint!", "dbg!", "format!(\"{:?}\"", "{env:?}", "{secret", "{value", "log::", "tracing::", "write_all(",
        ] {
            assert!(!code.contains(forbidden), "the runner source holds {forbidden}: a value could be printed");
        }
        // Every manual `Debug` of a type that can hold a value prints counts.
        assert!(code.contains("impl std::fmt::Debug for Env"), "a manual Debug for Env");
        assert!(code.contains("impl std::fmt::Debug for EnvExtra"), "a manual Debug for EnvExtra");
    }
}
