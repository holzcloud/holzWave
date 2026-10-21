//! The integration service the app shell holds: the registry of tools, reads with single flight, faults, prepare and
//! confirm, and terminal launches.
//!
//! The service is the one object the shell talks to. It owns no rule of its own beyond putting the earlier pieces in
//! order: the detector finds the program, the context state and the profiles say what is selected, the tool module
//! builds the argument list and reads the output, the environment policy builds the child's environment, and the
//! runner is the only code that starts a process. No tool is named here: what differs between the tools is a
//! compiled-in description and the methods of the [`Integration`] trait.
//!
//! The order of a read, which every failure leaves at a fixed place:
//!
//! 1. the request is checked against the closed catalogue: the tool, the verb (a read with a view), the arguments;
//! 2. the selection of the pane is resolved with its profile;
//! 3. the tool is found (`missing` with the program's name when it is not), and looked at again right before the run;
//! 4. the tool module builds the argument list; the context is applied to the environment; the profile's secret values
//!    are read from the keychain for this one run;
//! 5. the runner runs it with a time limit and a cap on the output;
//! 6. the answer becomes a model or a fault.
//!
//! The environment of a child is the inherited one without the app's own and the hijackable names, the rebuilt `PATH`,
//! the profile's variables, the tool's fixed ones, the isolation variable of the selection and the secrets.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::args::ArgError;
use super::context::{self, ContextError, ContextState, Detection as ContextDetection, Resolved};
use super::detect::{self, Detection, Found, UnusableReason};
use super::kube::ContextEntry;
use super::profiles::{IntegrationProfiles, LocalFile};
use super::run::{
    spawn_supported, Env, EnvExtra, EnvPolicy, RunCancel, RunEnd, RunError, RunLimits, RunResult, Runner,
    DEFAULT_STDOUT, DEFAULT_TIMEOUT,
};
use super::view::{text_fallback, ViewModel};
use super::{
    CheckedSelection, Integration, IntegrationSpec, ParseError, SelectKind, Selection, ToolId, Validated, Verb,
    VerbEffect,
};
use crate::layout::PaneId;
use crate::platform::{self, Os, ProgramCheck, ShellLaunch};
use crate::secrets::SecretStore;

// ---------------------------------------------------------------------------------------------------------------------
// The machine.
// ---------------------------------------------------------------------------------------------------------------------

type CheckFn = dyn Fn(&str) -> ProgramCheck + Send + Sync;
type ModeFn = dyn Fn(&str) -> Option<u32> + Send + Sync;
type LoginFn = dyn Fn() -> Vec<String> + Send + Sync;

/// What the service knows of the machine it runs on. The shell builds it from the real one ([`HostFacts::system`]); the
/// tests build it over a scratch directory ([`HostFacts::for_tests`]). Nothing of it is read from the process after it
/// was built, so a service behaves the same wherever it is.
///
/// `Debug` prints no environment: the inherited variables can hold anything.
pub struct HostFacts {
    /// The system whose rules apply.
    pub os: Os,
    /// The person's home directory.
    pub home: Option<String>,
    /// The environment the app was started with. The child's is built from it by the environment policy.
    pub inherited: Vec<(OsString, OsString)>,
    /// The person's shell, for a launch that is the shell itself.
    pub shell: ShellLaunch,
    /// Whether this is a Flatpak build, where no integration runs.
    pub in_flatpak: bool,
    /// The data directory of the app: profiles, the local file, overlays and isolated directories live below it.
    pub data_dir: PathBuf,
    check: Arc<CheckFn>,
    mode_of: Arc<ModeFn>,
    login_path: Arc<LoginFn>,
}

impl fmt::Debug for HostFacts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostFacts")
            .field("os", &self.os)
            .field("in_flatpak", &self.in_flatpak)
            .field("inherited", &self.inherited.len())
            .finish_non_exhaustive()
    }
}

/// What the file system says about a candidate program.
fn check_on_disk(path: &str) -> ProgramCheck {
    let Ok(meta) = std::fs::metadata(path) else { return ProgramCheck::Missing };
    if !meta.is_file() {
        return ProgramCheck::NotAFile;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return ProgramCheck::NotExecutable;
        }
    }
    ProgramCheck::Usable
}

/// The Unix mode bits of a path, following links; `None` where there are none.
fn mode_on_disk(path: &str) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|meta| meta.mode())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

impl HostFacts {
    /// The real machine: the process environment as it is now, the real file system, and the login shell's `PATH`
    /// asked for once, the first time it is needed (never on Windows, never in a Flatpak build).
    pub fn system(data_dir: &Path, shell: ShellLaunch) -> HostFacts {
        let os = Os::current();
        let inherited: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        let lookup = |name: &str| std::env::var(name).ok();
        let in_flatpak = platform::in_flatpak(&lookup, &|path| Path::new(path).exists());
        let home = if os == Os::Windows { lookup("USERPROFILE") } else { lookup("HOME") }.filter(|home| !home.is_empty());
        let login_path: Arc<LoginFn> = {
            let once: Arc<std::sync::OnceLock<Vec<String>>> = Arc::new(std::sync::OnceLock::new());
            let probe_env: Vec<(OsString, OsString)> = ["HOME", "USER", "SHELL"]
                .iter()
                .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(*name), value)))
                .collect();
            let shell_path = lookup("SHELL");
            Arc::new(move || {
                once.get_or_init(|| login_shell_path(os, in_flatpak, shell_path.as_deref(), &probe_env)).clone()
            })
        };
        HostFacts {
            os,
            home,
            inherited,
            shell,
            in_flatpak,
            data_dir: data_dir.to_path_buf(),
            check: Arc::new(check_on_disk),
            mode_of: Arc::new(mode_on_disk),
            login_path,
        }
    }

    /// A machine for a test. Everything below `root` is real; everywhere else the detector finds nothing, so no tool
    /// that is installed on the computer running the test can be used. The `PATH` lists `path_dirs` and then the system
    /// directories (the fake programs call `env` and `sleep`), the login shell's `PATH` is empty, and the environment
    /// holds the `PATH` and the `HOME` only; add more with [`HostFacts::with_env`].
    pub fn for_tests(root: &Path, path_dirs: &[PathBuf], home: &Path, data_dir: &Path) -> HostFacts {
        let mut path: Vec<String> = path_dirs.iter().map(|dir| dir.to_string_lossy().into_owned()).collect();
        path.push("/usr/bin".to_owned());
        path.push("/bin".to_owned());
        let root = root.to_string_lossy().into_owned();
        let below = move |path: &str| path == root || path.starts_with(&format!("{root}/"));
        let (below_check, below_mode) = (below.clone(), below);
        HostFacts {
            os: Os::current(),
            home: Some(home.to_string_lossy().into_owned()),
            inherited: vec![
                (OsString::from("PATH"), OsString::from(path.join(":"))),
                (OsString::from("HOME"), home.as_os_str().to_owned()),
            ],
            shell: ShellLaunch { program: "/bin/zsh".to_owned(), args: vec!["-l".to_owned()] },
            in_flatpak: false,
            data_dir: data_dir.to_path_buf(),
            check: Arc::new(move |path| if below_check(path) { check_on_disk(path) } else { ProgramCheck::Missing }),
            mode_of: Arc::new(move |path| if below_mode(path) { mode_on_disk(path) } else { None }),
            login_path: Arc::new(Vec::new),
        }
    }

    /// The same machine as a Flatpak build or not.
    #[must_use]
    pub fn with_flatpak(mut self, in_flatpak: bool) -> Self {
        self.in_flatpak = in_flatpak;
        self
    }

    /// The same machine with one more inherited variable.
    #[must_use]
    pub fn with_env(mut self, name: &str, value: &str) -> Self {
        self.inherited.push((OsString::from(name), OsString::from(value)));
        self
    }

    /// The same machine with another shell.
    #[must_use]
    pub fn with_shell(mut self, shell: ShellLaunch) -> Self {
        self.shell = shell;
        self
    }

    /// A variable of the inherited environment, when it is set and is text.
    pub fn env(&self, name: &str) -> Option<String> {
        let same = |own: &OsString| {
            let own = own.to_string_lossy();
            if self.os == Os::Windows { own.eq_ignore_ascii_case(name) } else { own == name }
        };
        self.inherited.iter().rev().find(|(own, _)| same(own)).and_then(|(_, value)| value.to_str().map(str::to_owned))
    }
}

/// The directories of the login shell's `PATH`, asked for by starting the shell once through the runner with the
/// script of the detector, a closed environment and a time limit. An empty list when there is nothing to ask.
fn login_shell_path(os: Os, in_flatpak: bool, shell: Option<&str>, probe_env: &[(OsString, OsString)]) -> Vec<String> {
    let Some(probe) = detect::login_probe(os, shell, &|path| Path::new(path).is_file()) else { return Vec::new() };
    let runner = Runner::new(spawn_supported(std::env::consts::OS, in_flatpak));
    let Ok(env) = EnvPolicy::build(probe_env.iter().cloned(), os, detect::LOGIN_PROBE_PATH.to_owned(), EnvExtra::default())
    else {
        return Vec::new();
    };
    let args: Vec<OsString> = probe.args.iter().map(OsString::from).collect();
    let limits = RunLimits { max_stdout: 64 * 1024, timeout: detect::LOGIN_PROBE_TIMEOUT };
    match runner.run_argv(Path::new(&probe.program), &args, env, &limits, &RunCancel::default()) {
        Ok(result) if result.end == RunEnd::Finished => {
            detect::parse_login_path(os, &result.stdout, &|dir| Path::new(dir).is_dir())
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Faults.
// ---------------------------------------------------------------------------------------------------------------------

/// The code of a fault, as the page reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultCode {
    /// The request is not in the catalogue, or an argument or the selection failed its check.
    Refused,
    /// The program of the tool was not found or cannot be used.
    Missing,
    /// The tool ran and reported an error, or could not be started.
    Failed,
    /// The tool did not answer within the time limit.
    Timeout,
    /// The tool wrote more than the cap.
    Overflow,
    /// All of the process slots are in use.
    Busy,
    /// The approval ticket is unknown, expired, answered with the wrong hash or no longer what the card showed.
    Ticket,
    /// A profile or a secret could not be used or saved.
    Profile,
    /// The integrations do not run in this build.
    Unsupported,
    /// The output could not be turned into a model.
    Parse,
}

/// Why a call gave no answer: a code, a short reason, and one sentence for the person. The sentence holds nothing of
/// the request; for a tool that reported an error it holds at most 240 cleaned characters of the tool's own first line.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Fault {
    /// The code.
    pub code: FaultCode,
    /// A fixed word for the cause inside the code.
    pub reason: &'static str,
    /// The sentence to show.
    pub text: String,
}

impl Fault {
    fn new(code: FaultCode, reason: &'static str, text: impl Into<String>) -> Self {
        Self { code, reason, text: text.into() }
    }

    fn refused(reason: &'static str, text: impl Into<String>) -> Self {
        Self::new(FaultCode::Refused, reason, text)
    }
}

impl fmt::Debug for Fault {
    /// The code, the reason and the size of the sentence: the sentence may carry a line of the tool's own message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fault({:?}, {}, {} bytes)", self.code, self.reason, self.text.len())
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for Fault {}

fn arg_fault(error: ArgError) -> Fault {
    Fault::refused("argument", format!("The request was refused: {}.", error.as_str()))
}

fn context_fault(error: ContextError) -> Fault {
    Fault::refused("selection", format!("The selection was refused: {}.", error.as_str()))
}

// ---------------------------------------------------------------------------------------------------------------------
// What the page gets.
// ---------------------------------------------------------------------------------------------------------------------

/// The selection a call used, after the pins of the profile were filled in, and whether it is production.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelectionView {
    /// The selection as it was used.
    pub selection: Selection,
    /// The display name of the production profile, or `None`.
    pub production: Option<String>,
}

/// The answer to a read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunAnswer {
    /// What to show.
    pub view: ViewModel,
    /// The selection the read used.
    pub selection: SelectionView,
    /// When it was read, in seconds since the Unix epoch.
    pub fetched: u32,
}

/// Where a tool stands on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolState {
    /// Found and usable.
    Found,
    /// Not found.
    NotFound,
}

/// One tool as the settings and the sidebar show it.
#[derive(Debug, Clone, Serialize)]
pub struct ToolView {
    /// Which tool.
    pub id: ToolId,
    /// The name the person reads.
    pub label: &'static str,
    /// Found or not.
    pub state: ToolState,
    /// The absolute path of the program, when found.
    pub path: Option<String>,
    /// The directories that were looked in, when it was not found.
    pub searched: Vec<String>,
    /// The verbs the tool has.
    pub verbs: &'static [Verb],
    /// The selection fields the tool takes.
    pub selects: &'static [SelectKind],
}

// ---------------------------------------------------------------------------------------------------------------------
// The service.
// ---------------------------------------------------------------------------------------------------------------------

/// What the service keeps about one tool.
struct Slot {
    found: Option<Found>,
    state: ContextDetection,
    searched: Vec<String>,
}

impl Default for Slot {
    fn default() -> Self {
        Self { found: None, state: ContextDetection::Unknown, searched: Vec::new() }
    }
}

struct State {
    profiles: IntegrationProfiles,
    local: LocalFile,
    slots: Vec<Slot>,
    contexts: Vec<Vec<ContextEntry>>,
    notes: Vec<String>,
}

/// The integration service. It is `Send + Sync`: the shell shares one behind an `Arc`.
pub struct IntegrationService {
    dir: PathBuf,
    secrets: Arc<dyn SecretStore>,
    facts: HostFacts,
    runner: Runner,
    enabled: bool,
    limits: RunLimits,
    tools: Vec<&'static dyn Integration>,
    context: ContextState,
    state: Mutex<State>,
}

impl fmt::Debug for IntegrationService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IntegrationService(enabled {}, {} tools)", self.enabled, self.tools.len())
    }
}

/// The request after it went through the catalogue and the selection.
struct Request {
    tool: ToolId,
    integration: &'static dyn Integration,
    verb: &'static Verb,
    validated: Validated,
    resolved: Resolved,
}

fn now_seconds() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| u32::try_from(elapsed.as_secs()).unwrap_or(u32::MAX))
}

fn slot_of(tool: ToolId) -> usize {
    ToolId::ALL.iter().position(|own| *own == tool).unwrap_or(0)
}

/// A `Selection` of the text of a checked one.
fn selection_text(checked: &CheckedSelection) -> Selection {
    Selection {
        profile: checked.profile.as_ref().map(|v| v.as_str().to_owned()),
        context: checked.context.as_ref().map(|v| v.as_str().to_owned()),
        namespace: checked.namespace.as_ref().map(|v| v.as_str().to_owned()),
        subscription: checked.subscription.as_ref().map(|v| v.as_str().to_owned()),
        section: checked.section.as_ref().map(|v| v.as_str().to_owned()),
        node: checked.node.as_ref().map(|v| v.as_str().to_owned()),
    }
}

impl IntegrationService {
    /// A service over the data directory `dir`: it reads the profiles, the machine-local file and the pane selections of
    /// `live_panes` (what could not be used is kept as `.bad` and named in [`IntegrationService::notes`]), and builds
    /// the runner, which is disabled when `facts.in_flatpak` is set.
    pub fn new(dir: &Path, secrets: Arc<dyn SecretStore>, facts: HostFacts, live_panes: &[PaneId]) -> Self {
        let mut notes = Vec::new();
        let (profiles, found) = IntegrationProfiles::load(dir);
        notes.extend(found);
        let (local, found) = LocalFile::load(dir);
        notes.extend(found);
        let (context, found) = ContextState::load(dir, live_panes);
        notes.extend(found);
        let enabled = spawn_supported(std::env::consts::OS, facts.in_flatpak);
        let tools: Vec<&'static dyn Integration> = vec![&super::kube::KUBE, &super::talos::TALOS, &super::azure::AZURE, &super::gam::GAM];
        let slots = tools.iter().map(|_| Slot::default()).collect();
        let contexts = tools.iter().map(|_| Vec::new()).collect();
        Self {
            dir: dir.to_path_buf(),
            secrets,
            facts,
            runner: Runner::new(enabled),
            enabled,
            limits: RunLimits { max_stdout: DEFAULT_STDOUT, timeout: DEFAULT_TIMEOUT },
            tools,
            context,
            state: Mutex::new(State { profiles, local, slots, contexts, notes }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The notes of the files that were read at the start, once each.
    pub fn notes(&self) -> Vec<String> {
        self.lock().notes.clone()
    }

    fn integration(&self, tool: ToolId) -> &'static dyn Integration {
        // The table holds every tool, so the first branch always answers; the second keeps the function total.
        self.tools.iter().copied().find(|own| own.spec().id == tool).unwrap_or(self.tools[0])
    }

    fn parse_tool(&self, name: &str) -> Result<ToolId, Fault> {
        ToolId::parse(name).map_err(|_| Fault::refused("tool", "The tool is not known."))
    }

    // ---- the tools -------------------------------------------------------------------------------------------------

    fn detect_facts<R>(&self, work: impl FnOnce(&detect::Facts<'_>) -> R) -> R {
        let env = |name: &str| self.facts.env(name);
        let check = |path: &str| (self.facts.check)(path);
        let login = || (self.facts.login_path)();
        let mode = |path: &str| (self.facts.mode_of)(path);
        let facts = detect::Facts {
            os: self.facts.os,
            env: &env,
            check: &check,
            home: self.facts.home.as_deref(),
            login_path: &login,
            mode_of: &mode,
        };
        work(&facts)
    }

    /// Finds the tool: the cached answer when the file is still usable, a new detection otherwise.
    fn locate(&self, tool: ToolId) -> Detection {
        let spec = self.integration(tool).spec();
        let (cached, set_path) = {
            let state = self.lock();
            let set_path = state.local.binary(tool).and_then(Path::to_str).map(str::to_owned);
            (state.slots[slot_of(tool)].found.clone(), set_path)
        };
        if let Some(found) = cached {
            if self.detect_facts(|facts| detect::recheck(&found, facts)) {
                return Detection::Found(found);
            }
        }
        let detection = self.detect_facts(|facts| detect::detect(spec, facts, set_path.as_deref()));
        let mut state = self.lock();
        let slot = &mut state.slots[slot_of(tool)];
        match &detection {
            Detection::Found(found) => {
                slot.found = Some(found.clone());
                slot.state = ContextDetection::Found;
                slot.searched.clear();
            }
            Detection::NotFound { searched } => {
                slot.found = None;
                slot.state = ContextDetection::NotFound;
                slot.searched = searched.clone();
            }
            Detection::Unusable { .. } => {
                slot.found = None;
                slot.state = ContextDetection::NotFound;
                slot.searched.clear();
            }
        }
        detection
    }

    /// The program of the tool, or the fault that says why there is none.
    fn found(&self, tool: ToolId) -> Result<Found, Fault> {
        let spec = self.integration(tool).spec();
        let name = self.binary_name(spec);
        match self.locate(tool) {
            Detection::Found(found) => Ok(found),
            Detection::NotFound { .. } => Err(Fault::new(
                FaultCode::Missing,
                "not_found",
                format!("{name} was not found on this computer. Install it or set where it is."),
            )),
            Detection::Unusable { reason, .. } => Err(Fault::new(
                FaultCode::Missing,
                "unusable",
                format!("{name} cannot be used: {}.", UnusableReason::as_str(reason)),
            )),
        }
    }

    /// The program name the person knows the tool by, for a sentence.
    fn binary_name(&self, spec: &IntegrationSpec) -> &'static str {
        let names = if self.facts.os == Os::Windows { spec.windows_binaries } else { spec.binaries };
        names.first().copied().unwrap_or(spec.label)
    }

    /// One entry per tool, in the order the person sees them.
    pub fn tools(&self) -> Vec<ToolView> {
        ToolId::ALL
            .iter()
            .map(|tool| {
                let spec = self.integration(*tool).spec();
                let detection = self.locate(*tool);
                let (state, path, searched) = match detection {
                    Detection::Found(found) => (ToolState::Found, Some(found.path), Vec::new()),
                    Detection::NotFound { searched } => (ToolState::NotFound, None, searched),
                    Detection::Unusable { path, .. } => (ToolState::NotFound, Some(path), Vec::new()),
                };
                ToolView { id: *tool, label: spec.label, state, path, searched, verbs: spec.verbs, selects: spec.selects }
            })
            .collect()
    }

    // ---- reads -----------------------------------------------------------------------------------------------------

    /// Checks a request against the closed catalogue and resolves the selection of the pane.
    fn request(
        &self,
        tool: ToolId,
        verb: &str,
        args: &BTreeMap<String, String>,
        selection: &Selection,
    ) -> Result<Request, Fault> {
        let integration = self.integration(tool);
        let verb = integration
            .spec()
            .verb(verb)
            .ok_or_else(|| Fault::refused("verb", "The action is not known for this tool."))?;
        if verb.effect != VerbEffect::Read || verb.output != super::Output::View {
            return Err(Fault::refused("not_a_view", "This action does not show a view."));
        }
        let validated = Validated::parse(verb, args).map_err(arg_fault)?;
        let resolved = {
            let state = self.lock();
            context::resolve(&state.profiles, tool, selection).map_err(context_fault)?
        };
        Ok(Request { tool, integration, verb, validated, resolved })
    }

    /// Reads a view of a tool: the verb must be a read with a view.
    pub fn run_view(
        &self,
        tool: &str,
        verb: &str,
        pane: Option<PaneId>,
        args: &BTreeMap<String, String>,
    ) -> Result<RunAnswer, Fault> {
        let tool = self.parse_tool(tool)?;
        let selection = self.context.selection_for(tool, pane);
        let request = self.request(tool, verb, args, &selection)?;
        self.run_read(&request, pane)
    }

    /// The environment of a child for a request.
    fn child_env(&self, request: &Request, found: &Found, pane: Option<PaneId>) -> Result<Env, Fault> {
        let spec = request.integration.spec();
        let (local, contexts) = {
            let state = self.lock();
            (state.local.clone(), state.contexts[slot_of(request.tool)].clone())
        };
        let home = self.facts.home.clone().unwrap_or_default();
        let user_env = |name: &str| self.facts.env(name);
        let at = context::AppliedFor {
            os: self.facts.os,
            home: &home,
            data_dir: &self.facts.data_dir,
            pane,
            user_env: &user_env,
            contexts: &contexts,
        };
        let applied = context::apply_env(request.tool, &request.resolved, &local, &at, false);
        if applied.failed {
            return Err(Fault::new(FaultCode::Failed, "isolation", "The private configuration directory could not be made, so nothing was started."));
        }
        let process_path = self.facts.env("PATH").unwrap_or_default();
        let login = (self.facts.login_path)();
        let path = detect::child_path(self.facts.os, &found.dir, &process_path, &login);
        let extra = EnvExtra {
            plain: request.resolved.env_plain.clone(),
            fixed: spec.env_set,
            isolation: applied.env,
            secrets: Vec::new(),
        };
        EnvPolicy::build(self.facts.inherited.iter().cloned(), self.facts.os, path, extra)
            .map_err(|error| Fault::new(FaultCode::Failed, "environment", error.to_string()))
    }

    fn run_fault(&self, error: RunError) -> Fault {
        Fault::new(FaultCode::Failed, "start", error.to_string())
    }

    fn run_read(&self, request: &Request, pane: Option<PaneId>) -> Result<RunAnswer, Fault> {
        let found = self.found(request.tool)?;
        let argv = request.integration.argv(request.verb, &request.validated, &request.resolved.selection).map_err(arg_fault)?;
        let env = self.child_env(request, &found, pane)?;
        let result = self
            .runner
            .run_argv(Path::new(&found.path), &argv, env, &self.limits, &RunCancel::default())
            .map_err(|error| self.run_fault(error))?;
        let stdout = self.read_stdout(request, result)?;
        let view = match request.integration.view(request.verb, &stdout) {
            Ok(view) => view,
            Err(ParseError::Unrecognised) => text_fallback(request.verb.label, &stdout),
            Err(other) => return Err(Fault::new(FaultCode::Parse, "unreadable", format!("{}.", other.as_str()))),
        };
        let fetched = now_seconds();
        Ok(RunAnswer {
            view: view.with_fetched(fetched),
            selection: SelectionView {
                selection: selection_text(&request.resolved.selection),
                production: request.resolved.production.clone(),
            },
            fetched,
        })
    }

    fn read_stdout(&self, request: &Request, result: RunResult) -> Result<Vec<u8>, Fault> {
        let label = request.integration.spec().label;
        if result.end == RunEnd::Finished && result.status == Some(0) {
            return Ok(result.stdout);
        }
        Err(Fault::new(FaultCode::Failed, "exit_status", format!("{label} reported an error.")))
    }
}
