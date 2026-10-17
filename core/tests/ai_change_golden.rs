//! The golden of the write card: the page keeps its own reader for the diff of a card, its own caps and its own words for the
//! faults of a change, and this test writes the cards the real table builds, the real limits and the real sentences to
//! `app/tests/fixtures/ai_write_card.json`, so that a changed member, row shape, limit or sentence fails here and in the
//! page's tests in the same commit.
//!
//! The document is built from the real table and constants only. No network, no engine. `UPDATE_FIXTURES=1` rewrites the
//! file; without it the file must equal what is built now.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use holzwave_core::ai::provider::CancelToken;
use holzwave_core::ai::tools::{
    ApprovalTable, Base, ChangeFault, DenyPolicy, Inspected, MAX_BASE_BYTES, MAX_DIFF_CHARS, MAX_DIFF_ROWS, MAX_PATH_BYTES,
    MAX_WRITE_BYTES, Pending, Resolved, Scope, Ticket, Tool, ToolError, ToolHost, ToolName, ToolOutput,
};
use holzwave_core::ai::types::ToolCall;
use holzwave_core::layout::PaneId;
use serde::Serialize;
use serde::ser::{Serializer, SerializeMap};
use serde_json::json;

/// A key that never belongs in the file. It appears in this test only: a hit anywhere else is a leak.
const CANARY: &str = "zq-golden-write-3f9a1c";

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures/ai_write_card.json")
}

fn update_fixtures() -> bool {
    std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
}

/// A remote host on pane 3 called `build box`, holding the files it is given.
struct Files {
    files: Mutex<BTreeMap<String, String>>,
}

impl Files {
    fn new(files: &[(&str, String)]) -> Self {
        Self { files: Mutex::new(files.iter().map(|(path, text)| ((*path).to_owned(), text.clone())).collect()) }
    }
}

impl ToolHost for Files {
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

    fn inspect(&self, resolved: &Resolved) -> Result<Option<Inspected>, ToolError> {
        let files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        let path = resolved.path().unwrap_or_default();
        Ok(Some(match files.get(path) {
            Some(text) => Inspected::new(Base::Text(text.clone()), Some(1_700_000_000)),
            None => Inspected::new(Base::Absent, None),
        }))
    }

    fn run(&self, _ticket: &Ticket, _cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(String::new()))
    }
}

fn card(files: &[(&str, String)], name: &str, args: serde_json::Value) -> Pending {
    let host = Files::new(files);
    let table = ApprovalTable::new(DenyPolicy::new(Vec::new()));
    let scope = Scope { conversation: "chat-1".to_owned(), turn: 1, pane: 3 };
    let call = ToolCall { id: "call_1".to_owned(), name: name.to_owned(), args };
    table.propose(&host, &scope, &call).unwrap_or_else(|error| panic!("the card for {name} was refused: {error}"))
}

/// The cards of the golden: every kind of diff, and the gap and note rows.
fn cards() -> Cards {
    let notes = "/w/notes.txt";
    let replace = card(
        &[(notes, "alpha\nbeta\ngamma\n".to_owned())],
        "write_file",
        json!({"path": notes, "content": "alpha\nbeta two\ngamma\n"}),
    );
    let create = card(&[], "write_file", json!({"path": "/w/new.txt", "content": "one\ntwo\nthree\n"}));
    let edit = card(
        &[(notes, "alpha\nbeta\ngamma\n".to_owned())],
        "edit_file",
        json!({"path": notes, "old_text": "beta", "new_text": "beta two"}),
    );
    // Forty lines without a final newline whose matches lie at lines 3, 20 and 40, so that the diff holds gaps and a note.
    let forty: Vec<String> =
        (1..=40).map(|n| if [3, 20, 40].contains(&n) { "the item".to_owned() } else { format!("line {n}") }).collect();
    let text = forty.join("\n");
    let edit_all = card(
        &[(notes, text)],
        "edit_file",
        json!({"path": notes, "old_text": "the item", "new_text": "an item", "replace_all": true}),
    );
    Cards { replace, create, edit, edit_all }
}

#[derive(Serialize)]
struct Cards {
    replace: Pending,
    create: Pending,
    edit: Pending,
    edit_all: Pending,
}

/// The longest value of one payload row a legal write can have, and the largest total of the values of a card.
///
/// Measured through the table: a `write_file` of the most bytes the tool allows, made of the characters that need the
/// longest escape per byte (a soft hyphen of two bytes is written in six characters, a zero-width space of three bytes in
/// eight), with a path of the most characters the tool allows.
fn measured_limits() -> Limits {
    let mut longest = 0;
    let mut total = 0;
    for unit in ['\u{00ad}', '\u{200b}', '\u{e0041}'] {
        let content: String = std::iter::repeat_n(unit, MAX_WRITE_BYTES / unit.len_utf8()).collect();
        let path = format!("/w/{}", "a".repeat(MAX_PATH_BYTES - 3));
        let pending = card(&[], "write_file", json!({"path": path, "content": content}));
        let values: Vec<usize> = pending.payload.iter().map(|field| field.value.chars().count()).collect();
        let this_value = pending.payload.iter().find(|f| f.name == "content").map_or(0, |f| f.value.chars().count());
        longest = longest.max(this_value);
        total = total.max(values.iter().sum());
    }
    Limits {
        payload_value_chars: longest,
        payload_total_chars: total,
        diff_rows: MAX_DIFF_ROWS,
        diff_chars: MAX_DIFF_CHARS,
        base_bytes: MAX_BASE_BYTES,
    }
}

#[derive(Serialize)]
struct Limits {
    payload_value_chars: usize,
    payload_total_chars: usize,
    diff_rows: usize,
    diff_chars: usize,
    base_bytes: usize,
}

/// Every fault of a change, with an exhaustive match so that a new one does not compile until the golden knows it.
fn faults() -> Vec<ChangeFault> {
    let all = [
        ChangeFault::NotAWrite,
        ChangeFault::NoFile,
        ChangeFault::NoMatch,
        ChangeFault::Ambiguous,
        ChangeFault::Same,
        ChangeFault::Unchanged,
        ChangeFault::TooLarge,
        ChangeFault::NotShown,
    ];
    for fault in all {
        match fault {
            ChangeFault::NotAWrite
            | ChangeFault::NoFile
            | ChangeFault::NoMatch
            | ChangeFault::Ambiguous
            | ChangeFault::Same
            | ChangeFault::Unchanged
            | ChangeFault::TooLarge
            | ChangeFault::NotShown => {}
        }
    }
    all.to_vec()
}

/// An object from the code of each fault to its phrase, in the order of the declaration.
struct Faults(Vec<(&'static str, &'static str)>);

impl Serialize for Faults {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (code, phrase) in &self.0 {
            map.serialize_entry(code, phrase)?;
        }
        map.end()
    }
}

#[derive(Serialize)]
struct Texts {
    payload_changed: String,
    faults: Faults,
}

#[derive(Serialize)]
struct Document {
    cards: Cards,
    limits: Limits,
    texts: Texts,
}

fn document() -> Document {
    Document {
        cards: cards(),
        limits: measured_limits(),
        texts: Texts {
            payload_changed: ToolError::PayloadChanged.to_string(),
            faults: Faults(faults().into_iter().map(|fault| (fault.code(), fault.phrase())).collect()),
        },
    }
}

fn text_of(document: &Document) -> String {
    let mut text = serde_json::to_string_pretty(document).expect("the document is JSON");
    text.push('\n');
    text
}

#[test]
fn the_write_card_golden_matches_the_cards_the_table_builds() {
    let text = text_of(&document());
    assert_eq!(text, text_of(&document()), "the document is built twice and equal");
    assert!(!text.contains(CANARY) && !text.contains("zq-"), "a canary reached the golden");

    // What the page relies on: the order of the top-level members, the kinds and the limits.
    let value: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(value["cards"]["replace"]["diff"]["kind"], json!("replace"));
    assert_eq!(value["cards"]["create"]["diff"]["kind"], json!("create"));
    assert_eq!(value["cards"]["edit"]["diff"]["kind"], json!("edit"));
    assert_eq!(value["cards"]["edit_all"]["diff"]["kind"], json!("edit"));
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
