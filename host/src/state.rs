//! The behaviour of the app shell, without Tauri (D-19).
//!
//! [`AppCore`] owns the layouts of all windows and the pane handles and answers every command of the webview. It
//! knows windows by their label and nothing else of them: what a webview has to be told leaves through a [`Sink`] as
//! an [`Out`] with the [`Target`] it is for, and `lib.rs` maps those to Tauri events. Opening, closing and finding
//! windows are calls on the sink too. That is what makes the shell testable headless, with a recording sink (that
//! can refuse to open a window) and real shells.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

use holzwave_core::ai::service::AskEvent;
use holzwave_core::blocks::{Block, BlockKind, BlockView, Blocks, Grant, LOCAL, Opened, Saved};
use holzwave_core::conn::ConnState;
use holzwave_core::connections::{ConnView, Connections, ProfileView, RemoteConn, SecretChange, home_dir as user_home};
use holzwave_core::files::{Entry, FileSource, Listing};
use holzwave_core::integrate::run::Launch as PaneLaunch;
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
use holzwave_core::profiles::{Kind as ConnectionKind, Profile};
use holzwave_core::secrets::SecretStore;
use holzwave_core::platform::{
    Os, ProgramCheck, ShellLaunch, ShellResolved, host_shell_launch, in_flatpak, resolve_shell,
};
use holzwave_core::pty::process_cwd;
use holzwave_core::session::{MonitorRect, Placement, SESSION_FILE, Session, WindowState};
use holzwave_core::settings::{SETTINGS_FILE, Settings};
use holzwave_core::ssh::{ExecCancel, ExecLimits, ExecResult, PromptLine, SshExec};
use holzwave_core::wshinstall::Bundled;
use holzwave_core::theme::{DEFAULT_THEME, THEMES_DIR, Theme, ThemeInfo, ThemeRegistry};
use holzwave_core::wire::{FrameMsg, IMAGE_CHUNK_BYTES, image_data_bytes};
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::ai_host::{AiHost, AiParts, AiPayload};
use crate::commands::wire::GrantCode;
use crate::ai_exec::RemoteExecs;
use crate::login_prompt::{PromptEnd, PromptHost};
use crate::ai_tools::{ExecFault, FileTarget, TerminalTarget};
use crate::integrate_host::{
    ConfirmAnswer, ConfirmKind, ContextAnswer, IntegrationEvent, IntegrationFault, IntegrationHost, IntegrationParts,
    LaunchAnswer, LaunchNote,
};
use crate::seams::{Host, NeutralHost, Platform, PrivateDirs};
use crate::sessions::{ServerParts, SessionFault, SessionRow};
use crate::windows::{CursorSpot, DropKind, Drops, Hovers, Place};

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

/// The most time `stop_all` spends waiting for the shells to end. A shell that ignores a hangup is killed by the pane's own
/// close path after its short grace period, so this is a bound for a pane that cannot be reached at all.
const STOP_ALL_BOUND: Duration = Duration::from_secs(2);

/// How many shells and how many connections these pane states are, in that order. A pane on this computer
/// ([`ConnState::Local`]) is a shell; a pane whose saved connection is up ([`ConnState::Connected`]) or is making the
/// connection again after a loss ([`ConnState::Reconnecting`], the session is still the person's and the process must
/// stay alive for it) is a connection. A connection that is being made for the first time, that failed or that ended is
/// neither: nothing lives yet or any more. Pure, so that every state has its answer in a test.
pub fn count_sessions(states: impl IntoIterator<Item = ConnState>) -> (usize, usize) {
    let mut counts = (0, 0);
    for state in states {
        match state {
            ConnState::Local => counts.0 += 1,
            ConnState::Connected | ConnState::Reconnecting { .. } => counts.1 += 1,
            ConnState::Connecting | ConnState::Failed | ConnState::Closed => {}
        }
    }
    counts
}

/// Keeps `AppCore::stopping` raised for the length of one `stop_all`, also when it unwinds.
struct Stopping<'a>(&'a AtomicUsize);

impl<'a> Stopping<'a> {
    fn raise(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for Stopping<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The bytes in one MiB of the `image_memory_mib` setting.
const IMAGE_MIB_BYTES: u64 = 1024 * 1024;

/// `path` with `bin` in front, when `wsh` is in `bin` and `bin` is not on `path` yet; otherwise `None`.
fn path_with_wsh(bin: &Path, path: Option<&std::ffi::OsStr>) -> Option<std::ffi::OsString> {
    let name = if cfg!(windows) { "wsh.exe" } else { "wsh" };
    if !bin.join(name).is_file() {
        return None;
    }
    let mut dirs: Vec<PathBuf> = path.map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    if dirs.iter().any(|d| d == bin) {
        return None;
    }
    dirs.insert(0, bin.to_path_buf());
    std::env::join_paths(dirs).ok()
}

/// The saved connections, and the keychain entries of the profiles that a list from another computer removed: those
/// profiles are gone from the list just applied, so their secrets go too. Only exactly those ids are touched (and none
/// when the new list did not read as one: the sync reports no ids then), see `Promoted::dropped_profiles`.
fn load_connections(
    dir: &Path,
    secrets: Arc<dyn SecretStore>,
    home: Option<PathBuf>,
    live_panes: &[PaneId],
    dropped: &[String],
) -> (Connections, holzwave_core::layout::LoadStatus) {
    let (connections, status) = Connections::load(dir, secrets, home, live_panes);
    connections.forget_secrets(dropped);
    (connections, status)
}

/// Whether `path` is an absolute path of this computer that names nothing (a link that points nowhere counts): the one
/// case of "not found" that is known without reading the core's text.
fn absent_here(path: &str) -> bool {
    let path = Path::new(path);
    path.is_absolute() && matches!(std::fs::metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

// ---- what the webview sees -------------------------------------------------------------------------------------

/// What the sidebar's widgets need.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WidgetsView {
    pub widgets: Vec<holzwave_core::widgets::WidgetDef>,
    pub notes: std::collections::BTreeMap<String, String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WorkspaceView {
    pub name: String,
    pub active: bool,
    /// How many panes the tabs of this workspace hold together (the × of a chip says what it closes).
    pub panes: usize,
    /// The ids of those panes: the page maps a pane to its workspace pill with them (the attention marks, design 6.23).
    pub pane_ids: Vec<PaneId>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TabView {
    pub title: String,
    pub active: bool,
    /// The pane of this tab that has the focus.
    pub focus: PaneId,
    /// How many panes the tab holds.
    pub panes: usize,
    /// The ids of those panes: the page maps a pane to its tab with them (the attention marks, design 6.23).
    pub pane_ids: Vec<PaneId>,
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
    /// How many windows the app had when this view was made: the confirmation of a close button says whether closing
    /// everything in this window ends the app (the only window) or the window. Only a view of this window is sent to
    /// it, so the number can lag behind a window that closed meanwhile; the next view corrects it.
    pub windows: usize,
}

impl View {
    /// The view of `layout`, stamped `rev`, in an app with `windows` windows. Take `rev` with [`AppCore::view_of`],
    /// under the layout lock.
    pub fn of(layout: &Layout, rev: u64, windows: usize) -> View {
        let (panes, dividers) = layout.tab().geometry();
        let active_tab = layout.workspace().active;
        View {
            rev,
            workspaces: layout
                .workspaces
                .iter()
                .enumerate()
                .map(|(i, w)| WorkspaceView {
                    name: w.name.clone(),
                    active: i == layout.active,
                    panes: w.tabs.iter().map(|t| t.panes().len()).sum(),
                    pane_ids: w.tabs.iter().flat_map(|t| t.panes()).collect(),
                })
                .collect(),
            tabs: layout
                .workspace()
                .tabs
                .iter()
                .enumerate()
                .map(|(i, t)| TabView {
                    title: t.title.clone(),
                    active: i == active_tab,
                    focus: t.focus,
                    panes: t.panes().len(),
                    pane_ids: t.panes(),
                })
                .collect(),
            focus: layout.tab().focus,
            pane_names: panes
                .iter()
                .filter_map(|(pane, _)| layout.pane_name(*pane).map(|name| (*pane, name.to_string())))
                .collect(),
            panes,
            dividers,
            windows,
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
    fn new(
        settings: Settings,
        themes: ThemeRegistry,
        file_notes: Vec<String>,
        stamps: Stamps,
        chord_table: &str,
    ) -> Config {
        let (keybindings, binding_notes) = Keybindings::resolve_for(chord_table, &settings.keybindings);
        Config { settings, themes, keybindings, file_notes, binding_notes, stamps }
    }

    /// Read `settings.json` and `themes/` of `dir`. Never fails: what cannot be used is reported as a note.
    fn load(dir: &Path, chord_table: &str) -> Config {
        // Stamped first: a file that changes while it is read is then seen as changed by the next reload.
        let stamped = stamps(dir);
        let (settings, status) = Settings::load(dir);
        let mut notes = status_notes(SETTINGS_FILE, status);
        let (themes, theme_notes) = ThemeRegistry::load(dir);
        notes.extend(theme_notes);
        Config::new(settings, themes, notes, stamped, chord_table)
    }

    /// Replace the settings and resolve the key bindings again.
    fn set_settings(&mut self, settings: Settings, chord_table: &str) {
        let (keybindings, binding_notes) = Keybindings::resolve_for(chord_table, &settings.keybindings);
        self.settings = settings;
        self.keybindings = keybindings;
        self.binding_notes = binding_notes;
    }

    /// The view of the settings. `on_host` is whether new panes start on the host (a Flatpak with Host shell on): then the
    /// `shell` setting names a program of the host and is not looked up here.
    fn view(&self, on_host: bool) -> SettingsView {
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
            warnings.extend(resolve_system_shell(&ShellSpec::System, &self.settings.shell, on_host).unusable);
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
    /// The program in `pane` rang the bell (BEL). At most one per half second per pane (the core's gate). Goes to the
    /// window that has the pane; the page marks the tab and the pill that hold the pane when it is out of view (design 6.23).
    Bell { pane: PaneId },
    /// A command ended in `pane`: the `D` mark of OSC 133 with its exit status, `None` when the mark carried none. At
    /// most one per half second per pane. Goes to the window that has the pane (design 6.23).
    CommandEnd { pane: PaneId, code: Option<u32> },
    /// The settings, the theme or the key bindings changed (a command, or a hand edit that was picked up).
    Settings(SettingsView),
    /// A pane or a tab that was released over this window comes from another window: where in this page it goes is
    /// for this page to say. `x` and `y` are the pointer in client coordinates (CSS pixels). The page answers with
    /// `attach_pane` or `attach_tab`, or with nothing when the pointer is over no drop zone. `pane` is the pane that
    /// was released (for a tab, one of its panes): the answer has to name it, and this page has never heard of it.
    IncomingDrop { kind: DropKind, pane: PaneId, x: f64, y: f64 },
    /// A pane or tab that is being dragged out of another window is over this one: the page shows the outline of the
    /// zone it would land in at the pointer (`x`, `y`, client coordinates in CSS pixels, the same as `IncomingDrop`).
    /// Nothing moves before the release; sent again whenever the pointer moves.
    DragHover { kind: DropKind, pane: PaneId, x: f64, y: f64 },
    /// The drag that was over this window left it, ended (released anywhere, Escape) or lost its window: the page
    /// takes the outline away.
    DragEnd,
    /// One event of a chat turn, for every window: the page routes it by conversation and rebuilds the chat from the
    /// `seq` of the turn, or from a snapshot when one is missing.
    Ai(AiPayload),
    /// A request from a shell waits for the person, or is over: the page draws or removes a card. `Waiting` goes to the
    /// window of the pane that asked, `Closed` to every window.
    AiAsk(AskEvent),
    /// The shell of `pane` asked for the history search (the binding added by `wsh init`): the page of the window that has
    /// the pane opens the dialog for it.
    HistoryOpen { pane: PaneId },
    /// The server of a saved connection asks the person a question to log in (a one-time code, a PAM prompt). Goes to every
    /// window; the page draws a dialog. `connection` and `host` are the name and the host of the person's own saved
    /// connection, never what the server says; `name`, `instructions` and the texts of `prompts` are the server's text,
    /// already cleaned and bounded by the core, and are shown as text only.
    LoginPrompt { id: String, connection: String, host: String, name: String, instructions: String, prompts: Vec<PromptLine> },
    /// The question `id` is over (answered, cancelled or timed out): every window takes its dialog away.
    LoginPromptEnd { id: String, outcome: PromptEnd },
    /// Something the integrations show has changed (a selection, the tools, the profiles, a pane that holds the last words
    /// of its tool). Small, for every window: the page asks for the data the word names.
    Integration(IntegrationEvent),
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
            Out::Bell { .. } => "bell",
            Out::CommandEnd { .. } => "command-end",
            Out::Settings(_) => "settings",
            Out::IncomingDrop { .. } => "incoming-drop",
            Out::DragHover { .. } => "drag-hover",
            Out::DragEnd => "drag-end",
            Out::Ai(_) => "ai",
            Out::AiAsk(_) => "ai-ask",
            Out::HistoryOpen { .. } => "history-open",
            Out::LoginPrompt { .. } => "login-prompt",
            Out::LoginPromptEnd { .. } => "login-prompt-end",
            Out::Integration(_) => "integration",
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
struct BellPayload {
    pane: PaneId,
}

#[derive(Serialize)]
struct CommandEndPayload {
    pane: PaneId,
    code: Option<u32>,
}

#[derive(Serialize)]
struct HistoryOpenPayload {
    pane: PaneId,
}

#[derive(Serialize)]
struct PromptLinePayload<'a> {
    text: &'a str,
    echo: bool,
}

#[derive(Serialize)]
struct LoginPromptPayload<'a> {
    id: &'a str,
    connection: &'a str,
    host: &'a str,
    name: &'a str,
    instructions: &'a str,
    prompts: Vec<PromptLinePayload<'a>>,
}

#[derive(Serialize)]
struct LoginPromptEndPayload<'a> {
    id: &'a str,
    outcome: PromptEnd,
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
            Out::Bell { pane } => BellPayload { pane: *pane }.serialize(serializer),
            Out::CommandEnd { pane, code } => CommandEndPayload { pane: *pane, code: *code }.serialize(serializer),
            Out::Settings(view) => view.serialize(serializer),
            Out::IncomingDrop { kind, pane, x, y } | Out::DragHover { kind, pane, x, y } => {
                IncomingDropPayload { kind: *kind, pane: *pane, x: *x, y: *y }.serialize(serializer)
            }
            Out::DragEnd => serializer.serialize_unit(),
            Out::Ai(payload) => payload.serialize(serializer),
            Out::AiAsk(event) => event.serialize(serializer),
            Out::HistoryOpen { pane } => HistoryOpenPayload { pane: *pane }.serialize(serializer),
            Out::LoginPrompt { id, connection, host, name, instructions, prompts } => LoginPromptPayload {
                id,
                connection,
                host,
                name,
                instructions,
                prompts: prompts.iter().map(|p| PromptLinePayload { text: &p.text, echo: p.echo }).collect(),
            }
            .serialize(serializer),
            Out::LoginPromptEnd { id, outcome } => LoginPromptEndPayload { id, outcome: *outcome }.serialize(serializer),
            Out::Integration(event) => event.serialize(serializer),
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
    /// A fixed program with arguments, for the tests of both crates.
    pub fn new(program: impl Into<String>, args: &[&str]) -> Self {
        Self::Fixed { program: program.into(), args: args.iter().map(|a| (*a).to_owned()).collect() }
    }

    /// The program and arguments of a pane's shell, and the reason when the `shell` setting could not be used and the
    /// platform default stands in. `configured` is the `shell` setting; `os`, `env`, `exists` and `check` are the
    /// facts [`resolve_shell`] decides from, passed in so that every platform is tested on any host. `on_host` is true only
    /// when this is a Flatpak whose person turned Host shell on and which has the permission
    /// ([`holzwave_core::host_shell::State::On`]); in every other case, a Flatpak without it included, the shell is
    /// resolved where the process runs, as on any Linux.
    fn launch(
        &self,
        configured: &str,
        os: Os,
        env: &dyn Fn(&str) -> Option<String>,
        exists: &dyn Fn(&str) -> bool,
        check: &dyn Fn(&str) -> ProgramCheck,
        on_host: bool,
    ) -> ShellResolved {
        match self {
            // On the host (a Flatpak with Host shell on) the shell is the host's: its program is not in the sandbox, so the
            // setting is not looked up here and nothing is wrong with a program the sandbox cannot see
            // (`PaneSpec::host_spawn`).
            Self::System if on_host && os == Os::Linux && in_flatpak(env, exists) => {
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
fn resolve_system_shell(spec: &ShellSpec, configured: &str, on_host: bool) -> ShellResolved {
    spec.launch(
        configured,
        Os::current(),
        &|key| std::env::var(key).ok(),
        &|path| Path::new(path).is_file(),
        &check_program_on_disk,
        on_host,
    )
}

impl Default for ShellSpec {
    /// The system shell: `$SHELL -l` on macOS and Linux, PowerShell or cmd on Windows, or the `shell` setting.
    fn default() -> Self {
        Self::System
    }
}

/// The folder a file path is in: the path up to its last separator, `/` for a name directly under the root, `None` for
/// a bare name.
fn parent_folder(path: &str) -> Option<String> {
    let cut = path.rfind(['/', '\\'])?;
    Some(if cut == 0 { path[..1].to_owned() } else { path[..cut].to_owned() })
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

/// The file that remembers, per pane that was started from a terminal launch, what it was called. Machine-local, and
/// read with a size limit like every file of the app.
const LAUNCHES_FILE: &str = "pane_launches.json";
/// The most bytes of `pane_launches.json` that are read or written.
const MAX_LAUNCH_MARKS_BYTES: u64 = 16 * 1024;
/// The most panes `pane_launches.json` names.
const MAX_LAUNCH_MARKS: usize = 256;
/// The most characters of a pane's label that are kept.
const MAX_LAUNCH_LABEL: usize = 64;

/// A launch's label as it is kept and shown: control characters removed, the ends trimmed, at most
/// [`MAX_LAUNCH_LABEL`] characters. An empty result is `a tool`.
fn clean_launch_label(label: &str) -> String {
    let cleaned: String = label.chars().filter(|c| !c.is_control()).collect();
    let cut: String = cleaned.trim().chars().take(MAX_LAUNCH_LABEL).collect();
    let cut = cut.trim_end();
    if cut.is_empty() { "a tool".to_owned() } else { cut.to_owned() }
}

/// The text of `pane_launches.json` for these markers: a map from the pane's number to its label.
fn launch_marks_json(marks: &BTreeMap<PaneId, String>) -> String {
    let map: BTreeMap<String, &String> = marks.iter().map(|(pane, label)| (pane.to_string(), label)).collect();
    serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_owned())
}

/// Write the markers. A failure is logged without any label.
fn write_launch_marks(dir: &Path, marks: &BTreeMap<PaneId, String>) {
    if let Err(e) = persist::write_atomic(&dir.join(LAUNCHES_FILE), launch_marks_json(marks).as_bytes()) {
        eprintln!("holzwave: cannot save {LAUNCHES_FILE}: {e}");
    }
}

/// The markers of `pane_launches.json` that name a pane in `live`, labels cleaned again (the file is not trusted). A file
/// that is missing, too large, not a map of strings or damaged gives none and is logged without its content; markers of
/// panes that are not in a layout are dropped and the file is written again without them.
fn load_launch_marks(dir: &Path, live: &[PaneId]) -> BTreeMap<PaneId, String> {
    let path = dir.join(LAUNCHES_FILE);
    let bytes = match persist::read_limited(&path, MAX_LAUNCH_MARKS_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return BTreeMap::new(),
        Err(e) => {
            eprintln!("holzwave: {LAUNCHES_FILE} was ignored ({:?})", e.kind());
            return BTreeMap::new();
        }
    };
    let map = match serde_json::from_slice::<BTreeMap<String, String>>(&bytes) {
        Ok(map) => map,
        Err(e) => {
            // The class of the error only: the message of a parser can quote the text.
            eprintln!("holzwave: {LAUNCHES_FILE} was ignored ({:?})", e.classify());
            return BTreeMap::new();
        }
    };
    let total = map.len();
    let marks: BTreeMap<PaneId, String> = map
        .into_iter()
        .filter_map(|(pane, label)| pane.parse::<PaneId>().ok().map(|pane| (pane, label)))
        .filter(|(pane, _)| live.contains(pane))
        .take(MAX_LAUNCH_MARKS)
        .map(|(pane, label)| (pane, clean_launch_label(&label)))
        .collect();
    if marks.len() != total {
        write_launch_marks(dir, &marks);
    }
    marks
}

/// The state of the app: the layouts of every window, the running panes, and what is needed to start a pane.
///
/// Lock order: `windows` first, then `panes` or `routes`; `panes` and `routes` are never held while waiting for
/// `windows`, and never together with each other. `flush` runs the save job, which locks `windows`, so it is never
/// called with `windows` held. The sink is never called with `windows` held, except `send`, which only queues.
pub struct AppCore {
    /// What the shell decides: the platform id, the store of every secret, the Local Network source, the text scale and
    /// the caps (see [`crate::seams`]).
    host: Arc<dyn Host>,
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
    /// Serialises the writers of `widget_notes.json`: a note save and the apply of a download of the file. Held across
    /// the file IO and nothing else: never across `reload_settings`, a sink call or a window call.
    notes_io: Mutex<()>,
    /// The last working directory each pane reported (its `Cwd` events).
    last_cwd: Arc<Mutex<HashMap<PaneId, String>>>,
    /// The last title each pane reported. The core sends a title when a program changes it and never again, so a
    /// pane that arrives in another window would show none: both this and the directory are replayed to it.
    last_title: Mutex<HashMap<PaneId, String>>,
    /// The drop of a pane or tab that is on its way from one window to another.
    drops: Mutex<Drops>,
    /// The window a drag in progress is over (its outline is shown there).
    hovers: Mutex<Hovers>,
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
    /// The saved SSH, WSL and Docker connections and the panes that run them.
    connections: Connections,
    /// The panes that show a file browser or a preview instead of a terminal.
    blocks: Blocks,
    /// The terminal launches registered for panes that have not started yet: `start_pane` takes the entry of its pane
    /// and starts that tool in place of the shell. The entry holds secrets, so it is dropped as soon as it is applied.
    launches: Mutex<HashMap<PaneId, PaneLaunch>>,
    /// What each pane that was started from a launch was called, kept in `pane_launches.json` while the pane lives. A
    /// pane in this table at the next start was a tool session, which is not started again (see [`LAUNCHES_FILE`]). A leaf
    /// lock: held across the write of the file and nothing else.
    launch_marks: Mutex<BTreeMap<PaneId, String>>,
    /// The panes that hold their tool's last words on the screen until the person presses Enter, with the status of the
    /// tool when there was one ([`PaneEvent::Held`]). Cleared by `pane_gone`.
    held: Mutex<BTreeMap<PaneId, Option<u32>>>,
    /// Where `wsh` finds the app, set once the socket listens: the folder and the secret of this run.
    ipc: Mutex<Option<(PathBuf, String)>>,
    /// Where the saved SSH connection of each running pane pointed when the pane started, so that a connection edited
    /// afterwards to another host is never answered for as the old one (see [`crate::wsh_tunnel::Target`]).
    pane_targets: Mutex<HashMap<PaneId, crate::wsh_tunnel::Target>>,
    /// The resource folder of the app, where the bundled `wsh` builds for Linux hosts are (set once at start; none in a test
    /// and in a build made without them).
    resource_dir: Mutex<Option<PathBuf>>,
    /// The typed commands, searchable. An empty in-memory one when the file cannot be opened.
    history: holzwave_core::history::History,
    /// The system monitor's sampler, shared by every monitor pane.
    monitor: holzwave_core::monitor::Monitor,
    /// The sessions of the other computers that monitor panes show: one per saved connection, built at the first sample
    /// and dropped when the last monitor pane of the connection closes.
    remote_monitors: holzwave_core::monitor::RemoteMonitors,
    /// The encrypted sync: its account, its runs and what the page shows of them.
    sync: crate::sync::SyncRuntime,
    /// The link of this device to a holzWave server: its address, its token in the keychain and the sign-in that gets one.
    server: Arc<crate::server::ServerRuntime>,
    /// The sessions of a server (see [`crate::sessions`]). The desktop's are those of the server this device is signed in to;
    /// a build whose sessions live somewhere else passes its own.
    sessions: Arc<dyn crate::sessions::SessionService>,
    /// Which pane of the layout shows which session of a server. Kept per device and never synced.
    remote_panes: crate::sessions::RemotePanes,
    /// The AI chat service and the calls the commands make on it. It owns its threads and takes no lock of the core.
    ai: AiHost,
    /// The Local Network permission of macOS: the person's stored choice and the probe of the system. Inert elsewhere.
    local_network: crate::local_network::LocalNetworkHost,
    /// Whether a Flatpak starts a pane on the host or in the sandbox: the person's stored choice and the permission of this
    /// Flatpak. Inert outside a Flatpak.
    host_shell: crate::host_shell::HostShellHost,
    /// The frame and button choices of the window on Windows and Linux: the stored choice and the one the running windows
    /// were built with. Inert elsewhere.
    window_frame: crate::window_frame::WindowFrameHost,
    /// The exec primitive of each saved connection a command ran on, kept for the life of the connection list.
    remote_execs: RemoteExecs<SshExec>,
    /// The questions of keyboard-interactive logins that wait for the person (see [`crate::login_prompt`]). Also the
    /// prompter of `connections`.
    prompts: Arc<PromptHost>,
    /// The integration service (Kubernetes, Talos, Azure and GAM) and what the shell keeps beside it.
    integrations: IntegrationHost,
    /// Writes the layout 250 ms after the first change of a burst and when flushed.
    saver: Debounced,
    /// How many times the saver job wrote the file, for tests.
    writes: Arc<AtomicUsize>,
    /// The app is ending (the last pane closed or ended, or `shutdown` ran): panes that end from now on stay in the
    /// layout, and windows that close from now on stay in it too, so that what is saved is what the user had.
    exiting: AtomicBool,
    /// How many [`AppCore::stop_all`] calls are running. While one is, a shell that ends stays in the layout (as when the
    /// app ends) and is told to its window as an ended pane, so that the layout, the windows and the app survive it.
    stopping: AtomicUsize,
    /// Whether animated pictures may move: the page's reduced-motion fact (`set_image_animation`), applied to every
    /// running pane and given to each pane that starts. It is the person's window that asks for reduced motion, not a
    /// pane, so there is one value for the whole core; it starts `true`.
    image_animation: AtomicBool,
    /// The `rev` of the last view built. Counted under the windows lock, so the order of the revs is the order of the
    /// states of the layouts.
    view_rev: AtomicU64,
    /// The panes that run outside the layout and count against the same cap ([`AppCore::take_outside_pane`]): the sessions of
    /// a server, which no window shows. A place is taken with the layouts locked, so a layout and a session cannot both take
    /// the last place.
    outside_panes: AtomicUsize,
    me: Weak<AppCore>,
}

/// A place under the pane cap for a pane that is in no window ([`AppCore::take_outside_pane`]). The place is given back
/// when this is dropped.
pub struct OutsidePane {
    core: Arc<AppCore>,
}

impl Drop for OutsidePane {
    fn drop(&mut self) {
        // Giving a place back needs no lock: a check that still sees it taken only refuses a little early.
        self.core.outside_panes.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What the single-answer path says to `wsh ai`: the request is answered as a stream (see `ai_ask`), never as one line.
const WSH_AI_STREAM_ONLY: &str = "wsh ai is answered as a stream, not as one line";

/// What a remote `wsh` is told when its pane has gone, and when its connection has gone or was turned off. Fixed
/// sentences: a request that came through a forward is answered with nothing of this computer in it.
const WSH_PANE_GONE: &str = "this pane is not open any more";
const WSH_NO_CONNECTION: &str = "this pane no longer runs a saved connection";
const WSH_TURNED_OFF: &str = "wsh is not turned on for the connection of this pane";
const WSH_NOT_ABSOLUTE: &str = "the path must be absolute: it starts with /";
const WSH_NOT_A_FILE: &str = "not a regular file on this host";
const WSH_NO_SUCH_FILE: &str = "no such file on this host, or it cannot be read";
/// What a file browser, a preview or a monitor is told when the layout cannot hold another pane.
const NO_ROOM_FOR_A_PANE: &str = "there is no room for another pane here";
const WSH_NO_HOST: &str = "the connection of this pane could not be used";
/// What a remote `wsh` is told when the saved connection of its pane was edited to point at another host after the pane
/// started: the pane is of the old host, so nothing is answered for it on the new one.
const WSH_TARGET_CHANGED: &str = "the saved connection of this pane now points to another host; close this pane and open it again";

/// What the install of `wsh` on a host says when it cannot start. Fixed sentences, like the ones of the core: nothing of this
/// computer, no command and no secret in them.
const INSTALL_NO_PANE: &str = "wsh was not installed: this pane is not running";
const INSTALL_NOT_SSH: &str = "wsh was not installed: this pane does not run a saved SSH connection";
const INSTALL_NOT_FOCUSED: &str = "wsh was not installed: this is not the pane in focus";
const INSTALL_TARGET_CHANGED: &str =
    "wsh was not installed: the saved connection of this pane now points to another host; close this pane and open it again";
const INSTALL_NO_CONNECTION: &str = "wsh was not installed: the connection of this pane could not be used";
/// The most bytes of a bundled `wsh` that are read: the builds are about half a megabyte.
const MAX_BUNDLED_WSH: u64 = 16 * 1024 * 1024;

/// The sentence the install says for its result: what was done on the connection called `name` (control characters of the
/// name made spaces), or the core's own sentence for a refusal.
fn install_sentence(result: &Result<holzwave_core::wshinstall::Installed, String>, name: &str) -> String {
    use holzwave_core::wshinstall::Installed;
    let name: String = name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let version = env!("CARGO_PKG_VERSION");
    match result {
        Ok(Installed::Installed(_)) => format!("wsh {version} was installed on {name}"),
        Ok(Installed::AlreadyCurrent(_)) => format!("wsh {version} is already installed on {name}"),
        Err(sentence) => sentence.clone(),
    }
}

/// One bundled build: a regular file (a link is not followed) of at most [`MAX_BUNDLED_WSH`] bytes, or `None`.
fn read_bundled(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    if !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() <= MAX_BUNDLED_WSH) {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    // The opened file again: what was checked is what is read.
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_BUNDLED_WSH {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.take(MAX_BUNDLED_WSH + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_BUNDLED_WSH).then_some(bytes)
}

/// The host of every history entry that comes from a pane without a saved connection.
const LOCAL_HOST: &str = "local";

/// The host a history entry of a pane that runs `profile` is stored under: the kind of the connection, a colon and
/// its id (`ssh:web-1`, `wsl:box`, `docker:api`). At most 71 bytes, and never `LOCAL_HOST`.
fn host_label(profile: &Profile) -> String {
    let kind = match profile.kind {
        ConnectionKind::Ssh => "ssh",
        ConnectionKind::Wsl => "wsl",
        ConnectionKind::Docker => "docker",
    };
    format!("{kind}:{}", profile.id)
}

/// What `wsh hist open` is told when its pane is open but is not the one that has the focus of its window: the history opens
/// for the pane the person types in, so a process of a background pane cannot take the keyboard.
const HISTORY_NOT_FOCUSED: &str = "this pane is not the one in focus; the history opens for the pane you type in";

impl AppCore {
    /// Load the layouts from `dir/layout.json` (a fresh one-pane window `main` when there is none or it is unusable;
    /// a layout file of before windows is one window) and start nothing: the webview starts each pane once it knows
    /// the pane's size, and the shell opens the windows other than `main` (see [`AppCore::restored_windows`]).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(dir: PathBuf, sink: Arc<dyn Sink>, shell: ShellSpec) -> Arc<AppCore> {
        AppCore::new_with_ai(dir, sink, shell, AiParts::system())
    }

    /// [`AppCore::new`] with the keychain and the clock of the AI service passed in, which is how the tests give it a
    /// vault of their own. The two AI settings of `settings.json` are applied before the core is returned, so a
    /// hand-edited file is honoured before the first command can run.
    pub fn new_with_ai(dir: PathBuf, sink: Arc<dyn Sink>, shell: ShellSpec, ai: AiParts) -> Arc<AppCore> {
        AppCore::new_with_parts(dir, sink, shell, ai, None)
    }

    /// [`AppCore::new_with_ai`] with the machine and the keychain of the integrations passed in as well, which is how the
    /// tests give them a scratch computer and a store in memory. `None` is the real machine and the store of the host.
    pub fn new_with_parts(
        dir: PathBuf,
        sink: Arc<dyn Sink>,
        shell: ShellSpec,
        ai: AiParts,
        integration: Option<IntegrationParts>,
    ) -> Arc<AppCore> {
        AppCore::new_with_host(dir, sink, shell, Some(ai), integration, Arc::new(NeutralHost::new()))
    }

    /// The full constructor: [`AppCore::new_with_parts`] with the [`Host`] of the shell passed in (see
    /// [`crate::seams`]); the other constructors use a [`NeutralHost`]. The platform id, the key binding table, the
    /// store of every secret, the Local Network source, the text scale and the caps come from `host`. `None` for `ai` or
    /// `integration` builds that service over the store of `host`; parts passed in are used as they are.
    pub fn new_with_host(
        dir: PathBuf,
        sink: Arc<dyn Sink>,
        shell: ShellSpec,
        ai: Option<AiParts>,
        integration: Option<IntegrationParts>,
        host: Arc<dyn Host>,
    ) -> Arc<AppCore> {
        AppCore::new_with_server(dir, sink, shell, ai, integration, host, ServerParts::default())
    }

    /// [`AppCore::new_with_host`] with the link to the server and the source of its sessions passed in (see
    /// [`ServerParts`]). A part that is `None` is built as the desktop builds it.
    pub fn new_with_server(
        dir: PathBuf,
        sink: Arc<dyn Sink>,
        shell: ShellSpec,
        ai: Option<AiParts>,
        integration: Option<IntegrationParts>,
        host: Arc<dyn Host>,
        server_parts: ServerParts,
    ) -> Arc<AppCore> {
        let secrets = host.secrets();
        let ai = ai.unwrap_or_else(|| AiParts::with_secrets(secrets.clone()));
        // What the sync downloaded last time is put in place before anything is read.
        let promoted = holzwave_core::sync::engine::promote_incoming_with_report(&dir);
        let applied = promoted.applied;
        if !applied.is_empty() {
            eprintln!("holzwave: applied from sync: {}", applied.join(", "));
        }
        // An adopted layout replaced the pane files as well (it is one unit), so the working directories kept for the old
        // panes belong to other panes now: pane ids are small per-device counters.
        let adopted_layout = applied.iter().any(|name| name == "layout.json");
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

        let config = Mutex::new(Config::load(&dir, host.platform().chord_table()));
        let connections = {
            let live = lock(&windows).all_panes();
            let (connections, status) =
                load_connections(&dir, secrets.clone(), shell_home(&*host), &live, &promoted.dropped_profiles);
            match status {
                holzwave_core::layout::LoadStatus::Clean => {}
                holzwave_core::layout::LoadStatus::Repaired(notes) => {
                    eprintln!("holzwave: connections.json was repaired: {}", notes.join("; "));
                }
                holzwave_core::layout::LoadStatus::Reset(why) => eprintln!("holzwave: connections.json was reset: {why}"),
            }
            connections
        };
        let blocks = {
            let live = lock(&windows).all_panes();
            Blocks::load(&dir, shell_home(&*host), &live)
        };

        // The panes that ran a tool last time (a tool session is never started again). Pane ids are small per-device
        // counters, so the markers of a layout that sync replaced belong to other panes and are dropped.
        let launch_marks = if adopted_layout {
            write_launch_marks(&dir, &BTreeMap::new());
            BTreeMap::new()
        } else {
            let live = lock(&windows).all_panes();
            load_launch_marks(&dir, &live)
        };

        let panes = Arc::new(Mutex::new(HashMap::new()));
        let last_cwd = Arc::new(Mutex::new(HashMap::new()));
        let routes = {
            let mut routes = Routes::default();
            routes.rebuild(&lock(&windows));
            Arc::new(Mutex::new(routes))
        };
        let session = {
            let mut session = load_session(&dir.join(SESSION_FILE));
            if adopted_layout {
                session.forget_panes();
            }
            // A directory of a pane that is not in the layout is of no use and must not be written back, and neither
            // is the frame of a window that is not in it.
            let set = lock(&windows);
            session.sanitise(&set.all_panes());
            session.sanitise_windows(&set.labels());
            drop(set);
            if adopted_layout {
                // Written now, so a start that ends before the next save does not bring the old directories back.
                if let Err(e) = session.save(&dir) {
                    eprintln!("holzwave: cannot save {SESSION_FILE}: {e}");
                }
            }
            Arc::new(Mutex::new(session))
        };
        let session_saver = {
            let path = dir.join(SESSION_FILE);
            let (windows, panes, session, last_cwd) = (windows.clone(), panes.clone(), session.clone(), last_cwd.clone());
            Debounced::new(SESSION_DELAY, move || write_session(&path, &windows, &panes, &session, &last_cwd))
        };

        let history = holzwave_core::history::History::open(&dir.join("history.db")).unwrap_or_else(|e| {
            eprintln!("holzwave: no history file ({e}); the history is kept in memory");
            holzwave_core::history::History::in_memory().expect("in-memory history")
        });
        let server =
            server_parts.runtime.unwrap_or_else(|| Arc::new(crate::server::ServerRuntime::new(dir.clone(), secrets.clone())));
        // The sessions a caller passes in, else the host's own (the server's worker), else the desktop's.
        let sessions: Arc<dyn crate::sessions::SessionService> = server_parts
            .sessions
            .or_else(|| host.sessions())
            .unwrap_or_else(|| Arc::new(crate::sessions::ServerSessions::new(server.clone())));
        // Which pane shows which session. Pane ids are small per-device counters, so the entries of a layout that sync
        // replaced belong to other panes and are dropped.
        let remote_panes = {
            let live = if adopted_layout { Vec::new() } else { lock(&windows).all_panes() };
            crate::sessions::RemotePanes::load(&dir, &live)
        };
        let sync = crate::sync::SyncRuntime::new(dir.clone(), secrets.clone(), server.clone());
        let ai_host = AiHost::new(&dir, sink.clone(), ai);
        let local_network = crate::local_network::LocalNetworkHost::new(&dir, Some(host.local_network()));
        let host_shell = crate::host_shell::HostShellHost::new(&dir);
        let window_frame = crate::window_frame::WindowFrameHost::new(&dir);
        let integrations = {
            let parts = integration.unwrap_or_else(|| {
                let configured = lock(&config).settings.shell.clone();
                IntegrationParts::system(
                    &dir,
                    resolve_system_shell(&shell, &configured, host_shell.state().spawns_on_host()).launch,
                    secrets.clone(),
                )
            });
            let live = lock(&windows).all_panes();
            IntegrationHost::new(&dir, sink.clone(), parts, &live)
        };
        let core = Arc::new_cyclic(|me| {
            // A login that asks a question raises it through this host. The name and the host of the dialog are looked up in
            // the saved list by the id of the connection: the core stamps it, the server cannot.
            let prompts = {
                let weak = me.clone();
                let lookup = Box::new(move |id: &str| weak.upgrade().and_then(|core: Arc<AppCore>| core.connections.identity_of(id)));
                PromptHost::new(sink.clone(), lookup)
            };
            connections.set_prompter(prompts.clone());
            // The source of shared content reaches the panes and the files through the core, so it gets a weak
            // reference before the core exists: it keeps the core alive for nothing.
            ai_host.bind(me.clone());
            // The sync applies what the app does not own right after a run, through the same weak reference.
            let weak = me.clone();
            sync.bind_applier(Arc::new(move || weak.upgrade().map(|core| core.apply_synced()).unwrap_or_default()));
            AppCore {
                dir,
                windows,
                routes,
                panes,
                config,
                settings_io: Mutex::new(()),
                notes_io: Mutex::new(()),
                last_cwd,
                last_title: Mutex::new(HashMap::new()),
                drops: Mutex::new(Drops::default()),
                hovers: Mutex::new(Hovers::default()),
                starts: Mutex::new(HashMap::new()),
                session,
                session_saver,
                launcher: Mutex::new(spawn_launch),
                sink,
                shell,
                connections,
                blocks,
                launches: Mutex::new(HashMap::new()),
                launch_marks: Mutex::new(launch_marks),
                held: Mutex::new(BTreeMap::new()),
                sync,
                server,
                sessions,
                remote_panes,
                ai: ai_host,
                host,
                local_network,
                host_shell,
                window_frame,
                remote_execs: RemoteExecs::new(),
                prompts,
                integrations,
                monitor: holzwave_core::monitor::Monitor::new(),
                remote_monitors: holzwave_core::monitor::RemoteMonitors::new(),
                history,
                ipc: Mutex::new(None),
                pane_targets: Mutex::new(HashMap::new()),
                resource_dir: Mutex::new(None),
                saver,
                writes,
                exiting: AtomicBool::new(false),
                stopping: AtomicUsize::new(0),
                image_animation: AtomicBool::new(true),
                view_rev: AtomicU64::new(0),
                outside_panes: AtomicUsize::new(0),
                me: me.clone(),
            }
        });
        // The settings are read under the config lock and the lock is released before the service is called.
        let settings = lock(&core.config).settings.clone();
        core.ai.apply_settings(&settings);
        core
    }

    /// The person answered the question of a login. Moves the answers into the login; false (and nothing changed) for an
    /// id that is unknown, old or malformed.
    pub fn login_prompt_answer(&self, id: &str, answers: Vec<String>) -> bool {
        self.prompts.answer(id, answers)
    }

    /// The person gave up on the question of a login.
    pub fn login_prompt_cancel(&self, id: &str) -> bool {
        self.prompts.cancel(id)
    }

    /// What the shell decided for this core: the platform, the secrets, the Local Network source, the text scale and the
    /// caps.
    pub fn host(&self) -> &Arc<dyn Host> {
        &self.host
    }

    /// The AI chat service of the shell.
    pub fn ai(&self) -> &AiHost {
        &self.ai
    }

    /// The integration service of the shell.
    pub fn integrations(&self) -> &IntegrationHost {
        &self.integrations
    }

    /// The four integration tools and where each stands on this computer. Blocking.
    pub fn integration_tools(&self) -> Vec<holzwave_core::integrate::service::ToolView> {
        self.integrations.tools()
    }

    /// One read of an integration verb that shows a view. Blocking.
    pub fn integration_run(
        &self,
        tool: &str,
        verb: &str,
        pane: Option<PaneId>,
        args: &BTreeMap<String, String>,
    ) -> Result<holzwave_core::integrate::service::RunAnswer, IntegrationFault> {
        self.integrations.run(tool, verb, pane, args)
    }

    /// The selection of an integration pane (or the pick of the tool when `pane` is none), checked and resolved by the
    /// service; one `integration` event follows and the file of the selections is saved soon after.
    pub fn integration_select(
        &self,
        tool: &str,
        pane: Option<PaneId>,
        selection: &holzwave_core::integrate::Selection,
    ) -> Result<holzwave_core::integrate::service::SelectionView, IntegrationFault> {
        self.integrations.select(tool, pane, selection)
    }

    /// Makes `path` the program of an integration tool (checked against the tool's own file names). Blocking.
    pub fn integration_binary_set(
        &self,
        tool: &str,
        path: &str,
    ) -> Result<Vec<holzwave_core::integrate::service::ToolView>, IntegrationFault> {
        self.integrations.binary_set(tool, path)
    }

    /// Forgets the program the person chose for an integration tool. Blocking.
    pub fn integration_binary_clear(
        &self,
        tool: &str,
    ) -> Result<Vec<holzwave_core::integrate::service::ToolView>, IntegrationFault> {
        self.integrations.binary_clear(tool)
    }

    /// The integration profiles; a secret variable is a name and whether the keychain has a value. Blocking.
    pub fn integration_profiles(&self) -> holzwave_core::integrate::service::ProfilesView {
        self.integrations.profiles()
    }

    /// Adds or replaces an integration profile. Blocking (the keychain).
    pub fn integration_profile_save(
        &self,
        profile: Value,
    ) -> Result<holzwave_core::integrate::service::ProfilesView, IntegrationFault> {
        self.integrations.profile_save(profile)
    }

    /// Removes an integration profile and its secrets from the keychain. Blocking.
    pub fn integration_profile_remove(
        &self,
        id: &str,
    ) -> Result<holzwave_core::integrate::service::ProfilesView, IntegrationFault> {
        self.integrations.profile_remove(id)
    }

    /// Stores one secret variable of a profile in the keychain. Write-only. Blocking.
    pub fn integration_secret_set(&self, profile: &str, name: &str, value: String) -> Result<(), IntegrationFault> {
        self.integrations.secret_set(profile, name, value)
    }

    /// Removes one secret variable of a profile from the keychain. Blocking.
    pub fn integration_secret_clear(&self, profile: &str, name: &str) -> Result<(), IntegrationFault> {
        self.integrations.secret_clear(profile, name)
    }

    /// The card for an integration verb that changes something or opens a session; nothing is started. Blocking.
    pub fn integration_prepare(
        &self,
        tool: &str,
        verb: &str,
        pane: Option<PaneId>,
        args: &BTreeMap<String, String>,
    ) -> Result<holzwave_core::integrate::ticket::Card, IntegrationFault> {
        self.integrations.prepare(tool, verb, pane, args)
    }

    /// The person closed a card.
    pub fn integration_cancel(&self, ticket: &str) {
        self.integrations.cancel(ticket);
    }

    /// The window `label`'s answer to a card, with the hash it was shown. The one place the person's decision enters. A
    /// verb that changes something runs once; an exec verb opens a pane (a tab, next to the others) whose launch is
    /// registered before the page can start it. Blocking.
    pub fn integration_confirm(&self, label: &str, ticket: &str, hash: &str) -> Result<ConfirmAnswer, IntegrationFault> {
        let (done, origin) = self.integrations.confirm(ticket, hash)?;
        match done {
            holzwave_core::integrate::service::Done::Ran { note } => {
                Ok(ConfirmAnswer { kind: ConfirmKind::Ran, pane: None, note, view: None })
            }
            holzwave_core::integrate::service::Done::Launch(launch) => {
                let note = clean_launch_label(&launch.label);
                let from = LaunchNote { tool: origin.tool.as_str(), verb: origin.verb, from: origin.pane };
                let (pane, view) = self.launch_in(label, "tab", launch, &from)?;
                Ok(ConfirmAnswer { kind: ConfirmKind::Launched, pane: Some(pane), note, view: Some(view) })
            }
        }
    }

    /// A terminal for a verb that only reads (logs, kernel messages, a shell): a pane in the window `label`, in a tab or
    /// beside the focused pane (`placement` is `tab`, `right` or `down`), with the launch registered for it. Blocking.
    pub fn integration_launch(
        &self,
        label: &str,
        tool: &str,
        verb: &str,
        pane: Option<PaneId>,
        args: &BTreeMap<String, String>,
        placement: &str,
    ) -> Result<LaunchAnswer, IntegrationFault> {
        let launch = self.integrations.launch(tool, verb, pane, args)?;
        let (opened, view) = self.launch_in(label, placement, launch, &LaunchNote { tool, verb, from: pane })?;
        Ok(LaunchAnswer { pane: opened, view })
    }

    /// Makes the pane of a launch: the same pane creation as `block_open`, then the launch is registered for the new pane
    /// (so that the page's `start_pane` runs the tool and not a shell) and the pane gets its selection to show. Nothing is
    /// started here. A pane that cannot be made ready is closed again, so a refusal leaves nothing behind.
    fn launch_in(
        &self,
        label: &str,
        placement: &str,
        launch: PaneLaunch,
        note: &LaunchNote<'_>,
    ) -> Result<(PaneId, View), IntegrationFault> {
        let focused = lock(&self.windows).layout(label).map(|layout| layout.tab().focus);
        let made = match placement {
            "right" => self.split_in(label, false),
            "down" => self.split_in(label, true),
            _ => self.new_tab_in(label),
        };
        let view = made.map_err(|_| IntegrationFault::refused("window", "The window is not open."))?;
        // At a limit of the layout the view comes back unchanged and its focus is still the pane that was asked to make
        // room: that pane must not become the launch.
        if Some(view.focus) == focused {
            return Err(IntegrationFault::refused("no_room", "There is no room for another pane here."));
        }
        let pane = view.focus;
        let ready = self
            .register_launch(pane, launch)
            .map_err(|why| IntegrationFault::launch(&why))
            .and_then(|()| self.integrations.pane_launched(pane, note));
        if let Err(fault) = ready {
            let _ = self.close_pane_in(label, pane);
            return Err(fault);
        }
        Ok((pane, view))
    }

    /// What the tools are set to and which panes have a selection, from what is known and at once. The ask lets the
    /// service refresh an old value in the background, and a change comes back as an `integration` event.
    pub fn integration_context(&self, pane: Option<PaneId>, refresh: bool) -> ContextAnswer {
        self.integrations.context(pane, refresh, &|pane| self.pane_held(pane))
    }

    /// The Local Network permission of macOS: the stored choice and the probe.
    pub fn local_network(&self) -> &crate::local_network::LocalNetworkHost {
        &self.local_network
    }

    /// Whether a Flatpak starts panes on the host: the stored choice, the permission of this Flatpak and the two commands
    /// of the page.
    pub fn host_shell(&self) -> &crate::host_shell::HostShellHost {
        &self.host_shell
    }

    /// The frame and button choices of the window (Windows and Linux): the stored choice, the one the running windows were
    /// built with, and the setter. The shell reads [`crate::window_frame::WindowFrameHost::started`] when it builds a window.
    pub fn window_frame(&self) -> &crate::window_frame::WindowFrameHost {
        &self.window_frame
    }

    /// Whether a new pane starts on the host through `flatpak-spawn --host`: only in a Flatpak with the permission whose
    /// person turned Host shell on.
    fn shell_on_host(&self) -> bool {
        self.host_shell.state().spawns_on_host()
    }

    /// Opens System Settings at Privacy & Security, Local Network. One fixed program with one fixed argument and nothing
    /// from the page; where the platform has no such setting nothing is started and the answer is an error.
    pub fn open_local_network_settings(&self) -> Result<(), String> {
        let launch = settings_launch(std::env::consts::OS)?;
        self.launch(&launch)
    }

    /// Whether `pane` is in a layout of some window. The lock of the table of routes is released before this returns.
    pub fn pane_is_open(&self, pane: PaneId) -> bool {
        lock(&self.routes).live.contains_key(&pane)
    }

    /// The label of the window that has `pane`, or `None` when no layout holds it. The lock of the table of routes is
    /// released before this returns.
    pub(crate) fn pane_window(&self, pane: PaneId) -> Option<String> {
        lock(&self.routes).live.get(&pane).map(|label| label.to_string())
    }

    /// The view of the window `label`. An unknown label is the error `no such window`.
    pub fn view_in(&self, label: &str) -> Result<View, String> {
        let set = lock(&self.windows);
        self.view_in_set(&set, label)
    }

    /// The view of the layout, stamped with the next `rev`. The caller holds the windows lock (it has the `&Layout`
    /// from the guard), so two views are stamped in the order their states happened.
    fn view_of(&self, layout: &Layout, windows: usize) -> View {
        View::of(layout, self.view_rev.fetch_add(1, Ordering::SeqCst) + 1, windows)
    }

    /// [`AppCore::view_of`] for the window `label` of a set the caller has locked.
    fn view_in_set(&self, set: &WindowSet, label: &str) -> Result<View, String> {
        set.layout(label).map(|layout| self.view_of(layout, set.len())).ok_or_else(no_such_window)
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
        self.grow_made_in(label, add).map(|(view, _)| view)
    }

    /// [`AppCore::grow_in`] that also answers the pane it made: `None` when the layout was at a limit and nothing was made.
    fn grow_made_in(
        &self,
        label: &str,
        add: impl FnOnce(&mut WindowSet) -> Option<PaneId>,
    ) -> Result<(View, Option<PaneId>), String> {
        let (view, creator, created) = {
            let mut set = lock(&self.windows);
            self.room_for_a_pane(&set)?;
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
        Ok((view, created))
    }

    // ---- connections ------------------------------------------------------------------------------------------

    /// Tell new panes where `wsh` finds the app.
    pub fn set_ipc(&self, dir: PathBuf, token: String) {
        *lock(&self.ipc) = Some((dir, token));
    }

    /// A command from `wsh`, typed in `pane`: open a file beside it, or name it. The window that has the pane is told
    /// the new layout; the answer is the text `wsh` prints.
    pub fn ipc_command(&self, pane: Option<u64>, cmd: holzwave_ipc::Cmd) -> Result<String, String> {
        use holzwave_ipc::Cmd;
        if cmd == Cmd::Ping {
            return Ok("holzWave".into());
        }
        if let Cmd::Ai { .. } = &cmd {
            return Err(WSH_AI_STREAM_ONLY.into());
        }
        if let Cmd::Hist { line, cwd, exit } = &cmd {
            // Not every command is kept (secrets, a leading space); that is not an error for the shell.
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            self.history.add(line, cwd, &self.history_host(pane), *exit, ts).map_err(|e| e.to_string())?;
            return Ok(String::new());
        }
        let pane = pane.ok_or("HOLZWAVE_PANE is not set")?;
        let label = lock(&self.routes).live.get(&pane).map(|l| l.to_string()).ok_or("this pane is not open any more")?;
        let view = match cmd {
            Cmd::View { path } => self.open_local_file_beside(&label, pane, &path, false)?,
            Cmd::Edit { path } => self.open_local_file_beside(&label, pane, &path, true)?,
            Cmd::Setmeta { value, .. } => self.rename_pane_in(&label, pane, &value)?,
            Cmd::Ai { .. } => return Err(WSH_AI_STREAM_ONLY.into()),
            Cmd::HistOpen => {
                self.history_open(pane, &label)?;
                return Ok(String::new());
            }
            Cmd::Ping | Cmd::Hist { .. } => return Ok(String::new()),
        };
        self.sink.send(Target::window(&label), Out::Layout(view));
        Ok(String::new())
    }

    /// A weak reference to this core, for the code that answers what comes through a tunnel.
    pub(crate) fn downgrade(&self) -> Weak<AppCore> {
        self.me.clone()
    }

    /// The saved connection `pane` runs, when `wsh` may be answered for it: an SSH connection with `wsh` turned on, in a
    /// pane that is a terminal. The words say which of the three is not true.
    pub(crate) fn wsh_profile(&self, pane: PaneId) -> Result<Profile, &'static str> {
        if self.blocks.block_of(pane).is_some() {
            return Err(WSH_NO_CONNECTION);
        }
        let profile = self.connections.profile_of(pane).ok_or(WSH_NO_CONNECTION)?;
        if profile.kind != ConnectionKind::Ssh || !profile.wsh {
            return Err(WSH_TURNED_OFF);
        }
        Ok(profile)
    }

    /// A command that a process on a remote host sent through the forward of `pane`. `pane` is the one the forward was made
    /// for, never the one the request names, and the answer is the text `wsh` prints.
    ///
    /// It can ping, name its own pane and show a regular file of its own host beside the pane; nothing else has a branch
    /// here (the `match` has no wildcard, so a new command does not compile until it is decided), and the protocol crate
    /// refuses the others before this is called. In particular nothing here consents, approves, confirms or grants.
    ///
    /// A path is resolved by the pane's own saved connection over SFTP and never on this computer. Blocks on the
    /// network: the sink's thread, never the main one.
    ///
    /// `target` is where the pane's connection pointed when its tunnel was made. A connection that was edited to another
    /// host since is refused with a fixed sentence, because the process that asks is on the old host and a path would
    /// otherwise be resolved on the new one.
    pub fn wsh_remote(&self, pane: PaneId, target: &crate::wsh_tunnel::Target, cmd: holzwave_ipc::Cmd) -> Result<String, String> {
        use holzwave_ipc::{Cmd, REMOTE_REFUSED};
        let label = lock(&self.routes).live.get(&pane).map(|l| l.to_string()).ok_or(WSH_PANE_GONE)?;
        let profile = self.wsh_profile(pane)?;
        if !target.is(&profile) {
            return Err(WSH_TARGET_CHANGED.into());
        }
        let view = match cmd {
            Cmd::Ping => return Ok("holzWave".into()),
            Cmd::Setmeta { value, .. } => self.rename_pane_in(&label, pane, &value)?,
            Cmd::View { path } => self.open_remote_file_beside(&label, pane, &profile, &path, false)?,
            Cmd::Edit { path } => self.open_remote_file_beside(&label, pane, &profile, &path, true)?,
            Cmd::Hist { .. } | Cmd::HistOpen | Cmd::Ai { .. } => return Err(REMOTE_REFUSED.into()),
        };
        self.sink.send(Target::window(&label), Out::Layout(view));
        Ok(String::new())
    }

    /// `wsh view` and `wsh edit` through a tunnel: an absolute path that the pane's own saved connection finds to be a
    /// regular file opens in a preview beside `pane`, in the editor when `edit` is set. The path is resolved on the pane's
    /// host over SFTP and never on this computer.
    fn open_remote_file_beside(&self, label: &str, pane: PaneId, profile: &Profile, path: &str, edit: bool) -> Result<View, String> {
        if !path.starts_with('/') {
            return Err(WSH_NOT_ABSOLUTE.into());
        }
        let source = self.blocks.source(&self.connections, &profile.id).map_err(|_| WSH_NO_HOST.to_owned())?;
        let entry = source.stat(path).map_err(|_| WSH_NO_SUCH_FILE.to_owned())?;
        if entry.kind != holzwave_core::files::Kind::File {
            return Err(WSH_NOT_A_FILE.into());
        }
        self.focus_pane_in(label, pane)?;
        self.block_open_edit_in(label, "right", BlockKind::Preview, &profile.id, path, edit)
    }

    /// `wsh view` and `wsh edit` of a local pane: an absolute path of a file of this computer opens in a preview beside
    /// `pane`, in the editor when `edit` is set.
    fn open_local_file_beside(&self, label: &str, pane: PaneId, path: &str, edit: bool) -> Result<View, String> {
        let p = std::path::Path::new(path);
        if !p.is_absolute() || !p.is_file() {
            return Err(format!("{path}: no such file"));
        }
        self.focus_pane_in(label, pane)?;
        self.block_open_edit_in(label, "right", BlockKind::Preview, LOCAL, path, edit)
    }

    /// Tell the app where the resource folder of the package is, which holds the static `wsh` builds for Linux hosts
    /// (`wsh/linux-x86_64` and `wsh/linux-aarch64`). Not set in a test, and a build made without them has no such folder.
    pub fn set_resource_dir(&self, dir: PathBuf) {
        *lock(&self.resource_dir) = Some(dir);
    }

    /// The bundled builds, read from the two fixed names in the resource folder. A name that is missing, is not a regular
    /// file (a link is not followed), or is larger than [`MAX_BUNDLED_WSH`] is `None`: the install then says this build
    /// of the app has no `wsh` for that processor.
    fn bundled_wsh(&self) -> Bundled {
        let Some(dir) = lock(&self.resource_dir).clone() else { return Bundled::default() };
        let folder = dir.join("wsh");
        // The folder must be a real folder, so that a link cannot send the two reads elsewhere.
        if !std::fs::symlink_metadata(&folder).is_ok_and(|meta| meta.is_dir()) {
            return Bundled::default();
        }
        Bundled { x86_64: read_bundled(&folder.join("linux-x86_64")), aarch64: read_bundled(&folder.join("linux-aarch64")) }
    }

    /// Put the bundled `wsh` on the host of the saved SSH connection that `pane` runs, as the person asked by choosing
    /// `Install wsh on this host` in the pane they have in focus. The result is one sentence, which is also put on the
    /// pane's screen, and a refusal is one too: a pane that is not running, one that does not run a saved SSH
    /// connection, one that is not the focused pane of its window, and one whose connection was edited to another host
    /// since the pane started (the install would otherwise go to a host the pane never showed). Nothing is installed without
    /// this call, and nothing but `~/.holzwave/bin/wsh` on the host is created (see `holzwave_core::wshinstall`). Blocks
    /// on the network: the command that calls it is async.
    pub fn wsh_install(&self, pane: PaneId) -> Result<String, String> {
        // A block pane has no screen to say anything on, and neither has a pane that is not running.
        if self.blocks.block_of(pane).is_some() {
            return Err(INSTALL_NOT_SSH.into());
        }
        let handle = self.handle(pane).ok_or_else(|| INSTALL_NO_PANE.to_owned())?;
        let say = |sentence: &str| -> String {
            let _ = handle.notice(sentence);
            sentence.to_owned()
        };
        if !self.is_focused(pane) {
            return Err(say(INSTALL_NOT_FOCUSED));
        }
        // An assignment takes effect when a pane starts, so a pane that has no recorded target runs a local shell.
        let recorded = lock(&self.pane_targets).get(&pane).cloned();
        let profile = self.connections.profile_of(pane).filter(|p| p.kind == ConnectionKind::Ssh);
        let (Some(profile), Some(recorded)) = (profile, recorded) else { return Err(say(INSTALL_NOT_SSH)) };
        if !recorded.is(&profile) {
            return Err(say(INSTALL_TARGET_CHANGED));
        }
        let connection = self.connections.session(&profile.id).map_err(|_| say(INSTALL_NO_CONNECTION))?;
        let result = holzwave_core::wshinstall::install(&connection, &self.bundled_wsh(), env!("CARGO_PKG_VERSION"));
        let sentence = say(&install_sentence(&result, &profile.name));
        result.map(|_| sentence.clone()).map_err(|_| sentence)
    }

    /// Whether `pane` is the focused pane of the tab its window shows.
    fn is_focused(&self, pane: PaneId) -> bool {
        let Some(label) = lock(&self.routes).live.get(&pane).map(|l| l.to_string()) else { return false };
        lock(&self.windows).layout(&label).is_some_and(|layout| layout.tab().focus == pane)
    }

    /// Tell the window `label` that the shell of `pane` asked for the history search, when `pane` is the one in focus in
    /// the tab that window shows. The window is named by the core and the event carries the pane id only.
    fn history_open(&self, pane: PaneId, label: &str) -> Result<(), String> {
        let focus = lock(&self.windows).layout(label).map(|layout| layout.tab().focus);
        if focus != Some(pane) {
            return Err(HISTORY_NOT_FOCUSED.into());
        }
        self.sink.send(Target::window(label), Out::HistoryOpen { pane });
        Ok(())
    }

    /// Find typed commands of the host the focused pane of window `label` runs on (the commands of a saved connection
    /// for a pane that runs it, the local ones for every other pane). An empty query lists the newest. An unknown window
    /// finds nothing. The page names no host: it cannot ask for another one.
    pub fn history_search_in(&self, label: &str, query: &str, limit: usize) -> Vec<holzwave_core::history::Hit> {
        // The focus is read without building a view, which would stamp a revision.
        let Some(focus) = lock(&self.windows).layout(label).map(|layout| layout.tab().focus) else {
            return Vec::new();
        };
        self.history.search(query, Some(&self.history_host(Some(focus))), limit)
    }

    /// The host a command typed in `pane` is stored under: the label of the saved connection the pane runs, or
    /// `LOCAL_HOST` for every other pane (and for no pane, and for one the app does not know).
    fn history_host(&self, pane: Option<PaneId>) -> String {
        pane.and_then(|pane| self.connections.profile_of(pane)).map_or_else(|| LOCAL_HOST.to_owned(), |p| host_label(&p))
    }

    /// Forget one command (every use of it).
    pub fn history_delete(&self, cmd: &str) -> Result<usize, String> {
        self.history.delete(cmd).map_err(|e| e.to_string())
    }

    /// Forget the whole history.
    pub fn history_clear(&self) -> Result<(), String> {
        self.history.clear().map_err(|e| e.to_string())
    }

    /// The widgets of the sidebar, the saved notes and a note for each widget file that was left out. Reads small files.
    pub fn widgets_list(&self) -> WidgetsView {
        let (widgets, warnings) = holzwave_core::widgets::load(&self.dir);
        WidgetsView { widgets, notes: holzwave_core::widgets::load_notes(&self.dir), warnings }
    }

    /// Save the text of a note widget.
    pub fn widget_note_save(&self, id: &str, text: &str) -> Result<(), String> {
        let _notes = lock(&self.notes_io);
        holzwave_core::widgets::save_note(&self.dir, id, text).map_err(|e| e.to_string())
    }

    /// What the computer of a monitor pane is doing now: this one, or the saved SSH connection the pane's block names. The
    /// page names the pane and never an address, so the core alone decides which computer is asked. Blocks (a few
    /// milliseconds here, a round trip for another computer): not on the main thread.
    pub fn monitor_sample(&self, pane: PaneId) -> Result<holzwave_core::monitor::Reading, String> {
        self.monitor_sample_with(pane, true)
    }

    /// [`AppCore::monitor_sample`] for a host that may not show the processes of the machine that runs the core: with
    /// `processes` false the reading of this computer carries the numbers and an empty list of processes. The reading of
    /// another computer, over a saved connection, is the person's own and is passed on as it is. Which of the two it is
    /// comes from the pane's block, so the answer cannot be local and listed at once.
    pub fn monitor_sample_with(&self, pane: PaneId, processes: bool) -> Result<holzwave_core::monitor::Reading, String> {
        let block = self
            .blocks
            .block_of(pane)
            .filter(|block| block.kind == BlockKind::Monitor)
            .ok_or_else(|| "this pane shows no system monitor".to_owned())?;
        if block.source == LOCAL {
            let mut snapshot = self.monitor.sample();
            if !processes {
                snapshot.procs.clear();
            }
            return Ok(holzwave_core::monitor::Reading::Live(snapshot));
        }
        self.remote_monitors.sample(&self.connections, &block.source)
    }

    /// Move what the last sync run downloaded into place, for the files the running app does not own: the themes, the
    /// widgets and the texts of the notes. The layout, the settings and the connections stay staged until the next
    /// start, because the app rewrites them from memory. A live file the person edited after the download was staged is
    /// copied to `sync-conflicts/` first. The notes lock is held for the move only; the registry is reloaded after it is
    /// released (a theme may have arrived or been removed), which announces `settings` to every window when it changed.
    /// Blocks on small files: not on the main thread.
    pub fn apply_synced(&self) -> holzwave_core::sync::engine::Promoted {
        use holzwave_core::sync::engine::{is_unowned, promote_where};
        let promoted = {
            let _notes = lock(&self.notes_io);
            promote_where(&self.dir, &is_unowned)
        };
        if !promoted.applied.is_empty() {
            eprintln!("holzwave: applied from sync: {}", promoted.applied.join(", "));
        }
        // Every time: the run may also have removed a theme, which nothing here staged. A reload compares sizes and
        // modification times and does nothing when nothing changed.
        self.reload_settings();
        promoted
    }

    /// The state of the sync, for the page.
    pub fn sync_status(&self) -> crate::sync::SyncView {
        self.sync.view()
    }

    /// Set up the sync for an account and run it once. Blocks on the network: not on the main thread.
    pub fn sync_save(&self, endpoint: &str, user: &str, password: &str, passphrase: &str) -> Result<crate::sync::SyncView, String> {
        self.sync.save(endpoint, user, password, passphrase)
    }

    /// Run the sync now. Blocks on the network.
    pub fn sync_now(&self) -> Result<crate::sync::SyncView, String> {
        self.sync.run()
    }

    pub fn sync_off(&self) -> crate::sync::SyncView {
        self.sync.off()
    }

    /// Use the server this device is signed in to as the sync's transport, with the person's passphrase, and run it once.
    /// Blocks on the network: not on the main thread.
    pub fn sync_use_server(&self, passphrase: &str) -> Result<crate::sync::SyncView, String> {
        self.sync.use_server(passphrase)
    }

    /// The link to the server as the page reads it: no request is made.
    pub fn server_status(&self) -> crate::server::ServerView {
        self.server.view()
    }

    /// Ask the server who this device is. Blocks on the network: not on the main thread.
    pub fn server_refresh(&self) -> crate::server::ServerView {
        self.server.refresh()
    }

    /// Start a sign-in at a server address. Blocks on the network: not on the main thread.
    pub fn server_sign_in_start(&self, address: &str) -> Result<crate::server::SignInView, String> {
        self.server.sign_in_start(address)
    }

    /// One poll of the sign-in in progress. Blocks on the network: not on the main thread.
    pub fn server_sign_in_poll(&self) -> Result<crate::server::SignInPoll, String> {
        self.server.sign_in_poll()
    }

    /// Drop the sign-in in progress.
    pub fn server_sign_in_cancel(&self) -> crate::server::ServerView {
        self.server.sign_in_cancel()
    }

    /// Sign this device out of the server. Blocks on the network: not on the main thread.
    pub fn server_sign_out(&self) -> Result<crate::server::ServerView, String> {
        self.server.sign_out()
    }

    // ---- sessions of a server ---------------------------------------------------------------------------------

    /// The sessions of the person on the server, oldest first. Blocks on the network: not on the main thread. An error is
    /// one of the fixed sentences of [`SessionFault`]; nothing the server wrote is passed on as a sentence.
    pub fn server_sessions(&self) -> Result<Vec<SessionRow>, String> {
        Ok(self.sessions.list()?)
    }

    /// Make a session on the server and show it in a new tab of the window `label`. The answer is the view with the new
    /// tab shown; the webview starts the pane when it sees the view, and the session takes the size of the pane's tile then.
    /// Refused before anything is made when the person has no room for a pane; a session that was made and cannot be shown
    /// is ended again, so nothing is left behind. Blocks on the network: not on the main thread.
    pub fn server_session_new_in(&self, label: &str) -> Result<View, String> {
        self.room_for_a_session_pane(label)?;
        let (cols, rows) = crate::sessions::NEW_SESSION_GRID;
        let session = self.sessions.create(cols, rows)?;
        self.show_session_in(label, session).inspect_err(|_| {
            let _ = self.sessions.end(session);
        })
    }

    /// Show the session `session` of the server in a new tab of the window `label`. The session has to exist for this
    /// person (anything else is the one `not found` sentence) and must not be shown in a pane of this device already.
    /// Blocks on the network: not on the main thread.
    pub fn server_session_open_in(&self, label: &str, session: u32) -> Result<View, String> {
        if self.remote_panes.pane_of(session).is_some() {
            return Err(SessionFault::AlreadyOpen.into());
        }
        self.room_for_a_session_pane(label)?;
        if !self.sessions.list()?.iter().any(|row| row.id == session) {
            return Err(SessionFault::NotFound.into());
        }
        self.show_session_in(label, session)
    }

    /// End the program of a session on the server. The panes that show it say so and stay until they are closed. Blocks
    /// on the network: not on the main thread.
    pub fn server_session_end(&self, session: u32) -> Result<(), String> {
        Ok(self.sessions.end(session)?)
    }

    /// Take the right to write in the session `pane` shows. The view of the pane (see [`AppCore::pane_connections`])
    /// changes when the server has said so.
    pub fn pane_takeover(&self, pane: PaneId) -> Result<(), String> {
        self.session_pane(pane)?.takeover().map_err(session_pane_failure)
    }

    /// Ask the server for older lines of the session `pane` shows. Nothing happens when there are none or a request is
    /// on its way.
    pub fn pane_load_older(&self, pane: PaneId) -> Result<(), String> {
        self.session_pane(pane)?.load_older().map_err(session_pane_failure)
    }

    /// The running pane `pane`, when it shows a session of a server.
    fn session_pane(&self, pane: PaneId) -> Result<PaneHandle, String> {
        let not_one = || crate::sessions::NOT_A_SESSION_PANE.to_owned();
        self.remote_panes.session_of(pane).ok_or_else(not_one)?;
        self.handle(pane).ok_or_else(not_one)
    }

    /// Said before a request goes to the server: the window exists and the person may have one more pane. The check is made
    /// again, with the layout locked, when the tab is made.
    fn room_for_a_session_pane(&self, label: &str) -> Result<(), String> {
        let set = lock(&self.windows);
        set.layout(label).ok_or_else(no_such_window)?;
        self.room_for_a_pane(&set)
    }

    /// A new tab in the window `label` whose pane shows `session`: the pane is in the record before the page can start
    /// it. No tab is left behind when the pane cannot be recorded, and none is made at a limit of the layout.
    fn show_session_in(&self, label: &str, session: u32) -> Result<View, String> {
        let (view, made) = self.grow_made_in(label, |set| set.new_tab(label))?;
        let Some(pane) = made else {
            return Err(crate::sessions::NO_ROOM.to_owned());
        };
        // The pane of a session starts nothing on this computer: it has no directory to start in.
        lock(&self.starts).remove(&pane);
        if !self.remote_panes.assign(pane, session) {
            self.close_pane_in(label, pane)?;
            return Err(crate::sessions::NO_ROOM.to_owned());
        }
        Ok(view)
    }

    /// Make `spec` the session of the server that `pane` shows, when the record names one; `false` for every other pane.
    /// Such a pane never becomes a local shell: without a way to the session it shows the failed state with one fixed
    /// sentence and runs nothing. It starts no program here, so it has no directory, no environment and no sandbox line,
    /// and it stays on the screen with its last output when the session ends, until Enter or a close.
    fn apply_remote(&self, pane: PaneId, spec: &mut PaneSpec) -> bool {
        let Some(session) = self.remote_panes.session_of(pane) else {
            return false;
        };
        let target = self
            .sessions
            .target(session)
            .unwrap_or_else(|fault| crate::sessions::failed_target(session, fault, self.sessions.name()));
        spec.remote = Some(target);
        spec.blank = false;
        spec.host_spawn = false;
        spec.cwd = None;
        spec.notice = None;
        spec.env.clear();
        spec.linger = Some(Duration::MAX);
        true
    }

    /// The scheduled run.
    pub fn sync_background(&self) {
        self.sync.background();
    }

    /// The vault was unlocked: a sync that was paused for it starts one run at once, on a thread of its own, so the
    /// command that unlocked answers without waiting for the network.
    pub fn vault_unlocked(&self) {
        if !self.sync.resume_due() {
            return;
        }
        let me = self.me.clone();
        let started = std::thread::Builder::new().name("sync-resume".into()).spawn(move || {
            if let Some(core) = me.upgrade() {
                let _ = core.sync.run();
            }
        });
        drop(started);
    }

    /// The saved connections, each with whether a secret is saved for it. Asks the keychain, so not on the main thread.
    pub fn connections_list(&self) -> Vec<ProfileView> {
        self.connections.list()
    }

    /// Add or change a connection and its secret; the answer is the connection as saved, or why it was refused.
    pub fn connection_save(&self, profile: Profile, secret: SecretChange) -> Result<ProfileView, String> {
        self.connections.save(profile, secret).map_err(|e| e.to_string())
    }

    /// Remove a connection and its secret.
    pub fn connection_delete(&self, id: &str) -> Result<(), String> {
        self.connections.delete(id).map_err(|e| e.to_string())
    }

    /// Open connection `id` in a new tab of the window `label`: the tab is made, its pane is told what to run, and the
    /// answer is the view with the new tab shown. The webview starts the pane when it sees the view.
    pub fn connect_in(&self, label: &str, id: &str) -> Result<View, String> {
        if !self.connections.contains(id) {
            return Err(format!("no connection {id}"));
        }
        // Refused before a tab is made for it (a WSL connection off Windows).
        self.connections.ensure_opens_here(id).map_err(|e| e.to_string())?;
        // A connection that needs a saved password or key passphrase is refused while the secret store is locked, before a
        // tab is made for it; it says the one sentence the page knows and offers the unlock. A login that asks the store
        // for nothing (an agent, a question the person answers) does not read it and is not held back. Any other refusal of
        // the store (a secret that is missing) is said where it always was, when the pane starts.
        if self.connections.kind_of(id) == Some(ConnectionKind::Ssh)
            && let Err(error) = self.connections.ssh_target(id)
            && crate::locked::is_locked(&error)
        {
            return Err(crate::locked::SECRETS_LOCKED.to_owned());
        }
        let view = self.new_tab_in(label)?;
        self.connections.assign(view.focus, id).map_err(|e| e.to_string())?;
        Ok(view)
    }

    // ---- files and previews ----------------------------------------------------------------------------------

    /// Open a file browser or a preview in a new tab (`placement` is `tab`) or beside the focused pane (`right`, `down`).
    /// The pane runs no process; the answer is the view with the new pane focused.
    pub fn block_open_in(&self, label: &str, placement: &str, kind: BlockKind, source: &str, path: &str) -> Result<View, String> {
        self.block_open_edit_in(label, placement, kind, source, path, false)
    }

    /// [`AppCore::block_open_in`] with the flag of a preview that opens its file in the editor at once (`wsh edit`). The
    /// flag is for a preview: any other kind with it is refused with the block.
    pub fn block_open_edit_in(
        &self,
        label: &str,
        placement: &str,
        kind: BlockKind,
        source: &str,
        path: &str,
        edit: bool,
    ) -> Result<View, String> {
        let path = if path.is_empty() && kind == BlockKind::Files {
            self.blocks.start_dir(&self.connections, source).map_err(|e| e.to_string())?
        } else {
            path.to_owned()
        };
        // A monitor of another computer needs a saved SSH connection: said before any pane is made.
        if kind == BlockKind::Monitor && source != LOCAL {
            match self.connections.kind_of(source) {
                Some(ConnectionKind::Ssh) => {}
                None => return Err(format!("no connection {source}")),
                Some(_) => {
                    let name = self.connections.label_of(source).unwrap_or_else(|| source.to_owned());
                    return Err(format!("the system monitor needs an SSH connection, and {name} is not one"));
                }
            }
        }
        let block = Block { kind, source: source.to_owned(), path, edit };
        // Checked before the pane is made, so a refused block leaves no empty pane behind.
        self.blocks.assign(0, block.clone()).map_err(|e| e.to_string())?;
        self.blocks.forget(0);
        let focused = lock(&self.windows).layout(label).map(|layout| layout.tab().focus);
        let view = match placement {
            "right" => self.split_in(label, false)?,
            "down" => self.split_in(label, true)?,
            _ => self.new_tab_in(label)?,
        };
        // At a limit of the layout (panes, or how deep a split may nest) the view comes back unchanged, and its focus is
        // still the pane that was asked to make room: it must not become the block.
        if Some(view.focus) == focused {
            return Err(NO_ROOM_FOR_A_PANE.into());
        }
        self.blocks.assign(view.focus, block).map_err(|e| e.to_string())?;
        if kind == BlockKind::Integration {
            // The pane starts from the pick of the tool. A pane that cannot be recorded (too many panes in the file) is
            // closed again, so that a refusal leaves nothing behind.
            let shown = self.blocks.block_of(view.focus).map(|assigned| assigned.path).unwrap_or_default();
            if let Err(fault) = self.integrations.pane_shows(view.focus, source, &shown) {
                let _ = self.close_pane_in(label, view.focus);
                return Err(fault.text);
            }
        }
        Ok(view)
    }

    /// Every pane that shows a file browser or a preview.
    pub fn blocks_list(&self) -> Vec<BlockView> {
        self.blocks.views(&self.connections)
    }

    /// The pane's block went to `path` (a folder entered, another file shown); remembered for the next start.
    pub fn block_update(&self, pane: PaneId, path: &str) -> Result<(), String> {
        self.blocks.update(pane, path).map_err(|e| e.to_string())?;
        // An integration pane that moved to another view keeps its selection and tells the service the view.
        if let Some(block) = self.blocks.block_of(pane).filter(|block| block.kind == BlockKind::Integration) {
            self.integrations.pane_shows(pane, &block.source, &block.path).map_err(|e| e.text)?;
        }
        Ok(())
    }

    pub fn files_list(&self, source: &str, path: &str) -> Result<Listing, String> {
        self.blocks.list(&self.connections, source, path).map_err(|e| e.to_string())
    }

    pub fn file_stat(&self, source: &str, path: &str) -> Result<Entry, String> {
        self.blocks.stat(&self.connections, source, path).map_err(|e| e.to_string())
    }

    pub fn file_open(&self, source: &str, path: &str) -> Result<Opened, String> {
        self.blocks.open(&self.connections, source, path).map_err(|e| e.to_string())
    }

    /// Permission to show one image, video, audio or PDF through the `holzfile` scheme: a token that is the whole URL
    /// path, and the file's media type and size.
    pub fn file_grant(&self, source: &str, path: &str) -> Result<Grant, String> {
        self.blocks.grant(&self.connections, source, path).map_err(|e| e.to_string())
    }

    /// Why a grant for the file at `path` is refused, as a code that does not change. It asks the source the same three
    /// things the grant asks (the entry, the first bytes, the kind of the file) and reads the answer by the typed error
    /// and by the kind, never by the text of the core's refusal. Nothing is granted. A file that would be granted has no
    /// refusal to explain and answers `Other`.
    pub fn file_grant_why(&self, source: &str, path: &str) -> GrantCode {
        use holzwave_core::files::Kind as EntryKind;
        use holzwave_core::preview::{Content, SNIFF, classify};
        let probe = || -> holzwave_core::Result<bool> {
            let src = self.blocks.source(&self.connections, source)?;
            let entry = src.stat(path)?;
            if entry.kind != EntryKind::File {
                return Ok(false);
            }
            let head = src.read_range(path, 0, SNIFF as u64)?;
            Ok(matches!(classify(&entry.name, &head).content, Content::Image | Content::Video | Content::Audio | Content::Pdf))
        };
        match probe() {
            Ok(false) => GrantCode::Kind,
            Ok(true) => GrantCode::Other,
            Err(error) if crate::locked::is_locked(&error) => GrantCode::Locked,
            Err(holzwave_core::Error::Io(io)) => match io.kind() {
                std::io::ErrorKind::NotFound => GrantCode::NotFound,
                std::io::ErrorKind::PermissionDenied => GrantCode::Denied,
                _ => GrantCode::Other,
            },
            // The core says "no such file" as a text only (`Unsupported`), so an absence on this computer is read from the
            // file system itself and not from that text. A saved connection has no such answer yet and says `Other`.
            Err(holzwave_core::Error::Unsupported(_)) if source == LOCAL && absent_here(path) => GrantCode::NotFound,
            Err(_) => GrantCode::Other,
        }
    }

    /// End a grant when the preview that used it is gone.
    pub fn file_revoke(&self, token: &str) {
        self.blocks.revoke(token);
    }

    /// What the `holzfile` scheme needs: the media type and size of a granted file.
    pub fn granted(&self, token: &str) -> Option<(String, u64)> {
        self.blocks.granted(token)
    }

    /// `len` bytes of a granted file from `offset`; `None` when the token is not a grant.
    pub fn read_granted(&self, token: &str, offset: u64, len: u64) -> Option<Result<Vec<u8>, String>> {
        self.blocks.read_granted(&self.connections, token, offset, len).map(|r| r.map_err(|e| e.to_string()))
    }

    /// Save the editor's text. With `expected_digest` the conflict check compares the bytes (see `Blocks::save_file`);
    /// the answer carries the new change time and the digest of what was written.
    pub fn file_save(&self, source: &str, path: &str, text: &str, expected: Option<i64>, expected_digest: Option<&str>) -> Result<Saved, String> {
        self.blocks.save_file(&self.connections, source, path, text, expected, expected_digest).map_err(|e| e.to_string())
    }

    /// The connection state of every running pane that has a connection. The webview asks about once a second while it
    /// shows one.
    pub fn pane_connections(&self) -> Vec<ConnView> {
        let running: Vec<(PaneId, PaneHandle)> = lock(&self.panes)
            .iter()
            .filter_map(|(pane, slot)| match slot {
                Slot::Running(handle) => Some((*pane, handle.clone())),
                Slot::Starting(_) => None,
            })
            .collect();
        let mut views = self.connections.views(running.iter().map(|(pane, handle)| (*pane, handle.conn_state())));
        // The panes that show a session of a server: the name is the host of the server and the remote member says who
        // writes, the grid of the session and what the pane is busy with.
        let mut name = None;
        for (pane, handle) in &running {
            if self.remote_panes.session_of(*pane).is_none() {
                continue;
            }
            let Some(view) = handle.remote_view() else { continue };
            let name = name.get_or_insert_with(|| self.sessions.name());
            views.push(ConnView::remote(*pane, name, &handle.conn_state(), RemoteConn::of(&view)));
        }
        views.sort_by_key(|view| view.pane);
        views
    }

    /// Whether the person may have one more pane: the layout, over all windows, holds fewer panes than the cap of the host
    /// (`limits().max_panes`). Checked with the layout locked by every path that makes a pane, so two requests cannot both
    /// take the last place. A layout restored with more panes than the cap keeps them all; this only refuses new ones.
    /// Without a cap the layout's own limits decide, as they always did.
    fn room_for_a_pane(&self, set: &WindowSet) -> Result<(), String> {
        match self.host.limits().max_panes {
            Some(cap) if set.pane_count() + self.outside_panes.load(Ordering::SeqCst) >= cap => {
                Err(format!("this server allows {cap} panes per person; close one to open another"))
            }
            _ => Ok(()),
        }
    }

    /// A place under the pane cap for a pane that no window shows (a session of a server). The cap counts the layout's panes
    /// and these together, so the two kinds cannot together pass it; `None` when the cap is reached. Without a cap of the
    /// host there is no limit here and the caller's own bound holds. The place is given back when the guard is dropped.
    pub fn take_outside_pane(&self) -> Option<OutsidePane> {
        let core = self.me.upgrade()?;
        let set = lock(&self.windows);
        if let Some(cap) = self.host.limits().max_panes
            && set.pane_count() + self.outside_panes.load(Ordering::SeqCst) >= cap
        {
            return None;
        }
        self.outside_panes.fetch_add(1, Ordering::SeqCst);
        drop(set);
        Some(OutsidePane { core })
    }

    /// How many panes run outside the layout ([`AppCore::take_outside_pane`]).
    pub fn outside_pane_count(&self) -> usize {
        self.outside_panes.load(Ordering::SeqCst)
    }

    /// How a session of a server starts: the person's shell, environment and setting of the scrollback and the pictures,
    /// as any pane of the window starts, but a pane that draws for no page ([`PaneSpec::headless`]) in the person's home
    /// directory, and with none of the variables `wsh` finds the app by (no socket, no token, no `wsh` folder in `PATH`:
    /// a session is a shell, and the server holds no `wsh` socket for it).
    pub fn session_pane_spec(&self, cols: u16, rows: u16) -> PaneSpec {
        let mut spec = self.pane_spec(cols, rows, (0, 0));
        spec.headless = true;
        spec.cwd = shell_home(&*self.host).filter(|home| home.is_dir());
        spec
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
            self.room_for_a_pane(&set)?;
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

    /// Drop `pane` on the chip of workspace `workspace`: it goes next to the focus pane of that workspace's shown
    /// tab. An unknown pane or workspace, the workspace the pane is in and a move over a limit change nothing.
    pub fn move_pane_to_workspace_in(&self, label: &str, pane: PaneId, workspace: usize) -> Result<View, String> {
        self.change_in(label, |l| {
            l.move_pane_to_workspace(pane, workspace);
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
                let view = before.as_ref().map(|layout| self.view_of(layout, set.len() + 1)).ok_or_else(no_such_window)?;
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
                let view = before.as_ref().map(|layout| self.view_of(layout, set.len() + 1)).ok_or_else(no_such_window)?;
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
        self.hover_window_gone(label);
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
        self.hover_window_gone(label);
        if self.exiting.load(Ordering::SeqCst) {
            return;
        }
        let (panes, slots) = {
            let mut set = lock(&self.windows);
            let Some(panes) = set.close_window(label) else {
                return;
            };
            self.sync_routes(&set);
            let mut panes_map = lock(&self.panes);
            let slots = panes.iter().filter_map(|pane| panes_map.remove(pane)).collect::<Vec<_>>();
            (panes, slots)
        };
        // The panes are not in any layout now; their shells end on their own threads.
        for slot in slots {
            if let Slot::Running(handle) = &slot {
                handle.close();
            }
        }
        // Every pane of the window is forgotten, also one whose shell never started: it has no slot, but it left the
        // layout all the same, and the chat turns that act for it have to hear of it.
        for pane in panes {
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
        self.owns(label, pane, kind)?;
        // The gesture is over, wherever it ended: the window that showed its outline stops showing it, before it is
        // asked where the pane goes.
        self.drag_left(label);
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

    /// `pane` (or, for a tab, the tab it is in, which has to be the shown one) is in the window `label`.
    fn owns(&self, label: &str, pane: PaneId, kind: DropKind) -> Result<(), String> {
        let set = lock(&self.windows);
        let layout = set.layout(label).ok_or_else(no_such_window)?;
        let owned = match kind {
            DropKind::Pane => set.window_of(pane) == Some(label),
            DropKind::Tab => set.locate(pane).is_some_and(|(owner, ws, _)| owner == label && ws == layout.active),
        };
        if owned { Ok(()) } else { Err("no such pane".into()) }
    }

    /// The page of `label` is dragging a pane (or a tab, named by one of its panes) and the pointer is outside the
    /// window: the shell reads the pointer, and when it is over another window of the app that window is sent
    /// `drag-hover` with the pointer in its client coordinates, so that it can show where the drop would land; a
    /// window the drag was over before is sent `drag-end` when it is another one, or when the pointer is over none
    /// now. Only ids and window-local geometry travel, and nothing moves before the release (`release_outside`).
    /// Where the pointer cannot be read nothing is shown.
    pub fn drag_over_in(&self, label: &str, pane: PaneId, kind: DropKind) -> Result<(), String> {
        if self.exiting.load(Ordering::SeqCst) {
            return Err("the app is closing".into());
        }
        self.owns(label, pane, kind)?;
        match self.sink.cursor_spot(label) {
            CursorSpot::Window { label: to, x, y } if to != label && lock(&self.windows).layout(&to).is_some() => {
                let left = lock(&self.hovers).enter(label, &to);
                if let Some(left) = left {
                    self.sink.send(Target::window(&left), Out::DragEnd);
                }
                self.sink.send(Target::window(&to), Out::DragHover { kind, pane, x, y });
            }
            _ => self.drag_left(label),
        }
        Ok(())
    }

    /// The drag that started in the window `label` ended (released, Escape, the pointer came back, the page is
    /// going): the window it was over is told. Nothing to tell is no error.
    pub fn drag_end_in(&self, label: &str) -> Result<(), String> {
        lock(&self.windows).layout(label).ok_or_else(no_such_window)?;
        self.drag_left(label);
        Ok(())
    }

    /// Tell the window that the drag from `from` was over that the drag left it. Call it with no lock held.
    fn drag_left(&self, from: &str) {
        let over = lock(&self.hovers).leave(from);
        if let Some(over) = over {
            self.sink.send(Target::window(&over), Out::DragEnd);
        }
    }

    /// The window `label` is gone: a drag that started in it ends for the window it was over, a drag over it is
    /// forgotten. Call it with no lock held.
    fn hover_window_gone(&self, label: &str) {
        let over = lock(&self.hovers).forget(label);
        if let Some(over) = over {
            self.sink.send(Target::window(&over), Out::DragEnd);
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
        self.integrations.flush();
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

    /// Write `pane_launches.json` from the table, now. A failure is logged and changes nothing else: the marker is a note
    /// for the next start, never a condition of this one.
    fn save_launch_marks(&self) {
        let marks = lock(&self.launch_marks);
        write_launch_marks(&self.dir, &marks);
    }

    /// A pane reported its working directory (from its sink, on the engine thread of the pane).
    fn cwd_reported(&self, pane: PaneId, cwd: &str) {
        // A pane that is already out of the map (closed) is not remembered again by a late event, and once the app
        // is ending the session has been written: a later event would only bring a write after the exit.
        if self.exiting.load(Ordering::SeqCst) || !lock(&self.panes).contains_key(&pane) {
            return;
        }
        // The directory of a session on a server is a directory of the server: it is not kept as one of this computer,
        // where a split would start in it and the session file would hold it.
        if self.remote_panes.session_of(pane).is_some() {
            return;
        }
        lock(&self.last_cwd).insert(pane, cwd.to_owned());
        self.session_saver.touch();
    }

    /// A pane left the layout: what is remembered of its directory and title goes, and the session is written
    /// without it.
    fn forget_pane(&self, pane: PaneId) {
        // The launch of a pane that never started is dropped with its secrets; the marker of one that ran goes too.
        lock(&self.launches).remove(&pane);
        let had_marker = lock(&self.launch_marks).remove(&pane).is_some();
        if had_marker {
            self.save_launch_marks();
        }
        lock(&self.last_cwd).remove(&pane);
        lock(&self.last_title).remove(&pane);
        lock(&self.starts).remove(&pane);
        lock(&self.pane_targets).remove(&pane);
        self.connections.forget(pane);
        self.remote_panes.forget(pane);
        // The selection, the held mark and the shell overlay of an integration pane, and the page is told.
        self.integrations.pane_closed(pane);
        // A chat pane that leaves the layout ends the shares of its chat: the block is read before it is forgotten.
        let chat = self.blocks.block_of(pane).filter(|block| block.kind == BlockKind::Chat).map(|block| block.path);
        self.blocks.forget(pane);
        // The last monitor pane of a computer that closes ends that computer's session.
        self.remote_monitors.keep_only(&self.blocks.remote_monitor_sources());
        self.session_saver.touch();
        if let Some(chat) = chat {
            self.ai.chat_closed(&chat);
        }
        // The requests of shells in this pane are withdrawn: their cards go and their turns stop.
        self.ai().asks().pane_closed(pane);
        // Last, with no guard alive: the calls of the chat turns that act for this pane are voided, and so are the
        // shares of the pane.
        self.ai.pane_closed(pane);
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

    /// Open a link with the platform opener. Only a link whose scheme is in the `link_schemes` setting, read at this
    /// call, is opened (never `javascript`, `data`, `vbscript`, `blob` or `about`); the length, the characters and the
    /// shape are checked first (no control, blank or invisible character, whatever the scheme), and nothing is started for a
    /// link that fails (T-02-52). The list is the person's, the page's own checks are a convenience: this command is the
    /// boundary that decides.
    ///
    /// A `file` link is not opened: with `file` in the list, in a pane that runs on this computer (`pane`, the pane that
    /// shows the link) and with a path that [`file_link_path`] accepts, the file is shown in the file manager through
    /// the code of [`AppCore::reveal_path`], which shows and never runs. Without a pane, for an unknown pane, for a pane
    /// of a connection and for any other path it is refused. `pane` means nothing for any other link.
    ///
    /// Starts a process, so the command is async.
    pub fn open_url(&self, url: &str, pane: Option<PaneId>) -> Result<(), String> {
        let schemes = lock(&self.config).settings.link_schemes.clone();
        let os = std::env::consts::OS;
        if is_file_link(url) {
            check_link_text(url)?;
            if !schemes.iter().any(|listed| listed == "file") {
                return Err("file links are not in the list of link schemes in the settings".into());
            }
            let pane = pane.ok_or("a file link is only shown from a pane, and none was named")?;
            // A pane of a connection shows the files of another computer: its links never reach this one.
            if self.connections.profile_of(pane).is_some() {
                return Err("a file link in a pane of a connection names a file on another computer".into());
            }
            if self.handle(pane).is_none() {
                return Err("a file link needs a running pane on this computer".into());
            }
            let path = file_link_path(os, url)?;
            return self.reveal_path(&path, None);
        }
        let launch = open_launch(os, url, &schemes, &process_env)?;
        self.launch(&launch)
    }

    /// Reveal a file or folder in the platform file manager. `path` is absolute, starts with `~/`, or is relative
    /// to `base` (the working directory of the pane); it has to exist. What is revealed is shown and never run
    /// (T-02-52).
    ///
    /// Touches the file system and starts a process, so the command is async.
    pub fn reveal_path(&self, path: &str, base: Option<&str>) -> Result<(), String> {
        let os = std::env::consts::OS;
        let home = home_dir(os, &process_env, self.private_home().as_deref());
        let resolved = resolve_reveal(os, path, base, home.as_deref())?;
        let launch = reveal_launch(os, &resolved, resolved.is_dir(), &process_env)?;
        self.launch(&launch)
    }

    /// The private folders of the app, on a host that names them and is the Android one; `None` on every other.
    fn private_dirs(&self) -> Option<PrivateDirs> {
        if self.host.platform() == Platform::Android { self.host.private_dirs() } else { None }
    }

    /// The home folder of the Android host, for `~/` ([`home_dir`]).
    fn private_home(&self) -> Option<PathBuf> {
        self.private_dirs().map(|dirs| dirs.home)
    }

    /// The version of the app, the operating system and the name of the machine.
    ///
    /// Runs a program on macOS, so the command is async.
    pub fn app_info(&self) -> AppInfo {
        AppInfo {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            os: self.host.platform().as_str().to_owned(),
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
        lock(&self.config).view(self.shell_on_host())
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
                return Ok(cfg.view(self.shell_on_host()));
            }
            next
        };
        next.save(&self.dir).map_err(|e| format!("cannot save {SETTINGS_FILE}: {e}"))?;
        let saved = stamps(&self.dir).settings;

        let (view, before, after) = {
            let mut cfg = lock(&self.config);
            let before = cfg.look();
            cfg.set_settings(next, self.host.platform().chord_table());
            cfg.stamps.settings = saved;
            (cfg.view(self.shell_on_host()), before, cfg.look())
        };
        self.push_to_panes(&before, &after);
        self.sink.send(Target::All, Out::Settings(view.clone()));
        // Still under `settings_io`, which only the settings commands wait on: two changes reach the service in the
        // order they were made. The service never calls back into the core.
        self.ai.apply_settings(&view.settings);
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
            *cfg = Config::new(settings, themes, notes, now, self.host.platform().chord_table());
            (cfg.view(self.shell_on_host()), before, cfg.look())
        };
        self.push_to_panes(&before, &after);
        self.sink.send(Target::All, Out::Settings(view.clone()));
        self.ai.apply_settings(&view.settings);
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

    /// How a new pane is started: the scrollback, the picture memory, the Option key and the theme palette come from the settings; the
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
    /// `/.flatpak-info` or `FLATPAK_ID`) the pane starts in the sandbox, as on any Linux, unless the person turned Host shell
    /// on and the Flatpak has the permission: only then the pane's shell runs on the host and its directory comes from OSC 7
    /// only, and the two flags are set exactly then. A sandbox pane in a Flatpak says so on its first line. Everywhere else
    /// the spec is what it always was.
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
        let state = self.host_shell.state_for_pane(os == Os::Linux && in_flatpak(env, exists));
        let ShellResolved { launch, unusable } =
            self.shell.launch(&cfg.settings.shell, os, env, exists, &check_program_on_disk, state.spawns_on_host());
        let mut spec = PaneSpec::new(launch.program, cols, rows).args(launch.args);
        if state.spawns_on_host() {
            spec.host_spawn = true;
            spec.cwd_lookup = false;
        }
        spec.notice = crate::host_shell::compose_notice(
            unusable.map(|note| format!("holzWave: {note}")),
            crate::host_shell::notice_for(state),
        );
        spec.cell_px = cell;
        spec.scrollback = cfg.settings.scrollback_lines as usize;
        let limits = self.host.limits();
        if let Some(cap) = limits.max_scrollback_lines {
            spec.scrollback = spec.scrollback.min(cap);
        }
        // The setting is whole MiB (at most 512 after the load); the multiplication is checked all the same, and the
        // host's cap is applied to the spec, so the setting itself is never rewritten by it.
        spec.image_bytes = u64::from(cfg.settings.image_memory_mib).checked_mul(IMAGE_MIB_BYTES).unwrap_or(0);
        if let Some(cap) = limits.max_image_bytes {
            spec.image_bytes = spec.image_bytes.min(u64::try_from(cap).unwrap_or(u64::MAX));
        }
        spec.image_animation = self.image_animation.load(Ordering::SeqCst);
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
        self.integrations.flush();
        // Before the shells are asked to end, while their directories can still be read.
        self.flush_session();
        for handle in self.running() {
            handle.close();
        }
        self.sink.send(Target::All, Out::Exit);
    }

    /// End everything and wait, for at most three seconds: the layout is written, every shell is closed and joined.
    /// Running chat turns are stopped first and waited for at most one second more. Safe to call more than once, and
    /// from the exit events of the app. Called by `Drop` too.
    pub fn shutdown(&self) {
        // First: panes that end from now on are not taken out of the layout.
        self.exiting.store(true, Ordering::SeqCst);
        // Running chat turns end now and keep the text they had, before the layout is flushed and the process ends.
        self.ai.shutdown();
        self.saver.flush();
        self.integrations.flush();
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

    /// How many shells and how many connections are alive right now, as `(shells, connections)` (see [`count_sessions`]).
    /// A pane whose program has ended and that is held on the screen until Enter is not alive, and a pane that is still
    /// starting is not yet. It only reads: the lock of the panes is held for the length of one pass over a small map, and
    /// nothing is started, stopped or written. The vault plays no part: a live session counts whether the secrets are
    /// locked or not.
    pub fn session_counts(&self) -> (usize, usize) {
        let live: Vec<(PaneId, ConnState)> = lock(&self.panes)
            .iter()
            .filter_map(|(pane, slot)| match slot {
                Slot::Running(handle) => Some((*pane, handle.conn_state())),
                Slot::Starting(_) => None,
            })
            .collect();
        let held = lock(&self.held);
        count_sessions(live.into_iter().filter(|(pane, _)| !held.contains_key(pane)).map(|(_, state)| state))
    }

    /// End every shell and every connection of every pane and wait for them, for at most two seconds. The layouts, the
    /// windows, the settings, the history and the secrets are left as they are: a pane whose shell ends here stays in its
    /// window and the window is told `pane-exit` for it, which is what it is told when a shell ends by itself. The app
    /// keeps running. A shell that ignores a hangup is killed by the pane's own close path. Harmless when nothing runs,
    /// and when called again.
    pub fn stop_all(&self) {
        let _stopping = Stopping::raise(&self.stopping);
        let deadline = Instant::now() + STOP_ALL_BOUND;
        // On an engine thread a join would wait for itself.
        let on_engine_thread = std::thread::current().name().is_some_and(|n| n.starts_with("hw-pane-"));
        loop {
            let handles = self.running();
            let starting = lock(&self.panes).values().any(|slot| matches!(slot, Slot::Starting(_)));
            if handles.is_empty() && !starting {
                return;
            }
            for handle in &handles {
                handle.close();
            }
            if !on_engine_thread {
                for handle in &handles {
                    handle.join(deadline.saturating_duration_since(Instant::now()));
                }
            }
            if Instant::now() >= deadline {
                eprintln!("holzwave: a session did not end within the stop bound");
                return;
            }
            // A pane that was starting becomes a handle on the next pass (and is closed as it is promoted).
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The handle of a running pane. `None` for an unknown pane and for one that is still starting.
    fn handle(&self, pane: PaneId) -> Option<PaneHandle> {
        match lock(&self.panes).get(&pane) {
            Some(Slot::Running(handle)) => Some(handle.clone()),
            _ => None,
        }
    }

    /// The handle of a running terminal pane, for the source of shared content. `None` for an unknown pane, for one
    /// that is still starting and for one that shows a block.
    pub(crate) fn context_pane(&self, pane: PaneId) -> Option<PaneHandle> {
        self.handle(pane)
    }

    /// What a block pane shows, with the files it reads from: this computer, or a saved connection. `None` when the
    /// pane shows no block or the source cannot be made.
    pub(crate) fn context_block(&self, pane: PaneId) -> Option<(Block, Arc<dyn FileSource>)> {
        let block = self.blocks.block_of(pane)?;
        let source = self.blocks.source(&self.connections, &block.source).ok()?;
        Some((block, source))
    }

    /// What the files of `pane` are, for the tools that read files: whose they are, where a relative path starts and
    /// whether the machine is another one. Reads the tables only: no file system, no connection, no lock held across a
    /// call into another table. `None` for a pane that is not open, one that shows a block other than a file browser or
    /// a preview, and one that runs a WSL or Docker connection: no file tool reaches those.
    pub(crate) fn tool_file_target(&self, pane: PaneId) -> Option<FileTarget> {
        if !self.pane_is_open(pane) {
            return None;
        }
        if let Some(block) = self.blocks.block_of(pane) {
            let cwd = match block.kind {
                BlockKind::Files => Some(block.path.clone()),
                BlockKind::Preview => parent_folder(&block.path),
                BlockKind::Monitor | BlockKind::Chat | BlockKind::Integration => return None,
            };
            let remote = block.source != LOCAL;
            let label = if remote {
                self.connections.label_of(&block.source).unwrap_or_else(|| block.source.clone())
            } else {
                "This computer".to_owned()
            };
            let home = (!remote).then(|| self.blocks.home());
            return Some(FileTarget { source: block.source, label, remote, home, cwd });
        }
        let cwd = lock(&self.last_cwd).get(&pane).cloned();
        match self.connections.profile_of(pane) {
            None => Some(FileTarget {
                source: LOCAL.to_owned(),
                label: "This computer".to_owned(),
                remote: false,
                home: Some(self.blocks.home()),
                cwd,
            }),
            Some(profile) if profile.kind == ConnectionKind::Ssh => {
                Some(FileTarget { source: profile.id, label: profile.name, remote: true, home: None, cwd })
            }
            Some(_) => None,
        }
    }

    /// The files `tool_file_target` describes, made if need be: this computer, or the SFTP files of a saved
    /// connection, which can open a connection. Only a tool that is about to run calls this.
    pub(crate) fn tool_file_source(&self, pane: PaneId) -> Option<Arc<dyn FileSource>> {
        let target = self.tool_file_target(pane)?;
        self.blocks.source(&self.connections, &target.source).ok()
    }

    /// Saves `text` over a file for the agent's write tools: the same `Blocks::save_file` the editor uses, with the change
    /// time the card showed as `expected`. No lock of this struct is held across the call.
    pub(crate) fn tool_save_text(&self, source: &str, path: &str, text: &str, expected: Option<i64>) -> Result<Option<i64>, String> {
        self.file_save(source, path, text, expected, None).map(|saved| saved.modified)
    }

    /// The files of `source` (`local` or a saved connection), made if need be; none when they cannot be made. Undo reads
    /// the current text of a file with it.
    pub(crate) fn tool_open_source(&self, source: &str) -> Option<Arc<dyn FileSource>> {
        self.blocks.source(&self.connections, source).ok()
    }

    /// What the terminal of `pane` is: the saved connection it runs, or this computer. `None` for a pane that is not
    /// open and for one that shows a block. Reads the tables only.
    pub(crate) fn tool_terminal(&self, pane: PaneId) -> Option<TerminalTarget> {
        if !self.pane_is_open(pane) || self.blocks.block_of(pane).is_some() {
            return None;
        }
        Some(match self.connections.profile_of(pane) {
            Some(profile) => TerminalTarget { label: profile.name, remote: true },
            None => TerminalTarget { label: "This computer".to_owned(), remote: false },
        })
    }

    /// Runs `command` on the host of the saved connection `connection` through its exec channel, for the command tool. The
    /// exec primitive of a connection is built once for each version of the connection list, outside every lock of this
    /// struct (building it reads the keychain and makes a runtime), and the run holds no lock either: it blocks the calling
    /// turn thread until the command ended, the time ran out or `cancel` was raised. A failure is a scrubbed reason.
    pub(crate) fn tool_run_remote(
        &self,
        connection: &str,
        command: &str,
        limits: &ExecLimits,
        cancel: &ExecCancel,
    ) -> Result<ExecResult, ExecFault> {
        let scrubbed = |text: &str| ExecFault::Remote(holzwave_core::ai::provider::Detail::new(text).as_str().to_owned());
        let generation = self.connections.generation();
        let exec = self
            .remote_execs
            .get_or_build(connection, generation, || {
                if !self.connections.contains(connection) {
                    return Err("there is no such connection".to_owned());
                }
                let shared = self.connections.session(connection).map_err(|error| error.to_string())?;
                SshExec::on(shared).map(Arc::new).map_err(|error| error.to_string())
            })
            .map_err(|why| scrubbed(&why))?;
        exec.exec(command, limits, cancel).map_err(|error| scrubbed(&error.to_string()))
    }

    /// Like [`AppCore::handle`], for the commands that answer with something and so need the pane to exist.
    fn handle_or_err(&self, pane: PaneId) -> Result<PaneHandle, String> {
        self.handle(pane).ok_or_else(|| "no such pane".to_owned())
    }

    /// Register a terminal launch for `pane`: when the webview starts the pane, it runs the tool of the launch (with a clean
    /// environment, see [`PaneLaunch::apply_to`]) in place of the shell. The pane must be in a layout and must not have
    /// started: a launch for a running pane would never be used. The pane's label is remembered in `pane_launches.json`
    /// while the pane lives, so that the next start can say what the pane was and does not run it again.
    ///
    /// Called by the service after the person confirmed the action or clicked a read verb; nothing here asks, and nothing
    /// is run until the pane starts.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn register_launch(&self, pane: PaneId, launch: PaneLaunch) -> Result<(), String> {
        if self.exiting.load(Ordering::SeqCst) {
            return Err("the app is closing".into());
        }
        if !self.pane_is_open(pane) {
            return Err("no such pane".into());
        }
        if lock(&self.panes).contains_key(&pane) {
            return Err("this pane has started already".into());
        }
        let label = clean_launch_label(&launch.label);
        {
            let mut marks = lock(&self.launch_marks);
            let before = marks.insert(pane, label);
            let text = launch_marks_json(&marks);
            let fits = marks.len() <= MAX_LAUNCH_MARKS && text.len() as u64 <= MAX_LAUNCH_MARKS_BYTES;
            if !fits || persist::write_atomic(&self.dir.join(LAUNCHES_FILE), text.as_bytes()).is_err() {
                match before {
                    Some(old) => marks.insert(pane, old),
                    None => marks.remove(&pane),
                };
                return Err(if fits { "the launch could not be recorded" } else { "too many tool panes are open" }.into());
            }
        }
        // Replaces an earlier launch of the same pane, which is dropped (and its secrets wiped) here.
        lock(&self.launches).insert(pane, launch);
        Ok(())
    }

    /// The panes that hold their tool's last words until Enter, with the status of the tool.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn held_panes(&self) -> Vec<(PaneId, Option<u32>)> {
        lock(&self.held).iter().map(|(pane, code)| (*pane, *code)).collect()
    }

    /// The status a held pane's tool ended with (`Some(None)`: held with no status), or `None` for a pane that is not held.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn pane_held(&self, pane: PaneId) -> Option<Option<u32>> {
        lock(&self.held).get(&pane).copied()
    }

    /// A pane's tool ended and the pane stays (from its sink, on the engine thread of the pane). A pane that is already
    /// out of the table is not recorded again by a late event.
    pub(crate) fn record_held(&self, pane: PaneId, code: Option<u32>) {
        if lock(&self.panes).contains_key(&pane) {
            lock(&self.held).insert(pane, code);
            self.integrations.pane_held(pane, code);
        }
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
        // A registered launch is consumed by this start: it replaces the shell, the connection and the environment of the
        // app (no socket, no token, no `wsh` folder in `PATH`), and its secrets are dropped as soon as they were copied.
        let launch = lock(&self.launches).remove(&pane);
        if self.apply_remote(pane, &mut spec) {
            // A pane that shows a session of a server runs nothing here: a launch has no place in it.
            drop(launch);
        } else if let Some(launch) = launch {
            launch.apply_to(&mut spec);
            drop(launch);
        } else {
            if let Some((dir, token)) = lock(&self.ipc).clone() {
                spec.env = vec![
                    ("HOLZWAVE_IPC".into(), dir.to_string_lossy().into_owned()),
                    ("HOLZWAVE_TOKEN".into(), token),
                    ("HOLZWAVE_PANE".into(), pane.to_string()),
                ];
                // `wsh` is installed beside the app; the panes find it by name. Android bundles none: the folder of its
                // process is the system's, and nothing of the app is put on the `PATH` of a pane.
                let bundled = std::env::current_exe().ok().filter(|_| self.host.platform() != Platform::Android);
                if let Some(path) = bundled.as_deref().and_then(std::path::Path::parent).and_then(|bin| {
                    path_with_wsh(bin, std::env::var_os("PATH").as_deref())
                }) {
                    spec.env.push(("PATH".into(), path.to_string_lossy().into_owned()));
                }
            }
            // On Android the shell lives in the app's private folder: it starts there when nothing else was asked, and
            // `HOME` and `TMPDIR` name that folder and the cache folder. Nothing is started from these folders.
            if let Some(dirs) = self.private_dirs() {
                let env = private_pane_env(&dirs);
                if spec.cwd.is_none() && env.iter().any(|(name, _)| name == "HOME") {
                    spec.cwd = Some(dirs.home.clone());
                }
                spec.env.extend(env);
            }
            // A pane that runs a saved connection starts that instead of a local shell. A connection that cannot be made up
            // (a missing jump host, an unreadable keychain) is said on the pane's screen, and the pane is a local shell.
            // A pane that shows a file browser or a preview runs no process: the page draws its content.
            spec.blank = self.blocks.block_of(pane).is_some();
            if spec.blank {
                spec.notice = None;
            } else {
                self.apply_connection(pane, &mut spec);
            }
            // A pane that ran a tool when the app last ended is a shell now. Nothing is run again; the pane says what it
            // was, once.
            let ran = lock(&self.launch_marks).remove(&pane);
            if let Some(label) = ran {
                let note = format!("holzWave: this pane ran {label}; it is not started again");
                spec.notice = Some(match spec.notice.take() {
                    Some(earlier) => format!("{earlier}; {note}"),
                    None => note,
                });
                self.save_launch_marks();
            }
        }
        lock(&self.starts).remove(&pane);
        let started_animation = spec.image_animation;
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
            PaneEvent::Bell => {
                // A hint for the marks of the bar: only the pane's id, to the window that has the pane.
                let target = lock(&routes).target_of(pane);
                if let Some(target) = target {
                    sink.send(target, Out::Bell { pane });
                }
            }
            PaneEvent::CommandEnd { exit } => {
                // A hint for the marks of the bar: the pane's id and the status number, to the window that has the pane.
                let target = lock(&routes).target_of(pane);
                if let Some(target) = target {
                    sink.send(target, Out::CommandEnd { pane, code: exit });
                }
            }
            PaneEvent::Exited { code } => {
                if let Some(core) = me.upgrade() {
                    core.pane_gone(pane, code);
                }
            }
            // The pane stays open with its last words on the screen until Enter or a close: it is recorded, so that the
            // shell can tell the page, and cleared by `pane_gone`.
            PaneEvent::Held { code } => {
                if let Some(core) = me.upgrade() {
                    core.record_held(pane, code);
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
                let animation = handle.clone();
                self.promote(pane, handle, started_with);
                // A switch that came in while the shell started missed this pane in the loop of `set_image_animation` (it
                // was not running yet) and the spec was made before it: tell it now what is true now.
                let now = self.image_animation.load(Ordering::SeqCst);
                if now != started_animation {
                    let _ = animation.set_image_animation(now);
                }
                Ok(())
            }
            // The sink was told, and `pane_gone` forgot the slot, before `spawn_ghostty` returned.
            Err(e) => Err(e.to_string()),
        }
    }

    /// Make `spec` the saved connection `pane` runs, if it runs one. A connection that cannot be made up (a missing jump
    /// host, an unreadable keychain) is said on the pane's screen and leaves the pane a local shell. Only a connection that
    /// was made up and has `wsh` turned on gets a tunnel; its variables live in the tunnel and never in `spec.env`, whose
    /// `Debug` prints its values.
    fn apply_connection(&self, pane: PaneId, spec: &mut PaneSpec) {
        match self.connections.apply(pane, spec) {
            // The pane is a local shell then, so what it was already told (its sandbox) stays after the new line.
            Err(e) => {
                let earlier = spec.notice.take();
                spec.notice = crate::host_shell::compose_notice(
                    Some(format!("holzWave: cannot open the connection: {e}")),
                    earlier.as_deref(),
                );
            }
            Ok(()) if spec.ssh_connection.is_some() => {
                // A saved SSH connection starts no local shell: nothing of it is wrapped for the host, and the sentence
                // about the sandbox of a local shell is not true of it.
                spec.host_spawn = false;
                spec.notice = crate::host_shell::without_sandbox_notice(spec.notice.take());
                // Where the connection points now is where this pane is: recorded before anything can be asked of it.
                if let Some(profile) = self.connections.profile_of(pane) {
                    lock(&self.pane_targets).insert(pane, crate::wsh_tunnel::Target::of(&profile));
                }
                spec.tunnel = crate::wsh_tunnel::tunnel_for(self, pane);
            }
            Ok(()) => {}
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
                if self.exiting.load(Ordering::SeqCst) || self.stopping.load(Ordering::SeqCst) > 0 {
                    // The app began to end (or every session is being stopped) while this shell started; `shutdown` and
                    // `stop_all` did not see it.
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
        lock(&self.held).remove(&pane);
        self.integrations.pane_ended(pane);
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
            if self.exiting.load(Ordering::SeqCst) || self.stopping.load(Ordering::SeqCst) > 0 {
                // The app is ending, or every session is being stopped: what the user had stays in the layout, and the
                // window hears that the pane ended.
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
                        // The pane ended by itself (its session ended and the person pressed Enter) and the app ends
                        // with it. The layout keeps the pane; it is not shown the same ended session at every start.
                        self.remote_panes.forget(pane);
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

    /// Tell the program of a pane that something that is not text was pasted: an empty bracketed paste when it turned on
    /// bracketed paste, [`PasteOutcome::Empty`] and no bytes otherwise. The bytes are fixed and carry nothing from the
    /// page. Waits for the engine's answer, so the command is async.
    pub fn paste_cue(&self, pane: PaneId) -> Result<PasteOutcome, String> {
        self.handle_or_err(pane)?.paste_cue().map_err(|e| e.to_string())
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

    /// Whether pictures may animate (the page's reduced-motion fact). Applied to every running pane now and given to each
    /// pane that starts later; `false` stops every animation on the frame it shows, `true` lets them go on. Takes no pane:
    /// motion is a property of the person's window.
    pub fn set_image_animation(&self, enabled: bool) {
        self.image_animation.store(enabled, Ordering::SeqCst);
        for handle in self.running() {
            // A pane that is closing refuses, which is fine: there is nothing left to tell.
            let _ = handle.set_image_animation(enabled);
        }
    }

    /// One piece of an image's pixels, as the raw answer of the `image_data` command: a header of four big-endian numbers
    /// (width, height, total length in bytes, offset) and then at most [`IMAGE_CHUNK_BYTES`] bytes of straight RGBA from
    /// `offset` on. Every failure (no such pane, no such image, another generation, a pane that is closing or busy) is the
    /// same constant text: it never carries a number or a byte that a program sent. Waits for the pane's own thread, so
    /// the command is async.
    pub fn image_data(&self, pane: PaneId, image: u32, generation: u64, offset: u64) -> Result<Vec<u8>, String> {
        const NO_IMAGE: &str = "no such image";
        let handle = self.handle(pane).ok_or(NO_IMAGE)?;
        // An offset that does not fit `usize` is past every image; the engine brings it to the end.
        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        match handle.image_chunk(image, generation, offset, IMAGE_CHUNK_BYTES) {
            Ok(Some(chunk)) => image_data_bytes(&chunk).ok_or_else(|| NO_IMAGE.to_owned()),
            _ => Err(NO_IMAGE.to_owned()),
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

/// The fixed sentence of a request of a session pane that failed: the fault of its connection (unreachable while it
/// reconnects, signed out, refused by the server), or, for any other failure of the pane, that it shows no session.
fn session_pane_failure(error: holzwave_core::Error) -> String {
    match holzwave_core::remote::hub::HubError::of(&error) {
        Some(fault) => SessionFault::from(fault).into(),
        None => crate::sessions::NOT_A_SESSION_PANE.to_owned(),
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

/// A variable of the process environment, for the pure functions that take a lookup.
fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Whether `url` is a link that may be opened under the list `schemes` (the `link_schemes` setting in force): at most
/// [`MAX_URL_BYTES`], no control, blank or invisible character, a scheme of 2 to 32 characters (the rule of the list,
/// checked again here, so no Windows drive letter is a scheme whatever the list holds) that the list holds (compared
/// without regard to case) and that is none of the five that are never opened, and the shape of that scheme: a host after
/// `//` for http and https, an address for mailto, something after the colon for any other. For every scheme except
/// http, https and mailto the link must not name a host or first part that starts with a dash, written `-` or `%2d`
/// ([`holzwave_core::links::option_like_authority`]), because a handler of a custom scheme gets the text as written and
/// may read that as an option. `file` is never handed to the opener: a file link is revealed through the pane path of
/// [`AppCore::open_url`]. The text is never interpreted further, and a refusal is a fixed sentence that does not repeat
/// the link.
fn check_link(url: &str, schemes: &[String]) -> Result<(), String> {
    let (scheme, rest) = check_link_text(url)?;
    let scheme = scheme.to_ascii_lowercase();
    if holzwave_core::links::hard_denied(&scheme) {
        return Err("links of this kind are never opened".into());
    }
    if scheme == "file" {
        return Err("a file link is shown in the file manager from a pane on this computer, it is not opened".into());
    }
    // The rule of the list, asked again of the link's own scheme: a one-letter scheme is a Windows drive letter and
    // never reaches the opener, whatever the list holds.
    if !holzwave_core::links::valid_scheme(&scheme) {
        return Err("the link does not start with a scheme name of 2 to 32 characters".into());
    }
    if !schemes.iter().any(|listed| *listed == scheme) {
        return Err("links of this kind are not in the list of link schemes in the settings".into());
    }
    let names_something = match scheme.as_str() {
        "http" | "https" => rest.strip_prefix("//").is_some_and(|host| !host.is_empty() && !host.starts_with('/')),
        _ => !rest.is_empty(),
    };
    if !names_something {
        return Err("the link names nothing to open".into());
    }
    // A handler of a custom scheme gets the text as written and may read a leading dash as an option.
    if !matches!(scheme.as_str(), "http" | "https" | "mailto") && holzwave_core::links::option_like_authority(rest) {
        return Err("the link names a host or a part that starts with a dash, which a handler may read as an option".into());
    }
    Ok(())
}

/// The checks that every link passes before its scheme is looked at: at most [`MAX_URL_BYTES`], no control, blank or
/// invisible character (a direction mark or override, a zero-width character, a soft hyphen: the rule is
/// [`holzwave_core::links::has_hidden_char`], the same one for every scheme and for `file` links), and a colon. Gives
/// the scheme as written and the rest. The page checks the same things, but only this check counts.
fn check_link_text(url: &str) -> Result<(&str, &str), String> {
    if url.len() > MAX_URL_BYTES {
        return Err(format!("the link is longer than {MAX_URL_BYTES} bytes"));
    }
    if holzwave_core::links::has_hidden_char(url) {
        return Err("the link has a control, blank or invisible character".into());
    }
    url.split_once(':').ok_or_else(|| "the link has no scheme".to_owned())
}

/// Whether `url` has the scheme `file`, in any letter case.
fn is_file_link(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("file"))
}

/// The path a `file:` link names on this machine, or why it is refused. `os` decides the forms: on `windows` a drive
/// path (`file:///C:/dir/file`, giving `C:/dir/file`) and nothing else, elsewhere an absolute path.
///
/// The host has to be empty or `localhost`. The path is split at its slashes first and each segment is percent decoded
/// once, so an escape can never make a separator: `%2f` and `%5c` inside a segment, a decoded `..`, NUL or any other
/// control character, bytes that are not UTF-8, an empty segment inside the path, a query or a fragment, and on
/// Windows a segment with a character that no name has, a segment of only dots and the network form (`file://///h/s`)
/// are all refused. It does not look at the file system, and it does not rely on the engine's cleaning of the link.
/// Every refusal is a fixed sentence that does not repeat the link.
fn file_link_path(os: &str, url: &str) -> Result<String, String> {
    let (scheme, rest) = url.split_once(':').ok_or("the link has no scheme")?;
    if !scheme.eq_ignore_ascii_case("file") {
        return Err("this is not a file link".into());
    }
    let Some(rest) = rest.strip_prefix("//") else {
        return Err("a file link needs the form file:///path".into());
    };
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return Err("a file link on another computer is not shown".into());
    }
    if path.is_empty() {
        return Err("the file link has no path".into());
    }
    if path.contains(['?', '#']) {
        return Err("a file link with a query or a fragment is not shown".into());
    }
    let windows = os == "windows";
    if windows && path.starts_with("//") {
        return Err("a network path is not shown".into());
    }
    let mut segments: Vec<String> = Vec::new();
    for raw in path[1..].split('/') {
        let segment = percent_decode(raw)?;
        if segment.contains(['/', '\\']) {
            return Err("the file link has an encoded separator".into());
        }
        if segment.chars().any(char::is_control) {
            return Err("the file link has a control character".into());
        }
        if segment == ".." {
            return Err("the file link goes up a folder".into());
        }
        if windows
            && (segment.contains(['<', '>', ':', '"', '|', '?', '*']) && !(segments.is_empty() && is_drive_name(&segment))
                || (!segment.is_empty() && segment.chars().all(|c| c == '.' || c == ' ')))
        {
            return Err("the file link has a name that Windows does not allow".into());
        }
        segments.push(segment);
    }
    // A folder may end in a slash; an empty segment anywhere else is a doubled slash.
    if segments.len() > 1 && segments.last().is_some_and(String::is_empty) {
        segments.pop();
    }
    if segments.len() > 1 && segments.iter().any(String::is_empty) {
        return Err("the file link has an empty part in its path".into());
    }
    if windows {
        if !segments.first().is_some_and(|first| is_drive_name(first)) {
            return Err("a file link needs a drive on Windows".into());
        }
        let mut text = segments.join("/");
        if segments.len() == 1 {
            text.push('/');
        }
        Ok(text)
    } else {
        if segments.first().is_some_and(|first| is_drive_name(first)) {
            return Err("a drive path is not a path on this system".into());
        }
        Ok(format!("/{}", segments.join("/")))
    }
}

/// Whether `text` is a drive letter and a colon (`C:`).
fn is_drive_name(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// `text` with its `%XX` escapes decoded once, as UTF-8. A `%` without two hex digits and bytes that are not UTF-8 are
/// refused. The input is a part of a link of at most [`MAX_URL_BYTES`] bytes, so nothing here is unbounded.
fn percent_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let digits = bytes.get(at + 1..at + 3).ok_or("the file link has a broken escape")?;
            let hex = |d: u8| char::from(d).to_digit(16).map(|v| v as u8);
            match (hex(digits[0]), hex(digits[1])) {
                (Some(high), Some(low)) => out.push(high << 4 | low),
                _ => return Err("the file link has a broken escape".into()),
            }
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "the file link is not valid text".into())
}

/// How the platform opens a link: validated against `schemes` first, then one fixed program with the link as its only argument.
/// macOS uses `/usr/bin/open` by absolute path; Linux uses `xdg-open`, the one program that is looked up through
/// PATH (there is no fixed place for it). Windows hands the link to the URL handler of `url.dll` through
/// `rundll32.exe` under `%SystemRoot%\System32`: no `cmd`, no shell string, so `&` and `^` of a link are plain text.
/// A double quote is refused there because the argument is rebuilt into a command line.
///
/// `env` looks up a variable of the process; only the Windows launcher reads it.
fn open_launch(
    os: &str,
    url: &str,
    schemes: &[String],
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Launch, String> {
    check_link(url, schemes)?;
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

/// The link that opens System Settings at Privacy & Security, Local Network (macOS).
///
/// The scheme `x-apple.systempreferences:` and the legacy identifier `com.apple.preference.security` of the Privacy &
/// Security pane are what several shipping apps use, and on macOS 27.0.1 the Privacy & Security settings extension still
/// declares `allowsXAppleSystemPreferencesURLScheme = true` with `legacyBundleIdentifier = com.apple.preference.security`.
/// The anchor `Privacy_LocalNetwork` is NOT documented by Apple and could not be confirmed from files on that system; a
/// system that does not know it is expected to open the Privacy & Security pane (test-plan row LN4 looks).
const LOCAL_NETWORK_SETTINGS_URL: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork";

/// How the platform opens the Local Network privacy setting: macOS only, `/usr/bin/open` by absolute path with the one
/// constant link as its only argument. It takes nothing from the page and does not go through [`check_link`], which allows
/// web links only. Every other platform has no such setting and gets an error.
fn settings_launch(os: &str) -> Result<Launch, String> {
    match os {
        "macos" => Ok(Launch::plain("/usr/bin/open", vec![LOCAL_NETWORK_SETTINGS_URL.to_owned()])),
        other => Err(format!("there is no local network setting on {other}")),
    }
}

/// The Windows directory: `SystemRoot` if it is a drive path, else `C:\Windows`. A relative or network value of the
/// variable is not taken, so the launchers never leave the local drive.
pub fn system_root(env: &dyn Fn(&str) -> Option<String>) -> String {
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

/// The variables a local shell gets from the private folders of an Android host: `HOME` and `TMPDIR`. A folder that is
/// not an absolute path, or whose text has a control character, gives no variable: the shell then keeps what it
/// inherits.
fn private_pane_env(dirs: &PrivateDirs) -> Vec<(String, String)> {
    let text = |dir: &Path| {
        let text = dir.to_str()?;
        (text.starts_with('/') && !text.chars().any(char::is_control)).then(|| text.to_owned())
    };
    let mut env = Vec::new();
    if let Some(home) = text(&dirs.home) {
        env.push(("HOME".to_owned(), home));
    }
    if let Some(cache) = text(&dirs.cache) {
        env.push(("TMPDIR".to_owned(), cache));
    }
    env
}

/// The home for `~/` in the key paths of connections and in the paths of blocks: the private home folder on the Android
/// host (the same folder `reveal_path` and the shell use, so `~/.ssh/id_ed25519` names a file a person can put there), the
/// process's own home everywhere else.
fn shell_home(host: &dyn Host) -> Option<PathBuf> {
    let private = if host.platform() == Platform::Android { host.private_dirs().map(|dirs| dirs.home) } else { None };
    private.or_else(user_home)
}

/// The home directory for `~/`: `HOME`, and on Windows `USERPROFILE`. An empty value is no value. `private_home` is the
/// home folder of the Android host ([`Host::private_dirs`]; `None` everywhere else): when it is given it is the home,
/// whatever the environment says, because an Android app's process has no `HOME` of its own worth reading.
fn home_dir(os: &str, env: &dyn Fn(&str) -> Option<String>, private_home: Option<&Path>) -> Option<PathBuf> {
    if let Some(home) = private_home {
        return Some(home.to_path_buf());
    }
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

/// The name of this computer from what the platform offers without starting a program (the link to a server names the device
/// with it). Empty on macOS and wherever the platform gives none.
pub(crate) fn computer_name(os: &str) -> String {
    hostname_from(os, &process_env, &|file| persist::read_limited(Path::new(file), 1024).ok().flatten())
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
    use holzwave_core::integrate::run::EnvPolicy;
    use holzwave_core::layout::Axis;

    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn a_failed_request_of_a_session_pane_says_the_fault_of_its_connection_not_that_it_shows_no_session() {
        use holzwave_core::remote::hub::HubError;
        use holzwave_core::remote::proto::ErrorWord;
        use holzwave_core::server_link::Fault;
        let said = |error: HubError| session_pane_failure(error.into());
        assert_eq!(said(HubError::Fault(Fault::Unreachable)), SessionFault::Unreachable.sentence());
        assert_eq!(said(HubError::Disconnected), SessionFault::Unreachable.sentence());
        assert_eq!(said(HubError::Fault(Fault::SignedOut)), SessionFault::SignedOut.sentence());
        assert_eq!(said(HubError::Word(ErrorWord::Refused)), SessionFault::Refused.sentence());
        assert_eq!(said(HubError::Word(ErrorWord::NotFound)), SessionFault::NotFound.sentence());
        // Any other failure of the pane is the one sentence of a pane that shows no session.
        assert_eq!(session_pane_failure(holzwave_core::Error::Pty("the pane is closed".into())), crate::sessions::NOT_A_SESSION_PANE);
        assert_eq!(session_pane_failure(std::io::Error::other("other").into()), crate::sessions::NOT_A_SESSION_PANE);
    }

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

        fn history_search(&self, query: &str, limit: usize) -> Vec<holzwave_core::history::Hit> {
            self.history_search_in(MAIN_WINDOW, query, limit)
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

        fn move_pane_to_workspace(&self, pane: PaneId, workspace: usize) -> View {
            self.move_pane_to_workspace_in(MAIN_WINDOW, pane, workspace).expect("main")
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
    fn a_pane_dropped_on_a_workspace_chip_moves_there_and_keeps_its_shell_and_name() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        let first = core.view().focus;
        let second = core.split(false).focus;
        core.rename_pane(second, "db");
        let made = core.new_workspace("work").focus;
        core.select_workspace(0);

        let view = core.move_pane_to_workspace(second, 1);

        assert_eq!(view.workspaces.iter().position(|w| w.active), Some(1), "the workspace it went to is shown");
        let mut ids: Vec<_> = view.panes.iter().map(|(p, _)| *p).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![second, made], "it sits next to the pane of that workspace; no pane is started again");
        assert_eq!(view.focus, second);
        assert_eq!(view.pane_names, vec![(second, "db".to_string())], "the name follows the pane");
        let left_behind: Vec<_> = core.select_workspace(0).panes.iter().map(|(p, _)| *p).collect();
        assert_eq!(left_behind, vec![first]);
    }

    #[test]
    fn a_pane_dropped_on_a_workspace_chip_with_bad_ids_changes_nothing() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        let first = core.view().focus;
        core.new_workspace("work");
        core.select_workspace(0);
        let before = core.view();

        for (pane, workspace) in [(9999, 1), (first, 9), (first, 0), (first, usize::MAX)] {
            assert_eq!(without_rev(&core.move_pane_to_workspace(pane, workspace)), without_rev(&before), "{pane} {workspace}");
        }
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
    fn the_paste_cue_is_an_error_for_an_unknown_pane_and_silent_unless_bracketed_paste_is_on() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        assert_eq!(core.paste_cue(pane), Ok(PasteOutcome::Empty), "cat did not ask for bracketed paste");
        assert!(core.paste_cue(31_337).is_err(), "an unknown pane has no program to tell");

        let (core, rec, _dir) = sh("stty -echo; printf '\\033[?2004h'; printf ready; exec cat -v");
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        assert!(wait_until(WAIT, || rec.has_text(pane, "ready")), "no output: {:?}", rec.all());
        assert_eq!(core.paste_cue(pane), Ok(PasteOutcome::Written));
        core.text(pane, "\r".into()).expect("enter");
        assert!(wait_until(WAIT, || rec.has_text(pane, "^[[200~^[[201~")), "the cue never arrived: {:?}", rec.all());
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
    fn a_block_opens_in_a_new_tab_or_beside_the_focused_pane_and_is_remembered() {
        let dir = Scratch::new("blocks");
        let folder = dir.0.join("docs");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("a.md"), "# A").unwrap();
        let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());
        let before = core.view().panes.len();

        let view = core.block_open_in("main", "tab", BlockKind::Files, "local", folder.to_str().unwrap()).unwrap();
        assert_eq!(view.tabs.len(), 2, "a new tab");
        let blocks = core.blocks_list();
        assert_eq!(blocks.len(), 1);
        assert_eq!((blocks[0].pane, blocks[0].block.kind), (view.focus, BlockKind::Files));
        assert_eq!(blocks[0].source_label, "This computer");

        let listing = core.files_list("local", folder.to_str().unwrap()).unwrap();
        assert_eq!(listing.entries.len(), 1);
        let opened = core.file_open("local", folder.join("a.md").to_str().unwrap()).unwrap();
        assert!(opened.html.unwrap().contains("<h1>A</h1>"));

        let beside = core.block_open_in("main", "right", BlockKind::Preview, "local", folder.join("a.md").to_str().unwrap()).unwrap();
        assert_eq!(beside.panes.len(), 2, "the focused tab was split: {before} panes became {}", beside.panes.len());
        assert_eq!(core.blocks_list().len(), 2);
        core.block_update(beside.focus, folder.join("b.md").to_str().unwrap()).unwrap();
        assert!(core.blocks_list().iter().any(|b| b.block.path.ends_with("b.md")));

        // A block with no source is refused before any pane is made.
        let tabs = core.view().tabs.len();
        assert!(core.block_open_in("main", "tab", BlockKind::Preview, "", "/x").is_err());
        assert_eq!(core.view().tabs.len(), tabs, "no empty pane is left behind");
        assert!(core.files_list("no-such-connection", "/").is_err());

        // A restart opens the same views on the same panes.
        core.flush();
        drop(core);
        let again = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());
        assert_eq!(again.blocks_list().len(), 2);
    }

    /// A saved SSH connection that needs no secret and is never reached: a key file that does not exist.
    fn saved_ssh(core: &AppCore, id: &str, name: &str) {
        let profile = Profile {
            user: "holz".into(),
            auth: holzwave_core::profiles::Auth::Key,
            key_path: "/nonexistent/holzwave-test-key".into(),
            ..Profile::new(id, name, "127.0.0.1")
        };
        core.connection_save(profile, SecretChange::Keep).expect("a valid connection");
    }

    #[test]
    fn the_connections_of_the_core_get_the_host_as_their_prompter() {
        use holzwave_core::ssh::{PromptAnswer, PromptRound, Prompter, SshAuth};

        let (core, rec, _dir) = core("/bin/cat", &[]);
        let profile = Profile {
            user: "holz".into(),
            auth: holzwave_core::profiles::Auth::Interactive,
            ..Profile::new("talos", "Talos", "talos.example")
        };
        core.connection_save(profile, SecretChange::Keep).expect("a valid connection");

        // The target of an interactive connection carries the host that raises the question.
        let target = core.connections.ssh_target("talos").unwrap();
        let SshAuth::Interactive { prompter: Some(prompter) } = &target.auth else { panic!("no prompter: {:?}", target.auth) };
        let prompter: Arc<dyn Prompter> = prompter.clone();
        let asking = std::thread::spawn(move || {
            prompter.ask(PromptRound {
                // Whatever the core's own adapter stamps wins: this is what a server would like the dialog to say.
                connection: "evil".into(),
                host: "evil.example".into(),
                name: "Login".into(),
                instructions: String::new(),
                prompts: vec![PromptLine { text: "Code:".into(), echo: false }],
            })
        });
        let started = Instant::now();
        let id = loop {
            let found = rec.all().into_iter().find_map(|o| match o {
                Out::LoginPrompt { id, connection, host, .. } => Some((id, connection, host)),
                _ => None,
            });
            if let Some((id, connection, host)) = found {
                assert_eq!((connection.as_str(), host.as_str()), ("Talos", "talos.example"), "the title is the saved connection's");
                break id;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "the question never arrived");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(!core.login_prompt_answer(&id, vec!["one".into(), "too many".into()]), "the count of answers is the question's");
        assert!(core.login_prompt_answer(&id, vec!["424242".into()]));
        assert!(matches!(asking.join().unwrap(), PromptAnswer::Answers(a) if a == ["424242"]));
        assert!(rec.all().iter().any(|o| matches!(o, Out::LoginPromptEnd { outcome: PromptEnd::Answered, .. })));
        assert!(!core.login_prompt_cancel(&id), "an ended question cannot be touched");
        // No answer is in anything the recorder saw.
        assert!(!format!("{:?}", rec.all()).contains("424242"));
    }

    #[test]
    fn monitor_pane_of_this_computer_is_sampled_by_its_pane_id() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        let view = core.block_open_in("main", "tab", BlockKind::Monitor, LOCAL, "").unwrap();
        match core.monitor_sample(view.focus) {
            Ok(holzwave_core::monitor::Reading::Live(s)) => assert!(s.cores >= 1 && s.mem_total > 0),
            other => panic!("not a live reading: {other:?}"),
        }
    }

    #[test]
    fn monitor_pane_refuses_a_pane_that_shows_no_monitor() {
        let (core, _rec, dir) = core("/bin/cat", &[]);
        let folder = dir.0.join("docs");
        std::fs::create_dir(&folder).unwrap();
        let shell = only_pane(&core);
        let files = core.block_open_in("main", "tab", BlockKind::Files, LOCAL, folder.to_str().unwrap()).unwrap().focus;
        for pane in [shell, files, 9_999] {
            assert_eq!(core.monitor_sample(pane), Err("this pane shows no system monitor".to_owned()), "pane {pane}");
        }
    }

    #[test]
    fn monitor_pane_of_another_computer_needs_a_saved_ssh_connection_at_open() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        let panes = core.view().panes.len();
        let tabs = core.view().tabs.len();
        let error = core.block_open_in("main", "tab", BlockKind::Monitor, "nope", "").unwrap_err();
        assert!(error.contains("no connection nope"), "{error}");
        let wsl = Profile { kind: ConnectionKind::Wsl, ..Profile::new("box", "Box", "Ubuntu") };
        core.connection_save(wsl, SecretChange::Keep).unwrap();
        let error = core.block_open_in("main", "tab", BlockKind::Monitor, "box", "").unwrap_err();
        assert!(error.contains("needs an SSH connection"), "{error}");
        assert_eq!((core.view().panes.len(), core.view().tabs.len()), (panes, tabs), "no empty pane was made");
        saved_ssh(&core, "talos", "Talos");
        let view = core.block_open_in("main", "tab", BlockKind::Monitor, "talos", "").unwrap();
        let blocks = core.blocks_list();
        let shown = blocks.iter().find(|b| b.pane == view.focus).expect("the pane has a block");
        assert_eq!((shown.block.source.as_str(), shown.source_label.as_str()), ("talos", "Talos"));
    }

    #[test]
    fn monitor_pane_of_a_deleted_connection_answers_with_the_reason() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        let pane = only_pane(&core);
        core.blocks.assign(pane, Block { kind: BlockKind::Monitor, source: "gone".into(), path: String::new(), edit: false }).unwrap();
        let error = core.monitor_sample(pane).unwrap_err();
        assert!(error.contains("no connection gone"), "{error}");
        assert!(!error.starts_with("unsupported"), "{error}");
    }

    #[test]
    fn closing_the_last_monitor_pane_of_a_computer_drops_its_session() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        saved_ssh(&core, "talos", "Talos");
        let first = core.block_open_in("main", "tab", BlockKind::Monitor, "talos", "").unwrap().focus;
        let second = core.block_open_in("main", "right", BlockKind::Monitor, "talos", "").unwrap().focus;
        // The key file does not exist, so the sample fails without any network; the session is built all the same.
        assert!(core.monitor_sample(first).is_err());
        assert_eq!(core.remote_monitors.len(), 1);
        core.close_pane(first);
        assert_eq!(core.remote_monitors.len(), 1, "another pane still shows the computer");
        core.close_pane(second);
        assert_eq!(core.remote_monitors.len(), 0, "the last pane is gone");
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
            .filter(|name| !name.starts_with("history.db"))
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
    #[test]
    fn the_single_answer_path_still_refuses_an_ai_request() {
        use holzwave_ipc::Cmd;
        let (core, _rec, _dir) = core("sh", &[]);
        let pane = core.view().focus;
        let ask = || Cmd::Ai { prompt: "hi".into(), stdin_bytes: 0 };
        let said = Err("wsh ai is answered as a stream, not as one line".to_owned());
        assert_eq!(core.ipc_command(Some(pane), ask()), said);
        assert_eq!(core.ipc_command(None, ask()), said);
        assert_eq!(core.ipc_command(Some(9999), ask()), said, "a missing pane cannot change the answer");
    }

    #[test]
    fn wsh_names_a_pane_opens_a_file_beside_it_and_refuses_what_is_not_a_pane_or_a_file() {
        use holzwave_ipc::Cmd;
        let (core, rec, dir) = core("sh", &[]);
        let pane = core.view().focus;
        assert_eq!(core.ipc_command(None, Cmd::Ping).as_deref(), Ok("holzWave"));
        assert!(core.ipc_command(None, Cmd::Setmeta { key: "title".into(), value: "x".into() }).is_err(), "no pane named");
        assert!(core.ipc_command(Some(9999), Cmd::Setmeta { key: "title".into(), value: "x".into() }).unwrap_err().contains("not open"));

        assert_eq!(core.ipc_command(Some(pane), Cmd::Setmeta { key: "title".into(), value: "build".into() }), Ok(String::new()));
        assert!(core.view().pane_names.iter().any(|(id, name)| *id == pane && name == "build"));
        assert!(rec.log.lock().unwrap().iter().any(|(_, o)| matches!(o, Out::Layout(_))), "the window is told");

        let before = core.view().panes.len();
        assert!(core.ipc_command(Some(pane), Cmd::View { path: "relative.md".into() }).unwrap_err().contains("no such file"));
        assert!(core.ipc_command(Some(pane), Cmd::View { path: dir.0.join("missing.md").to_string_lossy().into_owned() }).is_err());
        assert_eq!(core.view().panes.len(), before, "a refused file makes no pane");
        let file = dir.0.join("notes.md");
        std::fs::write(&file, "# hi").unwrap();
        assert_eq!(core.ipc_command(Some(pane), Cmd::Edit { path: file.to_string_lossy().into_owned() }), Ok(String::new()));
        assert_eq!(core.view().panes.len(), before + 1, "the file opens beside the pane");
    }

    #[test]
    fn a_history_request_from_the_focused_pane_opens_the_dialog_in_its_window() {
        use holzwave_ipc::Cmd;
        let (core, rec, _dir) = core("sh", &[]);
        let pane = core.view().focus;
        assert_eq!(core.ipc_command(Some(pane), Cmd::HistOpen), Ok(String::new()));
        let opened: Vec<(Target, Out)> = rec.log.lock().unwrap().iter().filter(|(_, o)| matches!(o, Out::HistoryOpen { .. })).cloned().collect();
        assert_eq!(opened, [(Target::window(MAIN_WINDOW), Out::HistoryOpen { pane })], "one event, for the window of the pane");
    }

    #[test]
    fn a_history_request_is_refused_for_no_pane_an_unknown_pane_and_a_pane_out_of_focus() {
        use holzwave_ipc::Cmd;
        let (core, rec, _dir) = core("sh", &[]);
        let first = core.view().focus;
        let opened = || rec.log.lock().unwrap().iter().filter(|(_, o)| matches!(o, Out::HistoryOpen { .. })).count();
        assert_eq!(core.ipc_command(None, Cmd::HistOpen), Err("HOLZWAVE_PANE is not set".to_owned()));
        assert_eq!(core.ipc_command(Some(9999), Cmd::HistOpen), Err("this pane is not open any more".to_owned()));
        let second = core.split_in(MAIN_WINDOW, true).expect("the window").focus;
        assert_ne!(core.view().focus, first, "the new pane has the focus now");
        assert_eq!(core.ipc_command(Some(first), Cmd::HistOpen), Err(HISTORY_NOT_FOCUSED.to_owned()));
        assert_eq!(opened(), 0, "a refusal sends nothing");
        assert_eq!(core.ipc_command(Some(second), Cmd::HistOpen), Ok(String::new()));
        assert_eq!(opened(), 1);
    }

    #[test]
    fn the_folder_of_wsh_goes_in_front_of_the_path_once() {
        let dir = Scratch::new("wshpath");
        let name = if cfg!(windows) { "wsh.exe" } else { "wsh" };
        assert!(path_with_wsh(&dir.0, Some(std::ffi::OsStr::new("/usr/bin"))).is_none(), "no wsh beside the app");
        std::fs::write(dir.0.join(name), "").unwrap();
        let joined = path_with_wsh(&dir.0, Some(std::env::join_paths([Path::new("/usr/bin")]).unwrap().as_os_str())).unwrap();
        let parts: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(parts, vec![dir.0.clone(), PathBuf::from("/usr/bin")]);
        assert!(path_with_wsh(&dir.0, Some(joined.as_os_str())).is_none(), "already there");
    }

    #[test]
    fn the_shell_hook_fills_the_history_and_secrets_stay_out_of_it() {
        use holzwave_ipc::Cmd;
        let (core, _rec, _dir) = core("sh", &[]);
        let pane = core.view().focus;
        let hist = |line: &str| Cmd::Hist { line: line.into(), cwd: "/home/holz".into(), exit: Some(0) };
        for line in ["kubectl get pods", "ls -la", "export TOKEN=abc", " hidden"] {
            assert_eq!(core.ipc_command(Some(pane), hist(line)), Ok(String::new()));
        }
        let all: Vec<String> = core.history_search("", 10).into_iter().map(|h| h.cmd).collect();
        assert_eq!(all.len(), 2);
        assert_eq!(core.history_search("kgp", 10)[0].cmd, "kubectl get pods");
        assert_eq!(core.history_delete("ls -la"), Ok(1));
        assert_eq!(core.history_search("", 10).len(), 1);
        assert_eq!(core.history_clear(), Ok(()));
        assert!(core.history_search("", 10).is_empty());
    }

    /// A core with the saved connection `web-1` (written before the core starts, so no keychain is touched), a local
    /// first pane and a second pane, in focus, that runs `web-1`. Returns the two panes in that order.
    fn core_with_a_connection_pane() -> (Arc<AppCore>, Scratch, PaneId, PaneId) {
        let dir = Scratch::new("hosts");
        let mut profiles = holzwave_core::profiles::Profiles::default();
        let profile = Profile {
            user: "holz".into(),
            key_path: "/nonexistent/holzwave-test-key".into(),
            ..Profile::new("web-1", "Web 1", "web1.example.invalid")
        };
        profiles.upsert(profile).unwrap();
        profiles.save(&dir.0).unwrap();
        let (core, _rec) = core_in(&dir, "sh", &[]);
        let local = only_pane(&core);
        let remote = core.split(true).focus;
        assert_ne!(remote, local, "the new pane has the focus now");
        core.connections.assign(remote, "web-1").unwrap();
        (core, dir, local, remote)
    }

    #[test]
    fn history_host_entries_follow_the_pane_that_sent_them() {
        use holzwave_ipc::Cmd;
        let (core, _dir, local, remote) = core_with_a_connection_pane();
        let hist = |line: &str| Cmd::Hist { line: line.into(), cwd: "/home/holz".into(), exit: Some(0) };
        assert_eq!(core.ipc_command(Some(remote), hist("kubectl get pods")), Ok(String::new()));
        assert_eq!(core.ipc_command(Some(local), hist("cargo build")), Ok(String::new()));
        assert_eq!(core.ipc_command(None, hist("git status")), Ok(String::new()));
        assert_eq!(core.ipc_command(Some(9999), hist("docker ps")), Ok(String::new()));
        let stored: HashMap<String, String> =
            core.history.search("", None, 10).into_iter().map(|h| (h.cmd, h.host)).collect();
        assert_eq!(stored.len(), 4);
        assert_eq!(stored["kubectl get pods"], "ssh:web-1");
        for line in ["cargo build", "git status", "docker ps"] {
            assert_eq!(stored[line], LOCAL_HOST, "{line}");
        }
    }

    #[test]
    fn history_host_search_follows_the_focused_pane() {
        use holzwave_ipc::Cmd;
        let (core, _dir, local, remote) = core_with_a_connection_pane();
        let hist = |line: &str| Cmd::Hist { line: line.into(), cwd: "/home/holz".into(), exit: Some(0) };
        core.ipc_command(Some(remote), hist("kubectl get pods")).unwrap();
        core.ipc_command(Some(local), hist("cargo build")).unwrap();
        core.ipc_command(Some(local), hist("git status")).unwrap();
        let found = |core: &AppCore| -> Vec<String> {
            let mut all: Vec<String> = core.history_search("", 10).into_iter().map(|h| h.cmd).collect();
            all.sort();
            all
        };
        assert_eq!(core.view().focus, remote);
        assert_eq!(found(&core), ["kubectl get pods"]);
        core.focus_pane(local);
        assert_eq!(found(&core), ["cargo build", "git status"]);
        assert!(core.history_search_in("nope", "", 10).is_empty(), "an unknown window finds nothing");
    }

    #[test]
    fn history_host_labels_name_the_kind_and_stay_bounded() {
        let long = "a".repeat(64);
        for (kind, word) in [(ConnectionKind::Ssh, "ssh"), (ConnectionKind::Wsl, "wsl"), (ConnectionKind::Docker, "docker")] {
            let short = host_label(&Profile { kind, ..Profile::new("web-1", "Web", "h") });
            assert_eq!(short, format!("{word}:web-1"));
            let label = host_label(&Profile { kind, ..Profile::new(long.clone(), "Web", "h") });
            assert_eq!(label, format!("{word}:{long}"));
            assert!(label.len() <= 71, "{label}");
            assert!(label.len() <= 128, "the history refuses a longer host");
            assert_ne!(label, LOCAL_HOST);
            assert!(!label.chars().any(char::is_control));
        }
    }

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
        // Every action has a default chord but the rename, swap, move, close-tab-or-workspace, integration and bar.focus actions and, off macOS, the line start and end ones.
        assert!(view.keybindings.iter().all(|row| row.chord.is_some() || row.action.contains("rename") || row.action.starts_with("pane.swap") || row.action.starts_with("pane.move") || row.action.starts_with("tab.move") || row.action == "tab.close" || row.action == "workspace.close" || row.action == "ai.new_chat" || row.action.starts_with("integration.") || row.action == "bar.focus" || row.action.starts_with("edit.line") || row.action == "edit.delete_to_line_start"));
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
        launch_of_on(spec, configured, os, env, files, false)
    }

    /// [`launch_of`] with the host shell decided: `on_host` is true only for a Flatpak whose Host shell is on and which
    /// has the permission.
    fn launch_of_on(
        spec: &ShellSpec,
        configured: &str,
        os: Os,
        env: &[(&str, &str)],
        files: &[&str],
        on_host: bool,
    ) -> (String, Vec<String>) {
        let (env, exists, check) = facts(env, files);
        let launch = spec.launch(configured, os, &env, &exists, &check, on_host).launch;
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
    fn in_a_flatpak_with_the_host_shell_on_the_system_shell_is_the_host_login_shell_and_the_setting_is_not_looked_up_in_the_sandbox() {
        use holzwave_core::platform::HOST_LOGIN_SHELL_SCRIPT;
        let system = ShellSpec::default();
        let flatpak = [("FLATPAK_ID", "ch.holzcloud.holzwave"), ("SHELL", "/bin/sh")];
        let on = |configured: &str, env: &[(&str, &str)], files: &[&str]| launch_of_on(&system, configured, Os::Linux, env, files, true);
        assert_eq!(
            on("", &flatpak, &[]),
            ("sh".to_owned(), vec!["-c".to_owned(), HOST_LOGIN_SHELL_SCRIPT.to_owned()]),
            "no setting: one constant script finds the host's login shell, $SHELL of the sandbox is not asked"
        );
        // The setting names a program of the host: the sandbox does not have it, and that is no defect.
        assert_eq!(on("/usr/bin/fish", &flatpak, &[]), ("/usr/bin/fish".to_owned(), vec!["-l".to_owned()]));
        let (env, exists, check) = facts(&flatpak, &[]);
        assert_eq!(system.launch("/usr/bin/fish", Os::Linux, &env, &exists, &check, true).unusable, None);
        // The info file alone is enough.
        assert_eq!(on("", &[], &["/.flatpak-info"]).0, "sh");
        // A fixed shell (what the tests inject) stays what it is.
        assert_eq!(launch_of_on(&ShellSpec::new("/bin/cat", &[]), "", Os::Linux, &flatpak, &[], true).0, "/bin/cat");
        // Not in a Flatpak, or another OS: the host shell is not asked for, whatever the flag says.
        assert_eq!(on("", &[("SHELL", "/usr/bin/fish")], &[]).0, "/usr/bin/fish");
        assert_eq!(launch_of_on(&system, "", Os::Macos, &flatpak, &[], true).0, "/bin/sh", "$SHELL of the environment, as on a Mac");
    }

    #[test]
    fn in_a_flatpak_with_the_host_shell_off_or_not_available_the_shell_is_resolved_in_the_sandbox() {
        let system = ShellSpec::default();
        let flatpak = [("FLATPAK_ID", "ch.holzcloud.holzwave"), ("SHELL", "/usr/bin/fish")];
        // As on any Linux: $SHELL of the sandbox with the login flag, never the script of the host.
        assert_eq!(launch_of(&system, "", Os::Linux, &flatpak, &[]), ("/usr/bin/fish".to_owned(), vec!["-l".to_owned()]));
        assert_eq!(launch_of(&system, "", Os::Linux, &[("FLATPAK_ID", "x")], &[]), ("/bin/sh".to_owned(), vec!["-l".to_owned()]), "no $SHELL");
        assert_eq!(launch_of(&system, "", Os::Linux, &[], &["/.flatpak-info"]).0, "/bin/sh", "the info file alone is a Flatpak too");
        // The `shell` setting is looked up in the sandbox now: a program that is there is used, one that is not is reported.
        assert_eq!(
            launch_of(&system, "/usr/bin/zsh", Os::Linux, &flatpak, &["/usr/bin/zsh"]),
            ("/usr/bin/zsh".to_owned(), vec!["-l".to_owned()])
        );
        assert!(unusable_of("/usr/bin/fish", Os::Linux, &flatpak, &[]).is_some(), "the sandbox does not have it, so it is reported");
        // A fixed shell stays what it is.
        assert_eq!(launch_of(&ShellSpec::new("/bin/cat", &[]), "", Os::Linux, &flatpak, &[]).0, "/bin/cat");
        // Nothing changes outside a Flatpak.
        assert_eq!(
            unusable_of("/usr/bin/fish", Os::Linux, &[("SHELL", "/bin/sh")], &[]).is_some(),
            true,
            "outside a Flatpak a setting that is not there is still reported"
        );
    }

    const FLATPAK_ENV: &[(&str, &str)] = &[("FLATPAK_ID", "ch.holzcloud.holzwave"), ("SHELL", "/usr/bin/fish")];

    /// A core that behaves as a Flatpak (or not) with or without the permission, whatever machine the test runs on.
    fn core_as(tag: &str, in_flatpak: bool, permission: bool) -> (Scratch, Arc<AppCore>) {
        let dir = Scratch::new(tag);
        let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());
        dir.attach(&core);
        core.host_shell().use_facts(in_flatpak, permission);
        (dir, core)
    }

    /// The spec of a new pane on `os` with the given environment and files.
    fn spec_on(core: &AppCore, os: Os, vars: &[(&str, &str)], files: &[&str]) -> PaneSpec {
        let (env, exists, _) = facts(vars, files);
        core.pane_spec_on(80, 24, (9, 18), os, &env, &exists)
    }

    #[test]
    fn in_a_flatpak_with_the_permission_and_the_switch_off_a_pane_starts_in_the_sandbox_and_says_so() {
        let (_dir, core) = core_as("hs-off", true, true);
        assert_eq!(core.host_shell().view().state, "off");
        let spec = spec_on(&core, Os::Linux, FLATPAK_ENV, &[]);
        assert!(!spec.host_spawn && spec.cwd_lookup, "the sandbox: nothing wrapped, the directory looked up from the process");
        assert_eq!((spec.program.as_str(), spec.args.as_slice()), ("/usr/bin/fish", ["-l".to_owned()].as_slice()), "as on any Linux");
        assert_eq!(spec.notice.as_deref(), Some(crate::host_shell::NOTICE_OFF));
        let by_file = spec_on(&core, Os::Linux, &[], &["/.flatpak-info"]);
        assert!(!by_file.host_spawn && by_file.cwd_lookup && by_file.notice.is_some(), "the info file alone is a Flatpak");
    }

    #[test]
    fn in_a_flatpak_with_the_permission_and_the_switch_on_a_pane_starts_on_the_host_without_a_notice() {
        use holzwave_core::platform::host_shell_launch;
        let (_dir, core) = core_as("hs-on", true, true);
        core.host_shell().set(true).expect("allowed with the permission");
        assert_eq!(core.host_shell().view().state, "on");

        let spec = spec_on(&core, Os::Linux, FLATPAK_ENV, &[]);
        let expected = host_shell_launch("");
        assert!(spec.host_spawn && !spec.cwd_lookup, "the host: wrapped, and the directory comes from OSC 7 only");
        assert_eq!((spec.program, spec.args), (expected.program, expected.args), "exactly the launch of before");
        assert!(spec.notice.is_none(), "{:?}", spec.notice);

        // The `shell` setting names a program of the host and is not looked up in the sandbox.
        core.set_setting("shell", json!("/usr/bin/fish")).expect("a string");
        let spec = spec_on(&core, Os::Linux, FLATPAK_ENV, &[]);
        let expected = host_shell_launch("/usr/bin/fish");
        assert_eq!((spec.program, spec.args), (expected.program, expected.args));
        assert!(spec.notice.is_none() && spec.host_spawn, "a program the sandbox cannot see is no defect on the host");
    }

    #[test]
    fn the_core_owns_the_window_frame_host_over_its_data_directory() {
        use holzwave_core::window_frame::{ButtonChoice, FrameChoice};
        let dir = Scratch::new("wf-core");
        std::fs::write(dir.0.join("window_frame.json"), "{\"version\": 1, \"window_frame\": \"system\", \"window_buttons\": \"round\"}\n")
            .unwrap();
        let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());
        dir.attach(&core);
        let host = core.window_frame();
        // The file is the started choice where the group applies, and is not looked at elsewhere.
        if matches!(Os::current(), Os::Windows | Os::Linux) {
            assert!(host.applies());
            assert_eq!(host.started(), (FrameChoice::System, ButtonChoice::Round));
            assert_eq!(host.view().frame, "system");
            assert!(!host.view().pending);
        } else {
            assert!(!host.applies());
            assert_eq!(host.started(), (FrameChoice::Auto, ButtonChoice::Auto));
            assert!(!host.view().applies);
        }
    }

    #[test]
    fn in_a_flatpak_without_the_permission_a_pane_starts_in_the_sandbox_whatever_the_file_says() {
        let dir = Scratch::new("hs-unavailable");
        // The person's file says on, from a time when the permission was there.
        std::fs::write(dir.0.join("host_shell.json"), "{\"version\": 1, \"host_shell\": true}\n").unwrap();
        let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());
        dir.attach(&core);
        core.host_shell().use_facts(true, false);
        assert!(core.host_shell().enabled(), "the choice is kept");
        assert_eq!(core.host_shell().view().state, "unavailable");

        let spec = spec_on(&core, Os::Linux, FLATPAK_ENV, &[]);
        assert!(!spec.host_spawn && spec.cwd_lookup, "the choice cannot grant the permission");
        assert_eq!(spec.program, "/usr/bin/fish");
        assert_eq!(spec.notice.as_deref(), Some(crate::host_shell::NOTICE_UNAVAILABLE));

        // And with the choice off, the same.
        std::fs::write(dir.0.join("host_shell.json"), "{\"version\": 1, \"host_shell\": false}\n").unwrap();
        core.host_shell().use_facts(true, false);
        let spec = spec_on(&core, Os::Linux, FLATPAK_ENV, &[]);
        assert!(!spec.host_spawn && spec.notice.as_deref() == Some(crate::host_shell::NOTICE_UNAVAILABLE));
    }

    #[test]
    fn outside_a_flatpak_the_spec_is_what_it_was_whatever_the_file_says() {
        let dir = Scratch::new("hs-outside");
        std::fs::write(dir.0.join("host_shell.json"), "{\"version\": 1, \"host_shell\": true}\n").unwrap();
        let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());
        dir.attach(&core);
        // The file says on and the permission is there: still no Flatpak, no sandbox.
        core.host_shell().use_facts(true, true);
        assert!(core.host_shell().enabled());
        let (_plain_dir, plain) = core_as("hs-outside-plain", false, false);
        for (os, vars, files) in [
            (Os::Linux, &[("SHELL", "/bin/sh")][..], &[][..]),
            (Os::Linux, &[("FLATPAK_ID", "")][..], &[][..]),
            (Os::Macos, &[("FLATPAK_ID", "x")][..], &["/.flatpak-info"][..]),
            (Os::Windows, &[("FLATPAK_ID", "x")][..], &[][..]),
        ] {
            let with_file = spec_on(&core, os, vars, files);
            let without = spec_on(&plain, os, vars, files);
            assert!(!with_file.host_spawn && with_file.cwd_lookup, "{os:?} {vars:?}: nothing is wrapped and the directory is looked up");
            assert!(with_file.notice.is_none(), "{os:?} {vars:?}: no sandbox sentence outside a Flatpak: {:?}", with_file.notice);
            assert_eq!(format!("{with_file:?}"), format!("{without:?}"), "{os:?} {vars:?}: byte for byte what it was");
        }
    }

    #[test]
    fn an_unusable_shell_setting_and_the_sandbox_give_one_notice_with_the_shell_sentence_first() {
        let (_dir, core) = core_as("hs-both", true, true);
        core.set_setting("shell", json!("/nonexistent/holzwave-no-shell")).expect("a string");
        let off = spec_on(&core, Os::Linux, FLATPAK_ENV, &[]).notice.expect("a notice");
        let unusable = off.strip_suffix(crate::host_shell::NOTICE_OFF).and_then(|rest| rest.strip_suffix(' ')).expect("the sandbox sentence ends it");
        assert!(unusable.starts_with("holzWave: ") && unusable.contains("holzwave-no-shell"), "{off:?}");
        assert!(!unusable.contains("sandbox"), "the shell sentence comes alone and first: {off:?}");
        assert!(!off.contains('\n') && !off.contains('\r'), "one line");

        core.host_shell().use_facts(true, false);
        let without = spec_on(&core, Os::Linux, FLATPAK_ENV, &[]).notice.expect("a notice");
        assert_eq!(without, format!("{unusable} {}", crate::host_shell::NOTICE_UNAVAILABLE));

        // On the host the setting is not looked up, so there is neither sentence.
        core.host_shell().use_facts(true, true);
        core.host_shell().set(true).expect("allowed");
        assert_eq!(spec_on(&core, Os::Linux, FLATPAK_ENV, &[]).notice, None);
    }

    #[test]
    fn a_saved_ssh_connection_is_no_sandbox_pane_and_is_not_wrapped_for_the_host() {
        let t = ticked(&[]);
        for (permission, on) in [(true, false), (true, true), (false, false)] {
            t.core.host_shell().use_facts(true, permission);
            if on {
                t.core.host_shell().set(true).expect("allowed");
            }
            let (env, exists, _) = facts(FLATPAK_ENV, &[]);
            let mut spec = t.core.pane_spec_on(80, 24, (9, 18), Os::Linux, &env, &exists);
            assert_eq!(spec.host_spawn, on, "a local pane before the connection is applied");
            assert_eq!(spec.notice.is_some(), !on, "the sandbox sentence belongs to a local shell");

            t.core.apply_connection(t.remote, &mut spec);

            assert!(spec.ssh_connection.is_some(), "permission {permission} on {on}: the connection was applied");
            assert!(!spec.host_spawn, "permission {permission} on {on}: an SSH pane starts no local shell");
            assert!(spec.notice.is_none(), "permission {permission} on {on}: {:?}", spec.notice);
        }
        // A local pane keeps its sentence: only a connection takes it away.
        t.core.host_shell().use_facts(true, true);
        t.core.host_shell().set(false).expect("allowed");
        let (env, exists, _) = facts(FLATPAK_ENV, &[]);
        let mut local = t.core.pane_spec_on(80, 24, (9, 18), Os::Linux, &env, &exists);
        t.core.apply_connection(t.local, &mut local);
        assert_eq!(local.notice.as_deref(), Some(crate::host_shell::NOTICE_OFF));
    }

    #[test]
    fn changing_the_switch_changes_no_pane_that_is_already_made_and_the_next_one_follows() {
        let (_dir, core) = core_as("hs-next", true, true);
        let before = spec_on(&core, Os::Linux, FLATPAK_ENV, &[]);
        core.host_shell().set(true).expect("allowed");
        let after = spec_on(&core, Os::Linux, FLATPAK_ENV, &[]);
        core.host_shell().set(false).expect("allowed");
        let back = spec_on(&core, Os::Linux, FLATPAK_ENV, &[]);
        assert!(!before.host_spawn && after.host_spawn && !back.host_spawn);
        assert_eq!(before.program, "/usr/bin/fish", "a spec made earlier keeps its shell");
        assert_eq!(format!("{before:?}"), format!("{back:?}"));
        assert!(before.notice.is_some() && after.notice.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn image_animation_the_spec_of_a_new_pane_follows_the_switch_of_the_core() {
        let dir = Scratch::new("animation-spec");
        let core = AppCore::new(dir.0.clone(), Arc::new(Recorder::default()), ShellSpec::default());
        dir.attach(&core);
        assert!(core.pane_spec(80, 24, (9, 18)).image_animation, "pictures may move until the page says otherwise");

        core.set_image_animation(false);
        let paused = core.pane_spec(80, 24, (9, 18));
        core.set_image_animation(true);
        let playing = core.pane_spec(80, 24, (9, 18));

        assert!(!paused.image_animation, "a pane started while reduced motion is on starts paused");
        assert!(playing.image_animation);
    }

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
        ShellSpec::default().launch(configured, os, &env, &exists, &check, false).unusable
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
        assert!(ShellSpec::new("/bin/cat", &[]).launch("/nonexistent", Os::Linux, &|_| None, &|_| false, &|_| ProgramCheck::Missing, false).unusable.is_none());
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
            ShellSpec::default().launch(path.to_str().unwrap(), Os::Linux, &env, &exists, &check_program_on_disk, false).unusable
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

    // ---- what the sync downloaded and the app does not own is applied at once ---------------------------------------

    /// A download as the sync leaves it: `bytes` staged under `name` in `sync-incoming/`.
    fn staged(dir: &Scratch, name: &str, bytes: &[u8]) {
        let path = dir.0.join("sync-incoming").join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("staging dir");
        std::fs::write(path, bytes).expect("staged file");
    }

    #[test]
    fn adopting_a_layout_forgets_the_working_directories_of_this_device() {
        let dir = Scratch::new("adopt");
        // A first start makes a layout; remember a directory for its pane and a frame for the window.
        let (first, _rec) = core_in(&dir, "/bin/cat", &[]);
        first.new_tab();
        let pane = only_pane(&first);
        first.flush();
        first.shutdown();
        drop(first);
        let layout: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.layout_file()).expect("layout")).expect("json");
        let frame = WindowState { x: 10, y: 20, width: 900, height: 700, maximized: false };
        let remember = || {
            let mut session = Session::default();
            assert!(session.set_cwd(pane, "/tmp"));
            session.set_window(frame);
            session.save(&dir.0).expect("session file");
        };

        // Nothing staged: the directory and the frame are kept.
        remember();
        let (kept, _rec) = core_in(&dir, "/bin/cat", &[]);
        assert_eq!(lock(&kept.session).cwds.get(&pane).map(String::as_str), Some("/tmp"));
        kept.shutdown();
        drop(kept);

        // A layout unit from another device is applied at the start: no directory survives, the frame does.
        remember();
        staged(&dir, "layout.json", serde_json::json!({"unit": 1, "layout": layout, "connections": {}}).to_string().as_bytes());
        let (adopted, _rec) = core_in(&dir, "/bin/cat", &[]);
        {
            let session = lock(&adopted.session);
            assert!(session.cwds.is_empty(), "{:?}", session.cwds);
            assert_eq!(session.window, Some(frame), "the window frame is kept");
        }
        assert!(!dir.0.join("sync-incoming").exists(), "the unit was applied");
        // And it is on disk already, so a start that ends before the next save cannot bring them back.
        let (on_disk, _status) = Session::load(&dir.0);
        assert!(on_disk.cwds.is_empty() && on_disk.window == Some(frame));
    }

    fn theme_json(name: &str) -> String {
        serde_json::to_string(&Theme { name: name.into(), ..Theme::papier() }).expect("theme json")
    }

    #[test]
    fn a_start_after_a_download_forgets_the_secrets_of_removed_profiles() {
        use holzwave_core::secrets::MemoryStore;
        let dir = Scratch::new("forgets-secrets");
        let list = |ids: &[&str]| {
            let items: Vec<String> = ids
                .iter()
                .map(|id| format!(r#"{{"id":"{id}","name":"{id}","host":"h.example","user":"u","auth":"password"}}"#))
                .collect();
            format!(r#"{{"version":1,"items":[{}]}}"#, items.join(","))
        };
        // This computer knows `web` and `db` and holds a secret for each; a download without `web` waits for the start.
        std::fs::write(dir.0.join("connections.json"), list(&["web", "db"])).unwrap();
        staged(&dir, "connections.json", list(&["db"]).as_bytes());
        let secrets = Arc::new(MemoryStore::default());
        secrets.set("ssh:web", "web-secret").unwrap();
        secrets.set("ssh:db", "db-secret").unwrap();
        secrets.set("ai:key", "ai-secret").unwrap();

        let promoted = holzwave_core::sync::engine::promote_incoming_with_report(&dir.0);
        assert_eq!(promoted.dropped_profiles, ["web"]);
        let (connections, _) = load_connections(&dir.0, secrets.clone(), user_home(), &[], &promoted.dropped_profiles);

        assert_eq!(secrets.get("ssh:web").unwrap(), None, "the removed profile's secret is gone");
        assert_eq!(secrets.get("ssh:db").unwrap().as_deref(), Some("db-secret"), "the other profile's secret stays");
        assert_eq!(secrets.get("ai:key").unwrap().as_deref(), Some("ai-secret"), "no other entry is touched");
        assert_eq!(connections.list().len(), 1);
    }

    #[test]
    fn a_downloaded_theme_is_applied_at_once_and_announced() {
        let (core, rec, dir) = core("/bin/cat", &[]);
        staged(&dir, "themes/mine.json", theme_json("mine").as_bytes());
        staged(&dir, "settings.json", br#"{"font_size": 31}"#);

        let promoted = core.apply_synced();

        assert_eq!(promoted.applied, ["themes/mine.json"]);
        assert!(dir.0.join("themes").join("mine.json").is_file(), "the theme is live");
        let events = rec.settings_events();
        assert_eq!(events.len(), 1, "one announcement");
        assert!(events[0].themes.iter().any(|t| t.name == "mine" && !t.builtin), "the theme list names it");
        assert!(core.get_settings().themes.iter().any(|t| t.name == "mine"));
        assert!(dir.0.join("sync-incoming").join("settings.json").is_file(), "settings.json still waits for a start");
        assert!(!dir.0.join("settings.json").exists());
        assert_ne!(core.get_settings().settings.font_size, 31, "the settings in force are not touched");
    }

    #[test]
    fn an_apply_after_a_run_reloads_the_themes_even_when_none_was_downloaded() {
        let (core, rec, dir) = core("/bin/cat", &[]);
        staged(&dir, "themes/mine.json", theme_json("mine").as_bytes());
        core.apply_synced();
        assert!(core.get_settings().themes.iter().any(|t| t.name == "mine"));
        let before = rec.settings_events().len();

        // A run that changed nothing announces nothing.
        core.apply_synced();
        assert_eq!(rec.settings_events().len(), before, "nothing changed, nothing announced");

        // A deletion by the sync removed the live file: nothing is staged, and the list still follows, once.
        std::fs::remove_file(dir.0.join("themes").join("mine.json")).unwrap();
        let promoted = core.apply_synced();
        assert!(promoted.applied.is_empty());
        let events = rec.settings_events();
        assert_eq!(events.len(), before + 1, "announced once");
        assert!(events.last().unwrap().themes.iter().all(|t| t.name != "mine"), "the theme list no longer holds it");
        assert!(core.get_settings().themes.iter().all(|t| t.name != "mine"));
    }

    #[test]
    fn a_downloaded_theme_that_is_not_one_is_left_out_and_the_rest_still_loads() {
        let (core, rec, dir) = core("/bin/cat", &[]);
        staged(&dir, "themes/bad.json", b"{ this is not a theme");
        staged(&dir, "themes/good.json", theme_json("good").as_bytes());

        core.apply_synced();

        let view = core.get_settings();
        assert!(view.themes.iter().any(|t| t.name == "good"), "{:?}", view.themes.iter().map(|t| &t.name).collect::<Vec<_>>());
        assert_eq!(view.themes.iter().filter(|t| t.builtin).count(), 3, "the built-in themes survive");
        assert!(view.warnings.iter().any(|w| w.contains("bad.json")), "{:?}", view.warnings);
        assert_eq!(rec.settings_events().len(), 1);
    }

    #[test]
    #[cfg(unix)]
    fn a_selected_theme_that_a_download_replaced_is_the_new_one_in_force() {
        let (core, _rec, dir) = core("/bin/cat", &[]);
        let first = Theme { name: "Mine".into(), ..Theme::papier() };
        let mut second = Theme { name: "Mine".into(), ..Theme::papier() };
        second.terminal.background = Rgb::new(first.terminal.background.r ^ 1, first.terminal.background.g, first.terminal.background.b);
        std::fs::create_dir_all(dir.0.join("themes")).unwrap();
        std::fs::write(dir.0.join("themes").join("mine.json"), serde_json::to_string(&first).unwrap()).unwrap();
        core.reload_settings();
        core.set_setting("theme", json!("Mine")).expect("select");
        assert_eq!(core.get_settings().theme.terminal.background, first.terminal.background);

        staged(&dir, "themes/mine.json", serde_json::to_string(&second).unwrap().as_bytes());
        core.apply_synced();

        assert_eq!(core.get_settings().theme.terminal.background, second.terminal.background);
    }

    #[test]
    fn settings_layout_and_connections_always_wait() {
        let (core, rec, dir) = core("/bin/cat", &[]);
        let before = std::fs::read(dir.layout_file()).ok();
        for name in ["layout.json", "settings.json", "connections.json"] {
            staged(&dir, name, b"{\"from\":\"another device\"}");
        }
        for _ in 0..5 {
            let promoted = core.apply_synced();
            assert!(promoted.applied.is_empty() && promoted.skipped.is_empty(), "{promoted:?}");
        }
        for name in ["layout.json", "settings.json", "connections.json"] {
            let waiting = dir.0.join("sync-incoming").join(name);
            assert_eq!(std::fs::read(waiting).unwrap(), b"{\"from\":\"another device\"}", "{name} stays staged");
            let live = std::fs::read(dir.0.join(name)).ok();
            assert!(live.as_deref() != Some(&b"{\"from\":\"another device\"}"[..]), "{name} is not applied while the app runs");
        }
        assert_eq!(std::fs::read(dir.layout_file()).ok(), before);
        assert!(rec.settings_events().is_empty(), "nothing was applied, nothing is announced");
    }

    #[test]
    fn a_downloaded_note_is_on_the_sidebar_at_once() {
        let (core, _rec, dir) = core("/bin/cat", &[]);
        core.widget_note_save("todo", "mine").expect("save");
        staged(&dir, "widget_notes.json", br#"{"todo":"from the other computer","extra":"second"}"#);
        staged(&dir, "widgets/todo.json", br#"{"id":"todo","title":"Todo","kind":"note"}"#);

        let promoted = core.apply_synced();

        assert_eq!(promoted.applied, ["widget_notes.json", "widgets/todo.json"]);
        let view = core.widgets_list();
        assert_eq!(view.notes.get("todo").map(String::as_str), Some("from the other computer"));
        assert_eq!(view.notes.get("extra").map(String::as_str), Some("second"));
        assert!(view.widgets.iter().any(|w| w.id == "todo"), "the widget is listed without a restart");
    }

    #[test]
    fn a_hand_edited_widget_file_is_kept_before_a_download_replaces_it() {
        let (core, _rec, dir) = core("/bin/cat", &[]);
        std::fs::create_dir_all(dir.0.join("widgets")).unwrap();
        std::fs::write(dir.0.join("widgets").join("deploy.json"), "my edit").unwrap();
        staged(&dir, "widgets/deploy.json", br#"{"id":"deploy","title":"Deploy","kind":"note"}"#);

        let promoted = core.apply_synced();

        assert_eq!(promoted.applied, ["widgets/deploy.json"]);
        assert_eq!(promoted.kept, ["widgets/deploy.json"]);
        let copies: Vec<_> = std::fs::read_dir(dir.0.join("sync-conflicts")).expect("copies").flatten().collect();
        assert_eq!(copies.len(), 1);
        assert!(copies[0].file_name().to_string_lossy().starts_with("widgets.deploy.json."));
        assert_eq!(std::fs::read_to_string(copies[0].path()).unwrap(), "my edit");
        assert!(std::fs::read_to_string(dir.0.join("widgets").join("deploy.json")).unwrap().contains("Deploy"));
    }

    #[test]
    fn the_apply_and_a_note_save_take_the_same_lock() {
        let (core, _rec, dir) = core("/bin/cat", &[]);
        staged(&dir, "widget_notes.json", br#"{"a":"staged"}"#);
        let held = lock(&core.notes_io);
        let (done_tx, done_rx) = std::sync::mpsc::channel::<&'static str>();
        let apply = {
            let (core, tx) = (core.clone(), done_tx.clone());
            std::thread::spawn(move || {
                core.apply_synced();
                let _ = tx.send("apply");
            })
        };
        let save = {
            let (core, tx) = (core.clone(), done_tx);
            std::thread::spawn(move || {
                core.widget_note_save("b", "typed").expect("save");
                let _ = tx.send("save");
            })
        };
        assert!(done_rx.recv_timeout(Duration::from_millis(300)).is_err(), "both wait for the lock");
        drop(held);
        let mut finished = vec![done_rx.recv_timeout(Duration::from_secs(20)).expect("first"), done_rx.recv_timeout(Duration::from_secs(20)).expect("second")];
        finished.sort_unstable();
        assert_eq!(finished, ["apply", "save"]);
        apply.join().unwrap();
        save.join().unwrap();
    }

    #[test]
    fn a_note_save_and_an_apply_never_interleave() {
        let (core, _rec, dir) = core("/bin/cat", &[]);
        let notes = dir.0.join("widget_notes.json");
        let read_valid = |notes: &std::path::Path| {
            if let Ok(bytes) = std::fs::read(notes) {
                let map: std::collections::BTreeMap<String, String> =
                    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("a torn notes file: {e}: {:?}", String::from_utf8_lossy(&bytes)));
                map
            } else {
                Default::default()
            }
        };
        let saver = {
            let (core, notes) = (core.clone(), notes.clone());
            std::thread::spawn(move || {
                for i in 0..100 {
                    core.widget_note_save("typed", &format!("typed {i}")).expect("save");
                    let map = read_valid(&notes);
                    assert!(map.is_empty() || map.contains_key("typed") || map.contains_key("staged"));
                }
            })
        };
        let applier = {
            let (core, dir_path, notes) = (core.clone(), dir.0.clone(), notes.clone());
            std::thread::spawn(move || {
                for i in 0..100 {
                    let path = dir_path.join("sync-incoming").join("widget_notes.json");
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    holzwave_core::persist::write_atomic(&path, format!(r#"{{"staged":"staged {i}"}}"#).as_bytes()).unwrap();
                    core.apply_synced();
                    read_valid(&notes);
                }
            })
        };
        // Neither thread may wait forever: a deadlock shows as a timeout here, not as a hung run.
        let deadline = Instant::now() + Duration::from_secs(20);
        while !(saver.is_finished() && applier.is_finished()) {
            assert!(Instant::now() < deadline, "the note save and the apply did not finish: a deadlock");
            std::thread::sleep(Duration::from_millis(5));
        }
        saver.join().expect("saver");
        applier.join().expect("applier");
        let map = read_valid(&notes);
        for (id, text) in &map {
            let ok = (id == "typed" && text.starts_with("typed ")) || (id == "staged" && text.starts_with("staged "));
            assert!(ok, "{id}: {text}");
        }
        assert!(!map.is_empty());
    }

    /// A sink that notes whether the notes lock was free when `settings` was announced.
    struct NotesProbe {
        inner: Arc<Recorder>,
        core: Mutex<Weak<AppCore>>,
        free: Mutex<Vec<bool>>,
    }

    impl Sink for NotesProbe {
        fn send(&self, target: Target, out: Out) {
            if matches!(out, Out::Settings(_)) {
                let free = lock(&self.core).upgrade().is_none_or(|core| core.notes_io.try_lock().is_ok());
                lock(&self.free).push(free);
            }
            self.inner.send(target, out);
        }

        fn close_window(&self, label: &str) {
            self.inner.close_window(label);
        }

        fn open_window(&self, label: &str, place: &Place) -> Result<(), String> {
            self.inner.open_window(label, place)
        }

        fn cursor_spot(&self, source: &str) -> CursorSpot {
            self.inner.cursor_spot(source)
        }
    }

    #[test]
    fn the_announcement_of_an_applied_theme_is_made_after_the_notes_lock_is_released() {
        let dir = Scratch::new("probe");
        let probe = Arc::new(NotesProbe { inner: Arc::new(Recorder::default()), core: Mutex::new(Weak::new()), free: Mutex::new(Vec::new()) });
        let core = AppCore::new(dir.0.clone(), probe.clone(), ShellSpec::new("/bin/cat", &[]));
        dir.attach(&core);
        *lock(&probe.core) = Arc::downgrade(&core);
        staged(&dir, "themes/mine.json", theme_json("mine").as_bytes());
        staged(&dir, "widget_notes.json", b"{}");

        core.apply_synced();

        assert_eq!(*lock(&probe.free), [true], "one announcement, with the notes lock free");
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
            assert_eq!(core.open_url(url, None), Ok(()), "{url}");
            assert_eq!(launches_with(url).len(), 1, "{url} is opened exactly once");
        }

        let long_ok = format!("https://example.org/{}", "a".repeat(MAX_URL_BYTES - "https://example.org/".len()));
        assert_eq!(long_ok.len(), MAX_URL_BYTES);
        assert_eq!(core.open_url(&long_ok, None), Ok(()), "2048 bytes are fine");

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
            assert!(core.open_url(url, None).is_err(), "{:?} must be refused", url.chars().take(40).collect::<String>());
            assert!(launches_with(url).is_empty(), "an opener was started for {:?}", url.chars().take(40).collect::<String>());
        }
    }

    /// Every character that a link must never carry, one example or the whole range each: the controls, the blanks,
    /// the soft hyphen, the Arabic letter mark, the zero-width and direction characters, the line and paragraph
    /// separators, the embedding and isolate controls, the byte order mark, the annotation marks and the tag characters.
    fn invisible_chars() -> Vec<char> {
        let mut all = vec!['\u{7}', '\u{7f}', '\u{9b}', ' ', '\t', '\u{a0}', '\u{3000}', '\u{ad}', '\u{61c}', '\u{feff}', '\u{e0001}', '\u{e0020}'];
        for range in [0x200B..=0x200F, 0x2028..=0x202E, 0x2060..=0x206F, 0xFFF9..=0xFFFB] {
            all.extend(range.map(|c| char::from_u32(c).expect("a scalar value")));
        }
        all
    }

    #[test]
    fn open_url_refuses_a_link_with_a_control_blank_or_invisible_character_in_every_position_and_for_every_scheme() {
        let (core, _rec, dir) = recording_core();
        set_schemes(&core, &["http", "https", "mailto", "ssh", "file"]);
        let base = real_dir(&dir, "hidden-files");
        let file = base.join("hid-file.txt");
        std::fs::write(&file, "x").unwrap();
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");

        let links = [
            "https://example.org/a-hid-1",
            "http://localhost:8080/b-hid-1",
            "mailto:holz@example.org?subject=hid-1",
            "ssh://host.example/c-hid-1",
            "ftp://example.org/not-listed-hid-1",
            "javascript:alert(1)//hid-1",
        ];
        for c in invisible_chars() {
            for link in links {
                let (scheme, tail) = link.split_once(':').unwrap();
                let variants = [format!("{c}{link}"), format!("{scheme}:{c}{tail}"), format!("{link}{c}")];
                for url in variants {
                    let err = core.open_url(&url, None).expect_err("refused");
                    assert!(err.contains("invisible"), "{:?} {c:?}: the sentence says what is wrong: {err}", &url);
                    assert!(!err.contains("example") && !err.contains("hid-") && err.len() < 160, "{err}");
                    assert!(launches_with(&url).is_empty(), "an opener was started for {url:?}");
                }
            }
            // A file link is refused before its path is looked at, with a pane that would show the file.
            let good = file_url(&file);
            for url in [format!("{c}{good}"), format!("{good}{c}"), format!("{}{c}{}", &good[..good.len() - 4], &good[good.len() - 4..])] {
                let err = core.open_url(&url, Some(pane)).expect_err("a file link with a hidden character");
                assert!(err.contains("invisible"), "{c:?}: {err}");
                assert!(!err.contains("hid-file"), "{err}");
            }
        }
        assert!(launches_with(&text_of(&file)).is_empty() && launches_with(&text_of(&base)).is_empty(), "no file was revealed");

        // The controls: an escaped override is plain ASCII, and a host with ordinary non-ASCII letters is no hidden character.
        for url in ["https://example.org/a%E2%80%AEb-hid-2", "https://ex\u{e4}mple.org/path-hid-3", "https://\u{65e5}\u{672c}.example/hid-4", "https://example.org/\u{1f600}-hid-5"] {
            assert_eq!(core.open_url(url, None), Ok(()), "{url}");
            assert_eq!(launches_with(url).len(), 1, "{url} is opened exactly once");
        }
    }

    /// The list of schemes that a new settings file has.
    fn web() -> Vec<String> {
        holzwave_core::settings::Settings::default().link_schemes
    }

    fn schemes(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn set_schemes(core: &AppCore, list: &[&str]) {
        core.set_setting("link_schemes", serde_json::json!(list)).expect("the list is accepted");
    }

    #[test]
    fn a_listed_custom_scheme_opens_through_the_opener_with_the_link_as_the_only_argument() {
        let (core, _rec, _dir) = recording_core();
        set_schemes(&core, &["https", "ssh"]);

        let ssh = "ssh://host.example/path-lk-1";
        assert_eq!(core.open_url(ssh, None), Ok(()));
        let launches = launches_with(ssh);
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].args.iter().filter(|a| a.contains("path-lk-1")).count(), 1);
        if OS != "windows" {
            assert_eq!(launches[0].args, [ssh], "the link is the only argument");
        }

        let https = "https://example.org/lk-2";
        assert_eq!(core.open_url(https, None), Ok(()));
        assert_eq!(launches_with(https).len(), 1, "a scheme that stays in the list still opens");

        let mailto = "mailto:a@b.example?subject=lk-3";
        assert!(core.open_url(mailto, None).is_err(), "mailto left the list");
        assert!(launches_with(mailto).is_empty());

        let upper = "HTTPS://EXAMPLE.ORG/lk-4";
        assert_eq!(core.open_url(upper, None), Ok(()), "the comparison ignores case");
        assert_eq!(launches_with(upper).len(), 1);

        let js = "javascript:alert(1)//lk-5";
        assert!(core.open_url(js, None).is_err());
        assert!(launches_with(js).is_empty());
    }

    #[test]
    fn open_url_refuses_a_one_letter_scheme_whatever_the_list_holds() {
        let (core, _rec, _dir) = recording_core();
        // The sanitiser drops the entry; the command does not depend on that.
        set_schemes(&core, &["https", "c", "ssh", "x", "ab"]);
        assert_eq!(core.get_settings().settings.link_schemes, ["https", "ssh", "ab"]);
        for url in [r"c:\Windows\System32\calc.exe-one-1", "C:/x.exe-one-2", "x:y-one-3", "d:\\server\\share\\a.exe-one-4", "z:-one-5"] {
            let err = core.open_url(url, None).expect_err(url);
            assert!(!err.contains("one-") && !err.contains("calc") && err.len() < 160, "{err}");
            assert!(launches_with(url).is_empty(), "an opener was started for {url}");
        }
        // A scheme of two characters that is listed opens as before.
        assert_eq!(core.open_url("ab:thing-one-6", None), Ok(()));
        assert_eq!(launches_with("ab:thing-one-6").len(), 1);
    }

    #[test]
    fn check_link_checks_the_length_of_the_scheme_itself_for_a_list_that_never_went_through_the_sanitiser() {
        let hostile = schemes(&["c", "x", "ssh"]);
        for url in [r"c:\x.exe", "C:\\x.exe", "c:/x.exe", "x:y"] {
            let err = check_link(url, &hostile).expect_err(url);
            assert!(!err.contains("x.exe") && err.len() < 160, "{err}");
        }
        assert_eq!(check_link("ssh://host.example/p", &hostile), Ok(()));
        // The refusal of the scheme comes with its own words, not the ones of the list or the shape.
        let one = check_link("c:/x", &hostile).unwrap_err();
        assert_ne!(one, check_link("ftp://h/", &hostile).unwrap_err());
        assert_ne!(one, check_link("ssh:", &hostile).unwrap_err());
    }

    #[test]
    fn open_url_refuses_a_listed_scheme_whose_host_or_first_part_starts_like_an_option_and_opens_the_others() {
        let (core, _rec, _dir) = recording_core();
        set_schemes(&core, &["http", "https", "mailto", "ssh", "x-man-page"]);
        let refused = [
            "ssh://-oProxyCommand=touch%20x@host.example/opt-1",
            "ssh:-oProxyCommand=x-opt-2",
            "ssh://%2DoProxyCommand=x/opt-3",
            "ssh://%2doProxyCommand=x/opt-4",
            "ssh://user@-host.example/opt-5",
            "ssh://user@%2dhost.example/opt-6",
            "ssh://user@%2Dhost.example/opt-7",
            "ssh://user:pw@-host.example/opt-8",
            "ssh://a@b@-host.example/opt-9",
            "ssh://-/opt-10",
            "ssh:-x-opt-11",
            "SSH://-oProxyCommand=x/opt-12",
            "x-man-page://-k/opt-13",
            "x-man-page:%2dk-opt-14",
        ];
        for url in refused {
            let err = core.open_url(url, None).expect_err(url);
            assert!(err.contains("dash") && !err.contains("opt-") && !err.contains("Proxy") && err.len() < 160, "{err}");
            assert!(launches_with(url).is_empty(), "an opener was started for {url}");
        }
        // Controls: a hyphen anywhere else is no option, and the web schemes and mailto are not touched by the rule.
        let opened = [
            "ssh://host.example/path-opt-20",
            "ssh://my-host.example/opt-21",
            "ssh://user-name@host.example:22/opt-22",
            "ssh://host.example/-x-opt-23",
            "ssh://host.example/?a=-b-opt-24",
            "ssh://host.example#-x-opt-25",
            "ssh:host.example-opt-26",
            "https://-x.example/opt-27",
            "http://user@-x.example/opt-28",
            "mailto:-x@example.org?subject=opt-29",
        ];
        for url in opened {
            assert_eq!(core.open_url(url, None), Ok(()), "{url}");
            let launches = launches_with(url);
            assert_eq!(launches.len(), 1, "{url} is opened exactly once");
            if OS != "windows" {
                assert_eq!(launches[0].args, [url], "the link is the only argument");
            }
        }
    }

    #[test]
    fn an_empty_list_opens_no_link_and_the_default_comes_back_with_a_reset() {
        let (core, _rec, _dir) = recording_core();
        set_schemes(&core, &[]);
        for url in ["https://example.org/lk-6", "http://example.org/lk-7", "mailto:a@b.example?lk=8"] {
            assert!(core.open_url(url, None).is_err(), "{url}");
            assert!(launches_with(url).is_empty(), "{url}");
        }
        // A change applies to the next call, without a restart, and a reset brings the default back.
        core.reset_setting("link_schemes").expect("reset");
        assert_eq!(core.get_settings().settings.link_schemes, web());
        let url = "https://example.org/lk-9";
        assert_eq!(core.open_url(url, None), Ok(()));
        assert_eq!(launches_with(url).len(), 1);
    }

    #[test]
    fn a_list_in_the_file_is_in_force_and_a_hand_edit_cannot_enable_a_denied_scheme() {
        let dir = Scratch::new("links-file");
        std::fs::write(
            dir.0.join("settings.json"),
            r#"{"link_schemes": ["JavaScript", "data", "ssh", "https", "vbscript", "blob", "about"]}"#,
        )
        .unwrap();
        let (core, _rec) = core_in(&dir, "/bin/cat", &[]);
        *lock(&core.launcher) = record_launch;

        assert_eq!(core.get_settings().settings.link_schemes, ["ssh", "https"]);
        for url in ["javascript:alert(1)//lk-10", "JavaScript:alert(1)//lk-11", "data:text/html,lk-12", "vbscript:x-lk-13", "blob:https://a/lk-14", "about:blank#lk-15"] {
            assert!(core.open_url(url, None).is_err(), "{url}");
            assert!(launches_with(url).is_empty(), "{url}");
        }
        assert_eq!(core.open_url("ssh://h.example/lk-16", None), Ok(()));
        // Defence in depth: even a list that holds a denied scheme (it cannot come through the settings) opens none.
        let hostile = schemes(&["javascript", "data", "vbscript", "blob", "about", "JavaScript"]);
        for url in ["javascript:alert(1)", "JAVASCRIPT:alert(1)", "data:text/html,x", "vbscript:x", "blob:https://a/b", "about:blank"] {
            assert!(check_link(url, &hostile).is_err(), "{url}");
        }
    }

    #[test]
    fn the_shape_of_a_link_is_checked_per_scheme_and_a_refusal_never_echoes_the_link() {
        let list = schemes(&["http", "https", "mailto", "ssh", "x-man-page"]);
        for ok in ["http://h", "https://example.org/a", "mailto:a@b", "ssh://h/p", "ssh:x", "x-man-page://ls", "SSH:x", "Https://h"] {
            assert_eq!(check_link(ok, &list), Ok(()), "{ok}");
        }
        for bad in ["http:", "http:x", "http://", "http:///x", "https:/h", "mailto:", "ssh:", "x-man-page:", "nocolon", ":x", "1ssh:x", "ss_h:x", "file:///tmp/a", "ftp://h/secret-lk-20"] {
            let err = check_link(bad, &list).expect_err(bad);
            assert!(!err.contains("secret") && err.len() < 200, "{err}");
        }
        for bad in ["https://h/secret-lk-21 x", "https://h/\u{7}secret-lk-22", &format!("https://h/{}", "s".repeat(MAX_URL_BYTES))] {
            let err = check_link(bad, &list).expect_err("refused");
            assert!(!err.contains("secret") && err.len() < 200, "{err}");
        }
        assert!(check_link("file:///tmp/a", &schemes(&["file"])).is_err(), "file is revealed, never handed to the opener");
        let two_thousand = format!("https://h/{}", "a".repeat(MAX_URL_BYTES - 10));
        assert_eq!(two_thousand.len(), MAX_URL_BYTES);
        assert_eq!(check_link(&two_thousand, &list), Ok(()));
        assert!(check_link(&format!("{two_thousand}a"), &list).is_err(), "2049 bytes");
    }

    // ---- file links reveal, they never open ----

    #[test]
    fn file_link_path_decodes_once_and_accepts_the_forms_of_this_machine() {
        let ok = |os: &str, url: &str| file_link_path(os, url);
        assert_eq!(ok("macos", "file:///Users/x/a%20b.txt").as_deref(), Ok("/Users/x/a b.txt"));
        assert_eq!(ok("linux", "file://localhost/tmp/a").as_deref(), Ok("/tmp/a"));
        assert_eq!(ok("linux", "FILE://LocalHost/tmp/a").as_deref(), Ok("/tmp/a"), "scheme and host are not case sensitive");
        assert_eq!(ok("linux", "file:///").as_deref(), Ok("/"), "the root is a path");
        assert_eq!(ok("linux", "file:///tmp/dir/").as_deref(), Ok("/tmp/dir"), "a folder may end in a slash");
        assert_eq!(ok("linux", "file:///tmp/%C3%A4%25.txt").as_deref(), Ok("/tmp/\u{e4}%.txt"));
        assert_eq!(ok("linux", "file:///tmp/%2541").as_deref(), Ok("/tmp/%41"), "decoded once, never twice");
        assert_eq!(ok("linux", "file:///tmp/./a").as_deref(), Ok("/tmp/./a"), "a single dot goes nowhere");
        assert_eq!(ok("windows", "file:///C:/Users/x/a.txt").as_deref(), Ok("C:/Users/x/a.txt"));
        assert_eq!(ok("windows", "file://localhost/d:/x%20y/z").as_deref(), Ok("d:/x y/z"));
        assert_eq!(ok("windows", "file:///C:/").as_deref(), Ok("C:/"));
    }

    #[test]
    fn file_link_path_refuses_every_form_that_could_leave_the_path_or_name_another_machine() {
        // Every link holds `secret-flp` so that a refusal that echoes it is found.
        let cases: [(&str, &str); 36] = [
            ("linux", "file://server/secret-flp/a"),
            ("linux", "file://127.0.0.1/secret-flp/a"),
            ("linux", "file://"),
            ("linux", "file://localhost"),
            ("linux", "file:secret-flp"),
            ("linux", "file:secret-flp/b"),
            ("linux", "file:/secret-flp/b"),
            ("linux", "file:///secret-flp/../b"),
            ("linux", "file:///secret-flp/%2e%2e/b"),
            ("linux", "file:///secret-flp/%2E%2e"),
            ("linux", "file:///secret-flp/.."),
            ("linux", "file:///secret-flp/%00"),
            ("linux", "file:///secret-flp/a%00b"),
            ("linux", "file:///secret-flp/%0a"),
            ("linux", "file:///secret-flp/%1b%5b"),
            ("linux", "file:///secret-flp/%7f"),
            ("linux", "file:///secret-flp/%c2%85"),
            ("linux", "file:///secret-flp/a%2fb"),
            ("linux", "file:///secret-flp/a%2Fb"),
            ("linux", "file:///secret-flp/a%5cb"),
            ("linux", "file:///secret-flp/a%5Cb"),
            ("linux", "file:///secret-flp/a\\b"),
            ("linux", "file:///secret-flp/%ff"),
            ("linux", "file:///secret-flp/%c3"),
            ("linux", "file:///secret-flp/%zz"),
            ("linux", "file:///secret-flp/%4"),
            ("linux", "file:///secret-flp/%"),
            ("linux", "file:///secret-flp//b"),
            ("linux", "file:///secret-flp/a?b"),
            ("linux", "file:///secret-flp/a#b"),
            ("linux", "file:///C:/secret-flp/a"),
            ("macos", "file:///c:/secret-flp"),
            ("windows", "file://///server/secret-flp/share"),
            ("windows", "file:///secret-flp/no-drive"),
            ("windows", "file:///C:/secret-flp/a:b"),
            ("windows", "file:///C:/secret-flp/..."),
        ];
        let mut texts = std::collections::BTreeSet::new();
        for (os, url) in cases {
            let err = file_link_path(os, url).expect_err(url);
            assert!(!err.contains("secret-flp") && !err.contains(url) && !err.is_empty() && err.len() < 120, "{os} {url}: {err}");
            texts.insert(err);
        }
        assert!(texts.len() >= 8, "the refusals say what is wrong in different words: {texts:?}");
        for not_a_file_link in ["https://h/a", "file", "", "ssh:///a"] {
            assert!(file_link_path("linux", not_a_file_link).is_err(), "{not_a_file_link:?}");
        }
    }

    fn file_url(path: &Path) -> String {
        let mut url = String::from("file://");
        for b in path.to_str().expect("text").bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' => url.push(char::from(b)),
                other => url.push_str(&format!("%{other:02X}")),
            }
        }
        url
    }

    #[test]
    fn a_file_link_in_a_local_pane_reveals_the_file_and_never_opens_it() {
        let (core, _rec, dir) = recording_core();
        let base = real_dir(&dir, "files");
        let file = base.join("note flp-1.txt");
        std::fs::write(&file, "x").unwrap();
        let script = base.join("run-flp-2.sh");
        std::fs::write(&script, "#!/bin/sh\necho run\n").unwrap();
        let pane = only_pane(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("starts");

        let text = |p: &Path| p.to_str().unwrap().to_owned();
        for (path, folder) in [(&file, false), (&script, false), (&base, true)] {
            let url = file_url(path);
            assert_eq!(core.open_url(&url, Some(pane)), Ok(()), "{url}");
            let launches = launches_with(&text(path)).into_iter().chain(launches_with(&text(path.parent().unwrap()))).collect::<Vec<_>>();
            assert!(!launches.is_empty(), "a reveal was recorded for {url}");
            let last = launches.last().unwrap();
            match OS {
                "macos" => {
                    assert_eq!((last.program.as_str(), last.args.as_slice()), ("/usr/bin/open", ["-R".to_owned(), text(path)].as_slice()));
                }
                "linux" => {
                    let shown = if folder { path.clone() } else { path.parent().unwrap().to_path_buf() };
                    assert_eq!((last.program.as_str(), last.args.as_slice()), ("xdg-open", [text(&shown)].as_slice()));
                }
                _ => assert!(last.program.ends_with("explorer.exe"), "{last:?}"),
            }
        }
        // The file itself is never an argument that opens: macOS and Linux only ever get the path after -R or its folder.
        let opened_plain = lock(&LAUNCHED).iter().any(|l| l.program == "/usr/bin/open" && l.args == [text(&script)]);
        assert!(!opened_plain, "the script was handed to open without -R");
    }

    #[test]
    fn a_file_link_is_refused_without_a_local_running_pane_or_the_scheme_in_the_list_each_with_its_own_words() {
        let (core, _rec, dir) = recording_core();
        let base = real_dir(&dir, "files");
        let file = base.join("flp-3.txt");
        std::fs::write(&file, "x").unwrap();
        let url = file_url(&file);
        let local = only_pane(&core);
        core.start_pane(local, 80, 24, 0, 0).expect("starts");
        saved_ssh(&core, "flp-ssh", "Server");
        let remote = core.connect_in(MAIN_WINDOW, "flp-ssh").expect("a tab for the connection").focus;

        let none = core.open_url(&url, None).expect_err("a missing pane");
        let unknown = core.open_url(&url, Some(999_999)).expect_err("an unknown pane");
        let ssh = core.open_url(&url, Some(remote)).expect_err("a pane of an ssh connection");
        let missing = core.open_url(&file_url(&base.join("flp-4-missing.txt")), Some(local)).expect_err("a path that is not there");
        set_schemes(&core, &["http", "https", "mailto"]);
        let unlisted = core.open_url(&url, Some(local)).expect_err("file is not in the list");
        let texts = [&none, &unknown, &ssh, &missing, &unlisted];
        for (i, a) in texts.iter().enumerate() {
            assert!(!a.contains("flp-") && !a.contains("file:") && a.len() < 160, "{a}");
            for b in &texts[i + 1..] {
                assert_ne!(a, b, "each refusal has its own words");
            }
        }
        assert!(launches_with(&text_of(&file)).is_empty() && launches_with(&text_of(&base)).is_empty(), "nothing was started");

        // The refusals of the link itself come first, whatever the pane is.
        set_schemes(&core, &["file"]);
        for bad in ["file:///tmp/a b", "file:///tmp/\u{7}a", "file://server/share/a", "file:///tmp/../etc/passwd-flp-5"] {
            assert!(core.open_url(bad, Some(local)).is_err(), "{bad:?}");
        }
        let long = format!("file:///{}", "a".repeat(MAX_URL_BYTES));
        assert!(core.open_url(&long, Some(local)).is_err(), "over {MAX_URL_BYTES} bytes");
        assert!(launches_with(&text_of(&file)).is_empty());
        assert_eq!(core.open_url(&url, Some(local)), Ok(()), "with file listed and a local pane it reveals");
    }

    fn text_of(p: &Path) -> String {
        p.to_str().unwrap().to_owned()
    }

    #[test]
    fn a_pane_given_for_another_kind_of_link_changes_nothing() {
        let (core, _rec, _dir) = recording_core();
        let pane = only_pane(&core);
        for (url, pane) in [("https://example.org/flp-6", Some(pane)), ("https://example.org/flp-7", None), ("https://example.org/flp-8", Some(999_999))] {
            assert_eq!(core.open_url(url, pane), Ok(()), "{url}");
            assert_eq!(launches_with(url).len(), 1);
        }
        assert!(core.open_url("ftp://example.org/flp-9", Some(pane)).is_err());
    }

    /// A process environment that has nothing in it.
    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn the_opener_is_one_fixed_program_with_the_link_as_a_single_argument() {
        let url = "https://example.org/a?b=c&d=e";

        assert_eq!(open_launch("macos", url, &web(), &no_env), Ok(Launch::plain("/usr/bin/open", vec![url.to_owned()])));
        assert_eq!(open_launch("linux", url, &web(), &no_env), Ok(Launch::plain("xdg-open", vec![url.to_owned()])));
        assert!(open_launch("macos", "file:///etc/passwd", &web(), &no_env).is_err());
        assert!(open_launch("plan9", url, &web(), &no_env).is_err(), "a platform nobody wrote an opener for");
    }

    #[test]
    fn the_local_network_settings_open_through_one_fixed_program_on_macos_only() {
        assert_eq!(
            settings_launch("macos"),
            Ok(Launch::plain(
                "/usr/bin/open",
                vec!["x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork".to_owned()]
            ))
        );
        for other in ["windows", "linux", "plan9", ""] {
            assert!(settings_launch(other).is_err(), "{other:?} has no such setting");
        }
    }

    #[test]
    fn opening_the_local_network_settings_starts_one_launch_on_macos_and_none_elsewhere() {
        let (core, _rec, _dir) = recording_core();
        let url = "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork";
        let before = launches_with(url).len();

        let result = core.open_local_network_settings();

        if OS == "macos" {
            assert_eq!(result, Ok(()));
            assert_eq!(launches_with(url).len(), before + 1, "exactly one launch");
            let launch = launches_with(url).pop().expect("a launch");
            assert_eq!(launch.program, "/usr/bin/open");
            assert_eq!(launch.args, [url]);
        } else {
            assert!(result.is_err());
            assert_eq!(launches_with(url).len(), before, "nothing is started where there is no setting");
        }
    }

    #[test]
    fn windows_launch_of_a_link_is_rundll32_with_the_link_unchanged_as_the_second_argument() {
        let url = "https://example.org/a?b=1&c=2^d";

        let launch = open_launch("windows", url, &web(), &no_env).expect("a link opens on Windows");

        assert_eq!(launch.program, r"C:\Windows\System32\rundll32.exe", r"SystemRoot defaults to C:\Windows");
        assert_eq!(launch.args, ["url.dll,FileProtocolHandler", url], "exactly two arguments, the link untouched");
        assert!(launch.raw_args.is_empty());
        let root = |name: &str| (name == "SystemRoot").then(|| r"D:\WinNT\".to_owned());
        assert_eq!(open_launch("windows", url, &web(), &root).unwrap().program, r"D:\WinNT\System32\rundll32.exe");
    }

    #[test]
    fn windows_launch_of_a_link_with_an_ampersand_or_a_caret_never_goes_through_a_shell() {
        // `&` and `^` are legal in a link and are metacharacters of cmd: no cmd, no command string, one argument.
        for url in ["https://example.org/?a=1&b=2", "https://example.org/a^b", "https://example.org/?a=1&calc.exe"] {
            let launch = open_launch("windows", url, &web(), &no_env).expect(url);
            assert!(launch.program.ends_with(r"\rundll32.exe"), "{launch:?}");
            assert!(!launch.program.to_ascii_lowercase().contains("cmd"), "{launch:?}");
            assert_eq!(launch.args.len(), 2, "{launch:?}");
            assert_eq!(launch.args[1], url, "the link is one argument");
        }
    }

    #[test]
    fn windows_launch_of_a_link_refuses_a_double_quote_and_what_every_platform_refuses() {
        assert!(open_launch("windows", "https://example.org/a\"b", &web(), &no_env).is_err(), "a double quote");
        assert!(open_launch("macos", "https://example.org/a\"b", &web(), &no_env).is_ok(), "only Windows rebuilds a command line");
        assert!(open_launch("windows", "file:///C:/Windows/System32/calc.exe", &web(), &no_env).is_err(), "a scheme");
        assert!(open_launch("windows", "https://example.org/a b", &web(), &no_env).is_err(), "a blank");
        assert!(open_launch("windows", "https://example.org/a\u{1b}b", &web(), &no_env).is_err(), "a control character");
        let too_long = format!("https://example.org/{}", "b".repeat(MAX_URL_BYTES));
        assert!(open_launch("windows", &too_long, &web(), &no_env).is_err(), "over {MAX_URL_BYTES} bytes");
    }

    #[test]
    fn windows_launch_ignores_a_system_root_that_is_not_a_drive_path() {
        let url = "https://example.org/";
        for hostile in [r"\\evil\share", "//evil/share", "relative", "", r"\\?\C:\Windows"] {
            let env = |name: &str| (name == "SystemRoot").then(|| hostile.to_owned());
            let program = open_launch("windows", url, &web(), &env).unwrap().program;
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
        assert_eq!(home_dir("linux", &env, None), Some(PathBuf::from("/home/me")));
        assert_eq!(home_dir("macos", &env, None), Some(PathBuf::from("/home/me")));
        assert_eq!(home_dir("windows", &env, None), Some(PathBuf::from(r"C:\Users\me")), "USERPROFILE, not HOME");
        assert_eq!(home_dir("windows", &|n: &str| (n == "HOME").then(|| "/home/me".to_owned()), None), None);
        assert_eq!(home_dir("linux", &|n: &str| (n == "HOME").then(String::new), None), None, "empty is no value");
        assert_eq!(home_dir("linux", &no_env, None), None);
    }

    #[test]
    fn the_private_folder_of_the_android_host_is_the_home_whatever_the_environment_says() {
        let private = Path::new("/data/user/0/ch.holzcloud.holzwave/files/home");
        let wanted = Some(private.to_path_buf());
        assert_eq!(home_dir("android", &no_env, Some(private)), wanted, "HOME unset");
        assert_eq!(home_dir("android", &|n: &str| (n == "HOME").then(String::new), Some(private)), wanted, "HOME empty");
        assert_eq!(home_dir("android", &|n: &str| (n == "HOME").then(|| "/".to_owned()), Some(private)), wanted, "HOME set");
        // Without a folder from the host the rule is the one of every other platform.
        assert_eq!(home_dir("android", &|n: &str| (n == "HOME").then(|| "/h".to_owned()), None), Some(PathBuf::from("/h")));
        assert_eq!(home_dir("android", &no_env, None), None);
    }

    /// A host that is the neutral one in everything but its platform and its private folders.
    struct PrivateHost {
        base: NeutralHost,
        platform: Platform,
        dirs: PrivateDirs,
    }

    impl crate::seams::Host for PrivateHost {
        fn platform(&self) -> Platform {
            self.platform
        }

        fn secrets(&self) -> Arc<dyn SecretStore> {
            self.base.secrets()
        }

        fn local_network(&self) -> Arc<dyn crate::local_network::Source> {
            self.base.local_network()
        }

        fn text_scale(&self) -> u32 {
            self.base.text_scale()
        }

        fn limits(&self) -> crate::seams::HostLimits {
            self.base.limits()
        }

        fn private_dirs(&self) -> Option<PrivateDirs> {
            Some(self.dirs.clone())
        }
    }

    #[test]
    fn a_core_asks_the_host_for_the_private_folders_only_on_android() {
        let dirs = PrivateDirs { home: PathBuf::from("/p/home"), cache: PathBuf::from("/p/cache") };
        let (dir, _pane) = dir_with_one_pane("private-dirs");
        for (platform, expected) in [(Platform::Android, Some(dirs.clone())), (Platform::Linux, None), (Platform::MacOs, None)] {
            let host = Arc::new(PrivateHost { base: NeutralHost::new(), platform, dirs: dirs.clone() });
            let core = AppCore::new_with_host(
                dir.0.clone(),
                Arc::new(Recorder::default()),
                ShellSpec::new("/bin/cat", &[]),
                None,
                None,
                host,
            );
            assert_eq!(core.private_dirs(), expected, "{platform:?}");
            assert_eq!(core.private_home(), expected.map(|d| d.home), "{platform:?}");
            assert_eq!(core.app_info().os, platform.as_str(), "the page is told the platform of the host");
            core.shutdown();
        }
    }

    #[test]
    fn the_home_of_connection_key_paths_and_blocks_is_the_private_home_only_on_android() {
        let dirs = PrivateDirs { home: PathBuf::from("/p/home"), cache: PathBuf::from("/p/cache") };
        for (platform, private) in [(Platform::Android, true), (Platform::Linux, false), (Platform::MacOs, false)] {
            let host = PrivateHost { base: NeutralHost::new(), platform, dirs: dirs.clone() };
            let home = shell_home(&host);
            if private {
                assert_eq!(home, Some(PathBuf::from("/p/home")), "{platform:?}: the private home, whatever HOME says");
            } else {
                assert_eq!(home, user_home(), "{platform:?}: the process's own home, as before");
            }
        }
        // A host with no private folders (the neutral one) keeps the process's home on every platform.
        assert_eq!(shell_home(&NeutralHost::new()), user_home());
    }

    #[test]
    fn the_variables_of_the_private_folders_are_home_and_tmpdir_and_a_bad_folder_gives_none() {
        let dirs = |home: &str, cache: &str| PrivateDirs { home: PathBuf::from(home), cache: PathBuf::from(cache) };
        assert_eq!(
            private_pane_env(&dirs("/data/x/files/home", "/data/x/cache")),
            [("HOME".to_owned(), "/data/x/files/home".to_owned()), ("TMPDIR".to_owned(), "/data/x/cache".to_owned())]
        );
        assert_eq!(private_pane_env(&dirs("relative", "/data/x/cache")), [("TMPDIR".to_owned(), "/data/x/cache".to_owned())]);
        assert_eq!(private_pane_env(&dirs("/data/x\nhome", "")), Vec::<(String, String)>::new());
        assert_eq!(private_pane_env(&dirs("/data/x/\u{7}home", "/data/x/cache\n")), Vec::<(String, String)>::new());
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

    /// A secret store that remembers what was asked of it, with the secret replaced by its length.
    struct RecordingStore {
        inner: holzwave_core::secrets::MemoryStore,
        log: Mutex<Vec<String>>,
    }

    impl RecordingStore {
        fn new() -> Arc<RecordingStore> {
            Arc::new(RecordingStore { inner: holzwave_core::secrets::MemoryStore::default(), log: Mutex::new(Vec::new()) })
        }

        fn calls(&self) -> Vec<String> {
            lock(&self.log).clone()
        }
    }

    impl SecretStore for RecordingStore {
        fn get(&self, account: &str) -> holzwave_core::Result<Option<String>> {
            lock(&self.log).push(format!("get {account}"));
            self.inner.get(account)
        }

        fn set(&self, account: &str, secret: &str) -> holzwave_core::Result<()> {
            lock(&self.log).push(format!("set {account} ({} bytes)", secret.len()));
            self.inner.set(account, secret)
        }

        fn delete(&self, account: &str) -> holzwave_core::Result<()> {
            lock(&self.log).push(format!("delete {account}"));
            self.inner.delete(account)
        }
    }

    /// A host that is not the desktop: the platform and the limits are the test's, the store records.
    struct TestHost {
        platform: crate::seams::Platform,
        store: Arc<RecordingStore>,
        limits: crate::seams::HostLimits,
    }

    impl TestHost {
        fn web() -> Arc<TestHost> {
            TestHost::on(crate::seams::Platform::Web, crate::seams::HostLimits::default())
        }

        fn on(platform: crate::seams::Platform, limits: crate::seams::HostLimits) -> Arc<TestHost> {
            Arc::new(TestHost { platform, store: RecordingStore::new(), limits })
        }
    }

    impl crate::seams::Host for TestHost {
        fn platform(&self) -> crate::seams::Platform {
            self.platform
        }

        fn secrets(&self) -> Arc<dyn SecretStore> {
            self.store.clone()
        }

        fn local_network(&self) -> Arc<dyn crate::local_network::Source> {
            Arc::new(crate::local_network::Unsupported)
        }

        fn text_scale(&self) -> u32 {
            100
        }

        fn limits(&self) -> crate::seams::HostLimits {
            self.limits
        }
    }

    fn core_on(host: Arc<TestHost>) -> (Arc<AppCore>, Arc<Recorder>, Scratch) {
        let dir = Scratch::new("host");
        let rec = Arc::new(Recorder::default());
        let core = AppCore::new_with_host(dir.0.clone(), rec.clone(), ShellSpec::new("/bin/cat", &[]), None, None, host);
        dir.attach(&core);
        (core, rec, dir)
    }

    #[test]
    fn seam_a_web_host_sets_the_platform_and_receives_the_connection_secret() {
        let host = TestHost::web();
        let (core, _rec, _dir) = core_on(host.clone());
        assert_eq!(core.app_info().os, "web");
        assert_eq!(core.host().platform(), crate::seams::Platform::Web);
        assert_eq!(core.host().text_scale(), 100);

        let profile = Profile {
            user: "holz".into(),
            auth: holzwave_core::profiles::Auth::Password,
            ..Profile::new("web-1", "Web 1", "web1.example.invalid")
        };
        let saved = core
            .connection_save(profile, SecretChange::Set { value: "correct horse battery".into() })
            .expect("a valid connection");
        assert!(saved.has_secret);

        let calls = host.store.calls();
        println!("recording store: {calls:?}");
        assert!(calls.iter().any(|call| call == "set ssh:web-1 (21 bytes)"), "{calls:?}");
        assert!(calls.iter().all(|call| !call.contains("correct horse")), "{calls:?}");
        assert_eq!(host.store.inner.get("ssh:web-1").expect("get").as_deref(), Some("correct horse battery"));
    }

    #[test]
    fn seam_the_constructors_without_a_host_use_the_neutral_one() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        assert_eq!(core.host().platform(), crate::seams::Platform::current());
        assert_eq!(core.app_info().os, std::env::consts::OS);
        assert_eq!(core.host().limits(), crate::seams::HostLimits::default());
        assert_eq!(core.host().text_scale(), 100);
        assert!(!core.host().local_network().applies());
    }

    #[test]
    fn seam_the_key_binding_table_follows_the_platform_of_the_host() {
        use crate::seams::{HostLimits, Platform};
        let rows = |platform: Platform| {
            let (core, _rec, _dir) = core_on(TestHost::on(platform, HostLimits::default()));
            core.get_settings().keybindings.iter().map(|r| (r.action.clone(), r.chord.clone())).collect::<Vec<_>>()
        };
        let (mac, win, linux, web) =
            (rows(Platform::MacOs), rows(Platform::Windows), rows(Platform::Linux), rows(Platform::Web));
        assert_ne!(mac, linux, "macOS has its own table");
        assert_ne!(win, linux, "Windows has its own table");
        // The browser table is the Linux one with close pane, new tab and new workspace moved off the chords a browser keeps.
        assert_ne!(web, linux, "the browser has its own table");
        let moved: Vec<&str> = web.iter().zip(&linux).filter(|(w, l)| w != l).map(|(w, _)| w.0.as_str()).collect();
        assert_eq!(moved, ["pane.close", "tab.new", "workspace.new"]);
    }

    /// The sentence a refused pane carries, for the cap `n`.
    fn cap_sentence(n: usize) -> String {
        format!("this server allows {n} panes per person; close one to open another")
    }

    /// A core on a web-flavoured host with `limits`, in a directory the caller keeps (so a second core can read the file).
    fn core_capped(dir: &Scratch, limits: crate::seams::HostLimits) -> (Arc<AppCore>, Arc<TestHost>) {
        let host = TestHost::on(crate::seams::Platform::Web, limits);
        let core = AppCore::new_with_host(
            dir.0.clone(),
            Arc::new(Recorder::default()),
            ShellSpec::new("/bin/cat", &[]),
            None,
            None,
            host.clone(),
        );
        dir.attach(&core);
        (core, host)
    }

    /// How many panes the layouts hold, over all windows. A function of its own so that a `println!` can call it twice.
    fn layout_panes(core: &AppCore) -> usize {
        lock(&core.windows).pane_count()
    }

    fn cap_of(max_panes: usize) -> crate::seams::HostLimits {
        crate::seams::HostLimits { max_panes: Some(max_panes), max_scrollback_lines: None, max_image_bytes: None }
    }

    /// What a refused request must leave as it was: the layout, the routes of the panes and the panes that run.
    fn cap_state(core: &AppCore) -> (String, usize, usize) {
        let layout = lock(&core.windows).to_json().expect("layout json");
        (layout, lock(&core.routes).live.len(), lock(&core.panes).len())
    }

    #[test]
    fn cap_the_nth_pane_is_accepted_in_any_mix_and_the_next_one_is_refused_with_a_sentence() {
        let dir = Scratch::new("cap-mix");
        let (core, _host) = core_capped(&dir, cap_of(4));
        let mut table = Vec::new();
        // One pane in the layout: a split, a new tab and a new workspace make the 2nd, 3rd and 4th.
        table.push(("cap 4", 1, "split", core.split_in(MAIN_WINDOW, true).map(|_| layout_panes(&core))));
        table.push(("cap 4", 2, "new tab", core.new_tab_in(MAIN_WINDOW).map(|_| layout_panes(&core))));
        table.push(("cap 4", 3, "new workspace", core.new_workspace_in(MAIN_WINDOW, "two").map(|_| layout_panes(&core))));
        let before = cap_state(&core);
        // The 5th, by every way of making a pane.
        table.push(("cap 4", 4, "split", core.split_in(MAIN_WINDOW, false).map(|_| layout_panes(&core))));
        table.push(("cap 4", 4, "new tab", core.new_tab_in(MAIN_WINDOW).map(|_| layout_panes(&core))));
        table.push(("cap 4", 4, "new workspace", core.new_workspace_in(MAIN_WINDOW, "three").map(|_| layout_panes(&core))));
        for (cap, panes, request, outcome) in &table {
            println!("{cap} | panes before: {panes} | {request} | {outcome:?}");
        }
        for (_, _, _, outcome) in &table[..3] {
            assert!(outcome.is_ok(), "{outcome:?}");
        }
        assert_eq!(table[2].3, Ok(4), "the 4th pane is the cap and is accepted");
        for (_, _, _, outcome) in &table[3..] {
            assert_eq!(outcome.as_ref().expect_err("refused"), &cap_sentence(4));
        }
        assert_eq!(cap_state(&core), before, "a refused request changes nothing and starts nothing");
        assert_eq!(layout_panes(&core), 4);
    }

    #[test]
    fn cap_of_one_refuses_the_first_split_and_the_sentence_names_the_cap() {
        let dir = Scratch::new("cap-one");
        let (core, _host) = core_capped(&dir, cap_of(1));
        let before = cap_state(&core);
        let outcome = core.split_in(MAIN_WINDOW, true);
        println!("cap 1 | panes before: 1 | split | {outcome:?}");
        assert_eq!(outcome.expect_err("refused"), "this server allows 1 panes per person; close one to open another");
        assert_eq!(cap_state(&core), before);
    }

    #[test]
    fn cap_a_pane_outside_the_layout_counts_with_the_layout_and_gives_its_place_back() {
        let dir = Scratch::new("cap-outside");
        let (core, _host) = core_capped(&dir, cap_of(3));
        // One pane in the layout and room for two more, which a session of a server may take.
        let first = core.take_outside_pane().expect("a place");
        let second = core.take_outside_pane().expect("a place");
        assert_eq!(core.outside_pane_count(), 2);
        assert!(core.take_outside_pane().is_none(), "the cap is reached: 1 pane and 2 sessions");
        // The window cannot take the place a session holds, and a refused request changes nothing.
        let before = cap_state(&core);
        assert_eq!(core.split_in(MAIN_WINDOW, true).expect_err("refused"), cap_sentence(3));
        assert_eq!(cap_state(&core), before);
        // A place that is given back is a place again, for either kind.
        drop(first);
        assert_eq!(core.outside_pane_count(), 1);
        assert!(core.split_in(MAIN_WINDOW, true).is_ok());
        assert!(core.take_outside_pane().is_none(), "the window took it");
        drop(second);
        assert!(core.take_outside_pane().is_some());
    }

    #[test]
    fn a_session_spec_is_headless_in_the_home_and_carries_no_wsh_variable() {
        let dir = Scratch::new("session-spec");
        let (core, _host) = core_capped(&dir, crate::seams::HostLimits::default());
        let spec = core.session_pane_spec(100, 30);
        assert!(spec.headless);
        assert_eq!((spec.cols, spec.rows), (100, 30));
        assert!(spec.env.is_empty(), "no socket, no token and no pane number: {:?}", spec.env);
        assert!(!spec.blank && spec.ssh.is_none());
    }

    #[test]
    fn cap_a_layout_restored_above_the_cap_keeps_every_pane_and_refuses_new_ones_until_it_is_below() {
        let dir = Scratch::new("cap-restored");
        {
            let (core, _host) = core_capped(&dir, crate::seams::HostLimits::default());
            for _ in 0..5 {
                core.split_in(MAIN_WINDOW, true).expect("split");
            }
            assert_eq!(layout_panes(&core), 6);
            core.flush();
        }
        let (core, _host) = core_capped(&dir, cap_of(4));
        assert_eq!(layout_panes(&core), 6, "the restored panes all stay");
        let outcome = |core: &AppCore, request: &str| {
            let before = layout_panes(core);
            let result = match request {
                "tab" => core.new_tab_in(MAIN_WINDOW),
                _ => core.split_in(MAIN_WINDOW, true),
            };
            println!("cap 4 | panes before: {before} | {request} | {:?}", result.as_ref().map(|_| layout_panes(core)));
            result
        };
        assert_eq!(outcome(&core, "tab").expect_err("refused"), cap_sentence(4));
        // Closing two panes brings the count to 4, which is the cap: still no room.
        let panes = lock(&core.windows).all_panes();
        core.close_pane_in(MAIN_WINDOW, panes[0]).expect("close");
        core.close_pane_in(MAIN_WINDOW, panes[1]).expect("close");
        assert_eq!(layout_panes(&core), 4);
        assert_eq!(outcome(&core, "split").expect_err("refused"), cap_sentence(4));
        // A third one closed leaves room for exactly one.
        core.close_pane_in(MAIN_WINDOW, panes[2]).expect("close");
        assert_eq!(layout_panes(&core), 3);
        assert!(outcome(&core, "split").is_ok());
        assert_eq!(layout_panes(&core), 4);
        assert_eq!(outcome(&core, "split").expect_err("refused"), cap_sentence(4));
    }

    #[test]
    fn cap_a_block_that_needs_a_new_pane_is_refused_at_the_cap_and_assigns_nothing() {
        let dir = Scratch::new("cap-block");
        let folder = dir.0.join("docs");
        std::fs::create_dir(&folder).unwrap();
        let (core, _host) = core_capped(&dir, cap_of(1));
        let before = cap_state(&core);
        for placement in ["tab", "right", "down"] {
            let outcome = core.block_open_in(MAIN_WINDOW, placement, BlockKind::Files, "local", folder.to_str().unwrap());
            println!("cap 1 | panes before: 1 | block {placement} | {:?}", outcome.as_ref().map(|_| layout_panes(&core)));
            assert_eq!(outcome.expect_err("refused"), cap_sentence(1));
        }
        assert!(core.blocks_list().is_empty(), "no block assignment is left behind");
        assert_eq!(cap_state(&core), before);
        // One more room and the same request is accepted: the refusal was the cap and nothing else.
        let dir = Scratch::new("cap-block-room");
        let (core, _host) = core_capped(&dir, cap_of(2));
        core.block_open_in(MAIN_WINDOW, "tab", BlockKind::Files, "local", folder.to_str().unwrap()).expect("room for it");
        assert_eq!(core.blocks_list().len(), 1);
    }

    #[test]
    fn cap_with_no_cap_the_layouts_own_limit_decides_at_255_panes() {
        let dir = Scratch::new("cap-none");
        let (core, _host) = core_capped(&dir, crate::seams::HostLimits::default());
        for k in 1..255 {
            let made = if k % 2 == 0 { core.new_tab_in(MAIN_WINDOW) } else { core.new_workspace_in(MAIN_WINDOW, "w") };
            made.expect("below the limit of the layout");
        }
        assert_eq!(layout_panes(&core), 255);
        let at_255 = core.new_tab_in(MAIN_WINDOW);
        println!("no cap | panes before: 255 | new tab | {:?}", at_255.as_ref().map(|_| layout_panes(&core)));
        assert!(at_255.is_ok(), "{at_255:?}");
        assert_eq!(layout_panes(&core), 256);
        // At the layout's own limit the view comes back unchanged and is no error, as it always did.
        let past = core.new_tab_in(MAIN_WINDOW);
        assert!(past.is_ok(), "{past:?}");
        assert_eq!(layout_panes(&core), 256);
    }

    #[cfg(unix)]
    #[test]
    fn cap_the_scrollback_of_a_new_pane_is_the_smaller_of_the_setting_and_the_cap() {
        let spec_with = |setting: u32, cap: Option<usize>| {
            let dir = Scratch::new("cap-scrollback");
            std::fs::write(dir.0.join("settings.json"), format!(r#"{{"scrollback_lines": {setting}}}"#)).unwrap();
            let limits = crate::seams::HostLimits { max_panes: None, max_scrollback_lines: cap, max_image_bytes: None };
            let (core, _host) = core_capped(&dir, limits);
            let lines = core.pane_spec(80, 24, (9, 18)).scrollback;
            println!("scrollback setting {setting} | cap {cap:?} | pane {lines}");
            // The setting itself is not rewritten by the cap.
            assert_eq!(core.get_settings().settings.scrollback_lines, setting);
            lines
        };
        assert_eq!(spec_with(10_000, Some(500)), 500);
        assert_eq!(spec_with(300, Some(500)), 300);
        assert_eq!(spec_with(500, Some(500)), 500);
        assert_eq!(spec_with(10_000, None), 10_000);
    }

    const MIB: u64 = 1024 * 1024;

    /// The image memory of the spec of a new pane, from a settings document and a host cap, and what the setting says
    /// afterwards (the cap never rewrites it).
    #[cfg(unix)]
    fn image_bytes_with(document: &str, cap: Option<usize>) -> (u64, u32) {
        let dir = Scratch::new("cap-images");
        if !document.is_empty() {
            std::fs::write(dir.0.join("settings.json"), document).unwrap();
        }
        let limits = crate::seams::HostLimits { max_panes: None, max_scrollback_lines: None, max_image_bytes: cap };
        let (core, _host) = core_capped(&dir, limits);
        let bytes = core.pane_spec(80, 24, (9, 18)).image_bytes;
        let setting = core.get_settings().settings.image_memory_mib;
        println!("document {document:?} | cap {cap:?} | spec {bytes} | setting {setting}");
        (bytes, setting)
    }

    #[cfg(unix)]
    #[test]
    fn image_memory_the_setting_in_mib_is_the_image_bytes_of_a_new_pane() {
        assert_eq!(image_bytes_with("", None), (64 * MIB, 64), "the default settings give 64 MiB");
        assert_eq!(image_bytes_with(r#"{"image_memory_mib": 5}"#, None), (5 * MIB, 5));
        assert_eq!(image_bytes_with(r#"{"image_memory_mib": 512}"#, None), (512 * MIB, 512));
        // A value past the range is clamped by the load, so the spec never asks for more than 512 MiB.
        assert_eq!(image_bytes_with(r#"{"image_memory_mib": 4000000000}"#, None), (512 * MIB, 512));
    }

    #[cfg(unix)]
    #[test]
    fn image_memory_the_host_cap_cuts_the_spec_and_not_the_setting() {
        let cap = Some(2 * MIB as usize);
        assert_eq!(image_bytes_with(r#"{"image_memory_mib": 5}"#, cap), (2 * MIB, 5), "the cap is the smaller");
        assert_eq!(image_bytes_with(r#"{"image_memory_mib": 1}"#, cap), (MIB, 1), "the setting is the smaller");
        assert_eq!(image_bytes_with("", cap), (2 * MIB, 64));
        // A cap of 0 keeps no picture whatever the person asks for.
        assert_eq!(image_bytes_with(r#"{"image_memory_mib": 5}"#, Some(0)), (0, 5));
    }

    #[cfg(unix)]
    #[test]
    fn image_memory_zero_in_the_setting_is_zero_in_the_spec() {
        assert_eq!(image_bytes_with(r#"{"image_memory_mib": 0}"#, None), (0, 0));
        assert_eq!(image_bytes_with(r#"{"image_memory_mib": 0}"#, Some(MIB as usize)), (0, 0));
    }

    #[cfg(unix)]
    #[test]
    fn image_memory_a_running_pane_keeps_its_value_when_the_setting_changes() {
        let dir = Scratch::new("image-running");
        std::fs::write(dir.0.join("settings.json"), r#"{"image_memory_mib": 8}"#).unwrap();
        let (core, _rec) = core_in(&dir, "/bin/cat", &[]);
        let before = core.pane_spec(80, 24, (9, 18));
        assert_eq!(before.image_bytes, 8 * MIB);
        // The spec is a value: a later write changes the next spec and not the one a pane was started with.
        core.set_setting("image_memory_mib", serde_json::json!(16)).expect("the setting is written");
        assert_eq!(before.image_bytes, 8 * MIB);
        assert_eq!(core.pane_spec(80, 24, (9, 18)).image_bytes, 16 * MIB);
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
            open_launch("windows", "https://example.org/", &web(), &no_env),
            open_launch("windows", "https://example.org/", &web(), &hostile),
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
    fn a_command_end_goes_to_the_window_that_has_the_pane() {
        // `ESC ] 133 ; D ; 3 ESC \`, written through printf as octal escapes; the shell then waits so the pane stays.
        let (core, rec, _dir, first, second, label) = two_windows("printf '\\033]133;D;3\\033\\\\'; sleep 30");
        core.start_pane(second, 80, 24, 0, 0).expect("starts");

        assert!(
            wait_until(WAIT, || rec.sent().iter().any(|(_, o)| matches!(o, Out::CommandEnd { .. }))),
            "no CommandEnd: {:?}",
            rec.all()
        );

        let ends: Vec<(Target, Out)> = rec.sent().into_iter().filter(|(_, o)| matches!(o, Out::CommandEnd { .. })).collect();
        assert_eq!(
            ends,
            vec![(Target::window(&label), Out::CommandEnd { pane: second, code: Some(3) })],
            "one command end, to the window of the pane"
        );
        assert_ne!(first, second);
        assert_eq!(rec.targets_of("command-end", 0), vec![Target::window(&label)]);
    }

    #[cfg(unix)]
    #[test]
    fn a_bell_goes_to_the_window_that_has_the_pane() {
        // One BEL, written as an octal escape; the shell then waits so the pane stays.
        let (core, rec, _dir, first, second, label) = two_windows("printf '\\007'; sleep 30");
        core.start_pane(second, 80, 24, 0, 0).expect("starts");

        assert!(wait_until(WAIT, || rec.sent().iter().any(|(_, o)| matches!(o, Out::Bell { .. }))), "no Bell: {:?}", rec.all());

        let bells: Vec<(Target, Out)> = rec.sent().into_iter().filter(|(_, o)| matches!(o, Out::Bell { .. })).collect();
        assert_eq!(bells, vec![(Target::window(&label), Out::Bell { pane: second })], "one bell, to the window of the pane");
        assert_ne!(first, second);
        assert_eq!(rec.targets_of("bell", 0), vec![Target::window(&label)]);
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
            assert!(core.move_pane_to_workspace_in(label, before.focus, 0).is_err());
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

    #[test]
    fn a_view_counts_the_panes_of_every_tab_and_workspace_and_the_windows() {
        let (core, _rec, _dir, _first, _second, label) = two_windows("sleep 30");
        core.split(false);
        core.new_tab();
        core.new_workspace("two");
        core.split(false);

        let view = core.view();
        assert_eq!(view.windows, 2);
        assert_eq!(view.workspaces.iter().map(|w| w.panes).collect::<Vec<_>>(), vec![3, 2]);
        assert_eq!(view.tabs.iter().map(|t| t.panes).collect::<Vec<_>>(), vec![2]);
        let view = core.select_workspace(0);
        assert_eq!(view.tabs.iter().map(|t| t.panes).collect::<Vec<_>>(), vec![2, 1]);
        let other = core.view_in(&label).unwrap();
        assert_eq!((other.windows, other.workspaces[0].panes, other.tabs[0].panes), (2, 1, 1));
        core.close_tab_in(&label, 0).unwrap();
        assert_eq!(core.view().windows, 1, "the next view knows that the other window is gone");
    }

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

    #[test]
    fn closing_a_window_forgets_a_pane_whose_shell_never_started() {
        // A pane that never started has no slot, but it left the layout all the same: what the core remembered of it
        // (and the chat turns that act for it) go with it.
        let (core, _rec, _dir, _first, second, label) = two_windows("sleep 30");
        lock(&core.last_title).insert(second, "never started".to_owned());

        core.window_closed(&label);

        assert_eq!(core.window_labels(), vec![MAIN_WINDOW.to_owned()]);
        assert!(!lock(&core.last_title).contains_key(&second), "the pane was not forgotten");
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

    // ---- the live outline across windows ----------------------------------------------------------------------

    fn hover(core: &AppCore, from: &str, pane: PaneId) {
        core.drag_over_in(from, pane, DropKind::Pane).expect("a drag over");
    }

    fn take(rec: &Recorder) -> Vec<(Target, Out)> {
        std::mem::take(&mut *lock(&rec.log))
    }

    #[test]
    fn a_drag_over_another_window_shows_its_outline_there_and_moves_nothing() {
        let (core, rec, _dir, a, b, c, w) = three_panes("sleep 30");
        let before = core.view();
        let before_w = core.view_in(&w).unwrap();
        over(&rec, CursorSpot::Window { label: w.clone(), x: 12.5, y: 30.0 });

        hover(&core, MAIN_WINDOW, b);
        hover(&core, MAIN_WINDOW, b);

        let hover = |x, y| (Target::window(&w), Out::DragHover { kind: DropKind::Pane, pane: b, x, y });
        assert_eq!(
            rec.sent(),
            vec![hover(12.5, 30.0), hover(12.5, 30.0)],
            "only the window under the pointer is told, with ids and the pointer in its own coordinates, every time"
        );
        assert_eq!(without_rev(&core.view()), without_rev(&before), "nothing moved");
        assert_eq!(without_rev(&core.view_in(&w).unwrap()), without_rev(&before_w));
        assert_eq!(panes_in(&core, MAIN_WINDOW), vec![a, b]);
        assert_eq!(panes_in(&core, &w), vec![c]);
        assert!(lock(&core.drops).is_empty(), "no drop is in flight: only a release makes one");
    }

    #[test]
    fn a_drag_that_moves_on_tells_the_window_it_left_and_one_that_ends_tells_the_last() {
        let (core, rec, _dir, a, b, _c, w) = three_panes("sleep 30");
        let w2 = raw_tear_off(&core, b);
        take(&rec);
        let pointer = |label: &str| over(&rec, CursorSpot::Window { label: label.to_owned(), x: 5.0, y: 6.0 });

        pointer(&w);
        core.drag_over_in(MAIN_WINDOW, a, DropKind::Pane).unwrap();
        pointer(&w2);
        core.drag_over_in(MAIN_WINDOW, a, DropKind::Pane).unwrap();
        assert_eq!(
            take(&rec),
            vec![
                (Target::window(&w), Out::DragHover { kind: DropKind::Pane, pane: a, x: 5.0, y: 6.0 }),
                (Target::window(&w), Out::DragEnd),
                (Target::window(&w2), Out::DragHover { kind: DropKind::Pane, pane: a, x: 5.0, y: 6.0 }),
            ],
            "the window the pointer left is told, then the one it is over"
        );

        over(&rec, CursorSpot::Nothing);
        core.drag_over_in(MAIN_WINDOW, a, DropKind::Pane).unwrap();
        core.drag_over_in(MAIN_WINDOW, a, DropKind::Pane).unwrap();
        assert_eq!(take(&rec), vec![(Target::window(&w2), Out::DragEnd)], "over nothing: told once");

        pointer(&w);
        core.drag_over_in(MAIN_WINDOW, a, DropKind::Pane).unwrap();
        take(&rec);
        core.drag_end_in(MAIN_WINDOW).unwrap();
        core.drag_end_in(MAIN_WINDOW).unwrap();
        assert_eq!(take(&rec), vec![(Target::window(&w), Out::DragEnd)], "Escape or a release anywhere: told once");
    }

    #[test]
    fn the_pointer_over_the_source_or_unknown_or_a_window_that_is_gone_shows_nothing() {
        let (core, rec, _dir, _a, b, _c, w) = three_panes("sleep 30");
        for spot in [
            CursorSpot::Window { label: MAIN_WINDOW.into(), x: 1.0, y: 1.0 },
            CursorSpot::Window { label: "window-404".into(), x: 1.0, y: 1.0 },
            CursorSpot::Unknown,
            CursorSpot::Nothing,
        ] {
            over(&rec, spot);
            hover(&core, MAIN_WINDOW, b);
        }
        assert!(rec.sent().is_empty(), "{:?}", rec.sent());
        assert!(core.view_in(&w).is_ok());
    }

    #[test]
    fn a_release_takes_the_outline_away_before_the_window_is_asked_where_the_pane_goes() {
        let (core, rec, _dir, _a, b, _c, w) = three_panes("sleep 30");
        over(&rec, CursorSpot::Window { label: w.clone(), x: 12.5, y: 30.0 });
        hover(&core, MAIN_WINDOW, b);
        take(&rec);

        assert_eq!(core.release_outside_in(MAIN_WINDOW, b, DropKind::Pane), Ok(Released::Handed { to: w.clone() }));

        assert_eq!(
            rec.sent(),
            vec![
                (Target::window(&w), Out::DragEnd),
                (Target::window(&w), Out::IncomingDrop { kind: DropKind::Pane, pane: b, x: 12.5, y: 30.0 }),
            ]
        );
        // A release that tears off (over nothing) ends the outline too.
        over(&rec, CursorSpot::Window { label: w.clone(), x: 1.0, y: 1.0 });
        let a = *panes_in(&core, MAIN_WINDOW).first().unwrap();
        hover(&core, MAIN_WINDOW, a);
        take(&rec);
        over(&rec, CursorSpot::Nothing);
        assert!(matches!(core.release_outside_in(MAIN_WINDOW, a, DropKind::Pane), Ok(Released::TornOff { .. })));
        assert_eq!(rec.sent().first(), Some(&(Target::window(&w), Out::DragEnd)));
    }

    #[test]
    fn a_window_that_closes_ends_the_outline_it_caused_and_one_it_showed_is_forgotten() {
        let (core, rec, _dir, _a, b, _c, w) = three_panes("sleep 30");
        over(&rec, CursorSpot::Window { label: w.clone(), x: 5.0, y: 6.0 });
        hover(&core, MAIN_WINDOW, b);
        take(&rec);

        // The window the drag is over closes: nobody is left to tell, and a later end finds nothing.
        core.window_closed(&w);
        assert!(rec.sent().is_empty(), "{:?}", rec.sent());
        core.drag_end_in(MAIN_WINDOW).unwrap();
        assert!(rec.sent().is_empty());

        // The window the drag started in closes (a pane that ends under the pointer, the red button): the other
        // window is told.
        let (core, rec, _dir, _first, second, label) = two_windows("sleep 30");
        over(&rec, CursorSpot::Window { label: MAIN_WINDOW.into(), x: 5.0, y: 6.0 });
        core.drag_over_in(&label, second, DropKind::Pane).unwrap();
        take(&rec);
        core.window_closed(&label);
        assert_eq!(take(&rec), vec![(Target::window(MAIN_WINDOW), Out::DragEnd)]);
    }

    #[test]
    fn a_window_can_only_show_the_outline_of_its_own_panes_and_not_while_the_app_ends() {
        let (core, rec, _dir, _a, b, c, w) = three_panes("sleep 30");
        over(&rec, CursorSpot::Window { label: w.clone(), x: 5.0, y: 6.0 });

        assert_eq!(core.drag_over_in(MAIN_WINDOW, c, DropKind::Pane), Err("no such pane".to_owned()), "a pane of another window");
        assert_eq!(core.drag_over_in(MAIN_WINDOW, 9999, DropKind::Pane), Err("no such pane".to_owned()));
        assert!(rec.sent().is_empty());
        assert_eq!(core.drag_over_in("window-404", b, DropKind::Pane), Err("no such window".to_owned()));
        assert_eq!(core.drag_end_in("window-404"), Err("no such window".to_owned()));
        // A tab is named by a pane of the shown tab.
        assert!(core.drag_over_in(MAIN_WINDOW, b, DropKind::Tab).is_ok());
        assert!(core.drag_over_in(MAIN_WINDOW, c, DropKind::Tab).is_err());

        core.exiting.store(true, Ordering::SeqCst);
        assert_eq!(core.drag_over_in(MAIN_WINDOW, b, DropKind::Pane), Err("the app is closing".to_owned()));
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

    // ---- wsh through a tunnel ---------------------------------------------------------------------------------------

    /// The files of a host, as the pane's saved connection sees them. It remembers every path it was asked about, so a
    /// test can tell what a request made the core ask and what it never did.
    #[derive(Default)]
    struct Host {
        files: HashMap<String, Vec<u8>>,
        dirs: Vec<String>,
        others: Vec<String>,
        seen: Mutex<Vec<String>>,
    }

    impl Host {
        fn with_files(files: &[(&str, &str)]) -> Arc<Host> {
            Arc::new(Host {
                files: files.iter().map(|(path, text)| ((*path).to_owned(), text.as_bytes().to_vec())).collect(),
                dirs: vec!["/srv".into()],
                others: vec!["/dev/null".into()],
                seen: Mutex::default(),
            })
        }

        fn seen(&self) -> Vec<String> {
            lock(&self.seen).clone()
        }
    }

    impl FileSource for Host {
        fn label(&self) -> String {
            "Web 1".into()
        }

        fn home(&self) -> holzwave_core::Result<String> {
            Ok("/home/holz".into())
        }

        fn list(&self, _path: &str) -> holzwave_core::Result<Listing> {
            Err(holzwave_core::Error::Unsupported("no listing in this double".into()))
        }

        fn stat(&self, path: &str) -> holzwave_core::Result<Entry> {
            lock(&self.seen).push(path.to_owned());
            let name = path.rsplit('/').next().unwrap_or(path).to_owned();
            let entry = |kind, size| Entry { name: name.clone(), kind, link: false, size, modified: Some(1), hidden: false };
            if let Some(bytes) = self.files.get(path) {
                Ok(entry(holzwave_core::files::Kind::File, bytes.len() as u64))
            } else if self.dirs.iter().any(|d| d == path) {
                Ok(entry(holzwave_core::files::Kind::Dir, 0))
            } else if self.others.iter().any(|d| d == path) {
                Ok(entry(holzwave_core::files::Kind::Other, 0))
            } else {
                Err(holzwave_core::Error::Unsupported("no such file".into()))
            }
        }

        fn read(&self, path: &str, _limit: u64) -> holzwave_core::Result<Vec<u8>> {
            self.files.get(path).cloned().ok_or_else(|| holzwave_core::Error::Unsupported("no such file".into()))
        }

        fn read_range(&self, path: &str, offset: u64, len: u64) -> holzwave_core::Result<Vec<u8>> {
            let bytes = self.read(path, u64::MAX)?;
            let from = (offset as usize).min(bytes.len());
            Ok(bytes[from..(from + len as usize).min(bytes.len())].to_vec())
        }

        fn write(&self, _path: &str, _bytes: &[u8]) -> holzwave_core::Result<()> {
            Err(holzwave_core::Error::Unsupported("read only".into()))
        }
    }

    /// A core with the saved SSH connections `web-1` (with `wsh` on) and `web-2` (with `wsh` off), a local first pane, a
    /// pane that runs `web-1` and one that runs `web-2`, and the files of `host` standing in for the host of `web-1`.
    /// Nothing is started: a request is answered against the layout, so no shell and no network is needed.
    struct Ticked {
        core: Arc<AppCore>,
        rec: Arc<Recorder>,
        _dir: Scratch,
        local: PaneId,
        remote: PaneId,
        unticked: PaneId,
        host: Arc<Host>,
    }

    impl Ticked {
        /// Where the connection of the ticked pane points now, as a pane that started now would have recorded it.
        fn target(&self) -> crate::wsh_tunnel::Target {
            crate::wsh_tunnel::Target::of(&self.core.wsh_profile(self.remote).expect("the ticked pane runs a ticked connection"))
        }
    }

    fn ticked(files: &[(&str, &str)]) -> Ticked {
        let dir = Scratch::new("ticked");
        let mut profiles = holzwave_core::profiles::Profiles::default();
        for (id, wsh) in [("web-1", true), ("web-2", false)] {
            let profile = Profile {
                user: "holz".into(),
                key_path: "/nonexistent/holzwave-test-key".into(),
                wsh,
                ..Profile::new(id, id, "web.example.invalid")
            };
            profiles.upsert(profile).unwrap();
        }
        profiles.save(&dir.0).unwrap();
        let (core, rec) = core_in(&dir, "sh", &[]);
        let local = only_pane(&core);
        let remote = core.split(true).focus;
        let unticked = core.split(true).focus;
        core.connections.assign(remote, "web-1").unwrap();
        core.connections.assign(unticked, "web-2").unwrap();
        let host = Host::with_files(files);
        core.blocks.inject_source("web-1", core.connections.generation(), host.clone());
        Ticked { core, rec, _dir: dir, local, remote, unticked, host }
    }

    /// Send `request` (a JSON object, with `token` filled in from the tunnel unless it names one) through the forwarded
    /// stream of `tunnel` and give the answer line, parsed. The real `serve_remote` reads it.
    fn through(tunnel: &holzwave_core::ssh::Tunnel, mut request: Value) -> Value {
        if request.get("token").is_none() {
            request["token"] = Value::String(token_of(tunnel));
        }
        through_raw(tunnel, &serde_json::to_vec(&request).unwrap())
    }

    /// [`through`] for the bytes of a request as they are (no token is added).
    fn through_raw(tunnel: &holzwave_core::ssh::Tunnel, request: &[u8]) -> Value {
        let mut line = request.to_vec();
        line.push(b'\n');
        let (stream, answered) = holzwave_core::pool::ForwardedStream::in_memory(&line);
        tunnel.sink.serve(stream);
        let deadline = Instant::now() + WAIT;
        loop {
            let bytes = lock(&answered).clone();
            if bytes.ends_with(b"\n") {
                assert_eq!(bytes.iter().filter(|b| **b == b'\n').count(), 1, "exactly one answer line");
                return serde_json::from_slice(&bytes).expect("the answer is JSON");
            }
            assert!(Instant::now() < deadline, "no answer came back");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn token_of(tunnel: &holzwave_core::ssh::Tunnel) -> String {
        tunnel.env.iter().find(|(name, _)| name == crate::wsh_tunnel::TOKEN_VAR).map(|(_, value)| value.clone()).expect("the token")
    }

    /// The last layout event the window `main` was sent.
    fn last_layout(rec: &Recorder) -> Option<View> {
        rec.all().into_iter().rev().find_map(|out| if let Out::Layout(view) = out { Some(view) } else { None })
    }

    #[test]
    fn a_ticked_ssh_pane_gets_a_tunnel_and_nothing_else_does() {
        use holzwave_core::pool::valid_forward_path;
        let t = ticked(&[]);
        let tunnel = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).expect("a ticked SSH pane has a tunnel");
        assert!(valid_forward_path(&tunnel.path), "{}", tunnel.path);
        let mut names: Vec<&str> = tunnel.env.iter().map(|(name, _)| name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["HOLZWAVE_PANE", "HOLZWAVE_SOCKET", "HOLZWAVE_TOKEN"]);
        let value = |name: &str| tunnel.env.iter().find(|(n, _)| n == name).unwrap().1.clone();
        assert_eq!(value("HOLZWAVE_SOCKET"), tunnel.path);
        assert_eq!(value("HOLZWAVE_PANE"), t.remote.to_string());
        let token = value("HOLZWAVE_TOKEN");
        assert!(token.len() == 32 && token.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')), "{token}");
        assert!(!tunnel.path.contains(&token), "the path and the token are independent values");

        let again = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap();
        assert_ne!(again.path, tunnel.path);
        assert_ne!(token_of(&again), token);

        assert!(crate::wsh_tunnel::tunnel_for(&t.core, t.unticked).is_none(), "an SSH pane that is not ticked");
        assert!(crate::wsh_tunnel::tunnel_for(&t.core, t.local).is_none(), "a local pane");
        assert!(crate::wsh_tunnel::tunnel_for(&t.core, 9999).is_none(), "a pane nobody knows");

        // A Docker connection, even one that was ticked in an older file, gets none.
        let mut docker = Profile::new("api", "API", "api-1");
        docker.kind = ConnectionKind::Docker;
        t.core.connections.save(docker, SecretChange::Keep).unwrap();
        let pane = t.core.split(true).focus;
        t.core.connections.assign(pane, "api").unwrap();
        assert!(crate::wsh_tunnel::tunnel_for(&t.core, pane).is_none(), "a Docker pane");

        // A block pane that has a saved connection assigned (a preview of a file of the host) runs nothing.
        let block = t.core.split(true).focus;
        t.core.connections.assign(block, "web-1").unwrap();
        t.core.blocks.assign(block, Block { kind: BlockKind::Preview, source: "web-1".into(), path: "/srv/a".into(), edit: false }).unwrap();
        assert!(crate::wsh_tunnel::tunnel_for(&t.core, block).is_none(), "a block pane");
    }

    #[test]
    fn a_pane_spec_gets_the_tunnel_only_when_the_connection_was_made_up() {
        let t = ticked(&[]);
        let mut spec = PaneSpec::new("sh", 80, 24);
        t.core.apply_connection(t.remote, &mut spec);
        assert!(spec.ssh_connection.is_some() && spec.notice.is_none(), "{:?}", spec.notice);
        let tunnel = spec.tunnel.clone().expect("the ticked pane starts with a tunnel");
        assert!(spec.env.is_empty(), "the variables live in the tunnel, not in the spec's environment");
        let shown = format!("{spec:?} {tunnel:?}");
        assert!(!shown.contains(&token_of(&tunnel)) && !shown.contains(&tunnel.path), "{shown}");

        let mut plain = PaneSpec::new("sh", 80, 24);
        t.core.apply_connection(t.unticked, &mut plain);
        assert!(plain.ssh_connection.is_some() && plain.tunnel.is_none(), "an SSH pane that is not ticked");
        let mut local = PaneSpec::new("sh", 80, 24);
        t.core.apply_connection(t.local, &mut local);
        assert!(local.ssh_connection.is_none() && local.tunnel.is_none(), "a local pane");
    }

    #[test]
    fn a_view_through_the_forwarded_socket_opens_a_preview_of_the_hosts_file() {
        let t = ticked(&[("/srv/notes.md", "# notes\n")]);
        let tunnel = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap();
        let before = t.core.view().panes.len();

        // The request names pane 99; the sink's pane is the one that is acted on.
        let answer = through(&tunnel, json!({"pane": 99, "cmd": "view", "path": "/srv/notes.md"}));

        assert_eq!(answer, json!({"ok": true, "message": ""}));
        let view = last_layout(&t.rec).expect("the window is told the new layout");
        assert_eq!(view.panes.len(), before + 1, "a pane was added beside the pane");
        let block = t.core.blocks.block_of(view.focus).expect("the new pane shows a block");
        assert_eq!(block.kind, BlockKind::Preview);
        assert_eq!((block.source.as_str(), block.path.as_str()), ("web-1", "/srv/notes.md"));
        assert_eq!(t.host.seen(), ["/srv/notes.md"], "the host's own source was asked, once");
        assert!(t.core.blocks.block_of(t.remote).is_none() && t.core.blocks.block_of(99).is_none());
    }

    #[test]
    fn setmeta_and_ping_work_and_the_title_is_the_panes_own() {
        let t = ticked(&[]);
        let tunnel = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap();
        assert_eq!(through(&tunnel, json!({"pane": 99, "cmd": "ping"})), json!({"ok": true, "message": "holzWave"}));
        let answer = through(&tunnel, json!({"pane": t.local, "cmd": "setmeta", "key": "title", "value": "build"}));
        assert_eq!(answer["ok"], json!(true), "{answer}");
        let names = t.core.view().pane_names;
        assert!(names.contains(&(t.remote, "build".to_owned())), "{names:?}");
        assert!(!names.iter().any(|(pane, _)| *pane == t.local), "the pane the request named keeps its name: {names:?}");
        assert!(last_layout(&t.rec).is_some(), "the window is told");
    }

    #[test]
    fn a_path_that_is_not_a_regular_file_on_the_host_is_refused() {
        let t = ticked(&[("/srv/a.txt", "a")]);
        let tunnel = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap();
        let before = t.core.view().panes.len();
        for path in ["/srv", "/missing", "relative/a.txt", "a.txt", "/dev/null"] {
            let answer = through(&tunnel, json!({"cmd": "view", "path": path}));
            assert_eq!(answer["ok"], json!(false), "{path}: {answer}");
            assert!(answer["message"].as_str().is_some_and(|m| m.len() > 8), "{path}: words: {answer}");
        }
        assert_eq!(t.core.view().panes.len(), before, "nothing was opened");
        assert!(t.core.blocks.views(&t.core.connections).is_empty());
        let asked = t.host.seen();
        assert!(!asked.iter().any(|p| !p.starts_with('/')), "a relative path was never asked of the host: {asked:?}");
        assert!(t.core.view().pane_names.is_empty());
    }

    #[test]
    fn a_preview_at_the_pane_limit_is_refused_and_no_pane_is_turned_into_it() {
        let (core, _rec, _dir) = core("/bin/cat", &[]);
        // Splitting the pane in focus again and again ends at a limit of the layout (its depth, before its pane count).
        loop {
            let before = core.view().panes.len();
            if core.split(true).panes.len() == before {
                break;
            }
        }
        let view = core.view();
        let panes = view.panes.len();
        assert!(panes > 8, "some panes were made: {panes}");

        let refused = core.block_open_in(MAIN_WINDOW, "right", BlockKind::Preview, "local", "/tmp/a.txt");

        assert!(refused.is_err(), "there is no room for another pane: {refused:?}");
        assert!(core.blocks.block_of(view.focus).is_none(), "the pane in focus was not turned into the preview");
        assert!(core.blocks.views(&core.connections).is_empty());
        assert_eq!(core.view().panes.len(), panes);
    }

    #[test]
    fn wsh_edit_sets_the_flag_and_wsh_view_does_not() {
        use holzwave_ipc::Cmd;
        // This computer: the local socket's path.
        let (core, _rec, dir) = core("/bin/cat", &[]);
        let file = dir.0.join("notes.md");
        std::fs::write(&file, "# notes\n").unwrap();
        let path = file.to_string_lossy().into_owned();
        let pane = only_pane(&core);
        let opened = |core: &AppCore| core.blocks.block_of(core.view().focus).expect("the new pane shows a block");

        assert_eq!(core.ipc_command(Some(pane), Cmd::Edit { path: path.clone() }), Ok(String::new()));
        let edit = opened(&core);
        assert_eq!((edit.kind, edit.source.as_str(), edit.path.as_str(), edit.edit), (BlockKind::Preview, LOCAL, path.as_str(), true));
        assert_ne!(core.view().focus, pane, "beside the pane, as before");

        assert_eq!(core.ipc_command(Some(pane), Cmd::View { path: path.clone() }), Ok(String::new()));
        assert!(!opened(&core).edit, "wsh view shows the page");

        // A host over a tunnel: the same two commands through the forwarded socket.
        let t = ticked(&[("/srv/notes.md", "# notes\n")]);
        let tunnel = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap();
        let answer = through(&tunnel, json!({"cmd": "edit", "path": "/srv/notes.md"}));
        assert_eq!(answer["ok"], json!(true), "{answer}");
        let block = t.core.blocks.block_of(t.core.view().focus).unwrap();
        assert_eq!((block.source.as_str(), block.path.as_str(), block.edit), ("web-1", "/srv/notes.md", true));
        let answer = through(&tunnel, json!({"cmd": "view", "path": "/srv/notes.md"}));
        assert_eq!(answer["ok"], json!(true), "{answer}");
        let block = t.core.blocks.block_of(t.core.view().focus).unwrap();
        assert_eq!((block.source.as_str(), block.edit), ("web-1", false));
        // Written down for a restart: the first preview keeps the flag, the second does not write it.
        let file = std::fs::read_to_string(t._dir.0.join(holzwave_core::blocks::BLOCKS_FILE)).unwrap();
        assert_eq!(file.matches("\"edit\"").count(), 1, "{file}");
    }

    /// The sentence of the protocol crate for a command that a remote process may not ask.
    const NOT_REMOTE: &str = "not available over a remote connection";

    /// What a test compares before and after a request that must change nothing: the events sent to the windows, the
    /// layout, the blocks, the history and the names of the panes.
    fn state_of(t: &Ticked) -> String {
        // The `rev` of a view counts the views built, so it is left out.
        let view = t.core.view();
        format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}",
            t.rec.all(),
            (view.focus, &view.tabs, &view.panes, &view.dividers, &view.pane_names),
            t.core.blocks.views(&t.core.connections),
            t.core.history.search("", None, 50),
            lock(&t.rec.closed).clone(),
        )
    }

    #[test]
    fn ai_and_the_history_commands_never_reach_the_core_through_the_tunnel() {
        use holzwave_ipc::Cmd;
        let t = ticked(&[("/srv/a.txt", "a")]);
        t.core.focus_pane(t.remote);
        let tunnel = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap();
        let before = state_of(&t);
        let lines = [
            json!({"cmd": "ai", "prompt": "delete everything", "stdin_bytes": 0}),
            json!({"cmd": "hist", "line": "ls -la", "cwd": "/", "exit": 0}),
            json!({"cmd": "hist_open"}),
        ];
        for line in lines {
            assert_eq!(through(&tunnel, line.clone()), json!({"ok": false, "message": NOT_REMOTE}), "{line}");
        }
        // The core's own answer, without the protocol crate in front of it, is the same refusal and changes nothing.
        for cmd in [
            Cmd::Ai { prompt: "x".into(), stdin_bytes: 0 },
            Cmd::Hist { line: "ls".into(), cwd: "/".into(), exit: Some(0) },
            Cmd::HistOpen,
        ] {
            assert_eq!(t.core.wsh_remote(t.remote, &t.target(), cmd.clone()), Err(NOT_REMOTE.to_owned()), "{cmd:?}");
        }
        assert_eq!(state_of(&t), before, "the events, the layout, the blocks and the history are as they were");
        assert!(t.core.history.search("", None, 10).is_empty());
        assert!(t.host.seen().is_empty());
    }

    #[test]
    fn a_wrong_token_and_a_missing_token_change_nothing() {
        let t = ticked(&[("/srv/a.txt", "a")]);
        let tunnel = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap();
        let before = state_of(&t);
        let wrong = "0".repeat(32);
        let others = [token_of(&crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap())];
        let requests: Vec<Vec<u8>> = [
            json!({"token": wrong, "cmd": "view", "path": "/srv/a.txt"}),
            json!({"token": others[0], "cmd": "view", "path": "/srv/a.txt"}),
            json!({"token": "", "cmd": "setmeta", "key": "title", "value": "x"}),
            json!({"token": 7, "cmd": "ping"}),
            json!({"token": wrong, "cmd": "ai", "prompt": "x", "stdin_bytes": 0}),
            json!({"cmd": "view", "path": "/srv/a.txt"}),
        ]
        .iter()
        .map(|request| serde_json::to_vec(request).unwrap())
        .collect();
        for request in requests {
            let answer = through_raw(&tunnel, &request);
            assert_eq!(answer["ok"], json!(false), "{answer}");
            assert!(answer["message"].as_str().unwrap().starts_with("refused:"), "the protocol's sentence: {answer}");
        }
        assert_eq!(state_of(&t), before);
        assert!(t.host.seen().is_empty(), "the host was never asked");
    }

    #[test]
    fn a_request_that_names_another_pane_is_answered_for_the_sinks_pane() {
        let t = ticked(&[]);
        t.core.rename_pane(t.local, "keep");
        t.core.rename_pane(t.unticked, "mine");
        let tunnel = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap();
        for named in [json!(t.local), json!(t.unticked), json!(7), json!(null)] {
            let answer = through(&tunnel, json!({"pane": named, "cmd": "setmeta", "key": "title", "value": "renamed"}));
            assert_eq!(answer["ok"], json!(true), "{named}: {answer}");
        }
        let names: HashMap<PaneId, String> = t.core.view().pane_names.into_iter().collect();
        assert_eq!(names.get(&t.remote).map(String::as_str), Some("renamed"), "the sink's pane");
        assert_eq!(names.get(&t.local).map(String::as_str), Some("keep"));
        assert_eq!(names.get(&t.unticked).map(String::as_str), Some("mine"));
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn the_twenty_first_request_in_ten_seconds_is_busy() {
        let t = ticked(&[]);
        let second = t.core.split(true).focus;
        t.core.connections.assign(second, "web-1").unwrap();
        let (one, two) = (crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap(), crate::wsh_tunnel::tunnel_for(&t.core, second).unwrap());
        for n in 1..=20 {
            assert_eq!(through(&one, json!({"cmd": "ping"}))["ok"], json!(true), "request {n}");
        }
        assert_eq!(through(&one, json!({"cmd": "ping"})), json!({"ok": false, "message": "busy"}));
        assert_eq!(through(&one, json!({"cmd": "view", "path": "/srv/a.txt"}))["message"], json!("busy"));
        assert_eq!(through(&two, json!({"cmd": "ping"})), json!({"ok": true, "message": "holzWave"}), "another pane has its own budget");
    }

    #[test]
    fn a_pane_that_closed_cannot_be_driven() {
        let t = ticked(&[("/srv/a.txt", "a")]);
        let closing = t.core.split(true).focus;
        t.core.connections.assign(closing, "web-1").unwrap();
        let (open, closed) = (crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap(), crate::wsh_tunnel::tunnel_for(&t.core, closing).unwrap());
        t.core.close_pane(closing);
        let before = t.core.view().panes.len();
        let answer = through(&closed, json!({"cmd": "view", "path": "/srv/a.txt"}));
        assert_eq!(answer, json!({"ok": false, "message": "this pane is not open any more"}));
        assert_eq!(through(&closed, json!({"cmd": "ping"}))["ok"], json!(false));
        assert_eq!(t.core.view().panes.len(), before);
        assert!(t.core.blocks.views(&t.core.connections).is_empty() && t.host.seen().is_empty());

        // The pane is open but its connection was turned off in the saved list: the tunnel that was made for it answers no more.
        let mut profile = t.core.connections.profile_of(t.remote).unwrap();
        profile.wsh = false;
        t.core.connection_save(profile, SecretChange::Keep).unwrap();
        let answer = through(&open, json!({"cmd": "view", "path": "/srv/a.txt"}));
        assert_eq!(answer, json!({"ok": false, "message": "wsh is not turned on for the connection of this pane"}));
        assert!(t.core.blocks.views(&t.core.connections).is_empty());

        // And deleted.
        t.core.connection_delete("web-1").unwrap();
        let answer = through(&open, json!({"cmd": "view", "path": "/srv/a.txt"}));
        assert_eq!(answer, json!({"ok": false, "message": "this pane no longer runs a saved connection"}));
        assert!(t.core.blocks.views(&t.core.connections).is_empty());
    }

    #[test]
    fn a_directory_a_device_and_a_huge_path_are_refused() {
        let t = ticked(&[("/srv/a.txt", "a")]);
        let tunnel = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap();
        let before = state_of(&t);
        let huge = format!("/{}", "a".repeat(4096));
        assert_eq!(huge.len(), 4097);
        for (path, said) in [
            ("/srv", "not a regular file on this host"),
            ("/dev/null", "not a regular file on this host"),
            (huge.as_str(), "the path is empty, too long or has a NUL"),
            ("/srv/a\u{0}.txt", "the path is empty, too long or has a NUL"),
            ("", "the path is empty, too long or has a NUL"),
            ("../srv/a.txt", "the path must be absolute: it starts with /"),
        ] {
            for cmd in ["view", "edit"] {
                let answer = through(&tunnel, json!({"cmd": cmd, "path": path}));
                assert_eq!(answer, json!({"ok": false, "message": said}), "{cmd} {path:?}");
            }
        }
        assert_eq!(state_of(&t), before, "nothing was opened");
        assert_eq!(t.host.seen(), ["/srv", "/srv", "/dev/null", "/dev/null"], "only the two that could be asked were asked");
    }

    /// Fails on any use: the files of this computer.
    struct NoLocalFiles;

    impl FileSource for NoLocalFiles {
        fn label(&self) -> String {
            panic!("this computer's files were asked");
        }
        fn home(&self) -> holzwave_core::Result<String> {
            panic!("this computer's files were asked");
        }
        fn list(&self, _: &str) -> holzwave_core::Result<Listing> {
            panic!("this computer's files were asked");
        }
        fn stat(&self, _: &str) -> holzwave_core::Result<Entry> {
            panic!("this computer's files were asked");
        }
        fn read(&self, _: &str, _: u64) -> holzwave_core::Result<Vec<u8>> {
            panic!("this computer's files were asked");
        }
        fn read_range(&self, _: &str, _: u64, _: u64) -> holzwave_core::Result<Vec<u8>> {
            panic!("this computer's files were asked");
        }
        fn write(&self, _: &str, _: &[u8]) -> holzwave_core::Result<()> {
            panic!("this computer's files were asked");
        }
    }

    #[test]
    fn a_remote_view_never_uses_the_local_source() {
        let t = ticked(&[("/srv/a.txt", "a")]);
        t.core.blocks.inject_local(Arc::new(NoLocalFiles));
        let tunnel = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap();
        // A path that exists on this computer (and not on the host) is refused by the host, and this computer is never asked.
        let here = t._dir.0.join("here.txt");
        std::fs::write(&here, "local").unwrap();
        let answer = through(&tunnel, json!({"cmd": "view", "path": here.to_string_lossy()}));
        assert_eq!(answer, json!({"ok": false, "message": "no such file on this host, or it cannot be read"}));
        for cmd in ["view", "edit"] {
            assert_eq!(through(&tunnel, json!({"cmd": cmd, "path": "/srv/a.txt"}))["ok"], json!(true), "{cmd}");
        }
        let block = t.core.blocks.block_of(t.core.view().focus).unwrap();
        assert_eq!(block.source, "web-1");
        // The double is really the local source: a local pane's request reaches it.
        let asked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.core.blocks.stat(&t.core.connections, "local", "/x")));
        assert!(asked.is_err(), "the double stands in for this computer");
    }

    #[test]
    fn the_commands_the_remote_scope_accepts_are_exactly_the_four_this_function_handles() {
        use holzwave_ipc::Cmd;
        // A `match` with no wildcard: a command added to the enum does not compile until it is listed here too.
        fn index(cmd: &Cmd) -> usize {
            match cmd {
                Cmd::View { .. } => 0,
                Cmd::Edit { .. } => 1,
                Cmd::Setmeta { .. } => 2,
                Cmd::Hist { .. } => 3,
                Cmd::HistOpen => 4,
                Cmd::Ai { .. } => 5,
                Cmd::Ping => 6,
            }
        }
        let t = ticked(&[("/srv/a.txt", "a")]);
        let every = [
            Cmd::View { path: "/srv/a.txt".into() },
            Cmd::Edit { path: "/srv/a.txt".into() },
            Cmd::Setmeta { key: "title".into(), value: "x".into() },
            Cmd::Hist { line: "ls".into(), cwd: "/".into(), exit: None },
            Cmd::HistOpen,
            Cmd::Ai { prompt: "x".into(), stdin_bytes: 0 },
            Cmd::Ping,
        ];
        assert_eq!(every.iter().map(index).collect::<Vec<_>>(), (0..7).collect::<Vec<_>>(), "every command is in the table once");
        let mut accepted = Vec::new();
        let mut handled = Vec::new();
        for cmd in &every {
            let name = serde_json::to_value(cmd).unwrap()["cmd"].as_str().unwrap().to_owned();
            if cmd.allowed_remote() {
                accepted.push(name.clone());
            }
            if t.core.wsh_remote(t.remote, &t.target(), cmd.clone()) != Err(NOT_REMOTE.to_owned()) {
                handled.push(name);
            }
        }
        assert_eq!(accepted, handled, "what the scope lets in is what the core answers, and nothing more");
        assert_eq!(accepted, ["view", "edit", "setmeta", "ping"]);
        // None of the four decides anything for the person.
        for name in &accepted {
            for word in ["decide", "decision", "approve", "approval", "consent", "grant", "allow", "deny", "confirm", "accept", "permit", "authorize", "trust"] {
                assert!(!name.contains(word), "{name} says {word}");
            }
        }
    }

    #[test]
    fn the_token_and_the_path_never_appear_in_any_text() {
        let t = ticked(&[("/srv/a.txt", "a")]);
        let tunnel = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap();
        let (token, path) = (token_of(&tunnel), tunnel.path.clone());
        let sink = crate::wsh_tunnel::WshSink::new(t.core.downgrade(), t.remote, token.clone(), t.target());
        let mut spec = PaneSpec::new("sh", 80, 24);
        t.core.apply_connection(t.remote, &mut spec);
        let spec_tunnel = spec.tunnel.clone().expect("a tunnel");
        let mut seen = vec![format!("{sink:?}"), format!("{tunnel:?}"), format!("{spec:?}"), format!("{:?}", spec.notice)];

        // Every answer, whatever the request, and every event the core sent.
        for request in [
            json!({"cmd": "ping"}),
            json!({"cmd": "view", "path": "/srv/a.txt"}),
            json!({"cmd": "edit", "path": "/srv/missing"}),
            json!({"cmd": "view", "path": "/srv"}),
            json!({"cmd": "setmeta", "key": "title", "value": "x"}),
            json!({"cmd": "setmeta", "key": "colour", "value": "x"}),
            json!({"cmd": "ai", "prompt": "x", "stdin_bytes": 0}),
            json!({"token": "0".repeat(32), "cmd": "ping"}),
        ] {
            seen.push(through(&tunnel, request).to_string());
        }
        seen.push(format!("{:?}", t.rec.all()));
        seen.push(format!("{:?}", t.core.view()));
        seen.push(format!("{:?}", t.core.blocks.views(&t.core.connections)));
        assert!(seen.len() > 10);
        let tokens = [token, token_of(&spec_tunnel)];
        let paths = [path, spec_tunnel.path.clone()];
        for text in &seen {
            for secret in tokens.iter().chain(&paths) {
                assert!(!text.contains(secret.as_str()), "a secret is in {text}");
            }
            assert!(!text.contains("holzwave-") || text.contains("holzwave-test"), "a socket path is in {text}");
        }
    }

    /// Edit the saved connection of the ticked pane (`web-1`), the rest as it was.
    fn edit_connection(t: &Ticked, change: impl FnOnce(&mut Profile)) {
        let mut profile = t.core.connections.profile_of(t.remote).expect("the connection is saved");
        change(&mut profile);
        t.core.connections.save(profile, SecretChange::Keep).expect("saves");
    }

    #[test]
    fn a_request_from_a_pane_of_an_edited_connection_is_refused_and_the_host_is_never_asked() {
        let t = ticked(&[("/srv/a.txt", "a")]);
        let tunnel = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap();
        assert_eq!(through(&tunnel, json!({"cmd": "ping"}))["ok"], true);

        // Renaming the connection moves nothing: the host, the port and the user are the target.
        let original = t.core.connections.profile_of(t.remote).unwrap();
        edit_connection(&t, |p| p.name = "Renamed".into());
        assert_eq!(through(&tunnel, json!({"cmd": "ping"}))["ok"], true, "a new name is the same host");

        for (what, change) in [
            ("host", Box::new(|p: &mut Profile| p.host = "other.example.invalid".into()) as Box<dyn Fn(&mut Profile)>),
            ("port", Box::new(|p: &mut Profile| p.port = 2222)),
            ("user", Box::new(|p: &mut Profile| p.user = "someone-else".into())),
        ] {
            edit_connection(&t, |p| *p = original.clone());
            let before = lock(&t.host.seen).len();
            edit_connection(&t, |p| change(p));
            for request in [
                json!({"cmd": "ping"}),
                json!({"cmd": "view", "path": "/srv/a.txt"}),
                json!({"cmd": "edit", "path": "/srv/a.txt"}),
                json!({"cmd": "setmeta", "key": "title", "value": "x"}),
            ] {
                let answer = through(&tunnel, request.clone());
                assert_eq!(answer, json!({"ok": false, "message": WSH_TARGET_CHANGED}), "{what}: {request}");
            }
            assert_eq!(lock(&t.host.seen).len(), before, "{what}: nothing was asked of any host");
            assert!(t.core.blocks.views(&t.core.connections).is_empty(), "{what}: no preview opened");
            // The sentence is fixed: it names no host, no user and no path.
            let text = WSH_TARGET_CHANGED;
            assert!(!text.contains("example") && !text.contains("someone"), "{text}");
        }

        // A pane that starts now records where the connection points now, and is answered.
        let fresh = crate::wsh_tunnel::tunnel_for(&t.core, t.remote).unwrap();
        assert_eq!(through(&fresh, json!({"cmd": "ping"}))["ok"], true);
        // The old tunnel stays refused until the connection points where it did.
        assert_eq!(through(&tunnel, json!({"cmd": "ping"}))["ok"], false);
        edit_connection(&t, |p| {
            p.host = "web.example.invalid".into();
            p.port = 22;
            p.user = "holz".into();
        });
        assert_eq!(through(&tunnel, json!({"cmd": "ping"}))["ok"], true, "pointing back is the same target again");
    }

    #[test]
    fn the_target_is_recorded_when_the_pane_starts_and_forgotten_when_it_closes() {
        let t = ticked(&[]);
        let mut spec = PaneSpec::new("sh", 80, 24);
        t.core.apply_connection(t.remote, &mut spec);
        assert!(lock(&t.core.pane_targets).contains_key(&t.remote));
        // A pane that runs a local shell, or a connection that is not SSH, records nothing.
        let mut local = PaneSpec::new("sh", 80, 24);
        t.core.apply_connection(t.local, &mut local);
        assert!(!lock(&t.core.pane_targets).contains_key(&t.local));
        let debug = format!("{:?}", t.target());
        assert!(!debug.contains("web.example") && !debug.contains("holz"), "{debug}");
        t.core.forget_pane(t.remote);
        assert!(!lock(&t.core.pane_targets).contains_key(&t.remote));
    }

    /// Start the pane `pane` of `t` and focus it, as the person's act would find it.
    fn started_and_focused(t: &Ticked, pane: PaneId) {
        t.core.start_pane(pane, 80, 24, 0, 0).expect("the pane starts");
        t.core.focus_pane_in(MAIN_WINDOW, pane).expect("focus");
    }

    #[cfg(unix)]
    fn says(t: &Ticked, pane: PaneId, words: &str) -> bool {
        wait_until(WAIT, || t.rec.has_text(pane, words))
    }

    #[test]
    #[cfg(unix)]
    fn an_install_on_a_pane_that_is_not_ssh_says_so_on_the_pane() {
        let t = ticked(&[]);
        // A local pane.
        started_and_focused(&t, t.local);
        assert_eq!(t.core.wsh_install(t.local), Err(INSTALL_NOT_SSH.to_owned()));
        assert!(says(&t, t.local, "does not run a saved SSH"), "the sentence is on the pane: {:?}", t.rec.all());

        // A connection assigned to a pane that already runs a local shell takes effect when it starts: not SSH yet.
        t.core.connections.assign(t.local, "web-1").unwrap();
        assert_eq!(t.core.wsh_install(t.local), Err(INSTALL_NOT_SSH.to_owned()));

        // A Docker connection on a running pane.
        let mut docker = Profile::new("api", "API", "api-1");
        docker.kind = ConnectionKind::Docker;
        t.core.connections.save(docker, SecretChange::Keep).unwrap();
        let pane = t.core.split(true).focus;
        started_and_focused(&t, pane);
        t.core.connections.assign(pane, "api").unwrap();
        assert_eq!(t.core.wsh_install(pane), Err(INSTALL_NOT_SSH.to_owned()));
        assert!(says(&t, pane, "does not run a saved SSH"));

        // A block pane has no screen, and a pane that is not open is not running.
        let block = t.core.split(true).focus;
        t.core.blocks.assign(block, Block { kind: BlockKind::Preview, source: "web-1".into(), path: "/srv/a".into(), edit: false }).unwrap();
        assert_eq!(t.core.wsh_install(block), Err(INSTALL_NOT_SSH.to_owned()));
        assert_eq!(t.core.wsh_install(9999), Err(INSTALL_NO_PANE.to_owned()));
        // An SSH pane that has not started yet is not running either.
        assert_eq!(t.core.wsh_install(t.unticked), Err(INSTALL_NO_PANE.to_owned()));
        // Nothing was recorded for any of them: no connection was made up.
        assert!(lock(&t.core.pane_targets).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn an_install_only_acts_for_the_pane_in_focus() {
        let t = ticked(&[]);
        started_and_focused(&t, t.remote);
        started_and_focused(&t, t.unticked);
        // `unticked` is in focus now: the install for `remote` is refused, on its own screen.
        assert_eq!(t.core.wsh_install(t.remote), Err(INSTALL_NOT_FOCUSED.to_owned()));
        assert!(says(&t, t.remote, "not the pane in focus"));
    }

    #[test]
    #[cfg(unix)]
    fn an_install_for_a_connection_edited_to_another_host_is_refused() {
        let t = ticked(&[]);
        started_and_focused(&t, t.remote);
        assert!(lock(&t.core.pane_targets).contains_key(&t.remote), "recorded when the pane started");
        edit_connection(&t, |p| p.host = "other.example.invalid".into());
        assert_eq!(t.core.wsh_install(t.remote), Err(INSTALL_TARGET_CHANGED.to_owned()));
        assert!(says(&t, t.remote, "now points to"), "{:?}", t.rec.all());
        let text = INSTALL_TARGET_CHANGED;
        assert!(!text.contains("example") && !text.contains("holz"), "{text}");
    }

    #[test]
    fn the_install_sentences_name_the_connection_and_nothing_else() {
        use holzwave_core::wshinstall::Installed;
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(install_sentence(&Ok(Installed::Installed("/home/holz/.holzwave/bin/wsh".into())), "Web 1"), format!("wsh {version} was installed on Web 1"));
        assert_eq!(install_sentence(&Ok(Installed::AlreadyCurrent("/x".into())), "Web 1"), format!("wsh {version} is already installed on Web 1"));
        assert_eq!(install_sentence(&Err("wsh was not installed: because".into()), "Web 1"), "wsh was not installed: because");
        // A name with control characters cannot be an escape sequence.
        let sentence = install_sentence(&Ok(Installed::Installed(String::new())), "A\u{1b}[2Jb\r\nc");
        assert!(!sentence.chars().any(char::is_control), "{sentence:?}");
        // No path of the host or of this computer is in a success sentence.
        assert!(!install_sentence(&Ok(Installed::Installed("/home/holz/.holzwave/bin/wsh".into())), "Web 1").contains("/home"));
    }

    #[test]
    fn an_install_without_bundled_binaries_says_so() {
        let t = ticked(&[]);
        // No resource folder at all, an empty one, and one without the `wsh` folder: nothing is bundled.
        assert!(t.core.bundled_wsh().x86_64.is_none() && t.core.bundled_wsh().aarch64.is_none());
        let res = Scratch::new("resources");
        t.core.set_resource_dir(res.0.clone());
        let none = t.core.bundled_wsh();
        assert!(none.x86_64.is_none() && none.aarch64.is_none());
        // With the files present they are read and passed on, each for its own processor.
        std::fs::create_dir(res.0.join("wsh")).unwrap();
        std::fs::write(res.0.join("wsh/linux-x86_64"), b"\x7fELFx86").unwrap();
        std::fs::write(res.0.join("wsh/linux-aarch64"), b"\x7fELFarm").unwrap();
        let both = t.core.bundled_wsh();
        assert_eq!((both.x86_64.as_deref(), both.aarch64.as_deref()), (Some(&b"\x7fELFx86"[..]), Some(&b"\x7fELFarm"[..])));
        // One of the two is enough for the host that needs it.
        std::fs::remove_file(res.0.join("wsh/linux-aarch64")).unwrap();
        let one = t.core.bundled_wsh();
        assert!(one.x86_64.is_some() && one.aarch64.is_none());
        // A file over 16 MiB is not read.
        let big = std::fs::File::create(res.0.join("wsh/linux-aarch64")).unwrap();
        big.set_len(MAX_BUNDLED_WSH + 1).unwrap();
        assert!(t.core.bundled_wsh().aarch64.is_none(), "over the bound");
        big.set_len(MAX_BUNDLED_WSH).unwrap();
        assert_eq!(t.core.bundled_wsh().aarch64.map(|b| b.len() as u64), Some(MAX_BUNDLED_WSH), "at the bound");
    }

    /// The text of every row of `pane` that any frame carried as changed, oldest first.
    #[cfg(unix)]
    fn rows_of(rec: &Recorder, pane: PaneId) -> Vec<String> {
        rec.frames(pane)
            .iter()
            .flat_map(|f| f.changed.iter().map(|row| row.runs.iter().flat_map(|r| r.0.chars()).filter(|c| *c != '\0').collect::<String>()))
            .collect()
    }

    /// The tunnel against a real OpenSSH server, with the real `wsh` binary: `scripts/ssh-loopback-check.sh` starts a
    /// throwaway `sshd` on 127.0.0.1 that allows Unix socket forwarding, builds `wsh`, and passes in where both are. The
    /// test saves a connection to it with `wsh` ticked, starts a pane, and types the real `wsh` into the real remote shell.
    ///
    /// The server runs on this computer, so the socket the forward makes is a file here, which the test can look at; and
    /// the `wsh` that runs is the macOS (or host) build by its absolute path, so nothing is installed into a real home.
    /// What it cannot prove: a Linux host, another processor, the static Linux build (Docker proves that one runs).
    #[cfg(unix)]
    #[test]
    #[ignore = "needs a real sshd and a built wsh: run scripts/ssh-loopback-check.sh"]
    fn wsh_tunnel_live() {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        fn var(name: &str) -> String {
            std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set; run this test through scripts/ssh-loopback-check.sh"))
        }
        const LIVE_WAIT: Duration = Duration::from_secs(30);
        let (host, port, user) = (var("HOLZWAVE_TEST_SSH_HOST"), var("HOLZWAVE_TEST_SSH_PORT"), var("HOLZWAVE_TEST_SSH_USER"));
        let (known, key_file, wsh) = (var("HOLZWAVE_TEST_SSH_KNOWN_HOSTS"), var("HOLZWAVE_TEST_SSH_KEY_ED25519"), var("HOLZWAVE_TEST_WSH"));
        assert!(Path::new(&wsh).is_absolute(), "HOLZWAVE_TEST_WSH names the built wsh by an absolute path");

        let dir = Scratch::new("live");
        std::fs::copy(&known, dir.0.join("known_hosts")).expect("the server's host key is the app's known host");
        let mut profiles = holzwave_core::profiles::Profiles::default();
        let mut profile = Profile::new("live-host", "Live host", host);
        profile.port = port.parse().expect("a port number");
        profile.user = user;
        profile.auth = holzwave_core::profiles::Auth::Key;
        profile.key_path = key_file;
        profile.wsh = true;
        profile.reconnect_attempts = 0;
        profiles.upsert(profile).unwrap();
        profiles.save(&dir.0).unwrap();
        let (core, rec) = core_in(&dir, "sh", &[]);
        let local = only_pane(&core);
        let remote = core.split(true).focus;
        core.connections.assign(remote, "live-host").unwrap();
        core.start_pane(remote, 100, 30, 0, 0).expect("the pane starts");
        core.focus_pane_in(MAIN_WINDOW, remote).unwrap();

        // The line as committed text, then the Enter key runs it, as a person types.
        let type_line = |line: &str| {
            core.text(remote, line.to_owned()).expect("typed");
            core.key(remote, key("Enter", "Enter")).expect("Enter");
        };
        let wait_for = |needle: &str, what: &str| {
            assert!(wait_until(LIVE_WAIT, || rec.has_text(remote, needle)), "no `{needle}` on the screen ({what}): {:?}", rows_of(&rec, remote));
        };

        // The login shell prints its prompt when it is ready; what is typed before that may be lost to its start-up files.
        assert!(
            wait_until(LIVE_WAIT, || rows_of(&rec, remote).iter().any(|row| !row.trim().is_empty())),
            "the remote shell printed nothing: {:?}",
            rows_of(&rec, remote)
        );
        std::thread::sleep(Duration::from_millis(500));
        // The remote shell runs: it does a sum. Then the variables the wrapped start gave it, by their effect only.
        type_line(&format!("W='{wsh}'"));
        type_line("printf 'L1=[%s]\\n' \"$((6*7))\"");
        wait_for("L1=[42]", "the shell runs");
        println!("live: the pane's shell runs on the real sshd");
        type_line("printf 'S1=[%s]\\n' \"$HOLZWAVE_SOCKET\"");
        type_line("printf 'I1=[%s]\\n' \"${HOLZWAVE_IPC-unset}\"");
        type_line("printf 'T1=[%s]\\n' \"${#HOLZWAVE_TOKEN}\"");
        wait_for("T1=[32]", "a token of 32 characters is set (its value is never printed)");
        wait_for("I1=[unset]", "HOLZWAVE_IPC is not set on a remote host, so the history hooks stay off");
        let socket = rows_of(&rec, remote)
            .iter()
            .find_map(|row| row.split("S1=[").nth(1).and_then(|rest| rest.split(']').next()).filter(|path| path.starts_with('/')).map(str::to_owned))
            .expect("the shell has HOLZWAVE_SOCKET");
        assert!(holzwave_core::pool::valid_forward_path(&socket), "{socket}");
        // The server made the socket (on this computer): a socket file that only its owner can reach.
        let meta = std::fs::metadata(&socket).expect("sshd made the socket while the pane runs");
        assert!(meta.file_type().is_socket(), "{socket} is a socket");
        assert_eq!(meta.permissions().mode() & 0o077, 0, "the socket is owner-only: {:o}", meta.permissions().mode());
        println!("live: the forwarded socket exists, is owner-only, and HOLZWAVE_IPC is not set");

        // ping through the real forward with the real binary.
        type_line("printf 'R1=[%s]\\n' \"$(\"$W\" ping 2>&1)\"");
        wait_for("R1=[holzWave]", "wsh ping through the tunnel");
        println!("live: wsh ping answers through the forward");

        // setmeta renames this pane.
        type_line("\"$W\" setmeta title live-ok");
        assert!(wait_until(LIVE_WAIT, || core.view().pane_names.contains(&(remote, "live-ok".to_owned()))), "{:?}", core.view().pane_names);
        assert!(!core.view().pane_names.iter().any(|(pane, _)| *pane == local), "no other pane was renamed");
        println!("live: wsh setmeta title renames the pane");

        // view and edit of a file of that host, over the real SFTP of the real server.
        let file = dir.0.join("note.md");
        std::fs::write(&file, "# live\n\nhello\n").unwrap();
        let file_text = file.to_string_lossy().into_owned();
        type_line(&format!("\"$W\" view '{file_text}'"));
        let open = |edit: bool| {
            core.blocks.views(&core.connections).into_iter().any(|v| {
                v.block.kind == BlockKind::Preview && v.block.source == "live-host" && v.block.path == file_text && v.block.edit == edit
            })
        };
        assert!(wait_until(LIVE_WAIT, || open(false)), "no preview of the host's file: {:?}", core.blocks.views(&core.connections));
        type_line(&format!("\"$W\" edit '{file_text}'"));
        assert!(wait_until(LIVE_WAIT, || open(true)), "no editor of the host's file: {:?}", core.blocks.views(&core.connections));
        println!("live: wsh view and wsh edit open the host's file beside the pane");

        // What a remote process must never do: the AI, the history and a wrong token.
        let history_before = core.history_search("", 50).len();
        type_line("printf 'A1=[%s]\\n' \"$(\"$W\" ai hello 2>&1)\"");
        wait_for("A1=[wsh: not available over a remote connection]", "wsh ai is refused");
        type_line("printf 'H1=[%s]\\n' \"$(\"$W\" hist add 0 /tmp pwned 2>&1)\"");
        wait_for("H1=[wsh: not available over a remote connection]", "wsh hist add is refused");
        type_line("printf 'X1=[%s]\\n' \"$(HOLZWAVE_TOKEN=wrong \"$W\" ping 2>&1)\"");
        wait_for("X1=[wsh: refused", "a wrong token is refused");
        assert_eq!(core.history_search("", 50).len(), history_before, "nothing reached the history");
        assert!(!core.history_search("pwned", 50).iter().any(|_| true), "the refused history command was not stored");
        println!("live: wsh ai, wsh hist add and a wrong token are refused");

        // Closing the pane ends the forward: nothing listens on the socket any more, so a process on the host can reach nothing
        // through it. The file may stay behind (this OpenSSH does not unlink it); the next pane gets a new path.
        core.close_pane_in(MAIN_WINDOW, remote).unwrap();
        assert!(
            wait_until(LIVE_WAIT, || std::os::unix::net::UnixStream::connect(&socket).is_err()),
            "the socket still accepts connections after the pane closed: {socket}"
        );
        let left = Path::new(&socket).exists();
        println!("live: nothing listens on the socket after the pane closed (the file {})", if left { "stays behind" } else { "is gone" });
        if left {
            let _ = std::fs::remove_file(&socket);
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_resource_files_are_read_only_from_the_resource_folder() {
        use std::os::unix::fs::symlink;
        let t = ticked(&[]);
        let res = Scratch::new("resources-links");
        let elsewhere = Scratch::new("elsewhere");
        std::fs::write(elsewhere.0.join("secret"), b"\x7fELFnot-a-resource").unwrap();
        t.core.set_resource_dir(res.0.clone());
        std::fs::create_dir(res.0.join("wsh")).unwrap();
        // A link in place of a file is not followed.
        symlink(elsewhere.0.join("secret"), res.0.join("wsh/linux-x86_64")).unwrap();
        // A directory in place of a file is not a regular file.
        std::fs::create_dir(res.0.join("wsh/linux-aarch64")).unwrap();
        let bundled = t.core.bundled_wsh();
        assert!(bundled.x86_64.is_none() && bundled.aarch64.is_none());
        // A link in place of the folder sends neither read elsewhere.
        std::fs::remove_dir_all(res.0.join("wsh")).unwrap();
        std::fs::write(elsewhere.0.join("linux-x86_64"), b"\x7fELFelsewhere").unwrap();
        symlink(&elsewhere.0, res.0.join("wsh")).unwrap();
        let bundled = t.core.bundled_wsh();
        assert!(bundled.x86_64.is_none() && bundled.aarch64.is_none(), "{:?}", bundled);
        // The two names are fixed: nothing in this function reads a name that came from anywhere else.
        let source = include_str!("state.rs");
        assert_eq!(source.matches("\"linux-x86_64\"").count(), 2, "the two fixed names appear in the reader and in this test only");
    }

    // ---- terminal launches ----------------------------------------------------------------------------------------

    /// A fake tool: it records its environment next to itself (`<name>.env`, NUL-terminated `NAME=VALUE` entries, put in
    /// place whole), prints one line and exits with `exit`.
    #[cfg(unix)]
    fn fake_tool(dir: &Scratch, name: &str, exit: i32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let env = dir.0.join(format!("{name}.env"));
        let script = format!(
            "#!/bin/sh\n/usr/bin/env -0 > \"{env}.tmp\" && mv \"{env}.tmp\" \"{env}\"\nprintf 'tool says no\\n'\nexit {exit}\n",
            env = env.display()
        );
        let path = dir.0.join(name);
        std::fs::write(&path, script).expect("write the tool");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("make the tool executable");
        path
    }

    /// The environment the fake tool `name` recorded, once it has.
    #[cfg(unix)]
    fn recorded_env(dir: &Scratch, name: &str) -> std::collections::BTreeMap<String, String> {
        let file = dir.0.join(format!("{name}.env"));
        // The first start of a new executable can take a while on a Mac whose system policy daemon is busy.
        assert!(wait_until(Duration::from_secs(60), || file.is_file()), "the tool did not run");
        std::fs::read(&file)
            .expect("the record")
            .split(|b| *b == 0)
            .filter(|entry| !entry.is_empty())
            .filter_map(|entry| {
                let text = String::from_utf8_lossy(entry).into_owned();
                text.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned()))
            })
            .collect()
    }

    /// A launch of `program` the way the service builds one: a `PATH` of its own, the inherited names it must lose
    /// (the app's own and the loader's that this process has) and `HOME` to prove a removal.
    #[cfg(unix)]
    fn tool_launch(program: PathBuf, label: &str) -> PaneLaunch {
        let mut env_remove = EnvPolicy::removals(std::env::vars_os().map(|(name, _)| name), Os::current());
        env_remove.push("HOME".into());
        PaneLaunch {
            program,
            args: Vec::new(),
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            env_remove,
            cwd: None,
            label: label.into(),
            secrets: Vec::new(),
        }
    }

    /// What `pane_launches.json` holds now (nothing when there is no file).
    fn launch_file(dir: &Scratch) -> std::collections::BTreeMap<String, String> {
        match std::fs::read(dir.0.join("pane_launches.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).expect("the markers file is a map of strings"),
            Err(_) => std::collections::BTreeMap::new(),
        }
    }

    #[cfg(unix)]
    fn enter_key() -> DomKey {
        DomKey { code: "Enter".into(), key: "Enter".into(), mods: Mods::NONE, release: false, ..DomKey::default() }
    }

    #[cfg(unix)]
    #[test]
    fn started_launch_has_no_socket_token_and_no_wsh_path() {
        let (core, rec, dir) = sh("sleep 5");
        // The core has a socket and a token, which is what every ordinary pane gets in its environment.
        core.set_ipc(dir.0.clone(), "app-token".into());
        let pane = only_pane(&core);
        let tool = fake_tool(&dir, "kubectl", 1);
        core.register_launch(pane, tool_launch(tool, "Exec in pod api-0")).expect("the launch is registered");

        core.start_pane(pane, 80, 24, 0, 0).expect("the pane starts");

        let seen = recorded_env(&dir, "kubectl");
        let names: Vec<&String> = seen.keys().collect();
        for name in ["HOLZWAVE_IPC", "HOLZWAVE_TOKEN", "HOLZWAVE_PANE"] {
            assert!(!seen.contains_key(name), "{name} reached the tool: {names:?}");
        }
        assert!(!names.iter().any(|name| name.starts_with("HOLZWAVE_")), "{names:?}");
        assert_eq!(seen.get("PATH").map(String::as_str), Some("/usr/bin:/bin"), "the launch's PATH, with no wsh folder");
        assert!(!seen.contains_key("HOME"), "the launch removed HOME");
        // A lingering pane: the failing tool stays on the screen with its words and is recorded as held.
        assert!(wait_until(WAIT, || core.held_panes() == [(pane, Some(1))]), "not held: {:?}", core.held_panes());
        assert!(wait_until(WAIT, || rec.has_text(pane, "tool says no")), "no output: {:?}", rec.all());
        assert!(rec.pane_exits().is_empty(), "a held pane does not end by itself: {:?}", rec.pane_exits());
    }

    #[cfg(unix)]
    #[test]
    fn a_launch_is_consumed_by_its_pane_start() {
        let (core, _rec, dir) = sh("sleep 5");
        let pane = only_pane(&core);
        let tool = fake_tool(&dir, "kubectl", 1);

        assert!(core.register_launch(9_999, tool_launch(tool.clone(), "x")).is_err(), "a pane that is in no layout");
        assert!(lock(&core.launches).is_empty() && launch_file(&dir).is_empty(), "nothing was kept for it");
        core.register_launch(pane, tool_launch(tool.clone(), "x")).expect("registered");
        assert_eq!(lock(&core.launches).len(), 1);

        core.start_pane(pane, 80, 24, 0, 0).expect("starts");
        assert!(lock(&core.launches).is_empty(), "the start consumed the entry");
        let _ = recorded_env(&dir, "kubectl");
        std::fs::remove_file(dir.0.join("kubectl.env")).unwrap();

        // The second start changes nothing: the pane has its slot, and the tool is not run again.
        core.start_pane(pane, 80, 24, 0, 0).expect("a second start is not an error");
        std::thread::sleep(Duration::from_millis(400));
        assert!(!dir.0.join("kubectl.env").exists(), "the tool was run a second time");
        assert!(
            core.register_launch(pane, tool_launch(tool, "again")).is_err(),
            "a pane that has started takes no launch: it would never be used"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_label_marker_is_written_cleaned_and_removed() {
        let (core, _rec, dir) = sh("sleep 5");
        let first = only_pane(&core);
        let second = core.split(true).focus;
        let tool = fake_tool(&dir, "kubectl", 1);
        assert!(launch_file(&dir).is_empty() && !dir.0.join("pane_launches.json").exists());

        // Control characters go, and the label is cut to 64 characters.
        let label = format!("Exec\u{7} in\u{1b}[31m pod {}", "x".repeat(100));
        core.register_launch(first, tool_launch(tool.clone(), &label)).expect("registered");
        core.register_launch(second, tool_launch(tool, "Logs")).expect("registered");

        let marks = launch_file(&dir);
        assert_eq!(marks.len(), 2, "{marks:?}");
        assert_eq!(marks[&first.to_string()], format!("Exec in[31m pod {}", "x".repeat(48)));
        assert_eq!(marks[&first.to_string()].chars().count(), 64);
        assert_eq!(marks[&second.to_string()], "Logs");
        assert!(std::fs::metadata(dir.0.join("pane_launches.json")).unwrap().len() <= 16 * 1024);

        // A pane that leaves the layout takes its launch and its marker with it, started or not.
        core.close_pane_in(MAIN_WINDOW, second).expect("the pane closes");
        assert_eq!(launch_file(&dir).keys().cloned().collect::<Vec<_>>(), [first.to_string()]);
        assert_eq!(lock(&core.launches).keys().copied().collect::<Vec<_>>(), [first]);
    }

    #[cfg(unix)]
    #[test]
    fn an_exec_pane_is_not_restored_and_says_what_it_was() {
        let dir = Scratch::new("restore");
        let tool = fake_tool(&dir, "kubectl", 1);
        let pane = {
            let (core, _rec) = core_in(&dir, "/bin/sh", &["-c", "sleep 5"]);
            let pane = only_pane(&core);
            core.register_launch(pane, tool_launch(tool, "Exec in pod api-0 (prod-eu)")).expect("registered");
            core.shutdown();
            pane
        };
        // A marker of a pane that is in no layout, and one with a label that carries a control character.
        let mut marks = launch_file(&dir);
        assert_eq!(marks.len(), 1, "{marks:?}");
        marks.insert("9999".into(), "ghost".into());
        std::fs::write(dir.0.join("pane_launches.json"), serde_json::to_vec(&marks).unwrap()).unwrap();

        // The next start: the pane is a normal shell, it says what it was, and nothing is run.
        let (core, rec) = core_in(&dir, "/bin/sh", &["-c", "sleep 5"]);
        assert_eq!(only_pane(&core), pane, "the layout came back");
        assert_eq!(lock(&core.launch_marks).keys().copied().collect::<Vec<_>>(), [pane], "the ghost marker is dropped");
        assert!(lock(&core.launches).is_empty(), "nothing is registered from a file");

        core.start_pane(pane, 80, 24, 0, 0).expect("the pane starts");

        let notice = "holzWave: this pane ran Exec in pod api-0 (prod-eu); it is not started again";
        assert!(wait_until(WAIT, || rec.has_text(pane, notice)), "no notice: {:?}", rec.all());
        assert!(core.pid(pane).is_some(), "a normal shell runs");
        assert!(core.held_panes().is_empty());
        assert!(launch_file(&dir).is_empty(), "the marker is one-shot: {:?}", launch_file(&dir));
        assert!(lock(&core.launch_marks).is_empty());
        assert!(!dir.0.join("kubectl.env").exists(), "the tool was not run again");
    }

    #[cfg(unix)]
    #[test]
    fn a_damaged_or_oversized_markers_file_is_ignored() {
        for (tag, bytes) in [
            ("damaged", br#"{"1": "a secret-looking label"#.to_vec()),
            ("oversized", format!(r#"{{"1": "{}"}}"#, "x".repeat(20 * 1024)).into_bytes()),
            ("notmap", br#"["a secret-looking label"]"#.to_vec()),
        ] {
            let dir = Scratch::new(tag);
            std::fs::write(dir.0.join("pane_launches.json"), bytes).unwrap();
            let (core, rec) = core_in(&dir, "/bin/sh", &["-c", "printf started; sleep 5"]);
            let pane = only_pane(&core);
            assert!(lock(&core.launch_marks).is_empty(), "{tag}");

            core.start_pane(pane, 80, 24, 0, 0).expect("the pane starts");

            assert!(wait_until(WAIT, || rec.has_text(pane, "started")), "{tag}: {:?}", rec.all());
            assert!(!rec.has_text(pane, "this pane ran"), "{tag}: no notice from a file that cannot be read");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_held_pane_is_recorded_until_it_is_gone() {
        let (core, rec, dir) = sh("sleep 5");
        let first = only_pane(&core);
        let second = core.split(true).focus;
        let tool = fake_tool(&dir, "talosctl", 2);
        core.register_launch(second, tool_launch(tool, "Talos dashboard")).expect("registered");
        assert_eq!(core.pane_held(second), None, "not held before it ran");

        core.start_pane(second, 80, 24, 0, 0).expect("the pane starts");

        assert!(wait_until(Duration::from_secs(60), || core.pane_held(second).is_some()), "{:?}", rec.all());
        assert_eq!(core.pane_held(second), Some(Some(2)), "held, with the status of the tool");
        assert_eq!(core.held_panes(), [(second, Some(2))]);
        assert_eq!(core.pane_held(first), None);
        assert_eq!(launch_file(&dir).get(&second.to_string()).map(String::as_str), Some("Talos dashboard"));
        std::thread::sleep(Duration::from_millis(300));
        assert!(rec.pane_exits().is_empty(), "held: the pane did not end");

        // Enter ends it: the held record, the launch marker and the pane go together.
        core.key(second, enter_key()).expect("a held pane takes Enter");
        assert!(wait_until(WAIT, || rec.pane_exits() == [(second, Some(2))]), "{:?}", rec.pane_exits());
        assert!(core.held_panes().is_empty(), "cleared when the pane is gone");
        assert_eq!(core.pane_held(second), None);
        assert!(launch_file(&dir).is_empty(), "the marker went with the pane: {:?}", launch_file(&dir));
    }
}
