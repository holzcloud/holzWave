//! Kubernetes through `kubectl`: the argument lists of the verbs, the parsers of what `kubectl` prints, and the overlay
//! kubeconfig that gives a shell pane its own current context.
//!
//! Nothing here starts a process: the parsers are held against recorded output and against drift and hostile text,
//! and the argument builders against names that try to become options. The real tool appears in one test that is
//! gated by `HW_INTEGRATION_REAL=1` and runs in a scratch directory.

mod common;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use holzwave_core::integrate::kube::{self, KUBE};
use holzwave_core::integrate::view::ViewModel;
use holzwave_core::integrate::{CheckedSelection, Integration, Selection, ToolId, Validated, Verb};

/// The canary strings of the scratch kubeconfig. No model, no argument list and no overlay may ever hold them.
const CANARY_TOKEN: &str = "HW-CANARY-TOKEN-0001";
const CANARY_EXEC_ENV: &str = "HW-CANARY-EXEC-ENV";

fn fixture(relative: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrations").join(relative);
    fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn verb(id: &str) -> &'static Verb {
    KUBE.spec().verb(id).unwrap_or_else(|| panic!("the kube spec has no verb {id}"))
}

/// The validated arguments of a verb call from (name, value) pairs.
fn args(id: &str, pairs: &[(&str, &str)]) -> Validated {
    let map: BTreeMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    Validated::parse(verb(id), &map).unwrap_or_else(|e| panic!("{id} {pairs:?} was refused: {e}"))
}

/// The selection of a pane, validated for the kube tool.
fn selection(context: Option<&str>, namespace: Option<&str>) -> CheckedSelection {
    let selection = Selection {
        context: context.map(str::to_owned),
        namespace: namespace.map(str::to_owned),
        ..Selection::default()
    };
    selection.check(ToolId::Kube).expect("a valid selection")
}

fn table(model: &ViewModel) -> &holzwave_core::integrate::view::TableView {
    match model {
        ViewModel::Table(table) => table,
        other => panic!("a table was expected, not {other:?}"),
    }
}

fn texts(argv: &[OsString]) -> Vec<String> {
    argv.iter().map(|a| a.to_str().expect("every argument of kubectl is text").to_owned()).collect()
}

// ---------------------------------------------------------------------------------------------------------------------
// The contexts verb, end to end on the recorded `config view`.
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn contexts_go_from_argument_list_to_table() {
    let contexts = verb("contexts");
    let argv = KUBE.argv(contexts, &args("contexts", &[]), &CheckedSelection::default()).expect("argv");
    assert_eq!(texts(&argv), ["config", "view", "-o", "json"], "the contexts verb is one fixed argument list");

    let model = KUBE.view(contexts, &fixture("kubectl/config_view.json")).expect("the recorded config view reads");
    let table = table(&model);
    let columns: Vec<&str> = table.columns().iter().map(|c| c.label()).collect();
    assert_eq!(columns, ["CURRENT", "NAME", "CLUSTER", "USER", "NAMESPACE"]);
    let rows: Vec<(String, Vec<&str>)> = table.rows().iter().map(|r| (r.key().to_owned(), r.cells().collect())).collect();
    assert_eq!(
        rows,
        [
            (
                "arn:aws:eks:eu-central-1:000000000000:cluster/demo".to_owned(),
                vec![
                    "",
                    "arn:aws:eks:eu-central-1:000000000000:cluster/demo",
                    "arn:aws:eks:eu-central-1:000000000000:cluster/demo",
                    "exec-user",
                    ""
                ]
            ),
            ("dev".to_owned(), vec!["*", "dev", "dev-cluster", "token-user", ""]),
            ("kind-test".to_owned(), vec!["", "kind-test", "kind-test", "token-user", "test"]),
        ],
        "one row per context, a star in the current one, the row key is the context name, the ARN-shaped name is whole"
    );
    for row in table.rows() {
        let actions: Vec<(&str, Vec<(&str, &str)>)> = row.actions().iter().map(|a| (a.verb(), a.args().collect())).collect();
        let name = row.cells().nth(1).expect("the name cell");
        assert_eq!(actions, [("use_context", vec![("context", name)])], "a context row offers use_context for its name");
    }
}

#[test]
fn the_credential_members_of_a_config_view_never_enter_a_model() {
    // The real tool leaves the values of an exec plugin's environment in clear; the canary stands for that.
    let mut config: serde_json::Value = serde_json::from_slice(&fixture("kubectl/config_view.json")).expect("json");
    config["users"][0]["user"]["exec"]["env"][0]["value"] = CANARY_EXEC_ENV.into();
    config["users"][1]["user"]["token"] = CANARY_TOKEN.into();
    config["users"][1]["user"]["client-key-data"] = "HW-CANARY-KEY".into();
    let bytes = serde_json::to_vec(&config).expect("serialise");
    let model = KUBE.view(verb("contexts"), &bytes).expect("a config view with canaries reads");
    let shown = serde_json::to_string(&model).expect("the model serialises");
    for canary in [CANARY_TOKEN, CANARY_EXEC_ENV, "HW-CANARY-KEY", "interactiveMode", "REDACTED", "eks.example.com", "get-token"] {
        assert!(!shown.contains(canary), "{canary} reached the model: {shown}");
    }
}

#[test]
fn the_version_probe_reads_the_client_version() {
    let probe = KUBE.spec().version_probe;
    assert_eq!(probe.argv, ["version", "--client", "-o", "json"]);
    let text = String::from_utf8(fixture("kubectl/version_client.json")).expect("utf-8");
    assert_eq!((probe.parse)(&text).as_deref(), Some("v1.36.1"));
    assert_eq!(kube::parse_version(&text).as_deref(), Some("v1.36.1"));
    for garbage in ["", "not json", "[]", "{}", r#"{"clientVersion":{}}"#, r#"{"clientVersion":{"gitVersion":5}}"#] {
        assert_eq!(kube::parse_version(garbage), None, "{garbage:?}");
    }
}

/// Keeps the helpers of later tasks used while the file grows.
#[allow(dead_code)]
fn scratch(tag: &str) -> PathBuf {
    common::scratch_dir(tag)
}
