//! The command half of the shell's tool host: where the command tool is offered, what a call acts on, and the run itself.
//!
//! This module starts a process only through the local runner of the core, writes nothing to any pane and holds no card,
//! hash or ticket constructor: the page decides, the table checks, and the host runs exactly what the ticket names, the
//! command and the working directory the card showed. The words the model hears about the outcome are made by one pure
//! function of the core, and no fixed phrase of a refusal echoes the command or the folder.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use holzwave_core::ai::provider::CancelToken;
use holzwave_core::ai::tools::{
    Access, CAPTURE_BYTES, CWD_MISSING, CWD_UNKNOWN, FsKind, Resolved, RunCommand, Scope, START_FAILED, Ticket, Tool,
    ToolError, ToolOutput, report,
};
use holzwave_core::ssh::{ExecCancel, ExecEnd, ExecLimits};
use holzwave_core::layout::PaneId;

use crate::ai_tools::{Ending, ShellTools, Targets, absolute_path, host, live};

/// How often the watcher of a run looks at the cancel token of the turn.
const WATCH: Duration = Duration::from_millis(25);

/// Whether the host offers the command tool on `pane`: the pane is open and has both a terminal and a file target, which
/// is a terminal on this computer or on a saved SSH connection and no other kind of pane (not a WSL or Docker pane, not a
/// Files or Preview block, not a closed pane).
pub(crate) fn offers(targets: &dyn Targets, pane: PaneId) -> bool {
    if !targets.is_open(pane) {
        return false;
    }
    let (Some(_), Some(files)) = (targets.terminal(pane), targets.file_target(pane)) else { return false };
    !files.remote
}

/// What a command acts on: the pane, the machine, and the working directory it will run in.
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
    if target.remote {
        return Err(ToolError::Unavailable);
    }
    let cwd = record.cwd().ok_or(ToolError::PayloadChanged)?;
    let timeout_secs = args.timeout_secs();
    let limits = ExecLimits { max_output: CAPTURE_BYTES, timeout: Duration::from_secs(timeout_secs) };
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
        let ran = tools.local_exec().exec(&args.command, Path::new(cwd), &limits, &stop);
        done.store(true, Ordering::SeqCst);
        ran.map_err(|_| host(if Path::new(cwd).is_dir() { START_FAILED } else { CWD_MISSING }))
    })?;
    if ended.end == ExecEnd::Cancelled {
        return Err(ToolError::Cancelled);
    }
    Ok(report(&ended, timeout_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::{BTreeMap, HashSet};
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use holzwave_core::ai::tools::{ApprovalTable, DenyPolicy, ToolHost, ToolName, specs};
    use holzwave_core::exec::LocalExec;
    use serde_json::json;

    use crate::ai_tools::rig::run_call;
    use crate::ai_tools::{FileTarget, RowsFault, TerminalTarget};

    /// Text a command prints; it appears in test code only, so a hit anywhere else is a leak.
    const OUT_CANARY: &str = "zq-cmd-out-6a1f40";

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
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The panes of a rig: which are open and what their files and terminals are.
    #[derive(Default)]
    struct Stage {
        open: HashSet<PaneId>,
        files: BTreeMap<PaneId, FileTarget>,
        terminals: BTreeMap<PaneId, TerminalTarget>,
    }

    impl Stage {
        fn pane(mut self, pane: PaneId, files: Option<FileTarget>, terminal: Option<TerminalTarget>) -> Stage {
            self.open.insert(pane);
            if let Some(files) = files {
                self.files.insert(pane, files);
            }
            if let Some(terminal) = terminal {
                self.terminals.insert(pane, terminal);
            }
            self
        }
    }

    impl Targets for Stage {
        fn is_open(&self, pane: PaneId) -> bool {
            self.open.contains(&pane)
        }

        fn file_target(&self, pane: PaneId) -> Option<FileTarget> {
            self.files.get(&pane).cloned()
        }

        fn file_source(&self, _pane: PaneId) -> Option<Arc<dyn holzwave_core::files::FileSource>> {
            None
        }

        fn terminal(&self, pane: PaneId) -> Option<TerminalTarget> {
            self.terminals.get(&pane).cloned()
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

    fn here() -> TerminalTarget {
        TerminalTarget { label: "This computer".to_owned(), remote: false }
    }

    fn scope(pane: PaneId) -> Scope {
        Scope { conversation: "chat-1".to_owned(), turn: 1, pane }
    }

    /// A runner that sees only the `PATH` of this process.
    fn runner() -> LocalExec {
        let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin"));
        LocalExec::new([(OsString::from("PATH"), path)])
    }

    #[test]
    fn the_host_offers_the_command_tool_where_a_terminal_and_a_file_target_exist() {
        let unbound = ShellTools::new(DenyPolicy::new(Vec::new()));
        assert!(!unbound.available(ToolName::RunCommand, 1), "an unbound host offers nothing");

        let stage = Stage::default()
            .pane(1, Some(local_files(None, None)), Some(here()))
            .pane(3, None, Some(here()))
            .pane(4, Some(local_files(None, None)), None);
        let host = ShellTools::with_targets(DenyPolicy::new(Vec::new()), Arc::new(stage));
        assert!(host.available(ToolName::RunCommand, 1), "a terminal on this computer");
        assert!(!host.available(ToolName::RunCommand, 3), "a terminal that has no file target, such as a Docker pane");
        assert!(!host.available(ToolName::RunCommand, 4), "a block with files and no terminal");
        assert!(!host.available(ToolName::RunCommand, 2), "a pane that is not open");
        let declared: Vec<String> = specs(&host, 1).into_iter().map(|spec| spec.name).collect();
        assert_eq!(declared, ["read_file", "list_dir", "grep", "read_pane", "write_file", "edit_file", "run_command"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_local_command_runs_through_the_runner_and_the_report_is_the_cores_text() {
        let tmp = Tmp::new("report");
        let stage = Stage::default().pane(1, Some(local_files(Some(&tmp.text()), Some(&tmp.text()))), Some(here()));
        let host = ShellTools::with_targets(DenyPolicy::new(Vec::new()), Arc::new(stage)).with_local_exec(runner());
        let table = ApprovalTable::new(DenyPolicy::new(Vec::new()));
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
    }
}
