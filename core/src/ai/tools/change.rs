//! The pure half of a file change: what a write tool would turn a file into, and the line diff the card shows.
//!
//! Nothing here opens a file, starts a process, reads a clock or reaches the network. The host reads the file and hands
//! its text over as a [`Base`]; [`plan`] makes the text that would be written from the call and that base, and [`diff`]
//! makes the rows the person reads. The diff is built from the very text whose digest the card's hash covers, and the
//! builder checks its own script before it formats a row: the unchanged and removed lines joined are exactly the old
//! text and the unchanged and added lines joined are exactly the new text, or no card is made. Every type that holds
//! file text has a hand-written `Debug` that prints sizes only.

use std::collections::HashMap;
use std::fmt;

use serde::Serialize;
use serde::ser::{SerializeStruct, Serializer};
use sha2::{Digest, Sha256};

use super::super::types::Size;
use super::catalog::{hex, visible};
use super::{DIFF_CONTEXT, MAX_BASE_BYTES, MAX_DIFF_CHARS, MAX_DIFF_ROWS, Tool};

/// The most comparisons the edit script may spend before the change counts as too large to show.
const WORK_LIMIT: usize = 20_000_000;

/// The text a note row carries after a last line that has no line break.
const NO_NEWLINE: &str = "No newline at end of file";

/// What the host found at the path a write tool will change: no file, or the text of the file.
#[derive(Clone, PartialEq, Eq)]
pub enum Base {
    /// The file does not exist.
    Absent,
    /// The file exists and holds this text.
    Text(String),
}

impl Base {
    /// `none` for a file that does not exist, otherwise `sha256:` and the 64 lower-case hex digits of the SHA-256 of the
    /// UTF-8 bytes of the text. An empty file has a digest, so it differs from no file.
    pub fn digest(&self) -> String {
        match self {
            Self::Absent => "none".to_owned(),
            Self::Text(text) => {
                let digest: [u8; 32] = Sha256::digest(text.as_bytes()).into();
                format!("sha256:{}", hex(&digest))
            }
        }
    }

    /// The text, when there is a file.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Absent => None,
            Self::Text(text) => Some(text),
        }
    }

    /// The length of the text in bytes; zero for no file.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.text().map_or(0, str::len)
    }
}

impl fmt::Debug for Base {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => f.write_str("absent"),
            Self::Text(text) => write!(f, "{:?}", Size(text.len())),
        }
    }
}

/// What kind of change a diff shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    /// The whole text of an existing file is replaced.
    Replace,
    /// A file that does not exist yet is made.
    Create,
    /// Parts of an existing file are replaced.
    Edit,
}

/// The text a write tool would leave in the file, and the text that is there now.
#[derive(Clone)]
pub struct Change {
    /// The text now in the file; none for a file that does not exist.
    pub before: Option<String>,
    /// The text the file would hold afterwards.
    pub after: String,
    /// What kind of change it is.
    pub kind: DiffKind,
}

impl fmt::Debug for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Change")
            .field("kind", &self.kind)
            .field("before", &self.before.as_ref().map(|text| Size(text.len())))
            .field("after", &Size(self.after.len()))
            .finish()
    }
}

/// Why a call does not make a change, or a change cannot be shown. The phrases are fixed one-line sentences that hold no
/// path and no text, because they go back to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeFault {
    /// The tool does not change a file.
    NotAWrite,
    /// An edit names a file that does not exist.
    NoFile,
    /// The text to replace is not in the file.
    NoMatch,
    /// The text to replace occurs more than once and the call did not ask for every occurrence.
    Ambiguous,
    /// The text to replace and the text to put in its place are the same.
    Same,
    /// The file already holds the text.
    Unchanged,
    /// The result would be larger than the agent may write.
    TooLarge,
    /// The change is too large to show in full.
    NotShown,
}

impl ChangeFault {
    /// The stable snake-case name of the fault.
    pub fn code(self) -> &'static str {
        match self {
            Self::NotAWrite => "not_a_write",
            Self::NoFile => "no_file",
            Self::NoMatch => "no_match",
            Self::Ambiguous => "ambiguous",
            Self::Same => "same",
            Self::Unchanged => "unchanged",
            Self::TooLarge => "too_large",
            Self::NotShown => "not_shown",
        }
    }

    /// The sentence the model is told: one line, no path and no file text.
    pub fn phrase(self) -> &'static str {
        match self {
            Self::NotAWrite => "this tool does not change a file",
            Self::NoFile => "the file does not exist; use write_file to create it",
            Self::NoMatch => "old_text was not found in the file",
            Self::Ambiguous => {
                "old_text occurs more than once; add surrounding text to make it unique or set replace_all"
            }
            Self::Same => "old_text and new_text are the same",
            Self::Unchanged => "the file already holds that text",
            Self::TooLarge => "the result would be larger than the agent may write",
            Self::NotShown => "the change is too large to show; make smaller changes",
        }
    }
}

/// What `tool` would turn the file `base` describes into.
///
/// `write_file` replaces a text file or makes one that does not exist; `edit_file` replaces exactly one match of its
/// `old_text`, or every match when it says so, and needs a file. Any other tool is [`ChangeFault::NotAWrite`].
pub fn plan(tool: &Tool, base: &Base) -> Result<Change, ChangeFault> {
    match tool {
        Tool::WriteFile(args) => {
            if base.text() == Some(args.content.as_str()) {
                return Err(ChangeFault::Unchanged);
            }
            let kind = if matches!(base, Base::Absent) { DiffKind::Create } else { DiffKind::Replace };
            Ok(Change { before: base.text().map(str::to_owned), after: args.content.clone(), kind })
        }
        Tool::EditFile(args) => {
            let Some(text) = base.text() else { return Err(ChangeFault::NoFile) };
            if args.old_text == args.new_text {
                return Err(ChangeFault::Same);
            }
            let found = text.matches(args.old_text.as_str()).count();
            if found == 0 {
                return Err(ChangeFault::NoMatch);
            }
            let replace_all = args.replace_all();
            if found > 1 && !replace_all {
                return Err(ChangeFault::Ambiguous);
            }
            let replaced = if replace_all { found } else { 1 };
            // The size is worked out before the text is built: a short `old_text` in a large file, replaced everywhere
            // with a long text, must not allocate what it would be refused for.
            let grown = replaced as u128 * args.new_text.len() as u128;
            let shrunk = replaced as u128 * args.old_text.len() as u128;
            let size = (text.len() as u128 + grown).saturating_sub(shrunk);
            if size > MAX_BASE_BYTES as u128 {
                return Err(ChangeFault::TooLarge);
            }
            let after = if replace_all {
                text.replace(args.old_text.as_str(), &args.new_text)
            } else {
                text.replacen(args.old_text.as_str(), &args.new_text, 1)
            };
            Ok(Change { before: Some(text.to_owned()), after, kind: DiffKind::Edit })
        }
        _ => Err(ChangeFault::NotAWrite),
    }
}

/// One row of a diff.
#[derive(Clone, PartialEq, Eq)]
pub enum DiffRow {
    /// A line that is in both texts.
    Ctx {
        /// Its number in the old text, counted from 1.
        old: u32,
        /// Its number in the new text, counted from 1.
        new: u32,
        /// The line, with hidden characters written out.
        text: String,
    },
    /// A line that is only in the old text.
    Del {
        /// Its number in the old text.
        old: u32,
        /// The line, with hidden characters written out.
        text: String,
    },
    /// A line that is only in the new text.
    Add {
        /// Its number in the new text.
        new: u32,
        /// The line, with hidden characters written out.
        text: String,
    },
    /// A run of unchanged lines that is left out.
    Gap {
        /// How many lines it stands for.
        skipped: u32,
    },
    /// A remark about the line before it.
    Note {
        /// The remark.
        text: String,
    },
}

impl DiffRow {
    /// The characters of the row's text, counted in UTF-16 units the way a page counts them.
    fn units(&self) -> usize {
        match self {
            Self::Ctx { text, .. } | Self::Del { text, .. } | Self::Add { text, .. } | Self::Note { text } => {
                text.encode_utf16().count()
            }
            Self::Gap { .. } => 0,
        }
    }
}

impl Serialize for DiffRow {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Ctx { old, new, text } => {
                let mut row = serializer.serialize_struct("DiffRow", 4)?;
                row.serialize_field("op", "ctx")?;
                row.serialize_field("old", old)?;
                row.serialize_field("new", new)?;
                row.serialize_field("text", text)?;
                row.end()
            }
            Self::Del { old, text } => {
                let mut row = serializer.serialize_struct("DiffRow", 4)?;
                row.serialize_field("op", "del")?;
                row.serialize_field("old", old)?;
                row.serialize_field("new", &None::<u32>)?;
                row.serialize_field("text", text)?;
                row.end()
            }
            Self::Add { new, text } => {
                let mut row = serializer.serialize_struct("DiffRow", 4)?;
                row.serialize_field("op", "add")?;
                row.serialize_field("old", &None::<u32>)?;
                row.serialize_field("new", new)?;
                row.serialize_field("text", text)?;
                row.end()
            }
            Self::Gap { skipped } => {
                let mut row = serializer.serialize_struct("DiffRow", 2)?;
                row.serialize_field("op", "gap")?;
                row.serialize_field("skipped", skipped)?;
                row.end()
            }
            Self::Note { text } => {
                let mut row = serializer.serialize_struct("DiffRow", 2)?;
                row.serialize_field("op", "note")?;
                row.serialize_field("text", text)?;
                row.end()
            }
        }
    }
}

impl fmt::Debug for DiffRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ctx { old, new, text } => {
                f.debug_struct("Ctx").field("old", old).field("new", new).field("text", &Size(text.len())).finish()
            }
            Self::Del { old, text } => f.debug_struct("Del").field("old", old).field("text", &Size(text.len())).finish(),
            Self::Add { new, text } => f.debug_struct("Add").field("new", new).field("text", &Size(text.len())).finish(),
            Self::Gap { skipped } => f.debug_struct("Gap").field("skipped", skipped).finish(),
            Self::Note { text } => f.debug_struct("Note").field("text", &Size(text.len())).finish(),
        }
    }
}

/// What the card shows of a change: its kind, the counts, the sizes and the rows.
#[derive(Clone, Serialize)]
pub struct Diff {
    /// The kind of the change.
    pub kind: DiffKind,
    /// How many lines were added.
    pub added: usize,
    /// How many lines were removed.
    pub removed: usize,
    /// The size of the old text in bytes; none for a file that does not exist.
    pub before_bytes: Option<usize>,
    /// The size of the new text in bytes.
    pub after_bytes: usize,
    /// The rows, in order.
    pub rows: Vec<DiffRow>,
}

impl fmt::Debug for Diff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Diff")
            .field("kind", &self.kind)
            .field("added", &self.added)
            .field("removed", &self.removed)
            .field("before_bytes", &self.before_bytes)
            .field("after_bytes", &self.after_bytes)
            .field("rows", &self.rows.len())
            .finish()
    }
}

/// The diff of `change` with [`DIFF_CONTEXT`] unchanged lines around each change.
pub fn diff(change: &Change) -> Result<Diff, ChangeFault> {
    diff_with(change, DIFF_CONTEXT)
}

/// The diff of `change` with `context` unchanged lines around each change.
///
/// Both texts are split into lines that keep their line break, a shortest edit script is made over what the two texts do
/// not share, and the script is checked against both texts before any row is made. A run of unchanged lines becomes one
/// gap row when that skips at least two lines. A change with more than [`MAX_DIFF_ROWS`] rows, more than
/// [`MAX_DIFF_CHARS`] characters of row text, or too many comparisons to find its script is
/// [`ChangeFault::NotShown`].
pub fn diff_with(change: &Change, context: usize) -> Result<Diff, ChangeFault> {
    let old_text = change.before.as_deref().unwrap_or("");
    let new_text = change.after.as_str();
    let old: Vec<&str> = old_text.split_inclusive('\n').collect();
    let new: Vec<&str> = new_text.split_inclusive('\n').collect();
    let ops = script(&old, &new)?;
    check_script(&ops, &old, &new, old_text, new_text)?;
    let rows = rows_of(&ops, &old, &new, context)?;
    let added = ops.iter().filter(|op| matches!(op, Op::Add(_))).count();
    let removed = ops.iter().filter(|op| matches!(op, Op::Del(_))).count();
    Ok(Diff {
        kind: change.kind,
        added,
        removed,
        before_bytes: change.before.as_ref().map(String::len),
        after_bytes: change.after.len(),
        rows,
    })
}

// ---- the edit script ----------------------------------------------------------------------------------------------

/// One step of the script, by the index of the line in its text.
#[derive(Clone, Copy)]
enum Op {
    /// The old line and the new line are the same.
    Keep(usize, usize),
    /// An old line goes.
    Del(usize),
    /// A new line comes.
    Add(usize),
}

/// The most half-steps the search walks before the edit distance is over what a card shows.
const SEARCH_DEPTH: usize = MAX_DIFF_ROWS / 2 + 2;

/// A shortest edit script from `old` to `new`: Myers' algorithm in linear space, over lines made comparable by number.
fn script(old: &[&str], new: &[&str]) -> Result<Vec<Op>, ChangeFault> {
    let mut ids: HashMap<&str, u32> = HashMap::new();
    let a = numbered(&mut ids, old);
    let b = numbered(&mut ids, new);
    let width = SEARCH_DEPTH * 2 + 3;
    let mut walk = Walk {
        a: &a,
        b: &b,
        ops: Vec::with_capacity(old.len().max(new.len())),
        work: 0,
        forward: vec![0; width],
        backward: vec![0; width],
        offset: isize::try_from(SEARCH_DEPTH).unwrap_or(isize::MAX) + 1,
    };
    walk.conquer(0, a.len(), 0, b.len())?;
    Ok(hunks_delete_first(walk.ops))
}

/// The lines as numbers, equal lines having equal numbers, so that a comparison is one integer test.
fn numbered<'a>(ids: &mut HashMap<&'a str, u32>, lines: &[&'a str]) -> Vec<u32> {
    lines
        .iter()
        .map(|line| {
            let next = u32::try_from(ids.len()).unwrap_or(u32::MAX);
            *ids.entry(line).or_insert(next)
        })
        .collect()
}

/// Within every run of changes between two unchanged lines the removed lines come before the added ones, so a replaced
/// line reads as the line that goes and then the line that comes. The two texts a script rebuilds do not depend on it.
fn hunks_delete_first(ops: Vec<Op>) -> Vec<Op> {
    let mut out = Vec::with_capacity(ops.len());
    let mut added: Vec<Op> = Vec::new();
    for op in ops {
        match op {
            Op::Add(_) => added.push(op),
            Op::Del(_) => out.push(op),
            Op::Keep(..) => {
                out.append(&mut added);
                out.push(op);
            }
        }
    }
    out.append(&mut added);
    out
}

struct Walk<'a> {
    a: &'a [u32],
    b: &'a [u32],
    ops: Vec<Op>,
    work: usize,
    forward: Vec<isize>,
    backward: Vec<isize>,
    offset: isize,
}

impl Walk<'_> {
    fn spend(&mut self, steps: usize) -> Result<(), ChangeFault> {
        self.work = self.work.saturating_add(steps);
        if self.work > WORK_LIMIT { Err(ChangeFault::NotShown) } else { Ok(()) }
    }

    /// The script for `a[a0..a1]` against `b[b0..b1]`, appended to `ops` in order.
    fn conquer(&mut self, mut a0: usize, a1: usize, mut b0: usize, b1: usize) -> Result<(), ChangeFault> {
        let mut steps = 0;
        while a0 < a1 && b0 < b1 && self.a[a0] == self.b[b0] {
            self.ops.push(Op::Keep(a0, b0));
            a0 += 1;
            b0 += 1;
            steps += 1;
        }
        let mut tail = 0;
        while a0 + tail < a1 && b0 + tail < b1 && self.a[a1 - 1 - tail] == self.b[b1 - 1 - tail] {
            tail += 1;
        }
        steps += tail;
        self.spend(steps)?;
        let (a1, b1) = (a1 - tail, b1 - tail);
        if a0 == a1 {
            self.ops.extend((b0..b1).map(Op::Add));
        } else if b0 == b1 {
            self.ops.extend((a0..a1).map(Op::Del));
        } else {
            match self.middle(a0, a1, b0, b1)? {
                Some((x, y)) if (x, y) != (a0, b0) && (x, y) != (a1, b1) => {
                    self.conquer(a0, x, b0, y)?;
                    self.conquer(x, a1, y, b1)?;
                }
                // A split that makes no progress would never end; the whole middle is replaced, which is still a script
                // that rebuilds both texts.
                _ => {
                    self.ops.extend((a0..a1).map(Op::Del));
                    self.ops.extend((b0..b1).map(Op::Add));
                }
            }
        }
        self.ops.extend((0..tail).map(|t| Op::Keep(a1 + t, b1 + t)));
        Ok(())
    }

    fn slot(&self, k: isize) -> usize {
        usize::try_from(self.offset + k).unwrap_or(0)
    }

    /// A point on a shortest path through the middle of `a[a0..a1]` against `b[b0..b1]`: the start of the snake in which
    /// the search from the front meets the search from the back. `Ok(None)` when no such point was found.
    fn middle(&mut self, a0: usize, a1: usize, b0: usize, b1: usize) -> Result<Option<(usize, usize)>, ChangeFault> {
        let n = isize::try_from(a1 - a0).unwrap_or(isize::MAX);
        let m = isize::try_from(b1 - b0).unwrap_or(isize::MAX);
        let delta = n - m;
        let odd = delta & 1 == 1;
        let reach = ((n + m + 1) / 2 + 1).min(isize::try_from(SEARCH_DEPTH).unwrap_or(isize::MAX));
        let at = self.slot(1);
        self.forward[at] = 0;
        self.backward[at] = 0;
        for d in 0..reach {
            let mut steps = 0;
            for k in (-d..=d).rev().step_by(2) {
                let (lo, hi) = (self.slot(k - 1), self.slot(k + 1));
                let mut x = if k == -d || (k != d && self.forward[lo] < self.forward[hi]) {
                    self.forward[hi]
                } else {
                    self.forward[lo] + 1
                };
                let mut y = x - k;
                let (x0, y0) = (x, y);
                while x >= 0 && y >= 0 && x < n && y < m {
                    let (i, j) = (a0 + x as usize, b0 + y as usize);
                    if self.a[i] != self.b[j] {
                        break;
                    }
                    x += 1;
                    y += 1;
                    steps += 1;
                }
                steps += 1;
                let here = self.slot(k);
                self.forward[here] = x;
                if odd && (k - delta).abs() <= d - 1 {
                    let other = self.slot(-(k - delta));
                    if x + self.backward[other] >= n {
                        self.spend(steps)?;
                        return Ok(point(a0, b0, n, m, x0, y0));
                    }
                }
            }
            for k in (-d..=d).rev().step_by(2) {
                let (lo, hi) = (self.slot(k - 1), self.slot(k + 1));
                let mut x = if k == -d || (k != d && self.backward[lo] < self.backward[hi]) {
                    self.backward[hi]
                } else {
                    self.backward[lo] + 1
                };
                let mut y = x - k;
                while x >= 0 && y >= 0 && x < n && y < m {
                    let (i, j) = (a1 - 1 - x as usize, b1 - 1 - y as usize);
                    if self.a[i] != self.b[j] {
                        break;
                    }
                    x += 1;
                    y += 1;
                    steps += 1;
                }
                steps += 1;
                let here = self.slot(k);
                self.backward[here] = x;
                if !odd && (k - delta).abs() <= d {
                    let other = self.slot(-(k - delta));
                    if x + self.forward[other] >= n {
                        self.spend(steps)?;
                        return Ok(point(a0, b0, n, m, n - x, m - y));
                    }
                }
            }
            self.spend(steps)?;
        }
        if reach < (n + m + 1) / 2 + 1 { Err(ChangeFault::NotShown) } else { Ok(None) }
    }
}

/// The point `(x, y)` of the middle snake as indexes into the whole texts, when it lies inside the part searched.
fn point(a0: usize, b0: usize, n: isize, m: isize, x: isize, y: isize) -> Option<(usize, usize)> {
    if x < 0 || y < 0 || x > n || y > m {
        return None;
    }
    Some((a0 + x as usize, b0 + y as usize))
}

/// Rebuilds both texts from the script and compares them with the ones it was made from.
fn check_script(ops: &[Op], old: &[&str], new: &[&str], old_text: &str, new_text: &str) -> Result<(), ChangeFault> {
    let mut rebuilt_old = String::with_capacity(old_text.len());
    let mut rebuilt_new = String::with_capacity(new_text.len());
    for op in ops {
        match *op {
            Op::Keep(i, j) => {
                let (Some(line), Some(other)) = (old.get(i), new.get(j)) else { return Err(ChangeFault::NotShown) };
                if line != other {
                    return Err(ChangeFault::NotShown);
                }
                rebuilt_old.push_str(line);
                rebuilt_new.push_str(line);
            }
            Op::Del(i) => rebuilt_old.push_str(old.get(i).ok_or(ChangeFault::NotShown)?),
            Op::Add(j) => rebuilt_new.push_str(new.get(j).ok_or(ChangeFault::NotShown)?),
        }
    }
    if rebuilt_old == old_text && rebuilt_new == new_text { Ok(()) } else { Err(ChangeFault::NotShown) }
}

// ---- the rows ---------------------------------------------------------------------------------------------------

/// The text of a line without its line break, with hidden characters written out.
fn shown(line: &str) -> String {
    visible(line.strip_suffix('\n').unwrap_or(line))
}

/// The number of a line as a row carries it. A text is at most `MAX_BASE_BYTES` long, so it has fewer lines than a `u32`.
fn number(index: usize) -> u32 {
    u32::try_from(index + 1).unwrap_or(u32::MAX)
}

/// The rows of a script: every changed line, and around each change `context` unchanged lines; longer unchanged runs
/// become one gap row.
fn rows_of(ops: &[Op], old: &[&str], new: &[&str], context: usize) -> Result<Vec<DiffRow>, ChangeFault> {
    let mut rows: Vec<DiffRow> = Vec::new();
    let mut units = 0usize;
    let mut push = |rows: &mut Vec<DiffRow>, row: DiffRow| -> Result<(), ChangeFault> {
        units = units.saturating_add(row.units());
        rows.push(row);
        if rows.len() > MAX_DIFF_ROWS || units > MAX_DIFF_CHARS { Err(ChangeFault::NotShown) } else { Ok(()) }
    };
    let both = context.saturating_mul(2).saturating_add(1);
    let one = context.saturating_add(1);
    let mut at = 0;
    while at < ops.len() {
        match ops[at] {
            Op::Keep(..) => {
                let start = at;
                while at < ops.len() && matches!(ops[at], Op::Keep(..)) {
                    at += 1;
                }
                let length = at - start;
                let before = start > 0;
                let after = at < ops.len();
                if !before && !after {
                    continue;
                }
                // The part of the run that is shown at its front and at its back.
                let (front, back, folds) = match (before, after) {
                    (true, true) => (context, context, length > both),
                    (true, false) => (context, 0, length > one),
                    _ => (0, context, length > one),
                };
                let (front, back) = if folds { (front, back) } else { (length, 0) };
                for offset in 0..front.min(length) {
                    push(&mut rows, keep_row(ops[start + offset], old))?;
                    note_after(&mut rows, ops[start + offset], old, new, &mut push)?;
                }
                if folds {
                    let skipped = length - front - back;
                    push(&mut rows, DiffRow::Gap { skipped: u32::try_from(skipped).unwrap_or(u32::MAX) })?;
                    for offset in length - back..length {
                        push(&mut rows, keep_row(ops[start + offset], old))?;
                        note_after(&mut rows, ops[start + offset], old, new, &mut push)?;
                    }
                }
            }
            op => {
                push(&mut rows, change_row(op, old, new))?;
                note_after(&mut rows, op, old, new, &mut push)?;
                at += 1;
            }
        }
    }
    Ok(rows)
}

fn keep_row(op: Op, old: &[&str]) -> DiffRow {
    match op {
        Op::Keep(i, j) => DiffRow::Ctx { old: number(i), new: number(j), text: shown(old.get(i).copied().unwrap_or("")) },
        Op::Del(_) | Op::Add(_) => DiffRow::Note { text: String::new() },
    }
}

fn change_row(op: Op, old: &[&str], new: &[&str]) -> DiffRow {
    match op {
        Op::Del(i) => DiffRow::Del { old: number(i), text: shown(old.get(i).copied().unwrap_or("")) },
        Op::Add(j) => DiffRow::Add { new: number(j), text: shown(new.get(j).copied().unwrap_or("")) },
        Op::Keep(..) => keep_row(op, old),
    }
}

/// Puts a note row after the row of `op` when its line has no line break, which only the last line of a text can lack.
fn note_after(
    rows: &mut Vec<DiffRow>,
    op: Op,
    old: &[&str],
    new: &[&str],
    push: &mut impl FnMut(&mut Vec<DiffRow>, DiffRow) -> Result<(), ChangeFault>,
) -> Result<(), ChangeFault> {
    let line = match op {
        Op::Keep(i, _) | Op::Del(i) => old.get(i),
        Op::Add(j) => new.get(j),
    };
    if line.is_some_and(|line| !line.ends_with('\n')) {
        push(rows, DiffRow::Note { text: NO_NEWLINE.to_owned() })?;
    }
    Ok(())
}
