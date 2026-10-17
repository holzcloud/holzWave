//! Reading the text of a pane: the bounds, the regions, the byte ranges of text the user could not see, and hostile
//! screens.
//!
//! The routing tests use a mock engine and need no Zig. The engine and live tests run the real terminal engine.

mod common;

use holzwave_core::engine::{Screen, Update};
use holzwave_core::textread::{MAX_HIDDEN_RANGES, MAX_READ_BYTES, MAX_READ_ROWS, TextRead, TextRegion};
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
    fn a_row_count_is_clamped_before_it_reaches_the_engine() {
        let (pane, calls) = reader_pane();
        pane.read_text(TextRegion::LastRows(u16::MAX), 1000).expect("answer");
        assert_eq!(*calls.lock().unwrap(), vec![(TextRegion::LastRows(MAX_READ_ROWS), 1000)]);
        assert_eq!(pane.read_text(TextRegion::LastRows(0), 1000).expect("answer"), None);
        assert_eq!(calls.lock().unwrap().len(), 1, "a zero row count never reaches the engine");
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

    fn text_of(e: &mut impl TerminalEngine, region: TextRegion) -> Option<String> {
        read(e, region).map(|r| r.text)
    }

    /// An engine of the given size that was fed `bytes`.
    fn fed(cols: u16, rows: u16, bytes: &str) -> holzwave_core::GhosttyEngine {
        let mut e = engine(cols, rows);
        e.feed(bytes.as_bytes());
        e
    }

    #[test]
    fn the_screen_is_what_the_viewport_shows_when_scrolled_back() {
        let mut e = engine(20, 5);
        numbered(&mut e, 12);
        e.scroll(-2);
        assert_eq!(text_of(&mut e, TextRegion::Screen).as_deref(), Some("row06\nrow07\nrow08\nrow09\nrow10"));
        e.scroll_to_bottom();
        assert_eq!(text_of(&mut e, TextRegion::Screen).as_deref(), Some("row08\nrow09\nrow10\nrow11\nrow12"));
    }

    #[test]
    fn last_rows_end_at_the_last_text_and_reach_into_the_scrollback() {
        let mut e = engine(20, 5);
        numbered(&mut e, 12);
        assert_eq!(text_of(&mut e, TextRegion::LastRows(3)).as_deref(), Some("row10\nrow11\nrow12"));
        let nine = "row04\nrow05\nrow06\nrow07\nrow08\nrow09\nrow10\nrow11\nrow12";
        assert_eq!(text_of(&mut e, TextRegion::LastRows(9)).as_deref(), Some(nine));
        e.scroll(-3);
        assert_eq!(text_of(&mut e, TextRegion::LastRows(9)).as_deref(), Some(nine), "the viewport does not matter");

        let mut e = fed(20, 10, "one\r\ntwo\r\nthree");
        assert_eq!(text_of(&mut e, TextRegion::LastRows(2)).as_deref(), Some("two\nthree"), "not blank rows");

        let mut e = fed(20, 10, "   \r\n  \r\n");
        assert_eq!(read(&mut e, TextRegion::LastRows(1)), None, "an engine that holds only blanks");
    }

    #[test]
    fn a_selection_is_read_from_its_first_cell_to_its_last_in_either_direction() {
        let mut e = engine(20, 5);
        numbered(&mut e, 12);
        let begin = |x, y| SelectOp::Begin { x, y, mode: SelectMode::Cell };
        e.select(begin(2, 1)).expect("select");
        e.select(SelectOp::Extend { x: 3, y: 3 }).expect("extend");
        let copied = e.selection_text().expect("text").expect("some");
        assert_eq!(text_of(&mut e, TextRegion::Selection).as_deref(), Some(copied.as_str()));
        assert_eq!(copied, "w09\nrow10\nrow1");

        e.select(begin(3, 3)).expect("select");
        e.select(SelectOp::Extend { x: 2, y: 1 }).expect("extend");
        assert_eq!(text_of(&mut e, TextRegion::Selection).as_deref(), Some(copied.as_str()), "backwards");

        e.select(begin(1, 2)).expect("select");
        assert_eq!(text_of(&mut e, TextRegion::Selection).as_deref(), Some("o"), "one cell");

        e.select(SelectOp::Begin { x: 3, y: 2, mode: SelectMode::Line }).expect("select");
        assert_eq!(text_of(&mut e, TextRegion::Selection).as_deref(), Some("row10"), "a whole row, no blanks");

        e.select(SelectOp::Clear).expect("clear");
        assert_eq!(read(&mut e, TextRegion::Selection), None, "no selection is no text");
    }

    #[test]
    fn reading_leaves_the_users_selection_and_the_next_frame_alone() {
        let build = || {
            let mut e = engine(20, 5);
            numbered(&mut e, 12);
            e.select(SelectOp::Begin { x: 0, y: 1, mode: SelectMode::Cell }).expect("select");
            e.select(SelectOp::Extend { x: 3, y: 2 }).expect("extend");
            e.scroll(-2);
            // A first update makes the next one a partial pass over what changes.
            e.update().expect("update");
            e
        };
        let (mut read_engine, mut twin) = (build(), build());
        for region in [TextRegion::Screen, TextRegion::Selection, TextRegion::LastRows(7), TextRegion::LastRows(1)] {
            assert!(read(&mut read_engine, region).is_some(), "{region:?}");
        }
        assert_eq!(read_engine.selection_text().expect("text"), twin.selection_text().expect("text"));
        assert_eq!(read_engine.screen().expect("screen").scrollbar, twin.screen().expect("screen").scrollbar);
        for more in ["\r\nrow13", "\r\nrow14"] {
            read_engine.feed(more.as_bytes());
            twin.feed(more.as_bytes());
            assert_eq!(read_engine.update().expect("update"), twin.update().expect("update"));
        }
        assert_eq!(read_engine.selection_text().expect("text"), twin.selection_text().expect("text"));
    }

    #[test]
    fn nothing_selected_and_nothing_on_the_screen_give_none() {
        let mut e = engine(20, 5);
        assert_eq!(read(&mut e, TextRegion::Screen), None);
        assert_eq!(read(&mut e, TextRegion::LastRows(5)), None);
        assert_eq!(read(&mut e, TextRegion::Selection), None);
        numbered(&mut e, 3);
        assert_eq!(e.read_text(TextRegion::Screen, 0).expect("read"), None, "a zero bound");
        assert!(read(&mut e, TextRegion::Screen).is_some());
    }

    const RESET: &str = "\x1b[0m";

    /// A row `pre <sequence>HIDDEN<reset> post` fed to a 40x5 engine and read back: the text and the hidden ranges.
    fn styled(sequence: &str, reset: &str) -> (String, Vec<std::ops::Range<usize>>) {
        let mut e = fed(40, 5, &format!("pre {sequence}HIDDEN{reset} post"));
        let read = read(&mut e, TextRegion::Screen).expect("text");
        (read.text, read.hidden)
    }

    #[test]
    fn concealed_and_low_contrast_text_is_reported_as_hidden_ranges() {
        let cases: [(&str, &str, &str); 6] = [
            ("conceal", "\x1b[8m", "\x1b[28m"),
            ("palette black on black", "\x1b[30;40m", RESET),
            ("equal palette entries", "\x1b[38;5;21;48;5;21m", RESET),
            ("equal truecolor", "\x1b[38;2;9;9;9;48;2;9;9;9m", RESET),
            ("near-equal truecolor", "\x1b[38;2;10;10;10;48;2;10;10;11m", RESET),
            ("inverse of equal colours", "\x1b[7;30;40m", RESET),
        ];
        for (name, sequence, reset) in cases {
            let (text, hidden) = styled(sequence, reset);
            assert_eq!(text, "pre HIDDEN post", "{name}");
            assert_eq!(hidden, vec![4..10], "{name}");
        }
        // A palette entry that a program redefined (OSC 4) against the same colour as an explicit background.
        let (text, hidden) = styled("\x1b]4;5;rgb:12/34/56\x1b\\\x1b[35;48;2;18;52;86m", RESET);
        assert_eq!(text, "pre HIDDEN post", "osc 4");
        assert_eq!(hidden, vec![4..10], "a redefined palette entry is resolved against the live palette");
    }

    #[test]
    fn normally_styled_text_is_not_hidden() {
        for (name, sequence) in [
            ("bold red", "\x1b[1;31m"),
            ("faint", "\x1b[2m"),
            ("bright black on black", "\x1b[90;40m"),
            ("underlined", "\x1b[4m"),
            ("truecolor on the default background", "\x1b[38;2;200;100;50m"),
        ] {
            let (text, hidden) = styled(sequence, RESET);
            assert_eq!(text, "pre HIDDEN post", "{name}");
            assert!(hidden.is_empty(), "{name}: {hidden:?}");
        }
    }

    #[test]
    fn hidden_ranges_are_exact_for_wide_characters_and_wrapped_lines() {
        let mut e = fed(40, 5, "a\x1b[8m\u{65E5}\x1b[28mb");
        let wide = read(&mut e, TextRegion::Screen).expect("text");
        assert_eq!(wide.text, "a\u{65E5}b");
        assert_eq!(wide.hidden, vec![1..4], "the three bytes of the wide character");

        let line = "abcdefghijklmnopqrstuvwxy";
        let bytes = format!("{}\x1b[8m{}\x1b[28m{}", &line[..19], &line[19..21], &line[21..]);
        let mut e = fed(20, 5, &bytes);
        let joined = read(&mut e, TextRegion::Screen).expect("text");
        assert_eq!(joined.text, line, "the line is still joined");
        assert_eq!(joined.hidden, vec![19..21], "one merged range across the wrap");
    }

    #[test]
    fn the_alternate_screen_is_read_as_shown_and_the_primary_one_is_left_alone() {
        let mut e = fed(20, 5, "primary\r\n\x1b[?1049h\x1b[HALT1\r\nALT2");
        assert_eq!(text_of(&mut e, TextRegion::Screen).as_deref(), Some("ALT1\nALT2"));
        assert_eq!(text_of(&mut e, TextRegion::LastRows(5)).as_deref(), Some("ALT1\nALT2"));
        e.feed(b"\x1b[?1049l");
        assert_eq!(text_of(&mut e, TextRegion::Screen).as_deref(), Some("primary"));
    }

    #[test]
    fn the_text_holds_no_control_characters() {
        let mut e = fed(
            40,
            6,
            "\x1b[31mred\x1b[0m \x1b]0;title\x07\x1b]52;c;aGVsbG8=\x07bell\x07 x\x08y\ttab\r\nabc\rXY\r\nend",
        );
        e.select(SelectOp::All).expect("select all");
        for region in [TextRegion::Screen, TextRegion::LastRows(6), TextRegion::Selection] {
            let text = text_of(&mut e, region).expect("text");
            for c in text.chars() {
                assert!(c == '\n' || !(c < ' ' || c == '\u{7f}' || ('\u{80}'..='\u{9f}').contains(&c)), "{region:?}: {c:?}");
            }
            assert!(text.contains("XYc"), "{region:?}: the overwritten characters are gone: {text:?}");
            assert!(!text.contains("abc"), "{region:?}: {text:?}");
            assert!(text.contains("red") && text.contains("bell") && text.contains("end"), "{text:?}");
        }
    }

    #[test]
    fn a_grapheme_flood_is_bounded_and_does_not_fail() {
        let flood = format!("e{}after\r\nnext", "\u{301}".repeat(10_000));
        let mut e = fed(20, 5, &flood);
        let read = read(&mut e, TextRegion::Screen).expect("text");
        assert!(read.text.len() < 4096, "{} bytes", read.text.len());
        assert!(read.text.contains("after\nnext"), "the text after the cell is intact: {:?}", read.text);
    }

    #[test]
    fn a_screen_taller_than_the_row_cap_keeps_the_last_rows_and_says_so() {
        let mut e = engine(20, 2500);
        let lines: Vec<String> = (0..2400).map(|n| format!("L{n:04}")).collect();
        e.feed(lines.join("\r\n").as_bytes());
        let read = read(&mut e, TextRegion::Screen).expect("text");
        assert!(read.truncated);
        assert!(read.text.starts_with("L0500"), "{:?}", &read.text[..20]);
        assert!(read.text.ends_with("L2399"));
    }

    #[test]
    fn a_flood_of_alternating_hidden_cells_leaves_no_hidden_character_uncovered() {
        let row = "\x1b[8mH\x1b[28mv".repeat(100);
        let rows: Vec<String> = (0..30).map(|_| row.clone()).collect();
        let mut e = fed(200, 30, &rows.join("\r\n"));
        let read = read(&mut e, TextRegion::Screen).expect("text");
        assert!(read.hidden.len() <= MAX_HIDDEN_RANGES, "{} ranges", read.hidden.len());
        let mut hidden_chars = 0;
        for (at, _) in read.text.match_indices('H') {
            hidden_chars += 1;
            assert!(read.hidden.iter().any(|r| r.contains(&at)), "the hidden H at {at} lies in no range");
        }
        assert_eq!(hidden_chars, 3000);
        for pair in read.hidden.windows(2) {
            assert!(pair[0].end <= pair[1].start);
        }
    }

    /// OSC 133 marks: `ESC ] 133 ; payload ST`.
    fn mark(payload: &str) -> String {
        format!("\x1b]133;{payload}\x1b\\")
    }

    /// A prompt, the input, the output lines and the end mark `end`.
    fn command(input: &str, output: &[&str], end: &str) -> String {
        let mut s = format!("{}$ {}{input}\r\n{}", mark("A"), mark("B"), mark("C"));
        for line in output {
            s += &format!("{line}\r\n");
        }
        s + &mark(end)
    }

    /// The active prompt, with no line break after it.
    fn active_prompt() -> String {
        format!("{}$ {}", mark("A"), mark("B"))
    }

    /// Three commands and an active prompt; the screen row above the prompt is the last output line.
    fn three_commands() -> (String, u32) {
        let s = command("echo 1", &["one-a", "one-b"], "D;0")
            + &command("false", &["two-a"], "D;1")
            + &command("ls", &["three-a", "three-b", "three-c"], "D;0")
            + &active_prompt();
        // Rows: 3 (first), 2 (second), 4 (third); the prompt is row 9 and its output ends on row 8.
        (s, 8)
    }

    const THREE: &str = "three-a\nthree-b\nthree-c";

    #[test]
    fn last_output_is_the_text_above_the_active_prompt() {
        let (bytes, _) = three_commands();
        let mut e = fed(20, 6, &bytes);
        let read = read(&mut e, TextRegion::LastOutput).expect("an output");
        assert_eq!(read.text, THREE, "without the prompt, the input line or the other commands");
        assert!(read.hidden.is_empty() && !read.truncated);
    }

    #[test]
    fn last_output_follows_the_command_through_scrolling_and_a_scrolled_back_viewport() {
        let lines: Vec<String> = (1..=9).map(|n| format!("out-{n}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let bytes = command("first", &["early"], "D;0") + &command("big", &refs, "D;0") + &active_prompt();
        let mut e = fed(20, 4, &bytes);
        let want = lines.join("\n");
        assert_eq!(text_of(&mut e, TextRegion::LastOutput).as_deref(), Some(want.as_str()));
        e.scroll(-3);
        assert_eq!(text_of(&mut e, TextRegion::LastOutput).as_deref(), Some(want.as_str()), "scrolled back");
        e.scroll_to_bottom();
        assert_eq!(text_of(&mut e, TextRegion::LastOutput).as_deref(), Some(want.as_str()), "and back again");
    }

    #[test]
    fn last_output_without_marks_is_none() {
        let plain: String = (0..30).map(|n| format!("plain {n:02}\r\n")).collect();
        let mut e = fed(20, 6, &plain);
        assert_eq!(read(&mut e, TextRegion::LastOutput), None);
        e.feed(b"\x1b[2J\x1b[H");
        assert_eq!(read(&mut e, TextRegion::LastOutput), None, "a cleared screen");
    }

    #[test]
    fn a_two_line_prompt_is_not_taken_for_output() {
        let bytes = command("ls", &["prev-a", "prev-b"], "D;0") + &format!("{}info line\r\n$ {}", mark("A"), mark("B"));
        let mut e = fed(20, 6, &bytes);
        let read = read(&mut e, TextRegion::LastOutput).expect("an output");
        assert_eq!(read.text, "prev-a\nprev-b", "never the information line of the prompt");
    }

    #[test]
    fn last_output_equals_the_spike_primitive_when_nothing_is_hidden() {
        let (bytes, above) = three_commands();
        let mut e = fed(20, 6, &bytes);
        let primitive = e.output_at(above, MAX_READ_BYTES).expect("output_at").expect("an output");
        let region = text_of(&mut e, TextRegion::LastOutput).expect("an output");
        assert_eq!(region.trim_matches('\n'), primitive.trim_matches('\n'));

        let lines = ["wrapped line that is longer than the twenty columns", "short"];
        let bytes = command("cat", &lines, "D;0") + &active_prompt();
        let mut e = fed(20, 8, &bytes);
        let above = 1 + 3 + 1 - 1;
        let primitive = e.output_at(above, MAX_READ_BYTES).expect("output_at").expect("an output");
        let region = text_of(&mut e, TextRegion::LastOutput).expect("an output");
        assert_eq!(region.trim_matches('\n'), primitive.trim_matches('\n'), "soft-wrapped lines too");
    }

    #[test]
    fn last_output_reports_concealed_text_in_its_region() {
        let bytes = command("show", &["ok", "pre \x1b[8mSECRET\x1b[28m post"], "D;0") + &active_prompt();
        let mut e = fed(30, 6, &bytes);
        let read = read(&mut e, TextRegion::LastOutput).expect("an output");
        assert_eq!(read.text, "ok\npre SECRET post");
        assert_eq!(read.hidden, vec![7..13]);
    }

    #[test]
    fn a_forged_mark_can_only_select_text_that_is_on_the_screen() {
        // A program prints its own start mark, some lines and its own end mark after the real output.
        let forged = format!("{}forged-1\r\nforged-2\r\n{}", mark("C"), mark("D;0"));
        let bytes = command("cat evil", &["real-1", "real-2"], "D;0")
            + &format!("{}$ {}", mark("A"), mark("B"))
            + &forged
            + &active_prompt();
        let mut e = fed(20, 10, &bytes);
        let everything = text_of(&mut e, TextRegion::LastRows(50)).expect("text");
        if let Some(read) = read(&mut e, TextRegion::LastOutput) {
            assert!(read.text.len() <= MAX_READ_BYTES);
            for line in read.text.lines() {
                // A forged start mark can begin in the middle of a row: the line is a piece of a row.
                assert!(everything.lines().any(|l| l.contains(line)), "{line:?} is not on the screen");
            }
        }
    }

    #[test]
    fn the_output_of_a_command_still_running_is_what_it_printed_so_far() {
        // No end mark and no prompt after the output.
        let bytes = format!("{}$ {}sleep 5\r\n{}partial-1\r\npartial-2", mark("A"), mark("B"), mark("C"));
        let mut e = fed(20, 6, &bytes);
        assert_eq!(text_of(&mut e, TextRegion::LastOutput).as_deref(), Some("partial-1\npartial-2"));
    }

    #[test]
    fn a_command_without_output_gives_none() {
        let bytes = command("true", &[], "D;0") + &active_prompt();
        let mut e = fed(20, 6, &bytes);
        assert_eq!(read(&mut e, TextRegion::LastOutput), None);
    }

    #[test]
    #[ignore = "release-mode timing, run with --release -- --ignored"]
    fn a_read_of_the_whole_budget_is_cut_flagged_and_fast() {
        // A 250x2000 screen with every cell styled and two bytes wide: the largest read the row budget allows, and one that
        // the byte bound has to cut.
        let line = format!("\x1b[1;32m{}\x1b[0m", "\u{e9}\u{e0}\u{fc}\u{f6}\u{e4}\u{df}\u{f1}\u{e7}\u{ea}\u{ef}".repeat(25));
        let rows: Vec<&str> = (0..2000).map(|_| line.as_str()).collect();
        let mut e = fed(250, 2000, &rows.join("\r\n"));
        let started = std::time::Instant::now();
        let big = read(&mut e, TextRegion::LastRows(MAX_READ_ROWS)).expect("text");
        let full = started.elapsed();
        println!("full budget read: {full:?}, {} bytes, truncated {}", big.text.len(), big.truncated);
        assert!(big.truncated, "the byte bound cuts it");
        assert!(big.text.len() <= MAX_READ_BYTES);
        assert!(full < Duration::from_millis(100), "{full:?}");

        let mut small = engine(200, 50);
        let rows: Vec<&str> = (0..50).map(|_| line.as_str()).collect();
        small.feed(rows.join("\r\n").as_bytes());
        let started = std::time::Instant::now();
        let screen = read(&mut small, TextRegion::Screen).expect("text");
        println!("200x50 screen read: {:?}, {} bytes", started.elapsed(), screen.text.len());
    }
}
