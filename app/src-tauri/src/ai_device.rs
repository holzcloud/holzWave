//! The shell's view of the on-device model: the state word it reports, the provider it registers and the fixed ids.
//!
//! The model exists only on a Mac with Apple Intelligence. The shell asks one question of it, "can you answer now", and
//! the answer is a pair of words: a state (`available`, `apple_intelligence_not_enabled`, ...) and the advice that fits
//! it. [`DeviceSource`] is that seam; the macOS source sits over the bridge crate, and the tests put a scripted one in
//! its place. This file carries no consent or tool logic, makes no network call and contains no foreign-function code.
//!
//! The availability is asked only through [`DeviceSource::probe`], and only when a person acts: the host does it when the
//! list of providers is read, when a send or a context call names the model, and when a request of `wsh ai` needs a
//! provider (`ai_host.rs`). Nothing is asked at construction, at launch or from a timer.

use std::sync::Arc;

use holzwave_core::ai::provider::Provider;
use serde::Serialize;

/// The id of the on-device provider, the one the core reserves.
pub const DEVICE_ID: &str = "apple";
/// The only model the on-device provider has.
pub const DEVICE_MODEL: &str = "system";
/// The one state word that registers the provider and offers the row.
pub const AVAILABLE: &str = "available";

/// What the model said when it was asked: a state word and an advice word. Both are fixed keys of the bridge, and only the
/// exact state word [`AVAILABLE`] counts as available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceState {
    pub state: &'static str,
    pub advice: &'static str,
}

impl DeviceState {
    /// True for the exact word `available` and for nothing else: another case, a blank or an unknown word is not.
    pub fn is_available(&self) -> bool {
        self.state == AVAILABLE
    }
}

/// Where the on-device model comes from. Implemented over the bridge crate on macOS and by a scripted double in tests.
pub(crate) trait DeviceSource: Send + Sync {
    /// Asks whether the model can answer now. Synchronous, no network, not cached: the answer can change while the app
    /// runs, so ask again when it matters.
    fn probe(&self) -> DeviceState;

    /// The provider to register under [`DEVICE_ID`]. Called once, when the state is first found to be available. The
    /// provider asks again on every turn and refuses by itself when the model went away.
    fn provider(&self) -> Arc<dyn Provider>;
}

/// The member of the answer of `ai_profiles` that tells the page about the model: `id`, `model`, `state`, `advice`, in
/// that order. The words are keys; the page chooses its own sentences by them and never shows them as they are.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeviceView {
    pub id: &'static str,
    pub model: &'static str,
    pub state: &'static str,
    pub advice: &'static str,
}

impl DeviceView {
    /// The view of a state: the fixed ids and the two words.
    pub fn of(state: DeviceState) -> DeviceView {
        DeviceView { id: DEVICE_ID, model: DEVICE_MODEL, state: state.state, advice: state.advice }
    }
}

/// The model of a call. A call for the on-device provider that names no model (or a blank one) gets `system`, because a
/// registered provider's own default is its id and the provider refuses a model it does not have. A model that is named
/// stays as it is, so the provider can refuse it itself; every other profile keeps what it was given.
pub fn fill_model(profile: Option<&str>, model: Option<String>) -> Option<String> {
    let unnamed = model.as_deref().is_none_or(|named| named.trim().is_empty());
    if profile == Some(DEVICE_ID) && unnamed { Some(DEVICE_MODEL.to_owned()) } else { model }
}

/// The source of this platform: over the bridge crate on macOS, none elsewhere (there is no model and nothing is drawn).
pub(crate) fn system() -> Option<Arc<dyn DeviceSource>> {
    #[cfg(target_os = "macos")]
    {
        Some(Arc::new(AppleDevice::system()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The on-device model over a backend of the bridge crate: the system's own on this Mac, a scripted one in tests. The state
/// and the advice words are the bridge's own keys; the provider asks the backend again on every turn and refuses by itself
/// with the class `unavailable` when the model went away.
#[cfg(target_os = "macos")]
pub(crate) struct AppleDevice {
    backend: Arc<dyn holzwave_apple_ai::Backend>,
}

#[cfg(target_os = "macos")]
impl AppleDevice {
    /// A source over `backend`.
    pub(crate) fn new(backend: Arc<dyn holzwave_apple_ai::Backend>) -> AppleDevice {
        AppleDevice { backend }
    }

    /// A source over the system model of this Mac.
    pub(crate) fn system() -> AppleDevice {
        AppleDevice::new(Arc::new(holzwave_apple_ai::SystemBackend::new()))
    }
}

#[cfg(target_os = "macos")]
impl DeviceSource for AppleDevice {
    fn probe(&self) -> DeviceState {
        let availability = self.backend.availability();
        DeviceState { state: availability.as_str(), advice: availability.advice().as_str() }
    }

    fn provider(&self) -> Arc<dyn Provider> {
        Arc::new(holzwave_apple_ai::AppleProvider::new(self.backend.clone()))
    }
}

/// A source for the tests: the state is set by the test, the probes and the provider constructions are counted, and the
/// provider is a scripted stand-in named `apple` that answers `Hello from the device`.
#[cfg(test)]
pub(crate) struct ScriptedDevice {
    state: std::sync::Mutex<DeviceState>,
    probes: std::sync::atomic::AtomicUsize,
    provider_calls: std::sync::atomic::AtomicUsize,
    provider: Arc<holzwave_core::ai::scripted::ScriptedProvider>,
}

#[cfg(test)]
impl ScriptedDevice {
    /// A source in `state` whose stand-in provider has one text turn.
    pub(crate) fn new(state: &'static str, advice: &'static str) -> ScriptedDevice {
        ScriptedDevice::with_turns(state, advice, 1)
    }

    /// A source in `state` whose stand-in provider has `turns` text turns.
    pub(crate) fn with_turns(state: &'static str, advice: &'static str, turns: usize) -> ScriptedDevice {
        use holzwave_core::ai::scripted::{Script, ScriptedProvider};
        use holzwave_core::ai::types::{Capabilities, Locality};
        let mut provider =
            ScriptedProvider::new(DEVICE_ID).with_capabilities(Capabilities::conservative(Locality::OnDevice));
        for _ in 0..turns {
            provider = provider.with_turn(Script::new().text("Hello from the device").usage(3, 4));
        }
        ScriptedDevice {
            state: std::sync::Mutex::new(DeviceState { state, advice }),
            probes: std::sync::atomic::AtomicUsize::new(0),
            provider_calls: std::sync::atomic::AtomicUsize::new(0),
            provider: Arc::new(provider),
        }
    }

    /// Changes what the next probe answers.
    pub(crate) fn set(&self, state: &'static str, advice: &'static str) {
        *self.state.lock().expect("state") = DeviceState { state, advice };
    }

    /// How many times the availability was asked.
    pub(crate) fn probes(&self) -> usize {
        self.probes.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many times the provider was asked for.
    pub(crate) fn provider_calls(&self) -> usize {
        self.provider_calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The stand-in provider, to read what it was asked.
    pub(crate) fn scripted(&self) -> Arc<holzwave_core::ai::scripted::ScriptedProvider> {
        self.provider.clone()
    }
}

#[cfg(test)]
impl DeviceSource for ScriptedDevice {
    fn probe(&self) -> DeviceState {
        self.probes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.state.lock().expect("state")
    }

    fn provider(&self) -> Arc<dyn Provider> {
        self.provider_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.provider.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use holzwave_core::ai::profiles::RESERVED_IDS;

    #[test]
    fn the_device_view_has_a_fixed_key_order() {
        let view = DeviceView::of(DeviceState { state: "model_not_ready", advice: "wait_for_model" });
        assert_eq!(
            serde_json::to_string(&view).expect("serialises"),
            r#"{"id":"apple","model":"system","state":"model_not_ready","advice":"wait_for_model"}"#
        );
    }

    #[test]
    fn the_fixed_model_fills_only_the_device_profile() {
        assert_eq!(fill_model(Some("apple"), None), Some("system".to_owned()));
        assert_eq!(fill_model(Some("apple"), Some(String::new())), Some("system".to_owned()));
        assert_eq!(fill_model(Some("apple"), Some("  ".to_owned())), Some("system".to_owned()));
        assert_eq!(fill_model(Some("apple"), Some("other".to_owned())), Some("other".to_owned()));
        assert_eq!(fill_model(Some("local"), None), None);
        assert_eq!(fill_model(None, None), None);
        assert_eq!(fill_model(Some("local"), Some("llama".to_owned())), Some("llama".to_owned()));
        assert_eq!(fill_model(None, Some(String::new())), Some(String::new()));
    }

    #[test]
    fn the_device_ids_are_the_reserved_profile_id() {
        assert!(RESERVED_IDS.contains(&DEVICE_ID), "the core reserves the id of the on-device provider");
        assert_eq!((DEVICE_ID, DEVICE_MODEL, AVAILABLE), ("apple", "system", "available"));
    }

    /// The tests over the bridge crate: only a Mac builds it.
    #[cfg(target_os = "macos")]
    mod mac {
        use super::*;

        use std::path::PathBuf;
        use std::sync::Mutex;
        use std::time::{Duration, Instant};

        use holzwave_apple_ai::{Availability, BackendScript, MODEL_ID, PROVIDER_ID, ScriptedBackend};
        use holzwave_core::ai::profiles::KeyVault;
        use holzwave_core::layout::MAIN_WINDOW;
        use serde_json::{Value, json};

        use crate::ai_host::AiParts;
        use crate::state::{AppCore, Out, ShellSpec, Sink, Target};
        use crate::windows::{CursorSpot, Place};

        /// Every state of the bridge, in declaration order.
        const STATES: [Availability; 8] = [
            Availability::Available,
            Availability::AppleIntelligenceNotEnabled,
            Availability::DeviceNotEligible,
            Availability::ModelNotReady,
            Availability::Other,
            Availability::OsTooOld,
            Availability::SdkLacksFramework,
            Availability::UnsupportedPlatform,
        ];

        /// Keeps every `ai` event the shell sends, as the page receives it.
        #[derive(Default)]
        struct Recorder {
            events: Mutex<Vec<Value>>,
        }

        impl Sink for Recorder {
            fn send(&self, _target: Target, out: Out) {
                if let Out::Ai(payload) = out {
                    self.events.lock().expect("events").push(serde_json::to_value(&payload).expect("serialises"));
                }
            }

            fn close_window(&self, _label: &str) {}

            fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
                Ok(())
            }

            fn cursor_spot(&self, _source: &str) -> CursorSpot {
                CursorSpot::Unknown
            }
        }

        impl Recorder {
            fn of(&self, chat: &str, kind: &str) -> Vec<Value> {
                let all = self.events.lock().expect("events");
                all.iter().filter(|event| event["conversation"] == json!(chat) && event["kind"] == json!(kind)).cloned().collect()
            }
        }

        fn wait_for(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
            let deadline = Instant::now() + timeout;
            while !f() {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            true
        }

        fn scratch(tag: &str) -> PathBuf {
            let path = std::env::temp_dir().join(format!("holzwave-device-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch dir");
            path
        }

        fn fixed_clock() -> i64 {
            1_800_000_000
        }

        #[test]
        fn the_device_ids_agree_with_the_crate() {
            assert_eq!(DEVICE_ID, PROVIDER_ID);
            assert_eq!(DEVICE_MODEL, MODEL_ID);
        }

        #[test]
        fn the_system_device_reports_what_the_crate_reports() {
            let device = AppleDevice::system();
            let seen = holzwave_apple_ai::probe();
            let state = device.probe();
            assert_eq!(state.state, seen.as_str());
            assert_eq!(state.advice, seen.advice().as_str());
            assert_eq!(device.provider().id(), "apple");
        }

        #[test]
        fn the_apple_device_over_a_flipping_backend_follows_every_state() {
            let dir = scratch("flip");
            let rec = Arc::new(Recorder::default());
            let core = AppCore::new_with_ai(
                dir.clone(),
                rec.clone(),
                ShellSpec::new("/bin/cat", &[]),
                AiParts { vault: KeyVault::unavailable(), clock: Some(fixed_clock), protected: Vec::new() },
            );
            let backend = Arc::new(ScriptedBackend::new().with_turn(BackendScript::reply("Hello from the model")));
            assert!(core.ai().use_device(Arc::new(AppleDevice::new(backend.clone()))));
            let pane = core.view_in(MAIN_WINDOW).expect("main").focus;

            // Every state is reported with its own words; the provider is registered once, at the first available one.
            for state in STATES {
                backend.set_availability(state);
                let answer = serde_json::to_value(core.ai().profiles()).expect("serialises");
                assert_eq!(
                    answer["on_device"],
                    json!({"advice": state.advice().as_str(), "id": "apple", "model": "system", "state": state.as_str()})
                );
            }
            assert_eq!(core.ai().service().register_provider("apple", AppleDevice::system().provider()).expect_err("taken").code(), "id_taken");

            // With the model available a send goes through the provider.
            backend.set_availability(Availability::Available);
            let chat = core.ai().conversation_new().expect("a chat id");
            core.ai_send(&chat, "hi".to_owned(), pane, Some("apple".to_owned()), None).expect("sent");
            assert!(wait_for(Duration::from_secs(5), || !rec.of(&chat, "turn_ended").is_empty()), "the turn ends");
            let started = rec.of(&chat, "turn_started").remove(0);
            assert_eq!(started["route"], json!({"profile": "apple", "model": "system", "host": null, "locality": "on_device"}));
            let text = serde_json::to_string(&core.ai().snapshot(&chat).expect("a snapshot")).expect("serialises");
            assert!(text.contains("Hello from the model"), "{text}");
            assert_eq!(backend.calls(), 1);

            // The model goes away: the list says so, the send is accepted, and the turn ends by itself as `unavailable`.
            backend.set_availability(Availability::AppleIntelligenceNotEnabled);
            let answer = serde_json::to_value(core.ai().profiles()).expect("serialises");
            assert_eq!(answer["on_device"]["state"], json!("apple_intelligence_not_enabled"));
            let again = core.ai().conversation_new().expect("a chat id");
            core.ai_send(&again, "hi".to_owned(), pane, Some("apple".to_owned()), None).expect("accepted");
            assert!(wait_for(Duration::from_secs(5), || !rec.of(&again, "turn_ended").is_empty()), "the turn ends");
            let ended = rec.of(&again, "turn_ended").remove(0);
            assert_eq!(ended["end"], json!("failed"));
            assert_eq!(ended["error"]["class"], json!("unavailable"));
            assert_eq!(backend.calls(), 1, "the refused turn did not reach the backend");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
