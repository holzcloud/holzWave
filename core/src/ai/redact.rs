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
pub const RULES: [&str; 22] = [
    "hidden_text",
    "escape_sequence",
    "control_chars",
    "bidi_control",
    "zero_width",
    "tag_chars",
    "private_key",
    "anthropic_key",
    "openai_key",
    "google_api_key",
    "github_token",
    "aws_access_key",
    "slack_token",
    "jwt",
    "authorization",
    "bearer_token",
    "cookie",
    "url_credentials",
    "secret_assignment",
    "high_entropy",
    "marker_escaped",
    "truncated",
];

const HIDDEN_TEXT: usize = 0;
const ESCAPE_SEQUENCE: usize = 1;
const CONTROL_CHARS: usize = 2;
const BIDI_CONTROL: usize = 3;
const ZERO_WIDTH: usize = 4;
const TAG_CHARS: usize = 5;
const PRIVATE_KEY: usize = 6;
const ANTHROPIC_KEY: usize = 7;
const OPENAI_KEY: usize = 8;
const GOOGLE_API_KEY: usize = 9;
const GITHUB_TOKEN: usize = 10;
const AWS_ACCESS_KEY: usize = 11;
const SLACK_TOKEN: usize = 12;
const JWT: usize = 13;
const AUTHORIZATION: usize = 14;
const BEARER_TOKEN: usize = 15;
const COOKIE: usize = 16;
const URL_CREDENTIALS: usize = 17;
const SECRET_ASSIGNMENT: usize = 18;
const HIGH_ENTROPY: usize = 19;
const MARKER_ESCAPED: usize = 20;
const TRUNCATED: usize = 21;

/// Characters that carry no visible shape and are removed, as sorted inclusive ranges of code points with the rule that
/// counts them. Hidden instructions travel in them: the tag block spells ASCII invisibly, and the zero-width and
/// direction controls reorder or hide text. The zero-width joiner and non-joiner are removed too, although emoji
/// sequences and some scripts use them: they are the usual carrier, so the model reads the text without them. U+FE0F is
/// kept because emoji need it.
const INVISIBLE: &[(u32, u32, usize)] = &[
    (0x00AD, 0x00AD, ZERO_WIDTH),
    (0x034F, 0x034F, ZERO_WIDTH),
    (0x061C, 0x061C, BIDI_CONTROL),
    (0x115F, 0x1160, ZERO_WIDTH),
    (0x17B4, 0x17B5, ZERO_WIDTH),
    (0x180B, 0x180E, ZERO_WIDTH),
    (0x200B, 0x200D, ZERO_WIDTH),
    (0x200E, 0x200F, BIDI_CONTROL),
    (0x202A, 0x202E, BIDI_CONTROL),
    (0x2060, 0x2065, ZERO_WIDTH),
    (0x2066, 0x2069, BIDI_CONTROL),
    (0x206A, 0x206F, ZERO_WIDTH),
    (0x3164, 0x3164, ZERO_WIDTH),
    (0xFE00, 0xFE0E, ZERO_WIDTH),
    (0xFEFF, 0xFEFF, ZERO_WIDTH),
    (0xFFA0, 0xFFA0, ZERO_WIDTH),
    (0xFFF9, 0xFFFB, ZERO_WIDTH),
    (0xE0000, 0xE007F, TAG_CHARS),
    (0xE0100, 0xE01EF, ZERO_WIDTH),
];

/// The rule that removes `c` if it is an invisible character.
fn invisible_rule(c: char) -> Option<usize> {
    let code = c as u32;
    if code < INVISIBLE[0].0 {
        return None;
    }
    let at = INVISIBLE.partition_point(|&(_, last, _)| last < code);
    INVISIBLE.get(at).filter(|&&(first, _, _)| first <= code).map(|&(_, _, rule)| rule)
}

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

    /// A text with the byte `hidden` ranges of it that the terminal engine drew concealed (SGR conceal) or with the
    /// foreground equal to the background. The shell that reads the grid fills them from the cell attributes. The gate
    /// removes those spans and reports them, because the user could not read them and so the model should not.
    pub fn with_hidden(text: impl Into<String>, hidden: Vec<Range<usize>>) -> Self {
        Self { text: text.into(), hidden }
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

    fn add_many(&mut self, rule: usize, n: usize) {
        self.0[rule] = self.0[rule].saturating_add(u32::try_from(n).unwrap_or(u32::MAX));
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

/// Removes every escape sequence with its payload, every control character except line feed and tab, and the
/// characters that carry no visible shape (direction controls, zero-width characters, the tag block).
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
            c => match invisible_rule(c) {
                Some(rule) => tally.add(rule),
                None => out.push(c),
            },
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

/// What a pattern replaces.
enum Target {
    /// The whole match.
    Whole,
    /// The named group `secret`; the rest of the match (the name of the setting, the header) stays.
    Secret,
}

/// A pattern and what it is called.
struct Rule {
    rule: usize,
    regex: Regex,
    placeholder: String,
    target: Target,
    /// A test the candidate has to pass to be replaced, for patterns that only find candidates.
    check: Option<fn(&str) -> bool>,
}

impl Rule {
    fn build(rule: usize, pattern: &str, target: Target) -> Self {
        // The patterns are constants of this file; a failure to compile is a programming error that a test catches.
        let regex = Regex::new(pattern).expect("a redaction pattern compiles");
        Self { rule, regex, placeholder: format!("{PLACEHOLDER_OPEN}{}]", RULES[rule]), target, check: None }
    }

    /// A rule that replaces every match.
    fn whole(rule: usize, pattern: &str) -> Self {
        Self::build(rule, pattern, Target::Whole)
    }

    /// A rule that replaces the group `secret` of every match.
    fn secret(rule: usize, pattern: &str) -> Self {
        Self::build(rule, pattern, Target::Secret)
    }

    fn checked(mut self, check: fn(&str) -> bool) -> Self {
        self.check = Some(check);
        self
    }

    /// `text` with every match replaced, or `None` when nothing was replaced.
    fn apply(&self, text: &str, tally: &mut Tally) -> Option<String> {
        let mut out: Option<String> = None;
        let mut last = 0;
        let mut replace = |span: Range<usize>| {
            let candidate = &text[span.clone()];
            // A value that is already a placeholder (also inside quotes) was redacted by an earlier rule or pass.
            if candidate.trim_start_matches(['"', '\'']).starts_with(PLACEHOLDER_OPEN) {
                return;
            }
            if self.check.is_some_and(|check| !check(candidate)) {
                return;
            }
            let buffer = out.get_or_insert_with(|| String::with_capacity(text.len()));
            buffer.push_str(&text[last..span.start]);
            buffer.push_str(&self.placeholder);
            last = span.end;
            tally.add(self.rule);
        };
        match self.target {
            Target::Whole => {
                for found in self.regex.find_iter(text) {
                    replace(found.range());
                }
            }
            Target::Secret => {
                for captures in self.regex.captures_iter(text) {
                    if let Some(secret) = captures.name("secret") {
                        replace(secret.range());
                    }
                }
            }
        }
        out.map(|mut buffer| {
            buffer.push_str(&text[last..]);
            buffer
        })
    }
}

/// The start of a private key block.
const PEM_BEGIN: &str = r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----";
/// The end of a private key block.
const PEM_END: &str = r"-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----";

/// How many bytes of a token have to look random before the entropy rule considers it.
const ENTROPY_MIN_LENGTH: usize = 40;

/// Bits per byte at or above which a mixed-case token with a digit is taken for a secret. Hex digests stay under 4,
/// natural paths and identifiers under about 4.3, random base64 is above 5.
const ENTROPY_THRESHOLD: f64 = 4.5;

/// The Shannon entropy of the bytes of `text`, in bits per byte.
fn entropy(text: &str) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    let mut counts = [0u32; 256];
    for byte in text.bytes() {
        counts[usize::from(byte)] += 1;
    }
    let total = text.len() as f64;
    counts
        .iter()
        .filter(|&&n| n > 0)
        .map(|&n| {
            let p = f64::from(n) / total;
            -p * p.log2()
        })
        .sum()
}

/// Whether a candidate token looks random: a digit, an upper-case and a lower-case letter, and enough entropy.
fn looks_random(candidate: &str) -> bool {
    candidate.len() >= ENTROPY_MIN_LENGTH
        && candidate.bytes().any(|b| b.is_ascii_digit())
        && candidate.bytes().any(|b| b.is_ascii_uppercase())
        && candidate.bytes().any(|b| b.is_ascii_lowercase())
        && entropy(candidate) >= ENTROPY_THRESHOLD
}

/// The rules, in the order they run. Vendor formats come first, then the shapes that keep a prefix, then the entropy
/// rule, which would otherwise swallow the tokens the named rules report better.
static REDACTION_RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        // A complete block, then a block whose end the window cut off, then the tail of a block whose start it cut off.
        Rule::whole(PRIVATE_KEY, &format!("(?s){PEM_BEGIN}.*?{PEM_END}")),
        Rule::whole(PRIVATE_KEY, &format!("(?s){PEM_BEGIN}.*")),
        Rule::whole(PRIVATE_KEY, &format!("(?m)(?:^[A-Za-z0-9+/=]{{16,}}\n)*^{PEM_END}")),
        Rule::whole(ANTHROPIC_KEY, r"\bsk-ant-[A-Za-z0-9_-]{20,}"),
        Rule::whole(OPENAI_KEY, r"\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{20,}"),
        Rule::whole(GOOGLE_API_KEY, r"\bAIza[A-Za-z0-9_-]{35}"),
        Rule::whole(GITHUB_TOKEN, r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})"),
        Rule::whole(AWS_ACCESS_KEY, r"\b(?:AKIA|ASIA|AGPA|AIDA|AROA|ANPA|ANVA|AIPA)[A-Z0-9]{16}\b"),
        Rule::whole(SLACK_TOKEN, r"\b(?:xox[abposr]-[A-Za-z0-9-]{10,}|xapp-[A-Za-z0-9-]{10,})"),
        Rule::whole(JWT, r"\beyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+"),
        Rule::secret(AUTHORIZATION, r"(?i)\bauthorization[ \t]*:[ \t]*(?<secret>[^\r\n]+)"),
        Rule::secret(BEARER_TOKEN, r"(?i)\bbearer[ \t]+(?<secret>[A-Za-z0-9._~+/=-]{16,})"),
        Rule::secret(COOKIE, r"(?im)^[ \t]*(?:set-)?cookie[ \t]*:[ \t]*(?<secret>[^\r\n]+)"),
        Rule::secret(URL_CREDENTIALS, r"\b[A-Za-z][A-Za-z0-9+.-]*://(?<secret>[^\s/@:]+:[^\s/@]+)@"),
        Rule::secret(
            SECRET_ASSIGNMENT,
            r#"(?i)[A-Za-z0-9_.-]*(?:key|token|secret|password|passwd|pwd|credentials?)["']?[ \t]*[=:][ \t]*(?<secret>"[^"\r\n]*"|'[^'\r\n]*'|[^\s"',;}&]+)"#,
        ),
        Rule::whole(HIGH_ENTROPY, r"[A-Za-z0-9+/_-]{40,}={0,2}").checked(looks_random),
    ]
});

/// Replaces every recognisable secret with `[REDACTED:<rule>]`.
///
/// Run [`sanitise`] first: a key cut by an escape sequence or an invisible character is not recognised. The rules are
/// named in [`RULES`] (private keys, vendor key formats, JWTs, authorization and cookie headers, bearer tokens, URL
/// credentials, secret-named assignments, random-looking tokens). Redacting redacted text changes nothing.
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

/// Removes the hidden spans of `source`, merging overlapping ranges, ignoring reversed and empty ones, clamping to the
/// text and growing a range that starts or ends inside a multi-byte character to the character. The hit counts the
/// removed characters that are not white space.
pub fn drop_hidden(source: &SourceText) -> Cleaned {
    let mut tally = Tally::new();
    let text = drop_hidden_into(source, &mut tally).into_owned();
    Cleaned { text, hits: tally.hits() }
}

fn drop_hidden_into<'a>(source: &'a SourceText, tally: &mut Tally) -> Cow<'a, str> {
    let text = source.text.as_str();
    let mut spans: Vec<(usize, usize)> = source
        .hidden
        .iter()
        .filter(|range| range.start < range.end && range.start < text.len())
        .map(|range| {
            let mut start = range.start;
            while !text.is_char_boundary(start) {
                start -= 1;
            }
            let mut end = range.end.min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            (start, end)
        })
        .collect();
    if spans.is_empty() {
        return Cow::Borrowed(text);
    }
    spans.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut removed = 0;
    let mut at = 0;
    for (start, end) in spans {
        if end <= at {
            continue;
        }
        let start = start.max(at);
        out.push_str(&text[at..start]);
        removed += text[start..end].chars().filter(|c| !c.is_whitespace()).count();
        at = end;
    }
    out.push_str(&text[at..]);
    tally.add_many(HIDDEN_TEXT, removed);
    Cow::Owned(out)
}

/// Neutralises the opening forms of the elements of the envelope inside `text`: the ASCII-case-insensitive sequences
/// `<holzwave-` and `</holzwave-` get their `<` replaced by `&lt;`, so text cannot open, close or forge an element.
pub fn neutralise_markers(text: &str) -> Cleaned {
    let mut tally = Tally::new();
    let text = neutralise_into(text, &mut tally).into_owned();
    Cleaned { text, hits: tally.hits() }
}

fn neutralise_into<'a>(text: &'a str, tally: &mut Tally) -> Cow<'a, str> {
    const NAME: &[u8] = b"holzwave-";
    let bytes = text.as_bytes();
    let mut out: Option<String> = None;
    let mut last = 0;
    for (at, _) in text.match_indices('<') {
        let rest = &bytes[at + 1..];
        let rest = rest.strip_prefix(b"/").unwrap_or(rest);
        if rest.len() < NAME.len() || !rest[..NAME.len()].eq_ignore_ascii_case(NAME) {
            continue;
        }
        let buffer = out.get_or_insert_with(|| String::with_capacity(text.len() + 16));
        buffer.push_str(&text[last..at]);
        buffer.push_str("&lt;");
        last = at + 1;
        tally.add(MARKER_ESCAPED);
    }
    match out {
        Some(mut buffer) => {
            buffer.push_str(&text[last..]);
            Cow::Owned(buffer)
        }
        None => Cow::Borrowed(text),
    }
}

/// Cuts `text` to at most `cap` bytes, keeping `keep`, and says so on a line at the cut. `omitted_before` counts bytes an
/// earlier cut already dropped. Returns the text unchanged when nothing was cut at all.
fn cut(text: &str, keep: Keep, cap: usize, omitted_before: usize, tally: &mut Tally) -> String {
    if text.len() <= cap && omitted_before == 0 {
        return text.trim_end_matches('\n').to_owned();
    }
    let room = cap.saturating_sub(TRUNCATION_RESERVE);
    let (kept, dropped) = if text.len() <= room {
        (text, 0)
    } else {
        match keep {
            Keep::Head => {
                let mut end = room;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                (&text[..end], text.len() - end)
            }
            Keep::Tail => {
                let mut start = text.len() - room;
                while !text.is_char_boundary(start) {
                    start += 1;
                }
                (&text[start..], start)
            }
        }
    };
    tally.add(TRUNCATED);
    let marker = format!("[truncated: {} bytes omitted]", omitted_before + dropped);
    match keep {
        Keep::Head => format!("{}\n{marker}", kept.trim_end_matches('\n')),
        Keep::Tail => format!("{marker}\n{}", kept.trim_end_matches('\n')),
    }
}

/// The cut to [`MAX_RAW_ITEM_BYTES`] before anything scans the text: the kept part and the bytes dropped.
fn raw_cut(text: &str, keep: Keep) -> (&str, usize) {
    if text.len() <= MAX_RAW_ITEM_BYTES {
        return (text, 0);
    }
    match keep {
        Keep::Head => {
            let mut end = MAX_RAW_ITEM_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            (&text[..end], text.len() - end)
        }
        Keep::Tail => {
            let mut start = text.len() - MAX_RAW_ITEM_BYTES;
            while !text.is_char_boundary(start) {
                start += 1;
            }
            (&text[start..], start)
        }
    }
}

/// Sanitise, redact, neutralise, cut, trim: the pipeline behind [`clean_text`] and [`prepare`].
fn clean_into(text: &str, keep: Keep, max_bytes: usize, tally: &mut Tally) -> String {
    let (text, omitted_before) = raw_cut(text, keep);
    let sanitised = sanitise_into(text, tally);
    let redacted = redact_into(&sanitised, tally);
    let neutral = neutralise_into(&redacted, tally);
    cut(&neutral, keep, max_bytes.max(MIN_CAP), omitted_before, tally)
}

/// The smallest cap [`clean_text`] accepts; a smaller value is raised to it.
const MIN_CAP: usize = 128;

/// Cleans any text that leaves the device and was not typed by the user as a prompt: sanitise, then redact, then
/// neutralise the envelope markers, then cut to `max_bytes` (a value below 128 is raised to 128) keeping the chosen end
/// with a line `[truncated: N bytes omitted]` at the cut, then trim trailing newlines.
///
/// This is the door for context items (through [`prepare`]) and for tool results. A text longer than
/// [`MAX_RAW_ITEM_BYTES`] is cut before it is scanned, so every pattern runs over a bounded input.
pub fn clean_text(text: &str, keep: Keep, max_bytes: usize) -> Cleaned {
    let mut tally = Tally::new();
    let text = clean_into(text, keep, max_bytes, &mut tally);
    Cleaned { text, hits: tally.hits() }
}

/// The text of one context item, cleaned: hidden spans dropped, then [`clean_text`] with the end that matters for the
/// kind (the tail for the screen and the last command, the head for the rest) and [`MAX_ITEM_BYTES`]. The hits are
/// merged per rule.
pub fn prepare(kind: ContextKind, source: &SourceText) -> Cleaned {
    let keep = match kind {
        ContextKind::Screen | ContextKind::LastCommand => Keep::Tail,
        ContextKind::Selection | ContextKind::File | ContextKind::Preview => Keep::Head,
    };
    let mut tally = Tally::new();
    let visible = drop_hidden_into(source, &mut tally);
    let text = clean_into(&visible, keep, MAX_ITEM_BYTES, &mut tally);
    Cleaned { text, hits: tally.hits() }
}

/// A label for the user and for the envelope: sanitised, redacted, on one line, and at most [`MAX_LABEL_CHARS`]
/// characters, a cut ending with an ellipsis. Quotes and angle brackets are escaped by the envelope, not here.
pub fn clean_label(label: &str) -> Cleaned {
    let (label, _) = raw_cut(label, Keep::Head);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_is_shannon_bits_per_byte() {
        assert!(entropy("aaaa").abs() < 1e-9);
        assert!((entropy("abcd") - 2.0).abs() < 1e-9);
        assert!((entropy("aabb") - 1.0).abs() < 1e-9);
        assert!(entropy("").abs() < 1e-9);
    }

    #[test]
    fn the_rule_list_matches_the_rules_that_are_built() {
        let distinct: std::collections::BTreeSet<&str> = RULES.iter().copied().collect();
        assert_eq!(RULES.len(), 22);
        assert_eq!(distinct.len(), 22, "rule names are distinct");

        // The rules that are built run in report order and together cover the 14 redaction names.
        let built: Vec<usize> = REDACTION_RULES.iter().map(|rule| rule.rule).collect();
        assert!(built.windows(2).all(|pair| pair[0] <= pair[1]), "built out of order: {built:?}");
        let names: std::collections::BTreeSet<&str> = built.iter().map(|index| RULES[*index]).collect();
        assert_eq!(names.len(), 14, "{names:?}");
        assert_eq!(RULES[PRIVATE_KEY], "private_key");
        assert_eq!(RULES[HIGH_ENTROPY], "high_entropy");
        assert!(built.iter().all(|index| (PRIVATE_KEY..=HIGH_ENTROPY).contains(index)));

        // Every index constant names the rule it is called after.
        for (index, name) in [
            (HIDDEN_TEXT, "hidden_text"),
            (ESCAPE_SEQUENCE, "escape_sequence"),
            (CONTROL_CHARS, "control_chars"),
            (BIDI_CONTROL, "bidi_control"),
            (ZERO_WIDTH, "zero_width"),
            (TAG_CHARS, "tag_chars"),
            (MARKER_ESCAPED, "marker_escaped"),
            (TRUNCATED, "truncated"),
        ] {
            assert_eq!(RULES[index], name);
        }
        // Every pattern compiled, or building the list above would have panicked; the invisible table is sorted.
        assert!(INVISIBLE.windows(2).all(|pair| pair[0].1 < pair[1].0));
    }
}
