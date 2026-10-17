//! The ask desk: one request from a shell that reaches a model only after the person answers in the window.
//!
//! A process in a local pane can hand the app a prompt and some piped text (`wsh ai`). That is not consent: every
//! process in a pane holds the same token, so the token proves "started in a pane" and nothing more. The desk turns the
//! request into an *ask*. It keeps the events of ask conversations away from the page, remembers the route of the last
//! ordinary chat turn, applies the route rule, builds the view of the card, waits for the person and then runs one turn.
//! It reads no pane, file, socket or key.
//!
//! # The flow of [`Asks::run`]
//!
//! 1. The sizes and the text are checked, a pane that was just refused rests, and the route is chosen: the last chat
//!    route of this run, else the only provider that is set up, else a sentence. Nothing is guessed.
//! 2. A conversation is made for the ask and a guard deletes it on every exit.
//! 3. The service classifies the destination ([`AiService::context_state`]), the prompt is cleaned, and the piped text
//!    goes through the context gate as one offered item bound to this ask.
//! 4. Unless the destination is on the device and the person switched the question off, the ask *waits*: the notifier
//!    tells the window, and only [`Asks::settle`] (reached by a webview command and by nothing else) can release it. The
//!    seal in the card is the hash of what will be sent, so an answer is bound to what was shown.
//! 5. A released ask runs as an ordinary turn without tools and streams its text to the caller.
//!
//! # What the desk never does
//!
//! - It never reaches the provider before `settle` recorded `send` for that very ask (for a destination that asks).
//! - It never declares a tool: the turn is sent for [`NO_PANE`], and a tool proposal stops it.
//! - It never keeps the request: the conversation is deleted when the ask ends, and its late events are dropped.
//! - It never prints the prompt or the piped text: every `Debug` here shows kinds, ids and sizes.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ai::gate::{Destination, OfferRequest, RequestItem};
use crate::ai::provider::CancelToken;
use crate::ai::redact::{self, SourceText};
use crate::ai::store::ConversationId;
use crate::ai::types::{ContextKind, Locality, Size};
use crate::layout::PaneId;

use super::{AiEvent, AiEventKind, AiService, AiSink, ContextRefusal, FindingView, SendRequest, ServiceError};

// ---- the constants -----------------------------------------------------------------------------------------------

/// The pane id an ask is sent for. No pane has it (ids start at 1), so the shell's tool host declares no tool for the
/// turn: an ask is a question, not an agent.
pub const NO_PANE: PaneId = 0;
/// The longest prompt, in bytes. The socket checks it too; the desk checks again.
pub const MAX_PROMPT_BYTES: usize = 16_384;
/// The longest piped text, in bytes.
pub const MAX_INPUT_BYTES: usize = 1_048_576;
/// What the envelope calls the piped text.
pub const INPUT_LABEL: &str = "text piped into wsh ai";
/// What is sent when the prompt is empty and text was piped in.
pub const INPUT_ONLY_TEXT: &str = "Respond to the attached text.";
/// How long an ask waits for the person before it ends.
pub const WAIT_LIMIT: Duration = Duration::from_secs(120);
/// How long a pane rests after one of its asks was refused or timed out.
pub const COOLDOWN: Duration = Duration::from_secs(10);
/// The most asks that wait for an answer at the same time.
pub const MAX_WAITING: usize = 4;
/// How often a waiting thread looks at its flags.
pub const POLL: Duration = Duration::from_millis(100);
/// How long a stopped turn is given to end.
pub const END_WAIT: Duration = Duration::from_secs(3);

/// The most panes whose cool-down is remembered.
const MAX_COOLDOWN_PANES: usize = 64;
/// The most finished ask conversations whose late events are still dropped.
const MAX_FINISHED: usize = 64;
/// What the seal is made of first, so a seal of another use of the hash never equals one of an ask.
const SEAL_DOMAIN: &str = "holzwave-ask-1";

/// The limits of the desk. The defaults are the constants above; a test sets shorter ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AskLimits {
    /// How long an ask waits for the person.
    pub wait: Duration,
    /// How long a pane rests after a refusal or a timeout.
    pub cooldown: Duration,
    /// How many asks wait at the same time.
    pub waiting: usize,
}

impl Default for AskLimits {
    fn default() -> Self {
        AskLimits { wait: WAIT_LIMIT, cooldown: COOLDOWN, waiting: MAX_WAITING }
    }
}

// ---- verdicts and endings -----------------------------------------------------------------------------------------

/// What the person pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskVerdict {
    /// Send this one request.
    Send,
    /// Do not send it.
    Refuse,
}

impl AskVerdict {
    /// The verdict a page word names, or `None` for any other word.
    pub fn parse(word: &str) -> Option<AskVerdict> {
        match word {
            "send" => Some(AskVerdict::Send),
            "refuse" => Some(AskVerdict::Refuse),
            _ => None,
        }
    }

    /// The word of the verdict.
    pub fn as_str(self) -> &'static str {
        match self {
            AskVerdict::Send => "send",
            AskVerdict::Refuse => "refuse",
        }
    }
}

/// Why a card closed: the page keys its sentence on this name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosedWhy {
    /// The person pressed `Send once`.
    Sent,
    /// The person pressed `Don't send`.
    Refused,
    /// The program that asked left.
    Gone,
    /// Nobody answered in time.
    TimedOut,
    /// The pane that asked was closed.
    PaneClosed,
    /// The app is closing.
    ShuttingDown,
}

impl ClosedWhy {
    /// The stable lower-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            ClosedWhy::Sent => "sent",
            ClosedWhy::Refused => "refused",
            ClosedWhy::Gone => "gone",
            ClosedWhy::TimedOut => "timed_out",
            ClosedWhy::PaneClosed => "pane_closed",
            ClosedWhy::ShuttingDown => "shutting_down",
        }
    }

    /// The fault an ask ends with for this reason. A card that was answered has no fault of its own.
    fn fault(self) -> AskFault {
        match self {
            ClosedWhy::Sent | ClosedWhy::Refused => AskFault::Refused,
            ClosedWhy::Gone => AskFault::Gone,
            ClosedWhy::TimedOut => AskFault::TimedOut,
            ClosedWhy::PaneClosed => AskFault::PaneClosed,
            ClosedWhy::ShuttingDown => AskFault::ShuttingDown,
        }
    }
}

// ---- the request and what the card shows ---------------------------------------------------------------------------

/// What a shell asked: the pane, the prompt and the piped text. `Debug` prints sizes only.
pub struct AskInput {
    /// The pane the program runs in.
    pub pane: PaneId,
    /// The words after `wsh ai`.
    pub prompt: String,
    /// The text piped in; empty when none was.
    pub input: String,
}

impl fmt::Debug for AskInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AskInput")
            .field("pane", &self.pane)
            .field("prompt", &Size(self.prompt.len()))
            .field("input", &Size(self.input.len()))
            .finish()
    }
}

/// The piped text as the card shows it: counts and what the cleaner removed, never the text.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct InputView {
    /// The lines of the piped text as it arrived.
    pub lines: u64,
    /// Its size in bytes as it arrived.
    pub bytes: u64,
    /// The size of what would be sent, in bytes, after cleaning.
    pub sent_bytes: u64,
    /// What the cleaner removed or masked.
    pub findings: Vec<FindingView>,
}

impl fmt::Debug for InputView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputView")
            .field("lines", &self.lines)
            .field("bytes", &self.bytes)
            .field("sent_bytes", &self.sent_bytes)
            .field("findings", &self.findings.len())
            .finish()
    }
}

/// The card of one waiting ask. `prompt` is the cleaned prompt that is sent; it is the only text in the view.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct AskView {
    /// The id of the ask: 32 lower-case hex digits.
    pub ask: String,
    /// The seal of what will be sent: 64 lower-case hex digits. The answer must carry it.
    pub seal: String,
    /// The pane that asked.
    pub pane: PaneId,
    /// The cleaned prompt that is sent.
    pub prompt: String,
    /// What cleaning removed from the prompt.
    pub prompt_findings: Vec<FindingView>,
    /// The piped text by counts, or `None` when nothing was piped in.
    pub input: Option<InputView>,
    /// The provider profile that would answer.
    pub profile: String,
    /// The model.
    pub model: String,
    /// The host the text would go to, as a label.
    pub host: String,
    /// Where that host is.
    pub locality: Locality,
}

impl fmt::Debug for AskView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AskView")
            .field("ask", &self.ask)
            .field("pane", &self.pane)
            .field("prompt", &Size(self.prompt.len()))
            .field("prompt_findings", &self.prompt_findings.len())
            .field("input", &self.input)
            .field("profile", &self.profile)
            .field("model", &self.model)
            .field("locality", &self.locality)
            .finish()
    }
}

/// What the desk tells the window.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskEvent {
    /// An ask waits for the person.
    Waiting {
        /// The card.
        view: AskView,
    },
    /// An ask is over: the card goes.
    Closed {
        /// The id of the ask.
        ask: String,
        /// Why.
        why: ClosedWhy,
    },
}

impl fmt::Debug for AskEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AskEvent::Waiting { view } => f.debug_struct("Waiting").field("view", view).finish(),
            AskEvent::Closed { ask, why } => f.debug_struct("Closed").field("ask", ask).field("why", why).finish(),
        }
    }
}

/// Where the desk tells the window. Called outside every lock of the desk. A `Closed` event is sent for every `Waiting`
/// event exactly once.
pub trait AskNotify: Send + Sync {
    /// Takes one event for the window of `pane`.
    fn notify(&self, pane: PaneId, event: &AskEvent);
}

/// Where the answer of an ask is written: text for the standard output of the program, notes for its standard error.
/// Both fail once the program is gone.
pub trait AskOut {
    /// Writes a piece of the answer.
    fn text(&mut self, text: &str) -> io::Result<()>;
    /// Writes one line of status.
    fn note(&mut self, text: &str) -> io::Result<()>;
}

// ---- faults ---------------------------------------------------------------------------------------------------------

/// The fixed phrases of the faults, one table: the reason and its sentence. No sentence ends with a full stop and none
/// names a pane, a path, a host or text of the person.
pub const FIXED: [(&str, &str); 17] = [
    ("no_provider", "no provider is set up: add one in AI providers in holzWave first"),
    (
        "choose_provider",
        "several providers are set up and none was used yet: send one message in a holzWave chat to choose, then ask again",
    ),
    ("no_model", "the provider has no model set: choose one in AI providers"),
    ("empty", "there is nothing to ask: the prompt and the piped text are empty after cleaning"),
    ("too_large", "the request is larger than holzWave accepts"),
    ("not_text", "the request is not plain text"),
    ("too_many_waiting", "too many requests wait for an answer in holzWave; answer or close them first"),
    ("cooling_down", "a request from this pane was refused a moment ago; try again in a few seconds"),
    ("refused", "the request was refused in holzWave"),
    ("timed_out", "nobody answered in holzWave in time, so nothing was sent"),
    ("gone", "the request was withdrawn"),
    ("pane_closed", "the pane was closed, so the request was withdrawn"),
    ("shutting_down", "holzWave is closing"),
    ("tools_refused", "the model asked for a tool, which wsh ai does not offer; the request was stopped"),
    ("unknown_ask", "this request is not waiting any more"),
    ("seal_mismatch", "this request changed since it was shown; look at it again"),
    ("bad_verdict", "the answer must be send or refuse"),
];

/// Why an ask ended without an answer, or why an answer to a card was refused. No variant holds the prompt or the
/// piped text.
pub enum AskFault {
    /// No provider is set up.
    NoProvider,
    /// Several are set up and no chat chose one yet.
    ChooseProvider,
    /// The provider has no model.
    NoModel,
    /// Nothing is left to ask after cleaning.
    Empty,
    /// The prompt or the piped text is over its limit.
    TooLarge,
    /// A NUL is in the prompt or the piped text.
    NotText,
    /// The waiting list is full.
    TooManyWaiting,
    /// The pane was refused a moment ago.
    CoolingDown,
    /// The person pressed `Don't send`.
    Refused,
    /// Nobody answered in time.
    TimedOut,
    /// The program that asked left.
    Gone,
    /// The pane was closed.
    PaneClosed,
    /// The app is closing.
    ShuttingDown,
    /// The model asked for a tool.
    ToolsRefused,
    /// No ask with this id waits.
    UnknownAsk,
    /// The seal is not the one of the card.
    SealMismatch,
    /// The verdict is neither word.
    BadVerdict,
    /// The context gate or the destination refused.
    Context(ContextRefusal),
    /// The service refused.
    Service(ServiceError),
    /// The turn ended without an answer: how (`cancelled`, `failed`, a `limit_` name) and the error text of a failure.
    Turn {
        /// The end of the turn.
        end: String,
        /// The error text of a failed turn; empty otherwise.
        text: String,
    },
}

impl AskFault {
    /// The stable lower-case name.
    pub fn reason(&self) -> &'static str {
        match self {
            AskFault::NoProvider => "no_provider",
            AskFault::ChooseProvider => "choose_provider",
            AskFault::NoModel => "no_model",
            AskFault::Empty => "empty",
            AskFault::TooLarge => "too_large",
            AskFault::NotText => "not_text",
            AskFault::TooManyWaiting => "too_many_waiting",
            AskFault::CoolingDown => "cooling_down",
            AskFault::Refused => "refused",
            AskFault::TimedOut => "timed_out",
            AskFault::Gone => "gone",
            AskFault::PaneClosed => "pane_closed",
            AskFault::ShuttingDown => "shutting_down",
            AskFault::ToolsRefused => "tools_refused",
            AskFault::UnknownAsk => "unknown_ask",
            AskFault::SealMismatch => "seal_mismatch",
            AskFault::BadVerdict => "bad_verdict",
            AskFault::Context(_) => "context",
            AskFault::Service(_) => "service",
            AskFault::Turn { .. } => "turn",
        }
    }

    /// The sentence of a reason of the table, or `None` for another word.
    pub fn phrase(reason: &str) -> Option<&'static str> {
        FIXED.iter().find(|(name, _)| *name == reason).map(|(_, text)| *text)
    }
}

impl fmt::Display for AskFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AskFault::Context(refusal) => fmt::Display::fmt(refusal, f),
            AskFault::Service(error) => fmt::Display::fmt(error, f),
            AskFault::Turn { end, text } => {
                if end == "cancelled" {
                    f.write_str("the answer was cancelled")
                } else if end == "failed" {
                    if text.is_empty() { f.write_str("the request failed") } else { f.write_str(text) }
                } else {
                    f.write_str("the answer stopped at a size limit")
                }
            }
            other => f.write_str(AskFault::phrase(other.reason()).unwrap_or("the request failed")),
        }
    }
}

impl fmt::Debug for AskFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AskFault::Context(refusal) => write!(f, "AskFault::Context({})", refusal.reason()),
            AskFault::Service(error) => write!(f, "AskFault::Service({})", error.code()),
            AskFault::Turn { end, text } => {
                f.debug_struct("AskFault::Turn").field("end", end).field("text", &Size(text.len())).finish()
            }
            other => write!(f, "AskFault::{}", other.reason()),
        }
    }
}

impl std::error::Error for AskFault {}

impl From<ServiceError> for AskFault {
    fn from(error: ServiceError) -> Self {
        match error {
            ServiceError::Context(refusal) => AskFault::Context(refusal),
            other => AskFault::Service(other),
        }
    }
}

// ---- the state --------------------------------------------------------------------------------------------------------

/// One ask that waits for the person.
struct Entry {
    id: String,
    pane: PaneId,
    seal: String,
    view: AskView,
    /// Set by [`Asks::settle`], and by nothing else.
    verdict: Option<AskVerdict>,
    since: Instant,
}

/// One ask that has an id and has not ended: it may wait, or it may be streaming.
struct Running {
    pane: PaneId,
    own: CancelToken,
    /// Why the desk itself stopped the ask (the pane closed, the app is closing).
    why: Option<ClosedWhy>,
}

#[derive(Default)]
struct State {
    waiting: Vec<Entry>,
    running: HashMap<String, Running>,
    /// The conversations of released asks, to the channel their events are read from.
    taps: HashMap<String, Sender<AiEvent>>,
    /// The conversations of asks that ended, newest last: their late events are dropped.
    finished: VecDeque<String>,
    last_route: Option<(String, String)>,
    candidates: Vec<(String, String)>,
    cooldown: HashMap<PaneId, Instant>,
    closing: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Lower-case hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            out.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
    out
}

/// The name of a place in the seal.
fn place(locality: Locality) -> &'static str {
    match locality {
        Locality::OnDevice => "on_device",
        Locality::Loopback => "loopback",
        Locality::Lan => "lan",
        Locality::Cloud => "cloud",
    }
}

/// The seal of an ask: the hash of everything that decides what is sent and where. The card carries it and the answer
/// must carry it back, so an answer is bound to what was shown.
#[allow(clippy::too_many_arguments)]
fn seal_of(
    ask: &str,
    pane: PaneId,
    profile: &str,
    model: &str,
    host: &str,
    locality: Locality,
    prompt: &str,
    envelope: &str,
) -> String {
    let pane = pane.to_string();
    let mut hash = Sha256::new();
    for part in [SEAL_DOMAIN, ask, &pane, profile, model, host, place(locality), prompt, envelope] {
        hash.update(part.as_bytes());
        hash.update([0u8]);
    }
    let digest: [u8; 32] = hash.finalize().into();
    hex(&digest)
}

/// Why the ask `ask` must stop, read from a state the caller holds: the caller left, or the desk stopped it.
fn stop_reason(state: &State, ask: &str, cancel: &CancelToken, own: &CancelToken) -> Option<ClosedWhy> {
    if cancel.is_cancelled() {
        return Some(ClosedWhy::Gone);
    }
    if own.is_cancelled() {
        let why = state.running.get(ask).and_then(|running| running.why);
        return Some(why.unwrap_or(ClosedWhy::ShuttingDown));
    }
    None
}

/// A fresh ask id: 32 lower-case hex digits.
fn fresh_id() -> Result<String, AskFault> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|_| AskFault::Service(ServiceError::Spawn))?;
    Ok(hex(&raw))
}

// ---- the desk -----------------------------------------------------------------------------------------------------------

/// The desk. It is the sink of the service: events of ask conversations go to the desk and the others to the page.
pub struct Asks {
    inner: Arc<dyn AiSink>,
    notify: Arc<dyn AskNotify>,
    limits: AskLimits,
    state: Mutex<State>,
    wake: Condvar,
}

impl fmt::Debug for Asks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = lock(&self.state);
        f.debug_struct("Asks")
            .field("waiting", &state.waiting.len())
            .field("running", &state.running.len())
            .field("taps", &state.taps.len())
            .field("candidates", &state.candidates.len())
            .finish()
    }
}

impl AiSink for Asks {
    fn emit(&self, event: &AiEvent) {
        enum Way {
            Tap(Sender<AiEvent>),
            Drop,
            Pass,
        }
        let way = {
            let mut state = lock(&self.state);
            if let Some(tap) = state.taps.get(&event.conversation) {
                Way::Tap(tap.clone())
            } else if state.finished.iter().any(|known| *known == event.conversation) {
                Way::Drop
            } else {
                if let AiEventKind::TurnStarted { route, .. } = &event.kind
                    && !route.profile.is_empty()
                    && !route.model.is_empty()
                {
                    state.last_route = Some((route.profile.clone(), route.model.clone()));
                }
                Way::Pass
            }
        };
        match way {
            Way::Tap(tap) => {
                // A reader that left has dropped its end; the event is of no use to anyone then.
                let _ = tap.send(event.clone());
            }
            Way::Drop => {}
            Way::Pass => self.inner.emit(event),
        }
    }
}

impl Asks {
    /// A desk over the sink of the page and the notifier of the window, with the default limits.
    pub fn new(inner: Arc<dyn AiSink>, notify: Arc<dyn AskNotify>) -> Arc<Asks> {
        Asks::with_limits(inner, notify, AskLimits::default())
    }

    /// A desk with limits of its own.
    pub fn with_limits(inner: Arc<dyn AiSink>, notify: Arc<dyn AskNotify>, limits: AskLimits) -> Arc<Asks> {
        Arc::new(Asks { inner, notify, limits, state: Mutex::new(State::default()), wake: Condvar::new() })
    }

    /// Registers a provider that counts as set up for the route rule: one that is not a profile (the on-device model).
    pub fn add_candidate(&self, profile: &str, model: &str) {
        let mut state = lock(&self.state);
        if !state.candidates.iter().any(|(known, _)| known == profile) {
            state.candidates.push((profile.to_owned(), model.to_owned()));
        }
    }

    /// The profile and the model of the last chat turn of this run, if there was one.
    pub fn last_route(&self) -> Option<(String, String)> {
        lock(&self.state).last_route.clone()
    }

    // ---- the route ------------------------------------------------------------------------------------------------

    /// The destination of an ask: the route of the last chat turn, else the only provider that is set up, else a
    /// sentence. The command line has no flag to choose one yet, so nothing here guesses.
    fn route(&self, service: &AiService) -> Result<(String, String), AskFault> {
        let (last, registered) = {
            let state = lock(&self.state);
            (state.last_route.clone(), state.candidates.clone())
        };
        if let Some(route) = last {
            return Ok(route);
        }
        let mut all = registered;
        for view in service.profiles() {
            if !all.iter().any(|(known, _)| *known == view.id) {
                all.push((view.id, view.model));
            }
        }
        match all.len() {
            0 => Err(AskFault::NoProvider),
            1 => {
                let (profile, model) = all.remove(0);
                if model.trim().is_empty() { Err(AskFault::NoModel) } else { Ok((profile, model)) }
            }
            _ => Err(AskFault::ChooseProvider),
        }
    }

    // ---- the cool-down -------------------------------------------------------------------------------------------------

    /// Starts the rest of `pane`. The table is bounded: the expired go first, then the oldest.
    fn start_cooldown(&self, state: &mut State, pane: PaneId) {
        let now = Instant::now();
        state.cooldown.retain(|_, until| *until > now);
        if state.cooldown.len() >= MAX_COOLDOWN_PANES
            && let Some(oldest) = state.cooldown.iter().min_by_key(|(_, until)| **until).map(|(pane, _)| *pane)
        {
            state.cooldown.remove(&oldest);
        }
        state.cooldown.insert(pane, now + self.limits.cooldown);
    }

    // ---- the answer -----------------------------------------------------------------------------------------------------

    /// Records the person's answer to the card of `ask`. This is the only place that sets a verdict, and it never
    /// blocks. A seal that is not the one of the card keeps the ask waiting.
    pub fn settle(&self, ask: &str, verdict: AskVerdict, seal: &str) -> Result<(), AskFault> {
        let pane = {
            let mut state = lock(&self.state);
            let found = state.waiting.iter().position(|entry| entry.id == ask && entry.verdict.is_none());
            let Some(at) = found else { return Err(AskFault::UnknownAsk) };
            if state.waiting[at].seal != seal {
                return Err(AskFault::SealMismatch);
            }
            state.waiting[at].verdict = Some(verdict);
            let pane = state.waiting[at].pane;
            if verdict == AskVerdict::Refuse {
                self.start_cooldown(&mut state, pane);
            }
            pane
        };
        self.wake.notify_all();
        let why = if verdict == AskVerdict::Send { ClosedWhy::Sent } else { ClosedWhy::Refused };
        self.notify.notify(pane, &AskEvent::Closed { ask: ask.to_owned(), why });
        Ok(())
    }

    /// The cards that wait for a pane `wanted` accepts, oldest first.
    pub fn pending(&self, wanted: &dyn Fn(PaneId) -> bool) -> Vec<AskView> {
        let candidates: Vec<(PaneId, AskView)> = lock(&self.state)
            .waiting
            .iter()
            .filter(|entry| entry.verdict.is_none())
            .map(|entry| (entry.pane, entry.view.clone()))
            .collect();
        candidates.into_iter().filter(|(pane, _)| wanted(*pane)).map(|(_, view)| view).collect()
    }

    /// Ends the asks of a pane that was closed. Returns how many it stopped.
    pub fn pane_closed(&self, pane: PaneId) -> usize {
        let stopped = {
            let mut state = lock(&self.state);
            state.cooldown.remove(&pane);
            let mut stopped = 0;
            for running in state.running.values_mut().filter(|running| running.pane == pane) {
                running.why.get_or_insert(ClosedWhy::PaneClosed);
                running.own.cancel();
                stopped += 1;
            }
            stopped
        };
        self.wake.notify_all();
        stopped
    }

    /// Ends every ask and refuses new ones. Returns how many it stopped.
    pub fn shutdown(&self) -> usize {
        let stopped = {
            let mut state = lock(&self.state);
            state.closing = true;
            for running in state.running.values_mut() {
                running.why.get_or_insert(ClosedWhy::ShuttingDown);
                running.own.cancel();
            }
            state.running.len()
        };
        self.wake.notify_all();
        stopped
    }

    // ---- the ask --------------------------------------------------------------------------------------------------------

    /// Runs one ask and blocks until it ends: it waits for the person where the destination asks, runs one turn
    /// without tools and writes the answer to `out`. Call it from a thread that may wait. A cancelled `cancel` (the
    /// program left) ends the ask at once.
    pub fn run(
        &self,
        service: &AiService,
        input: AskInput,
        out: &mut dyn AskOut,
        cancel: &CancelToken,
    ) -> Result<(), AskFault> {
        let AskInput { pane, prompt, input: piped } = input;

        // The sizes and the text. Nothing is made or looked up before these pass.
        if prompt.len() > MAX_PROMPT_BYTES || piped.len() > MAX_INPUT_BYTES {
            return Err(AskFault::TooLarge);
        }
        if prompt.contains('\0') || piped.contains('\0') {
            return Err(AskFault::NotText);
        }
        let cleaned = redact::sanitise(&prompt);
        let piped_left = !piped.is_empty() && !redact::sanitise(&piped).text.trim().is_empty();
        if cleaned.text.trim().is_empty() && !piped_left {
            return Err(AskFault::Empty);
        }
        drop(prompt);

        let ask = fresh_id()?;
        let own = CancelToken::new();
        self.admit(&ask, pane, &own)?;
        // From here on every exit takes the ask off the desk. The conversation is made next and added to the guard.
        let mut guard = Cleanup { asks: self, service, ask: ask.clone(), conversation: None };

        let (profile, model) = self.route(service)?;
        let conversation = service.new_conversation().map_err(AskFault::from)?;
        guard.conversation = Some(conversation.clone());

        let state = service.context_state(&conversation, pane, Some(&profile), Some(&model)).map_err(AskFault::from)?;
        let destination = Destination { host: state.host.clone(), locality: state.locality };

        // The piped text goes through the gate as one item bound to this ask; its counts are read before it moves.
        let lines = piped.lines().count() as u64;
        let raw_bytes = piped.len() as u64;
        let scope = format!("wsh-ask:{ask}");
        let mut offered = None;
        if piped_left {
            let item = RequestItem::new(ContextKind::Selection, INPUT_LABEL, SourceText::plain(piped));
            let request = OfferRequest::new(profile.clone(), destination, vec![item]).scoped(scope.clone());
            // An offer that is refused as empty means nothing is left of the piped text after cleaning.
            offered = service.gate().offer(request).ok();
        }
        if cleaned.text.trim().is_empty() && offered.is_none() {
            return Err(AskFault::Empty);
        }
        let (input_view, envelope) = match &offered {
            Some(offer) => {
                let first = offer.items().first();
                let view = InputView {
                    lines,
                    bytes: raw_bytes,
                    sent_bytes: first.map_or(0, |item| item.bytes()),
                    findings: first
                        .map(|item| {
                            item.findings()
                                .iter()
                                .map(|found| FindingView { rule: found.rule.clone(), count: found.count })
                                .collect()
                        })
                        .unwrap_or_default(),
                };
                let envelope = offer.envelope(&[1]).unwrap_or_default();
                (Some(view), envelope)
            }
            None => (None, String::new()),
        };
        let seal = seal_of(&ask, pane, &state.profile, &state.model, &state.host, state.locality, &cleaned.text, &envelope);
        let view = AskView {
            ask: ask.clone(),
            seal: seal.clone(),
            pane,
            prompt: cleaned.text.clone(),
            prompt_findings: cleaned
                .hits
                .iter()
                .map(|hit| FindingView { rule: hit.rule.to_owned(), count: hit.count })
                .collect(),
            input: input_view,
            profile: state.profile.clone(),
            model: state.model.clone(),
            host: state.host.clone(),
            locality: state.locality,
        };

        // A destination that asks waits here until the person answers; the answer is recorded by `settle` only.
        if state.required {
            self.wait_for_the_person(&ask, pane, view, seal, out, cancel, &own)?;
        }

        // Released. The approval is made now, from the offer the gate remembers for this very ask.
        let mut context = Vec::new();
        if let Some(offer) = &offered {
            let approved = service
                .gate()
                .approve_scoped(offer.id(), &[1], &profile, &scope)
                .map_err(|_| AskFault::Context(ContextRefusal::StaleOffer))?;
            context.push(approved);
        }
        let text = if cleaned.text.trim().is_empty() { INPUT_ONLY_TEXT.to_owned() } else { cleaned.text.clone() };
        let (tap, events) = mpsc::channel();
        lock(&self.state).taps.insert(conversation.as_str().to_owned(), tap);
        // `NO_PANE` on purpose: no pane has this id, so the shell's tool host declares no tool for the turn.
        let request = SendRequest::new(conversation.clone(), text, NO_PANE)
            .profile(profile.clone())
            .model(model.clone())
            .context(context);
        service.send(request).map_err(AskFault::from)?;
        self.stream(service, &conversation, &ask, &events, out, cancel, &own)
    }

    /// Puts the ask on the desk's list of live asks. A closing desk takes none.
    fn admit(&self, ask: &str, pane: PaneId, own: &CancelToken) -> Result<(), AskFault> {
        let mut state = lock(&self.state);
        if state.closing {
            return Err(AskFault::ShuttingDown);
        }
        let now = Instant::now();
        if state.cooldown.get(&pane).is_some_and(|until| *until > now) {
            return Err(AskFault::CoolingDown);
        }
        state.running.insert(ask.to_owned(), Running { pane, own: own.clone(), why: None });
        Ok(())
    }

    /// Why the ask must stop now, if it must: the caller left, or the desk stopped it.
    fn must_stop(&self, ask: &str, cancel: &CancelToken, own: &CancelToken) -> Option<ClosedWhy> {
        // The caller's token is read first and needs no lock.
        if cancel.is_cancelled() {
            return Some(ClosedWhy::Gone);
        }
        stop_reason(&lock(&self.state), ask, cancel, own)
    }

    /// Shows the card and waits for the verdict. Returns when the person said `send`; every other end is a fault.
    #[allow(clippy::too_many_arguments)]
    fn wait_for_the_person(
        &self,
        ask: &str,
        pane: PaneId,
        view: AskView,
        seal: String,
        out: &mut dyn AskOut,
        cancel: &CancelToken,
        own: &CancelToken,
    ) -> Result<(), AskFault> {
        {
            let mut state = lock(&self.state);
            let waiting = state.waiting.iter().filter(|entry| entry.verdict.is_none()).count();
            if waiting >= self.limits.waiting {
                return Err(AskFault::TooManyWaiting);
            }
            state.waiting.push(Entry {
                id: ask.to_owned(),
                pane,
                seal,
                view: view.clone(),
                verdict: None,
                since: Instant::now(),
            });
        }
        self.notify.notify(pane, &AskEvent::Waiting { view });
        let said = format!("waiting for your answer in holzWave (pane {pane})");
        if out.note(&said).is_err() {
            return Err(self.withdraw(ask, ClosedWhy::Gone));
        }

        let mut state = lock(&self.state);
        loop {
            let Some(at) = state.waiting.iter().position(|entry| entry.id == ask) else {
                return Err(AskFault::Gone);
            };
            if let Some(verdict) = state.waiting[at].verdict {
                state.waiting.remove(at);
                return match verdict {
                    AskVerdict::Send => Ok(()),
                    AskVerdict::Refuse => Err(AskFault::Refused),
                };
            }
            let why = stop_reason(&state, ask, cancel, own).or_else(|| {
                (state.waiting[at].since.elapsed() >= self.limits.wait).then_some(ClosedWhy::TimedOut)
            });
            if let Some(why) = why {
                state.waiting.remove(at);
                let pane = state.running.get(ask).map_or(pane, |running| running.pane);
                if why == ClosedWhy::TimedOut {
                    self.start_cooldown(&mut state, pane);
                }
                drop(state);
                self.notify.notify(pane, &AskEvent::Closed { ask: ask.to_owned(), why });
                return Err(why.fault());
            }
            let (next, _) = self.wake.wait_timeout(state, POLL).unwrap_or_else(PoisonError::into_inner);
            state = next;
        }
    }

    /// Takes a waiting ask off the list and tells the window why, once. Answers the fault for the reason.
    fn withdraw(&self, ask: &str, why: ClosedWhy) -> AskFault {
        let pane = {
            let mut state = lock(&self.state);
            let at = state.waiting.iter().position(|entry| entry.id == ask && entry.verdict.is_none());
            at.map(|at| state.waiting.remove(at).pane)
        };
        if let Some(pane) = pane {
            self.notify.notify(pane, &AskEvent::Closed { ask: ask.to_owned(), why });
        }
        why.fault()
    }

    /// Reads the events of the released turn and writes its text to `out`.
    #[allow(clippy::too_many_arguments)]
    fn stream(
        &self,
        service: &AiService,
        conversation: &ConversationId,
        ask: &str,
        events: &mpsc::Receiver<AiEvent>,
        out: &mut dyn AskOut,
        cancel: &CancelToken,
        own: &CancelToken,
    ) -> Result<(), AskFault> {
        loop {
            if let Some(why) = self.must_stop(ask, cancel, own) {
                self.stop_turn(service, conversation, events);
                return Err(why.fault());
            }
            match events.recv_timeout(POLL) {
                Ok(event) => match event.kind {
                    AiEventKind::Delta { text } => {
                        if out.text(&text).is_err() {
                            self.stop_turn(service, conversation, events);
                            return Err(AskFault::Gone);
                        }
                    }
                    AiEventKind::ToolProposed { .. } => {
                        self.stop_turn(service, conversation, events);
                        return Err(AskFault::ToolsRefused);
                    }
                    AiEventKind::TurnEnded(end) => {
                        if end.end == "done" {
                            return Ok(());
                        }
                        let text = end.error.map(|error| error.text).unwrap_or_default();
                        return Err(AskFault::Turn { end: end.end, text });
                    }
                    _ => {}
                },
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(AskFault::Gone),
            }
        }
    }

    /// Stops the turn of an ask, voids its calls and gives it a moment to end.
    fn stop_turn(&self, service: &AiService, conversation: &ConversationId, events: &mpsc::Receiver<AiEvent>) {
        service.cancel(conversation);
        service.approvals().cancel_conversation(conversation.as_str());
        let deadline = Instant::now() + END_WAIT;
        while Instant::now() < deadline {
            match events.recv_timeout(POLL) {
                Ok(event) if matches!(event.kind, AiEventKind::TurnEnded(_)) => return,
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// What every exit of `run` does: the ask leaves the desk, its conversation's late events are dropped.
    fn finish(&self, ask: &str, conversation: Option<&ConversationId>) {
        let orphan = {
            let mut state = lock(&self.state);
            state.running.remove(ask);
            if let Some(conversation) = conversation {
                state.taps.remove(conversation.as_str());
                state.finished.push_back(conversation.as_str().to_owned());
                while state.finished.len() > MAX_FINISHED {
                    state.finished.pop_front();
                }
            }
            let at = state.waiting.iter().position(|entry| entry.id == ask);
            at.map(|at| state.waiting.remove(at)).filter(|entry| entry.verdict.is_none()).map(|entry| entry.pane)
        };
        // A card that is still on the list when the ask leaves is told away, so a card never outlives its ask.
        if let Some(pane) = orphan {
            self.notify.notify(pane, &AskEvent::Closed { ask: ask.to_owned(), why: ClosedWhy::Gone });
        }
    }
}

/// Runs when `run` is left, however it is left: the ask leaves the desk and its conversation is stopped and deleted, so
/// the request is not kept.
struct Cleanup<'a> {
    asks: &'a Asks,
    service: &'a AiService,
    ask: String,
    conversation: Option<ConversationId>,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        self.asks.finish(&self.ask, self.conversation.as_ref());
        if let Some(conversation) = &self.conversation {
            self.service.cancel(conversation);
            self.service.approvals().cancel_conversation(conversation.as_str());
            let _ = self.service.delete_conversation(conversation);
        }
    }
}
