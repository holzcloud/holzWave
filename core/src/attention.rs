//! What a pane tells the window when a program wants attention: the bell and the end of a command (docs/design.md
//! section 6.23).
//!
//! A program can ring the bell as fast as it can write, so the pane lets one event through per [`GAP`]. The gate is a
//! UI hint, never a decision about security or content: a bell that is held back is simply not shown again.
//!
//! [`CommandMarks`] reads the end of a command, the `D` mark of OSC 133, with its exit status from the PTY bytes. It
//! exists because the pinned libghostty-vt (rev d2036ba) parses OSC 133 for its own rows but its OSC API (`osc.rs`)
//! exposes only the command type `SemanticPrompt`, no status and no callback (checked in the vendored source). Only
//! the 7-bit forms (`ESC ]`) are read, because those are the only ones the shell hooks (`wsh/hooks`) and the Windows
//! wrapper (`core/src/winshell.rs`) write. What it finds feeds a UI hint (a mark on a tab) and never a decision about
//! security or content.

use std::time::{Duration, Instant};

/// The least time between two attention events of one kind from one pane.
pub const GAP: Duration = Duration::from_millis(500);

/// Lets one call through per [`GAP`]. The reference time is the last call that passed, not the last attempt, so a
/// steady flood still gets two per second and cannot keep the gate closed by trying.
#[derive(Clone, Copy, Debug, Default)]
pub struct Gate {
    last_pass: Option<Instant>,
}

impl Gate {
    /// A gate that lets the first call through.
    pub const fn new() -> Self {
        Self { last_pass: None }
    }

    /// Whether the event at `now` goes through. An instant earlier than the last pass does not (and never panics).
    pub fn pass(&mut self, now: Instant) -> bool {
        let open = match self.last_pass {
            None => true,
            Some(last) => now.checked_duration_since(last).is_some_and(|since| since >= GAP),
        };
        if open {
            self.last_pass = Some(now);
        }
        open
    }
}

/// The longest payload (the bytes between `ESC ]` and the terminator) that can be a mark: `133;D;` + ten digits +
/// room for `;aid=...` options. Anything longer is dropped.
const PAYLOAD_MAX: usize = 24;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Outside any sequence.
    Ground,
    /// Saw `ESC`; `]` opens an OSC.
    Escape,
    /// Inside an OSC, collecting the payload.
    Osc,
    /// Inside an OSC, saw `ESC`; `\` ends it, `]` starts a new one, anything else ends the attempt.
    OscEscape,
    /// Inside an OSC whose payload is too long to be a mark: skipped up to the terminator.
    Skip,
    /// Inside a skipped OSC, saw `ESC`.
    SkipEscape,
}

/// A streaming scanner for `ESC ] 133 ; D [; status [; options]]` ended by BEL or `ESC \`. Fixed 24-byte buffer, no
/// allocation, one pass over the bytes; the same marks come out however the stream is cut into chunks. CAN and SUB
/// cancel a sequence in progress, a new `ESC ]` inside one starts over.
#[derive(Clone, Debug)]
pub struct CommandMarks {
    phase: Phase,
    buf: [u8; PAYLOAD_MAX],
    len: usize,
}

impl Default for CommandMarks {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandMarks {
    pub const fn new() -> Self {
        Self { phase: Phase::Ground, buf: [0; PAYLOAD_MAX], len: 0 }
    }

    /// Scan `bytes` and call `mark` with the exit status of every `D` mark in them (`None` when the mark carries
    /// none). Anything that is not exactly a `D` mark is ignored.
    pub fn feed(&mut self, bytes: &[u8], mut mark: impl FnMut(Option<u32>)) {
        for &b in bytes {
            self.step(b, &mut mark);
        }
    }

    fn open(&mut self) {
        self.phase = Phase::Osc;
        self.len = 0;
    }

    fn step(&mut self, b: u8, mark: &mut impl FnMut(Option<u32>)) {
        // CAN and SUB cancel whatever is in progress, in every phase.
        if b == CAN || b == SUB {
            self.phase = Phase::Ground;
            return;
        }
        match self.phase {
            Phase::Ground => {
                if b == ESC {
                    self.phase = Phase::Escape;
                }
            }
            Phase::Escape => match b {
                b']' => self.open(),
                ESC => {}
                _ => self.phase = Phase::Ground,
            },
            Phase::Osc => match b {
                BEL => {
                    self.finish(mark);
                    self.phase = Phase::Ground;
                }
                ESC => self.phase = Phase::OscEscape,
                _ if self.len < PAYLOAD_MAX => {
                    self.buf[self.len] = b;
                    self.len += 1;
                }
                _ => self.phase = Phase::Skip,
            },
            Phase::OscEscape => match b {
                b'\\' => {
                    self.finish(mark);
                    self.phase = Phase::Ground;
                }
                b']' => self.open(),
                ESC => {}
                _ => self.phase = Phase::Ground,
            },
            Phase::Skip => match b {
                BEL => self.phase = Phase::Ground,
                ESC => self.phase = Phase::SkipEscape,
                _ => {}
            },
            Phase::SkipEscape => match b {
                b'\\' => self.phase = Phase::Ground,
                b']' => self.open(),
                ESC => {}
                _ => self.phase = Phase::Ground,
            },
        }
    }

    /// The payload is complete: report it when it is exactly a `D` mark.
    fn finish(&mut self, mark: &mut impl FnMut(Option<u32>)) {
        if let Some(status) = parse_d_mark(&self.buf[..self.len]) {
            mark(status);
        }
    }
}

/// `Some(None)` for `133;D`, `Some(Some(n))` for `133;D;n` with one to ten digits that fit a `u32`, followed by the
/// end or `;` and options; `None` for everything else.
fn parse_d_mark(payload: &[u8]) -> Option<Option<u32>> {
    let rest = payload.strip_prefix(b"133;D")?;
    if rest.is_empty() {
        return Some(None);
    }
    let rest = rest.strip_prefix(b";")?;
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > 10 {
        return None;
    }
    if rest.len() > digits && rest[digits] != b';' {
        return None;
    }
    let mut value: u64 = 0;
    for &d in &rest[..digits] {
        value = value * 10 + u64::from(d - b'0');
    }
    u32::try_from(value).ok().map(Some)
}
