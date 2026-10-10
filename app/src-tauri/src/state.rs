//! The behaviour of the app shell, without Tauri (D-19).
//!
//! [`AppCore`] owns the layout and the pane handles and answers every command of the webview. It knows nothing about
//! windows: what the webview has to be told leaves through a [`Sink`] as an [`Out`], and `lib.rs` maps those to Tauri
//! events. That is what makes the shell testable headless, with a recording sink and real shells.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use holzwave_core::engine::{CopyOp, CopyState, DomKey, PasteOutcome, SearchOp, SearchState, SelectOp};
use holzwave_core::layout::{Divider, Layout, LoadStatus, PaneId, Rect};
use holzwave_core::pane::PaneHandle;
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
            me: me.clone(),
        })
    }

    pub fn view(&self) -> View {
        View::of(&lock(&self.layout))
    }

    // Skeleton: the pane commands do nothing yet.

    pub fn start_pane(&self, _pane: PaneId, _cols: u32, _rows: u32, _cell_w: u32, _cell_h: u32) -> Result<(), String> {
        let _ = (&self.panes, &self.sink, &self.shell, &self.dir, &self.exiting, &self.me);
        Err("not implemented".into())
    }

    pub fn key(&self, _pane: PaneId, _key: DomKey) -> Result<(), String> {
        Ok(())
    }

    pub fn text(&self, _pane: PaneId, _text: String) -> Result<(), String> {
        Ok(())
    }

    pub fn paste(&self, _pane: PaneId, _text: String, _allow_unsafe: bool) -> Result<PasteOutcome, String> {
        Err("not implemented".into())
    }

    pub fn resize(&self, _pane: PaneId, _cols: u32, _rows: u32, _cell_w: u32, _cell_h: u32) -> Result<(), String> {
        Ok(())
    }

    pub fn scroll(&self, _pane: PaneId, _rows: i32) -> Result<(), String> {
        Ok(())
    }

    pub fn scroll_to_bottom(&self, _pane: PaneId) -> Result<(), String> {
        Ok(())
    }

    pub fn request_full(&self, _pane: PaneId) -> Result<(), String> {
        Ok(())
    }

    pub fn select(&self, _pane: PaneId, _op: SelectOp) -> Result<(), String> {
        Ok(())
    }

    pub fn selection_text(&self, _pane: PaneId) -> Result<Option<String>, String> {
        Err("not implemented".into())
    }

    pub fn search(&self, _pane: PaneId, _op: SearchOp) -> Result<SearchState, String> {
        Err("not implemented".into())
    }

    pub fn copy_mode(&self, _pane: PaneId, _op: CopyOp) -> Result<CopyState, String> {
        Err("not implemented".into())
    }

    #[cfg(test)]
    fn pid(&self, _pane: PaneId) -> Option<u32> {
        None
    }
}

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
            rec.frames(pane).len()
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

        assert!(core.key(pane, key(&long, "x")).is_err(), "code of 33 bytes");
        assert!(core.key(pane, key("KeyX", &long)).is_err(), "key of 33 bytes");
        assert!(core.key(pane, key(&"K".repeat(32), "x")).is_ok(), "32 bytes are fine");
        assert!(core.key(424_242, key("KeyX", "x")).is_ok(), "an unknown pane is a no-op");
        std::thread::sleep(Duration::from_millis(100));
        assert!(!rec.has_text(pane, "x"), "a rejected key must not reach the shell");
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
