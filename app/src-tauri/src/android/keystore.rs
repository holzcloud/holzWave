//! The platform half of the secret store, seen from Rust: [`AndroidKeystore`] is the vault's `Keystore` over the Kotlin
//! plugin, and [`AndroidClock`] is its `Clock` over the plugin's elapsed-realtime call.
//!
//! Both talk through a [`Bridge`], one call at a time: a command name and a JSON value in, a JSON value or a fixed failure
//! word out. On Android the bridge is the plugin handle (`run_mobile_plugin`, which blocks until Kotlin answers); in the
//! unit tests it is a recorder, so the commands, the shapes of what crosses, the mapping of every failure and the rules of the
//! cache and of the clock are proved on the desktop. A call that may wait for the person (`keystoreWrap` and `keystoreUnwrap`
//! under a protected key) is made from a worker thread only, which the vault's callers already do.
//!
//! What crosses holds the 32-byte data key as a JSON array of numbers, one way when it is wrapped and the other when it is
//! unwrapped. The arrays are zeroed here as soon as the call is over, and the answers are read only through
//! `host::keystore_wire`, which is strict and never carries received text into an error.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use holzwave_core::vault::{Clock, Keystore, KeystoreCaps, KeystoreError, Protection, Zeroizing};
use holzwave_host::keystore_wire::{
    FAILED_TEXT, MAX_KEY_BYTES, MAX_WRAPPED_BYTES, bytes_from_wire, bytes_to_wire, caps_from_wire, keystore_error,
    millis_from_wire, protection_to_wire,
};
use serde_json::{Value, json};

/// The commands of the Kotlin plugin that the adapters use.
pub const CAPS: &str = "keystoreCaps";
pub const GENERATE: &str = "keystoreGenerate";
pub const WRAP: &str = "keystoreWrap";
pub const UNWRAP: &str = "keystoreUnwrap";
pub const RETIRE: &str = "keystoreRetire";
pub const DELETE: &str = "keystoreDelete";
pub const ELAPSED: &str = "elapsedRealtime";

/// How long the answer of `keystoreCaps` is kept. The status of the vault asks for it at every look and the page looks often;
/// two seconds is short enough that a removed screen lock shows quickly, and the answer is dropped at once by every call that
/// changes a key.
const CAPS_TTL: Duration = Duration::from_secs(2);

/// One call into the Kotlin plugin.
pub trait Bridge: Send + Sync {
    /// Run `command` with `payload` and wait for the answer. A rejection is the failure its code names (`keystore_error`);
    /// anything else that goes wrong is the fixed `failed`.
    fn call(&self, command: &str, payload: &Value) -> Result<Value, KeystoreError>;
}

/// The bridge on Android: the handle of the plugin the app registered.
#[cfg(target_os = "android")]
pub struct PluginBridge;

#[cfg(target_os = "android")]
impl Bridge for PluginBridge {
    fn call(&self, command: &str, payload: &Value) -> Result<Value, KeystoreError> {
        use tauri::plugin::mobile::PluginInvokeError;

        let Some(handle) = crate::android::plugin::handle() else {
            return Err(KeystoreError::Failed(FAILED_TEXT.to_owned()));
        };
        match handle.run_mobile_plugin::<Value>(command, payload) {
            Ok(answer) => Ok(answer),
            Err(PluginInvokeError::InvokeRejected(rejected)) => Err(keystore_error(rejected.code.as_deref().unwrap_or(""))),
            Err(_) => Err(KeystoreError::Failed(FAILED_TEXT.to_owned())),
        }
    }
}

/// Overwrite every number of a JSON array with zero. The value is ours (built for one call or read from one answer), so
/// nothing else refers to it.
fn scrub(value: &mut Value) {
    if let Value::Array(items) = value {
        for item in items.iter_mut() {
            *item = Value::from(0u8);
        }
    }
}

/// The vault's `Keystore` over the Kotlin plugin.
pub struct AndroidKeystore {
    bridge: Arc<dyn Bridge>,
    caps: Mutex<Option<(Instant, KeystoreCaps)>>,
}

impl AndroidKeystore {
    pub fn new(bridge: Arc<dyn Bridge>) -> AndroidKeystore {
        AndroidKeystore { bridge, caps: Mutex::new(None) }
    }

    fn forget_caps(&self) {
        *self.caps.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

impl Keystore for AndroidKeystore {
    /// What the device can do. A device that does not answer is one with no screen lock and no secure chip: the vault then
    /// makes no protected key, which is the safe reading.
    fn caps(&self) -> KeystoreCaps {
        let mut cached = self.caps.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, caps)) = *cached {
            if at.elapsed() < CAPS_TTL {
                return caps;
            }
        }
        let caps = self
            .bridge
            .call(CAPS, &json!({}))
            .and_then(|answer| caps_from_wire(&answer))
            .unwrap_or(KeystoreCaps { has_screen_lock: false, strongbox: false });
        *cached = Some((Instant::now(), caps));
        caps
    }

    fn generate(&self, protection: Protection) -> Result<(), KeystoreError> {
        self.forget_caps();
        let made = self.bridge.call(GENERATE, &json!({ "protection": protection_to_wire(protection) })).map(|_| ());
        self.forget_caps();
        made
    }

    fn wrap(&self, plain: &[u8]) -> Result<Vec<u8>, KeystoreError> {
        let mut payload = json!({ "plain": bytes_to_wire(plain) });
        let answer = self.bridge.call(WRAP, &payload);
        scrub(&mut payload["plain"]);
        let answer = answer?;
        bytes_from_wire(answer.get("wrapped").unwrap_or(&Value::Null), MAX_WRAPPED_BYTES)
    }

    fn unwrap(&self, wrapped: &[u8]) -> Result<Zeroizing<Vec<u8>>, KeystoreError> {
        if wrapped.len() > MAX_WRAPPED_BYTES {
            return Err(KeystoreError::Invalid);
        }
        let mut answer = self.bridge.call(UNWRAP, &json!({ "wrapped": bytes_to_wire(wrapped) }))?;
        let plain = bytes_from_wire(answer.get("plain").unwrap_or(&Value::Null), MAX_KEY_BYTES);
        scrub(&mut answer["plain"]);
        Ok(Zeroizing::new(plain?))
    }

    fn retire_previous(&self) -> Result<(), KeystoreError> {
        self.bridge.call(RETIRE, &json!({})).map(|_| ())
    }

    fn delete(&self) -> Result<(), KeystoreError> {
        self.forget_caps();
        let deleted = self.bridge.call(DELETE, &json!({})).map(|_| ());
        self.forget_caps();
        deleted
    }
}

/// The vault's `Clock` over the plugin's `elapsedRealtime`: milliseconds since the device started, which keep counting while
/// the phone sleeps. The monotonic clock of the process (`Instant`) stops in suspend, and a phone asleep in a pocket would
/// then stay unlocked for hours; it is never used here.
///
/// When the plugin does not answer (it is not registered yet, or Kotlin failed) the clock returns the last value it read, so
/// it never goes back and a vault never locks by a misread; the first answer comes with the first call that reaches the
/// plugin. Nothing but the plugin is a source of time.
pub struct AndroidClock {
    bridge: Arc<dyn Bridge>,
    last: AtomicU64,
    complained: AtomicBool,
}

impl AndroidClock {
    pub fn new(bridge: Arc<dyn Bridge>) -> AndroidClock {
        AndroidClock { bridge, last: AtomicU64::new(0), complained: AtomicBool::new(false) }
    }
}

impl Clock for AndroidClock {
    fn now_ms(&self) -> u64 {
        match self.bridge.call(ELAPSED, &json!({})).and_then(|answer| millis_from_wire(&answer)) {
            Ok(ms) => {
                // Never back, whatever the plugin says.
                self.last.fetch_max(ms, Ordering::SeqCst);
                self.last.load(Ordering::SeqCst)
            }
            Err(_) => {
                if !self.complained.swap(true, Ordering::SeqCst) {
                    eprintln!("holzwave: the device clock did not answer; the secret store keeps the last time it read");
                }
                self.last.load(Ordering::SeqCst)
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A bridge that records every call and answers from a script.
    #[derive(Default)]
    pub struct Recorder {
        pub calls: Mutex<Vec<(String, Value)>>,
        pub answers: Mutex<Vec<(String, Result<Value, KeystoreError>)>>,
    }

    impl Recorder {
        /// The answer of the next call of `command` (the last scripted one stays for later calls).
        pub fn answer(&self, command: &str, answer: Result<Value, KeystoreError>) {
            self.answers.lock().expect("answers").push((command.to_owned(), answer));
        }

        pub fn count(&self, command: &str) -> usize {
            self.calls.lock().expect("calls").iter().filter(|(c, _)| c == command).count()
        }

        pub fn payloads(&self, command: &str) -> Vec<Value> {
            self.calls.lock().expect("calls").iter().filter(|(c, _)| c == command).map(|(_, p)| p.clone()).collect()
        }
    }

    impl Bridge for Recorder {
        fn call(&self, command: &str, payload: &Value) -> Result<Value, KeystoreError> {
            self.calls.lock().expect("calls").push((command.to_owned(), payload.clone()));
            let answers = self.answers.lock().expect("answers");
            answers
                .iter()
                .rev()
                .find(|(c, _)| c == command)
                .map(|(_, a)| a.clone())
                .unwrap_or_else(|| Err(KeystoreError::Failed(FAILED_TEXT.to_owned())))
        }
    }

    fn failed() -> KeystoreError {
        KeystoreError::Failed(FAILED_TEXT.to_owned())
    }

    fn keystore(recorder: &Arc<Recorder>) -> AndroidKeystore {
        AndroidKeystore::new(recorder.clone())
    }

    #[test]
    fn caps_come_from_the_device_and_are_kept_for_a_moment() {
        let recorder = Arc::new(Recorder::default());
        recorder.answer(CAPS, Ok(json!({ "has_screen_lock": true, "strongbox": false })));
        let store = keystore(&recorder);
        assert_eq!(store.caps(), KeystoreCaps { has_screen_lock: true, strongbox: false });
        assert_eq!(store.caps(), KeystoreCaps { has_screen_lock: true, strongbox: false });
        assert_eq!(recorder.count(CAPS), 1, "the second look is the kept answer");
        // A call that changes a key drops the answer, so the next look asks again.
        recorder.answer(GENERATE, Ok(Value::Null));
        store.generate(Protection::Plain).expect("generate");
        recorder.answer(CAPS, Ok(json!({ "has_screen_lock": true, "strongbox": true })));
        assert_eq!(store.caps(), KeystoreCaps { has_screen_lock: true, strongbox: true });
        assert_eq!(recorder.count(CAPS), 2);
    }

    #[test]
    fn a_device_that_does_not_answer_has_no_lock_and_no_chip() {
        let recorder = Arc::new(Recorder::default());
        let store = keystore(&recorder);
        assert_eq!(store.caps(), KeystoreCaps { has_screen_lock: false, strongbox: false });
        recorder.answer(CAPS, Ok(json!({ "has_screen_lock": "yes" })));
        std::thread::sleep(CAPS_TTL + Duration::from_millis(50));
        assert_eq!(store.caps(), KeystoreCaps { has_screen_lock: false, strongbox: false }, "a misread answer is never true");
    }

    #[test]
    fn the_calls_carry_the_protection_and_the_bytes_as_arrays_and_read_the_answers_strictly() {
        let recorder = Arc::new(Recorder::default());
        let store = keystore(&recorder);
        recorder.answer(GENERATE, Ok(Value::Null));
        store.generate(Protection::Auth).expect("auth");
        store.generate(Protection::Plain).expect("plain");
        assert_eq!(recorder.payloads(GENERATE), vec![json!({ "protection": "auth" }), json!({ "protection": "plain" })]);

        recorder.answer(WRAP, Ok(json!({ "wrapped": [0, 0, 0, 1, 9, 9] })));
        assert_eq!(store.wrap(&[7u8; 32]).expect("wrap"), vec![0, 0, 0, 1, 9, 9]);
        let sent = recorder.payloads(WRAP);
        assert_eq!(sent[0]["plain"].as_array().map(Vec::len), Some(32));
        recorder.answer(WRAP, Ok(json!({ "wrapped": "text" })));
        assert!(matches!(store.wrap(&[7u8; 32]), Err(KeystoreError::Failed(_))));
        recorder.answer(WRAP, Ok(json!({})));
        assert!(matches!(store.wrap(&[7u8; 32]), Err(KeystoreError::Failed(_))));

        recorder.answer(UNWRAP, Ok(json!({ "plain": vec![5u8; 32] })));
        assert_eq!(&**store.unwrap(&[1, 2, 3]).expect("unwrap"), &vec![5u8; 32]);
        assert_eq!(recorder.payloads(UNWRAP)[0], json!({ "wrapped": [1, 2, 3] }));
        // An answer longer than a data key is refused, not truncated.
        recorder.answer(UNWRAP, Ok(json!({ "plain": vec![5u8; 33] })));
        assert!(matches!(store.unwrap(&[1, 2, 3]), Err(KeystoreError::Failed(_))));
        // A value longer than any wrapped key is refused before it is sent.
        let calls = recorder.count(UNWRAP);
        assert_eq!(store.unwrap(&vec![0u8; MAX_WRAPPED_BYTES + 1]).err(), Some(KeystoreError::Invalid));
        assert_eq!(recorder.count(UNWRAP), calls);
    }

    #[test]
    fn every_rejection_is_the_failure_its_word_names() {
        let recorder = Arc::new(Recorder::default());
        let store = keystore(&recorder);
        for (word, want) in [
            ("cancelled", KeystoreError::Cancelled),
            ("key_gone", KeystoreError::KeyGone),
            ("no_screen_lock", KeystoreError::NoScreenLock),
            ("invalid", KeystoreError::Invalid),
            ("failed", KeystoreError::Failed(FAILED_TEXT.to_owned())),
            ("javax.crypto.AEADBadTagException at /data/x", KeystoreError::Failed(FAILED_TEXT.to_owned())),
        ] {
            recorder.answer(UNWRAP, Err(keystore_error(word)));
            assert_eq!(store.unwrap(&[1, 2, 3]).err(), Some(want), "{word}");
        }
    }

    #[test]
    fn retire_and_delete_call_their_commands_and_delete_drops_the_kept_caps() {
        let recorder = Arc::new(Recorder::default());
        recorder.answer(RETIRE, Ok(Value::Null));
        recorder.answer(DELETE, Ok(Value::Null));
        recorder.answer(CAPS, Ok(json!({ "has_screen_lock": true, "strongbox": false })));
        let store = keystore(&recorder);
        store.caps();
        store.retire_previous().expect("retire");
        store.caps();
        assert_eq!(recorder.count(CAPS), 1, "retire changes no capability");
        store.delete().expect("delete");
        store.caps();
        assert_eq!((recorder.count(RETIRE), recorder.count(DELETE), recorder.count(CAPS)), (1, 1, 2));
    }

    #[test]
    fn the_clock_reads_the_device_clock_and_never_goes_back() {
        let recorder = Arc::new(Recorder::default());
        let clock = AndroidClock::new(recorder.clone());
        assert_eq!(clock.now_ms(), 0, "nothing read yet, nothing invented");
        recorder.answer(ELAPSED, Ok(json!({ "ms": 5_000 })));
        assert_eq!(clock.now_ms(), 5_000);
        recorder.answer(ELAPSED, Ok(json!({ "ms": 4_000 })));
        assert_eq!(clock.now_ms(), 5_000, "a smaller answer is not taken");
        recorder.answer(ELAPSED, Ok(json!({ "ms": 9_000 })));
        assert_eq!(clock.now_ms(), 9_000);
        recorder.answer(ELAPSED, Err(failed()));
        assert_eq!(clock.now_ms(), 9_000, "no answer keeps the last value");
        // It is the plugin's call each time, never the process's own monotonic clock, which stops in suspend.
        assert_eq!(recorder.count(ELAPSED), 5);
    }

    #[test]
    fn the_data_key_is_scrubbed_from_the_value_that_carried_it() {
        let mut value = json!([1, 2, 3, 200]);
        scrub(&mut value);
        assert_eq!(value, json!([0, 0, 0, 0]));
        let mut other = json!("not an array");
        scrub(&mut other);
        assert_eq!(other, json!("not an array"));
    }
}
