//! The command half of the shell's tool host: where the command tool is offered, what a call acts on, and the run itself.
//!
//! This module starts a process only through the local runner of the core, writes nothing to any pane and holds no card,
//! hash or ticket constructor: the page decides, the table checks, and the host runs exactly what the ticket names, the
//! command and the working directory the card showed. The words the model hears about the outcome are made by one pure
//! function of the core, and no fixed phrase of a refusal echoes the command or the folder.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use holzwave_core::ai::provider::{CancelToken, Detail};
use holzwave_core::ai::tools::{
    Access, CAPTURE_BYTES, CWD_MISSING, CWD_REMOTE_RELATIVE, CWD_UNKNOWN, FsKind, REMOTE_FAILED, REMOTE_TOO_LONG, Resolved,
    RunCommand, Scope, START_FAILED, Ticket, Tool, ToolError, ToolOutput, remote_command, report,
};
use holzwave_core::layout::PaneId;
use holzwave_core::ssh::{ExecCancel, ExecEnd, ExecLimits};

use crate::ai_tools::{Ending, ExecFault, ShellTools, Targets, absolute_path, host, live};

/// How often the watcher of a run looks at the cancel token of the turn.
const WATCH: Duration = Duration::from_millis(25);

/// Whether the host offers the command tool on `pane`: the pane is open and has both a terminal and a file target, which
/// is a terminal on this computer or on a saved SSH connection and no other kind of pane (not a WSL or Docker pane, not a
/// Files or Preview block, not a closed pane).
pub(crate) fn offers(targets: &dyn Targets, pane: PaneId) -> bool {
    if !targets.is_open(pane) {
        return false;
    }
    targets.terminal(pane).is_some() && targets.file_target(pane).is_some()
}

/// What a command acts on: the pane, the machine, and the working directory it will run in.
///
/// On a saved connection the working directory is the one the call gave, which must be an absolute path, or none (the
/// account's home); it is never looked at here, only checked for the length of the string that would be sent, so a command
/// that cannot be sent is refused before any card exists.
///
/// On this computer the working directory is the one the call gave (made absolute by the rules of the file tools), else
/// the last one the pane reported, else the home folder; it is judged by the deny list first and only then looked at, so a
/// protected place is refused as protected whether it exists or not, and it must be a folder.
pub(crate) fn resolve(tools: &ShellTools, scope: &Scope, args: &RunCommand) -> Result<Resolved, ToolError> {
    let world = tools.world()?;
    let pane = args.pane.unwrap_or(scope.pane);
    if !offers(world.as_ref(), pane) {
        return Err(ToolError::Unavailable);
    }
    let target = world.file_target(pane).ok_or(ToolError::Unavailable)?;
    if target.remote {
        let cwd = match args.cwd.as_deref() {
            Some(raw) if raw.starts_with('/') => Some(raw),
            Some(_) => return Err(host(CWD_REMOTE_RELATIVE)),
            None => None,
        };
        if remote_command(&args.command, cwd).is_none() {
            return Err(host(REMOTE_TOO_LONG));
        }
        let record = Resolved::remote(pane, target.label);
        return Ok(match cwd {
            Some(cwd) => record.with_cwd(cwd),
            None => record,
        });
    }
    let chosen = match args.cwd.as_deref() {
        Some(raw) => absolute_path(raw, &target, Ending::Folder)?,
        None => target.cwd.clone().or_else(|| target.home.clone()).ok_or_else(|| host(CWD_UNKNOWN))?,
    };
    let canonical = tools
        .policy()
        .check_local(Path::new(&chosen), Access::Read)
        .map_err(|denied| ToolError::Denied(denied.rule()))?;
    if !canonical.is_dir() {
        return Err(host(CWD_MISSING));
    }
    let text = canonical.into_os_string().into_string().map_err(|_| host(CWD_MISSING))?;
    Ok(Resolved::local(pane, target.label).with_cwd(text))
}

/// Where an approved command runs: a folder of this computer, or the string for a saved connection.
enum Route<'a> {
    Local { cwd: &'a str },
    Remote { source: String, command: String },
}

/// Runs the approved command and reports how it ended.
///
/// Looks at the cancel token first. The ticket's record must still describe what the pane is now (a pane that changed
/// kind is a changed payload). One switch is made for the run and raised by a watcher thread from the turn's token, so a
/// stop reaches the process or the remote call. The limits are the capture size and the time limit of the card; the text
/// the model hears is made by [`report`] from the result of either route, and a stop is [`ToolError::Cancelled`].
pub(crate) fn run(tools: &ShellTools, ticket: &Ticket, cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
    live(cancel)?;
    let Tool::RunCommand(args) = ticket.tool() else { return Err(ToolError::Unavailable) };
    let world = tools.world()?;
    let record = ticket.resolved();
    let target = world.file_target(record.pane()).ok_or(ToolError::PayloadChanged)?;
    if target.remote != (record.fs() == FsKind::Remote) {
        return Err(ToolError::PayloadChanged);
    }
    let timeout_secs = args.timeout_secs();
    let limits = ExecLimits { max_output: CAPTURE_BYTES, timeout: Duration::from_secs(timeout_secs) };
    // The string and the folder are made before anything starts, from the ticket alone.
    let route = if target.remote {
        let command = remote_command(&args.command, record.cwd()).ok_or_else(|| host(REMOTE_TOO_LONG))?;
        Route::Remote { source: target.source, command }
    } else {
        Route::Local { cwd: record.cwd().ok_or(ToolError::PayloadChanged)? }
    };
    let stop = ExecCancel::new();
    let done = AtomicBool::new(false);
    let ended = thread::scope(|scope| {
        let watcher = thread::Builder::new().name("hw-exec-watch".to_owned()).spawn_scoped(scope, || {
            while !done.load(Ordering::SeqCst) {
                if cancel.wait_cancelled(WATCH) {
                    stop.cancel();
                    break;
                }
            }
        });
        if watcher.is_err() {
            return Err(host(START_FAILED));
        }
        let ran = match &route {
            Route::Local { cwd } => tools
                .local_exec()
                .exec(&args.command, Path::new(cwd), &limits, &stop)
                .map_err(|_| host(if Path::new(cwd).is_dir() { START_FAILED } else { CWD_MISSING })),
            Route::Remote { source, command } => {
                world.run_remote(source, command, &limits, &stop).map_err(|fault| match fault {
                    ExecFault::NoRunner => ToolError::Unavailable,
                    ExecFault::Start => host(START_FAILED),
                    ExecFault::Remote(why) => ToolError::Host(Detail::new(&format!("{REMOTE_FAILED}: {why}"))),
                })
            }
        };
        done.store(true, Ordering::SeqCst);
        ran
    })?;
    if ended.end == ExecEnd::Cancelled {
        return Err(ToolError::Cancelled);
    }
    Ok(report(&ended, timeout_secs))
}

/// The most exec primitives kept at once.
const MAX_REMOTE_EXECS: usize = 8;

/// A pool of the exec primitives of saved connections, keyed by the connection and the version of the connection list. At
/// most [`MAX_REMOTE_EXECS`] are kept and the oldest goes first. The lock is never held while a primitive is built, because
/// building reads the keychain and starts a runtime, nor while one runs: a caller takes an `Arc` and lets the lock go.
pub(crate) struct RemoteExecs<T> {
    entries: Mutex<Vec<(String, u64, Arc<T>)>>,
}

impl<T> RemoteExecs<T> {
    /// An empty pool.
    pub(crate) fn new() -> Self {
        Self { entries: Mutex::new(Vec::new()) }
    }

    /// How many primitives the pool holds.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// The primitive of connection `id` for `generation`, built by `build` when there is none. A builder that fails stores
    /// nothing. When two callers build at the same time, the first to finish is kept and the other one is dropped.
    pub(crate) fn get_or_build(
        &self,
        id: &str,
        generation: u64,
        build: impl FnOnce() -> Result<Arc<T>, String>,
    ) -> Result<Arc<T>, String> {
        let find = |entries: &Vec<(String, u64, Arc<T>)>| {
            entries.iter().find(|(name, made, _)| name == id && *made == generation).map(|(_, _, found)| Arc::clone(found))
        };
        if let Some(found) = find(&self.entries.lock().unwrap_or_else(PoisonError::into_inner)) {
            return Ok(found);
        }
        let built = build()?;
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = find(&entries) {
            return Ok(found);
        }
        // An entry of an older version of the connection list is replaced; the oldest goes when the pool is full.
        entries.retain(|(name, _, _)| name != id);
        entries.push((id.to_owned(), generation, Arc::clone(&built)));
        while entries.len() > MAX_REMOTE_EXECS {
            entries.remove(0);
        }
        Ok(built)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    use holzwave_core::ai::tools::{
        ApprovalTable, DenyPolicy, START_FAILED as START, ToolHost, ToolName, specs,
    };
    use holzwave_core::exec::LocalExec;
    use holzwave_core::ssh::ExecResult;
    use serde_json::{Value, json};

    use crate::ai_tools::rig::{run_call, run_call_between, run_call_with};
    use crate::ai_tools::{FileTarget, RowsFault, TerminalTarget};

    /// Text a command prints, a folder name and a command word; they appear in test code only, so a hit anywhere else is a
    /// leak.
    const OUT_CANARY: &str = "zq-cmd-out-6a1f40";
    const DIR_CANARY: &str = "zq-cmd-dir-8e55d1";
    const CMD_CANARY: &str = "zq-cmd-word-3b7d90";

    /// A scratch folder with its canonical spelling, removed at the end of the test.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Tmp {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("holzwave-exec-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch dir");
            Tmp(path.canonicalize().expect("canonical"))
        }

        fn text(&self) -> String {
            self.0.to_str().expect("utf-8").to_owned()
        }

        fn path(&self, name: &str) -> String {
            self.0.join(name).to_str().expect("utf-8").to_owned()
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// One call of `run_remote`.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct RemoteCall {
        source: String,
        command: String,
        max_output: usize,
        timeout: Duration,
    }

    type Answer = Box<dyn Fn(&ExecCancel) -> Result<ExecResult, ExecFault> + Send + Sync>;

    /// The panes of a rig: which are open and what their files and terminals are, and what a remote run answers.
    #[derive(Default)]
    struct Stage {
        open: Mutex<HashSet<PaneId>>,
        files: Mutex<BTreeMap<PaneId, FileTarget>>,
        terminals: Mutex<BTreeMap<PaneId, TerminalTarget>>,
        calls: Mutex<Vec<RemoteCall>>,
        answer: Mutex<Option<Answer>>,
    }

    use std::collections::BTreeMap;

    impl Stage {
        fn pane(self, pane: PaneId, files: Option<FileTarget>, terminal: Option<TerminalTarget>) -> Stage {
            self.set(pane, files, terminal);
            self
        }

        fn set(&self, pane: PaneId, files: Option<FileTarget>, terminal: Option<TerminalTarget>) {
            self.open.lock().unwrap_or_else(PoisonError::into_inner).insert(pane);
            let mut all = self.files.lock().unwrap_or_else(PoisonError::into_inner);
            match files {
                Some(files) => all.insert(pane, files),
                None => all.remove(&pane),
            };
            let mut all = self.terminals.lock().unwrap_or_else(PoisonError::into_inner);
            match terminal {
                Some(terminal) => all.insert(pane, terminal),
                None => all.remove(&pane),
            };
        }

        /// From now on a remote run answers by calling `answer`.
        fn answering(self, answer: impl Fn(&ExecCancel) -> Result<ExecResult, ExecFault> + Send + Sync + 'static) -> Stage {
            *self.answer.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(answer));
            self
        }

        fn calls(&self) -> Vec<RemoteCall> {
            self.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
        }
    }

    impl Targets for Stage {
        fn is_open(&self, pane: PaneId) -> bool {
            self.open.lock().unwrap_or_else(PoisonError::into_inner).contains(&pane)
        }

        fn file_target(&self, pane: PaneId) -> Option<FileTarget> {
            self.files.lock().unwrap_or_else(PoisonError::into_inner).get(&pane).cloned()
        }

        fn file_source(&self, _pane: PaneId) -> Option<Arc<dyn holzwave_core::files::FileSource>> {
            None
        }

        fn terminal(&self, pane: PaneId) -> Option<TerminalTarget> {
            self.terminals.lock().unwrap_or_else(PoisonError::into_inner).get(&pane).cloned()
        }

        fn read_rows(
            &self,
            _pane: PaneId,
            _rows: u16,
            _max: usize,
        ) -> Result<Option<holzwave_core::textread::TextRead>, RowsFault> {
            Ok(None)
        }

        fn shared(&self, _conversation: &str, _pane: PaneId) -> bool {
            false
        }

        fn run_remote(
            &self,
            source: &str,
            command: &str,
            limits: &ExecLimits,
            cancel: &ExecCancel,
        ) -> Result<ExecResult, ExecFault> {
            self.calls.lock().unwrap_or_else(PoisonError::into_inner).push(RemoteCall {
                source: source.to_owned(),
                command: command.to_owned(),
                max_output: limits.max_output,
                timeout: limits.timeout,
            });
            match self.answer.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
                Some(answer) => answer(cancel),
                None => Ok(finished("", "", Some(0), ExecEnd::Finished)),
            }
        }
    }

    fn finished(stdout: &str, stderr: &str, status: Option<u32>, end: ExecEnd) -> ExecResult {
        ExecResult {
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            status,
            end,
            truncated: false,
            dropped: 0,
        }
    }

    fn local_files(cwd: Option<&str>, home: Option<&str>) -> FileTarget {
        FileTarget {
            source: "local".to_owned(),
            label: "This computer".to_owned(),
            remote: false,
            home: home.map(str::to_owned),
            cwd: cwd.map(str::to_owned),
        }
    }

    fn box_files(cwd: Option<&str>) -> FileTarget {
        FileTarget {
            source: "box-id".to_owned(),
            label: "build box".to_owned(),
            remote: true,
            home: None,
            cwd: cwd.map(str::to_owned),
        }
    }

    fn here() -> TerminalTarget {
        TerminalTarget { label: "This computer".to_owned(), remote: false }
    }

    fn there() -> TerminalTarget {
        TerminalTarget { label: "build box".to_owned(), remote: true }
    }

    fn scope(pane: PaneId) -> Scope {
        Scope { conversation: "chat-1".to_owned(), turn: 1, pane }
    }

    /// A runner that sees only the `PATH` of this process.
    fn runner() -> LocalExec {
        let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin"));
        LocalExec::new([(OsString::from("PATH"), path)])
    }

    fn host_over(stage: Stage) -> (ShellTools, Arc<Stage>) {
        let stage = Arc::new(stage);
        (ShellTools::with_targets(DenyPolicy::new(Vec::new()), stage.clone()).with_local_exec(runner()), stage)
    }

    fn table() -> ApprovalTable {
        ApprovalTable::new(DenyPolicy::new(Vec::new()))
    }

    /// What `resolve` makes of a `run_command` call with `args`.
    fn resolve_of(host: &ShellTools, args: Value) -> Result<Resolved, ToolError> {
        let tool = Tool::parse("run_command", &args).expect("arguments");
        host.resolve(&scope(1), &tool)
    }

    #[test]
    fn the_host_offers_the_command_tool_where_a_terminal_and_a_file_target_exist() {
        let unbound = ShellTools::new(DenyPolicy::new(Vec::new()));
        assert!(!unbound.available(ToolName::RunCommand, 1), "an unbound host offers nothing");

        let stage = Stage::default()
            .pane(1, Some(local_files(None, None)), Some(here()))
            .pane(3, None, Some(here()))
            .pane(4, Some(local_files(None, None)), None)
            .pane(5, Some(box_files(None)), Some(there()))
            .pane(6, None, Some(there()))
            .pane(7, Some(box_files(None)), None);
        let host = ShellTools::with_targets(DenyPolicy::new(Vec::new()), Arc::new(stage));
        assert!(host.available(ToolName::RunCommand, 1), "a terminal on this computer");
        assert!(host.available(ToolName::RunCommand, 5), "a terminal on a saved SSH connection, with its files");
        assert!(!host.available(ToolName::RunCommand, 3), "a terminal that has no file target");
        assert!(!host.available(ToolName::RunCommand, 6), "a WSL or Docker pane: a terminal on a connection and no files");
        assert!(!host.available(ToolName::RunCommand, 4) && !host.available(ToolName::RunCommand, 7), "a block");
        assert!(!host.available(ToolName::RunCommand, 2), "a pane that is not open");
        let declared: Vec<String> = specs(&host, 1).into_iter().map(|spec| spec.name).collect();
        assert_eq!(declared, ["read_file", "list_dir", "grep", "read_pane", "write_file", "edit_file", "run_command"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_local_command_runs_through_the_runner_and_the_report_is_the_cores_text() {
        let tmp = Tmp::new("report");
        let stage = Stage::default().pane(1, Some(local_files(Some(&tmp.text()), Some(&tmp.text()))), Some(here()));
        let (host, stage) = host_over(stage);
        let table = table();
        let scope = scope(1);
        let cwd = tmp.text();
        let go = |command: &str| {
            run_call(&table, &host, &scope, "run_command", json!({"command": command, "cwd": cwd})).expect("the call ran")
        };

        let printed = go(&format!("printf '{OUT_CANARY}\\n'"));
        assert_eq!(printed.text(), format!("Exit status: 0.\n\nStandard output:\n{OUT_CANARY}"));
        assert!(!printed.is_failure());

        let failed = go("false");
        assert_eq!(failed.text(), "Exit status: 1.\n\nNo output.");
        assert!(failed.is_failure());

        let place = go("pwd -P");
        assert_eq!(place.text(), format!("Exit status: 0.\n\nStandard output:\n{}", tmp.text()));
        assert!(stage.calls().is_empty(), "a local command never reaches the remote route");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_picks_the_working_directory_by_the_read_tool_rules() {
        let tmp = Tmp::new("resolve");
        for dir in ["proj", "proj/sub", "home/work", "other"] {
            std::fs::create_dir_all(tmp.0.join(dir)).expect("a folder");
        }
        std::fs::write(tmp.0.join("file.txt"), b"x").expect("a file");
        std::os::unix::fs::symlink(tmp.0.join("proj/sub"), tmp.0.join("link")).expect("a link");
        let proj = tmp.path("proj");
        let home = tmp.path("home");
        let stage = Stage::default()
            .pane(1, Some(local_files(Some(&proj), Some(&home))), Some(here()))
            .pane(2, Some(local_files(None, None)), Some(here()))
            .pane(3, Some(local_files(Some(&tmp.path("other")), Some(&home))), Some(here()))
            .pane(5, Some(box_files(Some("/srv/app"))), Some(there()))
            .pane(6, Some(box_files(None)), Some(there()));
        let (host, _) = host_over(stage);
        let cwd_of = |args: Value| resolve_of(&host, args).map(|record| record.cwd().map(str::to_owned));
        let host_error = |phrase: &str| Err(ToolError::Host(Detail::new(phrase)));

        // This computer: an absolute folder is canonicalised, a relative one is read against the pane's folder, `~/` is the
        // home, and a link to a folder is the folder.
        assert_eq!(cwd_of(json!({"command": "ls", "cwd": proj})), Ok(Some(proj.clone())));
        assert_eq!(cwd_of(json!({"command": "ls", "cwd": "sub"})), Ok(Some(tmp.path("proj/sub"))));
        assert_eq!(cwd_of(json!({"command": "ls", "cwd": "~/work"})), Ok(Some(tmp.path("home/work"))));
        assert_eq!(cwd_of(json!({"command": "ls", "cwd": tmp.path("link")})), Ok(Some(tmp.path("proj/sub"))));
        assert_eq!(cwd_of(json!({"command": "ls", "cwd": format!("{proj}/sub/..")})), Ok(Some(proj.clone())));

        // No folder given: the pane's last one, then the home folder, then a refusal.
        assert_eq!(cwd_of(json!({"command": "ls"})), Ok(Some(proj.clone())));
        assert_eq!(cwd_of(json!({"command": "ls", "pane": 2})), host_error(CWD_UNKNOWN));
        let home_only = Stage::default().pane(1, Some(local_files(None, Some(&home))), Some(here()));
        let (home_host, _) = host_over(home_only);
        assert_eq!(resolve_of(&home_host, json!({"command": "ls"})).map(|r| r.cwd().map(str::to_owned)), Ok(Some(home.clone())));

        // A file and a missing folder are not folders.
        assert_eq!(cwd_of(json!({"command": "ls", "cwd": tmp.path("file.txt")})), host_error(CWD_MISSING));
        assert_eq!(cwd_of(json!({"command": "ls", "cwd": tmp.path(DIR_CANARY)})), host_error(CWD_MISSING));

        // A `pane` argument that names another open terminal pane uses that pane's target.
        let other = resolve_of(&host, json!({"command": "ls", "pane": 3})).expect("a record");
        assert_eq!((other.pane(), other.cwd()), (3, Some(tmp.path("other").as_str())));
        assert_eq!(resolve_of(&host, json!({"command": "ls", "pane": 9})), Err(ToolError::Unavailable));

        // A saved connection: an absolute folder is kept as written, a relative one is refused, none leaves the card without
        // a folder (the account's home), and a string that cannot be sent is refused before any card.
        let record = resolve_of(&host, json!({"command": "ls", "cwd": "/srv/other/", "pane": 5})).expect("a record");
        assert_eq!((record.fs(), record.cwd(), record.target()), (FsKind::Remote, Some("/srv/other/"), "build box"));
        assert_eq!(cwd_of(json!({"command": "ls", "cwd": "app", "pane": 5})), host_error(CWD_REMOTE_RELATIVE));
        assert_eq!(cwd_of(json!({"command": "ls", "cwd": "~/app", "pane": 5})), host_error(CWD_REMOTE_RELATIVE));
        assert_eq!(cwd_of(json!({"command": "ls", "pane": 6})), Ok(None), "the account's home");
        assert_eq!(cwd_of(json!({"command": "ls", "pane": 5})), Ok(None), "the pane's folder is not used on a remote host");
        let long = "'".repeat(4200);
        assert_eq!(cwd_of(json!({"command": long, "pane": 5})), host_error(REMOTE_TOO_LONG));
        assert!(cwd_of(json!({"command": "x".repeat(4200), "pane": 5})).is_ok(), "a plain command that long still fits");
    }

    #[test]
    fn a_remote_command_goes_through_run_remote_with_the_quoted_string() {
        let tmp = Tmp::new("remote");
        let stage = Stage::default()
            .pane(1, Some(local_files(Some(&tmp.text()), None)), Some(here()))
            .pane(5, Some(box_files(Some("/srv/app"))), Some(there()))
            .pane(8, None, Some(there()));
        let (host, stage) = host_over(stage);
        let table = table();
        let command = format!("cd 'a b' && echo \"it's {CMD_CANARY}\"\n\ttouch {}/marker", tmp.text());
        let output = run_call(
            &table,
            &host,
            &scope(5),
            "run_command",
            json!({"command": command, "cwd": "/srv/it's here", "timeout_secs": 120}),
        )
        .expect("the call ran");
        assert_eq!(output.text(), "Exit status: 0.\n\nNo output.");
        let expected = remote_command(&command, Some("/srv/it's here")).expect("short enough");
        assert_eq!(
            stage.calls(),
            [RemoteCall {
                source: "box-id".to_owned(),
                command: expected,
                max_output: CAPTURE_BYTES,
                timeout: Duration::from_secs(120),
            }]
        );
        assert!(!tmp.0.join("marker").exists(), "the local runner was touched");

        // The default time is the card's effective one; a Docker pane is not offered, so its call has no card.
        run_call(&table, &host, &scope(5), "run_command", json!({"command": "ls"})).expect("the call ran");
        assert_eq!(stage.calls()[1].timeout, Duration::from_secs(60));
        assert_eq!(stage.calls()[1].command, "exec sh -c -- 'ls'", "no folder: no `cd`");
        let docker = run_call(&table, &host, &scope(8), "run_command", json!({"command": "ls"}));
        assert_eq!(docker.expect_err("not offered"), ToolError::Unavailable);

        // A pane whose target was this computer when the card was made and is a saved connection now is a changed payload.
        let (host, stage) = host_over(Stage::default().pane(1, Some(local_files(Some(&tmp.text()), None)), Some(here())));
        let swap = stage.clone();
        let changed = run_call_between(
            &table,
            &host,
            &scope(1),
            "run_command",
            json!({"command": "ls", "cwd": tmp.text()}),
            &|| swap.set(1, Some(box_files(Some("/srv/app"))), Some(there())),
        );
        assert_eq!(changed.expect_err("changed"), ToolError::PayloadChanged);
        assert!(stage.calls().is_empty(), "a command ran on the other machine");
        // And the other way round: a remote record on a pane that is this computer now.
        let (host, stage) = host_over(Stage::default().pane(1, Some(box_files(Some("/srv/app"))), Some(there())));
        let swap = stage.clone();
        let changed = run_call_between(&table, &host, &scope(1), "run_command", json!({"command": "ls"}), &|| {
            swap.set(1, Some(local_files(Some("/tmp"), None)), Some(here()));
        });
        assert_eq!(changed.expect_err("changed"), ToolError::PayloadChanged);
    }

    #[test]
    fn a_failed_a_timed_out_and_a_lost_command_are_failures_with_their_output() {
        let answers = [
            finished("one\n", "boom\n", Some(2), ExecEnd::Finished),
            finished("partial", "", None, ExecEnd::TimedOut),
            finished("", "", None, ExecEnd::Lost),
            finished("fine\n", "", Some(0), ExecEnd::Finished),
            finished("late", "", None, ExecEnd::Cancelled),
        ];
        let next = Arc::new(AtomicUsize::new(0));
        let queue = answers.to_vec();
        let counter = next.clone();
        let stage = Stage::default()
            .pane(5, Some(box_files(None)), Some(there()))
            .answering(move |_| Ok(queue[counter.fetch_add(1, Ordering::SeqCst)].clone()));
        let (host, _) = host_over(stage);
        let table = table();
        let go = || run_call(&table, &host, &scope(5), "run_command", json!({"command": "make", "timeout_secs": 30}));

        for (answer, failure) in answers.iter().take(4).zip([true, true, true, false]) {
            let output = go().expect("the call ran");
            assert_eq!(output.text(), report(answer, 30).text());
            assert_eq!(output.is_failure(), failure, "{}", output.text());
        }
        assert!(report(&answers[1], 30).text().starts_with("Stopped: the command ran longer than 30 seconds and was ended."));
        assert_eq!(go().expect_err("a stop"), ToolError::Cancelled);
    }

    #[test]
    fn the_cancel_token_reaches_the_runner() {
        let stage = Stage::default().pane(5, Some(box_files(None)), Some(there())).answering(|stop| {
            let until = Instant::now() + Duration::from_secs(5);
            while !stop.is_cancelled() && Instant::now() < until {
                thread::sleep(Duration::from_millis(5));
            }
            Ok(finished("", "", None, if stop.is_cancelled() { ExecEnd::Cancelled } else { ExecEnd::Finished }))
        });
        let (host, stage) = host_over(stage);
        let table = table();
        let token = CancelToken::new();
        let raise = token.clone();
        let helper = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            raise.cancel();
        });
        let started = Instant::now();
        let result = run_call_with(&table, &host, &scope(5), "run_command", json!({"command": "sleep 60"}), &token);
        assert_eq!(result.expect_err("stopped"), ToolError::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(1), "the stop took {:?}", started.elapsed());
        helper.join().expect("the helper ends");
        assert_eq!(stage.calls().len(), 1, "the runner was called once");

        // A token that is cancelled before the call never reaches the runner.
        let early = CancelToken::new();
        early.cancel();
        let result = run_call_with(&table, &host, &scope(5), "run_command", json!({"command": "sleep 60"}), &early);
        assert_eq!(result.expect_err("stopped"), ToolError::Cancelled);
        assert_eq!(stage.calls().len(), 1, "a stopped call started the runner");
    }

    #[test]
    fn faults_become_fixed_phrases_and_no_debug_output_shows_text() {
        let tmp = Tmp::new("faults");
        let fault = |answer: ExecFault| -> Result<ToolOutput, ToolError> {
            let stage = Stage::default().pane(5, Some(box_files(None)), Some(there())).answering(move |_| Err(answer.clone()));
            let (host, _) = host_over(stage);
            let command = format!("echo {CMD_CANARY} {DIR_CANARY}");
            run_call(&table(), &host, &scope(5), "run_command", json!({"command": command, "cwd": format!("/srv/{DIR_CANARY}")}))
        };
        assert_eq!(fault(ExecFault::Start).expect_err("a fault"), ToolError::Host(Detail::new(START)));
        assert_eq!(fault(ExecFault::NoRunner).expect_err("a fault"), ToolError::Unavailable);
        let why = format!("the host key is not known\u{1b}[31m\u{202e}{}", "x".repeat(600));
        let error = fault(ExecFault::Remote(why)).expect_err("a fault");
        let ToolError::Host(detail) = &error else { panic!("{error:?}") };
        assert!(detail.as_str().starts_with(&format!("{REMOTE_FAILED}: the host key is not known")), "{}", detail.as_str());
        // The core's own rule for free text: 240 characters and an ellipsis, with no control character.
        assert!(detail.as_str().chars().count() <= 241 && !detail.as_str().chars().any(char::is_control));
        for error in [fault(ExecFault::Start), fault(ExecFault::NoRunner), fault(ExecFault::Remote("down".to_owned()))] {
            let shown = format!("{}", error.expect_err("a fault"));
            assert!(!shown.contains(CMD_CANARY) && !shown.contains(DIR_CANARY), "{shown}");
        }
        for fault in [ExecFault::NoRunner, ExecFault::Start, ExecFault::Remote(format!("{CMD_CANARY} down"))] {
            let shown = format!("{fault:?} {fault:#?}");
            assert!(!shown.contains(CMD_CANARY) && !shown.contains("down"), "{shown}");
            assert!(shown.contains("NoRunner") || shown.contains("Start") || shown.contains("Remote"), "{shown}");
        }
        drop(tmp);
    }

    #[test]
    fn the_exec_pool_reuses_rebuilds_on_a_new_generation_and_stays_bounded() {
        let pool: RemoteExecs<u32> = RemoteExecs::new();
        let built = AtomicUsize::new(0);
        let make = |value: u32| {
            built.fetch_add(1, Ordering::SeqCst);
            Ok::<_, String>(Arc::new(value))
        };

        // The same id and generation twice builds once; a new generation builds again and replaces the old entry.
        let first = pool.get_or_build("a", 1, || make(1)).expect("built");
        let again = pool.get_or_build("a", 1, || make(2)).expect("cached");
        assert!(Arc::ptr_eq(&first, &again) && *again == 1);
        assert_eq!(built.load(Ordering::SeqCst), 1);
        let newer = pool.get_or_build("a", 2, || make(3)).expect("rebuilt");
        assert_eq!((*newer, built.load(Ordering::SeqCst), pool.len()), (3, 2, 1));

        // Nine distinct ids leave at most eight, and the oldest is the one that is gone.
        for n in 0..9u32 {
            pool.get_or_build(&format!("id{n}"), 2, || make(n)).expect("built");
        }
        assert_eq!(pool.len(), 8);
        let before = built.load(Ordering::SeqCst);
        pool.get_or_build("id8", 2, || make(0)).expect("the newest is still cached");
        assert_eq!(built.load(Ordering::SeqCst), before);
        pool.get_or_build("id0", 2, || make(0)).expect("the oldest was dropped and is built again");
        assert_eq!(built.load(Ordering::SeqCst), before + 1);

        // A builder that fails stores nothing.
        let size = pool.len();
        assert_eq!(pool.get_or_build("bad", 2, || Err("no keychain".to_owned())).expect_err("failed"), "no keychain");
        assert_eq!(pool.len(), size);

        // A builder that itself asks the pool for another id does not deadlock: the lock is not held while it runs.
        let pool = Arc::new(RemoteExecs::<u32>::new());
        let inner = pool.clone();
        let worker = thread::spawn(move || {
            let outer = pool.get_or_build("outer", 1, || {
                let other = inner.get_or_build("inner", 1, || Ok(Arc::new(1)))?;
                Ok(Arc::new(*other + 1))
            });
            (outer.map(|value| *value), pool.len())
        });
        let until = Instant::now() + Duration::from_secs(2);
        while !worker.is_finished() && Instant::now() < until {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(worker.is_finished(), "the pool deadlocked");
        assert_eq!(worker.join().expect("the worker ends"), (Ok(2), 2));
    }
}
