//! The agent's tools: typed definitions, the approval state machine, the taint rule and the deny list.
//!
//! All of it is headless. The shell implements [`ToolHost`] and gives the agent loop an [`ApprovalTable`]; nothing here
//! opens a file, starts a process, touches a terminal or reaches the network, so every guarantee below is a unit that a
//! test can break.
//!
//! - **No tool runs without a decision.** [`ToolHost::run`] takes a [`Ticket`], which only the approval table can
//!   issue, and only for a call that was approved, has not run, and still resolves to the payload the user saw.
//! - **What the card shows is what runs.** The fields of the card are the list the payload hash is computed over; the
//!   decision has to repeat that hash, and the table resolves and hashes the call again right before it runs.
//! - **No approval is ever remembered.** There is no setting, grant or "always"; every call needs its own decision,
//!   and an approval of a write or a command does not survive new untrusted content in the same turn.
//! - **The agent cannot rewrite its own gate.** A deny list keeps the file tools away from the user's secrets, the
//!   shell's own configuration and the directories the shell protects, with symbolic links resolved first.

mod approval;
mod catalog;
mod deny;

use std::error::Error;
use std::fmt;

use crate::layout::PaneId;

use super::provider::{CancelToken, Detail};
use super::types::{Size, ToolResult, ToolSpec};

pub use approval::{ApprovalTable, Decision, PayloadField, Pending, TaintSource, Ticket, denied_result};
pub use catalog::{
    EditFile, Effect, Field, FieldKind, Grep, HASH_PREFIX, ListDir, Newlines, ReadFile, ReadPane, RunCommand, Tool,
    ToolName, WriteFile, fields_of, payload_hash, visible, visible_multiline,
};
pub use deny::{Access, Denied, DenyPolicy, FsKind, Style, canonical_local};

/// The most bytes a tool result carries back to the model.
pub const MAX_RESULT_BYTES: usize = 16 * 1024;
/// The longest path a tool accepts.
pub const MAX_PATH_BYTES: usize = crate::files::MAX_PATH;
/// The longest search text.
pub const MAX_PATTERN_BYTES: usize = 512;
/// The longest command line.
pub const MAX_COMMAND_BYTES: usize = 8192;
/// The most bytes one `write_file` call writes.
pub const MAX_WRITE_BYTES: usize = 128 * 1024;
/// The most bytes of either side of one `edit_file` call.
pub const MAX_EDIT_BYTES: usize = 32 * 1024;
/// The longest call id.
pub const MAX_CALL_ID_BYTES: usize = 128;
/// The most calls open at the same time.
pub const MAX_OPEN_CALLS: usize = 16;
/// The most turns that hold a taint ledger or a call at the same time.
pub const MAX_TURNS: usize = 64;
/// The most labels one turn's taint ledger lists.
pub const MAX_TAINT_LABELS: usize = 8;
/// How long a command may run when the call names no time, in seconds.
pub const DEFAULT_COMMAND_SECS: u64 = 60;
/// The longest time a command may be given, in seconds.
pub const MAX_COMMAND_SECS: u64 = 600;

/// Where a call happens: the conversation, the turn, and the pane a tool acts for by default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    /// The conversation that owns the call.
    pub conversation: String,
    /// The turn inside the conversation.
    pub turn: u64,
    /// The pane a tool acts for unless the call names another.
    pub pane: PaneId,
}

/// What the host made of a call: the pane, the machine, and the paths exactly as it will use them.
///
/// The host builds it twice for one call, once when the call is proposed and once right before it runs, so building
/// one must be cheap and free of side effects. For a local target the table replaces the path with the canonical one
/// before it hashes, and the host acts on that one.
#[derive(Clone, PartialEq, Eq)]
pub struct Resolved {
    pane: PaneId,
    target: String,
    fs: FsKind,
    path: Option<String>,
    cwd: Option<String>,
    stamp: Option<i64>,
}

impl Resolved {
    /// A call that acts on this computer, for `pane`; `target_label` is what the card calls the machine.
    pub fn local(pane: PaneId, target_label: impl Into<String>) -> Self {
        Self::new(pane, target_label.into(), FsKind::Local)
    }

    /// A call that acts on another machine, for `pane`.
    pub fn remote(pane: PaneId, target_label: impl Into<String>) -> Self {
        Self::new(pane, target_label.into(), FsKind::Remote)
    }

    fn new(pane: PaneId, target: String, fs: FsKind) -> Self {
        Self { pane, target, fs, path: None, cwd: None, stamp: None }
    }

    /// Sets the absolute path the call acts on.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Sets the absolute working directory of a command.
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Sets the stamp of the file a write is based on (its modification time, in the unit the host uses).
    #[must_use]
    pub fn with_stamp(mut self, stamp: i64) -> Self {
        self.stamp = Some(stamp);
        self
    }

    /// The pane the call acts for.
    pub fn pane(&self) -> PaneId {
        self.pane
    }

    /// What the card calls the machine.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Whether the call acts on this computer or on another one.
    pub fn fs(&self) -> FsKind {
        self.fs
    }

    /// The path the call acts on.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// The working directory of a command.
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// The stamp of the file a write is based on.
    pub fn stamp(&self) -> Option<i64> {
        self.stamp
    }
}

impl fmt::Debug for Resolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolved")
            .field("pane", &self.pane)
            .field("target", &self.target)
            .field("fs", &self.fs)
            .field("path", &self.path.as_ref().map(|p| Size(p.len())))
            .field("cwd", &self.cwd.as_ref().map(|c| Size(c.len())))
            .field("stamp", &self.stamp.is_some())
            .finish()
    }
}

/// What a tool produced. Text longer than [`MAX_RESULT_BYTES`] keeps its head and its tail.
#[derive(Clone, PartialEq, Eq)]
pub struct ToolOutput {
    text: String,
    omitted: usize,
}

/// The room the marker line takes: two line breaks, the words, and a number of up to twenty digits.
const MARKER_ROOM: usize = 48;

impl ToolOutput {
    /// Wraps `text`, cutting it to head and tail with one marker line when it is longer than [`MAX_RESULT_BYTES`].
    ///
    /// Both cuts fall on character boundaries, so the result is valid text and never longer than the limit.
    pub fn new(text: String) -> Self {
        if text.len() <= MAX_RESULT_BYTES {
            return Self { text, omitted: 0 };
        }
        let side = (MAX_RESULT_BYTES - MARKER_ROOM) / 2;
        let head_end = floor_boundary(&text, side);
        let tail_start = ceil_boundary(&text, text.len() - side);
        let omitted = tail_start - head_end;
        let mut cut = String::with_capacity(MAX_RESULT_BYTES);
        cut.push_str(&text[..head_end]);
        cut.push_str(&format!("\n[... {omitted} bytes omitted ...]\n"));
        cut.push_str(&text[tail_start..]);
        Self { text: cut, omitted }
    }

    /// The text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The text, by value.
    pub fn into_text(self) -> String {
        self.text
    }

    /// Whether the text was cut.
    pub fn truncated(&self) -> bool {
        self.omitted > 0
    }

    /// How many bytes the cut left out.
    pub fn omitted(&self) -> usize {
        self.omitted
    }
}

/// The largest index not above `at` that starts a character.
fn floor_boundary(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The smallest index not below `at` that starts a character.
fn ceil_boundary(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

impl fmt::Debug for ToolOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolOutput").field("text", &Size(self.text.len())).field("omitted", &self.omitted).finish()
    }
}

/// Why a call did not run, or did not run as asked.
///
/// `Display` is one fixed sentence per variant, plus a scrubbed detail for the three variants that hold one. It never
/// echoes an argument, a path or the name of a rule, because the text goes back to the model and to the log. `Debug`
/// prints the stable code and nothing else.
#[derive(Clone, PartialEq, Eq)]
pub enum ToolError {
    /// The model named a tool that does not exist.
    UnknownTool(Detail),
    /// The arguments do not fit the tool.
    InvalidArguments(Detail),
    /// The host does not offer this tool for this pane.
    Unavailable,
    /// The location is protected; the field holds the name of the rule.
    Denied(&'static str),
    /// Too many calls or turns are open.
    TooMany,
    /// A call with this id is already open.
    DuplicateCall,
    /// No open call has this id.
    UnknownCall,
    /// The call has been decided already.
    NotPending,
    /// The decision does not carry the hash the card showed.
    HashMismatch,
    /// The user has not approved the call.
    NotApproved,
    /// The approval was used already.
    AlreadyUsed,
    /// The target changed after the approval.
    PayloadChanged,
    /// The call belongs to another turn or pane.
    ScopeMismatch,
    /// New untrusted content arrived after the approval was asked for.
    TaintChanged,
    /// The turn was stopped.
    Cancelled,
    /// The host failed to do what was approved.
    Host(Detail),
}

impl ToolError {
    /// The stable code of the variant.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownTool(_) => "unknown_tool",
            Self::InvalidArguments(_) => "invalid_arguments",
            Self::Unavailable => "unavailable",
            Self::Denied(_) => "denied",
            Self::TooMany => "too_many",
            Self::DuplicateCall => "duplicate_call",
            Self::UnknownCall => "unknown_call",
            Self::NotPending => "not_pending",
            Self::HashMismatch => "hash_mismatch",
            Self::NotApproved => "not_approved",
            Self::AlreadyUsed => "already_used",
            Self::PayloadChanged => "payload_changed",
            Self::ScopeMismatch => "scope_mismatch",
            Self::TaintChanged => "taint_changed",
            Self::Cancelled => "cancelled",
            Self::Host(_) => "host",
        }
    }

    /// The result the model receives for the call `call_id`.
    pub fn to_result(&self, call_id: &str) -> ToolResult {
        ToolResult { call_id: call_id.to_owned(), content: self.to_string(), is_error: true }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTool(d) => write!(f, "There is no such tool: {}", d.as_str()),
            Self::InvalidArguments(d) => write!(f, "The arguments of the call are not valid: {}", d.as_str()),
            Self::Unavailable => f.write_str("This tool is not available here."),
            Self::Denied(_) => f.write_str("This location is protected and tools cannot use it."),
            Self::TooMany => f.write_str("Too many calls are open."),
            Self::DuplicateCall => f.write_str("A call with this id is already open."),
            Self::UnknownCall => f.write_str("No open call has this id."),
            Self::NotPending => f.write_str("The call was decided already."),
            Self::HashMismatch => f.write_str("The decision does not match the action it was asked for."),
            Self::NotApproved => f.write_str("The user has not approved this action."),
            Self::AlreadyUsed => f.write_str("This approval was used already."),
            Self::PayloadChanged => f.write_str("The target changed after the approval, so the action was not run."),
            Self::ScopeMismatch => f.write_str("The call belongs to another turn or pane."),
            Self::TaintChanged => {
                f.write_str("New untrusted content arrived after the approval was asked for, so the action was not run.")
            }
            Self::Cancelled => f.write_str("The turn was stopped."),
            Self::Host(d) => write!(f, "The action failed: {}", d.as_str()),
        }
    }
}

impl fmt::Debug for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl Error for ToolError {}

/// The seam between the approval table and the shell.
///
/// The shell implements it once; core never reads a file, starts a process or touches a terminal itself.
///
/// - `available` says whether the host offers `tool` for `pane`; only those tools are declared to the model.
/// - `resolve` names the pane, the machine, the absolute path, the working directory and, for a write, the stamp of the
///   file. It is called twice for one call (when it is proposed and right before it runs), so it must be cheap and free
///   of side effects. A local path may be given unresolved: the table resolves it and hands the host the canonical one.
/// - `run` receives the only value that proves an approval and must act on `ticket.resolved()`, never on the string the
///   model wrote. It must honour `cancel` and return [`ToolError::Cancelled`] when it is stopped. Nothing the trait
///   does may be typed into a terminal.
pub trait ToolHost: Send + Sync {
    /// Whether the host offers `tool` for `pane`.
    fn available(&self, tool: ToolName, pane: PaneId) -> bool;

    /// What `tool` would act on in `scope`.
    fn resolve(&self, scope: &Scope, tool: &Tool) -> Result<Resolved, ToolError>;

    /// Does what the user approved.
    fn run(&self, ticket: &Ticket, cancel: &CancelToken) -> Result<ToolOutput, ToolError>;
}

/// The tools the host offers for `pane`, as they are declared to a model, in the order of [`ToolName::ALL`].
///
/// A tool the host does not report as available is not declared, so the model cannot ask for it.
pub fn specs(host: &dyn ToolHost, pane: PaneId) -> Vec<ToolSpec> {
    ToolName::ALL.into_iter().filter(|tool| host.available(*tool, pane)).map(ToolName::spec).collect()
}
