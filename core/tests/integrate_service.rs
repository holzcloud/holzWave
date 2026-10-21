//! The integration service: a request goes from the page's words to a model through the detector, the runner and the
//! tool modules, and fails in fixed, readable ways.
//!
//! Nothing here uses a real tool, a cluster or an account. The tools are fake programs (POSIX shell scripts of the
//! rig) in a scratch directory that print the recorded fixtures, and the detector finds them through facts that are
//! injected: it sees the scratch directory and nothing else on this computer, so a `kubectl` that is installed here
//! can never stand in for the fake. The tests are Unix only.
#![cfg(unix)]

mod common;
#[path = "common/fake_cli.rs"]
mod fake_cli;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use holzwave_core::integrate::service::{FaultCode, HostFacts, IntegrationService, ToolState};
use holzwave_core::integrate::view::ViewModel;
use holzwave_core::integrate::ToolId;
use holzwave_core::secrets::MemoryStore;

use common::scratch_dir;
use fake_cli::FakeCli;

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
        let root = scratch_dir("service");
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

/// An argument bag from pairs.
fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

/// A machine whose environment holds a variable of the app, a loader variable and a harmless one.
fn dirty_facts(rig: &Rig) -> HostFacts {
    rig.facts().with_env("HOLZWAVE_TOKEN", "app-secret").with_env("LD_PRELOAD", "/tmp/x.so").with_env("FOO", "bar")
}

/// A fake for each of the four tools.
fn all_four_fakes(rig: &Rig) -> Vec<FakeCli> {
    ["kubectl", "talosctl", "az", "gam"].iter().map(|name| FakeCli::new(&rig.bin, name)).collect()
}

fn argv_of(fake: &FakeCli) -> Vec<String> {
    fake.argv().into_iter().map(|a| String::from_utf8(a).expect("text arguments")).collect()
}

// ---- the tracer ----------------------------------------------------------------------------------------------------

#[test]
fn a_read_goes_from_request_to_model_through_a_fake_kubectl() {
    let rig = Rig::new();
    let fake = FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json"));
    let service = rig.service();

    let tools = service.tools();
    let kube = tools.iter().find(|tool| tool.id == ToolId::Kube).expect("a Kubernetes entry");
    assert_eq!(kube.state, ToolState::Found, "the detector finds the fake on the injected PATH");
    assert_eq!(kube.path.as_deref(), fake.path().to_str(), "with its absolute path");

    let answer = service.run_view("kube", "contexts", None, &args(&[])).expect("the read gives a model");
    let ViewModel::Table(table) = &answer.view else { panic!("contexts is a table, got {:?}", answer.view) };
    assert_eq!(table.rows().len(), 3, "the three contexts of the recorded kubeconfig");
    assert!(answer.fetched > 1_600_000_000, "the time of the read is in the answer: {}", answer.fetched);
    assert_eq!(table.fetched(), Some(answer.fetched), "and in the model");
    assert_eq!(argv_of(&fake), ["config", "view", "-o", "json"], "the tool module built the argument list");
}

#[test]
fn an_unknown_or_non_read_verb_is_refused() {
    let rig = Rig::new();
    let fake = FakeCli::new(&rig.bin, "kubectl").prints("never printed");
    let service = rig.service();
    let hostile = "$(touch /tmp/never)";
    let cases: [(&str, &str, BTreeMap<String, String>); 7] = [
        ("kube", "nonesuch", args(&[])),
        ("kube", "delete_pod", args(&[("pod", "api-0")])),
        ("kube", "use_context", args(&[("context", "dev")])),
        ("kube", "exec", args(&[("pod", "api-0")])),
        ("kube", "logs", args(&[("pod", "api-0")])),
        ("nonesuch", "contexts", args(&[])),
        ("kube", "contexts", args(&[("anything", hostile)])),
    ];
    for (tool, verb, request) in cases {
        let fault = service.run_view(tool, verb, None, &request).expect_err(&format!("{tool} {verb} is refused"));
        assert_eq!(fault.code, FaultCode::Refused, "{tool} {verb}");
        assert!(!fault.text.is_empty(), "{tool} {verb}: a sentence to read");
        assert!(!fault.text.contains("touch"), "the refusal never repeats the input: {}", fault.text);
    }
    assert!(!fake.started(), "no process was started for a refused request");
}

#[test]
fn a_missing_tool_is_a_readable_fault() {
    let rig = Rig::new();
    let service = rig.service();
    let fault = service.run_view("kube", "contexts", None, &args(&[])).expect_err("nothing to run");
    assert_eq!(fault.code, FaultCode::Missing);
    assert!(fault.text.contains("kubectl"), "the text names the program: {}", fault.text);
    let tools = service.tools();
    let kube = tools.iter().find(|tool| tool.id == ToolId::Kube).expect("an entry");
    assert_eq!(kube.state, ToolState::NotFound);
    assert!(kube.path.is_none());
}

// ---- the reads of all four tools, the signed-out state, the defaults and every fault ------------------------------

use std::time::Duration;

use holzwave_core::integrate::context::RowState;
use holzwave_core::integrate::run::RunLimits;
use holzwave_core::integrate::service::RunAnswer;

impl Rig {
    fn service_over(&self, facts: HostFacts) -> IntegrationService {
        IntegrationService::new(&self.data, self.store.clone(), facts, &[])
    }
}

fn kind(answer: &RunAnswer) -> &'static str {
    match &answer.view {
        ViewModel::Table(_) => "table",
        ViewModel::Kv(_) => "kv",
        ViewModel::Text(_) => "text",
    }
}

#[test]
fn every_tool_reads_through_the_same_path() {
    let rig = Rig::new();
    let facts = dirty_facts(&rig);
    let service = rig.service_over(facts);
    let cases: [(&str, &str, &str, &str, &str); 10] = [
        ("kubectl", "kube", "contexts", "kubectl/config_view.json", "table"),
        ("kubectl", "kube", "namespaces", "kubectl/namespaces.json", "table"),
        ("kubectl", "kube", "pods", "kubectl/pods.json", "table"),
        ("az", "azure", "account", "az/account_show.json", "kv"),
        ("az", "azure", "subscriptions", "az/account_list.json", "table"),
        ("az", "azure", "groups", "az/group_list.json", "table"),
        ("gam", "gam", "sections", "gam/showsections.txt", "table"),
        ("gam", "gam", "users", "gam/print_users.csv", "table"),
        ("talosctl", "talos", "contexts", "talosctl/config_contexts.txt", "table"),
        ("talosctl", "talos", "nodes", "talosctl/get_members.json", "table"),
    ];
    for (program, tool, verb, file, want) in cases {
        let fake = FakeCli::new(&rig.bin, program).prints(&fixture(file));
        let answer = service.run_view(tool, verb, None, &args(&[])).unwrap_or_else(|e| panic!("{tool} {verb}: {e}"));
        assert_eq!(kind(&answer), want, "{tool} {verb}");
        assert!(!answer.view.title().is_empty(), "{tool} {verb}: a title");
        let env = fake.env();
        assert_eq!(env.get("FOO").map(String::as_str), Some("bar"), "{tool} {verb}: the inherited environment passes");
        assert!(!env.contains_key("HOLZWAVE_TOKEN"), "{tool} {verb}: the app's own variables never reach a child");
        assert!(!env.contains_key("LD_PRELOAD"), "{tool} {verb}: nor does a loader variable");
        let path = env.get("PATH").expect("a PATH");
        let first = format!("{}:", rig.bin.display());
        assert!(path.starts_with(&first), "{tool} {verb}: the tool's directory is first: {path}");
        if tool == "azure" {
            let telemetry = env.get("AZURE_CORE_COLLECT_TELEMETRY").map(String::as_str);
            assert_eq!(telemetry, Some("false"), "{tool} {verb}: the tool's fixed variable reaches the child");
        }
    }
    let node = args(&[("node", "192.0.2.11")]);
    let _talosctl = FakeCli::new(&rig.bin, "talosctl").prints(&fixture("talosctl/get_services.json"));
    service.run_view("talos", "services", None, &node).expect("a declared optional argument is accepted");
}

#[test]
fn signed_out_is_a_state_for_the_account_verb_only() {
    let rig = Rig::new();
    let _az = FakeCli::new(&rig.bin, "az").prints_stderr(&fixture("az/account_show_signed_out.stderr.txt")).exits(1);
    let service = rig.service();
    let answer = service.run_view("azure", "account", None, &args(&[])).expect("signed out is an answer, not a fault");
    let ViewModel::Kv(view) = &answer.view else { panic!("the account is a key-value view, got {:?}", answer.view) };
    assert!(
        view.rows().iter().any(|row| row.label() == "State" && row.value() == "Signed out"),
        "the model says Signed out"
    );
    let fault = service.run_view("azure", "subscriptions", None, &args(&[])).expect_err("the same exit elsewhere");
    assert_eq!(fault.code, FaultCode::Failed);
    let rows = service.context_rows(None, false);
    let azure = rows.iter().find(|row| row.tool == ToolId::Azure).expect("an Azure row");
    assert_eq!(azure.state, RowState::SignedOut, "the tool's default is marked signed out");
}

#[test]
fn a_default_read_sets_the_tools_own_selection() {
    let rig = Rig::new();
    let service = rig.service();
    let rows = service.context_rows(None, false);
    assert!(rows.iter().all(|row| row.state == RowState::Unknown), "nothing is known before a read");

    let _kubectl = FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json"));
    let _az = FakeCli::new(&rig.bin, "az").prints(&fixture("az/account_show.json"));
    let _gam = FakeCli::new(&rig.bin, "gam").prints(&fixture("gam/showsections.txt"));
    let _talosctl = FakeCli::new(&rig.bin, "talosctl").prints(&fixture("talosctl/config_contexts.txt"));
    for (tool, verb) in [("kube", "contexts"), ("azure", "account"), ("gam", "sections"), ("talos", "contexts")] {
        service.run_view(tool, verb, None, &args(&[])).unwrap_or_else(|e| panic!("{tool} {verb}: {e}"));
    }
    let rows = service.context_rows(None, false);
    let row = |tool: ToolId| rows.iter().find(|row| row.tool == tool).expect("a row per tool").clone();
    assert_eq!(row(ToolId::Kube).context.as_deref(), Some("dev"), "the current context of the kubeconfig");
    assert_eq!(row(ToolId::Kube).state, RowState::Found);
    assert_eq!(row(ToolId::Azure).subscription.as_deref(), Some("00000000-0000-4000-8000-000000000002"));
    assert_eq!(row(ToolId::Azure).state, RowState::Found, "signed in");
    assert_eq!(row(ToolId::Gam).section.as_deref(), Some("DEFAULT"));
    assert_eq!(row(ToolId::Talos).context.as_deref(), Some("demo-prod"));

    // A read that carries a selection of its own says nothing about the tool's own selection.
    let selection = holzwave_core::integrate::Selection { context: Some("kind-test".into()), ..Default::default() };
    service.select("kube", Some(5), &selection).expect("a selection");
    service.run_view("kube", "contexts", Some(5), &args(&[])).expect("a read for the pane");
    let rows = service.context_rows(None, false);
    let kube = rows.iter().find(|row| row.tool == ToolId::Kube).expect("a row");
    assert_eq!(kube.context.as_deref(), Some("dev"), "the default is still the tool's own");
    let pane_rows = service.context_rows(Some(5), false);
    let kube = pane_rows.iter().find(|row| row.tool == ToolId::Kube).expect("a row");
    assert_eq!(kube.context.as_deref(), Some("kind-test"), "the pane shows its own");
}

#[test]
fn a_failing_tool_gives_a_cleaned_bounded_fault() {
    let rig = Rig::new();
    let message = format!("\u{1b}[31mError from server: {}\u{1b}]0;title\u{7}\nsecond line\n", "x".repeat(2000));
    let _kubectl = FakeCli::new(&rig.bin, "kubectl").prints_stderr(&message).exits(1);
    let service = rig.service();
    let fault = service.run_view("kube", "pods", None, &args(&[])).expect_err("the tool failed");
    assert_eq!(fault.code, FaultCode::Failed);
    let prefix = "Kubernetes reported an error: ";
    assert!(fault.text.starts_with(prefix), "the label comes first: {}", fault.text);
    let shown = &fault.text[prefix.len()..];
    assert!(shown.chars().count() <= 240, "at most 240 characters of the message: {}", shown.chars().count());
    assert!(shown.starts_with("Error from server: xxx"), "the first line, cleaned: {shown}");
    let dirty = |c: char| fault.text.contains(c);
    assert!(!dirty('\u{1b}') && !dirty('\n') && !dirty('\u{7}'), "one clean line");
    assert!(!fault.text.contains("second line"), "only the first line");
    let debugged = format!("{fault:?}");
    assert!(!debugged.contains("Error from server"), "the debug text does not carry the tool's message: {debugged}");

    let _quiet = FakeCli::new(&rig.bin, "kubectl").exits(3);
    let fault = service.run_view("kube", "pods", None, &args(&[])).expect_err("a failure without a message");
    assert_eq!(fault.code, FaultCode::Failed);
    assert!(fault.text.starts_with("Kubernetes reported an error"), "{}", fault.text);
}

#[test]
fn timeout_overflow_and_unreadable_output_are_distinct() {
    let rig = Rig::new();
    let limits = RunLimits { max_stdout: 1024 * 1024, timeout: Duration::from_millis(300) };
    let service = rig.service().with_limits(limits);

    let _slow = FakeCli::new(&rig.bin, "kubectl").sleeps(5);
    let fault = service.run_view("kube", "pods", None, &args(&[])).expect_err("too slow");
    assert_eq!(fault.code, FaultCode::Timeout);
    assert!(fault.text.contains("Kubernetes"), "{}", fault.text);

    let _big = FakeCli::new(&rig.bin, "kubectl").prints_bytes(3 * 1024 * 1024);
    let fault = service.run_view("kube", "pods", None, &args(&[])).expect_err("too large");
    assert_eq!(fault.code, FaultCode::Overflow);

    let _odd = FakeCli::new(&rig.bin, "kubectl").prints("this is not a list of pods");
    let answer = service.run_view("kube", "pods", None, &args(&[])).expect("drift is a text, not a fault");
    let ViewModel::Text(text) = &answer.view else { panic!("a plain text model, got {:?}", answer.view) };
    assert!(text.note().is_some(), "with its note");
    assert!(text.text().contains("this is not a list of pods"));
}

#[test]
fn a_kubeconfig_that_cannot_be_read_is_not_shown_as_text() {
    let rig = Rig::new();
    let output = "{\"token\": \"CANARY-TOKEN-VALUE\", \"not\": \"a config\"}";
    let _kubectl = FakeCli::new(&rig.bin, "kubectl").prints(output);
    let service = rig.service();
    let answer = service.run_view("kube", "contexts", None, &args(&[])).expect("an answer");
    let shown = format!("{:?}", answer.view);
    assert!(!shown.contains("CANARY-TOKEN-VALUE"), "a kubeconfig can hold credentials and is never echoed: {shown}");
}

#[test]
fn the_version_a_tool_prints_is_shown_and_a_failing_probe_does_not_make_it_unusable() {
    let rig = Rig::new();
    let _kubectl = FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/version_client.json"));
    let _az = FakeCli::new(&rig.bin, "az").prints("not a version").exits(1);
    let service = rig.service();
    let tools = service.tools();
    let kube = tools.iter().find(|tool| tool.id == ToolId::Kube).expect("an entry");
    let want = holzwave_core::integrate::kube::parse_version(&fixture("kubectl/version_client.json"));
    assert!(want.is_some(), "the fixture has a version");
    assert_eq!(kube.version, want);
    let azure = tools.iter().find(|tool| tool.id == ToolId::Azure).expect("an entry");
    assert_eq!(azure.state, ToolState::Found, "a failing version probe is no reason to refuse the tool");
    assert!(azure.version.is_none());
}

// ---- the verbs with a card, the launches and the three trait methods in use ---------------------------------------

use holzwave_core::integrate::args::{ContextName, ProfileId};
use holzwave_core::integrate::kube::POD_SHELL;
use holzwave_core::integrate::profiles::IntegrationProfile;
use holzwave_core::integrate::service::Done;
use holzwave_core::integrate::{ArgKind, IntegrationSpec, Selection};

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

fn selecting(context: &str, namespace: &str) -> Selection {
    Selection { context: Some(context.into()), namespace: Some(namespace.into()), ..Selection::default() }
}

#[test]
fn a_pod_deletion_runs_once_after_its_card_is_confirmed() {
    let rig = Rig::new();
    let fake = counted(FakeCli::new(&rig.bin, "kubectl").prints("pod \"api-0\" deleted\n"));
    let service = rig.service();
    service.select("kube", Some(3), &selecting("dev", "payments")).expect("a selection");

    let card = service.prepare("kube", "delete_pod", Some(3), &args(&[("pod", "api-0")])).expect("a card");
    let want = [
        "delete",
        "pod",
        "--context=dev",
        "--namespace=payments",
        "--wait=false",
        "--request-timeout=10s",
        "--",
        "api-0",
    ];
    assert_eq!(card.argv, want, "the card shows the argument list the tool module built");
    assert!(card.target.contains(&("context".to_owned(), "dev".to_owned())), "{:?}", card.target);
    assert!(card.target.contains(&("namespace".to_owned(), "payments".to_owned())), "{:?}", card.target);
    assert_eq!(runs(&fake), 0, "a card starts nothing");

    let done = service.confirm(&card.ticket, &card.hash).expect("the confirm runs it");
    let Done::Ran { note } = done else { panic!("a write verb runs, got {done:?}") };
    assert_eq!(note, "pod \"api-0\" deleted", "the outcome is the first line of the tool's answer");
    assert_eq!(runs(&fake), 1);
    assert_eq!(argv_of(&fake), card.argv, "what ran is what the card showed");

    let again = service.confirm(&card.ticket, &card.hash).expect_err("a ticket approves once");
    assert_eq!(again.code, FaultCode::Ticket);
    assert_eq!(runs(&fake), 1, "and the second confirm ran nothing");
}

#[test]
fn a_changed_selection_between_card_and_click_runs_nothing() {
    let rig = Rig::new();
    let fake = counted(FakeCli::new(&rig.bin, "kubectl").prints("deleted\n"));
    let service = rig.service();
    service.select("kube", Some(3), &selecting("dev", "payments")).expect("a selection");
    let card = service.prepare("kube", "delete_pod", Some(3), &args(&[("pod", "api-0")])).expect("a card");

    let wrong = service.confirm(&card.ticket, &"0".repeat(64)).expect_err("a hash that is not the card's");
    assert_eq!(wrong.code, FaultCode::Ticket);

    service.select("kube", Some(3), &selecting("kind-test", "payments")).expect("another cluster");
    let fault = service.confirm(&card.ticket, &card.hash).expect_err("the request is not the one that was shown");
    assert_eq!(fault.code, FaultCode::Ticket);
    assert_eq!(runs(&fake), 0, "nothing ran");
}

#[test]
fn a_production_profile_makes_the_card_say_so() {
    let rig = Rig::new();
    let _fake = FakeCli::new(&rig.bin, "kubectl");
    let service = rig.service();
    let mut profile = IntegrationProfile::new(ProfileId::parse("prod-eu").expect("an id"), ToolId::Kube, "Prod EU");
    profile.context = Some(ContextName::parse("dev").expect("a name"));
    profile.production = true;
    service.save_profile(profile).expect("a profile");
    service.select("kube", Some(3), &selecting("dev", "payments")).expect("a selection");
    let card = service.prepare("kube", "delete_pod", Some(3), &args(&[("pod", "api-0")])).expect("a card");
    assert_eq!(card.guard.production.as_deref(), Some("Prod EU"));
    assert!(card.fields.contains(&("guard".to_owned(), "production".to_owned())));
    assert!(card.fields.contains(&("production_name".to_owned(), "Prod EU".to_owned())));

    service.select("kube", Some(4), &selecting("kind-test", "payments")).expect("a selection");
    let card = service.prepare("kube", "delete_pod", Some(4), &args(&[("pod", "api-0")])).expect("a card");
    assert_eq!(card.guard.production, None, "another context is not production");
}

#[test]
fn a_local_verb_names_what_it_changes_and_marks_the_default_stale() {
    let rig = Rig::new();
    let _fake = counted(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json")));
    let service = rig.service();
    service.run_view("kube", "contexts", None, &args(&[])).expect("the default read");
    let rows = service.context_rows(None, false);
    let kube = rows.iter().find(|row| row.tool == ToolId::Kube).expect("a row");
    assert!(!kube.stale, "a value that was just read is not stale");

    let fake = counted(FakeCli::new(&rig.bin, "kubectl").prints("Switched to context \"kind-test\".\n"));
    let card = service.prepare("kube", "use_context", None, &args(&[("context", "kind-test")])).expect("a card");
    let changes = "the kubeconfig files named by KUBECONFIG, by default ~/.kube/config";
    assert!(card.target.contains(&("changes".to_owned(), changes.to_owned())), "{:?}", card.target);
    assert!(card.target.contains(&("context".to_owned(), "kind-test".to_owned())), "the context it switches to");
    assert_eq!(card.argv, ["config", "use-context", "--", "kind-test"]);

    let read = service.prepare("kube", "pods", None, &args(&[])).expect_err("a read needs no card");
    assert_eq!(read.code, FaultCode::Refused);

    let before = runs(&fake);
    let done = service.confirm(&card.ticket, &card.hash).expect("the confirm runs it");
    let Done::Ran { note } = done else { panic!("a local verb runs, got {done:?}") };
    assert_eq!(note, "Switched to context \"kind-test\".");
    assert_eq!(runs(&fake), before + 1);
    let rows = service.context_rows(None, false);
    let kube = rows.iter().find(|row| row.tool == ToolId::Kube).expect("a row");
    assert!(kube.stale, "the tool's own selection may have changed, so it is marked");
    assert_eq!(kube.context.as_deref(), Some("dev"), "the value is kept");
    let after = runs(&fake);
    service.run_view("kube", "contexts", None, &args(&[])).expect("a read");
    assert_eq!(runs(&fake), after + 1, "the cached views of the tool were dropped, so the read runs again");
}

#[test]
fn every_local_verb_names_what_it_changes() {
    let rig = Rig::new();
    for program in ["kubectl", "talosctl", "az", "gam"] {
        let _ = FakeCli::new(&rig.bin, program);
    }
    let service = rig.service();
    let cases = [
        ("kube", "use_context", args(&[("context", "dev")])),
        ("kube", "set_namespace", args(&[("namespace", "payments")])),
        ("talos", "use_context", args(&[("context", "demo-prod")])),
        ("azure", "set_subscription", args(&[("subscription", "00000000-0000-4000-8000-000000000002")])),
    ];
    for (tool, verb, request) in cases {
        let card = service.prepare(tool, verb, None, &request).unwrap_or_else(|e| panic!("{tool} {verb}: {e}"));
        let found = card.target.iter().find(|(label, _)| label == "changes");
        let changes = found.unwrap_or_else(|| panic!("{tool} {verb}: {:?}", card.target));
        assert!(changes.1.len() > 20, "{tool} {verb}: a sentence: {}", changes.1);
    }
}

#[test]
fn exec_confirms_into_a_clean_launch_and_starts_nothing() {
    let rig = Rig::new();
    let fake = FakeCli::new(&rig.bin, "kubectl");
    let facts = dirty_facts(&rig);
    let service = rig.service_over(facts);
    service.select("kube", Some(3), &selecting("dev", "payments")).expect("a selection");
    let card = service.prepare("kube", "exec", Some(3), &args(&[("pod", "api-0")])).expect("a card");
    assert_eq!(card.guard.production, None);

    let done = service.confirm(&card.ticket, &card.hash).expect("the confirm gives a launch");
    let Done::Launch(launch) = done else { panic!("an exec verb launches, got {done:?}") };
    assert!(!fake.started(), "the confirm started nothing: the pane does");
    assert_eq!(launch.program, fake.path());
    let head = ["exec", "-i", "-t", "--context=dev", "--namespace=payments", "api-0", "--", "sh", "-c"];
    let mut want: Vec<OsString> = head.iter().map(OsString::from).collect();
    want.push(OsString::from(POD_SHELL));
    assert_eq!(launch.args, want, "the argument list of the card");
    let names: Vec<&String> = launch.env.iter().map(|(name, _)| name).collect();
    assert!(names.iter().all(|name| !name.starts_with("HOLZWAVE_")), "no variable of the app: {names:?}");
    let path = launch.env.iter().find(|(name, _)| name == "PATH").expect("the rebuilt PATH");
    assert!(path.1.starts_with(&format!("{}:", rig.bin.display())), "the tool's directory first: {}", path.1);
    assert_eq!(launch.env_remove, ["HOLZWAVE_TOKEN", "LD_PRELOAD"], "the pane removes what it inherits");
    assert_eq!(launch.label, "Exec in pod api-0");
}

#[test]
fn read_terminals_launch_without_a_ticket_and_exec_cannot() {
    let rig = Rig::new();
    let kubectl = FakeCli::new(&rig.bin, "kubectl");
    let talosctl = FakeCli::new(&rig.bin, "talosctl");
    let _az = FakeCli::new(&rig.bin, "az");
    let service = rig.service();

    let logs = service.launch("kube", "logs", None, &args(&[("pod", "api-0")])).expect("logs launch");
    let want: Vec<OsString> = ["logs", "--follow", "--tail=200", "--", "api-0"].iter().map(OsString::from).collect();
    assert_eq!(logs.args, want);
    assert_eq!(logs.label, "Logs in pod api-0");
    let dmesg = service.launch("talos", "dmesg", None, &args(&[])).expect("dmesg launches");
    assert_eq!(dmesg.program, talosctl.path());
    assert_eq!(dmesg.args, [OsString::from("dmesg"), OsString::from("--follow")]);

    for (tool, verb, request) in [
        ("kube", "exec", args(&[("pod", "api-0")])),
        ("azure", "login", args(&[])),
        ("talos", "dashboard", args(&[])),
        ("kube", "delete_pod", args(&[("pod", "api-0")])),
        ("kube", "use_context", args(&[("context", "dev")])),
        ("kube", "contexts", args(&[])),
        ("nonesuch", "shell", args(&[])),
    ] {
        let fault = service.launch(tool, verb, None, &request).expect_err("not a launch without a card");
        assert_eq!(fault.code, FaultCode::Refused, "{tool} {verb}");
    }
    assert!(!kubectl.started(), "a launch only describes the program");
}

#[test]
fn the_shell_launch_gets_the_overlay() {
    let rig = Rig::new();
    let _fake = FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json"));
    let shell = holzwave_core::platform::ShellLaunch { program: "/bin/zsh".to_owned(), args: vec!["-l".to_owned()] };
    let service = rig.service_over(rig.facts().with_shell(shell));
    service.select("kube", Some(7), &selecting("kind-test", "payments")).expect("a selection");

    let launch = service.launch("kube", "shell", Some(7), &args(&[])).expect("the shell launches");
    assert_eq!(launch.program, PathBuf::from("/bin/zsh"), "the person's own shell");
    assert_eq!(launch.args, [OsString::from("-l")]);
    let overlay = rig.data.join("run").join("kube").join("7.json");
    let kubeconfig = launch.env.iter().find(|(name, _)| name == "KUBECONFIG").expect("KUBECONFIG is set");
    assert_eq!(kubeconfig.1, format!("{}:{}", overlay.display(), rig.home.join(".kube").join("config").display()));
    let text = std::fs::read_to_string(&overlay).expect("the overlay exists below the data directory");
    assert!(text.contains("kind-test") && text.contains("payments"), "it names the context and the namespace: {text}");
    for secret in ["REDACTED", "\"token\"", "certificate-authority-data", "client-key", "exec", "server"] {
        assert!(!text.contains(secret), "the overlay holds names only, not {secret}: {text}");
    }
    assert!(launch.env.iter().all(|(name, _)| !name.starts_with("HOLZWAVE_")));
}

/// A value for every argument a verb declares, so that a refusal can only come from the lane.
fn valid_args(spec: &IntegrationSpec, verb: &str) -> BTreeMap<String, String> {
    let verb = spec.verb(verb).expect("a verb of the spec");
    verb.args
        .iter()
        .map(|arg| {
            let value = match arg.kind {
                ArgKind::Context => "dev".to_owned(),
                ArgKind::Namespace => "payments".to_owned(),
                ArgKind::Pod => "api-0".to_owned(),
                ArgKind::Container => "app".to_owned(),
                ArgKind::Node => "192.0.2.11".to_owned(),
                ArgKind::Subscription => "00000000-0000-4000-8000-000000000002".to_owned(),
                ArgKind::Section => "DEFAULT".to_owned(),
                ArgKind::Lines => "10".to_owned(),
                ArgKind::Flag => "true".to_owned(),
                ArgKind::Choice(words) => words[0].to_owned(),
            };
            (arg.name.to_owned(), value)
        })
        .collect()
}

#[test]
fn no_gated_verb_runs_without_a_ticket() {
    use holzwave_core::integrate::{azure, gam, kube, talos, Output, VerbEffect};
    let rig = Rig::new();
    let fakes = all_four_fakes(&rig);
    let service = rig.service();
    let mut checked = 0;
    for spec in [&kube::SPEC, &talos::SPEC, &azure::SPEC, &gam::SPEC] {
        let tool = spec.id.as_str();
        for verb in spec.verbs {
            let request = valid_args(spec, verb.id);
            if verb.effect != VerbEffect::Read || verb.output != Output::View {
                let fault = service.run_view(tool, verb.id, None, &request).expect_err("only a read with a view");
                assert_eq!(fault.code, FaultCode::Refused, "run_view {tool} {}", verb.id);
                checked += 1;
            }
            if verb.effect != VerbEffect::Read || verb.output != Output::Terminal {
                let fault = service.launch(tool, verb.id, None, &request).expect_err("only a read with a terminal");
                assert_eq!(fault.code, FaultCode::Refused, "launch {tool} {}", verb.id);
                checked += 1;
            }
            if verb.effect == VerbEffect::Read {
                let fault = service.prepare(tool, verb.id, None, &request).expect_err("a read has no card");
                assert_eq!(fault.code, FaultCode::Refused, "prepare {tool} {}", verb.id);
                checked += 1;
            }
        }
    }
    assert!(checked > 40, "the matrix was walked: {checked}");
    assert!(fakes.iter().all(|fake| !fake.started()), "no lane started a process for another lane's verb");
}

// ---- single flight and the cache, secret variables, the Flatpak refusal and cleanup -------------------------------

use std::sync::Barrier;

use holzwave_core::integrate::service::ProfilesView;

/// Makes a fake log `start` and `end` around a pause of `secs` seconds (the pause is in the settings the script
/// sources, so it replaces nothing of the rig). The log shows whether two runs overlapped.
fn logged(fake: FakeCli, secs: u32) -> FakeCli {
    use std::io::Write;
    let path = format!("{}.conf", fake.path().display());
    let mut conf = std::fs::OpenOptions::new().append(true).open(path).expect("the settings of the fake");
    writeln!(conf, "echo start >> \"$d/$n.log\"; sleep {secs}; echo end >> \"$d/$n.log\"").expect("one more line");
    fake
}

fn log(fake: &FakeCli) -> Vec<String> {
    std::fs::read_to_string(format!("{}.log", fake.path().display()))
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn starts(fake: &FakeCli) -> usize {
    log(fake).iter().filter(|line| *line == "start").count()
}

#[test]
fn identical_requests_share_one_run_and_a_young_answer_is_served_again() {
    let rig = Rig::new();
    let fake = logged(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/pods.json")), 1);
    let service = rig.service();
    let request = args(&[]);

    let gate = Barrier::new(20);
    let answers: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..20)
            .map(|_| {
                scope.spawn(|| {
                    gate.wait();
                    service.run_view("kube", "pods", Some(3), &request)
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().expect("a thread")).collect()
    });
    assert_eq!(starts(&fake), 1, "twenty identical requests at the same moment cause one run");
    let first = answers[0].as_ref().expect("an answer");
    assert!(answers.iter().all(|answer| answer.as_ref().ok() == Some(first)), "and all get the same answer");

    std::thread::sleep(Duration::from_millis(200));
    let again = service.run_view("kube", "pods", Some(3), &request).expect("served from the cache");
    assert_eq!(starts(&fake), 1, "a young answer is served again");
    assert_eq!(&again, first);

    std::thread::sleep(Duration::from_millis(1200));
    service.run_view("kube", "pods", Some(3), &request).expect("a new run");
    assert_eq!(starts(&fake), 2, "an old answer is not");

    // Two panes are two requests, and they run side by side.
    let before = log(&fake).len();
    let gate = Barrier::new(2);
    std::thread::scope(|scope| {
        for pane in [4, 5] {
            let (service, gate, request) = (&service, &gate, &request);
            scope.spawn(move || {
                gate.wait();
                service.run_view("kube", "pods", Some(pane), request).expect("a read");
            });
        }
    });
    let added: Vec<String> = log(&fake)[before..].to_vec();
    assert_eq!(added, ["start", "start", "end", "end"], "different panes run at the same time");
}

#[test]
fn one_pane_never_has_two_runs_at_once() {
    let rig = Rig::new();
    let fake = logged(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/pods.json")), 1);
    let service = rig.service();
    let gate = Barrier::new(2);
    std::thread::scope(|scope| {
        for verb in ["pods", "namespaces"] {
            let (service, gate) = (&service, &gate);
            scope.spawn(move || {
                gate.wait();
                service.run_view("kube", verb, Some(3), &args(&[])).expect("a read");
            });
        }
    });
    assert_eq!(log(&fake), ["start", "end", "start", "end"], "the second request of a pane waits for the first");
}

#[test]
fn a_busy_runner_is_a_busy_fault() {
    let rig = Rig::new();
    let _fake = logged(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/pods.json")), 2);
    let service = rig.service();
    let gate = Barrier::new(5);
    let results: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (10..15)
            .map(|pane| {
                let (service, gate) = (&service, &gate);
                scope.spawn(move || {
                    gate.wait();
                    service.run_view("kube", "pods", Some(pane), &args(&[]))
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().expect("a thread")).collect()
    });
    let is_busy = |result: &&Result<_, holzwave_core::integrate::service::Fault>| {
        result.as_ref().is_err_and(|fault| fault.code == FaultCode::Busy)
    };
    let busy = results.iter().filter(is_busy).count();
    let ok = results.iter().filter(|result| result.is_ok()).count();
    let codes: Vec<_> = results.iter().map(|result| result.as_ref().map(|_| ()).map_err(|fault| fault.code)).collect();
    assert_eq!((busy, ok), (1, 4), "four slots, so the fifth call is told to try again: {codes:?}");
}

/// Longer than the time an answer is served again.
const CACHE_PAUSE: Duration = Duration::from_millis(1100);

const CANARY: &str = "CANARY-8f3a91c2d7e45b60-SECRET";

fn aws_profile() -> IntegrationProfile {
    let mut profile = IntegrationProfile::new(ProfileId::parse("aws-prod").expect("an id"), ToolId::Kube, "AWS prod");
    profile.env.insert("AWS_PROFILE".to_owned(), "prod".to_owned());
    profile.secret_env.push("AWS_SECRET_ACCESS_KEY".to_owned());
    profile
}

#[test]
fn a_secret_variable_reaches_the_child_environment_only() {
    let rig = Rig::new();
    let fake = FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/pods.json"));
    let facts = rig.facts();
    let service = rig.service_over(rig.facts());
    service.save_profile(aws_profile()).expect("a profile");
    service.set_secret("aws-prod", "AWS_SECRET_ACCESS_KEY", CANARY).expect("a value in the keychain");
    let with_profile = Selection { profile: Some("aws-prod".into()), ..Selection::default() };
    service.select("kube", Some(3), &with_profile).expect("a selection");

    let answer = service.run_view("kube", "pods", Some(3), &args(&[])).expect("a read");
    let env = fake.env();
    assert_eq!(env.get("AWS_SECRET_ACCESS_KEY").map(String::as_str), Some(CANARY), "the child has the value");
    assert_eq!(env.get("AWS_PROFILE").map(String::as_str), Some("prod"), "and the plain variable of the profile");

    // A failing run, a card, a launch: every output the value could reach.
    let failing = FakeCli::new(&rig.bin, "kubectl").prints_stderr("boom: the server said no").exits(1);
    let fault = service.run_view("kube", "namespaces", Some(3), &args(&[])).expect_err("a failure");
    let card = service.prepare("kube", "delete_pod", Some(3), &args(&[("pod", "api-0")])).expect("a card");
    let exec = service.prepare("kube", "exec", Some(3), &args(&[("pod", "api-0")])).expect("a card");
    let done = service.confirm(&exec.ticket, &exec.hash).expect("a launch");
    let Done::Launch(launch) = done else { panic!("an exec verb launches") };
    let logs = service.launch("kube", "logs", Some(3), &args(&[("pod", "api-0")])).expect("a launch");
    let profiles = service.profiles();
    let tools = service.tools();
    let outputs: Vec<(&str, String)> = vec![
        ("the argument log of the fake", argv_of(&failing).join(" ")),
        ("the answer", format!("{answer:?} {}", serde_json::to_string(&answer).expect("json"))),
        ("the fault", format!("{fault:?} {} {}", fault.text, serde_json::to_string(&fault).expect("json"))),
        ("the card", format!("{card:?} {}", serde_json::to_string(&card).expect("json"))),
        ("the card of the exec", serde_json::to_string(&exec).expect("json")),
        ("the launch (without its secrets)", format!("{launch:?} {:?} {:?}", launch.args, launch.env)),
        ("the launch of the logs", format!("{logs:?} {:?} {:?}", logs.args, logs.env)),
        ("the service", format!("{service:?}")),
        ("the host facts", format!("{facts:?}")),
        ("the profiles", format!("{profiles:?} {}", serde_json::to_string(&profiles).expect("json"))),
        ("the tools", format!("{tools:?}")),
        ("the notes", service.notes().join(" ")),
        ("the pane rows", format!("{:?}", service.pane_rows())),
        ("the context rows", format!("{:?}", service.context_rows(Some(3), false))),
    ];
    for (what, text) in &outputs {
        assert!(!text.contains(CANARY), "the secret value is in {what}");
    }
    // The files of the data directory hold no value either.
    let mut stack = vec![rig.data.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("a directory").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).expect("a file");
                assert!(!String::from_utf8_lossy(&bytes).contains(CANARY), "{} holds the secret value", path.display());
            }
        }
    }
    // The launch is the one place the value goes besides a run: into the pane's environment, as a wiped string.
    assert_eq!(launch.secrets.len(), 1, "the exec pane gets the value in its environment");

    // After the run the profile API shows that there is a value and nothing more.
    let view: &ProfilesView = &profiles;
    let secret = &view.profiles[0].secret_env[0];
    assert_eq!((secret.name.as_str(), secret.has_value), ("AWS_SECRET_ACCESS_KEY", true));
}

#[test]
fn a_flatpak_build_runs_nothing_and_says_so() {
    let rig = Rig::new();
    let fakes = all_four_fakes(&rig);
    let sentence = "The integrations are not available in the Flatpak build.";
    for with_profile in [false, true] {
        let service = rig.service_over(rig.facts().with_flatpak(true));
        if with_profile {
            service.save_profile(aws_profile()).expect("a profile");
            service.set_secret("aws-prod", "AWS_SECRET_ACCESS_KEY", CANARY).expect("a value");
        }
        let tools = service.tools();
        assert_eq!(tools.len(), 4);
        for tool in &tools {
            assert_eq!(tool.state, ToolState::Unsupported, "{:?}", tool.id);
            assert_eq!(tool.reason.as_deref(), Some(sentence));
            assert!(tool.path.is_none());
        }
        let faults = [
            service.run_view("kube", "pods", None, &args(&[])).expect_err("nothing runs"),
            service.prepare("kube", "delete_pod", None, &args(&[("pod", "api-0")])).expect_err("no card"),
            service.confirm(&"0".repeat(32), &"0".repeat(64)).expect_err("no confirm"),
            service.launch("kube", "logs", None, &args(&[("pod", "api-0")])).expect_err("no launch"),
        ];
        for fault in faults {
            assert_eq!((fault.code, fault.text.as_str()), (FaultCode::Unsupported, sentence), "{fault:?}");
        }
        let _ = service.context_rows(None, true);
    }
    assert!(fakes.iter().all(|fake| !fake.started()), "no process was started");
}

#[test]
fn forgetting_a_pane_removes_its_selection_overlay_and_cache() {
    let rig = Rig::new();
    let fake = logged(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/config_view.json")), 0);
    let service = rig.service();
    service.select("kube", Some(4), &selecting("dev", "payments")).expect("a selection");
    service.launch("kube", "shell", Some(4), &args(&[])).expect("a shell launch");
    let overlay = rig.data.join("run").join("kube").join("4.json");
    assert!(overlay.is_file(), "the shell pane has an overlay");
    service
        .note_pane(9, "kube", "logs", &selecting("dev", "payments"))
        .expect("a pane that a launch opened is noted");
    let rows = service.pane_rows();
    let panes: Vec<u64> = rows.iter().map(|row| row.pane).collect();
    assert_eq!(panes, [4, 9], "both kinds of pane are listed");
    assert_eq!(rows[0].context.as_deref(), Some("dev"));
    assert_eq!(rows[0].namespace.as_deref(), Some("payments"));
    assert!(!rows[0].held && !rows[1].held, "the slot for a held status starts empty");
    service.set_held(9, true);
    assert!(service.pane_rows().iter().any(|row| row.pane == 9 && row.held));

    service.run_view("kube", "pods", Some(6), &args(&[])).expect("a read");
    let before = starts(&fake);
    service.run_view("kube", "pods", Some(6), &args(&[])).expect("served from the cache");
    assert_eq!(starts(&fake), before, "the answer is cached");

    for pane in [4, 6, 9] {
        service.forget_pane(pane);
    }
    assert!(service.pane_rows().is_empty(), "nothing is listed any more");
    assert!(!overlay.exists(), "the overlay of the pane is removed");
    service.run_view("kube", "pods", Some(6), &args(&[])).expect("a new run");
    assert_eq!(starts(&fake), before + 1, "its cached answers are gone");
}

#[test]
fn selecting_changes_only_that_panes_next_argument_list() {
    let rig = Rig::new();
    let fake = FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/pods.json"));
    let service = rig.service();
    service.select("kube", Some(3), &selecting("dev", "payments")).expect("a selection");
    service.select("kube", Some(4), &selecting("kind-test", "test")).expect("a selection");

    service.run_view("kube", "pods", Some(3), &args(&[])).expect("a read");
    assert_eq!(argv_of(&fake)[4..6], ["--context=dev", "--namespace=payments"]);
    service.run_view("kube", "pods", Some(4), &args(&[])).expect("a read");
    assert_eq!(argv_of(&fake)[4..6], ["--context=kind-test", "--namespace=test"]);

    service.select("kube", Some(3), &selecting("kind-test", "ops")).expect("another selection");
    service.run_view("kube", "pods", Some(3), &args(&[])).expect("a read");
    assert_eq!(argv_of(&fake)[4..6], ["--context=kind-test", "--namespace=ops"], "the pane that was changed");
    std::thread::sleep(CACHE_PAUSE);
    let answer = service.run_view("kube", "pods", Some(4), &args(&[])).expect("a read");
    assert_eq!(argv_of(&fake)[4..6], ["--context=kind-test", "--namespace=test"], "the other pane is as it was");
    assert_eq!(answer.selection.selection.namespace.as_deref(), Some("test"));

    let refused = service.select("kube", Some(3), &Selection { node: Some("n".into()), ..Selection::default() });
    assert_eq!(refused.expect_err("a field the tool does not select").code, FaultCode::Refused);
}

#[test]
fn a_set_binary_must_name_the_tool_and_be_usable() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new();
    let on_path = FakeCli::new(&rig.bin, "kubectl");
    let other_dir = rig.root.join("other");
    std::fs::create_dir(&other_dir).expect("a directory");
    let chosen = FakeCli::new(&other_dir, "kubectl").prints(&fixture("kubectl/pods.json"));
    let service = rig.service();

    let view = service.set_binary("kube", chosen.path().to_str().expect("text")).expect("a usable kubectl");
    assert_eq!(view.path.as_deref(), chosen.path().to_str());
    assert_eq!(view.place, Some("override"));
    assert_eq!(view.binary.as_deref(), chosen.path().to_str());
    service.run_view("kube", "pods", None, &args(&[])).expect("a read");
    assert!(chosen.started() && !on_path.started(), "the person chose which kubectl runs");
    let again = rig.service();
    assert_eq!(
        again.tools().iter().find(|tool| tool.id == ToolId::Kube).and_then(|tool| tool.path.clone()).as_deref(),
        chosen.path().to_str(),
        "the choice is kept in the machine-local file"
    );

    let evil = FakeCli::new(&other_dir, "kubectl-evil");
    let plain = other_dir.join("plain");
    std::fs::write(&plain, "x").expect("a file");
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).expect("a mode");
    let quiet = rig.root.join("quiet");
    std::fs::create_dir(&quiet).expect("a directory");
    let not_exec = quiet.join("kubectl");
    std::fs::write(&not_exec, "x").expect("a file");
    std::fs::set_permissions(&not_exec, std::fs::Permissions::from_mode(0o644)).expect("a mode");
    let open = rig.root.join("open");
    std::fs::create_dir(&open).expect("a directory");
    let _inside = FakeCli::new(&open, "kubectl");
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).expect("a mode");
    let refusals: [(&str, String); 7] = [
        ("kube", evil.path().display().to_string()),
        ("kube", "kubectl".to_owned()),
        ("kube", "other/kubectl".to_owned()),
        ("kube", other_dir.join("nothing").join("kubectl").display().to_string()),
        ("kube", not_exec.display().to_string()),
        ("kube", open.join("kubectl").display().to_string()),
        ("talos", chosen.path().display().to_string()),
    ];
    for (tool, path) in &refusals {
        let fault = service.set_binary(tool, path).expect_err(&format!("{tool} {path} is refused"));
        assert_eq!(fault.code, FaultCode::Refused, "{tool} {path}");
    }
    let tools = service.tools();
    let kube = tools.iter().find(|tool| tool.id == ToolId::Kube).expect("an entry");
    assert_eq!(kube.path.as_deref(), chosen.path().to_str(), "a refused path changes nothing");

    std::fs::remove_file(chosen.path()).expect("the program goes away");
    let tools = service.tools();
    let kube = tools.iter().find(|tool| tool.id == ToolId::Kube).expect("an entry");
    assert_eq!(kube.state, ToolState::Unusable, "a set path that stopped working is not silently replaced");
    assert_eq!(kube.reason.as_deref(), Some("kubectl cannot be used: not found."));

    let view = service.clear_binary("kube").expect("cleared");
    assert_eq!(view.state, ToolState::Found);
    assert_eq!(view.path.as_deref(), on_path.path().to_str(), "back to the search");
    assert_eq!(view.binary, None);
}

#[test]
fn profile_operations_pass_through_and_a_saved_profile_drops_the_tools_cache() {
    let rig = Rig::new();
    let fake = logged(FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/pods.json")), 0);
    let service = rig.service();
    service.run_view("kube", "pods", None, &args(&[])).expect("a read");
    service.run_view("kube", "pods", None, &args(&[])).expect("a cached read");
    assert_eq!(starts(&fake), 1);
    service.save_profile(aws_profile()).expect("a profile");
    service.run_view("kube", "pods", None, &args(&[])).expect("a read");
    assert_eq!(starts(&fake), 2, "a saved profile drops the cached views of its tool");

    let mut bad = aws_profile();
    bad.env.insert("LD_PRELOAD".to_owned(), "/tmp/x.so".to_owned());
    let fault = service.save_profile(bad).expect_err("a name the runner denies");
    assert_eq!(fault.code, FaultCode::Profile);
    assert!(!fault.text.contains("/tmp/x.so"), "{}", fault.text);
    let kept = service.profiles();
    assert_eq!(kept.profiles.len(), 1);
    assert!(!kept.profiles[0].env.contains_key("LD_PRELOAD"), "a refused profile changes nothing");

    let unlisted = service.set_secret("aws-prod", "OTHER_NAME", CANARY).expect_err("not a listed name");
    assert_eq!(unlisted.code, FaultCode::Profile);
    assert!(!unlisted.text.contains(CANARY));
    service.set_secret("aws-prod", "AWS_SECRET_ACCESS_KEY", CANARY).expect("a value");
    assert!(service.profiles().profiles[0].secret_env[0].has_value);
    service.clear_secret("aws-prod", "AWS_SECRET_ACCESS_KEY").expect("cleared");
    assert!(!service.profiles().profiles[0].secret_env[0].has_value);

    service.set_secret("aws-prod", "AWS_SECRET_ACCESS_KEY", CANARY).expect("a value");
    let left = service.remove_profile("aws-prod").expect("removed with its secrets");
    assert!(left.profiles.is_empty());
    use holzwave_core::secrets::SecretStore;
    let left = rig.store.get("int:aws-prod:AWS_SECRET_ACCESS_KEY").expect("the store");
    assert_eq!(left, None, "the value left the keychain");
}

#[test]
fn nothing_that_starts_a_process_is_outside_the_runner() {
    let source = |name: &str| {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/integrate").join(name);
        std::fs::read_to_string(path).expect(name)
    };
    let service = source("service.rs");
    let production = service.split("\n#[cfg(test)]\nmod ").next().expect("a production part");
    for forbidden in ["Command::new", ".spawn()", "std::process", "tokio::process"] {
        assert!(!production.contains(forbidden), "the service starts a process itself: {forbidden}");
    }
    let run = source("run.rs");
    let gate = "#[cfg(not(any(target_os = \"ios\", target_os = \"android\")))]\nmod spawn {";
    assert!(run.contains(gate), "the runner's spawn code is behind the mobile gate the service relies on");
}

// ---- found while reading the finished service back ----------------------------------------------------------

#[test]
fn a_later_read_of_the_default_verb_updates_the_default() {
    let rig = Rig::new();
    let original = fixture("kubectl/config_view.json");
    let _kubectl = FakeCli::new(&rig.bin, "kubectl").prints(&original);
    let service = rig.service();
    service.run_view("kube", "contexts", None, &args(&[])).expect("the first read");
    let kube = |service: &IntegrationService| {
        service.context_rows(None, false).into_iter().find(|row| row.tool == ToolId::Kube).expect("a row")
    };
    assert_eq!(kube(&service).context.as_deref(), Some("dev"));

    // The person changes the current context with kubectl itself. The next read of the default verb starts from the
    // default the first read learned, and that is still the tool's own selection, not a choice of the person's.
    let changed = original.replace("\"current-context\": \"dev\"", "\"current-context\": \"kind-test\"");
    assert_ne!(changed, original, "the fixture names its current context the way the test expects");
    let _kubectl = FakeCli::new(&rig.bin, "kubectl").prints(&changed);
    std::thread::sleep(CACHE_PAUSE);
    service.run_view("kube", "contexts", None, &args(&[])).expect("the second read");
    let row = kube(&service);
    assert_eq!(row.context.as_deref(), Some("kind-test"), "the default follows the tool");
    assert_eq!(row.namespace.as_deref(), Some("test"));
    assert!(!row.stale);
}

#[test]
fn a_card_that_was_closed_can_be_cancelled() {
    let rig = Rig::new();
    let fake = counted(FakeCli::new(&rig.bin, "kubectl").prints("deleted\n"));
    let service = rig.service();
    let card = service.prepare("kube", "delete_pod", None, &args(&[("pod", "api-0")])).expect("a card");
    assert!(service.cancel(&card.ticket), "there was a ticket");
    assert!(!service.cancel(&card.ticket), "and now there is none");
    let fault = service.confirm(&card.ticket, &card.hash).expect_err("a cancelled card approves nothing");
    assert_eq!(fault.code, FaultCode::Ticket);
    assert_eq!(runs(&fake), 0);
}

// ---- the panes of the shell: a view for each pane and the writes of the selections -----------------------------

fn with_context(context: &str) -> Selection {
    Selection { context: Some(context.to_owned()), ..Selection::default() }
}

fn pane_row(service: &IntegrationService, pane: u64) -> holzwave_core::integrate::service::PaneRow {
    service.pane_rows().into_iter().find(|row| row.pane == pane).expect("a row for the pane")
}

#[test]
fn a_pane_opens_on_a_view_of_its_tool_with_the_pick_or_nothing() {
    let rig = Rig::new();
    let service = rig.service();

    service.open_pane(5, "kube", "pods").expect("a pane on the pods");
    assert_eq!(pane_row(&service, 5).view, "pods");
    assert_eq!(pane_row(&service, 5).context, None, "nothing was picked, so the pane has nothing of its own");

    // The pick of the tool is what the next pane starts from; a pane that has its own selection keeps it.
    service.select("kube", None, &with_context("dev")).expect("a pick");
    service.open_pane(6, "kube", "pods").expect("a second pane");
    assert_eq!(pane_row(&service, 6).context.as_deref(), Some("dev"));
    service.select("kube", Some(6), &with_context("kind-test")).expect("the pane's own selection");
    service.open_pane(6, "kube", "namespaces").expect("the pane moves to another view");
    let row = pane_row(&service, 6);
    assert_eq!((row.view.as_str(), row.context.as_deref()), ("namespaces", Some("kind-test")));

    // A verb that is not a view of the tool, another tool's view and a word that is none are refused and change nothing.
    for (tool, view) in [("kube", "delete_pod"), ("kube", "exec"), ("kube", "nonesuch"), ("azure", "pods"), ("nonesuch", "pods")] {
        let fault = service.open_pane(7, tool, view).expect_err("refused");
        assert_eq!(fault.code, FaultCode::Refused, "{tool} {view}");
    }
    assert!(service.pane_rows().iter().all(|row| row.pane != 7));
}

#[test]
fn deferred_pane_saves_wait_for_the_host_and_an_immediate_service_still_writes_each_change() {
    let rig = Rig::new();
    let file = rig.data.join("pane_integrations.json");

    // By default every change of a pane's selection is on disk when the call returns.
    let service = rig.service();
    service.open_pane(3, "kube", "pods").expect("a pane");
    assert!(file.exists(), "an immediate service writes at once");
    drop(service);
    std::fs::remove_file(&file).expect("start over");

    let service = rig.service().with_deferred_pane_saves();
    service.open_pane(3, "kube", "pods").expect("a pane");
    service.select("kube", Some(3), &with_context("dev")).expect("a selection");
    service.open_pane(4, "talos", "contexts").expect("another pane");
    assert!(!file.exists(), "a deferred service writes nothing until the host asks");

    service.save_panes().expect("the host saves");
    let again = IntegrationService::new(&rig.data, rig.store.clone(), rig.facts(), &[3, 4]);
    assert_eq!(pane_row(&again, 3).context.as_deref(), Some("dev"), "a restart restores the selections of live panes");
    assert_eq!(pane_row(&again, 4).view, "contexts");

    // A pane that closed is forgotten, and the file follows only when the host saves again.
    service.forget_pane(4);
    let still = IntegrationService::new(&rig.data, rig.store.clone(), rig.facts(), &[3, 4]);
    assert_eq!(pane_row(&still, 4).view, "contexts", "the file still has the pane until the host saves");
    service.save_panes().expect("the host saves again");
    let after = IntegrationService::new(&rig.data, rig.store.clone(), rig.facts(), &[3, 4]);
    assert!(after.pane_rows().iter().all(|row| row.pane != 4));
}
