//! `wsh ai [--] <prompt>...`: ask the model of the running holzWave.
//!
//! The prompt is the words of the command line joined by single spaces. Text piped into `wsh` is sent along as it is.
//! The answer is untrusted text: it is cleaned (see `clean`) before it is printed. Nothing here can consent, approve or
//! decide anything: the command sends a prompt and an input and prints what comes back.

use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;

use holzwave_ipc::{Cmd, Event, MAX_STDIN, Outcome, Request, stream_request};

use crate::clean;

/// What the printer knows about what it wrote to standard output.
struct Printer<'a> {
    out: &'a mut dyn Write,
    wrote: bool,
    last_newline: bool,
}

impl<'a> Printer<'a> {
    fn new(out: &'a mut dyn Write) -> Self {
        Printer { out, wrote: false, last_newline: true }
    }

    /// Print what an event carries: text, cleaned, to standard output.
    fn event(&mut self, event: &Event) -> io::Result<()> {
        if let Event::Text { text } = event {
            let text = clean::text(text);
            if text.is_empty() {
                return Ok(());
            }
            self.out.write_all(text.as_bytes())?;
            self.out.flush()?;
            self.wrote = true;
            self.last_newline = text.ends_with('\n');
        }
        Ok(())
    }

    /// The end of the answer: one newline when the text did not end with one.
    fn finish(&mut self) -> io::Result<()> {
        if self.wrote && !self.last_newline {
            self.out.write_all(b"\n")?;
            self.out.flush()?;
        }
        Ok(())
    }
}

/// The words of the command line as one prompt.
fn prompt_of(args: &[String]) -> String {
    args.join(" ")
}

fn say(message: &str) {
    let _ = writeln!(io::stderr().lock(), "{message}");
}

/// Run the subcommand with the arguments after `ai`; the result is the exit status.
pub fn run(args: &[String]) -> ExitCode {
    let prompt = prompt_of(args);
    let (dir, token, pane) = match crate::app_env() {
        Ok(env) => env,
        Err((code, message)) => {
            say(&message);
            return ExitCode::from(code);
        }
    };
    let mut input = Vec::new();
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        let mut limited = stdin.lock().take(MAX_STDIN as u64 + 1);
        if let Err(e) = limited.read_to_end(&mut input) {
            say(&format!("wsh: cannot read the input: {e}"));
            return ExitCode::from(1);
        }
        if input.len() > MAX_STDIN {
            say("wsh: the input is longer than 1048576 bytes; send less, for example the last lines");
            return ExitCode::from(2);
        }
    }
    let cmd = Cmd::Ai { prompt, stdin_bytes: input.len() as u32 };
    if let Err(why) = cmd.check() {
        say(&format!("wsh: {why}"));
        return ExitCode::from(2);
    }
    let mut stdout = io::stdout().lock();
    let mut printer = Printer::new(&mut stdout);
    let outcome = stream_request(&dir, &Request { token, pane, cmd }, &input, &mut |event| printer.event(event));
    let _ = printer.finish();
    match outcome {
        Ok(Outcome { ok: true, .. }) => ExitCode::SUCCESS,
        Ok(Outcome { message, .. }) => {
            say(&format!("wsh: {}", clean::text(&message)));
            ExitCode::from(1)
        }
        Err(e) => {
            say(&format!("wsh: holzWave does not answer ({e}); is it still running?"));
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_are_joined_with_single_spaces() {
        let words: Vec<String> = ["why", "is", "it", "slow"].iter().map(|w| (*w).to_owned()).collect();
        assert_eq!(prompt_of(&words), "why is it slow");
        assert_eq!(prompt_of(&[]), "");
    }
}
