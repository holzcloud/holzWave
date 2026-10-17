//! The Apple on-device model of holzWave: a first-party Swift package behind a small C ABI, built from this crate's
//! build script, and an [`AppleProvider`] that makes it a provider of the AI core. Chat only: no tools.
//!
//! [`probe`] answers whether Apple's on-device language model is available, as an [`Availability`]. [`AppleProvider`]
//! implements the `Provider` trait over a [`Backend`]: the system model on a Mac ([`AppleProvider::system`]) or a
//! [`ScriptedBackend`] in tests, on any host. The crate opens no socket, chooses no other provider when the model is not
//! available and sends nothing but the approved text to the model. Off macOS it is plain Rust and no Swift tool is
//! needed to build it.
//!
//! The framework is linked weakly, so a process that contains this crate still starts on a Mac whose operating system
//! has no FoundationModels framework (see `build.rs`).

#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::time::Duration;

mod backend;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod native;
mod prompt;
mod provider;
// The state machine is used by the system backend only; the other hosts still compile and test it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod pump;
mod scripted;

pub use backend::{AppleError, Backend, GenerateRequest, MAX_INPUT_BYTES};
pub use prompt::{
    Budget, Estimator, FALLBACK_CONTEXT_TOKENS, FRAME_MARGIN_TOKENS, Rendered, SYSTEM_BUDGET_TOKENS, estimate_tokens, render,
};
pub use provider::{AppleProvider, MODEL_ID, PROVIDER_ID};
pub use scripted::{BackendScript, ScriptedBackend};
#[cfg(target_os = "macos")]
pub use native::SystemBackend;

/// Whether the system language model can be used right now.
///
/// The enum is exhaustive on purpose: a new variant forces every `match` on it to be revisited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// The model is there and ready.
    Available,
    /// The user has not turned Apple Intelligence on.
    AppleIntelligenceNotEnabled,
    /// This device cannot run the model.
    DeviceNotEligible,
    /// The model is not ready yet (downloading or being prepared).
    ModelNotReady,
    /// Another reason, including reasons that a later SDK adds. Never treated as available.
    Other,
    /// macOS is older than 26: the framework does not exist on this system.
    OsTooOld,
    /// The SDK the crate was built with has no FoundationModels framework.
    SdkLacksFramework,
    /// Not macOS: nothing was asked.
    UnsupportedPlatform,
}

impl Availability {
    /// Maps the code of `hw_afm_availability`: 0 is available, 1 Apple Intelligence not enabled, 2 device not eligible,
    /// 3 model not ready, 4 another reason, 5 macOS below 26, 6 SDK without the framework. Every other number is
    /// [`Availability::Other`], so an answer nobody knows can never offer the provider.
    pub fn from_code(code: i32) -> Availability {
        match code {
            0 => Availability::Available,
            1 => Availability::AppleIntelligenceNotEnabled,
            2 => Availability::DeviceNotEligible,
            3 => Availability::ModelNotReady,
            5 => Availability::OsTooOld,
            6 => Availability::SdkLacksFramework,
            _ => Availability::Other,
        }
    }

    /// True for [`Availability::Available`] only.
    pub fn is_available(self) -> bool {
        self == Availability::Available
    }

    /// A stable snake_case key for logs, the settings page and the launch check.
    pub fn as_str(self) -> &'static str {
        match self {
            Availability::Available => "available",
            Availability::AppleIntelligenceNotEnabled => "apple_intelligence_not_enabled",
            Availability::DeviceNotEligible => "device_not_eligible",
            Availability::ModelNotReady => "model_not_ready",
            Availability::Other => "other",
            Availability::OsTooOld => "os_too_old",
            Availability::SdkLacksFramework => "sdk_lacks_framework",
            Availability::UnsupportedPlatform => "unsupported_platform",
        }
    }
}

/// What a person can do about an availability state, as a key. The wording is the shell's: this crate gives the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advice {
    /// Nothing: the model is available.
    None,
    /// Turn Apple Intelligence on in the system settings.
    EnableAppleIntelligence,
    /// Wait: the model is still being prepared.
    WaitForModel,
    /// This device or platform cannot run the model.
    NotSupported,
    /// Update macOS.
    UpdateMacos,
    /// This build of the app was made without the framework.
    NotInThisBuild,
    /// The reason is not known.
    Unknown,
}

impl Advice {
    /// A stable snake_case key.
    pub fn as_str(self) -> &'static str {
        match self {
            Advice::None => "none",
            Advice::EnableAppleIntelligence => "enable_apple_intelligence",
            Advice::WaitForModel => "wait_for_model",
            Advice::NotSupported => "not_supported",
            Advice::UpdateMacos => "update_macos",
            Advice::NotInThisBuild => "not_in_this_build",
            Advice::Unknown => "unknown",
        }
    }
}

impl Availability {
    /// The action that fits this state. The match is exhaustive on purpose, so a new state forces the mapping to be
    /// revisited.
    pub fn advice(self) -> Advice {
        match self {
            Availability::Available => Advice::None,
            Availability::AppleIntelligenceNotEnabled => Advice::EnableAppleIntelligence,
            Availability::ModelNotReady => Advice::WaitForModel,
            Availability::DeviceNotEligible | Availability::UnsupportedPlatform => Advice::NotSupported,
            Availability::OsTooOld => Advice::UpdateMacos,
            Availability::SdkLacksFramework => Advice::NotInThisBuild,
            Availability::Other => Advice::Unknown,
        }
    }
}

/// Asks the system language model whether it is available. Synchronous, no network, callable from any thread. The answer
/// is not cached: Apple can change it while the app runs, so ask again when the answer matters.
pub fn probe() -> Availability {
    #[cfg(target_os = "macos")]
    {
        Availability::from_code(ffi::availability())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Availability::UnsupportedPlatform
    }
}

/// What [`selftest`] answers when the Swift task called back.
pub const SELFTEST_VALUE: i32 = 42;

/// Smoke test of the Swift concurrency runtime and of the callback path: starts a Swift task that calls back into Rust
/// exactly once with [`SELFTEST_VALUE`], and waits for it. `None` off macOS, on a timeout, and when the callback came
/// more than once.
///
/// On a timeout, or after a second callback, the callback's reference to the shared state is leaked on purpose, so a
/// late call still finds live memory.
pub fn selftest(timeout: Duration) -> Option<i32> {
    #[cfg(target_os = "macos")]
    {
        ffi::selftest(timeout)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = timeout;
        None
    }
}

/// The place that asks Swift for the availability and runs the self test: the first of the two homes of foreign calls
/// (the stream calls are in `native`).
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod ffi {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    unsafe extern "C" {
        fn hw_afm_availability() -> i32;
        fn hw_afm_selftest(callback: extern "C" fn(*mut c_void, i32), user_data: *mut c_void);
    }

    pub(crate) fn availability() -> i32 {
        // SAFETY: the function takes no arguments, passes no pointers and returns a plain integer; it is defined by
        // `@_cdecl("hw_afm_availability")` in the Swift package and linked from the static library of build.rs.
        unsafe { hw_afm_availability() }
    }

    /// Shared between the waiter and the callback.
    #[derive(Default)]
    struct State {
        value: Mutex<Option<i32>>,
        signal: Condvar,
        calls: AtomicU32,
    }

    /// Called by Swift, from a thread of its concurrency pool. It only borrows `user_data`: the waiter owns the
    /// reference behind it and releases it after it has seen the value.
    extern "C" fn callback(user_data: *mut c_void, value: i32) {
        // SAFETY: `user_data` is the pointer `selftest` got from `Arc::into_raw`; that reference is released only after
        // this call was seen (or leaked), so the state is alive. The callback never frees it.
        let state = unsafe { &*(user_data as *const State) };
        state.calls.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut slot) = state.value.lock() {
            *slot = Some(value);
        }
        state.signal.notify_all();
    }

    pub(crate) fn selftest(timeout: Duration) -> Option<i32> {
        let state = Arc::new(State::default());
        let raw = Arc::into_raw(Arc::clone(&state)) as *mut c_void;
        // SAFETY: `callback` has the C signature the Swift side expects (`@convention(c) (UnsafeMutableRawPointer?,
        // Int32) -> Void`), and `raw` stays valid until the reference it stands for is released below or leaked.
        unsafe { hw_afm_selftest(callback, raw) };

        let guard = state.value.lock().ok()?;
        let (guard, result) = state.signal.wait_timeout_while(guard, timeout, |value| value.is_none()).ok()?;
        let value = *guard;
        drop(guard);
        if result.timed_out() {
            return None; // the callback's reference is leaked, see the doc comment of `selftest`
        }
        // Give a second call the chance to show itself while the state is still alive.
        std::thread::sleep(Duration::from_millis(20));
        if state.calls.load(Ordering::SeqCst) != 1 {
            return None; // leaked too: a further call must still find live memory
        }
        // SAFETY: `raw` came from `Arc::into_raw` above and has not been released; exactly one callback was seen, and
        // none can follow, so nothing reads it any more.
        drop(unsafe { Arc::from_raw(raw as *const State) });
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_map_to_the_documented_states() {
        assert_eq!(Availability::from_code(0), Availability::Available);
        assert_eq!(Availability::from_code(1), Availability::AppleIntelligenceNotEnabled);
        assert_eq!(Availability::from_code(2), Availability::DeviceNotEligible);
        assert_eq!(Availability::from_code(3), Availability::ModelNotReady);
        assert_eq!(Availability::from_code(4), Availability::Other);
        assert_eq!(Availability::from_code(5), Availability::OsTooOld);
        assert_eq!(Availability::from_code(6), Availability::SdkLacksFramework);
    }

    #[test]
    fn an_unknown_code_is_other_never_available() {
        for code in [-1, 7, 100, i32::MAX, i32::MIN] {
            assert_eq!(Availability::from_code(code), Availability::Other, "code {code}");
            assert!(!Availability::from_code(code).is_available(), "code {code}");
        }
        assert!(Availability::Available.is_available());
        assert!(!Availability::UnsupportedPlatform.is_available());
    }

    #[test]
    fn keys_are_unique_snake_case_and_stable() {
        let all = [
            (Availability::Available, "available"),
            (Availability::AppleIntelligenceNotEnabled, "apple_intelligence_not_enabled"),
            (Availability::DeviceNotEligible, "device_not_eligible"),
            (Availability::ModelNotReady, "model_not_ready"),
            (Availability::Other, "other"),
            (Availability::OsTooOld, "os_too_old"),
            (Availability::SdkLacksFramework, "sdk_lacks_framework"),
            (Availability::UnsupportedPlatform, "unsupported_platform"),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for (state, key) in all {
            assert_eq!(state.as_str(), key);
            assert!(key.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "{key} is not snake_case");
            assert!(seen.insert(key), "{key} appears twice");
        }
        assert_eq!(seen.len(), 8);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn probe_reports_unsupported_platform_off_macos() {
        assert_eq!(probe(), Availability::UnsupportedPlatform);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn probe_reports_a_defined_state_on_this_mac() {
        let output = std::process::Command::new("sw_vers").arg("-productVersion").output().expect("sw_vers runs");
        let version = String::from_utf8_lossy(&output.stdout);
        let major: u32 = version.trim().split('.').next().and_then(|m| m.parse().ok()).expect("a macOS version number");
        let state = probe();
        println!("macOS {} sdk_has_framework={} probe={}", version.trim(), cfg!(apple_ai_sdk_foundation_models), state.as_str());
        if !cfg!(apple_ai_sdk_foundation_models) {
            assert_eq!(state, Availability::SdkLacksFramework);
        } else if major < 26 {
            assert_eq!(state, Availability::OsTooOld);
        } else {
            assert!(
                matches!(
                    state,
                    Availability::Available
                        | Availability::AppleIntelligenceNotEnabled
                        | Availability::DeviceNotEligible
                        | Availability::ModelNotReady
                        | Availability::Other
                ),
                "unexpected state {state:?} on macOS {major} with the framework in the SDK"
            );
        }
    }
}
