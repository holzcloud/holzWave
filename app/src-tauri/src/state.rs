//! The behaviour of the app shell, without Tauri (D-19).
//!
//! [`AppCore`] owns the layout and the pane handles and answers every command of the webview. It knows nothing about
//! windows: what the webview has to be told leaves through a [`Sink`] as an [`Out`], and `lib.rs` maps those to Tauri
//! events. That is what makes the shell testable headless, with a recording sink and real shells.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

use holzwave_core::engine::{CopyOp, CopyState, DomKey, EngineTheme, PasteOutcome, SearchOp, SearchState, SelectOp};
use holzwave_core::keybindings::Keybindings;
use holzwave_core::layout::{Axis, CloseOutcome, Divider, Layout, LoadStatus, PaneId, Rect, SplitId};
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
use holzwave_core::persist::{self, Debounced};
use holzwave_core::session::{MonitorRect, Placement, WindowState};
use holzwave_core::settings::{SETTINGS_FILE, Settings};
use holzwave_core::theme::{DEFAULT_THEME, THEMES_DIR, Theme, ThemeInfo, ThemeRegistry};
use holzwave_core::wire::FrameMsg;
use serde::{Serialize, Serializer};
use serde_json::Value;

/// A layout file larger than this is not read: a real layout is a few KiB.
const MAX_LAYOUT_BYTES: u64 = 1 << 20;

/// How long a layout change waits before it is written (RESEARCH Pitfall 3): a divider drag is hundreds of changes
/// and costs one write per window.
const SAVE_DELAY: Duration = Duration::from_millis(250);

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
            Out::Settings(view) => view.serialize(serializer),
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
    #[cfg(test)]
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
///
/// Lock order: `layout` first, then `panes`; `panes` is never held while waiting for `layout`. `flush` runs the
/// save job, which locks `layout`, so it is never called with `layout` held.
pub struct AppCore {
    /// The app data directory: `layout.json`, `settings.json`, `session.json` and `themes/` live in it.
    dir: PathBuf,
    layout: Arc<Mutex<Layout>>,
    panes: Mutex<HashMap<PaneId, Slot>>,
    /// The settings, themes and key bindings in force. Locked briefly and never across file IO or a wait.
    config: Mutex<Config>,
    /// Serialises the commands that write `settings.json` or reload it, so the file, the memory and the events
    /// of two of them never interleave. Held across the file IO; `config` is not.
    settings_io: Mutex<()>,
    /// The last working directory each pane reported (its `Cwd` events).
    last_cwd: Arc<Mutex<HashMap<PaneId, String>>>,
    sink: Arc<dyn Sink>,
    shell: ShellSpec,
    /// Writes the layout 250 ms after the first change of a burst and when flushed.
    saver: Debounced,
    /// How many times the saver job wrote the file, for tests.
    writes: Arc<AtomicUsize>,
    /// The app is ending (the last pane closed or ended, or `shutdown` ran): panes that end from now on stay in the
    /// layout, so that what is saved is what the user had.
    exiting: AtomicBool,
    me: Weak<AppCore>,
}

impl AppCore {
    /// Load the layout from `dir/layout.json` (a fresh one-pane layout when there is none or it is unusable) and
    /// start nothing: the webview starts each pane once it knows the pane's size.
    pub fn new(dir: PathBuf, sink: Arc<dyn Sink>, shell: ShellSpec) -> Arc<AppCore> {
        let path = dir.join("layout.json");
        let layout = Arc::new(Mutex::new(load_layout(&path)));
        let writes = Arc::new(AtomicUsize::new(0));

        let job_layout = layout.clone();
        let job_writes = writes.clone();
        let saver = Debounced::new(SAVE_DELAY, move || {
            // Serialise a copy: the layout is not locked while the file is written and synced.
            let snapshot = lock(&job_layout).clone();
            match serde_json::to_vec_pretty(&snapshot) {
                Ok(json) => {
                    job_writes.fetch_add(1, Ordering::SeqCst);
                    if let Err(e) = persist::write_atomic(&path, &json) {
                        eprintln!("holzwave: cannot save {}: {e}", path.display());
                    }
                }
                Err(e) => eprintln!("holzwave: cannot serialise the layout: {e}"),
            }
        });

        let config = Mutex::new(Config::load(&dir));

        Arc::new_cyclic(|me| AppCore {
            dir,
            layout,
            panes: Mutex::new(HashMap::new()),
            config,
            settings_io: Mutex::new(()),
            last_cwd: Arc::new(Mutex::new(HashMap::new())),
            sink,
            shell,
            saver,
            writes,
            exiting: AtomicBool::new(false),
            me: me.clone(),
        })
    }

    pub fn view(&self) -> View {
        View::of(&lock(&self.layout))
    }

    // ---- layout commands: lock, mutate, mark dirty, answer with the new view. No disk IO, no waiting. ------------

    /// Apply a layout change, schedule a save and return the new view.
    fn change(&self, f: impl FnOnce(&mut Layout)) -> View {
        let view = {
            let mut layout = lock(&self.layout);
            f(&mut layout);
            View::of(&layout)
        };
        self.saver.touch();
        view
    }

    /// Split the focused pane; the new pane gets the focus. At the pane limits the view comes back unchanged.
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

    /// Close a pane (RESEARCH Pitfall 4). The shell is killed on its own engine thread and the app keeps running;
    /// only closing the last pane of the layout ends it, after the layout is on disk. An id that is in no tab
    /// changes nothing.
    ///
    /// Not waiting for anything, but the exit path writes the layout, so the command is async.
    pub fn close_pane(&self, pane: PaneId) -> View {
        let mut layout = lock(&self.layout);
        match layout.close_pane(pane) {
            CloseOutcome::Closed => {
                let view = View::of(&layout);
                // Under the layout lock, so `pane_gone` of the same pane sees either both or neither of the changes.
                let slot = lock(&self.panes).remove(&pane);
                drop(layout);
                let never_started = slot.is_none();
                if let Some(Slot::Running(handle)) = &slot {
                    // The actor kills and reaps the shell on its own thread (about 200 ms) and sends the PaneExit.
                    handle.close();
                }
                drop(slot);
                self.saver.touch();
                self.sink.send(Out::Layout(view.clone()));
                if never_started {
                    // No shell, so no Exited event will come: the page still hears that the pane is gone.
                    self.sink.send(Out::PaneExit { pane, code: None });
                }
                view
            }
            CloseOutcome::LastPane => {
                let view = View::of(&layout);
                drop(layout);
                self.begin_exit();
                view
            }
            CloseOutcome::Unknown => View::of(&layout),
        }
    }

    /// Write the pending layout now, on the calling thread. Never call it with the layout locked.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn flush(&self) {
        self.saver.flush();
    }

    /// How many times the layout file was written; the tests count writes with it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn layout_writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }

    // ---- session and window --------------------------------------------------------------------------------

    /// The directory a new shell of `pane` starts in; `None` for the default directory.
    fn start_dir(&self, _pane: PaneId) -> Option<PathBuf> {
        None
    }

    /// Remember the window geometry for the next start.
    pub fn note_window(&self, _window: WindowState) {}

    /// Where and how large the window opens, given the monitors that are connected now.
    pub fn placement(&self, _monitors: &[MonitorRect]) -> Placement {
        Placement { size: (0, 0), position: None, maximized: false }
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
        self.sink.send(Out::Settings(view.clone()));
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
        self.sink.send(Out::Settings(view.clone()));
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
        let cfg = lock(&self.config);
        let mut spec = PaneSpec::new(self.shell.program.clone(), cols, rows).args(self.shell.args.clone());
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
                Slot::Starting => None,
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
        for handle in self.running() {
            handle.close();
        }
        self.sink.send(Out::Exit);
    }

    /// End everything and wait, for at most three seconds: the layout is written, every shell is closed and joined.
    /// Safe to call more than once, and from the exit events of the app. Called by `Drop` too.
    pub fn shutdown(&self) {
        // First: panes that end from now on are not taken out of the layout.
        self.exiting.store(true, Ordering::SeqCst);
        self.saver.flush();
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
        let spec = self.pane_spec(cols, rows, (clamp_cell(cell_w), clamp_cell(cell_h)));
        // What the shell is started with; a settings change while it starts is pushed once it runs.
        let started_with = PaneLook {
            theme: spec.theme.unwrap_or_default(),
            option_as_alt: spec.option_as_alt,
        };

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
                    Some(slot @ Slot::Starting) => {
                        *slot = Slot::Running(handle.clone());
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
                Ok(())
            }
            // The sink was told, and `pane_gone` forgot the slot, before `spawn_ghostty` returned.
            Err(e) => Err(e.to_string()),
        }
    }

    /// The shell of a pane ended, or never started (called from the pane's sink). Forgets the handle, takes the pane
    /// out of the layout and tells the webview. A pane the user already closed is `Unknown` to the layout by now, so
    /// this cannot end the app a second time (RESEARCH Pitfall 4); only the last pane of the layout ends it.
    fn pane_gone(&self, pane: PaneId, code: Option<u32>) {
        let (slot, outcome) = {
            let mut layout = lock(&self.layout);
            let slot = lock(&self.panes).remove(&pane);
            let outcome = if self.exiting.load(Ordering::SeqCst) {
                // The app is ending: what the user had stays in the layout.
                None
            } else {
                match layout.close_pane(pane) {
                    CloseOutcome::Closed => Some((CloseOutcome::Closed, Some(View::of(&layout)))),
                    other => Some((other, None)),
                }
            };
            (slot, outcome)
        };
        drop(slot);

        let mut last = false;
        match outcome {
            Some((CloseOutcome::Closed, Some(view))) => {
                self.saver.touch();
                self.sink.send(Out::Layout(view));
            }
            Some((CloseOutcome::LastPane, _)) => last = true,
            _ => {}
        }
        self.sink.send(Out::PaneExit { pane, code });
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

impl Drop for AppCore {
    fn drop(&mut self) {
        self.shutdown();
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
    // ---- settings, themes and key bindings (plan 23, task 1) -----------------------------------------------------

    use holzwave_core::keybindings::actions;
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
        fn has_full_frame_with_bg(&self, pane: PaneId, skip: usize, bg: u32) -> bool {
            self.frames(pane).iter().skip(skip).any(|f| f.full && f.bg == bg)
        }
    }

    fn packed(c: Rgb) -> u32 {
        u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
    }

    /// A core over an existing scratch directory, so that a test can put files there first.
    fn core_in(dir: &Scratch, program: &str, args: &[&str]) -> (Arc<AppCore>, Arc<Recorder>) {
        let rec = Arc::new(Recorder::default());
        let core = AppCore::new(dir.0.clone(), rec.clone(), ShellSpec::new(program, args));
        (core, rec)
    }

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
        assert!(view.keybindings.iter().all(|row| row.is_default && row.chord.is_some()));
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

        assert_eq!(view.settings.font_size, 40, "the number is clamped");
        let said = |needle: &str| view.warnings.iter().any(|w| w.contains(needle));
        assert!(said("font_size"), "{:?}", view.warnings);
        assert!(said("broken.json"), "{:?}", view.warnings);
        assert!(said("pane.close"), "the refused chord is named: {:?}", view.warnings);
        assert!(said("no.such.action"), "{:?}", view.warnings);
        let close = view.keybindings.iter().find(|r| r.action == "pane.close").expect("row");
        assert_eq!(close.chord.as_deref(), Some("Cmd+W"), "the refused override leaves the default");
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

        let view = core
            .set_setting("keybindings", json!({"pane.close": "Cmd+Shift+W", "tab.new": "x", "pane.split_right": ""}))
            .expect("the map has the right type, so it is accepted; the resolver reports what it refuses");

        let row = |action: &str| view.keybindings.iter().find(|r| r.action == action).cloned().expect("row");
        assert_eq!(row("pane.close").chord.as_deref(), Some("Cmd+Shift+W"));
        assert!(!row("pane.close").is_default);
        assert_eq!(row("tab.new").chord.as_deref(), Some("Cmd+T"), "a plain key is refused, the default stays");
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
    fn two_directory_script(marker: &Path, a: &Path, b: &Path) -> String {
        format!(
            "echo \"start:$(pwd)\"; if mkdir '{m}' 2>/dev/null; then d='{a}'; else d='{b}'; fi; cd \"$d\"; \
             printf '\\033]7;file://localhost%s\\007' \"$d\"; sleep 30",
            m = marker.display(),
            a = a.display(),
            b = b.display()
        )
    }

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

        core.start_pane(pane, 80, 24, 0, 0).expect("starts");

        let needle = format!("in:{}", saved.display());
        assert!(wait_until(WAIT, || rec.has_text(pane, &needle)), "the shell did not start in {saved:?}");
    }

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

    #[test]
    fn a_split_starts_in_the_last_known_directory_of_the_pane_that_was_focused() {
        let dir = Scratch::new("split-cwd");
        let (a, b) = (real_dir(&dir, "a"), real_dir(&dir, "b"));
        let script = two_directory_script(&dir.0.join("marker"), &a, &b);
        let (core, rec) = core_in(&dir, "/bin/sh", &["-c", &script]);
        let first = only_pane(&core);
        core.start_pane(first, 80, 24, 0, 0).expect("starts");
        let a_text = a.display().to_string();
        assert!(wait_until(WAIT, || rec.cwds(first).contains(&a_text)), "no cwd event: {:?}", rec.cwds(first));

        let second = core.split(false).focus;
        assert_ne!(second, first);
        core.start_pane(second, 80, 24, 0, 0).expect("starts");

        let needle = format!("start:{a_text}");
        assert!(wait_until(WAIT, || rec.has_text(second, &needle)), "the split did not start in {a_text}");
    }

    #[test]
    fn a_new_tab_starts_in_the_last_known_directory_of_the_pane_that_was_focused() {
        let dir = Scratch::new("tab-cwd");
        let (a, b) = (real_dir(&dir, "a"), real_dir(&dir, "b"));
        let script = two_directory_script(&dir.0.join("marker"), &a, &b);
        let (core, rec) = core_in(&dir, "/bin/sh", &["-c", &script]);
        let first = only_pane(&core);
        core.start_pane(first, 80, 24, 0, 0).expect("starts");
        let a_text = a.display().to_string();
        assert!(wait_until(WAIT, || rec.cwds(first).contains(&a_text)), "no cwd event");

        let tab_pane = core.new_tab().focus;
        assert_ne!(tab_pane, first);
        core.start_pane(tab_pane, 80, 24, 0, 0).expect("starts");

        let needle = format!("start:{a_text}");
        assert!(wait_until(WAIT, || rec.has_text(tab_pane, &needle)), "the new tab did not start in {a_text}");
    }

    #[test]
    fn a_directory_saved_for_the_new_pane_wins_over_the_one_it_would_inherit() {
        let (dir, first) = dir_with_one_pane("saved-wins");
        // The id the split will give: the same layout split on a copy.
        let (layout, _) = Layout::new();
        let mut probe = layout.clone();
        probe.split_focused(Axis::Row);
        let predicted = probe.tab().focus;
        let saved = real_dir(&dir, "saved");
        let inherited = real_dir(&dir, "inherited");
        std::fs::write(
            dir.0.join("session.json"),
            format!(r#"{{"version": 1, "cwds": {{"{predicted}": {:?}}}}}"#, saved.to_str().unwrap()),
        )
        .unwrap();
        let (core, _rec) = core_in(&dir, "/bin/cat", &[]);
        lock(&core.last_cwd).insert(first, inherited.display().to_string());

        let second = core.split(false).focus;

        assert_eq!(second, predicted);
        assert_eq!(core.start_dir(second), Some(saved));
    }

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

    #[test]
    fn a_pane_that_reported_nothing_is_asked_for_its_directory_when_the_session_is_written() {
        let (dir, pane) = dir_with_one_pane("session-process");
        let (core, _rec) = core_in(&dir, "/bin/sh", &["-c", "sleep 30"]);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");

        core.shutdown();

        let expected = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let session = session_on_disk(&dir);
        let got = session.cwds.get(&pane).map(std::fs::canonicalize);
        assert_eq!(got.and_then(Result::ok), Some(expected), "{:?}", session.cwds);
    }

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
        assert_eq!(core.placement(&monitors).size, holzwave_core::session::DEFAULT_SIZE, "nothing saved yet");

        core.note_window(WindowState { x: 100, y: 80, width: 1200, height: 700, maximized: false });
        let on_screen = core.placement(&monitors);
        core.note_window(WindowState { x: 5000, y: 80, width: 1200, height: 700, maximized: true });
        let off_screen = core.placement(&monitors);
        core.flush();

        assert_eq!(on_screen.position, Some((100, 80)));
        assert_eq!(on_screen.size, (1200, 700));
        assert_eq!(off_screen.position, None, "a position that is on no monitor is dropped");
        assert_eq!(off_screen.size, (1200, 700), "the size is still applied");
        assert!(off_screen.maximized);
        assert_eq!(core.placement(&[]).position, None, "with no monitor known the system places the window");
        assert_eq!(session_on_disk(&dir).window.map(|w| (w.x, w.width)), Some((5000, 1200)));
    }

    #[test]
    fn a_window_far_outside_the_range_is_clamped_before_it_is_stored() {
        let (core, _rec, dir) = core("/bin/cat", &[]);

        core.note_window(WindowState { x: i32::MAX, y: i32::MIN, width: 7, height: u32::MAX, maximized: false });
        core.flush();

        let window = session_on_disk(&dir).window.expect("stored");
        assert_eq!((window.x, window.y), (32768, -32768));
        assert_eq!((window.width, window.height), (200, 16384));
    }

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
        CopyMotionMsg, CopyOpMsg, CopyStateMsg, PasteOutcomeMsg, SearchOpMsg, SelectModeMsg, SelectOpMsg,
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
        /// The argument names in order, State and AppHandle parameters left out.
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
                    if ty.starts_with("State<") || ty.contains("AppHandle") {
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
        let view = View::of(&layout);
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
            &Keybindings::default(),
            vec!["settings.json: font_size 9999 is outside 8..=40; 40 is used".into()],
        );
        let events = [
            Out::Frame(frame),
            Out::Layout(view.clone()),
            Out::Title { pane: 3, title: "vim README.md".into() },
            Out::Cwd { pane: 3, cwd: "/Users/holz/Projects/holzWave".into() },
            Out::PaneExit { pane: 3, code: Some(0) },
            Out::Settings(settings_view.clone()),
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
            },
            "settings_view": to_value(&settings_view),
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

        check(|doc| doc["commands"].clone(), "the command argument names");
    }

    #[test]
    fn the_scanner_reads_generics_and_skips_state_and_handles() {
        let source = r#"
            #[tauri::command(async, rename_all = "snake_case")]
            pub fn demo(core: State<Arc<AppCore>>, app: AppHandle, pane: PaneId, map: HashMap<String, Vec<u8>>) -> Result<(), String> {
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
