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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use holzwave_core::integrate::context::ContextRow;
use holzwave_core::integrate::kube;
use holzwave_core::integrate::service::{
    Fault, FaultCode, HostFacts, IntegrationService, PaneRow, RunAnswer, SelectionView, ToolView,
};
use holzwave_core::integrate::{Selection, ToolId};
use holzwave_core::layout::PaneId;
use holzwave_core::persist::Debounced;
use holzwave_core::platform::ShellLaunch;
use holzwave_core::secrets::SecretStore;
use serde::Serialize;

use crate::state::{Out, Sink, Target};

/// How long the file of the pane selections waits after the first change of a burst.
const SAVE_DELAY: Duration = Duration::from_millis(250);

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

/// What changed, in the event `integration`. The page asks for the data the word names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationChange {
    /// The tools, or where one is found.
    Tools,
    /// A selection, a pick or what a tool is set to.
    Context,
    /// The profiles or the secrets of a profile.
    Profiles,
    /// A pane holds the last words of its tool on the screen until Enter (`code` is the status), or let go of them.
    Held,
}

/// The event `integration`: small, to every window. It says what changed and for which tool and pane; it carries no
/// data, so a window that missed one asks for everything and is right again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IntegrationEvent {
    /// From 1, up by one with each event of this run.
    pub seq: u64,
    pub kind: IntegrationChange,
    pub tool: Option<ToolId>,
    pub pane: Option<PaneId>,
    /// The status of the tool in a `held` event.
    pub code: Option<u32>,
}

/// One pane that has an integration selection, as `integration_context` answers it: the service's row, and the status
/// of the tool when the pane holds its last words.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaneAnswer {
    pub pane: PaneId,
    pub tool: ToolId,
    /// The view the pane shows, or the verb a launch opened.
    pub view: String,
    pub context: Option<String>,
    pub namespace: Option<String>,
    /// The display name of the production profile, or `null`.
    pub production: Option<String>,
    /// The tool ended and the pane stays until Enter.
    pub held: bool,
    /// The exit status of a held pane's tool, `null` when there is none or the pane is not held.
    pub status: Option<u32>,
}

/// The answer of `integration_context`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ContextAnswer {
    /// One row per tool, in the order the person sees them.
    pub rows: Vec<ContextRow>,
    /// One entry per pane that has an integration selection.
    pub panes: Vec<PaneAnswer>,
}

/// The service and what the shell keeps beside it.
pub struct IntegrationHost {
    service: Arc<IntegrationService>,
    sink: Arc<dyn Sink>,
    seq: AtomicU64,
    /// Writes `pane_integrations.json` 250 ms after the first change of a burst, and when flushed.
    saver: Debounced,
    /// How many times the saver wrote the file, for the tests.
    writes: Arc<AtomicUsize>,
}

impl IntegrationHost {
    /// Builds the service over the data directory `dir`.
    ///
    /// The overlays that shell panes of an earlier run left in the data directory are swept first: a pane that was
    /// closed while the app was not running cannot clean up after itself. The notes about the files that were read
    /// (a file that could not be used, kept as `.bad`) are said once.
    pub fn new(dir: &Path, sink: Arc<dyn Sink>, parts: IntegrationParts, live_panes: &[PaneId]) -> IntegrationHost {
        if let Err(error) = kube::sweep_overlays(&parts.facts.data_dir, live_panes) {
            eprintln!("holzwave: the overlays of closed panes were not all removed: {error}");
        }
        let service = IntegrationService::new(dir, parts.secrets, parts.facts, live_panes).with_deferred_pane_saves();
        for note in service.notes() {
            eprintln!("holzwave: {note}");
        }
        let service = Arc::new(service);
        let writes = Arc::new(AtomicUsize::new(0));
        let saver = {
            let (service, writes) = (service.clone(), writes.clone());
            Debounced::new(SAVE_DELAY, move || match service.save_panes() {
                Ok(()) => {
                    writes.fetch_add(1, Ordering::SeqCst);
                }
                // The kind of the error only: a message can quote a path.
                Err(error) => eprintln!("holzwave: cannot save the selections of the integration panes: {:?}", error.kind()),
            })
        };
        IntegrationHost { service, sink, seq: AtomicU64::new(0), saver, writes }
    }

    /// The service, for the calls that need nothing of the shell.
    pub fn service(&self) -> &IntegrationService {
        &self.service
    }

    /// Writes the file of the pane selections now, if a change is pending.
    pub fn flush(&self) {
        self.saver.flush();
    }

    /// How many times the file of the pane selections was written; the tests count writes with it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn pane_writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }

    /// Tells every window that something changed. The event holds no data.
    pub fn event(&self, kind: IntegrationChange, tool: Option<ToolId>, pane: Option<PaneId>, code: Option<u32>) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.sink.send(Target::All, Out::Integration(IntegrationEvent { seq, kind, tool, pane, code }));
    }

    /// The tool a call named, when it names one (an unknown word gives `None`, and the call is refused on its own).
    fn tool_of(word: &str) -> Option<ToolId> {
        ToolId::parse(word).ok()
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

    /// Selects for a pane, or sets the pick of the tool when `pane` is none. One event follows a selection that was
    /// accepted; a refused one changes nothing and says nothing.
    pub fn select(
        &self,
        tool: &str,
        pane: Option<PaneId>,
        selection: &Selection,
    ) -> Result<SelectionView, IntegrationFault> {
        let answer = self.service.select(tool, pane, selection)?;
        self.saver.touch();
        self.event(IntegrationChange::Context, Self::tool_of(tool), pane, None);
        Ok(answer)
    }

    /// The rows of the tools and the entries of the panes. With `refresh` the tools are looked for again and their
    /// default verbs are read, which starts processes: blocking. `held` says whether a pane holds its tool's last words,
    /// and with which status.
    pub fn context(
        &self,
        pane: Option<PaneId>,
        refresh: bool,
        held: &dyn Fn(PaneId) -> Option<Option<u32>>,
    ) -> ContextAnswer {
        let rows = self.service.context_rows(pane, refresh);
        if refresh {
            // What the tools are set to may have moved: every window asks again.
            self.event(IntegrationChange::Context, None, None, None);
        }
        let panes = self
            .service
            .pane_rows()
            .into_iter()
            .map(|PaneRow { pane, tool, view, context, namespace, production, held: _ }| {
                let status = held(pane);
                PaneAnswer {
                    pane,
                    tool,
                    view,
                    context,
                    namespace,
                    production,
                    held: status.is_some(),
                    status: status.flatten(),
                }
            })
            .collect();
        ContextAnswer { rows, panes }
    }

    /// A pane shows `view` of `tool`: it opened, or it moved to another view. Starts from the pick of the tool, or keeps
    /// the selection the pane has.
    pub fn pane_shows(&self, pane: PaneId, tool: &str, view: &str) -> Result<(), IntegrationFault> {
        self.service.open_pane(pane, tool, view)?;
        self.saver.touch();
        self.event(IntegrationChange::Context, Self::tool_of(tool), Some(pane), None);
        Ok(())
    }

    /// A pane that had a selection or a launch closed: everything the service kept for it goes (its selection, its held
    /// mark, its cached answers and its overlay) and the windows are told. A pane the service knew nothing of says
    /// nothing.
    pub fn pane_closed(&self, pane: PaneId) {
        let known = self.service.pane_rows().into_iter().find(|row| row.pane == pane);
        self.service.forget_pane(pane);
        if let Some(row) = known {
            self.saver.touch();
            self.event(IntegrationChange::Context, Some(row.tool), Some(pane), None);
            if row.held {
                self.event(IntegrationChange::Held, Some(row.tool), Some(pane), None);
            }
        }
    }

    /// The tool of a pane ended and the pane stays open with its last words: marked, and the windows are told with the
    /// status of the tool.
    pub fn pane_held(&self, pane: PaneId, code: Option<u32>) {
        let tool = self.service.pane_rows().into_iter().find(|row| row.pane == pane).map(|row| row.tool);
        self.service.set_held(pane, true);
        self.event(IntegrationChange::Held, tool, Some(pane), code);
    }

    /// The pane ended: it no longer holds anything. The windows hear of it from [`IntegrationHost::pane_closed`].
    pub fn pane_ended(&self, pane: PaneId) {
        self.service.set_held(pane, false);
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

    use holzwave_core::blocks::BlockKind;
    use holzwave_core::integrate::service::ToolState;
    use holzwave_core::integrate::view::ViewModel;
    use holzwave_core::integrate::{Selection, ToolId};
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
    impl Rec {
        /// The `integration` events so far, with who they went to.
        pub(crate) fn integration_events(&self) -> Vec<(Target, IntegrationEvent)> {
            self.log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .filter_map(|(target, out)| match out {
                    Out::Integration(event) => Some((target.clone(), event.clone())),
                    _ => None,
                })
                .collect()
        }
    }

    fn wait_until(timeout: std::time::Duration, mut f: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if f() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn with_context(context: &str) -> Selection {
        Selection { context: Some(context.to_owned()), ..Selection::default() }
    }

    fn pane_of(view: &crate::state::View) -> PaneId {
        view.focus
    }

    #[test]
    fn block_open_of_an_integration_block_opens_a_pane_or_leaves_nothing() {
        let machine = Machine::new("open");
        let (core, _rec) = machine.core();
        let before = core.view_in("main").expect("the main window");

        // A refused block leaves no pane, no tab and no entry behind.
        for (tool, view) in [("docker", "pods"), ("kube", "delete_pod"), ("kube", "exec"), ("kube", "nonesuch"), ("kube", "")] {
            let refused = core.block_open_in("main", "tab", BlockKind::Integration, tool, view);
            assert!(refused.is_err(), "{tool} {view}");
            let now = core.view_in("main").expect("the main window");
            assert_eq!((now.panes.len(), now.tabs.len()), (before.panes.len(), before.tabs.len()), "{tool} {view}");
        }
        assert!(core.blocks_list().is_empty());
        assert!(core.integrations().service().pane_rows().is_empty());

        // A view of a tool opens in a tab, and beside the pane that has the focus.
        let tab = core.block_open_in("main", "tab", BlockKind::Integration, "kube", "pods").expect("a tab");
        let beside = core.block_open_in("main", "right", BlockKind::Integration, "azure", "account").expect("a split");
        let blocks = core.blocks_list();
        assert_eq!(blocks.len(), 2);
        assert_eq!((blocks[0].pane, blocks[0].block.kind), (pane_of(&tab), BlockKind::Integration));
        assert_eq!(blocks[0].source_label, "Kubernetes");
        assert_eq!(blocks[1].source_label, "Azure");
        let after = core.view_in("main").expect("the main window");
        assert_eq!(after.tabs.len(), before.tabs.len() + 1, "one tab for the first");
        assert_eq!(after.panes.len(), 2, "and a split of that tab for the second");

        // The pane has a selection entry: the tool's view, and no selection of its own yet.
        let rows = core.integrations().service().pane_rows();
        assert_eq!(rows.iter().map(|row| (row.pane, row.tool, row.view.as_str())).collect::<Vec<_>>(), [
            (pane_of(&tab), ToolId::Kube, "pods"),
            (pane_of(&beside), ToolId::Azure, "account"),
        ]);
    }

    #[test]
    fn an_integration_pane_that_moves_to_another_view_keeps_its_selection() {
        let machine = Machine::new("move");
        let (core, rec) = machine.core();
        let pane = pane_of(&core.block_open_in("main", "tab", BlockKind::Integration, "kube", "pods").expect("a tab"));
        core.integration_select("kube", Some(pane), &with_context("dev")).expect("a selection");

        core.block_update(pane, "namespaces").expect("another view of the tool");
        let row = core.integrations().service().pane_rows().into_iter().find(|row| row.pane == pane).expect("a row");
        assert_eq!((row.view.as_str(), row.context.as_deref()), ("namespaces", Some("dev")));
        assert!(core.block_update(pane, "delete_pod").is_err(), "a verb that changes something is not a view");
        assert_eq!(core.blocks_list()[0].block.path, "namespaces");
        assert!(rec.integration_events().iter().any(|(_, event)| event.kind == IntegrationChange::Context));
    }

    #[test]
    fn selecting_changes_one_pane_and_sends_one_event() {
        let machine = Machine::new("select");
        let (core, rec) = machine.core();
        let first = pane_of(&core.block_open_in("main", "tab", BlockKind::Integration, "kube", "pods").expect("a tab"));
        let second = pane_of(&core.block_open_in("main", "tab", BlockKind::Integration, "kube", "pods").expect("a tab"));
        let seen = rec.integration_events().len();

        let answer = core.integration_select("kube", Some(first), &with_context("dev")).expect("a selection");
        assert_eq!(answer.selection.context.as_deref(), Some("dev"));
        assert_eq!(answer.production, None);

        let events = rec.integration_events();
        assert_eq!(events.len(), seen + 1, "one event for one change");
        let (target, event) = events.last().expect("an event");
        assert_eq!(*target, Target::All, "every window hears it");
        assert_eq!((event.kind, event.tool, event.pane, event.code), (IntegrationChange::Context, Some(ToolId::Kube), Some(first), None));
        let context = |pane| {
            core.integration_context(None, false)
                .panes
                .into_iter()
                .find(|row| row.pane == pane)
                .expect("a row")
                .context
        };
        assert_eq!(context(first).as_deref(), Some("dev"));
        assert_eq!(context(second), None, "another pane is not touched");

        // Without a pane the call sets the pick of the tool, which the next block starts from.
        core.integration_select("kube", None, &with_context("kind-test")).expect("a pick");
        let third = pane_of(&core.block_open_in("main", "tab", BlockKind::Integration, "kube", "pods").expect("a tab"));
        assert_eq!(context(third).as_deref(), Some("kind-test"));
        assert_eq!(context(first).as_deref(), Some("dev"));

        // A selection the tool refuses is a fault and changes nothing, and nothing is announced.
        let before = rec.integration_events().len();
        let fault = core
            .integration_select("kube", Some(first), &Selection { subscription: Some("x".into()), ..Selection::default() })
            .expect_err("kube has no subscription");
        assert_eq!(fault.code, FaultCode::Refused);
        let fault = core.integration_select("nonesuch", Some(first), &with_context("dev")).expect_err("no such tool");
        assert_eq!(fault.code, FaultCode::Refused);
        assert_eq!(rec.integration_events().len(), before);
        assert_eq!(context(first).as_deref(), Some("dev"));
    }

    #[test]
    fn the_context_answer_has_a_row_for_every_tool_and_a_row_for_every_integration_pane() {
        let machine = Machine::new("context");
        let (core, _rec) = machine.core();
        let pane = pane_of(&core.block_open_in("main", "tab", BlockKind::Integration, "talos", "contexts").expect("a tab"));
        let answer = core.integration_context(None, false);
        assert_eq!(answer.rows.iter().map(|row| row.tool).collect::<Vec<_>>(), ToolId::ALL, "one row per tool, in order");
        assert_eq!(answer.panes.len(), 1);
        let row = &answer.panes[0];
        assert_eq!((row.pane, row.tool, row.view.as_str(), row.held, row.status), (pane, ToolId::Talos, "contexts", false, None));
        let json = serde_json::to_value(row).expect("JSON");
        let mut keys: Vec<&str> = json.as_object().expect("an object").keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["context", "held", "namespace", "pane", "production", "status", "tool", "view"]);
    }

    #[test]
    fn a_burst_of_selections_is_saved_once() {
        let machine = Machine::new("burst");
        let (core, _rec) = machine.core();
        let pane = pane_of(&core.block_open_in("main", "tab", BlockKind::Integration, "kube", "pods").expect("a tab"));
        let host = core.integrations();
        let wait = std::time::Duration::from_secs(5);
        assert!(wait_until(wait, || host.pane_writes() == 1), "the opened pane is saved once");
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert_eq!(host.pane_writes(), 1);

        for n in 0..10 {
            core.integration_select("kube", Some(pane), &with_context(&format!("context-{n}"))).expect("a selection");
        }
        assert!(wait_until(wait, || host.pane_writes() == 2), "the burst is written");
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(host.pane_writes(), 2, "ten changes in a burst are one write");
        let text = std::fs::read_to_string(machine.data.join("pane_integrations.json")).expect("the file");
        assert!(text.contains("context-9") && !text.contains("context-8"), "the last change is what was written: {text}");

        // A restart restores the selection of a live pane.
        core.flush();
        drop(core);
        let (again, _rec) = machine.core();
        let row = again.integration_context(None, false).panes;
        assert_eq!(row.len(), 1);
        assert_eq!((row[0].pane, row[0].context.as_deref()), (pane, Some("context-9")));
    }

    #[test]
    fn a_closed_pane_forgets_its_selection_and_overlay() {
        let machine = Machine::new("close");
        let (core, rec) = machine.core();
        let pane = pane_of(&core.block_open_in("main", "tab", BlockKind::Integration, "kube", "pods").expect("a tab"));
        core.integration_select("kube", Some(pane), &with_context("dev")).expect("a selection");
        let overlay = kube::overlay_path(&machine.data, pane);
        std::fs::create_dir_all(overlay.parent().expect("a directory")).expect("the overlay directory");
        std::fs::write(&overlay, "{}").expect("an overlay");
        let seen = rec.integration_events().len();

        core.close_pane_in("main", pane).expect("the pane closes");
        assert!(core.integration_context(None, false).panes.is_empty(), "no selection is left");
        assert!(core.blocks_list().is_empty(), "and no block");
        assert!(!overlay.exists(), "the overlay of the pane is removed");
        let events = rec.integration_events();
        assert!(
            events[seen..].iter().any(|(_, e)| (e.kind, e.pane) == (IntegrationChange::Context, Some(pane))),
            "the page is told: {events:?}"
        );
        core.flush();
        let text = std::fs::read_to_string(machine.data.join("pane_integrations.json")).unwrap_or_default();
        assert!(!text.contains("dev"), "and the file no longer has it: {text}");
    }

    #[cfg(unix)]
    #[test]
    fn a_held_pane_sends_a_held_event() {
        let machine = Machine::new("held");
        let (core, rec) = machine.core();
        let pane = pane_of(&core.block_open_in("main", "tab", BlockKind::Integration, "kube", "pods").expect("a tab"));
        core.start_pane(pane, 80, 24, 0, 0).expect("the pane starts");
        let seen = rec.integration_events().len();

        core.record_held(pane, Some(1));
        let events = rec.integration_events();
        let (target, event) = &events[seen];
        assert_eq!(*target, Target::All);
        assert_eq!((event.kind, event.pane, event.code, event.tool), (IntegrationChange::Held, Some(pane), Some(1), Some(ToolId::Kube)));
        let row = core.integration_context(None, false).panes.remove(0);
        assert_eq!((row.held, row.status), (true, Some(1)), "the status of the tool is in the answer");

        // Held without a status is still held.
        core.record_held(pane, None);
        let row = core.integration_context(None, false).panes.remove(0);
        assert_eq!((row.held, row.status), (true, None));

        // The end of the pane clears it.
        core.close_pane_in("main", pane).expect("the pane closes");
        assert_eq!(core.pane_held(pane), None);
        assert!(core.integration_context(None, false).panes.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn closing_a_plain_terminal_pane_writes_and_says_nothing_for_the_integrations() {
        let machine = Machine::new("plain");
        let (core, rec) = machine.core();
        let view = core.split_in("main", false).expect("a split");
        core.close_pane_in("main", view.focus).expect("the pane closes");
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(core.integrations().pane_writes(), 0);
        assert!(!machine.data.join("pane_integrations.json").exists());
        assert!(rec.integration_events().is_empty());
    }

    #[test]
    fn the_integration_event_has_five_members_and_absent_ones_are_null() {
        let event = IntegrationEvent { seq: 4, kind: IntegrationChange::Tools, tool: None, pane: None, code: None };
        let json = serde_json::to_value(Out::Integration(event)).expect("JSON");
        assert_eq!(json, serde_json::json!({"seq": 4, "kind": "tools", "tool": null, "pane": null, "code": null}));
        assert_eq!(Out::Integration(IntegrationEvent { seq: 1, kind: IntegrationChange::Profiles, tool: Some(ToolId::Gam), pane: Some(3), code: None }).name(), "integration");
        for (kind, word) in [
            (IntegrationChange::Tools, "tools"),
            (IntegrationChange::Context, "context"),
            (IntegrationChange::Profiles, "profiles"),
            (IntegrationChange::Held, "held"),
        ] {
            assert_eq!(serde_json::to_value(kind).expect("JSON"), word);
        }
    }

    #[test]
    fn the_sequence_of_the_events_goes_up_by_one() {
        let machine = Machine::new("seq");
        let (core, rec) = machine.core();
        let pane = pane_of(&core.block_open_in("main", "tab", BlockKind::Integration, "kube", "pods").expect("a tab"));
        for n in 0..3 {
            core.integration_select("kube", Some(pane), &with_context(&format!("c{n}"))).expect("a selection");
        }
        let seqs: Vec<u64> = rec.integration_events().iter().map(|(_, e)| e.seq).collect();
        assert!(seqs.len() >= 3);
        assert_eq!(seqs[0], 1, "the first event is number one");
        assert!(seqs.windows(2).all(|pair| pair[1] == pair[0] + 1), "{seqs:?}");
    }
}
