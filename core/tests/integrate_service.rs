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
