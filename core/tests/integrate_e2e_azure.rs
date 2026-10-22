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
use holzwave_core::integrate::profiles::{IntegrationProfile, LocalFile};
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

/// An action of a row: the verb, its effect and its arguments.
type Action<'a> = (&'a str, VerbEffect, Vec<(&'a str, &'a str)>);

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
        let actions: Vec<Action> =
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

// ---- isolation and the telemetry rule -------------------------------------------------------------------------------

/// The names the telemetry rule is checked on: every start of the tool carries this one variable.
const TELEMETRY: &str = "AZURE_CORE_COLLECT_TELEMETRY";

fn telemetry(env: &BTreeMap<String, String>) -> Option<&str> {
    env.get(TELEMETRY).map(String::as_str)
}

fn profile(id: &str, name: &str, isolated: bool) -> IntegrationProfile {
    let mut profile = IntegrationProfile::new(ProfileId::parse(id).expect("an id"), ToolId::Azure, name);
    profile.isolated = isolated;
    profile
}

fn using(id: &str) -> Selection {
    Selection { profile: Some(id.into()), ..Selection::default() }
}

#[test]
fn isolation_gives_the_tool_its_own_folder_and_telemetry_is_always_off() {
    use std::os::unix::fs::PermissionsExt;

    let rig = Rig::new();
    let fake = counted(FakeCli::new(&rig.bin, "az").prints(&fixture("account_show.json")));
    let service = rig.service();
    service.save_profile(profile("acme", "Acme", true)).expect("an isolated profile");
    service.save_profile(profile("plain", "Plain", false)).expect("a plain profile");
    service.select("azure", Some(3), &using("acme")).expect("pane 3 uses the isolated profile");
    service.select("azure", Some(4), &using("plain")).expect("pane 4 uses the plain profile");
    let own = rig.data.join("integrations").join("azure").join("acme");
    let own_text = own.to_str().expect("text").to_owned();
    assert!(!own.exists(), "nothing is made before a run needs it");

    // A read: the isolated profile's folder, made with the private mode, and the telemetry variable.
    service.run_view("azure", "account", Some(3), &args(&[])).expect("a read");
    let env = fake.env();
    assert_eq!(env.get("AZURE_CONFIG_DIR"), Some(&own_text), "the tool reads and writes its own folder");
    assert_eq!(telemetry(&env), Some("false"));
    let mode = std::fs::metadata(&own).expect("the folder was made").permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "and only this person can read it");
    assert!(std::fs::read_dir(&own).expect("a folder").next().is_none(), "holzWave put nothing in it");

    // A confirmed Local verb: the same folder, the same variable. The card names the person's configuration as the
    // thing it changes, which is true only when the tool is not isolated; the folder is what the run really writes.
    let card = service.prepare("azure", "set_subscription", Some(3), &args(&[("subscription", SECOND)])).expect("card");
    service.confirm(&card.ticket, &card.hash).expect("the Local verb ran");
    let env = fake.env();
    assert_eq!(env.get("AZURE_CONFIG_DIR"), Some(&own_text));
    assert_eq!(telemetry(&env), Some("false"));

    // The sign-in: the launch carries the folder and the variable too.
    let card = service.prepare("azure", "login", Some(3), &args(&[])).expect("the card of the sign-in");
    let Done::Launch(launch) = service.confirm(&card.ticket, &card.hash).expect("a launch") else { panic!("a launch") };
    let pair = |name: &str| launch.env.iter().find(|(own, _)| own == name).map(|(_, value)| value.as_str());
    assert_eq!(pair("AZURE_CONFIG_DIR"), Some(own_text.as_str()), "the pane signs in inside the folder");
    assert_eq!(pair(TELEMETRY), Some("false"));

    // A profile that is not isolated gets no folder of its own, and the variable is not set by holzWave at all.
    service.run_view("azure", "account", Some(4), &args(&[])).expect("a read for the plain profile");
    let env = fake.env();
    assert_eq!(env.get("AZURE_CONFIG_DIR"), None, "nothing is set when nothing is configured");
    assert_eq!(telemetry(&env), Some("false"), "the telemetry variable does not depend on isolation");
    let card = service.prepare("azure", "login", Some(4), &args(&[])).expect("a card");
    let Done::Launch(launch) = service.confirm(&card.ticket, &card.hash).expect("a launch") else { panic!("a launch") };
    assert!(launch.env.iter().all(|(name, _)| name != "AZURE_CONFIG_DIR"));
    assert!(launch.env.contains(&(TELEMETRY.to_owned(), "false".to_owned())));
    assert!(!rig.data.join("integrations").join("azure").join("plain").exists(), "and no folder is made for it");

    // The tool's version probe is a run too.
    let tools = service.tools();
    assert!(tools.iter().any(|tool| tool.id == ToolId::Azure));
    let env = fake.env();
    assert_eq!(argv_of(&fake), ["version", "-o", "json"]);
    assert_eq!(telemetry(&env), Some("false"), "the probe of the version carries it as well");
}

#[test]
fn a_machine_local_configuration_path_is_used_when_no_folder_is_isolated() {
    let rig = Rig::new();
    let fake = FakeCli::new(&rig.bin, "az").prints(&fixture("account_show.json"));
    let machine = rig.root.join("machine-config");
    std::fs::create_dir(&machine).expect("a directory");
    let mut local = LocalFile::default();
    local.set_config_paths(ToolId::Azure, std::slice::from_ref(&machine)).expect("an absolute path");
    local.save(&rig.data).expect("the machine-local file");
    let service = rig.service();
    service.save_profile(profile("plain", "Plain", false)).expect("a profile");
    service.save_profile(profile("acme", "Acme", true)).expect("an isolated profile");
    service.select("azure", Some(3), &using("plain")).expect("a selection");
    service.select("azure", Some(4), &using("acme")).expect("a selection");

    service.run_view("azure", "account", Some(3), &args(&[])).expect("a read");
    let env = fake.env();
    assert_eq!(env.get("AZURE_CONFIG_DIR").map(String::as_str), machine.to_str(), "this computer's path");
    assert_eq!(telemetry(&env), Some("false"));

    // Isolation wins over the machine-local path: the folder of the profile, not the person's own.
    service.run_view("azure", "account", Some(4), &args(&[])).expect("a read");
    let env = fake.env();
    let own = rig.data.join("integrations").join("azure").join("acme");
    assert_eq!(env.get("AZURE_CONFIG_DIR").map(String::as_str), own.to_str());
    assert_eq!(telemetry(&env), Some("false"));

    // No selection at all: the same path, from the file, and the same telemetry rule.
    service.run_view("azure", "subscriptions", None, &args(&[])).expect("a read without a pane");
    let env = fake.env();
    assert_eq!(env.get("AZURE_CONFIG_DIR").map(String::as_str), machine.to_str());
    assert_eq!(telemetry(&env), Some("false"));
}

// ---- az login in a real pane -------------------------------------------------------------------------------------

/// The machine of a person whose shell has the environment of this process: the pane inherits the real one, so the
/// names the launch must have removed are the real ones (plus two that a hijacker would use). `PATH` and `HOME` stay
/// the rig's.
#[cfg(feature = "ghostty")]
fn facts_with_the_real_environment(rig: &Rig) -> HostFacts {
    let mut facts = rig.facts().with_env("HOLZWAVE_TOKEN", "app-secret").with_env("LD_PRELOAD", "/tmp/x.so");
    facts.inherited.extend(std::env::vars_os().filter(|(name, _)| name != "PATH" && name != "HOME"));
    facts
}

#[cfg(feature = "ghostty")]
mod pane {
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;

    use holzwave_core::engine::{DomKey, Mods};
    use holzwave_core::integrate::run::Launch;
    use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
    use holzwave_core::wire::RowMsg;

    use super::common::wait_until;
    use super::*;

    const WAIT: Duration = Duration::from_secs(10);

    /// Collects everything a pane sends to its sink.
    #[derive(Clone, Default)]
    struct Probe {
        log: Arc<Mutex<Vec<PaneEvent>>>,
    }

    impl Probe {
        fn sink(&self) -> impl FnMut(PaneEvent) + Send + 'static {
            let log = self.log.clone();
            move |event| log.lock().expect("the log").push(event)
        }

        fn events(&self) -> Vec<PaneEvent> {
            self.log.lock().expect("the log").clone()
        }

        fn has_text(&self, needle: &str) -> bool {
            let row_text = |row: &RowMsg| -> String {
                row.runs.iter().flat_map(|run| run.0.chars()).filter(|c| *c != '\0').collect()
            };
            self.events().iter().any(|event| match event {
                PaneEvent::Frame(frame) => frame.changed.iter().any(|row| row_text(row).contains(needle)),
                _ => false,
            })
        }

        fn helds(&self) -> Vec<Option<u32>> {
            self.events()
                .into_iter()
                .filter_map(|event| if let PaneEvent::Held { code } = event { Some(code) } else { None })
                .collect()
        }

        fn exits(&self) -> Vec<Option<u32>> {
            self.events()
                .into_iter()
                .filter_map(|event| if let PaneEvent::Exited { code } = event { Some(code) } else { None })
                .collect()
        }
    }

    fn key(code: &str, name: &str) -> DomKey {
        DomKey { code: code.into(), key: name.into(), mods: Mods::NONE, release: false, ..DomKey::default() }
    }

    /// The card of the sign-in, its confirmation and the pane the launch makes: `Launch::apply_to` is the one place a
    /// launch becomes a pane, and the pane runs the fake in a real PTY with the real engine.
    fn sign_in(rig: &Rig, service: &IntegrationService, probe: &Probe, fake: &FakeCli) -> (Launch, PaneHandle) {
        let card = service.prepare("azure", "login", None, &args(&[])).expect("the card of the sign-in");
        let Done::Launch(launch) = service.confirm(&card.ticket, &card.hash).expect("a launch") else {
            panic!("an Exec verb confirms into a launch")
        };
        // Checked here, before the pane exists: once the pane is started the fake may already have run, and on a fast
        // machine it has.
        assert!(!fake.started(), "the confirm started nothing: the pane does");
        assert!(launch.program.starts_with(&rig.bin), "the program is the one the detector found");
        let mut spec = PaneSpec::new("/bin/zsh", 100, 24);
        spec.env = vec![("HOLZWAVE_TOKEN".into(), "app-token".into())];
        launch.apply_to(&mut spec);
        let pane = PaneHandle::spawn_ghostty(1, spec, probe.sink()).expect("the pane starts");
        (launch, pane)
    }

    #[test]
    fn az_login_runs_the_exact_argument_list_in_a_real_pane() {
        let rig = Rig::new();
        let fake = FakeCli::new(&rig.bin, "az").prints("To sign in, use a web browser.\n");
        let service = rig.service_over(facts_with_the_real_environment(&rig));

        // The card first: golden, effect Exec, the bare list `login`, nothing started.
        let card = service.prepare("azure", "login", None, &args(&[])).expect("the card of the sign-in");
        assert_eq!(card.effect, VerbEffect::Exec);
        assert_eq!(card.program, "az");
        assert_eq!(card.argv, ["login"], "no tenant, no device-code switch, no flag at all");
        assert_eq!(card.guard.production, None);
        assert!(!fake.started(), "a card starts nothing");
        let mut document = serde_json::to_value(&card).expect("the card is JSON");
        normalise(&mut document, &rig);
        golden("azure_card_login.json", &document);
        service.cancel(&card.ticket);

        let probe = Probe::default();
        let (launch, pane) = sign_in(&rig, &service, &probe, &fake);
        assert_eq!(launch.args, ["login"]);
        assert_eq!(launch.program, fake.path());
        assert!(launch.env.iter().all(|(name, _)| !name.starts_with("HOLZWAVE_")), "no variable of the app");
        for name in ["HOLZWAVE_TOKEN", "LD_PRELOAD"] {
            assert!(launch.env_remove.iter().any(|removed| removed == name), "{name} is removed from the pane");
        }

        assert!(wait_until(WAIT, || !probe.helds().is_empty()), "a quick tool is held: {:?}", probe.events());
        let seen = fake.env();
        assert_eq!(argv_of(&fake), ["login"], "the fake ran with the exact list");
        assert_eq!(telemetry(&seen), Some("false"), "the pane's environment carries the telemetry variable");
        let names: Vec<&String> = seen.keys().collect();
        assert!(!names.iter().any(|name| name.starts_with("HOLZWAVE_")), "no variable of the app: {names:?}");
        for removed in &launch.env_remove {
            assert!(!seen.contains_key(removed), "{removed} was removed from the pane: {names:?}");
        }
        eprintln!(
            "the pane's parent had {} of the {} names the launch removes",
            launch.env_remove.iter().filter(|name| std::env::var_os(name).is_some()).count(),
            launch.env_remove.len()
        );
        pane.close();
        assert!(pane.join(Duration::from_secs(5)));
    }

    #[test]
    fn a_failing_login_is_held_until_enter() {
        let rig = Rig::new();
        let fake =
            FakeCli::new(&rig.bin, "az").prints_stderr("ERROR: Unable to reach the sign-in page.\n").exits(1);
        let service = rig.service_over(facts_with_the_real_environment(&rig));
        let probe = Probe::default();
        let (_launch, pane) = sign_in(&rig, &service, &probe, &fake);

        assert!(wait_until(WAIT, || !probe.helds().is_empty()), "no Held event: {:?}", probe.events());
        assert!(probe.has_text("ERROR: Unable to reach the sign-in page."), "the tool's line stays on screen");
        assert!(
            wait_until(WAIT, || probe.has_text("[holzWave] exited with status 1. Press Enter to close.")),
            "the notice line is on screen"
        );
        assert_eq!(probe.helds(), [Some(1)]);
        assert_eq!(argv_of(&fake), ["login"]);

        // Waiting and other keys close nothing.
        thread::sleep(Duration::from_millis(800));
        let _ = pane.key(key("KeyA", "a"));
        let _ = pane.key(key("Escape", "Escape"));
        thread::sleep(Duration::from_millis(400));
        assert!(probe.exits().is_empty(), "the pane ended without Enter: {:?}", probe.events());

        pane.key(key("Enter", "Enter")).expect("the held pane takes a key");
        assert!(wait_until(WAIT, || !probe.exits().is_empty()), "Enter did not end the pane: {:?}", probe.events());
        assert!(pane.join(Duration::from_secs(5)));
        thread::sleep(Duration::from_millis(300));
        assert_eq!(probe.exits(), [Some(1)], "exactly one Exited, with the tool's status");
    }
}

// ---- the Windows batch-file rule ------------------------------------------------------------------------------------

#[test]
fn windows_batch_arguments_stay_in_the_charset_end_to_end() {
    use std::ffi::OsString;

    use holzwave_core::integrate::azure::SPEC;
    use holzwave_core::integrate::detect::{detect, version_probe, Detection, Facts};
    use holzwave_core::integrate::run::{check_batch_args, RunError};
    use holzwave_core::platform::{Os, ProgramCheck};

    // A Windows machine whose `az` is the batch file of the Azure CLI installer.
    let env = |name: &str| match name {
        "PATH" => Some(r"C:\Tools".to_owned()),
        "PATHEXT" => Some(".COM;.EXE;.BAT;.CMD".to_owned()),
        _ => None,
    };
    let check = |path: &str| {
        if path.eq_ignore_ascii_case(r"C:\Tools\az.cmd") { ProgramCheck::Usable } else { ProgramCheck::Missing }
    };
    let login = Vec::<String>::new;
    let mode_of = |_: &str| None;
    let facts = Facts { os: Os::Windows, env: &env, check: &check, home: None, login_path: &login, mode_of: &mode_of };
    let Detection::Found(found) = detect(&SPEC, &facts, None) else { panic!("the detector finds az.cmd") };
    assert_eq!(found.path, r"C:\Tools\az.cmd");
    assert!(found.batch, "it is a batch file, so the runner applies the strict charset");

    // Every argument list the service builds for every Azure verb, recorded from the fake on this computer.
    let rig = Rig::new();
    let fake = counted(FakeCli::new(&rig.bin, "az"));
    let service = rig.service();
    let mut lists: Vec<(String, Vec<String>)> = Vec::new();
    let upper = SECOND.to_uppercase();
    service.select("azure", Some(3), &choosing(FIRST)).expect("a selection");
    service.select("azure", Some(4), &choosing(&upper)).expect("an upper-case guid is accepted");
    let reads =
        [("account", "account_show.json"), ("subscriptions", "account_list.json"), ("groups", "group_list.json")];
    for (verb, text) in reads {
        let fake = fake.clone().prints(&fixture(text));
        for pane in [None, Some(3), Some(4)] {
            service.run_view("azure", verb, pane, &args(&[])).unwrap_or_else(|e| panic!("{verb} {pane:?}: {e}"));
            lists.push((format!("{verb} {pane:?}"), argv_of(&fake)));
        }
    }
    for guid in [FIRST, upper.as_str()] {
        let request = args(&[("subscription", guid)]);
        let card = service.prepare("azure", "set_subscription", None, &request).expect("a card");
        lists.push((format!("set_subscription {guid} (card)"), card.argv.clone()));
        service.confirm(&card.ticket, &card.hash).expect("it ran");
        lists.push((format!("set_subscription {guid} (run)"), argv_of(&fake)));
    }
    let card = service.prepare("azure", "login", None, &args(&[])).expect("a card");
    lists.push(("login (card)".to_owned(), card.argv.clone()));
    let Done::Launch(launch) = service.confirm(&card.ticket, &card.hash).expect("a launch") else { panic!("a launch") };
    lists.push(("login (launch)".to_owned(), launch.args.iter().map(|a| a.to_string_lossy().into_owned()).collect()));
    let probe: Vec<String> = version_probe(&SPEC, &found).args.iter().map(|arg| (*arg).to_owned()).collect();
    lists.push(("version probe".to_owned(), probe));
    assert!(lists.len() >= 15, "every verb, with and without a selection, was walked: {}", lists.len());
    let flagged = |list: &Vec<String>| list.iter().any(|arg| arg.starts_with("--subscription="));
    assert!(lists.iter().any(|(what, list)| what.starts_with("groups") && flagged(list)));

    for (what, list) in &lists {
        let os_list: Vec<OsString> = list.iter().map(OsString::from).collect();
        assert!(!list.is_empty() && !list.iter().any(String::is_empty), "{what}: an empty list or argument");
        assert_eq!(check_batch_args(&found.path, &os_list, Os::Windows), Ok(()), "{what}: {list:?}");
    }
    assert!(
        lists.iter().all(|(_, list)| list.iter().all(|a| !a.contains(char::is_uppercase) || a.starts_with("-o"))),
        "an upper-case guid is written in lower case before it reaches a batch file"
    );

    // An argument outside the charset is refused by the same check, whatever put it there.
    let base = ["group", "list", "-o", "json", "--only-show-errors"];
    for hostile in ["my sub", "a&calc", "%PATH%", "x^", "\"q\"", "a|b", "a<b", "line\nfeed", "caf\u{e9}"] {
        let mut list: Vec<OsString> = base.iter().map(OsString::from).collect();
        list.push(OsString::from(format!("--subscription={hostile}")));
        assert_eq!(check_batch_args(&found.path, &list, Os::Windows), Err(RunError::BatchArgument), "{hostile:?}");
    }
    let empty = [OsString::from("account"), OsString::new()];
    assert_eq!(check_batch_args(&found.path, &empty, Os::Windows), Err(RunError::BatchArgument), "an empty argument");

    // And no such text gets as far as a list: the validators refuse it, and no process exists for it.
    let before = runs(&fake);
    for hostile in ["my sub", "a&calc", "%PATH%", "--subscription=x", &format!("{FIRST}&calc")] {
        let fault = service
            .prepare("azure", "set_subscription", None, &args(&[("subscription", hostile)]))
            .expect_err("a forged subscription has no card");
        assert_eq!(fault.code, FaultCode::Refused, "{hostile:?}");
        let fault = service.select("azure", Some(9), &choosing(hostile)).expect_err("nor a selection");
        assert_eq!(fault.code, FaultCode::Refused, "{hostile:?}");
    }
    assert_eq!(runs(&fake), before, "no process was started for any of them");
}
