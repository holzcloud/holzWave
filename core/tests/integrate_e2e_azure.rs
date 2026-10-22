//! Azure from a fake `az` to the answers the page receives, with no account and no network.
//!
//! The tool is a fake program of the rig (a POSIX shell script in a scratch directory) that prints the recorded or
//! written files of `tests/fixtures/integrations/az`, and the detector finds it through injected facts, so an `az`
//! that is installed on this computer can never stand in for it. The person's own Azure CLI configuration is a
//! scratch directory whose bytes are compared before and after. The answers of the core are written as golden files
//! under `app/tests/fixtures/integration/` (only with `UPDATE_FIXTURES=1`, `fetched` and the ticket normalised); the
//! page tests answer their commands with those files, so the page draws exactly what the core produced. The tests
//! are Unix only.
#![cfg(unix)]

mod common;
#[path = "common/fake_cli.rs"]
mod fake_cli;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use holzwave_core::integrate::args::{ProfileId, SubscriptionId};
use holzwave_core::integrate::context::RowState;
use holzwave_core::integrate::profiles::IntegrationProfile;
use holzwave_core::integrate::service::{Done, FaultCode, HostFacts, IntegrationService, RunAnswer};
use holzwave_core::integrate::view::{Tone, ViewModel};
use holzwave_core::integrate::{Selection, ToolId, VerbEffect};
use holzwave_core::secrets::MemoryStore;
use serde_json::{json, Value};

use common::{fixtures_dir, scratch_dir, update_fixtures};
use fake_cli::FakeCli;

/// The time every golden says it was read at.
const FETCHED: u64 = 1_760_000_000;

/// A scratch computer: a `bin` directory on the `PATH`, a home, a data directory and the person's own Azure CLI
/// configuration directory, all removed when the test ends.
struct Rig {
    root: PathBuf,
    bin: PathBuf,
    home: PathBuf,
    data: PathBuf,
    azure_config: PathBuf,
    store: Arc<MemoryStore>,
}

impl Rig {
    fn new() -> Rig {
        let root = scratch_dir("azure_e2e");
        let (bin, home, data, azure_config) =
            (root.join("bin"), root.join("home"), root.join("data"), root.join("azure-config"));
        for dir in [&bin, &home, &data, &azure_config] {
            std::fs::create_dir(dir).expect("a directory of the rig");
        }
        // What the Azure CLI keeps of a person: it is compared byte for byte after every test that selects.
        std::fs::write(azure_config.join("azureProfile.json"), "{\"subscriptions\": []}\n").expect("a profile file");
        std::fs::write(azure_config.join("config"), "[core]\ncollect_telemetry = no\n").expect("a config file");
        Rig { root, bin, home, data, azure_config, store: Arc::new(MemoryStore::default()) }
    }

    fn facts(&self) -> HostFacts {
        HostFacts::for_tests(&self.root, std::slice::from_ref(&self.bin), &self.home, &self.data)
    }

    fn service(&self) -> IntegrationService {
        self.service_over(self.facts())
    }

    fn service_over(&self, facts: HostFacts) -> IntegrationService {
        IntegrationService::new(&self.data, self.store.clone(), facts, &[])
    }

    /// The machine of a person who has an Azure CLI configuration of their own, named by the environment.
    fn facts_with_a_configuration(&self) -> HostFacts {
        self.facts().with_env("AZURE_CONFIG_DIR", self.azure_config.to_str().expect("text"))
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A recorded or written file of the Azure fixtures, as text.
fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrations/az").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture az/{name}: {e}"))
}

fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn argv_of(fake: &FakeCli) -> Vec<String> {
    fake.argv().into_iter().map(|a| String::from_utf8(a).expect("text arguments")).collect()
}

// ---- the goldens ---------------------------------------------------------------------------------------------------

/// Makes a document the same on every run: the time of a read, the ticket of a card and the rig's directories.
fn normalise(value: &mut Value, rig: &Rig) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map.iter_mut() {
                match (key.as_str(), &*inner) {
                    ("fetched", Value::Number(_)) => *inner = json!(FETCHED),
                    ("ticket", Value::String(_)) => *inner = json!("0123456789abcdef0123456789abcdef"),
                    _ => normalise(inner, rig),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| normalise(item, rig)),
        Value::String(text) => {
            *text = text.replace(rig.bin.to_str().expect("text"), "/opt/homebrew/bin");
        }
        _ => {}
    }
}

/// Compares a document with its golden file, or rewrites the file with `UPDATE_FIXTURES=1`.
fn golden(name: &str, document: &Value) {
    let path = fixtures_dir().join("integration").join(name);
    let text = format!("{}\n", serde_json::to_string_pretty(document).expect("the document is JSON"));
    if update_fixtures() {
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the fixtures directory");
        std::fs::write(&path, text).expect("write the golden");
        return;
    }
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is missing ({e}); run with UPDATE_FIXTURES=1", path.display()));
    assert!(
        on_disk == text,
        "{name} differs from the answer of the core; review the change and run with UPDATE_FIXTURES=1"
    );
}

/// A read of one view as the page receives it: the answer of `integration_run`, the Azure entry of
/// `integration_tools` and the rows of `integration_context`, in one document. The document is also returned as the
/// typed answer, and with the argument list the fake recorded for the read.
fn read(rig: &Rig, service: &IntegrationService, fake: &FakeCli, verb: &str) -> (RunAnswer, Value, Vec<String>) {
    read_for(rig, service, fake, verb, None)
}

/// [`read`] for a pane: the selection of the pane decides the argument list, and the rows are the pane's.
fn read_for(
    rig: &Rig,
    service: &IntegrationService,
    fake: &FakeCli,
    verb: &str,
    pane: Option<u64>,
) -> (RunAnswer, Value, Vec<String>) {
    let tools = service.tools();
    let azure = tools.iter().find(|tool| tool.id == ToolId::Azure).expect("an Azure entry");
    let tool = serde_json::to_value(azure).expect("the tool is JSON");
    let answer = service.run_view("azure", verb, pane, &args(&[])).unwrap_or_else(|e| panic!("azure {verb}: {e}"));
    let recorded = argv_of(fake);
    let mut document = serde_json::to_value(&answer).expect("the answer is JSON");
    document["tool"] = tool;
    document["rows"] = serde_json::to_value(service.context_rows(pane, false)).expect("the rows are JSON");
    normalise(&mut document, rig);
    (answer, document, recorded)
}

// ---- the tracer: the signed-out and the signed-in account ----------------------------------------------------------

#[test]
fn signed_out_is_a_state_with_a_sign_in_action() {
    let rig = Rig::new();
    let fake = FakeCli::new(&rig.bin, "az").prints_stderr(&fixture("account_show_signed_out.stderr.txt")).exits(1);
    let service = rig.service();
    let (answer, document, recorded) = read(&rig, &service, &fake, "account");

    assert_eq!(recorded, ["account", "show", "-o", "json", "--only-show-errors"], "the list of the account view");
    let ViewModel::Kv(view) = &answer.view else { panic!("the account is a key-value view, got {:?}", answer.view) };
    let state: Vec<_> = view.rows().iter().filter(|row| row.label() == "State").collect();
    assert_eq!(state.len(), 1);
    assert_eq!(state[0].value(), "Signed out");
    assert_eq!(state[0].tone(), Tone::Warn, "a state a person can act on is told with the warn tone, not as an error");
    let actions: Vec<(&str, &str, VerbEffect)> =
        view.actions().iter().map(|action| (action.verb(), action.label(), action.effect())).collect();
    assert_eq!(actions, [("login", "Sign in", VerbEffect::Exec)], "the one action is the sign-in");

    // The section and the status line read the same fact from the same summary.
    let rows = service.context_rows(None, false);
    let azure = rows.iter().find(|row| row.tool == ToolId::Azure).expect("an Azure row");
    assert_eq!(azure.state, RowState::SignedOut);
    assert_eq!(azure.subscription, None, "a signed-out account has no subscription to show");

    assert_eq!(document["view"]["kind"], "kv");
    assert_eq!(document["rows"][2]["state"], "signed_out");
    golden("azure_account_signed_out.json", &document);
}

#[test]
fn signed_in_gives_the_account_rows_and_no_sign_in_action() {
    let rig = Rig::new();
    let fake = FakeCli::new(&rig.bin, "az").prints(&fixture("account_show.json"));
    let service = rig.service();
    let (answer, document, recorded) = read(&rig, &service, &fake, "account");

    assert_eq!(recorded, ["account", "show", "-o", "json", "--only-show-errors"]);
    let ViewModel::Kv(view) = &answer.view else { panic!("the account is a key-value view, got {:?}", answer.view) };
    let rows: Vec<(&str, &str)> = view.rows().iter().map(|row| (row.label(), row.value())).collect();
    assert_eq!(
        rows,
        [
            ("State", "Signed in"),
            ("User", "user@example.com"),
            ("Subscription", "Item 1"),
            ("Subscription ID", "00000000-0000-4000-8000-000000000002"),
            ("Tenant ID", "00000000-0000-4000-8000-000000000001"),
            ("Environment", "AzureCloud"),
            ("Subscription state", "Enabled"),
        ]
    );
    assert!(view.actions().is_empty(), "a signed-in account has no sign-in action");

    let rows = service.context_rows(None, false);
    let azure = rows.iter().find(|row| row.tool == ToolId::Azure).expect("an Azure row");
    assert_eq!(azure.state, RowState::Found);
    assert_eq!(azure.subscription.as_deref(), Some("00000000-0000-4000-8000-000000000002"), "the tool's own default");

    assert_eq!(document["view"]["kind"], "kv");
    golden("azure_account_signed_in.json", &document);
}

/// Everything below `dir`, by relative path.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.strip_prefix(base).expect("below the base").to_string_lossy().into_owned();
            if path.is_dir() {
                out.insert(format!("{name}/"), Vec::new());
                walk(base, &path, out);
            } else {
                out.insert(name, std::fs::read(&path).unwrap_or_default());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

#[test]
fn a_signed_out_read_leaves_no_file_behind() {
    // Reading the account of a signed-out tool is a state, not a reason to write anything: not in the data directory,
    // not in the person's own configuration.
    let rig = Rig::new();
    let fake = FakeCli::new(&rig.bin, "az").prints_stderr(&fixture("account_show_signed_out.stderr.txt")).exits(1);
    let (config_before, data_before) = (snapshot(&rig.azure_config), snapshot(&rig.data));
    let service = rig.service();
    let _ = read(&rig, &service, &fake, "account");
    assert_eq!(snapshot(&rig.azure_config), config_before, "the person's Azure CLI configuration is untouched");
    assert_eq!(snapshot(&rig.data), data_before, "and so is the data directory");
}

// ---- the subscriptions and the groups -------------------------------------------------------------------------------

const FIRST: &str = "00000000-0000-4000-8000-000000000002";
const SECOND: &str = "00000000-0000-4000-8000-000000000003";
const THIRD: &str = "00000000-0000-4000-8000-000000000004";

/// The recorded list of subscriptions with one change: the third entry is disabled, which the recorded account does not
/// have. Everything else is the recorded text.
fn subscriptions_with_a_disabled_one() -> String {
    let text = fixture("account_list.json");
    let enabled = "\"state\": \"Enabled\"";
    let at = text.rfind(enabled).expect("the recorded list has a state in its last entry");
    let mut out = text.clone();
    out.replace_range(at..at + enabled.len(), "\"state\": \"Disabled\"");
    assert_ne!(out, text);
    out
}

/// The cells of a table, as text, one array each.
fn cells(view: &ViewModel) -> Vec<Vec<String>> {
    let ViewModel::Table(table) = view else { panic!("a table was expected, got {view:?}") };
    table.rows().iter().map(|row| row.cells().map(str::to_owned).collect()).collect()
}

#[test]
fn subscriptions_and_groups_goldens_match() {
    let rig = Rig::new();
    let fake = FakeCli::new(&rig.bin, "az").prints(&subscriptions_with_a_disabled_one());
    let service = rig.service();

    // The subscriptions: the default is starred, the disabled one is told in words and in the warn tone, and every
    // entry with a valid GUID has the action that makes it the tool's default.
    let (answer, document, recorded) = read(&rig, &service, &fake, "subscriptions");
    assert_eq!(recorded, ["account", "list", "-o", "json", "--only-show-errors"]);
    let ViewModel::Table(table) = &answer.view else { panic!("a table, got {:?}", answer.view) };
    let columns: Vec<&str> = table.columns().iter().map(|column| column.label()).collect();
    assert_eq!(columns, ["Default", "Name", "ID", "State", "Tenant"]);
    let tenant = "00000000-0000-4000-8000-000000000001";
    assert_eq!(
        cells(&answer.view),
        [
            vec!["*", "Item 1", FIRST, "Enabled", tenant],
            vec!["", "Item 2", SECOND, "Enabled", tenant],
            vec!["", "Item 3", THIRD, "Disabled", tenant],
        ]
    );
    let tones: Vec<Tone> = table.rows().iter().map(|row| row.tone()).collect();
    assert_eq!(tones, [Tone::Normal, Tone::Normal, Tone::Warn], "only the state decides the tone");
    for (row, guid) in table.rows().iter().zip([FIRST, SECOND, THIRD]) {
        let actions: Vec<(&str, VerbEffect, Vec<(&str, &str)>)> =
            row.actions().iter().map(|a| (a.verb(), a.effect(), a.args().collect())).collect();
        assert_eq!(actions, [("set_subscription", VerbEffect::Local, vec![("subscription", guid)])]);
    }
    golden("azure_subscriptions.json", &document);

    // The groups of a pane that uses a profile which pins the first subscription and says it is production: the list
    // names that subscription on its command line, and the rows of the pane say production.
    let id = ProfileId::parse("prod-azure").expect("an id");
    let mut profile = IntegrationProfile::new(id, ToolId::Azure, "Production");
    profile.subscription = Some(SubscriptionId::parse(FIRST).expect("a guid"));
    profile.production = true;
    service.save_profile(profile).expect("a profile");
    service
        .select("azure", Some(3), &Selection { profile: Some("prod-azure".into()), ..Selection::default() })
        .expect("a selection");
    let fake = fake.prints(&fixture("group_list.json"));
    let (answer, document, recorded) = read_for(&rig, &service, &fake, "groups", Some(3));
    assert_eq!(recorded, ["group", "list", "-o", "json", "--only-show-errors", &format!("--subscription={FIRST}")]);
    let ViewModel::Table(table) = &answer.view else { panic!("a table, got {:?}", answer.view) };
    let columns: Vec<&str> = table.columns().iter().map(|column| column.label()).collect();
    assert_eq!(columns, ["Name", "Location", "State", "Tags"]);
    assert!(table.rows().iter().all(|row| row.actions().is_empty()), "a group has no action");
    let shown = cells(&answer.view);
    assert_eq!(shown[0], ["rg-demo-one", "westeurope", "Succeeded", "1"], "the tags are counted, never read");
    assert!(shown.iter().any(|row| row[2] == "Deleting"), "a state other than Succeeded is told in words: {shown:?}");
    assert!(
        table.rows().iter().zip(&shown).any(|(row, cells)| cells[2] == "Deleting" && row.tone() != Tone::Normal),
        "and in the tone"
    );
    assert_eq!(answer.selection.production.as_deref(), Some("Production"));
    assert_eq!(document["rows"][2]["production"], "Production");
    assert_eq!(document["rows"][2]["subscription"], FIRST);
    golden("azure_groups.json", &document);
}

/// Makes a fake count its starts: the rig's script sources `<name>.conf` before it records anything, so one more line
/// there appends to `<name>.runs` at every start. Call it after the last builder method (a builder rewrites the file).
fn counted(fake: FakeCli) -> FakeCli {
    use std::io::Write;
    let path = format!("{}.conf", fake.path().display());
    let mut conf = std::fs::OpenOptions::new().append(true).open(path).expect("the settings of the fake");
    writeln!(conf, "echo run >> \"$d/$n.runs\"").expect("one more line");
    fake
}

/// How many times a counted fake was started.
fn runs(fake: &FakeCli) -> usize {
    std::fs::read_to_string(format!("{}.runs", fake.path().display())).map_or(0, |text| text.lines().count())
}

fn choosing(guid: &str) -> Selection {
    Selection { subscription: Some(guid.into()), ..Selection::default() }
}

#[test]
fn a_subscription_is_selected_per_pane_and_nothing_is_written() {
    let rig = Rig::new();
    let fake = FakeCli::new(&rig.bin, "az").prints(&fixture("group_list.json"));
    let service = rig.service_over(rig.facts_with_a_configuration());
    let (config_before, data_before) = (snapshot(&rig.azure_config), snapshot(&rig.data));
    assert!(data_before.is_empty(), "the data directory starts empty: {data_before:?}");

    service.select("azure", Some(3), &choosing(FIRST)).expect("a selection for pane 3");
    service.select("azure", Some(4), &choosing(SECOND)).expect("a selection for pane 4");
    let list = |pane: u64| {
        service.run_view("azure", "groups", Some(pane), &args(&[])).unwrap_or_else(|e| panic!("pane {pane}: {e}"));
        (argv_of(&fake), fake.env())
    };
    let (first, first_env) = list(3);
    let (second, second_env) = list(4);
    let (bare, _) = list(5);

    // The two lists differ in the subscription flag and in nothing else.
    let base = ["group", "list", "-o", "json", "--only-show-errors"];
    assert_eq!(first, [base.as_slice(), &[format!("--subscription={FIRST}").as_str()]].concat());
    assert_eq!(second, [base.as_slice(), &[format!("--subscription={SECOND}").as_str()]].concat());
    assert_eq!(bare, base, "a pane without a selection names no subscription: the tool's own default decides");
    assert_eq!(first_env, second_env, "and the environment of the two runs is the same");
    assert_eq!(
        first_env.get("AZURE_CONFIG_DIR").map(String::as_str),
        rig.azure_config.to_str(),
        "the person's own configuration is what the tool reads"
    );

    // The core's summary is what the section and the status line draw, for the focused pane.
    let subscription = |pane: Option<u64>| {
        let rows = service.context_rows(pane, false);
        rows.iter().find(|row| row.tool == ToolId::Azure).and_then(|row| row.subscription.clone())
    };
    assert_eq!(subscription(Some(3)).as_deref(), Some(FIRST));
    assert_eq!(subscription(Some(4)).as_deref(), Some(SECOND));
    assert_eq!(subscription(None), None, "the tool's own default was not read, and no pane's choice became it");

    // Nothing was written but the file of the pane selections, and the tool's own configuration is byte for byte
    // what it was.
    assert_eq!(snapshot(&rig.azure_config), config_before, "the Azure CLI configuration is untouched");
    let after: Vec<String> = snapshot(&rig.data).into_keys().collect();
    assert_eq!(after, ["pane_integrations.json"], "only the file of the pane selections was created");
}

#[test]
fn set_subscription_asks_first_and_runs_once() {
    let rig = Rig::new();
    let _account = FakeCli::new(&rig.bin, "az").prints(&fixture("account_show.json"));
    let service = rig.service_over(rig.facts_with_a_configuration());
    service.run_view("azure", "account", None, &args(&[])).expect("the default read");
    let stale = |service: &IntegrationService| {
        service.context_rows(None, false).iter().find(|row| row.tool == ToolId::Azure).map(|row| row.stale)
    };
    assert_eq!(stale(&service), Some(false), "a value that was just read is not stale");
    let config_before = snapshot(&rig.azure_config);

    // `az account set` prints nothing when it works.
    let fake = counted(FakeCli::new(&rig.bin, "az"));
    let card = service.prepare("azure", "set_subscription", None, &args(&[("subscription", SECOND)])).expect("a card");
    assert_eq!(card.argv, ["account", "set", &format!("--subscription={SECOND}"), "--only-show-errors"]);
    assert_eq!(card.effect, VerbEffect::Local);
    assert_eq!(card.program, "az");
    let changes = "the Azure CLI configuration of this computer (azureProfile.json)";
    assert!(card.target.contains(&("changes".to_owned(), changes.to_owned())), "{:?}", card.target);
    assert!(card.target.contains(&("subscription".to_owned(), SECOND.to_owned())), "{:?}", card.target);
    assert_eq!(card.guard.production, None, "no guard without a production profile");
    assert_eq!(runs(&fake), 0, "a card starts nothing");
    let mut document = serde_json::to_value(&card).expect("the card is JSON");
    normalise(&mut document, &rig);
    golden("azure_card_set_subscription.json", &document);

    let done = service.confirm(&card.ticket, &card.hash).expect("the confirm runs it");
    let Done::Ran { note } = done else { panic!("a local verb runs, got {done:?}") };
    assert_eq!(note, "Make default done.", "the tool said nothing, so the core says what ran");
    assert_eq!(runs(&fake), 1, "the fake ran once");
    assert_eq!(argv_of(&fake), card.argv, "and with exactly the list of the card");
    assert_eq!(fake.env().get("AZURE_CORE_COLLECT_TELEMETRY").map(String::as_str), Some("false"));
    assert_eq!(stale(&service), Some(true), "the tool's own default may have changed, so it is marked");
    let rows = service.context_rows(None, false);
    let azure = rows.iter().find(|row| row.tool == ToolId::Azure).expect("an Azure row");
    assert_eq!(azure.subscription.as_deref(), Some(FIRST), "and the value is kept");

    let again = service.confirm(&card.ticket, &card.hash).expect_err("a ticket approves once");
    assert_eq!(again.code, FaultCode::Ticket);
    assert_eq!(runs(&fake), 1, "the second confirm ran nothing");
    assert_eq!(snapshot(&rig.azure_config), config_before, "holzWave itself never edits the tool's configuration");
}
