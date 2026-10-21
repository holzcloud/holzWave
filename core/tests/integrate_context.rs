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
use holzwave_core::integrate::context::{resolve, ContextError};
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
