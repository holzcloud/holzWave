//! The view model of the integrations: what a tool's output becomes for the page.
//!
//! Three things are held here. The cleaning and the caps (text from a cluster or an account is another party's text, so
//! it is made printable and bounded in the core, before the page sees a byte). The builders and the row actions. And the
//! contract: `docs/integration-view.schema.json` is the published description of a view model, `ViewModel` is the type
//! the core produces, and a parity corpus holds them equal in both directions, with the gaps that no JSON Schema keyword
//! can describe named and pinned.

mod common;
#[path = "common/schema_check.rs"]
mod schema_check;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use holzwave_core::integrate::args::ArgError;
use holzwave_core::integrate::view::{
    clean_cell, clean_text, text_fallback, text_view, KvBuilder, RowBuilder, TableBuilder, TextFormat, Tone, ViewModel,
    MAX_ACTIONS, MAX_ACTION_ARGS, MAX_CELL, MAX_COLUMNS, MAX_KEY, MAX_ROWS, MAX_TEXT, MAX_TITLE,
};
use holzwave_core::integrate::{
    ArgKind, ArgSpec, Integration, IntegrationSpec, Output, ParseError, Selection, ToolId, Validated, Verb,
    VerbEffect, VersionProbe,
};
use serde_json::{json, Value};

const SCHEMA_PATH: &str = "docs/integration-view.schema.json";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("core/ has a parent: the workspace root").to_path_buf()
}

/// The schema file, parsed once and linted before it is returned.
fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| schema_check::load_schema(&root(), SCHEMA_PATH))
}

/// What the schema says about the text of a document.
fn schema_verdict(text: &str) -> Result<(), String> {
    schema_check::schema_verdict(schema(), SCHEMA_PATH, text)
}

/// What the type says: the document is read as a model, which is the whole of what `serde` checks.
fn type_verdict(text: &str) -> Result<ViewModel, String> {
    serde_json::from_str::<ViewModel>(text).map_err(|e| e.to_string())
}

/// Both sides must give the same verdict; says whether they accepted.
fn same_verdict(label: &str, text: &str) -> bool {
    let (typed, schema) = (type_verdict(text), schema_verdict(text));
    assert!(
        typed.is_ok() == schema.is_ok(),
        "{label}: the type and the schema differ\n  document: {text}\n  type: {:?}\n  schema: {schema:?}",
        typed.as_ref().map(|_| "accepted")
    );
    typed.is_ok()
}

fn table() -> Value {
    json!({
        "kind": "table",
        "title": "Pods",
        "columns": [{ "id": "name", "label": "Name" }, { "id": "status", "label": "Status" }],
        "rows": [{
            "key": "default/api-0",
            "cells": ["api-0", "Running"],
            "tone": "normal",
            "actions": [{ "verb": "delete_pod", "label": "Delete", "effect": "write", "args": { "pod": "api-0" } }]
        }],
        "truncated": null,
        "note": null,
        "fetched": 1760000000
    })
}

#[test]
fn a_table_gets_the_same_verdict_from_the_schema_and_the_type() {
    let base = table();
    assert!(same_verdict("a table with one row", &base.to_string()), "the base table must be accepted");

    // The same document is read back to an equal value: the type writes exactly what it reads.
    let model = type_verdict(&base.to_string()).expect("the base table is a model");
    let written = serde_json::to_value(&model).expect("a model is JSON");
    assert_eq!(written, base, "the model must serialise back to the document it was read from");
    assert_eq!(schema_verdict(&written.to_string()), Ok(()), "the written document must pass the schema");
    assert_eq!(type_verdict(&written.to_string()), Ok(model));

    let mut extra = base.clone();
    extra["extra"] = json!(1);
    let mut no_kind = base.clone();
    no_kind.as_object_mut().expect("object").remove("kind");
    let mut chart = base.clone();
    chart["kind"] = json!("chart");
    let mut no_cells = base.clone();
    no_cells["rows"][0].as_object_mut().expect("object").remove("cells");

    let refused = [
        ("an unknown member at the root", extra),
        ("a missing kind", no_kind),
        ("a kind of chart", chart),
        ("a row without cells", no_cells),
    ];
    for (label, document) in refused {
        let text = document.to_string();
        assert!(!same_verdict(label, &text), "{label} must be refused by both sides: {text}");
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Characters, spelt by number so that no special character is typed into this file.
// ---------------------------------------------------------------------------------------------------------------------

fn ch(code_point: u32) -> char {
    char::from_u32(code_point).expect("a Unicode scalar value")
}

fn rep(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

const ESC: char = '\u{1b}';

/// The characters a cell must never hold, written down here and not taken from the code under test: controls, direction
/// controls, zero-width and other format characters, the tag block and the variation selectors.
fn is_never_shown(c: char) -> bool {
    let code = c as u32;
    c.is_control()
        || [
            (0x00AD, 0x00AD),
            (0x034F, 0x034F),
            (0x061C, 0x061C),
            (0x115F, 0x1160),
            (0x17B4, 0x17B5),
            (0x180B, 0x180E),
            (0x200B, 0x200F),
            (0x2028, 0x202E),
            (0x2060, 0x206F),
            (0x3164, 0x3164),
            (0xFE00, 0xFE0E),
            (0xFEFF, 0xFEFF),
            (0xFFA0, 0xFFA0),
            (0xFFF9, 0xFFFB),
            (0xE0000, 0xE007F),
            (0xE0100, 0xE01EF),
        ]
        .iter()
        .any(|&(first, last)| (first..=last).contains(&code))
}

/// A cleaned one-line text is printable, in single plain spaces, trimmed and within the cap.
fn assert_clean_line(label: &str, cleaned: &str) {
    assert!(cleaned.len() <= MAX_CELL, "{label}: {} bytes is over the cap", cleaned.len());
    for c in cleaned.chars() {
        assert!(!is_never_shown(c), "{label}: the cleaned text holds U+{:04X}", c as u32);
        assert!(c == ' ' || !c.is_whitespace(), "{label}: the cleaned text holds the white space U+{:04X}", c as u32);
    }
    assert!(!cleaned.contains("  "), "{label}: a run of spaces survived");
    assert!(cleaned == cleaned.trim(), "{label}: the ends are not trimmed");
}

// ---------------------------------------------------------------------------------------------------------------------
// Cleaning.
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn cell_text_is_cleaned_and_bounded() {
    // An escape sequence vanishes with its payload and the visible text stays.
    assert_eq!(clean_cell(&format!("a{ESC}[31mred{ESC}[0m b")), "ared b");
    // A line feed, a tab and a carriage return are one space each, and runs collapse.
    assert_eq!(clean_cell("a\nb\tc\rd"), "a b c d");
    assert_eq!(clean_cell("a\r\nb"), "a b");
    assert_eq!(clean_cell("a \n\t \r b"), "a b");
    assert_eq!(clean_cell(&format!("a{}b{}c", ch(0xa0), ch(0x3000))), "a b c");
    assert_eq!(clean_cell("  padded  "), "padded");
    // Direction controls, zero-width characters, a tag character and a NUL are removed, not spelt out.
    for code in [0x202e, 0x2066, 0x200b, 0x200d, 0xfeff, 0xe0041, 0x00ad, 0x0000, 0x007f, 0x0085, 0x2028] {
        let cleaned = clean_cell(&format!("pod{}name", ch(code)));
        let expected = if matches!(code, 0x0085 | 0x2028) { "pod name" } else { "podname" };
        assert_eq!(cleaned, expected, "U+{code:04X}");
    }
    // An operating system command with a bell and with a string terminator, and a C1 sequence.
    assert_eq!(clean_cell(&format!("a{ESC}]0;title{}b", ch(7))), "ab");
    assert_eq!(clean_cell(&format!("a{ESC}]0;title{ESC}\\b")), "ab");
    assert_eq!(clean_cell(&format!("a{}2Jb", ch(0x9b))), "ab");
    // An empty result stays empty.
    assert_eq!(clean_cell(""), "");
    assert_eq!(clean_cell(&format!("{ESC}[2J{}", ch(0x200b))), "");
    assert_eq!(clean_cell(" \n\t "), "");
    // Printable text is kept as it is, including non-ASCII.
    let kept = format!("m{}nchen {} {}", ch(0xfc), ch(0x4e2d), ch(0x1f600));
    assert_eq!(clean_cell(&kept), kept);

    // A long cell is cut at 4096 bytes on a character boundary and ends with an ellipsis.
    let long = clean_cell(&"x".repeat(5000));
    assert_eq!(long.len(), MAX_CELL);
    assert!(long.ends_with(ch(0x2026)), "a cut cell ends with an ellipsis");
    assert!(long.starts_with(&"x".repeat(MAX_CELL - 3)));
    for width in [2, 3, 4] {
        let wide = clean_cell(&rep(ch(match width { 2 => 0xe9, 3 => 0x4e2d, _ => 0x1f600 }), 5000));
        assert!(wide.len() <= MAX_CELL, "{width}-byte characters: {} bytes", wide.len());
        assert!(wide.ends_with(ch(0x2026)));
        assert!(wide.chars().count() > 1000, "the cut keeps most of the cell");
    }
    // A text at the cap is not cut.
    let exact = "y".repeat(MAX_CELL);
    assert_eq!(clean_cell(&exact), exact);
    assert_eq!(clean_cell(&"y".repeat(MAX_CELL + 1)).len(), MAX_CELL);

    // A text block keeps its lines and tabs but loses everything else.
    let block = clean_text(&format!("one\r\ntwo\tx{ESC}[31m\nthree{}{}", ch(0x202e), ch(0)));
    assert_eq!(block, "one\ntwo\tx\nthree");
    assert_eq!(clean_text("a\rb"), "a\nb");
    assert!(clean_text(&"z".repeat(MAX_TEXT + 10)).len() <= MAX_TEXT);
}

#[test]
fn a_hostile_cell_corpus_gives_only_printable_bounded_text() {
    let mut inputs: Vec<(String, String)> = Vec::new();
    let mut add = |name: &str, text: String| inputs.push((name.to_owned(), text));

    add("a colour change", format!("{ESC}[31mred{ESC}[0m"));
    add("a clear screen", format!("before{ESC}[2J{ESC}[Hafter"));
    add("a title with a bell", format!("{ESC}]0;title{}text", ch(7)));
    add("a title with a string terminator", format!("{ESC}]0;title{ESC}\\text"));
    add("a hyperlink", format!("{ESC}]8;;https://example.org{ESC}\\link{ESC}]8;;{ESC}\\"));
    add("a device control string", format!("{ESC}Pq#0;2;0;0;0{ESC}\\text"));
    add("an unterminated title", format!("{ESC}]0;never ends"));
    add("a C1 control sequence", format!("a{}31mb", ch(0x9b)));
    add("a C1 operating system command", format!("a{}0;titleb{}c", ch(0x9d), ch(7)));
    add("a right-to-left override", format!("gpj.{}exe.txt", ch(0x202e)));
    add("a left-to-right isolate", format!("{}bidi{}", ch(0x2066), ch(0x2069)));
    add("a zero-width space", format!("zero{}width", ch(0x200b)));
    add("a zero-width joiner and a word joiner", format!("a{}b{}c", ch(0x200d), ch(0x2060)));
    add("a byte order mark", format!("{}text", ch(0xfeff)));
    add("a tag character spelling text", (0..5).map(|i| ch(0xe0041 + i)).collect::<String>());
    add("an embedded NUL", format!("a{}b", ch(0)));
    add("a delete and a backspace", format!("a{}b{}c", ch(0x7f), ch(8)));
    add("a line separator", format!("a{}b{}c", ch(0x2028), ch(0x2029)));
    add("a cell of 5000 characters", "x".repeat(5000));
    add("a cell of 5000 four-byte characters", rep(ch(0x1f600), 5000));
    add("5000 escape sequences", format!("{ESC}[31m").repeat(5000));
    add("5000 white-space characters", " \n\t".repeat(5000));
    add("only invisible characters", format!("{}{}{}", ch(0x200b), ch(0x202e), ch(0xe0041)));
    add("a lone escape at the end", format!("text{ESC}"));
    add("an escape before a multi-byte character", format!("{ESC}{}text", ch(0x4e2d)));

    // The shapes of the hostile pod fixture: its names, labels and annotation values go through the cleaner too.
    let fixture = std::fs::read_to_string(root().join("core/tests/fixtures/integrations/kubectl/pods_hostile.json"))
        .expect("the hostile pod fixture");
    let pods: Value = serde_json::from_str(&fixture).expect("the fixture is JSON");
    let items = pods["items"].as_array().expect("items");
    let mut from_fixture = 0;
    for item in items {
        let meta = &item["metadata"];
        let mut texts: Vec<&str> = vec![meta["name"].as_str().expect("a name")];
        for group in ["labels", "annotations"] {
            if let Some(map) = meta[group].as_object() {
                texts.extend(map.values().filter_map(Value::as_str));
            }
        }
        for text in texts {
            from_fixture += 1;
            add(&format!("fixture text {from_fixture}"), text.to_owned());
        }
    }
    assert!(from_fixture >= 5, "the fixture gave {from_fixture} texts");
    assert!(
        inputs.iter().any(|(_, t)| t.len() >= 5000 && t.starts_with('x')),
        "the fixture's 5000-character annotation is part of the corpus"
    );

    for (name, text) in &inputs {
        let cell = clean_cell(text);
        assert_clean_line(name, &cell);
        // Cleaning twice changes nothing.
        assert_eq!(clean_cell(&cell), cell, "{name}: the cleaner is not idempotent");
        // A model that holds the text holds only clean text, whatever door the text came in by.
        let mut table = TableBuilder::new(text);
        table.column(text, text);
        table.push(RowBuilder::new(text, &[text.as_str()]));
        let model = table.finish();
        let written = serde_json::to_string(&model).expect("a model is JSON");
        assert!(!written.contains("\\u001b"), "{name}: an escape reached the document");
        let ViewModel::Table(view) = &model else { panic!("a table") };
        assert_clean_line(name, view.title());
        assert!(view.title().len() <= MAX_TITLE, "{name}: the title is over its cap");
        assert_clean_line(name, view.columns()[0].label());
        assert_clean_line(name, view.rows()[0].key());
        assert!(view.rows()[0].key().len() <= MAX_KEY, "{name}: the key is over its cap");
        for cell in view.rows()[0].cells() {
            assert_clean_line(name, cell);
        }
        assert_eq!(model.check(), Ok(()), "{name}");
        let kv = {
            let mut kv = KvBuilder::new(text);
            kv.row(text, text, Tone::Normal);
            kv.finish()
        };
        let ViewModel::Kv(view) = &kv else { panic!("a kv") };
        assert_clean_line(name, view.rows()[0].label());
        assert_clean_line(name, view.rows()[0].value());
        // A text block keeps its lines and is otherwise as clean.
        let ViewModel::Text(view) = text_view(text, text, TextFormat::Plain) else { panic!("a text") };
        assert!(view.text().len() <= MAX_TEXT, "{name}");
        assert!(view.text().chars().all(|c| matches!(c, '\n' | '\t') || !is_never_shown(c)), "{name}: a text block holds a hidden character");
    }
    eprintln!("hostile cell corpus: {} texts", inputs.len());

    // Every Unicode scalar value, alone and between two letters: nothing survives that the cell must never hold.
    let mut removed = 0_u32;
    for code in 0..=0x10_ffff_u32 {
        let Some(c) = char::from_u32(code) else { continue };
        let cleaned = clean_cell(&format!("a{c}b"));
        assert!(cleaned.chars().all(|x| !is_never_shown(x)), "U+{code:04X} survived as {cleaned:?}");
        assert!(cleaned.chars().all(|x| x == ' ' || !x.is_whitespace()), "U+{code:04X} left white space");
        // An escape and the two C1 introducers start a sequence that takes the next character with it.
        let starts_sequence = matches!(code, 0x1b | 0x9b | 0x9d);
        assert!(cleaned.starts_with('a') && (starts_sequence || cleaned.ends_with('b')), "U+{code:04X} damaged the letters: {cleaned:?}");
        if is_never_shown(c) {
            removed += 1;
        } else {
            assert!(cleaned == "ab" || cleaned == format!("a{c}b") || cleaned == "a b", "U+{code:04X}: {cleaned:?}");
        }
    }
    assert!(removed >= 490,"the sweep removed {removed} scalar values");
}

// ---------------------------------------------------------------------------------------------------------------------
// Builders, caps and row actions.
// ---------------------------------------------------------------------------------------------------------------------

static POD_ARGS: [ArgSpec; 1] = [ArgSpec { name: "pod", kind: ArgKind::Pod, required: true }];
static TWO_ARGS: [ArgSpec; 2] =
    [ArgSpec { name: "pod", kind: ArgKind::Pod, required: true }, ArgSpec { name: "container", kind: ArgKind::Container, required: false }];

const fn verb(id: &'static str, label: &'static str, effect: VerbEffect, args: &'static [ArgSpec]) -> Verb {
    Verb { id, label, effect, output: Output::Done, args, refresh_secs: None, exit_is_state: false }
}

static DELETE_POD: Verb = verb("delete_pod", "Delete", VerbEffect::Write, &POD_ARGS);
static SHOW_POD: Verb = verb("show_pod", "Describe", VerbEffect::Read, &POD_ARGS);
static SHELL: Verb = verb("shell", "Shell", VerbEffect::Exec, &TWO_ARGS);
static USE_POD: Verb = verb("use_pod", "Use", VerbEffect::Local, &POD_ARGS);
static NO_ARGS: Verb = verb("refresh", "Refresh", VerbEffect::Read, &[]);

fn table_of(columns: usize, rows: usize) -> ViewModel {
    let mut table = TableBuilder::new("Pods");
    for n in 0..columns {
        table.column(&format!("c{n}"), &format!("Column {n}"));
    }
    for n in 0..rows {
        let cells: Vec<String> = (0..columns).map(|c| format!("r{n}c{c}")).collect();
        let refs: Vec<&str> = cells.iter().map(String::as_str).collect();
        table.push(RowBuilder::new(&format!("row-{n}"), &refs));
    }
    table.finish()
}

#[test]
fn the_builders_enforce_every_cap() {
    // 70 columns keep 64.
    let ViewModel::Table(wide) = table_of(70, 2) else { panic!("a table") };
    assert_eq!(wide.columns().len(), MAX_COLUMNS);
    assert!(wide.rows().iter().all(|row| row.cells().len() == MAX_COLUMNS));
    assert!(wide.note().is_some_and(|n| n.contains("64 of 70")), "the dropped columns are named: {:?}", wide.note());

    // 2500 rows keep 2000 and say so.
    let model = table_of(2, 2500);
    assert_eq!(model.check(), Ok(()));
    let ViewModel::Table(tall) = &model else { panic!("a table") };
    assert_eq!(tall.rows().len(), MAX_ROWS);
    let truncated = tall.truncated().expect("dropped rows are named");
    assert_eq!(truncated.shown(), 2000);
    assert!(truncated.note().contains("2000") && truncated.note().contains("2500"), "{}", truncated.note());
    assert_eq!(tall.rows()[1999].key(), "row-1999", "the first rows are the ones kept");
    // A table at the cap is not truncated.
    let ViewModel::Table(exact) = table_of(1, MAX_ROWS) else { panic!("a table") };
    assert!(exact.truncated().is_none() && exact.rows().len() == MAX_ROWS);

    // A row with 12 actions keeps 8.
    let mut row = RowBuilder::new("k", &["a"]);
    let results: Vec<bool> = (0..12).map(|_| row.action(&DELETE_POD, &[("pod", "api-0")])).collect();
    assert_eq!(results.iter().filter(|ok| **ok).count(), MAX_ACTIONS);
    assert!(results[..8].iter().all(|ok| *ok) && results[8..].iter().all(|ok| !*ok));
    let mut table = TableBuilder::new("t");
    table.column("a", "A");
    table.push(row);
    let ViewModel::Table(view) = table.finish() else { panic!("a table") };
    assert_eq!(view.rows()[0].actions().len(), 8);

    // A key of 300 bytes is cut to 253, a title of 200 characters to 128, also with multi-byte characters.
    let mut table = TableBuilder::new(&"t".repeat(200));
    table.column("a", "A");
    table.push(RowBuilder::new(&"k".repeat(300), &["x"]));
    table.push(RowBuilder::new(&rep(ch(0x1f600), 100), &["x"]));
    let ViewModel::Table(view) = table.finish() else { panic!("a table") };
    assert_eq!(view.title().len(), MAX_TITLE);
    assert_eq!(view.rows()[0].key().len(), MAX_KEY);
    assert!(view.rows()[1].key().len() <= MAX_KEY && view.rows()[1].key().len() >= MAX_KEY - 4);
    let mut label = TableBuilder::new("t");
    label.column(&"i".repeat(300), &"l".repeat(300));
    let ViewModel::Table(view) = label.finish() else { panic!("a table") };
    assert_eq!((view.columns()[0].id().len(), view.columns()[0].label().len()), (128, 128));

    // Rows have exactly as many cells as the table has columns, and the keys are unique.
    let mut table = TableBuilder::new("t");
    table.column("a", "A").column("b", "B");
    table.push(RowBuilder::new("same", &["only one"]));
    table.push(RowBuilder::new("same", &["one", "two", "three"]));
    table.push(RowBuilder::new("same", &[]));
    table.push(RowBuilder::new(&"k".repeat(MAX_KEY), &["1", "2"]));
    table.push(RowBuilder::new(&"k".repeat(MAX_KEY), &["1", "2"]));
    let model = table.finish();
    assert_eq!(model.check(), Ok(()));
    let ViewModel::Table(view) = &model else { panic!("a table") };
    let keys: Vec<&str> = view.rows().iter().map(|row| row.key()).collect();
    assert_eq!(&keys[..3], ["same", "same#2", "same#3"]);
    assert!(keys.iter().all(|key| key.len() <= MAX_KEY));
    assert_eq!(keys.iter().collect::<std::collections::HashSet<_>>().len(), keys.len());
    let cells: Vec<Vec<&str>> = view.rows().iter().map(|row| row.cells().collect()).collect();
    assert_eq!(cells[0], ["only one", ""]);
    assert_eq!(cells[1], ["one", "two"]);
    assert_eq!(cells[2], ["", ""]);

    // A key-value list is capped the same way and says so.
    let mut kv = KvBuilder::new("Context");
    for n in 0..2500 {
        kv.row(&format!("k{n}"), "v", Tone::Dim);
    }
    let ViewModel::Kv(view) = kv.finish() else { panic!("a kv") };
    assert_eq!(view.rows().len(), MAX_ROWS);
    assert!(view.note().is_some_and(|n| n.contains("2000 of 2500")), "{:?}", view.note());
    let mut kv = KvBuilder::new("Context");
    let results: Vec<bool> = (0..10).map(|_| kv.action(&NO_ARGS, &[])).collect();
    assert_eq!(results.iter().filter(|ok| **ok).count(), MAX_ACTIONS);

    // The time is set by `with_fetched` and the note by `with_note`, which cleans it.
    let model = table_of(1, 1).with_fetched(1_760_000_000).with_note(&format!("a{ESC}[31m  note"));
    let ViewModel::Table(view) = &model else { panic!("a table") };
    assert_eq!(view.fetched(), Some(1_760_000_000));
    assert_eq!(view.note(), Some("a note"));
    assert_eq!(model.title(), "Pods");
}

#[test]
fn a_row_action_needs_valid_arguments_and_takes_the_verbs_effect() {
    let mut row = RowBuilder::new("default/api-0", &["api-0"]);
    assert!(row.action(&DELETE_POD, &[("pod", "api-0")]));
    let name_253 = format!("{0}.{0}.{0}.{1}", "a".repeat(63), "a".repeat(61));
    assert_eq!(name_253.len(), MAX_KEY);
    assert!(row.action(&DELETE_POD, &[("pod", name_253.as_str())]), "a name at 253 bytes is a valid pod name");
    let mut table = TableBuilder::new("Pods");
    table.column("name", "Name");
    table.push(row);
    let ViewModel::Table(view) = table.finish() else { panic!("a table") };
    let action = &view.rows()[0].actions()[0];
    assert_eq!((action.verb(), action.label(), action.effect()), ("delete_pod", "Delete", VerbEffect::Write));
    assert_eq!(action.args().collect::<Vec<_>>(), [("pod", "api-0")]);

    // A name that fails validation gives no button, and the row is still there.
    let hidden = format!("api{}0", ch(0x200b));
    let bad_pod: Vec<(&str, String)> = vec![
        ("a hidden character", hidden.clone()),
        ("a right-to-left override", format!("api{}0", ch(0x202e))),
        ("a leading dash", "--all".to_owned()),
        ("an upper-case letter", "Api-0".to_owned()),
        ("a space", "api 0".to_owned()),
        ("a slash", "a/b".to_owned()),
        ("a line feed", "api\n0".to_owned()),
        ("empty", String::new()),
        ("254 bytes", format!("{0}.{0}.{0}.{1}", "a".repeat(63), "a".repeat(62))),
        ("a name that is too long to cut", "a".repeat(300)),
    ];
    for (label, name) in &bad_pod {
        let mut row = RowBuilder::new("k", &[name]);
        assert!(!row.action(&DELETE_POD, &[("pod", name)]), "{label}: the action must be refused");
        let mut table = TableBuilder::new("Pods");
        table.column("name", "Name");
        table.push(row);
        let ViewModel::Table(view) = table.finish() else { panic!("a table") };
        assert_eq!(view.rows().len(), 1, "{label}: the row is still shown");
        assert!(view.rows()[0].actions().is_empty(), "{label}: the row has no action buttons");
    }

    // An argument the verb does not declare, a missing one, a repeated one and too many are all refused.
    let mut row = RowBuilder::new("k", &["x"]);
    assert!(!row.action(&DELETE_POD, &[("namespace", "default")]), "an undeclared argument name");
    assert!(!row.action(&DELETE_POD, &[("pod", "api-0"), ("namespace", "default")]), "an extra argument");
    assert!(!row.action(&DELETE_POD, &[]), "a missing required argument");
    assert!(!row.action(&DELETE_POD, &[("pod", "api-0"), ("pod", "api-1")]), "a repeated argument");
    assert!(!row.action(&DELETE_POD, &[("--pod", "api-0")]), "an option-shaped name");
    assert!(!row.action(&SHELL, &[("pod", "api-0"), ("container", "-x")]), "a bad optional argument");
    let nine: Vec<(String, String)> = (0..=MAX_ACTION_ARGS).map(|n| (format!("a{n}"), "x".to_owned())).collect();
    let nine: Vec<(&str, &str)> = nine.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    assert!(!row.action(&SHELL, &nine), "more than eight arguments");
    assert!(row.action(&SHELL, &[("pod", "api-0")]), "an optional argument may be left out");
    assert!(row.action(&SHELL, &[("container", "app"), ("pod", "api-0")]), "the order of the arguments does not matter");
    assert!(row.action(&NO_ARGS, &[]), "a verb without arguments takes none");
    assert!(!row.action(&NO_ARGS, &[("pod", "api-0")]), "a verb without arguments refuses one");

    // The effect is always the verb's: the caller has no way to give one.
    let mut row = RowBuilder::new("k", &["x"]);
    for verb in [&SHOW_POD, &USE_POD, &DELETE_POD, &SHELL] {
        assert!(row.action(verb, &[("pod", "api-0")]));
    }
    let mut table = TableBuilder::new("Pods");
    table.column("name", "Name");
    table.push(row);
    let model = table.finish();
    let ViewModel::Table(view) = &model else { panic!("a table") };
    let effects: Vec<VerbEffect> = view.rows()[0].actions().iter().map(|a| a.effect()).collect();
    assert_eq!(effects, [VerbEffect::Read, VerbEffect::Local, VerbEffect::Write, VerbEffect::Exec]);
    let written = serde_json::to_value(&model).expect("JSON");
    let wire: Vec<&str> =
        written["rows"][0]["actions"].as_array().expect("actions").iter().filter_map(|a| a["effect"].as_str()).collect();
    assert_eq!(wire, ["read", "local", "write", "exec"]);
    assert_eq!(written["rows"][0]["actions"][2]["args"], json!({ "pod": "api-0" }));
}

#[test]
fn output_that_cannot_be_read_becomes_a_plain_text_model() {
    let model = text_fallback("Pods", b"garbage \x1b[31m\xff");
    let ViewModel::Text(view) = &model else { panic!("a text model") };
    assert_eq!(view.title(), "Pods");
    assert_eq!(view.format(), TextFormat::Plain);
    assert_eq!(view.text(), "garbage \u{fffd}", "an escape sequence is removed and invalid UTF-8 is replaced");
    assert!(!view.truncated());
    assert!(view.note().is_some_and(|n| n.contains("not in the expected shape")), "{:?}", view.note());
    assert_eq!(schema_verdict(&serde_json::to_string(&model).expect("JSON")), Ok(()));

    // Lines stay: a tool's output is read as lines.
    let ViewModel::Text(view) = text_fallback("Pods", b"one\r\ntwo\nthree\x00") else { panic!("a text model") };
    assert_eq!(view.text(), "one\ntwo\nthree");

    // A 1 MiB output is cut to 64 KiB and says so.
    let big = vec![b'x'; 1024 * 1024];
    let ViewModel::Text(view) = text_fallback("Pods", &big) else { panic!("a text model") };
    assert!(view.text().len() <= MAX_TEXT && view.text().len() > MAX_TEXT - 8, "{} bytes", view.text().len());
    assert!(view.truncated());
    assert!(view.note().is_some_and(|n| n.contains("64 KiB")));
    // Output of exactly the cap is not truncated; one byte more is.
    let ViewModel::Text(view) = text_fallback("Pods", &vec![b'x'; MAX_TEXT]) else { panic!("a text model") };
    assert!(!view.truncated() && view.text().len() == MAX_TEXT);
    let ViewModel::Text(view) = text_fallback("Pods", &vec![b'x'; MAX_TEXT + 1]) else { panic!("a text model") };
    assert!(view.truncated());
    // Nothing at all is an empty text, not an error.
    let ViewModel::Text(view) = text_fallback("Pods", b"") else { panic!("a text model") };
    assert_eq!(view.text(), "");
    // A hostile output is clean.
    let hostile = format!("a{}b{ESC}]0;t{}c{}", ch(0x202e), ch(7), ch(0xe0041)).into_bytes();
    let ViewModel::Text(view) = text_fallback("Pods", &hostile) else { panic!("a text model") };
    assert_eq!(view.text(), "abc");
    // A JSON text keeps its format.
    let ViewModel::Text(view) = text_view("Config", "{\n  \"a\": 1\n}", TextFormat::Json) else { panic!("a text model") };
    assert_eq!((view.format(), view.text()), (TextFormat::Json, "{\n  \"a\": 1\n}"));
}

// ---------------------------------------------------------------------------------------------------------------------
// The trait and its error.
// ---------------------------------------------------------------------------------------------------------------------

static FAKE_SPEC: IntegrationSpec = IntegrationSpec {
    id: ToolId::Kube,
    label: "Fake",
    binaries: &["fake"],
    windows_binaries: &["fake.exe"],
    fixed_dirs: &[],
    version_probe: VersionProbe { argv: &["--version"], parse: |_| None },
    env_set: &[],
    selects: &[],
    verbs: &[],
};

struct Fake;

impl Integration for Fake {
    fn spec(&self) -> &'static IntegrationSpec {
        &FAKE_SPEC
    }

    fn argv(&self, _verb: &Verb, _args: &Validated, _sel: &holzwave_core::integrate::CheckedSelection) -> Result<Vec<OsString>, ArgError> {
        Ok(vec![OsString::from("get")])
    }

    fn view(&self, _verb: &Verb, stdout: &[u8]) -> Result<ViewModel, ParseError> {
        if stdout.is_empty() {
            return Err(ParseError::Empty);
        }
        Ok(text_fallback("Fake", stdout))
    }
}

#[test]
fn the_integration_trait_can_be_held_as_an_object_and_a_parse_error_holds_no_input() {
    let fake: Box<dyn Integration> = Box::new(Fake);
    assert_eq!(fake.spec().label, "Fake");
    let selection = Selection::default().check(ToolId::Kube).expect("an empty selection");
    let args = Validated::parse(&NO_ARGS, &Default::default()).expect("no arguments");
    assert_eq!(fake.argv(&NO_ARGS, &args, &selection), Ok(vec![OsString::from("get")]));
    assert!(matches!(fake.view(&NO_ARGS, b"text"), Ok(ViewModel::Text(_))));
    assert_eq!(fake.view(&NO_ARGS, b""), Err(ParseError::Empty));

    let all = [ParseError::Unrecognised, ParseError::TooLarge, ParseError::Empty];
    let sentences: Vec<&str> = all.iter().map(|e| e.as_str()).collect();
    assert_eq!(sentences.iter().collect::<std::collections::HashSet<_>>().len(), 3, "each error has its own sentence");
    for error in all {
        assert_eq!(error.to_string(), error.as_str());
        assert!(!error.as_str().is_empty() && error.as_str().is_ascii());
        // The error is a unit variant: its debug form is its name and nothing else.
        let debug = format!("{error:?}");
        assert!(debug.chars().all(|c| c.is_ascii_alphabetic()), "{debug}");
    }
}
