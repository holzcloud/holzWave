//! A subset evaluator of JSON Schema 2020-12 for the tests that hold a published schema equal to the type it describes.
//!
//! It is not a schema crate (the dependency budget of the project). It fails closed: a keyword it does not evaluate is
//! an error and never an accept, and the schema file is linted keyword by keyword before any verdict is read. Include
//! it from a test file with `#[path = "common/schema_check.rs"] mod schema_check;`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use regex::Regex;
use serde_json::{Number, Value};

/// The checker was asked to judge a schema that uses something it does not evaluate.
#[derive(Debug, PartialEq)]
pub struct Unsupported(pub String);

/// `Ok(None)` is valid, `Ok(Some(path))` is invalid and names the first failing keyword, `Err` is a schema the checker
/// cannot judge.
pub type Outcome = Result<Option<String>, Unsupported>;

/// Keywords that decide a verdict.
pub const EVALUATED: &[&str] = &[
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
pub const ANNOTATIONS: &[&str] = &["$schema", "$comment", "title", "description", "default", "examples", "$defs"];

pub const TYPE_NAMES: &[&str] = &["object", "array", "string", "integer", "number", "boolean", "null"];

/// The deepest chain of references the checker follows.
pub const MAX_DEPTH: usize = 32;

pub fn unsupported(at: &str, why: &str) -> Unsupported {
    Unsupported(format!("{at}: {why}"))
}

/// A compiled pattern, cached: the sweeps judge thousands of documents with the same few patterns.
pub fn regex(pattern: &str) -> Result<Arc<Regex>, Unsupported> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Regex>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = cache.get(pattern) {
        return Ok(Arc::clone(found));
    }
    let compiled = Regex::new(pattern)
        .map_err(|e| Unsupported(format!("the pattern {pattern:?} does not compile: {e}")))?;
    let compiled = Arc::new(compiled);
    cache.insert(pattern.to_owned(), Arc::clone(&compiled));
    Ok(compiled)
}

/// The `$defs` entry that a reference names. Only `#/$defs/<name>` is followed: no remote, no anchor, no other pointer.
pub fn resolve<'a>(root: &'a Value, reference: &Value, at: &str) -> Result<&'a Value, Unsupported> {
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
pub fn lint(root: &Value) -> Result<(), Unsupported> {
    walk(root, root, "#")
}

pub fn walk_map(root: &Value, value: &Value, at: &str) -> Result<(), Unsupported> {
    let map = value.as_object().ok_or_else(|| unsupported(at, "this must be an object of subschemas"))?;
    map.iter().try_for_each(|(name, sub)| walk(root, sub, &format!("{at}/{name}")))
}

pub fn walk_list(root: &Value, value: &Value, at: &str) -> Result<(), Unsupported> {
    let list = value.as_array().ok_or_else(|| unsupported(at, "this must be an array of subschemas"))?;
    list.iter().enumerate().try_for_each(|(n, sub)| walk(root, sub, &format!("{at}/{n}")))
}

pub fn walk(root: &Value, schema: &Value, at: &str) -> Result<(), Unsupported> {
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

pub fn as_i128(n: &Number) -> Option<i128> {
    n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from))
}

pub fn compare(a: &Number, b: &Number) -> std::cmp::Ordering {
    match (as_i128(a), as_i128(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        _ => {
            let (x, y) = (a.as_f64().expect("a JSON number has an f64"), b.as_f64().expect("a JSON number has an f64"));
            x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
        }
    }
}

pub fn type_matches(name: &str, instance: &Value) -> bool {
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

pub fn limit(value: &Value, at: &str) -> Result<u64, Unsupported> {
    value.as_u64().ok_or_else(|| unsupported(at, "a limit must be a whole number"))
}

/// Judge `instance` by `schema` (a subschema of `root`). Every keyword of the schema must be known, reached or not.
pub fn evaluate(root: &Value, schema: &Value, instance: &Value) -> Outcome {
    lint(root)?;
    eval(root, schema, instance, "#", 0)
}

pub fn eval_all<'a>(
    root: &Value,
    schemas: impl Iterator<Item = (String, &'a Value)>,
    instance: &Value,
    depth: usize,
) -> Result<Vec<Option<String>>, Unsupported> {
    schemas.map(|(at, sub)| eval(root, sub, instance, &at, depth)).collect()
}

pub fn eval(root: &Value, schema: &Value, instance: &Value, at: &str, depth: usize) -> Outcome {
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
                    let Value::Number(bound) = value else {
                        return Err(unsupported(&here, "a bound must be a number"));
                    };
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
// Helpers that do not name any one schema.
// ---------------------------------------------------------------------------------------------------------------------

/// A schema file below `root`, parsed and linted: a keyword the checker does not evaluate stops the test.
pub fn load_schema(root: &Path, relative: &str) -> Value {
    let path = root.join(relative);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let value: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{relative} is not JSON: {e}"));
    if let Err(Unsupported(why)) = lint(&value) {
        panic!("{relative} uses what the checker does not evaluate: {why}");
    }
    value
}

/// What `schema` says about the text of a document: `Err` is "not JSON" or the first failing keyword path. A keyword the
/// checker does not evaluate stops the test and is never an accept.
pub fn schema_verdict(schema: &Value, name: &str, text: &str) -> Result<(), String> {
    let document: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    match eval(schema, schema, &document, "#", 0) {
        Ok(None) => Ok(()),
        Ok(Some(path)) => Err(path),
        Err(Unsupported(why)) => panic!("the checker cannot judge {name}: {why}"),
    }
}

/// The whole number at `pointer` of `schema`.
pub fn pointer_number(schema: &Value, name: &str, pointer: &str) -> u64 {
    let number = schema.pointer(pointer).and_then(Value::as_u64);
    number.unwrap_or_else(|| panic!("{name} has no number at {pointer}"))
}

/// The pointer of every limit the schema states (`minLength`, `maxLength`, `minimum`, `maximum`, `minItems`, `maxItems`).
/// A `default` and an `enum` are not limits and are skipped.
pub fn limit_pointers(value: &Value, at: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                let here = format!("{at}/{key}");
                match (key.as_str(), inner) {
                    ("minLength" | "maxLength" | "minimum" | "maximum" | "minItems" | "maxItems", Value::Number(_)) => {
                        out.push(here);
                    }
                    ("default" | "enum", _) => {}
                    _ => limit_pointers(inner, &here, out),
                }
            }
        }
        Value::Array(list) => {
            list.iter().enumerate().for_each(|(n, inner)| limit_pointers(inner, &format!("{at}/{n}"), out));
        }
        _ => {}
    }
}
