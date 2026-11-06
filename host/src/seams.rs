//! The one seam between the core and the shell that runs it: everything the shell decides is asked of a [`Host`].
//!
//! The desktop app, the Android app and the server build the same [`AppCore`](crate::state::AppCore) and differ in
//! exactly these eight answers: which platform the page is told it runs on (and so which table of default key chords it gets), where
//! passwords, key passphrases, provider keys and integration secrets are kept, where the answer of the Local Network
//! permission comes from, which text size the system asks for, how many panes and lines of scrollback a person may
//! have, at which address the page finds a file it was granted, whether the machine's process table may be shown, and
//! which private folders the shell of a pane lives in (only Android names them).
//! [`NeutralHost`] answers for a build that is given no host: the platform of the build, secrets in memory, no
//! permission source, a text scale of 100, no limits, the address `/files/<token>`, a process list that may be shown
//! and no private folders.
//! Every constructor that does not take a host uses it, so a test or a tool never touches a keychain.

use std::path::PathBuf;
use std::sync::Arc;

use holzwave_core::secrets::{MemoryStore, SecretStore};

use crate::local_network::{Source, Unsupported};

/// The platform the page is told it runs on. The page reads its spelling (`os-<name>` class, shortcut labels) and the
/// core picks the table of default key chords by [`Platform::chord_table`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
    /// The Android app (arm64 only).
    Android,
    /// The page in a browser, served by the server.
    Web,
}

impl Platform {
    /// The platform of this build, read from `std::env::consts::OS` by [`from_os_name`](Self::from_os_name).
    pub fn current() -> Platform {
        Platform::from_os_name(std::env::consts::OS)
    }

    /// The platform of an OS name as `std::env::consts::OS` spells it: macOS, Windows and Android as they are, every
    /// other name as Linux. The server's `web` is never the name of a build, so it is not read here.
    pub fn from_os_name(name: &str) -> Platform {
        match name {
            "macos" => Platform::MacOs,
            "windows" => Platform::Windows,
            "android" => Platform::Android,
            _ => Platform::Linux,
        }
    }

    /// The spelling the page and `std::env::consts::OS` use: `macos`, `windows`, `linux`, `android` or `web`.
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::MacOs => "macos",
            Platform::Windows => "windows",
            Platform::Linux => "linux",
            Platform::Android => "android",
            Platform::Web => "web",
        }
    }

    /// The name of the table of default key chords this platform selects, as `Keybindings::resolve_for` takes it. It is
    /// the platform's own spelling; a name the core has no table for gets the Linux one.
    pub fn chord_table(self) -> &'static str {
        self.as_str()
    }
}

/// The two folders of the app's private storage that a local shell uses on a host that sandboxes the app (Android).
///
/// `home` is the shell's home and start directory; it is a folder of its own, never the folder that holds the app's own
/// settings and vault, so that a program a person starts does not find them next to its files by accident. `cache` is
/// its `TMPDIR`. Both are absolute and exist when the host names them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateDirs {
    pub home: PathBuf,
    pub cache: PathBuf,
}

/// What a person may have on this host. `None` is no limit of the host (the layout's own limits still hold).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostLimits {
    /// The most panes in the layout of the person, over all windows.
    pub max_panes: Option<usize>,
    /// The most lines of scrollback one pane keeps, whatever the setting says.
    pub max_scrollback_lines: Option<usize>,
    /// The most bytes of pictures (Kitty graphics) one pane keeps, whatever the setting says. The desktop has none; a
    /// server fills it from its worker limits.
    pub max_image_bytes: Option<usize>,
}

/// Everything the shell decides for the core. Object safe; the core keeps an `Arc<dyn Host>`.
pub trait Host: Send + Sync + 'static {
    /// The platform the page is told it runs on.
    fn platform(&self) -> Platform;

    /// The one store for connection passwords, key passphrases, provider keys and integration secrets. Every call
    /// answers the same store.
    fn secrets(&self) -> Arc<dyn SecretStore>;

    /// Where the answer of the Local Network permission comes from.
    fn local_network(&self) -> Arc<dyn Source>;

    /// The text size the operating system asks for, in percent (100 where there is no such setting).
    fn text_scale(&self) -> u32;

    /// The caps of this host.
    fn limits(&self) -> HostLimits;

    /// Where the page finds the file a grant with `token` names: the address the browser can load it from. The server
    /// serves a granted file at `/files/<token>`; the desktop overrides this with the address of its own file scheme.
    fn file_url(&self, token: &str) -> String {
        format!("/files/{token}")
    }

    /// Whether the reading of the local monitor may list the busiest processes of the machine. It may where the machine
    /// is the person's own; the server answers false, because its machine also runs other people's programs and the
    /// names of their processes are not this person's to see.
    fn local_processes(&self) -> bool {
        true
    }

    /// The private folders of the app that the local shell lives in: its home and start directory, and its `TMPDIR`.
    /// Only the Android host answers it; everywhere else the shell has the person's own home and the environment it
    /// inherits, so the answer is `None` and nothing about a pane changes.
    fn private_dirs(&self) -> Option<PrivateDirs> {
        None
    }
}

/// The host of a build that is given none: the platform of the build, secrets in memory, no permission source, a text
/// scale of 100 and no limits.
pub struct NeutralHost {
    secrets: Arc<dyn SecretStore>,
}

impl NeutralHost {
    pub fn new() -> NeutralHost {
        NeutralHost { secrets: Arc::new(MemoryStore::default()) }
    }
}

impl Default for NeutralHost {
    fn default() -> NeutralHost {
        NeutralHost::new()
    }
}

impl Host for NeutralHost {
    fn platform(&self) -> Platform {
        Platform::current()
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        self.secrets.clone()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seam_each_platform_has_its_spelling_and_its_chord_table() {
        for (platform, name) in [
            (Platform::MacOs, "macos"),
            (Platform::Windows, "windows"),
            (Platform::Linux, "linux"),
            (Platform::Android, "android"),
            (Platform::Web, "web"),
        ] {
            assert_eq!(platform.as_str(), name);
            assert_eq!(platform.chord_table(), name);
        }
        assert_eq!(Platform::current().as_str(), std::env::consts::OS);
    }

    #[test]
    fn seam_platform_reads_the_os_name_and_every_unknown_name_is_linux() {
        assert_eq!(Platform::from_os_name("macos"), Platform::MacOs);
        assert_eq!(Platform::from_os_name("windows"), Platform::Windows);
        assert_eq!(Platform::from_os_name("android"), Platform::Android);
        assert_eq!(Platform::from_os_name("linux"), Platform::Linux);
        for name in ["freebsd", "openbsd", "ios", "web", ""] {
            assert_eq!(Platform::from_os_name(name), Platform::Linux, "{name:?}");
        }
        assert_eq!(Platform::current(), Platform::from_os_name(std::env::consts::OS));
        // The spelling the page reads is the spelling of the core's OS, so `app_info` and `Os::current` agree.
        for platform in [Platform::MacOs, Platform::Windows, Platform::Linux, Platform::Android] {
            assert_eq!(Platform::from_os_name(platform.as_str()), platform);
            assert_eq!(holzwave_core::platform::Os::from_name(platform.as_str()).name(), platform.as_str());
        }
    }

    #[test]
    fn seam_android_takes_the_chords_of_linux_and_not_those_of_macos() {
        use holzwave_core::keybindings::Keybindings;
        use std::collections::BTreeMap;

        let table = |platform: Platform| Keybindings::resolve_for(platform.chord_table(), &BTreeMap::new()).0.table();
        assert_eq!(Platform::Android.chord_table(), "android");
        let (android, linux, macos) = (table(Platform::Android), table(Platform::Linux), table(Platform::MacOs));
        assert_eq!(android, linux, "the core has no table of that name and falls back to the Linux one");
        // Three of the chords that differ between the tables of macOS and Linux, by action: Android has the Linux ones.
        let differing: Vec<_> = linux.iter().zip(&macos).filter(|(l, m)| l.chord != m.chord).take(3).collect();
        assert_eq!(differing.len(), 3, "the tables of macOS and Linux differ in at least three chords");
        for (l, m) in differing {
            let a = android.iter().find(|row| row.action == l.action).expect("the action is in the table");
            assert_eq!(a.chord, l.chord, "{}", l.action);
            assert_ne!(a.chord, m.chord, "{}", l.action);
        }
    }

    #[test]
    fn seam_the_neutral_host_has_memory_secrets_no_permission_source_and_no_limits() {
        let host = NeutralHost::new();
        assert_eq!(host.platform(), Platform::current());
        assert_eq!(host.text_scale(), 100);
        assert_eq!(host.limits(), HostLimits { max_panes: None, max_scrollback_lines: None, max_image_bytes: None });
        assert!(!host.local_network().applies());
        assert_eq!(host.private_dirs(), None, "no private folders unless the Android host names them");
        // One store, however often it is asked for.
        host.secrets().set("ssh:a", "one").expect("set");
        assert_eq!(host.secrets().get("ssh:a").expect("get").as_deref(), Some("one"));
        // And a second host has a store of its own.
        assert_eq!(NeutralHost::new().secrets().get("ssh:a").expect("get"), None);
    }
}
