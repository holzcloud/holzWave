//! What the app shell adds to the integration service: the machine it runs on, the fault the webview reads, and the
//! small event that tells the page to ask again.
//!
//! The service in the core owns every rule: which tool and verb may run, what a card shows, what a ticket allows. This
//! file has no policy of its own. It builds the service from the real machine (the process environment, the file
//! system, the person's shell and the keychain that the saved connections use), turns a refusal into the object
//! `{code, reason, text}` that the page reads, and sends the event `integration` when something the page shows has
//! changed.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use holzwave_core::integrate::kube;
use holzwave_core::integrate::service::{Fault, FaultCode, HostFacts, IntegrationService, RunAnswer, ToolView};
use holzwave_core::layout::PaneId;
use holzwave_core::platform::ShellLaunch;
use holzwave_core::secrets::SecretStore;
use serde::Serialize;

/// What the service is built from: the machine, and the keychain that the values of the profiles' secret variables go
/// to. The tests put a scratch machine and a store in memory here.
pub struct IntegrationParts {
    facts: HostFacts,
    secrets: Arc<dyn SecretStore>,
}

impl IntegrationParts {
    /// The real machine and the platform keychain. `shell` is the person's shell as the app resolved it for the
    /// settings, for a pane that is the shell itself.
    pub fn system(dir: &Path, shell: ShellLaunch) -> IntegrationParts {
        IntegrationParts {
            facts: HostFacts::system(dir, shell),
            secrets: Arc::new(holzwave_core::secrets::KeychainStore),
        }
    }

    /// A machine and a store of the caller's making.
    #[cfg(test)]
    pub fn new(facts: HostFacts, secrets: Arc<dyn SecretStore>) -> IntegrationParts {
        IntegrationParts { facts, secrets }
    }
}

/// Why a command was refused, as the page receives it: `{code, reason, text}`. The code is one of `refused`, `missing`,
/// `failed`, `timeout`, `overflow`, `busy`, `ticket`, `profile`, `unsupported` and `parse`, the reason is a fixed word
/// inside the code, and the text is one sentence. The service builds all three: a sentence holds nothing of the
/// request, and for a tool that reported an error at most one cleaned line of the tool's own message.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct IntegrationFault {
    pub code: FaultCode,
    pub reason: &'static str,
    pub text: String,
}

impl From<Fault> for IntegrationFault {
    fn from(fault: Fault) -> Self {
        IntegrationFault { code: fault.code, reason: fault.reason, text: fault.text }
    }
}

/// The service and what the shell keeps beside it.
pub struct IntegrationHost {
    service: Arc<IntegrationService>,
}

impl IntegrationHost {
    /// Builds the service over the data directory `dir`.
    ///
    /// The overlays that shell panes of an earlier run left in the data directory are swept first: a pane that was
    /// closed while the app was not running cannot clean up after itself. The notes about the files that were read
    /// (a file that could not be used, kept as `.bad`) are said once.
    pub fn new(dir: &Path, parts: IntegrationParts, live_panes: &[PaneId]) -> IntegrationHost {
        if let Err(error) = kube::sweep_overlays(&parts.facts.data_dir, live_panes) {
            eprintln!("holzwave: the overlays of closed panes were not all removed: {error}");
        }
        let service = IntegrationService::new(dir, parts.secrets, parts.facts, live_panes);
        for note in service.notes() {
            eprintln!("holzwave: {note}");
        }
        IntegrationHost { service: Arc::new(service) }
    }

    /// The service, for the calls that need nothing of the shell.
    pub fn service(&self) -> &IntegrationService {
        &self.service
    }

    /// The four tools and where each stands on this computer. Blocking: it looks for the programs and asks a found one
    /// for its version, so it is called off the UI thread.
    pub fn tools(&self) -> Vec<ToolView> {
        self.service.tools()
    }

    /// One read of a verb that shows a view. Blocking.
    pub fn run(
        &self,
        tool: &str,
        verb: &str,
        pane: Option<PaneId>,
        args: &BTreeMap<String, String>,
    ) -> Result<RunAnswer, IntegrationFault> {
        self.service.run_view(tool, verb, pane, args).map_err(IntegrationFault::from)
    }
}

/// The fake program rig of the core's integration tests: a script that stands in for a command-line tool.
#[cfg(all(test, unix))]
#[path = "../../../core/tests/common/fake_cli.rs"]
mod fake_cli;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use holzwave_core::integrate::service::ToolState;
    use holzwave_core::integrate::view::ViewModel;
    use holzwave_core::integrate::ToolId;
    use holzwave_core::secrets::MemoryStore;

    use crate::ai_host::AiParts;
    use crate::state::{AppCore, Out, ShellSpec, Sink, Target};
    use crate::windows::{CursorSpot, Place};

    #[cfg(unix)]
    use super::fake_cli;

    /// Collects what the core tells the webviews.
    #[derive(Default)]
    pub(crate) struct Rec {
        log: Mutex<Vec<(Target, Out)>>,
    }

    impl Sink for Rec {
        fn send(&self, target: Target, out: Out) {
            self.log.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((target, out));
        }

        fn close_window(&self, _label: &str) {}

        fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
            Ok(())
        }

        fn cursor_spot(&self, _source: &str) -> CursorSpot {
            CursorSpot::Unknown
        }
    }

    /// A scratch computer: `bin` is on the `PATH`, `home` and `data` are empty directories. Removed when dropped.
    pub(crate) struct Machine {
        pub root: PathBuf,
        pub bin: PathBuf,
        pub home: PathBuf,
        pub data: PathBuf,
        pub store: Arc<MemoryStore>,
    }

    impl Machine {
        pub fn new(tag: &str) -> Machine {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let root = std::env::temp_dir().join(format!("holzwave-host-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let (bin, home, data) = (root.join("bin"), root.join("home"), root.join("data"));
            for dir in [&root, &bin, &home, &data] {
                std::fs::create_dir_all(dir).expect("a directory of the scratch machine");
            }
            Machine { root, bin, home, data, store: Arc::new(MemoryStore::default()) }
        }

        pub fn parts(&self) -> IntegrationParts {
            let facts = HostFacts::for_tests(&self.root, std::slice::from_ref(&self.bin), &self.home, &self.data);
            IntegrationParts::new(facts, self.store.clone())
        }

        pub fn core(&self) -> (Arc<AppCore>, Arc<Rec>) {
            let rec = Arc::new(Rec::default());
            let core = AppCore::new_with_parts(
                self.data.clone(),
                rec.clone(),
                ShellSpec::new("/bin/sh", &["-c", "cat"]),
                AiParts::system(),
                Some(self.parts()),
            );
            (core, rec)
        }
    }

    impl Drop for Machine {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// A recorded fixture of the tools, as text.
    pub(crate) fn fixture(relative: &str) -> String {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../core/tests/fixtures/integrations")
            .join(relative);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {relative}: {e}"))
    }

    pub(crate) fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[cfg(unix)]
    #[test]
    fn integration_tools_and_run_reach_the_service_through_the_app_core() {
        let machine = Machine::new("tools");
        let fake = fake_cli::FakeCli::new(&machine.bin, "kubectl").prints(&fixture("kubectl/config_view.json"));
        let (core, _rec) = machine.core();

        let tools = core.integration_tools();
        let kube = tools.iter().find(|tool| tool.id == ToolId::Kube).expect("a Kubernetes entry");
        assert_eq!(kube.state, ToolState::Found, "the fake kubectl on the scratch PATH is found");
        assert_eq!(kube.path.as_deref(), fake.path().to_str());
        assert_eq!(tools.len(), 4, "one entry for each of the four tools");

        let answer = core.integration_run("kube", "contexts", None, &args(&[])).expect("the read gives a model");
        let ViewModel::Table(table) = &answer.view else { panic!("contexts is a table, got {:?}", answer.view) };
        assert_eq!(table.rows().len(), 3, "the three contexts of the recorded kubeconfig");
    }

    #[test]
    fn the_overlay_of_a_pane_that_closed_while_the_app_was_off_is_swept_at_start() {
        let machine = Machine::new("sweep");
        // A first run leaves a layout with one pane behind.
        let open = {
            let (core, _rec) = machine.core();
            let view = core.split_in("main", false).expect("a split");
            core.flush();
            view.focus
        };
        let stale = kube::overlay_path(&machine.data, 4711);
        let live = kube::overlay_path(&machine.data, open);
        std::fs::create_dir_all(stale.parent().expect("the overlay directory")).expect("the overlay directory");
        std::fs::write(&stale, "{}").expect("an overlay of a pane that closed");
        std::fs::write(&live, "{}").expect("the overlay of a pane that is still open");

        let (_core, _rec) = machine.core();
        assert!(!stale.exists(), "the overlay of pane 4711 is gone");
        assert!(live.exists(), "the overlay of an open pane stays");
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_request_is_a_fault_object() {
        let machine = Machine::new("fault");
        let fake = fake_cli::FakeCli::new(&machine.bin, "kubectl").prints("never printed");
        let (core, _rec) = machine.core();

        for (tool, verb) in [("nonesuch", "contexts"), ("kube", "nonesuch"), ("kube", "delete_pod")] {
            let fault = core.integration_run(tool, verb, None, &args(&[])).expect_err("a refused request");
            assert_eq!(fault.code, FaultCode::Refused, "{tool} {verb}");
            let json = serde_json::to_value(&fault).expect("a fault is JSON");
            let mut keys: Vec<&str> = json.as_object().expect("an object").keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(keys, ["code", "reason", "text"], "the fault object has exactly these members");
            assert_eq!(json["code"], "refused");
            assert!(!fault.reason.is_empty() && fault.text.ends_with('.'), "a fixed word and a sentence: {fault:?}");
        }
        assert!(!fake.started(), "a refused request starts nothing");
    }
}
