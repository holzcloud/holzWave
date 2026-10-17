//! Panes that are not a terminal: a file browser and a preview. The app shell holds one [`Blocks`], which knows what
//! each such pane shows, gives the page the files it asks for, and remembers the panes in `pane_blocks.json` so a
//! restart opens the same views.
//!
//! A block pane is a pane like any other (a header, a place in the split tree, drag and close) that runs no process
//! ([`crate::conn::NullLink`]); the page draws its content over the empty terminal. Which source a block reads from is
//! `local` or the id of a saved SSH connection.
//!
//! Everything a block reads goes through [`FileSource`], with the limits of [`crate::files`]. The page never sees a
//! path it did not ask for and never gets a file larger than [`PREVIEW_LIMIT`] in one piece.
//!
//! A chat pane keeps only the id of its conversation here: the messages belong to the conversation store, and the id is
//! checked by the one definition of an id ([`ConversationId::parse`]) wherever a chat block is stored or read.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::ai::store::ConversationId;
use crate::connections::Connections;
use crate::files::{Entry, FileSource, Listing, LocalSource};
use crate::layout::PaneId;
use crate::persist::{read_limited, write_atomic};
use crate::preview::{Classified, SNIFF, classify, render_markdown};
use crate::{Error, Result};

pub const BLOCKS_FILE: &str = "pane_blocks.json";
const MAX_BYTES: u64 = 256 * 1024;
/// The most a preview or the editor loads in one piece. Larger files are shown in ranges or refused.
pub const PREVIEW_LIMIT: u64 = 128 * 1024 * 1024;
/// The largest text file the editor opens.
pub const EDIT_LIMIT: u64 = 8 * 1024 * 1024;
/// The source name of this computer.
pub const LOCAL: &str = "local";
/// The most grants alive at once; the oldest is dropped for a new one.
pub const MAX_GRANTS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    /// A file browser.
    Files,
    /// The preview of one file.
    Preview,
    /// The system monitor of this computer or of a saved SSH connection. It has no path.
    Monitor,
    /// A conversation with an AI model. Its path is the id of the conversation; the messages are kept by the
    /// conversation store, never in `pane_blocks.json`.
    Chat,
}

/// What a block pane shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub kind: BlockKind,
    /// `local` or the id of a saved SSH connection.
    pub source: String,
    /// A directory for a browser, a file for a preview.
    pub path: String,
}

/// What the page is told about a block pane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BlockView {
    pub pane: PaneId,
    #[serde(flatten)]
    pub block: Block,
    /// The name of the source for the header: `This computer` or the connection's name.
    pub source_label: String,
}

/// A file opened for a preview: what it is, and what the page needs to show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Opened {
    pub name: String,
    pub size: u64,
    #[serde(flatten)]
    pub class: Classified,
    /// For Markdown: the sanitised HTML. `None` for everything else.
    pub html: Option<String>,
    /// For text: the content, when it is small enough for the editor ([`EDIT_LIMIT`]).
    pub text: Option<String>,
    /// When the file was last changed (seconds since the epoch): what a save is checked against.
    pub modified: Option<i64>,
    /// For text and Markdown the editor can open: the SHA-256 of the bytes that were read, as lowercase hex. What a
    /// save is checked against exactly, because `modified` has a resolution of one second. `None` for everything else.
    pub digest: Option<String>,
}

/// What a save answers: when the file was changed now and the digest of the bytes that were written, so the next save
/// is checked against what this one wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Saved {
    pub modified: Option<i64>,
    pub digest: String,
}

/// The SHA-256 of `bytes` as 64 lowercase hex characters.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Permission to read one file through the `holzfile` scheme, which images, video, audio and PDFs are shown from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Grant {
    /// 128 random bits as hex: the whole of the URL's path. Nothing can be read without one.
    pub token: String,
    pub mime: String,
    pub size: u64,
}

#[derive(Clone)]
struct Granted {
    source: String,
    path: String,
    mime: String,
    size: u64,
    /// Orders the grants by age.
    seq: u64,
}

pub struct Blocks {
    dir: PathBuf,
    home: Option<PathBuf>,
    panes: Mutex<HashMap<PaneId, Block>>,
    /// One source per connection, made on first use and made again when the connection was saved.
    sources: Mutex<HashMap<String, (u64, Arc<dyn FileSource>)>>,
    local: Arc<dyn FileSource>,
    grants: Mutex<HashMap<String, Granted>>,
    grant_seq: AtomicU64,
}

impl Blocks {
    /// Read `pane_blocks.json` from `dir`; a pane that is not in `live_panes` is dropped.
    pub fn load(dir: &Path, home: Option<PathBuf>, live_panes: &[PaneId]) -> Self {
        let mut panes: HashMap<PaneId, Block> = match read_limited(&dir.join(BLOCKS_FILE), MAX_BYTES) {
            Ok(Some(bytes)) => serde_json::from_slice::<HashMap<String, Block>>(&bytes)
                .map(|m| m.into_iter().filter_map(|(k, v)| k.parse::<PaneId>().ok().map(|k| (k, v))).collect())
                .unwrap_or_default(),
            _ => HashMap::new(),
        };
        panes.retain(|pane, block| live_panes.contains(pane) && valid(block));
        Self {
            dir: dir.to_path_buf(),
            local: Arc::new(LocalSource::new(home.clone())),
            home,
            panes: Mutex::new(panes),
            sources: Mutex::new(HashMap::new()),
            grants: Mutex::new(HashMap::new()),
            grant_seq: AtomicU64::new(0),
        }
    }

    fn panes(&self) -> std::sync::MutexGuard<'_, HashMap<PaneId, Block>> {
        self.panes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The directory a browser opens in on this computer.
    pub fn home(&self) -> String {
        self.home.as_ref().map_or_else(|| "/".into(), |h| h.to_string_lossy().into_owned())
    }

    /// Say that `pane` shows `block`. Takes effect when the pane is shown.
    pub fn assign(&self, pane: PaneId, block: Block) -> Result<()> {
        if !valid(&block) {
            return Err(Error::Unsupported("the block names no source or no path".into()));
        }
        self.panes().insert(pane, block);
        self.save();
        Ok(())
    }

    /// The pane's block, changed: the browser went into a folder, the preview shows another file.
    pub fn update(&self, pane: PaneId, path: &str) -> Result<()> {
        let mut panes = self.panes();
        let block = panes.get_mut(&pane).ok_or_else(|| Error::Unsupported("no such block".into()))?;
        if block.kind == BlockKind::Chat {
            // The fixed phrase holds nothing of the refused value.
            if ConversationId::parse(path).is_err() {
                return Err(Error::Unsupported("a chat block can only name a conversation by a valid chat id".into()));
            }
        } else if path.is_empty() || path.len() > crate::files::MAX_PATH || path.contains('\0') {
            return Err(Error::Unsupported("the path is empty, too long or has a NUL".into()));
        }
        block.path = path.to_owned();
        drop(panes);
        self.save();
        Ok(())
    }

    pub fn forget(&self, pane: PaneId) {
        if self.panes().remove(&pane).is_some() {
            self.save();
        }
    }

    pub fn block_of(&self, pane: PaneId) -> Option<Block> {
        self.panes().get(&pane).cloned()
    }

    /// The saved connections that a monitor pane shows, each once. The shell uses it to keep only the hosts a pane still
    /// shows.
    pub fn remote_monitor_sources(&self) -> std::collections::BTreeSet<String> {
        self.panes().values().filter(|b| b.kind == BlockKind::Monitor && b.source != LOCAL).map(|b| b.source.clone()).collect()
    }

    /// Every block pane, for the page.
    pub fn views(&self, connections: &Connections) -> Vec<BlockView> {
        let panes = self.panes().clone();
        let mut out: Vec<BlockView> = panes
            .into_iter()
            .map(|(pane, block)| {
                let source_label = self.label_of(&block.source, connections);
                BlockView { pane, block, source_label }
            })
            .collect();
        out.sort_by_key(|v| v.pane);
        out
    }

    fn label_of(&self, source: &str, connections: &Connections) -> String {
        if source == LOCAL {
            return "This computer".into();
        }
        connections.label_of(source).unwrap_or_else(|| source.to_owned())
    }

    /// The source named `id`: this computer, or the files of a saved SSH connection.
    pub fn source(&self, connections: &Connections, id: &str) -> Result<Arc<dyn FileSource>> {
        if id == LOCAL {
            return Ok(self.local.clone());
        }
        let generation = connections.generation();
        let mut sources = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((g, source)) = sources.get(id) {
            if *g == generation {
                return Ok(source.clone());
            }
        }
        let source: Arc<dyn FileSource> = Arc::new(crate::files::SftpSource::on(connections.session(id)?));
        sources.insert(id.to_owned(), (generation, source.clone()));
        Ok(source)
    }

    pub fn list(&self, connections: &Connections, source: &str, path: &str) -> Result<Listing> {
        self.source(connections, source)?.list(path)
    }

    /// The directory a browser on `source` starts in.
    pub fn start_dir(&self, connections: &Connections, source: &str) -> Result<String> {
        if source == LOCAL { Ok(self.home()) } else { self.source(connections, source)?.home() }
    }

    pub fn stat(&self, connections: &Connections, source: &str, path: &str) -> Result<Entry> {
        self.source(connections, source)?.stat(path)
    }

    /// Open a file for a preview: classify it and, for Markdown and small text, read it.
    pub fn open(&self, connections: &Connections, source: &str, path: &str) -> Result<Opened> {
        let src = self.source(connections, source)?;
        let entry = src.stat(path)?;
        let head = src.read_range(path, 0, SNIFF as u64)?;
        let class = classify(&entry.name, &head);
        let mut opened = Opened { name: entry.name.clone(), size: entry.size, class: class.clone(), html: None, text: None, modified: entry.modified, digest: None };
        use crate::preview::Content;
        match class.content {
            Content::Markdown if entry.size <= crate::preview::MAX_MARKDOWN as u64 => {
                let bytes = src.read(path, crate::preview::MAX_MARKDOWN as u64)?;
                let text = String::from_utf8_lossy(&bytes).into_owned();
                opened.html = render_markdown(&text);
                if entry.size <= EDIT_LIMIT {
                    opened.digest = Some(sha256_hex(&bytes));
                    opened.text = Some(text);
                }
            }
            Content::Markdown | Content::Text if entry.size <= EDIT_LIMIT => {
                let bytes = src.read(path, EDIT_LIMIT)?;
                opened.digest = Some(sha256_hex(&bytes));
                opened.text = Some(String::from_utf8_lossy(&bytes).into_owned());
            }
            _ => {}
        }
        Ok(opened)
    }

    /// Permission to read the file at `path` through the `holzfile` scheme, for an image, a video, audio or a PDF. The
    /// grant names the file, not a folder; a file of any other kind is refused.
    pub fn grant(&self, connections: &Connections, source: &str, path: &str) -> Result<Grant> {
        use crate::preview::Content;
        let src = self.source(connections, source)?;
        let entry = src.stat(path)?;
        let head = src.read_range(path, 0, SNIFF as u64)?;
        let class = classify(&entry.name, &head);
        if !matches!(class.content, Content::Image | Content::Video | Content::Audio | Content::Pdf) {
            return Err(Error::Unsupported("only an image, a video, audio or a PDF is shown this way".into()));
        }
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).map_err(|e| Error::Unsupported(format!("no random numbers: {e}")))?;
        let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let mut grants = self.grants.lock().unwrap_or_else(PoisonError::into_inner);
        if grants.len() >= MAX_GRANTS {
            if let Some(oldest) = grants.iter().min_by_key(|(_, g)| g.seq).map(|(k, _)| k.clone()) {
                grants.remove(&oldest);
            }
        }
        grants.insert(
            token.clone(),
            Granted {
                source: source.to_owned(),
                path: path.to_owned(),
                mime: class.mime.clone(),
                size: entry.size,
                seq: self.grant_seq.fetch_add(1, Ordering::SeqCst),
            },
        );
        Ok(Grant { token, mime: class.mime, size: entry.size })
    }

    /// End a grant; the URL stops working.
    pub fn revoke(&self, token: &str) {
        self.grants.lock().unwrap_or_else(PoisonError::into_inner).remove(token);
    }

    /// What a token names: its media type and size, or `None` when there is no such grant.
    pub fn granted(&self, token: &str) -> Option<(String, u64)> {
        self.grants.lock().unwrap_or_else(PoisonError::into_inner).get(token).map(|g| (g.mime.clone(), g.size))
    }

    /// `len` bytes of a granted file from `offset`. `None` when the token is not a grant.
    pub fn read_granted(&self, connections: &Connections, token: &str, offset: u64, len: u64) -> Option<Result<Vec<u8>>> {
        let g = self.grants.lock().unwrap_or_else(PoisonError::into_inner).get(token).cloned()?;
        Some(self.source(connections, &g.source).and_then(|s| s.read_range(&g.path, offset, len.min(PREVIEW_LIMIT))))
    }

    /// Save the editor's text over the file. A file that was changed since the editor opened it (by another program, or
    /// on another machine) is not overwritten and the error says so.
    ///
    /// With `expected_digest` (the digest [`Opened`] or the last [`Saved`] carried) the check is exact: the file is read
    /// again and refused when its bytes differ, whatever the modification time says, so two edits within one second are
    /// caught and a touch without a change is not a conflict. Without one, `expected` (the change time) is compared as
    /// before; `None` for both overwrites whatever is there. A file that does not exist yet has nothing to conflict with.
    pub fn save_file(
        &self,
        connections: &Connections,
        source: &str,
        path: &str,
        text: &str,
        expected: Option<i64>,
        expected_digest: Option<&str>,
    ) -> Result<Saved> {
        if text.len() as u64 > EDIT_LIMIT {
            return Err(Error::Unsupported(format!("the text is larger than {EDIT_LIMIT} bytes")));
        }
        if let Some(digest) = expected_digest {
            if digest.len() != 64 || !digest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                return Err(Error::Unsupported("the expected digest is not valid".into()));
            }
        }
        let src = self.source(connections, source)?;
        let conflict = || Error::Unsupported("the file changed on disk since it was opened; save again to overwrite it".into());
        if let Some(digest) = expected_digest {
            if let Ok(now) = src.stat(path) {
                // The editor opened a file of at most EDIT_LIMIT bytes: a larger one has changed, and is not read.
                if now.size > EDIT_LIMIT {
                    return Err(conflict());
                }
                if sha256_hex(&src.read(path, EDIT_LIMIT)?) != digest {
                    return Err(conflict());
                }
            }
        } else if let Some(expected) = expected {
            if let Ok(now) = src.stat(path) {
                if now.modified.is_some_and(|m| m != expected) {
                    return Err(conflict());
                }
            }
        }
        src.write(path, text.as_bytes())?;
        Ok(Saved { modified: src.stat(path).ok().and_then(|e| e.modified), digest: sha256_hex(text.as_bytes()) })
    }

    fn save(&self) {
        let map: HashMap<String, Block> = self.panes().iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let text = serde_json::to_string_pretty(&map).unwrap_or_else(|_| "{}".into());
        if let Err(e) = fs::create_dir_all(&self.dir).and_then(|()| write_atomic(&self.dir.join(BLOCKS_FILE), text.as_bytes())) {
            eprintln!("holzwave: cannot save {BLOCKS_FILE}: {e}");
        }
    }
}

/// Whether a block makes sense on another computer, so the sync may share it with a layout: a block whose source is a
/// saved connection, and the monitor (of this computer, whichever it is). A local folder, a local preview and a chat
/// are tied to this computer and are not shared.
pub fn portable(block: &Block) -> bool {
    block.source != LOCAL || block.kind == BlockKind::Monitor
}

fn valid(block: &Block) -> bool {
    match block.kind {
        // The monitor looks at this computer or at a saved SSH connection and has no path. Whether the connection exists
        // and is SSH is checked when the pane is opened.
        BlockKind::Monitor => block.path.is_empty() && !block.source.is_empty() && block.source.len() <= 64,
        // A chat lives on this computer and its path is a conversation id: the store's own rule decides (length,
        // characters, device names), so no second copy of it exists.
        BlockKind::Chat => block.source == LOCAL && ConversationId::parse(&block.path).is_ok(),
        BlockKind::Files | BlockKind::Preview => {
            !block.source.is_empty()
                && block.source.len() <= 64
                && !block.path.is_empty()
                && block.path.len() <= crate::files::MAX_PATH
                && !block.path.contains('\0')
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

    #[test]
    fn portable_blocks_are_the_ones_with_a_saved_connection_or_the_monitor() {
        let block = |kind, source: &str| Block { kind, source: source.into(), path: String::new() };
        for kind in [BlockKind::Files, BlockKind::Preview, BlockKind::Monitor, BlockKind::Chat] {
            // A saved connection is the same saved connection on every computer.
            assert!(portable(&block(kind, "web-1")), "{kind:?} of a saved connection");
            // On this computer only the monitor makes sense elsewhere: it looks at whichever computer runs it.
            assert_eq!(portable(&block(kind, LOCAL)), kind == BlockKind::Monitor, "{kind:?} of this computer");
        }
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("holzwave-blocks-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn setup(name: &str) -> (PathBuf, Blocks, Connections) {
        let d = dir(name);
        let (conns, _) = Connections::load(&d, Arc::new(MemoryStore::default()), None, &[]);
        let blocks = Blocks::load(&d, Some(d.clone()), &[1, 2]);
        (d, blocks, conns)
    }

    #[test]
    fn a_block_is_assigned_changed_saved_and_restored() {
        let (d, blocks, conns) = setup("assign");
        blocks.assign(1, Block { kind: BlockKind::Files, source: LOCAL.into(), path: "/tmp".into() }).unwrap();
        blocks.update(1, "/var").unwrap();
        assert!(blocks.assign(2, Block { kind: BlockKind::Preview, source: String::new(), path: "/x".into() }).is_err());
        assert!(blocks.update(9, "/x").is_err());
        let views = blocks.views(&conns);
        assert_eq!(views.len(), 1);
        assert_eq!((views[0].pane, views[0].block.path.as_str(), views[0].source_label.as_str()), (1, "/var", "This computer"));
        let again = Blocks::load(&d, None, &[1]);
        assert_eq!(again.block_of(1).unwrap().path, "/var");
        assert!(Blocks::load(&d, None, &[2]).block_of(1).is_none(), "a pane that left the layout is dropped");
        blocks.forget(1);
        assert!(blocks.block_of(1).is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_monitor_block_names_this_computer_or_a_saved_connection_and_has_no_path() {
        let (d, blocks, _) = setup("monitor");
        let monitor = |source: &str, path: &str| Block { kind: BlockKind::Monitor, source: source.into(), path: path.into() };
        blocks.assign(1, monitor(LOCAL, "")).unwrap();
        blocks.assign(2, monitor("talos", "")).unwrap();
        assert!(blocks.assign(3, monitor(LOCAL, "/x")).is_err(), "no path");
        assert!(blocks.assign(3, monitor("talos", "/x")).is_err(), "no path on a saved connection either");
        assert!(blocks.assign(4, monitor("", "")).is_err(), "a source is named");
        assert!(blocks.assign(5, monitor(&"x".repeat(65), "")).is_err(), "a source of 65 bytes is refused");
        let again = Blocks::load(&d, None, &[1, 2]);
        assert_eq!(again.block_of(1).unwrap().kind, BlockKind::Monitor, "it survives a restart");
        assert_eq!(again.block_of(2).unwrap().source, "talos");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_hosts_that_have_a_monitor_are_listed_once() {
        let (d, blocks, _) = setup("hosts");
        let block = |kind, source: &str, path: &str| Block { kind, source: source.into(), path: path.into() };
        blocks.assign(1, block(BlockKind::Monitor, "talos", "")).unwrap();
        blocks.assign(2, block(BlockKind::Monitor, "talos", "")).unwrap();
        blocks.assign(3, block(BlockKind::Monitor, LOCAL, "")).unwrap();
        blocks.assign(4, block(BlockKind::Files, "nas", "/")).unwrap();
        assert_eq!(blocks.remote_monitor_sources().into_iter().collect::<Vec<_>>(), vec!["talos".to_string()]);
        blocks.forget(1);
        assert_eq!(blocks.remote_monitor_sources().len(), 1, "one pane still shows it");
        blocks.forget(2);
        assert!(blocks.remote_monitor_sources().is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    fn chat(path: &str) -> Block {
        Block { kind: BlockKind::Chat, source: LOCAL.into(), path: path.into() }
    }

    #[test]
    fn a_chat_block_holds_a_conversation_id_and_survives_a_restart() {
        let (d, blocks, conns) = setup("chat");
        blocks.assign(1, chat("chat-golden")).unwrap();
        assert_eq!(blocks.block_of(1), Some(chat("chat-golden")));
        let views = blocks.views(&conns);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].source_label, "This computer");
        // The same literal object is read by `readBlocks` in the page test of the block state.
        let literal = serde_json::json!({
            "pane": 1, "kind": "chat", "source": "local", "path": "chat-golden", "source_label": "This computer"
        });
        assert_eq!(serde_json::to_value(&views[0]).unwrap(), literal);
        let text = fs::read_to_string(d.join(BLOCKS_FILE)).unwrap();
        assert!(text.contains("\"kind\": \"chat\""), "the kind is written as chat: {text}");
        assert!(text.contains("chat-golden"), "{text}");
        assert!(!text.contains("messages"), "the file keeps the id and no messages: {text}");
        assert_eq!(Blocks::load(&d, None, &[1]).block_of(1), Some(chat("chat-golden")), "it survives a restart");
        assert!(Blocks::load(&d, None, &[2]).block_of(1).is_none(), "a pane that left the layout is dropped");
        let made = ConversationId::generate().unwrap();
        blocks.assign(2, chat(made.as_str())).unwrap();
        assert_eq!(blocks.block_of(2).unwrap().path, made.as_str());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_chat_block_needs_this_computer_and_a_valid_chat_id() {
        let (d, blocks, _) = setup("chatvalid");
        let on = |source: &str, path: &str| Block { kind: BlockKind::Chat, source: source.into(), path: path.into() };
        let long = "a".repeat(65);
        let refused =
            ["../etc/passwd", "a/b", "a\\b", "a.b", "con", "NUL", long.as_str(), "chat\0", "\u{e9}", "", "a b"];
        for path in refused {
            let err = blocks.assign(1, chat(path)).unwrap_err().to_string();
            assert!(path.is_empty() || !err.contains(path), "the refusal holds none of the value: {err}");
        }
        assert!(blocks.assign(1, on("talos", "chat-1")).is_err(), "a chat lives on this computer");
        assert!(blocks.assign(1, on("", "chat-1")).is_err());
        assert!(blocks.assign(1, on(LOCAL, "")).is_err());
        let err = blocks.assign(1, chat("../etc/passwd")).unwrap_err().to_string();
        assert!(!err.contains("etc/passwd"), "the refusal holds none of the value: {err}");
        let (d2, fresh, conns) = setup("chatvalid2");
        for path in refused {
            let _ = fresh.assign(1, chat(path));
        }
        assert!(fresh.assign(1, on("talos", "chat-1")).is_err());
        assert!(fresh.views(&conns).is_empty(), "nothing was stored");
        assert!(!d2.join(BLOCKS_FILE).exists(), "nothing was written");
        let sixty_four = "a".repeat(64);
        for path in ["chat-1", "Chat_One-2", sixty_four.as_str()] {
            blocks.assign(1, chat(path)).unwrap();
            assert_eq!(blocks.block_of(1).unwrap().path, path);
        }
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&d2);
    }

    #[test]
    fn a_chat_block_can_only_move_to_another_valid_chat_id() {
        let (d, blocks, _) = setup("chatupdate");
        blocks.assign(1, chat("chat-1")).unwrap();
        blocks.assign(2, Block { kind: BlockKind::Files, source: LOCAL.into(), path: "/tmp".into() }).unwrap();
        blocks.update(1, "chat-2").unwrap();
        assert_eq!(blocks.block_of(1).unwrap().path, "chat-2");
        assert_eq!(Blocks::load(&d, None, &[1, 2]).block_of(1).unwrap().path, "chat-2");
        let long = "a".repeat(65);
        for path in ["../x", "", "a b", long.as_str()] {
            let err = blocks.update(1, path).unwrap_err().to_string();
            assert!(path.is_empty() || !err.contains(path), "the refusal holds none of the value: {err}");
            assert_eq!(blocks.block_of(1).unwrap().path, "chat-2", "memory is unchanged after {path:?}");
            assert_eq!(Blocks::load(&d, None, &[1]).block_of(1).unwrap().path, "chat-2", "the file is unchanged");
        }
        blocks.update(2, "/var").unwrap();
        assert_eq!(blocks.block_of(2).unwrap().path, "/var", "the rule of a file browser is unchanged");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_block_file_with_a_bad_chat_id_loses_only_that_block() {
        let d = dir("chatfile");
        let file = r#"{
            "1": {"kind": "chat", "source": "local", "path": "chat-1"},
            "2": {"kind": "chat", "source": "local", "path": "../x"},
            "3": {"kind": "chat", "source": "talos", "path": "chat-3"},
            "4": {"kind": "files", "source": "local", "path": "/tmp"}
        }"#;
        fs::write(d.join(BLOCKS_FILE), file).unwrap();
        let blocks = Blocks::load(&d, None, &[1, 2, 3, 4]);
        assert_eq!(blocks.block_of(1), Some(chat("chat-1")));
        assert!(blocks.block_of(2).is_none() && blocks.block_of(3).is_none());
        assert_eq!(blocks.block_of(4).unwrap().kind, BlockKind::Files);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn block_kinds_keep_their_names_in_the_file() {
        let name = |kind: BlockKind| serde_json::to_string(&kind).unwrap();
        assert_eq!(name(BlockKind::Files), "\"files\"");
        assert_eq!(name(BlockKind::Preview), "\"preview\"");
        assert_eq!(name(BlockKind::Monitor), "\"monitor\"");
        assert_eq!(name(BlockKind::Chat), "\"chat\"");
        assert_eq!(serde_json::from_str::<BlockKind>("\"chat\"").unwrap(), BlockKind::Chat);
        assert!(serde_json::from_str::<BlockKind>("\"Chat\"").is_err());
        assert!(serde_json::from_str::<BlockKind>("\"terminal\"").is_err());
    }

    #[test]
    fn a_markdown_file_opens_as_html_and_text_and_a_picture_as_an_image() {
        let (d, blocks, conns) = setup("open");
        fs::write(d.join("a.md"), "# Hi\n\n<script>x</script>").unwrap();
        fs::write(d.join("b.rs"), "fn main() {}").unwrap();
        fs::write(d.join("c.bin"), [0u8, 1, 2, 255]).unwrap();
        let md = blocks.open(&conns, LOCAL, d.join("a.md").to_str().unwrap()).unwrap();
        assert_eq!(md.class.content, crate::preview::Content::Markdown);
        assert!(md.html.as_deref().unwrap().contains("<h1>Hi</h1>"));
        assert!(!md.html.unwrap().contains("<script"));
        assert!(md.text.unwrap().starts_with("# Hi"), "the source is there for the editor");
        let rs = blocks.open(&conns, LOCAL, d.join("b.rs").to_str().unwrap()).unwrap();
        assert_eq!((rs.class.content, rs.class.language), (crate::preview::Content::Text, Some("rust")));
        assert_eq!(rs.text.as_deref(), Some("fn main() {}"));
        let bin = blocks.open(&conns, LOCAL, d.join("c.bin").to_str().unwrap()).unwrap();
        assert_eq!((bin.class.content, bin.text), (crate::preview::Content::Binary, None));
        assert!(blocks.open(&conns, LOCAL, d.join("none").to_str().unwrap()).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_grant_names_one_file_and_ends_when_it_is_revoked() {
        let (d, blocks, conns) = setup("grant");
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 1, 2, 3, 4];
        fs::write(d.join("a.png"), png).unwrap();
        fs::write(d.join("b.txt"), "text").unwrap();
        let grant = blocks.grant(&conns, LOCAL, d.join("a.png").to_str().unwrap()).unwrap();
        assert_eq!((grant.mime.as_str(), grant.size, grant.token.len()), ("image/png", 20, 32));
        assert!(grant.token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(blocks.granted(&grant.token), Some(("image/png".into(), 20)));
        assert_eq!(blocks.read_granted(&conns, &grant.token, 8, 4).unwrap().unwrap(), &png[8..12]);
        assert_eq!(blocks.read_granted(&conns, &grant.token, 18, 100).unwrap().unwrap(), &png[18..], "a range stops at the end");
        assert!(blocks.read_granted(&conns, "0123", 0, 1).is_none(), "a token that was never granted reads nothing");
        assert!(blocks.grant(&conns, LOCAL, d.join("b.txt").to_str().unwrap()).is_err(), "text is not shown this way");
        let other = blocks.grant(&conns, LOCAL, d.join("a.png").to_str().unwrap()).unwrap();
        assert_ne!(other.token, grant.token, "every grant has its own token");
        blocks.revoke(&grant.token);
        assert!(blocks.granted(&grant.token).is_none());
        assert!(blocks.read_granted(&conns, &grant.token, 0, 1).is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_oldest_grant_goes_when_there_are_too_many() {
        let (d, blocks, conns) = setup("many");
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        fs::write(d.join("a.png"), png).unwrap();
        let p = d.join("a.png");
        let first = blocks.grant(&conns, LOCAL, p.to_str().unwrap()).unwrap();
        for _ in 0..MAX_GRANTS {
            blocks.grant(&conns, LOCAL, p.to_str().unwrap()).unwrap();
        }
        assert!(blocks.granted(&first.token).is_none(), "the first one was dropped");
        let _ = fs::remove_dir_all(&d);
    }

    const CONFLICT: &str = "the file changed on disk since it was opened; save again to overwrite it";

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn set_modified_secs(path: &Path, secs: i64) {
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::try_from(secs).unwrap())).unwrap();
    }

    #[test]
    fn an_open_file_carries_its_digest() {
        let (d, blocks, conns) = setup("digest-open");
        fs::write(d.join("a.md"), "# Hi\n").unwrap();
        fs::write(d.join("b.rs"), "fn main() {}").unwrap();
        fs::write(d.join("c.bin"), [0u8, 1, 2, 255]).unwrap();
        fs::write(d.join("d.txt"), vec![b'x'; usize::try_from(EDIT_LIMIT).unwrap() + 1]).unwrap();
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 1, 2, 3, 4];
        fs::write(d.join("e.png"), png).unwrap();
        let open = |name: &str| blocks.open(&conns, LOCAL, d.join(name).to_str().unwrap()).unwrap();
        assert_eq!(open("a.md").digest, Some(sha256_hex(b"# Hi\n")));
        assert_eq!(open("b.rs").digest, Some(sha256_hex(b"fn main() {}")));
        assert_eq!(open("c.bin").digest, None);
        let big = open("d.txt");
        assert_eq!((big.text, big.digest), (None, None), "a file over the editor's limit has neither");
        assert_eq!(open("e.png").digest, None);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_save_is_refused_when_the_bytes_changed_even_in_the_same_second() {
        let (d, blocks, conns) = setup("digest-same-second");
        let p = d.join("f.txt");
        let path = p.to_str().unwrap();
        fs::write(&p, "aaaa").unwrap();
        let opened = blocks.open(&conns, LOCAL, path).unwrap();
        // Someone else changes the bytes (same length) and the time is put back to the opened value.
        fs::write(&p, "bbbb").unwrap();
        set_modified_secs(&p, opened.modified.unwrap());
        assert_eq!(blocks.stat(&conns, LOCAL, path).unwrap().modified, opened.modified, "the time did not move");
        let err = blocks.save_file(&conns, LOCAL, path, "mine", opened.modified, opened.digest.as_deref()).unwrap_err().to_string();
        assert!(err.contains(CONFLICT), "{err}");
        assert_eq!(fs::read_to_string(&p).unwrap(), "bbbb", "the file is untouched");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_touch_without_a_change_is_not_a_conflict() {
        let (d, blocks, conns) = setup("digest-touch");
        let p = d.join("f.txt");
        let path = p.to_str().unwrap();
        fs::write(&p, "same").unwrap();
        let opened = blocks.open(&conns, LOCAL, path).unwrap();
        set_modified_secs(&p, opened.modified.unwrap() + 3600);
        blocks.save_file(&conns, LOCAL, path, "mine", opened.modified, opened.digest.as_deref()).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "mine");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_save_without_a_digest_keeps_the_time_check() {
        let (d, blocks, conns) = setup("digest-none");
        let p = d.join("f.txt");
        let path = p.to_str().unwrap();
        fs::write(&p, "old").unwrap();
        let opened = blocks.open(&conns, LOCAL, path).unwrap();
        let stale = opened.modified.map(|m| m - 100);
        let err = blocks.save_file(&conns, LOCAL, path, "mine", stale, None).unwrap_err().to_string();
        assert!(err.contains(CONFLICT), "{err}");
        assert_eq!(fs::read_to_string(&p).unwrap(), "old");
        blocks.save_file(&conns, LOCAL, path, "mine", None, None).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "mine");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_save_answers_with_the_digest_of_what_it_wrote() {
        let (d, blocks, conns) = setup("digest-saved");
        let p = d.join("f.txt");
        let path = p.to_str().unwrap();
        fs::write(&p, "old").unwrap();
        let opened = blocks.open(&conns, LOCAL, path).unwrap();
        let first = blocks.save_file(&conns, LOCAL, path, "one", opened.modified, opened.digest.as_deref()).unwrap();
        assert_eq!(first.digest, sha256_hex(b"one"));
        assert!(first.modified.is_some());
        // The next save is checked against what was just written.
        let second = blocks.save_file(&conns, LOCAL, path, "two", first.modified, Some(&first.digest)).unwrap();
        assert_eq!(second.digest, sha256_hex(b"two"));
        let err = blocks.save_file(&conns, LOCAL, path, "three", first.modified, Some(&first.digest)).unwrap_err().to_string();
        assert!(err.contains(CONFLICT), "the old digest is stale now: {err}");
        assert_eq!(fs::read_to_string(&p).unwrap(), "two");
        // A file that does not exist yet saves without a conflict.
        let fresh = d.join("new.txt");
        let made = blocks.save_file(&conns, LOCAL, fresh.to_str().unwrap(), "hello", None, Some(&sha256_hex(b"whatever"))).unwrap();
        assert_eq!(made.digest, sha256_hex(b"hello"));
        assert_eq!(fs::read_to_string(&fresh).unwrap(), "hello");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_digest_that_is_not_a_digest_is_refused_before_anything_is_read() {
        let (d, blocks, conns) = setup("digest-bad");
        let p = d.join("f.txt");
        fs::write(&p, "old").unwrap();
        for bad in ["", "abc", &"A".repeat(64), &"g".repeat(64), &"a".repeat(65)] {
            let err = blocks.save_file(&conns, LOCAL, p.to_str().unwrap(), "mine", None, Some(bad)).unwrap_err().to_string();
            assert!(err.contains("the expected digest is not valid"), "{bad:?}: {err}");
        }
        assert_eq!(fs::read_to_string(&p).unwrap(), "old");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_file_that_grew_past_the_editors_limit_is_a_conflict() {
        let (d, blocks, conns) = setup("digest-grew");
        let p = d.join("f.txt");
        let path = p.to_str().unwrap();
        fs::write(&p, "old").unwrap();
        let opened = blocks.open(&conns, LOCAL, path).unwrap();
        fs::write(&p, vec![b'x'; usize::try_from(EDIT_LIMIT).unwrap() + 1]).unwrap();
        let err = blocks.save_file(&conns, LOCAL, path, "mine", opened.modified, opened.digest.as_deref()).unwrap_err().to_string();
        assert!(err.contains(CONFLICT), "{err}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_editor_saves_over_the_file_and_an_unknown_source_is_an_error() {
        let (d, blocks, conns) = setup("save");
        let p = d.join("f.txt");
        fs::write(&p, "old").unwrap();
        let opened = blocks.open(&conns, LOCAL, p.to_str().unwrap()).unwrap();
        let saved = blocks.save_file(&conns, LOCAL, p.to_str().unwrap(), "new", opened.modified, None).unwrap().modified;
        assert_eq!(fs::read_to_string(&p).unwrap(), "new");
        assert!(saved.is_some(), "the new change time comes back");
        // The file is changed by someone else after it was opened: the save is refused, and an overwrite is explicit.
        fs::write(&p, "theirs").unwrap();
        let stale = opened.modified.map(|m| m - 100);
        let err = blocks.save_file(&conns, LOCAL, p.to_str().unwrap(), "mine", stale, None).unwrap_err().to_string();
        assert!(err.contains("changed on disk"), "{err}");
        assert_eq!(fs::read_to_string(&p).unwrap(), "theirs", "nothing was overwritten");
        blocks.save_file(&conns, LOCAL, p.to_str().unwrap(), "mine", None, None).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "mine");
        let err = blocks.list(&conns, "nope", "/").unwrap_err().to_string();
        assert!(err.contains("nope"), "{err}");
        let _ = fs::remove_dir_all(&d);
    }
}
