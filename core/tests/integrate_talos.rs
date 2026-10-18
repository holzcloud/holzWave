//! Talos through `talosctl`: the argument lists of its verbs and the parsers of its answers.
//!
//! Nothing here starts a process. `talosctl` was not installed where the fixtures were made, so every fixture of the
//! `talosctl` directory is written from the tool's documents and from what its JSON writer prints, not recorded from a
//! run. An assertion that rests on such a fixture says so in a comment (`written fixture`): it holds the parser to a
//! shape that nobody has seen the real tool print yet, and a real capture replaces the fixture later.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use holzwave_core::integrate::talos::{parse_version, TALOS};
use holzwave_core::integrate::view::{TableView, ViewModel};
use holzwave_core::integrate::{CheckedSelection, Integration, Selection, Validated, Verb};

fn fixture(name: &str) -> Vec<u8> {
    let path: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrations/talosctl").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read the fixture {name}: {e}"))
}

fn verb(id: &str) -> &'static Verb {
    TALOS.spec().verb(id).unwrap_or_else(|| panic!("the catalogue has no verb {id}"))
}

/// The selection of a pane after validation.
fn selection(context: Option<&str>, node: Option<&str>) -> CheckedSelection {
    let text = Selection {
        context: context.map(str::to_owned),
        node: node.map(str::to_owned),
        ..Selection::default()
    };
    text.check(holzwave_core::integrate::ToolId::Talos).expect("a valid selection")
}

fn validated(id: &str, pairs: &[(&str, &str)]) -> Validated {
    let map: BTreeMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    Validated::parse(verb(id), &map).expect("valid arguments")
}

/// The argument list of a verb as text.
fn argv(id: &str, pairs: &[(&str, &str)], selection: &CheckedSelection) -> Vec<String> {
    TALOS
        .argv(verb(id), &validated(id, pairs), selection)
        .expect("an argument list")
        .iter()
        .map(|a| a.to_str().expect("UTF-8 arguments").to_owned())
        .collect()
}

fn table(model: &ViewModel) -> &TableView {
    match model {
        ViewModel::Table(table) => table,
        other => panic!("a table was expected, got {other:?}"),
    }
}

fn cells(table: &TableView) -> Vec<Vec<String>> {
    table.rows().iter().map(|row| row.cells().map(str::to_owned).collect()).collect()
}

fn column_ids(table: &TableView) -> Vec<&str> {
    table.columns().iter().map(|c| c.id()).collect()
}

fn column_labels(table: &TableView) -> Vec<&str> {
    table.columns().iter().map(|c| c.label()).collect()
}

#[test]
fn nodes_go_from_argument_list_through_the_object_stream_to_a_table() {
    // The argument list: the context flag is there only when a context is selected, and it is one argument.
    assert_eq!(
        argv("nodes", &[], &selection(Some("talos-prod"), None)),
        ["get", "members", "-o", "json", "--context=talos-prod"]
    );
    assert_eq!(argv("nodes", &[], &selection(None, None)), ["get", "members", "-o", "json"]);

    // The written fixture: three indented objects back to back with a `node` member, `metadata` and `spec`. The shape
    // is from the documents and the JSON writer, not from a recording.
    let model = TALOS.view(verb("nodes"), &fixture("get_members.json")).expect("a view");
    let table = table(&model);
    assert_eq!(column_ids(table), ["node", "hostname", "type", "addresses"]);
    assert_eq!(column_labels(table), ["NODE", "HOSTNAME", "TYPE", "ADDRESSES"]);
    assert_eq!(
        cells(table),
        [
            ["192.0.2.11", "node-1.example.com", "controlplane", "192.0.2.11"],
            ["192.0.2.12", "node-2.example.com", "controlplane", "192.0.2.12"],
            ["192.0.2.13", "node-3.example.com", "worker", "192.0.2.13"],
        ]
    );
    let keys: Vec<&str> = table.rows().iter().map(|r| r.key()).collect();
    assert_eq!(keys, ["node-1.example.com", "node-2.example.com", "node-3.example.com"]);
    assert!(table.truncated().is_none() && table.note().is_none(), "a complete stream carries no note");
    assert!(model.check().is_ok(), "every row has as many cells as the table has columns");
}

#[test]
fn the_version_probe_reads_the_tag() {
    // Written fixture: the `Client:` block of `talosctl version --client`, plausible values.
    let text = String::from_utf8(fixture("version.txt")).expect("UTF-8");
    assert_eq!(parse_version(&text).as_deref(), Some("v1.12.0"));
    assert_eq!(TALOS.spec().version_probe.argv, ["version", "--client"]);
    assert_eq!((TALOS.spec().version_probe.parse)(&text).as_deref(), Some("v1.12.0"));

    for garbage in ["", "Client:\n", "Tag:", "Tag:   ", "no tag here", "\u{1b}[31mTag: v1\u{1b}[0m", "Tag: v1 v2"] {
        assert_eq!(parse_version(garbage), None, "{garbage:?}");
    }
    assert_eq!(parse_version(&format!("Tag: {}", "v".repeat(200))), None, "an over-long tag");
}
