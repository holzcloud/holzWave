//! The approval state machine: a call is proposed, shown as a card, decided by the user with the hash of the card, and
//! run once, after the table resolved it and hashed it again.
//!
//! The only way an approval comes into being is [`ApprovalTable::decide`], and the only way a host receives the right to
//! act is a [`Ticket`], which this module alone can issue.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::layout::PaneId;

use super::super::provider::{CancelToken, Detail};
use super::super::types::{Size, ToolCall, ToolResult};
use super::catalog::{Item, hash_items, hex, payload, visible, visible_multiline};
use super::deny::{Access, DenyPolicy, FsKind, Style};
use super::{
    Effect, MAX_CALL_ID_BYTES, Newlines, Resolved, Scope, Tool, ToolError, ToolHost, ToolName, ToolOutput,
};

/// How often a waiting turn looks at its cancel token.
const POLL: Duration = Duration::from_millis(25);

/// What the user decided. There is no third value: an approval is never remembered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    /// Run this call, once.
    Allow,
    /// Do not run it.
    Deny,
}

/// One line of a card: the name of a field, its value as the card shows it, and the length of the raw value.
#[derive(Clone, Serialize)]
pub struct PayloadField {
    /// The name of the field.
    pub name: &'static str,
    /// The value with every hidden character spelled out.
    pub value: String,
    /// The length of the raw value in bytes.
    pub bytes: usize,
}

impl fmt::Debug for PayloadField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PayloadField").field("name", &self.name).field("value", &Size(self.value.len())).finish()
    }
}

/// What the approval card shows for a call that waits for a decision.
///
/// The payload is the list the hash is computed over. The decision has to carry `hash`.
#[derive(Clone, Serialize)]
pub struct Pending {
    /// The conversation that owns the call.
    pub conversation: String,
    /// The id of the call.
    pub call_id: String,
    /// The tool.
    pub tool: ToolName,
    /// What the tool does to the world.
    pub effect: Effect,
    /// The pane the call acts for.
    pub pane: PaneId,
    /// The machine, as the card shows it.
    pub target: String,
    /// Whether the machine is not this computer.
    pub remote: bool,
    /// Every field the decision covers.
    pub payload: Vec<PayloadField>,
    /// The hash of the payload: 64 lower-case hex digits.
    pub hash: String,
    /// Whether untrusted content was already read in this turn.
    pub tainted: bool,
    /// What that content was.
    pub taint: Vec<String>,
}

impl fmt::Debug for Pending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pending")
            .field("conversation", &self.conversation)
            .field("call_id", &self.call_id)
            .field("tool", &self.tool)
            .field("effect", &self.effect)
            .field("pane", &self.pane)
            .field("target", &Size(self.target.len()))
            .field("remote", &self.remote)
            .field("payload", &self.payload)
            .field("hash", &self.hash)
            .field("tainted", &self.tainted)
            .field("taint", &self.taint.len())
            .finish()
    }
}

/// The proof that a call was approved and still is what the user saw.
///
/// A host receives one in [`ToolHost::run`] and acts on [`Ticket::resolved`]. It cannot be made anywhere else: the
/// fields are private, the one constructor is private to this module, and it has no `Default`, `Clone`, `Serialize` or
/// `Deserialize`.
///
/// A ticket can be received and read:
///
/// ```
/// use holzwave_core::ai::tools::{Resolved, Scope, Ticket, Tool};
///
/// fn describe(ticket: &Ticket) -> String {
///     format!("{} {}", ticket.call_id(), ticket.hash_hex())
/// }
/// ```
///
/// It cannot be written as a struct literal:
///
/// ```compile_fail
/// use holzwave_core::ai::tools::{Resolved, Scope, Ticket, Tool};
///
/// fn forge() -> Ticket {
///     Ticket { call_id: String::new() }
/// }
/// ```
///
/// It cannot be issued by calling its constructor:
///
/// ```compile_fail
/// use holzwave_core::ai::tools::{Resolved, Scope, Ticket, Tool};
///
/// fn forge(scope: Scope, tool: Tool, resolved: Resolved) -> Ticket {
///     Ticket::issue(scope, tool, resolved, [0; 32], String::new())
/// }
/// ```
///
/// It has no default:
///
/// ```compile_fail
/// use holzwave_core::ai::tools::{Resolved, Scope, Ticket, Tool};
///
/// fn forge() -> Ticket {
///     Ticket::default()
/// }
/// ```
///
/// It cannot be copied:
///
/// ```compile_fail
/// use holzwave_core::ai::tools::{Resolved, Scope, Ticket, Tool};
///
/// fn copy(ticket: &Ticket) -> Ticket {
///     ticket.clone()
/// }
/// ```
///
/// It cannot be read from data:
///
/// ```compile_fail
/// use holzwave_core::ai::tools::{Resolved, Scope, Ticket, Tool};
///
/// fn forge(json: &str) -> Ticket {
///     serde_json::from_str::<Ticket>(json).unwrap()
/// }
/// ```
///
/// The examples above share the import of the one that compiles, so a typo cannot make a failing one pass.
pub struct Ticket {
    scope: Scope,
    tool: Tool,
    resolved: Resolved,
    hash: [u8; 32],
    call_id: String,
}

impl Ticket {
    fn issue(scope: Scope, tool: Tool, resolved: Resolved, hash: [u8; 32], call_id: String) -> Self {
        Self { scope, tool, resolved, hash, call_id }
    }

    /// The approved call, with its arguments.
    pub fn tool(&self) -> &Tool {
        &self.tool
    }

    /// What the call acts on, as the table resolved it. The host acts on this, never on the model's string.
    pub fn resolved(&self) -> &Resolved {
        &self.resolved
    }

    /// Where the call happens.
    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// The id of the call.
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// The hash the user approved, in lower-case hex.
    pub fn hash_hex(&self) -> String {
        hex(&self.hash)
    }
}

impl fmt::Debug for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ticket")
            .field("call_id", &self.call_id)
            .field("tool", &self.tool)
            .field("resolved", &self.resolved)
            .field("hash", &self.hash_hex())
            .finish()
    }
}

/// The result the model receives for a call the user denied.
pub fn denied_result(call_id: &str) -> ToolResult {
    ToolResult { call_id: call_id.to_owned(), content: "The user denied this action.".to_owned(), is_error: true }
}

// `Voided` is set when a turn, a pane or a conversation is cancelled and when new untrusted content voids an approval.
#[allow(dead_code)]
enum State {
    Awaiting,
    Approved,
    Denied,
    Running,
    Voided(ToolError),
}

struct Entry {
    seq: u64,
    scope: Scope,
    tool: Tool,
    hash: [u8; 32],
    hash_hex: String,
    pending: Pending,
    state: State,
}

#[derive(Default)]
struct Inner {
    entries: BTreeMap<(String, String), Entry>,
    next_seq: u64,
}

/// The open calls of the agent and the decisions on them.
///
/// There is no remembered approval of any kind, on purpose: every call needs its own decision, because a remembered
/// approval is what an injected instruction rides on.
pub struct ApprovalTable {
    policy: DenyPolicy,
    inner: Mutex<Inner>,
    wake: Condvar,
}

fn key(conversation: &str, call_id: &str) -> (String, String) {
    (conversation.to_owned(), call_id.to_owned())
}

impl ApprovalTable {
    /// An empty table that refuses what `policy` denies.
    pub fn new(policy: DenyPolicy) -> Self {
        Self { policy, inner: Mutex::new(Inner::default()), wake: Condvar::new() }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn remove(&self, key: &(String, String)) {
        self.lock().entries.remove(key);
    }

    /// Checks the call, asks the host what it would act on, and opens it for a decision.
    ///
    /// Nothing is opened for a call that names a protected location: it fails here, so no card exists for it.
    pub fn propose(&self, host: &dyn ToolHost, scope: &Scope, call: &ToolCall) -> Result<Pending, ToolError> {
        check_call_id(&call.id)?;
        let tool = Tool::parse(&call.name, &call.args)?;
        if !host.available(tool.name(), tool.pane().unwrap_or(scope.pane)) {
            return Err(ToolError::Unavailable);
        }
        let (resolved, items, hash) = self.resolve_checked(host, scope, &tool)?;
        let pending = Pending {
            conversation: scope.conversation.clone(),
            call_id: call.id.clone(),
            tool: tool.name(),
            effect: tool.effect(),
            pane: resolved.pane(),
            target: visible(resolved.target()),
            remote: resolved.fs() == FsKind::Remote,
            payload: items.iter().map(card_field).collect(),
            hash: hex(&hash),
            tainted: false,
            taint: Vec::new(),
        };
        let mut inner = self.lock();
        let key = key(&scope.conversation, &call.id);
        if inner.entries.contains_key(&key) {
            return Err(ToolError::DuplicateCall);
        }
        let seq = inner.next_seq;
        inner.next_seq += 1;
        let entry = Entry {
            seq,
            scope: scope.clone(),
            tool,
            hash,
            hash_hex: pending.hash.clone(),
            pending: pending.clone(),
            state: State::Awaiting,
        };
        inner.entries.insert(key, entry);
        Ok(pending)
    }

    /// Records the user's decision on a call. It never blocks.
    ///
    /// `hash` must be the one the card showed. This is the only way an approval comes into being; nothing else in this
    /// crate can approve a call.
    pub fn decide(&self, conversation: &str, call_id: &str, decision: Decision, hash: &str) -> Result<(), ToolError> {
        let mut inner = self.lock();
        let entry = inner.entries.get_mut(&key(conversation, call_id)).ok_or(ToolError::UnknownCall)?;
        if !matches!(entry.state, State::Awaiting) {
            return Err(ToolError::NotPending);
        }
        if entry.hash_hex != hash {
            return Err(ToolError::HashMismatch);
        }
        entry.state = match decision {
            Decision::Allow => State::Approved,
            Decision::Deny => State::Denied,
        };
        drop(inner);
        self.wake.notify_all();
        Ok(())
    }

    /// Blocks the calling turn until the call is decided, voided or `cancel` is raised.
    ///
    /// It looks at the token every 25 ms. A denied call is removed and answers [`Decision::Deny`]; an approved call
    /// stays for [`ApprovalTable::run`].
    pub fn wait(&self, conversation: &str, call_id: &str, cancel: &CancelToken) -> Result<Decision, ToolError> {
        let key = key(conversation, call_id);
        let mut inner = self.lock();
        loop {
            if cancel.is_cancelled() {
                if inner.entries.get(&key).is_some_and(|e| !matches!(e.state, State::Running)) {
                    inner.entries.remove(&key);
                }
                return Err(ToolError::Cancelled);
            }
            let Some(entry) = inner.entries.get(&key) else {
                return Err(ToolError::UnknownCall);
            };
            match &entry.state {
                State::Approved => return Ok(Decision::Allow),
                State::Denied => {
                    inner.entries.remove(&key);
                    return Ok(Decision::Deny);
                }
                State::Voided(error) => {
                    let error = error.clone();
                    inner.entries.remove(&key);
                    return Err(error);
                }
                State::Running => return Err(ToolError::AlreadyUsed),
                State::Awaiting => {}
            }
            inner = self.wake.wait_timeout(inner, POLL).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Runs an approved call, once.
    ///
    /// The host is asked again what the call acts on, the payload is hashed again, and the call runs only when that
    /// hash is the one the user approved. A call that is not approved, was decided against, has run, belongs to another
    /// turn or pane, or whose target changed never reaches the host.
    pub fn run(
        &self,
        host: &dyn ToolHost,
        scope: &Scope,
        call_id: &str,
        cancel: &CancelToken,
    ) -> Result<ToolOutput, ToolError> {
        let key = key(&scope.conversation, call_id);
        let (tool, approved) = {
            let mut inner = self.lock();
            let entry = inner.entries.get(&key).ok_or(ToolError::UnknownCall)?;
            match claim_check(entry, scope) {
                Ok(()) => (entry.tool.clone(), entry.hash),
                Err(error) => {
                    if matches!(entry.state, State::Voided(_)) {
                        inner.entries.remove(&key);
                    }
                    return Err(error);
                }
            }
        };
        if cancel.is_cancelled() {
            self.remove(&key);
            return Err(ToolError::Cancelled);
        }
        let (resolved, _, hash) = match self.resolve_checked(host, scope, &tool) {
            Ok(checked) => checked,
            Err(error) => {
                self.remove(&key);
                return Err(error);
            }
        };
        if hash != approved {
            self.remove(&key);
            return Err(ToolError::PayloadChanged);
        }
        {
            let mut inner = self.lock();
            let entry = inner.entries.get_mut(&key).ok_or(ToolError::UnknownCall)?;
            match claim_check(entry, scope) {
                Ok(()) => entry.state = State::Running,
                Err(error) => {
                    if matches!(entry.state, State::Voided(_)) {
                        inner.entries.remove(&key);
                    }
                    return Err(error);
                }
            }
        }
        let ticket = Ticket::issue(scope.clone(), tool, resolved, approved, call_id.to_owned());
        let result = host.run(&ticket, cancel);
        self.remove(&key);
        result
    }

    /// The calls of `conversation` that still wait for a decision, in the order they were proposed.
    pub fn pending(&self, conversation: &str) -> Vec<Pending> {
        let inner = self.lock();
        let mut open: Vec<&Entry> = inner
            .entries
            .iter()
            .filter(|((owner, _), entry)| owner == conversation && matches!(entry.state, State::Awaiting))
            .map(|(_, entry)| entry)
            .collect();
        open.sort_by_key(|entry| entry.seq);
        open.into_iter().map(|entry| entry.pending.clone()).collect()
    }

    /// Asks the host what the call acts on, applies the deny list, and hashes the payload.
    fn resolve_checked(
        &self,
        host: &dyn ToolHost,
        scope: &Scope,
        tool: &Tool,
    ) -> Result<(Resolved, Vec<Item>, [u8; 32]), ToolError> {
        let resolved = host.resolve(scope, tool)?;
        let resolved = self.guard(tool, resolved)?;
        let items = payload(tool, &resolved);
        let hash = hash_items(&items);
        Ok((resolved, items, hash))
    }

    /// Applies the deny list to the model's spelling, to what the host resolved, and to the working directory. A local
    /// path is replaced by the path the file system resolved it to.
    fn guard(&self, tool: &Tool, resolved: Resolved) -> Result<Resolved, ToolError> {
        let access = tool.access().unwrap_or(Access::Read);
        let fs = resolved.fs();
        if let Some(path) = tool.path() {
            self.spelling(fs, path, access)?;
            if resolved.path().is_none() {
                return Err(ToolError::Host(Detail::new("the host did not resolve the path")));
            }
        }
        if let Some(cwd) = tool.cwd() {
            self.spelling(fs, cwd, Access::Read)?;
        }
        let mut resolved = resolved;
        if let Some(path) = resolved.path().map(str::to_owned) {
            resolved = match self.resolve_one(fs, &path, access)? {
                Some(canonical) => resolved.with_path(canonical),
                None => resolved,
            };
        }
        if let Some(cwd) = resolved.cwd().map(str::to_owned) {
            resolved = match self.resolve_one(fs, &cwd, Access::Read)? {
                Some(canonical) => resolved.with_cwd(canonical),
                None => resolved,
            };
        }
        Ok(resolved)
    }

    /// Checks the spelling of a path the model wrote. A remote machine's style is not known, so both are tried.
    fn spelling(&self, fs: FsKind, path: &str, access: Access) -> Result<(), ToolError> {
        let native = [Style::native()];
        let both = [Style::Posix, Style::Windows];
        let styles: &[Style] = if fs == FsKind::Local { &native } else { &both };
        for style in styles {
            self.policy.check(*style, fs, path, access).map_err(|denied| ToolError::Denied(denied.rule()))?;
        }
        Ok(())
    }

    /// Checks a path the host resolved. On this computer it returns the canonical path to use instead.
    fn resolve_one(&self, fs: FsKind, path: &str, access: Access) -> Result<Option<String>, ToolError> {
        match fs {
            FsKind::Remote => {
                self.spelling(fs, path, access)?;
                Ok(None)
            }
            FsKind::Local => {
                let canonical =
                    self.policy.check_local(Path::new(path), access).map_err(|denied| ToolError::Denied(denied.rule()))?;
                let text = canonical.to_str().ok_or(ToolError::Denied("unresolvable_path"))?;
                Ok(Some(text.to_owned()))
            }
        }
    }
}

impl fmt::Debug for ApprovalTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApprovalTable").field("open", &self.lock().entries.len()).finish()
    }
}

/// Whether the entry may run for `scope` now.
fn claim_check(entry: &Entry, scope: &Scope) -> Result<(), ToolError> {
    if entry.scope.turn != scope.turn || entry.scope.pane != scope.pane {
        return Err(ToolError::ScopeMismatch);
    }
    match &entry.state {
        State::Approved => Ok(()),
        State::Awaiting | State::Denied => Err(ToolError::NotApproved),
        State::Running => Err(ToolError::AlreadyUsed),
        State::Voided(error) => Err(error.clone()),
    }
}

fn card_field(item: &Item) -> PayloadField {
    let value = match item.newlines {
        Newlines::Keep => visible_multiline(&item.value),
        Newlines::Reject | Newlines::Escape => visible(&item.value),
    };
    PayloadField { name: item.name, value, bytes: item.value.len() }
}

fn check_call_id(id: &str) -> Result<(), ToolError> {
    let invalid = |text: &str| ToolError::InvalidArguments(Detail::new(text));
    if id.is_empty() {
        return Err(invalid("the call id is empty"));
    }
    if id.len() > MAX_CALL_ID_BYTES {
        return Err(invalid("the call id is too long"));
    }
    if id.chars().any(char::is_control) {
        return Err(invalid("the call id contains a control character"));
    }
    Ok(())
}
