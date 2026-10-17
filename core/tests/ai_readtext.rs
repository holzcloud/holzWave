//! Reading the text of a pane: the bounds, the regions, the byte ranges of text the user could not see, and hostile
//! screens.
//!
//! The routing tests use a mock engine and need no Zig. The engine and live tests run the real terminal engine.

mod common;

use holzwave_core::engine::{Screen, Update};
use holzwave_core::textread::{MAX_READ_BYTES, TextRead, TextRegion};
use holzwave_core::TerminalEngine;

#[cfg(unix)]
mod routing {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use holzwave_core::engine::{KeyEvent, PasteOutcome};
    use holzwave_core::pane::{PaneHandle, PaneSpec};
    use holzwave_core::{Error, Result};

    use super::*;

    type Calls = Arc<Mutex<Vec<(TextRegion, usize)>>>;

    /// An engine that answers reads with a canned value and records what it was asked.
    struct Reader {
        calls: Calls,
    }

    /// The same engine without a read: the default of the trait answers.
    struct Bare;

    macro_rules! engine_basics {
        () => {
            fn feed(&mut self, _bytes: &[u8]) {}
            fn resize(&mut self, _cols: u16, _rows: u16, _cell_px: (u32, u32)) -> Result<()> {
                Ok(())
            }
            fn size(&self) -> Result<(u16, u16)> {
                Ok((80, 24))
            }
            fn scroll(&mut self, _rows: isize) {}
            fn scroll_to_bottom(&mut self) {}
            fn screen(&mut self) -> Result<Screen> {
                Ok(Screen::default())
            }
            fn set_option_as_alt(&mut self, _on: bool) {}
            fn encode_key(&mut self, _event: &KeyEvent) -> Result<Vec<u8>> {
                Ok(Vec::new())
            }
            fn encode_text(&mut self, text: &str) -> Result<Vec<u8>> {
                Ok(text.as_bytes().to_vec())
            }
            fn paste(&mut self, _text: &str, _allow_unsafe: bool) -> Result<PasteOutcome> {
                Ok(PasteOutcome::Empty)
            }
            fn take_replies(&mut self) -> Vec<u8> {
                Vec::new()
            }
            fn update(&mut self) -> Result<Update> {
                Ok(Update::default())
            }
        };
    }

    fn canned() -> TextRead {
        TextRead { text: "canned".into(), hidden: vec![1..3], truncated: true }
    }

    impl TerminalEngine for Reader {
        engine_basics!();
        fn read_text(&mut self, region: TextRegion, max_bytes: usize) -> Result<Option<TextRead>> {
            self.calls.lock().unwrap().push((region, max_bytes));
            Ok(Some(canned()))
        }
    }

    impl TerminalEngine for Bare {
        engine_basics!();
    }

    fn reader_pane() -> (PaneHandle, Calls) {
        let calls = Calls::default();
        let mock = Reader { calls: calls.clone() };
        let pane = PaneHandle::spawn(1, PaneSpec::new("/bin/cat", 80, 24), |_| Ok(mock), |_event| {})
            .expect("the pane starts");
        (pane, calls)
    }

    #[test]
    fn a_read_reaches_the_engine_thread_with_the_bound_applied() {
        let (pane, calls) = reader_pane();
        let read = pane.read_text(TextRegion::Screen, usize::MAX).expect("answer");
        assert_eq!(read, Some(canned()), "the canned value comes back unchanged");
        assert_eq!(*calls.lock().unwrap(), vec![(TextRegion::Screen, MAX_READ_BYTES)]);
        pane.close();
        assert!(pane.join(Duration::from_secs(3)));
    }

    #[test]
    fn a_zero_bound_is_answered_without_the_engine() {
        let (pane, calls) = reader_pane();
        assert_eq!(pane.read_text(TextRegion::Screen, 0).expect("answer"), None);
        assert!(calls.lock().unwrap().is_empty(), "the engine saw no call");
        pane.close();
        assert!(pane.join(Duration::from_secs(3)));
    }

    #[test]
    fn an_engine_without_the_capability_answers_unsupported() {
        let pane = PaneHandle::spawn(1, PaneSpec::new("/bin/cat", 80, 24), |_| Ok(Bare), |_event| {})
            .expect("the pane starts");
        let answer = pane.read_text(TextRegion::Screen, 100);
        assert!(matches!(answer, Err(Error::Unsupported(_))), "{answer:?}");
        pane.close();
        assert!(pane.join(Duration::from_secs(3)));
    }

    #[test]
    fn a_closed_pane_answers_with_an_error() {
        let (pane, _calls) = reader_pane();
        pane.close();
        assert!(pane.join(Duration::from_secs(3)));
        assert!(pane.read_text(TextRegion::Screen, 100).is_err());
    }
}

#[cfg(feature = "ghostty")]
mod engine_reads {
    use std::time::Duration;

    use holzwave_core::engine::{SelectMode, SelectOp};
    use holzwave_core::pane::{PaneHandle, PaneSpec};

    use super::*;
    use common::{engine, wait_until};

    /// The text of a region, panicking when the read fails.
    fn read(e: &mut impl TerminalEngine, region: TextRegion) -> Option<TextRead> {
        e.read_text(region, MAX_READ_BYTES).expect("read_text")
    }

    #[cfg(unix)]
    #[test]
    fn a_live_pane_gives_the_text_of_its_screen() {
        let script = "printf 'alpha\\nbeta  \\n\\ngamma'; exec cat";
        let pane = PaneHandle::spawn_ghostty(1, PaneSpec::new("/bin/sh", 80, 24).args(["-c", script]), |_event| {})
            .expect("the pane starts");
        let mut last = None;
        let arrived = wait_until(Duration::from_secs(5), || {
            last = pane.read_text(TextRegion::Screen, 4096).expect("read");
            last.as_ref().is_some_and(|r| r.text.contains("gamma"))
        });
        assert!(arrived, "the text never arrived: {last:?}");
        let read = last.expect("text");
        assert_eq!(read.text, "alpha\nbeta\n\ngamma");
        assert!(read.hidden.is_empty());
        assert!(!read.truncated);
        println!("live pane screen text: {:?}", read.text);
        pane.close();
        assert!(pane.join(Duration::from_secs(3)));
    }

    /// Twelve numbered rows on a 20x5 engine: the viewport has scrolled and holds `row08` to `row12`.
    fn numbered(e: &mut impl TerminalEngine, count: usize) {
        for n in 1..=count {
            let line = if n < count { format!("row{n:02}\r\n") } else { format!("row{n:02}") };
            e.feed(line.as_bytes());
        }
    }

    #[test]
    fn the_screen_text_equals_a_copy_of_the_whole_viewport() {
        let long = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHI";
        let scenarios: Vec<(&str, String)> = vec![
            ("scrolled rows", (1..=12).map(|n| if n < 12 { format!("row{n:02}\r\n") } else { format!("row{n:02}") }).collect()),
            ("a wrapped line", format!("{long}\r\nshort")),
            ("wide, combining and a cluster", "\u{65E5}\u{672C}\u{8A9E} e\u{301}x \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} end\r\nline2".into()),
            ("trailing spaces and an inner blank row", "a   \r\n\r\nb   \r\nc".into()),
            ("a blank bottom", "x\r\ny\r\n".into()),
        ];
        for (name, bytes) in scenarios {
            let mut e = engine(20, 5);
            e.feed(bytes.as_bytes());
            e.select(SelectOp::Begin { x: 0, y: 0, mode: SelectMode::Cell }).expect("select");
            e.select(SelectOp::Extend { x: 19, y: 4 }).expect("extend");
            let copied = e.selection_text().expect("selection_text").unwrap_or_default();
            let read = read(&mut e, TextRegion::Screen).map(|r| r.text).unwrap_or_default();
            assert_eq!(read.trim_matches('\n'), copied.trim_matches('\n'), "{name}");
            println!("parity {name}: {read:?}");
        }
        // The helper is used by the other tests of this module.
        let mut e = engine(20, 5);
        numbered(&mut e, 12);
        assert_eq!(read(&mut e, TextRegion::Screen).expect("text").text, "row08\nrow09\nrow10\nrow11\nrow12");
    }
}
