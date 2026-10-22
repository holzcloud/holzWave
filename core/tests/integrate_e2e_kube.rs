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

// ---- the one Write verb: card, one confirm, the production guard --------------------------------------------------

use holzwave_core::integrate::service::{Done, FaultCode};

/// Makes a fake count its starts (see `integrate_service.rs`): one more line in the settings the script sources.
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

#[test]
fn the_write_verb_runs_once_after_its_confirmed_card() {
    let rig = Rig::new();
    let fake = counted(FakeCli::new(&rig.bin, "kubectl").prints("pod \"web-7d9f\" deleted\n"));
    let service = rig.service();
    service.save_profile(production_profile()).expect("a production profile");
    service.select("kube", Some(3), &selecting("kind-test", "default")).expect("a selection");

    let card = service.prepare("kube", "delete_pod", Some(3), &args(&[("pod", "web-7d9f")])).expect("a card");
    let want = [
        "delete",
        "pod",
        "--context=kind-test",
        "--namespace=default",
        "--wait=false",
        "--request-timeout=10s",
        "--",
        "web-7d9f",
    ];
    assert_eq!(card.argv, want, "the exact argument list and no more");
    assert!(card.target.contains(&("pod".to_owned(), "web-7d9f".to_owned())), "{:?}", card.target);
    assert_eq!(card.guard.production, None);
    assert_eq!(runs(&fake), 0, "a card starts nothing");
    golden("kube_card_delete_pod.json", &card);

    let done = service.confirm(&card.ticket, &card.hash).expect("the confirm runs it");
    assert!(matches!(done, Done::Ran { .. }), "a write verb runs: {done:?}");
    assert_eq!(runs(&fake), 1, "the fake ran once");
    assert_eq!(argv_of(&fake), card.argv, "what ran is what the card showed");

    // A replay is a ticket fault and runs nothing.
    let again = service.confirm(&card.ticket, &card.hash).expect_err("a ticket approves once");
    assert_eq!(again.code, FaultCode::Ticket);
    assert_eq!(runs(&fake), 1);

    // A selection changed between the card and the click: the request is not the one that was shown.
    let card = service.prepare("kube", "delete_pod", Some(3), &args(&[("pod", "web-7d9f")])).expect("another card");
    service.select("kube", Some(3), &selecting("dev", "default")).expect("another cluster");
    let fault = service.confirm(&card.ticket, &card.hash).expect_err("changed");
    assert_eq!(fault.code, FaultCode::Ticket);
    assert_eq!(runs(&fake), 1, "a changed selection ran nothing");

    // A hash that is not the card's.
    let card = service.prepare("kube", "delete_pod", Some(3), &args(&[("pod", "web-7d9f")])).expect("a third card");
    let wrong = service.confirm(&card.ticket, &"0".repeat(64)).expect_err("a wrong hash");
    assert_eq!(wrong.code, FaultCode::Ticket);
    assert_eq!(runs(&fake), 1);
}

#[test]
fn a_production_card_has_the_guard_in_its_hash() {
    let rig = Rig::new();
    let _fake = FakeCli::new(&rig.bin, "kubectl");
    let service = rig.service();
    service.select("kube", Some(2), &selecting("prod-eu", "payments")).expect("a selection");

    // The same request with and without the production flag of the profile: the guard is a field, so the hash differs.
    let mut profile = production_profile();
    profile.production = false;
    service.save_profile(profile).expect("a profile that is not production");
    let plain = service.prepare("kube", "delete_pod", Some(2), &args(&[("pod", "web-7d9f")])).expect("a card");
    assert_eq!(plain.guard.production, None);
    service.save_profile(production_profile()).expect("a production profile");
    let card = service.prepare("kube", "delete_pod", Some(2), &args(&[("pod", "web-7d9f")])).expect("a card");
    assert_eq!(card.guard.production.as_deref(), Some(PRODUCTION), "the guard names the profile");
    assert!(card.fields.contains(&("guard".to_owned(), "production".to_owned())), "{:?}", card.fields);
    assert!(card.fields.contains(&("production_name".to_owned(), PRODUCTION.to_owned())), "{:?}", card.fields);
    assert_ne!(card.hash, plain.hash, "what the person allows on production is not what they allow elsewhere");
    assert_eq!(card.argv, plain.argv, "the arguments are the same: the guard alone changed the hash");
    golden("kube_card_delete_pod_production.json", &card);

    // The production card is confirmed by its own hash and by no other.
    let fault = service.confirm(&card.ticket, &plain.hash).expect_err("the hash of the card without the guard");
    assert_eq!(fault.code, FaultCode::Ticket);
}

// ---- the shell pane for a context ----------------------------------------------------------------------------------

#[test]
fn the_shell_launch_gets_a_private_overlay() {
    let rig = Rig::new();
    // The person's kubeconfig on the scratch computer holds credentials; the tool's redacted view of it is what the
    // service reads, and the overlay is made from names only.
    let personal = rig.home.join(".kube").join("config");
    std::fs::create_dir_all(personal.parent().expect("a parent")).expect("a directory");
    std::fs::write(&personal, fixture("kubectl/config_view_canary.kubeconfig")).expect("the person's kubeconfig");
    let _fake = FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json"));
    let shell = holzwave_core::platform::ShellLaunch { program: "/bin/zsh".to_owned(), args: vec!["-l".to_owned()] };
    let service = rig.service_over(rig.facts().with_shell(shell));
    service.select("kube", Some(7), &selecting("kind-test", "payments")).expect("a selection");

    let launch = service.launch("kube", "shell", Some(7), &args(&[])).expect("the shell launches");
    assert_eq!(launch.program, PathBuf::from("/bin/zsh"), "the person's own shell");
    let overlay = rig.data.join("run").join("kube").join("7.json");
    let kubeconfig = launch.env.iter().find(|(name, _)| name == "KUBECONFIG").expect("KUBECONFIG is set");
    assert_eq!(
        kubeconfig.1,
        format!("{}:{}", overlay.display(), personal.display()),
        "the overlay first, the person's own list after it"
    );

    let text = std::fs::read_to_string(&overlay).expect("the overlay exists below the data directory");
    assert!(text.contains("kind-test") && text.contains("payments"), "it names the context and the namespace: {text}");
    for canary in ["HW-CANARY-TOKEN-0001", "HW-CANARY-EXEC-ENV", "REDACTED"] {
        assert!(!text.contains(canary), "the overlay holds no credential, not {canary}: {text}");
    }
    for member in ["\"token\"", "certificate-authority-data", "client-key", "\"exec\"", "\"server\"", "https://"] {
        assert!(!text.contains(member), "the overlay holds names only, not {member}: {text}");
    }
    assert!(launch.env.iter().all(|(name, _)| !name.starts_with("HOLZWAVE_")), "no variable of the app");

    // The pane that the launch opened is noted with the selection of the pane it was made from; the core's list of panes
    // is where the chip of the pane takes its text from.
    service.note_launch(8, "kube", "shell", Some(7)).expect("the new pane is noted");
    let row = service.pane_rows().into_iter().find(|row| row.pane == 8).expect("the pane is listed");
    assert_eq!((row.context.as_deref(), row.namespace.as_deref()), (Some("kind-test"), Some("payments")));
    assert_eq!(row.view, "shell");
    assert_eq!(row.production, None);

    // Closing the pane removes the overlay.
    service.forget_pane(7);
    assert!(!overlay.exists(), "the overlay is removed with the pane");
    service.forget_pane(8);
    assert!(service.pane_rows().is_empty());
}

impl Rig {
    fn service_over(&self, facts: HostFacts) -> IntegrationService {
        IntegrationService::new(&self.data, self.store.clone(), facts, &[])
    }
}

// ---- hostile names and faults ----------------------------------------------------------------------------------------

#[test]
fn hostile_pods_and_faults_become_goldens() {
    let rig = Rig::new();
    let service = rig.service();
    service.save_profile(production_profile()).expect("a production profile");
    service.select("kube", Some(2), &selecting("prod-eu", "payments")).expect("a selection");

    // The pods of a hostile cluster: a 253-character name, escape sequences, a right-to-left override, a 5000-character
    // value. The core cuts and cleans them; the golden is what the page receives.
    let _fake = FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/pods_hostile.json"));
    let hostile = service.run_view("kube", "pods", Some(2), &args(&[])).expect("hostile output is an answer");
    golden("kube_pods_hostile.json", &hostile);

    // The tool failed: the first line of its message, cleaned and cut at 240 characters, behind the tool's name.
    let message = format!(
        "\u{1b}[31mError from server (Forbidden): pods is forbidden: {}\u{1b}]0;title\u{7}\nsecond line\n",
        "User \"dev\" cannot list resource \"pods\" in the namespace \"payments\"; ".repeat(5)
    );
    let _fake = FakeCli::new(&rig.bin, "kubectl").prints_stderr(&message).exits(1);
    let failed = service.run_view("kube", "pods", Some(3), &args(&[])).expect_err("the tool failed");
    assert_eq!(failed.code, FaultCode::Failed);
    assert!(failed.text.starts_with("Kubernetes reported an error: Error from server (Forbidden)"), "{}", failed.text);
    assert!(!failed.text.contains('\u{1b}') && !failed.text.contains("second line"), "one clean line");
    golden("kube_fault_failed.json", &failed);

    // No kubectl on this computer: the core's own sentence.
    let empty = Rig::new();
    let missing = empty.service().run_view("kube", "pods", Some(2), &args(&[])).expect_err("nothing to run");
    assert_eq!(missing.code, FaultCode::Missing);
    golden("kube_fault_missing.json", &missing);
}

// ---- Exec in Pod in a real pane -------------------------------------------------------------------------------------
//
// These need the engine (Zig); the cases above run without it.

#[cfg(feature = "ghostty")]
mod panes {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use holzwave_core::engine::{DomKey, Mods};
    use holzwave_core::integrate::kube::POD_SHELL;
    use holzwave_core::integrate::run::{Launch, LAUNCH_LINGER};
    use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
    use holzwave_core::wire::{FrameMsg, RowMsg};

    use super::common::wait_until;
    use super::*;

    const WAIT: Duration = Duration::from_secs(10);
    const NOTFOUND: &str = "Error from server (NotFound): pods \"x\" not found\n";

    /// Collects everything a pane sends to its sink.
    #[derive(Clone, Default)]
    struct Probe {
        log: Arc<Mutex<Vec<PaneEvent>>>,
    }

    impl Probe {
        fn sink(&self) -> impl FnMut(PaneEvent) + Send + 'static {
            let log = self.log.clone();
            move |event| log.lock().unwrap().push(event)
        }

        fn events(&self) -> Vec<PaneEvent> {
            self.log.lock().unwrap().clone()
        }

        /// Whether any frame carried a changed row with this text.
        fn has_text(&self, needle: &str) -> bool {
            self.events().iter().any(|event| match event {
                PaneEvent::Frame(FrameMsg { changed, .. }) => changed.iter().any(|row| row_text(row).contains(needle)),
                _ => false,
            })
        }

        fn exits(&self) -> Vec<Option<u32>> {
            self.events()
                .into_iter()
                .filter_map(|event| match event {
                    PaneEvent::Exited { code } => Some(code),
                    _ => None,
                })
                .collect()
        }

        fn helds(&self) -> Vec<Option<u32>> {
            self.events()
                .into_iter()
                .filter_map(|event| match event {
                    PaneEvent::Held { code } => Some(code),
                    _ => None,
                })
                .collect()
        }
    }

    /// The text of a wire row: one code point per cell, the spacer marker left out.
    fn row_text(row: &RowMsg) -> String {
        row.runs.iter().flat_map(|run| run.0.chars()).filter(|c| *c != '\0').collect()
    }

    fn enter() -> DomKey {
        DomKey { code: "Enter".into(), key: "Enter".into(), mods: Mods::NONE, ..DomKey::default() }
    }

    /// A machine whose environment is the real one of this process (as `HostFacts::system` takes it: a test that runs in a
    /// holzWave terminal has the app's own `HOLZWAVE_*` variables, which is the case the removal is for) plus a variable of
    /// the app and a loader variable, and a production selection for pane 2.
    fn dirty_service(rig: &Rig) -> IntegrationService {
        let mut facts = rig.facts();
        // Not `PATH`: the detector must see the scratch directory and nothing else of this computer.
        for (name, value) in std::env::vars().filter(|(name, _)| name != "PATH") {
            facts = facts.with_env(&name, &value);
        }
        let facts = facts.with_env("HOLZWAVE_TOKEN", "app-secret").with_env("LD_PRELOAD", "/tmp/x.so");
        let service = rig.service_over(facts);
        service.save_profile(production_profile()).expect("a production profile");
        service.select("kube", Some(2), &selecting("prod-eu", "payments")).expect("a selection");
        service
    }

    /// Prepares `Exec…` of the pod `api-0` in container `app`, confirms it once and returns the launch.
    fn confirmed_exec(service: &IntegrationService, fake: &FakeCli) -> Launch {
        let request = args(&[("pod", "api-0"), ("container", "app")]);
        let card = service.prepare("kube", "exec", Some(2), &request).expect("a card");
        assert_eq!(card.guard.production.as_deref(), Some(PRODUCTION), "the exec card is guarded as production");
        let done = service.confirm(&card.ticket, &card.hash).expect("the confirm gives a launch");
        let Done::Launch(launch) = done else { panic!("an exec verb launches, got {done:?}") };
        assert!(!fake.started(), "the confirm started nothing: the pane does");
        launch
    }

    #[test]
    fn exec_in_pod_runs_the_exact_argument_list_in_a_real_pane() {
        let rig = Rig::new();
        let fake = FakeCli::new(&rig.bin, "kubectl").prints("connected\n");
        let service = dirty_service(&rig);
        let mut launch = confirmed_exec(&service, &fake);

        let head = [
            "exec",
            "-i",
            "-t",
            "--context=prod-eu",
            "--namespace=payments",
            "--container=app",
            "api-0",
            "--",
            "sh",
            "-c",
        ];
        let mut want: Vec<String> = head.iter().map(|word| (*word).to_owned()).collect();
        want.push(POD_SHELL.to_owned());
        let list: Vec<String> = launch.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(list, want, "the argument list of the launch");
        assert_eq!(launch.program, fake.path());
        for name in ["HOLZWAVE_TOKEN", "LD_PRELOAD"] {
            assert!(launch.env_remove.iter().any(|removed| removed == name), "{name} is in the removal list: {:?}", launch.env_remove);
        }
        for (name, _) in std::env::vars().filter(|(name, _)| name.starts_with("HOLZWAVE_")) {
            assert!(launch.env_remove.contains(&name), "the app variable {name} of this process is removed");
        }
        // `HOME` is a name every process has: with it in the list the removal of a plain name is visible in the child.
        launch.env_remove.push("HOME".into());

        // The pane of the shell: its own program and arguments are replaced by the launch's, and it is started for real.
        let mut spec = PaneSpec::new("/bin/zsh", 100, 24);
        launch.apply_to(&mut spec);
        let probe = Probe::default();
        let pane = PaneHandle::spawn_ghostty(1, spec, probe.sink()).expect("the pane starts");
        assert!(wait_until(WAIT, || !probe.helds().is_empty()), "a tool that ends at once is held: {:?}", probe.events());

        let argv: Vec<String> = fake.argv().iter().map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        assert_eq!(argv, want, "the fake received exactly that list, one argument each");
        let env = fake.env();
        let names: Vec<&String> = env.keys().collect();
        assert!(!names.iter().any(|name| name.starts_with("HOLZWAVE_")), "no variable of the app in the child: {names:?}");
        for removed in ["HOLZWAVE_TOKEN", "LD_PRELOAD", "HOME"] {
            assert!(!env.contains_key(removed), "{removed} was removed from the child: {names:?}");
        }
        assert!(env["PATH"].starts_with(&format!("{}:", rig.bin.display())), "the tool's directory first: {}", env["PATH"]);
        assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"), "the pane's own variables stay");
        pane.close();
        assert!(pane.join(Duration::from_secs(5)));
    }

    #[test]
    fn a_failing_exec_is_held_until_enter() {
        let rig = Rig::new();
        let fake = FakeCli::new(&rig.bin, "kubectl").prints(NOTFOUND).exits(1);
        let service = dirty_service(&rig);
        let launch = confirmed_exec(&service, &fake);
        assert_eq!(LAUNCH_LINGER, Duration::from_secs(3), "the linger time of a launch");

        let mut spec = PaneSpec::new("/bin/zsh", 100, 24);
        launch.apply_to(&mut spec);
        let probe = Probe::default();
        let started = Instant::now();
        let pane = PaneHandle::spawn_ghostty(1, spec, probe.sink()).expect("the pane starts");

        // The pane stays: the tool's line and the notice are on the screen, and nothing ended.
        assert!(wait_until(WAIT, || !probe.helds().is_empty()), "no Held event: {:?}", probe.events());
        assert!(probe.has_text("Error from server (NotFound): pods \"x\" not found"), "the tool's own error line is on screen");
        assert!(
            wait_until(WAIT, || probe.has_text("[holzWave] exited with status 1. Press Enter to close.")),
            "the notice is on screen"
        );
        assert_eq!(probe.helds(), [Some(1)], "one Held with the status");
        assert!(probe.exits().is_empty(), "no Exited yet");

        // Waiting does not close it, and a letter does not either; the pane lives longer than the linger time.
        std::thread::sleep(LAUNCH_LINGER + Duration::from_millis(500));
        let letter = DomKey { code: "KeyA".into(), key: "a".into(), mods: Mods::NONE, ..DomKey::default() };
        let _ = pane.key(letter);
        std::thread::sleep(Duration::from_millis(300));
        assert!(probe.exits().is_empty(), "the pane ended without Enter: {:?}", probe.events());
        assert!(started.elapsed() > LAUNCH_LINGER, "the check ran after the linger time");

        // Enter ends it, with the tool's status, once.
        pane.key(enter()).expect("the held pane takes a key");
        assert!(wait_until(WAIT, || !probe.exits().is_empty()), "Enter did not end the pane: {:?}", probe.events());
        assert!(pane.join(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(probe.exits(), [Some(1)], "exactly one Exited, with the tool's status");
        assert_eq!(probe.helds(), [Some(1)], "and still one Held");
        assert!(matches!(probe.events().last(), Some(PaneEvent::Exited { code: Some(1) })), "nothing after Exited");
    }
}
