//! The pure half of the command tool and the way the approval table handles a command, with no process, no terminal and
//! no network.
//!
//! A fake host stands in for the shell: it answers what a call would act on and records the tickets it was handed. A call
//! goes through every layer the shell uses: typed arguments, host resolution, the deny check, a hash over the exact
//! payload including the working directory, a card, a decision that repeats that hash, a second resolution at execution
//! and only then a ticket for the host.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use holzwave_core::ai::provider::CancelToken;
use holzwave_core::ai::tools::{
    ApprovalTable, Decision, DenyPolicy, FsKind, Pending, Resolved, SHOWN_BYTES, Scope, Shown, Ticket, Tool, ToolError,
    ToolHost, ToolName, ToolOutput, exit_line, fit, quote, remote_command, report, timed_out_line,
};
use holzwave_core::ai::types::ToolCall;
use holzwave_core::layout::PaneId;
use holzwave_core::ssh::{ExecEnd, ExecResult, MAX_EXEC_COMMAND};
use serde_json::{Value, json};

mod common;

/// A folder name that appears in test code only: a hit in an error text is a leak.
const DIR_CANARY: &str = "zq-cmd-dir-8e55d1";
/// Text a command prints; it appears in test code only.
const OUT_CANARY: &str = "zq-cmd-out-6a1f40";

/// What the fake host was asked to run: the tool, and the working directory of the ticket.
#[derive(Clone)]
struct Ran {
    tool: Tool,
    cwd: Option<String>,
}

struct HostState {
    fs: FsKind,
    target: String,
    /// The working directory the host answers instead of the one the call named: a link that was retargeted.
    cwd_override: Option<String>,
    ran: Vec<Ran>,
    answer: String,
}

/// The shell, as far as the approval table can tell: a machine and the working directory a command would run in.
struct CommandHost {
    state: Arc<Mutex<HostState>>,
}

impl CommandHost {
    fn new(fs: FsKind, target: &str) -> Self {
        Self {
            state: Arc::new(Mutex::new(HostState {
                fs,
                target: target.to_owned(),
                cwd_override: None,
                ran: Vec::new(),
                answer: "ok".to_owned(),
            })),
        }
    }

    /// A host that runs commands on this computer.
    fn local() -> Self {
        Self::new(FsKind::Local, "This computer")
    }

    /// A host that runs commands on a saved connection called `build box`.
    fn remote() -> Self {
        Self::new(FsKind::Remote, "build box")
    }

    fn state(&self) -> MutexGuard<'_, HostState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// From now on the host answers this working directory for every command.
    fn retarget(&self, cwd: &str) {
        self.state().cwd_override = Some(cwd.to_owned());
    }

    /// From now on the host answers another machine.
    fn move_to(&self, fs: FsKind, target: &str) {
        let mut state = self.state();
        state.fs = fs;
        state.target = target.to_owned();
    }

    fn ran(&self) -> Vec<Ran> {
        self.state().ran.clone()
    }
}

impl ToolHost for CommandHost {
    fn available(&self, _tool: ToolName, _pane: PaneId) -> bool {
        true
    }

    fn resolve(&self, scope: &Scope, tool: &Tool) -> Result<Resolved, ToolError> {
        let state = self.state();
        let mut resolved = match state.fs {
            FsKind::Local => Resolved::local(scope.pane, state.target.clone()),
            FsKind::Remote => Resolved::remote(scope.pane, state.target.clone()),
        };
        if let Some(cwd) = state.cwd_override.as_deref().or(tool.cwd()) {
            resolved = resolved.with_cwd(cwd);
        }
        if let Some(path) = tool.path() {
            resolved = resolved.with_path(path);
        }
        Ok(resolved)
    }

    fn run(&self, ticket: &Ticket, _cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        let mut state = self.state();
        state.ran.push(Ran { tool: ticket.tool().clone(), cwd: ticket.resolved().cwd().map(str::to_owned) });
        Ok(ToolOutput::new(state.answer.clone()))
    }
}

fn scope(pane: PaneId) -> Scope {
    Scope { conversation: "chat-1".to_owned(), turn: 1, pane }
}

fn call(id: &str, args: Value) -> ToolCall {
    ToolCall { id: id.to_owned(), name: "run_command".to_owned(), args }
}

fn table() -> ApprovalTable {
    ApprovalTable::new(DenyPolicy::new(Vec::new()))
}

/// A scratch folder, removed at the end of the test; its canonical spelling is `canonical`.
struct Folder {
    path: PathBuf,
    canonical: String,
}

impl Folder {
    fn new(tag: &str) -> Folder {
        let path = common::scratch_dir(tag);
        let canonical = path.canonicalize().expect("canonical").to_str().expect("utf-8").to_owned();
        Folder { path, canonical }
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn names(card: &Pending) -> Vec<&str> {
    card.payload.iter().map(|field| field.name).collect()
}

fn value<'a>(card: &'a Pending, name: &str) -> &'a str {
    card.payload.iter().find(|field| field.name == name).map(|field| field.value.as_str()).expect("a field")
}

#[test]
fn a_run_command_call_builds_a_card_with_the_exact_command_and_the_host_runs_on_the_approved_hash() {
    let folder = Folder::new(DIR_CANARY);
    let host = CommandHost::local();
    let table = table();
    let scope = scope(1);
    let command = format!("printf 'a\n\tb {OUT_CANARY} \u{202e}c'");
    let cwd = folder.path.to_str().expect("utf-8");
    let pending = table
        .propose(&host, &scope, &call("call_1", json!({"command": command, "cwd": cwd, "pane": 1})))
        .expect("a card");

    // The card as the page receives it: eleven members in this order.
    let text = serde_json::to_string(&pending).expect("JSON");
    let card: Value = serde_json::from_str(&text).expect("JSON");
    let order = ["conversation", "call_id", "tool", "effect", "pane", "target", "remote", "payload", "hash", "tainted", "taint"];
    let at: Vec<usize> = order.iter().map(|name| text.find(&format!("\"{name}\":")).expect("a member")).collect();
    assert!(at.windows(2).all(|pair| pair[0] < pair[1]), "the members are not in the order of the card: {at:?}");
    assert_eq!(card.as_object().map(serde_json::Map::len), Some(order.len()), "eleven members");
    assert_eq!(card["effect"], json!("exec"));
    assert_eq!(names(&pending), ["tool", "pane", "target", "remote", "command", "cwd", "timeout_secs", "resolved_cwd"]);

    // The command is shown with every hidden character as a visible escape.
    let shown = value(&pending, "command");
    for visible in ["\\n", "\\t", "\\u{202e}"] {
        assert!(shown.contains(visible), "{visible} is not shown: {shown}");
    }
    assert!(!shown.chars().any(|c| matches!(c, '\n' | '\t' | '\u{202e}')), "a hidden character is shown raw: {shown}");
    assert_eq!(value(&pending, "timeout_secs"), "60");
    assert_eq!(value(&pending, "resolved_cwd"), folder.canonical);
    assert!(pending.hash.len() == 64 && pending.hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));

    // Nothing runs before a decision on the card's hash; then the host is handed the ticket once.
    assert!(host.ran().is_empty());
    table.decide("chat-1", "call_1", Decision::Allow, &pending.hash).expect("allowed");
    let output = table.run(&host, &scope, "call_1", &CancelToken::new()).expect("ran");
    assert_eq!(output.text(), "ok");
    let ran = host.ran();
    assert_eq!(ran.len(), 1);
    assert!(matches!(&ran[0].tool, Tool::RunCommand(args) if args.command == command));
    assert_eq!(ran[0].cwd.as_deref(), Some(folder.canonical.as_str()), "the host runs in the canonical folder");
    assert!(table.pending("chat-1").is_empty(), "the call is spent");
}

/// An exec result with the given parts.
fn result(stdout: &str, stderr: &str, status: Option<u32>, end: ExecEnd) -> ExecResult {
    ExecResult { stdout: stdout.as_bytes().to_vec(), stderr: stderr.as_bytes().to_vec(), status, end, truncated: false, dropped: 0 }
}

#[test]
fn the_report_tells_the_end_and_the_streams_in_fixed_words() {
    // The sample of the contract.
    let ok = report(&result("build ok\n", "", Some(0), ExecEnd::Finished), 60);
    assert_eq!(ok.text(), "Exit status: 0.\n\nStandard output:\nbuild ok");
    assert!(!ok.is_failure());

    // Both streams, a failing status.
    let both = report(&result("one\ntwo\n", "boom\n", Some(2), ExecEnd::Finished), 60);
    assert_eq!(both.text(), "Exit status: 2.\n\nStandard output:\none\ntwo\n\nStandard error:\nboom");
    assert!(both.is_failure());

    // Nothing printed.
    let quiet = report(&result("", "", Some(0), ExecEnd::Finished), 60);
    assert_eq!(quiet.text(), "Exit status: 0.\n\nNo output.");
    assert!(!quiet.is_failure());

    // A time out: the words of the limit, singular for one second.
    let slow = report(&result("partial", "", None, ExecEnd::TimedOut), 60);
    assert_eq!(
        slow.text(),
        "Stopped: the command ran longer than 60 seconds and was ended.\n\nStandard output:\npartial"
    );
    assert_eq!(timed_out_line(1), "Stopped: the command ran longer than 1 second and was ended.");
    assert_eq!(timed_out_line(60), "Stopped: the command ran longer than 60 seconds and was ended.");
    assert!(slow.is_failure());

    // A lost connection, a stop and a status the host did not tell.
    let lost = report(&result("", "", None, ExecEnd::Lost), 60);
    assert_eq!(
        lost.text(),
        "Stopped: the connection to the host was lost while the command ran. Whether it finished is unknown.\n\nNo output."
    );
    assert!(lost.is_failure());
    let stopped = report(&result("x", "", None, ExecEnd::Cancelled), 60);
    assert_eq!(stopped.text(), "Stopped: the command was ended before it finished.\n\nStandard output:\nx");
    assert!(stopped.is_failure());
    let unknown = report(&result("", "", None, ExecEnd::Finished), 60);
    assert_eq!(unknown.text(), "Exit status: unknown.\n\nNo output.");
    assert!(unknown.is_failure());
    assert_eq!(exit_line(7), "Exit status: 7.");

    // A stream that was cut upstream: the capture limit was reached and the exec result says output was dropped.
    let mut cut = result("", "", Some(0), ExecEnd::Finished);
    cut.stdout = format!("{}\u{20ac}tail", "x".repeat(65536 - 8)).into_bytes();
    cut.stdout.truncate(65536);
    cut.truncated = true;
    cut.dropped = 10;
    let text = report(&cut, 60).into_text();
    assert!(text.starts_with("Exit status: 0.\n\nStandard output:\n[earlier output omitted]\n"), "{}", &text[..80]);
    // A stream that did not reach the limit is not marked even when the other one was cut.
    let mut other = result("short", "", Some(0), ExecEnd::Finished);
    other.stderr = vec![b'e'; 65536];
    other.truncated = true;
    let text = report(&other, 60).into_text();
    assert!(text.contains("Standard output:\nshort\n\nStandard error:\n[earlier output omitted]\n"), "{}", &text[..120]);
}

/// A text of up to `max` characters from an alphabet with one-, two-, three- and four-byte characters and line breaks.
fn random_text(rng: &mut common::Xorshift64, max: usize) -> String {
    const ALPHABET: [char; 8] = ['a', 'b', ' ', '\n', '\u{e9}', '\u{20ac}', '\u{1f600}', 'z'];
    let length = rng.below(max + 1);
    (0..length).map(|_| ALPHABET[rng.below(ALPHABET.len())]).collect()
}

#[test]
fn fit_keeps_the_end_of_each_stream_on_a_character_boundary_within_the_budget() {
    let mut rng = common::Xorshift64::new(0x00c0_ffee);
    for round in 0..200 {
        let (out, err) = (random_text(&mut rng, 40000), random_text(&mut rng, 40000));
        let budget = rng.below(16385);
        let shown = fit(&out, &err, budget);
        let ctx = format!("round {round}, budget {budget}, lengths {} and {}", out.len(), err.len());
        assert!(shown.out.len() + shown.err.len() <= budget, "over the budget: {ctx}");
        assert!(out.ends_with(&shown.out) && err.ends_with(&shown.err), "not the end of the stream: {ctx}");
        assert_eq!(shown.out_cut, shown.out.len() < out.len(), "{ctx}");
        assert_eq!(shown.err_cut, shown.err.len() < err.len(), "{ctx}");
        if out.len() + err.len() <= budget {
            assert!(!shown.out_cut && !shown.err_cut, "both fit and one was cut: {ctx}");
        }
        if err.len() <= budget / 2 {
            assert!(!shown.err_cut, "a quiet stream was cut: {ctx}");
        }
        if out.len() <= budget / 2 {
            assert!(!shown.out_cut, "a quiet stream was cut: {ctx}");
        }
        // A stream that is cut keeps the longest end that fits: what is shown is within a character of the whole budget.
        if shown.out_cut || shown.err_cut {
            assert!(shown.out.len() + shown.err.len() + 8 > budget, "a cut that was too deep: {ctx}");
        }
    }

    // A quiet stream leaves its share to the other.
    let loud = "x".repeat(20000);
    let shown = fit(&loud, &"e".repeat(10), SHOWN_BYTES);
    assert_eq!((shown.err.len(), shown.out.len()), (10, SHOWN_BYTES - 10));
    assert!(shown.out_cut && !shown.err_cut);
    // Whole when it fits, and an empty budget shows nothing.
    let whole = fit("abc", "de", 5);
    assert_eq!((whole.out.as_str(), whole.err.as_str(), whole.out_cut, whole.err_cut), ("abc", "de", false, false));
    let none = fit("abc", "de", 0);
    assert_eq!((none.out.len() + none.err.len(), none.out_cut, none.err_cut), (0, true, true));
    assert!(format!("{whole:?}").contains("Shown") && !format!("{whole:?}").contains("abc"));
}

#[test]
fn a_failed_output_carries_its_flag_and_a_normal_one_does_not() {
    assert!(!ToolOutput::new("x".to_owned()).is_failure());
    assert!(ToolOutput::failed("x".to_owned()).is_failure());

    // The same head-and-tail cut applies to both.
    let big = format!("{}{}", "a".repeat(20 * 1024), "b".repeat(20 * 1024));
    let normal = ToolOutput::new(big.clone());
    let failed = ToolOutput::failed(big);
    assert_eq!(normal.text(), failed.text());
    assert!(normal.truncated() && failed.truncated());
    assert_eq!(normal.omitted(), failed.omitted());
    assert!(failed.text().len() <= holzwave_core::ai::tools::MAX_RESULT_BYTES);

    // `Debug` shows the size and the flag and not the text.
    for (output, flag) in [(ToolOutput::new(OUT_CANARY.to_owned()), "false"), (ToolOutput::failed(OUT_CANARY.to_owned()), "true")] {
        for shown in [format!("{output:?}"), format!("{output:#?}")] {
            assert!(!shown.contains(OUT_CANARY), "the text is shown: {shown}");
            assert!(shown.contains("failure") && shown.contains(flag) && shown.contains("17"), "{shown}");
        }
    }
}

/// The output of `/bin/sh -c <script>`, as bytes, and its exit status.
#[cfg(unix)]
fn sh(script: &str, dir: Option<&std::path::Path>) -> (Vec<u8>, Option<i32>) {
    let mut command = std::process::Command::new("/bin/sh");
    command.arg("-c").arg(script);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let out = command.output().expect("the shell runs");
    (out.stdout, out.status.code())
}

#[cfg(unix)]
#[test]
fn the_remote_string_survives_a_real_posix_shell_byte_for_byte() {
    // Thirty texts that a shell would read as something other than text if they were not quoted.
    let mut texts: Vec<String> = [
        "'", "''", "a'b'c", "\"", "say \"hi\"", "\\", "a\\nb", "$(echo no)", "`echo no`", "`", "$HOME", "${HOME}", ";", "a; echo no", "&&",
        "a && echo no", "|", "> out", "\n", "line one\nline two", "\t", "\r", "tab\there", "carriage\rreturn", "caf\u{e9} \u{20ac} \u{1f600}",
        "-n", "--", "   ", "~/x", "*", "!history", "#not a comment",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    texts.push("'".repeat(8192));
    assert!(texts.len() >= 30);
    for text in &texts {
        let (out, status) = sh(&format!("printf %s {}", quote(text)), None);
        assert_eq!(status, Some(0), "{text:?}");
        assert_eq!(out, text.as_bytes(), "the shell read {text:?} as something else");
    }

    // With a working directory: the folder has a space and a quote in its name, and the command sees it as its own.
    let folder = Folder::new("cmd-quote");
    let odd = folder.path.join("it's a \"dir\"");
    std::fs::create_dir(&odd).expect("a folder");
    let canonical = odd.canonicalize().expect("canonical");
    let cwd = canonical.to_str().expect("utf-8");
    let there = remote_command("printf '%s' \"$PWD\"", Some(cwd)).expect("short enough");
    assert!(there.starts_with("cd -- ") && there.contains(" && exec sh -c -- "), "{there}");
    let (out, status) = sh(&there, None);
    assert_eq!((status, out), (Some(0), cwd.as_bytes().to_vec()));
    let (_, status) = sh(&remote_command("exit 7", Some(cwd)).expect("short"), None);
    assert_eq!(status, Some(7), "the status of the command is the status of the string");

    // A folder that does not exist: the command does not run, and the status is not 0.
    let marker = folder.path.join("marker");
    let missing = folder.path.join("no such folder");
    let (_, status) = sh(
        &remote_command(&format!("touch '{}'", marker.display()), missing.to_str()).expect("short"),
        None,
    );
    assert!(status != Some(0), "{status:?}");
    assert!(!marker.exists(), "the command ran although its folder is missing");

    // Without a folder there is no `cd`; a command of several lines runs line by line as written.
    let bare = remote_command("pwd", None).expect("short");
    assert_eq!(bare, "exec sh -c -- 'pwd'");
    let (out, _) = sh(&remote_command("printf a\nprintf b\nprintf c", None).expect("short"), None);
    assert_eq!(out, b"abc");

    // A command whose string is longer than the exec limit has none.
    assert_eq!(remote_command(&"'".repeat(8192), None), None);
    assert!(remote_command(&"x".repeat(MAX_EXEC_COMMAND), None).is_none());
    assert!(remote_command(&"x".repeat(100), Some("/srv/app")).is_some());
}

#[cfg(unix)]
#[test]
fn a_working_directory_in_a_protected_place_is_refused_before_a_card() {
    let folder = Folder::new(DIR_CANARY);
    let data = folder.path.join("data");
    let ipc = data.join("ipc");
    let ssh = folder.path.join("home/.ssh");
    let proj = folder.path.join("proj");
    for dir in [&data, &ipc, &ssh, &proj] {
        std::fs::create_dir_all(dir).expect("a folder");
    }
    std::os::unix::fs::symlink(&ssh, proj.join("link")).expect("a link");
    let host = CommandHost::local();
    let table = ApprovalTable::new(DenyPolicy::new(vec![data.canonicalize().expect("canonical")]));
    let scope = scope(1);
    for (n, cwd) in [data.clone(), ipc.clone(), ssh.clone(), proj.join("link")].iter().enumerate() {
        let args = json!({"command": "ls", "cwd": cwd});
        let error = table.propose(&host, &scope, &call(&format!("call_{n}"), args)).expect_err("a protected folder");
        assert!(matches!(error, ToolError::Denied(_)), "{error:?}");
        assert!(!error.to_string().contains(DIR_CANARY), "the refusal holds the folder: {error}");
    }
    assert!(table.pending("chat-1").is_empty(), "a card was made for a protected folder");
    assert!(host.ran().is_empty());

    // A folder next to them is fine.
    let ok = table.propose(&host, &scope, &call("call_ok", json!({"command": "ls", "cwd": proj}))).expect("a card");
    assert_eq!(ok.target, "This computer");
    assert_eq!(table.pending("chat-1").len(), 1);
}

#[test]
fn a_working_directory_that_changed_after_the_card_refuses_the_call() {
    let run_after = |change: &dyn Fn(&CommandHost)| {
        let host = CommandHost::remote();
        let table = table();
        let scope = scope(3);
        let pending = table
            .propose(&host, &scope, &call("call_1", json!({"command": "make", "cwd": "/srv/app", "pane": 3})))
            .expect("a card");
        table.decide("chat-1", "call_1", Decision::Allow, &pending.hash).expect("allowed");
        change(&host);
        let result = table.run(&host, &scope, "call_1", &CancelToken::new());
        assert_eq!(host.ran().len(), 0, "the host ran a command that was not the one the card showed");
        assert!(table.pending("chat-1").is_empty());
        assert_eq!(
            table.run(&host, &scope, "call_1", &CancelToken::new()).expect_err("the call is gone"),
            ToolError::UnknownCall
        );
        result
    };
    // A retargeted folder, another machine name and another kind of machine are each a changed payload.
    assert_eq!(run_after(&|host| host.retarget("/srv/other")).expect_err("changed"), ToolError::PayloadChanged);
    assert_eq!(run_after(&|host| host.move_to(FsKind::Remote, "other box")).expect_err("changed"), ToolError::PayloadChanged);
    assert_eq!(run_after(&|host| host.move_to(FsKind::Local, "build box")).expect_err("changed"), ToolError::PayloadChanged);
}

#[test]
fn a_command_after_untrusted_text_is_tainted_and_a_read_is_not() {
    let host = CommandHost::remote();
    let table = table();
    let scope = scope(3);
    let read = |id: &str| ToolCall { id: id.to_owned(), name: "read_file".to_owned(), args: json!({"path": "/srv/app/notes.txt"}) };

    // A fresh turn: no mark.
    let first = table.propose(&host, &scope, &call("c1", json!({"command": "make"}))).expect("a card");
    assert!(!first.tainted && first.taint.is_empty());
    table.decide("chat-1", "c1", Decision::Allow, &first.hash).expect("allowed");
    table.run(&host, &scope, "c1", &CancelToken::new()).expect("ran");

    // After a successful command the next one carries the mark `output of run_command`.
    let second = table.propose(&host, &scope, &call("c2", json!({"command": "make again"}))).expect("a card");
    assert!(second.tainted);
    assert_eq!(second.taint, ["output of run_command"]);

    // A read adds its own label, and a read's card is not marked by a read.
    let before = table.propose(&host, &scope, &read("r1")).expect("a read card");
    table.decide("chat-1", "r1", Decision::Allow, &before.hash).expect("allowed");
    table.run(&host, &scope, "r1", &CancelToken::new()).expect("the read ran");
    let third = table.propose(&host, &scope, &call("c3", json!({"command": "make third"}))).expect("a card");
    assert_eq!(third.taint, ["output of run_command", "output of read_file"]);

    // A command card proposed before untrusted text came in is void when it is decided after it.
    let waiting = table.propose(&host, &scope, &call("c4", json!({"command": "make fourth"}))).expect("a card");
    let again = table.propose(&host, &scope, &read("r2")).expect("a read card");
    table.decide("chat-1", "r2", Decision::Allow, &again.hash).expect("allowed");
    table.run(&host, &scope, "r2", &CancelToken::new()).expect("the read ran");
    let before_count = host.ran().len();
    assert_eq!(table.decide("chat-1", "c4", Decision::Allow, &waiting.hash), Err(ToolError::TaintChanged));
    assert_eq!(host.ran().len(), before_count, "a voided command ran");

    // A turn that has read nothing has a fresh mark.
    let other = Scope { conversation: "chat-1".to_owned(), turn: 2, pane: 3 };
    let fresh = table.propose(&host, &other, &call("c5", json!({"command": "make"}))).expect("a card");
    assert!(!fresh.tainted);
}

#[test]
fn no_debug_output_of_the_report_types_shows_text() {
    let folder = Folder::new(DIR_CANARY);
    let host = CommandHost::local();
    let table = table();
    let cwd = folder.path.to_str().expect("utf-8");
    let command = format!("echo {OUT_CANARY}");
    let pending = table
        .propose(&host, &scope(1), &call("call_1", json!({"command": command, "cwd": cwd})))
        .expect("a card");
    let result = result(OUT_CANARY, &format!("{OUT_CANARY} {DIR_CANARY}"), Some(3), ExecEnd::Finished);
    let shown: Shown = fit(OUT_CANARY, DIR_CANARY, SHOWN_BYTES);
    let output = report(&result, 60);
    for text in [
        format!("{shown:?} {shown:#?}"),
        format!("{output:?} {output:#?}"),
        format!("{result:?} {result:#?}"),
        format!("{pending:?} {pending:#?}"),
    ] {
        assert!(!text.contains(OUT_CANARY) && !text.contains(DIR_CANARY) && !text.contains(&folder.canonical), "{text}");
    }
    assert!(format!("{shown:?}").contains(&OUT_CANARY.len().to_string()) || format!("{shown:?}").contains("Size"), "sizes are shown");
    assert!(format!("{result:?}").contains("stdout_bytes"), "the sizes of the exec result are shown");
    assert!(format!("{output:?}").contains("failure"), "the flag is shown");
}
