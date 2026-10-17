//! The planner, the diff and the approval table's handling of a write, with no terminal, no network and no page.
//!
//! A write host in memory stands in for the shell. It answers `inspect` with the text of a file and its change time, and
//! it records what it is asked to run. What these tests prove: the diff of a card is made from the very text whose digest
//! is in the hash and rebuilds both texts exactly, the card of a read tool keeps its shape, a host that does not look at
//! files keeps the old card, and a write runs only on the hash of the card the person was shown.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use holzwave_core::ai::provider::CancelToken;
use holzwave_core::ai::tools::{
    ApprovalTable, Base, Change, ChangeFault, Decision, DenyPolicy, DiffKind, DiffRow, FsKind, Inspected, MAX_BASE_BYTES,
    Pending, Resolved, Scope, Ticket, Tool, ToolError, ToolHost, ToolName, ToolOutput, diff, diff_with, plan, visible,
    visible_multiline,
};
use holzwave_core::ai::types::ToolCall;
use holzwave_core::layout::PaneId;
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod common;

/// Text of the old file; it appears in test code only, so a hit anywhere else is a leak.
const OLD_CANARY: &str = "zq-old-text-4d2e91";
/// Text the model writes; it appears in test code only.
const NEW_CANARY: &str = "zq-new-text-b83f17";

const NOTES: &str = "/w/notes.txt";

/// What the write host holds for one path: the text and the change time.
#[derive(Clone)]
struct File {
    text: String,
    stamp: i64,
}

struct State {
    files: BTreeMap<String, File>,
    inspects: usize,
    reads: usize,
    ran: Vec<Ran>,
    kind: FsKind,
}

/// One call the host was asked to run: what its ticket held.
struct Ran {
    path: Option<String>,
    base: Option<Base>,
    after: Result<String, ChangeFault>,
}

/// A remote host on pane 3 called `build box`.
#[derive(Clone)]
struct WriteHost {
    state: Arc<Mutex<State>>,
}

impl WriteHost {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                files: BTreeMap::new(),
                inspects: 0,
                reads: 0,
                ran: Vec::new(),
                kind: FsKind::Remote,
            })),
        }
    }

    /// The same host acting on this computer instead.
    fn on_this_computer(self) -> Self {
        self.state().kind = FsKind::Local;
        self
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn with_file(self, path: &str, text: &str, stamp: i64) -> Self {
        self.state().files.insert(path.to_owned(), File { text: text.to_owned(), stamp });
        self
    }

    /// The file changes without a call: another program wrote it.
    fn change(&self, path: &str, text: &str, stamp: i64) {
        self.state().files.insert(path.to_owned(), File { text: text.to_owned(), stamp });
    }

    fn remove(&self, path: &str) {
        self.state().files.remove(path);
    }

    fn inspects(&self) -> usize {
        self.state().inspects
    }

    fn run_count(&self) -> usize {
        self.state().ran.len()
    }
}

impl ToolHost for WriteHost {
    fn available(&self, _tool: ToolName, _pane: PaneId) -> bool {
        true
    }

    fn resolve(&self, _scope: &Scope, tool: &Tool) -> Result<Resolved, ToolError> {
        let mut resolved = match self.state().kind {
            FsKind::Remote => Resolved::remote(3, "build box"),
            FsKind::Local => Resolved::local(3, "This computer"),
        };
        if let Some(path) = tool.path() {
            resolved = resolved.with_path(path);
        }
        Ok(resolved)
    }

    fn inspect(&self, resolved: &Resolved) -> Result<Option<Inspected>, ToolError> {
        let mut state = self.state();
        state.inspects += 1;
        let Some(path) = resolved.path() else { return Ok(None) };
        state.reads += 1;
        Ok(Some(match state.files.get(path) {
            Some(file) => Inspected::new(Base::Text(file.text.clone()), Some(file.stamp)),
            None => Inspected::new(Base::Absent, None),
        }))
    }

    fn run(&self, ticket: &Ticket, _cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        let base = ticket.resolved().base().cloned();
        let after = match &base {
            Some(base) => plan(ticket.tool(), base).map(|change| change.after),
            None => Err(ChangeFault::NotAWrite),
        };
        self.state().ran.push(Ran { path: ticket.resolved().path().map(str::to_owned), base, after });
        Ok(ToolOutput::new("done".to_owned()))
    }
}

fn scope() -> Scope {
    Scope { conversation: "chat-1".to_owned(), turn: 1, pane: 3 }
}

fn table() -> ApprovalTable {
    ApprovalTable::new(DenyPolicy::new(Vec::new()))
}

fn write_call(id: &str, path: &str, content: &str) -> ToolCall {
    ToolCall { id: id.to_owned(), name: "write_file".to_owned(), args: json!({"path": path, "content": content}) }
}

fn sha256(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256:{hex}")
}

/// The members of the top-level object of `text`, in the order they were written.
struct Ordered(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Members;
        impl<'de> Visitor<'de> for Members {
            type Value = Ordered;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Ordered, A::Error> {
                let mut members = Vec::new();
                while let Some((name, value)) = map.next_entry::<String, Value>()? {
                    members.push((name, value));
                }
                Ok(Ordered(members))
            }
        }
        deserializer.deserialize_map(Members)
    }
}

fn member_names(text: &str) -> Vec<String> {
    serde_json::from_str::<Ordered>(text).expect("an object").0.into_iter().map(|(name, _)| name).collect()
}

fn payload_names(card: &Value) -> Vec<String> {
    card["payload"].as_array().expect("a payload").iter().map(|f| f["name"].as_str().expect("a name").to_owned()).collect()
}

fn payload_value(card: &Value, name: &str) -> Option<String> {
    card["payload"]
        .as_array()?
        .iter()
        .find(|field| field["name"] == name)
        .map(|field| field["value"].as_str().expect("a value").to_owned())
}

#[test]
fn a_write_over_an_existing_file_builds_a_card_with_a_diff_and_the_host_runs_on_the_approved_hash() {
    let old = format!("alpha\nbeta {OLD_CANARY}\ngamma\n");
    let new = format!("alpha\nbeta {NEW_CANARY}\ngamma\n");
    let host = WriteHost::new().with_file(NOTES, &old, 1_700_000_000);
    let table = table();

    let pending = table.propose(&host, &scope(), &write_call("call_1", NOTES, &new)).expect("a card");
    let text = serde_json::to_string(&pending).expect("serialises");
    assert_eq!(
        member_names(&text),
        ["conversation", "call_id", "tool", "effect", "pane", "target", "remote", "payload", "hash", "tainted", "taint", "diff"]
    );
    let card: Value = serde_json::from_str(&text).expect("a card");
    assert_eq!(card["effect"], json!("write"));
    assert_eq!(
        payload_names(&card),
        ["tool", "pane", "target", "remote", "path", "content", "resolved_path", "stamp", "base"]
    );
    assert_eq!(payload_value(&card, "stamp").as_deref(), Some("1700000000"));
    assert_eq!(payload_value(&card, "base"), Some(sha256(&old)), "the digest of the text the diff is made from");
    let hash = card["hash"].as_str().expect("a hash").to_owned();
    assert!(hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{hash}");

    let diff = &card["diff"];
    assert_eq!(diff["kind"], json!("replace"));
    assert_eq!((diff["added"].clone(), diff["removed"].clone()), (json!(1), json!(1)));
    assert_eq!(diff["before_bytes"], json!(old.len()));
    assert_eq!(diff["after_bytes"], json!(new.len()));
    let rows: Vec<Value> = diff["rows"].as_array().expect("rows").clone();
    let expected = [
        json!({"op": "ctx", "old": 1, "new": 1, "text": "alpha"}),
        json!({"op": "del", "old": 2, "new": null, "text": format!("beta {OLD_CANARY}")}),
        json!({"op": "add", "old": null, "new": 2, "text": format!("beta {NEW_CANARY}")}),
        json!({"op": "ctx", "old": 3, "new": 3, "text": "gamma"}),
    ];
    assert_eq!(rows, expected, "the rows are the unchanged line, the removed one, the added one and the unchanged one");
    for row in diff["rows"].as_array().expect("rows") {
        let names = member_names(&row.to_string());
        assert!(names.len() == 4 || names.len() == 2, "{names:?}");
    }
    assert_eq!(member_names(&serde_json::to_string(&pending.diff().expect("a diff")).expect("serialises")),
        ["kind", "added", "removed", "before_bytes", "after_bytes", "rows"]);
    let first = serde_json::to_string(&pending.diff().expect("a diff").rows[1]).expect("serialises");
    assert_eq!(member_names(&first), ["op", "old", "new", "text"], "the absent number is null and never left out");

    // The call runs on the hash of the card and reaches the host once, with the base the card was made from.
    assert_eq!(host.run_count(), 0, "nothing runs before a decision");
    table.decide("chat-1", "call_1", Decision::Allow, &hash).expect("decided");
    table.run(&host, &scope(), "call_1", &CancelToken::new()).expect("ran");
    assert_eq!(host.run_count(), 1);
    {
        let state = host.state();
        let ran = &state.ran[0];
        assert_eq!(ran.path.as_deref(), Some(NOTES));
        assert_eq!(ran.base, Some(Base::Text(old.clone())), "the ticket holds the text the card showed");
        assert_eq!(ran.after, Ok(new.clone()), "the text to save is the one the planner makes from the ticket");
    }
    assert_eq!(host.inspects(), 2, "the file is looked at when the call is proposed and again when it runs");
    assert!(table.pending("chat-1").is_empty());
}

/// A small pseudo-random generator that never reads the clock.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        usize::try_from((self.0 >> 33) % bound as u64).unwrap_or(0)
    }
}

const WORDS: [&str; 5] = ["alpha", "beta", "gamma", "delta", "epsilon"];

/// A text of up to 30 lines of five words, with a final newline or none.
fn random_text(rng: &mut Lcg) -> String {
    let lines: Vec<&str> = (0..rng.next(31)).map(|_| WORDS[rng.next(5)]).collect();
    let mut text = lines.join("\n");
    if !lines.is_empty() && rng.next(2) == 0 {
        text.push('\n');
    }
    text
}

/// The second text, made from the first by random inserts, deletes, replaces and duplicated lines.
fn mutate(rng: &mut Lcg, text: &str) -> String {
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_owned).collect();
    for _ in 0..rng.next(6) {
        let at = rng.next(lines.len() + 1);
        match rng.next(4) {
            0 => lines.insert(at, format!("{}\n", WORDS[rng.next(5)])),
            1 if at < lines.len() => {
                lines.remove(at);
            }
            2 if at < lines.len() => lines[at] = format!("{}\n", WORDS[rng.next(5)]),
            3 if at < lines.len() => {
                let copy = lines[at].clone();
                lines.insert(at, copy);
            }
            _ => {}
        }
    }
    lines.concat()
}

/// The text of the rows with these operations, a line break after each row except one that a note follows.
fn joined(rows: &[DiffRow], keep: impl Fn(&DiffRow) -> Option<&str>) -> String {
    let mut out = String::new();
    for (at, row) in rows.iter().enumerate() {
        if let Some(text) = keep(row) {
            out.push_str(text);
            let noted = matches!(rows.get(at + 1), Some(DiffRow::Note { .. }));
            if !noted {
                out.push('\n');
            }
        }
    }
    out
}

/// The least number of added and removed lines that turn `old` into `new`, by a table of longest common subsequences.
fn least_edits(old: &[&str], new: &[&str]) -> usize {
    let mut table = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            table[i][j] = if old[i] == new[j] { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
        }
    }
    old.len() + new.len() - 2 * table[0][0]
}

#[test]
fn the_diff_rows_rebuild_the_new_text_for_edits_of_every_shape() {
    let mut rng = Lcg(0x5eed_1234_abcd_ef01);
    let mut compared = 0;
    for case in 0..300 {
        let old = random_text(&mut rng);
        let new = if rng.next(10) == 0 { random_text(&mut rng) } else { mutate(&mut rng, &old) };
        if old == new {
            continue;
        }
        compared += 1;
        let change = Change { before: Some(old.clone()), after: new.clone(), kind: DiffKind::Replace };
        let full = diff_with(&change, usize::MAX).unwrap_or_else(|fault| panic!("case {case}: {fault:?}"));
        assert!(full.rows.iter().all(|row| !matches!(row, DiffRow::Gap { .. })), "case {case}: a gap in a full diff");

        let rebuilt_old = joined(&full.rows, |row| match row {
            DiffRow::Ctx { text, .. } | DiffRow::Del { text, .. } => Some(text.as_str()),
            _ => None,
        });
        let rebuilt_new = joined(&full.rows, |row| match row {
            DiffRow::Ctx { text, .. } | DiffRow::Add { text, .. } => Some(text.as_str()),
            _ => None,
        });
        // A line that has no line break is the last one and a note follows it, so the texts come out exactly.
        assert_eq!(rebuilt_old, old, "case {case}: the old text");
        assert_eq!(rebuilt_new, new, "case {case}: the new text");

        let adds = full.rows.iter().filter(|row| matches!(row, DiffRow::Add { .. })).count();
        let dels = full.rows.iter().filter(|row| matches!(row, DiffRow::Del { .. })).count();
        assert_eq!((full.added, full.removed), (adds, dels), "case {case}: the counts are the rows");

        let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
        let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
        assert_eq!(adds + dels, least_edits(&old_lines, &new_lines), "case {case}: the script is a shortest one");

        // Line numbers rise by one in each column.
        let (mut next_old, mut next_new) = (1u32, 1u32);
        for row in &full.rows {
            match row {
                DiffRow::Ctx { old, new, .. } => {
                    assert_eq!((*old, *new), (next_old, next_new), "case {case}");
                    next_old += 1;
                    next_new += 1;
                }
                DiffRow::Del { old, .. } => {
                    assert_eq!(*old, next_old, "case {case}");
                    next_old += 1;
                }
                DiffRow::Add { new, .. } => {
                    assert_eq!(*new, next_new, "case {case}");
                    next_new += 1;
                }
                DiffRow::Gap { .. } | DiffRow::Note { .. } => {}
            }
        }

        // The default context is the full rows with exactly the unchanged runs folded: a gap stands for that many
        // unchanged lines (and the note of the last one), every other row is the next row of the full diff.
        let folded = diff(&change).expect("a folded diff");
        let mut cursor = 0;
        for row in &folded.rows {
            if let DiffRow::Gap { skipped } = row {
                assert!(*skipped >= 2, "case {case}: a gap that skips one line");
                let mut left = *skipped;
                while left > 0 {
                    assert!(matches!(full.rows.get(cursor), Some(DiffRow::Ctx { .. })), "case {case}: a gap hides a change");
                    cursor += 1;
                    left -= 1;
                }
                if matches!(full.rows.get(cursor), Some(DiffRow::Note { .. })) {
                    cursor += 1;
                }
            } else {
                assert_eq!(Some(row), full.rows.get(cursor), "case {case}: the rows keep their order");
                cursor += 1;
            }
        }
        assert_eq!(cursor, full.rows.len(), "case {case}: the folded rows account for every row");
    }
    assert!(compared >= 200, "only {compared} of 300 pairs differed");
}

fn kinds(diff: &holzwave_core::ai::tools::Diff) -> Vec<String> {
    diff.rows
        .iter()
        .map(|row| match row {
            DiffRow::Ctx { .. } => "ctx".to_owned(),
            DiffRow::Del { .. } => "del".to_owned(),
            DiffRow::Add { .. } => "add".to_owned(),
            DiffRow::Gap { skipped } => format!("gap {skipped}"),
            DiffRow::Note { text } => {
                assert_eq!(text, "No newline at end of file");
                "note".to_owned()
            }
        })
        .collect()
}

fn lines(range: std::ops::RangeInclusive<usize>, changed: &[usize], tag: &str) -> String {
    range.map(|n| if changed.contains(&n) { format!("{tag} {n}\n") } else { format!("line {n}\n") }).collect()
}

fn replace(before: &str, after: &str) -> Change {
    Change { before: Some(before.to_owned()), after: after.to_owned(), kind: DiffKind::Replace }
}

#[test]
fn context_gaps_and_a_missing_final_newline_are_told_in_the_rows() {
    // Twenty lines with line 10 changed: nine unchanged lines before keep three, eleven after keep three.
    let diff20 = diff(&replace(&lines(1..=20, &[], "new"), &lines(1..=20, &[10], "new"))).expect("a diff");
    assert_eq!(kinds(&diff20), ["gap 6", "ctx", "ctx", "ctx", "del", "add", "ctx", "ctx", "ctx", "gap 7"]);

    // Two changes with an unchanged run between them: seven lines are shown whole, eight fold their middle two.
    let seven = diff(&replace(&lines(1..=20, &[], "new"), &lines(1..=20, &[1, 9], "new"))).expect("a diff");
    let seven_kinds = kinds(&seven);
    assert_eq!(seven_kinds.iter().filter(|k| k.starts_with("gap")).count(), 1, "{seven_kinds:?}");
    assert_eq!(&seven_kinds[..9], ["del", "add", "ctx", "ctx", "ctx", "ctx", "ctx", "ctx", "ctx"], "all seven shown");
    let eight = diff(&replace(&lines(1..=20, &[], "new"), &lines(1..=20, &[1, 10], "new"))).expect("a diff");
    assert_eq!(
        &kinds(&eight)[..11],
        ["del", "add", "ctx", "ctx", "ctx", "gap 2", "ctx", "ctx", "ctx", "del", "add"],
        "three, a gap of two, three"
    );

    // Unchanged lines at the start: four are shown whole, five keep the last three and fold the first two.
    let four = diff(&replace(&lines(1..=6, &[], "new"), &lines(1..=6, &[5], "new"))).expect("a diff");
    assert_eq!(&kinds(&four)[..6], ["ctx", "ctx", "ctx", "ctx", "del", "add"]);
    let five = diff(&replace(&lines(1..=7, &[], "new"), &lines(1..=7, &[6], "new"))).expect("a diff");
    assert_eq!(&kinds(&five)[..6], ["gap 2", "ctx", "ctx", "ctx", "del", "add"]);
    if let DiffRow::Ctx { old, new, .. } = &five.rows[1] {
        assert_eq!((*old, *new), (3, 3), "the last three lines before the change are kept");
    } else {
        panic!("a context row");
    }

    // A last line without a line break is told by a note after it.
    let changed = diff(&replace("a\nb", "a\nb\nc")).expect("a diff");
    assert_eq!(kinds(&changed), ["ctx", "del", "note", "add", "add", "note"]);
    let only_newline = diff(&replace("a", "a\n")).expect("a diff");
    assert_eq!(kinds(&only_newline), ["del", "note", "add"]);
    let same_last = diff(&replace("a\nb", "x\nb")).expect("a diff");
    assert_eq!(kinds(&same_last), ["del", "add", "ctx", "note"], "an unchanged last line without a break has one note");
}

#[test]
fn a_read_card_keeps_its_shape_and_a_host_that_does_not_inspect_keeps_the_old_card() {
    struct Plain;
    impl ToolHost for Plain {
        fn available(&self, _tool: ToolName, _pane: PaneId) -> bool {
            true
        }
        fn resolve(&self, _scope: &Scope, tool: &Tool) -> Result<Resolved, ToolError> {
            let mut resolved = Resolved::remote(3, "build box");
            if let Some(path) = tool.path() {
                resolved = resolved.with_path(path);
            }
            Ok(resolved)
        }
        fn run(&self, _ticket: &Ticket, _cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::new(String::new()))
        }
    }

    // A read card from the table has the eleven members in the old order, no diff and no base row, also for a host that
    // does look at files.
    let host = WriteHost::new().with_file(NOTES, "alpha\n", 7);
    let table = table();
    let read = ToolCall { id: "r1".to_owned(), name: "read_file".to_owned(), args: json!({"path": NOTES}) };
    let pending = table.propose(&host, &scope(), &read).expect("a card");
    let text = serde_json::to_string(&pending).expect("serialises");
    assert_eq!(
        member_names(&text),
        ["conversation", "call_id", "tool", "effect", "pane", "target", "remote", "payload", "hash", "tainted", "taint"]
    );
    let card: Value = serde_json::from_str(&text).expect("a card");
    assert!(!payload_names(&card).contains(&"base".to_owned()));
    assert_eq!(host.inspects(), 0, "a read call never asks the host to inspect");

    // A write proposed to a host whose inspect answers none has no diff and no base row.
    let plain = Plain;
    let table = self::table();
    let pending = table.propose(&plain, &scope(), &write_call("w1", NOTES, "text\n")).expect("a card");
    let text = serde_json::to_string(&pending).expect("serialises");
    assert_eq!(
        member_names(&text),
        ["conversation", "call_id", "tool", "effect", "pane", "target", "remote", "payload", "hash", "tainted", "taint"]
    );
    let card: Value = serde_json::from_str(&text).expect("a card");
    assert_eq!(payload_names(&card), ["tool", "pane", "target", "remote", "path", "content", "resolved_path"]);
    assert!(pending.diff().is_none());
}

fn edit_call(id: &str, path: &str, old: &str, new: &str, all: bool) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: "edit_file".to_owned(),
        args: json!({"path": path, "old_text": old, "new_text": new, "replace_all": all}),
    }
}

/// The card as JSON, for a call that must make one.
fn card_of(table: &ApprovalTable, host: &WriteHost, call: &ToolCall) -> Value {
    let pending = table.propose(host, &scope(), call).unwrap_or_else(|error| panic!("no card: {error}"));
    serde_json::to_value(&pending).expect("a card")
}

/// The sentence the model hears for a call that the table refused before any card existed.
fn refusal_of(table: &ApprovalTable, host: &WriteHost, call: &ToolCall) -> String {
    let error = table.propose(host, &scope(), call).expect_err("a call that makes no card");
    assert!(matches!(error, ToolError::InvalidArguments(_)), "{error:?}");
    assert!(table.pending("chat-1").iter().all(|card| card.call_id != call.id), "a refused call leaves no card waiting");
    error.to_result(&call.id).content
}

fn texts_of(card: &Value) -> Vec<String> {
    card["diff"]["rows"].as_array().expect("rows").iter().filter_map(|row| row["text"].as_str().map(str::to_owned)).collect()
}

#[test]
fn hidden_characters_are_escaped_in_the_rows_and_the_payload() {
    let old = "plain\nbad\u{202e}line\u{200b}x\n\tindent\u{e0041}\r\nback\\slash\n";
    let new = "plain\nbad\u{202e}LINE\u{200b}x\n\tindent\u{e0041}\r\nback\\slash\nu{202e} literal\n";
    let host = WriteHost::new().with_file(NOTES, old, 5);
    let card = card_of(&table(), &host, &write_call("c1", NOTES, new));
    let rows = card["diff"]["rows"].as_array().expect("rows");
    let shown = serde_json::to_string(rows).expect("rows");
    for raw in ['\u{202e}', '\u{200b}', '\u{e0041}', '\r'] {
        assert!(!shown.contains(raw), "a raw {raw:?} is in the rows");
    }
    let lines: Vec<String> = texts_of(&card);
    assert!(lines.iter().any(|t| t.contains("\\u{202e}")), "the bidi override is written out: {lines:?}");
    assert!(lines.iter().any(|t| t.contains("\\u{200b}")), "{lines:?}");
    assert!(lines.iter().any(|t| t.contains("\\u{e0041}")), "{lines:?}");
    assert!(lines.iter().any(|t| t.contains("\\t")), "a tab: {lines:?}");
    assert!(lines.iter().any(|t| t.ends_with("\\r") || t.contains("\\r")), "a carriage return is shown as \\r: {lines:?}");
    assert!(lines.iter().any(|t| t.contains("back\\\\slash")), "a backslash is doubled: {lines:?}");
    // A line that really holds a backslash and the letters differs from the line that holds the character.
    let escaped = visible("a\u{202e}b");
    let literal = visible("a\\u{202e}b");
    assert_ne!(escaped, literal);
    // Every row is `visible` of its line, and the content of the payload is `visible_multiline` of the content.
    let del: Vec<&str> = rows.iter().filter(|r| r["op"] == "del").map(|r| r["text"].as_str().expect("text")).collect();
    assert_eq!(del, [visible("bad\u{202e}line\u{200b}x")]);
    assert_eq!(payload_value(&card, "content"), Some(visible_multiline(new)));
}

#[test]
fn a_change_too_large_to_show_is_refused_with_a_phrase_and_no_card() {
    let lines = |tag: &str, n: usize| -> String { (0..n).map(|i| format!("{tag} {i}\n")).collect() };
    let table = table();
    let phrase = ChangeFault::NotShown.phrase();

    // 3000 lines replaced by 3000 other lines are 6000 rows: refused, nothing waits, nothing runs.
    let host = WriteHost::new().with_file(NOTES, &lines("old", 3000), 5);
    let said = refusal_of(&table, &host, &write_call("big", NOTES, &lines("new", 3000)));
    assert!(said.contains(phrase), "{said}");
    assert_eq!(host.run_count(), 0);

    // Exactly 4000 rows are shown, and one more pair is not.
    let host = WriteHost::new().with_file(NOTES, &lines("old", 2000), 5);
    let accepted = table.propose(&host, &scope(), &write_call("edge", NOTES, &lines("new", 2000))).expect("4000 rows");
    let accepted_diff = accepted.diff().expect("a diff").clone();
    assert_eq!(accepted_diff.rows.len(), 4000);
    let rebuilt_old: String = accepted_diff.rows.iter().filter_map(|r| if let DiffRow::Del { text, .. } = r { Some(format!("{text}\n")) } else { None }).collect();
    let rebuilt_new: String = accepted_diff.rows.iter().filter_map(|r| if let DiffRow::Add { text, .. } = r { Some(format!("{text}\n")) } else { None }).collect();
    assert_eq!((rebuilt_old, rebuilt_new), (lines("old", 2000), lines("new", 2000)), "the accepted diff rebuilds both texts");
    let host = WriteHost::new().with_file(NOTES, &lines("old", 2001), 5);
    let said = refusal_of(&table, &host, &write_call("over", NOTES, &lines("new", 2001)));
    assert!(said.contains(phrase), "{said}");

    // Ten scattered edits in a file of 20000 lines stay cheap and keep their context and gaps.
    let long: String = (0..20_000).map(|i| if i % 2000 == 7 { format!("XX {i}\n") } else { format!("l{i:05}\n") }).collect();
    assert!(long.len() < MAX_BASE_BYTES);
    let host = WriteHost::new().with_file(NOTES, &long, 5);
    let started = std::time::Instant::now();
    let card = card_of(&table, &host, &edit_call("scatter", NOTES, "XX", "YY", true));
    let spent = started.elapsed();
    println!("diff of 20000 lines with ten edits: {spent:?}");
    assert!(spent.as_secs() < 20, "ten scattered edits took {spent:?}");
    let rows = card["diff"]["rows"].as_array().expect("rows");
    assert_eq!(rows.iter().filter(|r| r["op"] == "del").count(), 10);
    assert_eq!(rows.iter().filter(|r| r["op"] == "add").count(), 10);
    assert!(rows.iter().any(|r| r["op"] == "gap"));
    assert!(rows.len() < 100, "context and gaps only: {}", rows.len());

    // One line of 600000 characters replaced by a short text is refused for its characters.
    let wide = "a".repeat(600_000);
    let host = WriteHost::new().with_file(NOTES, &wide, 5);
    let said = refusal_of(&table, &host, &write_call("wide", NOTES, "short"));
    assert!(said.contains(phrase), "{said}");
}

#[test]
fn edit_file_replaces_one_match_or_all_and_refuses_the_rest() {
    let table = table();
    let text = "one foo two\nfoo\nthree foo\n";
    let host = WriteHost::new().with_file(NOTES, text, 9);

    // One match: a card of kind edit with the eleven fields and the effective `replace_all`.
    let single = "keep\nonly bar here\nkeep\n";
    let host_one = WriteHost::new().with_file(NOTES, single, 9);
    let card = card_of(&table, &host_one, &edit_call("e1", NOTES, "bar", "baz", false));
    assert_eq!(card["diff"]["kind"], json!("edit"));
    assert_eq!(
        payload_names(&card),
        ["tool", "pane", "target", "remote", "path", "old_text", "new_text", "replace_all", "resolved_path", "stamp", "base"]
    );
    assert_eq!(payload_value(&card, "replace_all").as_deref(), Some("false"));

    // Three matches without `replace_all`: a refusal; with it, the card has the three changes.
    let said = refusal_of(&table, &host, &edit_call("e2", NOTES, "foo", "bar", false));
    assert!(said.contains(ChangeFault::Ambiguous.phrase()), "{said}");
    let card = card_of(&table, &host, &edit_call("e3", NOTES, "foo", "bar", true));
    let rows = card["diff"]["rows"].as_array().expect("rows");
    assert_eq!(rows.iter().filter(|r| r["op"] == "del").count(), 3);
    assert_eq!(rows.iter().filter(|r| r["op"] == "add").count(), 3);
    assert_eq!(payload_value(&card, "replace_all").as_deref(), Some("true"));

    // No match, equal texts, no file.
    let said = refusal_of(&table, &host, &edit_call("e4", NOTES, "absent", "x", false));
    assert!(said.contains(ChangeFault::NoMatch.phrase()), "{said}");
    let said = refusal_of(&table, &host, &edit_call("e5", NOTES, "foo", "foo", false));
    assert!(said.contains(ChangeFault::Same.phrase()), "{said}");
    let said = refusal_of(&table, &WriteHost::new(), &edit_call("e6", NOTES, "foo", "bar", false));
    assert!(said.contains(ChangeFault::NoFile.phrase()), "{said}");

    // Matches are counted without overlap: `aa` in `aaaa` is two.
    let host_aa = WriteHost::new().with_file(NOTES, "aaaa\n", 9);
    let said = refusal_of(&table, &host_aa, &edit_call("e7", NOTES, "aa", "b", false));
    assert!(said.contains(ChangeFault::Ambiguous.phrase()), "{said}");
    let card = card_of(&table, &host_aa, &edit_call("e8", NOTES, "aa", "b", true));
    assert_eq!(card["diff"]["after_bytes"], json!(3), "two matches replaced by `b`, then the line break");

    // A result over the limit is refused before any diff work.
    let large = "x".repeat(MAX_BASE_BYTES - 10);
    let host_large = WriteHost::new().with_file(NOTES, &large, 9);
    let said = refusal_of(&table, &host_large, &edit_call("e9", NOTES, "x", "yy", true));
    assert!(said.contains(ChangeFault::TooLarge.phrase()), "{said}");

    // The planner's text for the allowed call is the base with the replacement made by hand.
    let tool = Tool::parse("edit_file", &json!({"path": NOTES, "old_text": "foo", "new_text": "bar", "replace_all": true})).expect("a tool");
    let change = plan(&tool, &Base::Text(text.to_owned())).expect("a change");
    assert_eq!(change.after, "one bar two\nbar\nthree bar\n");
    let tool = Tool::parse("edit_file", &json!({"path": NOTES, "old_text": "bar", "new_text": "baz"})).expect("a tool");
    assert_eq!(plan(&tool, &Base::Text(single.to_owned())).expect("a change").after, "keep\nonly baz here\nkeep\n");

    // None of the phrases holds a path or text.
    for fault in [
        ChangeFault::NotAWrite, ChangeFault::NoFile, ChangeFault::NoMatch, ChangeFault::Ambiguous, ChangeFault::Same,
        ChangeFault::Unchanged, ChangeFault::TooLarge, ChangeFault::NotShown,
    ] {
        let phrase = fault.phrase();
        assert!(!phrase.contains('/') && !phrase.contains(OLD_CANARY) && !phrase.contains(NEW_CANARY), "{phrase}");
        assert!(!phrase.contains('\n') && !phrase.is_empty());
    }
}

#[test]
fn a_new_file_has_only_added_rows_and_no_stamp() {
    let table = table();
    let host = WriteHost::new();
    let card = card_of(&table, &host, &write_call("n1", "/w/new.txt", "one\ntwo\n"));
    assert_eq!(card["diff"]["kind"], json!("create"));
    assert_eq!(card["diff"]["before_bytes"], Value::Null);
    assert_eq!(card["diff"]["removed"], json!(0));
    let rows = card["diff"]["rows"].as_array().expect("rows");
    assert!(rows.iter().all(|r| r["op"] == "add"), "only added rows");
    assert_eq!(payload_names(&card), ["tool", "pane", "target", "remote", "path", "content", "resolved_path", "base"]);
    assert_eq!(payload_value(&card, "base").as_deref(), Some("none"));

    // An empty text for a file that does not exist is a card with no rows.
    let card = card_of(&table, &host, &write_call("n2", "/w/empty.txt", ""));
    assert_eq!(card["diff"]["rows"], json!([]));
    assert_eq!(card["diff"]["kind"], json!("create"));

    // The same text as the file holds is refused with its phrase, and no card exists.
    let host = WriteHost::new().with_file(NOTES, "same\n", 5);
    let said = refusal_of(&table, &host, &write_call("n3", NOTES, "same\n"));
    assert!(said.contains(ChangeFault::Unchanged.phrase()), "{said}");
}

#[test]
fn the_base_digest_and_the_stamp_are_part_of_the_hash() {
    let call = write_call("h1", NOTES, "text\n");
    let hash_with = |host: &WriteHost| table().propose(host, &scope(), &call).expect("a card").hash;
    let baseline = hash_with(&WriteHost::new().with_file(NOTES, "abc\n", 7));
    let one_byte = hash_with(&WriteHost::new().with_file(NOTES, "abd\n", 7));
    let stamp = hash_with(&WriteHost::new().with_file(NOTES, "abc\n", 8));
    let absent = hash_with(&WriteHost::new());
    let empty = hash_with(&WriteHost::new().with_file(NOTES, "", 7));
    let all = [&baseline, &one_byte, &stamp, &absent, &empty];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a, b, "two different worlds gave the same hash");
        }
    }
    assert_eq!(baseline, hash_with(&WriteHost::new().with_file(NOTES, "abc\n", 7)), "the same inputs give the same hash");

    // The row is `none` or `sha256:` and 64 lower-case hex digits, and its size is that length.
    for (host, expected) in [(WriteHost::new(), 4usize), (WriteHost::new().with_file(NOTES, "", 1), 71)] {
        let pending = table().propose(&host, &scope(), &call).expect("a card");
        let base = pending.payload.iter().find(|field| field.name == "base").expect("a base row");
        assert_eq!(base.bytes, expected);
        assert_eq!(base.value.len(), expected);
        if expected == 71 {
            assert!(base.value.starts_with("sha256:") && base.value[7..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        } else {
            assert_eq!(base.value, "none");
        }
    }
}

#[test]
fn a_file_changed_after_the_card_refuses_the_call_even_with_the_same_time_and_size() {
    let call = write_call("x1", NOTES, "text one\n");
    let propose_and_allow = |host: &WriteHost| -> ApprovalTable {
        let table = table();
        let pending = table.propose(host, &scope(), &call).expect("a card");
        table.decide("chat-1", "x1", Decision::Allow, &pending.hash).expect("decided");
        table
    };
    let attempt = |table: &ApprovalTable, host: &WriteHost| {
        let outcome = table.run(host, &scope(), "x1", &CancelToken::new());
        assert_eq!(outcome.expect_err("refused"), ToolError::PayloadChanged);
        assert_eq!(host.run_count(), 0, "the host never ran");
        assert!(table.pending("chat-1").is_empty());
        let again = table.run(host, &scope(), "x1", &CancelToken::new()).expect_err("the call is gone");
        assert_eq!(again, ToolError::UnknownCall);
    };

    // Other text of the same length and the same change time.
    let host = WriteHost::new().with_file(NOTES, "abcdef\n", 100);
    let table = propose_and_allow(&host);
    host.change(NOTES, "abcdeX\n", 100);
    attempt(&table, &host);

    // The file is removed meanwhile.
    let host = WriteHost::new().with_file(NOTES, "abcdef\n", 100);
    let table = propose_and_allow(&host);
    host.remove(NOTES);
    attempt(&table, &host);

    // A file appears where the card was for a new file.
    let host = WriteHost::new();
    let table = propose_and_allow(&host);
    host.change(NOTES, "somebody else\n", 100);
    attempt(&table, &host);

    // Only the change time moves, the text is the same.
    let host = WriteHost::new().with_file(NOTES, "abcdef\n", 100);
    let table = propose_and_allow(&host);
    host.change(NOTES, "abcdef\n", 101);
    attempt(&table, &host);

    // An edit whose text vanished from the file is a changed payload too, not an argument error.
    let host = WriteHost::new().with_file(NOTES, "keep foo keep\n", 100);
    let table = self::table();
    let edit = edit_call("x2", NOTES, "foo", "bar", false);
    let pending = table.propose(&host, &scope(), &edit).expect("a card");
    table.decide("chat-1", "x2", Decision::Allow, &pending.hash).expect("decided");
    host.change(NOTES, "keep fo0 keep\n", 100);
    assert_eq!(table.run(&host, &scope(), "x2", &CancelToken::new()).expect_err("refused"), ToolError::PayloadChanged);
    assert_eq!(host.run_count(), 0);
}

#[test]
fn the_host_is_asked_to_inspect_only_after_the_walls_passed() {
    let table = table();
    let host = WriteHost::new();
    for path in ["/home/u/.ssh/config", "/proj/.env", "/home/u/.zshrc", "/proj/.git/hooks/pre-commit"] {
        let error = table.propose(&host, &scope(), &write_call("w", path, "x")).expect_err(path);
        assert!(matches!(error, ToolError::Denied(_)), "{path}: {error:?}");
    }
    assert_eq!(host.inspects(), 0, "a protected place is never looked at");

    // The directories the shell protects: a write below one is refused before the host looks.
    let data = std::env::temp_dir().join(format!("holzwave-change-{}", std::process::id()));
    std::fs::create_dir_all(&data).expect("a folder");
    let protected = data.canonicalize().expect("canonical");
    let guarded = ApprovalTable::new(DenyPolicy::new(vec![protected.clone()]));
    let local = WriteHost::new().on_this_computer();
    let inside = protected.join("settings.json");
    let error = guarded.propose(&local, &scope(), &write_call("w", inside.to_str().expect("utf-8"), "x")).expect_err("protected");
    assert!(matches!(error, ToolError::Denied(_)), "{error:?}");
    assert_eq!(local.inspects(), 0);
    let _ = std::fs::remove_dir_all(&data);

    // A read call never asks the host to inspect; a write that passes the walls asks once when proposed and once when run.
    let host = WriteHost::new().with_file(NOTES, "a\n", 3);
    let read = ToolCall { id: "r".to_owned(), name: "read_file".to_owned(), args: json!({"path": NOTES}) };
    table.propose(&host, &scope(), &read).expect("a read card");
    assert_eq!(host.inspects(), 0);
    let pending = table.propose(&host, &scope(), &write_call("w2", NOTES, "b\n")).expect("a card");
    assert_eq!(host.inspects(), 1);
    table.decide("chat-1", "w2", Decision::Allow, &pending.hash).expect("decided");
    table.run(&host, &scope(), "w2", &CancelToken::new()).expect("ran");
    assert_eq!(host.inspects(), 2);
}

#[test]
fn no_debug_output_of_the_change_types_shows_text() {
    let old = format!("alpha\nbeta {OLD_CANARY}\ngamma\n");
    let new = format!("alpha\nbeta {NEW_CANARY}\ngamma\n");
    let host = WriteHost::new().with_file(NOTES, &old, 4);
    let table = table();
    let pending: Pending = table.propose(&host, &scope(), &write_call("d1", NOTES, &new)).expect("a card");
    let change = Change { before: Some(old.clone()), after: new.clone(), kind: DiffKind::Replace };
    let made = diff(&change).expect("a diff");
    let inspected = Inspected::new(Base::Text(old.clone()), Some(4));
    let resolved = Resolved::remote(3, "build box").with_path(NOTES).with_base(Base::Text(old.clone()));
    let mut shown: Vec<String> = Vec::new();
    for text in [&old, &new] {
        shown.push(format!("{:?} {:#?}", Base::Text(text.clone()), Base::Text(text.clone())));
    }
    shown.push(format!("{:?} {:#?}", Base::Absent, Base::Absent));
    shown.push(format!("{change:?} {change:#?}"));
    shown.push(format!("{made:?} {made:#?}"));
    for row in &made.rows {
        shown.push(format!("{row:?} {row:#?}"));
    }
    for row in [DiffRow::Gap { skipped: 4 }, DiffRow::Note { text: NEW_CANARY.to_owned() }] {
        shown.push(format!("{row:?} {row:#?}"));
    }
    shown.push(format!("{inspected:?} {inspected:#?}"));
    shown.push(format!("{resolved:?} {resolved:#?}"));
    shown.push(format!("{pending:?} {pending:#?}"));
    let all = shown.join("\n");
    assert!(!all.contains(OLD_CANARY) && !all.contains(NEW_CANARY) && !all.contains("alpha") && !all.contains("gamma"), "{all}");
    assert!(all.contains("bytes>") && all.contains("absent"), "a size is shown: {all}");
}
