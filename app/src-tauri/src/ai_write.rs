//! The write half of the shell's tool host: it looks at the file a write tool will change and, once the person approved
//! the card, saves what the planner made of the call.
//!
//! The module changes a file only through `Targets::save_text`, which is `Blocks::save_file` with the change time the card
//! showed, so a file that changed meanwhile is refused and left as the other program made it. It opens only the path of the
//! ticket, holds no card, hash or ticket constructor, and never removes, renames or cuts a file: the text it saves is the
//! whole new text. Every refusal is a fixed phrase that holds no path and no file text.

use holzwave_core::ai::provider::CancelToken;
use holzwave_core::ai::tools::{
    Access, Base, Diff, FsKind, Inspected, MAX_BASE_BYTES, Resolved, Style, Ticket, ToolError, ToolOutput, diff, plan,
};
use holzwave_core::files::{FileSource, Kind};

use crate::ai_tools::{
    FOLDER_TOO_LARGE, NOT_A_FILE, NOT_TEXT, REMOTE_LINK, SNIFF_BYTES, ShellTools, UNREADABLE, host, invalid, join_remote,
    live, split_remote,
};

/// The change time a file must have for a save that makes a new file: no file has it, so the save is refused when the file
/// appeared after the card was made and goes ahead while the file is still absent.
pub(crate) const CREATE_ONLY: i64 = i64::MIN;

/// What the model hears after a file was replaced, followed by the counts.
pub(crate) const SAVED: &str = "Saved the file.";
/// What the model hears after a file was made, followed by the counts.
pub(crate) const CREATED: &str = "Created the file.";

/// The fixed phrases of a refusal. Each is one English line and holds nothing the model wrote.
pub(crate) const NO_FOLDER: &str = "the folder of that file does not exist";
pub(crate) const TOO_BIG: &str = "that file is larger than the agent may change";
pub(crate) const NOT_THE_REAL_PATH: &str =
    "that path goes through a link or a relative segment on a remote host; list the folder and use its real path";
pub(crate) const SAVE_REFUSED: &str = "the file changed or could not be saved; nothing was written";

/// The state of the file a write tool will change: its text, or that there is none, and its change time. Called after
/// the table's walls have passed the path, at proposal and again when the call runs.
pub(crate) fn inspect(tools: &ShellTools, resolved: &Resolved) -> Result<Inspected, ToolError> {
    let world = tools.world()?;
    let pane = resolved.pane();
    let target = world.file_target(pane).ok_or(ToolError::PayloadChanged)?;
    if target.remote != (resolved.fs() == FsKind::Remote) {
        return Err(ToolError::PayloadChanged);
    }
    let path = resolved.path().ok_or(ToolError::Unavailable)?;
    let source = world.file_source(pane).ok_or(ToolError::Unavailable)?;
    match resolved.fs() {
        FsKind::Local => inspect_local(source.as_ref(), path),
        FsKind::Remote => inspect_remote(tools, source.as_ref(), path),
    }
}

/// A file of this computer. The path is the canonical one the table made, so no link is followed here.
fn inspect_local(source: &dyn FileSource, path: &str) -> Result<Inspected, ToolError> {
    match source.stat(path) {
        Ok(entry) => {
            if entry.kind != Kind::File {
                return Err(host(NOT_A_FILE));
            }
            if entry.size > MAX_BASE_BYTES as u64 {
                return Err(host(TOO_BIG));
            }
            let text = read_text(source, path)?;
            Ok(Inspected::new(Base::Text(text), entry.modified))
        }
        // No such file, or one that cannot be looked at: a file is made only in a folder that is there.
        Err(_) => {
            let folder = std::path::Path::new(path).parent().and_then(|parent| parent.to_str()).filter(|f| !f.is_empty());
            match folder.map(|folder| source.stat(folder)) {
                Some(Ok(entry)) if entry.kind == Kind::Dir => Ok(Inspected::new(Base::Absent, None)),
                _ => Err(host(NO_FOLDER)),
            }
        }
    }
}

/// A file of a remote host. The folder is listed first and must be the real folder: a path through a link, a `.` or `..`
/// segment or a doubled slash is refused, because the server would resolve it to a place the deny list never saw.
fn inspect_remote(tools: &ShellTools, source: &dyn FileSource, path: &str) -> Result<Inspected, ToolError> {
    if !path.starts_with('/') || path.contains("//") || path.split('/').any(|part| part == "." || part == "..") {
        return Err(host(NOT_THE_REAL_PATH));
    }
    let (folder, name) = split_remote(path);
    if name.is_empty() {
        return Err(host(NOT_THE_REAL_PATH));
    }
    let listing = source.list(folder).map_err(|_| host(UNREADABLE))?;
    if listing.path != folder {
        return Err(host(NOT_THE_REAL_PATH));
    }
    let real = join_remote(&listing.path, name);
    tools
        .policy()
        .check(Style::Posix, FsKind::Remote, &real, Access::Write)
        .map_err(|denied| ToolError::Denied(denied.rule()))?;
    let Some(entry) = listing.entries.iter().find(|entry| entry.name == name) else {
        return if listing.truncated { Err(host(FOLDER_TOO_LARGE)) } else { Ok(Inspected::new(Base::Absent, None)) };
    };
    if entry.link {
        return Err(host(REMOTE_LINK));
    }
    if entry.kind != Kind::File {
        return Err(host(NOT_A_FILE));
    }
    if entry.size > MAX_BASE_BYTES as u64 {
        return Err(host(TOO_BIG));
    }
    let text = read_text(source, &real)?;
    Ok(Inspected::new(Base::Text(text), entry.modified))
}

/// The text of a file of at most [`MAX_BASE_BYTES`] bytes: no NUL in its first bytes and valid UTF-8, or it is not text.
fn read_text(source: &dyn FileSource, path: &str) -> Result<String, ToolError> {
    let bytes = source.read_range(path, 0, MAX_BASE_BYTES as u64 + 1).map_err(|_| host(UNREADABLE))?;
    if bytes.len() > MAX_BASE_BYTES {
        return Err(host(TOO_BIG));
    }
    if bytes.iter().take(SNIFF_BYTES).any(|byte| *byte == 0) {
        return Err(host(NOT_TEXT));
    }
    String::from_utf8(bytes).map_err(|_| host(NOT_TEXT))
}

/// Saves what the person approved. The text is the one the planner makes from the ticket's tool and the base the table
/// looked at when the call ran, and the save carries the change time the card showed, so a file that changed in the
/// meantime is not overwritten.
pub(crate) fn run(tools: &ShellTools, ticket: &Ticket, cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
    let world = tools.world()?;
    let record = ticket.resolved();
    let target = world.file_target(record.pane()).ok_or(ToolError::PayloadChanged)?;
    if target.remote != (record.fs() == FsKind::Remote) {
        return Err(ToolError::PayloadChanged);
    }
    let base = record.base().ok_or(ToolError::Unavailable)?;
    let change = plan(ticket.tool(), base).map_err(|fault| invalid(fault.phrase()))?;
    let counts = diff(&change).map_err(|fault| invalid(fault.phrase()))?;
    let path = record.path().ok_or(ToolError::Unavailable)?;
    let expected = match base {
        Base::Absent => Some(CREATE_ONLY),
        Base::Text(_) => record.stamp(),
    };
    live(cancel)?;
    world.save_text(&target.source, path, &change.after, expected).map_err(|_| host(SAVE_REFUSED))?;
    Ok(ToolOutput::new(outcome(matches!(base, Base::Absent), &counts)))
}

/// What the model is told: that the file was saved or made, and how many lines were added and removed.
fn outcome(created: bool, diff: &Diff) -> String {
    let lines = if diff.added == 1 { "line" } else { "lines" };
    format!("{} {} {lines} added, {} removed.", if created { CREATED } else { SAVED }, diff.added, diff.removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::{BTreeMap, BTreeSet, HashSet};
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use holzwave_core::ai::tools::{ApprovalTable, DenyPolicy, Scope, specs};
    use holzwave_core::files::{Entry, Listing};
    use holzwave_core::layout::PaneId;
    use holzwave_core::{Error, Result as CoreResult};
    use serde_json::{Value, json};

    use crate::ai_tools::tests::run_call;
    use crate::ai_tools::{FileTarget, RowsFault, SaveFault, TerminalTarget, Targets};

    use holzwave_core::ai::tools::{ToolHost, ToolName};
    use holzwave_core::textread::TextRead;

    /// Text of a file and of a path in these tests; they appear in test code only, so a hit in an output is a leak.
    const OLD_TEXT: &str = "zq-old-text-4d2e91";
    const NEW_TEXT: &str = "zq-new-text-b83f17";
    const PATH_CANARY: &str = "zq-write-path-5c2e08";

    /// What one file of the rig holds.
    #[derive(Clone)]
    struct Held {
        bytes: Vec<u8>,
        modified: i64,
    }

    /// The files of one machine in memory, with a change time per file, and a count of the bytes it was asked to read.
    #[derive(Default)]
    struct Mem {
        files: Mutex<BTreeMap<String, Held>>,
        folders: Mutex<BTreeSet<String>>,
        links: Mutex<BTreeSet<String>>,
        listings: Mutex<BTreeMap<String, Listing>>,
        reads: AtomicUsize,
        stats: AtomicUsize,
    }

    impl Mem {
        fn put(&self, path: &str, bytes: &[u8], modified: i64) {
            self.files.lock().unwrap_or_else(PoisonError::into_inner).insert(path.to_owned(), Held { bytes: bytes.to_vec(), modified });
        }

        fn folder(&self, path: &str) {
            self.folders.lock().unwrap_or_else(PoisonError::into_inner).insert(path.to_owned());
        }

        fn remove(&self, path: &str) {
            self.files.lock().unwrap_or_else(PoisonError::into_inner).remove(path);
        }

        fn text(&self, path: &str) -> Option<String> {
            self.files
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(path)
                .map(|held| String::from_utf8_lossy(&held.bytes).into_owned())
        }

        fn modified(&self, path: &str) -> Option<i64> {
            self.files.lock().unwrap_or_else(PoisonError::into_inner).get(path).map(|held| held.modified)
        }

        /// A folder as a remote server lists it: `asked` resolves to `resolved` and holds `entries`.
        fn list_as(&self, asked: &str, resolved: &str, entries: Vec<Entry>, truncated: bool) {
            let listing = Listing { path: resolved.to_owned(), parent: None, entries, truncated };
            self.listings.lock().unwrap_or_else(PoisonError::into_inner).insert(asked.to_owned(), listing);
        }

        fn read_count(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    fn entry(name: &str, kind: Kind, link: bool, size: u64, modified: Option<i64>) -> Entry {
        Entry { name: name.to_owned(), kind, link, size, modified, hidden: false }
    }

    impl FileSource for Mem {
        fn label(&self) -> String {
            "memory".to_owned()
        }

        fn home(&self) -> CoreResult<String> {
            Ok("/".to_owned())
        }

        fn list(&self, path: &str) -> CoreResult<Listing> {
            self.listings
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(path)
                .cloned()
                .ok_or_else(|| Error::Unsupported("no such folder".into()))
        }

        fn stat(&self, path: &str) -> CoreResult<Entry> {
            self.stats.fetch_add(1, Ordering::SeqCst);
            if let Some(held) = self.files.lock().unwrap_or_else(PoisonError::into_inner).get(path) {
                let name = path.rsplit('/').next().unwrap_or("");
                return Ok(entry(name, Kind::File, false, held.bytes.len() as u64, Some(held.modified)));
            }
            if self.folders.lock().unwrap_or_else(PoisonError::into_inner).contains(path) {
                return Ok(entry(path, Kind::Dir, false, 0, Some(1)));
            }
            Err(Error::Unsupported("no such entry".into()))
        }

        fn read(&self, path: &str, limit: u64) -> CoreResult<Vec<u8>> {
            self.read_range(path, 0, limit)
        }

        fn read_range(&self, path: &str, offset: u64, len: u64) -> CoreResult<Vec<u8>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
            let held = files.get(path).ok_or_else(|| Error::Unsupported("no such file".into()))?;
            let start = usize::try_from(offset).unwrap_or(usize::MAX).min(held.bytes.len());
            let end = start.saturating_add(usize::try_from(len).unwrap_or(usize::MAX)).min(held.bytes.len());
            Ok(held.bytes[start..end].to_vec())
        }

        fn write(&self, _path: &str, _bytes: &[u8]) -> CoreResult<()> {
            Err(Error::Unsupported("saves go through the targets".into()))
        }
    }

    /// One call of `save_text`.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Save {
        source: String,
        path: String,
        text: String,
        expected: Option<i64>,
    }

    /// The panes of a rig: which are open, what their files are, and every save the host makes. A save follows the rule of
    /// `Blocks::save_file`: a file whose change time differs from `expected` is refused.
    #[derive(Default)]
    struct Stage {
        open: HashSet<PaneId>,
        files: BTreeMap<PaneId, (FileTarget, Arc<Mem>)>,
        terminals: HashSet<PaneId>,
        saves: Mutex<Vec<Save>>,
        clock: AtomicI64,
        refuse: Mutex<Option<SaveFault>>,
        appear: Mutex<Option<(String, String)>>,
    }

    impl Stage {
        fn with_files(mut self, pane: PaneId, target: FileTarget, mem: Arc<Mem>) -> Stage {
            self.open.insert(pane);
            self.files.insert(pane, (target, mem));
            self
        }

        fn with_terminal(mut self, pane: PaneId) -> Stage {
            self.open.insert(pane);
            self.terminals.insert(pane);
            self
        }

        fn saves(&self) -> Vec<Save> {
            self.saves.lock().unwrap_or_else(PoisonError::into_inner).clone()
        }

        fn refuse_with(&self, fault: Option<SaveFault>) {
            *self.refuse.lock().unwrap_or_else(PoisonError::into_inner) = fault;
        }

        /// The next save finds `text` at `path`, as if another program made the file just before.
        fn appear_before_save(&self, path: &str, text: &str) {
            *self.appear.lock().unwrap_or_else(PoisonError::into_inner) = Some((path.to_owned(), text.to_owned()));
        }

        fn mem_of(&self, source: &str) -> Option<Arc<Mem>> {
            self.files.values().find(|(target, _)| target.source == source).map(|(_, mem)| mem.clone())
        }
    }

    impl Targets for Stage {
        fn is_open(&self, pane: PaneId) -> bool {
            self.open.contains(&pane)
        }

        fn file_target(&self, pane: PaneId) -> Option<FileTarget> {
            self.files.get(&pane).map(|(target, _)| target.clone())
        }

        fn file_source(&self, pane: PaneId) -> Option<Arc<dyn FileSource>> {
            self.files.get(&pane).map(|(_, mem)| mem.clone() as Arc<dyn FileSource>)
        }

        fn terminal(&self, pane: PaneId) -> Option<TerminalTarget> {
            self.terminals.contains(&pane).then(|| TerminalTarget { label: "This computer".to_owned(), remote: false })
        }

        fn read_rows(&self, _pane: PaneId, _rows: u16, _max: usize) -> Result<Option<TextRead>, RowsFault> {
            Ok(None)
        }

        fn shared(&self, _conversation: &str, _pane: PaneId) -> bool {
            false
        }

        fn save_text(&self, source: &str, path: &str, text: &str, expected: Option<i64>) -> Result<Option<i64>, SaveFault> {
            self.saves.lock().unwrap_or_else(PoisonError::into_inner).push(Save {
                source: source.to_owned(),
                path: path.to_owned(),
                text: text.to_owned(),
                expected,
            });
            if let Some(fault) = *self.refuse.lock().unwrap_or_else(PoisonError::into_inner) {
                return Err(fault);
            }
            let mem = self.mem_of(source).ok_or(SaveFault::Other)?;
            if let Some((appeared, body)) = self.appear.lock().unwrap_or_else(PoisonError::into_inner).take() {
                mem.put(&appeared, body.as_bytes(), 5);
            }
            if let (Some(expected), Some(now)) = (expected, mem.modified(path)) {
                if now != expected {
                    return Err(SaveFault::Refused);
                }
            }
            let modified = 1_800_000_000 + self.clock.fetch_add(1, Ordering::SeqCst);
            mem.put(path, text.as_bytes(), modified);
            Ok(Some(modified))
        }

        fn open_source(&self, source: &str) -> Option<Arc<dyn FileSource>> {
            self.mem_of(source).map(|mem| mem as Arc<dyn FileSource>)
        }
    }

    fn target(source: &str, label: &str, remote: bool) -> FileTarget {
        FileTarget {
            source: source.to_owned(),
            label: label.to_owned(),
            remote,
            home: (!remote).then(|| "/home/u".to_owned()),
            cwd: Some("/w".to_owned()),
        }
    }

    /// This computer on pane 1 with `/w` as a folder, over `mem`.
    fn this_computer(mem: &Arc<Mem>) -> (ShellTools, Arc<Stage>) {
        mem.folder("/w");
        let stage = Arc::new(Stage::default().with_files(1, target("local", "This computer", false), mem.clone()));
        (ShellTools::with_targets(DenyPolicy::new(Vec::new()), stage.clone()), stage)
    }

    fn scope(pane: PaneId) -> Scope {
        Scope { conversation: "chat-1".to_owned(), turn: 1, pane }
    }

    fn write_args(path: &str, content: &str) -> Value {
        json!({"path": path, "content": content})
    }

    fn table() -> ApprovalTable {
        ApprovalTable::new(DenyPolicy::new(Vec::new()))
    }

    #[test]
    fn the_host_offers_the_write_tool_where_it_offers_read_file() {
        let unbound = ShellTools::new(DenyPolicy::new(Vec::new()));
        assert!(!unbound.available(ToolName::WriteFile, 1), "an unbound host offers nothing");

        let mem = Arc::new(Mem::default());
        let stage = Stage::default()
            .with_files(1, target("local", "This computer", false), mem.clone())
            .with_terminal(1)
            .with_terminal(3)
            .with_files(4, target("box", "build box", true), mem);
        let host = ShellTools::with_targets(DenyPolicy::new(Vec::new()), Arc::new(stage));
        assert!(host.available(ToolName::WriteFile, 1), "this computer has files and a terminal");
        assert!(!host.available(ToolName::WriteFile, 3), "a terminal with no file target has no file to write");
        assert!(host.available(ToolName::WriteFile, 4), "a file browser has files");
        assert!(!host.available(ToolName::WriteFile, 2), "a pane that is not open");
        for tool in [ToolName::EditFile, ToolName::RunCommand] {
            assert!(!host.available(tool, 1), "{tool:?} is not built yet");
        }
        let declared: Vec<String> = specs(&host, 1).into_iter().map(|spec| spec.name).collect();
        assert_eq!(declared, ["read_file", "list_dir", "grep", "read_pane", "write_file"]);
    }

    #[test]
    fn inspect_reads_a_local_text_file_and_gives_its_time() {
        let mem = Arc::new(Mem::default());
        mem.put("/w/notes.txt", format!("alpha {OLD_TEXT}\n").as_bytes(), 1_700_000_123);
        let (host, _) = this_computer(&mem);
        let scope = scope(1);
        let resolve = |path: &str| {
            let tool = holzwave_core::ai::tools::Tool::parse("write_file", &write_args(path, "x")).expect("arguments");
            host.resolve(&scope, &tool).expect("a record")
        };

        let found = host.inspect(&resolve("/w/notes.txt")).expect("inspected").expect("a state");
        assert_eq!(found.base(), &Base::Text(format!("alpha {OLD_TEXT}\n")));
        assert_eq!(found.stamp(), Some(1_700_000_123), "the change time of its entry");
        assert_eq!(mem.read_count(), 1, "the file is read once per call");

        let missing = host.inspect(&resolve("/w/new.txt")).expect("inspected").expect("a state");
        assert_eq!((missing.base(), missing.stamp()), (&Base::Absent, None), "a new file in a folder that is there");

        let debug = format!("{found:?} {found:#?} {missing:?}");
        assert!(!debug.contains(OLD_TEXT) && !debug.contains("notes.txt"), "{debug}");
        assert!(debug.contains("bytes"), "a size is shown: {debug}");
    }

    #[test]
    fn a_write_goes_through_save_text_with_the_time_the_card_showed() {
        let mem = Arc::new(Mem::default());
        mem.put(&format!("/w/{PATH_CANARY}.txt"), format!("alpha\nbeta {OLD_TEXT}\ngamma\n").as_bytes(), 1_700_000_000);
        let (host, stage) = this_computer(&mem);
        let path = format!("/w/{PATH_CANARY}.txt");
        let new = format!("alpha\nbeta {NEW_TEXT}\ngamma\n");

        let output = run_call(&table(), &host, &scope(1), "write_file", write_args(&path, &new)).expect("saved");
        let saves = stage.saves();
        assert_eq!(saves.len(), 1, "exactly one save");
        assert_eq!(
            saves[0],
            Save { source: "local".to_owned(), path: path.clone(), text: new.clone(), expected: Some(1_700_000_000) },
            "the source of the pane, the resolved path, the content, and the change time of the card"
        );
        assert!(output.text().starts_with("Saved the file."), "{}", output.text());
        assert!(output.text().ends_with("1 line added, 1 removed."), "{}", output.text());
        for canary in [OLD_TEXT, NEW_TEXT, PATH_CANARY] {
            assert!(!output.text().contains(canary), "the outcome says too much: {}", output.text());
        }
        assert_eq!(mem.text(&path), Some(new), "the file holds the new text");
    }
}
