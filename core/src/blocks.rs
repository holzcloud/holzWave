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

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    /// A file browser.
    Files,
    /// The preview of one file.
    Preview,
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
}

pub struct Blocks {
    dir: PathBuf,
    home: Option<PathBuf>,
    panes: Mutex<HashMap<PaneId, Block>>,
    /// One source per connection, made on first use and made again when the connection was saved.
    sources: Mutex<HashMap<String, (u64, Arc<dyn FileSource>)>>,
    local: Arc<dyn FileSource>,
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
        if path.is_empty() || path.len() > crate::files::MAX_PATH || path.contains('\0') {
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
        let target = connections.ssh_target(id)?;
        let label = connections.label_of(id).unwrap_or_else(|| id.to_owned());
        let source: Arc<dyn FileSource> = Arc::new(crate::files::SftpSource::new(label, target)?);
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
        let mut opened = Opened { name: entry.name.clone(), size: entry.size, class: class.clone(), html: None, text: None };
        use crate::preview::Content;
        match class.content {
            Content::Markdown if entry.size <= crate::preview::MAX_MARKDOWN as u64 => {
                let bytes = src.read(path, crate::preview::MAX_MARKDOWN as u64)?;
                let text = String::from_utf8_lossy(&bytes).into_owned();
                opened.html = render_markdown(&text);
                if entry.size <= EDIT_LIMIT {
                    opened.text = Some(text);
                }
            }
            Content::Markdown | Content::Text if entry.size <= EDIT_LIMIT => {
                let bytes = src.read(path, EDIT_LIMIT)?;
                opened.text = Some(String::from_utf8_lossy(&bytes).into_owned());
            }
            _ => {}
        }
        Ok(opened)
    }

    /// The bytes of a file for an image, a video or a PDF, at most [`PREVIEW_LIMIT`].
    pub fn read(&self, connections: &Connections, source: &str, path: &str) -> Result<Vec<u8>> {
        self.source(connections, source)?.read(path, PREVIEW_LIMIT)
    }

    /// Save the editor's text over the file.
    pub fn save_file(&self, connections: &Connections, source: &str, path: &str, text: &str) -> Result<()> {
        if text.len() as u64 > EDIT_LIMIT {
            return Err(Error::Unsupported(format!("the text is larger than {EDIT_LIMIT} bytes")));
        }
        self.source(connections, source)?.write(path, text.as_bytes())
    }

    fn save(&self) {
        let map: HashMap<String, Block> = self.panes().iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let text = serde_json::to_string_pretty(&map).unwrap_or_else(|_| "{}".into());
        if let Err(e) = fs::create_dir_all(&self.dir).and_then(|()| write_atomic(&self.dir.join(BLOCKS_FILE), text.as_bytes())) {
            eprintln!("holzwave: cannot save {BLOCKS_FILE}: {e}");
        }
    }
}

fn valid(block: &Block) -> bool {
    !block.source.is_empty()
        && block.source.len() <= 64
        && !block.path.is_empty()
        && block.path.len() <= crate::files::MAX_PATH
        && !block.path.contains('\0')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

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
    fn the_editor_saves_over_the_file_and_an_unknown_source_is_an_error() {
        let (d, blocks, conns) = setup("save");
        let p = d.join("f.txt");
        fs::write(&p, "old").unwrap();
        blocks.save_file(&conns, LOCAL, p.to_str().unwrap(), "new").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "new");
        let err = blocks.list(&conns, "nope", "/").unwrap_err().to_string();
        assert!(err.contains("nope"), "{err}");
        let _ = fs::remove_dir_all(&d);
    }
}
