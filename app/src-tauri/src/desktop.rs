//! What the desktop shell decides for the core: [`DesktopHost`] answers the [`Host`] seam with the platform of the
//! build, the operating system's keychain, the macOS source of the Local Network permission (none elsewhere), the
//! operating system's text size, and no limits.

use std::sync::Arc;

use holzwave_core::secrets::{KeychainStore, SecretStore};
use holzwave_host::local_network::{Source, Unsupported};
use holzwave_host::seams::{Host, HostLimits, Platform};

/// The host of the desktop app.
pub struct DesktopHost {
    secrets: Arc<dyn SecretStore>,
}

impl DesktopHost {
    pub fn new() -> DesktopHost {
        DesktopHost { secrets: Arc::new(KeychainStore) }
    }
}

impl Host for DesktopHost {
    fn platform(&self) -> Platform {
        Platform::current()
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        self.secrets.clone()
    }

    fn local_network(&self) -> Arc<dyn Source> {
        crate::local_network::system().unwrap_or_else(|| Arc::new(Unsupported))
    }

    /// Runs a program on Windows and Linux for at most a moment: call it off the main thread.
    fn text_scale(&self) -> u32 {
        crate::textscale::read(std::env::consts::OS)
    }

    fn limits(&self) -> HostLimits {
        HostLimits::default()
    }

    /// The address of the desktop's own file scheme, which depends on the platform the way custom schemes do.
    fn file_url(&self, token: &str) -> String {
        crate::files_protocol::url_of(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracer_the_desktop_host_serves_a_granted_file_at_the_address_of_its_file_scheme() {
        let host = DesktopHost::new();
        for token in ["0123456789abcdef0123456789abcdef", "ffffffffffffffffffffffffffffffff", "a"] {
            assert_eq!(host.file_url(token), crate::files_protocol::url_of(token), "{token}");
        }
        let expected = if cfg!(any(windows, target_os = "android")) {
            "http://holzfile.localhost/abc"
        } else {
            "holzfile://localhost/abc"
        };
        assert_eq!(host.file_url("abc"), expected);
    }

    #[test]
    fn the_desktop_host_answers_what_the_removed_code_answered() {
        let host = DesktopHost::new();
        assert_eq!(host.platform().as_str(), std::env::consts::OS);
        assert_eq!(host.limits(), HostLimits::default());
        assert!(host.local_processes(), "the person's own computer shows its processes");
        // One store however often it is asked for (nothing is read from or written to the keychain here).
        assert!(Arc::ptr_eq(&host.secrets(), &host.secrets()));
    }
}
