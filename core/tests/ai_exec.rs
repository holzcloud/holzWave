//! The local runner of the command tool, with no terminal, no page and no network.
//!
//! Every test that starts a process runs a real `/bin/sh` and is marked for Unix; the tests marked for Windows run only on a
//! Windows machine. Nothing here imports the shell: the runner is `holzwave_core::exec::LocalExec` and its output is
//! `holzwave_core::ssh::ExecResult`, the same value the exec primitive of a saved connection gives.

#![cfg(any(unix, windows))]
// The tests that start `/bin/sh` are Unix tests, so on Windows some helpers and imports are unused.
#![cfg_attr(windows, allow(unused_imports, dead_code, unused_variables))]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use holzwave_core::Error;
use holzwave_core::exec::{ALLOWED_ENV, ALLOWED_ENV_PREFIXES, LocalExec, MAX_ENV_PAIRS, env_allowed};
use holzwave_core::ssh::{ExecCancel, ExecEnd, ExecLimits, ExecResult, MAX_EXEC_COMMAND, MAX_EXEC_OUTPUT};

mod common;

/// Text a command prints, and values in the environment of the app; they appear in test code only.
const OUT_CANARY: &str = "zq-cmd-out-6a1f40";
const ENV_CANARY: &str = "zq-env-secret-d41c";

/// A runner over `pairs` as the environment of the app.
fn local(pairs: &[(&str, &str)]) -> LocalExec {
    LocalExec::new(pairs.iter().map(|(name, value)| (OsString::from(name), OsString::from(value))))
}

/// A runner with only the `PATH` of this process, so the commands find their programs.
fn plain() -> LocalExec {
    let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin"));
    LocalExec::new([(OsString::from("PATH"), path)])
}

fn limits(millis: u64) -> ExecLimits {
    ExecLimits { max_output: 65536, timeout: Duration::from_millis(millis) }
}

fn run(runner: &LocalExec, command: &str, dir: &Path, millis: u64) -> ExecResult {
    runner.exec(command, dir, &limits(millis), &ExecCancel::new()).expect("the command was started")
}

/// A scratch folder that is removed at the end of the test.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        Scratch(common::scratch_dir(tag))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Whether the process is alive: a zombie counts as dead.
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let out = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().expect("ps runs");
    let state = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    !state.is_empty() && !state.starts_with('Z')
}

/// The number a command wrote to `path`, waited for.
#[cfg(unix)]
fn read_pid(path: &Path) -> u32 {
    let mut found = None;
    assert!(
        common::wait_until(Duration::from_secs(3), || {
            found = std::fs::read_to_string(path).ok().and_then(|text| text.trim().parse::<u32>().ok());
            found.is_some()
        }),
        "the command did not write {}",
        path.display()
    );
    found.expect("a pid")
}

#[cfg(unix)]
#[test]
fn a_command_runs_in_a_separate_process_and_its_output_comes_back() {
    let dir = Scratch::new("exec-run");
    let result = run(&plain(), &format!("printf '{OUT_CANARY}\\n'"), &dir.0, 5000);
    assert_eq!(text(&result.stdout), format!("{OUT_CANARY}\n"));
    assert!(result.stderr.is_empty(), "nothing on standard error");
    assert_eq!((result.status, result.end, result.truncated, result.dropped), (Some(0), ExecEnd::Finished, false, 0));

    let again = run(&plain(), "echo $$", &dir.0, 5000);
    let pid: u32 = text(&again.stdout).trim().parse().expect("a process id");
    assert_ne!(pid, std::process::id(), "the command ran in a process of its own");
}

#[cfg(unix)]
#[test]
fn stdout_stderr_and_the_exit_status_are_kept_apart() {
    let dir = Scratch::new("exec-status");
    let split = run(&plain(), "printf out; printf err >&2; exit 7", &dir.0, 5000);
    assert_eq!((text(&split.stdout), text(&split.stderr), split.status), ("out".to_owned(), "err".to_owned(), Some(7)));
    assert_eq!(split.end, ExecEnd::Finished);

    let signalled = run(&plain(), "kill -s TERM $$", &dir.0, 5000);
    assert_eq!(signalled.status, Some(143), "128 plus the signal");
    assert_eq!(signalled.end, ExecEnd::Finished);

    let missing = run(&plain(), "zq-no-such-program-91c4", &dir.0, 5000);
    assert_eq!(missing.status, Some(127));
}

#[cfg(unix)]
#[test]
fn the_child_gets_a_scrubbed_environment_and_a_closed_stdin() {
    let dir = Scratch::new("exec-env");
    let secret = ENV_CANARY;
    let base = [
        ("PATH", &std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned())[..]),
        ("HOME", "/home/holz"),
        ("LANG", "C"),
        ("LC_ALL", "C"),
        ("TZ", "UTC"),
        ("TMPDIR", "/tmp"),
        ("USER", "holz"),
        ("HOLZWAVE_IPC", secret),
        ("HOLZWAVE_TOKEN", secret),
        ("HOLZWAVE_PANE", secret),
        ("ANTHROPIC_API_KEY", secret),
        ("OPENAI_API_KEY", secret),
        ("SSH_AUTH_SOCK", secret),
        ("GITHUB_TOKEN", secret),
        ("AWS_SECRET_ACCESS_KEY", secret),
        ("DATABASE_URL", secret),
        ("EDITOR", secret),
    ];
    let runner = local(&base);
    let shown = run(&runner, "env", &dir.0, 5000);
    let names: Vec<String> = text(&shown.stdout)
        .lines()
        .filter(|line| line.contains('='))
        .map(|line| line.split('=').next().unwrap_or("").to_owned())
        .collect();
    let extra = ["PWD", "TERM", "SHLVL", "_", "OLDPWD"];
    for name in &names {
        let allowed = env_allowed(name) || extra.contains(&name.as_str());
        assert!(allowed, "the child sees {name}");
        for secret_name in ["HOLZWAVE", "API_KEY", "SSH_AUTH_SOCK", "TOKEN", "SECRET", "DATABASE_URL", "EDITOR"] {
            assert!(!name.contains(secret_name), "{name} reached the child");
        }
    }
    assert!(!text(&shown.stdout).contains(ENV_CANARY), "a value of a secret name reached the child");
    assert!(runner.env_names().contains(&"PATH".to_owned()) && !runner.env_names().contains(&"EDITOR".to_owned()));

    let facts = run(&runner, "printf '%s' \"$TERM\"", &dir.0, 5000);
    assert_eq!(text(&facts.stdout), "dumb");
    let at = run(&runner, "printf '%s' \"$PWD\"", &dir.0, 5000);
    let canonical = dir.0.canonicalize().expect("canonical");
    let pwd = PathBuf::from(text(&at.stdout));
    assert!(pwd == dir.0 || pwd == canonical, "PWD is the folder of the call: {}", pwd.display());

    // Standard input is closed: a reader of it ends at once.
    let cat = run(&plain(), "cat", &dir.0, 5000);
    assert_eq!((cat.stdout.len(), cat.status, cat.end), (0, Some(0), ExecEnd::Finished));
    let line = run(&plain(), "read line; echo \"[$line]\"", &dir.0, 5000);
    assert_eq!(text(&line.stdout), "[]\n");
    assert!(ALLOWED_ENV.contains(&"PATH") && ALLOWED_ENV_PREFIXES.contains(&"LC_"));
}

#[cfg(unix)]
#[test]
fn a_timeout_ends_the_command_and_everything_it_started() {
    let dir = Scratch::new("exec-timeout");
    let runner = plain();
    let started = Instant::now();
    let result = run(&runner, "sleep 30 & echo $! > bg.pid; echo $$ > sh.pid; wait", &dir.0, 400);
    assert!(started.elapsed() < Duration::from_secs(3), "the run took {:?}", started.elapsed());
    assert_eq!((result.end, result.status), (ExecEnd::TimedOut, None));
    let (bg, sh) = (read_pid(&dir.0.join("bg.pid")), read_pid(&dir.0.join("sh.pid")));
    assert!(
        common::wait_until(Duration::from_secs(2), || !alive(bg) && !alive(sh)),
        "a process of the command is still alive (helper {bg}: {}, shell {sh}: {})",
        alive(bg),
        alive(sh)
    );

    let again = run(&runner, "printf again", &dir.0, 5000);
    assert_eq!(text(&again.stdout), "again", "the runner is usable after a time out");
}

#[cfg(unix)]
#[test]
fn a_cancel_ends_the_command_and_everything_it_started_promptly() {
    let dir = Scratch::new("exec-cancel");
    let runner = plain();
    let stop = ExecCancel::new();
    let raise = stop.clone();
    let raised_at = std::sync::Arc::new(std::sync::Mutex::new(None));
    let note = std::sync::Arc::clone(&raised_at);
    let helper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        *note.lock().expect("note") = Some(Instant::now());
        raise.cancel();
        raise.cancel(); // a second call is harmless
    });
    let started = Instant::now();
    let limits = ExecLimits { max_output: 65536, timeout: Duration::from_secs(30) };
    let result = runner
        .exec("sleep 30 & echo $! > bg.pid; echo $$ > sh.pid; wait", &dir.0, &limits, &stop)
        .expect("the command was started");
    let returned = Instant::now();
    helper.join().expect("the helper thread ends");
    let raised = raised_at.lock().expect("note").expect("the stop was raised");
    assert!(returned.duration_since(raised) < Duration::from_secs(2), "the stop took {:?}", returned.duration_since(raised));
    assert!(returned.duration_since(started) < Duration::from_secs(5), "far below the time limit");
    assert_eq!((result.end, result.status), (ExecEnd::Cancelled, None));
    let (bg, sh) = (read_pid(&dir.0.join("bg.pid")), read_pid(&dir.0.join("sh.pid")));
    assert!(common::wait_until(Duration::from_secs(2), || !alive(bg) && !alive(sh)), "a process of the command is still alive");

    // A switch that is raised before the call ends it at once and starts nothing.
    let early = ExecCancel::new();
    early.cancel();
    let at = Instant::now();
    let result = runner.exec("echo $$ > never.pid", &dir.0, &limits, &early).expect("a result");
    assert_eq!((result.end, result.status), (ExecEnd::Cancelled, None));
    assert!(at.elapsed() < Duration::from_millis(500));
    assert!(!dir.0.join("never.pid").exists(), "a command started after the stop");
}

#[cfg(unix)]
#[test]
fn output_is_kept_as_a_tail_and_the_rest_is_counted() {
    let dir = Scratch::new("exec-flood");
    let runner = plain();
    // Three MiB of `x` and then `END-OUT`, on both streams.
    let flood = "f() { head -c 3145728 /dev/zero | tr '\\000' x; printf END-OUT; }; f; f >&2";
    let started = Instant::now();
    let result = runner
        .exec(flood, &dir.0, &ExecLimits { max_output: 65536, timeout: Duration::from_secs(30) }, &ExecCancel::new())
        .expect("a result");
    assert!(started.elapsed() < Duration::from_secs(10), "the flood took {:?}", started.elapsed());
    for (name, stream) in [("stdout", &result.stdout), ("stderr", &result.stderr)] {
        assert_eq!(stream.len(), 65536, "{name} holds exactly the cap");
        assert!(stream.ends_with(b"END-OUT"), "{name} keeps the end");
    }
    assert!(result.truncated);
    assert_eq!(result.dropped, 2 * (3_145_735 - 65536));
    assert_eq!((result.status, result.end), (Some(0), ExecEnd::Finished));

    // A cap of zero keeps one byte per stream; a cap above the largest is the largest.
    let tiny = runner
        .exec("printf abc; printf xyz >&2", &dir.0, &ExecLimits { max_output: 0, timeout: Duration::from_secs(5) }, &ExecCancel::new())
        .expect("a result");
    assert_eq!((text(&tiny.stdout), text(&tiny.stderr)), ("c".to_owned(), "z".to_owned()));
    let big = runner
        .exec(
            "head -c 2097152 /dev/zero | tr '\\000' x",
            &dir.0,
            &ExecLimits { max_output: usize::MAX, timeout: Duration::from_secs(10) },
            &ExecCancel::new(),
        )
        .expect("a result");
    assert_eq!(big.stdout.len(), MAX_EXEC_OUTPUT);
    assert!(big.truncated);
}

#[cfg(unix)]
#[test]
fn a_background_holder_of_the_pipes_does_not_hang_the_run() {
    let dir = Scratch::new("exec-holder");
    let runner = plain();
    let mut ran = vec!["the same group"];

    // The leader ends at once and a helper of the same group holds both pipes: the group is killed after the drain.
    let started = Instant::now();
    let result = run(&runner, "sleep 30 & echo $! > bg.pid; printf done", &dir.0, 10_000);
    assert!(started.elapsed() < Duration::from_secs(2), "the run took {:?}", started.elapsed());
    assert_eq!((result.end, result.status, text(&result.stdout)), (ExecEnd::Finished, Some(0), "done".to_owned()));
    let bg = read_pid(&dir.0.join("bg.pid"));
    assert!(common::wait_until(Duration::from_secs(2), || !alive(bg)), "the helper of the group is still alive");

    // A helper that left the group cannot be reached by the group kill; the run still ends and says output may be missing.
    let have_setsid = std::process::Command::new("sh").args(["-c", "command -v setsid"]).output().is_ok_and(|o| o.status.success());
    if have_setsid {
        ran.push("a helper that left the group");
        let started = Instant::now();
        let result = run(&runner, "setsid sleep 30 & echo $! > far.pid; printf done", &dir.0, 10_000);
        assert!(started.elapsed() < Duration::from_secs(3), "the run took {:?}", started.elapsed());
        assert_eq!((result.status, text(&result.stdout)), (Some(0), "done".to_owned()));
        assert!(result.truncated, "a reader was released and the result says so");
        let far = read_pid(&dir.0.join("far.pid"));
        let _ = std::process::Command::new("kill").args(["-s", "KILL", &far.to_string()]).status();
    }
    println!("sub-cases of the pipe holder that ran: {}", ran.join(", "));
}

#[cfg(unix)]
#[test]
fn bad_input_is_refused_before_anything_starts() {
    let dir = Scratch::new("exec-bad");
    let runner = plain();
    let folder_name = "zq-cmd-dir-8e55d1";
    let file = dir.0.join(folder_name);
    std::fs::write(&file, b"not a folder").expect("a file");
    let missing = dir.0.join(format!("{folder_name}-missing"));
    let marker = dir.0.join("marker");
    let command_name = "zq-cmd-in-refusal-4c0a";
    let touch = format!("touch '{}' # {command_name}", marker.display());
    let go = |command: &str, cwd: &Path| runner.exec(command, cwd, &limits(2000), &ExecCancel::new());
    let cases: Vec<(Result<ExecResult, Error>, &str)> = vec![
        (go("", &dir.0), "empty"),
        (go(" \t\n ", &dir.0), "empty"),
        (go(&format!("{touch}\0"), &dir.0), "NUL"),
        (go(&format!("{touch}{}", " ".repeat(MAX_EXEC_COMMAND)), &dir.0), "longer"),
        (go(&touch, &file), "working directory"),
        (go(&touch, &missing), "working directory"),
    ];
    for (result, word) in cases {
        let Err(Error::Unsupported(why)) = result else { panic!("{word}: not refused") };
        assert!(why.contains(word), "{why}");
        assert!(!why.contains(command_name) && !why.contains(folder_name), "the refusal quotes the input: {why}");
    }
    assert!(!marker.exists(), "a refused command ran");

    // The longest command is accepted.
    let longest = format!(":{}", " ".repeat(MAX_EXEC_COMMAND - 1));
    assert_eq!(longest.len(), MAX_EXEC_COMMAND);
    let result = go(&longest, &dir.0).expect("accepted");
    assert_eq!(result.status, Some(0));
}

#[cfg(unix)]
#[test]
fn a_command_that_starts_with_a_dash_is_still_a_command() {
    let dir = Scratch::new("exec-dash");
    let runner = plain();
    let missing = run(&runner, "-zq-no-such-command", &dir.0, 5000);
    assert_eq!(missing.status, Some(127));
    assert!(text(&missing.stderr).contains("not found"), "{}", text(&missing.stderr));
    let lead = run(&runner, "\nprintf ok", &dir.0, 5000);
    assert_eq!(text(&lead.stdout), "ok");
}

#[test]
fn secret_looking_names_never_pass_even_when_listed() {
    #[cfg(unix)]
    for name in ["PATH", "HOME", "USER", "LOGNAME", "LANG", "LANGUAGE", "LC_ALL", "LC_CTYPE", "TZ", "TMPDIR"] {
        assert!(env_allowed(name), "{name} is listed");
    }
    for name in [
        "HOLZWAVE_IPC", "HOLZWAVE_TOKEN", "HOLZWAVE_PANE", "ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GEMINI_API_KEY",
        "OPENROUTER_API_KEY", "SSH_AUTH_SOCK", "SSH_AGENT_PID", "GITHUB_TOKEN", "GH_TOKEN", "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN", "NPM_TOKEN", "DATABASE_URL", "KUBECONFIG", "GPG_AGENT_INFO", "DOCKER_HOST",
        "EDITOR", "SHELL", "PAGER",
    ] {
        assert!(!env_allowed(name), "{name} reaches a command");
    }
    // A name that carries a secret word fails even where its prefix or its listing would pass it.
    for name in ["LC_AUTH", "LC_TOKEN_X", "lc_secret", "TMPDIR_KEY", "PATH_PASSWORD", "HOME_CREDENTIAL", "USER_PASSWD"] {
        assert!(!env_allowed(name), "{name} reaches a command");
    }
    assert!(ALLOWED_ENV_PREFIXES.iter().all(|prefix| !prefix.is_empty()));

    // The runner keeps at most `MAX_ENV_PAIRS`, drops what is not valid Unicode and sorts the names it reports.
    let many: Vec<(OsString, OsString)> = (0..200).map(|n| (OsString::from(format!("LC_{n:03}")), OsString::from("x"))).collect();
    let capped = LocalExec::new(many);
    #[cfg(unix)]
    assert_eq!(capped.env_names().len(), MAX_ENV_PAIRS);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let odd = LocalExec::new([
            (OsString::from("PATH"), OsString::from_vec(vec![0x66, 0xff, 0x6f])),
            (OsString::from_vec(vec![0x4c, 0x43, 0x5f, 0xff]), OsString::from("x")),
            (OsString::from("TZ"), OsString::from("UTC")),
            (OsString::from("LANG"), OsString::from("C")),
        ]);
        assert_eq!(odd.env_names(), ["LANG", "TZ"], "only valid Unicode pairs stay, sorted");
    }
}

#[test]
fn no_debug_output_of_the_runner_shows_text_or_values() {
    let runner = local(&[("PATH", ENV_CANARY), ("LANG", ENV_CANARY), ("HOLZWAVE_TOKEN", ENV_CANARY)]);
    for shown in [format!("{runner:?}"), format!("{runner:#?}")] {
        assert!(!shown.contains(ENV_CANARY), "a value is shown: {shown}");
        assert!(!shown.contains("PATH") && !shown.contains("LANG") && !shown.contains("HOLZWAVE"), "a name is shown: {shown}");
        assert!(shown.contains("LocalExec") && shown.contains('2') || cfg!(windows), "{shown}");
    }
    let dir = Scratch::new("exec-debug");
    let secret_command = format!("echo {OUT_CANARY}");
    let refused = runner.exec("", &dir.0, &limits(1000), &ExecCancel::new());
    assert!(!format!("{refused:?}").contains(ENV_CANARY));
    let missing = dir.0.join(OUT_CANARY);
    let Err(error) = runner.exec(&secret_command, &missing, &limits(1000), &ExecCancel::new()) else { panic!("refused") };
    let shown = format!("{error} {error:?}");
    assert!(!shown.contains(OUT_CANARY), "the refusal holds the command or the folder: {shown}");
}

/// The names of the variants of `pub enum <name>` in `source`: the identifiers that start a line at the depth of the body.
fn variants_of(source: &str, name: &str) -> Vec<String> {
    let head = format!("pub enum {name} ");
    let Some(start) = source.find(&head) else { return Vec::new() };
    let mut depth = 0i32;
    let mut found = Vec::new();
    for line in source[start..].lines() {
        let trimmed = line.trim_start();
        if depth == 1 && trimmed.starts_with(|c: char| c.is_ascii_uppercase()) {
            found.push(trimmed.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect());
        }
        depth += i32::try_from(line.matches('{').count()).unwrap_or(0) - i32::try_from(line.matches('}').count()).unwrap_or(0);
        if depth <= 0 {
            break;
        }
    }
    found
}

/// The words a variant of the local IPC enum may never carry: nothing that decides, approves, allows, denies or grants.
const FORBIDDEN_WORDS: [&str; 8] = ["approv", "decid", "allow", "deny", "consent", "grant", "permit", "confirm"];

fn approving_variants(source: &str) -> Vec<String> {
    variants_of(source, "Cmd")
        .into_iter()
        .filter(|variant| {
            let lower = variant.to_ascii_lowercase();
            FORBIDDEN_WORDS.iter().any(|word| lower.contains(word))
        })
        .collect()
}

#[test]
fn an_agent_started_process_has_no_way_to_approve() {
    // The scan has its own self-test: an enum with a variant `Approve` is flagged and one with `Ai` is not.
    assert_eq!(approving_variants("pub enum Cmd {\n    Approve { call: String },\n    Ping,\n}\n"), ["Approve"]);
    assert!(approving_variants("pub enum Cmd {\n    /// Ask.\n    Ai { prompt: String },\n    Ping,\n}\n").is_empty());
    assert_eq!(variants_of("pub enum Cmd {\n    A { x: u8 },\n    B,\n}\npub enum Other {\n    C,\n}\n", "Cmd"), ["A", "B"]);

    // The real enum: what `wsh` can ask holds no variant that approves anything.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ipc/src/lib.rs");
    let source = std::fs::read_to_string(&path).expect("the source of the local IPC");
    let variants = variants_of(&source, "Cmd");
    assert!(variants.len() >= 4, "the scan found the enum: {variants:?}");
    assert_eq!(approving_variants(&source), Vec::<String>::new(), "a variant of the IPC enum could approve: {variants:?}");

    // A command started by the agent finds none of the variables `wsh` needs to reach the app.
    #[cfg(unix)]
    {
        let dir = Scratch::new("exec-wsh");
        let runner = LocalExec::new(
            [("PATH", std::env::var("PATH").unwrap_or_default()), ("HOLZWAVE_IPC", "/x".to_owned()), ("HOLZWAVE_TOKEN", ENV_CANARY.to_owned()), ("HOLZWAVE_PANE", "1".to_owned())]
                .into_iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value))),
        );
        let result = run(
            &runner,
            "for n in HOLZWAVE_IPC HOLZWAVE_TOKEN HOLZWAVE_PANE; do eval \"v=\\${$n+set}\"; [ -n \"$v\" ] && echo \"$n\"; done; echo end",
            &dir.0,
            5000,
        );
        assert_eq!(text(&result.stdout), "end\n", "a variable of the app reached the command");
    }
}

#[cfg(windows)]
#[test]
fn a_powershell_command_runs_with_a_scrubbed_environment() {
    let dir = Scratch::new("exec-ps");
    let runner = LocalExec::new(std::env::vars_os().chain([(OsString::from("HOLZWAVE_TOKEN"), OsString::from(ENV_CANARY))]));
    let said = run(&runner, "Write-Output hello", &dir.0, 20_000);
    assert_eq!(text(&said.stdout).trim(), "hello");
    let token = run(&runner, "Write-Output \"[$env:HOLZWAVE_TOKEN]\"", &dir.0, 20_000);
    assert_eq!(text(&token.stdout).trim(), "[]");
    let status = run(&runner, "exit 3", &dir.0, 20_000);
    assert_eq!(status.status, Some(3));
}

#[cfg(windows)]
#[test]
fn a_powershell_timeout_ends_the_process_tree() {
    let dir = Scratch::new("exec-ps-tree");
    let runner = LocalExec::from_process();
    let command = "$p = Start-Process -PassThru -WindowStyle Hidden ping -ArgumentList '-n','60','127.0.0.1'; $p.Id | Out-File child.pid -Encoding ascii; $p.WaitForExit()";
    let result = run(&runner, command, &dir.0, 1500);
    assert_eq!(result.end, ExecEnd::TimedOut);
    let mut pid = None;
    assert!(common::wait_until(Duration::from_secs(5), || {
        pid = std::fs::read_to_string(dir.0.join("child.pid")).ok().and_then(|t| t.trim().parse::<u32>().ok());
        pid.is_some()
    }));
    let pid = pid.expect("a pid");
    let gone = common::wait_until(Duration::from_secs(3), || {
        let out = std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {pid}")]).output().expect("tasklist");
        !String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
    });
    assert!(gone, "the child of the command is still alive");
}
