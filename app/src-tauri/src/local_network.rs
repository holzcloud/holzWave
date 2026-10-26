//! The macOS source of the Local Network permission: the one place that touches the bridge crate `holzwave-local-network`.
//! The host that uses it, the view and the seam ([`Source`]) are in [`holzwave_host::local_network`]. Windows and Linux have
//! no source: [`system`] answers `None` there. This file contains no foreign-function code.

#[cfg(target_os = "macos")]
use std::time::Duration;
use std::sync::Arc;

use holzwave_host::local_network::{PlatformParts, Source};
#[cfg(target_os = "macos")]
use holzwave_core::local_network::Reading;

/// The source of this platform: over the bridge crate on macOS, none elsewhere (no notice, no row, no file).
pub fn system() -> Option<Arc<dyn Source>> {
    #[cfg(target_os = "macos")]
    {
        Some(Arc::new(SystemSource))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The pieces the platform supplies to the core: the source of this platform when it has one, the host's default (which
/// reports the setting as unsupported) when it has none.
pub fn platform_parts() -> PlatformParts {
    match system() {
        Some(local_network) => PlatformParts { local_network },
        None => PlatformParts::default(),
    }
}

/// The source over the bridge crate, which holds the only foreign calls.
#[cfg(target_os = "macos")]
struct SystemSource;

#[cfg(target_os = "macos")]
impl Source for SystemSource {
    fn applies(&self) -> bool {
        holzwave_local_network::applies()
    }

    fn probe(&self, window: Duration) -> Reading {
        match holzwave_local_network::probe(window) {
            holzwave_local_network::Reading::Allowed => Reading::Allowed,
            holzwave_local_network::Reading::Denied => Reading::Denied,
            holzwave_local_network::Reading::Unknown => Reading::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_source_exists_only_on_macos() {
        assert_eq!(system().is_some(), cfg!(target_os = "macos"));
    }
}
