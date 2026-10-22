//! Kubernetes end to end, with a fake `kubectl` and no cluster: the answers of the service are written as golden files
//! that the page's browser tests answer their commands with, so the contract between the core and the page is tested once
//! in each direction. The terminal part runs a real pane with the real engine.
//!
//! Nothing here uses a real `kubectl`, a cluster, a network or the person's kubeconfig. The tool is a POSIX shell script
//! of the rig that prints a recorded fixture, found through injected host facts on a scratch machine. The goldens are the
//! serialisation of the real types, rewritten only with `UPDATE_FIXTURES=1` (the diff is reviewed before it is
//! committed); the time of a read and the random id of a ticket are replaced by fixed values before they are written.
#![cfg(unix)]

mod common;
#[path = "common/fake_cli.rs"]
mod fake_cli;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use holzwave_core::integrate::args::{ContextName, ProfileId};
use holzwave_core::integrate::profiles::IntegrationProfile;
use holzwave_core::integrate::service::{HostFacts, IntegrationService};
use holzwave_core::integrate::{Selection, ToolId};
use holzwave_core::secrets::MemoryStore;
use serde_json::Value;

use common::{scratch_dir, update_fixtures};
use fake_cli::FakeCli;

/// The time every golden carries, in seconds since the Unix epoch.
const FETCHED: u64 = 1_760_000_000;
/// The id every card golden carries (a ticket id is random).
const TICKET: &str = "0123456789abcdef0123456789abcdef";
/// The name the production profile shows.
const PRODUCTION: &str = "Production EU";

/// A scratch computer: a `bin` directory on the `PATH`, a home, a data directory, removed when the test ends.
struct Rig {
    root: PathBuf,
    bin: PathBuf,
    home: PathBuf,
    data: PathBuf,
    store: Arc<MemoryStore>,
}

impl Rig {
    fn new() -> Rig {
        let root = scratch_dir("e2e-kube");
        let (bin, home, data) = (root.join("bin"), root.join("home"), root.join("data"));
        for dir in [&bin, &home, &data] {
            std::fs::create_dir(dir).expect("a directory of the rig");
        }
        Rig { root, bin, home, data, store: Arc::new(MemoryStore::default()) }
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

/// A recorded fixture of the tools, as text.
fn fixture(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrations").join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {relative}: {e}"))
}

fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn argv_of(fake: &FakeCli) -> Vec<String> {
    fake.argv().into_iter().map(|a| String::from_utf8(a).expect("text arguments")).collect()
}

fn selecting(context: &str, namespace: &str) -> Selection {
    Selection { context: Some(context.into()), namespace: Some(namespace.into()), ..Selection::default() }
}

/// The profile that makes `prod-eu` a production context.
fn production_profile() -> IntegrationProfile {
    let mut profile = IntegrationProfile::new(ProfileId::parse("prod-eu").expect("an id"), ToolId::Kube, PRODUCTION);
    profile.context = Some(ContextName::parse("prod-eu").expect("a name"));
    profile.production = true;
    profile
}

// ---- the goldens ---------------------------------------------------------------------------------------------------

/// The directory of the goldens, shared with the page's tests.
fn golden_dir() -> PathBuf {
    common::fixtures_dir().join("integration")
}

/// The age every row of a table golden shows, in place of the age that the core computed against the clock.
const AGE: &str = "3d";

/// Replaces what changes from run to run: the time of a read (in the answer and in its view), the id of a ticket and the
/// age of a row (the core computes it from the creation time and the clock of the computer, so it is a different word
/// every day).
fn normalise(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let age_at = map
                .get("columns")
                .and_then(Value::as_array)
                .and_then(|columns| columns.iter().position(|column| column["id"] == "age"));
            if let (Some(at), Some(Value::Array(rows))) = (age_at, map.get_mut("rows")) {
                for row in rows {
                    if let Some(cell) = row["cells"].get_mut(at) {
                        *cell = Value::from(AGE);
                    }
                }
            }
            for (name, member) in map.iter_mut() {
                match name.as_str() {
                    "fetched" if member.is_number() => *member = Value::from(FETCHED),
                    "ticket" if member.is_string() => *member = Value::from(TICKET),
                    _ => normalise(member),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalise),
        _ => {}
    }
}

/// Compares `value` (normalised, as pretty JSON with a line feed at the end) with the golden `name`, or writes the
/// golden when `UPDATE_FIXTURES=1`.
fn golden(name: &str, value: &impl serde::Serialize) {
    let mut value = serde_json::to_value(value).expect("the answer is JSON");
    normalise(&mut value);
    let mut text = serde_json::to_string_pretty(&value).expect("pretty JSON");
    text.push('\n');
    let path = golden_dir().join(name);
    if update_fixtures() {
        std::fs::create_dir_all(golden_dir()).expect("the directory of the goldens");
        std::fs::write(&path, &text).expect("write the golden");
        return;
    }
    let have = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("golden {name} is missing ({e}): write it with UPDATE_FIXTURES=1 and review the diff"));
    assert_eq!(have, text, "golden {name} differs: the core's answer changed, or rewrite it with UPDATE_FIXTURES=1");
}

// ---- the tracer ----------------------------------------------------------------------------------------------------

#[test]
fn the_pods_answer_is_written_as_a_golden_and_matches_it() {
    let rig = Rig::new();
    let fake = FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/pods.json"));
    let service = rig.service();
    service.save_profile(production_profile()).expect("a production profile");
    let shown = service.select("kube", Some(2), &selecting("prod-eu", "payments")).expect("a selection");
    assert_eq!(shown.production.as_deref(), Some(PRODUCTION), "the core calls the selection production");

    let answer = service.run_view("kube", "pods", Some(2), &args(&[])).expect("the read gives a model");
    assert_eq!(
        argv_of(&fake),
        ["get", "pods", "-o", "json", "--context=prod-eu", "--namespace=payments", "--request-timeout=10s"],
        "the pane's selection is in the argument list, in the equals form"
    );
    assert_eq!(answer.selection.production.as_deref(), Some(PRODUCTION));
    golden("kube_pods.json", &answer);
}

// ---- switching the context and the namespace -----------------------------------------------------------------------

/// Makes a fake append the arguments of every start, one line each, to `<name>.calls` (the rig's script sources
/// `<name>.conf` before it records anything, so one more line there sees the arguments). Call it after the last builder
/// method: a builder rewrites the file. No argument of these tests holds a space.
fn recording(fake: FakeCli) -> FakeCli {
    use std::io::Write;
    let path = format!("{}.conf", fake.path().display());
    let mut conf = std::fs::OpenOptions::new().append(true).open(path).expect("the settings of the fake");
    writeln!(conf, "echo \"$*\" >> \"$d/$n.calls\"").expect("one more line");
    fake
}

/// Every argument list the recording fake was started with, oldest first.
fn calls(fake: &FakeCli) -> Vec<Vec<String>> {
    std::fs::read_to_string(format!("{}.calls", fake.path().display()))
        .map(|text| text.lines().map(|line| line.split(' ').map(str::to_owned).collect()).collect())
        .unwrap_or_default()
}

#[test]
fn the_selection_of_one_pane_changes_its_own_argument_list_only() {
    let rig = Rig::new();
    let fake = recording(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/pods.json")));
    let service = rig.service();
    service.save_profile(production_profile()).expect("a production profile");
    service.select("kube", Some(3), &selecting("kind-test", "default")).expect("pane 3 selects another cluster");
    service.select("kube", Some(4), &selecting("prod-eu", "payments")).expect("pane 4 keeps the production one");

    service.run_view("kube", "pods", Some(4), &args(&[])).expect("a read for pane 4");
    service.run_view("kube", "pods", Some(3), &args(&[])).expect("a read for pane 3");
    let [four, three] = &calls(&fake)[..] else { panic!("two starts, got {:?}", calls(&fake)) };
    assert_eq!(four.len(), three.len(), "the same verb builds the same shape of list");
    let differ: Vec<usize> = (0..four.len()).filter(|i| four[*i] != three[*i]).collect();
    assert_eq!(differ.len(), 2, "exactly the context and the namespace differ: {four:?} / {three:?}");
    assert_eq!(four[differ[0]], "--context=prod-eu");
    assert_eq!(three[differ[0]], "--context=kind-test");
    assert_eq!(four[differ[1]], "--namespace=payments");
    assert_eq!(three[differ[1]], "--namespace=default");
    assert!(differ[0] + 1 == differ[1], "in the equals form, one element each, side by side");

    // Pane 3 moves on; pane 4 asks again, after a young answer is too old to be served (the selection drops only the
    // answers of its own pane), and its list is as it was.
    service.select("kube", Some(3), &selecting("kind-test", "ops")).expect("pane 3 changes its namespace");
    std::thread::sleep(std::time::Duration::from_millis(1200));
    service.run_view("kube", "pods", Some(4), &args(&[])).expect("a read for pane 4");
    service.run_view("kube", "pods", Some(3), &args(&[])).expect("a read for pane 3");
    let all = calls(&fake);
    assert_eq!(all.len(), 4, "both reads ran again: {all:?}");
    assert_eq!(&all[2], four, "the other pane's list is unchanged");
    assert_eq!(all[3][differ[1]], "--namespace=ops", "the pane that changed has its new list");
    assert_eq!(all[3][differ[0]], "--context=kind-test");
}

/// Every regular file below `dir`, as a path relative to it, sorted.
fn files_below(dir: &std::path::Path) -> Vec<String> {
    fn walk(base: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("a readable directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.push(path.strip_prefix(base).expect("below the base").to_string_lossy().into_owned());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn switching_writes_no_file_and_leaves_the_tools_files_alone() {
    let rig = Rig::new();
    // The person's own configuration of two tools, on the scratch computer: a switch of the pane must not touch either.
    let kubeconfig = rig.home.join(".kube").join("config");
    let talosconfig = rig.home.join(".talos").join("config");
    let (kube_bytes, talos_bytes) = (fixture("kubectl/config_view_canary.kubeconfig"), "context: demo-prod\n".to_owned());
    for (path, bytes) in [(&kubeconfig, &kube_bytes), (&talosconfig, &talos_bytes)] {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        std::fs::write(path, bytes).expect("a configuration file");
    }
    let fake = recording(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/pods.json")));
    let service = rig.service();
    service.save_profile(production_profile()).expect("a production profile");

    // Everything a person does to switch: select, read, switch again, read the lists, a terminal that follows the pane.
    for (pane, context, namespace) in [(3, "prod-eu", "payments"), (4, "kind-test", "default"), (3, "kind-test", "ops")] {
        service.select("kube", Some(pane), &selecting(context, namespace)).expect("a selection");
        service.run_view("kube", "pods", Some(pane), &args(&[])).expect("a read");
    }
    service.select("kube", None, &selecting("kind-test", "default")).expect("the pick for new panes");
    service.launch("kube", "logs", Some(4), &args(&[("pod", "api-0")])).expect("a log launch describes a pane");
    service.forget_pane(3);

    assert_eq!(calls(&fake).len(), 3, "only the three reads started the tool");
    assert_eq!(std::fs::read_to_string(&kubeconfig).expect("still there"), kube_bytes, "the kubeconfig is byte for byte as it was");
    assert_eq!(std::fs::read_to_string(&talosconfig).expect("still there"), talos_bytes, "so is the talosconfig");
    assert_eq!(files_below(&rig.home), [".kube/config", ".talos/config"], "nothing else appeared in the home");

    let files = files_below(&rig.data);
    assert_eq!(
        files,
        ["integrations.json", "pane_integrations.json"],
        "below the data directory only the profile file this test saved and the pane selections"
    );
}

#[test]
fn the_contexts_namespaces_and_card_goldens_match() {
    let rig = Rig::new();
    let service = rig.service();
    service.save_profile(production_profile()).expect("a production profile");
    service.select("kube", Some(2), &selecting("prod-eu", "payments")).expect("a selection");

    // The contexts of the recorded kubeconfig: the current one marked, and a Use action on the others.
    let _kubectl = FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json"));
    let contexts = service.run_view("kube", "contexts", Some(2), &args(&[])).expect("the contexts");
    golden("kube_contexts.json", &contexts);

    let _kubectl = FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/namespaces.json"));
    let namespaces = service.run_view("kube", "namespaces", Some(2), &args(&[])).expect("the namespaces");
    golden("kube_namespaces.json", &namespaces);

    // The card of Make default… for a pane that is not on a production context: the sentence of the file it changes.
    service.select("kube", Some(2), &selecting("kind-test", "default")).expect("another cluster");
    let _kubectl = FakeCli::new(&rig.bin, "kubectl").prints("Switched to context \"kind-test\".\n");
    let card = service.prepare("kube", "use_context", Some(2), &args(&[("context", "kind-test")])).expect("a card");
    assert_eq!(card.guard.production, None, "kind-test is not a production context");
    let changes = "the kubeconfig files named by KUBECONFIG, by default ~/.kube/config";
    assert!(card.target.contains(&("changes".to_owned(), changes.to_owned())), "{:?}", card.target);
    golden("kube_card_use_context.json", &card);
}
