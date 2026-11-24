//! [`AndroidOpener`]: the host's link opener over the Kotlin plugin, which hands an address to the system with an
//! `ACTION_VIEW` intent.
//!
//! The opener holds no state: an open is one command of the plugin, made through the same [`Bridge`] as the keystore and the
//! picker (the plugin handle on Android, a recorder in the unit tests, so the command, the shape that crosses and the mapping
//! of every answer are proved on the desktop). The host has checked the address before it asks; the plugin checks the scheme
//! and the length again on its own side. The answer is one of two fixed words, `opened` and `no_app`; a rejection of the
//! plugin, a missing command and every other answer are the one fixed [`OpenError::Failed`], and no text of the plugin or of
//! the system reaches an error.

use std::sync::Arc;

use holzwave_host::seams::{LinkOpener, OpenError};
use serde_json::{Value, json};

use crate::android::keystore::Bridge;

/// The command of the Kotlin plugin that starts a view of an address and answers `opened` or `no_app`.
pub const OPEN_LINK: &str = "openLink";

/// The system's way of opening a link, through the plugin.
pub struct AndroidOpener {
    bridge: Arc<dyn Bridge>,
}

impl AndroidOpener {
    pub fn new(bridge: Arc<dyn Bridge>) -> AndroidOpener {
        AndroidOpener { bridge }
    }
}

/// Read the answer of `openLink`: the member `kind` is exactly `opened` or `no_app`, anything else is the failure.
fn read_answer(answer: &Value) -> Result<(), OpenError> {
    match answer.get("kind").and_then(Value::as_str) {
        Some("opened") => Ok(()),
        Some("no_app") => Err(OpenError::NoApp),
        _ => Err(OpenError::Failed),
    }
}

impl LinkOpener for AndroidOpener {
    fn open(&self, url: &str) -> Result<(), OpenError> {
        match self.bridge.call(OPEN_LINK, &json!({ "url": url })) {
            Ok(answer) => read_answer(&answer),
            Err(_) => Err(OpenError::Failed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::android::keystore::tests::Recorder;
    use holzwave_core::vault::KeystoreError;

    fn opener(recorder: &Arc<Recorder>) -> AndroidOpener {
        AndroidOpener::new(recorder.clone())
    }

    #[test]
    fn an_open_sends_the_address_to_the_command_and_reads_the_two_answers() {
        let recorder = Arc::new(Recorder::default());
        recorder.answer(OPEN_LINK, Ok(json!({ "kind": "opened" })));
        assert_eq!(opener(&recorder).open("https://example.org/#device=BCDF-GHJK"), Ok(()));
        assert_eq!(recorder.payloads(OPEN_LINK), vec![json!({ "url": "https://example.org/#device=BCDF-GHJK" })]);
        assert_eq!(recorder.calls.lock().expect("calls").len(), 1, "no other command was called");

        recorder.answer(OPEN_LINK, Ok(json!({ "kind": "no_app" })));
        assert_eq!(opener(&recorder).open("mailto:someone@example.org"), Err(OpenError::NoApp));
    }

    #[test]
    fn a_rejection_a_missing_command_and_any_other_answer_are_one_failure() {
        let recorder = Arc::new(Recorder::default());
        // No scripted answer: the plugin fails every call.
        assert_eq!(opener(&recorder).open("https://example.org/"), Err(OpenError::Failed));
        for error in [KeystoreError::Cancelled, KeystoreError::KeyGone, KeystoreError::Invalid] {
            recorder.answer(OPEN_LINK, Err(error));
            assert_eq!(opener(&recorder).open("https://example.org/"), Err(OpenError::Failed));
        }
        for answer in [
            json!({}),
            json!(null),
            json!("opened"),
            json!({ "kind": "Opened" }),
            json!({ "kind": "opened " }),
            json!({ "kind": 1 }),
            json!({ "kind": "failed" }),
            json!({ "kind": "no_app_here", "text": "com.example.secret" }),
        ] {
            recorder.answer(OPEN_LINK, Ok(answer.clone()));
            assert_eq!(opener(&recorder).open("https://example.org/"), Err(OpenError::Failed), "{answer}");
        }
    }

    #[test]
    fn nothing_of_the_plugins_text_reaches_an_error() {
        // The error is a closed set of two words: it has no field to carry received text.
        let recorder = Arc::new(Recorder::default());
        recorder.answer(OPEN_LINK, Err(KeystoreError::Failed("com.android.chrome crashed at /data/user/0".to_owned())));
        let got = opener(&recorder).open("https://example.org/");
        assert_eq!(got, Err(OpenError::Failed));
        assert!(!format!("{got:?}").contains("chrome"));
    }
}
