//! Prepare and confirm: the card for a verb that changes something, the ticket behind it, and the approval that only a
//! confirm of that ticket can produce.
//!
//! The shape is the one of the AI chat's approval table on purpose: a card shows the exact request, a hash binds the
//! card to the request, only the window decides, the core resolves and hashes the request again when the answer
//! arrives, and what runs happens once. Nothing here starts a process: [`Approved`] is a token that the service turns
//! into a run.
//!
//! The fields of a card, in this fixed order, are what the hash covers: `tool`, `verb`, `effect`, `program`, `arg 1` to
//! `arg n`, the target pairs in the order the resolver gives them (a context, a namespace, a subscription, a section, a
//! node, and for a local verb `changes`, the sentence that names the file it changes), `guard` (`production` or
//! `none`) and, only for a production context, `production_name`. Every text is spelled with
//! [`crate::ai::tools::visible`], so a line break, a direction mark or a terminal escape in a name is shown as its
//! escape and never acts; the hash is [`crate::ai::tools::payload_hash`] over the fields as shown, hex encoded.
//!
//! Limits: a card waits [`TICKET_LIFETIME`] (120 s), [`MAX_ATTEMPTS`] (3) wrong hashes drop it, [`MAX_PENDING`] (8)
//! cards wait, and a card holds at most [`MAX_CARD_BYTES`] (8 KiB) of field text. Nothing is approved by the absence of
//! an answer, nothing is remembered, and the table is in memory only.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::ai::tools::{hex, payload_hash, visible};
use crate::layout::PaneId;

use super::{ToolId, Verb, VerbEffect};

/// How long a card waits for its answer.
pub const TICKET_LIFETIME: Duration = Duration::from_secs(120);
/// How many wrong hashes drop a ticket.
pub const MAX_ATTEMPTS: usize = 3;
/// How many tickets wait at most; a ninth drops the oldest.
pub const MAX_PENDING: usize = 8;

/// The most bytes of field text (names and values, as shown) one card holds.
pub const MAX_CARD_BYTES: usize = 8 * 1024;

const HASH_LEN: usize = 64;
const MAX_VERB_BYTES: usize = 64;
const MAX_REQUEST_ARGS: usize = 16;
const MAX_KEY_BYTES: usize = 64;
const MAX_VALUE_BYTES: usize = 4096;

/// What the table asks the service for: the request, resolved to what would run.
///
/// The service implements it over the person's selection and the verb catalogue; the tests implement it with a toy.
/// The table calls it twice for one ticket, at prepare and at confirm, and refuses when the answers differ.
pub trait Resolve {
    /// Resolves the request to the argument list and the facts the card shows.
    fn resolve(
        &self,
        tool: ToolId,
        verb: &str,
        pane: Option<PaneId>,
        args: &BTreeMap<String, String>,
    ) -> Result<Resolution, TicketError>;
}

/// A request resolved to what would run.
#[derive(Debug, Clone)]
pub struct Resolution {
    /// The verb of the closed catalogue.
    pub verb: &'static Verb,
    /// What the runner will pass after the program.
    pub argv: Vec<OsString>,
    /// The binary's file name only, for the card.
    pub program: String,
    /// Labelled facts: the context, the namespace, the subscription, the section, the node and, for a local verb, what
    /// it changes.
    pub target: Vec<(String, String)>,
    /// The name of the production context, when the resolved context is one.
    pub production: Option<String>,
}

/// The guard line of a card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Guard {
    /// The name of the production context, or `None`.
    pub production: Option<String>,
}

/// Why a ticket was not made or not approved. Every refusal is a fixed sentence that holds none of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketError {
    /// The verb only reads, so it needs no ticket.
    NotGated,
    /// No ticket by this id: it never existed, was used, was dropped or was cancelled.
    Unknown,
    /// The ticket waited too long.
    Expired,
    /// The hash is not the card's.
    Mismatch,
    /// The request resolves to something else than the card showed.
    Changed,
    /// The request cannot be made into a card.
    Refused,
}

impl fmt::Display for TicketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotGated => "this verb only reads, so it needs no approval",
            Self::Unknown => "there is no such approval request",
            Self::Expired => "the approval request expired",
            Self::Mismatch => "the answer is not the one of this card",
            Self::Changed => "the request is not the one the card showed",
            Self::Refused => "the request cannot be shown as a card",
        })
    }
}

impl std::error::Error for TicketError {}

/// The card the person reads before a verb that changes something runs.
#[derive(Debug, Clone, Serialize)]
pub struct Card {
    /// The ticket id: 32 lower-case hex digits.
    pub ticket: String,
    /// The tool.
    pub tool: ToolId,
    /// The verb's id.
    pub verb: String,
    /// What the verb does to the world.
    pub effect: VerbEffect,
    /// The binary's file name.
    pub program: String,
    /// Every argument, one element each.
    pub argv: Vec<String>,
    /// The labelled facts of the target.
    pub target: Vec<(String, String)>,
    /// The guard line.
    pub guard: Guard,
    /// Every field the hash covers, in order.
    pub fields: Vec<(String, String)>,
    /// The hash of the fields: 64 lower-case hex digits.
    pub hash: String,
    /// Seconds until the ticket expires.
    pub expires_in_secs: u64,
}

/// The proof that a card was confirmed and still is what the person saw. Only [`TicketTable::confirm`] makes one: the
/// fields are private, the one constructor is private to this module, and it has no `Default`, `Clone`, `Serialize` or
/// `Deserialize`.
///
/// An approval can be received and read:
///
/// ```
/// use holzwave_core::integrate::ticket::{Approved, Resolution};
/// use holzwave_core::integrate::ToolId;
///
/// fn describe(approved: &Approved) -> usize {
///     approved.argv().len()
/// }
///
/// fn forge(tool: ToolId, resolution: Resolution) -> Option<Approved> {
///     let _ = (tool, resolution);
///     None
/// }
/// ```
///
/// It cannot be written as a struct literal:
///
/// ```compile_fail
/// use holzwave_core::integrate::ticket::{Approved, Resolution};
/// use holzwave_core::integrate::ToolId;
///
/// fn forge(tool: ToolId, resolution: Resolution) -> Approved {
///     Approved { tool, pane: None, hash: String::new(), resolution }
/// }
/// ```
///
/// It cannot be issued by calling its constructor:
///
/// ```compile_fail
/// use holzwave_core::integrate::ticket::{Approved, Resolution};
/// use holzwave_core::integrate::ToolId;
///
/// fn forge(tool: ToolId, resolution: Resolution) -> Approved {
///     Approved::issue(tool, None, String::new(), resolution)
/// }
/// ```
///
/// It has no default:
///
/// ```compile_fail
/// use holzwave_core::integrate::ticket::{Approved, Resolution};
/// use holzwave_core::integrate::ToolId;
///
/// fn forge() -> Approved {
///     Approved::default()
/// }
/// ```
///
/// It cannot be copied:
///
/// ```compile_fail
/// use holzwave_core::integrate::ticket::{Approved, Resolution};
/// use holzwave_core::integrate::ToolId;
///
/// fn copy(approved: &Approved) -> Approved {
///     approved.clone()
/// }
/// ```
///
/// It cannot be read from data:
///
/// ```compile_fail
/// use holzwave_core::integrate::ticket::{Approved, Resolution};
/// use holzwave_core::integrate::ToolId;
///
/// fn forge(json: &str) -> Approved {
///     serde_json::from_str::<Approved>(json).unwrap()
/// }
/// ```
///
/// The examples share the imports of the one that compiles, so a typo cannot make a failing one pass.
pub struct Approved {
    tool: ToolId,
    pane: Option<PaneId>,
    hash: String,
    resolution: Resolution,
}

impl Approved {
    fn issue(tool: ToolId, pane: Option<PaneId>, hash: String, resolution: Resolution) -> Self {
        Approved { tool, pane, hash, resolution }
    }

    /// The tool.
    pub fn tool(&self) -> ToolId {
        self.tool
    }

    /// The verb.
    pub fn verb(&self) -> &'static Verb {
        self.resolution.verb
    }

    /// What the runner passes after the program.
    pub fn argv(&self) -> &[OsString] {
        &self.resolution.argv
    }

    /// The pane the request came from.
    pub fn pane(&self) -> Option<PaneId> {
        self.pane
    }

    /// The whole resolution that was confirmed.
    pub fn resolution(&self) -> &Resolution {
        &self.resolution
    }

    /// The hash the person confirmed.
    pub fn hash(&self) -> &str {
        &self.hash
    }
}

impl fmt::Debug for Approved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Approved")
            .field("tool", &self.tool)
            .field("verb", &self.resolution.verb.id)
            .field("args", &self.resolution.argv.len())
            .finish()
    }
}

struct Request {
    tool: ToolId,
    verb: String,
    pane: Option<PaneId>,
    args: BTreeMap<String, String>,
}

struct Entry {
    id: String,
    request: Request,
    fields: Vec<(String, String)>,
    hash: String,
    created: Instant,
    attempts: usize,
}

struct Built {
    effect: VerbEffect,
    argv: Vec<String>,
    fields: Vec<(String, String)>,
    hash: String,
}

fn effect_word(effect: VerbEffect) -> &'static str {
    match effect {
        VerbEffect::Read => "read",
        VerbEffect::Local => "local",
        VerbEffect::Write => "write",
        VerbEffect::Exec => "exec",
    }
}

/// Builds the card's fields from a resolution: every text is spelled out with [`visible`], the order is fixed, and the
/// whole is bounded. The hash is over the fields as the person reads them.
fn build(tool: ToolId, resolution: &Resolution) -> Result<Built, TicketError> {
    let effect = resolution.verb.effect;
    if effect == VerbEffect::Read {
        return Err(TicketError::NotGated);
    }
    // The spelled text is never shorter than the raw text, so a raw size over the bound is refused before any copy.
    let raw: usize = resolution.argv.iter().map(|a| a.len()).sum::<usize>()
        + resolution.program.len()
        + resolution.target.iter().map(|(l, v)| l.len() + v.len()).sum::<usize>()
        + resolution.production.as_ref().map_or(0, String::len);
    if raw > MAX_CARD_BYTES {
        return Err(TicketError::Refused);
    }
    // A local change has to say what it changes, and a production guard has to say which one it is.
    if effect == VerbEffect::Local && !resolution.target.iter().any(|(label, _)| label == "changes") {
        return Err(TicketError::Refused);
    }
    if resolution.production.as_deref() == Some("") {
        return Err(TicketError::Refused);
    }
    let mut argv = Vec::with_capacity(resolution.argv.len());
    for arg in &resolution.argv {
        argv.push(visible(arg.to_str().ok_or(TicketError::Refused)?));
    }
    let mut fields = vec![
        ("tool".to_owned(), visible(tool.as_str())),
        ("verb".to_owned(), visible(resolution.verb.id)),
        ("effect".to_owned(), effect_word(effect).to_owned()),
        ("program".to_owned(), visible(&resolution.program)),
    ];
    for (index, arg) in argv.iter().enumerate() {
        fields.push((format!("arg {}", index + 1), arg.clone()));
    }
    for (label, value) in &resolution.target {
        fields.push((visible(label), visible(value)));
    }
    match &resolution.production {
        Some(name) => {
            fields.push(("guard".to_owned(), "production".to_owned()));
            fields.push(("production_name".to_owned(), visible(name)));
        }
        None => fields.push(("guard".to_owned(), "none".to_owned())),
    }
    let size: usize = fields.iter().map(|(name, value)| name.len() + value.len()).sum();
    if size > MAX_CARD_BYTES {
        return Err(TicketError::Refused);
    }
    let hash = hex(&payload_hash(&fields));
    Ok(Built { effect, argv, fields, hash })
}

/// Bounds the request that is kept for the ticket's lifetime. The validators of the arguments cut far below these.
fn check_request(verb: &str, args: &BTreeMap<String, String>) -> Result<(), TicketError> {
    let fits = verb.len() <= MAX_VERB_BYTES
        && args.len() <= MAX_REQUEST_ARGS
        && args.iter().all(|(key, value)| key.len() <= MAX_KEY_BYTES && value.len() <= MAX_VALUE_BYTES);
    if fits { Ok(()) } else { Err(TicketError::Refused) }
}

fn new_id() -> Result<String, TicketError> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|_| TicketError::Refused)?;
    Ok(hex(&raw))
}

/// Whether `text` has the shape of a hash of a card: 64 lower-case hex digits.
fn is_hash_shaped(text: &str) -> bool {
    text.len() == HASH_LEN && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Compares two texts of equal length without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The cards that wait for an answer. In memory only: nothing survives a restart and nothing is remembered.
///
/// A card waits for [`TICKET_LIFETIME`]; at most [`MAX_PENDING`] wait (a ninth drops the oldest);
/// [`MAX_ATTEMPTS`] wrong hashes drop it. A confirm removes the ticket before it returns, so the same ticket never
/// approves twice.
pub struct TicketTable {
    clock: Box<dyn Fn() -> Instant + Send + Sync>,
    inner: Mutex<Vec<Entry>>,
}

impl Default for TicketTable {
    fn default() -> Self {
        Self::new()
    }
}

impl TicketTable {
    /// An empty table that reads the system's monotonic clock.
    pub fn new() -> Self {
        Self::with_clock(Instant::now)
    }

    /// An empty table with its own clock, so that a test needs no waiting.
    pub fn with_clock(clock: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        Self { clock: Box::new(clock), inner: Mutex::new(Vec::new()) }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Entry>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn expired(&self, entry: &Entry) -> bool {
        (self.clock)().saturating_duration_since(entry.created) >= TICKET_LIFETIME
    }

    /// How many tickets wait for an answer. An expired ticket is not counted and is dropped.
    pub fn pending(&self) -> usize {
        let mut entries = self.lock();
        entries.retain(|entry| !self.expired(entry));
        entries.len()
    }

    /// Drops a ticket, for a card that was closed. Returns whether there was one.
    pub fn cancel(&self, ticket: &str) -> bool {
        let mut entries = self.lock();
        let before = entries.len();
        entries.retain(|entry| entry.id != ticket);
        entries.len() != before
    }

    /// Makes the card for a request, or refuses a verb that only reads.
    pub fn prepare(
        &self,
        resolver: &dyn Resolve,
        tool: ToolId,
        verb: &str,
        pane: Option<PaneId>,
        args: &BTreeMap<String, String>,
    ) -> Result<Card, TicketError> {
        check_request(verb, args)?;
        let resolution = resolver.resolve(tool, verb, pane, args)?;
        let built = build(tool, &resolution)?;
        let id = new_id()?;
        let card = Card {
            ticket: id.clone(),
            tool,
            verb: resolution.verb.id.to_owned(),
            effect: built.effect,
            program: visible(&resolution.program),
            argv: built.argv,
            target: resolution.target.iter().map(|(label, value)| (visible(label), visible(value))).collect(),
            guard: Guard { production: resolution.production.as_deref().map(visible) },
            fields: built.fields.clone(),
            hash: built.hash.clone(),
            expires_in_secs: TICKET_LIFETIME.as_secs(),
        };
        let request = Request { tool, verb: verb.to_owned(), pane, args: args.clone() };
        let mut entries = self.lock();
        entries.retain(|entry| !self.expired(entry));
        while entries.len() >= MAX_PENDING {
            entries.remove(0);
        }
        entries.push(Entry {
            id,
            request,
            fields: built.fields,
            hash: built.hash,
            created: (self.clock)(),
            attempts: 0,
        });
        Ok(card)
    }

    /// Turns the answer of the window into an approval, once.
    ///
    /// The ticket is found, and refused when it is unknown, expired or answered with a hash that is not the card's.
    /// With the right hash the ticket is removed first, then the request is resolved again from the facts stored at
    /// prepare and the card is built again; any difference is [`TicketError::Changed`]. So what runs is what the card
    /// showed, and a ticket approves at most once.
    pub fn confirm(&self, resolver: &dyn Resolve, ticket: &str, hash: &str) -> Result<Approved, TicketError> {
        let entry = {
            let mut entries = self.lock();
            let index = entries.iter().position(|e| same(&e.id, ticket)).ok_or(TicketError::Unknown)?;
            if self.expired(&entries[index]) {
                entries.remove(index);
                return Err(TicketError::Expired);
            }
            if !is_hash_shaped(hash) || !same(&entries[index].hash, hash) {
                entries[index].attempts += 1;
                if entries[index].attempts >= MAX_ATTEMPTS {
                    entries.remove(index);
                }
                return Err(TicketError::Mismatch);
            }
            entries.remove(index)
        };
        // The lock is released: the resolver is the service's and may take its own locks.
        let request = &entry.request;
        let resolution = resolver
            .resolve(request.tool, &request.verb, request.pane, &request.args)
            .map_err(|_| TicketError::Changed)?;
        let built = build(request.tool, &resolution).map_err(|_| TicketError::Changed)?;
        if built.fields != entry.fields || !same(&built.hash, &entry.hash) {
            return Err(TicketError::Changed);
        }
        Ok(Approved::issue(request.tool, request.pane, entry.hash, resolution))
    }
}
