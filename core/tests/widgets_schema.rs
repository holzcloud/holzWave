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

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use holzwave_core::widgets::{self, WidgetDef};
use regex::Regex;
use serde_json::{json, Number, Value};

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
