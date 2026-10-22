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

use holzwave_core::integrate::context::RowState;
use holzwave_core::integrate::service::{HostFacts, IntegrationService, RunAnswer};
use holzwave_core::integrate::view::{Tone, ViewModel};
use holzwave_core::integrate::{ToolId, VerbEffect};
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
        IntegrationService::new(&self.data, self.store.clone(), self.facts(), &[])
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
    let tools = service.tools();
    let azure = tools.iter().find(|tool| tool.id == ToolId::Azure).expect("an Azure entry");
    let tool = serde_json::to_value(azure).expect("the tool is JSON");
    let answer = service.run_view("azure", verb, None, &args(&[])).unwrap_or_else(|e| panic!("azure {verb}: {e}"));
    let recorded = argv_of(fake);
    let mut document = serde_json::to_value(&answer).expect("the answer is JSON");
    document["tool"] = tool;
    document["rows"] = serde_json::to_value(service.context_rows(None, false)).expect("the rows are JSON");
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
