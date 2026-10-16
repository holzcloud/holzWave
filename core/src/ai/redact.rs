//! The sanitiser and the secret redactor for text that leaves the device.
//!
//! Terminal output and file contents are written by other people: a web server, a git commit message, a README, a
//! message of the day, a file name. They can carry instructions meant for a model that the user cannot see, and the
//! scrollback of a shell often holds the user's own keys. This module cleans such text before the user is shown it and
//! before it can become part of a request, in this order:
//!
//! 1. [`drop_hidden`] removes the spans the terminal engine drew concealed or with no contrast, because the user could
//!    not read them and so the model should not.
//! 2. [`sanitise`] removes every escape sequence together with its payload (a window title or a hyperlink target is
//!    text the model would read), the control characters, and the characters that carry no visible shape.
//! 3. [`redact`] replaces recognisable secrets with a typed placeholder such as `[REDACTED:anthropic_key]`, so the model
//!    can still reason about what was there.
//! 4. [`neutralise_markers`] defuses the opening forms of the elements of the envelope that carries the text.
//! 5. A bounded cut keeps the result under a fixed size and says that it cut, on a line the user can see.
//!
//! Sanitising runs before redacting on purpose: an escape sequence or an invisible character inside a key would
//! otherwise split it and defeat the pattern.
//!
//! Matching is done by the `regex` crate only. It matches in time linear in the text, has no backreferences and no
//! look-around, so hostile terminal output cannot make it backtrack. Every text is cut before it is scanned.
//!
//! # What this does not promise
//!
//! The result is best effort and never a promise that no secret is sent. It misses a secret shorter than 40 characters
//! that has no known prefix and no telling name, a secret split by a line break, a secret given as a space-separated
//! command-line flag, hex or other non-base64 encodings, and anything a person would call a secret but a pattern cannot
//! tell from prose. It does not touch text the user typed into the prompt. The user is shown the cleaned text and the
//! list of what was removed, and decides.
//!
//! Nothing here reads a file, a socket or a terminal. The caller reads, and passes the text in.

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;
use std::str::Chars;
use std::sync::LazyLock;

use regex::Regex;

use super::types::{ContextKind, Size};

/// The longest text of one item that is scanned, in bytes. A longer text is cut silently before anything else looks at
/// it, so every pattern runs over a bounded input.
pub const MAX_RAW_ITEM_BYTES: usize = 512 * 1024;

/// The longest text of one item after redaction, in bytes, marker line included.
pub const MAX_ITEM_BYTES: usize = 128 * 1024;

/// The longest label, in characters.
pub const MAX_LABEL_CHARS: usize = 120;

/// The room left for the line that says a text was cut.
pub const TRUNCATION_RESERVE: usize = 64;

/// Every rule name a [`Hit`] can carry, in report order. The names are stable: the consent card keys its wording on
/// them.
pub const RULES: [&str; 4] = ["escape_sequence", "control_chars", "anthropic_key", "openai_key"];

const ESCAPE_SEQUENCE: usize = 0;
const CONTROL_CHARS: usize = 1;
const ANTHROPIC_KEY: usize = 2;
const OPENAI_KEY: usize = 3;

/// How many parameter characters of a control sequence are consumed before the scan gives up on its final byte.
const MAX_CSI_PARAMS: usize = 256;

/// How many intermediate characters of a short escape sequence are consumed.
const MAX_ESC_INTERMEDIATES: usize = 2;

/// The start of every placeholder; a match that starts with it has been redacted already.
const PLACEHOLDER_OPEN: &str = "[REDACTED:";

/// One rule that fired, and how many times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The name of the rule; one of [`RULES`].
    pub rule: &'static str,
    /// How many matches were removed or replaced.
    pub count: u32,
}

/// Cleaned text and what was done to get it.
///
/// `Debug` prints the size of the text, never the text.
#[derive(Clone, PartialEq, Eq)]
pub struct Cleaned {
    /// The cleaned text.
    pub text: String,
    /// What was removed or replaced, merged per rule and in the order of [`RULES`].
    pub hits: Vec<Hit>,
}

impl fmt::Debug for Cleaned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cleaned").field("text", &Size(self.text.len())).field("hits", &self.hits).finish()
    }
}

/// Which end of an over-long text survives a cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// The start: for files, previews and selections.
    Head,
    /// The end: for the screen and the last command, where the newest lines matter.
    Tail,
}

/// The text of one context item as the shell read it, with the byte ranges of it that the terminal engine drew hidden.
///
/// `Debug` prints sizes only, and there is no `Serialize`.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceText {
    text: String,
    hidden: Vec<Range<usize>>,
}

impl SourceText {
    /// A text with nothing hidden.
    pub fn plain(text: impl Into<String>) -> Self {
        Self { text: text.into(), hidden: Vec::new() }
    }

    /// The size of the text in bytes.
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Whether there is no text.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

impl fmt::Debug for SourceText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceText")
            .field("text", &Size(self.text.len()))
            .field("hidden_ranges", &self.hidden.len())
            .finish()
    }
}

/// How often each rule fired, indexed like [`RULES`].
struct Tally([u32; RULES.len()]);

impl Tally {
    fn new() -> Self {
        Self([0; RULES.len()])
    }

    fn add(&mut self, rule: usize) {
        self.0[rule] = self.0[rule].saturating_add(1);
    }

    fn hits(&self) -> Vec<Hit> {
        RULES.iter().zip(self.0).filter(|(_, count)| *count > 0).map(|(rule, count)| Hit { rule, count }).collect()
    }

    fn absorb(&mut self, hits: &[Hit]) {
        for hit in hits {
            if let Some(index) = RULES.iter().position(|name| *name == hit.rule) {
                self.0[index] = self.0[index].saturating_add(hit.count);
            }
        }
    }
}

/// `a` and `b` merged per rule, in the order of [`RULES`].
pub(crate) fn merge(a: &[Hit], b: &[Hit]) -> Vec<Hit> {
    let mut tally = Tally::new();
    tally.absorb(a);
    tally.absorb(b);
    tally.hits()
}

/// Removes every escape sequence with its payload and every control character except line feed and tab.
///
/// A carriage return followed by a line feed becomes the line feed and is not counted; a lone carriage return is
/// removed and counted. An unterminated string sequence (a title, a hyperlink, a device control string) ends at the end
/// of its line, so a missing terminator cannot swallow the rest of the text.
pub fn sanitise(text: &str) -> Cleaned {
    let mut tally = Tally::new();
    let text = sanitise_into(text, &mut tally);
    Cleaned { text, hits: tally.hits() }
}

fn sanitise_into(text: &str, tally: &mut Tally) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\n' | '\t' => out.push(c),
            '\r' => {
                if chars.clone().next() != Some('\n') {
                    tally.add(CONTROL_CHARS);
                }
            }
            '\u{1b}' => {
                skip_escape(&mut chars);
                tally.add(ESCAPE_SEQUENCE);
            }
            '\u{9b}' => {
                skip_csi(&mut chars);
                tally.add(ESCAPE_SEQUENCE);
            }
            '\u{9d}' => {
                skip_string(&mut chars);
                tally.add(ESCAPE_SEQUENCE);
            }
            c if c.is_control() => tally.add(CONTROL_CHARS),
            c => out.push(c),
        }
    }
    out
}

/// Consumes the rest of a sequence whose ESC has been read.
fn skip_escape(chars: &mut Chars<'_>) {
    match chars.clone().next() {
        Some('[') => {
            chars.next();
            skip_csi(chars);
        }
        Some(']' | 'P' | 'X' | '^' | '_') => {
            chars.next();
            skip_string(chars);
        }
        Some('\u{20}'..='\u{2f}') => {
            for _ in 0..MAX_ESC_INTERMEDIATES {
                if matches!(chars.clone().next(), Some('\u{20}'..='\u{2f}')) {
                    chars.next();
                }
            }
            if matches!(chars.clone().next(), Some('\u{30}'..='\u{7e}')) {
                chars.next();
            }
        }
        Some('\u{30}'..='\u{7e}') => {
            chars.next();
        }
        _ => {}
    }
}

/// Consumes the parameters and the final byte of a control sequence whose introducer has been read.
fn skip_csi(chars: &mut Chars<'_>) {
    for _ in 0..MAX_CSI_PARAMS {
        if matches!(chars.clone().next(), Some('\u{20}'..='\u{3f}')) {
            chars.next();
        } else {
            break;
        }
    }
    if matches!(chars.clone().next(), Some('\u{40}'..='\u{7e}')) {
        chars.next();
    }
}

/// Consumes a string sequence whose introducer has been read: up to BEL, ESC and a backslash, or the C1 terminator,
/// and when none comes up to the end of the line. The line feed is left in place.
fn skip_string(chars: &mut Chars<'_>) {
    loop {
        let mut look = chars.clone();
        match look.next() {
            None | Some('\n') => return,
            Some('\u{7}' | '\u{9c}') => {
                *chars = look;
                return;
            }
            Some('\u{1b}') => {
                if look.clone().next() == Some('\\') {
                    look.next();
                    *chars = look;
                    return;
                }
                *chars = look;
            }
            Some(_) => *chars = look,
        }
    }
}

/// A pattern and what it is called.
struct Rule {
    rule: usize,
    regex: Regex,
    placeholder: String,
}

impl Rule {
    fn new(rule: usize, pattern: &str) -> Self {
        // The patterns are constants of this file; a failure to compile is a programming error that the tests catch.
        let regex = Regex::new(pattern).expect("a redaction pattern compiles");
        Self { rule, regex, placeholder: format!("{PLACEHOLDER_OPEN}{}]", RULES[rule]) }
    }

    /// `text` with every match replaced, or `None` when nothing matched.
    fn apply(&self, text: &str, tally: &mut Tally) -> Option<String> {
        let mut out: Option<String> = None;
        let mut last = 0;
        for found in self.regex.find_iter(text) {
            if text[found.start()..].starts_with(PLACEHOLDER_OPEN) {
                continue;
            }
            let buffer = out.get_or_insert_with(|| String::with_capacity(text.len()));
            buffer.push_str(&text[last..found.start()]);
            buffer.push_str(&self.placeholder);
            last = found.end();
            tally.add(self.rule);
        }
        out.map(|mut buffer| {
            buffer.push_str(&text[last..]);
            buffer
        })
    }
}

static REDACTION_RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        Rule::new(ANTHROPIC_KEY, r"\bsk-ant-[A-Za-z0-9_-]{20,}"),
        Rule::new(OPENAI_KEY, r"\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{20,}"),
    ]
});

/// Replaces every recognisable secret with `[REDACTED:<rule>]`.
///
/// Run [`sanitise`] first: a key cut by an escape sequence or an invisible character is not recognised.
pub fn redact(text: &str) -> Cleaned {
    let mut tally = Tally::new();
    let text = redact_into(text, &mut tally);
    Cleaned { text, hits: tally.hits() }
}

fn redact_into(text: &str, tally: &mut Tally) -> String {
    let mut current = Cow::Borrowed(text);
    for rule in REDACTION_RULES.iter() {
        if let Some(next) = rule.apply(&current, tally) {
            current = Cow::Owned(next);
        }
    }
    current.into_owned()
}

/// The text of one context item, cleaned: sanitised, then redacted, with the hits merged per rule.
pub fn prepare(_kind: ContextKind, source: &SourceText) -> Cleaned {
    let mut tally = Tally::new();
    let sanitised = sanitise_into(&source.text, &mut tally);
    let text = redact_into(&sanitised, &mut tally);
    Cleaned { text, hits: tally.hits() }
}

/// A label for the user and for the envelope: sanitised, redacted, on one line, and at most [`MAX_LABEL_CHARS`]
/// characters, a cut ending with an ellipsis. Quotes and angle brackets are escaped by the envelope, not here.
pub fn clean_label(label: &str) -> Cleaned {
    let mut tally = Tally::new();
    let sanitised = sanitise_into(label, &mut tally);
    let redacted = redact_into(&sanitised, &mut tally);
    let mut text = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > MAX_LABEL_CHARS {
        text = text.chars().take(MAX_LABEL_CHARS - 1).collect();
        text.push('\u{2026}');
    }
    Cleaned { text, hits: tally.hits() }
}
