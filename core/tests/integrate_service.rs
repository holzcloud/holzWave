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
