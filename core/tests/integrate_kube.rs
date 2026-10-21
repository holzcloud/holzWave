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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use holzwave_core::integrate::kube::{self, KUBE};
use holzwave_core::integrate::view::{Row, TableView, Tone, ViewModel, MAX_CELL, MAX_TEXT};
use holzwave_core::integrate::args::ArgError;
use holzwave_core::integrate::{
    CheckedSelection, Integration, Output, ParseError, SelectKind, Selection, ToolId, Validated, Verb, VerbEffect,
};

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

fn table(model: &ViewModel) -> &TableView {
    match model {
        ViewModel::Table(table) => table,
        other => panic!("a table was expected, not {other:?}"),
    }
}

/// The actions of a row as (verb, arguments).
fn actions_of(row: &Row) -> Vec<(&str, Vec<(&str, &str)>)> {
    row.actions().iter().map(|a| (a.verb(), a.args().collect())).collect()
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
    let rows: Vec<(String, Vec<&str>)> =
        table.rows().iter().map(|r| (r.key().to_owned(), r.cells().collect())).collect();
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
        let name = row.cells().nth(1).expect("the name cell");
        let expected = [("use_context", vec![("context", name)])];
        assert_eq!(actions_of(row), expected, "a context row offers use_context for its name");
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
    let canaries = [CANARY_TOKEN, CANARY_EXEC_ENV, "HW-CANARY-KEY", "interactiveMode", "REDACTED", "eks.example.com"];
    for canary in canaries.into_iter().chain(["get-token"]) {
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

// ---------------------------------------------------------------------------------------------------------------------
// Namespaces and pods.
// ---------------------------------------------------------------------------------------------------------------------

/// 2026-01-05T10:00:00Z, the creation time of every pod of the recorded list.
const CREATED: u64 = 1_767_607_200;

fn at(seconds_after_created: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(CREATED + seconds_after_created)
}

fn pods_at(bytes: &[u8], seconds_after_created: u64) -> ViewModel {
    KUBE.view_at(verb("pods"), bytes, at(seconds_after_created)).expect("a pod list reads")
}

fn cells(model: &ViewModel) -> Vec<Vec<String>> {
    table(model).rows().iter().map(|row| row.cells().map(str::to_owned).collect()).collect()
}

fn argv_of(id: &str, pairs: &[(&str, &str)], context: Option<&str>, namespace: Option<&str>) -> Vec<String> {
    texts(&KUBE.argv(verb(id), &args(id, pairs), &selection(context, namespace)).expect("argv"))
}

#[test]
fn namespace_and_pod_argument_lists_carry_the_selection_in_the_equals_form() {
    assert_eq!(
        argv_of("namespaces", &[], Some("dev"), None),
        ["get", "namespaces", "-o", "json", "--context=dev", "--request-timeout=10s"]
    );
    assert_eq!(
        argv_of("pods", &[], Some("dev"), Some("payments")),
        ["get", "pods", "-o", "json", "--context=dev", "--namespace=payments", "--request-timeout=10s"]
    );
    assert_eq!(argv_of("pods", &[], None, None), ["get", "pods", "-o", "json", "--request-timeout=10s"]);
    assert_eq!(
        argv_of("pods", &[], None, Some("payments")),
        ["get", "pods", "-o", "json", "--namespace=payments", "--request-timeout=10s"]
    );
    assert_eq!(
        argv_of("namespaces", &[], Some("dev"), Some("payments")),
        ["get", "namespaces", "-o", "json", "--context=dev", "--request-timeout=10s"],
        "a list of namespaces is not scoped to a namespace"
    );
    // A context name with a colon, a slash and an at sign is one argument.
    let arn = "arn:aws:eks:eu-central-1:000000000000:cluster/demo";
    assert_eq!(argv_of("pods", &[], Some(arn), None)[4], format!("--context={arn}"));
    assert_eq!(argv_of("pods", &[], Some("user@cluster"), None)[4], "--context=user@cluster");
    // A selection field that is invalid never gets as far as an argument list.
    for bad in ["--server=https://x", "-x", "a\nb", "a\u{202e}b", ""] {
        let selection = Selection { context: Some(bad.to_owned()), ..Selection::default() };
        assert!(selection.check(ToolId::Kube).is_err(), "{bad:?} passed the selection check");
    }
    // The 10 s refresh is on the pods verb only, and the other reads are on request.
    let refresh: Vec<(&str, Option<u32>)> = KUBE.spec().verbs.iter().map(|v| (v.id, v.refresh_secs)).collect();
    assert_eq!(refresh.iter().filter(|(_, secs)| secs.is_some()).collect::<Vec<_>>(), [&("pods", Some(10))]);
}

#[test]
fn namespaces_become_a_table() {
    let model = KUBE.view_at(verb("namespaces"), &fixture("kubectl/namespaces.json"), at(860_400)).expect("reads");
    let table = table(&model);
    let columns: Vec<&str> = table.columns().iter().map(|c| c.label()).collect();
    assert_eq!(columns, ["NAME", "STATUS", "AGE"]);
    assert_eq!(
        cells(&model),
        [["default", "Active", "14d"], ["kube-system", "Active", "13d"], ["team-a", "Active", "12d"]],
        "the namespaces were created on 1, 2 and 3 January at 9:00 and the list is read on 15 January at 9:00"
    );
    let keys: Vec<&str> = table.rows().iter().map(|r| r.key()).collect();
    assert_eq!(keys, ["default", "kube-system", "team-a"]);
    for row in table.rows() {
        let name = row.key();
        let expected = [("set_namespace", vec![("namespace", name)])];
        assert_eq!(actions_of(row), expected, "a namespace row offers set_namespace");
    }
}

#[test]
fn pods_become_a_table_with_the_status_a_person_expects() {
    let model = pods_at(&fixture("kubectl/pods.json"), 45);
    let table = table(&model);
    let columns: Vec<&str> = table.columns().iter().map(|c| c.label()).collect();
    assert_eq!(columns, ["NAME", "READY", "STATUS", "RESTARTS", "AGE", "NODE"]);
    assert_eq!(
        cells(&model),
        [
            ["web-7d9f", "2/2", "Running", "0", "45s", "node-1"],
            ["batch-1", "0/1", "ContainerCreating", "0", "45s", ""],
            ["api-5c6d", "0/1", "CrashLoopBackOff", "7", "45s", "node-2"],
        ]
    );
    let tones: Vec<Tone> = table.rows().iter().map(|r| r.tone()).collect();
    assert_eq!(tones, [Tone::Normal, Tone::Warn, Tone::Danger]);
    // The age is computed from the clock that is passed in.
    let ages = [(0, "0s"), (59, "59s"), (60, "1m"), (7 * 60, "7m"), (3 * 3600 + 5, "3h"), (12 * 86_400, "12d")];
    for (after, shown) in ages {
        assert_eq!(cells(&pods_at(&fixture("kubectl/pods.json"), after))[0][4], shown, "after {after} s");
    }
    assert_eq!(cells(&pods_at(&fixture("kubectl/pods.json"), 0))[0][4], "0s");
    // A clock before the creation time gives 0s and not a wrapped number.
    let early = KUBE.view_at(verb("pods"), &fixture("kubectl/pods.json"), UNIX_EPOCH).expect("reads");
    assert_eq!(cells(&early)[0][4], "0s");
}

/// A pod list with one pod whose `status` is `status` (a JSON text) and whose `metadata` has `extra` members.
fn one_pod(extra: &str, status: &str) -> Vec<u8> {
    format!(
        r#"{{"items":[{{"metadata":{{"name":"p-1","creationTimestamp":"2026-01-05T10:00:00Z"{extra}}},
        "spec":{{"nodeName":"n-1","containers":[{{"name":"app"}}]}},"status":{status}}}]}}"#
    )
    .into_bytes()
}

fn status_of(bytes: &[u8]) -> (String, Tone) {
    let model = pods_at(bytes, 10);
    let row = &table(&model).rows()[0];
    (row.cells().nth(2).expect("status").to_owned(), row.tone())
}

#[test]
fn the_status_follows_the_waiting_reason_then_the_terminated_reason_then_terminating_then_the_phase() {
    let with_state = |state: &str| {
        let container = format!(r#"{{"name":"app","ready":false,"restartCount":1,"state":{state}}}"#);
        format!(r#"{{"phase":"Running","containerStatuses":[{container}]}}"#)
    };
    let waiting = |reason: &str| with_state(&format!(r#"{{"waiting":{{"reason":"{reason}"}}}}"#));
    let terminated = |reason: &str| with_state(&format!(r#"{{"terminated":{{"reason":"{reason}","exitCode":137}}}}"#));
    for (reason, tone) in [
        ("CrashLoopBackOff", Tone::Danger),
        ("ImagePullBackOff", Tone::Danger),
        ("ErrImagePull", Tone::Danger),
        ("ContainerCreating", Tone::Warn),
        ("PodInitializing", Tone::Warn),
        ("SomethingNew", Tone::Normal),
    ] {
        assert_eq!(status_of(&one_pod("", &waiting(reason))), (reason.to_owned(), tone), "waiting {reason}");
    }
    for (reason, tone) in [("OOMKilled", Tone::Danger), ("Error", Tone::Danger), ("Completed", Tone::Normal)] {
        assert_eq!(status_of(&one_pod("", &terminated(reason))), (reason.to_owned(), tone), "terminated {reason}");
    }
    // A waiting reason wins over a terminated one of a later container only by order of the containers.
    let both = r#"{"phase":"Running","containerStatuses":[
        {"name":"a","state":{"terminated":{"reason":"Error"}}},
        {"name":"b","state":{"waiting":{"reason":"CrashLoopBackOff"}}}]}"#;
    assert_eq!(status_of(&one_pod("", both)).0, "Error", "the first container with a reason decides, as kubectl does");
    // No reason: terminating when a deletion timestamp is set, else the phase.
    let plain = r#"{"phase":"Running","containerStatuses":[{"name":"app","ready":true,"state":{"running":{}}}]}"#;
    assert_eq!(
        status_of(&one_pod(r#","deletionTimestamp":"2026-01-05T10:00:05Z""#, plain)),
        ("Terminating".to_owned(), Tone::Warn)
    );
    for (phase, tone) in [
        ("Running", Tone::Normal),
        ("Succeeded", Tone::Normal),
        ("Pending", Tone::Warn),
        ("Failed", Tone::Danger),
        ("Unknown", Tone::Warn),
    ] {
        let status = format!(r#"{{"phase":"{phase}"}}"#);
        assert_eq!(status_of(&one_pod("", &status)), (phase.to_owned(), tone), "phase {phase}");
    }
    // A completed container next to a running one is not a completed pod.
    let mixed = r#"{"phase":"Running","containerStatuses":[
        {"name":"init","state":{"terminated":{"reason":"Completed"}}},
        {"name":"app","ready":true,"state":{"running":{}}}]}"#;
    assert_eq!(status_of(&one_pod("", mixed)).0, "Running");
    // No status at all is shown as unknown.
    assert_eq!(status_of(&one_pod("", "null")), ("?".to_owned(), Tone::Warn));
}

#[test]
fn pod_actions_need_valid_names() {
    let model = pods_at(&fixture("kubectl/pods.json"), 1);
    let first = &table(&model).rows()[0];
    assert_eq!(
        actions_of(first),
        [
            ("exec", vec![("container", "app"), ("pod", "web-7d9f")]),
            ("logs", vec![("container", "app"), ("pod", "web-7d9f")]),
            ("delete_pod", vec![("pod", "web-7d9f")]),
        ],
        "a pod row has Exec, Logs and Delete; the first container travels with the first two"
    );
    let effects: Vec<VerbEffect> = first.actions().iter().map(|a| a.effect()).collect();
    assert_eq!(effects, [VerbEffect::Exec, VerbEffect::Read, VerbEffect::Write]);

    let hostile = |name: &str, container: &str| -> Vec<(String, usize)> {
        let name = serde_json::to_string(name).expect("json text");
        let container = serde_json::to_string(container).expect("json text");
        let bytes = format!(
            r#"{{"items":[{{"metadata":{{"name":{name}}},"spec":{{"containers":[{{"name":{container}}}]}},
            "status":{{"phase":"Running"}}}}]}}"#
        );
        let model = pods_at(bytes.as_bytes(), 1);
        table(&model).rows()[0].actions().iter().map(|a| (a.verb().to_owned(), a.args().count())).collect()
    };
    let hostile_names = [
        "bad\u{202e}name",
        "zero\u{200b}width",
        "tab\tname",
        "line\nbreak",
        "esc\u{1b}[31m",
        "--server=x",
        "Upper",
        "a b",
        "x.-y",
    ];
    for name in hostile_names {
        assert!(hostile(name, "app").is_empty(), "a pod named {name:?} must show no action");
    }
    assert_eq!(hostile("good", "app").len(), 3);
    // A first container whose name does not validate takes Exec and Logs away; deleting the valid pod stays possible.
    assert_eq!(hostile("good", "bad\u{200b}c"), [("delete_pod".to_owned(), 1)]);
    // A pod that names no container offers Exec and Logs for its default container.
    let no_container = br#"{"items":[{"metadata":{"name":"good"},"status":{"phase":"Running"}}]}"#;
    let model = pods_at(no_container, 1);
    let row = &table(&model).rows()[0];
    let shapes: Vec<(&str, usize)> = row.actions().iter().map(|a| (a.verb(), a.args().count())).collect();
    assert_eq!(shapes, [("exec", 1), ("logs", 1), ("delete_pod", 1)]);
}

#[test]
fn drift_degrades_to_text_or_an_empty_table_and_never_panics() {
    // An unknown member at every level changes nothing.
    let base = cells(&KUBE.view(verb("contexts"), &fixture("kubectl/config_view.json")).expect("base"));
    let drifted = fixture("kubectl/config_view_unknown_member.json");
    let unknown = KUBE.view(verb("contexts"), &drifted).expect("unknown member");
    assert_eq!(cells(&unknown), base, "unknown members are ignored");
    // A missing list and a wrong type give a plain text that names no byte of the answer.
    for name in ["kubectl/config_view_missing_contexts.json", "kubectl/config_view_wrong_type.json"] {
        let model = KUBE.view(verb("contexts"), &fixture(name)).expect("degrades, not an error");
        let ViewModel::Text(text) = &model else { panic!("{name} gave {model:?} and not a text") };
        assert!(text.note().is_some_and(|note| !note.is_empty()), "{name} has no note");
        let shown = serde_json::to_string(&model).expect("serialises");
        for leak in ["REDACTED", "token", "exec", "eks.example.com", "DATA+OMITTED", "dev-cluster"] {
            assert!(!shown.contains(leak), "{name}: {leak} was shown as text: {shown}");
        }
    }
    // A shape that is valid and empty is an empty table: `kubectl` prints `null` lists for an empty kubeconfig.
    let nothing = br#"{"kind":"Config","clusters":null,"users":null,"contexts":null,"current-context":""}"#;
    let empty = KUBE.view(verb("contexts"), nothing).expect("an empty kubeconfig reads");
    assert!(table(&empty).rows().is_empty());
    assert_eq!(table(&empty).columns().len(), 5);
    for text in [r#"{"contexts":[],"current-context":"x"}"#, r#"{"contexts":null}"#] {
        assert!(table(&KUBE.view(verb("contexts"), text.as_bytes()).expect("reads")).rows().is_empty(), "{text}");
    }
    // A kubeconfig that holds a clear credential and cannot be read shows none of it.
    let leaky = br#"{"users":[{"name":"u","user":{"exec":{"env":[{"name":"K","value":"HW-CANARY-EXEC-ENV"}]}}}],
        "contexts":5}"#;
    let model = KUBE.view(verb("contexts"), leaky).expect("degrades");
    assert!(!serde_json::to_string(&model).expect("serialises").contains("HW-CANARY"), "a clear value was shown");
    // Nothing, too much, and not JSON at all are errors without text, never a panic.
    assert_eq!(KUBE.view(verb("contexts"), b"").unwrap_err(), ParseError::Empty);
    assert_eq!(KUBE.view(verb("contexts"), b" \n").unwrap_err(), ParseError::Empty);
    assert_eq!(kube::parse_contexts(b"not json").unwrap_err(), ParseError::Unrecognised);
    assert_eq!(kube::parse_contexts(b"[]").unwrap_err(), ParseError::Unrecognised);
    assert_eq!(kube::parse_contexts(br#"{"contexts":[]} trailing"#).unwrap_err(), ParseError::Unrecognised);

    // Objects only: a list entry or a member that is an array is never read as its member values in order.
    for text in [
        r#"{"contexts":[["dev",{"cluster":"c","user":"u"}]],"current-context":"dev"}"#,
        r#"{"contexts":[{"name":"dev","context":["c","u"]}]}"#,
        r#"["contexts",[]]"#,
        r#"{"contexts":{"name":"dev"}}"#,
        r#"{"contexts":[{"name":["dev"]}]}"#,
        r#"{"contexts":[{"name":"dev","context":{"cluster":["c"]}}]}"#,
    ] {
        assert_eq!(kube::parse_contexts(text.as_bytes()).unwrap_err(), ParseError::Unrecognised, "{text}");
    }
    // A duplicated member is a refusal and not a silent last-wins.
    assert!(kube::parse_contexts(br#"{"contexts":[],"contexts":[]}"#).is_err());
    // A pod list: a truncated one is shown as a bounded text with a note, a wrong shape too.
    let unreadable = [
        fixture("kubectl/pods_truncated.json"),
        br#"{"items":"x"}"#.to_vec(),
        b"[]".to_vec(),
        b"7".to_vec(),
        br#"{"x":1}"#.to_vec(),
    ];
    for bytes in unreadable {
        let model = pods_at(&bytes, 1);
        let ViewModel::Text(text) = &model else { panic!("{} bytes gave {model:?}", bytes.len()) };
        assert!(text.note().is_some(), "the text says it is the plain output");
        assert!(text.text().len() <= MAX_TEXT);
    }
    assert!(table(&pods_at(br#"{"items":[]}"#, 1)).rows().is_empty());
    assert!(table(&pods_at(br#"{"items":null,"kind":"List"}"#, 1)).rows().is_empty());
    assert_eq!(KUBE.view_at(verb("pods"), b"", at(0)).unwrap_err(), ParseError::Empty);
    // A pod list whose items are arrays, null or numbers is counted, never read positionally.
    let mixed = br#"{"items":[["x"],null,3,{"metadata":{"name":"ok"}},{"metadata":["name"]}]}"#;
    let model = pods_at(mixed, 1);
    assert_eq!(cells(&model)[0][0], "ok");
    assert_eq!(cells(&model)[1][0], "?", "metadata that is an array is no name");
    let note = table(&model).note();
    assert!(note.is_some_and(|note| note.contains('3')), "three entries could not be read: {note:?}");
    // A wrong type in one member is a ? and the list still shows.
    let wrong = br#"{"items":[{"metadata":{"name":5,"creationTimestamp":7},
        "spec":{"nodeName":[],"containers":"x"},"status":{"phase":1}}]}"#;
    assert_eq!(cells(&pods_at(wrong, 1)), [["?", "?", "?", "0", "?", "?"]], "no container list at all is no restarts");
}

#[test]
fn hostile_pods_give_clean_bounded_cells() {
    let model = pods_at(&fixture("kubectl/pods_hostile.json"), 1);
    let list = table(&model);
    assert_eq!(list.rows().len(), 3);
    // The 253-character name stays whole in its cell and its key; it is no pod name a verb accepts, so no action.
    let long = &list.rows()[0];
    assert_eq!(long.key().len(), 253);
    assert_eq!(long.cells().next().expect("name").len(), 253);
    assert!(long.actions().is_empty(), "a 253-character name without dots has a label over 63 characters");
    // Labels and annotations are not read, so neither the escape text nor the 5000 characters is in the model.
    let shown = serde_json::to_string(&model).expect("serialises");
    assert!(!shown.contains("xxxxxxxx"), "the long annotation reached the model");
    assert!(!shown.contains("evil") && !shown.contains("rtl") && !shown.contains("width"), "a label reached the model");
    // A restart count that is a string is a ? and not a failed list.
    let third = &list.rows()[2];
    assert_eq!(third.cells().nth(3), Some("?"));
    assert_eq!(third.cells().next(), Some("unknown-members"));

    // Escape sequences, direction and zero-width characters in the fields that ARE read never come out raw.
    let evil = "evil\u{1b}[31mred\u{1b}]0;title\u{7}\u{202e}rtl\u{200b}zw\u{9b}2Jx\nbreak\ttab";
    let json_text = serde_json::to_string(evil).expect("json text");
    let document = format!(
        r#"{{"items":[{{"metadata":{{"name":{json_text}}},
        "spec":{{"nodeName":{json_text},"containers":[{{"name":{json_text}}}]}},
        "status":{{"phase":{json_text},
        "containerStatuses":[{{"name":"c","state":{{"waiting":{{"reason":{json_text}}}}}}}]}}}}]}}"#
    );
    let model = pods_at(document.as_bytes(), 1);
    let shown = serde_json::to_string(&model).expect("serialises");
    for raw in ["\\u001b", "\\u009b", "\\u202e", "\\u200b", "\\u0007", "\\n", "\\t"] {
        assert!(!shown.contains(raw), "{raw} is in the model: {shown}");
    }
    assert!(table(&model).rows()[0].actions().is_empty(), "a hostile name offers no action");
    for cell in cells(&model).concat() {
        assert!(cell.len() <= MAX_CELL);
    }
    // A name of 100,000 characters is cut at the cell cap, with its key at 253 bytes, and offers no action.
    let big = "n".repeat(100_000);
    let document = format!(r#"{{"items":[{{"metadata":{{"name":"{big}"}}}}]}}"#);
    let model = pods_at(document.as_bytes(), 1);
    let row = &table(&model).rows()[0];
    assert!(row.cells().next().expect("cell").len() <= MAX_CELL);
    assert!(row.key().len() <= 253 && row.actions().is_empty());
    // A list of empty objects cannot make the reader keep more than its cap.
    let many = format!(r#"{{"items":[{}]}}"#, vec!["{}"; 300_000].join(","));
    let model = pods_at(many.as_bytes(), 1);
    assert_eq!(table(&model).rows().len(), 2000);
    // The containers of one pod are bounded too.
    let statuses = vec![r#"{"ready":true}"#; 5000].join(",");
    let document =
        format!(r#"{{"items":[{{"metadata":{{"name":"p"}},"status":{{"containerStatuses":[{statuses}]}}}}]}}"#);
    let model = pods_at(document.as_bytes(), 1);
    assert_eq!(cells(&model)[0][1], "?", "a pod with more containers than are kept does not claim a count");
}

#[test]
fn a_long_pod_list_is_cut_with_a_note() {
    let item = |n: u32| format!(r#"{{"metadata":{{"name":"pod-{n}"}},"status":{{"phase":"Running"}}}}"#);
    let items: Vec<String> = (0..3000).map(item).collect();
    let document = format!(r#"{{"kind":"List","items":[{}]}}"#, items.join(","));
    let model = pods_at(document.as_bytes(), 1);
    let list = table(&model);
    assert_eq!(list.rows().len(), 2000);
    let truncated = list.truncated().expect("a cut list says so");
    assert_eq!(truncated.shown(), 2000);
    assert!(truncated.note().contains("3000"), "{}", truncated.note());
    assert!(model.check().is_ok());
    // The same for contexts.
    let context = |n: u32| format!(r#"{{"name":"c-{n}","context":{{"cluster":"x","user":"y"}}}}"#);
    let contexts: Vec<String> = (0..2500).map(context).collect();
    let document = format!(r#"{{"contexts":[{}],"current-context":"c-1"}}"#, contexts.join(","));
    let model = KUBE.view(verb("contexts"), document.as_bytes()).expect("reads");
    assert_eq!(table(&model).rows().len(), 2000);
    assert!(table(&model).truncated().is_some());
}

// ---------------------------------------------------------------------------------------------------------------------
// The verbs that change or open something, and the hostile names.
// ---------------------------------------------------------------------------------------------------------------------

const ARN: &str = "arn:aws:eks:eu-central-1:000000000000:cluster/demo";

#[test]
fn the_catalogue_is_the_closed_list_of_nine_with_their_effects() {
    let listed: Vec<(&str, VerbEffect, Output, Option<u32>)> =
        KUBE.spec().verbs.iter().map(|v| (v.id, v.effect, v.output, v.refresh_secs)).collect();
    assert_eq!(
        listed,
        [
            ("contexts", VerbEffect::Read, Output::View, None),
            ("namespaces", VerbEffect::Read, Output::View, None),
            ("pods", VerbEffect::Read, Output::View, Some(10)),
            ("use_context", VerbEffect::Local, Output::Done, None),
            ("set_namespace", VerbEffect::Local, Output::Done, None),
            ("delete_pod", VerbEffect::Write, Output::Done, None),
            ("exec", VerbEffect::Exec, Output::Terminal, None),
            ("logs", VerbEffect::Read, Output::Terminal, None),
            ("shell", VerbEffect::Read, Output::Terminal, None),
        ]
    );
    let spec = KUBE.spec();
    assert_eq!((spec.id, spec.label), (ToolId::Kube, "Kubernetes"));
    assert_eq!((spec.binaries, spec.windows_binaries), (&["kubectl"][..], &["kubectl.exe"][..]));
    assert!(spec.env_set.is_empty(), "kubectl gets no environment of its own from the spec");
    assert_eq!(spec.selects, [SelectKind::Context, SelectKind::Namespace]);
    assert!(spec.fixed_dirs.iter().any(|d| *d == "/usr/local/bin") && spec.fixed_dirs.iter().any(|d| d.starts_with('%')));
    // A verb that is not in the list is not found, and one of another spec is refused even under the same id.
    assert!(spec.verb("get_secret").is_none() && spec.verb("").is_none() && spec.verb("Pods").is_none());
    static FOREIGN: Verb = Verb {
        id: "pods",
        label: "Pods",
        effect: VerbEffect::Read,
        output: Output::View,
        args: &[],
        refresh_secs: Some(1),
        exit_is_state: false,
    };
    let none = Validated::parse(&FOREIGN, &BTreeMap::new()).expect("no arguments");
    assert_eq!(KUBE.argv(&FOREIGN, &none, &CheckedSelection::default()), Err(ArgError::Unknown));
    assert_eq!(KUBE.view(&FOREIGN, b"{}").unwrap_err(), ParseError::Unrecognised);
    // A verb that shows nothing has no view.
    assert_eq!(KUBE.view(verb("exec"), b"{}").unwrap_err(), ParseError::Unrecognised);
}

#[test]
fn the_local_and_write_verbs_build_their_exact_argument_lists() {
    assert_eq!(argv_of("use_context", &[("context", ARN)], None, None), ["config", "use-context", "--", ARN]);
    assert_eq!(
        argv_of("use_context", &[("context", "dev")], Some("other"), Some("ns")),
        ["config", "use-context", "--", "dev"],
        "the selection plays no part in switching the context"
    );
    assert_eq!(
        argv_of("set_namespace", &[("namespace", "payments")], Some("dev"), None),
        ["config", "set-context", "--namespace=payments", "--", "dev"]
    );
    assert_eq!(
        argv_of("set_namespace", &[("namespace", "payments")], Some(ARN), Some("old")),
        ["config", "set-context", "--namespace=payments", "--", ARN]
    );
    assert_eq!(
        argv_of("set_namespace", &[("namespace", "payments")], None, None),
        ["config", "set-context", "--current", "--namespace=payments"]
    );
    assert_eq!(
        argv_of("delete_pod", &[("pod", "web-1")], Some("dev"), Some("payments")),
        [
            "delete",
            "pod",
            "--context=dev",
            "--namespace=payments",
            "--wait=false",
            "--request-timeout=10s",
            "--",
            "web-1"
        ]
    );
    assert_eq!(
        argv_of("delete_pod", &[("pod", "web-1")], None, None),
        ["delete", "pod", "--wait=false", "--request-timeout=10s", "--", "web-1"]
    );
    // An argument bag that belongs to another verb has no pod, no context and no namespace: refused, not guessed.
    let nothing = args("contexts", &[]);
    for id in ["use_context", "set_namespace", "delete_pod", "exec", "logs"] {
        assert_eq!(KUBE.argv(verb(id), &nothing, &CheckedSelection::default()), Err(ArgError::Missing), "{id}");
    }
}

#[test]
fn exec_logs_and_shell_build_their_exact_argument_lists() {
    let shell = kube::POD_SHELL;
    assert_eq!(shell, "command -v bash >/dev/null 2>&1 && exec bash || exec sh");
    assert_eq!(
        argv_of("exec", &[("pod", "web-1"), ("container", "app")], Some("dev"), Some("payments")),
        [
            "exec",
            "-i",
            "-t",
            "--context=dev",
            "--namespace=payments",
            "--container=app",
            "web-1",
            "--",
            "sh",
            "-c",
            shell
        ]
    );
    assert_eq!(argv_of("exec", &[("pod", "web-1")], None, None), ["exec", "-i", "-t", "web-1", "--", "sh", "-c", shell]);
    assert_eq!(
        argv_of("logs", &[("pod", "web-1"), ("container", "app")], Some("dev"), Some("payments")),
        [
            "logs",
            "--follow",
            "--tail=200",
            "--context=dev",
            "--namespace=payments",
            "--container=app",
            "--",
            "web-1"
        ]
    );
    assert_eq!(argv_of("logs", &[("pod", "web-1")], None, None), ["logs", "--follow", "--tail=200", "--", "web-1"]);
    assert_eq!(
        argv_of("logs", &[("pod", "web-1"), ("lines", "5000")], None, Some("ns")),
        ["logs", "--follow", "--tail=5000", "--namespace=ns", "--", "web-1"]
    );
    assert_eq!(argv_of("logs", &[("pod", "web-1"), ("lines", "1")], None, None)[2], "--tail=1");
    for lines in ["0", "5001", "-1", "+5", "007", "1e3", "", " 5", "5 ", "10\n"] {
        let pairs = BTreeMap::from([("pod".to_owned(), "web-1".to_owned()), ("lines".to_owned(), lines.to_owned())]);
        assert!(Validated::parse(verb("logs"), &pairs).is_err(), "{lines:?} is no line count");
    }
    assert!(argv_of("shell", &[], Some("dev"), Some("ns")).is_empty(), "the shell is the person's own, with the overlay");
}

/// Inputs that try to become an option, a second argument or a spoof; some are valid names for some kinds.
fn corpus() -> Vec<String> {
    let mut out: Vec<String> = [
        "-",
        "--",
        "-x",
        "-v=9",
        "--server=https://x",
        "--as=admin",
        "--as-group=system:masters",
        "--token=abc",
        "--raw",
        "--kubeconfig=/etc/passwd",
        "--insecure-skip-tls-verify",
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
        "x/../y",
        "",
        " ",
        " a",
        "a ",
        "UPPER",
        "a_b",
        "a.b",
        "a..b",
        "dev",
        "kind-test",
        "user@cluster",
        ARN,
        "gke_proj_europe-west1_cluster",
        "web-1",
        "app",
        "default",
        "5000",
        "0",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    for length in [62, 63, 64, 252, 253, 254, 255, 4096, 4097] {
        out.push("a".repeat(length));
    }
    out.push(format!("a.{}", "b".repeat(63)));
    out.push(format!("a.{}", "b".repeat(64)));
    out.push(format!("-{}", "a".repeat(10)));
    out
}

/// The closed set of argument elements that start with a dash, and the prefixes of the equals forms.
const FIXED: [&str; 18] = [
    "config",
    "view",
    "get",
    "namespaces",
    "pods",
    "use-context",
    "set-context",
    "delete",
    "pod",
    "exec",
    "logs",
    "json",
    "sh",
    "-o",
    "-i",
    "-t",
    "--",
    "-c",
];
const FIXED_FLAGS: [&str; 5] = ["--current", "--wait=false", "--request-timeout=10s", "--follow", "--tail=200"];
const PREFIXES: [&str; 4] = ["--context=", "--namespace=", "--container=", "--tail="];

/// What a list of arguments may hold: every element is a fixed word, a fixed flag, an equals form whose value is one of
/// the `names` (or a line count), or a name that follows `--` (or, for `exec`, the one positional before it).
fn assert_shape(argv: &[String], names: &[&str], label: &str) {
    let dashes = argv.iter().position(|a| a == "--");
    for (at, element) in argv.iter().enumerate() {
        let fixed = FIXED.contains(&element.as_str()) || FIXED_FLAGS.contains(&element.as_str()) || element == kube::POD_SHELL;
        if fixed {
            continue;
        }
        if let Some(prefix) = PREFIXES.iter().find(|p| element.starts_with(**p)) {
            let value = &element[prefix.len()..];
            let ok = names.contains(&value) || (*prefix == "--tail=" && value.bytes().all(|b| b.is_ascii_digit()));
            assert!(ok, "{label}: {element:?} carries a value that no name of the call has: {argv:?}");
            continue;
        }
        assert!(!element.starts_with('-'), "{label}: the element {element:?} starts with a dash and is no flag of ours: {argv:?}");
        assert!(names.contains(&element.as_str()), "{label}: {element:?} is no name of the call: {argv:?}");
        let after_dashes = dashes.is_some_and(|d| at == d + 1 && argv[0] != "exec");
        let exec_pod = argv[0] == "exec" && dashes.is_some_and(|d| at + 1 == d);
        assert!(after_dashes || exec_pod, "{label}: the name {element:?} is not where a name belongs: {argv:?}");
    }
    // No name is in more places than it has roles in the call.
    for name in names {
        let roles = names.iter().filter(|other| *other == name).count();
        let holders = argv.iter().filter(|e| e == name || PREFIXES.iter().any(|p| e.strip_prefix(p) == Some(*name))).count();
        assert!(holders <= roles, "{label}: {name:?} appears {holders} times for {roles} roles: {argv:?}");
    }
}

/// Every way a text can be put into a verb call: (label, verb, arguments, context, namespace).
type Call = (String, &'static str, Vec<(&'static str, String)>, Option<String>, Option<String>);

fn calls_with(text: &str) -> Vec<Call> {
    let t = || text.to_owned();
    let mut calls: Vec<Call> = Vec::new();
    let mut add = |label: &str, id: &'static str, pairs: Vec<(&'static str, String)>, c: Option<String>, n: Option<String>| {
        calls.push((format!("{label} {text:?}"), id, pairs, c, n));
    };
    add("use_context arg", "use_context", vec![("context", t())], None, None);
    add("set_namespace arg", "set_namespace", vec![("namespace", t())], None, None);
    add("set_namespace arg with context", "set_namespace", vec![("namespace", t())], Some("dev".into()), None);
    add("set_namespace selection", "set_namespace", vec![("namespace", "ns".into())], Some(t()), None);
    for id in ["namespaces", "pods"] {
        add(&format!("{id} context"), id, vec![], Some(t()), None);
        add(&format!("{id} namespace"), id, vec![], None, Some(t()));
    }
    for id in ["delete_pod", "exec", "logs"] {
        add(&format!("{id} pod"), id, vec![("pod", t())], None, None);
        add(&format!("{id} context"), id, vec![("pod", "web-1".into())], Some(t()), None);
        add(&format!("{id} namespace"), id, vec![("pod", "web-1".into())], None, Some(t()));
    }
    for id in ["exec", "logs"] {
        add(&format!("{id} container"), id, vec![("pod", "web-1".into()), ("container", t())], None, None);
    }
    add("logs lines", "logs", vec![("pod", "web-1".into()), ("lines", t())], None, None);
    calls
}

/// Builds a call; `None` when a validator refused the text before an argument list existed.
fn try_build(id: &str, pairs: &[(&str, String)], context: Option<&str>, namespace: Option<&str>) -> Option<Vec<String>> {
    let map: BTreeMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect();
    let validated = Validated::parse(verb(id), &map).ok()?;
    let selection = Selection {
        context: context.map(str::to_owned),
        namespace: namespace.map(str::to_owned),
        ..Selection::default()
    }
    .check(ToolId::Kube)
    .ok()?;
    Some(texts(&KUBE.argv(verb(id), &validated, &selection).expect("a validated call builds")))
}

#[test]
fn a_hostile_name_never_changes_the_kubectl_argument_list() {
    let (mut accepted, mut refused) = (0, 0);
    for text in corpus() {
        for (label, id, pairs, context, namespace) in calls_with(&text) {
            match try_build(id, &pairs, context.as_deref(), namespace.as_deref()) {
                None => refused += 1,
                Some(argv) => {
                    accepted += 1;
                    let mut names: Vec<&str> = pairs.iter().map(|(_, v)| v.as_str()).collect();
                    names.extend(context.as_deref());
                    names.extend(namespace.as_deref());
                    assert_shape(&argv, &names, &label);
                    // A name that starts with a dash is never accepted, so it is never an element of the list.
                    assert!(!text.starts_with('-') || !argv.contains(&text), "{label}: a dash name became an argument");
                }
            }
        }
    }
    assert!(accepted > 150 && refused > 600, "the corpus must exercise both sides: {accepted} accepted, {refused} refused");

    // Spot checks of what is refused and what is whole.
    for bad in ["--server=https://x", "-x", "a\nb", "a\u{202e}b", "a\u{200b}b", &"a".repeat(254)] {
        assert!(try_build("pods", &[], Some(bad), None).is_none(), "context {bad:?} was accepted");
        assert!(try_build("delete_pod", &[("pod", bad.to_owned())], None, None).is_none(), "pod {bad:?} was accepted");
    }
    let spaced = try_build("pods", &[], Some("a b"), None).expect("a space inside a context name is allowed");
    assert_eq!(spaced[4], "--context=a b", "a name with a space is one argument");
    let argument = try_build("use_context", &[("context", "a=b".to_owned())], None, None).expect("equals sign is allowed");
    assert_eq!(argument, ["config", "use-context", "--", "a=b"]);

    // Generated names: whatever mix of dashes, equals signs, quotes and control characters, an accepted list keeps its shape.
    let mut rng = common::Xorshift64::new(0x6b75_6265);
    let alphabet: Vec<char> = "-=aZ09_. /@:'\"$`|&;<>\n\t\u{0}\u{1b}\u{202e}\u{200b}\u{a0}\u{e9}".chars().collect();
    for _ in 0..4000 {
        let length = 1 + rng.below(12);
        let name: String = (0..length).map(|_| alphabet[rng.below(alphabet.len())]).collect();
        for (label, id, pairs, context, namespace) in calls_with(&name) {
            if let Some(argv) = try_build(id, &pairs, context.as_deref(), namespace.as_deref()) {
                let mut names: Vec<&str> = pairs.iter().map(|(_, v)| v.as_str()).collect();
                names.extend(context.as_deref());
                names.extend(namespace.as_deref());
                assert_shape(&argv, &names, &label);
            }
        }
    }
}

#[test]
fn no_argument_list_holds_a_forbidden_flag() {
    const FORBIDDEN: [&str; 9] =
        ["--raw", "-v", "--v", "--token", "--server", "--kubeconfig", "--insecure-skip-tls-verify", "--as", "--as-group"];
    let mut lists = 0;
    let mut check = |argv: &[String], label: &str| {
        lists += 1;
        for element in argv {
            for flag in FORBIDDEN {
                let equals = format!("{flag}=");
                assert!(element != flag && !element.starts_with(&equals), "{label}: {flag} is in {argv:?}");
            }
            assert!(!element.starts_with("-v") || element.starts_with("--"), "{label}: a verbosity form {element:?}");
            assert!(!element.contains("kubeconfig") || element.starts_with("--context="), "{label}: kubeconfig in {element:?}");
        }
    };
    // Every verb, with every valid name of the corpus in every role and with no name at all.
    for text in corpus() {
        for (label, id, pairs, context, namespace) in calls_with(&text) {
            if let Some(argv) = try_build(id, &pairs, context.as_deref(), namespace.as_deref()) {
                check(&argv, &label);
            }
        }
    }
    for verb_listed in KUBE.spec().verbs {
        for (context, namespace) in [(None, None), (Some("dev"), None), (None, Some("ns")), (Some(ARN), Some("ns"))] {
            let id = verb_listed.id;
            let pairs: Vec<(&str, String)> = verb_listed
                .args
                .iter()
                .map(|spec| (spec.name, if spec.name == "lines" { "10".to_owned() } else { "dev".to_owned() }))
                .collect();
            let pairs: Vec<(&str, String)> =
                pairs.into_iter().map(|(n, v)| (n, if n == "container" || n == "pod" { "web-1".into() } else { v })).collect();
            let argv = try_build(id, &pairs, context, namespace).unwrap_or_else(|| panic!("{id} with valid names was refused"));
            check(&argv, id);
        }
    }
    assert!(lists > 300, "the sweep looked at {lists} argument lists");
    // The module never names a forbidden flag as an argument of its own: the sweep above is the proof for what it builds,
    // and this holds the source to it.
    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/integrate/kube.rs")).expect("source");
    let code = source.split("#[cfg(test)]").next().expect("code");
    for flag in ["\"--raw\"", "\"--token", "\"--server", "\"--kubeconfig", "\"--insecure-skip-tls-verify", "\"--as", "\"-v"] {
        let hits = code.lines().filter(|l| !l.trim_start().starts_with("//") && l.contains(flag)).count();
        assert_eq!(hits, 0, "the source builds {flag}");
    }
}

#[test]
fn the_pod_shell_is_the_one_constant() {
    let shell = kube::POD_SHELL;
    // Every list of every verb: the constant appears only as the last element of `exec`, after `sh` and `-c`.
    for verb_listed in KUBE.spec().verbs {
        let pairs: Vec<(&str, String)> = verb_listed
            .args
            .iter()
            .map(|spec| (spec.name, if spec.name == "lines" { "10".to_owned() } else { "web-1".to_owned() }))
            .collect();
        let argv = try_build(verb_listed.id, &pairs, Some("dev"), Some("ns")).expect("builds");
        let holders = argv.iter().filter(|e| e.contains("bash") || e.contains("exec bash") || *e == shell).count();
        if verb_listed.id == "exec" {
            assert_eq!(holders, 1);
            assert_eq!(&argv[argv.len() - 4..], ["--", "sh", "-c", shell]);
            assert_eq!(argv.iter().filter(|e| e.as_str() == "sh").count(), 1);
        } else {
            assert_eq!(holders, 0, "{}: a shell string outside exec", verb_listed.id);
            assert!(!argv.iter().any(|e| e == "sh" || e == "-c"), "{}: {argv:?}", verb_listed.id);
        }
    }
    // The constant has no placeholder and no name in it, and no name can reach it: it is a constant.
    assert!(!shell.contains('{') && !shell.contains('$') && !shell.contains('`'));
    // The source: one literal of it, no process started, no program named, nothing but the constant is a shell string.
    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/integrate/kube.rs")).expect("source");
    let code = source.split("#[cfg(test)]").next().expect("code");
    let code_lines: Vec<&str> = code.lines().filter(|l| !l.trim_start().starts_with("//")).collect();
    assert_eq!(code_lines.iter().filter(|l| l.contains("\"command -v bash")).count(), 1, "the constant is written once");
    assert!(code_lines.iter().filter(|l| l.contains("POD_SHELL")).count() >= 2);
    for forbidden in ["Command::", "std::process", "/bin/sh", "cmd.exe", "format!(\"sh", "sh -c"] {
        assert!(!code_lines.iter().any(|l| l.contains(forbidden)), "the module holds {forbidden}");
    }
}

/// Keeps the helpers of later tasks used while the file grows.
#[allow(dead_code)]
fn scratch(tag: &str) -> PathBuf {
    common::scratch_dir(tag)
}
