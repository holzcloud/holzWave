//! Talos end to end: a fake `talosctl` that prints recorded or written documents, the integration service over a
//! scratch computer, and the answers written as golden files that the page tests read back.
//!
//! No real cluster, no real `talosctl`, no network. `talosctl` was not installed where the module was written, so every
//! document the fake prints is a fixture that is `written` from the tool's documents (see the manifest of the fixtures),
//! and every assertion that rests on such a document says so in a comment line that starts with `ASSUMPTION`. The claim
//! of this file is "everything around the module works for Talos", never "the module reads what the real tool prints".
//!
//! The goldens are the real types of the core, serialised as the page receives them, with `fetched` set to a fixed number
//! and the id of a card's ticket to a fixed one. `UPDATE_FIXTURES=1` rewrites them; without it each file must equal what is
//! built now. Nothing here edits a golden by hand.
#![cfg(unix)]

mod common;
#[path = "common/fake_cli.rs"]
mod fake_cli;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use holzwave_core::integrate::service::{HostFacts, IntegrationService, RunAnswer};
use holzwave_core::integrate::view::ViewModel;
use holzwave_core::integrate::Selection;
use holzwave_core::secrets::MemoryStore;
use serde::Serialize;
use serde_json::Value;

use common::scratch_dir;
use fake_cli::FakeCli;

/// The time every golden says it was read.
const FETCHED: u64 = 1_760_000_000;

/// The id every golden card has for its ticket.
const TICKET: &str = "0123456789abcdef0123456789abcdef";

/// A scratch computer: a `bin` directory on the `PATH`, a home, a data directory, removed when the test ends.
struct Rig {
    root: PathBuf,
    bin: PathBuf,
    home: PathBuf,
    data: PathBuf,
    store: Arc<MemoryStore>,
}

impl Rig {
    fn new() -> Rig {
        let root = scratch_dir("e2e-talos");
        let (bin, home, data) = (root.join("bin"), root.join("home"), root.join("data"));
        for dir in [&bin, &home, &data] {
            std::fs::create_dir(dir).expect("a directory of the rig");
        }
        Rig { root, bin, home, data, store: Arc::new(MemoryStore::default()) }
    }

    fn facts(&self) -> HostFacts {
        HostFacts::for_tests(&self.root, std::slice::from_ref(&self.bin), &self.home, &self.data)
    }

    fn service(&self) -> IntegrationService {
        IntegrationService::new(&self.data, self.store.clone(), self.facts(), &[])
    }

    fn service_over(&self, facts: HostFacts) -> IntegrationService {
        IntegrationService::new(&self.data, self.store.clone(), facts, &[])
    }

    /// A fake `talosctl` that prints `text`.
    fn talosctl(&self, text: &str) -> FakeCli {
        FakeCli::new(&self.bin, "talosctl").prints(text)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A recorded or written document of the tool, as text.
fn fixture(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrations").join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {relative}: {e}"))
}

fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn argv_of(fake: &FakeCli) -> Vec<String> {
    fake.argv().into_iter().map(|a| String::from_utf8(a).expect("text arguments")).collect()
}

/// The selection of a pane that talks to this context.
fn in_context(context: &str) -> Selection {
    Selection { context: Some(context.into()), ..Selection::default() }
}

// ---- the goldens ---------------------------------------------------------------------------------------------------

fn golden_path(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures/integration").join(file)
}

fn update_fixtures() -> bool {
    std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
}

/// `value` with the members of every object in sorted order (so that a feature of `serde_json` somewhere in the
/// workspace cannot change the file), every `fetched` number fixed and the ticket of a card fixed.
fn normalised(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .into_iter()
                .map(|(name, item)| {
                    let item = match (name.as_str(), &item) {
                        ("fetched", Value::Number(_)) => Value::from(FETCHED),
                        ("ticket", Value::String(_)) => Value::from(TICKET),
                        _ => normalised(item),
                    };
                    (name, item)
                })
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(list) => Value::Array(list.into_iter().map(normalised).collect()),
        other => other,
    }
}

/// Writes `file` when `UPDATE_FIXTURES=1`, else requires it to equal the serialised `value`.
fn check_golden(file: &str, value: &impl Serialize) -> Value {
    let value = normalised(serde_json::to_value(value).expect("the answer is JSON"));
    let mut text = serde_json::to_string_pretty(&value).expect("pretty JSON");
    text.push('\n');
    let path = golden_path(file);
    if update_fixtures() {
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the fixture directory");
        std::fs::write(&path, &text).expect("write the golden");
        return value;
    }
    let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("{} cannot be read ({error}); run this test with UPDATE_FIXTURES=1 to write it", path.display())
    });
    assert_eq!(on_disk, text, "{file} is stale; run this test with UPDATE_FIXTURES=1 and review the diff");
    value
}

// ---- the tracer ----------------------------------------------------------------------------------------------------

#[test]
fn the_nodes_answer_is_written_as_a_golden_and_matches_it() {
    let rig = Rig::new();
    // ASSUMPTION: the three-object stream of get_members.json is written from the documents, not recorded.
    let fake = rig.talosctl(&fixture("talosctl/get_members.json"));
    let service = rig.service();
    service.select("talos", Some(3), &in_context("talos-prod")).expect("a selection");

    let answer: RunAnswer = service.run_view("talos", "nodes", Some(3), &args(&[])).expect("the nodes are read");
    assert_eq!(argv_of(&fake), ["get", "members", "-o", "json", "--context=talos-prod"], "the exact argument list");
    let ViewModel::Table(table) = &answer.view else { panic!("nodes is a table, got {:?}", answer.view) };
    assert_eq!(table.rows().len(), 3, "one row for each member of the stream");
    assert_eq!(answer.selection.selection.context.as_deref(), Some("talos-prod"));

    let golden = check_golden("talos_nodes.json", &answer);
    assert_eq!(golden["fetched"], FETCHED);
    assert_eq!(golden["view"]["kind"], "table");
    let columns: Vec<&str> =
        golden["view"]["columns"].as_array().expect("columns").iter().map(|c| c["label"].as_str().expect("a label")).collect();
    assert_eq!(columns, ["NODE", "HOSTNAME", "TYPE", "ADDRESSES"]);
}
