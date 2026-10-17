//! `wsh ai [--] <prompt>...`: ask the model of the running holzWave.
//!
//! The command line is a small public contract that scripts rely on:
//!
//! - The prompt is the words after `ai`, joined by single spaces. A word that starts with a dash before the first ordinary
//!   word is an unknown option (exit status 2): those words are reserved for later flags, and `--` ends them, so
//!   `wsh ai -- --model x` asks the text `--model x` and `wsh ai explain -v` asks `explain -v`.
//! - Standard input is read only when it is not a terminal, only after the environment check (outside a holzWave terminal
//!   `wsh ai` does nothing, reads nothing and opens no socket), at most 1 MiB, and it must be UTF-8 text without a NUL;
//!   anything else is exit status 2 and nothing is sent. Input is never cut: too much is refused.
//! - The answer is printed on standard output and ends with exactly one newline; notes of the app (waiting for a
//!   confirmation, for example) go to standard error with a `wsh: ` prefix, and so does the number of characters removed
//!   from what came back.
//! - Exit status 0 is a finished answer, 1 a refusal, an app that does not answer, or a closed standard output (which also
//!   cancels the request), 2 a usage or input problem.
//!
//! The answer is untrusted text: everything that came from the app passes `clean` before it is written, so a terminal
//! cannot be made to act on it. Nothing here can consent, approve or decide anything: the command sends a prompt and an
//! input and prints what comes back.

use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;

use holzwave_ipc::{Cmd, Event, MAX_STDIN, Outcome, Request, stream_request};

use crate::clean;

const INPUT_TOO_LONG: &str = "wsh: the input is longer than 1048576 bytes; send less, for example the last lines";
const INPUT_NOT_TEXT: &str = "wsh: the input is not text (it is not valid UTF-8 or has a NUL byte)";
const NOTHING_TO_ASK: &str = "wsh: nothing to ask: give a prompt or pipe text in";

/// The prompt: the words joined by single spaces. A word with a leading dash before the first ordinary word is an option;
/// none exists yet, so it is an error, and `--` ends the options. From the first ordinary word on, every word (dash words
/// included) is prompt text.
fn parse_prompt(args: &[String]) -> Result<String, String> {
    let mut words: Vec<&str> = Vec::new();
    let mut options_over = false;
    for word in args {
        if !options_over {
            if word == "--" {
                options_over = true;
                continue;
            }
            if word.starts_with('-') {
                return Err(format!("unknown option `{}`: put -- before a prompt that starts with a dash", clean::text(word)));
            }
            options_over = true;
        }
        words.push(word);
    }
    Ok(words.join(" "))
}

/// The text piped in. A terminal gives no input and is not read. At most [`MAX_STDIN`] bytes are taken (one more is read to
/// tell that there is too much), and the input must be UTF-8 text without a NUL.
fn read_input(source: &mut dyn Read, interactive: bool) -> Result<Vec<u8>, (u8, String)> {
    if interactive {
        return Ok(Vec::new());
    }
    let mut input = Vec::new();
    if let Err(e) = source.take(MAX_STDIN as u64 + 1).read_to_end(&mut input) {
        return Err((1, format!("wsh: cannot read the input: {e}")));
    }
    if input.len() > MAX_STDIN {
        return Err((2, INPUT_TOO_LONG.into()));
    }
    if input.contains(&0) || std::str::from_utf8(&input).is_err() {
        return Err((2, INPUT_NOT_TEXT.into()));
    }
    Ok(input)
}

/// Writes what comes back: text to standard output, notes and the closing remarks to standard error. Everything from the
/// app is cleaned first.
struct Printer<'a> {
    out: &'a mut dyn Write,
    err: &'a mut dyn Write,
    wrote: bool,
    last_newline: bool,
    removed: usize,
    /// The kind of the first error of standard output, when there was one.
    out_error: Option<io::ErrorKind>,
}

impl<'a> Printer<'a> {
    fn new(out: &'a mut dyn Write, err: &'a mut dyn Write) -> Self {
        Printer { out, err, wrote: false, last_newline: true, removed: 0, out_error: None }
    }

    /// Print what an event carries. An error of standard output is returned, which drops the connection.
    fn event(&mut self, event: &Event) -> io::Result<()> {
        match event {
            Event::Text { text } => {
                let text = clean::clean(text, &mut self.removed);
                if text.is_empty() {
                    return Ok(());
                }
                self.out.write_all(text.as_bytes()).and_then(|()| self.out.flush()).inspect_err(|e| self.out_error = Some(e.kind()))?;
                self.wrote = true;
                self.last_newline = text.ends_with('\n');
            }
            Event::Note { text } => {
                let note = clean::clean(text, &mut self.removed);
                let _ = writeln!(self.err, "wsh: {note}");
            }
            _ => {}
        }
        Ok(())
    }

    /// The end: what the outcome was, as the exit status. One newline closes an answer that did not end with one, and
    /// standard error says how many characters were removed and what the app said last.
    fn finish(&mut self, outcome: io::Result<Outcome>) -> u8 {
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(e) => return self.failed(&e),
        };
        let message = clean::clean(&outcome.message, &mut self.removed);
        if self.wrote && !self.last_newline && self.out.write_all(b"\n").and_then(|()| self.out.flush()).is_err() {
            return 1;
        }
        match self.removed {
            0 => {}
            1 => {
                let _ = writeln!(self.err, "wsh: 1 control character was removed from the answer");
            }
            n => {
                let _ = writeln!(self.err, "wsh: {n} control characters were removed from the answer");
            }
        }
        if outcome.ok {
            if !message.is_empty() {
                let _ = writeln!(self.err, "wsh: {message}");
            }
            return 0;
        }
        let message = if message.is_empty() { "the request failed" } else { message.as_str() };
        let _ = writeln!(self.err, "wsh: {message}");
        1
    }

    /// An error of the connection or of standard output.
    fn failed(&mut self, error: &io::Error) -> u8 {
        if self.out_error == Some(io::ErrorKind::BrokenPipe) {
            // The reader of the answer went away: nothing more to say, and the connection is dropped already.
            return 1;
        }
        let _ = match (self.out_error, error.kind()) {
            (Some(_), _) => writeln!(self.err, "wsh: cannot write the answer: {error}"),
            (None, io::ErrorKind::UnexpectedEof) => writeln!(self.err, "wsh: holzWave closed the connection before the answer ended"),
            (None, _) => writeln!(self.err, "wsh: holzWave does not answer ({error}); is it still running?"),
        };
        1
    }
}

fn say(err: &mut dyn Write, message: &str) {
    let _ = writeln!(err, "{message}");
}

/// Run the subcommand with the arguments after `ai`; the result is the exit status.
pub fn run(args: &[String]) -> ExitCode {
    let mut stderr = io::stderr().lock();
    let prompt = match parse_prompt(args) {
        Ok(prompt) => prompt,
        Err(message) => {
            say(&mut stderr, &format!("wsh: {message}"));
            return ExitCode::from(2);
        }
    };
    // The environment first: outside a holzWave terminal nothing is read and no socket is opened.
    let (dir, token, pane) = match crate::app_env() {
        Ok(env) => env,
        Err((code, message)) => {
            say(&mut stderr, &message);
            return ExitCode::from(code);
        }
    };
    let stdin = io::stdin();
    let input = match read_input(&mut stdin.lock(), stdin.is_terminal()) {
        Ok(input) => input,
        Err((code, message)) => {
            say(&mut stderr, &message);
            return ExitCode::from(code);
        }
    };
    if prompt.trim().is_empty() && input.is_empty() {
        say(&mut stderr, NOTHING_TO_ASK);
        return ExitCode::from(2);
    }
    let cmd = Cmd::Ai { prompt, stdin_bytes: u32::try_from(input.len()).unwrap_or(u32::MAX) };
    if let Err(why) = cmd.check() {
        say(&mut stderr, &format!("wsh: {why}"));
        return ExitCode::from(2);
    }
    let mut stdout = io::stdout().lock();
    let mut printer = Printer::new(&mut stdout, &mut stderr);
    let outcome = stream_request(&dir, &Request { token, pane, cmd }, &input, &mut |event| printer.event(event));
    ExitCode::from(printer.finish(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    fn text(t: &str) -> Event {
        Event::Text { text: t.into() }
    }

    fn done(ok: bool, message: &str) -> io::Result<Outcome> {
        Ok(Outcome { ok, message: message.into() })
    }

    #[test]
    fn the_words_are_joined_with_single_spaces() {
        assert_eq!(parse_prompt(&words(&["why", "is", "it", "slow"])).unwrap(), "why is it slow");
        assert_eq!(parse_prompt(&[]).unwrap(), "");
    }

    #[test]
    fn the_options_before_the_prompt_are_reserved_and_the_double_dash_ends_them() {
        for bad in [&["--model", "x", "q"][..], &["-v"], &["-"], &["--print", "--", "q"], &["-h"]] {
            let error = parse_prompt(&words(bad)).unwrap_err();
            assert!(error.starts_with("unknown option `") && error.contains("put -- before"), "{bad:?}: {error}");
        }
        assert_eq!(parse_prompt(&words(&["--", "--model", "x"])).unwrap(), "--model x");
        assert_eq!(parse_prompt(&words(&["--"])).unwrap(), "", "only the end of the options");
        assert_eq!(parse_prompt(&words(&["--", "--", "x"])).unwrap(), "-- x", "a second one is prompt text");
        // The name of an option is shown cleaned.
        assert!(!parse_prompt(&words(&["-\u{1b}[2J"])).unwrap_err().contains('\u{1b}'));
    }

    #[test]
    fn words_after_the_first_word_are_prompt_text() {
        assert_eq!(parse_prompt(&words(&["explain", "-v"])).unwrap(), "explain -v");
        assert_eq!(parse_prompt(&words(&["what", "does", "--force", "do", "-f"])).unwrap(), "what does --force do -f");
        assert_eq!(parse_prompt(&words(&["a", "--"])).unwrap(), "a --");
        assert_eq!(parse_prompt(&words(&["", "-x"])).unwrap(), " -x", "an empty word is an ordinary word");
    }

    /// A source that fails the test when it is read.
    struct Unread;

    impl Read for Unread {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("a terminal must not be read"))
        }
    }

    #[test]
    fn terminal_input_is_not_read() {
        assert_eq!(read_input(&mut Unread, true).unwrap(), Vec::<u8>::new());
        assert_eq!(read_input(&mut Unread, false).unwrap_err().0, 1, "a read error is a failure of wsh, not a usage error");
    }

    /// A source of endless `a` that counts what it handed out.
    struct Endless(usize);

    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            buf.fill(b'a');
            self.0 += buf.len();
            Ok(buf.len())
        }
    }

    #[test]
    fn input_over_the_limit_is_refused_without_reading_further() {
        let mut source = Endless(0);
        let (code, message) = read_input(&mut source, false).unwrap_err();
        assert_eq!((code, message.as_str()), (2, INPUT_TOO_LONG));
        assert!(source.0 <= MAX_STDIN + 1, "{} bytes were taken", source.0);
        let exactly = vec![b'x'; MAX_STDIN];
        assert_eq!(read_input(&mut exactly.as_slice(), false).unwrap().len(), MAX_STDIN, "the limit itself is allowed");
    }

    #[test]
    fn input_that_is_not_text_is_refused() {
        for bad in [&[0xffu8, 0xfe][..], b"a\0b", b"\xc3"] {
            assert_eq!(read_input(&mut &*bad, false).unwrap_err(), (2, INPUT_NOT_TEXT.to_owned()), "{bad:?}");
        }
        assert_eq!(read_input(&mut "na\u{ef}ve\n".as_bytes(), false).unwrap(), "na\u{ef}ve\n".as_bytes());
        assert_eq!(read_input(&mut &b""[..], false).unwrap(), b"", "no input is fine");
    }

    #[test]
    fn the_answer_ends_with_one_newline() {
        for (chunks, expect) in [
            (&["a", "b"][..], "ab\n"),
            (&["ab\n"], "ab\n"),
            (&["ab", "\n"], "ab\n"),
            (&["ab\n\n"], "ab\n\n"),
            (&[], ""),
            (&["\u{1b}"], ""),
        ] {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let mut printer = Printer::new(&mut out, &mut err);
            for chunk in chunks {
                printer.event(&text(chunk)).unwrap();
            }
            let status = printer.finish(done(true, ""));
            assert_eq!(status, 0);
            assert_eq!(String::from_utf8(out).unwrap(), expect, "{chunks:?}");
        }
    }

    #[test]
    fn removed_characters_are_counted_once_on_standard_error() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut printer = Printer::new(&mut out, &mut err);
        printer.event(&text("\u{1b}]0;t\u{7}")).unwrap();
        printer.event(&Event::Note { text: "wait\u{1b}".into() }).unwrap();
        printer.event(&text("line\r\n")).unwrap();
        assert_eq!(printer.finish(done(false, "no\u{7}")), 1);
        assert_eq!(String::from_utf8(out).unwrap(), "]0;tline\n");
        let err = String::from_utf8(err).unwrap();
        assert_eq!(err, "wsh: wait\nwsh: 4 control characters were removed from the answer\nwsh: no\n");

        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut printer = Printer::new(&mut out, &mut err);
        printer.event(&text("a\u{7}")).unwrap();
        assert_eq!(printer.finish(done(true, "done")), 0);
        assert_eq!(String::from_utf8(err).unwrap(), "wsh: 1 control character was removed from the answer\nwsh: done\n");

        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut printer = Printer::new(&mut out, &mut err);
        assert_eq!(printer.finish(done(false, "")), 1);
        assert_eq!(String::from_utf8(err).unwrap(), "wsh: the request failed\n");
    }

    /// A writer that fails.
    struct Failing(io::ErrorKind);

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_closed_standard_output_ends_the_request_quietly() {
        let mut out = Failing(io::ErrorKind::BrokenPipe);
        let mut err = Vec::new();
        let mut printer = Printer::new(&mut out, &mut err);
        let error = printer.event(&text("x")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe, "the callback returns it, which drops the connection");
        assert_eq!(printer.finish(Err(error)), 1);
        assert!(err.is_empty(), "no message: {:?}", String::from_utf8_lossy(&err));

        let mut out = Failing(io::ErrorKind::PermissionDenied);
        let mut err = Vec::new();
        let mut printer = Printer::new(&mut out, &mut err);
        let error = printer.event(&text("x")).unwrap_err();
        assert_eq!(printer.finish(Err(error)), 1);
        assert!(String::from_utf8(err).unwrap().starts_with("wsh: cannot write the answer"));

        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut printer = Printer::new(&mut out, &mut err);
        assert_eq!(printer.finish(Err(io::Error::from(io::ErrorKind::UnexpectedEof))), 1);
        assert_eq!(String::from_utf8(err).unwrap(), "wsh: holzWave closed the connection before the answer ended\n");
    }
}
