//! The behaviour of the app shell, without Tauri (D-19).
//!
//! [`AppCore`] owns the layout and the pane handles and answers every command of the webview. It knows nothing about
//! windows: what the webview has to be told leaves through a [`Sink`] as an [`Out`], and `lib.rs` maps those to Tauri
//! events. That is what makes the shell testable headless, with a recording sink and real shells.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use holzwave_core::engine::{CopyOp, CopyState, DomKey, PasteOutcome, SearchOp, SearchState, SelectOp};
use holzwave_core::layout::{Axis, CloseOutcome, Divider, Layout, LoadStatus, PaneId, Rect, SplitId};
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
use holzwave_core::persist;
use holzwave_core::wire::FrameMsg;
use serde::{Serialize, Serializer};

/// A layout file larger than this is not read: a real layout is a few KiB.
const MAX_LAYOUT_BYTES: u64 = 1 << 20;

// ---- what the webview sees -------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WorkspaceView {
    pub name: String,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TabView {
    pub title: String,
    pub active: bool,
    /// The pane of this tab that has the focus.
    pub focus: PaneId,
}

/// The layout as the webview mirrors it: the workspaces, the tabs of the active workspace, and the panes and
/// dividers of the active tab in unit coordinates.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct View {
    pub workspaces: Vec<WorkspaceView>,
    pub tabs: Vec<TabView>,
    pub focus: PaneId,
    pub panes: Vec<(PaneId, Rect)>,
    pub dividers: Vec<Divider>,
}

impl View {
    pub fn of(layout: &Layout) -> View {
        let (panes, dividers) = layout.tab().geometry();
        let active_tab = layout.workspace().active;
        View {
            workspaces: layout
                .workspaces
                .iter()
                .enumerate()
                .map(|(i, w)| WorkspaceView { name: w.name.clone(), active: i == layout.active })
                .collect(),
            tabs: layout
                .workspace()
                .tabs
                .iter()
                .enumerate()
                .map(|(i, t)| TabView { title: t.title.clone(), active: i == active_tab, focus: t.focus })
                .collect(),
            focus: layout.tab().focus,
            panes,
            dividers,
        }
    }
}

/// Everything the core tells the webview.
#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    Frame(FrameMsg),
    Layout(View),
    Title { pane: PaneId, title: String },
    Cwd { pane: PaneId, cwd: String },
    PaneExit { pane: PaneId, code: Option<u32> },
    /// The application is to end now: the layout is on disk and every shell has been asked to close.
    Exit,
}

impl Out {
    /// The name of the Tauri event this is sent as.
    pub fn name(&self) -> &'static str {
        match self {
            Out::Frame(_) => "frame",
            Out::Layout(_) => "layout",
            Out::Title { .. } => "title",
            Out::Cwd { .. } => "cwd",
            Out::PaneExit { .. } => "pane-exit",
            Out::Exit => "exit",
        }
    }
}

#[derive(Serialize)]
struct TitlePayload<'a> {
    pane: PaneId,
    title: &'a str,
}

#[derive(Serialize)]
struct CwdPayload<'a> {
    pane: PaneId,
    cwd: &'a str,
}

#[derive(Serialize)]
struct PaneExitPayload {
    pane: PaneId,
    code: Option<u32>,
}

/// An event serialises as its payload alone; the name travels separately ([`Out::name`]).
impl Serialize for Out {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Out::Frame(frame) => frame.serialize(serializer),
            Out::Layout(view) => view.serialize(serializer),
            Out::Title { pane, title } => TitlePayload { pane: *pane, title }.serialize(serializer),
            Out::Cwd { pane, cwd } => CwdPayload { pane: *pane, cwd }.serialize(serializer),
            Out::PaneExit { pane, code } => PaneExitPayload { pane: *pane, code: *code }.serialize(serializer),
            Out::Exit => serializer.serialize_unit(),
        }
    }
}

/// Where the core's events go. `send` may be called from any thread, including a pane's engine thread, and must
/// return quickly.
pub trait Sink: Send + Sync + 'static {
    fn send(&self, out: Out);
}

/// The program a pane runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl ShellSpec {
    pub fn new(program: impl Into<String>, args: &[&str]) -> Self {
        Self { program: program.into(), args: args.iter().map(|a| (*a).to_owned()).collect() }
    }
}

impl Default for ShellSpec {
    /// The user's login shell: `$SHELL -l`, `/bin/zsh` when it is not set.
    fn default() -> Self {
        let program = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
        Self { program, args: vec!["-l".into()] }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the map of panes holds for a pane id.
enum Slot {
    /// A shell is being started; nothing is known of it yet.
    Starting,
    Running(PaneHandle),
}

/// The state of the app: the layout, the running panes, and what is needed to start a pane.
pub struct AppCore {
    layout: Arc<Mutex<Layout>>,
    panes: Mutex<HashMap<PaneId, Slot>>,
    sink: Arc<dyn Sink>,
    shell: ShellSpec,
    dir: PathBuf,
    exiting: AtomicBool,
    writes: AtomicUsize,
    me: Weak<AppCore>,
}

impl AppCore {
    /// Load the layout from `dir/layout.json` (a fresh one-pane layout when there is none or it is unusable) and
    /// start nothing: the webview starts each pane once it knows the pane's size.
    pub fn new(dir: PathBuf, sink: Arc<dyn Sink>, shell: ShellSpec) -> Arc<AppCore> {
        let layout = load_layout(&dir.join("layout.json"));
        Arc::new_cyclic(|me| AppCore {
            layout: Arc::new(Mutex::new(layout)),
            panes: Mutex::new(HashMap::new()),
            sink,
            shell,
            dir,
            exiting: AtomicBool::new(false),
            writes: AtomicUsize::new(0),
            me: me.clone(),
        })
    }

    pub fn view(&self) -> View {
        View::of(&lock(&self.layout))
    }

    // Skeleton: the behaviour of the old shell. Every change is written at once, an unknown pane ends the app and
    // nothing is flushed at the end.

    fn save(&self, layout: &Layout) {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if let Ok(json) = serde_json::to_vec_pretty(layout) {
            let _ = persist::write_atomic(&self.dir.join("layout.json"), &json);
        }
    }

    fn change(&self, f: impl FnOnce(&mut Layout)) -> View {
        let mut layout = lock(&self.layout);
        f(&mut layout);
        self.save(&layout);
        View::of(&layout)
    }

    pub fn split(&self, column: bool) -> View {
        self.change(|l| {
            l.split_focused(if column { Axis::Column } else { Axis::Row });
        })
    }

    pub fn new_tab(&self) -> View {
        self.change(|l| {
            l.new_tab();
        })
    }

    pub fn new_workspace(&self, name: &str) -> View {
        self.change(|l| {
            l.new_workspace(name);
        })
    }

    pub fn close_pane(&self, pane: PaneId) -> View {
        lock(&self.panes).remove(&pane);
        let mut layout = lock(&self.layout);
        if layout.close_pane(pane) != CloseOutcome::Closed {
            self.sink.send(Out::Exit);
            return View::of(&layout);
        }
        self.save(&layout);
        View::of(&layout)
    }

    pub fn focus_pane(&self, pane: PaneId) -> View {
        self.change(|l| l.focus_pane(pane))
    }

    pub fn cycle_focus(&self, step: i32) -> View {
        self.change(|l| l.cycle_focus(step as isize))
    }

    pub fn select_tab(&self, index: usize) -> View {
        self.change(|l| l.select_tab(index))
    }

    pub fn select_workspace(&self, index: usize) -> View {
        self.change(|l| l.select_workspace(index))
    }

    pub fn set_ratio(&self, split: SplitId, ratio: f32) -> View {
        self.change(|l| l.set_ratio(split, ratio))
    }

    pub fn flush(&self) {}

    pub fn shutdown(&self) {}

    pub fn layout_writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }

    /// The handle of a running pane. `None` for an unknown pane and for one that is still starting.
    fn handle(&self, pane: PaneId) -> Option<PaneHandle> {
        match lock(&self.panes).get(&pane) {
            Some(Slot::Running(handle)) => Some(handle.clone()),
            _ => None,
        }
    }

    /// Like [`AppCore::handle`], for the commands that answer with something and so need the pane to exist.
    fn handle_or_err(&self, pane: PaneId) -> Result<PaneHandle, String> {
        self.handle(pane).ok_or_else(|| "no such pane".to_owned())
    }

    /// Start the shell of a pane if it has none yet. The webview calls this once it knows the pane's size. A pane
    /// that is not in the layout is refused; a second call for a pane that runs (or is starting) changes nothing.
    ///
    /// Waits for the actor's startup handshake (a few milliseconds), so the command is async.
    pub fn start_pane(&self, pane: PaneId, cols: u32, rows: u32, cell_w: u32, cell_h: u32) -> Result<(), String> {
        if self.exiting.load(Ordering::SeqCst) {
            return Err("the app is closing".into());
        }
        // Checked and reserved under both locks, so that a concurrent close cannot slip in between.
        {
            let layout = lock(&self.layout);
            if !layout.all_panes().contains(&pane) {
                return Err("no such pane".into());
            }
            let mut panes = lock(&self.panes);
            if panes.contains_key(&pane) {
                return Ok(());
            }
            panes.insert(pane, Slot::Starting);
        }

        let (cols, rows) = clamp_grid(cols, rows);
        let mut spec = PaneSpec::new(self.shell.program.clone(), cols, rows).args(self.shell.args.clone());
        spec.cell_px = (clamp_cell(cell_w), clamp_cell(cell_h));

        // The sink runs on the pane's engine thread, possibly before `spawn_ghostty` has returned (a failed start
        // reports first), so it only holds the sink and a weak reference and never waits for the handle.
        let sink = self.sink.clone();
        let me = self.me.clone();
        let started = PaneHandle::spawn_ghostty(pane, spec, move |event| match event {
            PaneEvent::Frame(frame) => sink.send(Out::Frame(frame)),
            PaneEvent::Title(title) => sink.send(Out::Title { pane, title }),
            PaneEvent::Cwd(cwd) => sink.send(Out::Cwd { pane, cwd }),
            PaneEvent::Exited { code } => {
                if let Some(core) = me.upgrade() {
                    core.pane_gone(pane, code);
                }
            }
            PaneEvent::Failed(reason) => {
                // The reason names the failure (no program, no PTY), never terminal content.
                eprintln!("holzwave: pane {pane} did not start: {reason}");
                if let Some(core) = me.upgrade() {
                    core.pane_gone(pane, None);
                }
            }
        });
        match started {
            Ok(handle) => {
                let mut panes = lock(&self.panes);
                match panes.get_mut(&pane) {
                    Some(slot @ Slot::Starting) => *slot = Slot::Running(handle),
                    // The pane was closed (or ended, or the app is closing) while the shell started.
                    _ => {
                        drop(panes);
                        handle.close();
                    }
                }
                Ok(())
            }
            // The sink was told, and `pane_gone` forgot the slot, before `spawn_ghostty` returned.
            Err(e) => Err(e.to_string()),
        }
    }

    /// The shell of a pane ended, or never started: forget the handle and tell the webview.
    fn pane_gone(&self, pane: PaneId, code: Option<u32>) {
        let slot = lock(&self.panes).remove(&pane);
        drop(slot);
        self.sink.send(Out::PaneExit { pane, code });
    }

    /// A key press or release, as the webview saw it. The engine encodes it against the terminal's live modes.
    pub fn key(&self, pane: PaneId, key: DomKey) -> Result<(), String> {
        if key.code.len() > MAX_KEY_BYTES || key.key.len() > MAX_KEY_BYTES {
            return Err("key description too long".into());
        }
        match self.handle(pane) {
            Some(handle) => handle.key(key).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    /// Committed text (an IME commit, a dead-key result) as input.
    pub fn text(&self, pane: PaneId, text: String) -> Result<(), String> {
        match self.handle(pane) {
            Some(handle) => handle.text(text).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    /// Paste clipboard text the way the program asked for it. Waits for the engine's answer for up to two seconds,
    /// so the command is async.
    pub fn paste(&self, pane: PaneId, text: String, allow_unsafe: bool) -> Result<PasteOutcome, String> {
        self.handle_or_err(pane)?.paste(text, allow_unsafe).map_err(|e| e.to_string())
    }

    pub fn resize(&self, pane: PaneId, cols: u32, rows: u32, cell_w: u32, cell_h: u32) -> Result<(), String> {
        let Some(handle) = self.handle(pane) else { return Ok(()) };
        let (cols, rows) = clamp_grid(cols, rows);
        handle.resize(cols, rows, (clamp_cell(cell_w), clamp_cell(cell_h))).map_err(|e| e.to_string())
    }

    /// Scroll the viewport by `rows`; negative is towards older output.
    pub fn scroll(&self, pane: PaneId, rows: i32) -> Result<(), String> {
        match self.handle(pane) {
            Some(handle) => handle.scroll(rows as isize).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    pub fn scroll_to_bottom(&self, pane: PaneId) -> Result<(), String> {
        match self.handle(pane) {
            Some(handle) => handle.scroll_to_bottom().map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    /// Make the next frame of the pane a full one: the webview reconnected or saw a gap in the sequence numbers.
    pub fn request_full(&self, pane: PaneId) -> Result<(), String> {
        match self.handle(pane) {
            Some(handle) => handle.request_full().map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    pub fn select(&self, pane: PaneId, op: SelectOp) -> Result<(), String> {
        match self.handle(pane) {
            Some(handle) => handle.select(op).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    /// The selected text; waits for the engine's answer, so the command is async.
    pub fn selection_text(&self, pane: PaneId) -> Result<Option<String>, String> {
        self.handle_or_err(pane)?.selection_text().map_err(|e| e.to_string())
    }

    /// Change the search and return its state; waits for the engine's answer, so the command is async.
    pub fn search(&self, pane: PaneId, op: SearchOp) -> Result<SearchState, String> {
        self.handle_or_err(pane)?.search(op).map_err(|e| e.to_string())
    }

    /// Run a copy-mode operation and return its state; waits for the engine's answer, so the command is async.
    pub fn copy_mode(&self, pane: PaneId, op: CopyOp) -> Result<CopyState, String> {
        self.handle_or_err(pane)?.copy_mode(op).map_err(|e| e.to_string())
    }

    #[cfg(test)]
    fn pid(&self, pane: PaneId) -> Option<u32> {
        self.handle(pane)?.pid()
    }
}

/// The longest `code` or `key` string of a key event the shell passes on (T-02-50): the longest real value is a
/// named key such as `MediaTrackPrevious`.
const MAX_KEY_BYTES: usize = 32;

/// The grid of a pane: at least 2 x 1, at most 1000 x 500.
fn clamp_grid(cols: u32, rows: u32) -> (u16, u16) {
    (cols.clamp(2, 1000) as u16, rows.clamp(1, 500) as u16)
}

/// The size of a cell in device pixels: 0 (unknown) up to 256.
fn clamp_cell(px: u32) -> u32 {
    px.min(256)
}

/// Read the layout file. A file that is missing gives a fresh layout. One that is damaged or inconsistent is
/// repaired or replaced, and its original bytes are copied to `<path>.bad` before anything can overwrite them. One
/// that is larger than `MAX_LAYOUT_BYTES` is not read; it is moved to `<path>.bad`.
fn load_layout(path: &Path) -> Layout {
    match persist::read_limited(path, MAX_LAYOUT_BYTES) {
        Ok(None) => Layout::new().0,
        Ok(Some(bytes)) => {
            let (layout, status) = match std::str::from_utf8(&bytes) {
                Ok(text) => Layout::from_json(text),
                Err(e) => (Layout::new().0, LoadStatus::Reset(format!("not UTF-8: {e}"))),
            };
            match &status {
                LoadStatus::Clean => {}
                LoadStatus::Repaired(repairs) => {
                    eprintln!("holzwave: {} repaired: {}", path.display(), repairs.join("; "));
                    keep_bad_copy(path);
                }
                LoadStatus::Reset(reason) => {
                    eprintln!("holzwave: {} reset: {reason}", path.display());
                    keep_bad_copy(path);
                }
            }
            layout
        }
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            eprintln!("holzwave: {e}; set aside as .bad");
            let bad = persist::bad_path(path);
            if let Err(e) = std::fs::rename(path, &bad) {
                eprintln!("holzwave: cannot move {} to {}: {e}", path.display(), bad.display());
            }
            Layout::new().0
        }
        Err(e) => {
            eprintln!("holzwave: cannot read {}: {e}", path.display());
            Layout::new().0
        }
    }
}

/// Copy a layout file that could not be used as it was to `<path>.bad`.
fn keep_bad_copy(path: &Path) {
    if let Err(e) = persist::backup_bad(path) {
        eprintln!("holzwave: cannot keep a copy of {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use holzwave_core::engine::{CopyMotion, Mods, SelectMode};
    use holzwave_core::layout::Axis;

    const WAIT: Duration = Duration::from_secs(5);

    /// Collects what the core tells the webview.
    #[derive(Default)]
    struct Recorder {
        log: Mutex<Vec<Out>>,
    }

    impl Sink for Recorder {
        fn send(&self, out: Out) {
            lock(&self.log).push(out);
        }
    }

    impl Recorder {
        fn all(&self) -> Vec<Out> {
            lock(&self.log).clone()
        }

        fn frames(&self, pane: PaneId) -> Vec<FrameMsg> {
            self.all()
                .into_iter()
                .filter_map(|o| match o {
                    Out::Frame(f) if f.pane == pane => Some(f),
                    _ => None,
                })
                .collect()
        }

        /// Whether any frame of the pane carried a changed row with this text.
        fn has_text(&self, pane: PaneId, needle: &str) -> bool {
            self.frames(pane).iter().any(|f| {
                f.changed.iter().any(|row| {
                    let text: String = row.runs.iter().flat_map(|r| r.0.chars()).filter(|c| *c != '\0').collect();
                    text.contains(needle)
                })
            })
        }

        fn titles(&self) -> Vec<(PaneId, String)> {
            self.all()
                .into_iter()
                .filter_map(|o| match o {
                    Out::Title { pane, title } => Some((pane, title)),
                    _ => None,
                })
                .collect()
        }

        fn pane_exits(&self) -> Vec<(PaneId, Option<u32>)> {
            self.all()
                .into_iter()
                .filter_map(|o| match o {
                    Out::PaneExit { pane, code } => Some((pane, code)),
                    _ => None,
                })
                .collect()
        }
    }

    fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if f() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A fresh scratch directory, removed at the end of the test.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("holzwave-app-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path).expect("scratch dir");
            Scratch(path)
        }

        fn layout_file(&self) -> PathBuf {
            self.0.join("layout.json")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A core whose panes run `program args` and whose events land in the returned recorder.
    fn core(program: &str, args: &[&str]) -> (Arc<AppCore>, Arc<Recorder>, Scratch) {
        let dir = Scratch::new("core");
        let rec = Arc::new(Recorder::default());
        let core = AppCore::new(dir.0.clone(), rec.clone(), ShellSpec::new(program, args));
        (core, rec, dir)
    }

    fn sh(script: &str) -> (Arc<AppCore>, Arc<Recorder>, Scratch) {
        core("/bin/sh", &["-c", script])
    }

    fn only_pane(core: &AppCore) -> PaneId {
        core.view().panes[0].0
    }

    fn key(code: &str, key: &str) -> DomKey {
        DomKey { code: code.into(), key: key.into(), mods: Mods::NONE, ..DomKey::default() }
    }

    #[test]
    fn a_new_core_with_an_empty_directory_has_one_pane_and_focuses_it() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);

        let view = core.view();

        assert_eq!(view.panes.len(), 1);
        assert_eq!(view.focus, view.panes[0].0);
        assert_eq!(view.workspaces.len(), 1);
        assert_eq!(view.tabs.len(), 1);
        assert_eq!(view.tabs[0].focus, view.focus, "the tab reports its focus pane");
    }

    #[test]
    fn start_pane_starts_one_shell_that_streams_frames_and_a_second_call_changes_nothing() {
        let (core, rec, _dir) = sh("printf hello; sleep 5");
        let pane = only_pane(&core);

        core.start_pane(pane, 80, 24, 0, 0).expect("the pane starts");
        assert!(wait_until(WAIT, || rec.has_text(pane, "hello")), "no frame with the text: {:?}", rec.all());
        let pid = core.pid(pane).expect("a running pane has a pid");

        core.start_pane(pane, 80, 24, 0, 0).expect("a second start is not an error");
        assert_eq!(core.pid(pane), Some(pid), "the second call must not start a second shell");
        std::thread::sleep(Duration::from_millis(150));
        assert!(rec.pane_exits().is_empty(), "the first shell was not replaced: {:?}", rec.all());
    }

    #[test]
    fn start_pane_for_a_pane_that_is_not_in_the_layout_is_an_error_and_starts_nothing() {
        let (core, rec, _dir) = core("/bin/cat", &[]);

        let result = core.start_pane(9_999, 80, 24, 0, 0);

        assert!(result.is_err());
        assert_eq!(core.pid(9_999), None);
        std::thread::sleep(Duration::from_millis(100));
        assert!(rec.all().is_empty(), "nothing was started, so nothing is reported: {:?}", rec.all());
    }

    #[test]
    fn the_grid_and_the_cell_size_are_clamped() {
        assert_eq!(clamp_grid(0, 0), (2, 1));
        assert_eq!(clamp_grid(80, 24), (80, 24));
        assert_eq!(clamp_grid(5_000, 5_000), (1000, 500));
        assert_eq!(clamp_grid(u32::MAX, u32::MAX), (1000, 500));
        assert_eq!(clamp_cell(0), 0);
        assert_eq!(clamp_cell(9), 9);
        assert_eq!(clamp_cell(100_000), 256);
    }

    #[test]
    fn a_pane_started_with_a_zero_grid_gets_the_smallest_grid() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);

        core.start_pane(pane, 0, 0, 0, 0).expect("starts");

        assert!(wait_until(WAIT, || !rec.frames(pane).is_empty()), "no frame: {:?}", rec.all());
        let first = rec.frames(pane).remove(0);
        assert_eq!((first.cols, first.rows), (2, 1));
    }

    #[test]
    fn the_cell_size_reaches_the_shell_in_size_reports() {
        // CSI 16 t asks for the cell size; the reply CSI 6 ; height ; width t arrives as tty input, which the tty
        // echoes (ESC as ^[) into the output, so the answer is visible in a frame.
        let (core, rec, _dir) = sh("printf '\\033[16t'; sleep 5");
        let pane = only_pane(&core);

        core.start_pane(pane, 80, 24, 9, 18).expect("starts");

        assert!(
            wait_until(WAIT, || rec.has_text(pane, "[6;18;9t")),
            "the size report did not carry 9 x 18: {:?}",
            rec.frames(pane).iter().map(|f| f.changed.iter().map(|r| r.runs.clone()).collect::<Vec<_>>()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_key_for_a_cat_shell_comes_back_as_a_frame_with_the_character() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");

        core.key(pane, key("KeyX", "x")).expect("the key is accepted");

        assert!(wait_until(WAIT, || rec.has_text(pane, "x")), "no echo: {:?}", rec.all());
    }

    #[test]
    fn a_key_with_a_code_or_key_over_32_bytes_is_rejected_and_an_unknown_pane_is_a_no_op() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        let long = "K".repeat(33);

        assert!(core.key(pane, key(&long, "q")).is_err(), "code of 33 bytes");
        assert!(core.key(pane, key("KeyQ", &long)).is_err(), "key of 33 bytes");
        assert!(core.key(pane, key(&"K".repeat(32), "w")).is_ok(), "32 bytes are fine");
        assert!(core.key(424_242, key("KeyX", "x")).is_ok(), "an unknown pane is a no-op");
        std::thread::sleep(Duration::from_millis(100));
        assert!(!rec.has_text(pane, "q"), "a rejected key must not reach the shell");
    }

    #[test]
    fn a_title_and_the_end_of_the_shell_reach_the_sink() {
        let (core, rec, _dir) = sh("printf '\\033]2;build done\\007'; sleep 0.3; exit 3");
        let pane = only_pane(&core);

        core.start_pane(pane, 80, 24, 0, 0).expect("starts");

        assert!(wait_until(WAIT, || !rec.pane_exits().is_empty()), "no PaneExit: {:?}", rec.all());
        assert_eq!(rec.titles(), vec![(pane, "build done".to_string())]);
        assert_eq!(rec.pane_exits(), vec![(pane, Some(3))]);
        assert_eq!(core.pid(pane), None, "the handle of an ended pane is gone");
    }

    #[test]
    fn a_multi_line_paste_is_rejected_without_bracketed_mode_until_it_is_allowed() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");

        assert_eq!(core.paste(pane, "one\ntwo".into(), false), Ok(PasteOutcome::Rejected));
        assert_eq!(core.paste(pane, String::new(), false), Ok(PasteOutcome::Empty));
        assert_eq!(core.paste(pane, "one\ntwo".into(), true), Ok(PasteOutcome::Written));
        assert!(wait_until(WAIT, || rec.has_text(pane, "two")), "the allowed paste never arrived: {:?}", rec.all());
        assert!(core.paste(31_337, "x".into(), false).is_err(), "an unknown pane cannot be pasted into");
    }

    #[test]
    fn a_selection_is_readable_and_the_search_and_copy_mode_answer_with_their_state() {
        let (core, rec, _dir) = sh("printf 'find me\\r\\nother\\r\\nfind again'; sleep 5");
        let pane = only_pane(&core);
        core.start_pane(pane, 40, 10, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || rec.has_text(pane, "find again")), "no output: {:?}", rec.all());

        core.select(pane, SelectOp::Begin { x: 0, y: 0, mode: SelectMode::Cell }).expect("select");
        core.select(pane, SelectOp::Extend { x: 3, y: 0 }).expect("extend");
        assert_eq!(core.selection_text(pane), Ok(Some("find".to_string())));

        let found = core.search(pane, SearchOp::Set("find".into())).expect("search");
        assert_eq!(found.total, 2);
        assert!(found.current.is_some());
        assert_eq!(core.search(pane, SearchOp::Clear).expect("clear").total, 0);

        assert!(core.copy_mode(pane, CopyOp::Enter).expect("enter").active);
        assert!(core.copy_mode(pane, CopyOp::Move(CopyMotion::Up)).expect("move").active);
        assert!(!core.copy_mode(pane, CopyOp::Exit).expect("exit").active);

        assert!(core.selection_text(555).is_err());
        assert!(core.search(555, SearchOp::Next).is_err());
        assert!(core.copy_mode(555, CopyOp::Enter).is_err());
    }

    // ---- layout commands, closing, persistence (task 2) --------------------------------------------------------

    /// A shell that ignores SIGHUP and says when it is ready: only a real kill ends it.
    const HUP_PROOF: &str = "trap '' HUP; echo ready; exec sleep 1000";

    fn process_exists(pid: u32) -> bool {
        std::process::Command::new("/bin/sh")
            .args(["-c", "kill -0 \"$1\" 2>/dev/null", "sh", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    impl Recorder {
        fn layouts(&self) -> Vec<View> {
            self.all()
                .into_iter()
                .filter_map(|o| match o {
                    Out::Layout(v) => Some(v),
                    _ => None,
                })
                .collect()
        }

        fn exit_count(&self) -> usize {
            self.all().iter().filter(|o| matches!(o, Out::Exit)).count()
        }
    }

    fn snapshot(core: &AppCore) -> Layout {
        lock(&core.layout).clone()
    }

    fn on_disk(dir: &Scratch) -> Layout {
        let text = std::fs::read_to_string(dir.layout_file()).expect("layout.json");
        let (layout, status) = Layout::from_json(&text);
        assert_eq!(status, LoadStatus::Clean, "what the core writes loads clean");
        layout
    }

    #[test]
    fn closing_one_of_two_panes_kills_its_shell_removes_it_and_does_not_exit() {
        let (core, rec, _dir) = sh(HUP_PROOF);
        let first = only_pane(&core);
        let second = core.split(false).focus;
        assert_ne!(first, second);
        core.start_pane(first, 80, 24, 0, 0).expect("first starts");
        core.start_pane(second, 80, 24, 0, 0).expect("second starts");
        assert!(
            wait_until(WAIT, || rec.has_text(first, "ready") && rec.has_text(second, "ready")),
            "the shells never said ready: {:?}",
            rec.all().len()
        );
        let pid = core.pid(second).expect("pid");
        assert!(process_exists(pid));

        let after = core.close_pane(second);

        assert_eq!(after.panes.len(), 1);
        assert_eq!(after.panes[0].0, first);
        assert!(
            wait_until(Duration::from_secs(3), || rec.pane_exits().iter().any(|(p, _)| *p == second)),
            "no PaneExit within 3 seconds"
        );
        assert!(!process_exists(pid), "the shell of the closed pane is still there");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(rec.pane_exits().iter().filter(|(p, _)| *p == second).count(), 1, "exactly one PaneExit");
        assert_eq!(rec.layouts().len(), 1, "exactly one Layout: {:?}", rec.layouts().len());
        assert_eq!(rec.exit_count(), 0, "closing one pane of two must not end the app");
        assert!(core.pid(first).is_some(), "the other pane keeps its shell");
    }

    #[test]
    fn a_pane_that_never_started_reports_its_exit_when_it_is_closed() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let first = only_pane(&core);
        let second = core.split(false).focus;

        let after = core.close_pane(second);

        assert_eq!(after.panes.iter().map(|p| p.0).collect::<Vec<_>>(), vec![first]);
        assert_eq!(rec.pane_exits(), vec![(second, None)]);
        assert_eq!(rec.layouts().len(), 1);
        assert_eq!(rec.exit_count(), 0);
    }

    #[test]
    fn closing_the_only_pane_flushes_emits_exit_once_and_kills_the_shell() {
        let (core, rec, dir) = sh(HUP_PROOF);
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || rec.has_text(pane, "ready")), "never ready");
        let pid = core.pid(pane).expect("pid");
        core.focus_pane(pane); // a change: the layout is dirty and its save is still pending
        assert!(!dir.layout_file().exists(), "the debounce window is still open");

        let view = core.close_pane(pane);

        assert_eq!(view.panes.len(), 1, "the last pane stays in the layout");
        assert_eq!(rec.exit_count(), 1);
        assert!(dir.layout_file().exists(), "the layout is on disk when Exit is sent");
        assert!(wait_until(Duration::from_secs(3), || !process_exists(pid)), "the shell survived");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(rec.exit_count(), 1, "Exit is sent once");
    }

    #[test]
    fn closing_a_pane_that_is_in_no_tab_changes_nothing_and_emits_nothing() {
        let (core, rec, dir) = core("/bin/cat", &[]);
        let before = core.view();

        let after = core.close_pane(31_337);

        assert_eq!(after, before);
        std::thread::sleep(Duration::from_millis(100));
        assert!(rec.all().is_empty(), "{:?}", rec.all());
        assert!(!dir.layout_file().exists(), "nothing changed, nothing is written");
    }

    #[test]
    fn a_shell_that_exits_by_itself_closes_its_pane_and_only_the_last_one_ends_the_app() {
        let (core, rec, dir) = sh("exit 0");
        let first = only_pane(&core);
        let second = core.split(false).focus;

        // Only the first pane has a shell; it ends at once.
        core.start_pane(first, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || !rec.layouts().is_empty()), "the pane stayed in the layout: {:?}", rec.all());
        assert_eq!(core.view().panes.iter().map(|p| p.0).collect::<Vec<_>>(), vec![second]);
        assert!(wait_until(WAIT, || rec.pane_exits().iter().any(|(p, _)| *p == first)), "no PaneExit");
        assert_eq!(rec.exit_count(), 0, "one of two panes ended: the app keeps running");

        // The last pane's shell ends: the app ends too.
        core.start_pane(second, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || rec.exit_count() > 0), "no Exit: {:?}", rec.all());
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(rec.exit_count(), 1);
        assert!(dir.layout_file().exists(), "flushed before Exit");
        assert_eq!(on_disk(&dir).all_panes(), vec![second], "the layout of the last pane is kept");
    }

    #[test]
    fn a_burst_of_divider_moves_writes_at_most_twice_and_the_flushed_file_equals_memory() {
        let (core, _rec, dir) = core("/bin/cat", &[]);
        let split = core.split(false).dividers[0].split;

        for i in 0..1000 {
            core.set_ratio(split, 0.2 + (i % 600) as f32 / 1000.0);
        }
        core.flush();

        assert!(core.layout_writes() <= 2, "{} writes for one burst", core.layout_writes());
        assert!(core.layout_writes() >= 1);
        assert_eq!(on_disk(&dir), snapshot(&core));
    }

    #[test]
    fn shutdown_writes_the_pending_layout_and_ends_every_shell_within_three_seconds() {
        let (core, rec, dir) = sh(HUP_PROOF);
        let first = only_pane(&core);
        let view = core.split(false);
        let second = view.focus;
        core.start_pane(first, 80, 24, 0, 0).expect("first starts");
        core.start_pane(second, 80, 24, 0, 0).expect("second starts");
        assert!(wait_until(WAIT, || rec.has_text(first, "ready") && rec.has_text(second, "ready")), "never ready");
        let pids = [core.pid(first).expect("pid"), core.pid(second).expect("pid")];
        core.set_ratio(view.dividers[0].split, 0.3);

        let started = Instant::now();
        core.shutdown();

        assert!(started.elapsed() < Duration::from_secs(4), "shutdown took {:?}", started.elapsed());
        for pid in pids {
            assert!(!process_exists(pid), "shell {pid} survived the shutdown");
        }
        let saved = on_disk(&dir);
        assert_eq!(saved, snapshot(&core));
        assert_eq!(saved.all_panes().len(), 2, "the panes ending at shutdown must not be taken out of the layout");
        assert!((saved.tab().geometry().1[0].ratio - 0.3).abs() < 1e-6, "the pending move is on disk");
        assert_eq!(rec.exit_count(), 0, "a shutdown is not asking the shell to exit again");
    }

    #[test]
    fn dropping_the_core_writes_the_pending_layout_and_ends_every_shell() {
        let (core, rec, dir) = sh(HUP_PROOF);
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || rec.has_text(pane, "ready")), "never ready");
        let pid = core.pid(pane).expect("pid");
        core.new_tab();
        let expected = snapshot(&core);

        drop(core);

        assert!(wait_until(Duration::from_secs(3), || !process_exists(pid)), "the shell survived the drop");
        assert_eq!(on_disk(&dir), expected);
    }

    #[test]
    fn the_layout_commands_return_the_new_view_and_split_stops_at_the_cap() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);

        let v = core.new_tab();
        assert_eq!(v.tabs.len(), 2);
        assert!(v.tabs[1].active);
        let v = core.select_tab(0);
        assert!(v.tabs[0].active);
        let v = core.new_workspace("two");
        assert_eq!(v.workspaces.len(), 2);
        assert!(v.workspaces[1].active);
        let v = core.select_workspace(0);
        assert!(v.workspaces[0].active);

        let mut view = core.view();
        let first = view.focus;
        view = core.split(true);
        assert_eq!(view.panes.len(), 2);
        let next = core.cycle_focus(1);
        assert_ne!(next.focus, view.focus, "the focus moved");
        assert_eq!(core.focus_pane(first).focus, first);

        // Always splitting the largest pane keeps the tree shallow, so the cap of 64 panes per tab is what stops it
        // and not the depth limit.
        for k in 0..62 {
            let largest = view
                .panes
                .iter()
                .max_by(|a, b| (a.1.w * a.1.h).total_cmp(&(b.1.w * b.1.h)))
                .map(|p| p.0)
                .expect("a pane");
            core.focus_pane(largest);
            view = core.split(k % 2 == 0);
        }
        assert_eq!(view.panes.len(), 64);
        assert_eq!(core.split(false), view, "a split past the cap returns the unchanged view");
    }

    #[test]
    fn a_layout_file_with_no_workspaces_starts_fresh_and_keeps_a_bad_copy() {
        let dir = Scratch::new("empty");
        let damaged = r#"{"workspaces":[],"active":0,"next_id":5}"#;
        std::fs::write(dir.layout_file(), damaged).unwrap();

        let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());

        assert_eq!(core.view().panes.len(), 1);
        assert_eq!(std::fs::read_to_string(dir.0.join("layout.json.bad")).unwrap(), damaged);
    }

    #[test]
    fn a_valid_layout_file_is_left_byte_identical_until_the_first_change() {
        let dir = Scratch::new("valid");
        let (mut layout, _) = Layout::new();
        layout.split_focused(Axis::Row);
        let bytes = serde_json::to_vec_pretty(&layout).unwrap();
        std::fs::write(dir.layout_file(), &bytes).unwrap();

        let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());
        core.flush();

        assert_eq!(std::fs::read(dir.layout_file()).unwrap(), bytes, "loading and flushing change nothing");
        assert!(!dir.0.join("layout.json.bad").exists());
        let split = core.view().dividers[0].split;
        core.set_ratio(split, 0.3);
        core.flush();
        assert_ne!(std::fs::read(dir.layout_file()).unwrap(), bytes, "the first change is written");
    }

    #[test]
    fn the_layout_survives_a_restart() {
        let dir = Scratch::new("restart");
        let before = {
            let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());
            core.split(false);
            core.new_workspace("two");
            core.flush();
            snapshot(&core)
        };

        let again = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());

        assert_eq!(snapshot(&again), before);
    }

    #[test]
    fn saving_creates_the_directory_and_leaves_no_temporary_file() {
        let dir = Scratch::new("nested");
        let nested = dir.0.join("nested");
        let core = AppCore::new(nested.clone(), Arc::new(Recorder::default()), ShellSpec::default());

        core.new_tab();
        core.flush();

        let names: Vec<_> = std::fs::read_dir(&nested)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["layout.json"]);
    }

    // ---- loading the layout file (ported from the old shell) -----------------------------------------------------

    #[test]
    fn a_corrupt_layout_file_falls_back_to_a_fresh_layout_and_is_kept() {
        let dir = Scratch::new("corrupt");
        std::fs::write(dir.layout_file(), "{ not json").unwrap();

        let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());

        assert_eq!(lock(&core.layout).all_panes().len(), 1);
        assert_eq!(
            std::fs::read_to_string(dir.0.join("layout.json.bad")).unwrap(),
            "{ not json",
            "the damaged file is kept next to the layout"
        );
    }

    #[test]
    fn a_layout_file_over_one_mebibyte_is_not_read_and_keeps_a_bad_copy() {
        let dir = Scratch::new("oversize");
        // A perfectly valid layout, padded with blanks past the limit: the size alone is the reason.
        let mut text = r#"{"workspaces":[{"id":1,"name":"main","tabs":[{"id":2,"title":"shell","root":{"Pane":3},"focus":3}],"active":0}],"active":0,"next_id":4}"#.to_owned();
        text.push_str(&" ".repeat(MAX_LAYOUT_BYTES as usize));
        std::fs::write(dir.layout_file(), &text).unwrap();

        let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());

        assert_eq!(*lock(&core.layout), Layout::new().0, "an oversized file is not used");
        let bad = std::fs::metadata(dir.0.join("layout.json.bad")).expect("a bad copy next to the file");
        assert_eq!(bad.len(), text.len() as u64);
    }

    #[test]
    fn a_layout_with_out_of_range_indices_loads_and_keeps_a_bad_copy() {
        let dir = Scratch::new("badcopy");
        let damaged = r#"{"workspaces":[{"id":1,"name":"main","tabs":[{"id":2,"title":"shell","root":{"Pane":3},"focus":3}],"active":7}],"active":3,"next_id":4}"#;
        std::fs::write(dir.layout_file(), damaged).unwrap();

        let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());

        {
            let l = lock(&core.layout);
            assert_eq!(l.active, 0);
            assert_eq!(l.workspace().active, 0);
            assert_eq!(l.all_panes(), vec![3], "the repaired layout keeps the file's pane");
        }
        assert_eq!(std::fs::read_to_string(dir.0.join("layout.json.bad")).unwrap(), damaged);
        assert_eq!(
            std::fs::read_to_string(dir.layout_file()).unwrap(),
            damaged,
            "loading alone does not rewrite the file"
        );
    }
}
