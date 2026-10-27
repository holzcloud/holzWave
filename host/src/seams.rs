//! The one seam between the core and the shell that runs it: everything the shell decides is asked of a [`Host`].
//!
//! The desktop app and the server build the same [`AppCore`](crate::state::AppCore) and differ in exactly these seven
//! answers: which platform the page is told it runs on (and so which table of default key chords it gets), where
//! passwords, key passphrases, provider keys and integration secrets are kept, where the answer of the Local Network
//! permission comes from, which text size the system asks for, how many panes and lines of scrollback a person may
//! have, at which address the page finds a file it was granted, and whether the machine's process table may be shown.
//! [`NeutralHost`] answers for a build that is given no host: the platform of the build, secrets in memory, no
//! permission source, a text scale of 100, no limits, the address `/files/<token>` and a process list that may be shown.
//! Every constructor that does not take a host uses it, so a test or a tool never touches a keychain.

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
    /// The page in a browser, served by the server.
    Web,
}

impl Platform {
    /// The platform of this build: macOS and Windows as they are, every other target as Linux (the desktop is built for
    /// these three only).
    pub fn current() -> Platform {
        match std::env::consts::OS {
            "macos" => Platform::MacOs,
            "windows" => Platform::Windows,
            _ => Platform::Linux,
        }
    }

    /// The spelling the page and `std::env::consts::OS` use: `macos`, `windows`, `linux` or `web`.
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::MacOs => "macos",
            Platform::Windows => "windows",
            Platform::Linux => "linux",
            Platform::Web => "web",
        }
    }

    /// The name of the table of default key chords this platform selects, as `Keybindings::resolve_for` takes it. It is
    /// the platform's own spelling; a name the core has no table for gets the Linux one.
    pub fn chord_table(self) -> &'static str {
        self.as_str()
    }
}

/// What a person may have on this host. `None` is no limit of the host (the layout's own limits still hold).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostLimits {
    /// The most panes in the layout of the person, over all windows.
    pub max_panes: Option<usize>,
    /// The most lines of scrollback one pane keeps, whatever the setting says.
    pub max_scrollback_lines: Option<usize>,
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
        for (platform, name) in
            [(Platform::MacOs, "macos"), (Platform::Windows, "windows"), (Platform::Linux, "linux"), (Platform::Web, "web")]
        {
            assert_eq!(platform.as_str(), name);
            assert_eq!(platform.chord_table(), name);
        }
        assert_eq!(Platform::current().as_str(), std::env::consts::OS);
    }

    #[test]
    fn seam_the_neutral_host_has_memory_secrets_no_permission_source_and_no_limits() {
        let host = NeutralHost::new();
        assert_eq!(host.platform(), Platform::current());
        assert_eq!(host.text_scale(), 100);
        assert_eq!(host.limits(), HostLimits { max_panes: None, max_scrollback_lines: None });
        assert!(!host.local_network().applies());
        // One store, however often it is asked for.
        host.secrets().set("ssh:a", "one").expect("set");
        assert_eq!(host.secrets().get("ssh:a").expect("get").as_deref(), Some("one"));
        // And a second host has a store of its own.
        assert_eq!(NeutralHost::new().secrets().get("ssh:a").expect("get"), None);
    }
}
