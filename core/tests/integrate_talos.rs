//! Talos through `talosctl`: the argument lists of its verbs and the parsers of its answers.
//!
//! Nothing here starts a process. `talosctl` was not installed where the fixtures were made, so every fixture of the
//! `talosctl` directory is written from the tool's documents and from what its JSON writer prints, not recorded from a
//! run. An assertion that rests on such a fixture says so in a comment (`written fixture`): it holds the parser to a
//! shape that nobody has seen the real tool print yet, and a real capture replaces the fixture later.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use holzwave_core::integrate::talos::{
    parse_version, read_objects, StreamEnd, ACCESS_FULL, ACCESS_LIMITED, ACCESS_READ_ONLY, MAX_OBJECTS, TALOS,
};
use holzwave_core::integrate::view::{KvView, TableView, TextView, Tone, ViewModel};
use holzwave_core::integrate::{CheckedSelection, Integration, Selection, ToolId, Validated, Verb, VerbEffect};
use serde_json::{json, Value};

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

// ---------------------------------------------------------------------------------------------------------------------
// Helpers of the read verbs.
// ---------------------------------------------------------------------------------------------------------------------

fn view(id: &str, bytes: &[u8]) -> ViewModel {
    TALOS.view(verb(id), bytes).unwrap_or_else(|e| panic!("{id} gave an error line instead of a model: {e}"))
}

fn kv(model: &ViewModel) -> &KvView {
    match model {
        ViewModel::Kv(kv) => kv,
        other => panic!("a key-value list was expected, got {other:?}"),
    }
}

fn text_of(model: &ViewModel) -> &TextView {
    match model {
        ViewModel::Text(text) => text,
        other => panic!("a text was expected, got {other:?}"),
    }
}

fn pairs(kv: &KvView) -> Vec<(&str, &str)> {
    kv.rows().iter().map(|row| (row.label(), row.value())).collect()
}

fn tones(table: &TableView) -> Vec<Tone> {
    table.rows().iter().map(|row| row.tone()).collect()
}

fn wire(model: &ViewModel) -> String {
    serde_json::to_string(model).expect("a model is JSON")
}

/// The text of a `config contexts` table the way a tab writer lays it out: every column as wide as its widest cell
/// plus three spaces, the last one not padded.
fn contexts_text(rows: &[[&str; 4]]) -> String {
    let mut all = vec![["CURRENT", "NAME", "ENDPOINTS", "NODES"]];
    all.extend(rows.iter().copied());
    let widths: Vec<usize> = (0..3).map(|c| all.iter().map(|row| row[c].chars().count()).max().unwrap_or(0) + 3).collect();
    let mut out = String::new();
    for row in &all {
        for (c, width) in widths.iter().enumerate() {
            out.push_str(row[c]);
            out.push_str(&" ".repeat(width - row[c].chars().count()));
        }
        out.push_str(row[3]);
        out.push('\n');
    }
    out
}

/// A stream of member objects as the JSON writer prints them: indented, back to back, no array.
fn members_stream(count: usize) -> String {
    let mut out = String::new();
    for n in 0..count {
        let object = json!({
            "node": format!("192.0.2.{}", n % 250 + 1),
            "metadata": { "namespace": "cluster", "id": format!("node-{n}") },
            "spec": {
                "hostname": format!("node-{n}.example.com"),
                "machineType": "worker",
                "addresses": [format!("192.0.2.{}", n % 250 + 1)]
            }
        });
        out.push_str(&serde_json::to_string_pretty(&object).expect("JSON"));
        out.push('\n');
    }
    out
}

fn is_unreadable_text(model: &ViewModel) -> bool {
    matches!(model, ViewModel::Text(text) if text.note().is_some_and(|n| n.contains("not in the expected shape")))
}

// ---------------------------------------------------------------------------------------------------------------------
// contexts
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn contexts_are_read_from_the_plain_table_and_anything_else_is_text() {
    // Written fixture: the plain table of `config contexts`, aligned under its header, the current row marked `*`.
    let model = view("contexts", &fixture("config_contexts.txt"));
    let table = table(&model);
    assert_eq!(column_ids(table), ["current", "name", "endpoints", "nodes"]);
    assert_eq!(column_labels(table), ["CURRENT", "NAME", "ENDPOINTS", "NODES"]);
    assert_eq!(
        cells(table),
        [
            ["*", "demo-prod", "192.0.2.11", "192.0.2.11,192.0.2.12"],
            ["", "demo-lab", "192.0.2.31", "192.0.2.31"],
        ]
    );
    let keys: Vec<&str> = table.rows().iter().map(|r| r.key()).collect();
    assert_eq!(keys, ["demo-prod", "demo-lab"]);
    // A row of a context that validates offers `use_context` with the name, and the effect is the verb's.
    for row in table.rows() {
        let name = row.cells().nth(1).expect("a name cell");
        let actions = row.actions();
        assert_eq!(actions.len(), 1, "{name}");
        assert_eq!(actions[0].verb(), "use_context");
        assert_eq!(actions[0].effect(), VerbEffect::Local);
        assert_eq!(actions[0].args().collect::<Vec<_>>(), [("context", name)]);
    }

    // A table of a header only is an empty list, not an error.
    let header_only = contexts_text(&[]);
    assert!(table_rows(&view("contexts", header_only.as_bytes())).is_empty());

    // Every other shape is the cleaned text of the output with a note, never an error line and never a guess.
    let long_name = "n".repeat(40);
    let misaligned = format!("CURRENT   NAME        ENDPOINTS      NODES\n*         {long_name} 192.0.2.1  192.0.2.1\n");
    let others: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("only blank lines", b"\n\n  \n".to_vec()),
        ("no header", b"* demo-prod  192.0.2.11  192.0.2.11\n".to_vec()),
        ("a fifth column", b"CURRENT   NAME   ENDPOINTS   NODES   EXTRA\n*         a      b           c       d\n".to_vec()),
        ("a missing column", b"CURRENT   NAME   ENDPOINTS\n*         a      b\n".to_vec()),
        ("another header word", b"CURRENT   NAME   ENDPOINTS   MEMBERS\n*         a      b           c\n".to_vec()),
        ("a header in lower case", b"current   name   endpoints   nodes\n*         a      b           c\n".to_vec()),
        ("a row that is not under its columns", misaligned.into_bytes()),
        ("tabs", b"CURRENT\tNAME\tENDPOINTS\tNODES\n*\ta\tb\tc\n".to_vec()),
        ("a current mark that is not a star", b"CURRENT   NAME   ENDPOINTS   NODES\nyes       a      b           c\n".to_vec()),
        ("a row with no name", b"CURRENT   NAME   ENDPOINTS   NODES\n*                b           c\n".to_vec()),
        ("bytes that are not UTF-8", vec![0xff, 0xfe, 0x41, 0x0a]),
        ("JSON", fixture("config_info.json")),
    ];
    for (label, bytes) in &others {
        let model = view("contexts", bytes);
        assert!(matches!(model, ViewModel::Text(_)), "{label}: {model:?}");
        assert!(text_of(&model).note().is_some(), "{label}: the note says the shape was not recognised");
    }
}

/// The rows of a table model as cell lists.
fn table_rows(model: &ViewModel) -> Vec<Vec<String>> {
    cells(table(model))
}

// ---------------------------------------------------------------------------------------------------------------------
// info and the role
// ---------------------------------------------------------------------------------------------------------------------

fn info(roles: Option<Value>) -> ViewModel {
    let mut object = json!({ "context": "demo-prod", "nodes": ["192.0.2.11"], "endpoints": ["192.0.2.11"] });
    if let Some(roles) = roles {
        object["roles"] = roles;
    }
    view("info", object.to_string().as_bytes())
}

/// The `Access` entry of an info view, as the words and the tone.
fn access(model: &ViewModel) -> Option<(String, Tone)> {
    kv(model).rows().iter().find(|row| row.label() == "Access").map(|row| (row.value().to_owned(), row.tone()))
}

fn roles_row(model: &ViewModel) -> Option<String> {
    kv(model).rows().iter().find(|row| row.label() == "Roles").map(|row| row.value().to_owned())
}

#[test]
fn info_shows_the_role_and_says_what_it_allows() {
    // Written fixture: the `roles` member and the whole JSON form of `config info` are an assumption from the
    // documents, not something the tool was seen to print.
    let model = view("info", &fixture("config_info.json"));
    assert_eq!(
        pairs(kv(&model)),
        [
            ("Context", "demo-prod"),
            ("Nodes", "192.0.2.11, 192.0.2.12"),
            ("Endpoints", "192.0.2.11"),
            ("Roles", "os:reader"),
            ("Access", "read-only (os:reader)"),
        ]
    );
    assert_eq!(access(&model), Some((ACCESS_READ_ONLY.to_owned(), Tone::Normal)));

    // The words are fixed: the page and the documents use them as they are.
    assert_eq!(ACCESS_READ_ONLY, "read-only (os:reader)");
    assert_eq!(ACCESS_FULL, "full access (os:admin)");
    assert_eq!(ACCESS_LIMITED, "limited (os:operator)");

    // The rule. Written fixtures again: the role names are the ones of the documents.
    let full = Some((ACCESS_FULL.to_owned(), Tone::Warn));
    let limited = Some((ACCESS_LIMITED.to_owned(), Tone::Normal));
    let raw = |text: &str| Some((text.to_owned(), Tone::Normal));
    let cases: Vec<(Value, Option<(String, Tone)>)> = vec![
        (json!(["os:reader"]), Some((ACCESS_READ_ONLY.to_owned(), Tone::Normal))),
        (json!(["os:reader", "os:reader"]), Some((ACCESS_READ_ONLY.to_owned(), Tone::Normal))),
        (json!(["os:admin"]), full.clone()),
        (json!(["os:reader", "os:admin"]), full.clone()),
        (json!(["os:admin", "os:reader"]), full.clone()),
        (json!(["os:operator", "os:admin"]), full.clone()),
        (json!(["os:operator"]), limited.clone()),
        (json!(["os:reader", "os:operator"]), limited.clone()),
        (json!(["os:etcd:backup"]), raw("os:etcd:backup")),
        // Anything but a reader alone is not called read-only.
        (json!(["os:reader", "os:etcd:backup"]), raw("os:reader, os:etcd:backup")),
        (json!(["os:reader", "os:custom"]), raw("os:reader, os:custom")),
        // A misread must never be read-only for an admin: case and padding do not hide it, and an element that is not
        // a string does not hide the one that is.
        (json!(["OS:ADMIN"]), full.clone()),
        (json!(["  os:admin\t"]), full.clone()),
        (json!(["os:reader", 5, "os:admin"]), full.clone()),
        (json!("os:admin"), full.clone()),
        (json!("os:reader, os:admin"), full.clone()),
        (json!("os:reader"), Some((ACCESS_READ_ONLY.to_owned(), Tone::Normal))),
        // A role list that cannot be read in full is not called read-only either.
        (json!(["os:reader", 5]), None),
        (json!(["os:reader", null]), None),
        (json!([]), None),
        (json!(5), None),
        (json!({ "os:admin": true }), None),
        (json!(null), None),
    ];
    for (roles, want) in cases {
        let model = info(Some(roles.clone()));
        assert_eq!(access(&model), want, "roles {roles}");
        if want.is_none() {
            // A role list that is not understood still shows what it is, as text, and claims nothing.
            let shown = roles_row(&model);
            assert!(shown.is_some(), "roles {roles}");
            assert!(!access_words_anywhere(&model), "roles {roles}");
        }
        // The rule that matters most, over every case: no role set that names the admin role is read-only.
        if roles.to_string().to_ascii_lowercase().contains("os:admin") {
            assert_ne!(access(&model).map(|a| a.0), Some(ACCESS_READ_ONLY.to_owned()), "roles {roles}");
        }
    }

    // `roles` absent: the row is simply not shown, and nothing is claimed.
    let model = info(None);
    assert_eq!(roles_row(&model), None);
    assert_eq!(access(&model), None);
    assert_eq!(kv(&model).rows().len(), 3);
    // An empty list says so in a word.
    assert_eq!(roles_row(&info(Some(json!([])))).as_deref(), Some("none"));
    // A member that is there but has the wrong type shows `?` and is not an error.
    let model = view("info", br#"{"context": 5, "nodes": "192.0.2.1", "endpoints": {"a": 1}}"#);
    assert_eq!(pairs(kv(&model)), [("Context", "?"), ("Nodes", "?"), ("Endpoints", "?")]);

    // Only the shown members reach the model: a key or a certificate that the output holds next to them does not.
    let canary = "CANARY-KEY-MATERIAL-0123456789abcdefghijklmnop";
    let model = view(
        "info",
        json!({
            "context": "demo-prod", "nodes": ["192.0.2.11"], "endpoints": ["192.0.2.11"], "roles": ["os:reader"],
            "talosconfig": { "ca": canary, "crt": canary, "key": canary },
            "token": canary
        })
        .to_string()
        .as_bytes(),
    );
    assert!(!wire(&model).contains(canary), "a member that is not shown reached the model");
    assert_eq!(kv(&model).rows().len(), 5);

    // Output that is not the JSON object is text, and text is redacted before it is shown.
    let pem = "-----BEGIN ED25519 PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIJCANARYCANARYCANARYCANARYCANARYCANARYCANARY\n-----END ED25519 PRIVATE KEY-----\n";
    let text = format!("context: demo\nkey: {canary}\n{pem}");
    for bytes in [text.as_bytes(), b"", b"not json", b"[1, 2]", b"{\"unrelated\": true}", b"{\"context\": \"a\"} trailing"] {
        let model = view("info", bytes);
        let wire = wire(&model);
        assert!(!wire.contains("CANARYCANARY") && !wire.contains(canary), "{bytes:?}: a secret reached the text");
        if bytes.is_empty() || bytes == b"not json" || bytes == b"[1, 2]" || bytes.starts_with(b"{\"unrelated") {
            assert!(is_unreadable_text(&model), "{bytes:?}: {model:?}");
        }
    }
}

fn access_words_anywhere(model: &ViewModel) -> bool {
    let wire = wire(model);
    [ACCESS_READ_ONLY, ACCESS_FULL, ACCESS_LIMITED].iter().any(|words| wire.contains(words))
}

// ---------------------------------------------------------------------------------------------------------------------
// services and machine status
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn services_and_machine_status_become_tables() {
    // The argument lists: the node comes from the verb's argument first and the selection second.
    let both = selection(Some("demo-prod"), Some("192.0.2.99"));
    let context_only = selection(Some("demo-prod"), None);
    for (id, resource) in [("services", "services"), ("machine_status", "machinestatus")] {
        assert_eq!(
            argv(id, &[], &both),
            ["get", resource, "-o", "json", "--context=demo-prod", "--nodes=192.0.2.99"],
            "{id}: the node of the selection"
        );
        assert_eq!(
            argv(id, &[("node", "192.0.2.11")], &both),
            ["get", resource, "-o", "json", "--context=demo-prod", "--nodes=192.0.2.11"],
            "{id}: the argument wins over the selection"
        );
        assert_eq!(argv(id, &[], &context_only), ["get", resource, "-o", "json", "--context=demo-prod"]);
        assert_eq!(argv(id, &[], &selection(None, None)), ["get", resource, "-o", "json"]);
    }
    assert_eq!(
        argv("info", &[], &selection(Some("demo-prod"), Some("192.0.2.99"))),
        ["config", "info", "-o", "json", "--context=demo-prod"]
    );
    assert_eq!(argv("info", &[], &selection(None, None)), ["config", "info", "-o", "json"]);
    assert_eq!(argv("contexts", &[], &both), ["config", "contexts"]);

    // Written fixture: six service objects, one of them unhealthy.
    let model = view("services", &fixture("get_services.json"));
    let table = table(&model);
    assert_eq!(column_labels(table), ["NODE", "SERVICE", "STATE", "HEALTH"]);
    assert_eq!(column_ids(table), ["node", "service", "state", "health"]);
    let rows = cells(table);
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0], ["192.0.2.11", "apid", "running", "healthy"]);
    assert_eq!(rows[5], ["192.0.2.12", "kubelet", "running", "healthy"]);
    assert_eq!(rows[4], ["192.0.2.12", "etcd", "running", "unhealthy"]);
    // The tone is a second cue; the state is also a word in the cells. No service is `danger`: the output does not say
    // which service is expected to run.
    let want: Vec<Tone> =
        (0..6).map(|n| if n == 4 { Tone::Warn } else { Tone::Normal }).collect();
    assert_eq!(tones(table), want);
    assert!(model.check().is_ok());

    // A stopped service is dim and says `stopped`; a service that reports no health says `unknown`; an unhealthy one
    // that runs is a warning. A stopped service that reports itself unhealthy is still a warning: nothing that says it
    // is unhealthy is dimmed. These shapes are written from the documents, not recorded.
    let stream = [
        json!({"node": "a", "metadata": {"id": "one"}, "spec": {"running": false, "healthy": true}}),
        json!({"node": "a", "metadata": {"id": "two"}, "spec": {"running": true}}),
        json!({"node": "a", "metadata": {"id": "three"}, "spec": {"running": false, "healthy": false}}),
        json!({"node": "a", "metadata": {"id": "four"}, "spec": {"running": true, "healthy": false, "unknown": true}}),
        json!({"node": "a", "metadata": {"id": "five"}, "spec": {"healthy": false}}),
    ]
    .map(|o| serde_json::to_string_pretty(&o).expect("JSON"))
    .join("\n");
    let model = view("services", stream.as_bytes());
    let table = table_of(&model);
    assert_eq!(
        cells(table),
        [
            ["a", "one", "stopped", "healthy"],
            ["a", "two", "running", "unknown"],
            ["a", "three", "stopped", "unhealthy"],
            ["a", "four", "running", "unknown"],
            ["a", "five", "?", "unhealthy"],
        ]
    );
    assert_eq!(tones(table), [Tone::Dim, Tone::Normal, Tone::Warn, Tone::Normal, Tone::Warn]);
    assert!(table.rows().iter().all(|row| row.tone() != Tone::Danger));

    // Written fixture: three machines, one booting with an unmet condition.
    let model = view("machine_status", &fixture("get_machinestatus.json"));
    let table = table_of(&model);
    assert_eq!(column_labels(table), ["NODE", "STAGE", "READY"]);
    assert_eq!(column_ids(table), ["node", "stage", "ready"]);
    assert_eq!(
        cells(table),
        [["192.0.2.11", "running", "yes"], ["192.0.2.12", "booting", "no"], ["192.0.2.13", "running", "yes"]]
    );
    assert_eq!(tones(table), [Tone::Normal, Tone::Warn, Tone::Normal]);
    assert!(model.check().is_ok());
}

// ---------------------------------------------------------------------------------------------------------------------
// the object stream
// ---------------------------------------------------------------------------------------------------------------------

const CUT_NOTE: &str = "The output was cut off; ";

#[test]
fn the_object_stream_survives_truncation_and_a_second_object_that_is_not_json() {
    // Written fixtures (cut in the middle of a value of a later object): the rows read so far and a note.
    for (id, name, rows) in [
        ("nodes", "get_members_truncated.json", 1),
        ("services", "get_services_truncated.json", 3),
        ("machine_status", "get_machinestatus_truncated.json", 1),
        ("nodes", "get_members_second_not_json.json", 1),
    ] {
        let model = view(id, &fixture(name));
        let table = table(&model);
        assert_eq!(table.rows().len(), rows, "{name}");
        let note = table.note().unwrap_or_else(|| panic!("{name}: no note"));
        assert_eq!(note, format!("{CUT_NOTE}{rows} of the expected objects were read."), "{name}");
        assert!(model.check().is_ok(), "{name}");
    }
    // The first row of the cut members is the first member in full.
    let model = view("nodes", &fixture("get_members_truncated.json"));
    assert_eq!(table_rows(&model), [["192.0.2.11", "node-1.example.com", "controlplane", "192.0.2.11"]]);

    // Nothing read: the text of the output, with the note of an unknown shape.
    for (id, bytes) in [
        ("nodes", &b"{\n    \"node\": \"192.0"[..]),
        ("nodes", b"error: rpc error: code = Unavailable"),
        ("nodes", b"[]"),
        ("nodes", b"[{\"node\": \"a\"}]"),
        ("nodes", b"7"),
        ("nodes", b"\"text\""),
        ("nodes", b"null"),
        ("services", b"<html>"),
        ("machine_status", b"{ \"node\": }"),
        ("nodes", &[0xff, 0xfe, 0xfd]),
    ] {
        let model = view(id, bytes);
        assert!(is_unreadable_text(&model), "{id} {bytes:?}: {model:?}");
    }

    // An empty output is an empty list: the tool had nothing to show.
    for bytes in [&b""[..], b"\n", b"  \r\n\t"] {
        let model = view("nodes", bytes);
        assert!(table(&model).rows().is_empty() && table(&model).note().is_none(), "{bytes:?}");
    }

    // A value that is not an object ends the stream: the rows before it stay, with the note.
    let two = members_stream(2);
    for later in ["[1, 2]", "7", "\"text\"", "null", "true", "{\"node\": ", "}"] {
        let model = view("nodes", format!("{two}{later}\n{}", members_stream(1)).as_bytes());
        let table = table(&model);
        assert_eq!(table.rows().len(), 2, "after {later}");
        assert_eq!(table.note(), Some("The output was cut off; 2 of the expected objects were read."), "after {later}");
    }

    // Every prefix of every written stream: no panic, a table or a text, never more rows than whole objects, and a
    // table that is consistent.
    for name in ["get_members.json", "get_services.json", "get_machinestatus.json"] {
        let bytes = fixture(name);
        let id = match name {
            "get_members.json" => "nodes",
            "get_services.json" => "services",
            _ => "machine_status",
        };
        for end in 0..=bytes.len() {
            let prefix = &bytes[..end];
            let whole = prefix.windows(2).filter(|w| w == b"\n}").count();
            let model = view(id, prefix);
            match &model {
                ViewModel::Table(table) => {
                    assert_eq!(table.rows().len(), whole, "{name} cut at {end}");
                    assert!(model.check().is_ok(), "{name} cut at {end}");
                }
                ViewModel::Text(_) => assert_eq!(whole, 0, "{name} cut at {end}: whole objects were dropped"),
                ViewModel::Kv(_) => panic!("{name} cut at {end}: a key-value list"),
            }
        }
    }
}

#[test]
fn a_long_stream_is_cut_with_a_note() {
    // The reader itself.
    let at_cap = members_stream(MAX_OBJECTS);
    let read = read_objects(at_cap.as_bytes());
    assert_eq!((read.items.len(), read.end), (MAX_OBJECTS, StreamEnd::Complete));
    let over = members_stream(MAX_OBJECTS + 1);
    let read = read_objects(over.as_bytes());
    assert_eq!((read.items.len(), read.end), (MAX_OBJECTS, StreamEnd::Capped));

    // 2000 objects are shown whole, without a note; 2500 give 2000 rows and a note.
    let model = view("nodes", at_cap.as_bytes());
    assert_eq!(table(&model).rows().len(), 2000);
    assert!(table(&model).note().is_none() && table(&model).truncated().is_none());
    let model = view("nodes", members_stream(2500).as_bytes());
    assert_eq!(table(&model).rows().len(), 2000);
    assert_eq!(table(&model).note(), Some("More than 2000 objects were written; the first 2000 are shown."));
    assert!(model.check().is_ok(), "the keys of 2000 rows are unique");

    // Garbage after the cap is the cap, not a cut: the first 2000 were read in full.
    let mut garbage = members_stream(MAX_OBJECTS);
    garbage.push_str("this is not json");
    assert_eq!(read_objects(garbage.as_bytes()).end, StreamEnd::Capped);

    // The byte cap: a stream of whitespace after one object is cut at 16 MiB and named, and one object that is longer
    // than the cap is text, bounded.
    let mut padded = members_stream(1).into_bytes();
    padded.resize(17 * 1024 * 1024, b' ');
    let model = view("nodes", &padded);
    assert_eq!(table(&model).rows().len(), 1);
    assert!(table(&model).note().is_some_and(|n| n.contains("More than")), "{:?}", table(&model).note());
    drop(padded);
    let mut huge = br#"{"node": ""#.to_vec();
    huge.resize(17 * 1024 * 1024, b'a');
    huge.extend_from_slice(br#""}"#);
    let model = view("nodes", &huge);
    assert!(is_unreadable_text(&model));
    assert!(text_of(&model).text().len() <= 64 * 1024 && text_of(&model).truncated());
}

#[test]
fn shapes_nobody_has_seen_degrade_to_unknown_cells() {
    // Objects of any other shape are rows of `?` and never an error: nothing here is known to be what the tool prints.
    let odd = [
        "{}",
        r#"{"node": 5, "metadata": [], "spec": "x"}"#,
        r#"{"node": "192.0.2.1", "spec": {"hostname": 7, "machineType": null, "addresses": "192.0.2.1"}}"#,
        r#"{"spec": {"addresses": [1, 2, null]}}"#,
        r#"{"spec": {"addresses": []}}"#,
        r#"{"node": "192.0.2.4", "spec": {"hostname": "", "machineType": "", "addresses": [""]}, "extra": [1, {"a": 2}]}"#,
    ]
    .join("\n");
    let model = view("nodes", odd.as_bytes());
    let table = table(&model);
    assert_eq!(
        cells(table),
        [
            ["?", "?", "?", "?"],
            ["?", "?", "?", "?"],
            ["192.0.2.1", "?", "?", "?"],
            ["?", "?", "?", "?"],
            ["?", "?", "?", "none"],
            ["192.0.2.4", "?", "?", "?"],
        ]
    );
    assert!(model.check().is_ok());
    let keys: Vec<&str> = table.rows().iter().map(|r| r.key()).collect();
    assert_eq!(keys[0], "object-1");
    assert_eq!(keys[2], "192.0.2.1");

    let model = view("services", odd.as_bytes());
    assert!(cells(table_of(&model)).iter().all(|row| row[2] == "?" && row[3] == "unknown"), "{model:?}");
    let model = view("machine_status", odd.as_bytes());
    assert!(cells(table_of(&model)).iter().all(|row| row[1] == "?" && row[2] == "?"), "{model:?}");

    // A list of addresses is bounded: the first eight and how many more there were.
    let many: Vec<String> = (1..=20).map(|n| format!("192.0.2.{n}")).collect();
    let model = view("nodes", json!({"node": "n", "spec": {"hostname": "h", "addresses": many}}).to_string().as_bytes());
    let address = table_rows(&model)[0][3].clone();
    assert_eq!(address, "192.0.2.1, 192.0.2.2, 192.0.2.3, 192.0.2.4, 192.0.2.5, 192.0.2.6, 192.0.2.7, 192.0.2.8, +12 more");

    // Another party's text: escape sequences, direction overrides and a very long name are cleaned and bounded, and a
    // member that is not shown never reaches the model.
    let canary = "CANARY-KEY-MATERIAL-0123456789abcdefghijklmnop";
    let hostile = json!({
        "node": "192.0.2.1",
        "spec": {
            "hostname": format!("evil\u{1b}[31m-host\u{202e}gnp.{}", "x".repeat(6000)),
            "machineType": "control\u{1b}]0;OSCPAYLOAD\u{7}plane",
            "addresses": ["192.0.2.1\nSECOND LINE"],
            "talosconfig": { "key": canary }
        },
        "token": canary
    });
    let model = view("nodes", hostile.to_string().as_bytes());
    let wire = wire(&model);
    assert!(!wire.contains('\u{1b}') && !wire.contains("\\u001b") && !wire.contains('\u{202e}'), "{wire}");
    assert!(!wire.contains("OSCPAYLOAD") && !wire.contains(canary));
    let row = &table_rows(&model)[0];
    assert!(row.iter().all(|c| c.len() <= 4096), "a cell is bounded");
    assert_eq!(row[2], "controlplane");
    assert_eq!(row[3], "192.0.2.1 SECOND LINE");
    // The address validates, so the row has its buttons; the hostile host name is never an argument of one of them.
    let row = &table_of(&model).rows()[0];
    assert_eq!(row.actions().len(), 4);
    assert!(row.actions().iter().all(|a| a.args().collect::<Vec<_>>() == [("node", "192.0.2.1")]), "{row:?}");
}

fn table_of(model: &ViewModel) -> &TableView {
    table(model)
}

// ---------------------------------------------------------------------------------------------------------------------
// The terminal and Local verbs, hostile names and the forbidden flags.
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn the_terminal_and_local_verbs_build_their_exact_argument_lists() {
    // `use_context` names the context after `--` and takes nothing from the selection. Whether `config context` writes
    // the file is an assumption of the documents, not something a recording shows.
    let none = selection(None, None);
    assert_eq!(argv("use_context", &[("context", "talos-prod")], &none), ["config", "context", "--", "talos-prod"]);
    let both = selection(Some("other"), Some("192.0.2.9"));
    assert_eq!(argv("use_context", &[("context", "talos-prod")], &both), ["config", "context", "--", "talos-prod"]);
    // The name is one argument whatever it holds inside.
    assert_eq!(argv("use_context", &[("context", "a b=c")], &none), ["config", "context", "--", "a b=c"]);
    let map = BTreeMap::new();
    assert!(Validated::parse(verb("use_context"), &map).is_err(), "the context is required");
    assert_eq!(verb("use_context").effect, VerbEffect::Local);

    // `dmesg` follows; `dashboard` is the terminal UI. Absent flags are left out, a context comes before a node.
    assert_eq!(argv("dmesg", &[], &none), ["dmesg", "--follow"]);
    assert_eq!(argv("dashboard", &[], &none), ["dashboard"]);
    let context = selection(Some("talos-prod"), None);
    assert_eq!(argv("dmesg", &[], &context), ["dmesg", "--follow", "--context=talos-prod"]);
    assert_eq!(argv("dashboard", &[], &context), ["dashboard", "--context=talos-prod"]);
    let node = selection(None, Some("node-1.example.com"));
    assert_eq!(argv("dmesg", &[], &node), ["dmesg", "--follow", "--nodes=node-1.example.com"]);
    assert_eq!(argv("dashboard", &[], &node), ["dashboard", "--nodes=node-1.example.com"]);
    let all = selection(Some("talos-prod"), Some("192.0.2.9"));
    assert_eq!(argv("dmesg", &[], &all), ["dmesg", "--follow", "--context=talos-prod", "--nodes=192.0.2.9"]);
    assert_eq!(argv("dashboard", &[], &all), ["dashboard", "--context=talos-prod", "--nodes=192.0.2.9"]);
    // The node of the verb's own argument wins over the node of the selection.
    assert_eq!(
        argv("dmesg", &[("node", "::1")], &all),
        ["dmesg", "--follow", "--context=talos-prod", "--nodes=::1"]
    );
    assert_eq!(
        argv("dashboard", &[("node", "node-2.example.com")], &all),
        ["dashboard", "--context=talos-prod", "--nodes=node-2.example.com"]
    );
    assert_eq!((verb("dmesg").effect, verb("dashboard").effect), (VerbEffect::Read, VerbEffect::Exec));

    // The reads take the same two flags in the same order, and the node of the argument wins here too.
    assert_eq!(
        argv("services", &[("node", "192.0.2.2")], &all),
        ["get", "services", "-o", "json", "--context=talos-prod", "--nodes=192.0.2.2"]
    );
    assert_eq!(
        argv("machine_status", &[], &all),
        ["get", "machinestatus", "-o", "json", "--context=talos-prod", "--nodes=192.0.2.9"]
    );
    assert_eq!(argv("info", &[], &context), ["config", "info", "-o", "json", "--context=talos-prod"]);
    assert_eq!(argv("contexts", &[], &all), ["config", "contexts"]);
    assert_eq!(TALOS.spec().verbs.len(), 8);
}

/// Inputs that try to become an option, a second argument or a spoof; some are valid names for some kinds.
fn corpus() -> Vec<String> {
    let mut out: Vec<String> = [
        "-",
        "--",
        "-e",
        "-n",
        "-x",
        "--endpoints=x",
        "--endpoints=192.0.2.1",
        "--talosconfig=/etc/passwd",
        "--insecure",
        "--cluster=x",
        "--nodes=1.2.3.4",
        "--context=prod",
        "a\nb",
        "a\tb",
        "a\rb",
        "a\u{0}b",
        "a\u{1b}[31mb",
        "a\u{7f}b",
        "a\u{202e}b",
        "a\u{2066}b",
        "a\u{200b}b",
        "\u{feff}a",
        "a\u{ad}b",
        "a\u{e0041}b",
        "a\u{a0}b",
        "a\u{3000}b",
        "a b",
        " a",
        "a ",
        "a=b",
        "a,b",
        "a;b",
        "a'b",
        "a\"b",
        "a\\b",
        "$(id)",
        "`id`",
        "a|b",
        "a&&b",
        "a>b",
        "../x",
        "",
        " ",
        "1.2.3.4.5",
        "10.0.4.12/24",
        "10.0.4.12:50000",
        "[::1]",
        "::1",
        "fe80::1%en0",
        "2001:db8::1",
        "192.0.2.1",
        "256.1.1.1",
        "0",
        "UPPER",
        "a_b",
        "a..b",
        "node-1.example.com",
        "node-1",
        "talos-prod",
        "admin@talos-prod",
        "user@cluster:443",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    for length in [62, 63, 64, 252, 253, 254, 255, 4096, 4097] {
        out.push("a".repeat(length));
    }
    out
}

/// Every way a text can be put into a verb call: (label, verb, arguments, selected context, selected node).
type Call = (String, &'static str, Vec<(&'static str, String)>, Option<String>, Option<String>);

fn calls_with(text: &str) -> Vec<Call> {
    let t = || text.to_owned();
    let mut calls: Vec<Call> = Vec::new();
    let mut add = |label: &str, id: &'static str, pairs: Vec<(&'static str, String)>, c: Option<String>, n: Option<String>| {
        calls.push((format!("{label} {text:?}"), id, pairs, c, n));
    };
    add("use_context arg", "use_context", vec![("context", t())], None, None);
    for id in ["services", "machine_status", "dmesg", "dashboard"] {
        add(&format!("{id} node arg"), id, vec![("node", t())], None, None);
        add(&format!("{id} node arg with selection"), id, vec![("node", t())], Some("talos-prod".into()), Some("192.0.2.9".into()));
    }
    for id in ["contexts", "info", "nodes", "services", "machine_status", "dmesg", "dashboard"] {
        add(&format!("{id} selected context"), id, vec![], Some(t()), None);
        add(&format!("{id} selected node"), id, vec![], None, Some(t()));
    }
    calls
}

/// Builds a call; `None` when a validator refused the text before an argument list existed.
fn try_build(id: &str, pairs: &[(&str, String)], context: Option<&str>, node: Option<&str>) -> Option<Vec<String>> {
    let map: BTreeMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect();
    let validated = Validated::parse(verb(id), &map).ok()?;
    let checked = Selection {
        context: context.map(str::to_owned),
        node: node.map(str::to_owned),
        ..Selection::default()
    }
    .check(ToolId::Talos)
    .ok()?;
    let list = TALOS.argv(verb(id), &validated, &checked).expect("a validated call builds");
    Some(list.iter().map(|a| a.to_str().expect("UTF-8 arguments").to_owned()).collect())
}

/// The words and flags of the module's own catalogue: nothing else may start with a dash or stand alone.
const FIXED: [&str; 12] =
    ["config", "context", "contexts", "info", "get", "members", "services", "machinestatus", "dmesg", "dashboard", "-o", "json"];
const FIXED_FLAGS: [&str; 2] = ["--follow", "--"];

/// What a list of arguments may hold: every element is a fixed word, a fixed flag, `--context=` or `--nodes=` with a
/// value that is one of the `names` of the call, or the one name that follows `--`.
fn assert_shape(argv: &[String], names: &[&str], label: &str) {
    let dashes = argv.iter().position(|a| a == "--");
    for (at, element) in argv.iter().enumerate() {
        if FIXED.contains(&element.as_str()) || FIXED_FLAGS.contains(&element.as_str()) {
            // A name that equals a fixed word is still only a fixed word here, except after `--`.
            if !dashes.is_some_and(|d| at == d + 1) {
                continue;
            }
        }
        if let Some(value) = element.strip_prefix("--context=").or_else(|| element.strip_prefix("--nodes=")) {
            assert!(names.contains(&value), "{label}: {element:?} carries a value that no name of the call has: {argv:?}");
            continue;
        }
        assert!(dashes.is_some_and(|d| at == d + 1), "{label}: {element:?} is no flag of ours and not after `--`: {argv:?}");
        assert!(names.contains(&element.as_str()), "{label}: {element:?} is no name of the call: {argv:?}");
    }
    // A node is always inside `--nodes=`: no element equals a name of the call except behind `--`.
    for name in names {
        let bare = argv.iter().enumerate().filter(|(at, e)| e == name && !dashes.is_some_and(|d| *at == d + 1)).count();
        let fixed = FIXED.contains(name) || FIXED_FLAGS.contains(name);
        assert!(bare == 0 || fixed, "{label}: the name {name:?} stands alone in {argv:?}");
    }
}

#[test]
fn a_hostile_name_never_changes_the_talosctl_argument_list() {
    let (mut accepted, mut refused) = (0, 0);
    for text in corpus() {
        for (label, id, pairs, context, node) in calls_with(&text) {
            match try_build(id, &pairs, context.as_deref(), node.as_deref()) {
                None => refused += 1,
                Some(argv) => {
                    accepted += 1;
                    let mut names: Vec<&str> = pairs.iter().map(|(_, v)| v.as_str()).collect();
                    names.extend(context.as_deref());
                    names.extend(node.as_deref());
                    assert_shape(&argv, &names, &label);
                    // A name that starts with a dash is never accepted, so it is never an element of the list.
                    assert!(!text.starts_with('-') || !argv.iter().any(|a| a.contains(text.as_str()) && a != "--"), "{label}: {argv:?}");
                }
            }
        }
    }
    assert!(accepted > 150 && refused > 400, "the corpus must exercise both sides: {accepted} accepted, {refused} refused");

    // Spot checks of what is refused, what is whole and the exact list that results.
    for bad in [
        "--endpoints=x",
        "-e",
        "a\nb",
        "a\u{202e}b",
        "a\u{200b}b",
        "1.2.3.4.5",
        "10.0.4.12/24",
        "[::1]",
        "a b",
        &"a".repeat(254),
    ] {
        for id in ["services", "machine_status", "dmesg", "dashboard"] {
            assert!(try_build(id, &[("node", bad.to_owned())], None, None).is_none(), "{id} node {bad:?} was accepted");
            assert!(try_build(id, &[], None, Some(bad)).is_none(), "{id} selected node {bad:?} was accepted");
        }
    }
    for bad in ["--endpoints=x", "-e", "a\nb", "a\u{202e}b", " a", "a ", &"a".repeat(254)] {
        assert!(try_build("use_context", &[("context", bad.to_owned())], None, None).is_none(), "context {bad:?}");
        assert!(try_build("dmesg", &[], Some(bad), None).is_none(), "selected context {bad:?}");
    }
    assert_eq!(
        try_build("dmesg", &[("node", "::1".to_owned())], None, None).expect("an IPv6 literal is a node"),
        ["dmesg", "--follow", "--nodes=::1"]
    );
    assert_eq!(
        try_build("dashboard", &[], Some("a b"), None).expect("a space inside a context name is allowed"),
        ["dashboard", "--context=a b"],
        "a name with a space is one argument"
    );
    assert_eq!(
        try_build("use_context", &[("context", "a=b".to_owned())], None, None).expect("an equals sign is allowed"),
        ["config", "context", "--", "a=b"]
    );
    assert_eq!(
        try_build("use_context", &[("context", "talos-prod".to_owned())], Some("x"), Some("1.2.3.4")),
        Some(["config", "context", "--", "talos-prod"].map(str::to_owned).to_vec())
    );

    // Generated names: whatever mix of dashes, equals signs, quotes and control characters, an accepted list keeps its shape.
    let mut rng = common::Xorshift64::new(0x7461_6c6f);
    let alphabet: Vec<char> = "-=aZ09_. /@:'\"$`|&;<>[]\n\t\u{0}\u{1b}\u{202e}\u{200b}\u{a0}\u{e9}".chars().collect();
    for _ in 0..4000 {
        let length = 1 + rng.below(12);
        let name: String = (0..length).map(|_| alphabet[rng.below(alphabet.len())]).collect();
        for (label, id, pairs, context, node) in calls_with(&name) {
            if let Some(argv) = try_build(id, &pairs, context.as_deref(), node.as_deref()) {
                let mut names: Vec<&str> = pairs.iter().map(|(_, v)| v.as_str()).collect();
                names.extend(context.as_deref());
                names.extend(node.as_deref());
                assert_shape(&argv, &names, &label);
            }
        }
    }
}

#[test]
fn no_argument_list_holds_a_config_or_endpoint_flag() {
    const FORBIDDEN: [&str; 6] = ["--talosconfig", "--endpoints", "-e", "--insecure", "--cluster", "-n"];
    let mut lists = 0;
    let mut check = |argv: &[String], label: &str| {
        lists += 1;
        for element in argv {
            for flag in FORBIDDEN {
                let equals = format!("{flag}=");
                assert!(element != flag && !element.starts_with(&equals), "{label}: {flag} is in {argv:?}");
            }
            let own = FIXED.contains(&element.as_str())
                || FIXED_FLAGS.contains(&element.as_str())
                || element.starts_with("--context=")
                || element.starts_with("--nodes=");
            let name_behind_dashes = argv.iter().position(|a| a == "--").is_some_and(|d| argv.get(d + 1) == Some(element));
            assert!(own || name_behind_dashes || !element.starts_with('-'), "{label}: the flag {element:?} is not in the catalogue");
            assert!(!element.contains("talosconfig") || element.starts_with("--context="), "{label}: {element:?}");
        }
    };
    // Every verb, with every valid name of the corpus in every role and with no name at all.
    for text in corpus() {
        for (label, id, pairs, context, node) in calls_with(&text) {
            if let Some(argv) = try_build(id, &pairs, context.as_deref(), node.as_deref()) {
                check(&argv, &label);
            }
        }
    }
    for verb_listed in TALOS.spec().verbs {
        for (context, node) in [(None, None), (Some("talos-prod"), None), (None, Some("192.0.2.9")), (Some("a b"), Some("::1"))] {
            let id = verb_listed.id;
            let pairs: Vec<(&str, String)> = verb_listed.args.iter().map(|spec| (spec.name, "talos-prod".to_owned())).collect();
            let argv = try_build(id, &pairs, context, node).unwrap_or_else(|| panic!("{id} with valid names was refused"));
            check(&argv, id);
        }
    }
    assert!(lists > 150, "the sweep looked at {lists} argument lists");

    // The module never names a forbidden flag as an argument of its own: the sweep above is the proof for what it
    // builds, and this holds the source to it. The talosconfig reaches the tool through the environment.
    let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/integrate/talos.rs")).expect("source");
    let code = source.split("#[cfg(test)]").next().expect("code");
    for flag in ["\"--talosconfig", "\"--endpoints", "\"-e\"", "\"-e=", "\"--insecure", "\"--cluster", "\"-n\"", "\"--nodes\""] {
        let hits = code.lines().filter(|l| !l.trim_start().starts_with("//") && l.contains(flag)).count();
        assert_eq!(hits, 0, "the source builds {flag}");
    }
}

#[test]
fn node_rows_carry_actions_only_for_valid_names() {
    let summary = |model: &ViewModel| -> Vec<Vec<(String, VerbEffect, Vec<(String, String)>)>> {
        table_of(model)
            .rows()
            .iter()
            .map(|row| {
                row.actions()
                    .iter()
                    .map(|a| {
                        let args = a.args().map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
                        (a.verb().to_owned(), a.effect(), args)
                    })
                    .collect()
            })
            .collect()
    };
    let node_actions = |node: &str| -> Vec<(String, VerbEffect, Vec<(String, String)>)> {
        [
            ("services", VerbEffect::Read),
            ("machine_status", VerbEffect::Read),
            ("dmesg", VerbEffect::Read),
            ("dashboard", VerbEffect::Exec),
        ]
        .iter()
        .map(|(id, effect)| ((*id).to_owned(), *effect, vec![("node".to_owned(), node.to_owned())]))
        .collect()
    };

    // Written fixture: each member row gets the four node verbs for its address, the address being what `--nodes` takes.
    let model = view("nodes", &fixture("get_members.json"));
    let rows = summary(&model);
    assert_eq!(rows, [node_actions("192.0.2.11"), node_actions("192.0.2.12"), node_actions("192.0.2.13")]);
    assert!(model.check().is_ok());

    let one = |node: Value, hostname: Value| {
        let object = json!({"node": node, "spec": {"hostname": hostname, "machineType": "worker", "addresses": []}});
        summary(&view("nodes", object.to_string().as_bytes()))[0].clone()
    };
    // An IPv6 literal validates; the bracket form and a mask do not.
    assert_eq!(one(json!("2001:db8::1"), json!(null)), node_actions("2001:db8::1"));
    assert_eq!(one(json!("::1"), json!(null)), node_actions("::1"));
    // Without a valid address the host name stands in, and without either there is no button.
    assert_eq!(one(json!(null), json!("node-9.example.com")), node_actions("node-9.example.com"));
    assert_eq!(one(json!("[::1]"), json!("node-9")), node_actions("node-9"));
    assert_eq!(one(json!("192.0.2.5"), json!("evil\u{202e}host")), node_actions("192.0.2.5"));
    let hostile = [
        "-x",
        "--endpoints=x",
        "-e",
        "1.2.3.4.5",
        "10.0.4.12/24",
        "[::1]",
        "a b",
        "a\nb",
        "a\u{1b}[31mb",
        "a\u{202e}b",
        "a\u{200b}b",
        "",
        "UPPER",
        "a_b",
    ];
    for bad in hostile {
        assert!(one(json!(bad), json!(bad)).is_empty(), "{bad:?} offered an action");
        assert!(one(json!(bad), json!(null)).is_empty(), "{bad:?} offered an action");
    }
    let long = "a".repeat(254);
    assert!(one(json!(long), json!(long)).is_empty(), "254 bytes offered an action");
    assert!(one(json!(5), json!(["x"])).is_empty(), "wrong-typed members offered an action");

    // The other tables carry the actions they carried before: context rows have `use_context`, the rest none.
    let contexts = view("contexts", &fixture("config_contexts.txt"));
    assert!(table_of(&contexts).rows().iter().all(|r| r.actions().iter().all(|a| a.verb() == "use_context")));
    for (id, name) in [("services", "get_services.json"), ("machine_status", "get_machinestatus.json")] {
        let model = view(id, &fixture(name));
        assert!(table_of(&model).rows().iter().all(|r| r.actions().is_empty()), "{id}");
    }
}
