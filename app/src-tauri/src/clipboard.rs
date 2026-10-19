//! The system clipboard as plain text, read and written in the app's own process.
//!
//! macOS goes through `NSPasteboard` (one call, no subprocess, so the text never sits on an argument list and nothing is
//! spawned per copy). Other platforms answer an error: the web view reaches the clipboard there and the page falls back to
//! it. Both directions are bounded at [`MAX_BYTES`], the paste limit of the core: a text over the limit is refused, never
//! cut, so a half text can not land on the clipboard or in a pane.

/// The most bytes a clipboard text may have, in either direction (the paste limit of the core).
pub const MAX_BYTES: usize = 16 * 1024 * 1024;

/// The pasteboard type of plain UTF-8 text: the value of `NSPasteboardTypeString`. Spelled out, so no extern static
/// (and no `unsafe`) is needed to name it.
#[cfg(target_os = "macos")]
const TEXT_TYPE: &str = "public.utf8-plain-text";

/// Whether a text of `len` bytes may cross the boundary.
pub fn check_len(len: usize) -> Result<(), String> {
    if len > MAX_BYTES {
        Err(format!("the clipboard text is {len} bytes; the limit is {MAX_BYTES} bytes"))
    } else {
        Ok(())
    }
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

/// Replace the clipboard with `text`. Only macOS has a native clipboard here.
#[cfg(not(target_os = "macos"))]
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

/// The plain text on the clipboard. Only macOS has a native clipboard here.
#[cfg(not(target_os = "macos"))]
pub fn read_text() -> Result<String, String> {
    Err(NOT_NATIVE.to_string())
}

#[cfg(not(target_os = "macos"))]
const NOT_NATIVE: &str = "this platform has no native clipboard access: the web view reads and writes the clipboard";

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

    #[cfg(not(target_os = "macos"))]
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
}
