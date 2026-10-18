//! The Azure command-line tool as an integration: the argument lists of its verbs, the sign-in state, the parsers of its
//! answers and the hostile text that an account can put into them.
//!
//! No process is started here and no account is contacted. The answers of the tool are the files of
//! `core/tests/fixtures/integrations/az` (some recorded, some written, see the manifest next to them) and texts built in
//! the tests; hostile characters are built from code points so that this file holds none of them.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;

use holzwave_core::integrate::args::SubscriptionId;
use holzwave_core::integrate::azure::{login_state, parse_version, LoginState, AZURE, SPEC};
use holzwave_core::integrate::view::{KvView, Tone, ViewModel};
use holzwave_core::integrate::{CheckedSelection, Integration, Selection, ToolId, Validated, Verb, VerbEffect};

const SUBSCRIPTION: &str = "00000000-0000-4000-8000-000000000002";

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrations/az").join(name);
    fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn verb(id: &str) -> &'static Verb {
    SPEC.verb(id).unwrap_or_else(|| panic!("the catalogue has no verb {id}"))
}

fn selection(subscription: Option<&str>) -> CheckedSelection {
    Selection { subscription: subscription.map(str::to_owned), ..Selection::default() }
        .check(ToolId::Azure)
        .expect("a valid selection")
}

fn bare(id: &str) -> Validated {
    Validated::parse(verb(id), &BTreeMap::new()).expect("a verb without arguments")
}

fn argv(id: &str, subscription: Option<&str>) -> Vec<String> {
    AZURE
        .argv(verb(id), &bare(id), &selection(subscription))
        .expect("an argument list")
        .into_iter()
        .map(|arg: OsString| arg.into_string().expect("an argument that is text"))
        .collect()
}

fn kv(model: ViewModel) -> KvView {
    match model {
        ViewModel::Kv(view) => view,
        other => panic!("a key-value model was expected, got {other:?}"),
    }
}

fn value_of<'a>(view: &'a KvView, label: &str) -> &'a str {
    view.rows()
        .iter()
        .find(|row| row.label() == label)
        .unwrap_or_else(|| panic!("the view has no row {label}"))
        .value()
}

fn labels(view: &KvView) -> Vec<&str> {
    view.rows().iter().map(|row| row.label()).collect()
}

#[test]
fn signed_out_is_a_state_and_offers_login() {
    let account = verb("account");
    assert!(account.exit_is_state, "a signed-out account command exits non-zero and that is a state");
    assert_eq!(argv("account", None), ["account", "show", "-o", "json", "--only-show-errors"]);
    assert_eq!(
        argv("account", Some(SUBSCRIPTION)),
        ["account", "show", "-o", "json", "--only-show-errors", &format!("--subscription={SUBSCRIPTION}")]
    );

    let view = kv(AZURE.view(account, b"").expect("empty output is a state"));
    assert_eq!(value_of(&view, "State"), "Signed out");
    assert_eq!(view.rows()[0].tone(), Tone::Warn, "the word is the cue, the tone is the second one");
    assert_eq!(view.rows().len(), 1, "nothing else is known when signed out");
    assert_eq!(view.actions().len(), 1);
    let login = &view.actions()[0];
    assert_eq!(login.verb(), "login");
    assert_eq!(login.effect(), VerbEffect::Exec);
    assert_eq!(login.args().count(), 0);

    // Blank output is the same state; a tool that wrote only white space has signed nothing in.
    assert_eq!(kv(AZURE.view(account, b" \r\n\t\n").expect("blank output")).actions().len(), 1);
    assert_eq!(login_state(b""), LoginState::SignedOut);
    assert_eq!(login_state(b" \r\n"), LoginState::SignedOut);
}

#[test]
fn signed_in_shows_the_account_and_no_login() {
    let view = kv(AZURE.view(verb("account"), &fixture("account_show.json")).expect("the recorded account"));
    assert_eq!(
        labels(&view),
        ["State", "User", "Subscription", "Subscription ID", "Tenant ID", "Environment", "Subscription state"]
    );
    assert_eq!(value_of(&view, "State"), "Signed in");
    assert_eq!(view.rows()[0].tone(), Tone::Normal);
    assert_eq!(value_of(&view, "User"), "user@example.com");
    assert_eq!(value_of(&view, "Subscription"), "Item 1");
    assert_eq!(value_of(&view, "Subscription ID"), SUBSCRIPTION);
    assert_eq!(value_of(&view, "Tenant ID"), "00000000-0000-4000-8000-000000000001");
    assert_eq!(value_of(&view, "Environment"), "AzureCloud");
    assert_eq!(value_of(&view, "Subscription state"), "Enabled");
    assert!(view.actions().is_empty(), "a signed-in account has no login button");

    match login_state(&fixture("account_show.json")) {
        LoginState::SignedIn { user, subscription_id, subscription_name } => {
            assert_eq!(user, "user@example.com");
            assert_eq!(subscription_id.as_str(), SUBSCRIPTION);
            assert_eq!(subscription_name, "Item 1");
        }
        other => panic!("signed in was expected, got {other:?}"),
    }
    assert_eq!(login_state(b"this is not json"), LoginState::Unknown);
    assert_eq!(login_state(b"[1, 2"), LoginState::Unknown);
    assert_eq!(login_state(&fixture("account_show_signed_out.stderr.txt")), LoginState::Unknown);
    assert_eq!(SubscriptionId::parse(SUBSCRIPTION).expect("a guid").as_str(), SUBSCRIPTION);
}

#[test]
fn the_spec_turns_telemetry_off_and_nothing_else() {
    assert_eq!(SPEC.id, ToolId::Azure);
    assert_eq!(SPEC.label, "Azure");
    assert_eq!(SPEC.env_set, [("AZURE_CORE_COLLECT_TELEMETRY", "false")]);
    assert_eq!(SPEC.binaries, ["az"]);
    assert_eq!(SPEC.windows_binaries, ["az.cmd", "az.exe"]);
    assert_eq!(SPEC.selects, ToolId::Azure.selects());
    assert_eq!(SPEC.version_probe.argv, ["version", "-o", "json"]);
    assert_eq!((SPEC.version_probe.parse)(&String::from_utf8(fixture("version.json")).expect("text")).as_deref(), Some("2.90.0"));
    assert_eq!(parse_version("{\"azure-cli-core\": \"2.90.0\"}"), None, "only the member of the tool itself");
    assert_eq!(parse_version("2.90.0"), None, "not JSON");
    let ids: Vec<&str> = SPEC.verbs.iter().map(|verb| verb.id).collect();
    assert_eq!(ids, ["account", "subscriptions", "groups", "set_subscription", "login"]);
    assert!(std::ptr::eq(AZURE.spec(), &SPEC), "the integration describes itself with the spec");
}
