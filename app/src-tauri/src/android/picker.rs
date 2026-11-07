//! [`AndroidPicker`]: the host's file picker over the Kotlin plugin, which shows the system's open-document and create-document
//! dialogs and copies one file.
//!
//! The picker holds no state of its own: every call is one command of the plugin, made through the same [`Bridge`] as the
//! vault's keystore (the plugin handle on Android, a recorder in the unit tests, so the commands, the shapes that cross and
//! the mapping of every failure are proved on the desktop). A call waits for the person in the dialog, which can take minutes,
//! so the host makes it from a blocking worker, never from the thread of the window system and never from the async runtime.
//!
//! The host decides what crosses: the staging file, the limit and the source path go out as they are; what comes back is read
//! only through [`holzwave_host::picker_wire`], which is strict and never carries received text into an error. A rejection of
//! the plugin, whatever its code, is the one fixed [`PickError::Failed`].

use std::path::Path;
use std::sync::Arc;

use holzwave_host::picker_wire::{pick_in_to_wire, picked_in_from_wire, picked_out_from_wire, save_out_to_wire};
use holzwave_host::seams::{FilePicker, PickError, PickedIn, PickedOut};

use crate::android::keystore::Bridge;

/// The command of the Kotlin plugin that shows the open-document dialog and copies the chosen document to the staging file.
pub const PICK_IN: &str = "pickIn";
/// The command of the Kotlin plugin that shows the create-document dialog and copies a file of the shell's folder to it.
pub const SAVE_OUT: &str = "saveOut";

/// The system's document dialogs, through the plugin.
pub struct AndroidPicker {
    bridge: Arc<dyn Bridge>,
}

impl AndroidPicker {
    pub fn new(bridge: Arc<dyn Bridge>) -> AndroidPicker {
        AndroidPicker { bridge }
    }
}

impl FilePicker for AndroidPicker {
    fn pick_in(&self, staging: &Path, max_bytes: u64) -> Result<PickedIn, PickError> {
        let payload = pick_in_to_wire(staging, max_bytes).ok_or(PickError::Failed)?;
        match self.bridge.call(PICK_IN, &payload) {
            Ok(answer) => picked_in_from_wire(&answer),
            Err(_) => Err(PickError::Failed),
        }
    }

    fn save_out(&self, source: &Path, suggested_name: &str) -> Result<PickedOut, PickError> {
        let payload = save_out_to_wire(source, suggested_name).ok_or(PickError::Failed)?;
        match self.bridge.call(SAVE_OUT, &payload) {
            Ok(answer) => picked_out_from_wire(&answer),
            Err(_) => Err(PickError::Failed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::android::keystore::tests::Recorder;
    use holzwave_core::vault::KeystoreError;
    use serde_json::json;

    fn picker(recorder: &Arc<Recorder>) -> AndroidPicker {
        AndroidPicker::new(recorder.clone())
    }

    #[test]
    fn an_open_sends_the_staging_file_and_the_limit_and_reads_the_answer_through_the_wire() {
        let recorder = Arc::new(Recorder::default());
        recorder.answer(PICK_IN, Ok(json!({ "kind": "done", "name": "report.pdf", "size": 1048576 })));
        let got = picker(&recorder).pick_in(Path::new("/data/user/0/app/files/home/handover/.incoming-1"), 268_435_456);
        assert_eq!(got, Ok(PickedIn { name: "report.pdf".to_owned(), size: 1_048_576 }));
        assert_eq!(
            recorder.payloads(PICK_IN),
            vec![json!({ "staging_path": "/data/user/0/app/files/home/handover/.incoming-1", "max_bytes": 268435456u64 })]
        );
        assert_eq!(recorder.count(SAVE_OUT), 0);
        recorder.answer(PICK_IN, Ok(json!({ "kind": "cancelled" })));
        assert_eq!(picker(&recorder).pick_in(Path::new("/x"), 1), Err(PickError::Cancelled));
        recorder.answer(PICK_IN, Ok(json!({ "kind": "too_large", "name": "big.bin", "size": 300_000_000u64 })));
        assert_eq!(
            picker(&recorder).pick_in(Path::new("/x"), 1),
            Err(PickError::TooLarge { name: "big.bin".to_owned(), size: Some(300_000_000) })
        );
    }

    #[test]
    fn a_save_sends_the_source_and_the_name_and_reads_the_answer_through_the_wire() {
        let recorder = Arc::new(Recorder::default());
        recorder.answer(SAVE_OUT, Ok(json!({ "kind": "done", "size": 42 })));
        let got = picker(&recorder).save_out(Path::new("/data/user/0/app/files/home/a.bin"), "a.bin");
        assert_eq!(got, Ok(PickedOut { size: 42 }));
        assert_eq!(
            recorder.payloads(SAVE_OUT),
            vec![json!({ "source_path": "/data/user/0/app/files/home/a.bin", "suggested_name": "a.bin" })]
        );
        assert_eq!(recorder.count(PICK_IN), 0);
        recorder.answer(SAVE_OUT, Ok(json!({ "kind": "cancelled" })));
        assert_eq!(picker(&recorder).save_out(Path::new("/x"), "x"), Err(PickError::Cancelled));
    }

    #[test]
    fn a_rejection_a_missing_command_and_an_answer_that_is_not_the_wire_are_one_failure() {
        let recorder = Arc::new(Recorder::default());
        // No scripted answer: the plugin fails every call.
        assert_eq!(picker(&recorder).pick_in(Path::new("/x"), 1), Err(PickError::Failed));
        assert_eq!(picker(&recorder).save_out(Path::new("/x"), "x"), Err(PickError::Failed));
        for error in [KeystoreError::Cancelled, KeystoreError::KeyGone, KeystoreError::Invalid] {
            recorder.answer(PICK_IN, Err(error.clone()));
            recorder.answer(SAVE_OUT, Err(error));
            assert_eq!(picker(&recorder).pick_in(Path::new("/x"), 1), Err(PickError::Failed));
            assert_eq!(picker(&recorder).save_out(Path::new("/x"), "x"), Err(PickError::Failed));
        }
        recorder.answer(PICK_IN, Ok(json!({ "kind": "done", "name": "a", "size": -1 })));
        recorder.answer(SAVE_OUT, Ok(json!({ "kind": "done" })));
        assert_eq!(picker(&recorder).pick_in(Path::new("/x"), 1), Err(PickError::Failed));
        assert_eq!(picker(&recorder).save_out(Path::new("/x"), "x"), Err(PickError::Failed));
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_text_never_reaches_the_plugin() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let recorder = Arc::new(Recorder::default());
        let bad = Path::new(OsStr::from_bytes(b"/data/\xff"));
        assert_eq!(picker(&recorder).pick_in(bad, 1), Err(PickError::Failed));
        assert_eq!(picker(&recorder).save_out(bad, "x"), Err(PickError::Failed));
        assert_eq!(recorder.count(PICK_IN) + recorder.count(SAVE_OUT), 0);
    }
}
