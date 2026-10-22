//! The release gate for output: documents that other parties wrote go through the parser of every verb of every tool, and
//! what comes out is a model of printable text within the caps, that the published schema accepts and that reads back as
//! the same model. A document that is not in the shape the parser knows becomes cleaned text, never an error and never a
//! panic.
//!
//! The documents are generated from the recorded and written fixtures of each tool and from nothing else: every string of
//! a valid document is replaced by each hostile shape in turn, replaced by values of the wrong type, a valid document is
//! cut at every tenth byte, wrapped in a byte order mark and carriage returns, nested 200 deep, repeated 3000 times, made
//! 70 columns wide, and joined by binary garbage. The count of documents per tool and per class is printed, and a minimum
//! of each is asserted, so the corpus cannot shrink without a test failing.
//!
//! No tool runs here: a parser is a function of bytes.

mod common;
#[path = "common/schema_check.rs"]
mod schema_check;

use std::collections::{BTreeMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use holzwave_core::integrate::azure::AZURE;
use holzwave_core::integrate::gam::GAM;
use holzwave_core::integrate::kube::KUBE;
use holzwave_core::integrate::talos::TALOS;
use holzwave_core::integrate::view::{ViewModel, MAX_CELL, MAX_KEY, MAX_LABEL, MAX_ROWS, MAX_COLUMNS, MAX_TEXT, MAX_TITLE};
use holzwave_core::integrate::{spec_of, Integration, Output, Selection, ToolId, Verb, VerbEffect};
use serde_json::{json, Value};

use common::Xorshift64;

const SCHEMA_PATH: &str = "docs/integration-view.schema.json";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("core/ has a parent: the workspace root").to_path_buf()
}

fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| schema_check::load_schema(&root(), SCHEMA_PATH))
}

fn tool_of(tool: ToolId) -> &'static dyn Integration {
    match tool {
        ToolId::Kube => &KUBE,
        ToolId::Talos => &TALOS,
        ToolId::Azure => &AZURE,
        ToolId::Gam => &GAM,
    }
}

fn fixture(relative: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrations").join(relative);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {relative}: {e}"))
}

// ---- the documents -------------------------------------------------------------------------------------------------

/// One document: the tool and verb whose parser reads it, the class it belongs to and its bytes.
struct Doc {
    tool: ToolId,
    verb: &'static Verb,
    class: String,
    bytes: Vec<u8>,
}

/// The recorded or written output of each verb that shows a view.
fn bases() -> Vec<(ToolId, &'static str, &'static str)> {
    vec![
        (ToolId::Kube, "contexts", "kubectl/config_view.json"),
        (ToolId::Kube, "namespaces", "kubectl/namespaces.json"),
        (ToolId::Kube, "pods", "kubectl/pods.json"),
        (ToolId::Kube, "pods", "kubectl/pods_hostile.json"),
        (ToolId::Talos, "contexts", "talosctl/config_contexts.txt"),
        (ToolId::Talos, "nodes", "talosctl/get_members.json"),
        (ToolId::Talos, "services", "talosctl/get_services.json"),
        (ToolId::Talos, "machine_status", "talosctl/get_machinestatus.json"),
        (ToolId::Talos, "info", "talosctl/config_info.json"),
        (ToolId::Azure, "account", "az/account_show.json"),
        (ToolId::Azure, "subscriptions", "az/account_list.json"),
        (ToolId::Azure, "groups", "az/group_list.json"),
        (ToolId::Gam, "sections", "gam/showsections.txt"),
        (ToolId::Gam, "domain", "gam/info_domain.json"),
        (ToolId::Gam, "users", "gam/print_users.csv"),
    ]
}

fn ch(code: u32) -> String {
    char::from_u32(code).expect("a scalar value").to_string()
}

/// The hostile shapes of the chat's page gate and of the research: an element with a handler, a script, a link with a
/// script scheme, markup in a name, an escape sequence, a title sequence, a right-to-left override, zero-width and format
/// characters, a tag character, a NUL, a byte order mark, a line break, a long value and a value that breaks out of a
/// string.
fn shapes() -> Vec<(&'static str, String)> {
    vec![
        ("img_onerror", "<img src=x onerror=\"window.__XSS=1\">".to_owned()),
        ("script", "<script>window.__XSS=1</script>".to_owned()),
        ("js_link", "javascript:window.__XSS=1".to_owned()),
        ("markup_name", "<b>pod</b>-0".to_owned()),
        ("svg_onload", "<svg onload=\"window.__XSS=1\"></svg>".to_owned()),
        ("escape", format!("{}[31mred{}[0m", ch(0x1b), ch(0x1b))),
        ("title_sequence", format!("{}]0;owned{}name", ch(0x1b), ch(0x07))),
        ("c1_csi", format!("{}31mred", ch(0x9b))),
        ("rlo", format!("{}gnp.exe", ch(0x202e))),
        ("zero_width", format!("a{}b{}c{}d", ch(0x200b), ch(0x200d), ch(0xfeff))),
        ("format", format!("a{}b{}c{}d", ch(0x206f), ch(0x180e), ch(0xfff9))),
        ("tag", format!("a{}b{}c", ch(0xe0041), ch(0xe007f))),
        ("nul", "a\0b".to_owned()),
        ("line_breaks", "a\r\nb\nc\rd\u{2028}e".to_owned()),
        ("long_5000", "a".repeat(5000)),
        ("long_70000", "x".repeat(70_000)),
        ("quote_break", "\"}}],\"x\":{\"y\":\"".to_owned()),
        ("emoji_joiner", format!("a{}b", ch(0x200d))),
    ]
}

/// Every string value of `value` (never a key) replaced by what `f` makes of it.
fn map_strings(value: &Value, f: &mut dyn FnMut(&str) -> Value) -> Value {
    match value {
        Value::String(text) => f(text),
        Value::Array(items) => Value::Array(items.iter().map(|item| map_strings(item, f)).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(key, item)| (key.clone(), map_strings(item, f))).collect()),
        other => other.clone(),
    }
}

/// The JSON values of a document: one value, or a stream of values one after the other. `None` for anything else.
fn json_values(bytes: &[u8]) -> Option<Vec<Value>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut values = Vec::new();
    for value in serde_json::Deserializer::from_str(text).into_iter::<Value>() {
        values.push(value.ok()?);
    }
    (!values.is_empty()).then_some(values)
}

fn join_values(values: &[Value]) -> Vec<u8> {
    values.iter().map(|value| serde_json::to_string(value).expect("JSON")).collect::<Vec<_>>().join("\n").into_bytes()
}

/// The longest array in `value`, found by depth first, grown to `n` copies of its first element.
fn blow_up(value: &mut Value, n: usize) -> bool {
    match value {
        Value::Array(items) if !items.is_empty() => {
            let first = items[0].clone();
            items.resize(n, first);
            true
        }
        Value::Array(_) => false,
        Value::Object(map) => map.values_mut().any(|item| blow_up(item, n)),
        _ => false,
    }
}

fn nest_array(depth: usize) -> Value {
    let mut value = json!("deep");
    for _ in 0..depth {
        value = Value::Array(vec![value]);
    }
    value
}

fn nest_object(depth: usize) -> Value {
    let mut value = json!("deep");
    for _ in 0..depth {
        value = json!({ "a": value });
    }
    value
}

/// A CSV field: quoted, with a doubled quote.
fn csv_field(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

fn push(docs: &mut Vec<Doc>, tool: ToolId, verb: &'static Verb, class: impl Into<String>, bytes: Vec<u8>) {
    docs.push(Doc { tool, verb, class: class.into(), bytes });
}

/// The documents every verb gets, whatever its format: damage to the bytes.
fn generic(docs: &mut Vec<Doc>, tool: ToolId, verb: &'static Verb, base: &[u8]) {
    let mut add = |class: &str, bytes: Vec<u8>| push(docs, tool, verb, class, bytes);
    add("valid", base.to_vec());
    for step in (10..base.len()).step_by(10) {
        add("truncated_every_10_bytes", base[..step].to_vec());
    }
    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend_from_slice(base);
    add("byte_order_mark", bom);
    let crlf = String::from_utf8_lossy(base).replace('\n', "\r\n");
    add("crlf", crlf.clone().into_bytes());
    add("cr_only", String::from_utf8_lossy(base).replace('\n', "\r").into_bytes());
    let mut both = vec![0xef, 0xbb, 0xbf];
    both.extend_from_slice(crlf.as_bytes());
    add("bom_and_crlf", both);
    let middle = base.len() / 2;
    for (class, insert) in [
        ("nul_inside", &[0_u8][..]),
        ("invalid_utf8", &[0xff, 0xfe, 0xc0, 0x80][..]),
        ("lone_continuation", &[0x80, 0x80][..]),
        ("escape_inside", b"\x1b[2J\x1b]0;x\x07"),
    ] {
        let mut bytes = base[..middle].to_vec();
        bytes.extend_from_slice(insert);
        bytes.extend_from_slice(&base[middle..]);
        add(class, bytes);
    }
    // A multi-byte character cut in the middle, at the end.
    let mut cut = base.to_vec();
    cut.extend_from_slice("\u{20ac}".as_bytes());
    cut.pop();
    add("cut_in_a_character", cut);
    for (class, bytes) in [
        ("empty", Vec::new()),
        ("blank", b" \n\t \r\n".to_vec()),
        ("bom_only", vec![0xef, 0xbb, 0xbf]),
        ("null", b"null".to_vec()),
        ("empty_object", b"{}".to_vec()),
        ("empty_array", b"[]".to_vec()),
        ("array_of_arrays", b"[[],[[]]]".to_vec()),
        ("scalar", b"42".to_vec()),
        ("string", b"\"text\"".to_vec()),
        ("nuls", vec![0; 10_000]),
        ("deep_unclosed", vec![b'['; 100_000]),
        ("deep_objects_unclosed", b"{\"a\":".repeat(50_000)),
        ("one_long_line", vec![b'a'; 1024 * 1024 + 7]),
        ("quote_run", vec![b'"'; 100_000]),
        ("brace_run", vec![b'{'; 100_000]),
    ] {
        add(class, bytes);
    }
    let mut rng = Xorshift64::new(0x5eed_0123_4567_89ab);
    for round in 0..4 {
        let bytes: Vec<u8> = (0..4096 + round * 1024).map(|_| rng.next_u64().to_le_bytes()[0]).collect();
        add("random_bytes", bytes);
    }
}

/// The documents of a JSON verb (one value or a stream of values).
fn from_json(docs: &mut Vec<Doc>, tool: ToolId, verb: &'static Verb, values: &[Value]) {
    for (name, shape) in shapes() {
        let swapped: Vec<Value> = values.iter().map(|v| map_strings(v, &mut |_| Value::String(shape.clone()))).collect();
        push(docs, tool, verb, format!("hostile_{name}"), join_values(&swapped));
    }
    let wrong: [(&str, Value); 8] = [
        ("null", Value::Null),
        ("number", json!(42)),
        ("float", json!(-1.5)),
        ("bool", json!(true)),
        ("empty_array", json!([])),
        ("empty_object", json!({})),
        ("array_of_arrays", json!([[["x"]]])),
        ("object_of_objects", json!({"a": {"b": {}}})),
    ];
    for (name, replacement) in wrong {
        let swapped: Vec<Value> = values.iter().map(|v| map_strings(v, &mut |_| replacement.clone())).collect();
        push(docs, tool, verb, format!("wrong_type_{name}"), join_values(&swapped));
    }
    for depth in [200, 1000] {
        let array: Vec<Value> = values.iter().map(|v| map_strings(v, &mut |_| nest_array(depth))).collect();
        push(docs, tool, verb, format!("nested_array_{depth}"), join_values(&array));
        let object: Vec<Value> = values.iter().map(|v| map_strings(v, &mut |_| nest_object(depth))).collect();
        push(docs, tool, verb, format!("nested_object_{depth}"), join_values(&object));
    }
    let mut top = String::new();
    top.push_str(&"[".repeat(200));
    top.push_str(&"]".repeat(200));
    push(docs, tool, verb, "nested_top_200", top.into_bytes());
    push(docs, tool, verb, "nested_top_object_200", serde_json::to_vec(&nest_object(200)).expect("JSON"));
    // 3000 of the first element of the longest list, or 3000 objects one after the other.
    let mut grown = values.to_vec();
    let found = grown.iter_mut().any(|v| blow_up(v, 3000));
    if found {
        push(docs, tool, verb, "rows_3000", join_values(&grown));
    }
    push(docs, tool, verb, "stream_3000_objects", join_values(&vec![values[0].clone(); 3000]));
    // 70 members more in every object.
    let wide: Vec<Value> = values.iter().map(|v| widen(v, 70)).collect();
    push(docs, tool, verb, "wide_70_members", join_values(&wide));
    // A 5000-character and a 2 MiB value in place of the first string.
    for (class, size) in [("cell_5000", 5000), ("cell_2_mib", 2 * 1024 * 1024)] {
        let mut done = false;
        let long: Vec<Value> = values
            .iter()
            .map(|v| {
                map_strings(v, &mut |text| {
                    if done {
                        return Value::String(text.to_owned());
                    }
                    done = true;
                    Value::String("a".repeat(size))
                })
            })
            .collect();
        push(docs, tool, verb, class, join_values(&long));
    }
    // The document inside an array, and a stream that goes wrong after the first object.
    push(docs, tool, verb, "wrapped_in_array", serde_json::to_vec(&Value::Array(values.to_vec())).expect("JSON"));
    let mut broken = join_values(&values[..1]);
    broken.extend_from_slice(b"\n{\"x\": not json}\n");
    push(docs, tool, verb, "second_object_broken", broken);
    push(docs, tool, verb, "pretty", serde_json::to_string_pretty(&values[0]).expect("JSON").into_bytes());
}

fn widen(value: &Value, n: usize) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(|item| widen(item, n)).collect()),
        Value::Object(map) => {
            let mut out: serde_json::Map<String, Value> = map.iter().map(|(k, v)| (k.clone(), widen(v, n))).collect();
            for i in 0..n {
                out.insert(format!("extra{i}"), json!("v"));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// The documents of the CSV verb.
fn from_csv(docs: &mut Vec<Doc>, tool: ToolId, verb: &'static Verb, base: &str) {
    let header = base.lines().next().unwrap_or("primaryEmail,name");
    let width = header.split(',').count();
    let row = |cells: &[String]| cells.iter().map(|c| csv_field(c)).collect::<Vec<_>>().join(",");
    for (name, shape) in shapes() {
        let cells = vec![shape.clone(); width];
        push(docs, tool, verb, format!("hostile_{name}"), format!("{header}\n{}\n{}\n", row(&cells), row(&cells)).into_bytes());
        let hostile_header = vec![shape.clone(); width];
        push(docs, tool, verb, format!("hostile_header_{name}"), format!("{}\n{}\n", row(&hostile_header), row(&cells)).into_bytes());
    }
    let mut many = String::from(header);
    many.push('\n');
    for i in 0..3000 {
        many.push_str(&row(&vec![format!("u{i}@example.com"); width]));
        many.push('\n');
    }
    push(docs, tool, verb, "rows_3000", many.into_bytes());
    let names: Vec<String> = (0..70).map(|i| format!("c{i}")).collect();
    let mut wide = names.join(",");
    wide.push('\n');
    wide.push_str(&names.iter().map(|n| format!("v{n}")).collect::<Vec<_>>().join(","));
    wide.push('\n');
    push(docs, tool, verb, "columns_70", wide.into_bytes());
    push(docs, tool, verb, "cell_5000", format!("{header}\n{}\n", row(&vec!["a".repeat(5000); width])).into_bytes());
    push(docs, tool, verb, "cell_2_mib", format!("{header}\n{}\n", row(&vec!["a".repeat(2 * 1024 * 1024); width])).into_bytes());
    push(docs, tool, verb, "duplicate_headers", "name,name,name\na,b,c\n".as_bytes().to_vec());
    push(docs, tool, verb, "ragged_rows", format!("{header}\na\na,b,c,d,e,f,g,h\n\n,\n").into_bytes());
    push(docs, tool, verb, "unterminated_quote", format!("{header}\n\"never closed,a\nb,c\n").into_bytes());
    push(docs, tool, verb, "multiline_cell", format!("{header}\n\"line one\nline two, with \"\"quotes\"\"\",x\n").into_bytes());
    push(docs, tool, verb, "header_only", format!("{header}\n").into_bytes());
    push(docs, tool, verb, "only_commas", ",,,,\n,,,,\n".as_bytes().to_vec());
    push(docs, tool, verb, "json_in_csv", b"{\"a\":1}\n".to_vec());
}

/// The documents of a plain-text verb (a table of words, a list of sections).
fn from_text(docs: &mut Vec<Doc>, tool: ToolId, verb: &'static Verb, base: &str) {
    for (name, shape) in shapes() {
        let one_line = shape.replace(['\r', '\n'], " ");
        push(docs, tool, verb, format!("hostile_{name}"), format!("{base}  {one_line}\n  {one_line} *\n* {one_line}  {one_line}  {one_line}\n").into_bytes());
        push(docs, tool, verb, format!("hostile_only_{name}"), format!("{one_line}\n").into_bytes());
    }
    push(docs, tool, verb, "rows_3000", format!("{base}{}", "  name\n".repeat(3000)).into_bytes());
    push(docs, tool, verb, "columns_70", format!("{}\n{}\n", vec!["COL"; 70].join(" "), vec!["val"; 70].join(" ")).into_bytes());
    push(docs, tool, verb, "cell_5000", format!("{base}  {}\n", "a".repeat(5000)).into_bytes());
    push(docs, tool, verb, "lines_2_mib", "line of text\n".repeat(160_000).into_bytes());
    push(docs, tool, verb, "only_stars", "* * * *\n* *\n".as_bytes().to_vec());
}

fn documents() -> &'static Vec<Doc> {
    static DOCS: OnceLock<Vec<Doc>> = OnceLock::new();
    DOCS.get_or_init(|| {
        let mut docs = Vec::new();
        for (tool, verb_id, file) in bases() {
            let verb = spec_of(tool).verb(verb_id).expect("a verb of the tool");
            assert!(verb.effect == VerbEffect::Read && verb.output == Output::View, "{verb_id} shows a view");
            let bytes = fixture(file);
            generic(&mut docs, tool, verb, &bytes);
            if file.ends_with(".csv") {
                from_csv(&mut docs, tool, verb, &String::from_utf8_lossy(&bytes));
            } else if let Some(values) = json_values(&bytes) {
                from_json(&mut docs, tool, verb, &values);
            } else {
                from_text(&mut docs, tool, verb, &String::from_utf8_lossy(&bytes));
            }
        }
        docs
    })
}

// ---- what a model may hold ------------------------------------------------------------------------------------------

/// Whether a character has no place in a cell: a control, a direction mark, a zero-width, format or tag character, or a
/// line or paragraph separator. The list is this test's own, written from the Unicode category Cf and the separators, so
/// that it does not depend on the list the core cleans with.
fn hidden(c: char, keep_lines: bool) -> bool {
    if keep_lines && matches!(c, '\n' | '\t') {
        return false;
    }
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08E2}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
        )
}

fn text_ok(what: &str, text: &str, cap: usize, keep_lines: bool) -> Result<(), String> {
    if text.len() > cap {
        return Err(format!("{what} holds {} bytes, the cap is {cap}", text.len()));
    }
    if let Some(c) = text.chars().find(|c| hidden(*c, keep_lines)) {
        return Err(format!("{what} holds the hidden character U+{:04X}", u32::from(c)));
    }
    Ok(())
}

/// Every text of a model is printable and within its cap, and the numbers are within theirs.
fn model_ok(model: &ViewModel) -> Result<(), String> {
    model.check().map_err(|e| e.to_string())?;
    match model {
        ViewModel::Table(table) => {
            text_ok("the title", table.title(), MAX_TITLE, false)?;
            if table.columns().len() > MAX_COLUMNS || table.rows().len() > MAX_ROWS {
                return Err(format!("{} columns and {} rows", table.columns().len(), table.rows().len()));
            }
            for column in table.columns() {
                text_ok("a column id", column.id(), MAX_LABEL, false)?;
                text_ok("a column label", column.label(), MAX_LABEL, false)?;
            }
            for row in table.rows() {
                text_ok("a row key", row.key(), MAX_KEY, false)?;
                for cell in row.cells() {
                    text_ok("a cell", cell, MAX_CELL, false)?;
                }
                for action in row.actions() {
                    text_ok("an action verb", action.verb(), MAX_LABEL, false)?;
                    text_ok("an action label", action.label(), MAX_LABEL, false)?;
                    for (name, value) in action.args() {
                        text_ok("an argument name", name, MAX_LABEL, false)?;
                        text_ok("an argument value", value, MAX_KEY, false)?;
                    }
                }
            }
            if let Some(cut) = table.truncated() {
                text_ok("the cut note", cut.note(), MAX_LABEL, false)?;
            }
            if let Some(note) = table.note() {
                text_ok("the note", note, MAX_LABEL, false)?;
            }
        }
        ViewModel::Kv(view) => {
            text_ok("the title", view.title(), MAX_TITLE, false)?;
            for row in view.rows() {
                text_ok("a label", row.label(), MAX_LABEL, false)?;
                text_ok("a value", row.value(), MAX_CELL, false)?;
            }
            for action in view.actions() {
                text_ok("an action label", action.label(), MAX_LABEL, false)?;
            }
            if let Some(note) = view.note() {
                text_ok("the note", note, MAX_LABEL, false)?;
            }
        }
        ViewModel::Text(view) => {
            text_ok("the title", view.title(), MAX_TITLE, false)?;
            text_ok("the text", view.text(), MAX_TEXT, true)?;
            if let Some(note) = view.note() {
                text_ok("the note", note, MAX_LABEL, false)?;
            }
        }
    }
    Ok(())
}

/// What a parser made of a document: a model, nothing (`ParseError`), or a panic.
enum Outcome {
    Model(ViewModel),
    Refused,
    Panic(String),
}

fn parse(doc: &Doc) -> Outcome {
    let integration = tool_of(doc.tool);
    match catch_unwind(AssertUnwindSafe(|| integration.view(doc.verb, &doc.bytes))) {
        Ok(Ok(model)) => Outcome::Model(model),
        Ok(Err(_)) => Outcome::Refused,
        Err(panic) => {
            let why = panic.downcast_ref::<&str>().map(|s| (*s).to_owned()).or_else(|| panic.downcast_ref::<String>().cloned());
            Outcome::Panic(why.unwrap_or_default())
        }
    }
}

fn label(doc: &Doc) -> String {
    format!("{}/{} {} ({} bytes)", doc.tool.as_str(), doc.verb.id, doc.class, doc.bytes.len())
}

/// Per tool and class: how many documents.
fn counts(docs: &[Doc]) -> BTreeMap<(&'static str, String), usize> {
    let mut out = BTreeMap::new();
    for doc in docs {
        *out.entry((doc.tool.as_str(), doc.class.clone())).or_default() += 1;
    }
    out
}

// ---- the tests -----------------------------------------------------------------------------------------------------

#[test]
fn every_parser_gives_only_clean_bounded_cells() {
    let docs = documents();
    let (mut models, mut refused) = (0_usize, 0_usize);
    let mut failures = Vec::new();
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for doc in docs {
        match parse(doc) {
            Outcome::Panic(why) => failures.push(format!("{}: panicked: {why}", label(doc))),
            Outcome::Refused => refused += 1,
            Outcome::Model(model) => {
                models += 1;
                *kinds.entry(match &model {
                    ViewModel::Table(_) => "table",
                    ViewModel::Kv(_) => "kv",
                    ViewModel::Text(_) => "text",
                })
                .or_default() += 1;
                if let Err(why) = model_ok(&model) {
                    failures.push(format!("{}: {why}", label(doc)));
                }
                // The model reads back as itself: the caps of the type hold for what the parser built.
                let json = serde_json::to_string(&model).expect("a model is JSON");
                match serde_json::from_str::<ViewModel>(&json) {
                    Ok(back) if back == model => {}
                    Ok(_) => failures.push(format!("{}: the model reads back as another", label(doc))),
                    Err(why) => failures.push(format!("{}: the model does not read back: {why}", label(doc))),
                }
            }
        }
        // The tool's own selection: it may not carry a hidden character past the validators.
        let integration = tool_of(doc.tool);
        let read = catch_unwind(AssertUnwindSafe(|| integration.read_default(doc.verb, &doc.bytes)));
        match read {
            Err(_) => failures.push(format!("{}: read_default panicked", label(doc))),
            Ok(None) => {}
            Ok(Some(found)) => {
                let selection = found.selection;
                for (field, value) in [
                    ("profile", &selection.profile),
                    ("context", &selection.context),
                    ("namespace", &selection.namespace),
                    ("subscription", &selection.subscription),
                    ("section", &selection.section),
                    ("node", &selection.node),
                ] {
                    let Some(value) = value else { continue };
                    let single = Selection {
                        context: (field == "context").then(|| value.clone()),
                        namespace: (field == "namespace").then(|| value.clone()),
                        subscription: (field == "subscription").then(|| value.clone()),
                        section: (field == "section").then(|| value.clone()),
                        node: (field == "node").then(|| value.clone()),
                        profile: None,
                    };
                    let taken = single.check(doc.tool).is_ok();
                    if taken && value.chars().any(|c| hidden(c, false)) {
                        failures.push(format!("{}: the selection {field} holds a hidden character and passes the check", label(doc)));
                    }
                }
            }
        }
    }
    let per_tool = counts(docs);
    let mut totals: BTreeMap<&str, usize> = BTreeMap::new();
    for ((tool, _), n) in &per_tool {
        *totals.entry(tool).or_default() += n;
    }
    println!("Hostile corpus: {} documents, {models} models {kinds:?}, {refused} refused as no answer", docs.len());
    println!("Hostile documents per tool: {totals:?}");
    let classes: HashSet<&str> = docs.iter().map(|doc| doc.class.as_str()).collect();
    println!("Hostile classes: {}", classes.len());
    for ((tool, class), n) in &per_tool {
        println!("Hostile {tool} {class}: {n}");
    }
    assert!(failures.is_empty(), "{} documents broke a rule:\n{}", failures.len(), failures.join("\n"));
    for tool in ToolId::ALL {
        let n = totals.get(tool.as_str()).copied().unwrap_or(0);
        assert!(n >= 40, "{} has {n} documents, at least 40 are asserted", tool.as_str());
    }
    // A minimum per class, so that none can be dropped.
    for (class, minimum) in [
        ("valid", 4),
        ("truncated_every_10_bytes", 200),
        ("hostile_img_onerror", 4),
        ("hostile_rlo", 4),
        ("hostile_zero_width", 4),
        ("hostile_format", 4),
        ("hostile_tag", 4),
        ("hostile_nul", 4),
        ("hostile_escape", 4),
        ("hostile_long_5000", 4),
        ("wrong_type_number", 3),
        ("nested_array_200", 3),
        ("rows_3000", 4),
        ("byte_order_mark", 4),
        ("crlf", 4),
        ("random_bytes", 12),
        ("empty", 4),
    ] {
        let n = docs.iter().filter(|doc| doc.class == class).count();
        assert!(n >= minimum, "class {class}: {n} documents, at least {minimum} are asserted");
    }
    assert!(models * 2 > docs.len(), "most documents give a model: {models} of {}", docs.len());
}

#[test]
fn every_produced_model_is_accepted_by_the_schema() {
    let docs = documents();
    let mut seen = HashSet::new();
    let (mut checked, mut failures) = (0_usize, Vec::new());
    for doc in docs {
        let Outcome::Model(model) = parse(doc) else { continue };
        let json = serde_json::to_string(&model).expect("a model is JSON");
        // Many cut documents give the same text model: each distinct model is judged once.
        if !seen.insert(json.clone()) {
            continue;
        }
        checked += 1;
        if let Err(why) = schema_check::schema_verdict(schema(), SCHEMA_PATH, &json) {
            failures.push(format!("{}: the schema refuses the model: {why}", label(doc)));
        }
    }
    println!("Hostile schema: {checked} distinct models accepted by {SCHEMA_PATH}");
    assert!(failures.is_empty(), "{} models are refused:\n{}", failures.len(), failures.join("\n"));
    assert!(checked >= 100, "{checked} distinct models were checked");
}

#[test]
fn a_parser_asked_twice_gives_the_same_model() {
    // A parser holds no state: the same bytes give the same model, which is what lets the page compare two reads.
    for doc in documents().iter().filter(|doc| doc.class == "valid" || doc.class.starts_with("hostile_img")) {
        let (Outcome::Model(first), Outcome::Model(second)) = (parse(doc), parse(doc)) else { continue };
        let strip = |model: ViewModel| model.with_fetched(0);
        assert_eq!(strip(first), strip(second), "{}", label(doc));
    }
}
