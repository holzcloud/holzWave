//! What a browser build shows in its settings: the platform `web` and the table of default chords that does not hold
//! the chords a browser keeps for itself.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use holzwave_core::keybindings::Keybindings;
use holzwave_core::secrets::SecretStore;
use holzwave_host::local_network::{Source, Unsupported};
use holzwave_host::seams::{Host, HostLimits, NeutralHost, Platform};
use holzwave_host::state::{AppCore, Out, ShellSpec, Sink, Target};
use holzwave_host::windows::{CursorSpot, Place};

/// A sink that drops what the core sends: these tests read the settings, not the events.
struct Silent;

impl Sink for Silent {
    fn send(&self, _target: Target, _out: Out) {}

    fn close_window(&self, _label: &str) {}

    fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
        Ok(())
    }

    fn cursor_spot(&self, _source: &str) -> CursorSpot {
        CursorSpot::Unknown
    }
}

/// A host that is the neutral one in everything but the platform it names.
struct Named {
    base: NeutralHost,
    platform: Platform,
}

impl Host for Named {
    fn platform(&self) -> Platform {
        self.platform
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        self.base.secrets()
    }

    fn local_network(&self) -> Arc<dyn Source> {
        Arc::new(Unsupported)
    }

    fn text_scale(&self) -> u32 {
        100
    }

    fn limits(&self) -> HostLimits {
        HostLimits::default()
    }
}

/// A core in a scratch directory of its own, removed when it goes out of scope.
struct World {
    dir: PathBuf,
    core: Arc<AppCore>,
}

impl World {
    fn new(tag: &str, host: Arc<dyn Host>) -> World {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("holzwave-webchords-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).expect("scratch dir");
        let core = AppCore::new_with_host(dir.clone(), Arc::new(Silent), ShellSpec::new("/bin/cat", &[]), None, None, host);
        World { dir, core }
    }

    /// The chord shown for `action` in the settings, `None` when the row is missing or unbound.
    fn chord(&self, action: &str) -> Option<String> {
        let view = self.core.get_settings();
        let row = view.keybindings.iter().find(|row| row.action == action)?;
        row.chord.clone()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.core.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn host_of(platform: Platform) -> Arc<dyn Host> {
    Arc::new(Named { base: NeutralHost::new(), platform })
}

#[test]
fn web_chords_a_web_host_shows_the_three_browser_chords_in_its_settings() {
    let world = World::new("web", host_of(Platform::Web));
    assert_eq!(world.core.app_info().os, "web");
    assert_eq!(world.chord("pane.close").as_deref(), Some("Alt+Shift+W"));
    assert_eq!(world.chord("tab.new").as_deref(), Some("Alt+Shift+T"));
    assert_eq!(world.chord("workspace.new").as_deref(), Some("Alt+Shift+N"));
    assert_eq!(world.chord("palette.open").as_deref(), Some("Ctrl+Shift+P"));
}

#[test]
fn web_chords_a_web_host_keeps_the_linux_default_in_every_other_row() {
    let world = World::new("rows", host_of(Platform::Web));
    let linux = Keybindings::default_for("linux").table();
    let shown = world.core.get_settings().keybindings;
    assert_eq!(shown.len(), linux.len());
    let mut different = Vec::new();
    for (row, want) in shown.iter().zip(&linux) {
        assert_eq!(row.action, want.action);
        assert!(row.is_default, "{} is a default", row.action);
        if row.chord != want.chord {
            different.push(row.action.clone());
        }
    }
    assert_eq!(different, ["pane.close", "tab.new", "workspace.new"]);
}

#[test]
fn web_chords_a_desktop_host_shows_the_default_of_its_own_platform() {
    let world = World::new("desktop", Arc::new(NeutralHost::new()));
    let own = Keybindings::default();
    for action in ["pane.close", "tab.new", "workspace.new", "palette.open"] {
        assert_eq!(world.chord(action), own.chord_of(action).map(ToString::to_string), "{action}");
    }
    assert_ne!(world.core.app_info().os, "web");
    if std::env::consts::OS == "linux" {
        assert_eq!(world.chord("pane.close").as_deref(), Some("Ctrl+Shift+W"));
    }
}

#[test]
fn web_chords_a_linux_host_is_unchanged() {
    let world = World::new("linux", host_of(Platform::Linux));
    assert_eq!(world.core.app_info().os, "linux");
    assert_eq!(world.chord("pane.close").as_deref(), Some("Ctrl+Shift+W"));
    assert_eq!(world.chord("tab.new").as_deref(), Some("Ctrl+Shift+T"));
    assert_eq!(world.chord("workspace.new").as_deref(), Some("Ctrl+Shift+N"));
}
