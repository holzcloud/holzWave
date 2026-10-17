//! The golden of the command card: the page keeps its own tables, caps and words for a command card, and this test writes
//! the cards the real table builds, the real limits and the real sentences to `app/tests/fixtures/ai_command_card.json`, so
//! that a changed member, limit or sentence fails here and in the page's tests in the same commit.
//!
//! The document is built from the real table and constants only. No network, no process, no engine. `UPDATE_FIXTURES=1`
//! rewrites the file; without it the file must equal what is built now. The cards name placeholder folders that exist on no
//! machine, so the document is the same everywhere; the file is written on a Unix machine, where such a path is kept as it is.

#![cfg(unix)]

use std::path::PathBuf;

use holzwave_core::ai::agent::STOPPED_RESULT;
use holzwave_core::ai::provider::CancelToken;
use holzwave_core::ai::tools::{
    ApprovalTable, CAPTURE_BYTES, CWD_MISSING, CWD_REMOTE_RELATIVE, CWD_UNKNOWN, DEFAULT_COMMAND_SECS,
    DenyPolicy, FsKind, MAX_COMMAND_BYTES, MAX_COMMAND_SECS, MAX_PATH_BYTES, Pending, REMOTE_FAILED,
    REMOTE_TOO_LONG, Resolved, SHOWN_BYTES, START_FAILED, Scope, Ticket, Tool, ToolError, ToolHost, ToolName, ToolOutput,
    report,
};
use holzwave_core::ai::types::ToolCall;
use holzwave_core::layout::PaneId;
use holzwave_core::ssh::{ExecEnd, ExecResult};
use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use serde_json::json;

/// A key that never belongs in the file. It appears in this test only: a hit anywhere else is a leak.
const CANARY: &str = "zq-golden-command-3f9a1c";

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures/ai_command_card.json")
}

fn update_fixtures() -> bool {
    std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
}

/// A machine that runs commands: where, what it is called, and the folder it answers when the call names none.
struct Machine {
    fs: FsKind,
    pane: PaneId,
    target: &'static str,
    home: &'static str,
}

impl ToolHost for Machine {
    fn available(&self, _tool: ToolName, _pane: PaneId) -> bool {
        true
    }

    fn resolve(&self, _scope: &Scope, tool: &Tool) -> Result<Resolved, ToolError> {
        let mut resolved = match self.fs {
            FsKind::Local => Resolved::local(self.pane, self.target),
            FsKind::Remote => Resolved::remote(self.pane, self.target),
        };
        if let Tool::RunCommand(args) = tool {
            resolved = resolved.with_cwd(args.cwd.as_deref().unwrap_or(self.home));
        }
        Ok(resolved)
    }

    fn run(&self, _ticket: &Ticket, _cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(String::new()))
    }
}

/// The card the table builds for a `run_command` call with `args` on `machine`.
fn card(machine: &Machine, args: serde_json::Value) -> Pending {
    let table = ApprovalTable::new(DenyPolicy::new(Vec::new()));
    let scope = Scope { conversation: "chat-1".to_owned(), turn: 1, pane: machine.pane };
    let call = ToolCall { id: "call_1".to_owned(), name: "run_command".to_owned(), args };
    table.propose(machine, &scope, &call).unwrap_or_else(|error| panic!("the card was refused: {error}"))
}

#[derive(Serialize)]
struct Cards {
    local: Pending,
}

fn cards() -> Cards {
    let local = Machine { fs: FsKind::Local, pane: 1, target: "This computer", home: "/Users/holz/proj" };
    Cards { local: card(&local, json!({"command": "printf 'build ok\\n'"})) }
}

/// The longest value of one payload row a legal command can have, and the largest total of the values of a card.
///
/// Measured through the table: a command of the most bytes the tool allows, made of the characters that need the longest
/// escape per byte, with a working directory of the most characters the tool allows.
fn measured_limits() -> Limits {
    let machine = Machine { fs: FsKind::Remote, pane: 3, target: "build box", home: "/" };
    let mut longest = 0;
    let mut total = 0;
    for unit in ['\u{00ad}', '\u{200b}', '\u{e0041}', '\\'] {
        let command: String = std::iter::repeat_n(unit, MAX_COMMAND_BYTES / unit.len_utf8()).collect();
        let cwd = format!("/{}", "a".repeat(MAX_PATH_BYTES - 1));
        let pending = card(&machine, json!({"command": command, "cwd": cwd}));
        let this_value = pending.payload.iter().find(|f| f.name == "command").map_or(0, |f| f.value.chars().count());
        longest = longest.max(this_value);
        total = total.max(pending.payload.iter().map(|field| field.value.chars().count()).sum());
    }
    Limits {
        command_value_chars: longest,
        payload_total_chars: total,
        shown_bytes: SHOWN_BYTES,
        capture_bytes: CAPTURE_BYTES,
        default_timeout_secs: DEFAULT_COMMAND_SECS,
        max_timeout_secs: MAX_COMMAND_SECS,
    }
}

#[derive(Serialize)]
struct Limits {
    command_value_chars: usize,
    payload_total_chars: usize,
    shown_bytes: usize,
    capture_bytes: usize,
    default_timeout_secs: u64,
    max_timeout_secs: u64,
}

/// An object from a name to a phrase, in the order given.
struct Named(Vec<(&'static str, String)>);

impl Serialize for Named {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, phrase) in &self.0 {
            map.serialize_entry(name, phrase)?;
        }
        map.end()
    }
}

/// An exec result with the given parts.
fn finished(stdout: &str, stderr: &str, status: Option<u32>, end: ExecEnd) -> ExecResult {
    ExecResult { stdout: stdout.as_bytes().to_vec(), stderr: stderr.as_bytes().to_vec(), status, end, truncated: false, dropped: 0 }
}

#[derive(Serialize)]
struct Reports {
    ok: String,
    failed: String,
    timed_out: String,
    lost: String,
    no_output: String,
    cut: String,
}

/// The texts `report` makes from fixed results.
fn reports() -> Reports {
    let mut cut = finished("", "", Some(0), ExecEnd::Finished);
    cut.stdout = format!("{}\nlast line\n", "x".repeat(CAPTURE_BYTES - 11)).into_bytes();
    cut.truncated = true;
    cut.dropped = 4096;
    let text = |result: &ExecResult| report(result, DEFAULT_COMMAND_SECS).into_text();
    Reports {
        ok: text(&finished("build ok\n", "", Some(0), ExecEnd::Finished)),
        failed: text(&finished("", "error[E0432]: unresolved import\n", Some(101), ExecEnd::Finished)),
        timed_out: text(&finished("", "", None, ExecEnd::TimedOut)),
        lost: text(&finished("", "", None, ExecEnd::Lost)),
        no_output: text(&finished("", "", Some(0), ExecEnd::Finished)),
        cut: text(&cut),
    }
}

#[derive(Serialize)]
struct Texts {
    payload_changed: String,
    stopped: String,
    faults: Named,
    report: Reports,
}

#[derive(Serialize)]
struct Document {
    cards: Cards,
    limits: Limits,
    texts: Texts,
}

fn document() -> Document {
    let faults = Named(vec![
        ("CWD_MISSING", CWD_MISSING.to_owned()),
        ("CWD_UNKNOWN", CWD_UNKNOWN.to_owned()),
        ("CWD_REMOTE_RELATIVE", CWD_REMOTE_RELATIVE.to_owned()),
        ("REMOTE_TOO_LONG", REMOTE_TOO_LONG.to_owned()),
        ("START_FAILED", START_FAILED.to_owned()),
        ("REMOTE_FAILED", REMOTE_FAILED.to_owned()),
    ]);
    Document {
        cards: cards(),
        limits: measured_limits(),
        texts: Texts {
            payload_changed: ToolError::PayloadChanged.to_string(),
            // What the loop hands the model and the page when a stop ends a call: the fixed sentence of the loop.
            stopped: STOPPED_RESULT.to_owned(),
            faults,
            report: reports(),
        },
    }
}

fn text_of(document: &Document) -> String {
    let mut text = serde_json::to_string_pretty(document).expect("the document is JSON");
    text.push('\n');
    text
}

#[test]
fn the_command_card_golden_matches_the_cards_the_table_builds() {
    let text = text_of(&document());
    assert_eq!(text, text_of(&document()), "the document is built twice and equal");
    assert!(!text.contains(CANARY) && !text.contains("zq-"), "a canary reached the golden");

    // What the page relies on: the order of the top-level members, the kind of the card and the limits.
    let value: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(value["cards"]["local"]["effect"], json!("exec"));
    assert_eq!(value["cards"]["local"]["tool"], json!("run_command"));
    let top = text.lines().filter(|line| line.starts_with("  \"")).map(str::trim).collect::<Vec<_>>();
    assert_eq!(top, ["\"cards\": {", "\"limits\": {", "\"texts\": {"], "the members are in the order cards, limits, texts");

    let path = golden_path();
    if update_fixtures() {
        std::fs::write(&path, &text).expect("write the golden");
        return;
    }
    let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("{} cannot be read ({error}); run this test with UPDATE_FIXTURES=1 to write it", path.display())
    });
    assert_eq!(on_disk, text, "the golden is stale; run this test with UPDATE_FIXTURES=1 and review the change");
}
