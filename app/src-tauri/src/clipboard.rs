//! The system clipboard as plain text, read and written in the app's own process.
//!
//! macOS goes through `NSPasteboard` (one call, no subprocess, so the text never sits on an argument list and nothing is
//! spawned per copy). Android goes through the app's own Kotlin plugin, because the web view's clipboard permission is not
//! there; the plugin reads and writes plain text only. Other platforms answer an error: the web view reaches the clipboard
//! there and the page falls back to it. Both directions are bounded, a text over the bound is refused and never cut, so a
//! half text can not land on the clipboard or in a pane: at [`MAX_BYTES`], the paste limit of the core, and on Android at
//! [`ANDROID_MAX_BYTES`].
//!
//! A text is never logged and never put into an error here: the answers of a failure are fixed words.

/// The most bytes a clipboard text may have, in either direction (the paste limit of the core). Android has the lower bound
/// below and does not use this one.
#[cfg_attr(target_os = "android", allow(dead_code))]
pub const MAX_BYTES: usize = 16 * 1024 * 1024;

/// The most bytes a clipboard text may have on Android, in either direction. The system clipboard lives in another process and
/// a clip crosses to it in one Binder transaction, whose buffer is about 1 MiB for the whole process; a clip near that size
/// makes the call fail (or the app crash in some versions). Half of it leaves room for the other transactions that run at
/// the same time, and is far above any paste a person makes by hand, so the bound is lower than the desktop's.
pub const ANDROID_MAX_BYTES: usize = 512 * 1024;

/// The pasteboard type of plain UTF-8 text: the value of `NSPasteboardTypeString`. Spelled out, so no extern static
/// (and no `unsafe`) is needed to name it.
#[cfg(target_os = "macos")]
const TEXT_TYPE: &str = "public.utf8-plain-text";

/// Whether a text of `len` bytes may cross the boundary when the bound is `limit`: the one check of every direction.
pub fn check_len_for(len: usize, limit: usize) -> Result<(), String> {
    if len > limit {
        Err(format!("the clipboard text is {len} bytes; the limit is {limit} bytes"))
    } else {
        Ok(())
    }
}

/// Whether a text of `len` bytes may cross the boundary, with the bound of the core.
#[cfg_attr(target_os = "android", allow(dead_code))]
pub fn check_len(len: usize) -> Result<(), String> {
    check_len_for(len, MAX_BYTES)
}

/// Replace the clipboard with `text`.
#[cfg(target_os = "macos")]
pub fn write_text(text: &str) -> Result<(), String> {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;

    check_len(text.len())?;
    // The command runs on a runtime thread that has no autorelease pool of its own.
    autoreleasepool(|_| {
        let board = NSPasteboard::generalPasteboard();
        board.clearContents();
        let kind = NSString::from_str(TEXT_TYPE);
        if board.setString_forType(&NSString::from_str(text), &kind) {
            Ok(())
        } else {
            Err("the system clipboard refused the text".to_string())
        }
    })
}

/// Replace the clipboard with `text`, as a plain text clip that the system is told is sensitive (Android 13 and later hide its
/// preview). Answers whether the clip carries that mark, which the debug self-test of the emulator reads.
#[cfg(target_os = "android")]
pub fn write_text_marked(text: &str) -> Result<bool, String> {
    check_len_for(text.len(), ANDROID_MAX_BYTES)?;
    android_write(&plugin_call, text)
}

/// Replace the clipboard with `text`.
#[cfg(target_os = "android")]
pub fn write_text(text: &str) -> Result<(), String> {
    write_text_marked(text).map(|_| ())
}

/// Replace the clipboard with `text`. Only macOS and Android have a native clipboard here.
#[cfg(not(any(target_os = "macos", target_os = "android")))]
pub fn write_text(text: &str) -> Result<(), String> {
    check_len(text.len())?;
    Err(NOT_NATIVE.to_string())
}

/// The plain text on the clipboard; the empty string when it holds none (an image, a file, nothing). A text over
/// [`MAX_BYTES`] is an error, not a prefix.
#[cfg(target_os = "macos")]
pub fn read_text() -> Result<String, String> {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;

    autoreleasepool(|_| {
        let board = NSPasteboard::generalPasteboard();
        let kind = NSString::from_str(TEXT_TYPE);
        match board.stringForType(&kind) {
            None => Ok(String::new()),
            Some(text) => {
                // The UTF-8 length is known before the text is copied out, so an oversized text is refused without a copy.
                check_len(text.len())?;
                Ok(text.to_string())
            }
        }
    })
}

/// The plain text on the clipboard; the empty string when it holds none (an image, a file, a link, nothing). Called only when
/// a person's own paste asks for it: nothing listens for changes of the clipboard. A text over [`ANDROID_MAX_BYTES`] is an
/// error, not a prefix.
#[cfg(target_os = "android")]
pub fn read_text() -> Result<String, String> {
    android_read(&plugin_call)
}

/// The plain text on the clipboard. Only macOS and Android have a native clipboard here.
#[cfg(not(any(target_os = "macos", target_os = "android")))]
pub fn read_text() -> Result<String, String> {
    Err(NOT_NATIVE.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "android")))]
const NOT_NATIVE: &str = "this platform has no native clipboard access: the web view reads and writes the clipboard";

// ---- Android: the plugin's two commands and how their answers are read ---------------------------------------------------

/// The command of the Kotlin plugin that reads the primary clip as plain text.
#[cfg(any(target_os = "android", test))]
const READ: &str = "clipboardRead";
/// The command of the Kotlin plugin that replaces the primary clip with a plain text clip.
#[cfg(any(target_os = "android", test))]
const WRITE: &str = "clipboardWrite";

/// The one text of every failure of the system clipboard that is not a size. It says nothing about the cause.
#[cfg(any(target_os = "android", test))]
const FAILED: &str = "the system clipboard failed";

/// One call into the plugin: a command and its payload in, the answer or the fixed failure text out. On Android it is the
/// plugin's handle; the unit tests give a scripted one, so the shapes and every refusal are proved on the desktop.
#[cfg(any(target_os = "android", test))]
type Call<'a> = &'a dyn Fn(&str, &serde_json::Value) -> Result<serde_json::Value, String>;

/// The text of a rejection of the plugin, by its `code`: the size has a word of its own, everything else is one text.
#[cfg(any(target_os = "android", test))]
fn rejection_text(code: &str) -> String {
    match code {
        "too_large" => format!("the clipboard text is over the limit of {ANDROID_MAX_BYTES} bytes"),
        _ => FAILED.to_owned(),
    }
}

/// The text of an answer of `clipboardRead`, `{ "text": string, "sensitive": bool }`. A missing field, a mistyped one or a
/// text over the bound is an error with a fixed text, never a partial text and never what the answer held.
#[cfg(any(target_os = "android", test))]
fn text_of(answer: &serde_json::Value) -> Result<String, String> {
    let text = answer.get("text").and_then(serde_json::Value::as_str);
    let marked = answer.get("sensitive").and_then(serde_json::Value::as_bool);
    let (Some(text), Some(_)) = (text, marked) else { return Err(FAILED.to_owned()) };
    check_len_for(text.len(), ANDROID_MAX_BYTES)?;
    Ok(text.to_owned())
}

/// Whether the clip carries the sensitive mark, from an answer of `clipboardWrite`, `{ "sensitive": bool }`.
#[cfg(any(target_os = "android", test))]
fn marked_of(answer: &serde_json::Value) -> Result<bool, String> {
    answer.get("sensitive").and_then(serde_json::Value::as_bool).ok_or_else(|| FAILED.to_owned())
}

#[cfg(any(target_os = "android", test))]
fn android_read(call: Call<'_>) -> Result<String, String> {
    text_of(&call(READ, &serde_json::json!({ "max_bytes": ANDROID_MAX_BYTES }))?)
}

#[cfg(any(target_os = "android", test))]
fn android_write(call: Call<'_>, text: &str) -> Result<bool, String> {
    marked_of(&call(WRITE, &serde_json::json!({ "text": text }))?)
}

/// The plugin of the app, called from a worker thread and waiting for its answer (the clipboard is the system's, in another
/// process). A rejection is read by its code only, never by the message the system gave.
#[cfg(target_os = "android")]
fn plugin_call(command: &str, payload: &serde_json::Value) -> Result<serde_json::Value, String> {
    use tauri::plugin::mobile::PluginInvokeError;

    let Some(handle) = crate::android::plugin::handle() else { return Err(FAILED.to_owned()) };
    match handle.run_mobile_plugin::<serde_json::Value>(command, payload) {
        Ok(answer) => Ok(answer),
        Err(PluginInvokeError::InvokeRejected(rejected)) => Err(rejection_text(rejected.code.as_deref().unwrap_or(""))),
        Err(_) => Err(FAILED.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_passes_and_one_byte_more_fails() {
        assert_eq!(check_len(0), Ok(()));
        assert_eq!(check_len(MAX_BYTES), Ok(()));
        let err = check_len(MAX_BYTES + 1).unwrap_err();
        assert!(err.contains(&MAX_BYTES.to_string()), "the error names the limit: {err}");
    }

    #[test]
    fn the_limit_is_the_paste_limit_of_the_core() {
        assert_eq!(MAX_BYTES, 16 * 1024 * 1024);
    }

    #[cfg(not(any(target_os = "macos", target_os = "android")))]
    #[test]
    fn a_platform_without_a_native_clipboard_says_so() {
        for err in [write_text("x").unwrap_err(), read_text().unwrap_err()] {
            assert!(err.contains("web view"), "{err}");
        }
    }

    #[test]
    fn a_text_over_the_limit_is_refused_before_the_clipboard_is_touched() {
        // Runs on every platform: the bound is checked first, so this never reaches the real pasteboard.
        let big = "a".repeat(MAX_BYTES + 1);
        assert!(write_text(&big).unwrap_err().contains("limit"));
    }

    #[test]
    fn one_function_makes_the_check_and_the_bounds_are_the_ones_named() {
        assert_eq!(ANDROID_MAX_BYTES, 524_288);
        assert!(ANDROID_MAX_BYTES <= MAX_BYTES, "the Android bound is never above the desktop's");
        assert_eq!(check_len_for(0, ANDROID_MAX_BYTES), Ok(()));
        assert_eq!(check_len_for(ANDROID_MAX_BYTES, ANDROID_MAX_BYTES), Ok(()), "exactly the bound passes");
        let err = check_len_for(ANDROID_MAX_BYTES + 1, ANDROID_MAX_BYTES).unwrap_err();
        assert_eq!(err, "the clipboard text is 524289 bytes; the limit is 524288 bytes", "the same message form as the bound of the core");
        assert_eq!(check_len(MAX_BYTES), check_len_for(MAX_BYTES, MAX_BYTES));
        assert_eq!(check_len(MAX_BYTES + 1), check_len_for(MAX_BYTES + 1, MAX_BYTES));
        assert_eq!(
            check_len(MAX_BYTES + 1).unwrap_err(),
            format!("the clipboard text is {} bytes; the limit is {MAX_BYTES} bytes", MAX_BYTES + 1)
        );
    }

    /// The plugin's answers are read as strictly on the desktop as on a phone: a scripted plugin stands in for the Kotlin side.
    mod android {
        use super::super::*;
        use serde_json::{Value, json};
        use std::cell::RefCell;

        const CANARY: &str = "HW-CLIP-CANARY-52c0";

        /// A scripted plugin: one answer for every command, and the calls it saw.
        struct Plugin {
            answer: Result<Value, String>,
            seen: RefCell<Vec<(String, Value)>>,
        }

        impl Plugin {
            fn new(answer: Result<Value, String>) -> Plugin {
                Plugin { answer, seen: RefCell::new(Vec::new()) }
            }

            fn call(&self) -> impl Fn(&str, &Value) -> Result<Value, String> + '_ {
                |command, payload| {
                    self.seen.borrow_mut().push((command.to_owned(), payload.clone()));
                    self.answer.clone()
                }
            }
        }

        #[test]
        fn a_read_passes_the_bound_and_takes_only_a_text_with_its_mark() {
            let plugin = Plugin::new(Ok(json!({ "text": "ls -l", "sensitive": false })));
            assert_eq!(android_read(&plugin.call()), Ok("ls -l".to_owned()));
            assert_eq!(*plugin.seen.borrow(), vec![("clipboardRead".to_owned(), json!({ "max_bytes": 524_288 }))]);
            // An empty clip is the empty text.
            let empty = Plugin::new(Ok(json!({ "text": "", "sensitive": false })));
            assert_eq!(android_read(&empty.call()), Ok(String::new()));
        }

        #[test]
        fn a_read_answer_that_is_not_exactly_text_is_one_fixed_failure_that_never_holds_what_was_received() {
            for answer in [
                json!({}),
                json!(null),
                json!("text"),
                json!({ "text": 5, "sensitive": false }),
                json!({ "text": null, "sensitive": false }),
                json!({ "text": ["a"], "sensitive": false }),
                json!({ "text": CANARY }),
                json!({ "sensitive": true }),
                json!({ "text": CANARY, "sensitive": "yes" }),
            ] {
                let plugin = Plugin::new(Ok(answer.clone()));
                let error = android_read(&plugin.call()).expect_err("a refused answer");
                assert_eq!(error, FAILED, "{answer}");
                assert!(!error.contains(CANARY));
            }
            // A text over the bound is refused whole, never cut, and the error names the sizes and not the text.
            let big = "a".repeat(ANDROID_MAX_BYTES + 1);
            let plugin = Plugin::new(Ok(json!({ "text": big, "sensitive": false })));
            let error = android_read(&plugin.call()).expect_err("over the bound");
            assert!(error.contains("524288") && !error.contains("aaaa"), "{error}");
            let exact = "b".repeat(ANDROID_MAX_BYTES);
            let plugin = Plugin::new(Ok(json!({ "text": exact.clone(), "sensitive": false })));
            assert_eq!(android_read(&plugin.call()), Ok(exact));
        }

        #[test]
        fn a_rejection_is_read_by_its_code_and_a_failed_call_passes_its_fixed_text_on() {
            assert_eq!(rejection_text("too_large"), "the clipboard text is over the limit of 524288 bytes");
            for code in ["", "failed", "TOO_LARGE", "java.lang.SecurityException: pasted HW-CLIP-CANARY-52c0", "\u{0}"] {
                assert_eq!(rejection_text(code), FAILED, "{code:?}");
            }
            let plugin = Plugin::new(Err(rejection_text("failed")));
            assert_eq!(android_read(&plugin.call()), Err(FAILED.to_owned()));
            assert_eq!(android_write(&plugin.call(), "x"), Err(FAILED.to_owned()));
        }

        #[test]
        fn a_write_sends_the_text_once_and_reports_the_mark() {
            let plugin = Plugin::new(Ok(json!({ "sensitive": true })));
            assert_eq!(android_write(&plugin.call(), CANARY), Ok(true));
            assert_eq!(*plugin.seen.borrow(), vec![("clipboardWrite".to_owned(), json!({ "text": CANARY }))]);
            let plugin = Plugin::new(Ok(json!({ "sensitive": false })));
            assert_eq!(android_write(&plugin.call(), "x"), Ok(false));
            for answer in [json!({}), json!(null), json!({ "sensitive": "true" }), json!({ "sensitive": 1 })] {
                let plugin = Plugin::new(Ok(answer.clone()));
                assert_eq!(android_write(&plugin.call(), CANARY), Err(FAILED.to_owned()), "{answer}");
            }
        }
    }
}
