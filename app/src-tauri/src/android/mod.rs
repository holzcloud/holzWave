//! The Android shell of the app: what the platform decides for the core, and the one first-party plugin through which the
//! Rust side and the Kotlin side of the app talk.
//!
//! [`host::AndroidHost`] is the answer of the [`Host`](holzwave_host::seams::Host) seam on Android. It and the rules of
//! [`service`] (when the foreground service is told the counts of the live sessions) are compiled for Android and for the
//! unit tests on the desktop (they touch no Android API, so their rules are proved without a device); the desktop app never
//! uses them. [`keystore`] is the vault's keystore and clock over the plugin, tested on the desktop with a recorder in place of the
//! plugin, and [`picker`] is the hand-over's file picker over the same plugin and the same bridge. [`plugin`], the handle to the Kotlin class, and the thread of [`service`] that uses it exist on Android only, and so
//! does `selftest`, which exists in debug builds of the app only.

pub mod host;
pub mod keystore;
pub mod picker;
#[cfg(target_os = "android")]
pub mod plugin;
#[cfg(all(target_os = "android", debug_assertions))]
pub mod selftest;
pub mod service;
#[cfg(target_os = "android")]
pub mod tls;
