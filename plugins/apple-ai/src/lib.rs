//! The Foundation Models bridge of holzWave: a first-party Swift package behind a small C ABI, built from this crate's
//! build script, for macOS only.
//!
//! The crate asks one question, whether Apple's on-device language model is available, and answers it as an
//! [`Availability`]. It opens no socket and knows no provider. Off macOS it is plain Rust: [`probe`] reports
//! [`Availability::UnsupportedPlatform`] without calling a foreign function and no Swift tool is needed to build it.
//!
//! The framework is linked weakly, so a process that contains this crate still starts on a Mac whose operating system
//! has no FoundationModels framework (see `build.rs`).

#![deny(unsafe_op_in_unsafe_fn)]

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

/// The only place that crosses into Swift, and the only `unsafe` code of the crate.
#[cfg(target_os = "macos")]
mod ffi {
    unsafe extern "C" {
        fn hw_afm_availability() -> i32;
    }

    pub(crate) fn availability() -> i32 {
        // SAFETY: the function takes no arguments, passes no pointers and returns a plain integer; it is defined by
        // `@_cdecl("hw_afm_availability")` in the Swift package and linked from the static library of build.rs.
        unsafe { hw_afm_availability() }
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
