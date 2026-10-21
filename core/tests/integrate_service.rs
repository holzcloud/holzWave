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
    let facts = rig.facts().with_env("HOLZWAVE_TOKEN", "app-secret").with_env("LD_PRELOAD", "/tmp/x.so").with_env("FOO", "bar");
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
        assert!(path.starts_with(&format!("{}:", rig.bin.display())), "{tool} {verb}: the tool's directory is first: {path}");
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
    let fault = service.run_view("azure", "subscriptions", None, &args(&[])).expect_err("the same exit elsewhere fails");
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
    assert!(!fault.text.contains('\u{1b}') && !fault.text.contains('\n') && !fault.text.contains('\u{7}'), "one clean line");
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
    let _kubectl = FakeCli::new(&rig.bin, "kubectl").prints("{\"token\": \"CANARY-TOKEN-VALUE\", \"not\": \"a config\"}");
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

// ---- the verbs with a card, the launches and the three trait methods in use --------------------------------------------

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
    let want = ["delete", "pod", "--context=dev", "--namespace=payments", "--wait=false", "--request-timeout=10s", "--", "api-0"];
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
        let changes = card.target.iter().find(|(label, _)| label == "changes").unwrap_or_else(|| panic!("{tool} {verb}: {:?}", card.target));
        assert!(changes.1.len() > 20, "{tool} {verb}: a sentence: {}", changes.1);
    }
}

#[test]
fn exec_confirms_into_a_clean_launch_and_starts_nothing() {
    let rig = Rig::new();
    let fake = FakeCli::new(&rig.bin, "kubectl");
    let facts = rig.facts().with_env("HOLZWAVE_TOKEN", "app-secret").with_env("LD_PRELOAD", "/tmp/x.so").with_env("FOO", "bar");
    let service = rig.service_over(facts);
    service.select("kube", Some(3), &selecting("dev", "payments")).expect("a selection");
    let card = service.prepare("kube", "exec", Some(3), &args(&[("pod", "api-0")])).expect("a card");
    assert_eq!(card.guard.production, None);

    let done = service.confirm(&card.ticket, &card.hash).expect("the confirm gives a launch");
    let Done::Launch(launch) = done else { panic!("an exec verb launches, got {done:?}") };
    assert!(!fake.started(), "the confirm started nothing: the pane does");
    assert_eq!(launch.program, fake.path());
    let mut want: Vec<OsString> = ["exec", "-i", "-t", "--context=dev", "--namespace=payments", "api-0", "--", "sh", "-c"]
        .iter()
        .map(OsString::from)
        .collect();
    want.push(OsString::from(POD_SHELL));
    assert_eq!(launch.args, want, "the argument list of the card");
    assert!(launch.env.iter().all(|(name, _)| !name.starts_with("HOLZWAVE_")), "no variable of the app: {:?}", launch.env);
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
    let fakes: Vec<FakeCli> = ["kubectl", "talosctl", "az", "gam"].iter().map(|name| FakeCli::new(&rig.bin, name)).collect();
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
