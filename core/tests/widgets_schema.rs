//! The widget schema and the widget loader judge every document alike.
//!
//! `docs/widget.schema.json` is the published description of a widget file (version 1) and `widgets::parse` is the
//! validator the app runs. The contract of this file has three sentences. A file that the loader accepts is accepted by
//! the schema. A file that the schema accepts is accepted by the loader, except the documents named in `GAPS`, which no
//! JSON Schema can describe. The schema is the published description and the loader is the judge: when they differ, a test
//! names the document.
//!
//! The checker below is a small subset evaluator of JSON Schema 2020-12 and not a schema crate (the dependency budget of
//! the project). It fails closed: a keyword it does not evaluate is an error and never an accept, and the schema file is
//! linted keyword by keyword before any verdict is read.

mod common;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use holzwave_core::widgets::{self, WidgetDef};
use regex::Regex;
use serde_json::{json, Map, Number, Value};

const SCHEMA_PATH: &str = "docs/widget.schema.json";
const CONFIG_PATH: &str = "docs/config.md";
const WIDGET_HEADING: &str = "## `widgets/*.json` and `widget_notes.json`";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("core/ has a parent: the workspace root").to_path_buf()
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------------------------------------------------
// The checker: a subset of JSON Schema 2020-12 that fails closed.
// ---------------------------------------------------------------------------------------------------------------------

/// The checker was asked to judge a schema that uses something it does not evaluate.
#[derive(Debug, PartialEq)]
struct Unsupported(String);

/// `Ok(None)` is valid, `Ok(Some(path))` is invalid and names the first failing keyword, `Err` is a schema the checker
/// cannot judge.
type Outcome = Result<Option<String>, Unsupported>;

/// Keywords that decide a verdict.
const EVALUATED: &[&str] = &[
    "type",
    "enum",
    "const",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "pattern",
    "minItems",
    "maxItems",
    "items",
    "required",
    "properties",
    "additionalProperties",
    "allOf",
    "oneOf",
    "if",
    "then",
    "else",
    "$ref",
];

/// Keywords that never decide a verdict. `$defs` is a container: its entries are reached through `$ref` and are linted.
const ANNOTATIONS: &[&str] = &["$schema", "$comment", "title", "description", "default", "examples", "$defs"];

const TYPE_NAMES: &[&str] = &["object", "array", "string", "integer", "number", "boolean", "null"];

/// The deepest chain of references the checker follows.
const MAX_DEPTH: usize = 32;

fn unsupported(at: &str, why: &str) -> Unsupported {
    Unsupported(format!("{at}: {why}"))
}

/// A compiled pattern, cached: the sweeps judge thousands of documents with the same few patterns.
fn regex(pattern: &str) -> Result<Arc<Regex>, Unsupported> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Regex>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = cache.get(pattern) {
        return Ok(Arc::clone(found));
    }
    let compiled = Regex::new(pattern).map_err(|e| Unsupported(format!("the pattern {pattern:?} does not compile: {e}")))?;
    let compiled = Arc::new(compiled);
    cache.insert(pattern.to_owned(), Arc::clone(&compiled));
    Ok(compiled)
}

/// The `$defs` entry that a reference names. Only `#/$defs/<name>` is followed: no remote, no anchor, no pointer elsewhere.
fn resolve<'a>(root: &'a Value, reference: &Value, at: &str) -> Result<&'a Value, Unsupported> {
    let reference = reference.as_str().ok_or_else(|| unsupported(at, "a $ref must be a string"))?;
    let name = reference
        .strip_prefix("#/$defs/")
        .filter(|n| !n.is_empty() && !n.contains('/') && !n.contains('~'))
        .ok_or_else(|| unsupported(at, &format!("the reference {reference:?} is not a local $defs entry")))?;
    root.get("$defs")
        .and_then(|defs| defs.get(name))
        .ok_or_else(|| unsupported(at, &format!("the reference {reference:?} names no $defs entry")))
}

/// Check every keyword of every subschema, also in branches that no document reaches.
fn lint(root: &Value) -> Result<(), Unsupported> {
    walk(root, root, "#")
}

fn walk_map(root: &Value, value: &Value, at: &str) -> Result<(), Unsupported> {
    let map = value.as_object().ok_or_else(|| unsupported(at, "this must be an object of subschemas"))?;
    map.iter().try_for_each(|(name, sub)| walk(root, sub, &format!("{at}/{name}")))
}

fn walk_list(root: &Value, value: &Value, at: &str) -> Result<(), Unsupported> {
    let list = value.as_array().ok_or_else(|| unsupported(at, "this must be an array of subschemas"))?;
    list.iter().enumerate().try_for_each(|(n, sub)| walk(root, sub, &format!("{at}/{n}")))
}

fn walk(root: &Value, schema: &Value, at: &str) -> Result<(), Unsupported> {
    let map = schema.as_object().ok_or_else(|| unsupported(at, "a subschema must be an object"))?;
    for (key, value) in map {
        let here = format!("{at}/{key}");
        match key.as_str() {
            "$defs" | "properties" => walk_map(root, value, &here)?,
            "allOf" | "oneOf" => walk_list(root, value, &here)?,
            "items" | "if" | "then" | "else" => walk(root, value, &here)?,
            "additionalProperties" => {
                if !value.is_boolean() {
                    walk(root, value, &here)?;
                }
            }
            "$ref" => {
                resolve(root, value, &here)?;
            }
            "pattern" => {
                let pattern = value.as_str().ok_or_else(|| unsupported(&here, "a pattern must be a string"))?;
                regex(pattern)?;
            }
            "type" => {
                let names: Vec<&str> = match value {
                    Value::String(one) => vec![one.as_str()],
                    Value::Array(many) => many.iter().filter_map(Value::as_str).collect(),
                    _ => Vec::new(),
                };
                let count = value.as_array().map_or(1, Vec::len);
                if names.len() != count || names.iter().any(|n| !TYPE_NAMES.contains(n)) {
                    return Err(unsupported(&here, "a type must be a name or an array of names the checker knows"));
                }
            }
            other if EVALUATED.contains(&other) || ANNOTATIONS.contains(&other) => {}
            other => return Err(unsupported(&here, &format!("the keyword {other} is not evaluated by the checker"))),
        }
    }
    Ok(())
}

fn as_i128(n: &Number) -> Option<i128> {
    n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from))
}

fn compare(a: &Number, b: &Number) -> std::cmp::Ordering {
    match (as_i128(a), as_i128(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        _ => {
            let (x, y) = (a.as_f64().expect("a JSON number has an f64"), b.as_f64().expect("a JSON number has an f64"));
            x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
        }
    }
}

fn type_matches(name: &str, instance: &Value) -> bool {
    match name {
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "boolean" => instance.is_boolean(),
        "null" => instance.is_null(),
        "number" => instance.is_number(),
        // A number written with a fraction or an exponent is not an integer, whatever its value: the loader reads its
        // integer members as integers and refuses 25.0 and 2.5e1.
        "integer" => matches!(instance, Value::Number(n) if n.is_i64() || n.is_u64()),
        _ => false,
    }
}

fn limit(value: &Value, at: &str) -> Result<u64, Unsupported> {
    value.as_u64().ok_or_else(|| unsupported(at, "a limit must be a whole number"))
}

/// Judge `instance` by `schema` (a subschema of `root`). The keywords of `schema` and everything below it must be known.
fn evaluate(root: &Value, schema: &Value, instance: &Value) -> Outcome {
    lint(root)?;
    eval(root, schema, instance, "#", 0)
}

fn eval_all<'a>(
    root: &Value,
    schemas: impl Iterator<Item = (String, &'a Value)>,
    instance: &Value,
    depth: usize,
) -> Result<Vec<Option<String>>, Unsupported> {
    schemas.map(|(at, sub)| eval(root, sub, instance, &at, depth)).collect()
}

fn eval(root: &Value, schema: &Value, instance: &Value, at: &str, depth: usize) -> Outcome {
    if depth > MAX_DEPTH {
        return Err(unsupported(at, "references nest deeper than the checker follows"));
    }
    let map = schema.as_object().ok_or_else(|| unsupported(at, "a subschema must be an object"))?;
    for (key, value) in map {
        let here = format!("{at}/{key}");
        let refused = Ok(Some(here.clone()));
        match key.as_str() {
            "type" => {
                let names: Vec<&str> = match value {
                    Value::String(one) => vec![one.as_str()],
                    other => other.as_array().into_iter().flatten().filter_map(Value::as_str).collect(),
                };
                if !names.iter().any(|name| type_matches(name, instance)) {
                    return refused;
                }
            }
            "enum" => {
                let allowed = value.as_array().ok_or_else(|| unsupported(&here, "an enum must be an array"))?;
                if !allowed.contains(instance) {
                    return refused;
                }
            }
            "const" => {
                if value != instance {
                    return refused;
                }
            }
            "minimum" | "maximum" => {
                if let Value::Number(n) = instance {
                    let Value::Number(bound) = value else { return Err(unsupported(&here, "a bound must be a number")) };
                    let order = compare(n, bound);
                    let below = key == "minimum" && order == std::cmp::Ordering::Less;
                    let above = key == "maximum" && order == std::cmp::Ordering::Greater;
                    if below || above {
                        return refused;
                    }
                }
            }
            "minLength" | "maxLength" => {
                if let Value::String(s) = instance {
                    let bound = limit(value, &here)?;
                    let chars = s.chars().count() as u64;
                    if (key == "minLength" && chars < bound) || (key == "maxLength" && chars > bound) {
                        return refused;
                    }
                }
            }
            "pattern" => {
                if let Value::String(s) = instance {
                    let pattern = value.as_str().ok_or_else(|| unsupported(&here, "a pattern must be a string"))?;
                    if !regex(pattern)?.is_match(s) {
                        return refused;
                    }
                }
            }
            "minItems" | "maxItems" => {
                if let Value::Array(list) = instance {
                    let bound = limit(value, &here)?;
                    let count = list.len() as u64;
                    if (key == "minItems" && count < bound) || (key == "maxItems" && count > bound) {
                        return refused;
                    }
                }
            }
            "items" => {
                if let Value::Array(list) = instance {
                    for (n, element) in list.iter().enumerate() {
                        if let Some(path) = eval(root, value, element, &here, depth)? {
                            return Ok(Some(format!("{path} (item {n})")));
                        }
                    }
                }
            }
            "required" => {
                if let Value::Object(members) = instance {
                    let names = value.as_array().ok_or_else(|| unsupported(&here, "required must be an array"))?;
                    if names.iter().any(|name| name.as_str().is_none_or(|n| !members.contains_key(n))) {
                        return refused;
                    }
                }
            }
            "properties" => {
                if let Value::Object(members) = instance {
                    let declared = value.as_object().ok_or_else(|| unsupported(&here, "properties must be an object"))?;
                    for (name, sub) in declared {
                        if let Some(member) = members.get(name) {
                            if let Some(path) = eval(root, sub, member, &format!("{here}/{name}"), depth)? {
                                return Ok(Some(path));
                            }
                        }
                    }
                }
            }
            "additionalProperties" => {
                if let Value::Object(members) = instance {
                    let declared = map.get("properties").and_then(Value::as_object);
                    for (name, member) in members {
                        if declared.is_some_and(|d| d.contains_key(name)) {
                            continue;
                        }
                        match value {
                            Value::Bool(false) => return Ok(Some(format!("{here} (member {name:?})"))),
                            Value::Bool(true) => {}
                            sub => {
                                if let Some(path) = eval(root, sub, member, &here, depth)? {
                                    return Ok(Some(path));
                                }
                            }
                        }
                    }
                }
            }
            "allOf" => {
                let list = value.as_array().ok_or_else(|| unsupported(&here, "allOf must be an array"))?;
                let all = list.iter().enumerate().map(|(n, sub)| (format!("{here}/{n}"), sub));
                if let Some(path) = eval_all(root, all, instance, depth)?.into_iter().flatten().next() {
                    return Ok(Some(path));
                }
            }
            "oneOf" => {
                let list = value.as_array().ok_or_else(|| unsupported(&here, "oneOf must be an array"))?;
                let all = list.iter().enumerate().map(|(n, sub)| (format!("{here}/{n}"), sub));
                let matching = eval_all(root, all, instance, depth)?.into_iter().filter(Option::is_none).count();
                if matching != 1 {
                    return refused;
                }
            }
            "if" => {
                let chosen = if eval(root, value, instance, &here, depth)?.is_none() { "then" } else { "else" };
                if let Some(branch) = map.get(chosen) {
                    if let Some(path) = eval(root, branch, instance, &format!("{at}/{chosen}"), depth)? {
                        return Ok(Some(path));
                    }
                }
            }
            "$ref" => {
                let target = resolve(root, value, &here)?;
                let name = value.as_str().unwrap_or_default().trim_start_matches('#');
                if let Some(path) = eval(root, target, instance, &format!("#{name}"), depth + 1)? {
                    return Ok(Some(path));
                }
            }
            // `then` and `else` are judged by `if`, and without `if` they are ignored, as the draft says.
            "then" | "else" => {}
            other if ANNOTATIONS.contains(&other) => {}
            other => return Err(unsupported(&here, &format!("the keyword {other} is not evaluated by the checker"))),
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------------------------------------------------
// The two verdicts.
// ---------------------------------------------------------------------------------------------------------------------

/// The schema file, parsed once and linted before it is returned.
fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let text = read(SCHEMA_PATH);
        let value: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{SCHEMA_PATH} is not JSON: {e}"));
        if let Err(Unsupported(why)) = lint(&value) {
            panic!("{SCHEMA_PATH} uses what the checker does not evaluate: {why}");
        }
        value
    })
}

/// What the app does with a widget file named `widget.json`.
fn loader_verdict(text: &str) -> Result<WidgetDef, String> {
    widgets::parse(text, "widget")
}

/// What the schema says: `Err` is "not JSON" or the first failing keyword path. A keyword the checker does not evaluate
/// stops the test and is never an accept.
fn schema_verdict(text: &str) -> Result<(), String> {
    let document: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let schema = schema();
    match eval(schema, schema, &document, "#", 0) {
        Ok(None) => Ok(()),
        Ok(Some(path)) => Err(path),
        Err(Unsupported(why)) => panic!("the checker cannot judge {SCHEMA_PATH}: {why}"),
    }
}

/// Both sides must give the same verdict; says whether they accepted.
fn same_verdict(label: &str, text: &str) -> bool {
    let (loader, schema) = (loader_verdict(text), schema_verdict(text));
    assert!(
        loader.is_ok() == schema.is_ok(),
        "{label}: the loader and the schema differ\n  document: {text}\n  loader: {:?}\n  schema: {schema:?}",
        loader.as_ref().map(|_| "accepted")
    );
    loader.is_ok()
}

// ---------------------------------------------------------------------------------------------------------------------
// The schema file itself.
// ---------------------------------------------------------------------------------------------------------------------

/// Every `pattern` string anywhere in a JSON value, with its pointer.
fn patterns_in(value: &Value, at: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                match (key.as_str(), inner) {
                    ("pattern", Value::String(p)) => out.push((format!("{at}/{key}"), p.clone())),
                    _ => patterns_in(inner, &format!("{at}/{key}"), out),
                }
            }
        }
        Value::Array(list) => list.iter().enumerate().for_each(|(n, inner)| patterns_in(inner, &format!("{at}/{n}"), out)),
        _ => {}
    }
}

fn backslash() -> char {
    char::from_u32(0x5c).expect("a backslash")
}

#[test]
fn the_schema_file_is_draft_2020_12_and_uses_only_keywords_the_checker_evaluates() {
    let schema = schema();
    assert!(
        schema["$schema"].as_str().is_some_and(|s| s.ends_with("2020-12/schema")),
        "the schema must name draft 2020-12"
    );
    assert!(schema.get("$id").is_none(), "an $id would give an editor an address to resolve");
    let properties = schema["properties"].as_object().expect("properties");
    let names: Vec<&str> = properties.keys().map(String::as_str).collect();
    assert_eq!(names, ["break_min", "id", "items", "kind", "title", "version", "work_min"]);
    assert_eq!(schema["required"], json!(["title", "kind"]));
    assert_eq!(schema["additionalProperties"], json!(false));
    assert_eq!(schema["$defs"].as_object().expect("$defs").keys().collect::<Vec<_>>(), ["item", "link", "listItem", "text"]);
    for (name, property) in properties.iter().chain(schema["$defs"]["item"]["properties"].as_object().expect("item properties")) {
        assert!(property["description"].as_str().is_some_and(|d| !d.is_empty()), "the member {name} has no description");
    }

    let mut patterns = Vec::new();
    patterns_in(schema, "#", &mut patterns);
    assert_eq!(patterns.len(), 4, "the file has four patterns: {patterns:?}");
    let escape = format!("{}u", backslash());
    for (at, pattern) in &patterns {
        assert!(pattern.is_ascii(), "{at}: a pattern is ASCII");
        assert!(!pattern.contains("(?"), "{at}: no group opener");
        let chars: Vec<char> = pattern.chars().collect();
        for pair in chars.windows(2) {
            let is_shorthand = pair[0] == backslash() && "sSwWdDbBpPk".contains(pair[1]);
            let is_reference = pair[0] == backslash() && pair[1].is_ascii_digit();
            assert!(!is_shorthand && !is_reference, "{at}: a shorthand class or a back-reference is not portable");
        }
        if at.ends_with("/properties/id/pattern") {
            assert!(!pattern.contains(backslash()), "{at}: the id pattern needs no escape");
        } else {
            assert!(pattern.contains(&escape), "{at}: a class pattern spells its characters as code point escapes");
        }
    }

    // A keyword nobody evaluates would be silently ignored by this test and honoured by an editor.
    let mut refs = Vec::new();
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
    refs_in(schema, &mut refs);
    assert!(refs.len() >= 4, "the file uses references: {refs:?}");
    for reference in &refs {
        let name = reference.strip_prefix("#/$defs/").unwrap_or_else(|| panic!("{reference} is not a local $defs reference"));
        assert!(schema["$defs"].get(name).is_some(), "{reference} names no $defs entry");
    }
}

fn evaluate_one(schema: &Value, instance: &Value) -> Outcome {
    evaluate(schema, schema, instance)
}

#[test]
fn the_checker_evaluates_every_keyword_and_can_reject() {
    let defs = json!({ "$defs": { "n": { "type": "integer" } } });
    let with_defs = |extra: Value| {
        let mut base = defs.clone();
        base.as_object_mut().expect("object").extend(extra.as_object().expect("object").clone());
        base
    };
    // (schema, an instance it accepts, an instance it refuses)
    let rows: Vec<(Value, Value, Value)> = vec![
        (json!({ "type": "string" }), json!("a"), json!(1)),
        (json!({ "type": ["string", "null"] }), json!(null), json!(1)),
        (json!({ "type": "integer" }), json!(3), json!(1.0)),
        (json!({ "type": "integer" }), json!(-3), json!("3")),
        (json!({ "type": "number" }), json!(1.5), json!("x")),
        (json!({ "type": "boolean" }), json!(false), json!(0)),
        (json!({ "type": "object" }), json!({}), json!([])),
        (json!({ "type": "array" }), json!([]), json!({})),
        (json!({ "type": "null" }), json!(null), json!(false)),
        (json!({ "enum": ["a", 1, null] }), json!(1), json!(2)),
        (json!({ "enum": [1] }), json!(1), json!(1.0)),
        (json!({ "const": "timer" }), json!("timer"), json!("Timer")),
        (json!({ "minimum": 1 }), json!(1), json!(0)),
        (json!({ "minimum": 0 }), json!(0), json!(-1)),
        (json!({ "maximum": 5 }), json!(5), json!(6)),
        (json!({ "maximum": 4294967295u64 }), json!(4294967295u64), json!(4294967296u64)),
        (json!({ "minLength": 2 }), json!("ab"), json!("a")),
        (json!({ "maxLength": 1 }), json!("\u{1F600}"), json!("ab")),
        (json!({ "pattern": "^a+$" }), json!("aa"), json!("ab")),
        (json!({ "pattern": "b" }), json!("abc"), json!("xyz")),
        (json!({ "minItems": 1 }), json!([1]), json!([])),
        (json!({ "maxItems": 1 }), json!([1]), json!([1, 2])),
        (json!({ "items": { "type": "integer" } }), json!([1, 2]), json!([1, "x"])),
        (json!({ "required": ["a"] }), json!({ "a": null }), json!({})),
        (json!({ "properties": { "a": { "type": "integer" } } }), json!({ "a": 1, "b": "x" }), json!({ "a": "x" })),
        (json!({ "properties": { "a": {} }, "additionalProperties": false }), json!({ "a": 1 }), json!({ "a": 1, "b": 2 })),
        (json!({ "additionalProperties": { "type": "integer" } }), json!({ "b": 1 }), json!({ "b": "x" })),
        (json!({ "allOf": [{ "minimum": 1 }, { "maximum": 3 }] }), json!(2), json!(4)),
        (json!({ "oneOf": [{ "type": "integer" }, { "minimum": 0 }] }), json!(-1), json!(1)),
        (json!({ "oneOf": [{ "type": "string" }, { "type": "integer" }] }), json!(1), json!(null)),
        (json!({ "if": { "const": 1 }, "then": { "minimum": 5 } }), json!(7), json!(1)),
        (json!({ "if": { "const": 1 }, "else": { "minimum": 5 } }), json!(1), json!(2)),
        (with_defs(json!({ "$ref": "#/$defs/n" })), json!(1), json!("x")),
        (with_defs(json!({ "$ref": "#/$defs/n", "maximum": 3 })), json!(2), json!(4)),
    ];
    assert!(rows.len() >= 24);
    for (schema, accepted, refused) in &rows {
        assert_eq!(evaluate_one(schema, accepted), Ok(None), "{schema} must accept {accepted}");
        assert!(matches!(evaluate_one(schema, refused), Ok(Some(_))), "{schema} must refuse {refused}");
    }

    // A keyword applies only to the instance types it is defined for.
    for (schema, other) in [
        (json!({ "minimum": 5 }), json!("x")),
        (json!({ "minLength": 5 }), json!(1)),
        (json!({ "pattern": "^z$" }), json!(1)),
        (json!({ "minItems": 5 }), json!("x")),
        (json!({ "required": ["a"] }), json!([])),
        (json!({ "properties": { "a": { "type": "null" } } }), json!([1])),
        (json!({ "additionalProperties": false }), json!("x")),
    ] {
        assert_eq!(evaluate_one(&schema, &other), Ok(None), "{schema} says nothing about {other}");
    }
    // `if` without `then` and `else` decides nothing; `then` without `if` is ignored.
    for instance in [json!(1), json!(2)] {
        assert_eq!(evaluate_one(&json!({ "if": { "const": 1 } }), &instance), Ok(None));
        assert_eq!(evaluate_one(&json!({ "then": { "const": 9 } }), &instance), Ok(None));
    }
    // The failure names the keyword.
    let failing = evaluate_one(&json!({ "properties": { "a": { "maxLength": 1 } } }), &json!({ "a": "ab" }));
    assert_eq!(failing, Ok(Some("#/properties/a/maxLength".to_owned())));
    // A cycle of references ends.
    let cycle = json!({ "$defs": { "a": { "$ref": "#/$defs/a" } }, "$ref": "#/$defs/a" });
    assert!(matches!(evaluate_one(&cycle, &json!(1)), Err(Unsupported(_))));
}

#[test]
fn the_checker_refuses_a_keyword_it_does_not_evaluate() {
    let names = [
        "format",
        "not",
        "anyOf",
        "patternProperties",
        "prefixItems",
        "uniqueItems",
        "dependentRequired",
        "dependentSchemas",
        "unevaluatedProperties",
        "unevaluatedItems",
        "$id",
        "$anchor",
        "$dynamicRef",
        "$dynamicAnchor",
        "$vocabulary",
        "contains",
        "propertyNames",
        "multipleOf",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "minProperties",
        "maxProperties",
        "x-vendor",
    ];
    for name in names {
        let one_line = json!({ name: "x" });
        assert!(matches!(evaluate_one(&one_line, &json!("x")), Err(Unsupported(_))), "{name} must not be silently accepted");
        // In branches that no instance reaches.
        let unreached = [
            json!({ "if": { "const": 1 }, "then": { name: "x" } }),
            json!({ "properties": { "a": { name: "x" } } }),
            json!({ "oneOf": [{ "type": "string" }, { "allOf": [{ name: "x" }] }] }),
            json!({ "$defs": { "never": { name: "x" } } }),
            json!({ "additionalProperties": { name: "x" } }),
            json!({ "items": { "items": { name: "x" } } }),
        ];
        for schema in unreached {
            assert!(matches!(evaluate_one(&schema, &json!(2)), Err(Unsupported(_))), "{name} in {schema} must not be skipped");
        }
    }
    for reference in ["https://example.org/x.json", "#/$defs/missing", "#", "#/properties/a", "#/$defs/", "#/$defs/a/b", "other.json#/$defs/a"] {
        let schema = json!({ "$defs": { "a": {} }, "properties": { "a": {} }, "$ref": reference });
        assert!(matches!(evaluate_one(&schema, &json!(1)), Err(Unsupported(_))), "{reference} is not a local $defs entry");
    }
    assert!(matches!(evaluate_one(&json!({ "type": "decimal" }), &json!(1)), Err(Unsupported(_))));
    assert!(matches!(evaluate_one(&json!({ "pattern": "(" }), &json!("x")), Err(Unsupported(_))));
}

// ---------------------------------------------------------------------------------------------------------------------
// The tracer: one list widget family.
// ---------------------------------------------------------------------------------------------------------------------

fn list_of(items: Value) -> Value {
    json!({ "title": "Deploy", "kind": "list", "items": items })
}

fn two_items() -> Value {
    json!([{ "label": "Pods", "command": "kubectl get pods" }, { "label": "Docs", "url": "https://example.org/docs" }])
}

#[test]
fn a_list_widget_gets_the_same_verdict_from_the_schema_and_the_loader() {
    let base = list_of(two_items());
    assert!(same_verdict("a list with a command and a link", &base.to_string()), "the loader must accept the base list");

    let mut with_extra = base.clone();
    with_extra["extra"] = json!(1);
    let mut item_extra = base.clone();
    item_extra["items"][0]["extra"] = json!(true);
    let mut both = base.clone();
    both["items"][0]["url"] = json!("https://example.org");
    let mut neither = base.clone();
    neither["items"][0] = json!({ "label": "Pods" });
    let mut newline = base.clone();
    newline["items"][0]["command"] = json!("ls\nrm -rf /");
    let mut scheme = base.clone();
    scheme["items"][1]["url"] = json!("javascript:alert(1)");
    let long_title = json!({ "title": "x".repeat(41), "kind": "list", "items": two_items() });

    let refused = [
        ("an unknown member at the root", with_extra),
        ("an unknown member in an item", item_extra),
        ("an item with a command and a link", both),
        ("an item with neither", neither),
        ("a command with a line break", newline),
        ("a link with the javascript scheme", scheme),
        ("an empty list", list_of(json!([]))),
        ("a title of 41 characters", long_title),
    ];
    for (label, document) in refused {
        let text = document.to_string();
        assert!(!same_verdict(label, &text), "{label} must be refused by both sides: {text}");
    }
}

#[test]
fn config_md_links_the_schema_from_the_widget_section() {
    let doc = read(CONFIG_PATH);
    let start = doc.find(WIDGET_HEADING).unwrap_or_else(|| panic!("{CONFIG_PATH} has no section {WIDGET_HEADING}"));
    let rest = &doc[start + WIDGET_HEADING.len()..];
    let section = rest.split("\n## ").next().expect("a section");
    assert!(section.contains("(widget.schema.json)"), "the widget section must link the schema");
    assert!(root().join(SCHEMA_PATH).is_file(), "{SCHEMA_PATH} must exist");
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
    "shape", "arrays", "required", "unknown", "kind", "version", "id", "title", "timer", "foreign", "list", "label", "command",
    "url",
];

/// A Unicode scalar value from its number, so that no special character is typed into this file.
fn ch(code_point: u32) -> char {
    char::from_u32(code_point).expect("a Unicode scalar value")
}

fn rep(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

fn note(title: &str) -> Value {
    json!({ "title": title, "kind": "note" })
}

fn timer() -> Value {
    json!({ "title": "Focus", "kind": "timer" })
}

fn with(mut base: Value, key: &str, value: Value) -> Value {
    base[key] = value;
    base
}

fn one_item(item: Value) -> Value {
    list_of(json!([item]))
}

fn command_item(command: &str) -> Value {
    json!({ "label": "Run", "command": command })
}

fn url_item(url: &str) -> Value {
    json!({ "label": "Open", "url": url })
}

fn n_items(n: usize) -> Value {
    list_of(Value::Array((0..n).map(|i| command_item(&format!("echo {i}"))).collect()))
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
    c.text("shape", "a string", "\"note\"");
    c.text("shape", "an empty array", "[]");
    c.text("shape", "text after the object", format!("{} x", note("Notes")));
    c.text("shape", "a byte order mark before the object", format!("{}{}", ch(0xfeff), note("Notes")));
    c.text("shape", "the smallest note", r#"{"title":"T","kind":"note"}"#);

    // The array forms: serde reads a struct from an array of its member values, and nothing documents that.
    c.text("arrays", "a widget as an array of member values", r#"[null,null,"Notes","note"]"#);
    c.json("arrays", "an item as an array", &list_of(json!([["Pods", "kubectl get pods", null]])));
    c.json("arrays", "an item array inside a note", &with(note("Notes"), "items", json!([["Pods"]])));

    c.text("required", "no title", r#"{"kind":"note"}"#);
    c.text("required", "no kind", r#"{"title":"T"}"#);
    c.text("required", "an empty object", "{}");

    for (name, key) in [("extra", "extra"), ("comment", "comment"), ("a schema member", "$schema"), ("an upper-case name", "Title")] {
        c.json("unknown", &format!("{name} at the root"), &with(note("Notes"), key, json!("x")));
    }
    for (name, key) in [("extra", "extra"), ("an upper-case name", "Label")] {
        let mut item = command_item("ls");
        item[key] = json!("x");
        c.json("unknown", &format!("{name} in an item"), &one_item(item));
    }

    for kind in ["note", "timer"] {
        c.json("kind", &format!("{kind}"), &json!({ "title": "T", "kind": kind }));
    }
    c.json("kind", "list", &n_items(1));
    for (name, kind) in [
        ("Note", json!("Note")),
        ("script", json!("script")),
        ("an empty string", json!("")),
        ("a number", json!(1)),
        ("null", json!(null)),
        ("an array", json!(["note"])),
    ] {
        c.json("kind", &format!("the kind is {name}"), &json!({ "title": "T", "kind": kind }));
    }

    c.json("version", "absent", &note("T"));
    for (name, version) in [
        ("1", json!(1)),
        ("null", json!(null)),
        ("0", json!(0)),
        ("2", json!(2)),
        ("the largest u32", json!(4294967295u64)),
        ("one more than the largest u32", json!(4294967296u64)),
        ("-1", json!(-1)),
        ("1.0", json!(1.0)),
        ("the string 1", json!("1")),
        ("true", json!(true)),
    ] {
        c.json("version", &format!("the version is {name}"), &with(note("T"), "version", version));
    }

    let id_set = format!("{}zz", "a-b_c0".repeat(5));
    c.json("id", "absent", &note("T"));
    for (name, id) in [
        ("null", json!(null)),
        ("a", json!("a")),
        ("32 characters of the allowed set", json!(id_set)),
        ("33 characters", json!(format!("{id_set}a"))),
        ("empty", json!("")),
        ("upper case", json!("Ab")),
        ("with a space", json!("a b")),
        ("a path", json!("../x")),
        ("with a dot", json!("a.b")),
        ("a non-ASCII letter", json!(format!("a{}", ch(0xe9)))),
        ("a number", json!(5)),
        ("a trailing line feed", json!("a\n")),
    ] {
        c.json("id", &format!("the id is {name}"), &with(note("T"), "id", id));
    }

    let controls = [("NUL", 0u32), ("a line feed", 0xa), ("DEL", 0x7f), ("U+0085", 0x85)];
    for (name, title) in [
        ("1 character", json!("T")),
        ("40 characters", json!(rep('x', 40))),
        ("40 two-byte characters", json!(rep(ch(0xe9), 40))),
        ("40 four-byte characters", json!(rep(ch(0x1f600), 40))),
        ("41 characters", json!(rep('x', 41))),
        ("41 four-byte characters", json!(rep(ch(0x1f600), 41))),
        ("empty", json!("")),
        ("spaces only", json!("   ")),
        ("U+00A0 only", json!(ch(0xa0).to_string())),
        ("a tab only", json!("\t")),
        ("a trailing line feed", json!("T\n")),
        ("a number", json!(1)),
        ("null", json!(null)),
    ] {
        c.json("title", &format!("the title is {name}"), &json!({ "title": title, "kind": "note" }));
    }
    for (name, code_point) in controls {
        c.json("title", &format!("the title holds {name}"), &note(&format!("a{}b", ch(code_point))));
    }

    c.json("timer", "no members", &timer());
    c.json("timer", "null minutes", &with(with(timer(), "work_min", json!(null)), "break_min", json!(null)));
    for (name, member, value) in [
        ("work 1", "work_min", json!(1)),
        ("work 180", "work_min", json!(180)),
        ("break 0", "break_min", json!(0)),
        ("break 60", "break_min", json!(60)),
        ("work 0", "work_min", json!(0)),
        ("work 181", "work_min", json!(181)),
        ("break 61", "break_min", json!(61)),
        ("work -1", "work_min", json!(-1)),
        ("break -1", "break_min", json!(-1)),
        ("work 25.0", "work_min", json!(25.0)),
        ("work 4294967296", "work_min", json!(4294967296u64)),
        ("work as a string", "work_min", json!("25")),
        ("break true", "break_min", json!(true)),
    ] {
        c.json("timer", name, &with(timer(), member, value));
    }
    c.text("timer", "work 1e1", r#"{"title":"T","kind":"timer","work_min":1e1}"#);
    c.text("timer", "work -0", r#"{"title":"T","kind":"timer","work_min":-0}"#);

    // Members that belong to another kind are accepted as the loader accepts them, and still shape-checked.
    c.json("foreign", "a note with work_min 0", &with(note("N"), "work_min", json!(0)));
    c.json("foreign", "a note with break_min 4294967295", &with(note("N"), "break_min", json!(4294967295u64)));
    c.json("foreign", "a note with well-formed items", &with(note("N"), "items", two_items()));
    c.json("foreign", "a note with an item that has a command and a url", &with(
        note("N"),
        "items",
        json!([{ "label": "a", "command": "ls", "url": "https://x.y" }]),
    ));
    c.json("foreign", "a note with an empty list of items", &with(note("N"), "items", json!([])));
    c.json("foreign", "a timer with items", &with(timer(), "items", two_items()));
    c.json("foreign", "a list with work_min 0", &with(n_items(1), "work_min", json!(0)));
    c.json("foreign", "a note with work_min 4294967296", &with(note("N"), "work_min", json!(4294967296u64)));
    c.json("foreign", "a note whose item has an unknown member", &with(note("N"), "items", json!([{ "label": "a", "x": 1 }])));
    c.json("foreign", "a note whose item is a number", &with(note("N"), "items", json!([1])));
    c.json("foreign", "a note with null items", &with(note("N"), "items", json!(null)));
    c.json("foreign", "a note with work_min as a string", &with(note("N"), "work_min", json!("x")));

    c.json("list", "no items", &json!({ "title": "T", "kind": "list" }));
    c.json("list", "null items", &with(n_items(1), "items", json!(null)));
    c.json("list", "an empty list", &list_of(json!([])));
    c.json("list", "1 item", &n_items(1));
    c.json("list", "50 items", &n_items(50));
    c.json("list", "51 items", &n_items(51));
    c.json("list", "items as a string", &with(n_items(1), "items", json!("x")));
    c.json("list", "items as an object", &with(n_items(1), "items", json!({ "label": "a", "command": "ls" })));
    c.json("list", "a null element", &list_of(json!([null])));
    c.json("list", "a number element", &list_of(json!([1])));
    c.json("list", "a string element", &list_of(json!(["x"])));
    c.json("list", "a good item after a bad one", &list_of(json!([{ "label": "a" }, command_item("ls")])));

    for (name, label) in [
        ("1 character", json!("a")),
        ("60 characters", json!(rep('x', 60))),
        ("60 four-byte characters", json!(rep(ch(0x1f600), 60))),
        ("61 characters", json!(rep('x', 61))),
        ("empty", json!("")),
        ("blank", json!("  ")),
        ("a control character", json!("a\u{1}b")),
        ("a line feed", json!("a\nb")),
        ("a number", json!(1)),
        ("null", json!(null)),
    ] {
        c.json("label", &format!("the label is {name}"), &one_item(json!({ "label": label, "command": "ls" })));
    }
    c.json("label", "the label is missing", &one_item(json!({ "command": "ls" })));

    for (name, command) in [
        ("plain", "kubectl get pods".to_owned()),
        ("1024 characters", rep('x', 1024)),
        ("1025 characters", rep('x', 1025)),
        ("a line feed", "ls\nrm -rf /".to_owned()),
        ("a carriage return", "ls\rrm".to_owned()),
        ("a tab", "ls\tx".to_owned()),
        ("NUL", format!("ls{}x", ch(0))),
        ("ESC", format!("ls{}[2J", ch(0x1b))),
        ("DEL", format!("ls{}x", ch(0x7f))),
        ("U+0085", format!("ls{}x", ch(0x85))),
        ("blank", "   ".to_owned()),
        ("empty", String::new()),
    ] {
        c.json("command", &format!("the command is {name}"), &one_item(command_item(&command)));
    }
    c.json("command", "a null command with a valid url", &one_item(json!({ "label": "a", "command": null, "url": "https://x.y" })));
    c.json("command", "a null url with a valid command", &one_item(json!({ "label": "a", "command": "ls", "url": null })));
    c.json("command", "a command and a url", &one_item(json!({ "label": "a", "command": "ls", "url": "https://x.y" })));
    c.json("command", "both null", &one_item(json!({ "label": "a", "command": null, "url": null })));
    c.json("command", "neither", &one_item(json!({ "label": "a" })));
    c.json("command", "a command that is a number", &one_item(json!({ "label": "a", "command": 1 })));

    let long = |n: usize| format!("https://{}", rep('a', n - 8));
    let spaced = |c: u32| format!("https://a{}b", ch(c));
    for (name, url) in [
        ("https", "https://example.org/docs".to_owned()),
        ("http", "http://example.org".to_owned()),
        ("a query and a fragment", "https://example.org/a?b=c&d=e#f".to_owned()),
        ("user info and a port", "https://user:pw@example.org:8080/p".to_owned()),
        ("a non-ASCII host", format!("https://m{}nchen.example/", ch(0xfc))),
        ("2048 characters", long(2048)),
        ("2049 characters", long(2049)),
        ("an upper-case scheme", "HTTPS://example.org".to_owned()),
        ("ftp", "ftp://example.org".to_owned()),
        ("javascript", "javascript:alert(1)".to_owned()),
        ("data", "data:text/plain,x".to_owned()),
        ("file", "file:///etc/passwd".to_owned()),
        ("mailto", "mailto:a@b.c".to_owned()),
        ("a bare scheme", "https://".to_owned()),
        ("a third slash", "https:///x".to_owned()),
        ("a space", "https://a b".to_owned()),
        ("U+00A0", spaced(0xa0)),
        ("U+2028", spaced(0x2028)),
        ("U+3000", spaced(0x3000)),
        ("a control character", spaced(7)),
        ("a leading space", " https://example.org".to_owned()),
        ("a trailing line feed", "https://example.org\n".to_owned()),
        ("empty", String::new()),
    ] {
        c.json("url", &format!("the url is {name}"), &one_item(url_item(&url)));
    }
    c.json("url", "the url is a number", &one_item(json!({ "label": "a", "url": 7 })));
    c.0
}

#[test]
fn the_corpus_is_not_thin() {
    let cases = corpus();
    let accepted = cases.iter().filter(|case| loader_verdict(&case.text).is_ok()).count();
    let refused = cases.len() - accepted;
    assert!(cases.len() >= 120, "the corpus holds {} documents", cases.len());
    assert!(accepted >= 35, "the loader accepts only {accepted} documents of the corpus");
    assert!(refused >= 70, "the loader refuses only {refused} documents of the corpus");
    for category in CATEGORIES {
        let count = cases.iter().filter(|case| case.category == *category).count();
        eprintln!("corpus {category}: {count}");
        assert!(count >= 3, "the category {category} holds {count} documents");
    }
    eprintln!("corpus: {} documents, {accepted} accepted, {refused} refused by the loader", cases.len());
    assert!(cases.iter().all(|case| CATEGORIES.contains(&case.category)), "a case names a category that is not listed");
    let names: HashSet<String> = cases.iter().map(|case| format!("{}/{}", case.category, case.name)).collect();
    assert_eq!(names.len(), cases.len(), "two cases of one category share a name");
}

#[test]
fn every_corpus_document_gets_the_same_verdict_from_the_schema_and_the_loader() {
    for case in corpus() {
        same_verdict(&format!("{}: {}", case.category, case.name), &case.text);
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// One-step mutations of every valid document.
// ---------------------------------------------------------------------------------------------------------------------

fn probes() -> Vec<Value> {
    vec![json!(null), json!(true), json!(0), json!(-1), json!(1.5), json!(""), json!("x"), json!([]), json!({})]
}

/// Copies of `object` with one member removed, one member replaced by each probe, or the member `extra` added.
fn edits(object: &Map<String, Value>) -> Vec<(String, Map<String, Value>)> {
    let mut out = Vec::new();
    for key in object.keys() {
        let mut without = object.clone();
        without.remove(key);
        out.push((format!("{key} removed"), without));
        for probe in probes() {
            let mut replaced = object.clone();
            replaced.insert(key.clone(), probe.clone());
            out.push((format!("{key} replaced by {probe}"), replaced));
        }
    }
    let mut extended = object.clone();
    extended.insert("extra".to_owned(), json!(1));
    out.push(("extra added".to_owned(), extended));
    out
}

/// Every one-step mutation of `document` at the root and in every item.
fn mutants(document: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let Some(root) = document.as_object() else { return out };
    out.extend(edits(root).into_iter().map(|(what, map)| (format!("root: {what}"), Value::Object(map))));
    let Some(items) = root.get("items").and_then(Value::as_array) else { return out };
    for (n, item) in items.iter().enumerate() {
        let mut put = |what: String, replacement: Option<Value>| {
            let mut copy = items.clone();
            match replacement {
                Some(value) => copy[n] = value,
                None => {
                    copy.remove(n);
                }
            }
            let mut changed = root.clone();
            changed.insert("items".to_owned(), Value::Array(copy));
            out.push((format!("item {n}: {what}"), Value::Object(changed)));
        };
        put("removed".to_owned(), None);
        for probe in probes() {
            put(format!("replaced by {probe}"), Some(probe));
        }
        if let Some(members) = item.as_object() {
            for (what, map) in edits(members) {
                put(what, Some(Value::Object(map)));
            }
        }
    }
    out
}

#[test]
fn every_one_step_mutation_of_a_valid_document_gets_the_same_verdict() {
    let mut seen = HashSet::new();
    let (mut checked, mut accepted) = (0usize, 0usize);
    for case in corpus() {
        let Ok(document) = serde_json::from_str::<Value>(&case.text) else { continue };
        if loader_verdict(&case.text).is_err() {
            continue;
        }
        for (what, mutant) in mutants(&document) {
            let text = mutant.to_string();
            if !seen.insert(text.clone()) {
                continue;
            }
            checked += 1;
            accepted += usize::from(same_verdict(&format!("{} / {what}", case.name), &text));
        }
    }
    eprintln!("mutants checked: {checked}, accepted by both: {accepted}");
    assert!(checked >= 2000, "only {checked} mutants were checked");
    assert!(accepted >= 100, "only {accepted} mutants were accepted, so the sweep rarely leaves the valid side");
}

// ---------------------------------------------------------------------------------------------------------------------
// Bounds: every number the schema states is the loader's number.
// ---------------------------------------------------------------------------------------------------------------------

struct Bound {
    name: &'static str,
    pointer: &'static str,
    /// An upper bound is tested at n and n plus one, a lower bound at n and n minus one.
    upper: bool,
    /// A text bound counts characters: it is also tested with two-byte and four-byte characters.
    wide: bool,
    /// The document whose measured value has size n (n is minus one for a lower bound of zero).
    build: fn(i64, char) -> Value,
}

fn text_of(n: i64, fill: char) -> String {
    rep(fill, usize::try_from(n).expect("a text size is not negative"))
}

fn count_of(n: i64) -> usize {
    usize::try_from(n).expect("a count is not negative")
}

const BOUNDS: &[Bound] = &[
    Bound {
        name: "title length",
        pointer: "/properties/title/maxLength",
        upper: true,
        wide: true,
        build: |n, f| note(&text_of(n, f)),
    },
    Bound { name: "text minimum", pointer: "/$defs/text/minLength", upper: false, wide: true, build: |n, f| note(&text_of(n, f)) },
    Bound {
        name: "id maximum",
        pointer: "/properties/id/maxLength",
        upper: true,
        wide: false,
        build: |n, _| with(note("T"), "id", json!(text_of(n, 'a'))),
    },
    Bound {
        name: "id minimum",
        pointer: "/properties/id/minLength",
        upper: false,
        wide: false,
        build: |n, _| with(note("T"), "id", json!(text_of(n, 'a'))),
    },
    Bound {
        name: "label length",
        pointer: "/$defs/listItem/properties/label/maxLength",
        upper: true,
        wide: true,
        build: |n, f| one_item(json!({ "label": text_of(n, f), "command": "ls" })),
    },
    Bound {
        name: "command length",
        pointer: "/$defs/listItem/oneOf/0/properties/command/maxLength",
        upper: true,
        wide: true,
        build: |n, f| one_item(command_item(&text_of(n, f))),
    },
    Bound {
        name: "link length (ASCII, so characters are bytes)",
        pointer: "/$defs/link/maxLength",
        upper: true,
        wide: false,
        build: |n, _| one_item(url_item(&format!("https://{}", text_of(n - 8, 'a')))),
    },
    Bound {
        name: "least items",
        pointer: "/allOf/1/then/properties/items/minItems",
        upper: false,
        wide: false,
        build: |n, _| n_items(count_of(n)),
    },
    Bound {
        name: "most items",
        pointer: "/allOf/1/then/properties/items/maxItems",
        upper: true,
        wide: false,
        build: |n, _| n_items(count_of(n)),
    },
    Bound {
        name: "timer work minimum",
        pointer: "/allOf/0/then/properties/work_min/minimum",
        upper: false,
        wide: false,
        build: |n, _| with(timer(), "work_min", json!(n)),
    },
    Bound {
        name: "timer work maximum",
        pointer: "/allOf/0/then/properties/work_min/maximum",
        upper: true,
        wide: false,
        build: |n, _| with(timer(), "work_min", json!(n)),
    },
    Bound {
        name: "timer break maximum",
        pointer: "/allOf/0/then/properties/break_min/maximum",
        upper: true,
        wide: false,
        build: |n, _| with(timer(), "break_min", json!(n)),
    },
    Bound {
        name: "work_min minimum on a note",
        pointer: "/properties/work_min/minimum",
        upper: false,
        wide: false,
        build: |n, _| with(note("T"), "work_min", json!(n)),
    },
    Bound {
        name: "break_min minimum on a note",
        pointer: "/properties/break_min/minimum",
        upper: false,
        wide: false,
        build: |n, _| with(note("T"), "break_min", json!(n)),
    },
    Bound {
        name: "work_min maximum on a note",
        pointer: "/properties/work_min/maximum",
        upper: true,
        wide: false,
        build: |n, _| with(note("T"), "work_min", json!(n)),
    },
    Bound {
        name: "break_min maximum on a note",
        pointer: "/properties/break_min/maximum",
        upper: true,
        wide: false,
        build: |n, _| with(note("T"), "break_min", json!(n)),
    },
];

/// The whole number at `pointer` of the schema file.
fn pointer_number(pointer: &str) -> u64 {
    schema().pointer(pointer).and_then(Value::as_u64).unwrap_or_else(|| panic!("{SCHEMA_PATH} has no number at {pointer}"))
}

#[test]
fn every_bound_of_the_schema_is_a_bound_of_the_loader() {
    for bound in BOUNDS {
        let n = i64::try_from(pointer_number(bound.pointer)).expect("a bound fits an i64");
        let beyond = if bound.upper { n + 1 } else { n - 1 };
        let fills: &[char] = if bound.wide { &['a', '\u{e9}', '\u{1f600}'] } else { &['a'] };
        for fill in fills {
            let inside = (bound.build)(n, *fill);
            let outside = (bound.build)(beyond, *fill);
            let label = format!("{} ({}), filled with U+{:04X}", bound.name, bound.pointer, u32::from(*fill));
            assert!(same_verdict(&format!("{label} at {n}"), &inside.to_string()), "{label}: {n} must be accepted");
            assert!(!same_verdict(&format!("{label} at {beyond}"), &outside.to_string()), "{label}: {beyond} must be refused");
        }
    }
    // The rows cover every number that the schema file states.
    let mut numbers = Vec::new();
    fn numbers_in(value: &Value, at: &str, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map {
                    let here = format!("{at}/{key}");
                    match (key.as_str(), inner) {
                        ("minLength" | "maxLength" | "minimum" | "maximum" | "minItems" | "maxItems", Value::Number(_)) => {
                            out.push(here);
                        }
                        // A default is a hint for editors; it is tested against the loader in its own test.
                        ("default" | "enum", _) => {}
                        _ => numbers_in(inner, &here, out),
                    }
                }
            }
            Value::Array(list) => list.iter().enumerate().for_each(|(n, inner)| numbers_in(inner, &format!("{at}/{n}"), out)),
            _ => {}
        }
    }
    numbers_in(schema(), "", &mut numbers);
    let covered: BTreeSet<&str> = BOUNDS.iter().map(|b| b.pointer).collect();
    for number in &numbers {
        assert!(covered.contains(number.as_str()), "the schema states a number at {number} that no bound row tests");
    }
    for pointer in covered {
        assert!(numbers.iter().any(|n| n == pointer), "the bound row {pointer} names a number the schema does not state");
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The character classes.
// ---------------------------------------------------------------------------------------------------------------------

fn class_patterns() -> (Arc<Regex>, Arc<Regex>, Arc<Regex>) {
    let text = |pointer: &str| {
        let pattern = schema().pointer(pointer).and_then(Value::as_str).unwrap_or_else(|| panic!("no pattern at {pointer}"));
        regex(pattern).unwrap_or_else(|e| panic!("{pointer}: {e:?}"))
    };
    (text("/$defs/text/allOf/0/pattern"), text("/$defs/text/allOf/1/pattern"), text("/$defs/link/pattern"))
}

#[test]
fn character_classes_of_the_schema_match_the_character_predicates_for_every_scalar_value() {
    let (no_control, some_non_blank, link) = class_patterns();
    let mut checked = 0u32;
    let (mut alone, mut after_scheme, mut inside) = (String::new(), String::new(), String::new());
    for code_point in 0..=0x10_ffffu32 {
        let Some(c) = char::from_u32(code_point) else { continue };
        checked += 1;
        alone.clear();
        alone.push(c);
        assert_eq!(no_control.is_match(&alone), !c.is_control(), "the control class and is_control differ at U+{code_point:04X}");
        assert_eq!(some_non_blank.is_match(&alone), !c.is_whitespace(), "the blank class and is_whitespace differ at U+{code_point:04X}");
        after_scheme.clear();
        after_scheme.push_str("https://");
        after_scheme.push(c);
        let first_ok = c != '/' && !c.is_whitespace() && !c.is_control();
        assert_eq!(link.is_match(&after_scheme), first_ok, "the link pattern differs from the loader as a first character at U+{code_point:04X}");
        inside.clear();
        inside.push_str("https://a");
        inside.push(c);
        inside.push('b');
        let later_ok = !c.is_whitespace() && !c.is_control();
        assert_eq!(link.is_match(&inside), later_ok, "the link pattern differs from the loader inside a link at U+{code_point:04X}");
    }
    assert_eq!(checked, 1_112_064, "the number of Unicode scalar values");
}

/// Each edge of a run of control or white-space characters, and its two neighbours.
fn edge_values() -> Vec<char> {
    let predicates: [fn(char) -> bool; 2] = [char::is_control, char::is_whitespace];
    let mut edges = BTreeSet::new();
    for predicate in predicates {
        let mut runs: Vec<(u32, u32)> = Vec::new();
        let mut start: Option<u32> = None;
        let mut last = 0;
        for code_point in 0..=0x10_ffffu32 {
            let Some(c) = char::from_u32(code_point) else { continue };
            if predicate(c) {
                start.get_or_insert(code_point);
                last = code_point;
            } else if let Some(s) = start.take() {
                runs.push((s, last));
            }
        }
        runs.extend(start.map(|s| (s, last)));
        for (s, e) in runs {
            for code_point in [s.wrapping_sub(1), s, s + 1, e.wrapping_sub(1), e, e + 1] {
                if let Some(c) = char::from_u32(code_point) {
                    edges.insert(c);
                }
            }
        }
    }
    edges.into_iter().collect()
}

#[test]
fn the_loader_and_the_schema_agree_at_every_edge_of_the_character_classes() {
    let edges = edge_values();
    assert!(edges.len() >= 40, "only {} edge values were found", edges.len());
    for c in &edges {
        let at = format!("U+{:04X}", u32::from(*c));
        same_verdict(&format!("a title around {at}"), &note(&format!("a{c}b")).to_string());
        same_verdict(&format!("a title of {at} only"), &note(&c.to_string()).to_string());
        same_verdict(&format!("a title that starts with {at}"), &note(&format!("{c}b")).to_string());
        same_verdict(&format!("a label around {at}"), &one_item(json!({ "label": format!("a{c}b"), "command": "ls" })).to_string());
        same_verdict(&format!("a command around {at}"), &one_item(command_item(&format!("l{c}s"))).to_string());
        same_verdict(&format!("a command of {at} only"), &one_item(command_item(&c.to_string())).to_string());
        same_verdict(&format!("a link around {at}"), &one_item(url_item(&format!("https://a{c}b"))).to_string());
        same_verdict(&format!("a link that starts with {at}"), &one_item(url_item(&format!("https://{c}b"))).to_string());
        same_verdict(&format!("a link that ends with {at}"), &one_item(url_item(&format!("https://a{c}"))).to_string());
    }
    // The edges include the characters the schema decides on its own.
    for needed in ['\u{8}', '\u{9}', '\u{d}', '\u{1f}', ' ', '\u{7f}', '\u{85}', '\u{a0}', '\u{3000}'] {
        assert!(edges.contains(&needed), "U+{:04X} is not among the edge values", u32::from(needed));
    }
}

#[test]
fn format_characters_are_accepted_by_both_and_stay_pinned() {
    // This pins today's behaviour and is not an endorsement. A bidirectional override or a zero-width character is not a
    // control character, so the loader accepts it and the schema describes it. Refusing format characters is a change of
    // the loader and a new version of the schema, and this test is where that change shows.
    for (name, c) in [("a right-to-left override", 0x202e), ("a zero-width space", 0x200b)] {
        let word = format!("a{}b", ch(c));
        assert!(same_verdict(&format!("a title with {name}"), &note(&word).to_string()), "{name} in a title");
        assert!(
            same_verdict(&format!("a label with {name}"), &one_item(json!({ "label": word, "command": "ls" })).to_string()),
            "{name} in a label"
        );
        assert!(same_verdict(&format!("a command with {name}"), &one_item(command_item(&word)).to_string()), "{name} in a command");
    }
    assert!(same_verdict("a title of the byte order mark alone", &note(&ch(0xfeff).to_string()).to_string()));
}

// ---------------------------------------------------------------------------------------------------------------------
// What the loader reads only as an object, and what a schema cannot say.
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn arrays_in_place_of_objects_are_refused_by_both() {
    let documents = [
        ("a widget as an array of its member values", r#"[null,null,"Notes","note"]"#.to_owned()),
        ("an item inside a list as an array", list_of(json!([["Pods", "kubectl get pods", null]])).to_string()),
        ("an item array inside a note", with(note("Notes"), "items", json!([["Pods"]])).to_string()),
    ];
    for (label, text) in documents {
        assert!(loader_verdict(&text).is_err(), "{label}: the loader must refuse it: {text}");
        assert!(schema_verdict(&text).is_err(), "{label}: the schema must refuse it: {text}");
    }
}

/// A document that the schema accepts and the loader refuses, because no JSON Schema keyword can state its rule.
struct Gap {
    name: &'static str,
    /// The words that the schema description and `docs/config.md` use for it.
    doc_phrase: &'static str,
    text: String,
    /// The file name stem the loader is given.
    stem: &'static str,
    why: &'static str,
}

static GAPS: LazyLock<Vec<Gap>> = LazyLock::new(|| {
    vec![
        Gap {
            name: "the same key twice",
            doc_phrase: "the same key twice",
            text: r#"{"title":"A","title":"B","kind":"note"}"#.to_owned(),
            stem: "widget",
            why: "serde refuses a repeated member; a parsed document has one value per key and cannot show it",
        },
        Gap {
            name: "a link that is longer in bytes than in characters",
            doc_phrase: "a link of more than 2048 bytes",
            text: one_item(url_item(&format!("https://a{}", rep(ch(0xe9), 2039)))).to_string(),
            stem: "widget",
            why: "the loader counts the bytes of a link and JSON Schema counts characters",
        },
        Gap {
            name: "an id taken from the file name",
            doc_phrase: "an id taken from the file name",
            text: note("Named by its file").to_string(),
            stem: "Bad Name",
            why: "a document without an id takes the file name, which is not part of the document",
        },
    ]
});

#[test]
fn each_gap_is_real_and_no_other_document_is_one() {
    let corpus_texts: HashSet<String> = corpus().into_iter().map(|case| case.text).collect();
    for gap in GAPS.iter() {
        assert!(!gap.doc_phrase.is_empty() && !gap.why.is_empty(), "the gap {} has no phrase or no reason", gap.name);
        assert!(schema_verdict(&gap.text).is_ok(), "the schema must accept the gap {}", gap.name);
        assert!(widgets::parse(&gap.text, gap.stem).is_err(), "the loader must refuse the gap {}", gap.name);
        assert!(!corpus_texts.contains(&gap.text), "the gap {} is also in the corpus", gap.name);
    }
    assert_eq!(GAPS.len(), 3, "a new gap needs its own sentence in the schema description and in docs/config.md");
}

#[test]
fn no_branch_declares_a_member_its_object_does_not_have() {
    let schema = schema();
    let names = |value: &Value| -> BTreeSet<String> {
        value.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default()
    };
    let root_members = names(&schema["properties"]);
    for (n, branch) in schema["allOf"].as_array().expect("allOf").iter().enumerate() {
        for member in names(&branch["then"]["properties"]) {
            assert!(root_members.contains(&member), "/allOf/{n}/then/properties names {member}, which the root does not declare");
        }
    }
    let item_members = names(&schema["$defs"]["item"]["properties"]);
    let list_item = &schema["$defs"]["listItem"];
    for member in names(&list_item["properties"]) {
        assert!(item_members.contains(&member), "/$defs/listItem/properties names {member}, which an item does not declare");
    }
    for (n, branch) in list_item["oneOf"].as_array().expect("oneOf").iter().enumerate() {
        for member in names(&branch["properties"]) {
            assert!(item_members.contains(&member), "/$defs/listItem/oneOf/{n} names {member}, which an item does not declare");
        }
    }
}
