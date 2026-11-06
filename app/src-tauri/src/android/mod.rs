//! The Android shell of the app: what the platform decides for the core. It holds [`host::AndroidHost`], the answer of
//! the [`Host`](holzwave_host::seams::Host) seam on Android. The module is compiled for Android and for the unit tests
//! on the desktop (it touches no Android API, so its rules are proved without a device); the desktop app never uses it.

pub mod host;
