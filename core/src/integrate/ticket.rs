//! Prepare and confirm: the card for a verb that changes something, the ticket behind it, and the approval that only a
//! confirm of that ticket can produce.
//!
//! The shape is the one of the AI chat's approval table on purpose: a card shows the exact request, a hash binds the
//! card to the request, only the window decides, the core resolves and hashes the request again when the answer
//! arrives, and what runs happens once. Nothing here starts a process: [`Approved`] is a token that the service turns
//! into a run.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::ai::tools::{hex, payload_hash};
use crate::layout::PaneId;

use super::{ToolId, Verb, VerbEffect};

/// How long a card waits for its answer.
pub const TICKET_LIFETIME: Duration = Duration::from_secs(120);

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

/// The proof that a card was confirmed and still is what the person saw. Only [`TicketTable::confirm`] makes one.
pub struct Approved {
    tool: ToolId,
    pane: Option<PaneId>,
    hash: String,
    resolution: Resolution,
}

impl Approved {
    fn issue(tool: ToolId, pane: Option<PaneId>, hash: String, resolution: Resolution) -> Self {
        Self { tool, pane, hash, resolution }
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

struct Request {
    tool: ToolId,
    verb: String,
    pane: Option<PaneId>,
    args: BTreeMap<String, String>,
}

struct Entry {
    id: String,
    request: Request,
    hash: String,
    created: Instant,
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

fn build(tool: ToolId, resolution: &Resolution) -> Result<Built, TicketError> {
    let effect = resolution.verb.effect;
    if effect == VerbEffect::Read {
        return Err(TicketError::NotGated);
    }
    let mut argv = Vec::new();
    for arg in &resolution.argv {
        argv.push(arg.to_str().ok_or(TicketError::Refused)?.to_owned());
    }
    let mut fields = vec![
        ("tool".to_owned(), tool.as_str().to_owned()),
        ("verb".to_owned(), resolution.verb.id.to_owned()),
        ("effect".to_owned(), effect_word(effect).to_owned()),
        ("program".to_owned(), resolution.program.clone()),
    ];
    for (index, arg) in argv.iter().enumerate() {
        fields.push((format!("arg {}", index + 1), arg.clone()));
    }
    for (label, value) in &resolution.target {
        fields.push((label.clone(), value.clone()));
    }
    match &resolution.production {
        Some(name) => {
            fields.push(("guard".to_owned(), "production".to_owned()));
            fields.push(("production_name".to_owned(), name.clone()));
        }
        None => fields.push(("guard".to_owned(), "none".to_owned())),
    }
    let hash = hex(&payload_hash(&fields));
    Ok(Built { effect, argv, fields, hash })
}

fn new_id() -> Result<String, TicketError> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|_| TicketError::Refused)?;
    Ok(hex(&raw))
}

/// The cards that wait for an answer. In memory only: nothing survives a restart and nothing is remembered.
pub struct TicketTable {
    inner: Mutex<Vec<Entry>>,
}

impl Default for TicketTable {
    fn default() -> Self {
        Self::new()
    }
}

impl TicketTable {
    /// An empty table.
    pub fn new() -> Self {
        Self { inner: Mutex::new(Vec::new()) }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Entry>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
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
        let resolution = resolver.resolve(tool, verb, pane, args)?;
        let built = build(tool, &resolution)?;
        let id = new_id()?;
        let card = Card {
            ticket: id.clone(),
            tool,
            verb: resolution.verb.id.to_owned(),
            effect: built.effect,
            program: resolution.program.clone(),
            argv: built.argv,
            target: resolution.target.clone(),
            guard: Guard { production: resolution.production.clone() },
            fields: built.fields,
            hash: built.hash.clone(),
            expires_in_secs: TICKET_LIFETIME.as_secs(),
        };
        let request = Request { tool, verb: verb.to_owned(), pane, args: args.clone() };
        self.lock().push(Entry { id, request, hash: built.hash, created: Instant::now() });
        Ok(card)
    }

    /// Turns the answer of the window into an approval, once.
    pub fn confirm(&self, resolver: &dyn Resolve, ticket: &str, hash: &str) -> Result<Approved, TicketError> {
        let entry = {
            let mut entries = self.lock();
            let index = entries.iter().position(|e| e.id == ticket).ok_or(TicketError::Unknown)?;
            if entries[index].hash != hash {
                return Err(TicketError::Mismatch);
            }
            entries.remove(index)
        };
        let request = &entry.request;
        let resolution = resolver.resolve(request.tool, &request.verb, request.pane, &request.args)?;
        Ok(Approved::issue(request.tool, request.pane, entry.hash, resolution))
    }
}
