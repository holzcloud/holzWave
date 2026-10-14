//! The behaviour of the app shell, without Tauri (D-19).
//!
//! [`AppCore`] owns the layouts of all windows and the pane handles and answers every command of the webview. It
//! knows windows by their label and nothing else of them: what a webview has to be told leaves through a [`Sink`] as
//! an [`Out`] with the [`Target`] it is for, and `lib.rs` maps those to Tauri events. Opening, closing and finding
//! windows are calls on the sink too. That is what makes the shell testable headless, with a recording sink (that
//! can refuse to open a window) and real shells.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

use holzwave_core::engine::{
    CopyOp, CopyState, DomKey, EngineTheme, Mods, MouseAction, MouseButton, MouseEvent, PasteOutcome, SearchOp,
    SearchState, SelectOp, WheelEvent,
};
use holzwave_core::keybindings::Keybindings;
use holzwave_core::layout::{
    AttachTo, Axis, Divider, Edge, Layout, LoadStatus, MAIN_WINDOW, Moved, PaneId, Rect, SetClose, SetCloseGroup, SplitId, WindowSet,
};
use holzwave_core::pane::{EditAction, PaneEvent, PaneHandle, PaneSpec};
use holzwave_core::persist::{self, Debounced};
use holzwave_core::platform::{
    Os, ProgramCheck, ShellLaunch, ShellResolved, host_shell_launch, in_flatpak, resolve_shell,
};
use holzwave_core::pty::process_cwd;
use holzwave_core::session::{MonitorRect, Placement, SESSION_FILE, Session, WindowState};
use holzwave_core::settings::{SETTINGS_FILE, Settings};
use holzwave_core::theme::{DEFAULT_THEME, THEMES_DIR, Theme, ThemeInfo, ThemeRegistry};
use holzwave_core::wire::FrameMsg;
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::windows::{CursorSpot, DropKind, Drops, Place};

/// A layout file larger than this is not read: a real layout is a few KiB.
const MAX_LAYOUT_BYTES: u64 = 1 << 20;

/// How long a layout change waits before it is written (RESEARCH Pitfall 3): a divider drag is hundreds of changes
/// and costs one write per window.
const SAVE_DELAY: Duration = Duration::from_millis(250);

/// How long a change of a working directory or of the window waits before the session is written. The session is a
/// hint for the next start, so it can be slower than the layout.
const SESSION_DELAY: Duration = Duration::from_secs(2);

/// The most time `shutdown` spends waiting for the shells to end.
const SHUTDOWN_BOUND: Duration = Duration::from_secs(3);

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
///
/// A view reaches the page on more than one path (the answer of a command and the `layout` event), and the paths
/// are not ordered against each other. `rev` is what orders them: it grows with every view the core builds, and
/// the page drops a view that is older than the one it shows.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct View {
    /// The place of this view in the order the core built them, from 1.
    pub rev: u64,
    pub workspaces: Vec<WorkspaceView>,
    pub tabs: Vec<TabView>,
    pub focus: PaneId,
    pub panes: Vec<(PaneId, Rect)>,
    pub dividers: Vec<Divider>,
    /// The names the user gave to the panes of the active tab; a pane without an entry has no name.
    pub pane_names: Vec<(PaneId, String)>,
}

impl View {
    /// The view of `layout`, stamped `rev`. Take `rev` with [`AppCore::view_of`], under the layout lock.
    pub fn of(layout: &Layout, rev: u64) -> View {
        let (panes, dividers) = layout.tab().geometry();
        let active_tab = layout.workspace().active;
        View {
            rev,
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
            pane_names: panes
                .iter()
                .filter_map(|(pane, _)| layout.pane_name(*pane).map(|name| (*pane, name.to_string())))
                .collect(),
            panes,
            dividers,
        }
    }
}

// ---- settings, themes and key bindings ---------------------------------------------------------------------------

/// One row of the key binding editor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KeybindingView {
    pub action: String,
    pub title: String,
    /// The chord in force in its canonical spelling, `null` when the action is unbound.
    pub chord: Option<String>,
    pub is_default: bool,
}

/// What the webview needs to draw the settings, the theme menu and the key binding editor, and to paint with the
/// theme in force. Every field is data from the core; the page decides nothing about validity.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SettingsView {
    pub settings: Settings,
    /// Every theme, built-ins first.
    pub themes: Vec<ThemeInfo>,
    /// The theme in force: the one `settings.theme` names, aubergine when there is none of that name.
    pub theme: Theme,
    pub keybindings: Vec<KeybindingView>,
    /// Plain sentences about what could not be used: load notes of `settings.json` and the theme files, and the
    /// key binding overrides that the resolver refused.
    pub warnings: Vec<String>,
}

impl SettingsView {
    pub fn new(
        settings: &Settings,
        themes: &ThemeRegistry,
        keybindings: &Keybindings,
        warnings: Vec<String>,
    ) -> SettingsView {
        SettingsView {
            settings: settings.clone(),
            themes: themes.list(),
            theme: themes.get(&settings.theme).clone(),
            keybindings: keybindings
                .table()
                .into_iter()
                .map(|row| KeybindingView {
                    action: row.action.to_owned(),
                    title: row.title.to_owned(),
                    chord: row.chord,
                    is_default: row.is_default,
                })
                .collect(),
            warnings,
        }
    }
}

/// How many bytes and when each configuration file was last changed, so that a reload can tell whether anything
/// changed without reading a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Stamps {
    settings: Option<(SystemTime, u64)>,
    themes: Vec<(OsString, SystemTime, u64)>,
}

/// What a pane shows and how its keys behave, as far as the settings decide it: the part of the configuration that
/// running panes are told about when it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PaneLook {
    theme: EngineTheme,
    option_as_alt: bool,
}

/// The settings, the themes and the resolved key bindings that are in force.
struct Config {
    settings: Settings,
    themes: ThemeRegistry,
    keybindings: Keybindings,
    /// What loading `settings.json` and the theme files had to say.
    file_notes: Vec<String>,
    /// What the key binding resolver refused.
    binding_notes: Vec<String>,
    stamps: Stamps,
}

impl Config {
    fn new(settings: Settings, themes: ThemeRegistry, file_notes: Vec<String>, stamps: Stamps) -> Config {
        let (keybindings, binding_notes) = Keybindings::resolve(&settings.keybindings);
        Config { settings, themes, keybindings, file_notes, binding_notes, stamps }
    }

    /// Read `settings.json` and `themes/` of `dir`. Never fails: what cannot be used is reported as a note.
    fn load(dir: &Path) -> Config {
        // Stamped first: a file that changes while it is read is then seen as changed by the next reload.
        let stamped = stamps(dir);
        let (settings, status) = Settings::load(dir);
        let mut notes = status_notes(SETTINGS_FILE, status);
        let (themes, theme_notes) = ThemeRegistry::load(dir);
        notes.extend(theme_notes);
        Config::new(settings, themes, notes, stamped)
    }

    /// Replace the settings and resolve the key bindings again.
    fn set_settings(&mut self, settings: Settings) {
        let (keybindings, binding_notes) = Keybindings::resolve(&settings.keybindings);
        self.settings = settings;
        self.keybindings = keybindings;
        self.binding_notes = binding_notes;
    }

    fn view(&self) -> SettingsView {
        let mut warnings = self.file_notes.clone();
        if !self.themes.contains(&self.settings.theme) {
            // The choice is kept as it is: a theme file that is missing for a moment must not rewrite it.
            warnings.push(format!(
                "there is no theme called {:?}; {} is used",
                self.settings.theme.chars().take(40).collect::<String>(),
                DEFAULT_THEME
            ));
        }
        warnings.extend(self.binding_notes.iter().cloned());
        if !self.settings.shell.is_empty() {
            // Looked at each time the view is made: a program that appears or disappears changes the sentence.
            warnings.extend(resolve_system_shell(&ShellSpec::System, &self.settings.shell).unusable);
        }
        SettingsView::new(&self.settings, &self.themes, &self.keybindings, warnings)
    }

    /// The theme in force.
    fn theme(&self) -> &Theme {
        self.themes.get(&self.settings.theme)
    }

    fn look(&self) -> PaneLook {
        PaneLook { theme: self.theme().engine_theme(), option_as_alt: self.settings.option_as_alt }
    }
}

/// One sentence per defect of a loaded document; nothing for a clean load.
fn status_notes(file: &str, status: LoadStatus) -> Vec<String> {
    match status {
        LoadStatus::Clean => Vec::new(),
        LoadStatus::Repaired(notes) => notes.into_iter().map(|n| format!("{file}: {n}")).collect(),
        LoadStatus::Reset(reason) => vec![format!("{file}: {reason}; the defaults are used")],
    }
}

/// The size and modification time of `settings.json` and of every `themes/*.json` of `dir`. Only metadata is read, and
/// at most as many theme files as the registry reads.
fn stamps(dir: &Path) -> Stamps {
    use holzwave_core::theme::MAX_THEME_FILES;
    use std::collections::BTreeSet;

    let stamp = |meta: &std::fs::Metadata| (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
    let settings = std::fs::metadata(dir.join(SETTINGS_FILE)).ok().map(|m| stamp(&m));

    let mut names: BTreeSet<OsString> = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(dir.join(THEMES_DIR)) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().to_ascii_lowercase().ends_with(".json") {
                names.insert(name);
                if names.len() > MAX_THEME_FILES {
                    names.pop_last();
                }
            }
        }
    }
    let themes = names
        .into_iter()
        .filter_map(|name| {
            let meta = std::fs::symlink_metadata(dir.join(THEMES_DIR).join(&name)).ok()?;
            let (modified, len) = stamp(&meta);
            Some((name, modified, len))
        })
        .collect();
    Stamps { settings, themes }
}

/// Everything the core tells the webview.
#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    Frame(FrameMsg),
    Layout(View),
    Title { pane: PaneId, title: String },
    Cwd { pane: PaneId, cwd: String },
    PaneExit { pane: PaneId, code: Option<u32> },
    /// The settings, the theme or the key bindings changed (a command, or a hand edit that was picked up).
    Settings(SettingsView),
    /// A pane or a tab that was released over this window comes from another window: where in this page it goes is
    /// for this page to say. `x` and `y` are the pointer in client coordinates (CSS pixels). The page answers with
    /// `attach_pane` or `attach_tab`, or with nothing when the pointer is over no drop zone. `pane` is the pane that
    /// was released (for a tab, one of its panes): the answer has to name it, and this page has never heard of it.
    IncomingDrop { kind: DropKind, pane: PaneId, x: f64, y: f64 },
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
            Out::Settings(_) => "settings",
            Out::IncomingDrop { .. } => "incoming-drop",
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

#[derive(Serialize)]
struct IncomingDropPayload {
    kind: DropKind,
    pane: PaneId,
    x: f64,
    y: f64,
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
            Out::Settings(view) => view.serialize(serializer),
            Out::IncomingDrop { kind, pane, x, y } => {
                IncomingDropPayload { kind: *kind, pane: *pane, x: *x, y: *y }.serialize(serializer)
            }
            Out::Exit => serializer.serialize_unit(),
        }
    }
}

/// Which webviews an event is for. A page that receives an event that is not for it draws another window's panes, so
/// the core names the target of every event and the sink delivers it to that window only (research section 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Every window: the settings, the end of the app.
    All,
    /// The window with this label.
    Window(Arc<str>),
}

impl Target {
    pub fn window(label: &str) -> Target {
        Target::Window(label.into())
    }
}

/// Where the core's events go, and the few things the core asks of the window system. `send` may be called from any
/// thread, including a pane's engine thread, and must return quickly. The others are never called with a lock of the
/// core held: they can wait for the main thread, whose window events take those locks.
pub trait Sink: Send + Sync + 'static {
    fn send(&self, target: Target, out: Out);

    /// The window `label` is no longer in the set (its last pane went): destroy it. Does not ask again.
    fn close_window(&self, label: &str);

    /// Create the window `label`, hidden: its page asks for its layout, which exists already, and the window is shown
    /// when the page calls `window_ready`. An error means there is no window, and the caller puts the pane back.
    fn open_window(&self, label: &str, place: &Place) -> Result<(), String>;

    /// What is under the pointer now, other than the window `source`.
    fn cursor_spot(&self, source: &str) -> CursorSpot;
}

/// What a tear off is about: one pane, or the tab that holds the pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Subject {
    Pane(PaneId),
    Tab(PaneId),
}

/// Why a tear off did not happen.
#[derive(Debug, PartialEq, Eq)]
enum TearOffError {
    /// Not allowed (an unknown pane, a pane that is alone in its window, too many windows, the app is ending);
    /// nothing changed.
    Refused(String),
    /// The window could not be built; the pane was put back.
    Failed(String),
}

impl From<TearOffError> for String {
    fn from(error: TearOffError) -> String {
        match error {
            TearOffError::Refused(reason) | TearOffError::Failed(reason) => reason,
        }
    }
}

/// What a pane or tab that was released outside its window came to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Released {
    /// It has a window of its own now.
    TornOff { label: String },
    /// It was released over this other window, which has been asked where in it the pane goes.
    Handed { to: String },
    /// Nothing happened (the pointer is unknown, or a window of its own is not allowed).
    Cancelled,
}

/// The program a pane runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellSpec {
    /// The shell of the platform and the `shell` setting, asked of [`shell_launch`] each time a pane starts, so a
    /// change of the setting reaches the panes started afterwards.
    System,
    /// One program with its arguments, whatever the platform and the setting say. What the tests inject.
    #[cfg_attr(not(test), allow(dead_code))]
    Fixed { program: String, args: Vec<String> },
}

impl ShellSpec {
    #[cfg(test)]
    pub fn new(program: impl Into<String>, args: &[&str]) -> Self {
        Self::Fixed { program: program.into(), args: args.iter().map(|a| (*a).to_owned()).collect() }
    }

    /// The program and arguments of a pane's shell, and the reason when the `shell` setting could not be used and the
    /// platform default stands in. `configured` is the `shell` setting; `os`, `env`, `exists` and `check` are the
    /// facts [`resolve_shell`] decides from, passed in so that every platform is tested on any host.
    fn launch(
        &self,
        configured: &str,
        os: Os,
        env: &dyn Fn(&str) -> Option<String>,
        exists: &dyn Fn(&str) -> bool,
        check: &dyn Fn(&str) -> ProgramCheck,
    ) -> ShellResolved {
        match self {
            // Inside a Flatpak the shell is the host's: its program is not in the sandbox, so the setting is not looked
            // up here and nothing is wrong with a program the sandbox cannot see (`PaneSpec::host_spawn`).
            Self::System if os == Os::Linux && in_flatpak(env, exists) => {
                ShellResolved { launch: host_shell_launch(configured), unusable: None }
            }
            Self::System => resolve_shell(os, configured, env, exists, check),
            Self::Fixed { program, args } => {
                ShellResolved { launch: ShellLaunch { program: program.clone(), args: args.clone() }, unusable: None }
            }
        }
    }
}

/// What the file system says about a candidate shell program.
fn check_program_on_disk(path: &str) -> ProgramCheck {
    let Ok(meta) = std::fs::metadata(path) else { return ProgramCheck::Missing };
    if !meta.is_file() {
        return ProgramCheck::NotAFile;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return ProgramCheck::NotExecutable;
        }
    }
    ProgramCheck::Usable
}

/// The shell of the platform and the `shell` setting as this machine resolves it: the one place that reads the process
/// environment and the file system for the shell's sake.
fn resolve_system_shell(spec: &ShellSpec, configured: &str) -> ShellResolved {
    spec.launch(
        configured,
        Os::current(),
        &|key| std::env::var(key).ok(),
        &|path| Path::new(path).is_file(),
        &check_program_on_disk,
    )
}

impl Default for ShellSpec {
    /// The system shell: `$SHELL -l` on macOS and Linux, PowerShell or cmd on Windows, or the `shell` setting.
    fn default() -> Self {
        Self::System
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What `AppCore::reserve` found.
#[derive(Debug, PartialEq, Eq)]
enum Reserved {
    /// The pane has a `Slot::Starting` now and the caller starts its shell.
    Now,
    /// The pane already had a slot; nothing was changed.
    Already,
}

/// The size the page asked for while a shell was still starting, already clamped like the running path clamps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WantedSize {
    cols: u16,
    rows: u16,
    cell_px: (u32, u32),
}

/// What the map of panes holds for a pane id.
enum Slot {
    /// A shell is being started; nothing is known of it yet, except the latest size the page asked for since. That
    /// size lives and dies with the slot: a pane that is closed while it starts takes it along, and an older request
    /// is replaced, so a flood of resizes costs no more than one value.
    Starting(Option<WantedSize>),
    Running(PaneHandle),
}

/// Which window each pane is in, for the events that go to one window: a frame, a title, a working directory, the
/// end of a shell. Kept apart from the layouts, with a lock of its own, so that the engine thread of a pane that
/// makes a frame never waits for the layout lock or scans the layouts.
#[derive(Default)]
struct Routes {
    /// The panes that are in a layout, with the window that has them. Rebuilt from the set under the set's lock after
    /// every change that adds, removes or moves a pane.
    live: HashMap<PaneId, Arc<str>>,
    /// The panes that were closed and whose shell has not reported its end yet, with the window they were in, so that
    /// the end of the shell is told to the window that closed the pane.
    closing: HashMap<PaneId, Arc<str>>,
}

impl Routes {
    fn rebuild(&mut self, set: &WindowSet) {
        let mut live = HashMap::with_capacity(self.live.len() + 1);
        for (label, layout) in set.windows() {
            // One allocation per window, shared by its panes.
            let label: Arc<str> = label.into();
            for pane in layout.all_panes() {
                live.insert(pane, label.clone());
            }
        }
        self.live = live;
    }

    fn target_of(&self, pane: PaneId) -> Option<Target> {
        self.live.get(&pane).map(|label| Target::Window(label.clone()))
    }
}

/// The state of the app: the layouts of every window, the running panes, and what is needed to start a pane.
///
/// Lock order: `windows` first, then `panes` or `routes`; `panes` and `routes` are never held while waiting for
/// `windows`, and never together with each other. `flush` runs the save job, which locks `windows`, so it is never
/// called with `windows` held. The sink is never called with `windows` held, except `send`, which only queues.
pub struct AppCore {
    /// The app data directory: `layout.json`, `settings.json`, `session.json` and `themes/` live in it.
    dir: PathBuf,
    /// The layout of every window.
    windows: Arc<Mutex<WindowSet>>,
    /// Which window each pane is in (see [`Routes`]).
    routes: Arc<Mutex<Routes>>,
    panes: Arc<Mutex<HashMap<PaneId, Slot>>>,
    /// The settings, themes and key bindings in force. Locked briefly and never across file IO or a wait.
    config: Mutex<Config>,
    /// Serialises the commands that write `settings.json` or reload it, so the file, the memory and the events
    /// of two of them never interleave. Held across the file IO; `config` is not.
    settings_io: Mutex<()>,
    /// The last working directory each pane reported (its `Cwd` events).
    last_cwd: Arc<Mutex<HashMap<PaneId, String>>>,
    /// The last title each pane reported. The core sends a title when a program changes it and never again, so a
    /// pane that arrives in another window would show none: both this and the directory are replayed to it.
    last_title: Mutex<HashMap<PaneId, String>>,
    /// The drop of a pane or tab that is on its way from one window to another.
    drops: Mutex<Drops>,
    /// The directory a pane that has not started yet is to inherit, recorded when its split or tab was made.
    starts: Mutex<HashMap<PaneId, String>>,
    /// What is restored on the next start: working directory per pane and the window geometry.
    session: Arc<Mutex<Session>>,
    /// Writes `session.json` two seconds after the first change of a burst, and when flushed.
    session_saver: Debounced,
    /// Starts the opener of the platform for `open_url` and `reveal_path`; tests put a recorder here.
    launcher: Mutex<fn(&Launch) -> Result<(), String>>,
    sink: Arc<dyn Sink>,
    shell: ShellSpec,
    /// Writes the layout 250 ms after the first change of a burst and when flushed.
    saver: Debounced,
    /// How many times the saver job wrote the file, for tests.
    writes: Arc<AtomicUsize>,
    /// The app is ending (the last pane closed or ended, or `shutdown` ran): panes that end from now on stay in the
    /// layout, and windows that close from now on stay in it too, so that what is saved is what the user had.
    exiting: AtomicBool,
    /// The `rev` of the last view built. Counted under the windows lock, so the order of the revs is the order of the
    /// states of the layouts.
    view_rev: AtomicU64,
    me: Weak<AppCore>,
}

impl AppCore {
    /// Load the layouts from `dir/layout.json` (a fresh one-pane window `main` when there is none or it is unusable;
    /// a layout file of before windows is one window) and start nothing: the webview starts each pane once it knows
    /// the pane's size, and the shell opens the windows other than `main` (see [`AppCore::restored_windows`]).
    pub fn new(dir: PathBuf, sink: Arc<dyn Sink>, shell: ShellSpec) -> Arc<AppCore> {
        let path = dir.join("layout.json");
        let windows = Arc::new(Mutex::new(load_layout(&path)));
        let writes = Arc::new(AtomicUsize::new(0));

        let job_windows = windows.clone();
        let job_writes = writes.clone();
        let saver = Debounced::new(SAVE_DELAY, move || {
            // Serialise a copy: the layouts are not locked while the file is written and synced.
            let snapshot = lock(&job_windows).clone();
            match snapshot.to_json() {
                Ok(json) => {
                    job_writes.fetch_add(1, Ordering::SeqCst);
                    if let Err(e) = persist::write_atomic(&path, json.as_bytes()) {
                        eprintln!("holzwave: cannot save {}: {e}", path.display());
                    }
                }
                Err(e) => eprintln!("holzwave: cannot serialise the layout: {e}"),
            }
        });

        let config = Mutex::new(Config::load(&dir));

        let panes = Arc::new(Mutex::new(HashMap::new()));
        let last_cwd = Arc::new(Mutex::new(HashMap::new()));
        let routes = {
            let mut routes = Routes::default();
            routes.rebuild(&lock(&windows));
            Arc::new(Mutex::new(routes))
        };
        let session = {
            let mut session = load_session(&dir.join(SESSION_FILE));
            // A directory of a pane that is not in the layout is of no use and must not be written back, and neither
            // is the frame of a window that is not in it.
            let set = lock(&windows);
            session.sanitise(&set.all_panes());
            session.sanitise_windows(&set.labels());
            drop(set);
            Arc::new(Mutex::new(session))
        };
        let session_saver = {
            let path = dir.join(SESSION_FILE);
            let (windows, panes, session, last_cwd) = (windows.clone(), panes.clone(), session.clone(), last_cwd.clone());
            Debounced::new(SESSION_DELAY, move || write_session(&path, &windows, &panes, &session, &last_cwd))
        };

        Arc::new_cyclic(|me| AppCore {
            dir,
            windows,
            routes,
            panes,
            config,
            settings_io: Mutex::new(()),
            last_cwd,
            last_title: Mutex::new(HashMap::new()),
            drops: Mutex::new(Drops::default()),
            starts: Mutex::new(HashMap::new()),
            session,
            session_saver,
            launcher: Mutex::new(spawn_launch),
            sink,
            shell,
            saver,
            writes,
            exiting: AtomicBool::new(false),
            view_rev: AtomicU64::new(0),
            me: me.clone(),
        })
    }

    /// The view of the window `label`. An unknown label is the error `no such window`.
    pub fn view_in(&self, label: &str) -> Result<View, String> {
        let set = lock(&self.windows);
        self.view_in_set(&set, label)
    }

    /// The view of the layout, stamped with the next `rev`. The caller holds the windows lock (it has the `&Layout`
    /// from the guard), so two views are stamped in the order their states happened.
    fn view_of(&self, layout: &Layout) -> View {
        View::of(layout, self.view_rev.fetch_add(1, Ordering::SeqCst) + 1)
    }

    /// [`AppCore::view_of`] for the window `label` of a set the caller has locked.
    fn view_in_set(&self, set: &WindowSet, label: &str) -> Result<View, String> {
        set.layout(label).map(|layout| self.view_of(layout)).ok_or_else(no_such_window)
    }

    /// Make the table of panes and windows match `set`. Call it under the windows lock, after every change that adds,
    /// removes or moves a pane, so that the table and the layouts are never seen apart.
    fn sync_routes(&self, set: &WindowSet) {
        lock(&self.routes).rebuild(set);
    }

    /// The labels of the windows the saved layout has other than `main`, in order: the shell opens them after the
    /// start (the configuration creates `main` itself).
    pub fn restored_windows(&self) -> Vec<String> {
        lock(&self.windows).labels().into_iter().filter(|label| *label != MAIN_WINDOW).map(str::to_owned).collect()
    }

    /// The labels of all windows.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn window_labels(&self) -> Vec<String> {
        lock(&self.windows).labels().into_iter().map(str::to_owned).collect()
    }

    // ---- layout commands: lock, mutate, mark dirty, answer with the new view. No disk IO, no waiting. ------------
    //
    // Each takes the label of the window that called, and answers with that window's view. A label that is no window
    // is the error `no such window`; nothing else can go wrong.

    /// Apply a layout change to one window, schedule a save and return the new view.
    fn change_in(&self, label: &str, f: impl FnOnce(&mut Layout)) -> Result<View, String> {
        let view = {
            let mut set = lock(&self.windows);
            set.edit(label, f).ok_or_else(no_such_window)?;
            self.view_in_set(&set, label)?
        };
        self.saver.touch();
        Ok(view)
    }

    /// Like [`AppCore::change_in`] for a change that makes a pane out of the focused one: the new pane is to start in
    /// the directory the focused pane was last known to be in, which is recorded now because the focus and the
    /// directory may both have moved by the time the webview starts the new pane. `add` makes the pane in the set
    /// (so that the pane limit counts over all windows) and answers its id, `None` at a limit.
    fn grow_in(&self, label: &str, add: impl FnOnce(&mut WindowSet) -> Option<PaneId>) -> Result<View, String> {
        let (view, creator, created) = {
            let mut set = lock(&self.windows);
            let creator = set.layout(label).ok_or_else(no_such_window)?.tab().focus;
            let created = add(&mut set);
            if created.is_some() {
                self.sync_routes(&set);
            }
            (self.view_in_set(&set, label)?, creator, created)
        };
        if let Some(pane) = created {
            let known = lock(&self.last_cwd).get(&creator).cloned();
            let cwd = known.or_else(|| lock(&self.session).cwds.get(&creator).cloned());
            if let Some(cwd) = cwd {
                lock(&self.starts).insert(pane, cwd);
            }
        }
        self.saver.touch();
        Ok(view)
    }

    /// Split the focused pane; the new pane gets the focus and starts where the focused pane is. At the pane limits
    /// the view comes back unchanged.
    pub fn split_in(&self, label: &str, column: bool) -> Result<View, String> {
        let axis = if column { Axis::Column } else { Axis::Row };
        self.grow_in(label, |set| set.split_focused(label, axis))
    }

    /// A new tab with one pane that starts where the focused pane is.
    pub fn new_tab_in(&self, label: &str) -> Result<View, String> {
        self.grow_in(label, |set| set.new_tab(label))
    }

    pub fn new_workspace_in(&self, label: &str, name: &str) -> Result<View, String> {
        let view = {
            let mut set = lock(&self.windows);
            if set.new_workspace(label, name).is_some() {
                self.sync_routes(&set);
            }
            self.view_in_set(&set, label)?
        };
        self.saver.touch();
        Ok(view)
    }

    pub fn focus_pane_in(&self, label: &str, pane: PaneId) -> Result<View, String> {
        self.change_in(label, |l| l.focus_pane(pane))
    }

    pub fn cycle_focus_in(&self, label: &str, step: i32) -> Result<View, String> {
        self.change_in(label, |l| l.cycle_focus(step as isize))
    }

    pub fn select_tab_in(&self, label: &str, index: usize) -> Result<View, String> {
        self.change_in(label, |l| l.select_tab(index))
    }

    pub fn select_workspace_in(&self, label: &str, index: usize) -> Result<View, String> {
        self.change_in(label, |l| l.select_workspace(index))
    }

    /// Name the pane `pane`; the empty text gives back the program title. An unknown pane changes nothing.
    pub fn rename_pane_in(&self, label: &str, pane: PaneId, name: &str) -> Result<View, String> {
        self.change_in(label, |l| {
            l.rename_pane(pane, name);
        })
    }

    /// Name the tab at `index` of the shown workspace; the empty text gives back `shell`.
    pub fn rename_tab_in(&self, label: &str, index: usize, name: &str) -> Result<View, String> {
        self.change_in(label, |l| {
            l.rename_tab(index, name);
        })
    }

    /// Name the workspace at `index`; the empty text gives back `ws N`.
    pub fn rename_workspace_in(&self, label: &str, index: usize, name: &str) -> Result<View, String> {
        self.change_in(label, |l| {
            l.rename_workspace(index, name);
        })
    }

    /// Drop `pane` on `target`, both in the window `label`: a side splits the target there, the middle swaps the two
    /// (DESK-20). Any id the layout does not know, and any move over a limit, changes nothing. (A pane that comes
    /// from another window is [`AppCore::attach_pane_in`].)
    pub fn move_pane_in(&self, label: &str, pane: PaneId, target: PaneId, edge: Edge) -> Result<View, String> {
        self.change_in(label, |l| {
            l.move_pane(pane, target, edge);
        })
    }

    /// Move the tab at `from` to the place `to` in the shown workspace.
    pub fn move_tab_in(&self, label: &str, from: usize, to: usize) -> Result<View, String> {
        self.change_in(label, |l| {
            l.move_tab(from, to);
        })
    }

    /// Move the tab at `tab` of the shown workspace to the end of workspace `workspace`.
    pub fn move_tab_to_workspace_in(&self, label: &str, tab: usize, workspace: usize) -> Result<View, String> {
        self.change_in(label, |l| {
            l.move_tab_to_workspace(tab, workspace);
        })
    }

    pub fn set_ratio_in(&self, label: &str, split: SplitId, ratio: f32) -> Result<View, String> {
        self.change_in(label, |l| l.set_ratio(split, ratio))
    }

    /// Close a pane of the window `label` (RESEARCH Pitfall 4). The shell is killed on its own engine thread and the
    /// app keeps running. What else happens depends on what the pane was:
    ///
    /// - one of several panes of its window: the window's layout changes, and that window is told;
    /// - the last pane of a window while there are other windows: the window closes with it (its layout and frame
    ///   go, the sink destroys it), and the answer is the view the window had;
    /// - the last pane of the last window: the app ends, after the layouts are on disk, and nothing changes;
    /// - an id that is in no tab of this window (another window's pane too): nothing changes.
    ///
    /// Not waiting for anything, but the exit path writes the layout, so the command is async.
    pub fn close_pane_in(&self, label: &str, pane: PaneId) -> Result<View, String> {
        let mut set = lock(&self.windows);
        let layout = set.layout(label).ok_or_else(no_such_window)?;
        if set.window_of(pane) != Some(label) {
            return self.view_in_set(&set, label);
        }
        // The view a window gives as its last answer when it is the pane's last and the window goes with it.
        let before = (set.len() > 1 && layout.all_panes() == [pane]).then(|| layout.clone());
        match set.close_pane(pane) {
            SetClose::Closed => {
                let view = self.view_in_set(&set, label)?;
                // Under the windows lock, so `pane_gone` of the same pane sees either both or neither of the changes.
                let slot = lock(&self.panes).remove(&pane);
                {
                    let mut routes = lock(&self.routes);
                    routes.rebuild(&set);
                    routes.closing.insert(pane, label.into());
                }
                drop(set);
                self.forget_pane(pane);
                let never_started = slot.is_none();
                if let Some(Slot::Running(handle)) = &slot {
                    // The actor kills and reaps the shell on its own thread (about 200 ms) and sends the PaneExit.
                    handle.close();
                }
                drop(slot);
                self.saver.touch();
                self.sink.send(Target::window(label), Out::Layout(view.clone()));
                if never_started {
                    // No shell, so no Exited event will come: the page still hears that the pane is gone.
                    lock(&self.routes).closing.remove(&pane);
                    self.sink.send(Target::window(label), Out::PaneExit { pane, code: None });
                }
                Ok(view)
            }
            SetClose::WindowClosed(gone) => {
                let view = before.as_ref().map(|layout| self.view_of(layout)).ok_or_else(no_such_window)?;
                let slot = lock(&self.panes).remove(&pane);
                self.sync_routes(&set);
                drop(set);
                self.forget_pane(pane);
                if let Some(Slot::Running(handle)) = &slot {
                    handle.close();
                }
                drop(slot);
                self.window_removed(&gone);
                Ok(view)
            }
            SetClose::LastPane => {
                let view = self.view_in_set(&set, label)?;
                drop(set);
                self.begin_exit();
                Ok(view)
            }
            SetClose::Unknown => self.view_in_set(&set, label),
        }
    }

    /// Close every pane of the tab at `index` of the shown workspace of the window `label` (the × of a tab). The panes go
    /// as [`AppCore::close_pane_in`] lets them go, all or nothing, and the window is told once. A tab that is all of
    /// its window closes the window when there are other windows, and ends the app when there are none (the layout on
    /// disk stays what it was). An index that is no tab changes nothing.
    pub fn close_tab_in(&self, label: &str, index: usize) -> Result<View, String> {
        self.close_group_in(label, |set| set.close_tab(label, index))
    }

    /// Like [`AppCore::close_tab_in`] for the workspace at `index` (the × of a workspace chip).
    pub fn close_workspace_in(&self, label: &str, index: usize) -> Result<View, String> {
        self.close_group_in(label, |set| set.close_workspace(label, index))
    }

    fn close_group_in(&self, label: &str, close: impl FnOnce(&mut WindowSet) -> SetCloseGroup) -> Result<View, String> {
        let mut set = lock(&self.windows);
        let layout = set.layout(label).ok_or_else(no_such_window)?;
        // The view a window gives as its last answer when the group is all of it and the window goes with it.
        let before = (set.len() > 1).then(|| layout.clone());
        match close(&mut set) {
            SetCloseGroup::Closed(panes) => {
                let view = self.view_in_set(&set, label)?;
                // Under the windows lock, like `close_pane_in`, so `pane_gone` sees either all or none of the changes.
                let slots = self.take_slots(&panes);
                {
                    let mut routes = lock(&self.routes);
                    routes.rebuild(&set);
                    for pane in &panes {
                        routes.closing.insert(*pane, label.into());
                    }
                }
                drop(set);
                let never_started = self.end_panes(slots);
                self.saver.touch();
                self.sink.send(Target::window(label), Out::Layout(view.clone()));
                for pane in never_started {
                    // No shell, so no Exited event will come: the page still hears that the pane is gone.
                    lock(&self.routes).closing.remove(&pane);
                    self.sink.send(Target::window(label), Out::PaneExit { pane, code: None });
                }
                Ok(view)
            }
            SetCloseGroup::WindowClosed(gone, panes) => {
                let view = before.as_ref().map(|layout| self.view_of(layout)).ok_or_else(no_such_window)?;
                let slots = self.take_slots(&panes);
                self.sync_routes(&set);
                drop(set);
                self.end_panes(slots);
                self.window_removed(&gone);
                Ok(view)
            }
            SetCloseGroup::LastPane => {
                let view = self.view_in_set(&set, label)?;
                drop(set);
                self.begin_exit();
                Ok(view)
            }
            SetCloseGroup::Unknown => self.view_in_set(&set, label),
        }
    }

    /// Take the handles of `panes` out of the table (a pane that never started has none).
    fn take_slots(&self, panes: &[PaneId]) -> Vec<(PaneId, Option<Slot>)> {
        let mut map = lock(&self.panes);
        panes.iter().map(|pane| (*pane, map.remove(pane))).collect()
    }

    /// Forget the frames of panes that left the layouts and end their shells, each on its own engine thread. Call it
    /// with no lock held. Answers the panes that had no shell yet.
    fn end_panes(&self, slots: Vec<(PaneId, Option<Slot>)>) -> Vec<PaneId> {
        let mut never_started = Vec::new();
        for (pane, slot) in slots {
            self.forget_pane(pane);
            match &slot {
                Some(Slot::Running(handle)) => handle.close(),
                Some(_) => {}
                None => never_started.push(pane),
            }
        }
        never_started
    }

    /// A window left the set (its last pane closed, or it was closed with the red button): its frame is forgotten,
    /// the layout is saved without it and, when the core did it, the sink destroys the window. Call it with no lock
    /// held.
    fn window_removed(&self, label: &str) {
        lock(&self.session).forget_window(label);
        self.session_saver.touch();
        self.saver.touch();
        self.sink.close_window(label);
    }

    /// The window `label` is gone (the red button, or the destroy that `close_window` of the sink asked for): its
    /// shells end and its layout and frame are dropped.
    ///
    /// Two cases change nothing. The last window: closing it ends the app, and the layout on disk stays what it was,
    /// so that the next start has the window back. And an app that is ending already: quitting must never drop the
    /// windows from the layout (the shells end, the windows are destroyed, none of that is the user closing them). A
    /// window that is not in the set any more (the core removed it itself) is not asked about again.
    pub fn window_closed(&self, label: &str) {
        if self.exiting.load(Ordering::SeqCst) {
            return;
        }
        let slots = {
            let mut set = lock(&self.windows);
            let Some(panes) = set.close_window(label) else {
                return;
            };
            self.sync_routes(&set);
            let mut panes_map = lock(&self.panes);
            panes.into_iter().filter_map(|pane| panes_map.remove(&pane).map(|slot| (pane, slot))).collect::<Vec<_>>()
        };
        // The panes are not in any layout now; their shells end on their own threads.
        let gone: Vec<PaneId> = {
            let mut gone = Vec::new();
            for (pane, slot) in slots {
                gone.push(pane);
                if let Slot::Running(handle) = &slot {
                    handle.close();
                }
            }
            gone
        };
        for pane in gone {
            self.forget_pane(pane);
        }
        lock(&self.session).forget_window(label);
        self.session_saver.touch();
        self.saver.touch();
    }

    // ---- panes and tabs between windows (DESK-29 part 2) -------------------------------------------------------

    /// The window `label` shows its first page: it asks for the last title and working directory of every pane it
    /// holds, which the programs said once and the page has never heard (a window that is new, or restored). The
    /// shell shows the window after this.
    pub fn window_ready_in(&self, label: &str) -> Result<(), String> {
        let panes = lock(&self.windows).layout(label).ok_or_else(no_such_window)?.all_panes();
        self.replay(label, &panes);
        Ok(())
    }

    /// Give a pane of the window `label` a window of its own. `at_cursor` opens it under the pointer, otherwise
    /// beside the window it came from (the palette action). The answer is the view of the window the pane left.
    ///
    /// The set is changed first (the new window's page asks for its layout as its first call, so the entry has to
    /// exist before the window does), then the lock is released, then the sink builds the window. If that fails the
    /// pane is put back in the window it came from and the answer is an error. Refused (an error, nothing changes)
    /// for a pane that is alone in its window: it would be the same window under another name.
    pub fn tear_off_pane_in(&self, label: &str, pane: PaneId, at_cursor: bool) -> Result<View, String> {
        let place = if at_cursor { Place::Cursor } else { Place::Beside(label.to_owned()) };
        self.tear_off(label, Subject::Pane(pane), place).map(|(view, _)| view).map_err(String::from)
    }

    /// Like [`AppCore::tear_off_pane_in`] for the whole tab the pane `pane` is in (the shown workspace of the
    /// window): its split tree, title and pane names go with it.
    pub fn tear_off_tab_in(&self, label: &str, pane: PaneId, at_cursor: bool) -> Result<View, String> {
        let place = if at_cursor { Place::Cursor } else { Place::Beside(label.to_owned()) };
        self.tear_off(label, Subject::Tab(pane), place).map(|(view, _)| view).map_err(String::from)
    }

    fn tear_off(&self, label: &str, subject: Subject, place: Place) -> Result<(View, String), TearOffError> {
        if self.exiting.load(Ordering::SeqCst) {
            return Err(TearOffError::Refused("the app is closing".into()));
        }
        let (new_label, view) = {
            let mut set = lock(&self.windows);
            let layout = set.layout(label).ok_or_else(|| TearOffError::Refused(no_such_window()))?;
            let torn = match subject {
                Subject::Pane(pane) => {
                    if set.window_of(pane) != Some(label) {
                        return Err(TearOffError::Refused("no such pane".into()));
                    }
                    set.tear_off_pane(pane)
                }
                Subject::Tab(pane) => match set.locate(pane) {
                    Some((owner, workspace, tab)) if owner == label && workspace == layout.active => {
                        set.tear_off_tab(label, tab)
                    }
                    _ => return Err(TearOffError::Refused("no such tab".into())),
                },
            }
            .ok_or_else(|| TearOffError::Refused("this cannot be a window of its own".into()))?;
            self.sync_routes(&set);
            let view = self.view_in_set(&set, label).map_err(TearOffError::Refused)?;
            (torn.label, view)
        };
        self.saver.touch();

        // No lock is held: the window is built on the main thread, whose window events take the locks of the core.
        if let Err(reason) = self.sink.open_window(&new_label, &place) {
            self.undo_tear_off(&new_label, label, subject);
            return Err(TearOffError::Failed(format!("cannot open a window: {reason}")));
        }
        self.sink.send(Target::window(label), Out::Layout(view.clone()));
        Ok((view, new_label))
    }

    /// The window `gone` could not be built: its pane or tab goes back into `home`, as a tab of the workspace that is
    /// shown there (where in the window it sat is not remembered), or into another window when `home` is gone too.
    /// The windows are told. If nothing takes it the entry stays and the next start opens it.
    fn undo_tear_off(&self, gone: &str, home: &str, subject: Subject) {
        let mut told: Vec<(String, View)> = Vec::new();
        {
            let mut set = lock(&self.windows);
            let mut targets: Vec<String> = vec![home.to_owned()];
            targets.extend(set.labels().into_iter().filter(|l| *l != home && *l != gone).map(str::to_owned));
            let mut moved = None;
            for target in targets {
                let Some(workspace) = set.layout(&target).map(|l| l.active) else { continue };
                moved = match subject {
                    Subject::Pane(pane) => set.move_pane_to(pane, &target, AttachTo::Tab { workspace }),
                    // The torn off tab is the one tab of the one workspace of the window that was not built.
                    Subject::Tab(_) => set.move_tab_to(gone, 0, &target, workspace),
                };
                if moved.is_some() {
                    break;
                }
            }
            self.sync_routes(&set);
            match moved {
                Some(moved) => {
                    if let Ok(view) = self.view_in_set(&set, &moved.to) {
                        told.push((moved.to, view));
                    }
                }
                None => eprintln!("holzwave: could not put {subject:?} back after the window {gone} failed to open"),
            }
        }
        self.saver.touch();
        for (label, view) in told {
            self.sink.send(Target::Window(label.as_str().into()), Out::Layout(view));
        }
    }

    /// The page of `label` released a pane (or a tab, named by one of its panes) outside its window; the core reads
    /// the pointer, the page cannot say where it is. Over another window of the app that window is told with
    /// `incoming-drop` and decides where in it the pane goes (`attach_pane` / `attach_tab`); the drop waits for that
    /// answer for a few seconds. Over nothing the pane or tab becomes a window under the pointer. Where the pointer
    /// cannot be read, or a window of its own is refused, nothing happens.
    pub fn release_outside_in(&self, label: &str, pane: PaneId, kind: DropKind) -> Result<Released, String> {
        if self.exiting.load(Ordering::SeqCst) {
            return Err("the app is closing".into());
        }
        {
            let set = lock(&self.windows);
            let layout = set.layout(label).ok_or_else(no_such_window)?;
            let owned = match kind {
                DropKind::Pane => set.window_of(pane) == Some(label),
                DropKind::Tab => set.locate(pane).is_some_and(|(owner, ws, _)| owner == label && ws == layout.active),
            };
            if !owned {
                return Err("no such pane".into());
            }
        }
        match self.sink.cursor_spot(label) {
            CursorSpot::Window { label: to, x, y } => {
                if to == label || lock(&self.windows).layout(&to).is_none() {
                    return Ok(Released::Cancelled);
                }
                lock(&self.drops).begin(kind, pane, label, Instant::now());
                self.sink.send(Target::window(&to), Out::IncomingDrop { kind, pane, x, y });
                Ok(Released::Handed { to })
            }
            CursorSpot::Nothing => {
                let subject = match kind {
                    DropKind::Pane => Subject::Pane(pane),
                    DropKind::Tab => Subject::Tab(pane),
                };
                match self.tear_off(label, subject, Place::Cursor) {
                    Ok((_, new_label)) => Ok(Released::TornOff { label: new_label }),
                    Err(TearOffError::Refused(_)) => Ok(Released::Cancelled),
                    Err(TearOffError::Failed(reason)) => Err(reason),
                }
            }
            CursorSpot::Unknown => Ok(Released::Cancelled),
        }
    }

    /// The page of `label` answers an `incoming-drop` of a pane: onto the pane `target` at `edge` (a side splits it,
    /// the middle swaps the two), or as a tab of workspace `workspace`. Only the drop that is in flight, for this
    /// very pane, counts; a stale, repeated or wrong answer changes nothing and answers the view as it is. The pane
    /// keeps its id, so its shell, name and scrollback come along, and the page is told its last title and
    /// directory. A window left without a pane closes.
    pub fn attach_pane_in(
        &self,
        label: &str,
        pane: PaneId,
        target: Option<PaneId>,
        edge: Option<Edge>,
        workspace: Option<usize>,
    ) -> Result<View, String> {
        let at = match (target, edge, workspace) {
            (Some(target), Some(edge), _) => AttachTo::Pane { target, edge },
            (_, _, Some(workspace)) => AttachTo::Tab { workspace },
            _ => return self.view_in(label),
        };
        self.attach(label, DropKind::Pane, pane, |set, from| {
            // The pane has to be where the drop came from: it may have moved or closed since.
            (set.window_of(pane) == Some(from)).then(|| set.move_pane_to(pane, label, at)).flatten().map(|m| (m, vec![pane]))
        })
    }

    /// The page of `label` answers an `incoming-drop` of a tab (named by one of its panes): as the last tab of
    /// workspace `workspace`. The same rules as [`AppCore::attach_pane_in`].
    pub fn attach_tab_in(&self, label: &str, pane: PaneId, workspace: usize) -> Result<View, String> {
        self.attach(label, DropKind::Tab, pane, |set, from| {
            let (owner, ws, tab) = set.locate(pane)?;
            let layout = set.layout(owner)?;
            if owner != from || ws != layout.active {
                return None;
            }
            let panes = layout.workspaces.get(ws)?.tabs.get(tab)?.panes();
            let owner = owner.to_owned();
            set.move_tab_to(&owner, tab, label, workspace).map(|m| (m, panes))
        })
    }

    /// The common part of the two attach commands: check the drop, run `go` (which moves and answers what moved
    /// and which panes came along), then tell the windows.
    fn attach(
        &self,
        label: &str,
        kind: DropKind,
        pane: PaneId,
        go: impl FnOnce(&mut WindowSet, &str) -> Option<(Moved, Vec<PaneId>)>,
    ) -> Result<View, String> {
        if self.exiting.load(Ordering::SeqCst) {
            return Err("the app is closing".into());
        }
        let Some(from) = lock(&self.drops).source(kind, pane, Instant::now()) else {
            return self.view_in(label);
        };
        let (moved, came, view, source_view) = {
            let mut set = lock(&self.windows);
            if set.layout(label).is_none() {
                return Err(no_such_window());
            }
            let Some((moved, came)) = go(&mut set, &from) else {
                return self.view_in_set(&set, label);
            };
            self.sync_routes(&set);
            let view = self.view_in_set(&set, label)?;
            let source_view = if moved.source_closed { None } else { self.view_in_set(&set, &moved.from).ok() };
            (moved, came, view, source_view)
        };
        lock(&self.drops).finish(kind, pane);
        self.saver.touch();
        self.sink.send(Target::window(label), Out::Layout(view.clone()));
        if let Some(source_view) = source_view {
            self.sink.send(Target::window(&moved.from), Out::Layout(source_view));
        }
        if moved.source_closed {
            self.window_removed(&moved.from);
        }
        // The page learns the pane after the layout that has it; a full frame makes sure it has something to draw.
        self.replay(label, &came);
        for pane in came {
            let _ = self.request_full(pane);
        }
        Ok(view)
    }

    /// Write the pending layout and session now, on the calling thread. Never call it with the windows locked.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn flush(&self) {
        self.saver.flush();
        self.flush_session();
    }

    /// Write the session if a change is pending, or if a running pane never reported its directory (the process is
    /// asked for it then). Does nothing when there is nothing to record.
    fn flush_session(&self) {
        let silent = {
            let known = lock(&self.last_cwd);
            lock(&self.panes).iter().any(|(pane, slot)| matches!(slot, Slot::Running(_)) && !known.contains_key(pane))
        };
        if silent {
            self.session_saver.touch();
        }
        self.session_saver.flush();
    }

    /// How many times the layout file was written; the tests count writes with it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn layout_writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }

    // ---- session and window --------------------------------------------------------------------------------

    /// The directory a new shell of `pane` starts in: the one saved for that pane id in the session, otherwise the
    /// one it inherits from the pane it was made from. Only a directory that exists now counts; `None` starts the
    /// shell in the default directory.
    fn start_dir(&self, pane: PaneId) -> Option<PathBuf> {
        if let Some(dir) = lock(&self.session).cwd_for(pane) {
            return Some(dir);
        }
        let inherited = lock(&self.starts).get(&pane).cloned()?;
        // The same rules as a saved directory (absolute, bounded, no control characters, exists), without a second
        // copy of them: a throwaway session is asked.
        let mut probe = Session::default();
        if probe.set_cwd(pane, &inherited) { probe.cwd_for(pane) } else { None }
    }

    /// A pane reported its working directory (from its sink, on the engine thread of the pane).
    fn cwd_reported(&self, pane: PaneId, cwd: &str) {
        // A pane that is already out of the map (closed) is not remembered again by a late event, and once the app
        // is ending the session has been written: a later event would only bring a write after the exit.
        if self.exiting.load(Ordering::SeqCst) || !lock(&self.panes).contains_key(&pane) {
            return;
        }
        lock(&self.last_cwd).insert(pane, cwd.to_owned());
        self.session_saver.touch();
    }

    /// A pane left the layout: what is remembered of its directory and title goes, and the session is written
    /// without it.
    fn forget_pane(&self, pane: PaneId) {
        lock(&self.last_cwd).remove(&pane);
        lock(&self.last_title).remove(&pane);
        lock(&self.starts).remove(&pane);
        self.session_saver.touch();
    }

    /// A pane reported a new title (from its sink, on the engine thread of the pane). Remembered for the window that
    /// the pane arrives in later.
    fn title_reported(&self, pane: PaneId, title: &str) {
        if self.exiting.load(Ordering::SeqCst) || !lock(&self.panes).contains_key(&pane) {
            return;
        }
        lock(&self.last_title).insert(pane, title.to_owned());
    }

    /// Tell the window `label` the last title and working directory of each of `panes`: a window that gets a pane
    /// (a page that is new, or one that a pane moved to) has never heard them, and the program says them once.
    fn replay(&self, label: &str, panes: &[PaneId]) {
        for pane in panes {
            let title = lock(&self.last_title).get(pane).cloned();
            if let Some(title) = title {
                self.sink.send(Target::window(label), Out::Title { pane: *pane, title });
            }
            let cwd = lock(&self.last_cwd).get(pane).cloned();
            if let Some(cwd) = cwd {
                self.sink.send(Target::window(label), Out::Cwd { pane: *pane, cwd });
            }
        }
    }

    /// Remember the window geometry for the next start. Clamped into range by the session; only touches the
    /// debounced saver, so it is cheap on the thread of the window events.
    pub fn note_window(&self, label: &str, window: WindowState) {
        if lock(&self.session).set_window_for(label, window) {
            self.session_saver.touch();
        }
    }

    /// Where and how large the window `label` opens, given the monitors that are connected now: the saved size, and
    /// the saved position only if enough of the window is on one of them. A window with no saved frame gets the
    /// default size and the position the window system chooses.
    pub fn placement(&self, label: &str, monitors: &[MonitorRect]) -> Placement {
        lock(&self.session).placement_for(label, monitors)
    }

    // ---- links, paths, app info and the pointer ------------------------------------------------------------

    /// Start `launch` through the configured launcher.
    fn launch(&self, launch: &Launch) -> Result<(), String> {
        let run = *lock(&self.launcher);
        run(launch)
    }

    /// Open a link with the platform opener. Only http, https and mailto links are opened; the length, the
    /// characters and the shape are checked first, and nothing is started for a link that fails (T-02-52).
    ///
    /// Starts a process, so the command is async.
    pub fn open_url(&self, url: &str) -> Result<(), String> {
        let launch = open_launch(std::env::consts::OS, url, &process_env)?;
        self.launch(&launch)
    }

    /// Reveal a file or folder in the platform file manager. `path` is absolute, starts with `~/`, or is relative
    /// to `base` (the working directory of the pane); it has to exist. What is revealed is shown and never run
    /// (T-02-52).
    ///
    /// Touches the file system and starts a process, so the command is async.
    pub fn reveal_path(&self, path: &str, base: Option<&str>) -> Result<(), String> {
        let os = std::env::consts::OS;
        let home = home_dir(os, &process_env);
        let resolved = resolve_reveal(os, path, base, home.as_deref())?;
        let launch = reveal_launch(os, &resolved, resolved.is_dir(), &process_env)?;
        self.launch(&launch)
    }

    /// The version of the app, the operating system and the name of the machine.
    ///
    /// Runs a program on macOS, so the command is async.
    pub fn app_info(&self) -> AppInfo {
        AppInfo {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            os: std::env::consts::OS.to_owned(),
            hostname: read_hostname(),
        }
    }

    /// A mouse press, release or motion at device pixels `x`, `y` from the origin of the pane's grid. The numbers
    /// are bounded here (T-02-90) and the event only goes to the pane's queue: whether the program hears of it is
    /// the engine's decision. An unknown pane, or one that is closing, is ignored.
    pub fn mouse(&self, pane: PaneId, action: MouseAction, button: MouseButton, x: f64, y: f64, mods: u32) {
        if let Some(handle) = self.handle(pane) {
            let event = MouseEvent {
                action,
                button,
                mods: pointer_mods(mods),
                x: clamp_pointer(x),
                y: clamp_pointer(y),
            };
            let _ = handle.mouse(event);
        }
    }

    /// A wheel event of `rows` rows (negative towards older output) at `x`, `y`. Bounded like [`AppCore::mouse`]
    /// and only pushed to the pane's queue.
    pub fn wheel(&self, pane: PaneId, rows: i32, x: f64, y: f64, mods: u32) {
        if let Some(handle) = self.handle(pane) {
            let event = WheelEvent {
                rows: clamp_rows(rows),
                mods: pointer_mods(mods),
                x: clamp_pointer(x),
                y: clamp_pointer(y),
            };
            let _ = handle.wheel(event);
        }
    }

    // ---- settings, themes and key bindings ---------------------------------------------------------------------

    /// The settings, themes and key bindings in force.
    pub fn get_settings(&self) -> SettingsView {
        lock(&self.config).view()
    }

    /// Set one setting: validated by the core (type, range, theme name), written to `settings.json` atomically,
    /// then made current, pushed to every live pane and announced as `settings`. Any failure, including a write
    /// that fails, returns the reason and leaves the settings, the file and the panes as they were.
    ///
    /// Writes a file, so the command is async.
    pub fn set_setting(&self, key: &str, value: Value) -> Result<SettingsView, String> {
        let _io = lock(&self.settings_io);
        let next = {
            let cfg = lock(&self.config);
            let mut next = cfg.settings.clone();
            next.set_value(key, value)?;
            if key == "theme" && !cfg.themes.contains(&next.theme) {
                return Err(format!("there is no theme called {:?}", next.theme.chars().take(40).collect::<String>()));
            }
            if next == cfg.settings {
                return Ok(cfg.view());
            }
            next
        };
        next.save(&self.dir).map_err(|e| format!("cannot save {SETTINGS_FILE}: {e}"))?;
        let saved = stamps(&self.dir).settings;

        let (view, before, after) = {
            let mut cfg = lock(&self.config);
            let before = cfg.look();
            cfg.set_settings(next);
            cfg.stamps.settings = saved;
            (cfg.view(), before, cfg.look())
        };
        self.push_to_panes(&before, &after);
        self.sink.send(Target::All, Out::Settings(view.clone()));
        Ok(view)
    }

    /// Restore the default of one setting; everything else about [`AppCore::set_setting`] applies.
    pub fn reset_setting(&self, key: &str) -> Result<SettingsView, String> {
        let default = match serde_json::to_value(Settings::default()) {
            Ok(Value::Object(mut object)) => object.remove(key),
            _ => None,
        };
        match default {
            Some(value) => self.set_setting(key, value),
            None => Err(format!("there is no setting called {:?}", key.chars().take(40).collect::<String>())),
        }
    }

    /// Pick up hand edits of `settings.json` and `themes/*.json`. Looks at sizes and modification times only, and
    /// reads the files, applies them to the panes and announces `settings` only when one of them changed. A
    /// `settings.json` that cannot be used (a half-saved edit) leaves the settings in force and adds a warning.
    pub fn reload_settings(&self) -> SettingsView {
        let _io = lock(&self.settings_io);
        let now = stamps(&self.dir);
        if lock(&self.config).stamps == now {
            return self.get_settings();
        }

        let (loaded, status) = Settings::load(&self.dir);
        let (themes, theme_notes) = ThemeRegistry::load(&self.dir);
        let (view, before, after) = {
            let mut cfg = lock(&self.config);
            let before = cfg.look();
            let (settings, mut notes) = match status {
                LoadStatus::Reset(reason) => (
                    cfg.settings.clone(),
                    vec![format!("{SETTINGS_FILE}: {reason}; the settings in force are kept")],
                ),
                other => (loaded, status_notes(SETTINGS_FILE, other)),
            };
            notes.extend(theme_notes);
            *cfg = Config::new(settings, themes, notes, now);
            (cfg.view(), before, cfg.look())
        };
        self.push_to_panes(&before, &after);
        self.sink.send(Target::All, Out::Settings(view.clone()));
        view
    }

    /// Tell every running pane what changed between two looks.
    fn push_to_panes(&self, before: &PaneLook, after: &PaneLook) {
        if before == after {
            return;
        }
        for handle in self.running() {
            Self::push_to_pane(&handle, before, after);
        }
    }

    /// A pane that is closing refuses the message, which is fine: there is nothing left to tell.
    fn push_to_pane(handle: &PaneHandle, before: &PaneLook, after: &PaneLook) {
        if before.theme != after.theme {
            let _ = handle.set_theme(after.theme);
        }
        if before.option_as_alt != after.option_as_alt {
            let _ = handle.set_option_as_alt(after.option_as_alt);
        }
    }

    /// How a new pane is started: the scrollback, the Option key and the theme palette come from the settings; the
    /// working directory is decided by the caller.
    fn pane_spec(&self, cols: u16, rows: u16, cell: (u32, u32)) -> PaneSpec {
        self.pane_spec_on(
            cols,
            rows,
            cell,
            Os::current(),
            &|key| std::env::var(key).ok(),
            &|path| Path::new(path).is_file(),
        )
    }

    /// [`pane_spec`](Self::pane_spec) with the facts it decides from passed in. Inside a Flatpak (Linux with
    /// `/.flatpak-info` or `FLATPAK_ID`) the pane's shell runs on the host and its directory comes from OSC 7 only; the
    /// two flags are set exactly then, and everywhere else the spec is what it always was.
    fn pane_spec_on(
        &self,
        cols: u16,
        rows: u16,
        cell: (u32, u32),
        os: Os,
        env: &dyn Fn(&str) -> Option<String>,
        exists: &dyn Fn(&str) -> bool,
    ) -> PaneSpec {
        let cfg = lock(&self.config);
        // An unusable `shell` never leaves the pane without a terminal: the default starts, and the pane says so first.
        let ShellResolved { launch, unusable } =
            self.shell.launch(&cfg.settings.shell, os, env, exists, &check_program_on_disk);
        let mut spec = PaneSpec::new(launch.program, cols, rows).args(launch.args);
        if os == Os::Linux && in_flatpak(env, exists) {
            spec.host_spawn = true;
            spec.cwd_lookup = false;
        }
        spec.notice = unusable.map(|note| format!("holzWave: {note}"));
        spec.cell_px = cell;
        spec.scrollback = cfg.settings.scrollback_lines as usize;
        spec.option_as_alt = cfg.settings.option_as_alt;
        spec.theme = Some(cfg.theme().engine_theme());
        spec
    }

    // ---- ending ------------------------------------------------------------------------------------------------

    /// The handles of every running pane.
    fn running(&self) -> Vec<PaneHandle> {
        lock(&self.panes)
            .values()
            .filter_map(|slot| match slot {
                Slot::Running(handle) => Some(handle.clone()),
                Slot::Starting(_) => None,
            })
            .collect()
    }

    /// The exit path of the last pane: stop touching the layout, write it, ask every shell to end and tell the
    /// shell of the app to exit. Does not wait for the shells; `shutdown` does that on the exit events.
    fn begin_exit(&self) {
        if self.exiting.swap(true, Ordering::SeqCst) {
            return;
        }
        self.saver.flush();
        // Before the shells are asked to end, while their directories can still be read.
        self.flush_session();
        for handle in self.running() {
            handle.close();
        }
        self.sink.send(Target::All, Out::Exit);
    }

    /// End everything and wait, for at most three seconds: the layout is written, every shell is closed and joined.
    /// Safe to call more than once, and from the exit events of the app. Called by `Drop` too.
    pub fn shutdown(&self) {
        // First: panes that end from now on are not taken out of the layout.
        self.exiting.store(true, Ordering::SeqCst);
        self.saver.flush();
        self.flush_session();
        let handles = self.running();
        for handle in &handles {
            handle.close();
        }
        // On an engine thread (the last reference dropped inside an event) a join would wait for itself.
        let on_engine_thread = std::thread::current().name().is_some_and(|n| n.starts_with("hw-pane-"));
        if !on_engine_thread {
            let deadline = Instant::now() + SHUTDOWN_BOUND;
            for handle in &handles {
                if !handle.join(deadline.saturating_duration_since(Instant::now())) {
                    eprintln!("holzwave: a shell did not end within the shutdown bound");
                }
            }
        }
        self.saver.flush();
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
        if self.reserve(pane)? == Reserved::Already {
            return Ok(());
        }

        let (cols, rows) = clamp_grid(cols, rows);
        let mut spec = self.pane_spec(cols, rows, (clamp_cell(cell_w), clamp_cell(cell_h)));
        spec.cwd = self.start_dir(pane);
        lock(&self.starts).remove(&pane);
        // What the shell is started with; a settings change while it starts is pushed once it runs.
        let started_with = PaneLook {
            theme: spec.theme.unwrap_or_default(),
            option_as_alt: spec.option_as_alt,
        };

        // The sink runs on the pane's engine thread, possibly before `spawn_ghostty` has returned (a failed start
        // reports first), so it only holds the sink and a weak reference and never waits for the handle.
        //
        // Frames, titles and directories go to the window that has the pane, found in the table of routes (its own
        // lock, never the layouts). A pane that is in no window any more (closed, and its last frames still on the
        // way) has no route and its events are dropped: there is no page left that wants them.
        let sink = self.sink.clone();
        let routes = self.routes.clone();
        let me = self.me.clone();
        let started = PaneHandle::spawn_ghostty(pane, spec, move |event| match event {
            PaneEvent::Frame(frame) => {
                let target = lock(&routes).target_of(pane);
                if let Some(target) = target {
                    sink.send(target, Out::Frame(frame));
                }
            }
            PaneEvent::Title(title) => {
                // Remembered before the webview hears of it, for the window the pane may arrive in later.
                if let Some(core) = me.upgrade() {
                    core.title_reported(pane, &title);
                }
                let target = lock(&routes).target_of(pane);
                if let Some(target) = target {
                    sink.send(target, Out::Title { pane, title });
                }
            }
            PaneEvent::Cwd(cwd) => {
                // Recorded before the webview hears of it, so that a split made on seeing it inherits it.
                if let Some(core) = me.upgrade() {
                    core.cwd_reported(pane, &cwd);
                }
                let target = lock(&routes).target_of(pane);
                if let Some(target) = target {
                    sink.send(target, Out::Cwd { pane, cwd });
                }
            }
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
                self.promote(pane, handle, started_with);
                Ok(())
            }
            // The sink was told, and `pane_gone` forgot the slot, before `spawn_ghostty` returned.
            Err(e) => Err(e.to_string()),
        }
    }

    /// Check that a pane is in the layout and give it a `Slot::Starting`, under both locks so that a concurrent
    /// close cannot slip in between. A pane that already has a slot (running, or being started by another call) is
    /// left alone and reported as such.
    fn reserve(&self, pane: PaneId) -> Result<Reserved, String> {
        let set = lock(&self.windows);
        if set.window_of(pane).is_none() {
            return Err("no such pane".into());
        }
        let mut panes = lock(&self.panes);
        if panes.contains_key(&pane) {
            return Ok(Reserved::Already);
        }
        panes.insert(pane, Slot::Starting(None));
        Ok(Reserved::Now)
    }

    /// The shell of a reserved pane is up: its slot becomes `Running`. `started_with` is what the shell was started
    /// with.
    fn promote(&self, pane: PaneId, handle: PaneHandle, started_with: PaneLook) {
        self.promote_with(pane, handle, started_with, || {});
    }

    /// [`AppCore::promote`], running `between` once right after the slot became `Running` and before anything is
    /// sent to the handle. `between` runs with the `panes` lock held; it is the seam at which a test forces a command
    /// to arrive in the middle of a promotion instead of hoping to hit the window by timing.
    fn promote_with(&self, pane: PaneId, handle: PaneHandle, started_with: PaneLook, between: impl FnOnce()) {
        let mut panes = lock(&self.panes);
        match panes.get_mut(&pane) {
            Some(slot) if matches!(slot, Slot::Starting(_)) => {
                let wanted = match std::mem::replace(slot, Slot::Running(handle.clone())) {
                    Slot::Starting(wanted) => wanted,
                    Slot::Running(_) => None,
                };
                between();
                // Made under the lock that made the slot Running: a `resize` can only see Running after this
                // section, so whatever it sends comes after this replay, never before it; one that saw Starting
                // stored its size before `wanted` was taken above. The send is a push on the pane's queue and never
                // waits. A pane that is already closing refuses it, which is fine.
                if let Some(size) = wanted {
                    let _ = handle.resize(size.cols, size.rows, size.cell_px);
                }
                if self.exiting.load(Ordering::SeqCst) {
                    // The app began to end while this shell started; `shutdown` did not see it.
                    handle.close();
                }
                drop(panes);
                // A setting changed between building the spec and now: `push_to_panes` did not see this pane.
                let now = lock(&self.config).look();
                Self::push_to_pane(&handle, &started_with, &now);
            }
            // The pane was closed (or ended, or the app is closing) while the shell started.
            _ => {
                drop(panes);
                handle.close();
            }
        }
    }

    /// The shell of a pane ended, or never started (called from the pane's sink). Forgets the handle, takes the pane
    /// out of its window's layout and tells that window. A pane the user already closed is `Unknown` to the layouts by
    /// now, so this cannot end the app a second time (RESEARCH Pitfall 4); only the last pane of the last window ends
    /// it. The last pane of any other window closes that window, and nobody is left to hear the exit of that pane.
    fn pane_gone(&self, pane: PaneId, code: Option<u32>) {
        let mut layout_event: Option<(Arc<str>, View)> = None;
        let mut exit_to: Option<Arc<str>> = None;
        let mut gone_window: Option<String> = None;
        let mut last = false;
        let slot = {
            let mut set = lock(&self.windows);
            let slot = lock(&self.panes).remove(&pane);
            // The window the pane is in; a pane that is in no layout (the user closed it) leaves it in `closing`.
            let label = lock(&self.routes).live.get(&pane).cloned();
            let closed_in = |routes: &Mutex<Routes>| lock(routes).closing.remove(&pane);
            if self.exiting.load(Ordering::SeqCst) {
                // The app is ending: what the user had stays in the layout.
                exit_to = label.or_else(|| closed_in(&self.routes));
            } else {
                match set.close_pane(pane) {
                    SetClose::Closed => {
                        if let Some(label) = label {
                            layout_event = self.view_in_set(&set, &label).ok().map(|view| (label.clone(), view));
                            exit_to = Some(label);
                        }
                        self.sync_routes(&set);
                    }
                    SetClose::WindowClosed(gone) => {
                        self.sync_routes(&set);
                        gone_window = Some(gone);
                    }
                    SetClose::LastPane => {
                        last = true;
                        exit_to = label;
                    }
                    SetClose::Unknown => exit_to = closed_in(&self.routes),
                }
            }
            slot
        };
        drop(slot);

        if let Some((label, view)) = layout_event {
            self.forget_pane(pane);
            self.saver.touch();
            self.sink.send(Target::Window(label), Out::Layout(view));
        }
        if let Some(gone) = gone_window {
            self.forget_pane(pane);
            self.window_removed(&gone);
        }
        if let Some(label) = exit_to {
            self.sink.send(Target::Window(label), Out::PaneExit { pane, code });
        }
        if last {
            self.begin_exit();
        }
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

    /// A line-editing step (word or line jump, delete word or line). The core owns the bytes.
    pub fn edit(&self, pane: PaneId, action: EditAction) -> Result<(), String> {
        match self.handle(pane) {
            Some(handle) => handle.edit(action).map_err(|e| e.to_string()),
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

    /// Change the grid of a pane. A pane that is still starting remembers the latest size and gets it when it is
    /// promoted (the page sends a size once and never again); a pane that is unknown is not an error.
    pub fn resize(&self, pane: PaneId, cols: u32, rows: u32, cell_w: u32, cell_h: u32) -> Result<(), String> {
        let (cols, rows) = clamp_grid(cols, rows);
        let cell_px = (clamp_cell(cell_w), clamp_cell(cell_h));
        // One look at the slot under the lock: a running pane's handle is used outside it, a starting pane stores
        // the size in the slot, so the promotion that takes it and a resize that arrives later cannot both miss it.
        let handle = match lock(&self.panes).get_mut(&pane) {
            Some(Slot::Running(handle)) => handle.clone(),
            Some(Slot::Starting(wanted)) => {
                *wanted = Some(WantedSize { cols, rows, cell_px });
                return Ok(());
            }
            None => return Ok(()),
        };
        handle.resize(cols, rows, cell_px).map_err(|e| e.to_string())
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

    #[cfg(all(test, unix))]
    fn pid(&self, pane: PaneId) -> Option<u32> {
        self.handle(pane)?.pid()
    }
}

impl Drop for AppCore {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The answer of a command that names a window the core has no layout for.
fn no_such_window() -> String {
    "no such window".to_owned()
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

/// Read the layout file: the layouts of all windows. A file that is missing gives a fresh layout. One that is damaged or inconsistent is
/// repaired or replaced, and its original bytes are copied to `<path>.bad` before anything can overwrite them. One
/// that is larger than `MAX_LAYOUT_BYTES` is not read; it is moved to `<path>.bad`.
fn load_layout(path: &Path) -> WindowSet {
    match persist::read_limited(path, MAX_LAYOUT_BYTES) {
        Ok(None) => WindowSet::new().0,
        Ok(Some(bytes)) => {
            let (layout, status) = match std::str::from_utf8(&bytes) {
                Ok(text) => WindowSet::from_json(text),
                Err(e) => (WindowSet::new().0, LoadStatus::Reset(format!("not UTF-8: {e}"))),
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
            WindowSet::new().0
        }
        Err(e) => {
            eprintln!("holzwave: cannot read {}: {e}", path.display());
            WindowSet::new().0
        }
    }
}

/// Copy a layout file that could not be used as it was to `<path>.bad`.
fn keep_bad_copy(path: &Path) {
    if let Err(e) = persist::backup_bad(path) {
        eprintln!("holzwave: cannot keep a copy of {}: {e}", path.display());
    }
}

// ---- links, paths and app info ------------------------------------------------------------------------------------

/// The version, the operating system and the host name, for the about dialog and the status bar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AppInfo {
    pub version: String,
    /// `macos`, `windows` or `linux`.
    pub os: String,
    /// Empty when it cannot be read.
    pub hostname: String,
}

/// A program of the platform to start and its arguments, one entry per argument: never a shell and never a string
/// that is split (V5).
///
/// The program is an owned string because a Windows launcher is `<SystemRoot>\System32\...`, known only at run
/// time. `raw_args` is for the one argument Windows cannot take quoted by the standard rules (the Explorer
/// `/select,"<path>"`); it is empty everywhere else.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Launch {
    program: String,
    args: Vec<String>,
    raw_args: Vec<String>,
}

impl Launch {
    /// A launch with normal arguments only.
    fn plain(program: &str, args: Vec<String>) -> Self {
        Self { program: program.to_owned(), args, raw_args: Vec::new() }
    }
}

/// The longest link that is opened, in bytes.
const MAX_URL_BYTES: usize = 2048;

/// The longest path (and base directory) that is revealed, in bytes.
const MAX_PATH_BYTES: usize = 4096;

/// The largest value of a pointer position, in device pixels.
const MAX_POINTER_PX: f64 = 100_000.0;

/// The most rows one wheel event moves, in either direction.
const MAX_WHEEL_ROWS: i32 = 100;

/// The first part of text that came from outside, for a message.
fn short(text: &str) -> String {
    text.chars().take(40).collect()
}

/// A variable of the process environment, for the pure functions that take a lookup.
fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Whether `url` is a link that may be opened: at most [`MAX_URL_BYTES`], no control or blank character, and the
/// scheme http or https with a host, or mailto with an address. The text is never interpreted further.
fn check_link(url: &str) -> Result<(), String> {
    if url.len() > MAX_URL_BYTES {
        return Err(format!("the link is longer than {MAX_URL_BYTES} bytes"));
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("the link has a control or blank character".into());
    }
    let Some((scheme, rest)) = url.split_once(':') else {
        return Err("the link has no scheme".into());
    };
    let names_something = match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => rest.strip_prefix("//").is_some_and(|host| !host.is_empty() && !host.starts_with('/')),
        "mailto" => !rest.is_empty(),
        _ => {
            return Err(format!(
                "links of the kind {:?} are not opened, only http, https and mailto",
                short(scheme)
            ));
        }
    };
    if names_something { Ok(()) } else { Err("the link names nothing to open".into()) }
}

/// How the platform opens a link: validated first, then one fixed program with the link as its only argument.
/// macOS uses `/usr/bin/open` by absolute path; Linux uses `xdg-open`, the one program that is looked up through
/// PATH (there is no fixed place for it). Windows hands the link to the URL handler of `url.dll` through
/// `rundll32.exe` under `%SystemRoot%\System32`: no `cmd`, no shell string, so `&` and `^` of a link are plain text.
/// A double quote is refused there because the argument is rebuilt into a command line.
///
/// `env` looks up a variable of the process; only the Windows launcher reads it.
fn open_launch(os: &str, url: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<Launch, String> {
    check_link(url)?;
    match os {
        "macos" => Ok(Launch::plain("/usr/bin/open", vec![url.to_owned()])),
        "linux" => Ok(Launch::plain("xdg-open", vec![url.to_owned()])),
        "windows" => {
            if url.contains('"') {
                return Err("the link has a double quote".into());
            }
            let rundll32 = format!(r"{}\System32\rundll32.exe", system_root(env));
            Ok(Launch::plain(&rundll32, vec!["url.dll,FileProtocolHandler".into(), url.to_owned()]))
        }
        other => Err(format!("opening links is not supported on {other}")),
    }
}

/// The Windows directory: `SystemRoot` if it is a drive path, else `C:\Windows`. A relative or network value of the
/// variable is not taken, so the launchers never leave the local drive.
pub(crate) fn system_root(env: &dyn Fn(&str) -> Option<String>) -> String {
    const DEFAULT: &str = r"C:\Windows";
    match env("SystemRoot") {
        Some(root) if is_drive_path(&root) => root.trim_end_matches(['\\', '/']).to_owned(),
        _ => DEFAULT.to_owned(),
    }
}

/// Whether `text` starts with a drive letter, a colon and a separator (`C:\` or `C:/`).
fn is_drive_path(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

/// Whether `text` is an absolute path on `os`. Windows is decided by the text (a drive path), so it is the same on
/// every host; elsewhere it is the host's rule.
fn is_absolute_on(os: &str, text: &str) -> bool {
    if os == "windows" { is_drive_path(text) } else { Path::new(text).is_absolute() }
}

/// The reason a path of Windows must not be given to the file system, if there is one. A path that starts with two
/// separators (`\\server\share`, `//server/share`, `\\?\` and `\\.\` too) names another machine, and Windows
/// authenticates to it when the path is merely checked (T-2.2-10). A double quote would end the quoting of the raw
/// Explorer argument and add arguments of its own (T-2.2-11).
fn windows_path_refusal(text: &str) -> Option<&'static str> {
    let mut chars = text.chars();
    let separator = |c: Option<char>| matches!(c, Some('\\' | '/'));
    if separator(chars.next()) && separator(chars.next()) {
        Some("a network or device path is not revealed")
    } else if text.contains('"') {
        Some("the path has a double quote")
    } else {
        None
    }
}

/// Whether `text` can be a path from the webview: not empty, at most [`MAX_PATH_BYTES`], no control character.
fn check_path_text(what: &str, text: &str) -> Result<(), String> {
    if text.is_empty() {
        Err(format!("{what} is empty"))
    } else if text.len() > MAX_PATH_BYTES {
        Err(format!("{what} is longer than {MAX_PATH_BYTES} bytes"))
    } else if text.chars().any(char::is_control) {
        Err(format!("{what} has a control character"))
    } else {
        Ok(())
    }
}

/// The home directory for `~/`: `HOME`, and on Windows `USERPROFILE`. An empty value is no value.
fn home_dir(os: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let name = if os == "windows" { "USERPROFILE" } else { "HOME" };
    env(name).filter(|home| !home.is_empty()).map(PathBuf::from)
}

/// The path a `reveal_path` call means: absolute, `~/` from the home directory, or relative to `base`, which has to
/// be absolute. The result is absolute (so it can never be taken for an option) and has to exist; a symbolic link
/// counts as what it is and is not followed.
///
/// On Windows the text of the path, the base and the result are refused before the file system is asked if they are
/// a network or device path or hold a double quote: even the existence check of `\\server\share` would contact the
/// server.
fn resolve_reveal(os: &str, path: &str, base: Option<&str>, home: Option<&Path>) -> Result<PathBuf, String> {
    let refuse = |text: &str| match os {
        "windows" => windows_path_refusal(text).map_or(Ok(()), |reason| Err(reason.to_owned())),
        _ => Ok(()),
    };
    check_path_text("the path", path)?;
    refuse(path)?;
    let resolved = if let Some(rest) = path.strip_prefix("~/") {
        let home = home
            .filter(|h| h.to_str().map_or_else(|| h.is_absolute(), |t| is_absolute_on(os, t)))
            .ok_or("the home directory is not known")?;
        // `~//etc` is below the home directory, not the root: a joined absolute path would replace it.
        home.join(rest.trim_start_matches('/'))
    } else if is_absolute_on(os, path) {
        PathBuf::from(path)
    } else {
        let base = base.ok_or("a relative path needs the working directory of the pane")?;
        check_path_text("the working directory", base)?;
        refuse(base)?;
        if !is_absolute_on(os, base) {
            return Err("the working directory is not an absolute path".into());
        }
        Path::new(base).join(path)
    };
    let text = resolved.to_str();
    if let Some(text) = text {
        refuse(text)?;
    }
    if !text.map_or_else(|| resolved.is_absolute(), |t| is_absolute_on(os, t)) {
        return Err("the path is not absolute".into());
    }
    std::fs::symlink_metadata(&resolved).map_err(|_| "there is no such file or folder".to_owned())?;
    Ok(resolved)
}

/// How the platform shows an existing absolute path in the file manager, without running it. macOS reveals the item
/// in Finder (`open -R`); Linux has no reveal, so the folder that holds a file is opened and a folder is opened as it
/// is. Windows starts `explorer.exe` under `%SystemRoot%`: for a file `/select,"<path>"`, as the one raw argument
/// and the only place a quote is allowed (the path holds none), for a folder the folder. The opener is only ever
/// given the path as an argument (T-02-52). `is_dir` says what the path is.
fn reveal_launch(
    os: &str,
    path: &Path,
    is_dir: bool,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Launch, String> {
    let text = |p: &Path| p.to_str().map(str::to_owned).ok_or_else(|| "the path is not valid text".to_owned());
    if !is_absolute_on(os, &text(path)?) {
        return Err("the path is not absolute".into());
    }
    match os {
        "macos" => Ok(Launch::plain("/usr/bin/open", vec!["-R".into(), text(path)?])),
        "linux" => {
            let target = if is_dir { path } else { path.parent().unwrap_or(path) };
            Ok(Launch::plain("xdg-open", vec![text(target)?]))
        }
        "windows" => {
            let path = text(path)?;
            if let Some(reason) = windows_path_refusal(&path) {
                return Err(reason.into());
            }
            let path = path.replace('/', "\\");
            let explorer = format!(r"{}\explorer.exe", system_root(env));
            if is_dir {
                // A trailing separator would escape the closing quote Rust puts around a path with a blank.
                let folder = if path.len() > 3 { path.trim_end_matches('\\') } else { &path };
                Ok(Launch::plain(&explorer, vec![folder.to_owned()]))
            } else {
                Ok(Launch { program: explorer, args: Vec::new(), raw_args: vec![format!("/select,\"{path}\"")] })
            }
        }
        other => Err(format!("revealing files is not supported on {other}")),
    }
}

/// Start `launch` and do not wait for it: a browser that is started anew can run for as long as the user keeps it.
/// Its status is collected on a thread of its own so that no zombie is left.
fn spawn_launch(launch: &Launch) -> Result<(), String> {
    let mut command = std::process::Command::new(&launch.program);
    command
        .args(&launch.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        for raw in &launch.raw_args {
            command.raw_arg(raw);
        }
    }
    #[cfg(not(windows))]
    debug_assert!(launch.raw_args.is_empty(), "a raw argument exists only on Windows");
    let mut child = command.spawn().map_err(|e| format!("cannot start {}: {e}", launch.program))?;
    let reaper = std::thread::Builder::new().name("hw-opener".into()).spawn(move || {
        let _ = child.wait();
    });
    if let Err(e) = reaper {
        eprintln!("holzwave: cannot start the thread that collects the opener: {e}");
    }
    Ok(())
}

/// A pointer coordinate as the engine gets it: finite and between 0 and [`MAX_POINTER_PX`]. A value that is not
/// finite is 0, as in the engine.
fn clamp_pointer(value: f64) -> f32 {
    if value.is_finite() { value.clamp(0.0, MAX_POINTER_PX) as f32 } else { 0.0 }
}

/// Wheel rows within the bounds of [`MAX_WHEEL_ROWS`], either direction.
fn clamp_rows(rows: i32) -> i32 {
    rows.clamp(-MAX_WHEEL_ROWS, MAX_WHEEL_ROWS)
}

/// The low four bits (shift, alt, ctrl, super) of what the page sent.
fn pointer_mods(mods: u32) -> Mods {
    Mods::from_bits((mods & 0xFF) as u8)
}

/// The host name from what the platform gave: the first line that is not blank, without control characters, at most
/// 255 characters. Text that is not UTF-8 is no name.
fn clean_hostname(raw: &[u8]) -> String {
    let Ok(text) = std::str::from_utf8(raw) else { return String::new() };
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    line.chars().filter(|c| !c.is_control()).take(255).collect()
}

/// The name of the machine from what the platform offers, `os` as `std::env::consts::OS` spells it: Windows has
/// `COMPUTERNAME`; Linux reads `/etc/hostname` and, when that is missing or holds no name, the kernel's
/// `/proc/sys/kernel/hostname`. A source that cannot be read is no name. macOS has no file for it (see
/// [`read_hostname`]), so it gives the empty string here.
fn hostname_from(
    os: &str,
    env: &dyn Fn(&str) -> Option<String>,
    read_file: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> String {
    match os {
        "windows" => env("COMPUTERNAME").map(|name| clean_hostname(name.as_bytes())).unwrap_or_default(),
        "linux" => ["/etc/hostname", "/proc/sys/kernel/hostname"]
            .iter()
            .filter_map(|file| read_file(file))
            .map(|bytes| clean_hostname(&bytes))
            .find(|name| !name.is_empty())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// The name of the machine. macOS asks `/bin/hostname` by absolute path; the other platforms read what
/// [`hostname_from`] says, and nothing is looked up through PATH (T-02-89). Empty when it cannot be read.
fn read_hostname() -> String {
    let os = std::env::consts::OS;
    if os == "macos" {
        return std::process::Command::new("/bin/hostname")
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| clean_hostname(&output.stdout))
            .unwrap_or_default();
    }
    hostname_from(os, &process_env, &|file| persist::read_limited(Path::new(file), 1024).ok().flatten())
}

/// Read the session file. There is no error case: a file that cannot be used costs the hints in it and nothing else,
/// and what could not be used is kept next to it as `.bad` before the next save can overwrite it.
fn load_session(path: &Path) -> Session {
    let Some(dir) = path.parent() else { return Session::default() };
    let (session, status) = Session::load(dir);
    match status {
        LoadStatus::Clean => {}
        LoadStatus::Repaired(notes) => {
            eprintln!("holzwave: {} repaired: {}", path.display(), notes.join("; "));
            keep_bad_copy(path);
        }
        // `Session::load` kept the copy already.
        LoadStatus::Reset(reason) => eprintln!("holzwave: {} reset: {reason}", path.display()),
    }
    session
}

/// Write the session: the geometry of every window and the working directory of every pane of the layouts. A pane's
/// directory is its last `Cwd` event; a running pane that never reported is asked for it, with no lock held because
/// that can start a process. A directory of a pane that is no longer in a layout is dropped, and so is the frame of a
/// window that is no longer in the set.
fn write_session(
    path: &Path,
    windows: &Mutex<WindowSet>,
    panes: &Mutex<HashMap<PaneId, Slot>>,
    session: &Mutex<Session>,
    last_cwd: &Mutex<HashMap<PaneId, String>>,
) {
    let (live, labels) = {
        let set = lock(windows);
        (set.all_panes(), set.labels().into_iter().map(str::to_owned).collect::<Vec<String>>())
    };
    let mut cwds = lock(last_cwd).clone();
    let silent: Vec<(PaneId, u32)> = {
        let panes = lock(panes);
        live.iter()
            .filter(|pane| !cwds.contains_key(pane))
            .filter_map(|pane| match panes.get(pane) {
                Some(Slot::Running(handle)) => handle.pid().map(|pid| (*pane, pid)),
                _ => None,
            })
            .collect()
    };
    for (pane, pid) in silent {
        if let Some(dir) = process_cwd(pid) {
            cwds.insert(pane, dir.to_string_lossy().into_owned());
        }
    }

    let snapshot = {
        let mut session = lock(session);
        for pane in &live {
            if let Some(cwd) = cwds.get(pane) {
                // A directory that fails the rules (control characters, too long) is simply not remembered.
                session.set_cwd(*pane, cwd);
            }
        }
        session.sanitise(&live);
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        session.sanitise_windows(&labels);
        session.clone()
    };
    if let Err(e) = persist::write_atomic(path, snapshot.to_json().as_bytes()) {
        eprintln!("holzwave: cannot save {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use holzwave_core::engine::Mods;
    #[cfg(unix)]
    use holzwave_core::engine::{CopyMotion, SelectMode};
    use holzwave_core::layout::Axis;

    #[cfg(unix)]
    const WAIT: Duration = Duration::from_secs(5);

    // Every test that starts a pane starts a real POSIX shell (`/bin/cat`, `/bin/sh -c ...`), so those are
    // `#[cfg(unix)]`; the tests that only build a core, a layout or a launch vector run everywhere. Real Windows
    // panes are tested where the PTY is: `core/tests/conpty.rs` (phase 2.2, plan 04).

    /// Collects what the core tells the webviews, with the window each event is for, and the windows it asked the
    /// sink to close.
    struct Recorder {
        log: Mutex<Vec<(Target, Out)>>,
        closed: Mutex<Vec<String>>,
        /// The windows the core asked to have built, with where, and whether every lock of the core was free then.
        opened: Mutex<Vec<(String, Place, bool)>>,
        /// Make `open_window` fail, as a window system that refuses.
        fail_open: AtomicBool,
        /// What is under the pointer; the tests set it.
        spot: Mutex<CursorSpot>,
        /// The core, to look at its locks from inside `open_window`.
        core: Mutex<Weak<AppCore>>,
    }

    impl Default for Recorder {
        fn default() -> Self {
            Recorder {
                log: Mutex::default(),
                closed: Mutex::default(),
                opened: Mutex::default(),
                fail_open: AtomicBool::new(false),
                spot: Mutex::new(CursorSpot::Unknown),
                core: Mutex::new(Weak::new()),
            }
        }
    }

    impl Sink for Recorder {
        fn send(&self, target: Target, out: Out) {
            lock(&self.log).push((target, out));
        }

        fn close_window(&self, label: &str) {
            lock(&self.closed).push(label.to_owned());
        }

        fn open_window(&self, label: &str, place: &Place) -> Result<(), String> {
            // The window is built on the main thread, which runs window events that take these locks: none may be
            // held by the caller. `try_lock` fails for a lock that the calling thread itself holds, too.
            let free = lock(&self.core).upgrade().is_none_or(|core| {
                core.windows.try_lock().is_ok() && core.routes.try_lock().is_ok() && core.panes.try_lock().is_ok()
            });
            lock(&self.opened).push((label.to_owned(), place.clone(), free));
            if self.fail_open.load(Ordering::SeqCst) { Err("the window system refused".into()) } else { Ok(()) }
        }

        fn cursor_spot(&self, _source: &str) -> CursorSpot {
            lock(&self.spot).clone()
        }
    }

    impl Recorder {
        /// Every event, whoever it was for.
        fn all(&self) -> Vec<Out> {
            lock(&self.log).iter().map(|(_, out)| out.clone()).collect()
        }

        /// Every event with its target.
        fn sent(&self) -> Vec<(Target, Out)> {
            lock(&self.log).clone()
        }

        /// The windows the core asked to have destroyed, in order.
        fn closed_windows(&self) -> Vec<String> {
            lock(&self.closed).clone()
        }

        #[cfg(unix)]
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
        #[cfg(unix)]
        fn has_text(&self, pane: PaneId, needle: &str) -> bool {
            self.frames(pane).iter().any(|f| {
                f.changed.iter().any(|row| {
                    let text: String = row.runs.iter().flat_map(|r| r.0.chars()).filter(|c| *c != '\0').collect();
                    text.contains(needle)
                })
            })
        }

        #[cfg(unix)]
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

    #[cfg(unix)]
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

    /// A fresh scratch directory, removed at the end of the test. A core attached to it is shut down first, so that
    /// a pending session write cannot bring the directory back.
    struct Scratch(PathBuf, Mutex<Option<Weak<AppCore>>>);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("holzwave-app-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path).expect("scratch dir");
            Scratch(path, Mutex::new(None))
        }

        fn layout_file(&self) -> PathBuf {
            self.0.join("layout.json")
        }

        fn attach(&self, core: &Arc<AppCore>) {
            *lock(&self.1) = Some(Arc::downgrade(core));
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            if let Some(core) = lock(&self.1).take().and_then(|weak| weak.upgrade()) {
                core.shutdown();
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A core whose panes run `program args` and whose events land in the returned recorder.
    fn core(program: &str, args: &[&str]) -> (Arc<AppCore>, Arc<Recorder>, Scratch) {
        let dir = Scratch::new("core");
        let rec = Arc::new(Recorder::default());
        let core = AppCore::new(dir.0.clone(), rec.clone(), ShellSpec::new(program, args));
        dir.attach(&core);
        (core, rec, dir)
    }

    fn sh(script: &str) -> (Arc<AppCore>, Arc<Recorder>, Scratch) {
        core("/bin/sh", &["-c", script])
    }

    fn only_pane(core: &AppCore) -> PaneId {
        core.view().panes[0].0
    }

    /// The single window of before: the commands as they were, on the window `main`. The tests that were written for
    /// one window keep calling these, which is what shows that one window behaves exactly as it did; what is new
    /// about several windows is tested with the `_in` methods and labels.
    impl AppCore {
        fn view(&self) -> View {
            self.view_in(MAIN_WINDOW).expect("main")
        }

        fn split(&self, column: bool) -> View {
            self.split_in(MAIN_WINDOW, column).expect("main")
        }

        fn new_tab(&self) -> View {
            self.new_tab_in(MAIN_WINDOW).expect("main")
        }

        fn new_workspace(&self, name: &str) -> View {
            self.new_workspace_in(MAIN_WINDOW, name).expect("main")
        }

        fn focus_pane(&self, pane: PaneId) -> View {
            self.focus_pane_in(MAIN_WINDOW, pane).expect("main")
        }

        fn cycle_focus(&self, step: i32) -> View {
            self.cycle_focus_in(MAIN_WINDOW, step).expect("main")
        }

        fn select_tab(&self, index: usize) -> View {
            self.select_tab_in(MAIN_WINDOW, index).expect("main")
        }

        fn select_workspace(&self, index: usize) -> View {
            self.select_workspace_in(MAIN_WINDOW, index).expect("main")
        }

        fn rename_pane(&self, pane: PaneId, name: &str) -> View {
            self.rename_pane_in(MAIN_WINDOW, pane, name).expect("main")
        }

        fn rename_tab(&self, index: usize, name: &str) -> View {
            self.rename_tab_in(MAIN_WINDOW, index, name).expect("main")
        }

        fn rename_workspace(&self, index: usize, name: &str) -> View {
            self.rename_workspace_in(MAIN_WINDOW, index, name).expect("main")
        }

        fn move_pane(&self, pane: PaneId, target: PaneId, edge: Edge) -> View {
            self.move_pane_in(MAIN_WINDOW, pane, target, edge).expect("main")
        }

        fn move_tab(&self, from: usize, to: usize) -> View {
            self.move_tab_in(MAIN_WINDOW, from, to).expect("main")
        }

        fn move_tab_to_workspace(&self, tab: usize, workspace: usize) -> View {
            self.move_tab_to_workspace_in(MAIN_WINDOW, tab, workspace).expect("main")
        }

        fn set_ratio(&self, split: SplitId, ratio: f32) -> View {
            self.set_ratio_in(MAIN_WINDOW, split, ratio).expect("main")
        }

        fn close_pane(&self, pane: PaneId) -> View {
            self.close_pane_in(MAIN_WINDOW, pane).expect("main")
        }
    }

    #[cfg(unix)]
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
    fn a_dropped_pane_moves_in_the_view_and_keeps_its_shell_and_name() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        let first = core.view().focus;
        let second = core.split(false).focus;
        core.rename_pane(first, "db");

        let view = core.move_pane(first, second, Edge::Bottom);

        let mut ids: Vec<_> = view.panes.iter().map(|(p, _)| *p).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![first, second], "no pane is lost, none is started again");
        assert_eq!(view.focus, first);
        assert_eq!(view.pane_names, vec![(first, "db".to_string())]);
        assert_eq!(core.move_pane(first, 9999, Edge::Left).focus, first, "an unknown target changes nothing");
    }

    #[test]
    fn tabs_are_reordered_and_moved_to_a_workspace_through_the_core() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        core.new_tab();
        core.new_workspace("work");
        core.select_workspace(0);

        let view = core.move_tab(0, 1);
        assert_eq!(view.tabs.len(), 2);
        let view = core.move_tab_to_workspace(0, 1);
        assert_eq!(view.workspaces.len(), 2);
        assert_eq!(view.tabs.len(), 2, "the shown workspace is now `work` with its own tab and the moved one");
    }

    #[test]
    fn a_rename_shows_in_the_view_is_cleaned_and_an_empty_name_restores_the_default() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        let pane = core.view().focus;

        let view = core.rename_pane(pane, "  api\u{7} server ");
        assert_eq!(view.pane_names, vec![(pane, "api server".to_string())]);
        let view = core.rename_tab(0, "website");
        assert_eq!(view.tabs[0].title, "website");
        let view = core.rename_workspace(0, "holzWave");
        assert_eq!(view.workspaces[0].name, "holzWave");

        let view = core.rename_pane(pane, "");
        assert!(view.pane_names.is_empty());
        assert_eq!(core.rename_tab(0, "").tabs[0].title, "shell");
        assert_eq!(core.rename_workspace(0, "").workspaces[0].name, "ws 1");
        assert_eq!(core.rename_pane(9999, "ghost").pane_names, vec![], "an unknown pane changes nothing");
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn a_pane_started_with_a_zero_grid_gets_the_smallest_grid() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);

        core.start_pane(pane, 0, 0, 0, 0).expect("starts");

        assert!(wait_until(WAIT, || !rec.frames(pane).is_empty()), "no frame: {:?}", rec.all());
        let first = rec.frames(pane).remove(0);
        assert_eq!((first.cols, first.rows), (2, 1));
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn a_key_for_a_cat_shell_comes_back_as_a_frame_with_the_character() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");

        core.key(pane, key("KeyX", "x")).expect("the key is accepted");

        assert!(wait_until(WAIT, || rec.has_text(pane, "x")), "no echo: {:?}", rec.all());
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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
    #[cfg(unix)]
    const HUP_PROOF: &str = "trap '' HUP; echo ready; exec sleep 1000";

    #[cfg(unix)]
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

    /// A view with its `rev` zeroed: two views of the same layout are equal otherwise, whenever they were built.
    fn without_rev(view: &View) -> View {
        View { rev: 0, ..view.clone() }
    }

    fn snapshot(core: &AppCore) -> Layout {
        lock(&core.windows).layout(MAIN_WINDOW).expect("the window main").clone()
    }

    fn on_disk(dir: &Scratch) -> Layout {
        let text = std::fs::read_to_string(dir.layout_file()).expect("layout.json");
        let (layout, status) = Layout::from_json(&text);
        assert_eq!(status, LoadStatus::Clean, "what the core writes loads clean");
        layout
    }

    #[cfg(unix)]
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

    // ---- the order of the views (WR-06) -----------------------------------------------------------------------

    #[test]
    fn every_view_is_stamped_in_the_order_it_was_built_and_the_event_has_the_rev_of_the_answer() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let first = only_pane(&core);
        let initial = core.view();
        let split = core.split(false);
        let second = split.focus;
        let focused = core.focus_pane(first);
        assert!(initial.rev < split.rev && split.rev < focused.rev, "{} {} {}", initial.rev, split.rev, focused.rev);

        let closed = core.close_pane(second);
        assert!(closed.rev > focused.rev);
        assert!(core.view().rev > closed.rev, "a read is stamped too, and is newer than what it follows");
        let events = rec.layouts();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], closed, "the event carries the view the answer carries, rev included");
    }

    #[test]
    fn views_built_at_the_same_time_are_stamped_in_the_order_of_their_layout_states() {
        // Three panes, and a focus that moves one pane further with every call. A view of rev r + 1 shows the focus
        // one step after the view of rev r, however the calls of four threads interleave: the stamp is taken under
        // the layout lock.
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        core.split(false);
        core.split(false);
        let base = core.view().rev;
        let seen = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..50 {
                        let view = core.cycle_focus(1);
                        lock(&seen).push((view.rev, view.focus));
                    }
                });
            }
        });
        let mut seen = lock(&seen).clone();
        seen.sort();
        let revs: Vec<u64> = seen.iter().map(|(rev, _)| *rev).collect();
        assert_eq!(revs, (base + 1..=base + 200).collect::<Vec<_>>(), "every view has a rev of its own");
        for pair in seen.windows(3) {
            assert_ne!(pair[0].1, pair[1].1);
            assert_ne!(pair[0].1, pair[2].1);
            assert_ne!(pair[1].1, pair[2].1);
        }
        for pair in seen.windows(4) {
            assert_eq!(pair[0].1, pair[3].1, "the focus cycles through three panes in the order of the revs");
        }
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

    #[cfg(unix)]
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

        assert_eq!(without_rev(&after), without_rev(&before), "the same layout");
        assert!(after.rev > before.rev, "but a view of its own: it is the newest one");
        std::thread::sleep(Duration::from_millis(100));
        assert!(rec.all().is_empty(), "{:?}", rec.all());
        assert!(!dir.layout_file().exists(), "nothing changed, nothing is written");
    }

    #[cfg(unix)]
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

    // ---- a resize while the pane is still starting (WR-03) ------------------------------------------------------

    /// A pane handle made the way `start_pane` makes one, whose frames land in the recorder. The window in which
    /// the slot is `Starting` is held open by the test: it reserves, asks, and only then promotes this handle.
    #[cfg(unix)]
    fn spawned_for(core: &AppCore, rec: &Arc<Recorder>, pane: PaneId) -> PaneHandle {
        let spec = core.pane_spec(80, 24, (9, 18));
        let rec = rec.clone();
        PaneHandle::spawn_ghostty(pane, spec, move |event| {
            if let PaneEvent::Frame(frame) = event {
                rec.send(Target::window(MAIN_WINDOW), Out::Frame(frame));
            }
        })
        .expect("the shell starts")
    }

    #[cfg(unix)]
    fn size_of_last_frame(rec: &Recorder, pane: PaneId) -> Option<(u16, u16)> {
        rec.frames(pane).last().map(|f| (f.cols, f.rows))
    }

    #[cfg(unix)]
    #[test]
    fn a_resize_that_arrives_while_the_pane_is_starting_is_applied_on_promotion() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        assert_eq!(core.reserve(pane), Ok(Reserved::Now));

        // The page asks twice while the shell is being started; both are accepted.
        core.resize(pane, 90, 20, 9, 18).expect("accepted while the pane is starting");
        core.resize(pane, 100, 30, 9, 18).expect("accepted while the pane is starting");

        let handle = spawned_for(&core, &rec, pane);
        let look = lock(&core.config).look();
        core.promote(pane, handle, look);

        assert!(
            wait_until(WAIT, || rec.frames(pane).iter().any(|f| (f.cols, f.rows) == (100, 30))),
            "no frame at 100 x 30, the sizes seen: {:?}",
            rec.frames(pane).iter().map(|f| (f.cols, f.rows)).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            rec.frames(pane).iter().all(|f| (f.cols, f.rows) != (90, 20)),
            "the older size was replayed: {:?}",
            rec.frames(pane).iter().map(|f| (f.cols, f.rows)).collect::<Vec<_>>()
        );
        assert_eq!(size_of_last_frame(&rec, pane), Some((100, 30)));
    }

    #[cfg(unix)]
    #[test]
    fn a_resize_that_overlaps_the_promotion_is_never_overtaken_by_the_replay() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        assert_eq!(core.reserve(pane), Ok(Reserved::Now));
        core.resize(pane, 100, 30, 9, 18).expect("accepted while the pane is starting");

        let handle = spawned_for(&core, &rec, pane);
        let look = lock(&core.config).look();
        let racer = core.clone();
        let mut thread = None;
        // The closure runs inside the promotion, after the slot became Running and before anything is sent to the
        // handle: a second resize is issued from another thread exactly there. A correct promotion holds the
        // `panes` lock, so that thread is blocked until the replay is made; the closure only gives a wrong order
        // the time to show itself.
        core.promote_with(pane, handle, look, || {
            let spawned = std::thread::spawn(move || racer.resize(pane, 120, 40, 9, 18));
            let deadline = Instant::now() + Duration::from_millis(300);
            while !spawned.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            thread = Some(spawned);
        });
        thread.expect("the closure ran").join().expect("no panic").expect("accepted");

        assert!(
            wait_until(WAIT, || rec.frames(pane).iter().any(|f| (f.cols, f.rows) == (120, 40))),
            "no frame at 120 x 40, the sizes seen: {:?}",
            rec.frames(pane).iter().map(|f| (f.cols, f.rows)).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            size_of_last_frame(&rec, pane),
            Some((120, 40)),
            "the newer size must be the last one applied: {:?}",
            rec.frames(pane).iter().map(|f| (f.cols, f.rows)).collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_size_asked_for_while_the_pane_is_starting_is_clamped_like_a_running_one() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        assert_eq!(core.reserve(pane), Ok(Reserved::Now));
        core.resize(pane, 5_000, 0, 9, 18).expect("accepted while the pane is starting");

        let handle = spawned_for(&core, &rec, pane);
        let look = lock(&core.config).look();
        core.promote(pane, handle, look);

        assert!(
            wait_until(WAIT, || rec.frames(pane).iter().any(|f| (f.cols, f.rows) == (1000, 1))),
            "no frame at the clamped size 1000 x 1, the sizes seen: {:?}",
            rec.frames(pane).iter().map(|f| (f.cols, f.rows)).collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pane_that_is_closed_while_it_starts_leaves_no_size_behind() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let first = only_pane(&core);
        let second = core.split(false).focus;
        assert_ne!(first, second);
        assert_eq!(core.reserve(second), Ok(Reserved::Now));
        core.resize(second, 100, 30, 9, 18).expect("accepted while the pane is starting");

        core.close_pane(second);
        assert!(lock(&core.panes).get(&second).is_none(), "the slot, and the size it carried, are gone");

        // The shell that was being started arrives late: it is closed and no size is applied to it.
        let handle = spawned_for(&core, &rec, second);
        let look = lock(&core.config).look();
        core.promote(second, handle, look);
        assert!(lock(&core.panes).get(&second).is_none());
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            rec.frames(second).iter().all(|f| (f.cols, f.rows) != (100, 30)),
            "a size was applied to a pane that was closed: {:?}",
            rec.frames(second).iter().map(|f| (f.cols, f.rows)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_starting_pane_remembers_one_size_the_latest_and_clamped() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        assert_eq!(core.reserve(pane), Ok(Reserved::Now));
        assert!(matches!(lock(&core.panes).get(&pane), Some(Slot::Starting(None))), "nothing is asked for yet");

        core.resize(pane, 90, 20, 9, 18).expect("accepted");
        core.resize(pane, 5_000, 0, 100_000, 18).expect("accepted");

        match lock(&core.panes).get(&pane) {
            Some(Slot::Starting(Some(size))) => {
                assert_eq!(*size, WantedSize { cols: 1000, rows: 1, cell_px: (256, 18) });
            }
            _ => panic!("the slot should be Starting with a remembered size"),
        }
        assert_eq!(core.reserve(pane), Ok(Reserved::Already), "a second reservation leaves the slot alone");
        assert!(matches!(lock(&core.panes).get(&pane), Some(Slot::Starting(Some(_)))));
    }

    #[cfg(unix)]
    #[test]
    fn a_resize_for_a_running_pane_and_for_an_unknown_pane_behaves_as_before() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        assert_eq!(core.resize(31_337, 100, 30, 9, 18), Ok(()), "an unknown pane is not an error");
        assert!(lock(&core.panes).is_empty(), "and no slot is made for it");

        core.start_pane(pane, 80, 24, 9, 18).expect("starts");
        assert!(wait_until(WAIT, || !rec.frames(pane).is_empty()), "no first frame");
        core.resize(pane, 100, 30, 9, 18).expect("a running pane takes the size at once");

        assert!(
            wait_until(WAIT, || size_of_last_frame(&rec, pane) == Some((100, 30))),
            "{:?}",
            rec.frames(pane).iter().map(|f| (f.cols, f.rows)).collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_that_ends_in_a_background_tab_leaves_the_shown_tab_and_the_saved_layout_alone() {
        let (core, rec, dir) = sh("exit 0");
        let a = only_pane(&core);
        let b = core.new_tab().focus;
        let c = core.new_tab().focus;
        assert_eq!(core.focus_pane(b).focus, b, "tab B is the one the user looks at");
        assert!(a != b && b != c && a != c);

        // Only the shell of tab A is started, so only it ends, while tab B is shown.
        core.start_pane(a, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || !rec.layouts().is_empty()), "the pane stayed in the layout: {:?}", rec.all());

        let view = core.view();
        assert_eq!(view.focus, b, "the focus did not move");
        assert_eq!(view.panes.iter().map(|p| p.0).collect::<Vec<_>>(), vec![b], "tab B is still shown");
        assert_eq!(view.tabs.len(), 2);
        assert!(view.tabs[0].active && !view.tabs[1].active, "the first of the two tabs is the shown one");
        assert_eq!(view.tabs[0].focus, b);
        assert_eq!(rec.layouts().last().map(without_rev), Some(without_rev(&view)), "the page was told the same");
        assert_eq!(rec.exit_count(), 0);

        core.flush();
        let saved = on_disk(&dir);
        assert_eq!(saved.tab().panes(), vec![b], "the saved layout names tab B as the shown tab");
        assert_eq!(saved.tab().focus, b);
        assert_eq!(saved.workspace().tabs.len(), 2);
        assert_eq!(saved.all_panes(), vec![b, c]);
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_that_ends_in_a_background_workspace_leaves_the_shown_workspace_and_the_saved_layout_alone() {
        let (core, rec, dir) = sh("exit 0");
        let a = only_pane(&core);
        let b = core.new_workspace("one").focus;
        let c = core.new_workspace("two").focus;
        assert_eq!(core.select_workspace(1).focus, b, "workspace W1 is the one the user looks at");
        assert!(a != b && b != c && a != c);

        // Only the shell of W0 is started, so only it ends, while W1 is shown.
        core.start_pane(a, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || !rec.layouts().is_empty()), "the pane stayed in the layout: {:?}", rec.all());

        let view = core.view();
        assert_eq!(view.focus, b, "the focus did not move");
        assert_eq!(view.panes.iter().map(|p| p.0).collect::<Vec<_>>(), vec![b], "workspace W1 is still shown");
        assert_eq!(view.workspaces.len(), 2);
        assert!(view.workspaces[0].active && !view.workspaces[1].active, "the first of the two is the shown one");
        assert_eq!(view.workspaces[0].name, "one");
        assert_eq!(rec.layouts().last().map(without_rev), Some(without_rev(&view)), "the page was told the same");
        assert_eq!(rec.exit_count(), 0);

        core.flush();
        let saved = on_disk(&dir);
        assert_eq!(saved.tab().panes(), vec![b], "the saved layout names W1 as the shown workspace");
        assert_eq!(saved.active, 0);
        assert_eq!(saved.workspaces.len(), 2);
        assert_eq!(saved.all_panes(), vec![b, c]);
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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
        let past_the_cap = core.split(false);
        assert_eq!(without_rev(&past_the_cap), without_rev(&view), "a split past the cap returns the same layout");
        assert!(past_the_cap.rev > view.rev, "as a newer view");
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

        assert_eq!(lock(&core.windows).all_panes().len(), 1);
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

        assert_eq!(snapshot(&core), Layout::new().0, "an oversized file is not used");
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
            let set = lock(&core.windows);
            let l = set.layout(MAIN_WINDOW).expect("main");
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
    // ---- settings, themes and key bindings (plan 23, task 1) -----------------------------------------------------

    use holzwave_core::keybindings::actions;
    #[cfg(unix)]
    use holzwave_core::theme::Rgb;
    use serde_json::json;

    impl Recorder {
        fn settings_events(&self) -> Vec<SettingsView> {
            self.all()
                .into_iter()
                .filter_map(|o| match o {
                    Out::Settings(view) => Some(view),
                    _ => None,
                })
                .collect()
        }

        /// Whether a frame of the pane at or after `skip` is a full one with this default background.
        #[cfg(unix)]
        fn has_full_frame_with_bg(&self, pane: PaneId, skip: usize, bg: u32) -> bool {
            self.frames(pane).iter().skip(skip).any(|f| f.full && f.bg == bg)
        }
    }

    #[cfg(unix)]
    fn packed(c: Rgb) -> u32 {
        u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
    }

    /// A core over an existing scratch directory, so that a test can put files there first.
    fn core_in(dir: &Scratch, program: &str, args: &[&str]) -> (Arc<AppCore>, Arc<Recorder>) {
        let rec = Arc::new(Recorder::default());
        let core = AppCore::new(dir.0.clone(), rec.clone(), ShellSpec::new(program, args));
        dir.attach(&core);
        (core, rec)
    }

    #[cfg(unix)]
    fn theme_file(dir: &Scratch, file: &str, theme: &Theme) {
        let themes = dir.0.join("themes");
        std::fs::create_dir_all(&themes).expect("themes dir");
        std::fs::write(themes.join(file), serde_json::to_string(theme).expect("theme json")).expect("theme file");
    }

    #[test]
    fn with_no_files_the_settings_view_is_the_defaults_the_three_built_in_themes_and_every_action() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);

        let view = core.get_settings();

        assert_eq!(view.settings, Settings::default());
        assert_eq!(view.themes.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["aubergine", "Schiefer", "Papier"]);
        assert!(view.themes.iter().all(|t| t.builtin));
        assert_eq!(view.theme, Theme::aubergine());
        assert_eq!(view.keybindings.len(), actions().len());
        assert!(view.keybindings.iter().all(|row| row.is_default));
        // Every action has a default chord but the rename, swap and move actions and, off macOS, the line start and end ones.
        assert!(view.keybindings.iter().all(|row| row.chord.is_some() || row.action.contains("rename") || row.action.starts_with("pane.swap") || row.action.starts_with("pane.move") || row.action.starts_with("tab.move") || row.action.starts_with("edit.line") || row.action == "edit.delete_to_line_start"));
        assert_eq!(view.warnings, Vec::<String>::new());
    }

    #[test]
    fn the_warnings_carry_the_load_notes_of_the_files_and_the_refused_key_bindings() {
        let dir = Scratch::new("warnings");
        std::fs::write(
            dir.0.join("settings.json"),
            r#"{"font_size": 9999, "keybindings": {"pane.close": "x", "no.such.action": "Cmd+J"}}"#,
        )
        .unwrap();
        std::fs::create_dir(dir.0.join("themes")).unwrap();
        std::fs::write(dir.0.join("themes").join("broken.json"), "{ nope").unwrap();

        let (core, _rec) = core_in(&dir, "/bin/cat", &[]);
        let view = core.get_settings();
        // The default of the host's table: it is Cmd+W on macOS and something else elsewhere.
        let default_close = Keybindings::default().chord_of("pane.close").map(|chord| chord.to_string());

        assert_eq!(view.settings.font_size, 40, "the number is clamped");
        let said = |needle: &str| view.warnings.iter().any(|w| w.contains(needle));
        assert!(said("font_size"), "{:?}", view.warnings);
        assert!(said("broken.json"), "{:?}", view.warnings);
        assert!(said("pane.close"), "the refused chord is named: {:?}", view.warnings);
        assert!(said("no.such.action"), "{:?}", view.warnings);
        let close = view.keybindings.iter().find(|r| r.action == "pane.close").expect("row");
        assert!(default_close.is_some());
        assert_eq!(close.chord, default_close, "the refused override leaves the default");
    }

    #[test]
    fn a_damaged_settings_file_starts_with_the_defaults_and_is_kept_as_bad() {
        let dir = Scratch::new("badsettings");
        std::fs::write(dir.0.join("settings.json"), "{ not json").unwrap();

        let (core, _rec) = core_in(&dir, "/bin/cat", &[]);

        assert_eq!(core.get_settings().settings, Settings::default());
        assert!(core.get_settings().warnings.iter().any(|w| w.contains("settings.json")));
        assert_eq!(std::fs::read_to_string(dir.0.join("settings.json.bad")).unwrap(), "{ not json");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_pane_is_built_from_the_settings_and_its_first_frame_has_the_theme_colours() {
        let dir = Scratch::new("spec");
        std::fs::write(
            dir.0.join("settings.json"),
            r#"{"theme": "Schiefer", "option_as_alt": true, "scrollback_lines": 500}"#,
        )
        .unwrap();
        let (core, rec) = core_in(&dir, "/bin/cat", &[]);

        let spec = core.pane_spec(80, 24, (9, 18));

        assert_eq!(spec.scrollback, 500);
        assert!(spec.option_as_alt);
        assert_eq!(spec.theme, Some(Theme::schiefer().engine_theme()));
        assert_eq!(spec.cell_px, (9, 18));
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        let bg = packed(Theme::schiefer().terminal.background);
        assert!(wait_until(WAIT, || rec.has_full_frame_with_bg(pane, 0, bg)), "the first frame lacks the theme");
    }

    #[cfg(unix)]
    #[test]
    fn set_setting_theme_persists_pushes_a_full_frame_with_the_new_colours_and_emits_settings() {
        let (core, rec, dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || !rec.frames(pane).is_empty()), "no first frame");
        let before = rec.frames(pane).len();

        let view = core.set_setting("theme", json!("Schiefer")).expect("a known theme is accepted");

        assert_eq!(view.settings.theme, "Schiefer");
        assert_eq!(view.theme.name, "Schiefer");
        let schiefer = packed(Theme::schiefer().terminal.background);
        assert!(
            wait_until(WAIT, || rec.has_full_frame_with_bg(pane, before, schiefer)),
            "no full frame with the new colours arrived"
        );
        let (saved, status) = Settings::load(&dir.0);
        assert_eq!(status, LoadStatus::Clean, "what the core writes loads clean");
        assert_eq!(saved.theme, "Schiefer");
        assert!(!dir.0.join("settings.json.tmp").exists(), "the write went through a temporary file that is gone");
        assert_eq!(rec.settings_events(), vec![view.clone()]);
        assert_eq!(core.get_settings(), view);
    }

    // ---- the shell a pane starts (DESK-30, DESK-31) -----------------------------------------------------------------

    /// An environment and a file system made of lists, so no case reads the host.
    fn facts<'a>(
        env: &'a [(&'a str, &'a str)],
        files: &'a [&'a str],
    ) -> (impl Fn(&str) -> Option<String> + 'a, impl Fn(&str) -> bool + 'a, impl Fn(&str) -> ProgramCheck + 'a) {
        (
            move |key: &str| env.iter().find(|(k, _)| *k == key).map(|(_, v)| (*v).to_owned()),
            move |path: &str| files.contains(&path),
            move |path: &str| if files.contains(&path) { ProgramCheck::Usable } else { ProgramCheck::Missing },
        )
    }

    fn launch_of(
        spec: &ShellSpec,
        configured: &str,
        os: Os,
        env: &[(&str, &str)],
        files: &[&str],
    ) -> (String, Vec<String>) {
        let (env, exists, check) = facts(env, files);
        let launch = spec.launch(configured, os, &env, &exists, &check).launch;
        (launch.program, launch.args)
    }

    #[test]
    fn the_system_shell_is_what_the_platform_names_on_every_os() {
        let system = ShellSpec::default();
        let login = vec!["-l".to_owned()];

        let linux = launch_of(&system, "", Os::Linux, &[("SHELL", "/usr/bin/fish")], &[]);
        assert_eq!(linux, ("/usr/bin/fish".to_owned(), login.clone()));
        assert_eq!(launch_of(&system, "", Os::Linux, &[], &[]), ("/bin/sh".to_owned(), login.clone()), "no $SHELL");
        assert_eq!(launch_of(&system, "", Os::Macos, &[], &[]), ("/bin/zsh".to_owned(), login.clone()));
        assert_eq!(launch_of(&system, "", Os::Macos, &[("SHELL", "")], &[]).0, "/bin/zsh", "an empty $SHELL is unset");

        let pwsh = r"C:\Program Files\PowerShell\7\pwsh.exe";
        let env = [("ProgramFiles", r"C:\Program Files"), ("SystemRoot", r"C:\Windows")];
        let (program, args) = launch_of(&system, "", Os::Windows, &env, &[pwsh]);
        assert_eq!(program, pwsh, "pwsh 7 first");
        assert_eq!(args.first().map(String::as_str), Some("-NoLogo"));
        assert!(args.iter().any(|a| a == "-EncodedCommand"), "PowerShell gets the prompt integration: {args:?}");

        let windows_powershell = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
        let (program, args) = launch_of(&system, "", Os::Windows, &env, &[windows_powershell]);
        assert_eq!(program, windows_powershell, "then Windows PowerShell");
        assert!(args.iter().any(|a| a == "-EncodedCommand"), "{args:?}");

        let (program, args) = launch_of(&system, "", Os::Windows, &[("ComSpec", r"C:\Windows\System32\cmd.exe")], &[]);
        assert_eq!(program, r"C:\Windows\System32\cmd.exe", "then cmd");
        assert!(args.is_empty(), "cmd.exe gets no arguments: {args:?}");
    }

    #[test]
    fn the_shell_setting_replaces_the_platform_default_and_the_arguments_follow_its_name() {
        let system = ShellSpec::default();
        let env = [("SHELL", "/bin/zsh")];
        let fish = launch_of(&system, "/usr/bin/fish", Os::Linux, &env, &["/usr/bin/fish"]);
        assert_eq!(fish, ("/usr/bin/fish".to_owned(), vec!["-l".to_owned()]));
        assert_eq!(launch_of(&system, "/opt/homebrew/bin/nu", Os::Macos, &env, &["/opt/homebrew/bin/nu"]).0, "/opt/homebrew/bin/nu");

        let (program, args) = launch_of(&system, r"C:\Windows\System32\cmd.exe", Os::Windows, &[], &[r"C:\Windows\System32\cmd.exe"]);
        assert_eq!(program, r"C:\Windows\System32\cmd.exe");
        assert!(args.is_empty(), "{args:?}");
        let (_, args) = launch_of(&system, r"D:\Tools\PWSH.EXE", Os::Windows, &[], &[r"D:\Tools\PWSH.EXE"]);
        assert!(args.iter().any(|a| a == "-EncodedCommand"), "the stem decides, without case: {args:?}");
        let (program, args) = launch_of(&system, r"C:\Program Files\Git\bin\bash.exe", Os::Windows, &[], &[r"C:\Program Files\Git\bin\bash.exe"]);
        assert_eq!((program.as_str(), args), (r"C:\Program Files\Git\bin\bash.exe", vec!["-l".to_owned()]));
        // One program, never split: a space or a quote stays inside the one string.
        let (program, _) = launch_of(&system, "/bin/sh -c 'x; rm -rf ~'", Os::Linux, &env, &["/bin/sh -c 'x; rm -rf ~'"]);
        assert_eq!(program, "/bin/sh -c 'x; rm -rf ~'");
    }

    #[test]
    fn a_fixed_shell_ignores_the_platform_and_the_setting() {
        let fixed = ShellSpec::new("/bin/cat", &["-u"]);
        for os in [Os::Macos, Os::Windows, Os::Linux] {
            let launch = launch_of(&fixed, "/usr/bin/fish", os, &[("SHELL", "/bin/zsh")], &[]);
            assert_eq!(launch, ("/bin/cat".to_owned(), vec!["-u".to_owned()]), "{os:?}");
        }
    }

    #[test]
    fn in_a_flatpak_the_system_shell_is_the_host_login_shell_and_the_setting_is_not_looked_up_in_the_sandbox() {
        use holzwave_core::platform::HOST_LOGIN_SHELL_SCRIPT;
        let system = ShellSpec::default();
        let flatpak = [("FLATPAK_ID", "ch.holzcloud.holzwave"), ("SHELL", "/bin/sh")];
        assert_eq!(
            launch_of(&system, "", Os::Linux, &flatpak, &[]),
            ("sh".to_owned(), vec!["-c".to_owned(), HOST_LOGIN_SHELL_SCRIPT.to_owned()]),
            "no setting: one constant script finds the host's login shell, $SHELL of the sandbox is not asked"
        );
        // The setting names a program of the host: the sandbox does not have it, and that is no defect.
        assert_eq!(launch_of(&system, "/usr/bin/fish", Os::Linux, &flatpak, &[]), ("/usr/bin/fish".to_owned(), vec!["-l".to_owned()]));
        assert_eq!(unusable_of("/usr/bin/fish", Os::Linux, &flatpak, &[]), None);
        // The info file alone is enough.
        assert_eq!(launch_of(&system, "", Os::Linux, &[], &["/.flatpak-info"]).0, "sh");
        // A fixed shell (what the tests inject) stays what it is.
        assert_eq!(launch_of(&ShellSpec::new("/bin/cat", &[]), "", Os::Linux, &flatpak, &[]).0, "/bin/cat");
        // No Flatpak, or another OS: nothing changes.
        assert_eq!(launch_of(&system, "", Os::Linux, &[("SHELL", "/usr/bin/fish")], &[]).0, "/usr/bin/fish");
        assert_eq!(launch_of(&system, "", Os::Macos, &flatpak, &[]).0, "/bin/sh", "$SHELL of the environment, as on a Mac");
        assert_eq!(
            unusable_of("/usr/bin/fish", Os::Linux, &[("SHELL", "/bin/sh")], &[]).is_some(),
            true,
            "outside a Flatpak a setting that is not there is still reported"
        );
    }

    #[test]
    fn the_host_spawn_and_lookup_flags_follow_the_environment() {
        let dir = Scratch::new("flatpak-flags");
        let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());
        dir.attach(&core);
        let spec = |os: Os, vars: &'static [(&'static str, &'static str)], files: &'static [&'static str]| {
            let (env, exists, _) = facts(vars, files);
            core.pane_spec_on(80, 24, (9, 18), os, &env, &exists)
        };

        let inside = spec(Os::Linux, &[("FLATPAK_ID", "ch.holzcloud.holzwave")], &[]);
        assert!(inside.host_spawn && !inside.cwd_lookup, "a Flatpak: spawn on the host, no lookup of the process directory");
        assert_eq!(inside.program, "sh");
        let by_file = spec(Os::Linux, &[], &["/.flatpak-info"]);
        assert!(by_file.host_spawn && !by_file.cwd_lookup, "the info file alone");

        for (os, vars, files) in [
            (Os::Linux, &[("SHELL", "/bin/sh")][..], &[][..]),
            (Os::Linux, &[("FLATPAK_ID", "")][..], &[][..]),
            (Os::Macos, &[("FLATPAK_ID", "x")][..], &["/.flatpak-info"][..]),
            (Os::Windows, &[("FLATPAK_ID", "x")][..], &[][..]),
        ] {
            let spec = spec(os, vars, files);
            assert!(!spec.host_spawn && spec.cwd_lookup, "{os:?} {vars:?}: nothing is wrapped and the directory is looked up");
        }
    }

    #[cfg(unix)]
    #[test]
    fn pane_spec_follows_the_shell_setting_for_panes_started_afterwards() {
        let dir = Scratch::new("shell-setting");
        let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());
        dir.attach(&core);
        let before = core.pane_spec(80, 24, (9, 18));
        assert_eq!(before.args, ["-l"]);

        // Not /bin/sh: that is $SHELL in a Linux container, and the point is that the spec changes.
        core.set_setting("shell", json!("/bin/cat")).expect("a string is accepted");
        let after = core.pane_spec(80, 24, (9, 18));

        assert_eq!((after.program.as_str(), after.args.as_slice()), ("/bin/cat", ["-l".to_owned()].as_slice()));
        assert_eq!(core.get_settings().settings.shell, "/bin/cat");
        assert_ne!(before.program, "/bin/cat", "a spec made earlier keeps its shell");
        core.set_setting("shell", json!("")).expect("empty is the default again");
        assert_eq!(core.pane_spec(80, 24, (9, 18)).program, before.program);
        assert!(core.set_setting("shell", json!(5)).is_err(), "another type is refused");
        assert_eq!(core.get_settings().settings.shell, "");
    }

    /// The notice and the warning that an unusable `shell` setting gives, as a resolution on `os` says it.
    fn unusable_of(configured: &str, os: Os, env: &[(&str, &str)], files: &[&str]) -> Option<String> {
        let (env, exists, check) = facts(env, files);
        ShellSpec::default().launch(configured, os, &env, &exists, &check).unusable
    }

    #[test]
    fn an_unusable_shell_setting_falls_back_to_the_platform_default_with_a_sentence() {
        let system = ShellSpec::default();
        let env = [("SHELL", "/usr/bin/fish")];
        let login = vec!["-l".to_owned()];

        // Not found: the login shell starts, and the sentence names the value and what is used instead.
        let (program, args) = launch_of(&system, "/nonexistent/shell", Os::Linux, &env, &[]);
        assert_eq!((program.as_str(), args), ("/usr/bin/fish", login.clone()));
        let said = unusable_of("/nonexistent/shell", Os::Linux, &env, &[]).expect("a sentence");
        assert!(said.contains("/nonexistent/shell") && said.contains("was not found"), "{said}");
        assert!(said.contains("/usr/bin/fish"), "it names the shell that is used: {said}");
        assert_eq!(launch_of(&system, "/nonexistent/shell", Os::Linux, &[], &[]).0, "/bin/sh", "no $SHELL: /bin/sh");
        assert_eq!(launch_of(&system, "/nonexistent/shell", Os::Macos, &[], &[]).0, "/bin/zsh");

        // Windows: pwsh, then Windows PowerShell, then cmd.
        let pwsh = r"C:\Program Files\PowerShell\7\pwsh.exe";
        let windows_powershell = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
        let vars = [("ProgramFiles", r"C:\Program Files"), ("SystemRoot", r"C:\Windows"), ("ComSpec", r"C:\cmd.exe")];
        let bad = r"D:\gone\shell.exe";
        assert_eq!(launch_of(&system, bad, Os::Windows, &vars, &[pwsh, windows_powershell]).0, pwsh);
        assert_eq!(launch_of(&system, bad, Os::Windows, &vars, &[windows_powershell]).0, windows_powershell);
        assert_eq!(launch_of(&system, bad, Os::Windows, &vars, &[]).0, r"C:\cmd.exe");
        assert!(unusable_of(bad, Os::Windows, &vars, &[]).is_some());

        // A usable program and the empty setting say nothing.
        assert_eq!(unusable_of("/usr/bin/nu", Os::Linux, &env, &["/usr/bin/nu"]), None);
        assert_eq!(unusable_of("", Os::Linux, &env, &[]), None);
        // The value is cut and has no control character in the sentence; Fixed says nothing.
        let long = format!("/{}", "x".repeat(200));
        let said = unusable_of(&long, Os::Linux, &env, &[]).expect("a sentence");
        assert!(said.len() < 200 && said.contains("..."), "{said}");
        assert!(ShellSpec::new("/bin/cat", &[]).launch("/nonexistent", Os::Linux, &|_| None, &|_| false, &|_| ProgramCheck::Missing).unusable.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_that_is_a_directory_or_not_executable_is_named_so_on_disk() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Scratch::new("shell-kinds");
        let root = dir.0.clone();
        std::fs::create_dir_all(&root).unwrap();
        let plain = root.join("plain");
        std::fs::write(&plain, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        let program = root.join("program");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(check_program_on_disk(root.to_str().unwrap()), ProgramCheck::NotAFile);
        assert_eq!(check_program_on_disk(plain.to_str().unwrap()), ProgramCheck::NotExecutable);
        assert_eq!(check_program_on_disk(program.to_str().unwrap()), ProgramCheck::Usable);
        assert_eq!(check_program_on_disk(root.join("nothing").to_str().unwrap()), ProgramCheck::Missing);
        let said = |path: &std::path::Path| {
            let (env, exists, _) = facts(&[("SHELL", "/bin/sh")], &[]);
            ShellSpec::default().launch(path.to_str().unwrap(), Os::Linux, &env, &exists, &check_program_on_disk).unusable
        };
        assert!(said(&root).expect("a directory").contains("is not a file"));
        assert!(said(&plain).expect("no x bit").contains("is not executable"));
        assert!(said(&program).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn an_unusable_shell_still_gives_a_working_pane_a_notice_and_a_warning() {
        let dir = Scratch::new("shell-missing");
        let rec = Arc::new(Recorder::default());
        let core = AppCore::new(dir.0.clone(), rec.clone(), ShellSpec::default());
        dir.attach(&core);
        core.set_setting("shell", json!("/nonexistent/holzwave-no-shell")).expect("accepted: it is only a string");
        let pane = only_pane(&core);

        let spec = core.pane_spec(80, 24, (9, 18));
        assert_ne!(spec.program, "/nonexistent/holzwave-no-shell", "the default starts instead");
        assert!(spec.notice.as_deref().is_some_and(|n| n.contains("holzwave-no-shell")), "{:?}", spec.notice);
        let warned = core.get_settings().warnings.iter().any(|w| w.contains("holzwave-no-shell"));
        assert!(warned, "the settings view carries the sentence: {:?}", core.get_settings().warnings);

        // The only pane of the only window: it starts, it is not removed and the app does not end.
        core.start_pane(pane, 80, 24, 0, 0).expect("the fallback shell starts");
        assert!(
            wait_until(WAIT, || rec.has_text(pane, "was not found")),
            "the pane's first output names the problem: {:?}",
            rec.all()
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(core.pid(pane).is_some(), "the pane runs");
        assert!(rec.pane_exits().is_empty(), "no pane ended: {:?}", rec.all());
        assert!(!rec.all().iter().any(|o| matches!(o, Out::Exit)), "the app did not end: {:?}", rec.all());
        assert_eq!(core.view().panes.len(), 1, "the pane is still in the layout");

        // A usable value or the empty one removes the sentence at once.
        core.set_setting("shell", json!("")).expect("empty is the default");
        assert!(!core.get_settings().warnings.iter().any(|w| w.contains("shell")));
    }

    #[test]
    fn a_wrong_type_an_unknown_key_or_an_unknown_theme_is_an_error_and_changes_nothing() {
        let (core, rec, dir) = core("/bin/cat", &[]);
        let before = core.get_settings();

        for (key, value) in [
            ("font_size", json!("big")),
            ("nonsense", json!(1)),
            ("version", json!(7)),
            ("theme", json!("No Such Theme")),
            ("theme", json!(5)),
            ("keybindings", json!(["x"])),
            ("option_as_alt", json!("yes")),
        ] {
            assert!(core.set_setting(key, value.clone()).is_err(), "{key} = {value} must be refused");
        }

        assert_eq!(core.get_settings(), before);
        assert!(!dir.0.join("settings.json").exists(), "a refused change writes nothing");
        assert!(rec.settings_events().is_empty(), "a refused change tells nobody");
    }

    #[test]
    fn a_setting_that_cannot_be_saved_is_an_error_and_the_memory_keeps_the_old_value() {
        let dir = Scratch::new("unsavable");
        // A directory where the file belongs: the atomic rename onto it cannot succeed.
        std::fs::create_dir(dir.0.join("settings.json")).unwrap();
        let (core, rec) = core_in(&dir, "/bin/cat", &[]);

        let result = core.set_setting("font_size", json!(20));

        assert!(result.is_err(), "{result:?}");
        assert_eq!(core.get_settings().settings.font_size, Settings::default().font_size);
        assert!(rec.settings_events().is_empty());
        let names: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(!names.iter().any(|n| n.ends_with(".tmp")), "no temporary file is left: {names:?}");
    }

    #[cfg(unix)]
    #[test]
    fn option_as_alt_reaches_a_live_pane_so_that_alt_f_is_escape_f() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        // What the keyboard produces for Option+F on macOS with the default layout; plain f elsewhere.
        let produced = if cfg!(target_os = "macos") { "\u{0192}" } else { "f" };
        let alt_f = DomKey { code: "KeyF".into(), key: produced.into(), mods: Mods::ALT, ..DomKey::default() };
        if cfg!(target_os = "macos") {
            core.key(pane, alt_f.clone()).expect("key");
            assert!(wait_until(WAIT, || rec.has_text(pane, "\u{0192}")), "option composes by default (D-12)");
        }

        core.set_setting("option_as_alt", json!(true)).expect("accepted");
        core.key(pane, alt_f).expect("key");

        // The tty echoes ESC as ^[.
        assert!(wait_until(WAIT, || rec.has_text(pane, "^[f")), "no ESC f: {:?}", rec.all().len());
    }

    #[test]
    fn keybindings_are_validated_by_the_resolver_and_the_view_reflects_the_result() {
        let (core, rec, dir) = core("/bin/cat", &[]);
        // The default of the host's table: it is Cmd+T on macOS and something else elsewhere.
        let default_of = |view: &SettingsView, action: &str| {
            view.keybindings.iter().find(|r| r.action == action).and_then(|r| r.chord.clone())
        };
        let new_tab = default_of(&core.get_settings(), "tab.new");
        assert!(new_tab.is_some());

        let view = core
            .set_setting("keybindings", json!({"pane.close": "Cmd+Shift+W", "tab.new": "x", "pane.split_right": ""}))
            .expect("the map has the right type, so it is accepted; the resolver reports what it refuses");

        let row = |action: &str| view.keybindings.iter().find(|r| r.action == action).cloned().expect("row");
        assert_eq!(row("pane.close").chord.as_deref(), Some("Cmd+Shift+W"));
        assert!(!row("pane.close").is_default);
        assert_eq!(row("tab.new").chord, new_tab, "a plain key is refused, the default stays");
        assert_eq!(row("pane.split_right").chord, None, "the empty chord unbinds");
        assert!(view.warnings.iter().any(|w| w.contains("tab.new")), "{:?}", view.warnings);
        assert_eq!(view.settings.keybindings.len(), 3, "the settings keep what the user typed");
        assert!(Settings::load(&dir.0).0.keybindings.contains_key("pane.close"));
        assert_eq!(rec.settings_events().len(), 1);
    }

    #[test]
    fn reset_setting_restores_the_default_of_one_key_and_leaves_the_others() {
        let (core, rec, dir) = core("/bin/cat", &[]);
        core.set_setting("font_size", json!(22)).expect("font size");
        core.set_setting("option_as_alt", json!(true)).expect("option");

        let view = core.reset_setting("font_size").expect("reset");

        assert_eq!(view.settings.font_size, Settings::default().font_size);
        assert!(view.settings.option_as_alt, "the other key is untouched");
        assert_eq!(Settings::load(&dir.0).0.font_size, Settings::default().font_size);
        assert_eq!(rec.settings_events().len(), 3);
        assert!(core.reset_setting("version").is_err());
        assert!(core.reset_setting("nonsense").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn reload_settings_picks_up_hand_edits_only_when_a_file_changed() {
        let dir = Scratch::new("reload");
        let (core, rec) = core_in(&dir, "/bin/cat", &[]);
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || !rec.frames(pane).is_empty()), "no first frame");
        let before = rec.frames(pane).len();

        assert_eq!(core.reload_settings(), core.get_settings());
        assert!(rec.settings_events().is_empty(), "nothing changed, nothing is announced");

        let mine = Theme { name: "Mine".into(), ..Theme::papier() };
        std::fs::write(dir.0.join("settings.json"), r#"{"theme": "Mine", "font_size": 22}"#).unwrap();
        theme_file(&dir, "Mine.json", &mine);
        let view = core.reload_settings();

        assert_eq!(view.settings.font_size, 22);
        assert_eq!(view.theme.name, "Mine");
        assert!(view.themes.iter().any(|t| t.name == "Mine" && !t.builtin));
        assert_eq!(rec.settings_events().len(), 1);
        let papier = packed(mine.terminal.background);
        assert!(wait_until(WAIT, || rec.has_full_frame_with_bg(pane, before, papier)), "the pane did not get the theme");

        core.reload_settings();
        assert_eq!(rec.settings_events().len(), 1, "an unchanged file is not read again");
        core.set_setting("font_size", json!(18)).expect("set");
        assert_eq!(rec.settings_events().len(), 2);
        core.reload_settings();
        assert_eq!(rec.settings_events().len(), 2, "the app's own save is not mistaken for a hand edit");
    }

    #[test]
    fn a_reload_that_finds_a_damaged_file_keeps_the_settings_in_force_and_says_so() {
        let (core, rec, dir) = core("/bin/cat", &[]);
        core.set_setting("font_size", json!(22)).expect("set");
        std::fs::write(dir.0.join("settings.json"), "{ half an edit").unwrap();

        let view = core.reload_settings();

        assert_eq!(view.settings.font_size, 22, "a half-written edit does not reset the running app");
        assert!(view.warnings.iter().any(|w| w.contains("settings.json")), "{:?}", view.warnings);
        assert_eq!(rec.settings_events().len(), 2);
    }

    // ---- session, working directories and the window (plan 23, task 2) -------------------------------------------

    use holzwave_core::session::{Session, WindowState};

    impl Recorder {
        #[cfg(unix)]
        fn cwds(&self, pane: PaneId) -> Vec<String> {
            self.all()
                .into_iter()
                .filter_map(|o| match o {
                    Out::Cwd { pane: p, cwd } if p == pane => Some(cwd),
                    _ => None,
                })
                .collect()
        }
    }

    /// A scratch directory whose `layout.json` holds exactly one pane, and that pane's id.
    fn dir_with_one_pane(tag: &str) -> (Scratch, PaneId) {
        let dir = Scratch::new(tag);
        let (layout, _) = Layout::new();
        std::fs::write(dir.layout_file(), serde_json::to_vec_pretty(&layout).unwrap()).unwrap();
        let pane = layout.all_panes()[0];
        (dir, pane)
    }

    /// An existing directory below the scratch directory, in its canonical spelling (macOS temp is a symlink).
    fn real_dir(dir: &Scratch, name: &str) -> PathBuf {
        let path = dir.0.join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    fn session_on_disk(dir: &Scratch) -> Session {
        let text = std::fs::read_to_string(dir.0.join("session.json")).expect("session.json was written");
        let (session, status) = Session::from_json(&text);
        assert_eq!(status, LoadStatus::Clean, "what the core writes loads clean");
        session
    }

    /// Prints where it starts, then goes to `a` (the first shell to get there) or `b`, announces that directory with
    /// OSC 7 and stays alive. The marker directory makes the choice atomic between two shells.
    #[cfg(unix)]
    fn two_directory_script(marker: &Path, a: &Path, b: &Path) -> String {
        format!(
            "echo \"start:$(pwd)\"; if mkdir '{m}' 2>/dev/null; then d='{a}'; else d='{b}'; fi; cd \"$d\"; \
             printf '\\033]7;file://localhost%s\\007' \"$d\"; sleep 30",
            m = marker.display(),
            a = a.display(),
            b = b.display()
        )
    }

    #[cfg(unix)]
    #[test]
    fn a_pane_starts_in_the_directory_the_session_saved_for_it() {
        let (dir, pane) = dir_with_one_pane("saved-cwd");
        let saved = real_dir(&dir, "project");
        std::fs::write(
            dir.0.join("session.json"),
            format!(r#"{{"version": 1, "cwds": {{"{pane}": {:?}}}}}"#, saved.to_str().unwrap()),
        )
        .unwrap();
        let (core, rec) = core_in(&dir, "/bin/sh", &["-c", "echo \"in:$(pwd)\"; sleep 30"]);

        core.start_pane(pane, 400, 24, 0, 0).expect("starts");

        let needle = format!("in:{}", saved.display());
        assert!(wait_until(WAIT, || rec.has_text(pane, &needle)), "the shell did not start in {saved:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_saved_directory_that_no_longer_exists_does_not_break_the_start() {
        let (dir, pane) = dir_with_one_pane("gone-cwd");
        let gone = real_dir(&dir, "was-here");
        std::fs::write(
            dir.0.join("session.json"),
            format!(r#"{{"version": 1, "cwds": {{"{pane}": {:?}}}}}"#, gone.to_str().unwrap()),
        )
        .unwrap();
        std::fs::remove_dir(&gone).unwrap();
        let (core, rec) = core_in(&dir, "/bin/sh", &["-c", "echo \"in:$(pwd)\"; echo up; sleep 30"]);

        assert_eq!(core.start_dir(pane), None, "a directory that is gone is not used");
        core.start_pane(pane, 80, 24, 0, 0).expect("starts anyway");

        assert!(wait_until(WAIT, || rec.has_text(pane, "up")), "the shell did not start");
        assert!(!rec.has_text(pane, &gone.display().to_string()), "it started in the missing directory");
    }

    #[cfg(unix)]
    #[test]
    fn a_split_starts_in_the_last_known_directory_of_the_pane_that_was_focused() {
        let dir = Scratch::new("split-cwd");
        let (a, b) = (real_dir(&dir, "a"), real_dir(&dir, "b"));
        let script = two_directory_script(&dir.0.join("marker"), &a, &b);
        let (core, rec) = core_in(&dir, "/bin/sh", &["-c", &script]);
        let first = only_pane(&core);
        core.start_pane(first, 400, 24, 0, 0).expect("starts");
        let a_text = a.display().to_string();
        assert!(wait_until(WAIT, || rec.cwds(first).contains(&a_text)), "no cwd event: {:?}", rec.cwds(first));

        let second = core.split(false).focus;
        assert_ne!(second, first);
        core.start_pane(second, 400, 24, 0, 0).expect("starts");

        let needle = format!("start:{a_text}");
        assert!(wait_until(WAIT, || rec.has_text(second, &needle)), "the split did not start in {a_text}");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_tab_starts_in_the_last_known_directory_of_the_pane_that_was_focused() {
        let dir = Scratch::new("tab-cwd");
        let (a, b) = (real_dir(&dir, "a"), real_dir(&dir, "b"));
        let script = two_directory_script(&dir.0.join("marker"), &a, &b);
        let (core, rec) = core_in(&dir, "/bin/sh", &["-c", &script]);
        let first = only_pane(&core);
        core.start_pane(first, 400, 24, 0, 0).expect("starts");
        let a_text = a.display().to_string();
        assert!(wait_until(WAIT, || rec.cwds(first).contains(&a_text)), "no cwd event");

        let tab_pane = core.new_tab().focus;
        assert_ne!(tab_pane, first);
        core.start_pane(tab_pane, 400, 24, 0, 0).expect("starts");

        let needle = format!("start:{a_text}");
        assert!(wait_until(WAIT, || rec.has_text(tab_pane, &needle)), "the new tab did not start in {a_text}");
    }

    #[test]
    fn a_directory_saved_for_the_pane_wins_over_the_one_it_would_inherit() {
        let dir = Scratch::new("saved-wins");
        let (mut layout, _) = Layout::new();
        layout.split_focused(Axis::Row);
        let second = layout.tab().focus;
        std::fs::write(dir.layout_file(), serde_json::to_vec_pretty(&layout).unwrap()).unwrap();
        let saved = real_dir(&dir, "saved");
        let inherited = real_dir(&dir, "inherited");
        std::fs::write(
            dir.0.join("session.json"),
            format!(r#"{{"version": 1, "cwds": {{"{second}": {:?}}}}}"#, saved.to_str().unwrap()),
        )
        .unwrap();
        let (core, _rec) = core_in(&dir, "/bin/cat", &[]);
        lock(&core.starts).insert(second, inherited.display().to_string());

        assert_eq!(core.start_dir(second), Some(saved));
    }

    #[test]
    fn an_inherited_directory_is_used_only_while_it_exists() {
        let (dir, pane) = dir_with_one_pane("inherit-gone");
        let (core, _rec) = core_in(&dir, "/bin/cat", &[]);
        let gone = real_dir(&dir, "soon-gone");
        lock(&core.starts).insert(pane, gone.display().to_string());
        assert_eq!(core.start_dir(pane), Some(gone.clone()));

        std::fs::remove_dir(&gone).unwrap();
        assert_eq!(core.start_dir(pane), None, "a directory that is gone is not a start directory");
        lock(&core.starts).insert(pane, "relative/dir".into());
        assert_eq!(core.start_dir(pane), None, "a relative path is not one either");
    }

    #[cfg(unix)]
    #[test]
    fn the_session_holds_the_directory_of_every_live_pane_after_shutdown() {
        let dir = Scratch::new("session-shutdown");
        let (a, b) = (real_dir(&dir, "a"), real_dir(&dir, "b"));
        let script = two_directory_script(&dir.0.join("marker"), &a, &b);
        let (core, rec) = core_in(&dir, "/bin/sh", &["-c", &script]);
        let first = only_pane(&core);
        let second = core.split(false).focus;
        core.start_pane(first, 80, 24, 0, 0).expect("starts");
        core.start_pane(second, 80, 24, 0, 0).expect("starts");
        assert!(
            wait_until(WAIT, || {
                let (x, y) = (rec.cwds(first), rec.cwds(second));
                !x.is_empty() && !y.is_empty() && x.last() != y.last()
            }),
            "both panes did not announce different directories: {:?} {:?}",
            rec.cwds(first),
            rec.cwds(second)
        );
        let expected_first = rec.cwds(first).last().cloned().unwrap();
        let expected_second = rec.cwds(second).last().cloned().unwrap();

        core.shutdown();

        let session = session_on_disk(&dir);
        assert_eq!(session.cwds.get(&first), Some(&expected_first));
        assert_eq!(session.cwds.get(&second), Some(&expected_second));
        assert_eq!(session.cwds.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn the_session_is_written_two_seconds_after_the_first_change_without_any_flush() {
        let (dir, pane) = dir_with_one_pane("session-debounce");
        let target = real_dir(&dir, "target");
        let script = format!("cd '{d}'; printf '\\033]7;file://localhost%s\\007' '{d}'; sleep 30", d = target.display());
        let (core, rec) = core_in(&dir, "/bin/sh", &["-c", &script]);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        let text = target.display().to_string();
        assert!(wait_until(WAIT, || rec.cwds(pane).contains(&text)), "no cwd event");
        let announced = Instant::now();

        assert!(
            wait_until(Duration::from_secs(6), || dir.0.join("session.json").exists()),
            "the session was not written within six seconds"
        );

        assert!(announced.elapsed() >= Duration::from_millis(1200), "written after {:?}: no debounce", announced.elapsed());
        assert_eq!(session_on_disk(&dir).cwds.get(&pane), Some(&text));
        drop(core);
    }

    #[cfg(unix)]
    #[test]
    fn a_pane_that_reported_nothing_is_asked_for_its_directory_when_the_session_is_written() {
        let (dir, pane) = dir_with_one_pane("session-process");
        let (core, _rec) = core_in(&dir, "/bin/sh", &["-c", "sleep 30"]);
        let start = real_dir(&dir, "start-here");
        lock(&core.starts).insert(pane, start.display().to_string());
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");

        // Before the pane's first report (150 ms of quiet): only the process knows where it is.
        core.shutdown();

        let session = session_on_disk(&dir);
        let got = session.cwds.get(&pane).map(std::fs::canonicalize);
        assert_eq!(got.and_then(Result::ok), Some(start), "{:?}", session.cwds);
    }

    #[cfg(unix)]
    #[test]
    fn closing_a_pane_takes_its_directory_out_of_the_session() {
        let dir = Scratch::new("session-close");
        let (a, b) = (real_dir(&dir, "a"), real_dir(&dir, "b"));
        let script = two_directory_script(&dir.0.join("marker"), &a, &b);
        let (core, rec) = core_in(&dir, "/bin/sh", &["-c", &script]);
        let first = only_pane(&core);
        let second = core.split(false).focus;
        core.start_pane(first, 80, 24, 0, 0).expect("starts");
        core.start_pane(second, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || !rec.cwds(first).is_empty() && !rec.cwds(second).is_empty()), "no cwd events");
        core.flush();
        assert_eq!(session_on_disk(&dir).cwds.len(), 2);

        core.close_pane(second);
        core.flush();

        let session = session_on_disk(&dir);
        assert!(session.cwds.contains_key(&first));
        assert!(!session.cwds.contains_key(&second), "a closed pane must not leave its directory behind");
    }

    #[test]
    fn note_window_is_saved_and_placement_drops_an_off_screen_position() {
        let (core, _rec, dir) = core("/bin/cat", &[]);
        let monitors = [MonitorRect { x: 0, y: 0, width: 1920, height: 1080 }];
        assert_eq!(core.placement(MAIN_WINDOW, &monitors).size, holzwave_core::session::DEFAULT_SIZE, "nothing saved yet");

        core.note_window(MAIN_WINDOW, WindowState { x: 100, y: 80, width: 1200, height: 700, maximized: false });
        let on_screen = core.placement(MAIN_WINDOW, &monitors);
        core.note_window(MAIN_WINDOW, WindowState { x: 5000, y: 80, width: 1200, height: 700, maximized: true });
        let off_screen = core.placement(MAIN_WINDOW, &monitors);
        core.flush();

        assert_eq!(on_screen.position, Some((100, 80)));
        assert_eq!(on_screen.size, (1200, 700));
        assert_eq!(off_screen.position, None, "a position that is on no monitor is dropped");
        assert_eq!(off_screen.size, (1200, 700), "the size is still applied");
        assert!(off_screen.maximized);
        assert_eq!(core.placement(MAIN_WINDOW, &[]).position, None, "with no monitor known the system places the window");
        assert_eq!(session_on_disk(&dir).window.map(|w| (w.x, w.width)), Some((5000, 1200)));
    }

    #[test]
    fn a_window_far_outside_the_range_is_clamped_before_it_is_stored() {
        let (core, _rec, dir) = core("/bin/cat", &[]);

        core.note_window(
            MAIN_WINDOW,
            WindowState { x: i32::MAX, y: i32::MIN, width: 7, height: u32::MAX, maximized: false },
        );
        core.flush();

        let window = session_on_disk(&dir).window.expect("stored");
        assert_eq!((window.x, window.y), (32768, -32768));
        assert_eq!((window.width, window.height), (200, 16384));
    }

    #[cfg(unix)]
    #[test]
    fn a_session_file_that_is_not_json_or_holds_hostile_entries_never_fails_the_start() {
        let (dir, pane) = dir_with_one_pane("session-bad");
        std::fs::write(dir.0.join("session.json"), "{ nope").unwrap();
        let (core, _rec) = core_in(&dir, "/bin/cat", &[]);
        assert_eq!(core.start_dir(pane), None);
        assert_eq!(std::fs::read_to_string(dir.0.join("session.json.bad")).unwrap(), "{ nope");
        drop(core);

        let (dir, pane) = dir_with_one_pane("session-hostile");
        let hostile = format!(
            r#"{{"version": 1, "cwds": {{"{pane}": "../../etc", "x": 5, "9": "relative", "8": "/tmp/\u001b]0;evil\u0007"}}, "window": "no"}}"#
        );
        std::fs::write(dir.0.join("session.json"), &hostile).unwrap();
        let (core, rec) = core_in(&dir, "/bin/cat", &[]);

        assert_eq!(core.start_dir(pane), None, "a relative path is not a directory to start in");
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || !rec.frames(pane).is_empty()), "no frame");
        assert_eq!(std::fs::read_to_string(dir.0.join("session.json.bad")).unwrap(), hostile);
    }

    // ---- links, paths, app info and the pointer (plan 23, task 3) ------------------------------------------------

    static LAUNCHED: Mutex<Vec<Launch>> = Mutex::new(Vec::new());

    /// The platform the tests run on, for those that use the real file system.
    const OS: &str = std::env::consts::OS;

    /// Stands in for the opener: records instead of starting a process.
    fn record_launch(launch: &Launch) -> Result<(), String> {
        lock(&LAUNCHED).push(launch.clone());
        Ok(())
    }

    fn launches_with(arg: &str) -> Vec<Launch> {
        lock(&LAUNCHED).iter().filter(|l| l.args.iter().any(|a| a == arg)).cloned().collect()
    }

    /// A core whose opener only records.
    fn recording_core() -> (Arc<AppCore>, Arc<Recorder>, Scratch) {
        let (core, rec, dir) = core("/bin/cat", &[]);
        *lock(&core.launcher) = record_launch;
        (core, rec, dir)
    }

    #[test]
    fn open_url_opens_http_https_and_mailto_and_nothing_else() {
        let (core, _rec, _dir) = recording_core();
        let good = [
            "https://example.org/a?b=c#d-open-1",
            "http://localhost:8080/open-2",
            "mailto:holz@example.org?subject=open-3",
            "HTTPS://EXAMPLE.ORG/open-4",
        ];
        for url in good {
            assert_eq!(core.open_url(url), Ok(()), "{url}");
            assert_eq!(launches_with(url).len(), 1, "{url} is opened exactly once");
        }

        let long_ok = format!("https://example.org/{}", "a".repeat(MAX_URL_BYTES - "https://example.org/".len()));
        assert_eq!(long_ok.len(), MAX_URL_BYTES);
        assert_eq!(core.open_url(&long_ok), Ok(()), "2048 bytes are fine");

        let too_long = format!("https://example.org/{}", "b".repeat(MAX_URL_BYTES));
        let bad = [
            "file:///etc/passwd-open-5",
            "javascript:alert(1)-open-6",
            "ftp://example.org/open-7",
            "data:text/html,open-8",
            "vscode://open-9",
            "x-man-page://open-10",
            "http:open-11",
            "https://",
            "https:///etc/open-12",
            "mailto:",
            "",
            "   https://example.org/open-13",
            "https://example.org/open 14",
            "https://exam\nple.org/open-15",
            "https://example.org/\u{7f}open-16",
            "https://example.org/\u{9b}open-17",
            "https://example.org/\u{0}open-18",
            "not a url open-19",
            too_long.as_str(),
        ];
        for url in bad {
            assert!(core.open_url(url).is_err(), "{:?} must be refused", url.chars().take(40).collect::<String>());
            assert!(launches_with(url).is_empty(), "an opener was started for {:?}", url.chars().take(40).collect::<String>());
        }
    }

    /// A process environment that has nothing in it.
    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn the_opener_is_one_fixed_program_with_the_link_as_a_single_argument() {
        let url = "https://example.org/a?b=c&d=e";

        assert_eq!(open_launch("macos", url, &no_env), Ok(Launch::plain("/usr/bin/open", vec![url.to_owned()])));
        assert_eq!(open_launch("linux", url, &no_env), Ok(Launch::plain("xdg-open", vec![url.to_owned()])));
        assert!(open_launch("macos", "file:///etc/passwd", &no_env).is_err());
        assert!(open_launch("plan9", url, &no_env).is_err(), "a platform nobody wrote an opener for");
    }

    #[test]
    fn windows_launch_of_a_link_is_rundll32_with_the_link_unchanged_as_the_second_argument() {
        let url = "https://example.org/a?b=1&c=2^d";

        let launch = open_launch("windows", url, &no_env).expect("a link opens on Windows");

        assert_eq!(launch.program, r"C:\Windows\System32\rundll32.exe", r"SystemRoot defaults to C:\Windows");
        assert_eq!(launch.args, ["url.dll,FileProtocolHandler", url], "exactly two arguments, the link untouched");
        assert!(launch.raw_args.is_empty());
        let root = |name: &str| (name == "SystemRoot").then(|| r"D:\WinNT\".to_owned());
        assert_eq!(open_launch("windows", url, &root).unwrap().program, r"D:\WinNT\System32\rundll32.exe");
    }

    #[test]
    fn windows_launch_of_a_link_with_an_ampersand_or_a_caret_never_goes_through_a_shell() {
        // `&` and `^` are legal in a link and are metacharacters of cmd: no cmd, no command string, one argument.
        for url in ["https://example.org/?a=1&b=2", "https://example.org/a^b", "https://example.org/?a=1&calc.exe"] {
            let launch = open_launch("windows", url, &no_env).expect(url);
            assert!(launch.program.ends_with(r"\rundll32.exe"), "{launch:?}");
            assert!(!launch.program.to_ascii_lowercase().contains("cmd"), "{launch:?}");
            assert_eq!(launch.args.len(), 2, "{launch:?}");
            assert_eq!(launch.args[1], url, "the link is one argument");
        }
    }

    #[test]
    fn windows_launch_of_a_link_refuses_a_double_quote_and_what_every_platform_refuses() {
        assert!(open_launch("windows", "https://example.org/a\"b", &no_env).is_err(), "a double quote");
        assert!(open_launch("macos", "https://example.org/a\"b", &no_env).is_ok(), "only Windows rebuilds a command line");
        assert!(open_launch("windows", "file:///C:/Windows/System32/calc.exe", &no_env).is_err(), "a scheme");
        assert!(open_launch("windows", "https://example.org/a b", &no_env).is_err(), "a blank");
        assert!(open_launch("windows", "https://example.org/a\u{1b}b", &no_env).is_err(), "a control character");
        let too_long = format!("https://example.org/{}", "b".repeat(MAX_URL_BYTES));
        assert!(open_launch("windows", &too_long, &no_env).is_err(), "over {MAX_URL_BYTES} bytes");
    }

    #[test]
    fn windows_launch_ignores_a_system_root_that_is_not_a_drive_path() {
        let url = "https://example.org/";
        for hostile in [r"\\evil\share", "//evil/share", "relative", "", r"\\?\C:\Windows"] {
            let env = |name: &str| (name == "SystemRoot").then(|| hostile.to_owned());
            let program = open_launch("windows", url, &env).unwrap().program;
            assert_eq!(program, r"C:\Windows\System32\rundll32.exe", "SystemRoot {hostile:?}");
        }
    }

    #[test]
    fn reveal_resolves_absolute_home_and_relative_paths_and_requires_that_they_exist() {
        let dir = Scratch::new("reveal-resolve");
        let home = real_dir(&dir, "home");
        let base = real_dir(&dir, "work");
        std::fs::create_dir_all(home.join("docs")).unwrap();
        std::fs::write(home.join("docs").join("note.txt"), "x").unwrap();
        std::fs::write(base.join("main.rs"), "x").unwrap();
        std::fs::write(base.join("-rf"), "x").unwrap();
        let file = base.join("main.rs");
        let text = |p: &Path| p.to_str().unwrap().to_owned();

        assert_eq!(resolve_reveal(OS, &text(&file), None, None), Ok(file.clone()), "absolute");
        assert_eq!(
            resolve_reveal(OS, "~/docs/note.txt", None, Some(&home)),
            Ok(home.join("docs").join("note.txt")),
            "home"
        );
        assert_eq!(resolve_reveal(OS, "main.rs", Some(&text(&base)), None), Ok(file.clone()), "relative to the base");
        assert_eq!(
            resolve_reveal(OS, "-rf", Some(&text(&base)), None),
            Ok(base.join("-rf")),
            "a name that looks like an option is a path below the base, so it is absolute when it is revealed"
        );

        assert!(resolve_reveal(OS, &text(&base.join("missing.rs")), None, None).is_err(), "missing");
        assert!(resolve_reveal(OS, "main.rs", None, None).is_err(), "relative without a base");
        assert!(resolve_reveal(OS, "main.rs", Some("relative/base"), None).is_err(), "a base that is not absolute");
        assert!(resolve_reveal(OS, "~/docs/note.txt", None, None).is_err(), "no home directory known");
        assert!(resolve_reveal(OS, "", Some(&text(&base)), None).is_err());
        assert!(resolve_reveal(OS, "main.rs\u{0}", Some(&text(&base)), None).is_err(), "control character");
        assert!(resolve_reveal(OS, "main\n.rs", Some(&text(&base)), None).is_err(), "control character");
        assert!(resolve_reveal(OS, &"a".repeat(MAX_PATH_BYTES + 1), Some(&text(&base)), None).is_err(), "too long");
    }

    #[test]
    fn reveal_shows_a_file_in_the_file_manager_and_never_runs_it() {
        let dir = Scratch::new("reveal-launch");
        let root = real_dir(&dir, "root");
        let file = root.join("tool.sh");
        std::fs::write(&file, "x").unwrap();
        let target = file.to_str().unwrap().to_owned();
        let folder = root.to_str().unwrap().to_owned();

        assert_eq!(
            reveal_launch("macos", &file, false, &no_env),
            Ok(Launch::plain("/usr/bin/open", vec!["-R".into(), target.clone()]))
        );
        // Linux has no reveal: the folder that holds a file is opened, a folder is opened as it is.
        assert_eq!(reveal_launch("linux", &file, false, &no_env), Ok(Launch::plain("xdg-open", vec![folder.clone()])));
        assert_eq!(reveal_launch("linux", &root, true, &no_env), Ok(Launch::plain("xdg-open", vec![folder])));
        assert!(reveal_launch("plan9", &file, false, &no_env).is_err(), "a platform nobody wrote a reveal for");
        assert!(reveal_launch("macos", Path::new("relative/file"), false, &no_env).is_err(), "never a relative path");
    }

    #[test]
    fn windows_reveal_of_a_file_is_explorer_with_one_raw_select_argument() {
        let launch = reveal_launch("windows", Path::new(r"C:\a b\c.txt"), false, &no_env).expect("a drive path");

        assert_eq!(launch.program, r"C:\Windows\explorer.exe");
        assert!(launch.args.is_empty(), "no normal argument: the quoting is in the raw one");
        assert_eq!(launch.raw_args, [r#"/select,"C:\a b\c.txt""#], "a space in a path stays inside the quotes");
        let root = |name: &str| (name == "SystemRoot").then(|| r"D:\WinNT".to_owned());
        let launch = reveal_launch("windows", Path::new("C:/forward/slashes.txt"), false, &root).unwrap();
        assert_eq!(launch.program, r"D:\WinNT\explorer.exe");
        assert_eq!(launch.raw_args, [r#"/select,"C:\forward\slashes.txt""#], "forward slashes become backslashes");
    }

    #[test]
    fn windows_reveal_of_a_comma_stays_inside_the_quotes_of_the_one_raw_argument() {
        // `/select,` ends at its comma; a comma in the path must not start another argument.
        let launch = reveal_launch("windows", Path::new(r"C:\a,b\c,d.txt"), false, &no_env).unwrap();
        assert_eq!(launch.raw_args, [r#"/select,"C:\a,b\c,d.txt""#]);
        assert!(launch.args.is_empty());
    }

    #[test]
    fn windows_reveal_of_a_folder_passes_the_folder_as_a_normal_argument() {
        let launch = reveal_launch("windows", Path::new(r"C:\a b\folder"), true, &no_env).unwrap();
        assert_eq!(launch, Launch::plain(r"C:\Windows\explorer.exe", vec![r"C:\a b\folder".into()]));
        let slashes = reveal_launch("windows", Path::new("C:/a b/folder/"), true, &no_env).unwrap();
        assert_eq!(slashes.args, [r"C:\a b\folder"], "backslashes, and no separator that would escape the quote");
        let drive = reveal_launch("windows", Path::new(r"C:\"), true, &no_env).unwrap();
        assert_eq!(drive.args, [r"C:\"], "the root of a drive keeps its separator");
        assert!(drive.raw_args.is_empty());
    }

    #[test]
    fn windows_reveal_refuses_a_double_quote_in_the_path_before_any_program_is_built() {
        // A quote would end the quoting of the raw argument and add arguments of its own (T-2.2-11).
        for path in [r#"C:\a"b\c.txt"#, r#"C:\a\c.txt" /x"#, r#"C:\a\"," /root,"\\evil\share"#] {
            assert!(reveal_launch("windows", Path::new(path), false, &no_env).is_err(), "{path}");
            assert!(reveal_launch("windows", Path::new(path), true, &no_env).is_err(), "{path} as a folder");
        }
        assert!(
            reveal_launch("linux", Path::new("/tmp/a\"b"), true, &no_env).is_ok(),
            "a quote is an ordinary character of a path elsewhere"
        );
    }

    #[test]
    fn windows_reveal_of_a_unc_or_device_path_is_refused_in_every_spelling() {
        for path in [r"\\server\share\x", "//server/share/x", r"\\?\C:\x", r"\\.\C:\x", r"\/server\share", r"/\server/share"] {
            assert!(reveal_launch("windows", Path::new(path), false, &no_env).is_err(), "{path}");
        }
        assert!(reveal_launch("windows", Path::new(r"C:\ordinary\x"), false, &no_env).is_ok());
    }

    #[test]
    fn reveal_of_a_unc_path_on_windows_is_refused_before_the_file_system_is_asked() {
        // On a Windows host even `symlink_metadata` of `\\server\share` would make Windows authenticate to `server`
        // (T-2.2-10). The refusal says why; a path that reached the file system says "no such file" instead.
        let refused = |result: Result<PathBuf, String>| result.expect_err("refused").contains("network or device");
        for path in [r"\\server\share\x", "//server/share/x", r"\\?\C:\x", r"\\.\PhysicalDrive0", r"\/server\share", "//"] {
            assert!(refused(resolve_reveal("windows", path, None, None)), "{path}");
            assert!(refused(resolve_reveal("windows", path, Some(r"C:\work"), None)), "{path} with a base");
        }
        assert!(refused(resolve_reveal("windows", "x.txt", Some(r"\\server\share"), None)), "a network base");
        assert!(refused(resolve_reveal("windows", "x.txt", Some("//server/share"), None)), "a network base, slashes");
        let home = Path::new(r"\\server\home\me");
        assert_eq!(
            resolve_reveal("windows", "~/x.txt", None, Some(home)),
            Err("the home directory is not known".to_owned()),
            "a network home directory is no home directory: only a drive path is one"
        );
    }

    #[test]
    fn reveal_of_an_ordinary_drive_path_on_windows_does_reach_the_file_system() {
        // The counterpart of the refusals above: the resolver is not refusing everything on Windows.
        let local = resolve_reveal("windows", r"C:\no\such\folder-reveal-1", None, None).expect_err("not there");
        assert_eq!(local, "there is no such file or folder");
        let relative = resolve_reveal("windows", "x.txt", Some(r"C:\no\such\base-reveal-2"), None).expect_err("not there");
        assert_eq!(relative, "there is no such file or folder");
        let home = resolve_reveal("windows", "~/x.txt", None, Some(Path::new(r"C:\Users\nobody-reveal-3")));
        assert_eq!(home, Err("there is no such file or folder".to_owned()), "~/ below the profile directory");
        assert_eq!(
            resolve_reveal("windows", "x.txt", Some("relative"), None),
            Err("the working directory is not an absolute path".to_owned())
        );
    }

    #[test]
    fn reveal_of_a_path_with_a_double_quote_on_windows_is_refused_by_the_resolver() {
        let refused = |result: Result<PathBuf, String>| result.expect_err("refused").contains("double quote");
        assert!(refused(resolve_reveal("windows", r#"C:\a"b.txt"#, None, None)));
        assert!(refused(resolve_reveal("windows", "a\"b.txt", Some(r"C:\work"), None)), "a quote in a relative path");
        assert!(refused(resolve_reveal("windows", "x.txt", Some(r#"C:\wo"rk"#), None)), "a quote in the base");
    }

    #[test]
    fn reveal_of_the_same_text_is_an_ordinary_path_on_linux_and_macos() {
        let dir = Scratch::new("reveal-ordinary");
        let root = real_dir(&dir, "root");
        let text = root.to_str().unwrap();
        let quoted = root.join("a\"b");
        std::fs::write(&quoted, "x").unwrap();

        // Two leading slashes are the root on Unix: `//tmp/x` is `/tmp/x`.
        let doubled = format!("/{text}");
        for os in ["linux", "macos"] {
            assert_eq!(resolve_reveal(os, &doubled, None, None), Ok(PathBuf::from(&doubled)), "{os}");
            assert_eq!(resolve_reveal(os, "a\"b", Some(text), None), Ok(quoted.clone()), "{os}");
        }
    }

    #[test]
    fn a_home_path_is_resolved_from_home_and_on_windows_from_userprofile() {
        let env = |name: &str| match name {
            "HOME" => Some("/home/me".to_owned()),
            "USERPROFILE" => Some(r"C:\Users\me".to_owned()),
            _ => None,
        };
        assert_eq!(home_dir("linux", &env), Some(PathBuf::from("/home/me")));
        assert_eq!(home_dir("macos", &env), Some(PathBuf::from("/home/me")));
        assert_eq!(home_dir("windows", &env), Some(PathBuf::from(r"C:\Users\me")), "USERPROFILE, not HOME");
        assert_eq!(home_dir("windows", &|n: &str| (n == "HOME").then(|| "/home/me".to_owned())), None);
        assert_eq!(home_dir("linux", &|n: &str| (n == "HOME").then(String::new)), None, "empty is no value");
        assert_eq!(home_dir("linux", &no_env), None);
    }

    #[cfg(unix)]
    #[test]
    fn reveal_path_of_an_executable_starts_the_opener_not_the_file() {
        use std::os::unix::fs::PermissionsExt;
        let (core, _rec, dir) = recording_core();
        let root = real_dir(&dir, "exec");
        let marker = root.join("ran");
        let script = root.join("runme.sh");
        std::fs::write(&script, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let target = script.to_str().unwrap().to_owned();

        assert_eq!(core.reveal_path(&target, None), Ok(()));

        // macOS selects the file; Linux has no reveal and opens the folder that holds it.
        let shown = if cfg!(target_os = "macos") { target.clone() } else { root.to_str().unwrap().to_owned() };
        let launches = launches_with(&shown);
        assert_eq!(launches.len(), 1, "the recorder saw one launch");
        assert_ne!(launches[0].program, target, "the file itself is never the program");
        assert!(matches!(launches[0].program.as_str(), "/usr/bin/open" | "xdg-open"), "{:?}", launches[0]);
        std::thread::sleep(Duration::from_millis(150));
        assert!(!marker.exists(), "the file was executed");
    }

    #[test]
    fn reveal_path_with_a_missing_path_is_an_error_and_starts_no_opener() {
        let (core, _rec, dir) = recording_core();
        let missing = dir.0.join("nothing-here-reveal-1");
        let missing_text = missing.to_str().unwrap().to_owned();

        assert!(core.reveal_path(&missing_text, None).is_err());
        assert!(core.reveal_path("nothing-here-reveal-2", Some(dir.0.to_str().unwrap())).is_err());

        assert!(launches_with(&missing_text).is_empty());
        assert!(launches_with(dir.0.join("nothing-here-reveal-2").to_str().unwrap()).is_empty());
    }

    #[test]
    fn app_info_has_the_crate_version_the_os_name_and_the_host_name() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);

        let info = core.app_info();

        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.os, std::env::consts::OS);
        assert!(["macos", "windows", "linux"].contains(&info.os.as_str()), "{}", info.os);
        assert!(!info.hostname.is_empty(), "the host name could not be read");
        assert!(!info.hostname.chars().any(char::is_control));
        assert!(info.hostname.chars().count() <= 255);
    }

    #[test]
    fn a_host_name_is_the_first_line_without_control_characters_and_bounded() {
        assert_eq!(clean_hostname(b"holz-mac.local\n"), "holz-mac.local");
        assert_eq!(clean_hostname(b"  holz-mac  \nsecond line\n"), "holz-mac");
        assert_eq!(clean_hostname(b"ho\x1b]0;evil\x07lz"), "ho]0;evillz");
        assert_eq!(clean_hostname(&[0xff, 0xfe]), "", "not UTF-8 is no name");
        assert_eq!(clean_hostname(b""), "");
        assert_eq!(clean_hostname("a".repeat(1000).as_bytes()).chars().count(), 255);
    }

    #[test]
    fn hostname_on_windows_is_the_computer_name_of_the_environment() {
        let env = |name: &str| (name == "COMPUTERNAME").then(|| "HOLZ-PC\n".to_owned());
        let no_file = |_: &str| -> Option<Vec<u8>> { panic!("Windows reads no file") };
        assert_eq!(hostname_from("windows", &env, &no_file), "HOLZ-PC");
        assert_eq!(hostname_from("windows", &no_env, &no_file), "", "no variable is no name");
    }

    #[test]
    fn hostname_on_linux_reads_etc_hostname_then_the_kernel_file() {
        let files = |present: &'static [(&'static str, &'static str)]| {
            move |path: &str| present.iter().find(|(p, _)| *p == path).map(|(_, text)| text.as_bytes().to_vec())
        };
        let both = files(&[("/etc/hostname", "holz-box\n"), ("/proc/sys/kernel/hostname", "kernel-name\n")]);
        assert_eq!(hostname_from("linux", &no_env, &both), "holz-box", "the file of the system comes first");

        let kernel_only = files(&[("/proc/sys/kernel/hostname", "kernel-name\n")]);
        assert_eq!(hostname_from("linux", &no_env, &kernel_only), "kernel-name", "a missing file falls back");

        let empty_etc = files(&[("/etc/hostname", "\n  \n"), ("/proc/sys/kernel/hostname", "kernel-name\n")]);
        assert_eq!(hostname_from("linux", &no_env, &empty_etc), "kernel-name", "an empty file falls back");

        let unreadable = files(&[]);
        assert_eq!(hostname_from("linux", &no_env, &unreadable), "", "nothing readable is no name");
        let binary = |_: &str| Some(vec![0xff, 0xfe]);
        assert_eq!(hostname_from("linux", &no_env, &binary), "", "text that is not UTF-8 is no name");
    }

    #[test]
    fn hostname_of_macos_and_of_an_unknown_platform_is_not_read_from_a_file() {
        // macOS asks `/bin/hostname`, which `read_hostname` runs; the pure part has nothing to say for it.
        let env = |_: &str| Some("not-used".to_owned());
        let file = |_: &str| Some(b"not-used".to_vec());
        assert_eq!(hostname_from("macos", &env, &file), "");
        assert_eq!(hostname_from("plan9", &env, &file), "");
    }

    #[test]
    fn programs_are_started_by_absolute_path_except_the_one_documented_linux_opener() {
        // T-02-89: nothing of the production code looks a program up through PATH, so that PATH cannot redirect it.
        let source = include_str!("state.rs");
        let production = source.split("#[cfg(test)]\nmod tests").next().expect("production part");
        let mut programs = Vec::new();
        for marker in ["Launch::plain(\"", "Command::new(\""] {
            for part in production.split(marker).skip(1) {
                programs.push(part.split('"').next().unwrap_or_default().to_owned());
            }
        }
        assert!(programs.len() >= 3, "the scanner found only {programs:?}");
        for program in programs {
            assert!(
                program.starts_with('/') || program == "xdg-open",
                "{program:?} is looked up through PATH"
            );
        }
        // The Windows programs are built at run time from the system directory, so they are checked as built: a
        // drive path to a program of that directory, whatever the environment says.
        let hostile = |_: &str| Some(r"\\evil\share".to_owned());
        let windows = [
            open_launch("windows", "https://example.org/", &no_env),
            open_launch("windows", "https://example.org/", &hostile),
            reveal_launch("windows", Path::new(r"C:\a\b.txt"), false, &no_env),
            reveal_launch("windows", Path::new(r"C:\a"), true, &hostile),
        ];
        for launch in windows {
            let program = launch.expect("a Windows launch").program;
            assert!(is_drive_path(&program) && program.starts_with(r"C:\Windows\"), "{program:?}");
            assert!(program.ends_with(".exe"), "{program:?}");
        }
    }

    #[test]
    fn pointer_values_are_finite_within_bounds_and_wheel_rows_and_modifiers_are_clamped() {
        assert_eq!(clamp_pointer(f64::NAN), 0.0);
        assert_eq!(clamp_pointer(f64::INFINITY), 0.0);
        assert_eq!(clamp_pointer(f64::NEG_INFINITY), 0.0);
        assert_eq!(clamp_pointer(-5.0), 0.0);
        assert_eq!(clamp_pointer(1e12), 100_000.0);
        assert_eq!(clamp_pointer(12.5), 12.5);
        assert_eq!(clamp_rows(i32::MIN), -100);
        assert_eq!(clamp_rows(i32::MAX), 100);
        assert_eq!(clamp_rows(-3), -3);
        assert_eq!(pointer_mods(0xFFFF_FFF3), Mods::from_bits(3));
        assert_eq!(pointer_mods(8), Mods::SUPER);
    }

    #[cfg(unix)]
    #[test]
    fn mouse_and_wheel_reach_a_program_that_asked_for_the_mouse() {
        let (core, rec, _dir) = sh("printf '\\033[?1000h\\033[?1006h'; echo ready; exec /bin/cat");
        let pane = only_pane(&core);
        core.start_pane(pane, 40, 10, 8, 16).expect("starts");
        assert!(wait_until(WAIT, || rec.has_text(pane, "ready")), "the program did not start");

        // Pixel (36, 40) on a grid of 8 x 16 cells is column 4, row 2: SGR reports are one based.
        core.mouse(pane, MouseAction::Press, MouseButton::Left, 36.0, 40.0, 0);
        assert!(wait_until(WAIT, || rec.has_text(pane, "^[[<0;5;3M")), "no press report: {:?}", rec.all().len());
        core.mouse(pane, MouseAction::Release, MouseButton::Left, 36.0, 40.0, 0);
        assert!(wait_until(WAIT, || rec.has_text(pane, "^[[<0;5;3m")), "no release report");

        core.wheel(pane, -2, 36.0, 40.0, 0);
        assert!(wait_until(WAIT, || rec.has_text(pane, "^[[<64;5;3M")), "no wheel report");
    }

    #[cfg(unix)]
    #[test]
    fn mouse_and_wheel_for_an_unknown_pane_or_with_hostile_numbers_change_nothing_and_do_not_panic() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        core.start_pane(pane, 40, 10, 8, 16).expect("starts");

        core.mouse(99_999, MouseAction::Press, MouseButton::Left, 1.0, 1.0, 0);
        core.wheel(99_999, 5, 1.0, 1.0, 0);
        core.mouse(pane, MouseAction::Motion, MouseButton::None, f64::NAN, f64::INFINITY, u32::MAX);
        core.wheel(pane, i32::MIN, f64::NEG_INFINITY, 1e300, u32::MAX);

        std::thread::sleep(Duration::from_millis(100));
        assert!(rec.pane_exits().is_empty());
        assert!(core.pid(pane).is_some(), "the pane is alive");
    }

    // ---- several windows: addressing, close rules, restore (plan 02.1-04) ---------------------------------------
    //
    // Everything here runs without a window: the sink is the recorder, which notes the target of every event and the
    // windows it was asked to destroy. What needs a real second window is check A12 of the owner.

    impl Recorder {
        /// The frames of `pane` with the window each went to.
        #[cfg(unix)]
        fn frame_targets(&self, pane: PaneId) -> Vec<Target> {
            self.sent()
                .into_iter()
                .filter_map(|(target, out)| match out {
                    Out::Frame(f) if f.pane == pane => Some(target),
                    _ => None,
                })
                .collect()
        }

        /// The targets of every event of the kind `name` (see `Out::name`) from `skip` on.
        fn targets_of(&self, name: &str, skip: usize) -> Vec<Target> {
            self.sent().into_iter().skip(skip).filter(|(_, out)| out.name() == name).map(|(target, _)| target).collect()
        }
    }

    /// Give `pane` a window of its own as the core does for a tear-off, without anything that opens a window.
    fn raw_tear_off(core: &AppCore, pane: PaneId) -> String {
        let mut set = lock(&core.windows);
        let torn = set.tear_off_pane(pane).expect("the pane is torn off");
        core.sync_routes(&set);
        torn.label
    }

    /// A core whose two panes run `script` and sit in two windows: `main` and another. `(core, rec, dir, first,
    /// second, label)`, `first` in `main` and `second` in the window `label`.
    fn two_windows(script: &str) -> (Arc<AppCore>, Arc<Recorder>, Scratch, PaneId, PaneId, String) {
        let (core, rec, dir) = sh(script);
        let first = only_pane(&core);
        let second = core.split(false).focus;
        let label = raw_tear_off(&core, second);
        assert_ne!(label, MAIN_WINDOW);
        (core, rec, dir, first, second, label)
    }

    fn view_panes(view: &View) -> Vec<PaneId> {
        let mut panes: Vec<PaneId> = view.panes.iter().map(|(p, _)| *p).collect();
        panes.sort_unstable();
        panes
    }

    #[cfg(unix)]
    #[test]
    fn a_frame_goes_to_the_window_that_has_the_pane_and_to_no_other() {
        let (core, rec, _dir, first, second, label) = two_windows("printf hello; sleep 30");
        core.start_pane(first, 80, 24, 0, 0).expect("first starts");
        core.start_pane(second, 80, 24, 0, 0).expect("second starts");

        assert!(wait_until(WAIT, || rec.has_text(first, "hello") && rec.has_text(second, "hello")), "no frames");

        let (main_frames, other_frames) = (rec.frame_targets(first), rec.frame_targets(second));
        assert!(!main_frames.is_empty() && !other_frames.is_empty());
        assert!(main_frames.iter().all(|t| *t == Target::window(MAIN_WINDOW)), "{main_frames:?}");
        assert!(other_frames.iter().all(|t| *t == Target::window(&label)), "{other_frames:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_title_and_a_directory_go_to_the_window_that_has_the_pane_and_the_end_of_its_shell_too() {
        // The directory is polled from the process, so the shell stays a while before it ends.
        let script = "printf '\\033]2;build done\\007\\033]7;file://localhost/tmp\\007'; sleep 1.5; exit 3";
        let (core, rec, _dir, _first, second, label) = two_windows(script);
        // A second pane keeps the window open when the shell of `second` ends.
        core.split_in(&label, true).unwrap();
        core.start_pane(second, 80, 24, 0, 0).expect("starts");

        assert!(wait_until(WAIT, || !rec.pane_exits().is_empty()), "no PaneExit: {:?}", rec.all());

        let ours = Target::window(&label);
        let events: Vec<(Target, Out)> = rec.sent().into_iter().filter(|(_, o)| !matches!(o, Out::Frame(_))).collect();
        let title = events.iter().find(|(_, o)| matches!(o, Out::Title { .. })).expect("a title");
        let cwd = events.iter().find(|(_, o)| matches!(o, Out::Cwd { .. })).expect("a directory");
        let exit = events.iter().find(|(_, o)| matches!(o, Out::PaneExit { .. })).expect("an exit");
        assert_eq!((&title.0, &cwd.0, &exit.0), (&ours, &ours, &ours));
        assert_eq!(exit.1, Out::PaneExit { pane: second, code: Some(3) });
    }

    #[cfg(unix)]
    #[test]
    fn the_layout_event_of_a_shell_that_ends_goes_to_its_window_only() {
        // Window `main` has two panes, the other window one; a pane of `main` ends.
        let (core, rec, _dir) = sh("exit 0");
        let a = only_pane(&core);
        let b = core.split(false).focus;
        let c = core.split(false).focus;
        let label = raw_tear_off(&core, c);

        core.start_pane(a, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || !rec.layouts().is_empty()), "no layout event: {:?}", rec.all());

        let layouts = rec.sent().into_iter().filter(|(_, o)| matches!(o, Out::Layout(_))).collect::<Vec<_>>();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].0, Target::window(MAIN_WINDOW), "not to {label}");
        assert_eq!(rec.pane_exits(), vec![(a, Some(0))]);
        assert_eq!(rec.targets_of("pane-exit", 0), vec![Target::window(MAIN_WINDOW)]);
        assert_eq!(core.view().panes.iter().map(|p| p.0).collect::<Vec<_>>(), vec![b]);
    }

    #[test]
    fn settings_and_the_end_of_the_app_go_to_every_window() {
        let (core, rec, _dir, _first, _second, _label) = two_windows("sleep 30");

        core.set_setting("font_size", json!(15)).expect("set");
        core.begin_exit();

        assert_eq!(rec.targets_of("settings", 0), vec![Target::All]);
        assert_eq!(rec.targets_of("exit", 0), vec![Target::All]);
    }

    #[test]
    fn a_layout_command_acts_on_the_window_that_called_and_answers_with_its_view() {
        let (core, _rec, _dir, first, second, label) = two_windows("sleep 30");
        let main_before = core.view_in(MAIN_WINDOW).unwrap();

        let view = core.split_in(&label, true).expect("the other window exists");

        assert_eq!(view.panes.len(), 2, "the other window has two panes now");
        assert!(view_panes(&view).contains(&second) && !view_panes(&view).contains(&first));
        let main_after = core.view_in(MAIN_WINDOW).unwrap();
        assert_eq!(without_rev(&main_after), without_rev(&main_before), "`main` did not change");

        let renamed = core.rename_tab_in(&label, 0, "logs").unwrap();
        assert_eq!(renamed.tabs[0].title, "logs");
        assert_eq!(core.view_in(MAIN_WINDOW).unwrap().tabs[0].title, "shell");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_pane_of_a_window_gets_a_route_to_that_window() {
        let (core, rec, _dir, _first, _second, label) = two_windows("printf made; sleep 30");

        let made = core.split_in(&label, false).unwrap().focus;
        core.start_pane(made, 80, 24, 0, 0).expect("starts");

        assert!(wait_until(WAIT, || rec.has_text(made, "made")), "no frame");
        assert!(rec.frame_targets(made).iter().all(|t| *t == Target::window(&label)));
    }

    #[test]
    fn a_label_that_is_no_window_is_an_error_and_changes_nothing() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let before = core.view();

        for label in ["window-99", "", "main ", "MAIN", "../main"] {
            assert_eq!(core.view_in(label), Err("no such window".to_owned()), "{label:?}");
            assert!(core.split_in(label, false).is_err());
            assert!(core.new_tab_in(label).is_err());
            assert!(core.new_workspace_in(label, "x").is_err());
            assert!(core.focus_pane_in(label, before.focus).is_err());
            assert!(core.cycle_focus_in(label, 1).is_err());
            assert!(core.select_tab_in(label, 0).is_err());
            assert!(core.select_workspace_in(label, 0).is_err());
            assert!(core.rename_pane_in(label, before.focus, "x").is_err());
            assert!(core.rename_tab_in(label, 0, "x").is_err());
            assert!(core.rename_workspace_in(label, 0, "x").is_err());
            assert!(core.move_pane_in(label, before.focus, before.focus, Edge::Left).is_err());
            assert!(core.move_tab_in(label, 0, 0).is_err());
            assert!(core.move_tab_to_workspace_in(label, 0, 0).is_err());
            assert!(core.set_ratio_in(label, 1, 0.5).is_err());
            assert!(core.close_pane_in(label, before.focus).is_err());
        }

        assert_eq!(without_rev(&core.view()), without_rev(&before));
        assert!(rec.all().is_empty(), "nothing was sent: {:?}", rec.all());
        assert_eq!(rec.exit_count(), 0);
    }

    #[test]
    fn a_window_cannot_close_a_pane_of_another_window() {
        let (core, rec, _dir, first, second, label) = two_windows("sleep 30");

        let answer = core.close_pane_in(MAIN_WINDOW, second).expect("main exists");

        assert_eq!(view_panes(&answer), vec![first], "`main` still has its own pane");
        assert!(core.view_in(&label).is_ok(), "the other window is untouched");
        assert_eq!(core.window_labels(), vec![MAIN_WINDOW.to_owned(), label]);
        assert!(rec.all().is_empty() && rec.closed_windows().is_empty());
    }

    // ---- the close rules --------------------------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn closing_the_last_pane_of_a_window_that_is_not_the_last_closes_that_window() {
        let (core, rec, dir, first, second, label) = two_windows(HUP_PROOF);
        core.start_pane(first, 80, 24, 0, 0).expect("starts");
        core.start_pane(second, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || rec.has_text(first, "ready") && rec.has_text(second, "ready")), "never ready");
        let (pid_first, pid_second) = (core.pid(first).unwrap(), core.pid(second).unwrap());
        core.note_window(&label, WindowState { x: 40, y: 40, width: 900, height: 600, maximized: false });
        core.flush();
        assert!(session_on_disk(&dir).window_for(&label).is_some(), "the frame of the window was saved");
        let sent_before = rec.sent().len();

        let answer = core.close_pane_in(&label, second).expect("the window exists");

        assert_eq!(answer.panes.len(), 1, "the answer is the view the window had");
        assert_eq!(rec.closed_windows(), vec![label.clone()], "the sink was asked to destroy it");
        assert_eq!(core.window_labels(), vec![MAIN_WINDOW.to_owned()]);
        assert_eq!(rec.exit_count(), 0, "the app goes on");
        assert!(wait_until(Duration::from_secs(3), || !process_exists(pid_second)), "its shell survived");
        assert!(process_exists(pid_first), "the shell of the other window is untouched");
        let told: Vec<_> = rec.sent().into_iter().skip(sent_before).filter(|(_, o)| matches!(o, Out::Layout(_))).collect();
        assert!(told.is_empty(), "a window that is going away is not sent a layout: {told:?}");
        core.flush();
        assert_eq!(session_on_disk(&dir).window_for(&label), None, "its frame is forgotten");
        assert_eq!(on_disk(&dir).all_panes(), vec![first], "and the layout on disk is the one window of before");
    }

    #[cfg(unix)]
    #[test]
    fn closing_the_last_pane_of_the_last_window_ends_the_app() {
        let (core, rec, dir, first, second, label) = two_windows(HUP_PROOF);
        core.start_pane(first, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || rec.has_text(first, "ready")), "never ready");
        core.close_pane_in(&label, second).expect("closes the other window");
        assert_eq!(rec.exit_count(), 0);

        let view = core.close_pane_in(MAIN_WINDOW, first).expect("the last window");

        assert_eq!(rec.exit_count(), 1, "the end of the app is asked for once");
        assert_eq!(view_panes(&view), vec![first], "the last pane stays in the layout, so the next start has it");
        assert!(rec.closed_windows() == vec![label], "the last window is not destroyed by the core: the app exits");
        assert_eq!(on_disk(&dir).all_panes(), vec![first]);
    }

    #[cfg(unix)]
    #[test]
    fn the_red_button_of_the_last_window_changes_nothing_on_disk() {
        let (dir, pane) = dir_with_one_pane("red-last");
        let (core, rec) = core_in(&dir, "/bin/cat", &[]);
        core.split(false);
        core.flush();
        let bytes = std::fs::read(dir.layout_file()).unwrap();
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        let writes = core.layout_writes();
        let sent_before = rec.sent().len();

        core.window_closed(MAIN_WINDOW);
        core.flush();

        assert_eq!(std::fs::read(dir.layout_file()).unwrap(), bytes, "the layout on disk is what it was");
        assert_eq!(core.layout_writes(), writes, "and was not even written again");
        assert_eq!(core.window_labels(), vec![MAIN_WINDOW.to_owned()]);
        assert_eq!(core.view().panes.len(), 2, "the layout in memory is intact");
        assert!(core.pid(pane).is_some(), "its shell is not ended by this rule (the exit does that)");
        let told: Vec<_> = rec.sent().into_iter().skip(sent_before).filter(|(_, o)| !matches!(o, Out::Frame(_))).collect();
        assert!(told.is_empty() && rec.closed_windows().is_empty(), "{told:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_red_button_of_another_window_ends_only_its_shells() {
        let (core, rec, dir, first, second, label) = two_windows(HUP_PROOF);
        let third = core.split_in(&label, true).unwrap().focus;
        core.start_pane(first, 80, 24, 0, 0).expect("starts");
        core.start_pane(second, 80, 24, 0, 0).expect("starts");
        core.start_pane(third, 80, 24, 0, 0).expect("starts");
        assert!(
            wait_until(WAIT, || [first, second, third].iter().all(|p| rec.has_text(*p, "ready"))),
            "never ready"
        );
        let pids: Vec<u32> = [first, second, third].iter().map(|p| core.pid(*p).unwrap()).collect();
        core.note_window(&label, WindowState { x: 40, y: 40, width: 900, height: 600, maximized: false });
        let sent_before = rec.sent().len();

        core.window_closed(&label);

        assert!(
            wait_until(Duration::from_secs(3), || !process_exists(pids[1]) && !process_exists(pids[2])),
            "the shells of the window survived"
        );
        assert!(process_exists(pids[0]), "the shell of the other window was not touched");
        assert_eq!(core.window_labels(), vec![MAIN_WINDOW.to_owned()]);
        assert!(rec.closed_windows().is_empty(), "the window is gone already, nobody destroys it again");
        assert_eq!(rec.exit_count(), 0);
        assert!(core.pid(first).is_some());
        std::thread::sleep(Duration::from_millis(300));
        let after: Vec<_> = rec.sent().into_iter().skip(sent_before).collect();
        assert!(
            after.iter().all(|(_, o)| !matches!(o, Out::Layout(_) | Out::Exit)),
            "no layout is sent for a window that closed itself: {after:?}"
        );
        core.flush();
        assert_eq!(on_disk(&dir).all_panes(), vec![first], "the layout on disk lost the window");
        assert_eq!(session_on_disk(&dir).window_for(&label), None, "and its frame");
        // A second report of the same window (the red button, then the destroy) changes nothing.
        core.window_closed(&label);
        assert_eq!(core.window_labels(), vec![MAIN_WINDOW.to_owned()]);
    }

    #[cfg(unix)]
    #[test]
    fn quitting_never_drops_windows() {
        let (core, rec, dir, first, second, label) = two_windows(HUP_PROOF);
        core.start_pane(first, 80, 24, 0, 0).expect("starts");
        core.start_pane(second, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || rec.has_text(first, "ready") && rec.has_text(second, "ready")), "never ready");
        core.note_window(&label, WindowState { x: 40, y: 40, width: 900, height: 600, maximized: false });

        // Cmd+Q: the exit request runs `shutdown`, then every window is destroyed and reports it.
        core.shutdown();
        core.window_closed(&label);
        core.window_closed(MAIN_WINDOW);

        assert_eq!(core.window_labels(), vec![MAIN_WINDOW.to_owned(), label.clone()], "both windows are still in the set");
        let text = std::fs::read_to_string(dir.layout_file()).unwrap();
        let (saved, status) = WindowSet::from_json(&text);
        assert_eq!(status, LoadStatus::Clean);
        assert_eq!(saved.labels(), vec![MAIN_WINDOW, label.as_str()], "and on disk");
        assert_eq!(saved.all_panes().len(), 2);
        assert!(session_on_disk(&dir).window_for(&label).is_some(), "with their frames");
    }

    #[cfg(unix)]
    #[test]
    fn the_end_of_the_app_through_the_last_pane_does_not_drop_windows_either() {
        let (core, rec, dir, first, _second, label) = two_windows("sleep 30");
        core.start_pane(first, 80, 24, 0, 0).expect("starts");

        // The main window's pane is not the last pane of the last window: closing it closes `main`.
        core.begin_exit();
        core.window_closed(&label);

        assert_eq!(rec.exit_count(), 1);
        assert_eq!(core.window_labels().len(), 2);
        let (saved, _) = WindowSet::from_json(&std::fs::read_to_string(dir.layout_file()).unwrap());
        assert_eq!(saved.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_that_ends_by_itself_closes_its_window_when_it_was_the_last_pane_of_it() {
        // Every shell ends at once. The pane of the other window goes first: its window closes. Then the last pane
        // of the last window ends: the app ends (CR-01 for each window).
        let (core, rec, dir, first, second, label) = two_windows("exit 0");

        core.start_pane(second, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || !rec.closed_windows().is_empty()), "the window did not close: {:?}", rec.all());
        assert_eq!(rec.closed_windows(), vec![label.clone()]);
        assert_eq!(rec.exit_count(), 0, "one window is left");
        assert_eq!(core.window_labels(), vec![MAIN_WINDOW.to_owned()]);

        core.start_pane(first, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || rec.exit_count() > 0), "no Exit: {:?}", rec.all());
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(rec.exit_count(), 1);
        assert_eq!(on_disk(&dir).all_panes(), vec![first], "the layout of the last pane is kept");
        assert!(
            rec.targets_of("pane-exit", 0).iter().all(|t| *t == Target::window(MAIN_WINDOW)),
            "nobody is told of the exit of a pane whose window went with it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pane_closed_by_the_user_reports_its_exit_to_the_window_that_closed_it() {
        let (core, rec, _dir, first, second, label) = two_windows(HUP_PROOF);
        let third = core.split_in(&label, false).unwrap().focus;
        core.start_pane(first, 80, 24, 0, 0).expect("starts");
        core.start_pane(third, 80, 24, 0, 0).expect("starts");
        core.start_pane(second, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || rec.has_text(third, "ready")), "never ready");

        core.close_pane_in(&label, third).expect("closes");

        assert!(wait_until(Duration::from_secs(3), || !rec.pane_exits().is_empty()), "no exit");
        assert_eq!(rec.targets_of("pane-exit", 0), vec![Target::window(&label)]);
        assert_eq!(rec.targets_of("layout", 0), vec![Target::window(&label)]);
        assert!(lock(&core.routes).closing.is_empty(), "the memory of the closing pane is gone once it was told");
    }

    // ---- closing a whole tab or workspace (the × buttons) -------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn closing_a_tab_ends_the_shells_of_its_panes_only_and_tells_the_window_once() {
        let (core, rec, _dir, first, second, label) = two_windows(HUP_PROOF);
        let split = core.split(false).focus;
        let other_tab = core.new_tab().focus;
        for pane in [first, split, other_tab, second] {
            core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        }
        assert!(wait_until(WAIT, || [first, split, other_tab, second].iter().all(|p| rec.has_text(*p, "ready"))), "never ready");
        let pids: Vec<u32> = [first, split, other_tab, second].iter().map(|p| core.pid(*p).unwrap()).collect();
        let sent_before = rec.sent().len();

        let view = core.close_tab_in(MAIN_WINDOW, 0).expect("main exists");

        assert_eq!(view_panes(&view), vec![other_tab], "the shown tab stays shown");
        assert!(wait_until(Duration::from_secs(3), || !process_exists(pids[0]) && !process_exists(pids[1])), "a shell survived");
        assert!(process_exists(pids[2]) && process_exists(pids[3]), "the other tab and the other window are untouched");
        assert!(wait_until(Duration::from_secs(3), || rec.pane_exits().len() == 2), "no exits: {:?}", rec.pane_exits());
        let mut exits: Vec<PaneId> = rec.pane_exits().iter().map(|(p, _)| *p).collect();
        exits.sort_unstable();
        assert_eq!(exits, vec![first, split]);
        assert_eq!(rec.targets_of("layout", sent_before), vec![Target::window(MAIN_WINDOW)], "one layout, for the window");
        assert!(rec.targets_of("pane-exit", sent_before).iter().all(|t| *t == Target::window(MAIN_WINDOW)));
        assert!(lock(&core.routes).closing.is_empty(), "the memory of the closing panes is gone once they were told");
        assert!(core.view_in(&label).is_ok());
        assert_eq!(rec.exit_count(), 0);
        assert!(rec.closed_windows().is_empty());
    }

    #[test]
    fn a_workspace_closes_with_every_pane_of_every_tab_and_panes_that_never_started_are_told() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let first = only_pane(&core);
        let b = core.new_workspace("two").focus;
        let c = core.split(false).focus;
        let d = core.new_tab().focus;
        core.select_workspace(1);

        let view = core.close_workspace_in(MAIN_WINDOW, 1).expect("main exists");

        assert_eq!(view_panes(&view), vec![first]);
        assert_eq!(view.workspaces.len(), 1);
        let mut exits: Vec<_> = rec.pane_exits();
        exits.sort_unstable();
        assert_eq!(exits, vec![(b, None), (c, None), (d, None)]);
        assert_eq!(rec.layouts().len(), 1, "the window hears of it once");
        assert!(lock(&core.routes).closing.is_empty());
        assert_eq!(rec.exit_count(), 0);
        assert_eq!(core.view().panes.iter().map(|p| p.0).collect::<Vec<_>>(), vec![first]);
    }

    #[cfg(unix)]
    #[test]
    fn a_tab_that_is_all_of_a_window_closes_the_window_and_no_other() {
        let (core, rec, _dir, first, second, label) = two_windows(HUP_PROOF);
        let third = core.split_in(&label, true).unwrap().focus;
        for pane in [first, second, third] {
            core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        }
        assert!(wait_until(WAIT, || [first, second, third].iter().all(|p| rec.has_text(*p, "ready"))), "never ready");
        let pids: Vec<u32> = [first, second, third].iter().map(|p| core.pid(*p).unwrap()).collect();
        let sent_before = rec.sent().len();

        let view = core.close_tab_in(&label, 0).expect("the window exists");

        assert_eq!(view_panes(&view).len(), 2, "the answer is the view the window had");
        assert_eq!(rec.closed_windows(), vec![label.clone()], "the sink was asked to destroy it");
        assert_eq!(core.window_labels(), vec![MAIN_WINDOW.to_owned()]);
        assert!(wait_until(Duration::from_secs(3), || !process_exists(pids[1]) && !process_exists(pids[2])), "shells survived");
        assert!(process_exists(pids[0]), "the shell of the other window is untouched");
        let told: Vec<_> = rec.sent().into_iter().skip(sent_before).filter(|(_, o)| matches!(o, Out::Layout(_))).collect();
        assert!(told.is_empty(), "a window that is going away is not sent a layout: {told:?}");
        assert_eq!(rec.exit_count(), 0, "the app goes on");
    }

    #[cfg(unix)]
    #[test]
    fn a_workspace_that_is_the_whole_layout_ends_the_app_and_changes_nothing() {
        let (core, rec, dir) = core("/bin/cat", &[]);
        let first = only_pane(&core);
        let second = core.split(false).focus;
        let third = core.new_tab().focus;
        let before = core.view();

        let view = core.close_workspace_in(MAIN_WINDOW, 0).expect("main exists");

        assert_eq!(rec.exit_count(), 1, "the end of the app is asked for once");
        assert_eq!(without_rev(&view), without_rev(&before), "nothing was removed, so the next start has it all");
        assert!(rec.layouts().is_empty() && rec.pane_exits().is_empty());
        assert_eq!(on_disk(&dir).all_panes(), vec![first, second, third]);
        assert!(core.close_tab_in(MAIN_WINDOW, 0).is_ok());
    }

    #[test]
    fn a_tab_or_workspace_that_is_not_there_and_a_window_that_is_not_there_change_nothing() {
        let (core, rec, dir) = core("/bin/cat", &[]);
        let first = only_pane(&core);
        core.split(false);
        let before = core.view();

        for view in [
            core.close_tab_in(MAIN_WINDOW, 1).unwrap(),
            core.close_tab_in(MAIN_WINDOW, usize::MAX).unwrap(),
            core.close_workspace_in(MAIN_WINDOW, 1).unwrap(),
            core.close_workspace_in(MAIN_WINDOW, usize::MAX).unwrap(),
        ] {
            assert_eq!(without_rev(&view), without_rev(&before));
        }
        assert_eq!(core.close_tab_in("window-9", 0), Err("no such window".into()));
        assert_eq!(core.close_workspace_in("window-9", 0), Err("no such window".into()));
        std::thread::sleep(Duration::from_millis(100));
        assert!(rec.all().is_empty(), "{:?}", rec.all());
        assert!(!dir.layout_file().exists(), "nothing changed, nothing is written");
        assert_eq!(core.view().panes.len(), 2);
        assert_eq!(core.view().panes[0].0, first);
    }

    #[test]
    fn a_window_closes_tabs_and_workspaces_of_its_own_layout_only() {
        let (core, rec, _dir, first, second, label) = two_windows("sleep 30");
        core.new_tab_in(&label).unwrap();

        // Index 0 of `main` is the tab of `first`, whatever the other window shows at its index 0.
        let view = core.close_tab_in(&label, 0).expect("the other window");

        assert_eq!(view_panes(&view).len(), 1, "the other window kept its second tab");
        assert!(core.view_in(MAIN_WINDOW).unwrap().panes.iter().any(|p| p.0 == first));
        assert!(core.view_in(&label).unwrap().panes.iter().all(|p| p.0 != second));
        assert!(rec.targets_of("layout", 0).iter().all(|t| *t == Target::window(&label)));
    }

    // ---- title and directory replay ---------------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn the_last_title_and_directory_of_a_pane_are_kept_and_replayed_to_a_window() {
        let script = "printf '\\033]2;one\\007\\033]2;two\\007\\033]7;file://localhost/tmp\\007'; sleep 30";
        let (core, rec, _dir, first, second, label) = two_windows(script);
        core.start_pane(first, 80, 24, 0, 0).expect("starts");
        core.start_pane(second, 80, 24, 0, 0).expect("starts");
        assert!(
            wait_until(WAIT, || rec.titles().len() >= 4 && rec.cwds(first).len() == 1 && rec.cwds(second).len() == 1),
            "{:?}",
            rec.all().len()
        );
        let mark = rec.sent().len();

        core.replay(&label, &[second]);

        let replayed: Vec<_> = rec.sent().into_iter().skip(mark).collect();
        assert_eq!(
            replayed,
            vec![
                (Target::window(&label), Out::Title { pane: second, title: "two".into() }),
                (Target::window(&label), Out::Cwd { pane: second, cwd: rec.cwds(second)[0].clone() }),
            ],
            "the last title only, then the directory, to that window only"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pane_that_never_had_a_title_replays_nothing_and_a_closed_pane_is_forgotten() {
        let (core, rec, _dir, first, second, label) = two_windows(HUP_PROOF);
        core.start_pane(second, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || rec.has_text(second, "ready")), "never ready");
        let mark = rec.sent().len();
        core.replay(&label, &[first, second]);
        assert_eq!(rec.sent().len(), mark, "no title and no directory known");

        lock(&core.last_title).insert(second, "old".into());
        core.close_pane_in(&label, second).ok();
        assert!(lock(&core.last_title).get(&second).is_none(), "a closed pane leaves no title behind");
    }

    // ---- restart ----------------------------------------------------------------------------------------------

    const TWO_MONITORS: [MonitorRect; 1] = [MonitorRect { x: 0, y: 0, width: 2560, height: 1440 }];

    #[test]
    fn a_restart_restores_two_windows_with_their_layouts_and_frames() {
        let dir = Scratch::new("two-windows");
        let (first, second, label) = {
            let (core, _rec) = core_in(&dir, "/bin/cat", &[]);
            let first = only_pane(&core);
            let second = core.split(false).focus;
            core.new_workspace("notes");
            let label = raw_tear_off(&core, second);
            core.split_in(&label, true).unwrap();
            core.note_window(MAIN_WINDOW, WindowState { x: 100, y: 80, width: 1200, height: 700, maximized: false });
            core.note_window(&label, WindowState { x: 1300, y: 90, width: 800, height: 500, maximized: true });
            core.shutdown();
            (first, second, label)
        };

        let (again, _rec) = core_in(&dir, "/bin/cat", &[]);

        assert_eq!(again.window_labels(), vec![MAIN_WINDOW.to_owned(), label.clone()]);
        assert_eq!(again.restored_windows(), vec![label.clone()], "`main` is made by the configuration, the rest by the shell");
        assert!(again.view_in(&label).unwrap().panes.len() == 2);
        {
            let set = lock(&again.windows);
            let main_panes = set.layout(MAIN_WINDOW).unwrap().all_panes();
            assert!(main_panes.contains(&first) && !main_panes.contains(&second));
            assert_eq!(set.window_of(second), Some(label.as_str()));
        }
        let main = again.placement(MAIN_WINDOW, &TWO_MONITORS);
        assert_eq!((main.size, main.position, main.maximized), ((1200, 700), Some((100, 80)), false));
        let other = again.placement(&label, &TWO_MONITORS);
        assert_eq!((other.size, other.position, other.maximized), ((800, 500), Some((1300, 90)), true));
    }

    #[test]
    fn the_off_screen_rule_is_applied_to_every_window_on_its_own() {
        let dir = Scratch::new("two-windows-offscreen");
        let label = {
            let (core, _rec) = core_in(&dir, "/bin/cat", &[]);
            let second = core.split(false).focus;
            let label = raw_tear_off(&core, second);
            core.note_window(MAIN_WINDOW, WindowState { x: 100, y: 80, width: 1200, height: 700, maximized: false });
            // The second window sat on a monitor that is not connected now.
            core.note_window(&label, WindowState { x: 9000, y: 90, width: 800, height: 500, maximized: false });
            core.shutdown();
            label
        };

        let (again, _rec) = core_in(&dir, "/bin/cat", &[]);

        assert_eq!(again.placement(MAIN_WINDOW, &TWO_MONITORS).position, Some((100, 80)), "this one is on a monitor");
        let other = again.placement(&label, &TWO_MONITORS);
        assert_eq!(other.position, None, "this one is not, so the window system places it");
        assert_eq!(other.size, (800, 500), "its size is kept");
        assert_eq!(again.placement("window-777", &TWO_MONITORS).size, holzwave_core::session::DEFAULT_SIZE);
    }

    #[test]
    fn a_version_1_layout_and_session_start_as_one_window() {
        let dir = Scratch::new("v1-files");
        let (mut layout, _) = Layout::new();
        layout.split_focused(Axis::Row);
        let bytes = serde_json::to_vec_pretty(&layout).unwrap();
        std::fs::write(dir.layout_file(), &bytes).unwrap();
        let session = r#"{"version":1,"window":{"x":100,"y":80,"width":1200,"height":700,"maximized":false},"cwds":{}}"#;
        std::fs::write(dir.0.join("session.json"), session).unwrap();

        let (core, _rec) = core_in(&dir, "/bin/cat", &[]);

        assert_eq!(core.window_labels(), vec![MAIN_WINDOW.to_owned()]);
        assert!(core.restored_windows().is_empty(), "no window to open besides main");
        assert_eq!(core.view().panes.len(), 2);
        let placement = core.placement(MAIN_WINDOW, &TWO_MONITORS);
        assert_eq!((placement.size, placement.position), ((1200, 700), Some((100, 80))));
        core.flush();
        assert_eq!(std::fs::read(dir.layout_file()).unwrap(), bytes, "the layout file is left as it was");
        assert!(!dir.0.join("layout.json.bad").exists() && !dir.0.join("session.json.bad").exists());
    }

    #[test]
    fn one_window_main_is_saved_as_the_plain_layout_and_several_as_a_version_2_document() {
        let (core, _rec, dir) = core("/bin/cat", &[]);
        core.split(false);
        core.flush();
        let one = std::fs::read_to_string(dir.layout_file()).unwrap();
        assert!(Layout::from_json(&one).1 == LoadStatus::Clean && !one.contains("\"windows\""), "{one}");

        let second = core.split(false).focus;
        raw_tear_off(&core, second);
        core.saver.touch();
        core.flush();

        let two = std::fs::read_to_string(dir.layout_file()).unwrap();
        assert!(two.contains("\"version\": 2") && two.contains("\"windows\""), "{two}");
    }

    // ---- tear off, release outside, attach (plan 02.1-04, task 2) ---------------------------------------------

    /// Prints a title and a directory, then echoes what it is typed.
    #[cfg(unix)]
    const TITLED_CAT: &str = "printf '\\033]2;t\\007\\033]7;file://localhost/tmp\\007'; exec cat";

    /// Lets the recorder look at the locks of `core` when the core asks it to open a window.
    fn watch(core: &Arc<AppCore>, rec: &Recorder) {
        *lock(&rec.core) = Arc::downgrade(core);
    }

    fn over(rec: &Recorder, spot: CursorSpot) {
        *lock(&rec.spot) = spot;
    }

    /// Two panes `a` and `b` in `main` and one pane `c` in the window `w`, running `script`.
    fn three_panes(script: &str) -> (Arc<AppCore>, Arc<Recorder>, Scratch, PaneId, PaneId, PaneId, String) {
        let (core, rec, dir) = sh(script);
        watch(&core, &rec);
        let a = only_pane(&core);
        let b = core.split(false).focus;
        let c = core.split(false).focus;
        let w = raw_tear_off(&core, c);
        (core, rec, dir, a, b, c, w)
    }

    #[cfg(unix)]
    fn start_all(core: &AppCore, panes: &[PaneId]) {
        for pane in panes {
            core.start_pane(*pane, 80, 24, 0, 0).expect("starts");
        }
    }

    fn panes_in(core: &AppCore, label: &str) -> Vec<PaneId> {
        let mut panes = lock(&core.windows).layout(label).expect("the window").all_panes();
        panes.sort_unstable();
        panes
    }

    #[cfg(unix)]
    #[test]
    fn a_tear_off_makes_a_window_of_its_own_with_the_same_shell_and_tells_the_window_the_pane_left() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        watch(&core, &rec);
        let first = only_pane(&core);
        let second = core.split(false).focus;
        start_all(&core, &[first, second]);
        let pid = core.pid(second).expect("a shell");

        let view = core.tear_off_pane_in(MAIN_WINDOW, second, false).expect("torn off");

        assert_eq!(view_panes(&view), vec![first], "the answer is the view of the window the pane left");
        let labels = core.window_labels();
        assert_eq!(labels.len(), 2);
        let label = labels[1].clone();
        assert_eq!(panes_in(&core, &label), vec![second]);
        assert_eq!(core.pid(second), Some(pid), "the same shell");
        let opened = lock(&rec.opened).clone();
        assert_eq!(opened, vec![(label.clone(), Place::Beside(MAIN_WINDOW.to_owned()), true)]);
        assert_eq!(rec.layouts(), vec![view.clone()], "the window the pane left was told");
        assert_eq!(rec.targets_of("layout", 0), vec![Target::window(MAIN_WINDOW)]);

        // The pane's events go to its new window from now on, and to no other.
        let mark = rec.sent().len();
        core.key(second, key("KeyX", "x")).expect("accepted");
        core.key(first, key("KeyY", "y")).expect("accepted");
        assert!(wait_until(WAIT, || rec.has_text(second, "x") && rec.has_text(first, "y")), "no echo");
        let after: Vec<(Target, Out)> = rec.sent().into_iter().skip(mark).collect();
        for (target, out) in &after {
            if let Out::Frame(f) = out {
                let expected = if f.pane == second { &label } else { MAIN_WINDOW };
                assert_eq!(*target, Target::window(expected), "frame of pane {}", f.pane);
            }
        }
    }

    #[test]
    fn a_tear_off_at_the_pointer_opens_the_window_there() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let first = only_pane(&core);
        core.split(false);

        core.tear_off_pane_in(MAIN_WINDOW, first, true).expect("torn off");

        let opened = lock(&rec.opened).clone();
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].1, Place::Cursor);
    }

    #[cfg(unix)]
    #[test]
    fn the_window_is_built_while_no_lock_of_the_core_is_held() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        watch(&core, &rec);
        let first = only_pane(&core);
        let second = core.split(false).focus;
        start_all(&core, &[first, second]);

        core.tear_off_pane_in(MAIN_WINDOW, second, false).expect("torn off");
        let tab = core.new_tab().focus;
        core.split(false);
        core.tear_off_tab_in(MAIN_WINDOW, tab, false).expect("a tab torn off");

        let opened = lock(&rec.opened).clone();
        assert_eq!(opened.len(), 2);
        assert!(opened.iter().all(|(_, _, free)| *free), "a lock was held while a window was built: {opened:?}");
    }

    #[test]
    fn a_pane_that_cannot_be_a_window_of_its_own_is_refused_and_nothing_changes() {
        let (core, rec, _dir, a, _b, c, w) = three_panes("sleep 30");

        // alone in its window; unknown; of another window; no such window
        assert!(core.tear_off_pane_in(&w, c, false).is_err());
        assert!(core.tear_off_pane_in(MAIN_WINDOW, 9_999, false).is_err());
        assert!(core.tear_off_pane_in(MAIN_WINDOW, c, false).is_err(), "a window tears off its own panes only");
        assert!(core.tear_off_pane_in("window-404", a, false).is_err());
        assert!(core.tear_off_tab_in(&w, c, false).is_err(), "the only tab of the only workspace");
        assert!(core.tear_off_tab_in(MAIN_WINDOW, c, false).is_err(), "a tab of another window");

        assert!(lock(&rec.opened).is_empty(), "no window was asked for");
        assert_eq!(core.window_labels().len(), 2);
        assert!(rec.layouts().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_window_build_puts_the_pane_back_and_answers_an_error() {
        let (core, rec, dir) = core("/bin/cat", &[]);
        watch(&core, &rec);
        let first = only_pane(&core);
        let second = core.split(false).focus;
        core.rename_pane(second, "db");
        start_all(&core, &[first, second]);
        let pid = core.pid(second).expect("a shell");
        rec.fail_open.store(true, Ordering::SeqCst);

        let answer = core.tear_off_pane_in(MAIN_WINDOW, second, true);

        let error = answer.expect_err("the window could not be built");
        assert!(error.contains("cannot open a window"), "{error}");
        assert_eq!(core.window_labels(), vec![MAIN_WINDOW.to_owned()], "no entry for a window that does not exist");
        assert_eq!(panes_in(&core, MAIN_WINDOW), vec![first, second], "the pane is back");
        assert_eq!(core.pid(second), Some(pid), "with its shell");
        assert_eq!(snapshot(&core).pane_name(second), Some("db"), "and its name");
        assert_eq!(lock(&rec.opened).len(), 1, "the window was asked for once, with no lock held");
        assert!(lock(&rec.opened)[0].2);
        // The page is told the layout as it is now, and the pane's events still go to `main`.
        assert_eq!(rec.layouts().last().map(without_rev), Some(without_rev(&core.view())));
        assert_eq!(rec.targets_of("layout", 0), vec![Target::window(MAIN_WINDOW); rec.layouts().len()]);
        let mark = rec.sent().len();
        core.key(second, key("KeyX", "x")).expect("accepted");
        assert!(wait_until(WAIT, || rec.has_text(second, "x")), "no echo");
        assert!(rec.frame_targets(second).iter().skip(1).all(|t| *t == Target::window(MAIN_WINDOW)));
        assert!(rec.sent().len() > mark);
        core.flush();
        let saved = std::fs::read_to_string(dir.layout_file()).unwrap();
        assert!(!saved.contains("\"windows\""), "one window main is the plain layout again: {saved}");
    }

    #[test]
    fn a_failed_window_build_puts_a_whole_tab_back_with_its_panes_and_title() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let first = only_pane(&core);
        core.new_tab();
        let inside = core.split(false).focus;
        core.rename_tab(1, "logs");
        let before = core.view();
        assert_eq!(before.panes.len(), 2);
        rec.fail_open.store(true, Ordering::SeqCst);

        assert!(core.tear_off_tab_in(MAIN_WINDOW, inside, true).is_err());

        let after = core.view();
        assert_eq!(core.window_labels(), vec![MAIN_WINDOW.to_owned()]);
        assert_eq!(view_panes(&after), view_panes(&before), "both panes of the tab are back and shown");
        assert_eq!(after.tabs.len(), 2);
        assert_eq!(after.tabs[1].title, "logs", "the tab kept its title");
        assert!(panes_in(&core, MAIN_WINDOW).contains(&first));
    }

    #[test]
    fn a_torn_off_tab_keeps_its_panes_its_title_and_the_names() {
        let (core, rec, _dir) = core("/bin/cat", &[]);
        let first = only_pane(&core);
        core.new_tab();
        let inside = core.split(false).focus;
        core.rename_pane(inside, "api");
        core.rename_tab(1, "logs");
        let tab_panes = view_panes(&core.view());

        let view = core.tear_off_tab_in(MAIN_WINDOW, inside, false).expect("torn off");

        assert_eq!(view.tabs.len(), 1, "`main` keeps the other tab");
        assert_eq!(view_panes(&view), vec![first]);
        let label = core.window_labels()[1].clone();
        let torn = core.view_in(&label).expect("the new window");
        assert_eq!(view_panes(&torn), tab_panes);
        assert_eq!(torn.tabs.len(), 1);
        assert_eq!(torn.tabs[0].title, "logs");
        assert_eq!(torn.pane_names, vec![(inside, "api".to_string())]);
        assert_eq!(lock(&rec.opened).len(), 1);
    }

    // ---- release outside --------------------------------------------------------------------------------------

    #[test]
    fn a_release_over_another_window_hands_the_drop_to_that_window_and_changes_nothing_yet() {
        let (core, rec, _dir, a, b, c, w) = three_panes("sleep 30");
        over(&rec, CursorSpot::Window { label: w.clone(), x: 12.5, y: 30.0 });
        let before = core.view();

        let outcome = core.release_outside_in(MAIN_WINDOW, b, DropKind::Pane);

        assert_eq!(outcome, Ok(Released::Handed { to: w.clone() }));
        assert_eq!(
            rec.sent(),
            vec![(Target::window(&w), Out::IncomingDrop { kind: DropKind::Pane, pane: b, x: 12.5, y: 30.0 })],
            "only the window under the pointer is told, with the pointer in its coordinates"
        );
        assert_eq!(without_rev(&core.view()), without_rev(&before), "nothing moved before the other page answers");
        assert_eq!(panes_in(&core, &w), vec![c]);
        assert_eq!(lock(&core.drops).source(DropKind::Pane, b, Instant::now()).as_deref(), Some(MAIN_WINDOW));
        assert_eq!(panes_in(&core, MAIN_WINDOW), vec![a, b]);
    }

    #[test]
    fn a_release_over_the_source_window_or_a_window_that_does_not_exist_is_cancelled() {
        let (core, rec, _dir, _a, b, _c, w) = three_panes("sleep 30");

        over(&rec, CursorSpot::Window { label: MAIN_WINDOW.into(), x: 1.0, y: 1.0 });
        assert_eq!(core.release_outside_in(MAIN_WINDOW, b, DropKind::Pane), Ok(Released::Cancelled));
        over(&rec, CursorSpot::Window { label: "window-404".into(), x: 1.0, y: 1.0 });
        assert_eq!(core.release_outside_in(MAIN_WINDOW, b, DropKind::Pane), Ok(Released::Cancelled));
        over(&rec, CursorSpot::Unknown);
        assert_eq!(core.release_outside_in(MAIN_WINDOW, b, DropKind::Pane), Ok(Released::Cancelled));

        assert!(rec.sent().is_empty() && lock(&rec.opened).is_empty());
        assert!(lock(&core.drops).is_empty());
        assert_eq!(core.window_labels().len(), 2);
        assert!(core.view_in(&w).is_ok());
    }

    #[test]
    fn a_release_over_nothing_tears_the_pane_or_the_tab_off_at_the_pointer() {
        let (core, rec, _dir, a, b, c, _w) = three_panes("sleep 30");
        over(&rec, CursorSpot::Nothing);

        let outcome = core.release_outside_in(MAIN_WINDOW, b, DropKind::Pane).expect("answers");

        let Released::TornOff { label } = outcome else { panic!("not torn off: {outcome:?}") };
        assert_eq!(panes_in(&core, &label), vec![b]);
        assert_eq!(lock(&rec.opened)[0].1, Place::Cursor);
        assert_eq!(panes_in(&core, MAIN_WINDOW), vec![a]);

        // A pane that is alone in its window cannot leave it: cancelled, not an error.
        assert_eq!(core.release_outside_in(MAIN_WINDOW, a, DropKind::Pane), Ok(Released::Cancelled));
        assert_eq!(core.release_outside_in(MAIN_WINDOW, a, DropKind::Tab), Ok(Released::Cancelled));
        assert_eq!(lock(&rec.opened).len(), 1);

        // A tab, named by one of its panes.
        core.new_tab();
        let in_tab = core.split(false).focus;
        let outcome = core.release_outside_in(MAIN_WINDOW, in_tab, DropKind::Tab).expect("answers");
        assert!(matches!(outcome, Released::TornOff { .. }), "{outcome:?}");
        assert_eq!(core.window_labels().len(), 4);
        assert!(core.window_labels().iter().all(|l| core.view_in(l).is_ok()));
        let _ = c;
    }

    #[test]
    fn a_release_names_a_pane_of_the_calling_window_only() {
        let (core, rec, _dir, a, _b, c, w) = three_panes("sleep 30");
        over(&rec, CursorSpot::Nothing);

        assert!(core.release_outside_in(MAIN_WINDOW, c, DropKind::Pane).is_err(), "c is in another window");
        assert!(core.release_outside_in(MAIN_WINDOW, c, DropKind::Tab).is_err());
        assert!(core.release_outside_in(&w, a, DropKind::Pane).is_err());
        assert!(core.release_outside_in(MAIN_WINDOW, 9_999, DropKind::Pane).is_err());
        assert!(core.release_outside_in("window-404", a, DropKind::Pane).is_err());

        assert!(lock(&rec.opened).is_empty() && rec.sent().is_empty() && lock(&core.drops).is_empty());
    }

    // ---- attach -----------------------------------------------------------------------------------------------

    /// `b` is released over the window `w` and handed to it.
    fn hand_over(core: &AppCore, rec: &Recorder, b: PaneId, w: &str, kind: DropKind) {
        over(rec, CursorSpot::Window { label: w.to_owned(), x: 5.0, y: 6.0 });
        assert_eq!(core.release_outside_in(MAIN_WINDOW, b, kind), Ok(Released::Handed { to: w.to_owned() }));
    }

    #[cfg(unix)]
    #[test]
    fn a_pane_released_over_another_window_moves_there_with_its_shell_and_its_events_follow() {
        let (core, rec, dir, a, b, c, w) = three_panes(TITLED_CAT);
        start_all(&core, &[a, b, c]);
        assert!(wait_until(WAIT, || rec.cwds(b).len() == 1 && rec.titles().iter().any(|(p, _)| *p == b)), "no title");
        let pid = core.pid(b).expect("a shell");
        hand_over(&core, &rec, b, &w, DropKind::Pane);
        let mark = rec.sent().len();

        let view = core.attach_pane_in(&w, b, Some(c), Some(Edge::Right), None).expect("attached");

        assert_eq!(view_panes(&view), vec![b, c], "the answer is the view of the window the pane came to");
        assert_eq!(panes_in(&core, MAIN_WINDOW), vec![a]);
        assert_eq!(core.pid(b), Some(pid), "the shell went along");
        // Both windows are told, the new owner first with what the answer says, and the title and directory of the
        // pane follow the layout that has it.
        let told: Vec<(Target, Out)> = rec.sent().into_iter().skip(mark).filter(|(_, o)| !matches!(o, Out::Frame(_))).collect();
        let layout_new = told.iter().position(|(t, o)| *t == Target::window(&w) && *o == Out::Layout(view.clone()));
        let layout_old = told.iter().position(|(t, o)| *t == Target::window(MAIN_WINDOW) && matches!(o, Out::Layout(_)));
        let title = told.iter().position(|(t, o)| *t == Target::window(&w) && matches!(o, Out::Title { pane, .. } if *pane == b));
        let cwd = told.iter().position(|(t, o)| *t == Target::window(&w) && matches!(o, Out::Cwd { pane, .. } if *pane == b));
        assert!(layout_new.is_some() && layout_old.is_some(), "{told:?}");
        assert!(title.is_some() && cwd.is_some(), "the title and the directory were replayed: {told:?}");
        assert!(layout_new < title && layout_new < cwd, "the page hears of the pane before its title");
        assert_eq!(told.iter().filter(|(_, o)| matches!(o, Out::Title { .. })).count(), 1, "to one window only");

        // From now on the pane's frames go to the new owner only, the first one a full frame.
        core.key(b, key("KeyX", "x")).expect("accepted");
        assert!(wait_until(WAIT, || rec.frames(b).iter().skip(1).any(|f| f.changed.iter().any(|r| !r.runs.is_empty()))), "no echo");
        let after: Vec<(Target, FrameMsg)> = rec
            .sent()
            .into_iter()
            .skip(mark)
            .filter_map(|(t, o)| match o {
                Out::Frame(f) if f.pane == b => Some((t, f)),
                _ => None,
            })
            .collect();
        assert!(!after.is_empty());
        assert!(after.iter().all(|(t, _)| *t == Target::window(&w)), "a frame of the moved pane went to the old window");
        assert!(after.first().is_some_and(|(_, f)| f.full), "the new window's first frame is a full one");

        // Both layouts are on disk.
        core.flush();
        let (saved, status) = WindowSet::from_json(&std::fs::read_to_string(dir.layout_file()).unwrap());
        assert_eq!(status, LoadStatus::Clean);
        assert_eq!(saved.window_of(b), Some(w.as_str()));
        assert_eq!(saved.window_of(a), Some(MAIN_WINDOW));
    }

    #[test]
    fn a_pane_dropped_on_the_middle_of_a_pane_of_another_window_swaps_them_and_their_routes() {
        let (core, rec, _dir, a, b, c, w) = three_panes("sleep 30");
        hand_over(&core, &rec, b, &w, DropKind::Pane);

        let view = core.attach_pane_in(&w, b, Some(c), Some(Edge::Center), None).expect("swapped");

        assert_eq!(view_panes(&view), vec![b], "`b` took the place of `c` in the other window");
        assert_eq!(panes_in(&core, MAIN_WINDOW), vec![a, c]);
        assert_eq!(lock(&core.routes).live.get(&b).map(|l| l.to_string()), Some(w.clone()));
        assert_eq!(lock(&core.routes).live.get(&c).map(|l| l.to_string()), Some(MAIN_WINDOW.to_owned()));
    }

    #[test]
    fn a_pane_can_be_dropped_on_a_tab_strip_as_a_tab_of_a_workspace() {
        let (core, rec, _dir, _a, b, c, w) = three_panes("sleep 30");
        hand_over(&core, &rec, b, &w, DropKind::Pane);

        let view = core.attach_pane_in(&w, b, None, None, Some(0)).expect("attached as a tab");

        assert_eq!(view.tabs.len(), 2, "the pane is the last tab of the window's workspace");
        assert!(view.tabs[1].active);
        assert_eq!(view_panes(&view), vec![b], "and shown");
        assert!(panes_in(&core, &w).contains(&c));
    }

    #[test]
    fn the_last_pane_of_a_window_dropped_on_another_window_closes_the_source_window() {
        let (core, rec, dir, a, b, c, w) = three_panes("sleep 30");
        core.note_window(&w, WindowState { x: 40, y: 40, width: 900, height: 600, maximized: false });
        over(&rec, CursorSpot::Window { label: MAIN_WINDOW.into(), x: 5.0, y: 6.0 });
        assert_eq!(core.release_outside_in(&w, c, DropKind::Pane), Ok(Released::Handed { to: MAIN_WINDOW.into() }));
        let mark = rec.sent().len();

        let view = core.attach_pane_in(MAIN_WINDOW, c, Some(a), Some(Edge::Left), None).expect("attached");

        assert_eq!(view_panes(&view), vec![a, b, c]);
        assert_eq!(core.window_labels(), vec![MAIN_WINDOW.to_owned()]);
        assert_eq!(rec.closed_windows(), vec![w.clone()], "the empty window is destroyed");
        let told: Vec<_> = rec.sent().into_iter().skip(mark).filter(|(t, _)| *t == Target::window(&w)).collect();
        assert!(told.iter().all(|(_, o)| !matches!(o, Out::Layout(_))), "no layout for a window that went: {told:?}");
        core.flush();
        assert_eq!(session_on_disk(&dir).window_for(&w), None, "its frame is forgotten");
        assert_eq!(lock(&core.routes).live.get(&c).map(|l| l.to_string()), Some(MAIN_WINDOW.to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn a_tab_released_over_another_window_moves_there_with_all_its_panes() {
        let (core, rec, _dir, a, _b, _c, w) = three_panes(TITLED_CAT);
        let tab_first = core.new_tab().focus;
        let tab_second = core.split(false).focus;
        start_all(&core, &[tab_first, tab_second]);
        assert!(wait_until(WAIT, || rec.cwds(tab_first).len() == 1 && rec.cwds(tab_second).len() == 1), "no cwd");
        hand_over(&core, &rec, tab_second, &w, DropKind::Tab);
        let mark = rec.sent().len();

        let view = core.attach_tab_in(&w, tab_second, 0).expect("attached");

        assert_eq!(view.tabs.len(), 2);
        assert_eq!(view_panes(&view), vec![tab_first, tab_second], "the tab with its split tree is shown");
        assert!(!panes_in(&core, MAIN_WINDOW).contains(&tab_first));
        assert!(panes_in(&core, MAIN_WINDOW).contains(&a));
        for pane in [tab_first, tab_second] {
            assert_eq!(lock(&core.routes).live.get(&pane).map(|l| l.to_string()), Some(w.clone()), "route of {pane}");
        }
        let replayed: Vec<PaneId> = rec
            .sent()
            .into_iter()
            .skip(mark)
            .filter_map(|(t, o)| match o {
                Out::Cwd { pane, .. } if t == Target::window(&w) => Some(pane),
                _ => None,
            })
            .collect();
        assert!(replayed.contains(&tab_first) && replayed.contains(&tab_second), "{replayed:?}");
    }

    #[test]
    fn a_stale_repeated_or_wrong_answer_changes_nothing() {
        let (core, rec, _dir, a, b, c, w) = three_panes("sleep 30");
        let (main_before, w_before) = (core.view(), core.view_in(&w).unwrap());
        let unchanged = |core: &AppCore| {
            assert_eq!(without_rev(&core.view()), without_rev(&main_before));
            assert_eq!(without_rev(&core.view_in(&w).unwrap()), without_rev(&w_before));
        };

        // Nothing is in flight.
        assert!(core.attach_pane_in(&w, b, Some(c), Some(Edge::Left), None).is_ok());
        assert!(core.attach_tab_in(&w, b, 0).is_ok());
        unchanged(&core);
        assert!(rec.sent().is_empty());

        // A drop of `b` is in flight; the answers are for something else.
        hand_over(&core, &rec, b, &w, DropKind::Pane);
        let mark = rec.sent().len();
        assert!(core.attach_pane_in(&w, a, Some(c), Some(Edge::Left), None).is_ok(), "another pane");
        assert!(core.attach_tab_in(&w, b, 0).is_ok(), "another kind");
        assert!(core.attach_pane_in(&w, b, None, None, None).is_ok(), "no place named");
        assert!(core.attach_pane_in(&w, b, Some(9_999), Some(Edge::Left), None).is_ok(), "a target that is not there");
        assert!(core.attach_pane_in(&w, b, None, None, Some(7)).is_ok(), "a workspace that is not there");
        assert!(core.attach_pane_in(MAIN_WINDOW, b, Some(a), Some(Edge::Left), None).is_ok(), "the window it came from");
        assert!(core.attach_pane_in("window-404", b, Some(c), Some(Edge::Left), None).is_err(), "no such window");
        unchanged(&core);
        assert_eq!(rec.sent().len(), mark, "no event for an answer that changed nothing");
        assert!(lock(&core.drops).source(DropKind::Pane, b, Instant::now()).is_some(), "the drop is still waiting");

        // The right answer works once; the same answer again finds nothing.
        core.attach_pane_in(&w, b, Some(c), Some(Edge::Left), None).expect("the right answer");
        let after = (core.view(), core.view_in(&w).unwrap());
        assert!(core.attach_pane_in(&w, b, Some(c), Some(Edge::Right), None).is_ok());
        assert_eq!((without_rev(&core.view()), without_rev(&core.view_in(&w).unwrap())), (without_rev(&after.0), without_rev(&after.1)));
    }

    #[test]
    fn a_drop_that_waited_too_long_or_whose_pane_went_elsewhere_is_not_answered() {
        let (core, rec, _dir, a, b, c, w) = three_panes("sleep 30");

        // Too old.
        let long_ago = Instant::now().checked_sub(crate::windows::DROP_TTL * 2).expect("the clock has run that long");
        lock(&core.drops).begin(DropKind::Pane, b, MAIN_WINDOW, long_ago);
        core.attach_pane_in(&w, b, Some(c), Some(Edge::Left), None).expect("answers");
        assert_eq!(panes_in(&core, MAIN_WINDOW), vec![a, b], "an expired drop moves nothing");

        // The pane was closed after the drop was handed over.
        hand_over(&core, &rec, b, &w, DropKind::Pane);
        core.close_pane(b);
        core.attach_pane_in(&w, b, Some(c), Some(Edge::Left), None).expect("answers");
        assert_eq!(panes_in(&core, &w), vec![c]);
        assert_eq!(panes_in(&core, MAIN_WINDOW), vec![a]);
    }

    #[cfg(unix)]
    #[test]
    fn a_window_ready_page_is_told_the_last_title_and_directory_of_every_pane_it_holds() {
        let (core, rec, _dir, _a, _b, c, w) = three_panes(TITLED_CAT);
        let d = core.split_in(&w, true).unwrap().focus;
        start_all(&core, &[c, d]);
        assert!(wait_until(WAIT, || rec.cwds(c).len() == 1 && rec.cwds(d).len() == 1), "no cwd");
        let mark = rec.sent().len();

        core.window_ready_in(&w).expect("a window");

        let replayed: Vec<(Target, Out)> = rec.sent().into_iter().skip(mark).collect();
        let targets: Vec<&Target> = replayed.iter().map(|(t, _)| t).collect();
        assert!(targets.iter().all(|t| **t == Target::window(&w)), "{targets:?}");
        for pane in [c, d] {
            assert!(replayed.iter().any(|(_, o)| matches!(o, Out::Title { pane: p, title } if *p == pane && title == "t")));
            assert!(replayed.iter().any(|(_, o)| matches!(o, Out::Cwd { pane: p, .. } if *p == pane)));
        }
        assert_eq!(core.window_ready_in("window-404"), Err("no such window".to_owned()));
    }

    #[test]
    fn restarting_after_a_tear_off_restores_the_new_window_with_its_layout_and_frame() {
        let dir = Scratch::new("tear-off-restart");
        let (second, label) = {
            let (core, rec) = core_in(&dir, "/bin/cat", &[]);
            let first = only_pane(&core);
            let second = core.split(false).focus;
            core.rename_pane(second, "db");
            core.tear_off_pane_in(MAIN_WINDOW, second, false).expect("torn off");
            let label = core.window_labels()[1].clone();
            core.note_window(&label, WindowState { x: 1200, y: 60, width: 700, height: 500, maximized: false });
            assert_eq!(lock(&rec.opened).len(), 1);
            assert!(panes_in(&core, MAIN_WINDOW).contains(&first));
            core.shutdown();
            (second, label)
        };

        let (again, _rec) = core_in(&dir, "/bin/cat", &[]);

        assert_eq!(again.restored_windows(), vec![label.clone()]);
        assert_eq!(panes_in(&again, &label), vec![second]);
        let placement = again.placement(&label, &TWO_MONITORS);
        assert_eq!((placement.size, placement.position), ((700, 500), Some((1200, 60))));
        let set = lock(&again.windows);
        assert_eq!(set.layout(&label).unwrap().pane_name(second), Some("db"));
    }
}

/// The protocol golden (`app/tests/fixtures/protocol.json`): what the webview depends on, generated from the real
/// Rust types and read by the TypeScript tests too, so that a renamed argument or a changed payload fails on both
/// sides at once.
///
/// `UPDATE_FIXTURES=1` rewrites the file, as for the frame golden of the core.
#[cfg(test)]
mod protocol {
    use super::*;
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use crate::commands::{
        CopyMotionMsg, CopyOpMsg, CopyStateMsg, MouseActionMsg, MouseButtonMsg, PasteOutcomeMsg, SearchOpMsg,
        SelectModeMsg, SelectOpMsg,
    };
    use holzwave_core::layout::Axis;
    use holzwave_core::wire::{ModesMsg, SearchMsg};

    const COMMANDS_SRC: &str = include_str!("commands.rs");
    const LIB_SRC: &str = include_str!("lib.rs");

    /// Serialises the writers of the golden: both tests write the same bytes in update mode.
    static GOLDEN: Mutex<()> = Mutex::new(());

    fn golden_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/protocol.json")
    }

    fn update_fixtures() -> bool {
        std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
    }

    /// One `#[tauri::command]` function of `commands.rs`.
    struct CommandSource {
        name: String,
        /// The attribute carries `rename_all = "snake_case"`.
        snake_case: bool,
        /// The argument names in order, State, AppHandle and WebviewWindow parameters left out: Tauri fills those in,
        /// the page never sends them.
        args: Vec<String>,
    }

    /// Split `text` at the commas that are not inside brackets, parentheses or angle brackets.
    fn split_top_level(text: &str) -> Vec<&str> {
        let mut parts = Vec::new();
        let (mut depth, mut start) = (0i32, 0usize);
        for (i, c) in text.char_indices() {
            match c {
                '(' | '[' | '<' | '{' => depth += 1,
                ')' | ']' | '>' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(&text[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        parts.push(&text[start..]);
        parts
    }

    /// Read the command functions out of the source text: the name and the argument names of every function that
    /// follows a `#[tauri::command` attribute.
    fn parse_commands(source: &str) -> Vec<CommandSource> {
        let mut found = Vec::new();
        let mut rest = source;
        while let Some(at) = rest.find("#[tauri::command") {
            rest = &rest[at + "#[tauri::command".len()..];
            let attr_end = rest.find(']').expect("the attribute is closed");
            let snake_case = rest[..attr_end].contains("rename_all = \"snake_case\"");
            rest = &rest[attr_end..];

            let fn_at = rest.find("fn ").expect("a function follows the attribute");
            let after = &rest[fn_at + 3..];
            let open = after.find('(').expect("an argument list");
            let name = after[..open].trim().to_owned();

            // The matching close parenthesis of the argument list.
            let (mut depth, mut close) = (0i32, None);
            for (i, c) in after[open..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(open + i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let list = &after[open + 1..close.expect("the argument list is closed")];
            let args = split_top_level(list)
                .into_iter()
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .filter_map(|a| {
                    let (arg, ty) = a.split_once(':').expect("name: Type");
                    let ty = ty.trim();
                    if ty.starts_with("State<") || ty.contains("AppHandle") || ty.contains("WebviewWindow") {
                        return None;
                    }
                    Some(arg.trim().to_owned())
                })
                .collect();
            found.push(CommandSource { name, snake_case, args });
        }
        found
    }

    fn sample_view() -> View {
        let (mut layout, _) = Layout::new();
        layout.split_focused(Axis::Row);
        layout.new_tab();
        layout.new_tab();
        layout.select_tab(0);
        layout.new_workspace("notes");
        layout.select_workspace(0);
        let view = View::of(&layout, 1);
        assert_eq!(view.workspaces.len(), 2);
        assert_eq!(view.tabs.len(), 3);
        assert_eq!(view.panes.len(), 2);
        assert_eq!(view.dividers.len(), 1);
        view
    }

    fn to_value<T: Serialize>(value: &T) -> Value {
        serde_json::to_value(value).expect("serialises")
    }

    /// Every value serialises, and deserialises back to itself: the page sends what the Rust side reads.
    fn ops<T>(values: Vec<T>) -> Value
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        Value::Array(
            values
                .into_iter()
                .map(|op| {
                    let json = to_value(&op);
                    let back: T = serde_json::from_value(json.clone()).expect("reads back");
                    assert_eq!(back, op, "{json} must read back as the same op");
                    json
                })
                .collect(),
        )
    }

    fn samples() -> Value {
        let frame = FrameMsg {
            pane: 3,
            seq: 1,
            full: true,
            cols: 80,
            rows: 24,
            fg: 0xffffff,
            bg: 0,
            cursor: None,
            scrollbar: (0, 24, 24),
            selection: Vec::new(),
            matches: Vec::new(),
            search: None,
            copy_cursor: None,
            modes: ModesMsg { alt: false, bracketed_paste: false, mouse: false },
            changed: Vec::new(),
        };
        let view = sample_view();
        let settings_view = SettingsView::new(
            &Settings::default(),
            &ThemeRegistry::builtin(),
            // By name, not the host's table: the golden is what the Mac produces and must read the same on every host.
            &Keybindings::default_for("macos"),
            vec!["settings.json: font_size 9999 is outside 8..=40; 40 is used".into()],
        );
        let events = [
            Out::Frame(frame),
            Out::Layout(view.clone()),
            Out::Title { pane: 3, title: "vim README.md".into() },
            Out::Cwd { pane: 3, cwd: "/Users/holz/Projects/holzWave".into() },
            Out::PaneExit { pane: 3, code: Some(0) },
            Out::Settings(settings_view.clone()),
            Out::IncomingDrop { kind: DropKind::Pane, pane: 3, x: 412.5, y: 96.0 },
        ];
        let names: Vec<&str> = events.iter().map(Out::name).collect();
        let payload = |i: usize| to_value(&events[i]);

        let mut motions = Vec::new();
        for motion in [
            CopyMotionMsg::Left,
            CopyMotionMsg::Right,
            CopyMotionMsg::Up,
            CopyMotionMsg::Down,
            CopyMotionMsg::LineStart,
            CopyMotionMsg::LineEnd,
            CopyMotionMsg::Top,
            CopyMotionMsg::Bottom,
            CopyMotionMsg::PageUp,
            CopyMotionMsg::PageDown,
            CopyMotionMsg::WordForward,
            CopyMotionMsg::WordBack,
        ] {
            motions.push(CopyOpMsg::Move { motion });
        }
        let mut copy_ops = vec![CopyOpMsg::Enter, CopyOpMsg::Exit, CopyOpMsg::Toggle, CopyOpMsg::Yank];
        copy_ops.extend(motions);

        json!({
            "view": to_value(&view),
            "events": {
                "names": names,
                "title": payload(2),
                "cwd": payload(3),
                "pane-exit": [
                    payload(4),
                    to_value(&Out::PaneExit { pane: 3, code: None }),
                ],
                "settings": payload(5),
                "incoming-drop": [
                    payload(6),
                    to_value(&Out::IncomingDrop { kind: DropKind::Tab, pane: 4, x: 40.0, y: 12.0 }),
                ],
            },
            "settings_view": to_value(&settings_view),
            "app_info": to_value(&AppInfo {
                version: "0.0.1-alpha1".into(),
                os: "macos".into(),
                hostname: "holz-mac.local".into(),
            }),
            "mouse": {
                "actions": ops(vec![MouseActionMsg::Press, MouseActionMsg::Release, MouseActionMsg::Motion]),
                "buttons": ops(vec![
                    MouseButtonMsg::Left,
                    MouseButtonMsg::Middle,
                    MouseButtonMsg::Right,
                    MouseButtonMsg::None,
                ]),
            },
            "ops": {
                "select": ops(vec![
                    SelectOpMsg::Begin { x: 3, y: 4, mode: SelectModeMsg::Cell },
                    SelectOpMsg::Begin { x: 3, y: 4, mode: SelectModeMsg::Word },
                    SelectOpMsg::Begin { x: 3, y: 4, mode: SelectModeMsg::Line },
                    SelectOpMsg::Extend { x: 10, y: 4 },
                    SelectOpMsg::All,
                    SelectOpMsg::Clear,
                ]),
                "search": ops(vec![
                    SearchOpMsg::Set { needle: "find".into() },
                    SearchOpMsg::Next,
                    SearchOpMsg::Prev,
                    SearchOpMsg::Clear,
                ]),
                "copy_mode": ops(copy_ops),
                "drop_kind": ops(vec![DropKind::Pane, DropKind::Tab]),
            },
            "paste": [
                to_value(&PasteOutcomeMsg::from(PasteOutcome::Written)),
                to_value(&PasteOutcomeMsg::from(PasteOutcome::Empty)),
                to_value(&PasteOutcomeMsg::from(PasteOutcome::Rejected)),
            ],
            "answers": {
                "search": [
                    to_value(&SearchMsg { total: 3, current: Some(1) }),
                    to_value(&SearchMsg { total: 0, current: None }),
                ],
                "copy_mode": [
                    to_value(&CopyStateMsg { active: true, selecting: false, yanked: None }),
                    to_value(&CopyStateMsg { active: false, selecting: false, yanked: Some("text".into()) }),
                ],
                "release_outside": [
                    to_value(&Released::TornOff { label: "window-12".into() }),
                    to_value(&Released::Handed { to: "main".into() }),
                    to_value(&Released::Cancelled),
                ],
            },
        })
    }

    /// The command table of the golden: Rust name to ordered argument names.
    fn command_table() -> Value {
        let table: BTreeMap<String, Vec<String>> =
            parse_commands(COMMANDS_SRC).into_iter().map(|c| (c.name, c.args)).collect();
        to_value(&table)
    }

    fn document() -> Value {
        let mut doc = samples();
        doc.as_object_mut().expect("object").insert("commands".into(), command_table());
        doc
    }

    /// Update mode writes the whole document; compare mode checks the part `pick` selects.
    fn check(pick: impl Fn(&Value) -> Value, what: &str) {
        let _guard = lock(&GOLDEN);
        let doc = document();
        let path = golden_path();
        if update_fixtures() {
            let mut text = serde_json::to_string_pretty(&doc).expect("json");
            text.push('\n');
            std::fs::create_dir_all(path.parent().expect("dir")).expect("fixtures dir");
            std::fs::write(&path, text).expect("write protocol.json");
            return;
        }
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is missing ({e}); run with UPDATE_FIXTURES=1", path.display()));
        let on_disk: Value = serde_json::from_str(&text).expect("protocol.json is JSON");
        assert!(
            pick(&on_disk) == pick(&doc),
            "{what} differ from protocol.json; review the change and run with UPDATE_FIXTURES=1"
        );
    }

    #[test]
    fn protocol_samples_match_golden() {
        check(
            |doc| {
                let mut rest = doc.clone();
                rest.as_object_mut().expect("object").remove("commands");
                rest
            },
            "the payload samples",
        );
    }

    #[test]
    fn protocol_commands_match_source() {
        let commands = parse_commands(COMMANDS_SRC);
        assert!(commands.len() >= 20, "the scanner found only {} commands", commands.len());
        for command in &commands {
            assert!(
                command.snake_case,
                "{} lacks rename_all = \"snake_case\": the page would have to send camelCase",
                command.name
            );
            assert!(
                LIB_SRC.contains(&format!("commands::{},", command.name)),
                "{} is not registered in generate_handler!",
                command.name
            );
        }
        assert!(
            !commands.iter().any(|c| c.name == "input"),
            "the raw input command must not exist: keys, text and paste go through the engine encoder (D-05)"
        );
        for name in ["note_window", "placement"] {
            assert!(
                !commands.iter().any(|c| c.name == name) && !COMMANDS_SRC.contains(&format!("fn {name}")),
                "{name} is a method of AppCore for the window event handlers, not a command"
            );
        }
        // The arguments the page was promised.
        let table = command_table();
        assert_eq!(table["paste"], json!(["pane", "text", "allow_unsafe"]));
        assert_eq!(table["start_pane"], json!(["pane", "cols", "rows", "cell_w", "cell_h"]));
        assert_eq!(table["resize"], json!(["pane", "cols", "rows", "cell_w", "cell_h"]));
        assert_eq!(
            table["key"],
            json!(["pane", "code", "key", "mods", "repeat", "release", "composing"])
        );
        // The calling window is not an argument: Tauri fills it in, so the page sends the same names as before.
        assert_eq!(table["split"], json!(["column"]));
        assert_eq!(table["get_layout"], json!([]));
        assert_eq!(table["close_pane"], json!(["pane"]));
        // The commands between windows carry pane ids and nothing about where the pointer is.
        assert_eq!(table["tear_off_pane"], json!(["pane", "at_cursor"]));
        assert_eq!(table["tear_off_tab"], json!(["pane", "at_cursor"]));
        assert_eq!(table["release_outside"], json!(["pane", "kind"]));
        assert_eq!(table["attach_pane"], json!(["pane", "target", "edge", "workspace"]));
        assert_eq!(table["attach_tab"], json!(["pane", "workspace"]));
        assert_eq!(table["window_ready"], json!([]));

        check(|doc| doc["commands"].clone(), "the command argument names");
    }

    #[test]
    fn the_scanner_reads_generics_and_skips_state_and_handles() {
        let source = r#"
            #[tauri::command(async, rename_all = "snake_case")]
            pub fn demo(core: State<Arc<AppCore>>, app: AppHandle, window: WebviewWindow, pane: PaneId, map: HashMap<String, Vec<u8>>) -> Result<(), String> {
                Ok(())
            }
            #[tauri::command]
            pub async fn camel(core: State<'_, Arc<AppCore>>, allow_unsafe: bool) {}
        "#;

        let found = parse_commands(source);

        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "demo");
        assert!(found[0].snake_case);
        assert_eq!(found[0].args, vec!["pane", "map"]);
        assert_eq!(found[1].name, "camel");
        assert!(!found[1].snake_case, "a bare attribute is camelCase");
        assert_eq!(found[1].args, vec!["allow_unsafe"]);
    }
}
