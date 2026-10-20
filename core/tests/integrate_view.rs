//! The view model of the integrations: what a tool's output becomes for the page.
//!
//! Three things are held here. The cleaning and the caps: text from a cluster or an account is another party's text,
//! so it is made printable and bounded in the core, before the page sees a byte. The builders and the row actions.
//! And the contract: `docs/integration-view.schema.json` is the published description of a view model, `ViewModel` is
//! the type the core produces, and a parity corpus holds them equal in both directions, with the gaps that no JSON
//! Schema keyword can describe named and pinned.

mod common;
#[path = "common/schema_check.rs"]
mod schema_check;

use std::collections::{BTreeSet, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, OnceLock};

use common::Xorshift64;

use holzwave_core::integrate::args::ArgError;
use holzwave_core::integrate::view::{
    clean_cell, clean_text, text_fallback, text_view, KvBuilder, RowBuilder, TableBuilder, TextFormat, Tone, ViewModel,
    MAX_ACTIONS, MAX_ACTION_ARGS, MAX_CELL, MAX_COLUMNS, MAX_KEY, MAX_LABEL, MAX_ROWS, MAX_TEXT, MAX_TITLE,
};
use holzwave_core::integrate::{
    ArgKind, ArgSpec, CheckedSelection, Integration, IntegrationSpec, Output, ParseError, Selection, ToolId, Validated,
    Verb, VerbEffect, VersionProbe,
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

/// What the type says: the document is read as a model, and the two rules that no schema keyword states are checked.
fn type_verdict(text: &str) -> Result<ViewModel, String> {
    let model = serde_json::from_str::<ViewModel>(text).map_err(|e| e.to_string())?;
    model.check().map_err(|e| e.to_string())?;
    Ok(model)
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
        assert!(
            view.text().chars().all(|c| matches!(c, '\n' | '\t') || !is_never_shown(c)),
            "{name}: a text block holds a hidden character",
        );
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
        assert!(
            cleaned.starts_with('a') && (starts_sequence || cleaned.ends_with('b')),
            "U+{code:04X} damaged the letters: {cleaned:?}",
        );
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
    [
        ArgSpec { name: "pod", kind: ArgKind::Pod, required: true },
        ArgSpec { name: "container", kind: ArgKind::Container, required: false },
    ];

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
    assert!(
        row.action(&SHELL, &[("container", "app"), ("pod", "api-0")]),
        "the order of the arguments does not matter",
    );
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
        written["rows"][0]["actions"]
            .as_array()
            .expect("actions")
            .iter()
            .filter_map(|a| a["effect"].as_str())
            .collect();
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
    let json_text = "{\n  \"a\": 1\n}";
    let ViewModel::Text(view) = text_view("Config", json_text, TextFormat::Json) else {
        panic!("a text model")
    };
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

    fn argv(
        &self,
        _verb: &Verb,
        _args: &Validated,
        _sel: &CheckedSelection,
    ) -> Result<Vec<OsString>, ArgError> {
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

// ---------------------------------------------------------------------------------------------------------------------
// The schema file itself.
// ---------------------------------------------------------------------------------------------------------------------

/// Every `$ref` string anywhere in a JSON value.
fn refs_in(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                match (key.as_str(), inner) {
                    ("$ref", Value::String(r)) => out.push(r.clone()),
                    _ => refs_in(inner, out),
                }
            }
        }
        Value::Array(list) => list.iter().for_each(|inner| refs_in(inner, out)),
        _ => {}
    }
}

/// Every subschema that declares `properties`, with its pointer.
fn objects_in<'a>(value: &'a Value, at: &str, out: &mut Vec<(String, &'a Value)>) {
    match value {
        Value::Object(map) => {
            if map.contains_key("properties") && map.get("type") == Some(&json!("object")) {
                out.push((at.to_owned(), value));
            }
            for (key, inner) in map {
                if key != "properties" || !inner.is_object() {
                    objects_in(inner, &format!("{at}/{key}"), out);
                } else {
                    for (name, member) in inner.as_object().expect("object") {
                        objects_in(member, &format!("{at}/properties/{name}"), out);
                    }
                }
            }
        }
        Value::Array(list) => {
            list.iter().enumerate().for_each(|(n, inner)| objects_in(inner, &format!("{at}/{n}"), out));
        }
        _ => {}
    }
}

#[test]
fn the_schema_file_is_draft_2020_12_and_uses_only_keywords_the_checker_evaluates() {
    let schema = schema();
    assert!(
        schema["$schema"].as_str().is_some_and(|s| s.ends_with("2020-12/schema")),
        "the schema must name draft 2020-12"
    );
    assert!(schema.get("$id").is_none(), "an $id would give an editor an address to resolve");
    assert!(schema.get("$defs").is_some_and(Value::is_object));
    let root_keys: Vec<&str> = schema.as_object().expect("object").keys().map(String::as_str).collect();
    assert_eq!(root_keys, ["$defs", "$schema", "description", "oneOf", "title"]);
    let defs = schema["$defs"].as_object().expect("$defs");
    assert_eq!(
        defs.keys().collect::<Vec<_>>(),
        ["action", "column", "effect", "kvRow", "tableRow", "text", "tone", "truncation"]
    );

    // Three variants, each closed and told apart by a constant `kind`.
    let variants = schema["oneOf"].as_array().expect("a root oneOf");
    let kinds: Vec<&Value> = variants.iter().map(|v| &v["properties"]["kind"]["const"]).collect();
    assert_eq!(kinds, [&json!("table"), &json!("kv"), &json!("text")]);
    for variant in variants {
        assert_eq!(variant["additionalProperties"], json!(false));
        assert!(variant["required"].as_array().expect("required").contains(&json!("kind")));
    }

    // Every object is closed, except the argument map, whose members are named by the verb; every member is described.
    let mut objects = Vec::new();
    objects_in(schema, "#", &mut objects);
    assert_eq!(objects.len(), 8, "the three variants and five definitions are objects with members: {}", objects.len());
    for (at, object) in &objects {
        assert_eq!(object["additionalProperties"], json!(false), "{at} is not closed");
        for (name, member) in object["properties"].as_object().expect("properties") {
            let described = member["description"].as_str().is_some_and(|d| !d.is_empty());
            assert!(described || member.get("$ref").is_some(), "{at}/properties/{name} has no description");
        }
        let members = object["properties"].as_object().expect("properties");
        let declared: BTreeSet<&str> = members.keys().map(String::as_str).collect();
        for name in object["required"].as_array().expect("required") {
            assert!(
                declared.contains(name.as_str().expect("a name")),
                "{at} requires {name}, which it does not declare",
            );
        }
    }
    let args = &schema["$defs"]["action"]["properties"]["args"];
    assert_eq!(args["type"], json!("object"));
    assert!(args.get("properties").is_none() && args["additionalProperties"].is_object());

    // No address, no anchor and no vendor keyword. The shared lint already refuses every keyword the checker does not
    // evaluate (a `format` or a `pattern` among them); a keyword that nobody evaluates would be honoured by an editor
    // only.
    let text = read_schema_text();
    for word in ["\"$id\"", "\"$anchor\"", "\"x-", "\"$comment\"", "http://", "https://example"] {
        assert!(!text.contains(word), "the schema file holds {word}");
    }
    let mut refs = Vec::new();
    refs_in(schema, &mut refs);
    assert!(refs.len() >= 8, "the file uses references: {refs:?}");
    for reference in &refs {
        let name = reference.strip_prefix("#/$defs/").unwrap_or_else(
            || panic!("{reference} is not a local $defs reference"),
        );
        assert!(defs.contains_key(name), "{reference} names no $defs entry");
    }
    for name in defs.keys() {
        assert!(refs.contains(&format!("#/$defs/{name}")), "$defs/{name} is never referenced");
    }
}

fn read_schema_text() -> String {
    std::fs::read_to_string(root().join(SCHEMA_PATH)).expect("the schema file")
}

// ---------------------------------------------------------------------------------------------------------------------
// The corpus: valid and invalid documents of every member and kind.
// ---------------------------------------------------------------------------------------------------------------------

struct Case {
    category: &'static str,
    name: String,
    text: String,
}

const CATEGORIES: &[&str] = &[
    "shape", "kind", "unknown", "arrays", "table", "truncation", "kv", "text", "tone", "effect", "actions", "args",
    "cells", "keys", "titles", "notes", "fetched",
];

fn with(mut base: Value, key: &str, value: Value) -> Value {
    base[key] = value;
    base
}

fn without(mut base: Value, key: &str) -> Value {
    base.as_object_mut().expect("an object").remove(key);
    base
}

fn kv() -> Value {
    json!({
        "kind": "kv",
        "title": "Context",
        "rows": [{ "label": "Cluster", "value": "dev", "tone": "dim" }, { "label": "User", "value": "me" }],
        "actions": [{ "verb": "refresh", "label": "Refresh", "effect": "read", "args": {} }],
        "note": null,
        "fetched": 1760000000
    })
}

fn text() -> Value {
    json!({
        "kind": "text",
        "title": "Output",
        "text": "one\ntwo",
        "format": "plain",
        "truncated": false,
        "note": null,
        "fetched": null,
    })
}

fn columns(n: usize) -> Value {
    Value::Array((0..n).map(|i| json!({ "id": format!("c{i}"), "label": format!("Column {i}") })).collect())
}

fn row(n: usize) -> Value {
    json!({ "key": format!("k{n}"), "cells": ["x"] })
}

fn rows(n: usize) -> Value {
    Value::Array((0..n).map(row).collect())
}

/// A table of one column with the given rows.
fn table_of_rows(rows: Value) -> Value {
    json!({ "kind": "table", "title": "T", "columns": columns(1), "rows": rows })
}

/// A table of one column and one row that carries `member` with `value`.
fn row_with(member: &str, value: Value) -> Value {
    table_of_rows(json!([with(row(0), member, value)]))
}

fn action_with(member: &str, value: Value) -> Value {
    let base = json!({ "verb": "delete_pod", "label": "Delete", "effect": "write", "args": { "pod": "api-0" } });
    with(base, member, value)
}

fn row_with_actions(n: usize) -> Value {
    row_with("actions", Value::Array((0..n).map(|_| action_with("verb", json!("delete_pod"))).collect()))
}

fn args_of(n: usize) -> Value {
    Value::Object((0..n).map(|i| (format!("a{i}"), json!("v"))).collect())
}

fn spaced(c: char, n: usize) -> String {
    rep(c, n)
}

#[derive(Default)]
struct Corpus(Vec<Case>);

impl Corpus {
    fn text(&mut self, category: &'static str, name: &str, text: impl Into<String>) {
        self.0.push(Case { category, name: name.to_owned(), text: text.into() });
    }

    fn json(&mut self, category: &'static str, name: &str, document: &Value) {
        self.text(category, name, document.to_string());
    }
}

fn corpus() -> Vec<Case> {
    let mut c = Corpus::default();

    c.text("shape", "not JSON", "not json");
    c.text("shape", "empty text", "");
    c.text("shape", "null", "null");
    c.text("shape", "true", "true");
    c.text("shape", "a number", "1");
    c.text("shape", "a string", "\"table\"");
    c.text("shape", "an empty array", "[]");
    c.text("shape", "an empty object", "{}");
    c.text("shape", "text after the object", format!("{} x", table()));
    c.text("shape", "a byte order mark before the object", format!("{}{}", ch(0xfeff), table()));

    for document in [table(), kv(), text()] {
        let kind = document["kind"].as_str().expect("a kind").to_owned();
        c.json("kind", &format!("a valid {kind}"), &document);
        c.json("kind", &format!("a {kind} without kind"), &without(document.clone(), "kind"));
    }
    for (name, kind) in [
        ("Table", json!("Table")),
        ("chart", json!("chart")),
        ("an empty string", json!("")),
        ("a number", json!(1)),
        ("null", json!(null)),
        ("an array", json!(["table"])),
        ("a padded name", json!(" table")),
    ] {
        c.json("kind", &format!("the kind is {name}"), &with(table(), "kind", kind));
    }
    c.json("kind", "a table with the members of a kv", &with(table(), "kind", json!("kv")));
    c.json("kind", "a text with the members of a table", &with(text(), "kind", json!("table")));

    for (name, document) in [("table", table()), ("kv", kv()), ("text", text())] {
        c.json("unknown", &format!("an extra member in a {name}"), &with(document.clone(), "extra", json!(1)));
        c.json("unknown", &format!("an upper-case name in a {name}"), &with(document, "Title", json!("x")));
    }
    c.json("unknown", "an extra member in a column", &with_in(table(), "/columns/0", "extra", json!(1)));
    c.json("unknown", "an extra member in a row", &with_in(table(), "/rows/0", "extra", json!(1)));
    c.json("unknown", "an extra member in an action", &with_in(table(), "/rows/0/actions/0", "extra", json!(1)));
    c.json("unknown", "an extra member in a kv row", &with_in(kv(), "/rows/0", "extra", json!(1)));
    c.json("unknown", "an extra member in a kv action", &with_in(kv(), "/actions/0", "extra", json!(1)));
    c.json(
        "unknown",
        "an extra member in the truncation",
        &with_in(truncated_table(), "/truncated", "extra", json!(1)),
    );
    c.json("unknown", "columns in a kv", &with(kv(), "columns", json!([])));
    c.json("unknown", "text in a table", &with(table(), "text", json!("x")));

    c.json(
        "arrays",
        "a column as an array",
        &json!({ "kind": "table", "title": "T", "columns": [["name", "Name"]], "rows": [] }),
    );
    c.json("arrays", "a row as an array", &with(table(), "rows", json!([["k", ["x"]]])));
    c.json(
        "arrays",
        "an action as an array",
        &with_in(table(), "/rows/0", "actions", json!([["verb", "Label", "read", {}]])),
    );
    c.json("arrays", "a kv row as an array", &with(kv(), "rows", json!([["Label", "value"]])));
    c.json("arrays", "a truncation as an array", &with(truncated_table(), "truncated", json!([2, "note"])));
    c.text("arrays", "a table as an array of member values", r#"["table","T",[],[]]"#);
    c.text(
        "arrays",
        "a table as an array that starts with the kind",
        r#"["table",{"title":"T","columns":[],"rows":[]}]"#,
    );

    c.json("table", "no rows", &with(table(), "rows", json!([])));
    c.json("table", "no columns and no rows", &with(with(table(), "rows", json!([])), "columns", json!([])));
    c.json(
        "table",
        "a row without cells and no columns",
        &json!({ "kind": "table", "title": "T", "columns": [], "rows": [{ "key": "k", "cells": [] }] }),
    );
    c.json("table", "one row", &table_of_rows(rows(1)));
    c.json("table", "2000 rows", &table_of_rows(rows(2000)));
    c.json("table", "2001 rows", &table_of_rows(rows(2001)));
    c.json("table", "64 columns", &json!({ "kind": "table", "title": "T", "columns": columns(64), "rows": [] }));
    c.json("table", "65 columns", &json!({ "kind": "table", "title": "T", "columns": columns(65), "rows": [] }));
    c.json(
        "table",
        "64 columns with a row of 64 cells",
        &json!({
            "kind": "table",
            "title": "T",
            "columns": columns(64),
            "rows": [{ "key": "k", "cells": vec!["x"; 64] }],
        }),
    );
    c.json(
        "table",
        "65 cells in a row",
        &json!({
            "kind": "table",
            "title": "T",
            "columns": columns(65),
            "rows": [{ "key": "k", "cells": vec!["x"; 65] }],
        }),
    );
    c.json("table", "columns missing", &without(table(), "columns"));
    c.json("table", "rows missing", &without(table(), "rows"));
    c.json("table", "columns null", &with(table(), "columns", json!(null)));
    c.json("table", "rows as an object", &with(table(), "rows", json!({})));
    c.json("table", "a null row", &with(table(), "rows", json!([null])));
    c.json("table", "a row without a key", &with(table(), "rows", json!([{ "cells": ["a", "b"] }])));
    c.json("table", "a row without cells", &with(table(), "rows", json!([{ "key": "k" }])));
    c.json("table", "a column without an id", &with(table(), "columns", json!([{ "label": "Name" }])));
    c.json("table", "a column without a label", &with(table(), "columns", json!([{ "id": "name" }])));
    c.json("table", "a row without tone and actions", &table_of_rows(json!([{ "key": "k", "cells": ["x"] }])));

    c.json("truncation", "a truncation", &truncated_table());
    c.json(
        "truncation",
        "a truncation of 0 rows shown",
        &with(truncated_table(), "truncated", json!({ "shown": 0, "note": "n" })),
    );
    c.json("truncation", "a truncation without a note", &with(truncated_table(), "truncated", json!({ "shown": 2 })));
    c.json("truncation", "a truncation without shown", &with(truncated_table(), "truncated", json!({ "note": "n" })));
    c.json(
        "truncation",
        "a negative shown",
        &with(truncated_table(), "truncated", json!({ "shown": -1, "note": "n" })),
    );
    c.json(
        "truncation",
        "a fractional shown",
        &with(truncated_table(), "truncated", json!({ "shown": 1.5, "note": "n" })),
    );
    c.json("truncation", "a truncation that is false", &with(truncated_table(), "truncated", json!(false)));
    c.json("truncation", "a truncation that is true", &with(truncated_table(), "truncated", json!(true)));
    c.json("truncation", "a truncation that is null", &with(truncated_table(), "truncated", json!(null)));
    c.json(
        "truncation",
        "a truncation note of 129 characters",
        &with(truncated_table(), "truncated", json!({ "shown": 2, "note": "n".repeat(129) })),
    );

    c.json("kv", "a kv", &kv());
    c.json("kv", "a kv without rows", &with(kv(), "rows", json!([])));
    c.json("kv", "a kv without actions", &without(kv(), "actions"));
    c.json("kv", "a kv with null actions", &with(kv(), "actions", json!(null)));
    c.json(
        "kv",
        "2000 entries",
        &with(
            kv(),
            "rows",
            Value::Array((0..2000).map(|i| json!({ "label": format!("l{i}"), "value": "v" })).collect()),
        ),
    );
    c.json(
        "kv",
        "2001 entries",
        &with(
            kv(),
            "rows",
            Value::Array((0..2001).map(|i| json!({ "label": format!("l{i}"), "value": "v" })).collect()),
        ),
    );
    c.json("kv", "8 view actions", &with(kv(), "actions", Value::Array(vec![action_with("verb", json!("a")); 8])));
    c.json("kv", "9 view actions", &with(kv(), "actions", Value::Array(vec![action_with("verb", json!("a")); 9])));
    c.json("kv", "an entry without a value", &with(kv(), "rows", json!([{ "label": "a" }])));
    c.json("kv", "an entry without a label", &with(kv(), "rows", json!([{ "value": "a" }])));
    c.json("kv", "a value that is a number", &with(kv(), "rows", json!([{ "label": "a", "value": 1 }])));
    c.json("kv", "a value that is null", &with(kv(), "rows", json!([{ "label": "a", "value": null }])));
    c.json(
        "kv",
        "a value of 4096 characters",
        &with(kv(), "rows", json!([{ "label": "a", "value": "v".repeat(4096) }])),
    );
    c.json(
        "kv",
        "a value of 4097 characters",
        &with(kv(), "rows", json!([{ "label": "a", "value": "v".repeat(4097) }])),
    );
    c.json("kv", "a label of 128 characters", &with(kv(), "rows", json!([{ "label": "l".repeat(128), "value": "v" }])));
    c.json("kv", "a label of 129 characters", &with(kv(), "rows", json!([{ "label": "l".repeat(129), "value": "v" }])));
    c.json(
        "kv",
        "the same label twice",
        &with(kv(), "rows", json!([{ "label": "a", "value": "1" }, { "label": "a", "value": "2" }])),
    );

    c.json("text", "a plain text", &text());
    c.json("text", "a json text", &with(text(), "format", json!("json")));
    c.json("text", "the smallest text", &json!({ "kind": "text", "title": "T", "text": "" }));
    c.json("text", "a truncated text", &with(text(), "truncated", json!(true)));
    c.json("text", "an unknown format", &with(text(), "format", json!("xml")));
    c.json("text", "a capitalised format", &with(text(), "format", json!("Plain")));
    c.json("text", "a null format", &with(text(), "format", json!(null)));
    c.json("text", "a truncated that is a string", &with(text(), "truncated", json!("yes")));
    c.json("text", "a truncated that is null", &with(text(), "truncated", json!(null)));
    c.json("text", "a truncated that is 1", &with(text(), "truncated", json!(1)));
    c.json("text", "a text without text", &without(text(), "text"));
    c.json("text", "a text that is a number", &with(text(), "text", json!(1)));
    c.json("text", "a text of 65536 characters", &with(text(), "text", json!("t".repeat(65536))));
    c.json("text", "a text of 65537 characters", &with(text(), "text", json!("t".repeat(65537))));
    c.json("text", "a text with a line feed and a tab", &with(text(), "text", json!("a\n\tb")));

    for tone in ["normal", "dim", "warn", "danger"] {
        c.json("tone", &format!("the tone {tone} in a row"), &row_with("tone", json!(tone)));
    }
    let wrong_tones = [
        ("Warn", json!("Warn")),
        ("error", json!("error")),
        ("a number", json!(1)),
        ("null", json!(null)),
        ("an empty string", json!("")),
    ];
    for (name, tone) in wrong_tones {
        c.json("tone", &format!("the tone is {name} in a row"), &row_with("tone", tone.clone()));
        c.json(
            "tone",
            &format!("the tone is {name} in a kv row"),
            &with(kv(), "rows", json!([{ "label": "a", "value": "b", "tone": tone }])),
        );
    }

    for effect in ["read", "local", "write", "exec"] {
        c.json(
            "effect",
            &format!("the effect {effect}"),
            &row_with("actions", json!([action_with("effect", json!(effect))])),
        );
    }
    let wrong_effects = [
        ("Write", json!("Write")),
        ("delete", json!("delete")),
        ("null", json!(null)),
        ("a number", json!(2)),
        ("an empty string", json!("")),
    ];
    for (name, effect) in wrong_effects {
        c.json(
            "effect",
            &format!("the effect is {name}"),
            &row_with("actions", json!([action_with("effect", effect)])),
        );
    }
    c.json(
        "effect",
        "an action without an effect",
        &row_with("actions", json!([without(action_with("verb", json!("a")), "effect")])),
    );

    for n in [0, 1, 8, 9, 12] {
        c.json("actions", &format!("{n} actions on a row"), &row_with_actions(n));
    }
    c.json("actions", "actions null", &row_with("actions", json!(null)));
    c.json("actions", "actions as an object", &row_with("actions", json!({})));
    c.json(
        "actions",
        "an action without a verb",
        &row_with("actions", json!([without(action_with("verb", json!("a")), "verb")])),
    );
    c.json(
        "actions",
        "an action without a label",
        &row_with("actions", json!([without(action_with("verb", json!("a")), "label")])),
    );
    c.json(
        "actions",
        "an action without args",
        &row_with("actions", json!([without(action_with("verb", json!("a")), "args")])),
    );
    c.json(
        "actions",
        "a verb of 128 characters",
        &row_with("actions", json!([action_with("verb", json!("v".repeat(128)))])),
    );
    c.json(
        "actions",
        "a verb of 129 characters",
        &row_with("actions", json!([action_with("verb", json!("v".repeat(129)))])),
    );
    c.json(
        "actions",
        "a label of 128 characters",
        &row_with("actions", json!([action_with("label", json!("l".repeat(128)))])),
    );
    c.json(
        "actions",
        "a label of 129 characters",
        &row_with("actions", json!([action_with("label", json!("l".repeat(129)))])),
    );

    for n in [0, 1, 8, 9] {
        c.json("args", &format!("{n} arguments"), &row_with("actions", json!([action_with("args", args_of(n))])));
    }
    c.json(
        "args",
        "an argument of 253 characters",
        &row_with("actions", json!([action_with("args", json!({ "pod": "p".repeat(253) }))])),
    );
    c.json(
        "args",
        "an argument of 254 characters",
        &row_with("actions", json!([action_with("args", json!({ "pod": "p".repeat(254) }))])),
    );
    c.json(
        "args",
        "an argument name of 128 characters",
        &row_with("actions", json!([action_with("args", json!({ "n".repeat(128): "v" }))])),
    );
    c.json(
        "args",
        "an argument name of 129 characters",
        &row_with("actions", json!([action_with("args", json!({ "n".repeat(129): "v" }))])),
    );
    c.json(
        "args",
        "an argument that is a number",
        &row_with("actions", json!([action_with("args", json!({ "pod": 1 }))])),
    );
    c.json(
        "args",
        "an argument that is null",
        &row_with("actions", json!([action_with("args", json!({ "pod": null }))])),
    );
    c.json("args", "args as an array", &row_with("actions", json!([action_with("args", json!([["pod", "a"]]))])));
    c.json("args", "args as null", &row_with("actions", json!([action_with("args", json!(null))])));
    c.json("args", "an empty argument name", &row_with("actions", json!([action_with("args", json!({ "": "v" }))])));

    for (name, len) in [("1", 1), ("4096", 4096), ("4097", 4097)] {
        c.json("cells", &format!("a cell of {name} characters"), &row_with("cells", json!(["c".repeat(len)])));
    }
    c.json("cells", "an empty cell", &row_with("cells", json!([""])));
    c.json("cells", "a cell that is a number", &row_with("cells", json!([1])));
    c.json("cells", "a cell that is null", &row_with("cells", json!([null])));
    c.json("cells", "cells as a string", &row_with("cells", json!("x")));
    c.json("cells", "cells as an object", &row_with("cells", json!({ "a": "x" })));
    c.json("cells", "a cell with a line feed", &row_with("cells", json!(["a\nb"])));
    c.json("cells", "a cell of 64 characters of four bytes", &row_with("cells", json!([spaced(ch(0x1f600), 64)])));

    for (name, len) in [("1", 1), ("253", 253), ("254", 254)] {
        c.json(
            "keys",
            &format!("a key of {name} characters"),
            &table_of_rows(json!([{ "key": "k".repeat(len), "cells": ["x"] }])),
        );
    }
    c.json("keys", "an empty key", &table_of_rows(json!([{ "key": "", "cells": ["x"] }])));
    c.json("keys", "a key that is a number", &table_of_rows(json!([{ "key": 1, "cells": ["x"] }])));
    c.json("keys", "a key that is null", &table_of_rows(json!([{ "key": null, "cells": ["x"] }])));
    c.json(
        "keys",
        "a key of two bytes per character within the cap in characters and bytes",
        &table_of_rows(json!([{ "key": spaced(ch(0xe9), 126), "cells": ["x"] }])),
    );

    for (name, len) in [("128", 128), ("129", 129)] {
        for kind in [table(), kv(), text()] {
            let title = json!("t".repeat(len));
            let label = kind["kind"].as_str().expect("a kind").to_owned();
            c.json("titles", &format!("a {name}-character title in a {label}"), &with(kind, "title", title));
        }
    }
    c.json("titles", "an empty title", &with(table(), "title", json!("")));
    c.json("titles", "a title that is a number", &with(table(), "title", json!(1)));
    c.json("titles", "a title that is null", &with(table(), "title", json!(null)));
    c.json("titles", "a missing title", &without(table(), "title"));

    for (name, len) in [("128", 128), ("129", 129)] {
        c.json("notes", &format!("a {name}-character note"), &with(table(), "note", json!("n".repeat(len))));
    }
    c.json("notes", "a note on a kv", &with(kv(), "note", json!("a note")));
    c.json("notes", "a note on a text", &with(text(), "note", json!("a note")));
    c.json("notes", "a note that is a number", &with(table(), "note", json!(1)));
    c.json("notes", "a note that is an array", &with(table(), "note", json!(["a"])));

    for (name, value) in [
        ("0", json!(0)),
        ("a time", json!(1760000000)),
        ("the largest u32", json!(4294967295u64)),
        ("one more than the largest u32", json!(4294967296u64)),
        ("-1", json!(-1)),
        ("1.5", json!(1.5)),
        ("1.0", json!(1.0)),
        ("the string 1", json!("1")),
        ("true", json!(true)),
        ("an array", json!([1])),
        ("null", json!(null)),
    ] {
        c.json("fetched", &format!("fetched is {name}"), &with(table(), "fetched", value));
    }
    c.json("fetched", "fetched is absent", &without(table(), "fetched"));
    c.text("fetched", "fetched is 1e1", r#"{"kind":"table","title":"T","columns":[],"rows":[],"fetched":1e1}"#);
    c.text("fetched", "fetched is -0", r#"{"kind":"table","title":"T","columns":[],"rows":[],"fetched":-0}"#);
    c.json("fetched", "fetched on a kv", &with(kv(), "fetched", json!(5)));
    c.json("fetched", "fetched on a text", &with(text(), "fetched", json!(5)));
    c.0
}

/// A member of the object at `pointer` of `base`, set to `value`.
fn with_in(mut base: Value, pointer: &str, member: &str, value: Value) -> Value {
    base.pointer_mut(pointer).unwrap_or_else(|| panic!("no value at {pointer}"))[member] = value;
    base
}

fn truncated_table() -> Value {
    with(table(), "truncated", json!({ "shown": 2, "note": "Showing the first 2 of 5 rows." }))
}

#[test]
fn the_corpus_is_not_thin() {
    let cases = corpus();
    let accepted = cases.iter().filter(|case| type_verdict(&case.text).is_ok()).count();
    let refused = cases.len() - accepted;
    assert!(cases.len() >= 60, "the corpus holds {} documents", cases.len());
    assert!(accepted >= 35, "the type accepts only {accepted} documents of the corpus");
    assert!(refused >= 60, "the type refuses only {refused} documents of the corpus");
    for category in CATEGORIES {
        let count = cases.iter().filter(|case| case.category == *category).count();
        eprintln!("View parity: {category}: {count}");
        assert!(count >= 3, "the category {category} holds {count} documents");
    }
    eprintln!("View parity: {} documents, {accepted} accepted, {refused} refused by the type", cases.len());
    assert!(cases.iter().all(|case| CATEGORIES.contains(&case.category)), "a case names a category that is not listed");
    let names: HashSet<String> = cases.iter().map(|case| format!("{}/{}", case.category, case.name)).collect();
    assert_eq!(names.len(), cases.len(), "two cases of one category share a name");
    // Every kind and every variant of the schema is reached by a document that the type accepts.
    for kind in ["table", "kv", "text"] {
        let reached = cases.iter().any(|case| {
            type_verdict(&case.text).is_ok_and(|model| {
                matches!(
                    (&model, kind),
                    (ViewModel::Table(_), "table") | (ViewModel::Kv(_), "kv") | (ViewModel::Text(_), "text"),
                )
            })
        });
        assert!(reached, "no accepted document of the kind {kind}");
    }
}

#[test]
fn every_view_document_gets_the_same_verdict_from_the_schema_and_the_type() {
    let mut accepted = 0;
    for case in corpus() {
        let label = format!("{}: {}", case.category, case.name);
        if same_verdict(&label, &case.text) {
            accepted += 1;
            // What the type reads, it writes back as a document that the schema accepts and that reads back equal.
            let model = type_verdict(&case.text).expect("accepted");
            let written = serde_json::to_string(&model).expect("a model is JSON");
            assert_eq!(schema_verdict(&written), Ok(()), "{label}: the written document is refused by the schema");
            assert_eq!(type_verdict(&written), Ok(model), "{label}: the written document does not read back equal");
        }
    }
    assert!(accepted >= 35, "{accepted} documents were accepted");
}

// ---------------------------------------------------------------------------------------------------------------------
// The bounds.
// ---------------------------------------------------------------------------------------------------------------------

struct Bound {
    name: &'static str,
    /// Where the schema file states the number.
    pointer: &'static str,
    /// An upper bound is accepted at the number and refused above it; a lower bound the other way round.
    upper: bool,
    /// The documents that put `n` into the member that the number bounds.
    build: fn(i64) -> Vec<Value>,
}

fn chars(n: i64, fill: char) -> String {
    rep(fill, usize::try_from(n).expect("a size"))
}

fn count(n: i64) -> usize {
    usize::try_from(n).expect("a count")
}

fn one_row_table(row: Value, columns_n: usize) -> Value {
    json!({ "kind": "table", "title": "T", "columns": columns(columns_n), "rows": [row] })
}

const BOUNDS: &[Bound] = &[
    Bound {
        name: "the title of a table",
        pointer: "/oneOf/0/properties/title/maxLength",
        upper: true,
        build: |n| vec![with(table(), "title", json!(chars(n, 't')))],
    },
    Bound {
        name: "the columns",
        pointer: "/oneOf/0/properties/columns/maxItems",
        upper: true,
        build: |n| vec![json!({ "kind": "table", "title": "T", "columns": columns(count(n)), "rows": [] })],
    },
    Bound {
        name: "the rows of a table",
        pointer: "/oneOf/0/properties/rows/maxItems",
        upper: true,
        build: |n| vec![table_of_rows(rows(count(n)))],
    },
    Bound {
        name: "the note of a table",
        pointer: "/oneOf/0/properties/note/maxLength",
        upper: true,
        build: |n| vec![with(table(), "note", json!(chars(n, 'n')))],
    },
    Bound {
        name: "fetched of a table, lowest",
        pointer: "/oneOf/0/properties/fetched/minimum",
        upper: false,
        build: |n| vec![with(table(), "fetched", json!(n))],
    },
    Bound {
        name: "fetched of a table, highest",
        pointer: "/oneOf/0/properties/fetched/maximum",
        upper: true,
        build: |n| vec![with(table(), "fetched", json!(n))],
    },
    Bound {
        name: "the title of a kv",
        pointer: "/oneOf/1/properties/title/maxLength",
        upper: true,
        build: |n| vec![with(kv(), "title", json!(chars(n, 't')))],
    },
    Bound {
        name: "the entries of a kv",
        pointer: "/oneOf/1/properties/rows/maxItems",
        upper: true,
        build: |n| vec![
            with(
                kv(),
                "rows",
                Value::Array((0..count(n)).map(|i| json!({ "label": format!("l{i}"), "value": "v" })).collect()),
            ),
        ],
    },
    Bound {
        name: "the actions of a kv",
        pointer: "/oneOf/1/properties/actions/maxItems",
        upper: true,
        build: |n| vec![with(kv(), "actions", Value::Array(vec![action_with("verb", json!("a")); count(n)]))],
    },
    Bound {
        name: "the note of a kv",
        pointer: "/oneOf/1/properties/note/maxLength",
        upper: true,
        build: |n| vec![with(kv(), "note", json!(chars(n, 'n')))],
    },
    Bound {
        name: "fetched of a kv, lowest",
        pointer: "/oneOf/1/properties/fetched/minimum",
        upper: false,
        build: |n| vec![with(kv(), "fetched", json!(n))],
    },
    Bound {
        name: "fetched of a kv, highest",
        pointer: "/oneOf/1/properties/fetched/maximum",
        upper: true,
        build: |n| vec![with(kv(), "fetched", json!(n))],
    },
    Bound {
        name: "the title of a text",
        pointer: "/oneOf/2/properties/title/maxLength",
        upper: true,
        build: |n| vec![with(text(), "title", json!(chars(n, 't')))],
    },
    Bound {
        name: "the text of a text",
        pointer: "/oneOf/2/properties/text/maxLength",
        upper: true,
        build: |n| vec![with(text(), "text", json!(chars(n, 't')))],
    },
    Bound {
        name: "the note of a text",
        pointer: "/oneOf/2/properties/note/maxLength",
        upper: true,
        build: |n| vec![with(text(), "note", json!(chars(n, 'n')))],
    },
    Bound {
        name: "fetched of a text, lowest",
        pointer: "/oneOf/2/properties/fetched/minimum",
        upper: false,
        build: |n| vec![with(text(), "fetched", json!(n))],
    },
    Bound {
        name: "fetched of a text, highest",
        pointer: "/oneOf/2/properties/fetched/maximum",
        upper: true,
        build: |n| vec![with(text(), "fetched", json!(n))],
    },
    Bound {
        name: "a cell and a value",
        pointer: "/$defs/text/maxLength",
        upper: true,
        build: |n| vec![
            row_with("cells", json!([chars(n, 'c')])),
            with(kv(), "rows", json!([{ "label": "a", "value": chars(n, 'v') }])),
        ],
    },
    Bound {
        name: "the verb of an action",
        pointer: "/$defs/action/properties/verb/maxLength",
        upper: true,
        build: |n| vec![row_with("actions", json!([action_with("verb", json!(chars(n, 'v')))]))],
    },
    Bound {
        name: "the label of an action",
        pointer: "/$defs/action/properties/label/maxLength",
        upper: true,
        build: |n| vec![row_with("actions", json!([action_with("label", json!(chars(n, 'l')))]))],
    },
    Bound {
        name: "the arguments of an action",
        pointer: "/$defs/action/properties/args/maxProperties",
        upper: true,
        build: |n| vec![row_with("actions", json!([action_with("args", args_of(count(n)))]))],
    },
    Bound {
        name: "an argument name",
        pointer: "/$defs/action/properties/args/propertyNames/maxLength",
        upper: true,
        build: |n| vec![row_with("actions", json!([action_with("args", json!({ chars(n, 'n'): "v" }))]))],
    },
    Bound {
        name: "an argument value",
        pointer: "/$defs/action/properties/args/additionalProperties/maxLength",
        upper: true,
        build: |n| vec![row_with("actions", json!([action_with("args", json!({ "pod": chars(n, 'p') }))]))],
    },
    Bound {
        name: "the id of a column",
        pointer: "/$defs/column/properties/id/maxLength",
        upper: true,
        build: |n| vec![
            with(with(table(), "rows", json!([])), "columns", json!([{ "id": chars(n, 'i'), "label": "L" }])),
        ],
    },
    Bound {
        name: "the label of a column",
        pointer: "/$defs/column/properties/label/maxLength",
        upper: true,
        build: |n| vec![
            with(with(table(), "rows", json!([])), "columns", json!([{ "id": "i", "label": chars(n, 'l') }])),
        ],
    },
    Bound {
        name: "the key of a row",
        pointer: "/$defs/tableRow/properties/key/maxLength",
        upper: true,
        build: |n| vec![table_of_rows(json!([{ "key": chars(n, 'k'), "cells": ["x"] }]))],
    },
    Bound {
        name: "the cells of a row",
        pointer: "/$defs/tableRow/properties/cells/maxItems",
        upper: true,
        build: |n| vec![one_row_table(json!({ "key": "k", "cells": vec!["x"; count(n)] }), count(n))],
    },
    Bound {
        name: "the actions of a row",
        pointer: "/$defs/tableRow/properties/actions/maxItems",
        upper: true,
        build: |n| vec![row_with_actions(count(n))],
    },
    Bound {
        name: "the label of an entry",
        pointer: "/$defs/kvRow/properties/label/maxLength",
        upper: true,
        build: |n| vec![with(kv(), "rows", json!([{ "label": chars(n, 'l'), "value": "v" }]))],
    },
    Bound {
        name: "shown, lowest",
        pointer: "/$defs/truncation/properties/shown/minimum",
        upper: false,
        build: |n| vec![with(table(), "truncated", json!({ "shown": n, "note": "n" }))],
    },
    Bound {
        name: "shown, highest",
        pointer: "/$defs/truncation/properties/shown/maximum",
        upper: true,
        build: |n| vec![with(table(), "truncated", json!({ "shown": n, "note": "n" }))],
    },
    Bound {
        name: "the note of a truncation",
        pointer: "/$defs/truncation/properties/note/maxLength",
        upper: true,
        build: |n| vec![with(table(), "truncated", json!({ "shown": 1, "note": chars(n, 'n') }))],
    },
];

#[test]
fn every_bound_of_the_schema_is_a_bound_of_the_type() {
    for bound in BOUNDS {
        let n = i64::try_from(schema_check::pointer_number(schema(), SCHEMA_PATH, bound.pointer)).expect(
            "a bound fits an i64",
        );
        let beyond = if bound.upper { n + 1 } else { n - 1 };
        for (inside, outside) in (bound.build)(n).into_iter().zip((bound.build)(beyond)) {
            let label = format!("{} ({})", bound.name, bound.pointer);
            assert!(same_verdict(&format!("{label} at {n}"), &inside.to_string()), "{label}: {n} must be accepted");
            let refused = !same_verdict(&format!("{label} at {beyond}"), &outside.to_string());
            assert!(refused, "{label}: {beyond} must be refused");
        }
    }
    // The rows cover every number that the schema file states, and no other.
    let mut numbers = Vec::new();
    schema_check::limit_pointers(schema(), "", &mut numbers);
    let covered: BTreeSet<&str> = BOUNDS.iter().map(|b| b.pointer).collect();
    assert_eq!(covered.len(), BOUNDS.len(), "two bound rows name the same number");
    for number in &numbers {
        assert!(covered.contains(number.as_str()), "the schema states a number at {number} that no bound row tests");
    }
    for pointer in covered {
        assert!(
            numbers.iter().any(|n| n == pointer),
            "the bound row {pointer} names a number the schema does not state",
        );
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// What the builders produce.
// ---------------------------------------------------------------------------------------------------------------------

/// Texts that a tool could hold, nasty on purpose.
fn nasty(rng: &mut Xorshift64) -> String {
    let pool: Vec<String> = vec![
        String::new(),
        "plain".to_owned(),
        format!("{ESC}[31mred{ESC}[0m"),
        format!("{ESC}]0;title{}x", ch(7)),
        format!("gpj.{}exe", ch(0x202e)),
        format!("zero{}width", ch(0x200b)),
        format!("{}", rep(ch(0xe0041), 4)),
        "line\nbreak\ttab\r".to_owned(),
        format!("a{}b", ch(0)),
        "x".repeat(300),
        "y".repeat(5000),
        rep(ch(0x1f600), 1200),
        rep(ch(0x4e2d), 100),
        "  spaced   out  ".to_owned(),
        "\"quotes\" and \\ backslash".to_owned(),
        "<b>markup</b> [link](http://x) $(command)".to_owned(),
    ];
    let mut text = pool[rng.below(pool.len())].clone();
    for _ in 0..rng.below(4) {
        let starts = [0x20, 0x41, 0xe9, 0x2028, 0x202e, 0xfeff, 0x1f600, 0xe0000, 0x7f, 0x85, 0x9b, 0x1b];
        let code = starts[rng.below(starts.len())] + rng.below(8) as u32;
        if let Some(c) = char::from_u32(code) {
            text.push(c);
        }
    }
    text
}

fn random_model(rng: &mut Xorshift64) -> ViewModel {
    let verbs: [&'static Verb; 5] = [&DELETE_POD, &SHOW_POD, &SHELL, &USE_POD, &NO_ARGS];
    let pods = ["api-0", "Api-0", "--all", "a/b", "web-1", ""];
    let add_actions = |rng: &mut Xorshift64, push: &mut dyn FnMut(&'static Verb, &[(&str, &str)]) -> bool| {
        for _ in 0..rng.below(11) {
            let verb = verbs[rng.below(verbs.len())];
            let pod = pods[rng.below(pods.len())];
            let args: Vec<(&str, &str)> = match rng.below(4) {
                0 => vec![],
                1 => vec![("pod", pod), ("container", "app")],
                _ => vec![("pod", pod)],
            };
            push(verb, &args);
        }
    };
    let tones = [Tone::Normal, Tone::Dim, Tone::Warn, Tone::Danger];
    let model = match rng.below(3) {
        0 => {
            let mut table = TableBuilder::new(&nasty(rng));
            let wide = if rng.chance(10) { 70 } else { rng.below(8) };
            for _ in 0..wide {
                table.column(&nasty(rng), &nasty(rng));
            }
            let tall = if rng.chance(5) { 2100 } else { rng.below(12) };
            for n in 0..tall {
                let cells: Vec<String> = (0..rng.below(wide + 3)).map(|_| nasty(rng)).collect();
                let refs: Vec<&str> = cells.iter().map(String::as_str).collect();
                let key = if rng.chance(30) {
                    "dup".to_owned()
                } else if n % 7 == 0 {
                    nasty(rng)
                } else {
                    format!("k{n}")
                };
                let mut row = RowBuilder::new(&key, &refs);
                row.tone(tones[rng.below(4)]);
                add_actions(rng, &mut |verb, args| row.action(verb, args));
                table.push(row);
            }
            table.finish()
        }
        1 => {
            let mut kv = KvBuilder::new(&nasty(rng));
            let entries = if rng.chance(5) { 2100 } else { rng.below(12) };
            for _ in 0..entries {
                kv.row(&nasty(rng), &nasty(rng), tones[rng.below(4)]);
            }
            add_actions(rng, &mut |verb, args| kv.action(verb, args));
            kv.finish()
        }
        _ => {
            if rng.chance(50) {
                let big = if rng.chance(20) { 100_000 } else { rng.below(300) };
                let alphabet = [b'a', b'\n', 0x1b, 0xff, b'[', 0xe2, 0x80, 0xae];
                let bytes: Vec<u8> = (0..big).map(|_| alphabet[rng.below(alphabet.len())]).collect();
                text_fallback(&nasty(rng), &bytes)
            } else {
                text_view(&nasty(rng), &nasty(rng), if rng.chance(50) { TextFormat::Json } else { TextFormat::Plain })
            }
        }
    };
    let model = if rng.chance(50) {
        model.with_fetched(u32::try_from(rng.below(1 << 30)).expect("fits"))
    } else {
        model
    };
    if rng.chance(30) { model.with_note(&nasty(rng)) } else { model }
}

#[test]
fn builder_output_is_always_accepted_by_the_schema() {
    let mut rng = Xorshift64::new(0x5eed_0007);
    let (mut tables, mut kvs, mut texts, mut cut_tables, mut with_actions) = (0, 0, 0, 0, 0);
    for n in 0..200 {
        let model = random_model(&mut rng);
        match &model {
            ViewModel::Table(view) => {
                tables += 1;
                cut_tables += usize::from(view.truncated().is_some());
                with_actions += usize::from(view.rows().iter().any(|row| !row.actions().is_empty()));
            }
            ViewModel::Kv(_) => kvs += 1,
            ViewModel::Text(_) => texts += 1,
        }
        let written = serde_json::to_string(&model).expect("a model is JSON");
        assert_eq!(schema_verdict(&written), Ok(()), "model {n}: the schema refuses what the builders produced");
        assert_eq!(model.check(), Ok(()), "model {n}");
        assert_eq!(
            type_verdict(&written),
            Ok(model.clone()),
            "model {n}: the type does not read its own output back equal",
        );
        assert!(!written.contains(ESC), "model {n}: an escape reached the document");
    }
    eprintln!(
        "View parity: builder sweep of 200 models: {tables} tables ({cut_tables} cut, {with_actions} with actions), \
         {kvs} kvs, {texts} texts",
    );
    assert!(tables >= 40 && kvs >= 40 && texts >= 40, "{tables} tables, {kvs} kvs, {texts} texts");
    assert!(cut_tables >= 3 && with_actions >= 10, "{cut_tables} cut tables, {with_actions} tables with actions");
}

// ---------------------------------------------------------------------------------------------------------------------
// The gaps.
// ---------------------------------------------------------------------------------------------------------------------

/// A document that the schema accepts and the type refuses, because no keyword of the schema can state its rule.
struct Gap {
    name: &'static str,
    /// The words that the schema description uses for it.
    doc_phrase: &'static str,
    text: String,
    why: &'static str,
}

const BYTES_PHRASE: &str = "every cap above counts bytes in the type and characters in this schema";

static GAPS: LazyLock<Vec<Gap>> = LazyLock::new(|| {
    let wide = |chars: usize| rep(ch(0xe9), chars);
    let bytes = "the type counts the bytes of a text and JSON Schema counts characters";
    vec![
        Gap {
            name: "a row with fewer cells than the table has columns",
            doc_phrase: "the cells of a row equal the columns in number",
            text: json!({
                "kind": "table", "title": "T", "columns": columns(2), "rows": [{ "key": "k", "cells": ["only one"] }]
            })
            .to_string(),
            why: "no keyword compares the length of one array with the length of another",
        },
        Gap {
            name: "a row with more cells than the table has columns",
            doc_phrase: "the cells of a row equal the columns in number",
            text: json!({
                "kind": "table", "title": "T", "columns": columns(1), "rows": [{ "key": "k", "cells": ["a", "b"] }]
            })
            .to_string(),
            why: "no keyword compares the length of one array with the length of another",
        },
        Gap {
            name: "two rows with one key",
            doc_phrase: "row keys are unique",
            text: table_of_rows(json!([row(0), row(0)])).to_string(),
            why: "uniqueItems cannot compare one member of an object, and the checker does not evaluate it",
        },
        Gap {
            name: "a cell of 4096 bytes made of fewer characters",
            doc_phrase: BYTES_PHRASE,
            text: row_with("cells", json!([wide(2049)])).to_string(),
            why: bytes,
        },
        Gap {
            name: "a value of more bytes than the cap within the cap in characters",
            doc_phrase: BYTES_PHRASE,
            text: with(kv(), "rows", json!([{ "label": "a", "value": wide(2049) }])).to_string(),
            why: bytes,
        },
        Gap {
            name: "a text of more than 65536 bytes",
            doc_phrase: BYTES_PHRASE,
            text: with(text(), "text", json!(spaced(ch(0x4e2d), 21846))).to_string(),
            why: bytes,
        },
        Gap {
            name: "a key of more than 253 bytes",
            doc_phrase: BYTES_PHRASE,
            text: table_of_rows(json!([{ "key": wide(127), "cells": ["x"] }])).to_string(),
            why: bytes,
        },
        Gap {
            name: "a title of more than 128 bytes",
            doc_phrase: BYTES_PHRASE,
            text: with(table(), "title", json!(wide(65))).to_string(),
            why: bytes,
        },
        Gap {
            name: "an argument value of more than 253 bytes",
            doc_phrase: BYTES_PHRASE,
            text: row_with("actions", json!([action_with("args", json!({ "pod": wide(127) }))])).to_string(),
            why: bytes,
        },
        Gap {
            name: "an argument name of more than 128 bytes",
            doc_phrase: BYTES_PHRASE,
            text: row_with("actions", json!([action_with("args", json!({ wide(65): "v" }))])).to_string(),
            why: bytes,
        },
        Gap {
            name: "the same member twice",
            doc_phrase: "the same key twice in an object",
            text: r#"{"kind":"text","title":"A","title":"B","text":""}"#.to_owned(),
            why: "serde refuses a repeated member; a parsed document has one value per key and cannot show it",
        },
    ]
});

#[test]
fn each_gap_is_real_and_no_other_document_is_one() {
    let corpus_texts: HashSet<String> = corpus().into_iter().map(|case| case.text).collect();
    for gap in GAPS.iter() {
        assert!(!gap.doc_phrase.is_empty() && !gap.why.is_empty(), "the gap {} has no phrase or no reason", gap.name);
        assert_eq!(schema_verdict(&gap.text), Ok(()), "the schema must accept the gap {}", gap.name);
        assert!(type_verdict(&gap.text).is_err(), "the type must refuse the gap {}", gap.name);
        assert!(!corpus_texts.contains(&gap.text), "the gap {} is also in the corpus", gap.name);
    }
    let names: HashSet<&str> = GAPS.iter().map(|gap| gap.name).collect();
    assert_eq!(names.len(), GAPS.len(), "two gaps share a name");
    let phrases: HashSet<&str> = GAPS.iter().map(|gap| gap.doc_phrase).collect();
    assert_eq!(phrases.len(), 4, "a new kind of gap needs its own sentence in the schema description");
    // The documents at the bound itself in the type's bytes are accepted: the gap is only the multi-byte excess.
    let at_the_bound = [
        row_with("cells", json!([chars(4096, 'c')])),
        table_of_rows(json!([{ "key": chars(253, 'k'), "cells": ["x"] }])),
        with(table(), "title", json!(chars(128, 't'))),
    ];
    for document in at_the_bound {
        assert!(same_verdict("at the bound", &document.to_string()));
    }
    // Which rule refuses it: the read, or the two checks that `serde` does not run.
    let (read, checked): (Vec<&Gap>, Vec<&Gap>) =
        GAPS.iter().partition(|gap| serde_json::from_str::<ViewModel>(&gap.text).is_err());
    assert_eq!(checked.len(), 3, "three gaps are refused by ViewModel::check and not by serde");
    assert_eq!(read.len(), GAPS.len() - 3);
    for gap in checked {
        let model: ViewModel = serde_json::from_str(&gap.text).expect("serde reads it");
        assert!(model.check().is_err(), "{}", gap.name);
    }
    eprintln!("View gaps: {} documents in {} named gaps", GAPS.len(), phrases.len());
}

#[test]
fn the_schema_description_states_the_limits_and_the_gaps_with_the_types_numbers() {
    let description = schema()["description"].as_str().expect("the root has a description");
    for phrase in [
        format!("at most {MAX_COLUMNS} columns and {MAX_ROWS} rows"),
        format!("at most {MAX_ROWS} entries"),
        format!("at most {MAX_COLUMNS} cells and {MAX_ACTIONS} actions"),
        format!("at most {MAX_ACTION_ARGS} arguments"),
        format!("at most {MAX_CELL} bytes"),
        format!("at most {MAX_TEXT} bytes"),
        format!("at most {MAX_KEY} bytes"),
        format!("at most {MAX_LABEL} bytes"),
        "no field is markup, a link or a command".to_owned(),
    ] {
        assert!(description.contains(&phrase), "the schema description lacks {phrase:?}");
    }
    assert_eq!(MAX_TITLE, MAX_LABEL, "the description states one number for titles and labels");
    for gap in GAPS.iter() {
        assert!(description.contains(gap.doc_phrase), "the schema description does not name the gap {:?}", gap.name);
    }
    // The numbers of the constants are the numbers the schema file states.
    let number = |pointer: &str| schema_check::pointer_number(schema(), SCHEMA_PATH, pointer);
    assert_eq!(number("/oneOf/0/properties/columns/maxItems"), MAX_COLUMNS as u64);
    assert_eq!(number("/oneOf/0/properties/rows/maxItems"), MAX_ROWS as u64);
    assert_eq!(number("/oneOf/1/properties/rows/maxItems"), MAX_ROWS as u64);
    assert_eq!(number("/$defs/text/maxLength"), MAX_CELL as u64);
    assert_eq!(number("/oneOf/2/properties/text/maxLength"), MAX_TEXT as u64);
    assert_eq!(number("/$defs/tableRow/properties/actions/maxItems"), MAX_ACTIONS as u64);
    assert_eq!(number("/oneOf/1/properties/actions/maxItems"), MAX_ACTIONS as u64);
    assert_eq!(number("/$defs/tableRow/properties/cells/maxItems"), MAX_COLUMNS as u64);
    assert_eq!(number("/$defs/action/properties/args/maxProperties"), MAX_ACTION_ARGS as u64);
    assert_eq!(number("/$defs/tableRow/properties/key/maxLength"), MAX_KEY as u64);
    assert_eq!(number("/$defs/action/properties/args/additionalProperties/maxLength"), MAX_KEY as u64);
    assert_eq!(number("/oneOf/0/properties/title/maxLength"), MAX_TITLE as u64);
    assert_eq!(number("/$defs/column/properties/label/maxLength"), MAX_LABEL as u64);
    assert_eq!(number("/$defs/action/properties/args/propertyNames/maxLength"), MAX_LABEL as u64);
}
