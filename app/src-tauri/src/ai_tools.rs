//! The shell's side of the agent's tools: the host that finally touches files and panes, the seam it reads the world
//! through, and the one place a person's verdict reaches the approval table.
//!
//! The host holds no card, no hash and no ticket constructor, and no policy beyond the deny list it is given: the page
//! decides, the table checks, and this module opens only what the ticket names. Every read is bounded before the
//! table's own cut, every loop looks at the cancel token, and no fixed text of a refusal echoes a path, a pattern or
//! file text.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use holzwave_core::ai::provider::{CancelToken, Detail};
use holzwave_core::ai::redact::{SourceText, drop_hidden};
use holzwave_core::ai::store::ConversationId;
use holzwave_core::ai::tools::{
    Access, Decision, DenyPolicy, FsKind, Grep, ListDir, MAX_RESULT_BYTES, ReadFile, ReadPane, Resolved, Scope, Style,
    Inspected, Ticket, Tool, ToolError, ToolHost, ToolName, ToolOutput, visible,
};
use holzwave_core::exec::LocalExec;
use holzwave_core::files::{FileSource, Kind};
use holzwave_core::layout::PaneId;
use holzwave_core::ssh::{ExecCancel, ExecLimits, ExecResult};
use holzwave_core::textread::{TextRead, TextRegion};
use holzwave_core::Error as CoreError;

use crate::ai_host::AiFault;
use crate::ai_write::{UndoLog, undo};
use crate::state::AppCore;

/// What a result keeps free for the line that says where to continue.
const FOOTER_RESERVE: usize = 128;

/// The most bytes of file text one `read_file` call returns: what is left of a result after the footer line.
pub const READ_BYTES: usize = MAX_RESULT_BYTES - FOOTER_RESERVE;

/// How many leading bytes of a file are looked at for a NUL, which marks a file as not text.
pub(crate) const SNIFF_BYTES: usize = 8192;

/// The most characters of one matching line a search shows; the rest of the line is cut.
const LINE_CHARS: usize = 200;

/// The most bytes of a pane's text one `read_pane` call asks the engine for.
const PANE_FETCH_BYTES: usize = 65536;

/// The largest file a search opens.
const GREP_FILE_BYTES: u64 = 1_048_576;

/// The fixed phrases of a refusal. Each is one English line and holds nothing the model wrote.
pub(crate) const NOT_TEXT: &str = "that file is not text";
pub(crate) const NOT_A_FILE: &str = "that path is not a regular file";
pub(crate) const UNREADABLE: &str = "that path could not be read";
const NEEDS_CWD: &str = "give an absolute path, the working directory of that pane is not known";
const REMOTE_ABSOLUTE: &str = "a path on a remote host must be absolute";
const NO_HOME: &str = "the home folder is not known";
const BAD_END: &str = "the path must name a file, not a folder";
pub(crate) const REMOTE_LINK: &str = "a link on a remote host is not followed";
pub(crate) const FOLDER_TOO_LARGE: &str = "that folder is too large to check the file";
const NOT_A_FOLDER: &str = "that path is not a folder";
const PANE_GONE: &str = "that pane is closed";
const PANE_BUSY: &str = "that pane is busy, try again";
const PANE_UNREADABLE: &str = "that pane could not be read";

pub(crate) fn host(phrase: &str) -> ToolError {
    ToolError::Host(Detail::new(phrase))
}

pub(crate) fn invalid(phrase: &str) -> ToolError {
    ToolError::InvalidArguments(Detail::new(phrase))
}

/// A cancelled turn stops the call before the next file system request.
pub(crate) fn live(cancel: &CancelToken) -> Result<(), ToolError> {
    if cancel.is_cancelled() { Err(ToolError::Cancelled) } else { Ok(()) }
}

// ---- the seam ---------------------------------------------------------------------------------------------------

/// What a pane's files are: whose, where it starts and what its relative paths mean. `Debug` prints the remote flag
/// and nothing else.
#[derive(Clone)]
pub struct FileTarget {
    /// `local`, or the id of the saved connection.
    pub source: String,
    /// What the card calls the machine: `This computer` or the connection's name.
    pub label: String,
    /// Whether the files are on another machine.
    pub remote: bool,
    /// The home folder of this computer; none for a remote host.
    pub home: Option<String>,
    /// The folder a relative path is read against.
    pub cwd: Option<String>,
}

impl fmt::Debug for FileTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileTarget").field("remote", &self.remote).finish_non_exhaustive()
    }
}

/// What a pane's terminal is: the machine it runs on. `Debug` prints the remote flag and nothing else.
#[derive(Clone)]
pub struct TerminalTarget {
    /// What the card calls the machine.
    pub label: String,
    /// Whether the terminal runs a saved connection.
    pub remote: bool,
}

impl fmt::Debug for TerminalTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalTarget").field("remote", &self.remote).finish_non_exhaustive()
    }
}

/// Why a pane's rows could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowsFault {
    /// The pane is closed.
    Gone,
    /// The pane did not answer in time.
    Busy,
    /// Anything else.
    Other,
}

/// Why a command could not be run on a remote host. `Debug` prints the variant and nothing else; the reason a remote run
/// failed has been scrubbed before it is kept.
#[derive(Clone, PartialEq, Eq)]
pub enum ExecFault {
    /// The world has no way to run a command on a remote host.
    NoRunner,
    /// The command could not be started.
    Start,
    /// The remote host or the connection to it failed; the reason is scrubbed text.
    Remote(String),
}

impl fmt::Debug for ExecFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoRunner => "NoRunner",
            Self::Start => "Start",
            Self::Remote(_) => "Remote",
        })
    }
}

/// The world the tools read through. The shell implements it over the core; tests implement it over tables.
///
/// `is_open`, `file_target`, `terminal` and `shared` look at tables only. `file_source` may make a source, which can
/// open a connection, so only `run` calls it. `read_rows` waits for the engine thread of the pane.
pub trait Targets: Send + Sync {
    /// Whether the pane is in a layout.
    fn is_open(&self, pane: PaneId) -> bool;
    /// What the pane's files are, without touching the file system. None when the pane has no files a tool may read.
    fn file_target(&self, pane: PaneId) -> Option<FileTarget>;
    /// The files of the pane, made if need be.
    fn file_source(&self, pane: PaneId) -> Option<Arc<dyn FileSource>>;
    /// What the pane's terminal is, without touching the engine.
    fn terminal(&self, pane: PaneId) -> Option<TerminalTarget>;
    /// The text of the last `rows` rows of the pane, at most `max_bytes` bytes.
    fn read_rows(&self, pane: PaneId, rows: u16, max_bytes: usize) -> Result<Option<TextRead>, RowsFault>;
    /// Whether the user shared the pane for the chat.
    fn shared(&self, conversation: &str, pane: PaneId) -> bool;

    /// Saves `text` as the whole content of `path` in the files of `source` (this computer or a saved connection): the
    /// one way the agent changes a file. `expected` is the change time the card showed, in whole seconds: a file whose
    /// change time differs is refused and left alone, and a file that does not exist yet has nothing to conflict with.
    /// Answers the change time of the file after the save. May do IO. The default saves nothing.
    fn save_text(&self, _source: &str, _path: &str, _text: &str, _expected: Option<i64>) -> Result<Option<i64>, SaveFault> {
        Err(SaveFault::Other)
    }

    /// The files of `source`, made if need be: what undo reads the current text with. The default has none.
    fn open_source(&self, _source: &str) -> Option<Arc<dyn FileSource>> {
        None
    }

    /// Runs `command` on the host of the saved connection `source` through its exec channel, with no terminal, and waits
    /// for it. Blocks, may open a connection, and ends promptly when `cancel` is raised. The default has no way to.
    fn run_remote(
        &self,
        _source: &str,
        _command: &str,
        _limits: &ExecLimits,
        _cancel: &ExecCancel,
    ) -> Result<ExecResult, ExecFault> {
        Err(ExecFault::NoRunner)
    }
}

/// Why a save did not happen. The library's message is dropped: the model and the page hear a fixed phrase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveFault {
    /// The file changed since the card was made, or is not what the save expected.
    Refused,
    /// Anything else: no connection, no room, no permission.
    Other,
}

// ---- the host ---------------------------------------------------------------------------------------------------

/// The tool host of the shell. It is built before the core exists and bound to the world once the core does; until
/// then it offers nothing.
pub struct ShellTools {
    policy: DenyPolicy,
    targets: OnceLock<Arc<dyn Targets>>,
    limits: Option<GrepLimits>,
    undo: UndoLog,
    exec: LocalExec,
}

impl fmt::Debug for ShellTools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShellTools").field("bound", &self.targets.get().is_some()).finish_non_exhaustive()
    }
}

/// What a search may spend: files opened, bytes read, folders deep and time. The defaults are smaller for a remote
/// host, where every read is a request over the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrepLimits {
    /// The most files opened.
    pub files: usize,
    /// The most bytes read in all.
    pub bytes: u64,
    /// The deepest folder visited, counted from the folder searched (which is level 0).
    pub depth: usize,
    /// The longest the walk may take.
    pub budget: Duration,
}

impl GrepLimits {
    /// The limits on this computer.
    pub fn local() -> GrepLimits {
        GrepLimits { files: 5000, bytes: 64 * 1024 * 1024, depth: 8, budget: Duration::from_secs(20) }
    }

    /// The limits on a remote host.
    pub fn remote() -> GrepLimits {
        GrepLimits { files: 400, bytes: 8 * 1024 * 1024, depth: 8, budget: Duration::from_secs(20) }
    }
}

/// How a path ends: a file tool wants a name, a folder tool takes a folder.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    File,
    Folder,
}

impl ShellTools {
    /// A host with nothing bound: it offers no tool.
    pub fn new(policy: DenyPolicy) -> ShellTools {
        ShellTools { policy, targets: OnceLock::new(), limits: None, undo: UndoLog::new(), exec: LocalExec::from_process() }
    }

    /// The same host with the limits of a search replaced, for both kinds of machine. For tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_limits(mut self, limits: GrepLimits) -> ShellTools {
        self.limits = Some(limits);
        self
    }

    /// The same host with the runner of local commands replaced. For tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_local_exec(mut self, exec: LocalExec) -> ShellTools {
        self.exec = exec;
        self
    }

    /// A host bound to `targets` from the start.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_targets(policy: DenyPolicy, targets: Arc<dyn Targets>) -> ShellTools {
        let tools = ShellTools::new(policy);
        tools.bind(targets);
        tools
    }

    /// Binds the world. Set once; a second call changes nothing.
    pub fn bind(&self, targets: Arc<dyn Targets>) {
        let _ = self.targets.set(targets);
    }

    pub(crate) fn world(&self) -> Result<&Arc<dyn Targets>, ToolError> {
        self.targets.get().ok_or(ToolError::Unavailable)
    }

    /// The world, when it is bound.
    pub(crate) fn targets(&self) -> Option<&Arc<dyn Targets>> {
        self.targets.get()
    }

    /// The deny list the host applies again to a path it resolved itself.
    pub(crate) fn policy(&self) -> &DenyPolicy {
        &self.policy
    }

    /// The runner of commands on this computer: it holds only the variables a command may see.
    pub(crate) fn local_exec(&self) -> &LocalExec {
        &self.exec
    }

    /// The changes the agent made that can still be taken back.
    pub(crate) fn undo_log(&self) -> &UndoLog {
        &self.undo
    }

    /// Forgets what a deleted chat could still undo.
    pub(crate) fn forget_conversation(&self, conversation: &str) {
        self.undo.forget_conversation(conversation);
    }

    /// What a file tool acts on, in the form the table hashes: the pane, the machine and the absolute path.
    fn file_record(&self, scope: &Scope, tool: &Tool, raw: &str, ending: Ending) -> Result<Resolved, ToolError> {
        let world = self.world()?;
        let pane = tool.pane().unwrap_or(scope.pane);
        if !world.is_open(pane) {
            return Err(ToolError::Unavailable);
        }
        let target = world.file_target(pane).ok_or(ToolError::Unavailable)?;
        let path = absolute_path(raw, &target, ending)?;
        let record =
            if target.remote { Resolved::remote(pane, target.label) } else { Resolved::local(pane, target.label) };
        Ok(record.with_path(path))
    }

    /// The file or folder the ticket names, with the pane's files: what a file tool opens and nothing else.
    fn files_of(&self, ticket: &Ticket) -> Result<(Arc<dyn FileSource>, String, FsKind), ToolError> {
        let world = self.world()?;
        let record = ticket.resolved();
        let pane = record.pane();
        let target = world.file_target(pane).ok_or(ToolError::PayloadChanged)?;
        if target.remote != (record.fs() == FsKind::Remote) {
            return Err(ToolError::PayloadChanged);
        }
        let source = world.file_source(pane).ok_or(ToolError::Unavailable)?;
        let path = record.path().ok_or(ToolError::Unavailable)?.to_owned();
        Ok((source, path, record.fs()))
    }

    /// `read_file`: a bounded slice of one text file, from an offset, with a line that says where to continue.
    fn read_file(&self, ticket: &Ticket, args: &ReadFile, cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        let (source, path, fs) = self.files_of(ticket)?;
        live(cancel)?;
        let path = match fs {
            FsKind::Local => {
                let entry = source.stat(&path).map_err(|_| host(UNREADABLE))?;
                if entry.kind != Kind::File {
                    return Err(host(NOT_A_FILE));
                }
                path
            }
            FsKind::Remote => self.remote_file(source.as_ref(), &path, cancel)?,
        };
        live(cancel)?;
        let wanted = usize::try_from(args.limit()).map_or(READ_BYTES, |limit| limit.min(READ_BYTES));
        let offset = args.offset();
        // Up to three bytes at the start can belong to a character that began before the offset, and one more byte
        // than wanted tells whether the file goes on.
        let ask = (wanted as u64).saturating_add(4);
        let bytes = source.read_range(&path, offset, ask).map_err(|_| host(UNREADABLE))?;
        slice_text(bytes, offset, wanted).map(ToolOutput::new)
    }

    /// The path of a file on a remote host that is safe to read: the folder is resolved by the server first, a link in
    /// the last position is refused, and the resolved path is checked against the deny list again.
    fn remote_file(&self, source: &dyn FileSource, path: &str, cancel: &CancelToken) -> Result<String, ToolError> {
        let (folder, name) = split_remote(path);
        let listing = source.list(folder).map_err(|_| host(UNREADABLE))?;
        live(cancel)?;
        let Some(entry) = listing.entries.iter().find(|entry| entry.name == name) else {
            return Err(host(if listing.truncated { FOLDER_TOO_LARGE } else { UNREADABLE }));
        };
        if entry.link {
            return Err(host(REMOTE_LINK));
        }
        if entry.kind != Kind::File {
            return Err(host(NOT_A_FILE));
        }
        let real = join_remote(&listing.path, name);
        self.policy
            .check(Style::Posix, FsKind::Remote, &real, Access::Read)
            .map_err(|denied| ToolError::Denied(denied.rule()))?;
        Ok(real)
    }

    /// What `read_pane` acts on: the pane the call names, which must have a terminal and must be shared with this chat.
    /// The table asks again right before the call runs, so a share that ended while the card waited gives
    /// `NotShared` and the engine is never read.
    fn pane_record(&self, scope: &Scope, args: &ReadPane) -> Result<Resolved, ToolError> {
        let world = self.world()?;
        let pane = args.pane;
        if !world.is_open(pane) {
            return Err(ToolError::Unavailable);
        }
        let terminal = world.terminal(pane).ok_or(ToolError::Unavailable)?;
        if !world.shared(&scope.conversation, pane) {
            return Err(ToolError::NotShared);
        }
        Ok(if terminal.remote { Resolved::remote(pane, terminal.label) } else { Resolved::local(pane, terminal.label) })
    }

    /// The folder of a remote host that the server resolved, checked against the deny list before any name in it is
    /// used.
    fn resolved_folder(&self, resolved: &str) -> Result<(), ToolError> {
        self.policy
            .check(Style::Posix, FsKind::Remote, resolved, Access::Read)
            .map_err(|denied| ToolError::Denied(denied.rule()))
    }

    /// `list_dir`: one escaped line per entry, within the bound of the call and of the result.
    fn list_dir(&self, ticket: &Ticket, args: &ListDir, cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        let (source, path, fs) = self.files_of(ticket)?;
        live(cancel)?;
        if fs == FsKind::Local {
            let entry = source.stat(&path).map_err(|_| host(UNREADABLE))?;
            if entry.kind != Kind::Dir {
                return Err(host(NOT_A_FOLDER));
            }
            live(cancel)?;
        }
        let listing = source.list(&path).map_err(|_| host(UNREADABLE))?;
        if fs == FsKind::Remote {
            self.resolved_folder(&listing.path)?;
        }
        let mut lines = vec![visible(&listing.path)];
        let mut size = lines[0].len();
        let wanted = usize::try_from(args.max_entries()).unwrap_or(usize::MAX);
        let mut shown = 0;
        for entry in listing.entries.iter().take(wanted) {
            let kind = match (entry.link, entry.kind) {
                (true, _) => "link",
                (false, Kind::Dir) => "dir",
                (false, Kind::File) => "file",
                (false, Kind::Other) => "other",
            };
            let bytes = if entry.kind == Kind::Dir { 0 } else { entry.size };
            let line = format!("{kind} {bytes} {}", visible(&entry.name));
            if size + line.len() + 1 > READ_BYTES {
                break;
            }
            size += line.len() + 1;
            lines.push(line);
            shown += 1;
        }
        let left = listing.entries.len() - shown;
        if left > 0 {
            lines.push(format!("[{left} more entries not shown]"));
        }
        if listing.truncated {
            lines.push("[the source cut the listing]".to_owned());
        }
        Ok(ToolOutput::new(lines.join("\n")))
    }

    /// `grep`: a literal search of one file or of a folder, with limits on matches, files, bytes, depth and time. It
    /// never follows a link, skips binary, large, denied and `.git` entries, and checks every child it opens again.
    fn grep(&self, ticket: &Ticket, args: &Grep, cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        let (source, root, fs) = self.files_of(ticket)?;
        let remote = fs == FsKind::Remote;
        let limits = self.limits.unwrap_or(if remote { GrepLimits::remote() } else { GrepLimits::local() });
        let ignore_case = args.ignore_case();
        let mut search = Search {
            needle: if ignore_case { args.pattern.to_lowercase() } else { args.pattern.clone() },
            ignore_case,
            max_matches: usize::try_from(args.max_matches()).unwrap_or(usize::MAX),
            limits,
            started: Instant::now(),
            source: source.as_ref(),
            cancel,
            out: String::new(),
            matches: 0,
            opened: 0,
            searched: 0,
            bytes: 0,
            skipped: 0,
            stopped: None,
        };
        live(cancel)?;
        let entry = source.stat(&root).map_err(|_| host(UNREADABLE))?;
        match entry.kind {
            Kind::File => {
                let (open, name) = if remote {
                    let real = self.remote_file(source.as_ref(), &root, cancel)?;
                    let name = split_remote(&real).1.to_owned();
                    (real, name)
                } else {
                    let name =
                        Path::new(&root).file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    (root.clone(), name)
                };
                search.file(&open, &name, entry.size)?;
            }
            Kind::Dir => self.walk(&mut search, &root, remote)?,
            Kind::Other => return Err(host(NOT_A_FILE)),
        }
        let mut text = search.out;
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!(
            "[{} matches in {} files searched; {} entries skipped]",
            search.matches, search.searched, search.skipped
        ));
        if let Some(reason) = search.stopped {
            text.push_str(&format!("\n[stopped: {reason}]"));
        }
        Ok(ToolOutput::new(text))
    }

    /// The folders of a search: an explicit stack, the files of a folder before its subfolders.
    fn walk(&self, search: &mut Search<'_>, root: &str, remote: bool) -> Result<(), ToolError> {
        let mut stack: Vec<(String, String, usize)> = vec![(root.to_owned(), String::new(), 0)];
        let mut first = true;
        while let Some((folder, relative, depth)) = stack.pop() {
            live(search.cancel)?;
            if search.expired() {
                search.stopped = Some("time limit");
                break;
            }
            let listing = match search.source.list(&folder) {
                Ok(listing) => listing,
                Err(_) if first => return Err(host(UNREADABLE)),
                Err(_) => {
                    search.skipped += 1;
                    continue;
                }
            };
            if remote {
                match self.resolved_folder(&listing.path) {
                    Ok(()) => {}
                    Err(error) if first => return Err(error),
                    Err(_) => {
                        search.skipped += 1;
                        continue;
                    }
                }
            }
            first = false;
            let base = if listing.path.is_empty() { folder.clone() } else { listing.path.clone() };
            let mut files: Vec<(String, String, u64)> = Vec::new();
            let mut folders: Vec<(String, String, usize)> = Vec::new();
            for entry in &listing.entries {
                if entry.link || entry.kind == Kind::Other {
                    search.skipped += 1;
                    continue;
                }
                let child = if remote { join_remote(&base, &entry.name) } else { join_local(&base, &entry.name) };
                let shown = if relative.is_empty() { entry.name.clone() } else { format!("{relative}/{}", entry.name) };
                if entry.kind == Kind::Dir {
                    let hidden = entry.name.eq_ignore_ascii_case(".git") || depth + 1 > search.limits.depth;
                    match self.openable(&child, remote).filter(|_| !hidden) {
                        Some(open) => folders.push((open, shown, depth + 1)),
                        None => search.skipped += 1,
                    }
                } else if entry.size > GREP_FILE_BYTES {
                    search.skipped += 1;
                } else {
                    match self.openable(&child, remote) {
                        Some(open) => files.push((open, shown, entry.size)),
                        None => search.skipped += 1,
                    }
                }
            }
            for (open, shown, size) in files {
                search.file(&open, &shown, size)?;
                if search.stopped.is_some() {
                    return Ok(());
                }
            }
            stack.extend(folders.into_iter().rev());
        }
        Ok(())
    }

    /// The path a search may open for `child`, or none when the deny list refuses it: on this computer the path after
    /// the file system resolved it, on a remote host the path as it is.
    fn openable(&self, child: &str, remote: bool) -> Option<String> {
        if remote {
            return self.policy.check(Style::Posix, FsKind::Remote, child, Access::Read).ok().map(|()| child.to_owned());
        }
        let resolved = self.policy.check_local(Path::new(child), Access::Read).ok()?;
        resolved.into_os_string().into_string().ok()
    }

    /// `read_pane`: the tail of the last rows of a shared pane, without the cells the user could not see.
    fn read_pane(&self, args: &ReadPane, cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        let world = self.world()?;
        live(cancel)?;
        let pane = args.pane;
        if world.terminal(pane).is_none() {
            return Err(ToolError::PayloadChanged);
        }
        let rows = u16::try_from(args.rows()).unwrap_or(u16::MAX);
        let read = match world.read_rows(pane, rows, PANE_FETCH_BYTES) {
            Ok(read) => read,
            Err(RowsFault::Gone) => return Err(host(PANE_GONE)),
            Err(RowsFault::Busy) => return Err(host(PANE_BUSY)),
            Err(RowsFault::Other) => return Err(host(PANE_UNREADABLE)),
        };
        let Some(read) = read else { return Ok(ToolOutput::new(NOTHING.to_owned())) };
        let cleaned = drop_hidden(&SourceText::with_hidden(read.text, read.hidden));
        let text = cleaned.text.trim_end_matches('\n');
        if text.is_empty() {
            return Ok(ToolOutput::new(NOTHING.to_owned()));
        }
        Ok(ToolOutput::new(tail_of(text)))
    }
}

/// What `read_pane` says when the pane shows nothing.
const NOTHING: &str = "[the pane shows nothing]";

/// The line that tells a result starts after the beginning of the output.
const OMITTED: &str = "[earlier output omitted]";

/// `text` itself when it fits, otherwise its tail from a line boundary with a first line that says output was left out.
fn tail_of(text: &str) -> String {
    if text.len() <= READ_BYTES {
        return text.to_owned();
    }
    let room = READ_BYTES - OMITTED.len() - 1;
    let mut start = text.len() - room;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    if let Some(newline) = text[start..].find('\n') {
        start += newline + 1;
    }
    format!("{OMITTED}\n{}", &text[start..])
}

/// The state of one search: what it looks for, what it may spend and what it has found.
struct Search<'a> {
    needle: String,
    ignore_case: bool,
    max_matches: usize,
    limits: GrepLimits,
    started: Instant,
    source: &'a dyn FileSource,
    cancel: &'a CancelToken,
    out: String,
    matches: usize,
    /// Files the search read, whether or not they turned out to be text: what the limit on files counts.
    opened: usize,
    /// Text files whose lines were looked at.
    searched: usize,
    bytes: u64,
    skipped: usize,
    stopped: Option<&'static str>,
}

impl Search<'_> {
    fn expired(&self) -> bool {
        self.started.elapsed() >= self.limits.budget
    }

    fn hit(&self, line: &str) -> bool {
        if self.ignore_case { line.to_lowercase().contains(&self.needle) } else { line.contains(&self.needle) }
    }

    /// Searches one file that is allowed and not too large, unless a limit says the search is over. `shown` is the path
    /// the matching lines start with; `size` is what the listing said.
    fn file(&mut self, open: &str, shown: &str, size: u64) -> Result<(), ToolError> {
        live(self.cancel)?;
        if self.expired() {
            self.stopped = Some("time limit");
        } else if self.opened >= self.limits.files {
            self.stopped = Some("file limit");
        } else if self.bytes.saturating_add(size) > self.limits.bytes {
            self.stopped = Some("size limit");
        }
        if self.stopped.is_some() {
            return Ok(());
        }
        self.opened += 1;
        let Ok(bytes) = self.source.read(open, GREP_FILE_BYTES) else {
            self.skipped += 1;
            return Ok(());
        };
        self.bytes += bytes.len() as u64;
        if bytes.iter().take(SNIFF_BYTES).any(|byte| *byte == 0) {
            self.skipped += 1;
            return Ok(());
        }
        self.searched += 1;
        let text = String::from_utf8_lossy(&bytes);
        for (number, line) in text.split('\n').enumerate() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if !self.hit(line) {
                continue;
            }
            let cut: String = line.chars().take(LINE_CHARS).collect();
            let ellipsis = if line.chars().nth(LINE_CHARS).is_some() { "\u{2026}" } else { "" };
            let entry = format!("{}:{}: {}{ellipsis}", visible(shown), number + 1, visible(&cut));
            if self.out.len() + entry.len() + 1 > READ_BYTES {
                self.stopped = Some("output limit");
                return Ok(());
            }
            if !self.out.is_empty() {
                self.out.push('\n');
            }
            self.out.push_str(&entry);
            self.matches += 1;
            if self.matches >= self.max_matches {
                self.stopped = Some("match limit");
                return Ok(());
            }
        }
        Ok(())
    }
}

/// `folder` and `name` joined as this computer spells paths.
fn join_local(folder: &str, name: &str) -> String {
    Path::new(folder).join(name).into_os_string().into_string().unwrap_or_default()
}

impl ToolHost for ShellTools {
    fn available(&self, tool: ToolName, pane: PaneId) -> bool {
        let Ok(world) = self.world() else { return false };
        if !world.is_open(pane) {
            return false;
        }
        match tool {
            ToolName::ReadFile | ToolName::ListDir | ToolName::Grep => world.file_target(pane).is_some(),
            ToolName::ReadPane => world.terminal(pane).is_some(),
            ToolName::WriteFile | ToolName::EditFile => world.file_target(pane).is_some(),
            ToolName::RunCommand => crate::ai_exec::offers(world.as_ref(), pane),
        }
    }

    fn resolve(&self, scope: &Scope, tool: &Tool) -> Result<Resolved, ToolError> {
        match tool {
            Tool::ReadFile(args) => self.file_record(scope, tool, &args.path, Ending::File),
            Tool::ListDir(args) => self.file_record(scope, tool, &args.path, Ending::Folder),
            Tool::Grep(args) => self.file_record(scope, tool, &args.path, Ending::Folder),
            Tool::ReadPane(args) => self.pane_record(scope, args),
            Tool::WriteFile(args) => self.file_record(scope, tool, &args.path, Ending::File),
            Tool::EditFile(args) => self.file_record(scope, tool, &args.path, Ending::File),
            Tool::RunCommand(args) => crate::ai_exec::resolve(self, scope, args),
        }
    }

    fn run(&self, ticket: &Ticket, cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        match ticket.tool() {
            Tool::ReadFile(args) => self.read_file(ticket, args, cancel),
            Tool::ListDir(args) => self.list_dir(ticket, args, cancel),
            Tool::Grep(args) => self.grep(ticket, args, cancel),
            Tool::ReadPane(args) => self.read_pane(args, cancel),
            Tool::WriteFile(_) | Tool::EditFile(_) => crate::ai_write::run(self, ticket, cancel),
            Tool::RunCommand(_) => crate::ai_exec::run(self, ticket, cancel),
        }
    }

    fn inspect(&self, resolved: &Resolved) -> Result<Option<Inspected>, ToolError> {
        crate::ai_write::inspect(self, resolved).map(Some)
    }
}

// ---- paths ------------------------------------------------------------------------------------------------------

/// Whether `raw` ends where a folder would: in a separator, or in a `.` or `..` part.
fn ends_like_a_folder(raw: &str, remote: bool) -> bool {
    let separator = |c: char| c == '/' || (!remote && cfg!(windows) && c == '\\');
    if raw.ends_with(separator) {
        return true;
    }
    let last = raw.rsplit(separator).next().unwrap_or("");
    last == "." || last == ".."
}

/// The part of `raw` after a leading `~` that stands for the home folder: `~` alone, or `~/` and what follows.
fn after_tilde(raw: &str) -> Option<&str> {
    if raw == "~" {
        return Some("");
    }
    raw.strip_prefix("~/").or_else(|| if cfg!(windows) { raw.strip_prefix("~\\") } else { None })
}

/// The path of a call as an absolute one, made by the host: a `~/` is the home of this computer, a relative path is
/// read against the working directory of the pane, and a remote path is absolute or relative to the working directory.
/// The `..` parts stay as the model wrote them: the table normalises them for its check and a remote host resolves
/// them itself. The errors are fixed phrases that hold none of the path.
pub(crate) fn absolute_path(raw: &str, target: &FileTarget, ending: Ending) -> Result<String, ToolError> {
    if ending == Ending::File && ends_like_a_folder(raw, target.remote) {
        return Err(invalid(BAD_END));
    }
    let trimmed = |path: String| -> String {
        if ending == Ending::Folder { trim_separators(&path, target.remote) } else { path }
    };
    if target.remote {
        if raw.starts_with('/') {
            return Ok(trimmed(raw.to_owned()));
        }
        if raw.starts_with('~') {
            return Err(invalid(REMOTE_ABSOLUTE));
        }
        let cwd = target.cwd.as_deref().ok_or_else(|| invalid(NEEDS_CWD))?;
        return Ok(trimmed(join_remote(cwd, raw)));
    }
    let joined: PathBuf = if let Some(rest) = after_tilde(raw) {
        let home = target.home.as_deref().ok_or_else(|| invalid(NO_HOME))?;
        if rest.is_empty() { PathBuf::from(home) } else { Path::new(home).join(rest) }
    } else if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        let cwd = target.cwd.as_deref().ok_or_else(|| invalid(NEEDS_CWD))?;
        Path::new(cwd).join(raw)
    };
    joined.into_os_string().into_string().map(trimmed).map_err(|_| invalid(NEEDS_CWD))
}

/// `path` without trailing separators, except that a root stays a root.
fn trim_separators(path: &str, remote: bool) -> String {
    let separator = |c: char| c == '/' || (!remote && cfg!(windows) && c == '\\');
    let trimmed = path.trim_end_matches(separator);
    if trimmed.is_empty() && !path.is_empty() { path[..1].to_owned() } else { trimmed.to_owned() }
}

/// `folder` and `name` joined with one `/`.
pub(crate) fn join_remote(folder: &str, name: &str) -> String {
    format!("{}/{name}", folder.trim_end_matches('/'))
}

/// A remote file path split at its last `/` into the folder and the name; a name directly under the root has the folder
/// `/`.
pub(crate) fn split_remote(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(at) => (&path[..at], &path[at + 1..]),
        None => ("/", path),
    }
}

// ---- file text --------------------------------------------------------------------------------------------------

/// How many of the last bytes of `bytes` start a character that is not complete: the part that has to wait for the
/// next slice.
fn incomplete_tail(bytes: &[u8]) -> usize {
    for back in 1..=bytes.len().min(3) {
        let byte = bytes[bytes.len() - back];
        if byte < 0x80 {
            return 0;
        }
        if byte >= 0xC0 {
            let needed = if byte >= 0xF0 {
                4
            } else if byte >= 0xE0 {
                3
            } else {
                2
            };
            return if needed > back { back } else { 0 };
        }
    }
    0
}

/// The text of a slice of a file. `bytes` start at `offset` and hold at most three bytes more than `wanted` plus one;
/// the leading bytes that belong to a character that began before the offset are dropped, a file that goes on past
/// `wanted` bytes is cut on a character boundary and ends with a line that names the offset to continue at, and a file
/// with a NUL in its first bytes is not text.
fn slice_text(mut bytes: Vec<u8>, offset: u64, wanted: usize) -> Result<String, ToolError> {
    let skipped = if offset == 0 { 0 } else { bytes.iter().take(3).take_while(|byte| **byte & 0xC0 == 0x80).count() };
    bytes.drain(..skipped);
    let more = bytes.len() > wanted;
    bytes.truncate(wanted);
    if bytes.iter().take(SNIFF_BYTES).any(|byte| *byte == 0) {
        return Err(host(NOT_TEXT));
    }
    if more {
        let keep = bytes.len() - incomplete_tail(&bytes);
        bytes.truncate(keep);
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if more {
        let next = offset + skipped as u64 + bytes.len() as u64;
        return Ok(format!("{text}\n[more text follows: continue with offset {next}]"));
    }
    if text.is_empty() {
        return Ok(if offset == 0 { "[the file is empty]" } else { "[no text at that offset]" }.to_owned());
    }
    Ok(text)
}

// ---- the world of the core --------------------------------------------------------------------------------------

/// The seam over the core: every call upgrades the weak reference and answers nothing when the core is gone.
pub struct CoreTargets(Weak<AppCore>);

impl CoreTargets {
    /// The world of `core`, held weakly.
    pub fn new(core: Weak<AppCore>) -> CoreTargets {
        CoreTargets(core)
    }
}

impl fmt::Debug for CoreTargets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreTargets").field("alive", &(self.0.strong_count() > 0)).finish()
    }
}

/// The reason a pane could not be read, from the engine's own words: a closed pane, one that did not answer in time,
/// or anything else. The library's message is dropped.
fn fault_of(error: &CoreError) -> RowsFault {
    match error {
        CoreError::Pty(message) if message == "the pane is closed" => RowsFault::Gone,
        CoreError::Pty(message) if message == "pane busy" => RowsFault::Busy,
        _ => RowsFault::Other,
    }
}

impl Targets for CoreTargets {
    fn is_open(&self, pane: PaneId) -> bool {
        self.0.upgrade().is_some_and(|core| core.pane_is_open(pane))
    }

    fn file_target(&self, pane: PaneId) -> Option<FileTarget> {
        self.0.upgrade()?.tool_file_target(pane)
    }

    fn file_source(&self, pane: PaneId) -> Option<Arc<dyn FileSource>> {
        self.0.upgrade()?.tool_file_source(pane)
    }

    fn terminal(&self, pane: PaneId) -> Option<TerminalTarget> {
        self.0.upgrade()?.tool_terminal(pane)
    }

    fn read_rows(&self, pane: PaneId, rows: u16, max_bytes: usize) -> Result<Option<TextRead>, RowsFault> {
        let core = self.0.upgrade().ok_or(RowsFault::Gone)?;
        let handle = core.context_pane(pane).ok_or(RowsFault::Gone)?;
        handle.read_text(TextRegion::LastRows(rows), max_bytes).map_err(|error| fault_of(&error))
    }

    fn shared(&self, conversation: &str, pane: PaneId) -> bool {
        let Ok(id) = ConversationId::parse(conversation) else { return false };
        self.0.upgrade().is_some_and(|core| core.ai().service().is_shared(&id, pane))
    }

    fn save_text(&self, source: &str, path: &str, text: &str, expected: Option<i64>) -> Result<Option<i64>, SaveFault> {
        let core = self.0.upgrade().ok_or(SaveFault::Other)?;
        core.tool_save_text(source, path, text, expected).map_err(|message| fault_of_save(&message))
    }

    fn open_source(&self, source: &str) -> Option<Arc<dyn FileSource>> {
        self.0.upgrade()?.tool_open_source(source)
    }
}

/// The words `Blocks::save_file` uses for a file that changed since the caller looked at it. A test of the shell pins them
/// against the real function, so a rewording there fails here and not silently.
pub(crate) const CONFLICT_WORDS: &str = "changed on disk";

/// A refusal of the save because the file is not what was expected, or any other failure; the message is dropped.
fn fault_of_save(message: &str) -> SaveFault {
    if message.contains(CONFLICT_WORDS) { SaveFault::Refused } else { SaveFault::Other }
}

// ---- the verdict ------------------------------------------------------------------------------------------------

impl AppCore {
    /// The person's press on `Undo` for one finished write: the chat and the call. It answers nothing. The old text of the
    /// file is put back only while the file still holds exactly what the assistant wrote; a refusal is the fault object with
    /// the code `undo` and a stable reason. This is not a verdict about a tool call: it never reaches the approval table.
    pub fn ai_undo(&self, conversation: &str, call_id: &str) -> Result<(), AiFault> {
        let id = ConversationId::parse(conversation).map_err(|_| AiFault::invalid_conversation())?;
        undo(self.ai().tools(), id.as_str(), call_id).map_err(|fault| AiFault::from_undo(&fault))
    }

    /// The page's verdict on one approval card: the chat, the call, `allow` or `deny`, and the hash of the card the
    /// person was shown. The table compares the hash and refuses any other; an answer of a call that is not waiting is
    /// refused too. This is the one place the approval table is decided, and the command of the webview is its only
    /// caller: no other command, event, socket or subcommand reaches it.
    pub fn ai_decide(&self, conversation: &str, call_id: &str, decision: &str, hash: &str) -> Result<(), AiFault> {
        let id = ConversationId::parse(conversation).map_err(|_| AiFault::invalid_conversation())?;
        let verdict = match decision {
            "allow" => Decision::Allow,
            "deny" => Decision::Deny,
            _ => return Err(AiFault::invalid_decision()),
        };
        self.ai()
            .service()
            .approvals()
            .decide(id.as_str(), call_id, verdict, hash)
            .map_err(|error| AiFault::from_tool(&error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::rig::run_call;

    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use holzwave_core::ai::tools::{ApprovalTable, specs};
    use holzwave_core::ai::types::ToolCall;
    use holzwave_core::files::{Entry, Listing, LocalSource};
    use holzwave_core::{Error, Result as CoreResult};
    use serde_json::{Value, json};

    /// Text in a path of these tests; it appears in test code only, so a hit in an error text is a leak.
    const PATH_CANARY: &str = "zq-unit-path-5c0a82";

    /// A scratch folder with its canonical spelling, removed at the end of the test.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Tmp {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("holzwave-tools-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch dir");
            Tmp(path.canonicalize().expect("canonical"))
        }

        fn path(&self, name: &str) -> String {
            self.0.join(name).to_str().expect("utf-8").to_owned()
        }

        fn write(&self, name: &str, bytes: &[u8]) -> String {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("parent");
            }
            std::fs::write(&path, bytes).expect("a file");
            path.to_str().expect("utf-8").to_owned()
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entry(name: &str, kind: Kind, link: bool) -> Entry {
        Entry { name: name.to_owned(), kind, link, size: 0, modified: None, hidden: false }
    }

    /// A remote file system in memory: folders as the server resolves them, and file bytes. It records every read.
    #[derive(Default)]
    struct MemFiles {
        folders: HashMap<String, Listing>,
        files: HashMap<String, Vec<u8>>,
        stats: HashMap<String, Entry>,
        reads: Mutex<Vec<String>>,
    }

    impl MemFiles {
        fn folder(mut self, asked: &str, resolved: &str, entries: Vec<Entry>, truncated: bool) -> MemFiles {
            let listing = Listing { path: resolved.to_owned(), parent: None, entries, truncated };
            self.folders.insert(asked.to_owned(), listing);
            self
        }

        fn file(mut self, path: &str, bytes: &[u8]) -> MemFiles {
            self.files.insert(path.to_owned(), bytes.to_vec());
            self
        }

        /// What `stat` answers for `path`.
        fn stat_of(mut self, path: &str, kind: Kind, size: u64) -> MemFiles {
            let name = path.rsplit('/').next().unwrap_or("").to_owned();
            self.stats.insert(path.to_owned(), Entry { name, kind, link: false, size, modified: None, hidden: false });
            self
        }

        /// A folder holding `count` files named `f<n>.txt`, each with the lines of `text` and the size of `text`.
        fn with_files(mut self, folder: &str, count: usize, text: &str) -> MemFiles {
            let mut entries = Vec::new();
            for n in 0..count {
                let name = format!("f{n:03}.txt");
                self.files.insert(format!("{folder}/{name}"), text.as_bytes().to_vec());
                entries.push(Entry { size: text.len() as u64, ..entry(&name, Kind::File, false) });
            }
            let listing = Listing { path: folder.to_owned(), parent: None, entries, truncated: false };
            self.folders.insert(folder.to_owned(), listing);
            self.stats.insert(folder.to_owned(), Entry { name: folder.to_owned(), ..entry("", Kind::Dir, false) });
            self
        }

        fn read_paths(&self) -> Vec<String> {
            self.reads.lock().expect("reads").clone()
        }
    }

    impl FileSource for MemFiles {
        fn label(&self) -> String {
            "remote".to_owned()
        }

        fn home(&self) -> CoreResult<String> {
            Ok("/".to_owned())
        }

        fn list(&self, path: &str) -> CoreResult<Listing> {
            self.folders.get(path).cloned().ok_or_else(|| Error::Unsupported("no such folder".into()))
        }

        fn stat(&self, path: &str) -> CoreResult<Entry> {
            self.stats.get(path).cloned().ok_or_else(|| Error::Unsupported("no such entry".into()))
        }

        fn read(&self, path: &str, limit: u64) -> CoreResult<Vec<u8>> {
            self.read_range(path, 0, limit)
        }

        fn read_range(&self, path: &str, offset: u64, len: u64) -> CoreResult<Vec<u8>> {
            self.reads.lock().expect("reads").push(path.to_owned());
            let bytes = self.files.get(path).ok_or_else(|| Error::Unsupported("no such file".into()))?;
            let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
            let end = start.saturating_add(usize::try_from(len).unwrap_or(usize::MAX)).min(bytes.len());
            Ok(bytes[start..end].to_vec())
        }

        fn write(&self, _path: &str, _bytes: &[u8]) -> CoreResult<()> {
            Err(Error::Unsupported("read only".into()))
        }
    }

    /// The panes of a fake world: which are open, what their files and terminals are, which are shared, and what a
    /// read of rows answers. Counters say how often the host asked for a source or for rows.
    #[derive(Default)]
    struct World {
        open: HashSet<PaneId>,
        files: HashMap<PaneId, (FileTarget, Arc<dyn FileSource>)>,
        terminals: HashMap<PaneId, TerminalTarget>,
        shares: HashSet<(String, PaneId)>,
        rows: HashMap<PaneId, Result<Option<TextRead>, RowsFault>>,
        sources_made: usize,
        rows_calls: Vec<(PaneId, u16, usize)>,
    }

    #[derive(Default)]
    struct FakeTargets {
        world: Mutex<World>,
    }

    impl FakeTargets {
        fn world(&self) -> std::sync::MutexGuard<'_, World> {
            self.world.lock().expect("world")
        }

        fn with_files(self, pane: PaneId, target: FileTarget, source: Arc<dyn FileSource>) -> FakeTargets {
            {
                let mut world = self.world();
                world.open.insert(pane);
                world.files.insert(pane, (target, source));
            }
            self
        }

        fn with_terminal(self, pane: PaneId, remote: bool) -> FakeTargets {
            {
                let mut world = self.world();
                world.open.insert(pane);
                world.terminals.insert(pane, TerminalTarget { label: "This computer".to_owned(), remote });
            }
            self
        }

        fn sources_made(&self) -> usize {
            self.world().sources_made
        }

        fn share(&self, conversation: &str, pane: PaneId) {
            self.world().shares.insert((conversation.to_owned(), pane));
        }

        fn unshare(&self, conversation: &str, pane: PaneId) {
            self.world().shares.remove(&(conversation.to_owned(), pane));
        }

        fn set_rows(&self, pane: PaneId, answer: Result<Option<TextRead>, RowsFault>) {
            self.world().rows.insert(pane, answer);
        }

        fn rows_calls(&self) -> Vec<(PaneId, u16, usize)> {
            self.world().rows_calls.clone()
        }
    }

    impl Targets for FakeTargets {
        fn is_open(&self, pane: PaneId) -> bool {
            self.world().open.contains(&pane)
        }

        fn file_target(&self, pane: PaneId) -> Option<FileTarget> {
            self.world().files.get(&pane).map(|(target, _)| target.clone())
        }

        fn file_source(&self, pane: PaneId) -> Option<Arc<dyn FileSource>> {
            let mut world = self.world();
            world.sources_made += 1;
            world.files.get(&pane).map(|(_, source)| source.clone())
        }

        fn terminal(&self, pane: PaneId) -> Option<TerminalTarget> {
            self.world().terminals.get(&pane).cloned()
        }

        fn read_rows(&self, pane: PaneId, rows: u16, max_bytes: usize) -> Result<Option<TextRead>, RowsFault> {
            let mut world = self.world();
            world.rows_calls.push((pane, rows, max_bytes));
            world.rows.get(&pane).cloned().unwrap_or(Ok(None))
        }

        fn shared(&self, conversation: &str, pane: PaneId) -> bool {
            self.world().shares.contains(&(conversation.to_owned(), pane))
        }
    }

    fn local_target(home: Option<&str>, cwd: Option<&str>) -> FileTarget {
        FileTarget {
            source: "local".to_owned(),
            label: "This computer".to_owned(),
            remote: false,
            home: home.map(str::to_owned),
            cwd: cwd.map(str::to_owned),
        }
    }

    fn remote_target(cwd: Option<&str>) -> FileTarget {
        FileTarget {
            source: "box".to_owned(),
            label: "build box".to_owned(),
            remote: true,
            home: None,
            cwd: cwd.map(str::to_owned),
        }
    }

    fn scope(conversation: &str, turn: u64, pane: PaneId) -> Scope {
        Scope { conversation: conversation.to_owned(), turn, pane }
    }

    fn host_of(world: FakeTargets, protected: Vec<PathBuf>) -> (ShellTools, Arc<FakeTargets>) {
        let world = Arc::new(world);
        (ShellTools::with_targets(DenyPolicy::new(protected), world.clone()), world)
    }

    fn local_world(tmp: &Tmp, pane: PaneId) -> FakeTargets {
        let target = local_target(Some(&tmp.path("")), Some(&tmp.path("")));
        FakeTargets::default().with_files(pane, target, Arc::new(LocalSource::new(Some(tmp.0.clone()))))
    }

    fn table_over(protected: &[PathBuf]) -> ApprovalTable {
        ApprovalTable::new(DenyPolicy::new(protected.to_vec()))
    }

    fn text_of(output: Result<ToolOutput, ToolError>) -> String {
        output.expect("the call ran").into_text()
    }

    #[test]
    fn the_host_offers_a_tool_only_when_it_can_run_it() {
        let unbound = ShellTools::new(DenyPolicy::new(Vec::new()));
        for tool in ToolName::ALL {
            assert!(!unbound.available(tool, 1), "an unbound host offers {tool:?}");
        }
        assert!(specs(&unbound, 1).is_empty());

        let world = FakeTargets::default()
            .with_files(1, local_target(None, None), Arc::new(MemFiles::default()))
            .with_terminal(1, false)
            .with_terminal(3, false)
            .with_files(4, remote_target(None), Arc::new(MemFiles::default()));
        let (host, _) = host_of(world, Vec::new());
        let offered = |pane: PaneId| -> Vec<&'static str> {
            ToolName::ALL.into_iter().filter(|tool| host.available(*tool, pane)).map(ToolName::as_str).collect()
        };
        assert_eq!(
            offered(1),
            ["read_file", "list_dir", "grep", "read_pane", "write_file", "edit_file", "run_command"],
            "this computer: files and a terminal"
        );
        assert_eq!(offered(2), Vec::<&str>::new(), "a pane that is not open");
        assert_eq!(offered(3), ["read_pane"], "a terminal with no file target offers no file tool");
        assert_eq!(
            offered(4),
            ["read_file", "list_dir", "grep", "write_file", "edit_file"],
            "a file browser has files and no terminal"
        );
        for pane in [3, 4] {
            assert!(!host.available(ToolName::RunCommand, pane), "a command needs a terminal and files together");
        }
        let declared: Vec<String> = specs(&host, 1).into_iter().map(|spec| spec.name).collect();
        assert_eq!(declared, ["read_file", "list_dir", "grep", "read_pane", "write_file", "edit_file", "run_command"]);
    }

    #[test]
    fn a_relative_path_uses_the_working_directory_and_a_tilde_the_home_of_this_computer() {
        let scope = scope("c1", 1, 1);
        let resolve = |host: &ShellTools, args: Value| -> Result<Resolved, ToolError> {
            let tool = Tool::parse("read_file", &args).expect("arguments");
            host.resolve(&scope, &tool)
        };
        let world = FakeTargets::default()
            .with_files(1, local_target(Some("/home/u"), Some("/home/u/proj")), Arc::new(MemFiles::default()))
            .with_files(2, remote_target(Some("/srv/app")), Arc::new(MemFiles::default()))
            .with_files(5, local_target(None, None), Arc::new(MemFiles::default()));
        let (host, _) = host_of(world, Vec::new());
        let path_of = |args: Value| resolve(&host, args).map(|record| record.path().map(str::to_owned));

        if cfg!(unix) {
            assert_eq!(path_of(json!({"path": "~/a.txt"})), Ok(Some("/home/u/a.txt".to_owned())));
            assert_eq!(path_of(json!({"path": "src/main.rs"})), Ok(Some("/home/u/proj/src/main.rs".to_owned())));
            assert_eq!(path_of(json!({"path": "/etc/hosts"})), Ok(Some("/etc/hosts".to_owned())));
            assert_eq!(path_of(json!({"path": "../x.txt"})), Ok(Some("/home/u/proj/../x.txt".to_owned())), "dots stay");
        }
        // Without a working directory or a home the path is refused by a fixed phrase that holds none of it.
        let bare = |path: String| {
            let tool = Tool::parse("read_file", &json!({"path": path, "pane": 5})).expect("arguments");
            host.resolve(&scope, &tool).expect_err("refused")
        };
        for error in [bare(format!("{PATH_CANARY}/a")), bare(format!("~/{PATH_CANARY}"))] {
            assert!(matches!(error, ToolError::InvalidArguments(_)), "{error:?}");
            assert!(!error.to_string().contains(PATH_CANARY), "the refusal quotes the path: {error}");
        }

        // A remote host: absolute, or relative to its working directory, never a home.
        let remote = |path: &str| path_of(json!({"path": path, "pane": 2}));
        assert_eq!(remote("/var/log/x"), Ok(Some("/var/log/x".to_owned())));
        assert_eq!(remote("logs/a.txt"), Ok(Some("/srv/app/logs/a.txt".to_owned())));
        for refused in ["~/a", "/srv/app/", "/srv/app/.", "/srv/app/..", "logs/"] {
            let error = remote(refused).expect_err(refused);
            assert!(matches!(error, ToolError::InvalidArguments(_)), "{refused}: {error:?}");
        }
        let record = resolve(&host, json!({"path": "/var/log/x", "pane": 2})).expect("a record");
        assert_eq!((record.pane(), record.target(), record.fs()), (2, "build box", FsKind::Remote));
        let record = resolve(&host, json!({"path": "/etc/hosts"})).expect("a record");
        assert_eq!((record.pane(), record.target(), record.fs()), (1, "This computer", FsKind::Local));

        // A pane that is not open has no files.
        let closed = resolve(&host, json!({"path": "/etc/hosts", "pane": 9})).expect_err("closed");
        assert_eq!(closed, ToolError::Unavailable);
    }

    #[test]
    fn read_file_returns_a_bounded_slice_and_says_where_to_continue() {
        let tmp = Tmp::new("slice");
        let table = table_over(&[]);
        let (host, _) = host_of(local_world(&tmp, 1), Vec::new());
        let scope = scope("c1", 1, 1);
        let read = |path: &str, extra: Value| {
            let mut args = json!({"path": path});
            if let (Some(args), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
                args.extend(extra.clone());
            }
            run_call(&table, &host, &scope, "read_file", args)
        };

        let pattern = "0123456789abcdef";
        let big: Vec<u8> = pattern.bytes().cycle().take(40_000).collect();
        let big_path = tmp.write("big.txt", &big);
        let first = read(&big_path, json!({})).expect("a slice");
        assert!(!first.truncated(), "the table's own cut does not apply");
        assert!(first.text().len() <= MAX_RESULT_BYTES);
        let (head, footer) = first.text().rsplit_once('\n').expect("a footer line");
        assert_eq!(head.as_bytes(), &big[..READ_BYTES]);
        assert_eq!(footer, format!("[more text follows: continue with offset {READ_BYTES}]"));

        let second = text_of(read(&big_path, json!({"offset": READ_BYTES})));
        let expected = &big[READ_BYTES..READ_BYTES + 100];
        assert!(second.as_bytes().starts_with(expected), "the next slice starts at that byte");
        let short = text_of(read(&big_path, json!({"limit": 100})));
        assert_eq!(short, format!("{}\n[more text follows: continue with offset 100]", &pattern.repeat(7)[..100]));

        let small = tmp.write("small.txt", b"just a few words");
        assert_eq!(text_of(read(&small, json!({}))), "just a few words", "a short file has no continue line");
        assert_eq!(text_of(read(&small, json!({"limit": 16}))), "just a few words");

        // Three-byte characters: the cut falls on a boundary and the offset counts the bytes consumed.
        let euros = tmp.write("euros.txt", "\u{20ac}".repeat(10_000).as_bytes());
        let cut = text_of(read(&euros, json!({})));
        let (head, footer) = cut.rsplit_once('\n').expect("a footer line");
        assert!(head.chars().all(|c| c == '\u{20ac}'), "no broken character");
        assert_eq!(head.len() % 3, 0);
        assert_eq!(footer, format!("[more text follows: continue with offset {}]", head.len()));
        // An offset inside a character skips the rest of that character.
        let inside = tmp.write("inside.txt", "\u{20ac}\u{20ac}\u{20ac}".as_bytes());
        assert_eq!(text_of(read(&inside, json!({"offset": 1}))), "\u{20ac}\u{20ac}");

        let empty = tmp.write("empty.txt", b"");
        assert_eq!(text_of(read(&empty, json!({}))), "[the file is empty]");
        assert_eq!(text_of(read(&small, json!({"offset": 1000}))), "[no text at that offset]");
    }

    #[test]
    fn read_file_refuses_binary_special_and_missing_files_with_fixed_texts() {
        let tmp = Tmp::new("refuse");
        let table = table_over(&[]);
        let (host, _) = host_of(local_world(&tmp, 1), Vec::new());
        let scope = scope("c1", 1, 1);
        let read = |path: &str| run_call(&table, &host, &scope, "read_file", json!({"path": path}));

        let binary = tmp.write(&format!("{PATH_CANARY}.bin"), b"ab\0cd");
        let folder = tmp.path(&format!("{PATH_CANARY}-dir"));
        std::fs::create_dir_all(&folder).expect("a folder");
        let missing = tmp.path(&format!("{PATH_CANARY}-missing.txt"));
        let mut cases = vec![
            (binary, ToolError::Host(Detail::new(NOT_TEXT))),
            (folder, ToolError::Host(Detail::new(NOT_A_FILE))),
            (missing, ToolError::Host(Detail::new(UNREADABLE))),
        ];
        if cfg!(unix) {
            cases.push(("/dev/null".to_owned(), ToolError::Host(Detail::new(NOT_A_FILE))));
        }
        for (path, expected) in cases {
            let error = read(&path).expect_err(&path);
            assert_eq!(error, expected);
            assert!(!error.to_string().contains(PATH_CANARY), "the refusal quotes the path: {error}");
        }
    }

    #[test]
    fn remote_read_file_resolves_the_folder_first_and_refuses_a_link() {
        let files = MemFiles::default()
            .folder(
                "/srv/app",
                "/srv/app",
                vec![
                    entry("notes.txt", Kind::File, false),
                    entry("link.txt", Kind::File, true),
                    entry("sub", Kind::Dir, false),
                ],
                false,
            )
            .folder("/srv/app/sub", "/home/u/.ssh", vec![entry("key", Kind::File, false)], false)
            .folder("/srv/big", "/srv/big", vec![entry("a", Kind::File, false)], true)
            .file("/srv/app/notes.txt", b"remote words")
            .file("/srv/app/link.txt", b"link bytes")
            .file("/home/u/.ssh/key", b"private bytes");
        let files = Arc::new(files);
        let world = FakeTargets::default().with_files(1, remote_target(Some("/srv/app")), files.clone());
        let (host, world) = host_of(world, Vec::new());
        let table = table_over(&[]);
        let scope = scope("c1", 1, 1);
        let read = |path: &str| run_call(&table, &host, &scope, "read_file", json!({"path": path}));

        assert_eq!(text_of(read("/srv/app/notes.txt")), "remote words");
        assert_eq!(text_of(read("notes.txt")), "remote words", "relative to the working directory");

        assert_eq!(read("/srv/app/link.txt").expect_err("a link"), ToolError::Host(Detail::new(REMOTE_LINK)));
        assert!(!files.read_paths().iter().any(|path| path.ends_with("link.txt")), "the link was read");

        // The lexical check passes, the folder the server resolved is a secret one: refused before any byte is read.
        assert_eq!(read("/srv/app/sub/key").expect_err("resolved"), ToolError::Denied("secret_location"));
        assert!(!files.read_paths().iter().any(|path| path.contains(".ssh")), "a secret file was read");

        assert_eq!(read("/srv/big/zzz").expect_err("too large"), ToolError::Host(Detail::new(FOLDER_TOO_LARGE)));
        assert_eq!(read("/srv/app/missing.txt").expect_err("missing"), ToolError::Host(Detail::new(UNREADABLE)));
        assert_eq!(read("/srv/app/sub").expect_err("a folder"), ToolError::Host(Detail::new(NOT_A_FILE)));

        // A spelled-out secret is refused at the proposal, before a source is even made.
        let before = world.sources_made();
        assert_eq!(read("/srv/app/.env").expect_err("an env file"), ToolError::Denied("env_file"));
        assert_eq!(world.sources_made(), before, "the source was made for a call that never had a card");
    }

    // ---- list_dir -----------------------------------------------------------------------------------------------

    #[test]
    fn list_dir_lists_one_escaped_line_per_entry_within_its_bound() {
        let tmp = Tmp::new("list");
        let table = table_over(&[]);
        let (host, _) = host_of(local_world(&tmp, 1), Vec::new());
        let scope = scope("c1", 1, 1);
        let list = |path: &str, extra: Value| {
            let mut args = json!({"path": path});
            if let (Some(args), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
                args.extend(extra.clone());
            }
            run_call(&table, &host, &scope, "list_dir", args)
        };

        for n in 0..250 {
            tmp.write(&format!("many/f{n:03}.txt"), b"x");
        }
        let many = tmp.path("many");
        let all = text_of(list(&many, json!({})));
        let lines: Vec<&str> = all.lines().collect();
        assert_eq!(lines.len(), 1 + 200 + 1, "the folder, 200 entries and the note");
        assert_eq!(lines[0], many);
        assert_eq!(lines[1], "file 1 f000.txt");
        assert_eq!(lines[201], "[50 more entries not shown]");
        let five = text_of(list(&many, json!({"max_entries": 5})));
        assert_eq!(five.lines().count(), 1 + 5 + 1);
        assert!(five.ends_with("[245 more entries not shown]"), "{five}");

        // Entries whose lines would not fit stop the list early, and the result is within the cap.
        for n in 0..150 {
            tmp.write(&format!("wide/{n:03}{}", "w".repeat(250)), b"x");
        }
        let wide = list(&tmp.path("wide"), json!({"max_entries": 150})).expect("a listing");
        assert!(!wide.truncated() && wide.text().len() <= MAX_RESULT_BYTES, "{}", wide.text().len());
        assert!(wide.text().lines().last().is_some_and(|line| line.ends_with("more entries not shown]")));

        // A file is not a folder and a missing path cannot be read; neither text holds the path.
        let file = tmp.write(&format!("{PATH_CANARY}.txt"), b"x");
        let gone = tmp.path(&format!("{PATH_CANARY}-gone"));
        assert_eq!(list(&file, json!({})).expect_err("a file"), ToolError::Host(Detail::new(NOT_A_FOLDER)));
        assert_eq!(list(&gone, json!({})).expect_err("missing"), ToolError::Host(Detail::new(UNREADABLE)));

        // Names with control characters stay one line each; a link is shown as a link and not followed.
        if cfg!(unix) {
            let names = ["line\nbreak", "esc\u{1b}[31m", "bidi\u{202e}txt"];
            for name in names {
                tmp.write(&format!("odd/{name}"), b"x");
            }
            let long = "l".repeat(250);
            tmp.write(&format!("odd/{long}"), b"x");
            std::os::unix::fs::symlink(&many, tmp.0.join("odd").join("pointer")).expect("a link");
            let odd = text_of(list(&tmp.path("odd"), json!({})));
            assert_eq!(odd.lines().count(), 1 + 5, "one line per entry: {odd:?}");
            assert!(odd.chars().all(|c| !c.is_control() || c == '\n'), "a raw control character: {odd:?}");
            for shown in ["line\\nbreak", "esc\\u{1b}[31m", "bidi\\u{202e}txt", &long] {
                assert!(odd.contains(shown), "{shown} is not in {odd:?}");
            }
            assert!(odd.lines().any(|line| line.starts_with("link ") && line.ends_with(" pointer")), "{odd:?}");
        }
    }

    #[test]
    fn list_dir_on_a_remote_folder_that_resolves_into_a_secret_folder_is_refused() {
        let files = MemFiles::default()
            .folder("/srv/app/sub", "/home/u/.ssh", vec![entry("zz-secret-name", Kind::File, false)], false)
            .folder("/srv/app/ok", "/srv/real", vec![entry("a.txt", Kind::File, false)], false)
            .folder("/srv/big", "/srv/big", vec![entry("a.txt", Kind::File, false)], true)
            .stat_of("/srv/app/sub", Kind::Dir, 0)
            .stat_of("/srv/app/ok", Kind::Dir, 0)
            .stat_of("/srv/big", Kind::Dir, 0);
        let world = FakeTargets::default().with_files(1, remote_target(Some("/srv/app")), Arc::new(files));
        let (host, _) = host_of(world, Vec::new());
        let table = table_over(&[]);
        let scope = scope("c1", 1, 1);
        let list = |path: &str| run_call(&table, &host, &scope, "list_dir", json!({"path": path}));

        // The lexical check passes; the folder the server resolved is a secret one, so no name leaves.
        let error = list("/srv/app/sub").expect_err("resolved into a secret folder");
        assert_eq!(error, ToolError::Denied("secret_location"));
        assert!(!error.to_string().contains("zz-secret-name"));
        let shown = text_of(list("/srv/app/ok/"));
        let listed = "a folder that resolves to an allowed place is listed under its name";
        assert_eq!(shown, "/srv/real\nfile 0 a.txt", "{listed}");
        let cut = text_of(list("/srv/big"));
        assert!(cut.ends_with("[the source cut the listing]"), "{cut}");
    }

    // ---- grep ---------------------------------------------------------------------------------------------------

    fn grep_args(pattern: &str, path: &str, extra: Value) -> Value {
        let mut args = json!({"pattern": pattern, "path": path});
        if let (Some(args), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
            args.extend(extra.clone());
        }
        args
    }

    #[test]
    fn grep_finds_literal_lines_and_skips_links_binaries_large_denied_and_git_files() {
        let tmp = Tmp::new("grep");
        let data = tmp.0.join("data");
        let protected = vec![data.clone()];
        let table = table_over(&protected);
        let (host, _) = host_of(local_world(&tmp, 1), protected);
        let scope = scope("c1", 1, 1);
        let grep = |pattern: &str, path: &str, extra: Value| {
            run_call(&table, &host, &scope, "grep", grep_args(pattern, path, extra))
        };

        tmp.write("a.txt", b"one\nNeedle here\nthree needle\n");
        tmp.write("sub/b.rs", b"needle in code\n");
        tmp.write("bin.dat", b"needle\0\0");
        let mut big = b"needle".to_vec();
        big.resize(1_500_000, b'x');
        tmp.write("big.txt", &big);
        for secret in [".git/config", ".env", ".ssh/id_x", "data/x.txt"] {
            tmp.write(secret, b"needle");
        }
        let mut skipped = 6;
        if cfg!(unix) {
            std::os::unix::fs::symlink(tmp.0.join("sub"), tmp.0.join("link")).expect("a link to a folder");
            std::os::unix::fs::symlink(tmp.0.join("a.txt"), tmp.0.join("linkfile")).expect("a link to a file");
            skipped += 2;
        }
        let root = tmp.path("");

        let found = text_of(grep("needle", &root, json!({})));
        let footer = format!("[2 matches in 2 files searched; {skipped} entries skipped]");
        assert_eq!(found, format!("a.txt:3: three needle\nsub/b.rs:1: needle in code\n{footer}"));
        let folded = text_of(grep("needle", &root, json!({"ignore_case": true})));
        assert!(folded.contains("a.txt:2: Needle here") && folded.contains("a.txt:3: three needle"), "{folded}");
        let none = text_of(grep(PATH_CANARY, &root, json!({})));
        assert_eq!(none, format!("[0 matches in 2 files searched; {skipped} entries skipped]"));
        assert!(!none.contains(PATH_CANARY), "the pattern is echoed");
        let single = text_of(grep("needle", &tmp.path("a.txt"), json!({})));
        assert_eq!(single, "a.txt:3: three needle\n[1 matches in 1 files searched; 0 entries skipped]");

        // The secret files and the protected folder never reach a line, even when asked for directly.
        for refused in [".env", ".ssh", "data", "data/x.txt"] {
            let error = grep("needle", &tmp.path(refused), json!({})).expect_err(refused);
            assert!(matches!(error, ToolError::Denied(_)), "{refused}: {error:?}");
        }

        // A pattern is a literal, a line is cut and escaped, and a path is not echoed anywhere but in a match.
        let lit = Tmp::new("grep-lit");
        let long = format!("needle{}", "x".repeat(400));
        lit.write("lit.txt", format!("a|b\n.*\nplain\nesc\u{1b}[31mneedle\n{long}\n").as_bytes());
        let (host2, _) = host_of(local_world(&lit, 1), Vec::new());
        let table2 = table_over(&[]);
        let grep2 = |pattern: &str| {
            run_call(&table2, &host2, &scope, "grep", grep_args(pattern, &lit.path("lit.txt"), json!({})))
        };
        assert!(text_of(grep2(".*")).starts_with("lit.txt:2: .*\n[1 matches"));
        assert!(text_of(grep2("a|b")).starts_with("lit.txt:1: a|b\n[1 matches"));
        let cut = text_of(grep2("needle"));
        let lines: Vec<&str> = cut.lines().collect();
        assert_eq!(lines[0], "lit.txt:4: esc\\u{1b}[31mneedle", "the escape is spelled out: {cut:?}");
        let bound = "lit.txt:5: ".len() + LINE_CHARS + 1;
        assert_eq!(lines[1].chars().count(), bound, "cut at the line bound plus an ellipsis");
        assert!(lines[1].ends_with('\u{2026}'));
    }

    /// A search over files in memory: a folder of `count` files of `text` on a remote host.
    fn remote_search(files: MemFiles, limits: GrepLimits) -> (ShellTools, ApprovalTable, Arc<FakeTargets>) {
        let world = FakeTargets::default().with_files(1, remote_target(Some("/srv")), Arc::new(files));
        let (host, world) = host_of(world, Vec::new());
        (host.with_limits(limits), table_over(&[]), world)
    }

    #[test]
    fn grep_stops_at_its_limits_and_on_cancel_and_says_why() {
        let scope = scope("c1", 1, 1);
        let any = GrepLimits { files: 1000, bytes: 1 << 30, depth: 8, budget: Duration::from_secs(60) };
        let search = |files: MemFiles, limits: GrepLimits, extra: Value| {
            let (host, table, _) = remote_search(files, limits);
            text_of(run_call(&table, &host, &scope, "grep", grep_args("needle", "/srv/p", extra)))
        };

        // Matches: two lines of five, then the reason.
        let five = "needle 1\nneedle 2\nneedle 3\nneedle 4\nneedle 5\n";
        let out = search(MemFiles::default().with_files("/srv/p", 1, five), any, json!({"max_matches": 2}));
        let expected = "f000.txt:1: needle 1\nf000.txt:2: needle 2\n[2 matches in 1 files searched; 0 entries skipped]";
        assert_eq!(out, format!("{expected}\n[stopped: match limit]"));

        // Files: three of ten.
        let ten = MemFiles::default().with_files("/srv/p", 10, "needle\n");
        let out = search(ten, GrepLimits { files: 3, ..any }, json!({}));
        assert!(out.ends_with("[3 matches in 3 files searched; 0 entries skipped]\n[stopped: file limit]"), "{out}");

        // Bytes: the third file would pass the limit.
        let ten = MemFiles::default().with_files("/srv/p", 10, "needle\n");
        let out = search(ten, GrepLimits { bytes: 20, ..any }, json!({}));
        assert!(out.ends_with("[2 matches in 2 files searched; 0 entries skipped]\n[stopped: size limit]"), "{out}");

        // Depth: a folder two levels down is skipped and counted, and the search is not stopped.
        let tree = MemFiles::default()
            .folder("/srv/p", "/srv/p", vec![entry("a", Kind::Dir, false)], false)
            .folder(
                "/srv/p/a",
                "/srv/p/a",
                vec![entry("b", Kind::Dir, false), entry("one.txt", Kind::File, false)],
                false,
            )
            .folder("/srv/p/a/b", "/srv/p/a/b", vec![entry("two.txt", Kind::File, false)], false)
            .file("/srv/p/a/one.txt", b"needle\n")
            .file("/srv/p/a/b/two.txt", b"needle\n")
            .stat_of("/srv/p", Kind::Dir, 0);
        let out = search(tree, GrepLimits { depth: 1, ..any }, json!({}));
        assert_eq!(out, "a/one.txt:1: needle\n[1 matches in 1 files searched; 1 entries skipped]");

        // Time: a budget of nothing reads no file.
        let files = Arc::new(MemFiles::default().with_files("/srv/p", 3, "needle\n"));
        let world = FakeTargets::default().with_files(1, remote_target(Some("/srv")), files.clone());
        let (host, _) = host_of(world, Vec::new());
        let host = host.with_limits(GrepLimits { budget: Duration::ZERO, ..any });
        let table = table_over(&[]);
        let out = text_of(run_call(&table, &host, &scope, "grep", grep_args("needle", "/srv/p", json!({}))));
        assert_eq!(out, "[0 matches in 0 files searched; 0 entries skipped]\n[stopped: time limit]");
        assert!(files.read_paths().is_empty(), "a file was read after the budget ran out");

        // The defaults of a remote host are the small ones.
        assert_eq!((GrepLimits::remote().files, GrepLimits::local().files), (400, 5000));
        assert!(GrepLimits::remote().bytes < GrepLimits::local().bytes);
        let many = MemFiles::default().with_files("/srv/p", 401, "needle\n");
        let world = FakeTargets::default().with_files(1, remote_target(Some("/srv")), Arc::new(many));
        let (host, _) = host_of(world, Vec::new());
        let args = grep_args("needle", "/srv/p", json!({"max_matches": 200}));
        let out = text_of(run_call(&table_over(&[]), &host, &scope, "grep", args));
        assert!(out.contains("files searched") && out.len() <= MAX_RESULT_BYTES);
        let lines = out.lines().filter(|line| line.starts_with('f')).count();
        assert!(lines > 0 && lines <= 200);

        // The whole output stays within the cap when every line is long.
        let wide = format!("needle{}\n", "y".repeat(240)).repeat(100);
        let out = run_call(
            &table_over(&[]),
            &remote_search(MemFiles::default().with_files("/srv/p", 1, &wide), any).0,
            &scope,
            "grep",
            grep_args("needle", "/srv/p", json!({"max_matches": 200})),
        )
        .expect("a search");
        assert!(!out.truncated() && out.text().len() <= MAX_RESULT_BYTES);
        assert!(out.text().ends_with("[stopped: output limit]"), "{}", out.text());

        // A cancel raised while the second file is read ends the call before the next read.
        let token = CancelToken::new();
        let ten = MemFiles::default().with_files("/srv/p", 10, "needle\n");
        let tripwire = Arc::new(Tripwire::new(ten, token.clone(), 2));
        let world = FakeTargets::default().with_files(1, remote_target(Some("/srv")), tripwire.clone());
        let (host, _) = host_of(world, Vec::new());
        let table = table_over(&[]);
        let args = grep_args("needle", "/srv/p", json!({}));
        let call = ToolCall { id: "call".to_owned(), name: "grep".to_owned(), args };
        let pending = table.propose(&host, &scope, &call).expect("a card");
        table.decide("c1", "call", Decision::Allow, &pending.hash).expect("decided");
        assert_eq!(table.run(&host, &scope, "call", &token).expect_err("cancelled"), ToolError::Cancelled);
        assert!(tripwire.reads() <= 3, "{} reads", tripwire.reads());
    }

    /// Files that raise a cancel token when the `after`-th read has happened.
    struct Tripwire {
        inner: MemFiles,
        token: CancelToken,
        after: usize,
        reads: AtomicUsize,
    }

    impl Tripwire {
        fn new(inner: MemFiles, token: CancelToken, after: usize) -> Tripwire {
            Tripwire { inner, token, after, reads: AtomicUsize::new(0) }
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    impl FileSource for Tripwire {
        fn label(&self) -> String {
            self.inner.label()
        }

        fn home(&self) -> CoreResult<String> {
            self.inner.home()
        }

        fn list(&self, path: &str) -> CoreResult<Listing> {
            self.inner.list(path)
        }

        fn stat(&self, path: &str) -> CoreResult<Entry> {
            self.inner.stat(path)
        }

        fn read(&self, path: &str, limit: u64) -> CoreResult<Vec<u8>> {
            let done = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            if done >= self.after {
                self.token.cancel();
            }
            self.inner.read(path, limit)
        }

        fn read_range(&self, path: &str, offset: u64, len: u64) -> CoreResult<Vec<u8>> {
            self.inner.read_range(path, offset, len)
        }

        fn write(&self, path: &str, bytes: &[u8]) -> CoreResult<()> {
            self.inner.write(path, bytes)
        }
    }

    // ---- read_pane ----------------------------------------------------------------------------------------------

    fn pane_text(text: &str) -> Result<Option<TextRead>, RowsFault> {
        Ok(Some(TextRead { text: text.to_owned(), hidden: Vec::new(), truncated: false }))
    }

    fn read_pane_args(pane: PaneId, rows: u64) -> Value {
        json!({"pane": pane, "rows": rows})
    }

    #[test]
    fn read_pane_needs_the_share_at_proposal_and_again_at_execution() {
        let world = FakeTargets::default()
            .with_files(1, local_target(None, None), Arc::new(MemFiles::default()))
            .with_terminal(1, false)
            .with_terminal(5, false)
            .with_files(6, local_target(None, None), Arc::new(MemFiles::default()));
        world.set_rows(5, pane_text("first\nsecond\n"));
        let (host, world) = host_of(world, Vec::new());
        let table = table_over(&[]);
        let scope = scope("c1", 1, 1);
        let call = |id: &str, pane: PaneId| ToolCall {
            id: id.to_owned(),
            name: "read_pane".to_owned(),
            args: read_pane_args(pane, 20),
        };

        // No share: refused when the call is proposed, no card, and the engine is never asked.
        assert_eq!(table.propose(&host, &scope, &call("a", 5)).expect_err("not shared"), ToolError::NotShared);
        assert!(table.pending("c1").is_empty());
        assert!(world.rows_calls().is_empty());
        // The share of another chat, or of the pane the call acts for by default, does not count.
        world.share("c2", 5);
        world.share("c1", 1);
        assert_eq!(table.propose(&host, &scope, &call("b", 5)).expect_err("not shared"), ToolError::NotShared);

        // Shared: the card names the fields of the tool, and the text comes back without its last line break.
        world.share("c1", 5);
        let card = table.propose(&host, &scope, &call("c", 5)).expect("a card");
        let names: Vec<&str> = card.payload.iter().map(|field| field.name).collect();
        assert_eq!(names, ["tool", "pane", "target", "remote", "rows"]);
        table.decide("c1", "c", Decision::Allow, &card.hash).expect("decided");
        let output = table.run(&host, &scope, "c", &CancelToken::new()).expect("a result");
        assert_eq!(output.text(), "first\nsecond");
        assert_eq!(world.rows_calls(), [(5, 20, PANE_FETCH_BYTES)]);

        // A share that ended while the card waited: the engine is never read.
        let card = table.propose(&host, &scope, &call("d", 5)).expect("a card");
        table.decide("c1", "d", Decision::Allow, &card.hash).expect("decided");
        world.unshare("c1", 5);
        let error = table.run(&host, &scope, "d", &CancelToken::new()).expect_err("not shared any more");
        assert_eq!(error, ToolError::NotShared);
        assert_eq!(world.rows_calls().len(), 1, "the engine was read after the share ended");

        // A pane that is a file browser has no terminal.
        assert_eq!(table.propose(&host, &scope, &call("e", 6)).expect_err("no terminal"), ToolError::Unavailable);
    }

    #[test]
    fn read_pane_drops_hidden_cells_keeps_the_tail_and_maps_faults_to_fixed_texts() {
        let world = FakeTargets::default().with_terminal(1, false);
        world.share("c1", 1);
        let (host, world) = host_of(world, Vec::new());
        let table = table_over(&[]);
        let scope = scope("c1", 1, 1);
        let read = |rows: Result<Option<TextRead>, RowsFault>| {
            world.set_rows(1, rows);
            run_call(&table, &host, &scope, "read_pane", read_pane_args(1, 100))
        };

        // A concealed word is gone; ranges that are reversed, outside the text or inside a character change nothing
        // else and do not panic.
        let hidden = TextRead { text: "visible SECRETWORD text".to_owned(), hidden: vec![8..18], truncated: false };
        assert_eq!(text_of(read(Ok(Some(hidden)))), "visible  text");
        #[allow(clippy::reversed_empty_ranges)]
        let odd = vec![5..3, 100..200, 2..3];
        let odd = TextRead { text: "a\u{e9}b".to_owned(), hidden: odd, truncated: false };
        assert_eq!(text_of(read(Ok(Some(odd)))), "ab");

        // A long pane gives its tail from a line boundary, with a first line that says so.
        let lines: String = (0..20_000).map(|n| format!("line {n}\n")).collect();
        assert!(lines.len() > 100 * 1024);
        let tail = text_of(read(pane_text(&lines)));
        assert!(tail.len() <= READ_BYTES, "{}", tail.len());
        assert!(tail.starts_with("[earlier output omitted]\nline "));
        assert!(tail.ends_with("line 19999"));
        let second = tail.lines().nth(1).expect("a line");
        assert!(second.trim_start_matches("line ").parse::<u32>().is_ok(), "a cut line: {second}");

        // Nothing to show.
        assert_eq!(text_of(read(Ok(None))), "[the pane shows nothing]");
        assert_eq!(text_of(read(pane_text(""))), "[the pane shows nothing]");
        assert_eq!(text_of(read(pane_text("\n\n"))), "[the pane shows nothing]");

        // Every fault is a fixed phrase.
        for (fault, phrase) in [
            (RowsFault::Gone, PANE_GONE),
            (RowsFault::Busy, PANE_BUSY),
            (RowsFault::Other, PANE_UNREADABLE),
        ] {
            assert_eq!(read(Err(fault)).expect_err("a fault"), ToolError::Host(Detail::new(phrase)));
        }
    }

    #[test]
    fn no_debug_output_of_the_host_or_its_targets_shows_a_path_a_pattern_or_text() {
        let target = FileTarget {
            source: "local".to_owned(),
            label: format!("{PATH_CANARY}-label"),
            remote: true,
            home: Some(format!("/{PATH_CANARY}-home")),
            cwd: Some(format!("/{PATH_CANARY}-cwd")),
        };
        let terminal = TerminalTarget { label: format!("{PATH_CANARY}-term"), remote: false };
        let policy = DenyPolicy::new(vec![PathBuf::from(format!("/{PATH_CANARY}"))]);
        let bound = ShellTools::with_targets(policy, Arc::new(FakeTargets::default()));
        let unbound = ShellTools::new(DenyPolicy::new(Vec::new()));
        let core = CoreTargets::new(Weak::new());
        let shown = [
            format!("{bound:?} {bound:#?}"),
            format!("{unbound:?} {unbound:#?}"),
            format!("{target:?} {target:#?}"),
            format!("{terminal:?} {terminal:#?}"),
            format!("{:?} {:#?}", RowsFault::Busy, RowsFault::Gone),
            format!("{:?} {:#?}", GrepLimits::local(), GrepLimits::remote()),
            format!("{core:?} {core:#?}"),
        ];
        for text in &shown {
            assert!(!text.contains(PATH_CANARY), "a Debug line shows a path or a label: {text}");
        }
        // Each type that holds a path or a label prints a flag or a number instead.
        for (text, expected) in shown.iter().zip(["bound: true", "bound: false", "remote: true", "remote: false"]) {
            assert!(text.contains(expected), "{text}");
        }
        assert!(shown[5].contains("5000") && shown[5].contains("400"), "{}", shown[5]);
        assert!(shown[6].contains("alive: false"), "{}", shown[6]);
    }
}

/// The way a call goes, for the tests of this module and of the write module: they share it so that the approval table is
/// decided in this file only.
#[cfg(test)]
pub(crate) mod rig {
    use super::*;

    use holzwave_core::ai::tools::ApprovalTable;
    use holzwave_core::ai::types::ToolCall;
    use serde_json::Value;

    /// Proposes a call, decides on the card's hash and runs it: the whole way a call goes, for a host under test.
    pub(crate) fn run_call(
        table: &ApprovalTable,
        host: &ShellTools,
        scope: &Scope,
        name: &str,
        args: Value,
    ) -> Result<ToolOutput, ToolError> {
        run_call_between(table, host, scope, name, args, &|| {})
    }

    /// [`run_call`] for the call named `id`, so that several calls of one chat can be told apart.
    pub(crate) fn run_call_as(
        table: &ApprovalTable,
        host: &ShellTools,
        scope: &Scope,
        id: &str,
        name: &str,
        args: Value,
    ) -> Result<ToolOutput, ToolError> {
        let call = ToolCall { id: id.to_owned(), name: name.to_owned(), args };
        let pending = table.propose(host, scope, &call)?;
        table.decide(&scope.conversation, id, Decision::Allow, &pending.hash)?;
        table.run(host, scope, id, &CancelToken::new())
    }

    /// [`run_call`] with a step between the decision and the run, in which a test changes the world: a file that another
    /// program writes after the person approved the card.
    pub(crate) fn run_call_between(
        table: &ApprovalTable,
        host: &ShellTools,
        scope: &Scope,
        name: &str,
        args: Value,
        between: &dyn Fn(),
    ) -> Result<ToolOutput, ToolError> {
        let call = ToolCall { id: "call".to_owned(), name: name.to_owned(), args };
        let pending = table.propose(host, scope, &call)?;
        table.decide(&scope.conversation, "call", Decision::Allow, &pending.hash)?;
        between();
        table.run(host, scope, "call", &CancelToken::new())
    }
}
