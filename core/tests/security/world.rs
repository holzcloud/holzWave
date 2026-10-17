//! The tool world of the release gate: a scratch tree with secrets, links and protected folders, and a stand-in tool
//! host that mirrors the contract of the shell's host.
//!
//! The host opens only `ticket.resolved().path()`, inspects files through `ToolHost::inspect`, writes only when the
//! change time still equals the stamp on the card, and runs commands through the real runner. It proves the approval
//! table and the loop; it proves nothing about the shell's own code, which has tests of its own. Test code only.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use holzwave_core::ai::agent::Limits;
use holzwave_core::ai::gate::ApprovedContext;
use holzwave_core::ai::provider::{CancelToken, Detail};
use holzwave_core::ai::scripted::{Script, ScriptedProvider};
use holzwave_core::ai::service::{AiService, AiSink, SendRequest};
use holzwave_core::ai::store::{ConversationId, MemoryStore};
use holzwave_core::ai::tools::{
    Base, Decision, DenyPolicy, Inspected, Resolved, Scope, Ticket, Tool, ToolError, ToolHost, ToolName, ToolOutput,
    plan, report,
};
use holzwave_core::exec::LocalExec;
use holzwave_core::layout::PaneId;
use holzwave_core::ssh::{ExecCancel, ExecLimits};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::common::wait_until;
use crate::security::canaries::{canaries, file_value, terminal_text};
use crate::security::rig::CollectSink;

/// The first pane of the world.
pub const PANE_A: PaneId = 1;
/// The second pane of the world.
pub const PANE_B: PaneId = 2;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A scratch folder that is removed when it goes out of scope, also when an assertion fails.
pub struct Scratch(PathBuf);

impl Scratch {
    /// A fresh, empty folder.
    pub fn new(tag: &str) -> Scratch {
        Scratch(crate::common::scratch_dir(tag))
    }

    /// The folder.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ---- the tree -----------------------------------------------------------------------------------------------------

/// A scratch tree: a home with secrets, a project, and the folders the shell protects.
pub struct Tree {
    root: PathBuf,
}

impl Tree {
    /// Builds the tree below the canonical form of `scratch` (on macOS the temporary folder is itself a link).
    pub fn new(scratch: &Path) -> Tree {
        let root = fs::canonicalize(scratch).expect("the scratch folder resolves").join("tree");
        fs::create_dir(&root).expect("the tree root");
        let tree = Tree { root };
        for folder in ["home", "proj", "data", "cfg", "ipc"] {
            fs::create_dir(tree.root.join(folder)).expect("a folder");
        }
        let secret = format!("secret material {}\n", file_value());
        let files: Vec<(&str, String)> = vec![
            ("home/.ssh/id_ed25519", secret.clone()),
            ("home/.zshrc", "export PATH=/usr/bin:/bin\n".to_owned()),
            ("home/.config/fish/config.fish", "set -x EDITOR vi\n".to_owned()),
            ("proj/src/lib.rs", "pub fn one() -> u32 {\n    1\n}\n".to_owned()),
            ("proj/notes.txt", "project notes\n".to_owned()),
            ("proj/a.txt", "alpha\n".to_owned()),
            ("proj/b.txt", "beta\n".to_owned()),
            ("proj/leaky.txt", format!("{}\nDEPLOY_SECRET={}\n", terminal_text(&canaries()), file_value())),
            ("proj/NOTES.md", "# Notes\n\nNothing special here.\n".to_owned()),
            ("proj/.env", format!("SERVICE_SECRET={}\n", file_value())),
            ("proj/.git/hooks/pre-commit", "#!/bin/sh\nexit 0\n".to_owned()),
            ("proj/.vscode/tasks.json", "{}\n".to_owned()),
            ("data/settings.json", "{}\n".to_owned()),
            ("data/ai.json", "{\"format\":1}\n".to_owned()),
            ("data/chats/c1.json", "{}\n".to_owned()),
            ("cfg/connections.json", "[]\n".to_owned()),
            ("ipc/ipc.sock", "not a socket\n".to_owned()),
        ];
        for (relative, text) in files {
            tree.write(relative, &text);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink("../home/.ssh", tree.root.join("proj/link")).expect("a link");
            symlink("../home/.ssh/authorized_keys", tree.root.join("proj/evil")).expect("a dangling link");
            symlink("../data", tree.root.join("proj/datalink")).expect("a link");
            symlink("a.txt", tree.root.join("proj/swap")).expect("a link");
        }
        tree
    }

    /// The root of the tree, canonical.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The absolute path of `relative`, as text.
    pub fn path(&self, relative: &str) -> String {
        self.root.join(relative).to_str().expect("a UTF-8 path").to_owned()
    }

    /// Writes a file below the root, making the folders it needs.
    pub fn write(&self, relative: &str, text: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().expect("a parent")).expect("the folders");
        fs::write(path, text).expect("a file");
    }

    /// The text of a file below the root, or `None` when there is none.
    pub fn read(&self, relative: &str) -> Option<String> {
        fs::read_to_string(self.root.join(relative)).ok()
    }

    /// Whether a path below the root exists, not following a link.
    pub fn exists(&self, relative: &str) -> bool {
        fs::symlink_metadata(self.root.join(relative)).is_ok()
    }

    /// The folders the shell passes to the deny policy: data, configuration and socket.
    pub fn protected(&self) -> Vec<PathBuf> {
        ["data", "cfg", "ipc"].iter().map(|folder| self.root.join(folder)).collect()
    }

    /// A digest of the whole tree: for every path its kind, its link target as written and, for a file, the SHA-256 of
    /// its bytes. It never follows a link, so a change anywhere (a new file, a changed byte, a retargeted link) changes it.
    pub fn digest(&self) -> String {
        fn walk(root: &Path, dir: &Path, lines: &mut Vec<String>) {
            let Ok(entries) = fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = fs::symlink_metadata(&path) else { continue };
                let relative = path.strip_prefix(root).expect("below the root").to_string_lossy().into_owned();
                if meta.file_type().is_symlink() {
                    let target = fs::read_link(&path).map(|t| t.to_string_lossy().into_owned()).unwrap_or_default();
                    lines.push(format!("{relative}\tlink\t{target}"));
                } else if meta.is_dir() {
                    lines.push(format!("{relative}\tfolder"));
                    walk(root, &path, lines);
                } else {
                    let bytes = fs::read(&path).unwrap_or_default();
                    lines.push(format!("{relative}\tfile\t{}", hex(&Sha256::digest(&bytes))));
                }
            }
        }
        let mut lines = Vec::new();
        walk(&self.root, &self.root, &mut lines);
        lines.sort();
        hex(&Sha256::digest(lines.join("\n").as_bytes()))
    }
}

// ---- the host -----------------------------------------------------------------------------------------------------

/// One run of the host: the tool, the path it acted on and the working directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRecord {
    pub tool: ToolName,
    pub path: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Default)]
struct HoldState {
    started: bool,
    released: bool,
}

/// The handle of a run that is held open inside the host.
#[derive(Clone)]
pub struct Hold {
    state: Arc<(Mutex<HoldState>, Condvar)>,
}

impl Hold {
    /// Whether the run has started and waits.
    pub fn started(&self) -> bool {
        lock(&self.state.0).started
    }

    /// Waits up to `timeout` for the run to start.
    pub fn wait_started(&self, timeout: Duration) -> bool {
        crate::common::wait_until(timeout, || self.started())
    }

    /// Lets the held run go on.
    pub fn release(&self) {
        lock(&self.state.0).released = true;
        self.state.1.notify_all();
    }
}

#[derive(Default)]
struct HostState {
    runs: Vec<RunRecord>,
    resolves: HashMap<String, usize>,
    retargets: Vec<(String, String)>,
    fail_next: bool,
    hold: Option<Hold>,
}

/// The stand-in tool host over a scratch tree. A comment of the tests says what it is: it mirrors the contract of the
/// shell's host (it opens only the resolved path, looks at files through `inspect`, writes only when the stamp still
/// holds, runs commands through the real runner) and proves nothing about the shell's own code.
pub struct GateHost {
    proj: PathBuf,
    panes: Vec<PaneId>,
    exec: Option<LocalExec>,
    state: Mutex<HostState>,
}

impl GateHost {
    /// A host for the project folder of `tree` that offers its tools for these panes (every tool but `read_pane`).
    pub fn new(tree: &Tree, panes: &[PaneId]) -> GateHost {
        GateHost {
            proj: tree.root.join("proj"),
            panes: panes.to_vec(),
            exec: None,
            state: Mutex::new(HostState::default()),
        }
    }

    /// The same host with a runner for `run_command`.
    #[must_use]
    pub fn with_exec(mut self, exec: LocalExec) -> GateHost {
        self.exec = Some(exec);
        self
    }

    /// Every run, in order.
    pub fn runs(&self) -> Vec<RunRecord> {
        lock(&self.state).runs.clone()
    }

    /// How many times the host ran a call.
    pub fn run_count(&self) -> usize {
        lock(&self.state).runs.len()
    }

    /// The path `from` (as the model wrote it) resolves to `to` from its second resolution on: a swap between the card
    /// and the run.
    pub fn retarget_on_second_resolve(&self, from: &str, to: &str) {
        lock(&self.state).retargets.push((from.to_owned(), to.to_owned()));
    }

    /// The next run fails with a host error.
    pub fn fail_next_run(&self) {
        lock(&self.state).fail_next = true;
    }

    /// The next run waits inside the host until the handle releases it, at most ten seconds.
    pub fn hold_next_run(&self) -> Hold {
        let hold = Hold { state: Arc::new((Mutex::new(HoldState::default()), Condvar::new())) };
        lock(&self.state).hold = Some(hold.clone());
        hold
    }

    fn absolute(&self, path: &str) -> String {
        if Path::new(path).is_absolute() {
            path.to_owned()
        } else {
            self.proj.join(path).to_str().expect("a UTF-8 path").to_owned()
        }
    }

    /// What the model's spelling `text` resolves to now: counts the call and applies a retarget on the second one.
    fn counted(&self, text: &str) -> String {
        let mut state = lock(&self.state);
        let count = state.resolves.entry(text.to_owned()).or_insert(0);
        *count += 1;
        if *count >= 2 {
            if let Some((_, to)) = state.retargets.iter().find(|(from, _)| from == text) {
                return to.clone();
            }
        }
        text.to_owned()
    }

    fn change_time(path: &Path) -> Option<i64> {
        let modified = fs::metadata(path).ok()?.modified().ok()?;
        i64::try_from(modified.duration_since(UNIX_EPOCH).ok()?.as_nanos()).ok()
    }
}

fn host_error(text: &str) -> ToolError {
    ToolError::Host(Detail::new(text))
}

impl ToolHost for GateHost {
    fn available(&self, tool: ToolName, pane: PaneId) -> bool {
        tool != ToolName::ReadPane && self.panes.contains(&pane)
    }

    fn resolve(&self, scope: &Scope, tool: &Tool) -> Result<Resolved, ToolError> {
        let pane = tool.pane().unwrap_or(scope.pane);
        let mut resolved = Resolved::local(pane, "this computer");
        if let Some(path) = tool.path() {
            let spelled = self.absolute(path);
            resolved = resolved.with_path(self.counted(&spelled));
        }
        if let Tool::RunCommand(call) = tool {
            let spelled = match &call.cwd {
                Some(cwd) => self.absolute(cwd),
                None => self.proj.to_str().expect("a UTF-8 path").to_owned(),
            };
            resolved = resolved.with_cwd(self.counted(&spelled));
        }
        Ok(resolved)
    }

    fn inspect(&self, resolved: &Resolved) -> Result<Option<Inspected>, ToolError> {
        let Some(path) = resolved.path() else { return Ok(None) };
        let path = Path::new(path);
        match fs::read_to_string(path) {
            Ok(text) => Ok(Some(Inspected::new(Base::Text(text), Self::change_time(path)))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Some(Inspected::new(Base::Absent, None))),
            Err(_) => Err(host_error("the file cannot be read")),
        }
    }

    fn run(&self, ticket: &Ticket, cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        let resolved = ticket.resolved();
        let record = RunRecord {
            tool: ticket.tool().name(),
            path: resolved.path().map(str::to_owned),
            cwd: resolved.cwd().map(str::to_owned),
        };
        let (fail, hold) = {
            let mut state = lock(&self.state);
            state.runs.push(record);
            (std::mem::take(&mut state.fail_next), state.hold.take())
        };
        if let Some(hold) = hold {
            let (mutex, wake) = &*hold.state;
            let mut state = lock(mutex);
            state.started = true;
            let deadline = Instant::now() + Duration::from_secs(10);
            while !state.released && !cancel.is_cancelled() && Instant::now() < deadline {
                state = wake.wait_timeout(state, Duration::from_millis(20)).unwrap_or_else(PoisonError::into_inner).0;
            }
        }
        if fail {
            return Err(host_error("the host could not do it"));
        }
        let path = resolved.path().map(PathBuf::from);
        match ticket.tool() {
            Tool::ReadFile(call) => {
                let text = fs::read_to_string(path.ok_or_else(|| host_error("no path"))?)
                    .map_err(|_| host_error("the file cannot be read"))?;
                let start = usize::try_from(call.offset()).unwrap_or(usize::MAX).min(text.len());
                let end = start.saturating_add(usize::try_from(call.limit()).unwrap_or(usize::MAX)).min(text.len());
                Ok(ToolOutput::new(String::from_utf8_lossy(&text.as_bytes()[start..end]).into_owned()))
            }
            Tool::ListDir(_) => {
                let entries = fs::read_dir(path.ok_or_else(|| host_error("no path"))?)
                    .map_err(|_| host_error("the folder cannot be read"))?;
                let mut names: Vec<String> =
                    entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
                names.sort();
                Ok(ToolOutput::new(names.join("\n")))
            }
            Tool::Grep(call) => {
                let text = fs::read_to_string(path.ok_or_else(|| host_error("no path"))?)
                    .map_err(|_| host_error("the file cannot be read"))?;
                let found: Vec<&str> = text.lines().filter(|line| line.contains(call.pattern.as_str())).collect();
                Ok(ToolOutput::new(found.join("\n")))
            }
            Tool::WriteFile(call) => {
                let path = path.ok_or_else(|| host_error("no path"))?;
                self.check_stamp(&path, resolved.stamp())?;
                fs::write(&path, &call.content).map_err(|_| host_error("the file cannot be written"))?;
                Ok(ToolOutput::new("written".to_owned()))
            }
            Tool::EditFile(_) => {
                let path = path.ok_or_else(|| host_error("no path"))?;
                self.check_stamp(&path, resolved.stamp())?;
                let base = match fs::read_to_string(&path) {
                    Ok(text) => Base::Text(text),
                    Err(_) => Base::Absent,
                };
                let change = plan(ticket.tool(), &base).map_err(|_| host_error("the edit does not apply"))?;
                fs::write(&path, change.after).map_err(|_| host_error("the file cannot be written"))?;
                Ok(ToolOutput::new("edited".to_owned()))
            }
            Tool::RunCommand(call) => {
                let exec = self.exec.as_ref().ok_or_else(|| host_error("no runner"))?;
                let cwd = PathBuf::from(resolved.cwd().ok_or_else(|| host_error("no folder"))?);
                let limits = ExecLimits { max_output: 65536, timeout: Duration::from_secs(call.timeout_secs()) };
                let stop = ExecCancel::new();
                let done = AtomicBool::new(false);
                let result = thread::scope(|scope| {
                    scope.spawn(|| {
                        while !done.load(Ordering::SeqCst) {
                            if cancel.is_cancelled() {
                                stop.cancel();
                                break;
                            }
                            thread::sleep(Duration::from_millis(10));
                        }
                    });
                    let result = exec.exec(&call.command, &cwd, &limits, &stop);
                    done.store(true, Ordering::SeqCst);
                    result
                });
                if cancel.is_cancelled() {
                    return Err(ToolError::Cancelled);
                }
                let result = result.map_err(|_| host_error("the command could not be started"))?;
                Ok(report(&result, call.timeout_secs()))
            }
            Tool::ReadPane(_) => Err(ToolError::Unavailable),
        }
    }
}

impl GateHost {
    /// A write is refused when the file changed after the card: its change time differs from the stamp of the card, or
    /// the file exists although the card was for a file that did not.
    fn check_stamp(&self, path: &Path, stamp: Option<i64>) -> Result<(), ToolError> {
        match (stamp, Self::change_time(path)) {
            (Some(card), Some(now)) if card == now => Ok(()),
            (None, None) => Ok(()),
            _ => Err(host_error("the file changed since the card was shown")),
        }
    }
}

// ---- the world ----------------------------------------------------------------------------------------------------

/// What a world is built from. Every field has a sensible default.
pub struct WorldOptions {
    /// The scripts the hostile model plays, one per provider request, in order.
    pub turns: Vec<Script>,
    /// The limits of a turn (the approval deadline is set small by the tests that wait for it).
    pub limits: Limits,
    /// The panes the host offers its tools for.
    pub panes: Vec<PaneId>,
    /// The runner of `run_command`, when the world runs commands.
    pub exec: Option<LocalExec>,
}

impl Default for WorldOptions {
    fn default() -> Self {
        WorldOptions { turns: Vec::new(), limits: Limits::default(), panes: vec![PANE_A, PANE_B], exec: None }
    }
}

/// A service with a scripted hostile model over a scratch tree: the model asks for what the script says and the test
/// plays the page.
pub struct World {
    scratch: Scratch,
    tree: Tree,
    host: Arc<GateHost>,
    sink: Arc<CollectSink>,
    service: Arc<AiService>,
    provider: Arc<ScriptedProvider>,
}

impl World {
    /// Builds the tree, the host, the service and the scripted provider `scripted`.
    pub fn new(options: WorldOptions) -> World {
        let scratch = Scratch::new("world");
        let tree = Tree::new(scratch.path());
        let mut host = GateHost::new(&tree, &options.panes);
        if let Some(exec) = options.exec {
            host = host.with_exec(exec);
        }
        let host = Arc::new(host);
        let sink = Arc::new(CollectSink::default());
        let config = scratch.path().join("config");
        std::fs::create_dir(&config).expect("a configuration folder");
        let service = AiService::builder(Arc::new(MemoryStore::new()), sink.clone() as Arc<dyn AiSink>, config)
            .tool_host(host.clone())
            .deny_policy(DenyPolicy::new(tree.protected()))
            .limits(options.limits)
            .build();
        let mut provider = ScriptedProvider::new("scripted");
        for turn in options.turns {
            provider = provider.with_turn(turn);
        }
        let provider = Arc::new(provider);
        service.register_provider("scripted", provider.clone()).expect("the provider is registered");
        World { scratch, tree, host, sink, service: Arc::new(service), provider }
    }

    /// The tree.
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// The scratch folder of the world.
    pub fn scratch(&self) -> &Path {
        self.scratch.path()
    }

    /// The host.
    pub fn host(&self) -> &Arc<GateHost> {
        &self.host
    }

    /// The service.
    pub fn service(&self) -> &Arc<AiService> {
        &self.service
    }

    /// The scripted model.
    pub fn provider(&self) -> &Arc<ScriptedProvider> {
        &self.provider
    }

    /// The sink of the events.
    pub fn sink(&self) -> &Arc<CollectSink> {
        &self.sink
    }

    /// A fresh chat.
    pub fn chat(&self) -> ConversationId {
        self.service.new_conversation().expect("a chat id")
    }

    /// Sends `text` to the scripted model for pane A.
    pub fn ask(&self, chat: &ConversationId, text: &str) {
        self.ask_with_context(chat, text, Vec::new());
    }

    /// Sends `text` with the approved contexts attached.
    pub fn ask_with_context(&self, chat: &ConversationId, text: &str, approved: Vec<ApprovedContext>) {
        let request = SendRequest::new(chat.clone(), text.to_owned(), PANE_A).profile("scripted").context(approved);
        self.service.send(request).expect("the send is accepted");
    }

    /// The cards of a chat so far, as JSON (`pending` of each `tool_proposed` event).
    pub fn cards(&self, chat: &ConversationId) -> Vec<Value> {
        self.sink
            .events(chat.as_str())
            .into_iter()
            .filter(|event| event["kind"] == "tool_proposed")
            .map(|event| event["pending"].clone())
            .collect()
    }

    /// The `n`-th card (counting from 1), waiting up to `timeout` for it.
    pub fn card(&self, chat: &ConversationId, n: usize, timeout: Duration) -> Option<Value> {
        wait_until(timeout, || self.cards(chat).len() >= n);
        self.cards(chat).into_iter().nth(n - 1)
    }

    /// The results of the calls of a chat, as JSON (the `tool_result` events).
    pub fn results(&self, chat: &ConversationId) -> Vec<Value> {
        self.sink.events(chat.as_str()).into_iter().filter(|event| event["kind"] == "tool_result").collect()
    }

    /// The decision of the page on a card, through the table the shell's decision command uses.
    pub fn decide(&self, chat: &ConversationId, card: &Value, decision: Decision, hash: &str) -> Result<(), ToolError> {
        let call_id = card["call_id"].as_str().unwrap_or("");
        self.service.approvals().decide(chat.as_str(), call_id, decision, hash)
    }

    /// Allows a card with its own hash.
    pub fn allow(&self, chat: &ConversationId, card: &Value) -> Result<(), ToolError> {
        let hash = card["hash"].as_str().unwrap_or("").to_owned();
        self.decide(chat, card, Decision::Allow, &hash)
    }

    /// The end of the last turn of a chat, waiting up to `timeout` for it.
    pub fn wait_end(&self, chat: &ConversationId, timeout: Duration) -> Option<Value> {
        self.sink.wait_ended(chat.as_str(), timeout)
    }

    /// Every run of the host.
    pub fn runs(&self) -> Vec<RunRecord> {
        self.host.runs()
    }

    /// The digest of the tree now.
    pub fn digest(&self) -> String {
        self.tree.digest()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // The turns are told to stop before the tree goes.
        self.service.shutdown(Duration::from_secs(2));
    }
}
