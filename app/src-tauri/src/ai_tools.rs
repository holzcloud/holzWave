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

use holzwave_core::ai::provider::{CancelToken, Detail};
use holzwave_core::ai::store::ConversationId;
use holzwave_core::ai::tools::{
    Access, Decision, DenyPolicy, FsKind, MAX_RESULT_BYTES, ReadFile, Resolved, Scope, Style, Ticket, Tool, ToolError,
    ToolHost, ToolName, ToolOutput,
};
use holzwave_core::files::{FileSource, Kind};
use holzwave_core::layout::PaneId;
use holzwave_core::textread::{TextRead, TextRegion};
use holzwave_core::Error as CoreError;

use crate::ai_host::AiFault;
use crate::state::AppCore;

/// What a result keeps free for the line that says where to continue.
const FOOTER_RESERVE: usize = 128;

/// The most bytes of file text one `read_file` call returns: what is left of a result after the footer line.
pub const READ_BYTES: usize = MAX_RESULT_BYTES - FOOTER_RESERVE;

/// How many leading bytes of a file are looked at for a NUL, which marks a file as not text.
const SNIFF_BYTES: usize = 8192;

/// The fixed phrases of a refusal. Each is one English line and holds nothing the model wrote.
const NOT_TEXT: &str = "that file is not text";
const NOT_A_FILE: &str = "that path is not a regular file";
const UNREADABLE: &str = "that path could not be read";
const NEEDS_CWD: &str = "give an absolute path, the working directory of that pane is not known";
const REMOTE_ABSOLUTE: &str = "a path on a remote host must be absolute";
const NO_HOME: &str = "the home folder is not known";
const BAD_END: &str = "the path must name a file, not a folder";
const REMOTE_LINK: &str = "a link on a remote host is not followed";
const FOLDER_TOO_LARGE: &str = "that folder is too large to check the file";

fn host(phrase: &str) -> ToolError {
    ToolError::Host(Detail::new(phrase))
}

fn invalid(phrase: &str) -> ToolError {
    ToolError::InvalidArguments(Detail::new(phrase))
}

/// A cancelled turn stops the call before the next file system request.
fn live(cancel: &CancelToken) -> Result<(), ToolError> {
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
#[cfg_attr(not(test), allow(dead_code))]
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
#[cfg_attr(not(test), allow(dead_code))]
pub enum RowsFault {
    /// The pane is closed.
    Gone,
    /// The pane did not answer in time.
    Busy,
    /// Anything else.
    Other,
}

/// The world the tools read through. The shell implements it over the core; tests implement it over tables.
///
/// `is_open`, `file_target`, `terminal` and `shared` look at tables only. `file_source` may make a source, which can
/// open a connection, so only `run` calls it. `read_rows` waits for the engine thread of the pane.
#[cfg_attr(not(test), allow(dead_code))]
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
}

// ---- the host ---------------------------------------------------------------------------------------------------

/// The tool host of the shell. It is built before the core exists and bound to the world once the core does; until
/// then it offers nothing.
pub struct ShellTools {
    policy: DenyPolicy,
    targets: OnceLock<Arc<dyn Targets>>,
}

impl fmt::Debug for ShellTools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShellTools").field("bound", &self.targets.get().is_some()).finish_non_exhaustive()
    }
}

/// How a path ends: a file tool wants a name, a folder tool takes a folder.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ending {
    File,
    Folder,
}

impl ShellTools {
    /// A host with nothing bound: it offers no tool.
    pub fn new(policy: DenyPolicy) -> ShellTools {
        ShellTools { policy, targets: OnceLock::new() }
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

    fn world(&self) -> Result<&Arc<dyn Targets>, ToolError> {
        self.targets.get().ok_or(ToolError::Unavailable)
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
}

impl ToolHost for ShellTools {
    fn available(&self, tool: ToolName, pane: PaneId) -> bool {
        let Ok(world) = self.world() else { return false };
        if !world.is_open(pane) {
            return false;
        }
        match tool {
            ToolName::ReadFile => world.file_target(pane).is_some(),
            ToolName::ListDir | ToolName::Grep | ToolName::ReadPane => false,
            ToolName::WriteFile | ToolName::EditFile | ToolName::RunCommand => false,
        }
    }

    fn resolve(&self, scope: &Scope, tool: &Tool) -> Result<Resolved, ToolError> {
        match tool {
            Tool::ReadFile(args) => self.file_record(scope, tool, &args.path, Ending::File),
            _ => Err(ToolError::Unavailable),
        }
    }

    fn run(&self, ticket: &Ticket, cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        match ticket.tool() {
            Tool::ReadFile(args) => self.read_file(ticket, args, cancel),
            _ => Err(ToolError::Unavailable),
        }
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
fn absolute_path(raw: &str, target: &FileTarget, ending: Ending) -> Result<String, ToolError> {
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
fn join_remote(folder: &str, name: &str) -> String {
    format!("{}/{name}", folder.trim_end_matches('/'))
}

/// A remote file path split at its last `/` into the folder and the name; a name directly under the root has the folder
/// `/`.
fn split_remote(path: &str) -> (&str, &str) {
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
#[cfg_attr(not(test), allow(dead_code))]
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
}

// ---- the verdict ------------------------------------------------------------------------------------------------

impl AppCore {
    /// The page's verdict on one approval card: the chat, the call, `allow` or `deny`, and the hash of the card the
    /// person was shown. The table compares the hash and refuses any other; an answer of a call that is not waiting is
    /// refused too. This is the one place the approval table is decided, and the command of the webview is its only
    /// caller: no other command, event, socket or subcommand reaches it.
    #[cfg_attr(not(test), allow(dead_code))]
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

        fn stat(&self, _path: &str) -> CoreResult<Entry> {
            Err(Error::Unsupported("stat is not used on a remote host".into()))
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

    /// Proposes a call, decides on the card's hash and runs it: the whole way a call goes, for a host under test.
    fn run_call(
        table: &ApprovalTable,
        host: &ShellTools,
        scope: &Scope,
        name: &str,
        args: Value,
    ) -> Result<ToolOutput, ToolError> {
        let call = ToolCall { id: "call".to_owned(), name: name.to_owned(), args };
        let pending = table.propose(host, scope, &call)?;
        table.decide(&scope.conversation, "call", Decision::Allow, &pending.hash)?;
        table.run(host, scope, "call", &CancelToken::new())
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
        assert_eq!(offered(1), ["read_file"], "this computer, with a file target and a terminal");
        assert_eq!(offered(2), Vec::<&str>::new(), "a pane that is not open");
        assert_eq!(offered(3), Vec::<&str>::new(), "a terminal with no file target offers no file tool yet");
        assert_eq!(offered(4), ["read_file"], "a file browser has files and no terminal");
        for tool in [ToolName::WriteFile, ToolName::EditFile, ToolName::RunCommand] {
            assert!(!host.available(tool, 1), "{tool:?} is not built");
        }
        let declared: Vec<String> = specs(&host, 1).into_iter().map(|spec| spec.name).collect();
        assert_eq!(declared, ["read_file"]);
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
}
