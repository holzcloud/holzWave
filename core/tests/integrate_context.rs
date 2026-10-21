//! The context of an integration: where a selection lives, how it becomes the arguments and the environment of a run,
//! and when a context counts as production.
//!
//! Nothing here starts a process. A selection travels in the argument list of a command, a shell pane gets a small
//! overlay file below the data directory, and a tool's own files are never written; the tests hold each of those to
//! the byte.

mod common;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;

use holzwave_core::integrate::args::ArgError;
use holzwave_core::integrate::context::{resolve, ContextError, ContextState, MAX_BYTES, MAX_PANES, PANES_FILE};
use holzwave_core::integrate::kube::KUBE;
use holzwave_core::integrate::profiles::IntegrationProfiles;
use holzwave_core::integrate::{Integration, Selection, ToolId, Validated};
use serde_json::json;

const PROD_ARN: &str = "arn:aws:eks:eu-central-1:000000000000:cluster/prod";

/// A set of profiles from the documents the loader reads.
fn profiles_from(items: serde_json::Value) -> IntegrationProfiles {
    let text = json!({ "version": 1, "profiles": items }).to_string();
    IntegrationProfiles::parse(&text).unwrap_or_else(|note| panic!("the test profiles must load: {note}"))
}

/// One production profile that pins a cluster and a namespace, as in the field.
fn prod_profiles() -> IntegrationProfiles {
    profiles_from(json!([
        { "id": "prod", "tool": "kube", "name": "Production", "context": PROD_ARN, "namespace": "payments", "production": true },
        { "id": "dev", "tool": "kube", "name": "Dev", "context": "kind-dev", "namespace": "default" },
        { "id": "acme", "tool": "azure", "name": "Acme", "subscription": "00000000-0000-0000-0000-000000000001" },
    ]))
}

fn pick(profile: Option<&str>, context: Option<&str>, namespace: Option<&str>) -> Selection {
    Selection {
        profile: profile.map(str::to_owned),
        context: context.map(str::to_owned),
        namespace: namespace.map(str::to_owned),
        ..Selection::default()
    }
}

fn texts(argv: &[OsString]) -> Vec<String> {
    argv.iter().map(|a| a.to_str().expect("every argument is text").to_owned()).collect()
}

/// The argument list of the `pods` verb for a resolved selection.
fn pods_argv(resolved: &holzwave_core::integrate::context::Resolved) -> Vec<String> {
    let verb = KUBE.spec().verb("pods").expect("the pods verb");
    let args = Validated::parse(verb, &BTreeMap::new()).expect("the pods verb takes no argument");
    texts(&KUBE.argv(verb, &args, &resolved.selection).expect("an argument list"))
}

fn is_empty_dir(dir: &Path) -> bool {
    fs::read_dir(dir).expect("the scratch directory").next().is_none()
}

// ---------------------------------------------------------------------------------------------------------------------
// The tracer: a pinned selection, end to end, writing nothing.
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn a_pinned_selection_becomes_flags_in_the_argument_list_and_writes_no_file() {
    let scratch = common::scratch_dir("context-pin");
    let profiles = prod_profiles();

    let resolved = resolve(&profiles, ToolId::Kube, &pick(Some("prod"), None, None)).expect("the pin resolves");
    assert_eq!(resolved.tool, ToolId::Kube);
    assert_eq!(resolved.profile.as_ref().map(|p| p.as_str()), Some("prod"));
    assert_eq!(resolved.selection.context.as_ref().map(|c| c.as_str()), Some(PROD_ARN));
    assert_eq!(resolved.selection.namespace.as_ref().map(|n| n.as_str()), Some("payments"));

    assert_eq!(
        pods_argv(&resolved),
        [
            "get".to_owned(),
            "pods".into(),
            "-o".into(),
            "json".into(),
            format!("--context={PROD_ARN}"),
            "--namespace=payments".into(),
            "--request-timeout=10s".into(),
        ],
        "the pin is two flags in the exact argument list"
    );
    assert!(is_empty_dir(&scratch), "resolving wrote nothing");
    fs::remove_dir_all(&scratch).ok();
}

#[test]
fn an_explicit_field_wins_over_the_pin() {
    let profiles = prod_profiles();

    let resolved = resolve(&profiles, ToolId::Kube, &pick(Some("prod"), None, Some("other"))).expect("resolves");
    assert_eq!(resolved.selection.namespace.as_ref().map(|n| n.as_str()), Some("other"));
    assert_eq!(resolved.selection.context.as_ref().map(|c| c.as_str()), Some(PROD_ARN), "the context stays pinned");
    let argv = pods_argv(&resolved);
    assert!(argv.contains(&format!("--context={PROD_ARN}")) && argv.contains(&"--namespace=other".to_owned()));

    // Another context in the selection replaces the pinned one and keeps the pinned namespace.
    let resolved = resolve(&profiles, ToolId::Kube, &pick(Some("prod"), Some("kind-dev"), None)).expect("resolves");
    assert_eq!(resolved.selection.context.as_ref().map(|c| c.as_str()), Some("kind-dev"));
    assert_eq!(resolved.selection.namespace.as_ref().map(|n| n.as_str()), Some("payments"));
}

#[test]
fn an_unknown_profile_or_a_foreign_field_is_refused() {
    let profiles = prod_profiles();
    let hostile = "ghost\u{1b}[31m-profile";

    let unknown = resolve(&profiles, ToolId::Kube, &pick(Some("ghost"), None, None)).expect_err("no such profile");
    assert_eq!(unknown, ContextError::UnknownProfile);
    assert_eq!(unknown.to_string(), "the profile does not exist", "a fixed sentence");

    // A value that is no profile id at all is refused by the id's own rule, and the sentence holds none of it.
    let shaped = resolve(&profiles, ToolId::Kube, &pick(Some(hostile), None, None)).expect_err("not an id");
    assert!(!shaped.to_string().contains("ghost"), "no byte of the input in the sentence");

    // A subscription is not a field of a Kubernetes selection.
    let foreign = Selection { subscription: Some("00000000-0000-0000-0000-000000000001".into()), ..Selection::default() };
    assert_eq!(
        resolve(&profiles, ToolId::Kube, &foreign).expect_err("not for the tool"),
        ContextError::Selection(ArgError::NotForTool)
    );
    // A name that would become an option is refused by the validators.
    assert_eq!(
        resolve(&profiles, ToolId::Kube, &pick(None, Some("--server=evil"), None)).expect_err("a dash"),
        ContextError::Selection(ArgError::LeadingDash)
    );
    // A profile of another tool is not a pin for this one.
    assert_eq!(
        resolve(&profiles, ToolId::Azure, &pick(Some("prod"), None, None)).expect_err("another tool's profile"),
        ContextError::WrongTool
    );
}

// ---------------------------------------------------------------------------------------------------------------------
// The state: picks, panes, the sequence number and the file.
// ---------------------------------------------------------------------------------------------------------------------

fn ctx(context: &str) -> Selection {
    pick(None, Some(context), None)
}

#[test]
fn the_state_can_be_shared_between_threads() {
    fn shared<T: Send + Sync>() {}
    shared::<ContextState>();

    let state = std::sync::Arc::new(ContextState::default());
    let workers: Vec<_> = (0..8u64)
        .map(|n| {
            let state = std::sync::Arc::clone(&state);
            std::thread::spawn(move || {
                state.set_pane(10 + n, ToolId::Kube, "pods", &ctx(&format!("ctx-{n}"))).expect("a valid selection")
            })
        })
        .collect();
    for worker in workers {
        assert!(worker.join().expect("no panic"), "each pane stored a new value");
    }
    assert_eq!(state.seq(), 8, "one step per change, none lost");
    assert_eq!(state.panes_of(ToolId::Kube).len(), 8);
}

#[test]
fn a_pane_selection_is_its_own() {
    let state = ContextState::default();
    state.set_pane(3, ToolId::Kube, "pods", &pick(None, Some("kind-a"), Some("one"))).expect("pane 3");
    state.set_pane(4, ToolId::Kube, "pods", &pick(None, Some("kind-b"), None)).expect("pane 4");
    let before_four = state.selection_for(ToolId::Kube, Some(4));
    let before_pick = state.pick(ToolId::Kube);

    state.set_pane(3, ToolId::Kube, "pods", &pick(None, Some("kind-c"), Some("two"))).expect("pane 3 again");

    assert_eq!(state.selection_for(ToolId::Kube, Some(3)), pick(None, Some("kind-c"), Some("two")));
    assert_eq!(state.selection_for(ToolId::Kube, Some(4)), before_four, "another pane does not move");
    assert_eq!(state.pick(ToolId::Kube), before_pick, "the pick does not move");
    assert_eq!(before_pick, None);
    assert_eq!(state.panes_of(ToolId::Kube), [3, 4]);
    assert!(state.panes_of(ToolId::Talos).is_empty());

    let held = state.pane(3).expect("pane 3 has an entry");
    assert_eq!((held.tool, held.view.as_str()), (ToolId::Kube, "pods"));

    // The selection must pass the tool's own check, and the view must be a view the tool has.
    let foreign = Selection { subscription: Some("00000000-0000-0000-0000-000000000001".into()), ..Selection::default() };
    assert_eq!(
        state.set_pane(5, ToolId::Kube, "pods", &foreign),
        Err(ContextError::Selection(ArgError::NotForTool))
    );
    for view in ["delete_pod", "exec", "use_context", "nonsense", ""] {
        assert_eq!(
            state.set_pane(5, ToolId::Kube, view, &ctx("kind-a")),
            Err(ContextError::UnknownView),
            "{view:?} is not a read view of the tool"
        );
    }
    assert!(state.pane(5).is_none(), "a refused selection stores nothing");
}

#[test]
fn the_sequence_moves_only_on_a_real_change() {
    let state = ContextState::default();
    assert_eq!(state.seq(), 0);
    assert_eq!(state.set_pane(3, ToolId::Kube, "pods", &ctx("a")), Ok(true));
    assert_eq!(state.seq(), 1);
    assert_eq!(state.set_pane(3, ToolId::Kube, "pods", &ctx("a")), Ok(false), "an equal value is no change");
    assert_eq!(state.seq(), 1);
    assert_eq!(state.set_pane(3, ToolId::Kube, "namespaces", &ctx("a")), Ok(true), "another view is a change");
    assert_eq!(state.set_pane(3, ToolId::Kube, "namespaces", &ctx("b")), Ok(true));
    assert_eq!(state.seq(), 3);
    assert!(state.forget_pane(3));
    assert_eq!(state.seq(), 4);
    assert!(!state.forget_pane(3), "there was nothing to forget");
    assert_eq!(state.seq(), 4);
    assert_eq!(state.set_pick(ToolId::Kube, &ctx("a")), Ok(true));
    assert_eq!(state.set_pick(ToolId::Kube, &ctx("a")), Ok(false));
    assert_eq!(state.seq(), 5);
    // A refused change does not move it either.
    assert!(state.set_pane(3, ToolId::Kube, "exec", &ctx("a")).is_err());
    assert_eq!(state.seq(), 5);
}

#[test]
fn a_pick_is_the_start_of_a_new_block_and_is_not_persisted() {
    let scratch = common::scratch_dir("context-pick");
    let state = ContextState::default();
    state.set_pick(ToolId::Kube, &pick(None, Some("kind-pick"), Some("team"))).expect("a valid pick");
    assert_eq!(state.selection_for(ToolId::Kube, None), pick(None, Some("kind-pick"), Some("team")));
    assert_eq!(
        state.selection_for(ToolId::Kube, Some(9)),
        pick(None, Some("kind-pick"), Some("team")),
        "a pane with no selection of its own starts from the pick"
    );
    assert_eq!(state.selection_for(ToolId::Azure, None), Selection::default(), "another tool has no pick");
    assert!(state.panes_of(ToolId::Kube).is_empty(), "a pick makes no pane");

    // A pane with a selection of its own does not follow the pick, and a pane of another tool is not this tool's.
    state.set_pane(3, ToolId::Kube, "pods", &ctx("kind-own")).expect("pane 3");
    state.set_pick(ToolId::Kube, &ctx("kind-later")).expect("a later pick");
    assert_eq!(state.selection_for(ToolId::Kube, Some(3)), ctx("kind-own"));
    assert_eq!(state.selection_for(ToolId::Azure, Some(3)), Selection::default());

    // The pick is not a field of the file, and a new window starts without it.
    state.save(&scratch).expect("saved");
    let text = fs::read_to_string(scratch.join(PANES_FILE)).expect("the file");
    assert!(!text.contains("kind-later") && !text.contains("kind-pick"), "{text}");
    let (restored, notes) = ContextState::load(&scratch, &[3]);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(restored.pick(ToolId::Kube), None);

    assert_eq!(
        state.set_pick(ToolId::Kube, &Selection { section: Some("work".into()), ..Selection::default() }),
        Err(ContextError::Selection(ArgError::NotForTool))
    );
    fs::remove_dir_all(&scratch).ok();
}

#[test]
fn the_pane_file_round_trips_and_restores_live_panes_only() {
    let scratch = common::scratch_dir("context-file");
    let state = ContextState::default();
    state.set_pane(3, ToolId::Kube, "pods", &pick(Some("prod"), Some(PROD_ARN), Some("payments"))).expect("3");
    state.set_pane(4, ToolId::Kube, "namespaces", &ctx("kind-a")).expect("4");
    state
        .set_pane(5, ToolId::Azure, "subscriptions", &Selection {
            subscription: Some("00000000-0000-0000-0000-000000000001".into()),
            ..Selection::default()
        })
        .expect("5");
    state.save(&scratch).expect("saved");

    let text = fs::read_to_string(scratch.join(PANES_FILE)).expect("the file");
    let document: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(document["version"], 1);
    assert_eq!(
        document["panes"]["3"],
        json!({ "tool": "kube", "view": "pods", "selection": { "profile": "prod", "context": PROD_ARN, "namespace": "payments" } })
    );
    assert_eq!(document.as_object().expect("an object").len(), 2, "version and panes, nothing else");

    let before: Vec<_> = fs::read_dir(&scratch).expect("dir").map(|e| e.expect("entry").file_name()).collect();
    let (restored, notes) = ContextState::load(&scratch, &[3, 5, 99]);
    assert!(notes.is_empty(), "a pane that is gone is not a fault: {notes:?}");
    assert_eq!(restored.pane(3), state.pane(3));
    assert_eq!(restored.pane(5), state.pane(5));
    assert!(restored.pane(4).is_none(), "pane 4 is not live");
    assert!(restored.pane(99).is_none());
    assert_eq!(restored.seq(), 0, "a restored state starts a new count");
    let after: Vec<_> = fs::read_dir(&scratch).expect("dir").map(|e| e.expect("entry").file_name()).collect();
    assert_eq!(before, after, "loading wrote nothing and started nothing: no run directory, no overlay");

    // A missing file is an empty state without a note.
    let empty = common::scratch_dir("context-file-none");
    let (none, notes) = ContextState::load(&empty, &[3]);
    assert!(notes.is_empty() && none.panes_of(ToolId::Kube).is_empty());
    fs::remove_dir_all(&scratch).ok();
    fs::remove_dir_all(&empty).ok();
}

#[test]
fn bad_entries_are_dropped_with_notes_and_a_damaged_file_is_quarantined() {
    let scratch = common::scratch_dir("context-bad");
    let hostile = "dock\u{1b}[31mer";
    let good = json!({ "tool": "kube", "view": "pods", "selection": { "context": "kind-a" } });
    let document = json!({
        "version": 1,
        "panes": {
            "3": good,
            "4": { "tool": hostile, "view": "pods", "selection": {} },
            "5": { "tool": "kube", "view": "delete_pod", "selection": {} },
            "6": { "tool": "kube", "view": "pods", "selection": { "subscription": "00000000-0000-0000-0000-000000000001" } },
            "x": good,
            "7": { "tool": "kube", "view": "pods", "selection": { "evil\u{1b}": "member" } },
            "8": "not an object",
            "9": { "tool": "kube", "view": "pods", "selection": {}, "extra": 1 },
            "10": { "tool": "kube", "view": "pods", "selection": { "context": "--server=evil" } },
            "11": { "tool": "kube", "view": "pods" },
            "012": good
        }
    });
    fs::write(scratch.join(PANES_FILE), document.to_string()).expect("written");

    let live: Vec<u64> = (0..=12).collect();
    let (state, notes) = ContextState::load(&scratch, &live);
    assert_eq!(state.panes_of(ToolId::Kube), [3], "only the well-formed entry is restored");
    assert_eq!(notes.len(), 10, "one note per dropped entry: {notes:?}");
    for note in &notes {
        assert!(!note.contains("dock") && !note.contains("evil") && !note.contains("kind-a"), "no value in {note:?}");
        assert!(!note.chars().any(char::is_control), "{note:?}");
    }
    assert!(!scratch.join("pane_integrations.json.bad").exists(), "an entry that is dropped is not a damaged file");

    // A file that is damaged as a whole starts an empty state, says so once and keeps a copy.
    let valid = json!({ "version": 1, "panes": { "3": good } });
    let mut too_many = serde_json::Map::new();
    for pane in 0..=MAX_PANES {
        too_many.insert(pane.to_string(), good.clone());
    }
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("not JSON", b"{ this is not json".to_vec()),
        ("an array", b"[1, 2]".to_vec()),
        ("another version", json!({ "version": 2, "panes": {} }).to_string().into_bytes()),
        ("a string version", json!({ "version": "1", "panes": {} }).to_string().into_bytes()),
        ("an unknown member", json!({ "version": 1, "panes": {}, "extra": true }).to_string().into_bytes()),
        ("panes as a list", json!({ "version": 1, "panes": [] }).to_string().into_bytes()),
        ("too many panes", json!({ "version": 1, "panes": too_many }).to_string().into_bytes()),
        ("over 64 KiB", format!("{}{}", valid, " ".repeat(MAX_BYTES as usize)).into_bytes()),
    ];
    for (label, bytes) in cases {
        let _ = fs::remove_file(scratch.join("pane_integrations.json.bad"));
        fs::write(scratch.join(PANES_FILE), &bytes).expect("written");
        let (state, notes) = ContextState::load(&scratch, &live);
        assert!(state.panes_of(ToolId::Kube).is_empty(), "{label}: an empty state");
        assert_eq!(notes.len(), 1, "{label}: one note: {notes:?}");
        assert!(!notes[0].contains("this is not json") && !notes[0].contains("kind-a"), "{label}: {notes:?}");
        let bad = fs::read(scratch.join("pane_integrations.json.bad")).unwrap_or_else(|e| panic!("{label}: no copy: {e}"));
        assert_eq!(bad, bytes, "{label}: the copy holds what was there");
    }
    fs::remove_dir_all(&scratch).ok();
}

#[test]
fn the_state_refuses_more_panes_than_the_file_may_hold() {
    let scratch = common::scratch_dir("context-cap");
    let state = ContextState::default();
    let long = |n: usize| format!("{n:03}{}", "c".repeat(250));
    let mut stored = 0;
    let mut refused = None;
    for pane in 0..(MAX_PANES as u64 + 10) {
        match state.set_pane(pane, ToolId::Kube, "pods", &ctx(&long(pane as usize))) {
            Ok(_) => stored += 1,
            Err(error) => {
                refused = Some(error);
                break;
            }
        }
    }
    assert_eq!(refused, Some(ContextError::TooMany), "the long names fill the file before the count does");
    assert!(stored > 100 && stored < MAX_PANES, "{stored}");
    state.save(&scratch).expect("saved");
    assert!(fs::metadata(scratch.join(PANES_FILE)).expect("file").len() <= MAX_BYTES);
    let live: Vec<u64> = (0..stored as u64).collect();
    let (restored, notes) = ContextState::load(&scratch, &live);
    assert!(notes.is_empty(), "what is written always loads again: {notes:?}");
    assert_eq!(restored.panes_of(ToolId::Kube).len(), stored);

    // An existing pane can still change; the count is of panes.
    assert!(state.set_pane(0, ToolId::Kube, "namespaces", &ctx("short")).is_ok());

    let counted = ContextState::default();
    for pane in 0..MAX_PANES as u64 {
        counted.set_pane(pane, ToolId::Kube, "pods", &ctx("a")).expect("within the count");
    }
    assert_eq!(counted.set_pane(MAX_PANES as u64, ToolId::Kube, "pods", &ctx("a")), Err(ContextError::TooMany));
    fs::remove_dir_all(&scratch).ok();
}
