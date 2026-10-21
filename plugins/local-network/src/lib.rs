//! The Local Network privacy probe of holzWave: a first-party Swift package behind a C ABI of two integer functions,
//! built from this crate's build script. macOS only; elsewhere the crate is plain Rust and no Swift tool is needed.
//!
//! macOS 15 and later let the person allow or deny the Local Network permission per app. There is no API to ask for it
//! and none to read it (Apple radar FB8711182, TN3179). [`probe`] runs one bounded Bonjour browse for a private service
//! type and reads the outcome as a [`Reading`]: a browse that waits with the "policy denied" error is denied, one that
//! stays ready is allowed, anything else is unknown. **A probe is itself a local network operation**, so on a Mac where
//! the person has not answered yet it makes the system show its own question; call it only when that is wanted.
//!
//! The crate opens no connection, starts no listener, publishes nothing and sends no data. All `unsafe` code is in
//! `system.rs`.

#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::time::Duration;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod system;

/// The private Bonjour type that is browsed. The Swift source and `NSBonjourServices` in the app's `Info.plist` name the
/// same string, and `tests/pins.rs` pins the three together.
pub const SERVICE_TYPE: &str = "_holzwave-probe._tcp";

/// The shortest window a probe is given. A browse needs time to reach the ready state and settle.
pub const MIN_WINDOW: Duration = Duration::from_millis(600);
/// The longest window a probe is given, so a call can never block for long.
pub const MAX_WINDOW: Duration = Duration::from_secs(10);

/// What the probe saw. The enum is exhaustive on purpose: a new variant forces every `match` on it to be revisited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// The browse stayed ready: holzWave may use the local network.
    Allowed,
    /// The browse waited with the "policy denied" error: the permission is off.
    Denied,
    /// Anything else, including: no network interface, a failed browse, nothing seen in the window, not macOS.
    Unknown,
}

impl Reading {
    /// Maps the code of `hw_ln_probe`: 0 is allowed and 1 denied. Every other number, also a negative one, is
    /// [`Reading::Unknown`], so an answer nobody knows can never read as allowed.
    pub fn from_code(code: i32) -> Reading {
        match code {
            0 => Reading::Allowed,
            1 => Reading::Denied,
            _ => Reading::Unknown,
        }
    }
}

/// The window in whole milliseconds, held between [`MIN_WINDOW`] and [`MAX_WINDOW`].
pub fn clamp_window(window: Duration) -> u32 {
    let millis = window.clamp(MIN_WINDOW, MAX_WINDOW).as_millis();
    // MAX_WINDOW is 10 000 ms, so the value always fits; the fallback is only for the type.
    u32::try_from(millis).unwrap_or(10_000)
}

/// Whether this system has the Local Network privacy setting: macOS 15 or later. False on every other system, where
/// nothing is asked and no notice would be true.
pub fn applies() -> bool {
    #[cfg(target_os = "macos")]
    {
        system::applies()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Whether both foreign functions of the Swift package link, checked by taking their addresses; nothing is called and the
/// network is not touched. Used by `tests/pins.rs`. Off macOS there is nothing to link, so it is true.
#[doc(hidden)]
pub fn symbols_link() -> bool {
    #[cfg(target_os = "macos")]
    {
        system::symbols_link()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Runs one browse for at most `window` (clamped, see [`clamp_window`]) and reads it. Blocks the calling thread for that
/// long at most. Off macOS it does nothing and answers [`Reading::Unknown`].
pub fn probe(window: Duration) -> Reading {
    #[cfg(target_os = "macos")]
    {
        Reading::from_code(system::probe(clamp_window(window)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        Reading::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codes_map_and_every_other_number_is_unknown() {
        assert_eq!(Reading::from_code(0), Reading::Allowed);
        assert_eq!(Reading::from_code(1), Reading::Denied);
        for code in [2, 3, 7, -1, -65570, i32::MIN, i32::MAX] {
            assert_eq!(Reading::from_code(code), Reading::Unknown, "code {code}");
        }
    }

    #[test]
    fn the_window_is_clamped_at_both_ends() {
        assert_eq!(clamp_window(Duration::ZERO), 600);
        assert_eq!(clamp_window(Duration::from_millis(599)), 600);
        assert_eq!(clamp_window(Duration::from_millis(600)), 600);
        assert_eq!(clamp_window(Duration::from_millis(1500)), 1500);
        assert_eq!(clamp_window(Duration::from_secs(10)), 10_000);
        assert_eq!(clamp_window(Duration::from_secs(11)), 10_000);
        assert_eq!(clamp_window(Duration::MAX), 10_000);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_nothing_applies_and_nothing_is_probed() {
        assert!(!applies());
        assert_eq!(probe(Duration::from_secs(1)), Reading::Unknown);
    }
}
