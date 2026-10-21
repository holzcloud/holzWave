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
use std::path::{Path, PathBuf};

use holzwave_core::integrate::args::ArgError;
use holzwave_core::integrate::context::{
    apply_env, resolve, summary, unsafe_path_reason, Applied, AppliedFor, ContextError, ContextRow, ContextState,
    Detection, RowState, MAX_BYTES, MAX_PANES, PANES_FILE,
};
use holzwave_core::integrate::kube::{overlay_path, ContextEntry, KUBE};
use holzwave_core::integrate::profiles::{IntegrationProfiles, LocalFile};
use holzwave_core::integrate::run::EnvPolicy;
use holzwave_core::platform::Os;
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

// ---------------------------------------------------------------------------------------------------------------------
// The production rule, the tool's defaults and the summary.
// ---------------------------------------------------------------------------------------------------------------------

const SUBSCRIPTION: &str = "00000000-0000-0000-0000-000000000001";
const OTHER_SUBSCRIPTION: &str = "00000000-0000-0000-0000-000000000002";

fn rule_profiles() -> IntegrationProfiles {
    profiles_from(json!([
        { "id": "zeta", "tool": "kube", "name": "Zeta production", "context": PROD_ARN, "production": true },
        { "id": "alpha", "tool": "kube", "name": "Alpha production", "context": PROD_ARN, "namespace": "payments", "production": true },
        { "id": "dev", "tool": "kube", "name": "Dev", "context": "kind-dev", "namespace": "default" },
        { "id": "soft", "tool": "kube", "name": "Soft", "context": PROD_ARN, "namespace": "soft" },
        { "id": "talos-prod", "tool": "talos", "name": "Talos production", "context": "talos-prod", "node": "10.0.0.1", "production": true },
        { "id": "az-prod", "tool": "azure", "name": "Azure production", "subscription": SUBSCRIPTION, "production": true },
        { "id": "az-dev", "tool": "azure", "name": "Azure dev", "subscription": OTHER_SUBSCRIPTION },
        { "id": "gam-prod", "tool": "gam", "name": "GAM production", "section": "corp", "production": true },
    ]))
}

fn production_of(profiles: &IntegrationProfiles, tool: ToolId, selection: &Selection) -> Option<String> {
    resolve(profiles, tool, selection).expect("resolves").production
}

#[test]
fn a_context_pinned_by_a_production_profile_is_production_however_it_was_chosen() {
    let profiles = rule_profiles();
    let alpha = Some("Alpha production".to_owned());

    // Through the profile, by a direct pick of the same context, and through another profile that does not say so.
    assert_eq!(production_of(&profiles, ToolId::Kube, &pick(Some("alpha"), None, None)), alpha);
    assert_eq!(production_of(&profiles, ToolId::Kube, &pick(None, Some(PROD_ARN), None)), alpha, "a direct pick");
    assert_eq!(production_of(&profiles, ToolId::Kube, &pick(Some("soft"), None, None)), alpha, "a profile that does not say so");
    assert_eq!(production_of(&profiles, ToolId::Kube, &pick(Some("dev"), Some(PROD_ARN), None)), alpha, "an explicit field");
    assert_eq!(
        production_of(&profiles, ToolId::Kube, &pick(Some("zeta"), None, Some("anything"))),
        alpha,
        "the first profile in id order names it, whichever profile the selection took"
    );

    // Another context, or none, is not production; a pin that an explicit field replaced no longer counts.
    assert_eq!(production_of(&profiles, ToolId::Kube, &pick(None, Some("kind-dev"), None)), None);
    assert_eq!(production_of(&profiles, ToolId::Kube, &pick(Some("alpha"), Some("kind-dev"), None)), None);
    assert_eq!(production_of(&profiles, ToolId::Kube, &Selection::default()), None);
    assert_eq!(production_of(&profiles, ToolId::Kube, &pick(None, Some("kind-dev"), Some("payments"))), None);

    // A context is the context of its own tool.
    assert_eq!(production_of(&profiles, ToolId::Talos, &pick(None, Some(PROD_ARN), None)), None);
    assert_eq!(production_of(&profiles, ToolId::Kube, &pick(None, Some("talos-prod"), None)), None);
    assert_eq!(
        production_of(&profiles, ToolId::Talos, &pick(None, Some("talos-prod"), None)),
        Some("Talos production".to_owned())
    );

    // The same rule holds for a subscription and a section, which a profile also pins.
    let sub = |value: &str| Selection { subscription: Some(value.into()), ..Selection::default() };
    assert_eq!(production_of(&profiles, ToolId::Azure, &sub(SUBSCRIPTION)), Some("Azure production".to_owned()));
    assert_eq!(production_of(&profiles, ToolId::Azure, &sub(OTHER_SUBSCRIPTION)), None);
    let section = |value: &str| Selection { section: Some(value.into()), ..Selection::default() };
    assert_eq!(production_of(&profiles, ToolId::Gam, &section("corp")), Some("GAM production".to_owned()));
    assert_eq!(production_of(&profiles, ToolId::Gam, &section("lab")), None);
}

#[test]
fn a_profile_without_the_flag_pins_nothing_production() {
    let profiles = profiles_from(json!([
        { "id": "dev", "tool": "kube", "name": "Dev", "context": "kind-dev", "namespace": "default" },
        { "id": "az-dev", "tool": "azure", "name": "Azure dev", "subscription": SUBSCRIPTION },
    ]));
    for selection in [pick(Some("dev"), None, None), pick(None, Some("kind-dev"), None), pick(Some("dev"), None, Some("x"))] {
        let resolved = resolve(&profiles, ToolId::Kube, &selection).expect("resolves");
        assert_eq!(resolved.production, None, "{selection:?}");
        assert!(!profiles.production_for(ToolId::Kube, resolved.selection.context.as_ref().expect("a context")));
    }
    let sub = Selection { profile: Some("az-dev".into()), ..Selection::default() };
    assert_eq!(resolve(&profiles, ToolId::Azure, &sub).expect("resolves").production, None);
}

#[test]
fn the_default_is_stale_until_read_again() {
    let state = ContextState::default();
    let profiles = IntegrationProfiles::default();
    assert!(state.default_of(ToolId::Kube).is_none());
    assert!(!state.mark_stale(ToolId::Kube), "there is nothing to mark");
    assert_eq!(state.seq(), 0);

    // The service stores what the tool reported; a profile has no place in it.
    let read = pick(Some("prod"), Some("kind-a"), Some("team"));
    assert_eq!(state.set_default(ToolId::Kube, &read), Ok(true));
    assert_eq!(state.default_of(ToolId::Kube), Some((pick(None, Some("kind-a"), Some("team")), false)));
    assert_eq!(state.set_default(ToolId::Kube, &read), Ok(false), "the same value read again is no change");
    assert_eq!(state.seq(), 1);
    assert!(!summary(&state, &profiles, ToolId::Kube, None, Detection::Found).stale);

    assert!(state.mark_stale(ToolId::Kube));
    assert!(!state.mark_stale(ToolId::Kube), "marked once");
    assert_eq!(state.seq(), 2);
    assert!(summary(&state, &profiles, ToolId::Kube, None, Detection::Found).stale, "the shown value is the default's");
    assert!(state.default_of(ToolId::Kube).expect("kept").1, "a stale value is kept, not dropped");

    // Read again: the same value is fresh, a different one replaces it.
    assert_eq!(state.set_default(ToolId::Kube, &pick(None, Some("kind-a"), Some("team"))), Ok(true));
    assert!(!summary(&state, &profiles, ToolId::Kube, None, Detection::Found).stale);
    assert_eq!(state.seq(), 3);
    assert_eq!(state.set_default(ToolId::Kube, &ctx("kind-b")), Ok(true));
    assert_eq!(state.default_of(ToolId::Kube), Some((ctx("kind-b"), false)));

    // A value that is not valid for the tool is not stored, and the tool of another default is untouched.
    assert!(state.set_default(ToolId::Kube, &ctx("--server=evil")).is_err());
    assert!(state.set_default(ToolId::Azure, &ctx("kind-a")).is_err());
    assert_eq!(state.default_of(ToolId::Kube), Some((ctx("kind-b"), false)));
    assert!(state.default_of(ToolId::Azure).is_none());
}

fn row(state: &ContextState, profiles: &IntegrationProfiles, tool: ToolId, pane: Option<u64>, d: Detection) -> ContextRow {
    state.summary(profiles, tool, pane, d)
}

#[test]
fn the_summary_picks_pane_then_pick_then_default() {
    let profiles = rule_profiles();
    let state = ContextState::default();

    // Nothing known yet.
    let empty = row(&state, &profiles, ToolId::Kube, Some(3), Detection::Unknown);
    assert_eq!(empty.state, RowState::Unknown);
    assert_eq!((empty.context.clone(), empty.namespace.clone(), empty.production.clone(), empty.stale), (None, None, None, false));

    // The default, as the tool reports it, is shown with its flag.
    state.set_default(ToolId::Kube, &pick(None, Some("from-default"), Some("d"))).expect("default");
    state.mark_stale(ToolId::Kube);
    let shown = row(&state, &profiles, ToolId::Kube, Some(3), Detection::Found);
    assert_eq!((shown.context.as_deref(), shown.namespace.as_deref(), shown.stale), (Some("from-default"), Some("d"), true));

    // The pick replaces it, and is not stale.
    state.set_pick(ToolId::Kube, &pick(None, Some("from-pick"), None)).expect("pick");
    let shown = row(&state, &profiles, ToolId::Kube, Some(3), Detection::Found);
    assert_eq!((shown.context.as_deref(), shown.namespace.as_deref(), shown.stale), (Some("from-pick"), None, false));

    // The pane's own selection replaces both, and a profile fills what it leaves empty.
    state.set_pane(3, ToolId::Kube, "pods", &pick(Some("alpha"), None, None)).expect("pane");
    let shown = row(&state, &profiles, ToolId::Kube, Some(3), Detection::Found);
    assert_eq!((shown.context.as_deref(), shown.namespace.as_deref(), shown.stale), (Some(PROD_ARN), Some("payments"), false));
    assert_eq!(shown.production.as_deref(), Some("Alpha production"));
    assert_eq!(shown.tool, ToolId::Kube);
    // Another pane still shows the pick.
    assert_eq!(row(&state, &profiles, ToolId::Kube, Some(4), Detection::Found).context.as_deref(), Some("from-pick"));
    // With no pane, the pick.
    assert_eq!(row(&state, &profiles, ToolId::Kube, None, Detection::Found).context.as_deref(), Some("from-pick"));

    // The fields shown are those the tool selects.
    state.set_pane(5, ToolId::Talos, "nodes", &Selection { context: Some("talos-prod".into()), node: Some("10.0.0.1".into()), ..Selection::default() }).expect("talos");
    let talos = row(&state, &profiles, ToolId::Talos, Some(5), Detection::Found);
    assert_eq!((talos.context.as_deref(), talos.node.as_deref()), (Some("talos-prod"), Some("10.0.0.1")));
    assert_eq!((talos.namespace, talos.subscription, talos.section), (None, None, None));
    assert_eq!(talos.production.as_deref(), Some("Talos production"));
    let azure = Selection { subscription: Some(SUBSCRIPTION.into()), ..Selection::default() };
    state.set_pane(6, ToolId::Azure, "subscriptions", &azure).expect("azure");
    let azure = row(&state, &profiles, ToolId::Azure, Some(6), Detection::Found);
    assert_eq!((azure.subscription.as_deref(), azure.context, azure.section), (Some(SUBSCRIPTION), None, None));
    assert_eq!(azure.production.as_deref(), Some("Azure production"));
    state.set_pane(7, ToolId::Gam, "sections", &Selection { section: Some("lab".into()), ..Selection::default() }).expect("gam");
    let gam = row(&state, &profiles, ToolId::Gam, Some(7), Detection::Found);
    assert_eq!((gam.section.as_deref(), gam.production), (Some("lab"), None));

    // The state word: the detection decides, and a selection or a known tool is `found`.
    for (detection, with, expected) in [
        (Detection::NotFound, true, RowState::NotFound),
        (Detection::NotFound, false, RowState::NotFound),
        (Detection::SignedOut, true, RowState::SignedOut),
        (Detection::Found, false, RowState::Found),
        (Detection::Found, true, RowState::Found),
        (Detection::Unknown, true, RowState::Found),
        (Detection::Unknown, false, RowState::Unknown),
    ] {
        let pane = if with { Some(3) } else { Some(99) };
        let fresh = ContextState::default();
        if with {
            fresh.set_pane(3, ToolId::Kube, "pods", &ctx("kind-a")).expect("pane");
        }
        assert_eq!(row(&fresh, &profiles, ToolId::Kube, pane, detection).state, expected, "{detection:?} with={with}");
    }
    assert_eq!(serde_json::to_value(RowState::NotFound).expect("json"), "not_found");
    assert_eq!(serde_json::to_value(RowState::SignedOut).expect("json"), "signed_out");

    // A profile that was deleted since the selection was stored does not break the row: the fields stay.
    let gone = ContextState::default();
    gone.set_pane(3, ToolId::Kube, "pods", &pick(Some("deleted"), Some(PROD_ARN), None)).expect("pane");
    let shown = row(&gone, &profiles, ToolId::Kube, Some(3), Detection::Found);
    assert_eq!(shown.context.as_deref(), Some(PROD_ARN));
    assert_eq!(shown.production.as_deref(), Some("Alpha production"), "the context is still production");
}

#[test]
fn resolve_and_summary_agree_on_production() {
    let profiles = rule_profiles();
    let sub = |value: &str| Selection { subscription: Some(value.into()), ..Selection::default() };
    let section = |value: &str| Selection { section: Some(value.into()), ..Selection::default() };
    let table: Vec<(ToolId, &str, Selection)> = vec![
        (ToolId::Kube, "pods", pick(Some("alpha"), None, None)),
        (ToolId::Kube, "pods", pick(Some("zeta"), None, Some("x"))),
        (ToolId::Kube, "pods", pick(Some("soft"), None, None)),
        (ToolId::Kube, "pods", pick(None, Some(PROD_ARN), None)),
        (ToolId::Kube, "pods", pick(None, Some("kind-dev"), None)),
        (ToolId::Kube, "pods", pick(Some("dev"), None, None)),
        (ToolId::Kube, "pods", pick(Some("alpha"), Some("kind-dev"), None)),
        (ToolId::Kube, "pods", Selection::default()),
        (ToolId::Talos, "nodes", pick(None, Some("talos-prod"), None)),
        (ToolId::Talos, "nodes", pick(Some("talos-prod"), None, None)),
        (ToolId::Azure, "subscriptions", sub(SUBSCRIPTION)),
        (ToolId::Azure, "subscriptions", sub(OTHER_SUBSCRIPTION)),
        (ToolId::Gam, "sections", section("corp")),
        (ToolId::Gam, "sections", section("lab")),
    ];
    let state = ContextState::default();
    for (pane, (tool, view, selection)) in table.iter().enumerate() {
        let pane = pane as u64;
        state.set_pane(pane, *tool, view, selection).expect("stored");
        let resolved = resolve(&profiles, *tool, selection).expect("resolves");
        let shown = state.summary(&profiles, *tool, Some(pane), Detection::Found);
        assert_eq!(shown.production, resolved.production, "{tool:?} {selection:?}");
        assert_eq!(summary(&state, &profiles, *tool, Some(pane), Detection::Found), shown, "the function and the method agree");
        // The context rule that existed before agrees for a Kubernetes context.
        if let (ToolId::Kube, Some(context)) = (tool, resolved.selection.context.as_ref()) {
            assert_eq!(profiles.production_for(*tool, context), resolved.production.is_some(), "{selection:?}");
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Application: the overlay, the isolated directories and the machine-local configuration paths.
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn an_unsafe_path_is_named_by_a_fixed_reason() {
    let linux = |p: &str| unsafe_path_reason(Os::Linux, Path::new(p));
    let windows = |p: &str| unsafe_path_reason(Os::Windows, Path::new(p));
    for ok in ["/home/u/.kube/config", "/a"] {
        assert_eq!(linux(ok), None, "{ok}");
        assert_eq!(unsafe_path_reason(Os::Macos, Path::new(ok)), None, "{ok}");
    }
    for bad in ["", "relative/config", "./config", "../config", "~/config", "//server/share/x", "\\\\server\\share", "/a\nb", "/a\u{0}b", "/a\u{1b}[31mb"] {
        assert!(linux(bad).is_some(), "{bad:?} must be refused on Linux");
    }
    assert!(linux(&format!("/{}", "a".repeat(5000))).is_some(), "a path over 4096 bytes");

    for ok in ["C:\\Users\\u\\.kube\\config", "d:/data/x", "C:\\a"] {
        assert_eq!(windows(ok), None, "{ok}");
    }
    for bad in [
        "",
        "\\\\server\\share\\x",
        "//server/share/x",
        "\\\\?\\C:\\x",
        "\\\\.\\COM1",
        "C:x",
        "\\bin",
        "bin\\x",
        "/unix/absolute",
        "C:\\a\nb",
    ] {
        assert!(windows(bad).is_some(), "{bad:?} must be refused on Windows");
    }
    // The reason is a sentence about the kind of fault and never the path.
    let reason = linux("relative/secret-name").expect("refused");
    assert!(!reason.contains("secret-name"));
}

#[cfg(unix)]
mod apply {
    use super::*;
    use std::collections::BTreeMap;

    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for entry in fs::read_dir(at).expect("read_dir") {
            let path = entry.expect("entry").path();
            let relative = path.strip_prefix(root).expect("below the root").to_string_lossy().into_owned();
            let meta = fs::symlink_metadata(&path).expect("metadata");
            if meta.is_dir() {
                out.insert(relative, None);
                walk(root, &path, out);
            } else {
                out.insert(relative, Some(fs::read(&path).unwrap_or_default()));
            }
        }
    }

    /// Every path below `root` (directories as `None`) with the bytes of every file.
    fn snapshot(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn value<'a>(applied: &'a Applied, name: &str) -> Option<&'a str> {
        applied.env.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).expect("metadata").permissions().mode() & 0o7777
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    /// The home of the person with the files that belong to the tools, which no application may touch.
    fn tool_home() -> PathBuf {
        let home = common::scratch_dir("context-home");
        for (file, bytes) in [(".kube/config", "tool-owned kubeconfig"), (".talos/config", "tool-owned talosconfig"), (".azure/azureProfile.json", "tool-owned azure"), (".gam/gam.cfg", "tool-owned gam")] {
            let path = home.join(file);
            fs::create_dir_all(path.parent().expect("a parent")).expect("dirs");
            fs::write(path, bytes).expect("file");
        }
        home
    }

    fn at<'a>(
        data: &'a Path,
        home: &'a str,
        pane: Option<u64>,
        user_env: &'a dyn Fn(&str) -> Option<String>,
        contexts: &'a [ContextEntry],
    ) -> AppliedFor<'a> {
        AppliedFor { os: Os::current(), home, data_dir: data, pane, user_env, contexts }
    }

    fn prod_entry() -> ContextEntry {
        ContextEntry::new(PROD_ARN, "cluster-prod", "user-prod", Some("old"))
    }

    #[test]
    fn a_kubernetes_shell_gets_an_overlay_and_a_kubeconfig_value() {
        let data = common::scratch_dir("context-overlay");
        let home = tool_home();
        let home_text = home.to_str().expect("text").to_owned();
        let tool_files = snapshot(&home);
        let contexts = [prod_entry(), ContextEntry::new("kind-dev", "kind-dev", "kind-dev", None)];
        let user_list = |name: &str| (name == "KUBECONFIG").then(|| "/home/u/a:/home/u/b".to_owned());
        let local = LocalFile::default();

        // The pinned context and namespace are in the overlay, and the person's list follows it.
        let resolved = resolve(&prod_profiles(), ToolId::Kube, &pick(Some("prod"), None, None)).expect("resolves");
        let applied = apply_env(ToolId::Kube, &resolved, &local, &at(&data, &home_text, Some(3), &user_list, &contexts), true);
        let overlay = overlay_path(&data, 3);
        assert!(applied.notes.is_empty() && !applied.failed, "{:?}", applied.notes);
        assert_eq!(value(&applied, "KUBECONFIG"), Some(format!("{}:/home/u/a:/home/u/b", overlay.display()).as_str()));
        assert_eq!(applied.env.len(), 1, "KUBECONFIG and nothing else");
        let document: serde_json::Value = serde_json::from_slice(&fs::read(&overlay).expect("the overlay")).expect("JSON");
        assert_eq!(document["current-context"], PROD_ARN);
        assert_eq!(
            document["contexts"][0]["context"],
            json!({ "cluster": "cluster-prod", "user": "user-prod", "namespace": "payments" }),
            "the pane's namespace replaces the one the context names"
        );
        assert_eq!((mode(&data.join("run")), mode(&data.join("run/kube")), mode(&overlay)), (0o700, 0o700, 0o600));

        // Without the person's variable the default file follows; a context's own namespace stays when none is selected.
        let resolved = resolve(&prod_profiles(), ToolId::Kube, &pick(None, Some(PROD_ARN), None)).expect("resolves");
        let applied = apply_env(ToolId::Kube, &resolved, &local, &at(&data, &home_text, Some(4), &no_env, &contexts), true);
        assert_eq!(
            value(&applied, "KUBECONFIG"),
            Some(format!("{}:{home_text}/.kube/config", overlay_path(&data, 4).display()).as_str())
        );
        let document: serde_json::Value = serde_json::from_slice(&fs::read(overlay_path(&data, 4)).expect("overlay")).expect("JSON");
        assert_eq!(document["contexts"][0]["context"]["namespace"], "old");

        // A second application for the pane replaces the file and names the new context.
        let resolved = resolve(&prod_profiles(), ToolId::Kube, &pick(None, Some("kind-dev"), None)).expect("resolves");
        apply_env(ToolId::Kube, &resolved, &local, &at(&data, &home_text, Some(3), &no_env, &contexts), true);
        let document: serde_json::Value = serde_json::from_slice(&fs::read(&overlay).expect("overlay")).expect("JSON");
        assert_eq!(document["current-context"], "kind-dev");
        assert!(document["contexts"][0]["context"].get("namespace").is_none(), "no namespace to name");

        // A context that is not in the list that was read gets no overlay, and says so without naming it.
        let resolved = resolve(&prod_profiles(), ToolId::Kube, &pick(None, Some("not-in-the-list"), None)).expect("resolves");
        let applied = apply_env(ToolId::Kube, &resolved, &local, &at(&data, &home_text, Some(5), &no_env, &contexts), true);
        assert!(!overlay_path(&data, 5).exists());
        assert_eq!(value(&applied, "KUBECONFIG"), None);
        assert_eq!(applied.notes.len(), 1, "{:?}", applied.notes);
        assert!(!applied.notes[0].contains("not-in-the-list") && !applied.failed);

        // No pane, or no selected context, or no wish for an overlay: no overlay and no variable from it.
        let nothing = resolve(&prod_profiles(), ToolId::Kube, &Selection::default()).expect("resolves");
        for (resolved, pane, want) in [(&resolved, None, true), (&nothing, Some(6), true), (&resolved, Some(7), false)] {
            let applied = apply_env(ToolId::Kube, resolved, &local, &at(&data, &home_text, pane, &user_list, &contexts), want);
            assert_eq!(value(&applied, "KUBECONFIG"), None, "pane {pane:?} want {want}");
        }
        assert!(!overlay_path(&data, 6).exists() && !overlay_path(&data, 7).exists());

        assert_eq!(snapshot(&home), tool_files, "the tools' own files are byte-identical");
        fs::remove_dir_all(&data).ok();
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn config_paths_from_the_local_file_become_the_tools_variables() {
        let data = common::scratch_dir("context-local");
        let home = tool_home();
        let home_text = home.to_str().expect("text").to_owned();
        let contexts = [prod_entry()];
        let mut local = LocalFile::default();
        local.set_config_paths(ToolId::Kube, &[PathBuf::from("/home/u/.kube/a"), PathBuf::from("/home/u/.kube/b")]).expect("kube");
        local.set_config_paths(ToolId::Talos, &[PathBuf::from("/home/u/talos/config")]).expect("talos");
        local.set_config_paths(ToolId::Azure, &[PathBuf::from("/home/u/az")]).expect("azure");
        local.set_config_paths(ToolId::Gam, &[PathBuf::from("/home/u/gam")]).expect("gam");
        let user_list = |name: &str| (name == "KUBECONFIG").then(|| "/home/u/ignored".to_owned());
        let profiles = profiles_from(json!([
            { "id": "plain", "tool": "azure", "name": "Plain" },
            { "id": "plain-gam", "tool": "gam", "name": "Plain GAM" },
        ]));
        let resolve_for = |tool, profile: Option<&str>| {
            resolve(&profiles, tool, &Selection { profile: profile.map(str::to_owned), ..Selection::default() }).expect("resolves")
        };

        let used = |tool: ToolId, profile: Option<&str>, local: &LocalFile, want_overlay: bool| {
            apply_env(tool, &resolve_for(tool, profile), local, &at(&data, &home_text, Some(3), &user_list, &contexts), want_overlay)
        };
        assert_eq!(value(&used(ToolId::Kube, None, &local, false), "KUBECONFIG"), Some("/home/u/.kube/a:/home/u/.kube/b"));
        assert_eq!(value(&used(ToolId::Talos, None, &local, false), "TALOSCONFIG"), Some("/home/u/talos/config"));
        assert_eq!(value(&used(ToolId::Azure, Some("plain"), &local, false), "AZURE_CONFIG_DIR"), Some("/home/u/az"));
        assert_eq!(value(&used(ToolId::Gam, Some("plain-gam"), &local, false), "GAMCFGDIR"), Some("/home/u/gam"));

        // With an overlay the paths of this computer are the list that follows it, ahead of the shell's variable.
        let resolved = resolve(&profiles, ToolId::Kube, &pick(None, Some(PROD_ARN), None)).expect("resolves");
        let applied = apply_env(ToolId::Kube, &resolved, &local, &at(&data, &home_text, Some(3), &user_list, &contexts), true);
        assert_eq!(
            value(&applied, "KUBECONFIG"),
            Some(format!("{}:/home/u/.kube/a:/home/u/.kube/b", overlay_path(&data, 3).display()).as_str())
        );

        // Nothing is invented when the file lists nothing.
        let empty = LocalFile::default();
        for tool in ToolId::ALL {
            let applied = used(tool, None, &empty, false);
            assert!(applied.env.is_empty() && applied.notes.is_empty(), "{tool:?}: {applied:?}");
        }
        fs::remove_dir_all(&data).ok();
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn isolation_gives_a_private_directory_per_profile() {
        let data = common::scratch_dir("context-isolation");
        let home = tool_home();
        let home_text = home.to_str().expect("text").to_owned();
        let tool_files = snapshot(&home);
        let profiles = profiles_from(json!([
            { "id": "acme", "tool": "azure", "name": "Acme", "subscription": SUBSCRIPTION, "isolated": true },
            { "id": "other", "tool": "azure", "name": "Other", "isolated": true },
            { "id": "plain", "tool": "azure", "name": "Plain" },
            { "id": "corp", "tool": "gam", "name": "Corp", "section": "corp", "isolated": true },
        ]));
        let mut local = LocalFile::default();
        local.set_config_paths(ToolId::Azure, &[PathBuf::from("/home/u/az")]).expect("azure");
        local.set_config_paths(ToolId::Gam, &[PathBuf::from("/home/u/gam")]).expect("gam");
        let run = |tool: ToolId, profile: &str| {
            let resolved = resolve(&profiles, tool, &Selection { profile: Some(profile.into()), ..Selection::default() }).expect("resolves");
            apply_env(tool, &resolved, &local, &at(&data, &home_text, Some(3), &no_env, &[]), false)
        };

        let acme = run(ToolId::Azure, "acme");
        let acme_dir = data.join("integrations/azure/acme");
        assert_eq!(value(&acme, "AZURE_CONFIG_DIR"), Some(acme_dir.to_str().expect("text")));
        assert!(acme.notes.is_empty() && !acme.failed);
        let other = run(ToolId::Azure, "other");
        assert_eq!(value(&other, "AZURE_CONFIG_DIR"), Some(data.join("integrations/azure/other").to_str().expect("text")));
        let corp = run(ToolId::Gam, "corp");
        assert_eq!(value(&corp, "GAMCFGDIR"), Some(data.join("integrations/gam/corp").to_str().expect("text")));
        for dir in ["integrations", "integrations/azure", "integrations/azure/acme", "integrations/azure/other", "integrations/gam", "integrations/gam/corp"] {
            assert_eq!(mode(&data.join(dir)), 0o700, "{dir}");
        }
        // Asking again changes nothing and keeps what the tool wrote there.
        fs::write(acme_dir.join("azureProfile.json"), "written by the tool after sign-in").expect("tool file");
        assert_eq!(run(ToolId::Azure, "acme").env, acme.env);
        assert_eq!(fs::read(acme_dir.join("azureProfile.json")).expect("kept"), b"written by the tool after sign-in");

        // A profile that is not isolated uses this computer's path, and no directory is made for it.
        let plain = run(ToolId::Azure, "plain");
        assert_eq!(value(&plain, "AZURE_CONFIG_DIR"), Some("/home/u/az"));
        assert!(!data.join("integrations/azure/plain").exists());

        // A link in the way is never followed, and the variable is not set from the person's own directory instead.
        let target = common::scratch_dir("context-isolation-target");
        std::os::unix::fs::symlink(&target, data.join("integrations/azure/linked")).expect("link");
        let linked_profiles = profiles_from(json!([{ "id": "linked", "tool": "azure", "name": "Linked", "isolated": true }]));
        let resolved = resolve(&linked_profiles, ToolId::Azure, &Selection { profile: Some("linked".into()), ..Selection::default() }).expect("resolves");
        let refused = apply_env(ToolId::Azure, &resolved, &local, &at(&data, &home_text, Some(3), &no_env, &[]), false);
        assert!(refused.failed && refused.env.is_empty(), "{refused:?}");
        assert_eq!(refused.notes.len(), 1);
        assert!(fs::read_dir(&target).expect("target").next().is_none(), "nothing was written through the link");

        assert_eq!(snapshot(&home), tool_files);
        for dir in [data, home, target] {
            fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn unsafe_config_paths_are_refused_with_a_note() {
        let data = common::scratch_dir("context-unsafe");
        let home = tool_home();
        let home_text = home.to_str().expect("text").to_owned();
        let profiles = profiles_from(json!([{ "id": "acme", "tool": "azure", "name": "Acme", "isolated": true }]));
        let mut local = LocalFile::default();
        for tool in ToolId::ALL {
            local.set_config_paths(tool, &[PathBuf::from("/home/u/fine")]).expect("a path of this computer");
        }
        let contexts = [prod_entry()];

        // Read as a Windows computer: a Unix path is not a drive path there, so none of these may become a variable.
        let windows = |tool: ToolId, profile: Option<&str>, data_dir: &Path, want_overlay: bool| {
            let resolved = resolve(&profiles, tool, &Selection { profile: profile.map(str::to_owned), context: (tool == ToolId::Kube).then(|| PROD_ARN.to_owned()), ..Selection::default() }).expect("resolves");
            let for_windows = AppliedFor { os: Os::Windows, home: "C:\\Users\\u", data_dir, pane: Some(3), user_env: &no_env, contexts: &contexts };
            apply_env(tool, &resolved, &local, &for_windows, want_overlay)
        };
        for tool in ToolId::ALL {
            let applied = windows(tool, None, &data, false);
            assert!(applied.env.is_empty(), "{tool:?}: {applied:?}");
            assert_eq!(applied.notes.len(), 1, "{tool:?}: {:?}", applied.notes);
            assert!(!applied.notes[0].contains("/home/u/fine"), "the note does not name the path");
        }
        // A data directory that is not a drive path on Windows gets neither an overlay nor an isolated directory.
        let overlay = windows(ToolId::Kube, None, &data, true);
        assert!(overlay.env.is_empty() && !overlay_path(&data, 3).exists(), "{overlay:?}");
        let isolated = windows(ToolId::Azure, Some("acme"), &data, false);
        assert!(isolated.failed && isolated.env.is_empty(), "{isolated:?}");
        assert!(fs::read_dir(&data).expect("data").next().is_none(), "nothing was created");

        // One bad entry among good ones is dropped; the rest are used.
        let mut mixed = LocalFile::default();
        mixed.set_config_paths(ToolId::Kube, &[PathBuf::from("/home/u/a"), PathBuf::from("/home/u/b")]).expect("kube");
        let resolved = resolve(&profiles, ToolId::Kube, &Selection::default()).expect("resolves");
        let on_linux = apply_env(ToolId::Kube, &resolved, &mixed, &at(&data, &home_text, None, &no_env, &[]), false);
        assert_eq!(value(&on_linux, "KUBECONFIG"), Some("/home/u/a:/home/u/b"));

        // No variable that the runner denies is ever set, whatever is asked.
        let profiles = profiles_from(json!([
            { "id": "iso-az", "tool": "azure", "name": "A", "isolated": true },
            { "id": "iso-gam", "tool": "gam", "name": "G", "isolated": true },
        ]));
        for (tool, profile) in [(ToolId::Kube, None), (ToolId::Talos, None), (ToolId::Azure, Some("iso-az")), (ToolId::Gam, Some("iso-gam"))] {
            let resolved = resolve(&profiles, tool, &Selection { profile: profile.map(str::to_owned), context: (tool == ToolId::Kube).then(|| PROD_ARN.to_owned()), ..Selection::default() }).expect("resolves");
            let applied = apply_env(tool, &resolved, &local, &at(&data, &home_text, Some(3), &no_env, &contexts), true);
            for (name, value) in &applied.env {
                for os in [Os::Linux, Os::Macos, Os::Windows] {
                    assert!(!EnvPolicy::is_denied(name, os), "{name} is denied on {os:?}");
                }
                assert!(!value.is_empty() && !value.contains('\0'));
            }
            assert!(!applied.env.is_empty(), "{tool:?} sets its variable");
        }
        fs::remove_dir_all(&data).ok();
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn application_writes_only_below_the_data_directory() {
        let data = common::scratch_dir("context-writes");
        let home = tool_home();
        let home_text = home.to_str().expect("text").to_owned();
        let tool_files = snapshot(&home);
        let contexts = [prod_entry()];
        let profiles = profiles_from(json!([
            { "id": "acme", "tool": "azure", "name": "Acme", "isolated": true },
            { "id": "corp", "tool": "gam", "name": "Corp", "isolated": true },
        ]));
        let mut local = LocalFile::default();
        local.set_config_paths(ToolId::Kube, &[home.join(".kube/config")]).expect("kube");
        local.set_config_paths(ToolId::Talos, &[home.join(".talos/config")]).expect("talos");

        for (tool, profile) in [(ToolId::Kube, None), (ToolId::Talos, None), (ToolId::Azure, Some("acme")), (ToolId::Gam, Some("corp"))] {
            for want_overlay in [false, true] {
                let selection = Selection { profile: profile.map(str::to_owned), context: (tool == ToolId::Kube).then(|| PROD_ARN.to_owned()), ..Selection::default() };
                let resolved = resolve(&profiles, tool, &selection).expect("resolves");
                apply_env(tool, &resolved, &local, &at(&data, &home_text, Some(3), &no_env, &contexts), want_overlay);
            }
        }

        let written = snapshot(&data);
        assert!(!written.is_empty());
        for path in written.keys() {
            assert!(path == "run" || path.starts_with("run/") || path == "integrations" || path.starts_with("integrations/"), "unexpected {path}");
        }
        assert!(written.contains_key("run/kube/3.json") && written.contains_key("integrations/azure/acme") && written.contains_key("integrations/gam/corp"));
        assert_eq!(snapshot(&home), tool_files, "no tool's own file or directory changed, and nothing new appeared there");
        fs::remove_dir_all(&data).ok();
        fs::remove_dir_all(&home).ok();
    }
}
