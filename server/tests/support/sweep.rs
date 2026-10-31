//! The canary sweep: canaries built at run time, every form in which a program could write them down, and the scanners that look
//! for them in bytes, files, trees, lines, responses and `Debug` text.
//!
//! A hit names the surface, the canary and the form. It never carries the canary text, so a failing sweep leaks nothing into
//! the output of the test run.
//!
//! Tests pull this file in with `#[path = "support/sweep.rs"] mod sweep;`.
#![allow(dead_code)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;

/// What a canary stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Category {
    /// A value the product must keep from every surface except the one place it shows it to its owner.
    Secret,
    /// A value of a person's own data (a command, a scrollback line, a file) that may live in the owner's home only.
    Content { owner: String },
}

/// One canary: a name for reports and a text for searching.
#[derive(Clone)]
pub struct Canary {
    pub name: String,
    pub category: Category,
    text: String,
}

/// The prefix of every canary; the rest is 24 random hexadecimal characters.
pub const PREFIX: &str = "hwcanary-";

/// The number of hexadecimal characters after the prefix.
pub const RANDOM_HEX: usize = 24;

impl std::fmt::Debug for Canary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Canary({}, {:?})", self.name, self.category)
    }
}

impl Canary {
    /// A new canary with random content; `name` is only a label.
    pub fn new(name: &str, category: Category) -> Canary {
        let mut raw = [0u8; RANDOM_HEX / 2];
        getrandom::fill(&mut raw).expect("random bytes");
        let mut text = String::from(PREFIX);
        for byte in raw {
            let _ = write!(text, "{byte:02x}");
        }
        Canary { name: name.to_owned(), category, text }
    }

    /// A canary with a given text, for the pure tests only.
    pub fn with_text(name: &str, category: Category, text: &str) -> Canary {
        Canary { name: name.to_owned(), category, text: text.to_owned() }
    }

    /// The text. Use it to send the canary somewhere, never in a message.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_secret(&self) -> bool {
        self.category == Category::Secret
    }

    /// Every form of the canary that the sweep searches.
    pub fn forms(&self) -> Vec<(&'static str, Vec<u8>)> {
        forms_of(&self.text)
    }
}

// ---- the forms ---------------------------------------------------------------------------------------------------------------

/// Names of the forms, in the order of [`forms_of`] (the same name may stand for several alignments).
pub const FORM_NAMES: [&str; 14] = [
    "plain",
    "base64",
    "base64url",
    "hex-lower",
    "hex-upper",
    "percent",
    "percent-all",
    "json",
    "json-unicode",
    "base32",
    "base32-lower",
    "reversed",
    "utf16le",
    "utf16be",
];

fn hex(bytes: &[u8], upper: bool) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        if upper {
            let _ = write!(out, "{byte:02X}");
        } else {
            let _ = write!(out, "{byte:02x}");
        }
    }
    out
}

fn percent(bytes: &[u8], every_byte: bool) -> String {
    let mut out = String::new();
    for &byte in bytes {
        if !every_byte && (byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte)) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The text as the inside of a JSON string.
fn json_inner(text: &str) -> String {
    let quoted = serde_json::to_string(text).expect("json");
    quoted[1..quoted.len() - 1].to_owned()
}

/// Every character as a `\u00XX` escape, which a JSON reader reads as the text.
fn json_unicode(text: &str) -> String {
    let mut out = String::new();
    for unit in text.encode_utf16() {
        let _ = write!(out, "\\u{unit:04x}");
    }
    out
}

const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32 without padding.
pub fn base32(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(BASE32[((buffer >> bits) & 31) as usize]));
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(BASE32[((buffer << (5 - bits)) & 31) as usize]));
    }
    out
}

/// The part of a block encoding of `bytes` that does not depend on the bytes before and after it: the encoding of `lead` zero
/// bytes and `bytes`, without the characters that the zero bytes or the end of the text touch. `bits` is the width of one
/// character, `encode` the encoder of the alphabet.
fn aligned(bytes: &[u8], lead: usize, bits: usize, encode: impl Fn(&[u8]) -> String) -> String {
    let mut padded = vec![0u8; lead];
    padded.extend_from_slice(bytes);
    let whole = encode(&padded);
    let from = (lead * 8).div_ceil(bits);
    let to = (padded.len() * 8) / bits;
    if from >= to { String::new() } else { whole[from..to].to_owned() }
}

/// Every form in which `text` could be written into a file, a log or a message: as it is, in base64 (standard and URL
/// alphabets, at the three alignments a longer buffer can give it), hexadecimal in both cases, percent-encoded, JSON-escaped,
/// base32 (five alignments, both cases), reversed, and as UTF-16 of either byte order.
pub fn forms_of(text: &str) -> Vec<(&'static str, Vec<u8>)> {
    let bytes = text.as_bytes();
    let mut forms: Vec<(&'static str, Vec<u8>)> = vec![("plain", bytes.to_vec())];
    for lead in 0..3 {
        let standard = aligned(bytes, lead, 6, |b| base64::engine::general_purpose::STANDARD_NO_PAD.encode(b));
        forms.push(("base64", standard.into_bytes()));
        let url = aligned(bytes, lead, 6, |b| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b));
        forms.push(("base64url", url.into_bytes()));
    }
    forms.push(("hex-lower", hex(bytes, false).into_bytes()));
    forms.push(("hex-upper", hex(bytes, true).into_bytes()));
    forms.push(("percent", percent(bytes, false).into_bytes()));
    forms.push(("percent-all", percent(bytes, true).into_bytes()));
    forms.push(("json", json_inner(text).into_bytes()));
    forms.push(("json-unicode", json_unicode(text).into_bytes()));
    for lead in 0..5 {
        let upper = aligned(bytes, lead, 5, |b| base32(b));
        forms.push(("base32-lower", upper.to_ascii_lowercase().into_bytes()));
        forms.push(("base32", upper.into_bytes()));
    }
    forms.push(("reversed", bytes.iter().rev().copied().collect()));
    forms.push(("utf16le", text.encode_utf16().flat_map(u16::to_le_bytes).collect()));
    forms.push(("utf16be", text.encode_utf16().flat_map(u16::to_be_bytes).collect()));
    forms.retain(|(_, form)| form.len() >= 8);
    // A form that is the same bytes as an earlier one (the JSON and percent forms of a text with a plain alphabet) is one search.
    let mut seen: Vec<Vec<u8>> = Vec::new();
    forms.retain(|(_, form)| {
        if seen.contains(form) {
            false
        } else {
            seen.push(form.clone());
            true
        }
    });
    forms
}

// ---- the hits ----------------------------------------------------------------------------------------------------------------

/// A place where a canary was found: the surface, the canary's name and the form. Never the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub surface: String,
    pub canary: String,
    pub form: &'static str,
    pub offset: usize,
}

impl Finding {
    /// A line for a failure message.
    pub fn line(&self) -> String {
        format!("{}: canary {} in form {} at {}", self.surface, self.canary, self.form, self.offset)
    }
}

/// The canaries to look for and their expanded forms.
pub struct Sweep {
    canaries: Vec<Canary>,
    forms: Vec<(usize, &'static str, Vec<u8>)>,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Totals of what a scan read.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub files: usize,
    pub bytes: usize,
    pub lines: usize,
}

impl std::ops::AddAssign for Counts {
    fn add_assign(&mut self, other: Counts) {
        self.files += other.files;
        self.bytes += other.bytes;
        self.lines += other.lines;
    }
}

impl Sweep {
    pub fn new(canaries: &[Canary]) -> Sweep {
        let mut forms = Vec::new();
        for (index, canary) in canaries.iter().enumerate() {
            for (name, form) in canary.forms() {
                forms.push((index, name, form));
            }
        }
        Sweep { canaries: canaries.to_vec(), forms }
    }

    pub fn canaries(&self) -> &[Canary] {
        &self.canaries
    }

    /// Every canary form in `bytes`: one finding per canary and form.
    pub fn scan_bytes(&self, surface: &str, bytes: &[u8]) -> Vec<Finding> {
        let mut found = Vec::new();
        for (index, name, form) in &self.forms {
            if let Some(offset) = find(bytes, form) {
                found.push(Finding { surface: surface.to_owned(), canary: self.canaries[*index].name.clone(), form: name, offset });
            }
        }
        found
    }

    /// The same for the text of a surface that is not a file.
    pub fn scan_text(&self, surface: &str, text: &str) -> Vec<Finding> {
        self.scan_bytes(surface, text.as_bytes())
    }

    /// A file; a file that cannot be read is a finding of its own kind (`unreadable`) so that it cannot hide a canary.
    pub fn scan_file(&self, path: &Path, counts: &mut Counts) -> Vec<Finding> {
        let surface = path.display().to_string();
        match std::fs::read(path) {
            Ok(bytes) => {
                counts.files += 1;
                counts.bytes += bytes.len();
                self.scan_bytes(&surface, &bytes)
            }
            Err(_) => vec![Finding { surface, canary: "(file)".to_owned(), form: "unreadable", offset: 0 }],
        }
    }

    /// Every regular file below `root` (links are not followed), the write-ahead log and the shared-memory file of a database
    /// included. The file name is part of the surface, so a canary in a name is found as well.
    pub fn scan_tree(&self, root: &Path, counts: &mut Counts) -> Vec<Finding> {
        let mut found = Vec::new();
        for file in files_below(root) {
            found.extend(self.scan_file(&file, counts));
            let name = file.strip_prefix(root).unwrap_or(&file).display().to_string();
            found.extend(self.scan_bytes(&format!("name of {}", file.display()), name.as_bytes()));
        }
        found
    }

    /// Lines of a captured stream; the count of lines is added to `counts`.
    pub fn scan_lines(&self, surface: &str, lines: &[String], counts: &mut Counts) -> Vec<Finding> {
        let mut found = Vec::new();
        for (number, line) in lines.iter().enumerate() {
            counts.lines += 1;
            counts.bytes += line.len();
            for mut finding in self.scan_bytes(surface, line.as_bytes()) {
                finding.offset = number;
                found.push(finding);
            }
        }
        found
    }
}

/// Every regular file below `root` and every directory name, links not followed, sorted.
pub fn files_below(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(path),
                Ok(kind) if kind.is_file() => found.push(path),
                _ => {}
            }
        }
    }
    found.sort();
    found
}

/// What a surface was allowed to hold: the findings that are not on the list. `allowed` names a canary and the surface prefix
/// where it may be (the one place the product shows a secret to its owner).
pub fn unexpected(findings: Vec<Finding>, allowed: &[(&str, &str)]) -> Vec<Finding> {
    findings
        .into_iter()
        .filter(|finding| !allowed.iter().any(|(canary, surface)| finding.canary == *canary && finding.surface.starts_with(surface)))
        .collect()
}

/// A content canary is allowed under its owner's home. Everything else that is a content canary found outside it is a finding.
pub fn outside_home(findings: Vec<Finding>, sweep: &Sweep, homes: &[(String, PathBuf)]) -> Vec<Finding> {
    findings
        .into_iter()
        .filter(|finding| {
            let Some(canary) = sweep.canaries().iter().find(|c| c.name == finding.canary) else { return true };
            let Category::Content { owner } = &canary.category else { return true };
            let Some((_, home)) = homes.iter().find(|(name, _)| name == owner) else { return true };
            !finding.surface.starts_with(&home.display().to_string())
        })
        .collect()
}
