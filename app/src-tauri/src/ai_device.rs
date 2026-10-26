//! The macOS source of the on-device model: the one place that touches the bridge crate `holzwave-apple-ai`. The seam
//! ([`DeviceSource`]), the state words and the view are in [`holzwave_host::ai_device`]. Windows and Linux have no model:
//! [`system`] answers `None` there, and nothing is drawn. This file contains no foreign-function code.

use std::sync::Arc;

#[cfg(target_os = "macos")]
use holzwave_core::ai::provider::Provider;
use holzwave_host::ai_device::DeviceSource;
#[cfg(target_os = "macos")]
use holzwave_host::ai_device::DeviceState;
#[cfg(all(test, target_os = "macos"))]
use holzwave_host::ai_device::{AVAILABLE, DEVICE_ID, DEVICE_MODEL};

/// The source of this platform: over the bridge crate on macOS, none elsewhere (there is no model and nothing is drawn).
pub fn system() -> Option<Arc<dyn DeviceSource>> {
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
    pub fn system() -> AppleDevice {
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

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

        use holzwave_host::ai_host::AiParts;
        use holzwave_host::state::{AppCore, Out, ShellSpec, Sink, Target};
        use holzwave_host::windows::{CursorSpot, Place};

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

/// The words of the shell for every state of the bridge, held to the bridge's own keys. Only a Mac builds the bridge.
#[cfg(all(test, target_os = "macos"))]
mod device_states {
    use super::*;

    use holzwave_apple_ai::{Availability, BackendScript, ScriptedBackend};
    use holzwave_core::ai::provider::{CancelToken, ChatRequest};
    use holzwave_core::ai::types::Message;

    /// The seven states the shell can be given by the bridge on a Mac, and the one it never is (the bridge answers it
    /// only where it was not built for the platform).
    const SENT: [Availability; 7] = [
        Availability::Available,
        Availability::AppleIntelligenceNotEnabled,
        Availability::DeviceNotEligible,
        Availability::ModelNotReady,
        Availability::Other,
        Availability::OsTooOld,
        Availability::SdkLacksFramework,
    ];

    /// A one-word request for the model of the device.
    fn request() -> ChatRequest {
        let mut request = ChatRequest::new(DEVICE_MODEL);
        request.messages.push(Message::user_text("m1", "hi", 1));
        request
    }

    #[test]
    fn apple_device_reports_the_words_of_every_backend_state() {
        for state in SENT {
            let backend = Arc::new(ScriptedBackend::new().with_availability(state).with_turn(BackendScript::reply("ok")));
            let device = AppleDevice::new(backend.clone());
            let probed = device.probe();
            assert_eq!(probed.state, state.as_str(), "{}: the state word", state.as_str());
            assert_eq!(probed.advice, state.advice().as_str(), "{}: the advice word", state.as_str());
            assert_eq!(probed.is_available(), probed.state == AVAILABLE, "{}: only the exact word counts", state.as_str());
            assert_eq!(probed.is_available(), state == Availability::Available, "{}: availability", state.as_str());

            let provider = device.provider();
            let result = provider.stream(&request(), &mut |_| holzwave_core::ai::provider::Flow::Continue, &CancelToken::new());
            if state == Availability::Available {
                assert!(result.is_ok(), "{}: the provider refused: {:?}", state.as_str(), result.err());
                assert_eq!(backend.calls(), 1, "{}: one backend call", state.as_str());
            } else {
                let error = result.expect_err("an unavailable model refuses the turn");
                assert_eq!(error.class().as_str(), "unavailable", "{}: the class", state.as_str());
                assert_eq!(backend.calls(), 0, "{}: the backend ran", state.as_str());
            }
        }

        // The state the shell never sends is not available.
        let unsent = ScriptedBackend::new().with_availability(Availability::UnsupportedPlatform);
        let probed = AppleDevice::new(Arc::new(unsent)).probe();
        assert!(!probed.is_available(), "the state the shell never sends is not available");
    }
}
