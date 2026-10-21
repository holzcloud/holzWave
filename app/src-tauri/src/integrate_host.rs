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
use holzwave_core::integrate::profiles::IntegrationProfile;
use holzwave_core::integrate::service::{
    Done, Fault, FaultCode, HostFacts, IntegrationService, PaneRow, ProfilesView, RunAnswer, SelectionView, ToolView,
};
use holzwave_core::integrate::ticket::Card;
use holzwave_core::integrate::{Selection, ToolId};
use holzwave_core::layout::PaneId;
use holzwave_core::persist::Debounced;
use holzwave_core::platform::ShellLaunch;
use holzwave_core::secrets::SecretStore;
use serde::{Deserialize, Serialize};

use crate::state::{Out, Sink, Target, View};

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

impl IntegrationFault {
    /// A refusal of the shell itself (a window that is gone, a layout that has no room): a fixed word and sentence.
    pub fn refused(reason: &'static str, text: &str) -> Self {
        IntegrationFault { code: FaultCode::Refused, reason, text: text.to_owned() }
    }

    /// The pane of a launch could not be made ready. The sentences of the shell hold nothing of the request.
    pub(crate) fn launch(text: &str) -> Self {
        let mut chars = text.chars();
        let text = chars.next().map_or_else(String::new, |first| format!("{}{}.", first.to_uppercase(), chars.as_str()));
        IntegrationFault { code: FaultCode::Failed, reason: "launch", text }
    }
}

/// The fault for a profile the page sent that is not a profile. The sentence of the core's own conversion names the profile
/// and the field and never a value; any other error of the parser may quote the text it was given, so it is replaced by a
/// fixed sentence.
fn profile_shape_fault(error: &serde_json::Error) -> IntegrationFault {
    let note = error.to_string();
    let first = note.split(" at line ").next().unwrap_or("");
    let own = first.starts_with("profile ") && !first.contains(['"', '`', '\n']);
    let text = if own {
        let mut chars = first.chars();
        chars.next().map_or_else(String::new, |c| format!("{}{}.", c.to_uppercase(), chars.as_str()))
    } else {
        "The profile is not valid.".to_owned()
    };
    IntegrationFault { code: FaultCode::Profile, reason: "invalid", text }
}

/// Overwrites a secret the page sent as soon as it has been stored. Best effort: the message that carried it into the
/// process is outside this function, but the copy that this process owns does not outlive the call.
fn wipe(value: String) {
    let mut bytes = value.into_bytes();
    bytes.fill(0);
    std::hint::black_box(&bytes);
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

/// What the answer of `integration_confirm` is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmKind {
    /// The verb ran once.
    Ran,
    /// The verb opens a session: a pane was made for it, and the page starts it.
    Launched,
}

/// The answer of `integration_confirm`, the one command that decides. `note` is the tool's own first line of output for a
/// verb that ran (cleaned and cut by the core), or the name of the pane for a launch; `pane` and `view` are the new pane
/// and the layout it is in, for a launch, and `null` otherwise.
#[derive(Clone, Debug, Serialize)]
pub struct ConfirmAnswer {
    pub kind: ConfirmKind,
    pub pane: Option<PaneId>,
    pub note: String,
    pub view: Option<View>,
}

/// The answer of `integration_launch`: the new pane, and the layout it is in (as `block_open` answers).
#[derive(Clone, Debug, Serialize)]
pub struct LaunchAnswer {
    pub pane: PaneId,
    pub view: View,
}

/// What a launch was made for, so that the pane it opens has a selection to show while it lives.
pub struct LaunchNote<'a> {
    pub tool: &'a str,
    pub verb: &'a str,
    /// The pane whose selection the request used.
    pub from: Option<PaneId>,
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

    /// The card for a verb that changes something or opens a session. Nothing is started: the card is described, shown and
    /// waits for the person. It may look for the program (and ask the login shell for its `PATH` once), so it is blocking.
    pub fn prepare(
        &self,
        tool: &str,
        verb: &str,
        pane: Option<PaneId>,
        args: &BTreeMap<String, String>,
    ) -> Result<Card, IntegrationFault> {
        self.service.prepare(tool, verb, pane, args).map_err(IntegrationFault::from)
    }

    /// The person closed a card. Answers nothing: a ticket that is not there is not a problem.
    pub fn cancel(&self, ticket: &str) {
        self.service.cancel(ticket);
    }

    /// The answer of the person to a card. A verb that changes a file, a cluster or an account runs once (blocking), and
    /// the windows are told that what the tool is set to may have moved; an exec verb comes back as a launch that the
    /// caller makes a pane of.
    pub fn confirm(&self, ticket: &str, hash: &str) -> Result<(Done, holzwave_core::integrate::service::Origin), IntegrationFault> {
        let (done, origin) = self.service.confirm_from(ticket, hash)?;
        if matches!(done, Done::Ran { .. }) {
            self.event(IntegrationChange::Context, Some(origin.tool), origin.pane, None);
        }
        Ok((done, origin))
    }

    /// A terminal for a verb that only reads and opens one. Blocking: it may read the default verb once for the overlay of
    /// a shell.
    pub fn launch(
        &self,
        tool: &str,
        verb: &str,
        pane: Option<PaneId>,
        args: &BTreeMap<String, String>,
    ) -> Result<holzwave_core::integrate::run::Launch, IntegrationFault> {
        self.service.launch(tool, verb, pane, args).map_err(IntegrationFault::from)
    }

    /// The pane `pane` was opened for a launch: it has a selection to show while it lives, and the windows are told.
    pub fn pane_launched(&self, pane: PaneId, note: &LaunchNote<'_>) -> Result<(), IntegrationFault> {
        self.service.note_launch(pane, note.tool, note.verb, note.from)?;
        self.event(IntegrationChange::Context, Self::tool_of(note.tool), Some(pane), None);
        Ok(())
    }

    /// Makes `path` the program of `tool`: the person chooses which `kubectl` runs and not which program, so the file
    /// name must be one of the tool's own names and the detector must find the file usable. Answers the four tools again.
    /// Blocking: the tool is asked for its version.
    pub fn binary_set(&self, tool: &str, path: &str) -> Result<Vec<ToolView>, IntegrationFault> {
        self.service.set_binary(tool, path)?;
        let tools = self.service.tools();
        self.event(IntegrationChange::Tools, Self::tool_of(tool), None, None);
        Ok(tools)
    }

    /// Forgets the program the person chose for `tool`; the search applies again. Answers the four tools again.
    pub fn binary_clear(&self, tool: &str) -> Result<Vec<ToolView>, IntegrationFault> {
        self.service.clear_binary(tool)?;
        let tools = self.service.tools();
        self.event(IntegrationChange::Tools, Self::tool_of(tool), None, None);
        Ok(tools)
    }

    /// The profiles, and for every secret variable only whether the keychain has a value. It asks the keychain, so it is
    /// blocking.
    pub fn profiles(&self) -> ProfilesView {
        self.service.profiles()
    }

    /// Adds a profile or replaces the one with the same id. `profile` is the shape of the profiles file: `secret_env` is a
    /// list of names and `env` an object of plain values. The rules of the loader decide; a refusal names the profile and
    /// the field and never a value. Answers the profiles again.
    pub fn profile_save(&self, profile: serde_json::Value) -> Result<ProfilesView, IntegrationFault> {
        let profile = IntegrationProfile::deserialize(profile).map_err(|error| profile_shape_fault(&error))?;
        let tool = profile.tool;
        let view = self.service.save_profile(profile)?;
        self.event(IntegrationChange::Profiles, Some(tool), None, None);
        Ok(view)
    }

    /// Removes a profile and the values of its secret variables from the keychain. Answers the profiles again.
    pub fn profile_remove(&self, id: &str) -> Result<ProfilesView, IntegrationFault> {
        let view = self.service.remove_profile(id)?;
        self.event(IntegrationChange::Profiles, None, None, None);
        Ok(view)
    }

    /// Stores the value of one secret variable of a profile in the keychain. Write-only: no answer, event or note ever
    /// holds the value, and the copy this process owns is overwritten when the call is over.
    pub fn secret_set(&self, profile: &str, name: &str, value: String) -> Result<(), IntegrationFault> {
        let stored = self.service.set_secret(profile, name, &value);
        wipe(value);
        stored?;
        self.event(IntegrationChange::Profiles, None, None, None);
        Ok(())
    }

    /// Removes the value of one secret variable of a profile from the keychain.
    pub fn secret_clear(&self, profile: &str, name: &str) -> Result<(), IntegrationFault> {
        self.service.clear_secret(profile, name)?;
        self.event(IntegrationChange::Profiles, None, None, None);
        Ok(())
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
    use holzwave_core::secrets::{MemoryStore, SecretStore};

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

        pub fn facts(&self) -> HostFacts {
            HostFacts::for_tests(&self.root, std::slice::from_ref(&self.bin), &self.home, &self.data)
        }

        pub fn core(&self) -> (Arc<AppCore>, Arc<Rec>) {
            self.core_over(self.facts())
        }

        pub fn core_over(&self, facts: HostFacts) -> (Arc<AppCore>, Arc<Rec>) {
            let rec = Arc::new(Rec::default());
            let core = AppCore::new_with_parts(
                self.data.clone(),
                rec.clone(),
                ShellSpec::new("/bin/sh", &["-c", "cat"]),
                AiParts::system(),
                Some(IntegrationParts::new(facts, self.store.clone())),
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
        assert!(core.integration_context(None, false).panes.is_empty());
        // The shell reports its end a moment later, and that clears the mark of the core as well.
        assert!(wait_until(std::time::Duration::from_secs(5), || core.pane_held(pane).is_none()));
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

    /// A fake that also counts how often it was started.
    #[cfg(unix)]
    fn counted(fake: fake_cli::FakeCli) -> fake_cli::FakeCli {
        use std::io::Write;
        let path = format!("{}.conf", fake.path().display());
        let mut conf = std::fs::OpenOptions::new().append(true).open(path).expect("the settings of the fake");
        writeln!(conf, "echo run >> \"$d/$n.runs\"").expect("one more line");
        fake
    }

    #[cfg(unix)]
    fn runs(fake: &fake_cli::FakeCli) -> usize {
        std::fs::read_to_string(format!("{}.runs", fake.path().display())).map_or(0, |text| text.lines().count())
    }

    #[cfg(unix)]
    #[test]
    fn a_card_is_prepared_and_confirmed_into_one_run() {
        let machine = Machine::new("card");
        let fake = counted(fake_cli::FakeCli::new(&machine.bin, "kubectl").prints("pod \"api-0\" deleted\n"));
        let (core, rec) = machine.core();
        let pod = args(&[("pod", "api-0")]);

        let card = core.integration_prepare("kube", "delete_pod", Some(3), &pod).expect("a card");
        assert_eq!((card.tool, card.verb.as_str()), (ToolId::Kube, "delete_pod"));
        assert_eq!(card.hash.len(), 64);
        assert_eq!(runs(&fake), 0, "preparing a card starts nothing");
        let json = serde_json::to_value(&card).expect("JSON");
        let mut keys: Vec<&str> = json.as_object().expect("an object").keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["argv", "effect", "expires_in_secs", "fields", "guard", "hash", "program", "target", "ticket", "tool", "verb"]);

        // A verb that only reads needs no card.
        let fault = core.integration_prepare("kube", "pods", None, &args(&[])).expect_err("a read");
        assert_eq!(fault.code, FaultCode::Refused);

        // The wrong hash runs nothing; the right one runs the verb once; a replay runs nothing.
        let wrong = core.integration_confirm("main", &card.ticket, &"0".repeat(64)).expect_err("a wrong hash");
        assert_eq!(wrong.code, FaultCode::Ticket);
        assert_eq!(runs(&fake), 0);
        let seen = rec.integration_events().len();
        let done = core.integration_confirm("main", &card.ticket, &card.hash).expect("confirmed");
        assert_eq!((done.kind, done.pane), (ConfirmKind::Ran, None));
        assert!(done.view.is_none());
        assert_eq!(done.note, "pod \"api-0\" deleted", "the tool's own first line");
        assert_eq!(runs(&fake), 1);
        assert_eq!(argv(&fake), card.argv, "exactly the argument list of the card");
        let replay = core.integration_confirm("main", &card.ticket, &card.hash).expect_err("a replay");
        assert_eq!(replay.code, FaultCode::Ticket);
        assert_eq!(runs(&fake), 1, "a ticket is good once");
        assert!(
            rec.integration_events()[seen..].iter().any(|(_, e)| (e.kind, e.tool) == (IntegrationChange::Context, Some(ToolId::Kube))),
            "what the tool is set to may have moved, and the windows are told"
        );

        // A selection that changed between the card and the press makes the request another one.
        core.integration_select("kube", Some(3), &with_context("dev")).expect("a selection");
        let card = core.integration_prepare("kube", "delete_pod", Some(3), &pod).expect("a card for dev");
        core.integration_select("kube", Some(3), &with_context("prod")).expect("another selection");
        let changed = core.integration_confirm("main", &card.ticket, &card.hash).expect_err("the request is another one");
        assert_eq!(changed.code, FaultCode::Ticket);
        assert_eq!(runs(&fake), 1, "and nothing ran");
    }

    /// The fake has started and recorded everything it records (the process id is the last of the records).
    #[cfg(unix)]
    fn ran(fake: &fake_cli::FakeCli) -> bool {
        fake.parent_pid().is_some()
    }

    #[cfg(unix)]
    fn argv(fake: &fake_cli::FakeCli) -> Vec<String> {
        fake.argv().into_iter().map(|arg| String::from_utf8(arg).expect("text arguments")).collect()
    }

    #[cfg(unix)]
    #[test]
    fn cancel_forgets_the_ticket() {
        let machine = Machine::new("cancel");
        let fake = counted(fake_cli::FakeCli::new(&machine.bin, "kubectl").prints("deleted\n"));
        let (core, _rec) = machine.core();
        let card = core.integration_prepare("kube", "delete_pod", None, &args(&[("pod", "api-0")])).expect("a card");
        core.integration_cancel(&card.ticket);
        core.integration_cancel(&card.ticket);
        core.integration_cancel("no-such-ticket");
        let fault = core.integration_confirm("main", &card.ticket, &card.hash).expect_err("a cancelled card approves nothing");
        assert_eq!(fault.code, FaultCode::Ticket);
        assert_eq!(runs(&fake), 0);
    }

    #[cfg(unix)]
    #[test]
    fn an_exec_confirm_opens_a_real_pane_with_a_clean_launch() {
        let machine = Machine::new("exec");
        let fake = fake_cli::FakeCli::new(&machine.bin, "kubectl");
        let mut facts = machine.facts().with_env("HOLZWAVE_TOKEN", "app-secret").with_env("FOO", "bar");
        // The pane inherits the environment of this very process, so the facts name it too, as the real machine's do: the
        // names a launch removes are those of the environment the app started with.
        let own = std::mem::take(&mut facts.inherited);
        facts.inherited = std::env::vars_os().filter(|(name, _)| name != "PATH" && name != "HOME").chain(own).collect();
        let (core, rec) = machine.core_over(facts);
        // A plain pane would get the socket and the token of the app; a launch must not.
        core.set_ipc(machine.root.clone(), "the-token-of-this-run".to_owned());
        core.integration_select("kube", Some(3), &with_context("dev")).expect("a selection of the pane the person is in");

        let card = core.integration_prepare("kube", "exec", Some(3), &args(&[("pod", "api-0")])).expect("a card");
        assert_eq!(card.effect, holzwave_core::integrate::VerbEffect::Exec);
        let before = core.view_in("main").expect("the main window");
        let answer = core.integration_confirm("main", &card.ticket, &card.hash).expect("confirmed");
        assert_eq!(answer.kind, ConfirmKind::Launched);
        assert_eq!(answer.note, "Exec in pod api-0", "the name of the pane");
        let (pane, view) = (answer.pane.expect("the new pane"), answer.view.expect("the new layout"));
        assert_eq!(view.focus, pane, "the new pane has the focus");
        assert_eq!(view.tabs.len(), before.tabs.len() + 1, "a tab, as the card was prepared without a placement");
        assert!(!fake.started(), "the command itself starts nothing");

        // The pane has a selection to show while it lives: that of the pane the person acted in.
        let rows = core.integration_context(None, false).panes;
        let row = rows.iter().find(|row| row.pane == pane).expect("the launched pane has a row");
        assert_eq!((row.tool, row.view.as_str(), row.context.as_deref()), (ToolId::Kube, "exec", Some("dev")));

        // The page starts the pane: it runs the tool with the argument list of the card, in a clean environment.
        core.start_pane(pane, 80, 24, 0, 0).expect("the pane starts");
        assert!(wait_until(std::time::Duration::from_secs(20), || ran(&fake)), "the tool ran in the pane");
        assert_eq!(argv(&fake), card.argv, "exactly the argument list of the card");
        assert_eq!(&card.argv[..5], ["exec", "-i", "-t", "--context=dev", "api-0"]);
        let env = fake.env();
        assert!(env.keys().all(|name| !name.starts_with("HOLZWAVE_")), "no variable of the app: {:?}", env.keys());
        assert!(!String::from_utf8_lossy(&fake.env_bytes()).contains("app-secret"));
        assert!(!String::from_utf8_lossy(&fake.env_bytes()).contains("the-token-of-this-run"));
        assert!(wait_until(std::time::Duration::from_secs(20), || core.pane_held(pane).is_some()), "a tool that ends at once is held");
        assert!(rec.integration_events().iter().any(|(_, e)| e.kind == IntegrationChange::Held && e.pane == Some(pane)));
    }

    #[cfg(unix)]
    #[test]
    fn a_read_terminal_launches_without_a_ticket_and_exec_does_not() {
        let machine = Machine::new("launch");
        let kubectl = fake_cli::FakeCli::new(&machine.bin, "kubectl");
        let (core, _rec) = machine.core();
        let before = core.view_in("main").expect("the main window");

        let beside = core.integration_launch("main", "kube", "logs", None, &args(&[("pod", "api-0")]), "right").expect("logs");
        assert_eq!(beside.view.focus, beside.pane);
        assert_eq!(beside.view.panes.len(), before.panes.len() + 1);
        assert_eq!(beside.view.tabs.len(), before.tabs.len(), "beside the focused pane, in the same tab");
        let in_tab = core.integration_launch("main", "kube", "logs", None, &args(&[("pod", "api-1")]), "tab").expect("logs in a tab");
        assert_eq!(in_tab.view.tabs.len(), before.tabs.len() + 1);
        assert!(!kubectl.started(), "a launch registers and starts nothing");

        // A verb that changes something or opens a session needs a card, and no pane is left behind.
        let panes_now = core.view_in("main").expect("the main window").panes.len();
        for (tool, verb, request) in [
            ("kube", "exec", args(&[("pod", "api-0")])),
            ("kube", "delete_pod", args(&[("pod", "api-0")])),
            ("azure", "login", args(&[])),
            ("kube", "pods", args(&[])),
            ("nonesuch", "logs", args(&[])),
        ] {
            let fault = core.integration_launch("main", tool, verb, None, &request, "tab").expect_err("refused");
            assert_eq!(fault.code, FaultCode::Refused, "{tool} {verb}");
        }
        assert_eq!(core.view_in("main").expect("the main window").panes.len(), panes_now);

        // The launched panes have a row each, and the launch of the first runs its tool when the page starts it.
        let rows = core.integration_context(None, false).panes;
        assert!(rows.iter().any(|row| row.pane == beside.pane && row.view == "logs"));
        core.start_pane(beside.pane, 80, 24, 0, 0).expect("the pane starts");
        assert!(wait_until(std::time::Duration::from_secs(20), || ran(&kubectl)));
        assert_eq!(argv(&kubectl), ["logs", "--follow", "--tail=200", "--", "api-0"].map(String::from).to_vec());
    }

    // ---- profiles, secrets and the program of a tool ---------------------------------------------------------------

    /// A value that must never appear in an answer, an event, a note, a fault or a file.
    const CANARY: &str = "CANARY-8f3a91c2d7e45b60-SECRET";

    fn production_profile() -> serde_json::Value {
        serde_json::json!({
            "id": "prod",
            "tool": "kube",
            "name": "Production",
            "context": "prod-cluster",
            "production": true,
            "isolated": false,
            "env": {"AWS_PROFILE": "prod"},
            "secret_env": ["AWS_SECRET_ACCESS_KEY"]
        })
    }

    fn keys_of(value: &serde_json::Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value.as_object().expect("an object").keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn profiles_are_listed_without_secret_values() {
        let machine = Machine::new("profiles");
        let (core, rec) = machine.core();
        assert!(core.integration_profiles().profiles.is_empty());

        let view = core.integration_profile_save(production_profile()).expect("a profile");
        assert_eq!(view.profiles.len(), 1);
        let json = serde_json::to_value(&view).expect("JSON");
        assert_eq!(keys_of(&json), ["notes", "profiles"]);
        let profile = &json["profiles"][0];
        assert_eq!(
            keys_of(profile),
            ["context", "env", "id", "isolated", "name", "namespace", "node", "production", "secret_env", "section", "subscription", "tool"]
        );
        assert_eq!(profile["env"], serde_json::json!({"AWS_PROFILE": "prod"}), "the plain variables are names and values");
        assert_eq!(profile["secret_env"], serde_json::json!([{"name": "AWS_SECRET_ACCESS_KEY", "has_value": false}]));
        assert!(rec.integration_events().iter().any(|(_, e)| e.kind == IntegrationChange::Profiles));

        // A value goes in; the list says only that there is one.
        core.integration_secret_set("prod", "AWS_SECRET_ACCESS_KEY", CANARY.to_owned()).expect("stored");
        let listed = serde_json::to_value(core.integration_profiles()).expect("JSON");
        assert_eq!(listed["profiles"][0]["secret_env"], serde_json::json!([{"name": "AWS_SECRET_ACCESS_KEY", "has_value": true}]));
        assert!(!listed.to_string().contains(CANARY));
        assert_eq!(
            machine.store.get("int:prod:AWS_SECRET_ACCESS_KEY").expect("the store").as_deref(),
            Some(CANARY),
            "and the keychain has it"
        );

        // Saving again replaces the profile and keeps the value; removing it removes the value too.
        let again = core.integration_profile_save(production_profile()).expect("saved again");
        assert!(again.profiles[0].secret_env[0].has_value);
        let seen = rec.integration_events().len();
        let gone = core.integration_profile_remove("prod").expect("removed");
        assert!(gone.profiles.is_empty());
        assert_eq!(machine.store.get("int:prod:AWS_SECRET_ACCESS_KEY").expect("the store"), None, "its secret went with it");
        assert!(rec.integration_events()[seen..].iter().any(|(_, e)| e.kind == IntegrationChange::Profiles));
        // A profile that is not there is already gone; an id that cannot be one is refused.
        assert!(core.integration_profile_remove("nonesuch").expect("nothing to remove").profiles.is_empty());
        assert_eq!(core.integration_profile_remove("Not A Valid Id").expect_err("no such id").code, FaultCode::Profile);
    }

    #[test]
    fn a_profile_save_refuses_a_bad_profile_with_its_field_and_no_value() {
        let machine = Machine::new("badprofile");
        let (core, _rec) = machine.core();
        let with = |extra: serde_json::Value| {
            let mut base = serde_json::json!({"id": "p1", "tool": "kube", "name": "x"});
            base.as_object_mut().expect("an object").extend(extra.as_object().expect("an object").clone());
            base
        };
        // The refusals of the loader's own rules name the profile and the field.
        let named = [
            (serde_json::json!({"id": "Prod!", "tool": "kube", "name": "x"}), "id"),
            (with(serde_json::json!({"context": format!("bad context\n{CANARY}")})), "context"),
            (with(serde_json::json!({"namespace": format!("{CANARY} x")})), "namespace"),
            (with(serde_json::json!({"env": {"LD_PRELOAD": CANARY}})), "LD_PRELOAD"),
            (with(serde_json::json!({"secret_env": ["not a name"]})), "env name"),
        ];
        for (profile, field) in named {
            let fault = core.integration_profile_save(profile).expect_err("refused");
            assert_eq!((fault.code, fault.reason), (FaultCode::Profile, "invalid"), "{field}");
            assert!(fault.text.contains(field), "the sentence names the field {field}: {fault:?}");
            assert!(fault.text.ends_with('.') && !fault.text.contains(CANARY), "{fault:?}");
        }
        // What is not a profile at all gets one fixed sentence that quotes nothing.
        let shapes = [
            serde_json::json!({"id": "p1", "tool": "nonesuch", "name": "x"}),
            with(serde_json::json!({"surprise": CANARY})),
            with(serde_json::json!({"production": CANARY})),
            serde_json::json!(CANARY),
            serde_json::json!([CANARY]),
            serde_json::json!(null),
        ];
        for profile in shapes {
            let fault = core.integration_profile_save(profile.clone()).expect_err("refused");
            assert_eq!((fault.code, fault.reason, fault.text.as_str()), (FaultCode::Profile, "invalid", "The profile is not valid."), "{profile}");
        }
        assert!(core.integration_profiles().profiles.is_empty(), "nothing was saved");
        assert!(!machine.data.join("integrations.json").exists() || !std::fs::read_to_string(machine.data.join("integrations.json")).unwrap_or_default().contains(CANARY));
    }

    #[test]
    fn a_secret_is_write_only_and_capped() {
        let machine = Machine::new("secret");
        let (core, rec) = machine.core();
        core.integration_profile_save(production_profile()).expect("a profile");
        let account = "int:prod:AWS_SECRET_ACCESS_KEY";
        let stored = || machine.store.get(account).expect("the store");

        // The largest value is the cap; one byte more, an empty value and a NUL are refused and store nothing.
        let largest = format!("{CANARY}{}", "x".repeat(1024 - CANARY.len()));
        assert_eq!(largest.len(), 1024);
        core.integration_secret_set("prod", "AWS_SECRET_ACCESS_KEY", largest.clone()).expect("1024 bytes");
        assert_eq!(stored().as_deref(), Some(largest.as_str()));
        for value in [format!("{largest}x"), String::new(), format!("{CANARY}\0")] {
            let fault = core.integration_secret_set("prod", "AWS_SECRET_ACCESS_KEY", value).expect_err("refused");
            assert_eq!((fault.code, fault.reason), (FaultCode::Profile, "secret"));
            assert!(!fault.text.contains(CANARY) && fault.text.ends_with('.'), "{fault:?}");
        }
        assert_eq!(stored().as_deref(), Some(largest.as_str()), "a refused value changes nothing");

        // A name the profile does not declare, a name that is not a name and a profile that is not there are refused too.
        for (profile, name) in [("prod", "OTHER_NAME"), ("prod", "not a name"), ("nonesuch", "AWS_SECRET_ACCESS_KEY"), ("Not Valid", "X")] {
            let fault = core.integration_secret_set(profile, name, CANARY.to_owned()).expect_err("refused");
            assert_eq!(fault.code, FaultCode::Profile, "{profile} {name}");
            assert!(!fault.text.contains(CANARY), "{fault:?}");
        }
        assert_eq!(machine.store.get("int:prod:OTHER_NAME").expect("the store"), None);

        // The events say that the profiles changed and nothing else.
        let events = rec.integration_events();
        assert!(events.iter().any(|(_, e)| e.kind == IntegrationChange::Profiles));
        assert!(!format!("{events:?}").contains(CANARY));

        // Clearing removes the value, and clearing it again is not a problem.
        core.integration_secret_clear("prod", "AWS_SECRET_ACCESS_KEY").expect("cleared");
        assert_eq!(stored(), None);
        core.integration_secret_clear("prod", "AWS_SECRET_ACCESS_KEY").expect("cleared again");
        assert_eq!(core.integration_secret_clear("nonesuch", "X").expect_err("no such profile").code, FaultCode::Profile);
        assert!(!core.integration_profiles().profiles[0].secret_env[0].has_value);
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_path_is_checked_against_the_tools_names() {
        use std::os::unix::fs::PermissionsExt;

        let machine = Machine::new("binary");
        let found = fake_cli::FakeCli::new(&machine.bin, "kubectl");
        let (core, rec) = machine.core();
        let kube = |tools: &[ToolView]| tools.iter().find(|tool| tool.id == ToolId::Kube).expect("an entry").clone();
        assert_eq!(kube(&core.integration_tools()).path.as_deref(), found.path().to_str());

        // The person's own kubectl: right name, absolute, a file nobody else can replace.
        let other_dir = machine.root.join("other");
        std::fs::create_dir_all(&other_dir).expect("a directory");
        let mine = fake_cli::FakeCli::new(&other_dir, "kubectl");
        let seen = rec.integration_events().len();
        let tools = core.integration_binary_set("kube", mine.path().to_str().expect("a path")).expect("set");
        assert_eq!(tools.len(), 4, "the four tools again");
        let entry = kube(&tools);
        assert_eq!((entry.path.as_deref(), entry.binary.as_deref(), entry.place), (mine.path().to_str(), mine.path().to_str(), Some("override")));
        let events = rec.integration_events();
        assert!(events[seen..].iter().any(|(_, e)| (e.kind, e.tool) == (IntegrationChange::Tools, Some(ToolId::Kube))), "{events:?}");
        assert_eq!(kube(&core.integration_tools()).binary.as_deref(), mine.path().to_str(), "and it stays");
        let file = std::fs::read_to_string(machine.data.join("integration_local.json")).expect("the machine-local file");
        assert!(file.contains("other/kubectl"), "{file}");

        // Refused: a name that is not one of the tool's own, a relative path, a file that is not executable, one that
        // everyone can write, and a tool that does not exist. Each is a fault and none changes the setting.
        let evil = fake_cli::FakeCli::new(&other_dir, "kubectl-evil");
        let plain_dir = machine.root.join("plain");
        std::fs::create_dir_all(&plain_dir).expect("a directory");
        let plain = plain_dir.join("kubectl");
        std::fs::write(&plain, "#!/bin/sh\n").expect("a file");
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).expect("not executable");
        let open_dir = machine.root.join("open");
        std::fs::create_dir_all(&open_dir).expect("a directory");
        // A copy, not a fake: the fakes are hard links of one script, and a mode set on one is the mode of all of them.
        let open = open_dir.join("kubectl");
        std::fs::copy(mine.path(), &open).expect("a copy of the script");
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o757)).expect("everyone can write");
        for (tool, path) in [
            ("kube", evil.path().to_string_lossy().into_owned()),
            ("kube", "bin/kubectl".to_owned()),
            ("kube", "kubectl".to_owned()),
            ("kube", plain.to_string_lossy().into_owned()),
            ("kube", open.to_string_lossy().into_owned()),
            ("kube", format!("{}/nonesuch/kubectl", machine.root.display())),
            ("kube", format!("{}\n", mine.path().display())),
            ("nonesuch", mine.path().to_string_lossy().into_owned()),
            ("talos", mine.path().to_string_lossy().into_owned()),
        ] {
            let fault = core.integration_binary_set(tool, &path).expect_err("refused");
            assert_eq!(fault.code, FaultCode::Refused, "{tool} {path}");
            assert!(fault.text.ends_with('.'), "{fault:?}");
        }
        assert_eq!(kube(&core.integration_tools()).binary.as_deref(), mine.path().to_str(), "a refusal changes nothing");

        // Clearing goes back to the search.
        let tools = core.integration_binary_clear("kube").expect("cleared");
        let entry = kube(&tools);
        assert_eq!((entry.binary.as_deref(), entry.path.as_deref()), (None, found.path().to_str()));
        assert_eq!(core.integration_binary_clear("nonesuch").expect_err("no such tool").code, FaultCode::Refused);
    }

    #[cfg(unix)]
    #[test]
    fn no_answer_or_event_holds_the_canary() {
        let machine = Machine::new("canary");
        let kubectl = fake_cli::FakeCli::new(&machine.bin, "kubectl").prints(&fixture("kubectl/config_view.json"));
        let (core, rec) = machine.core();
        core.integration_profile_save(production_profile()).expect("a profile");
        core.integration_secret_set("prod", "AWS_SECRET_ACCESS_KEY", CANARY.to_owned()).expect("stored");
        let pane = 7;
        let selection = Selection { profile: Some("prod".into()), ..Selection::default() };

        // Every answer and every fault of every command, as the page would read it (JSON) and as a log would (Debug).
        let mut seen: Vec<(String, String)> = Vec::new();
        let mut note = |what: &str, json: serde_json::Value, debug: String| seen.push((what.to_owned(), format!("{json}\n{debug}")));
        macro_rules! take {
            ($what:expr, $value:expr) => {{
                let value = $value;
                note($what, serde_json::to_value(&value).expect("JSON"), format!("{value:?}"));
                value
            }};
        }
        take!("tools", core.integration_tools());
        let selected = core.integration_select("kube", Some(pane), &selection);
        take!("select", selected.expect("selected"));
        let run = core.integration_run("kube", "contexts", Some(pane), &args(&[]));
        take!("run", run.expect("a read with the secret's profile"));
        assert!(
            String::from_utf8_lossy(&kubectl.env_bytes()).contains(CANARY),
            "the value reached the environment of the child: the canary is not vacuous"
        );
        take!("run refused", core.integration_run("kube", "delete_pod", Some(pane), &args(&[])).expect_err("refused"));
        take!("context", core.integration_context(Some(pane), false));
        let card = core.integration_prepare("kube", "delete_pod", Some(pane), &args(&[("pod", "api-0")]));
        let card = take!("card", card.expect("a card"));
        take!("confirm wrong", core.integration_confirm("main", &card.ticket, &"0".repeat(64)).expect_err("a wrong hash"));
        let ran = core.integration_confirm("main", &card.ticket, &card.hash);
        take!("confirm ran", ran.expect("ran"));
        let exec = core.integration_prepare("kube", "exec", Some(pane), &args(&[("pod", "api-0")])).expect("a card");
        let launched = core.integration_confirm("main", &exec.ticket, &exec.hash).expect("launched");
        assert_eq!(launched.kind, ConfirmKind::Launched);
        take!("confirm launched", launched);
        let logs = core.integration_launch("main", "kube", "logs", Some(pane), &args(&[("pod", "api-0")]), "tab");
        take!("launch", logs.expect("logs"));
        let cancelled = core.integration_prepare("kube", "delete_pod", Some(pane), &args(&[("pod", "api-1")])).expect("a card");
        core.integration_cancel(&cancelled.ticket);
        take!("profiles", core.integration_profiles());
        take!("profile saved", core.integration_profile_save(production_profile()).expect("saved"));
        take!("profile refused", core.integration_profile_save(serde_json::json!({"id": "p2", "tool": "kube", "name": "x", "context": format!("{CANARY}\nbad")})).expect_err("refused"));
        take!("secret refused", core.integration_secret_set("prod", "OTHER", CANARY.to_owned()).expect_err("refused"));
        take!("binary refused", core.integration_binary_set("kube", CANARY).expect_err("refused"));
        take!("binary cleared", core.integration_binary_clear("kube").expect("cleared"));
        take!("context after", core.integration_context(None, false));

        for (what, text) in &seen {
            assert!(!text.contains(CANARY), "the answer of {what} holds the secret");
        }
        assert_eq!(seen.len(), 17, "{} answers were looked at", seen.len());
        let events = rec.integration_events();
        assert!(events.len() >= 6);
        let events_json = serde_json::to_string(&events.iter().map(|(_, e)| e).collect::<Vec<_>>()).expect("JSON");
        assert!(!events_json.contains(CANARY) && !format!("{events:?}").contains(CANARY), "an event holds the secret");

        // The value is in the keychain and in no file of the data directory.
        let mut files = vec![machine.data.clone()];
        let mut looked_at = 0;
        while let Some(dir) = files.pop() {
            for entry in std::fs::read_dir(&dir).expect("a directory").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    files.push(path);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    looked_at += 1;
                    assert!(!String::from_utf8_lossy(&bytes).contains(CANARY), "{} holds the secret", path.display());
                }
            }
        }
        assert!(looked_at >= 3, "{looked_at} files were looked at");
        assert_eq!(machine.store.get("int:prod:AWS_SECRET_ACCESS_KEY").expect("the store").as_deref(), Some(CANARY));
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
