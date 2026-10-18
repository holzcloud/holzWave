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

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use holzwave_core::integrate::view::ViewModel;
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
