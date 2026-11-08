//! The window frame choices, on the host side: the host that joins the stored choice of the person
//! ([`holzwave_core::window_frame`]) with the choice the running windows were built with, the view the page reads, and the
//! setter.
//!
//! On Windows and Linux the page's own bar can be the window bar, or the system can keep its title bar. A window cannot
//! change its frame while it is open, so two values exist at once: the **started** choice, read once when the host is built
//! and fixed for the life of the process (it is what the open windows were built with), and the **stored** choice, which
//! changes when the person sets it. The view says whether the two differ (`pending`), so that the dialog can say that a
//! change applies after the next start.
//!
//! The group applies on Windows and Linux only. Elsewhere no file is read or written, the view says it does not apply and a
//! set is refused. Every refusal is a fixed sentence: it repeats neither the word it was given nor a path.
//!
//! Every fact (the operating system, the data directory) is passed in, so a test runs every case on any machine and needs no
//! window. This file reads no environment, starts no process, touches no window and contains no foreign-function code.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use holzwave_core::layout::LoadStatus;
use holzwave_core::platform::Os;
use holzwave_core::window_frame::{ButtonChoice, FILE, FrameChoice, WindowFrame};
use serde::Serialize;

/// The reason a set is refused where the group does not apply.
const NOT_APPLICABLE: &str = "The window frame can be chosen on Windows and Linux only.";
/// The reason a set is refused for a frame word outside its table.
const BAD_FRAME_WORD: &str = "The frame word is not one of auto, own, system.";
/// The reason a set is refused for a button word outside its table.
const BAD_BUTTON_WORD: &str = "The buttons word is not one of auto, flat, round.";
/// The reason a set is refused when the file cannot be written.
const NOT_SAVED: &str = "The window frame choice could not be saved.";

/// The page's reading of the group, in the order of the wire: `applies`, `frame`, `buttons`, `pending`. The words are the
/// tables of the core. When `applies` is false the page draws nothing and both words read `auto`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WindowFrameView {
    pub applies: bool,
    pub frame: String,
    pub buttons: String,
    /// The stored choice differs from the one the running windows were built with, so it applies after the next start.
    pub pending: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the group exists on `os`.
fn applies_on(os: Os) -> bool {
    matches!(os, Os::Windows | Os::Linux)
}

/// The two choices of the person on this computer, behind the methods the page's commands call.
pub struct WindowFrameHost {
    dir: PathBuf,
    os: Os,
    /// The pair the running windows were built with. Read once, never changed.
    started: (FrameChoice, ButtonChoice),
    /// The stored pair. Locked briefly; held across the write of the file so two answers never interleave.
    stored: Mutex<WindowFrame>,
}

impl WindowFrameHost {
    /// A host over the data directory `dir`, deciding for the operating system this build runs on.
    pub fn new(dir: &Path) -> WindowFrameHost {
        WindowFrameHost::with_os(dir, Os::current())
    }

    /// A host that decides for `os`. Where the group applies the file is loaded once (a damaged one starts as `auto` for the
    /// doubtful member and its bytes are kept as `.bad`); elsewhere nothing is read.
    pub fn with_os(dir: &Path, os: Os) -> WindowFrameHost {
        let stored = if applies_on(os) {
            let (stored, status) = WindowFrame::load(dir);
            if status != LoadStatus::Clean {
                eprintln!("holzwave: {FILE}: {status:?}; the doubtful choice starts as auto");
            }
            stored
        } else {
            WindowFrame::default()
        };
        WindowFrameHost { dir: dir.to_owned(), os, started: (stored.frame, stored.buttons), stored: Mutex::new(stored) }
    }

    /// Whether the group exists on this system.
    pub fn applies(&self) -> bool {
        applies_on(self.os)
    }

    /// The pair the running windows were built with. It does not change after a [`set`](WindowFrameHost::set).
    pub fn started(&self) -> (FrameChoice, ButtonChoice) {
        self.started
    }

    fn view_of(&self, stored: &WindowFrame) -> WindowFrameView {
        if !self.applies() {
            return WindowFrameView {
                applies: false,
                frame: FrameChoice::Auto.word().to_owned(),
                buttons: ButtonChoice::Auto.word().to_owned(),
                pending: false,
            };
        }
        // Windows draws flat buttons whatever the word says, so only the frame decides there.
        let buttons_differ = self.os != Os::Windows && stored.buttons != self.started.1;
        WindowFrameView {
            applies: true,
            frame: stored.frame.word().to_owned(),
            buttons: stored.buttons.word().to_owned(),
            pending: stored.frame != self.started.0 || buttons_differ,
        }
    }

    /// What the page shows now.
    pub fn view(&self) -> WindowFrameView {
        self.view_of(&lock(&self.stored))
    }

    /// The person chose. Both words must be in the table of their member, and the group must apply; otherwise the set is
    /// refused with a fixed sentence and nothing is written. Otherwise the pair is stored (the file is written only when it
    /// changes; a write that fails is refused and leaves the last good pair) and the new view is returned. Windows that are
    /// already open are not touched: only the next start reads the choice.
    pub fn set(&self, frame: &str, buttons: &str) -> Result<WindowFrameView, String> {
        let frame = FrameChoice::from_word(frame).ok_or(BAD_FRAME_WORD);
        let buttons = ButtonChoice::from_word(buttons).ok_or(BAD_BUTTON_WORD);
        let (frame, buttons) = (frame?, buttons?);
        if !self.applies() {
            return Err(NOT_APPLICABLE.to_owned());
        }
        let mut stored = lock(&self.stored);
        if (stored.frame, stored.buttons) != (frame, buttons) {
            let document = WindowFrame { frame, buttons, ..WindowFrame::default() };
            document.save(&self.dir).map_err(|_| NOT_SAVED.to_owned())?;
            *stored = document;
        }
        Ok(self.view_of(&stored))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("holzwave-windowframe-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path).expect("scratch dir");
            Scratch(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join(FILE)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn file_text(frame: &str, buttons: &str) -> String {
        format!("{{\n  \"version\": 1,\n  \"window_frame\": \"{frame}\",\n  \"window_buttons\": \"{buttons}\"\n}}\n")
    }

    /// No refusal repeats the input or names the data directory or the file.
    fn assert_fixed(reason: &str, dir: &Scratch, input: &[&str]) {
        assert!(!reason.contains(&*dir.0.to_string_lossy()), "{reason}");
        assert!(!reason.contains("window_frame.json") && !reason.contains('/') && !reason.contains('\\'), "{reason}");
        for word in input {
            assert!(!word.is_empty() && !reason.contains(word), "{reason} repeats {word}");
        }
    }

    #[test]
    fn where_the_group_does_not_apply_nothing_is_read_or_written() {
        for os in [Os::Macos, Os::Android] {
            let dir = Scratch::new("outside");
            // A file that says system: it is not looked at.
            std::fs::write(dir.file(), file_text("system", "round")).unwrap();
            let host = WindowFrameHost::with_os(&dir.0, os);
            let expected = WindowFrameView { applies: false, frame: "auto".into(), buttons: "auto".into(), pending: false };
            assert_eq!(host.view(), expected, "{os:?}");
            assert_eq!(host.started(), (FrameChoice::Auto, ButtonChoice::Auto), "{os:?}");
            assert!(!host.applies());

            let before = std::fs::read(dir.file()).unwrap();
            let refused = host.set("system", "round").expect_err("refused where the group does not apply");
            assert_fixed(&refused, &dir, &["system", "round"]);
            assert_eq!(std::fs::read(dir.file()).unwrap(), before, "{os:?}: nothing was written");
            assert!(!dir.0.join("window_frame.json.bad").exists());
            assert_eq!(host.view(), expected, "the view is unchanged");
        }
        // No file there at all: still no file afterwards.
        let dir = Scratch::new("outside-empty");
        let host = WindowFrameHost::with_os(&dir.0, Os::Macos);
        assert!(host.set("own", "flat").is_err());
        assert!(!dir.file().exists());
    }

    #[test]
    fn with_no_file_the_view_is_auto_and_not_pending_and_with_a_file_it_shows_the_file() {
        for os in [Os::Windows, Os::Linux] {
            let dir = Scratch::new("start");
            let host = WindowFrameHost::with_os(&dir.0, os);
            assert_eq!(
                host.view(),
                WindowFrameView { applies: true, frame: "auto".into(), buttons: "auto".into(), pending: false },
                "{os:?}"
            );
            assert_eq!(host.started(), (FrameChoice::Auto, ButtonChoice::Auto));
            assert!(!dir.file().exists(), "starting writes nothing");

            std::fs::write(dir.file(), file_text("system", "round")).unwrap();
            let host = WindowFrameHost::with_os(&dir.0, os);
            assert_eq!(host.started(), (FrameChoice::System, ButtonChoice::Round), "{os:?}");
            let view = host.view();
            assert_eq!((view.applies, view.frame.as_str(), view.buttons.as_str(), view.pending), (true, "system", "round", false));
        }
    }

    #[test]
    fn a_damaged_file_starts_as_auto_and_is_kept() {
        let dir = Scratch::new("damaged");
        std::fs::write(dir.file(), "{\"version\": 1, \"window_frame\": \"Own\", \"window_buttons\": \"round\"}").unwrap();
        let host = WindowFrameHost::with_os(&dir.0, Os::Linux);
        assert_eq!(host.started(), (FrameChoice::Auto, ButtonChoice::Round), "only the doubtful member is auto");
        assert!(dir.0.join("window_frame.json.bad").is_file());
    }

    #[test]
    fn a_set_stores_the_pair_and_pending_follows_the_started_words() {
        let dir = Scratch::new("set");
        let host = WindowFrameHost::with_os(&dir.0, Os::Linux);
        let view = host.set("system", "round").expect("valid words");
        assert_eq!((view.frame.as_str(), view.buttons.as_str(), view.pending), ("system", "round", true));
        assert_eq!(host.view(), view);
        assert_eq!(std::fs::read_to_string(dir.file()).unwrap(), file_text("system", "round"));
        assert_eq!(host.started(), (FrameChoice::Auto, ButtonChoice::Auto), "the started pair is fixed");

        // The next start reads it, and then it is not pending any more.
        let next = WindowFrameHost::with_os(&dir.0, Os::Linux);
        assert_eq!(next.started(), (FrameChoice::System, ButtonChoice::Round));
        assert!(!next.view().pending);

        // Setting the started words back is not pending.
        let back = host.set("auto", "auto").expect("valid words");
        assert!(!back.pending);
        assert_eq!(std::fs::read_to_string(dir.file()).unwrap(), file_text("auto", "auto"));
        // Setting the same words again writes nothing new.
        let again = host.set("auto", "auto").expect("valid words");
        assert_eq!(again, back);
    }

    #[test]
    fn on_windows_only_the_frame_decides_pending() {
        let dir = Scratch::new("windows");
        let host = WindowFrameHost::with_os(&dir.0, Os::Windows);
        let buttons_only = host.set("auto", "round").expect("valid words");
        assert!(!buttons_only.pending, "a Windows button word changes nothing that is built");
        assert_eq!(buttons_only.buttons, "round", "but it is stored");
        let frame = host.set("own", "round").expect("valid words");
        assert!(frame.pending);
        assert!(!host.set("auto", "flat").expect("valid words").pending);

        let dir = Scratch::new("linux");
        let host = WindowFrameHost::with_os(&dir.0, Os::Linux);
        assert!(host.set("auto", "round").expect("valid words").pending, "on Linux the buttons count");
        assert!(host.set("own", "auto").expect("valid words").pending, "and so does the frame");
        assert!(!host.set("auto", "auto").expect("valid words").pending);
    }

    #[test]
    fn a_word_outside_its_table_is_refused_without_echo_and_changes_nothing() {
        let dir = Scratch::new("bad-words");
        std::fs::write(dir.file(), file_text("own", "flat")).unwrap();
        let host = WindowFrameHost::with_os(&dir.0, Os::Linux);
        let before_view = host.view();
        let before = std::fs::read(dir.file()).unwrap();
        let cases: [(&str, &str); 8] = [
            ("hunter2", "auto"),
            ("auto", "hunter2"),
            ("hunter2", "hunter3"),
            ("Own", "flat"),
            ("own", "round "),
            ("flat", "auto"),
            ("auto", "system"),
            ("", ""),
        ];
        for (frame, buttons) in cases {
            let refused = host.set(frame, buttons).expect_err("refused");
            // Only the marker words are checked for an echo: a good word may legitimately appear in the table's sentence.
            let marker: Vec<&str> = [frame, buttons].into_iter().filter(|w| w.starts_with("hunter")).collect();
            assert_fixed(&refused, &dir, &marker);
            assert_eq!(std::fs::read(dir.file()).unwrap(), before, "{frame:?} {buttons:?}: nothing was written");
            assert_eq!(host.view(), before_view);
        }
        assert!(host.set("hunter2", "auto").unwrap_err().contains("frame"));
        assert!(host.set("auto", "hunter2").unwrap_err().contains("buttons"));
    }

    #[test]
    fn a_write_that_fails_is_refused_without_a_path_and_keeps_the_last_good_pair() {
        let dir = Scratch::new("unwritable");
        let host = WindowFrameHost::with_os(&dir.0, Os::Linux);
        host.set("own", "flat").expect("valid words");
        let good = host.view();
        // The target is a directory now, so the atomic write cannot replace it.
        std::fs::remove_file(dir.file()).unwrap();
        std::fs::create_dir(dir.file()).unwrap();
        let refused = host.set("system", "round").expect_err("cannot be saved");
        assert_fixed(&refused, &dir, &["system", "round"]);
        assert_eq!(host.view(), good, "the view keeps the stored value that was last good");
    }

    #[test]
    fn the_view_is_a_plain_object_of_four_members() {
        let dir = Scratch::new("wire");
        let host = WindowFrameHost::with_os(&dir.0, Os::Linux);
        assert_eq!(
            serde_json::to_string(&host.view()).unwrap(),
            "{\"applies\":true,\"frame\":\"auto\",\"buttons\":\"auto\",\"pending\":false}"
        );
        let host = WindowFrameHost::with_os(&dir.0, Os::Macos);
        assert_eq!(
            serde_json::to_string(&host.view()).unwrap(),
            "{\"applies\":false,\"frame\":\"auto\",\"buttons\":\"auto\",\"pending\":false}"
        );
    }
}
