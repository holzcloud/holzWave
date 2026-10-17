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
    ApprovalTable, Decision, DenyPolicy, FsKind, Pending, Resolved, Scope, SHOWN_BYTES, Ticket, Tool,
    ToolError, ToolHost, ToolName, ToolOutput, exit_line, fit, report, timed_out_line,
};
use holzwave_core::ai::types::ToolCall;
use holzwave_core::layout::PaneId;
use holzwave_core::ssh::{ExecEnd, ExecResult};
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
