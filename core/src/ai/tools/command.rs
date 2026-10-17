//! The pure half of the command tool: the words the model hears about a command, the fit of two output streams into one
//! budget, the quoting of a text for a POSIX shell and the string that is sent to a remote host.
//!
//! Nothing here starts a process, touches a file, reads the clock or the environment. Every type that could hold text has
//! a hand-written `Debug` that prints sizes only, and no fixed phrase echoes a command or a folder.

use std::fmt;

use crate::ssh::{ExecEnd, ExecResult, MAX_EXEC_COMMAND};

use super::super::types::Size;
use super::ToolOutput;

/// The most bytes of both streams together that the model sees, with the end of each stream kept.
pub const SHOWN_BYTES: usize = 15360;
/// The most bytes kept of each stream of a command while it runs.
pub const CAPTURE_BYTES: usize = 65536;

/// The heading of the standard output section.
pub const STDOUT_HEAD: &str = "Standard output:";
/// The heading of the standard error section.
pub const STDERR_HEAD: &str = "Standard error:";
/// What a command that printed nothing says.
pub const NO_OUTPUT: &str = "No output.";
/// The line that tells a stream starts after its beginning.
pub const OMITTED: &str = "[earlier output omitted]";
/// The first line of a command that ended by itself without telling a status.
pub const UNKNOWN_EXIT: &str = "Exit status: unknown.";
/// The first line of a command whose connection broke while it ran.
pub const LOST_LINE: &str =
    "Stopped: the connection to the host was lost while the command ran. Whether it finished is unknown.";
/// The first line of a command that was stopped.
pub const CANCELLED_LINE: &str = "Stopped: the command was ended before it finished.";

/// The refusal of a working directory that is not a folder on this computer.
pub const CWD_MISSING: &str = "the working directory does not exist or is not a folder";
/// The refusal of a call that names no working directory when none is known.
pub const CWD_UNKNOWN: &str = "the working directory of that pane is not known; give cwd as an absolute path";
/// The refusal of a working directory on a remote host that is not absolute.
pub const CWD_REMOTE_RELATIVE: &str = "a working directory on a remote host must be an absolute path";
/// The refusal of a command whose remote string is longer than the exec limit.
pub const REMOTE_TOO_LONG: &str = "the command is too long to send to a remote host";
/// The failure of a process that could not be started.
pub const START_FAILED: &str = "the command could not be started";
/// The failure of a remote host that could not run the command.
pub const REMOTE_FAILED: &str = "the host could not run the command";

/// The first line of a command that ended by itself: `Exit status: 0.`
pub fn exit_line(status: u32) -> String {
    format!("Exit status: {status}.")
}

/// The first line of a command that was ended after its time limit.
pub fn timed_out_line(secs: u64) -> String {
    let unit = if secs == 1 { "second" } else { "seconds" };
    format!("Stopped: the command ran longer than {secs} {unit} and was ended.")
}

/// The text the model hears about a finished command, and whether the command failed.
///
/// The text is the status or stop line, a blank line, then the sections `Standard output:` and `Standard error:` for the
/// streams that printed something (each without trailing line breaks, separated by a blank line), or `No output.` when
/// both are empty. A stream that was cut, upstream or by [`fit`], starts with `[earlier output omitted]` on its own line.
/// The output is a failure unless the command ended by itself with status 0. The caller passes the time limit of the call
/// in `timeout_secs`.
pub fn report(result: &ExecResult, timeout_secs: u64) -> ToolOutput {
    let first = match (result.end, result.status) {
        (ExecEnd::Finished, Some(status)) => exit_line(status),
        (ExecEnd::Finished, None) => UNKNOWN_EXIT.to_owned(),
        (ExecEnd::TimedOut, _) => timed_out_line(timeout_secs),
        (ExecEnd::Cancelled, _) => CANCELLED_LINE.to_owned(),
        (ExecEnd::Lost, _) => LOST_LINE.to_owned(),
    };
    let (stdout, out_upstream) = decode(&result.stdout, result.truncated);
    let (stderr, err_upstream) = decode(&result.stderr, result.truncated);
    let shown = fit(&stdout, &stderr, SHOWN_BYTES);
    let mut sections = Vec::new();
    for (head, text, cut) in [
        (STDOUT_HEAD, &shown.out, shown.out_cut || out_upstream),
        (STDERR_HEAD, &shown.err, shown.err_cut || err_upstream),
    ] {
        let text = text.trim_end_matches(['\n', '\r']);
        if text.is_empty() {
            continue;
        }
        sections.push(if cut { format!("{head}\n{OMITTED}\n{text}") } else { format!("{head}\n{text}") });
    }
    let body = if sections.is_empty() { NO_OUTPUT.to_owned() } else { sections.join("\n\n") };
    let text = format!("{first}\n\n{body}");
    if result.end == ExecEnd::Finished && result.status == Some(0) { ToolOutput::new(text) } else { ToolOutput::failed(text) }
}

/// One stream as text, and whether its start was cut before it reached the report: the tail of a stream that hit the
/// capture limit may start inside a character, so up to three leading continuation bytes are dropped first.
fn decode(bytes: &[u8], truncated: bool) -> (String, bool) {
    let cut = truncated && bytes.len() >= CAPTURE_BYTES;
    let skipped = if cut { bytes.iter().take(3).take_while(|byte| **byte & 0xC0 == 0x80).count() } else { 0 };
    (String::from_utf8_lossy(&bytes[skipped..]).into_owned(), cut)
}

/// What [`fit`] keeps of the two streams. `Debug` prints sizes only.
#[derive(Clone, PartialEq, Eq)]
pub struct Shown {
    /// The end of standard output that fits.
    pub out: String,
    /// The end of standard error that fits.
    pub err: String,
    /// Whether the shown part of standard output is shorter than the stream.
    pub out_cut: bool,
    /// Whether the shown part of standard error is shorter than the stream.
    pub err_cut: bool,
}

impl fmt::Debug for Shown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shown")
            .field("out", &Size(self.out.len()))
            .field("err", &Size(self.err.len()))
            .field("out_cut", &self.out_cut)
            .field("err_cut", &self.err_cut)
            .finish()
    }
}

/// Fits two streams into `budget` bytes. Both stay whole when they fit. Otherwise each gets at most half of the budget, and
/// a stream that needs less leaves the rest to the other. The end of a stream is kept, cut on a character boundary.
pub fn fit(stdout: &str, stderr: &str, budget: usize) -> Shown {
    let (out_room, err_room) = if stdout.len().saturating_add(stderr.len()) <= budget {
        (stdout.len(), stderr.len())
    } else {
        let half = budget / 2;
        if stderr.len() <= half {
            (budget - stderr.len(), stderr.len())
        } else if stdout.len() <= half {
            (stdout.len(), budget - stdout.len())
        } else {
            (half, half)
        }
    };
    let out = end_of(stdout, out_room);
    let err = end_of(stderr, err_room);
    Shown { out_cut: out.len() < stdout.len(), err_cut: err.len() < stderr.len(), out: out.to_owned(), err: err.to_owned() }
}

/// The last at most `max` bytes of `text`, starting on a character boundary.
fn end_of(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// `text` as one word for a POSIX shell: in single quotes, each embedded single quote written as `'\''`. The shell reads the
/// word back as exactly `text`, whatever it holds.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// The string sent to a remote host: `cd -- <quoted cwd> && exec sh -c -- <quoted command>`, without the `cd` half when
/// there is no working directory. `None` when the string is longer than the exec limit. Both parts are quoted, so a
/// remote login shell reads them as the two words they are.
pub fn remote_command(command: &str, cwd: Option<&str>) -> Option<String> {
    let run = format!("exec sh -c -- {}", quote(command));
    let text = match cwd {
        Some(cwd) => format!("cd -- {} && {run}", quote(cwd)),
        None => run,
    };
    (text.len() <= MAX_EXEC_COMMAND).then_some(text)
}
