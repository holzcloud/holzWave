//! The seam between the context gate and whatever holds the text: the source trait, the record of which pane is shared
//! for which chat and route, and the views the page gets.
//!
//! Terminal and file text reaches a provider in four steps, and each step is a call of [`AiService`](super::AiService):
//!
//! 1. **share**: the user shares a pane for a chat. The service records the pane against the chat and the route (the
//!    profile, the model and the origin of the base address). The record is the permission to read that pane into an
//!    offer; nothing is read yet.
//! 2. **preview**: the service reads the kinds the user asked for through a [`ContextSource`], hands the text to the
//!    gate and answers an [`OfferView`]: labels, sizes and what the gate removed. The view has no text member.
//! 3. **send**: the page sends the offer id and the picked item numbers with the typed text. The service spends the
//!    offer for this chat, this pane and this route only.
//! 4. **end**: a route change, the end of the chat or of the pane, and the removal of the profile each end the share.
//!
//! The text never goes to the page. It leaves this module only through the gate, as the approved envelope of the send
//! that spends the offer.
//!
//! This module holds no sanitising and no redaction: a source returns the text and the hidden ranges as the grid or the
//! file holds them, and the gate is the one cleaner.

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;

use crate::ai::gate::{ContextOffer, OfferId};
use crate::ai::redact::{self, SourceText};
use crate::ai::types::{ContextKind, Locality, Size};
use crate::layout::PaneId;

/// The most bytes a source reads for one item: the limit of the gate's raw cut, so a read is never wasted.
pub const READ_BYTES: usize = redact::MAX_RAW_ITEM_BYTES;
/// The most kinds one preview asks for.
pub const MAX_PREVIEW_KINDS: usize = 5;
/// The most shares the service remembers; a new one drops the oldest.
pub const MAX_SHARES: usize = 256;
/// What a label gets when the read was cut at the limit.
pub const CUT_NOTE: &str = " (cut)";

// ---- the source -----------------------------------------------------------------------------------------------------

/// Why a source could not give an item. `Copy` and nothing else: no message, no path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadRefusal {
    /// The pane is gone.
    PaneGone,
    /// The pane does not offer this kind.
    Unsupported,
    /// The place is on the deny list.
    Denied,
    /// The file is not text.
    NotText,
    /// The text could not be read.
    Unreadable,
    /// The pane did not answer in time.
    Busy,
}

impl ReadRefusal {
    /// The stable lower-case name.
    pub fn reason(self) -> &'static str {
        match self {
            ReadRefusal::PaneGone => "pane_gone",
            ReadRefusal::Unsupported => "unsupported_kind",
            ReadRefusal::Denied => "denied",
            ReadRefusal::NotText => "not_text",
            ReadRefusal::Unreadable => "unreadable",
            ReadRefusal::Busy => "busy",
        }
    }
}

/// Text a source read, before the gate has cleaned it.
///
/// `Debug` prints sizes only: the text, and a label that may be a file name, stay out of every log line.
pub struct Fetched {
    /// A short name for the user: `Screen of pane 3`, a file name. The gate cleans it.
    pub label: String,
    /// The text as the grid or the file holds it, with the byte ranges of cells the engine drew hidden.
    pub text: SourceText,
    /// Whether the source cut the read at the limit it was given.
    pub cut: bool,
}

impl Fetched {
    /// An item.
    pub fn new(label: impl Into<String>, text: SourceText, cut: bool) -> Self {
        Self { label: label.into(), text, cut }
    }
}

impl fmt::Debug for Fetched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fetched")
            .field("label", &Size(self.label.len()))
            .field("text", &self.text)
            .field("cut", &self.cut)
            .finish()
    }
}

/// What holds the text: the shell, over its panes and files, or a double in a test.
///
/// A source is asked only after the user shared the pane. `read` may wait on an engine thread or a remote file, so the
/// service never calls it under one of its locks and a caller must not call the service from the main thread.
pub trait ContextSource: Send + Sync {
    /// What `pane` can offer now, in the order the card lists them. Empty when it can offer nothing.
    fn kinds(&self, pane: PaneId) -> Vec<ContextKind>;

    /// Reads one kind from `pane`, at most `max_bytes`. `Ok(None)` means nothing is there: no selection, no marks, an
    /// empty screen.
    fn read(&self, pane: PaneId, kind: ContextKind, max_bytes: usize) -> Result<Option<Fetched>, ReadRefusal>;
}

/// The default source: no pane offers anything.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoContext;

impl ContextSource for NoContext {
    fn kinds(&self, _pane: PaneId) -> Vec<ContextKind> {
        Vec::new()
    }

    fn read(&self, _pane: PaneId, _kind: ContextKind, _max_bytes: usize) -> Result<Option<Fetched>, ReadRefusal> {
        Err(ReadRefusal::Unsupported)
    }
}

// ---- refusals -------------------------------------------------------------------------------------------------------

/// Why a context call was refused. One of 13 stable reasons; `Display` is a fixed phrase that holds no pane number, no
/// path and no host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextRefusal {
    /// The pane is not shared for this chat and route, and the destination asks.
    NotShared,
    /// The offer is spent, expired, voided, or was made for another chat, pane or route.
    StaleOffer,
    /// The picked item numbers are empty, too many or unknown.
    BadPick,
    /// The kinds asked for are none, too many or repeated.
    BadKinds,
    /// The pane does not offer a kind that was asked for.
    UnsupportedKind,
    /// Everything read was empty, or nothing was left after cleaning.
    NothingToShare,
    /// The pane is gone.
    PaneGone,
    /// The place is on the deny list.
    Denied,
    /// The file is not text.
    NotText,
    /// The text could not be read.
    Unreadable,
    /// The pane did not answer in time.
    Busy,
    /// The destination cannot be checked, or is farther from the device than the user approved.
    Destination,
    /// "Local only" is on and the destination leaves the device.
    LocalOnly,
}

impl ContextRefusal {
    /// All 13, in the order of the list in the documentation.
    pub const ALL: [ContextRefusal; 13] = [
        ContextRefusal::NotShared,
        ContextRefusal::StaleOffer,
        ContextRefusal::BadPick,
        ContextRefusal::BadKinds,
        ContextRefusal::UnsupportedKind,
        ContextRefusal::NothingToShare,
        ContextRefusal::PaneGone,
        ContextRefusal::Denied,
        ContextRefusal::NotText,
        ContextRefusal::Unreadable,
        ContextRefusal::Busy,
        ContextRefusal::Destination,
        ContextRefusal::LocalOnly,
    ];

    /// The stable lower-case name the page keys its text on.
    pub fn reason(self) -> &'static str {
        match self {
            ContextRefusal::NotShared => "not_shared",
            ContextRefusal::StaleOffer => "stale_offer",
            ContextRefusal::BadPick => "bad_pick",
            ContextRefusal::BadKinds => "bad_kinds",
            ContextRefusal::UnsupportedKind => "unsupported_kind",
            ContextRefusal::NothingToShare => "nothing_to_share",
            ContextRefusal::PaneGone => "pane_gone",
            ContextRefusal::Denied => "denied",
            ContextRefusal::NotText => "not_text",
            ContextRefusal::Unreadable => "unreadable",
            ContextRefusal::Busy => "busy",
            ContextRefusal::Destination => "destination",
            ContextRefusal::LocalOnly => "local_only",
        }
    }
}

impl From<ReadRefusal> for ContextRefusal {
    fn from(refusal: ReadRefusal) -> Self {
        match refusal {
            ReadRefusal::PaneGone => ContextRefusal::PaneGone,
            ReadRefusal::Unsupported => ContextRefusal::UnsupportedKind,
            ReadRefusal::Denied => ContextRefusal::Denied,
            ReadRefusal::NotText => ContextRefusal::NotText,
            ReadRefusal::Unreadable => ContextRefusal::Unreadable,
            ReadRefusal::Busy => ContextRefusal::Busy,
        }
    }
}

impl fmt::Display for ContextRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ContextRefusal::NotShared => "this pane is not shared with the assistant",
            ContextRefusal::StaleOffer => "this offer can no longer be sent",
            ContextRefusal::BadPick => "the picked items are not valid",
            ContextRefusal::BadKinds => "the kinds asked for are not valid",
            ContextRefusal::UnsupportedKind => "this pane does not offer that kind",
            ContextRefusal::NothingToShare => "there is nothing to share",
            ContextRefusal::PaneGone => "the pane is gone",
            ContextRefusal::Denied => "this place may not be shared",
            ContextRefusal::NotText => "this is not a text file",
            ContextRefusal::Unreadable => "the text could not be read",
            ContextRefusal::Busy => "the pane did not answer in time",
            ContextRefusal::Destination => "the destination cannot be used for shared content",
            ContextRefusal::LocalOnly => "local only is on and this destination leaves the device",
        })
    }
}

impl std::error::Error for ContextRefusal {}

// ---- the route ------------------------------------------------------------------------------------------------------

/// The route a share is bound to: the provider profile, the model and the scope of its base address. A call on another
/// route removes the share, so a different provider, model or address asks again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareKey {
    /// The profile id, or the id of a registered provider.
    pub profile: String,
    /// The model.
    pub model: String,
    /// The origin (scheme, host and port) of the base address, or a fixed word for a registered provider.
    pub scope: String,
}

/// The string an offer is bound to inside the gate: a JSON array of the chat, the pane and the route, so that no two
/// routes give one string.
pub(super) fn offer_scope(conversation: &str, pane: PaneId, key: &ShareKey) -> String {
    serde_json::to_string(&(conversation, pane, &key.profile, &key.model, &key.scope)).unwrap_or_default()
}

/// Whether the user must share a pane before its text is read for a destination of class `locality`. Only a local
/// destination with the "auto" setting skips the ask; a destination on the local network or the internet never does.
pub fn share_required(locality: Locality, auto: bool) -> bool {
    !(auto && locality.is_local())
}

// ---- the share book -------------------------------------------------------------------------------------------------

struct Share {
    conversation: String,
    pane: PaneId,
    key: ShareKey,
    /// The insertion counter: the oldest share goes first when the book is full.
    order: u64,
}

struct LiveOffer {
    conversation: String,
    pane: PaneId,
    key: ShareKey,
    id: OfferId,
}

#[derive(Default)]
struct Book {
    shares: Vec<Share>,
    offers: Vec<LiveOffer>,
    counter: u64,
}

/// Which pane is shared for which chat and route, and which offer each chat holds. Every method that removes something
/// returns the offer ids the caller must discard at the gate, so the gate is told outside this lock.
#[derive(Default)]
pub(super) struct ShareBook {
    state: Mutex<Book>,
}

impl ShareBook {
    fn lock(&self) -> MutexGuard<'_, Book> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `pane` is shared for the chat, on any route.
    pub(super) fn is_shared(&self, conversation: &str, pane: PaneId) -> bool {
        self.lock().shares.iter().any(|share| share.conversation == conversation && share.pane == pane)
    }

    /// Whether `pane` is shared for the chat on exactly this route.
    pub(super) fn has(&self, conversation: &str, pane: PaneId, key: &ShareKey) -> bool {
        self.lock()
            .shares
            .iter()
            .any(|share| share.conversation == conversation && share.pane == pane && share.key == *key)
    }

    /// Whether the chat has a share or a live offer.
    pub(super) fn holds_any(&self, conversation: &str) -> bool {
        let book = self.lock();
        book.shares.iter().any(|share| share.conversation == conversation)
            || book.offers.iter().any(|offer| offer.conversation == conversation)
    }

    /// Removes what the chat holds on a route other than `key`: its shares and its live offer.
    pub(super) fn reset_route(&self, conversation: &str, key: &ShareKey) -> Vec<OfferId> {
        let mut book = self.lock();
        book.shares.retain(|share| share.conversation != conversation || share.key == *key);
        take_offers(&mut book, |offer| offer.conversation == conversation && offer.key != *key)
    }

    /// Records the share of `pane` for the chat. A share of the same pane on another route is replaced. When the book
    /// is full the oldest share goes, with the offer made for its pane.
    pub(super) fn record(&self, conversation: &str, pane: PaneId, key: &ShareKey) -> Vec<OfferId> {
        let mut book = self.lock();
        let mut voided = Vec::new();
        if let Some(existing) =
            book.shares.iter().position(|share| share.conversation == conversation && share.pane == pane)
        {
            if book.shares[existing].key == *key {
                return voided;
            }
            book.shares.remove(existing);
            voided.extend(take_offers(&mut book, |offer| offer.conversation == conversation && offer.pane == pane));
        }
        book.counter += 1;
        let order = book.counter;
        book.shares.push(Share { conversation: conversation.to_owned(), pane, key: key.clone(), order });
        while book.shares.len() > MAX_SHARES {
            let Some(oldest) = book.shares.iter().enumerate().min_by_key(|(_, share)| share.order).map(|(at, _)| at)
            else {
                break;
            };
            let gone = book.shares.remove(oldest);
            voided.extend(take_offers(&mut book, |offer| {
                offer.conversation == gone.conversation && offer.pane == gone.pane
            }));
        }
        voided
    }

    /// Removes the share of `pane` for the chat and the live offer made for it.
    pub(super) fn unshare(&self, conversation: &str, pane: PaneId) -> Vec<OfferId> {
        let mut book = self.lock();
        book.shares.retain(|share| share.conversation != conversation || share.pane != pane);
        take_offers(&mut book, |offer| offer.conversation == conversation && offer.pane == pane)
    }

    /// Makes `id` the live offer of the chat, and returns the one it replaces. When `needs_share` is set the share of
    /// `pane` must still exist on this route: a share that ended while the text was read leaves no offer behind, and
    /// the answer is `Err(())`.
    pub(super) fn set_offer(
        &self,
        conversation: &str,
        pane: PaneId,
        key: &ShareKey,
        id: OfferId,
        needs_share: bool,
    ) -> Result<Option<OfferId>, ()> {
        let mut book = self.lock();
        if needs_share
            && !book
                .shares
                .iter()
                .any(|share| share.conversation == conversation && share.pane == pane && share.key == *key)
        {
            return Err(());
        }
        let old = take_offers(&mut book, |offer| offer.conversation == conversation).into_iter().next();
        book.offers.push(LiveOffer { conversation: conversation.to_owned(), pane, key: key.clone(), id });
        Ok(old)
    }

    /// Forgets the live offer `id` of the chat: it was spent.
    pub(super) fn forget_offer(&self, conversation: &str, id: &OfferId) {
        self.lock().offers.retain(|offer| offer.conversation != conversation || offer.id != *id);
    }

    /// The chat ended: every share and the live offer go.
    pub(super) fn chat_closed(&self, conversation: &str) -> Vec<OfferId> {
        let mut book = self.lock();
        book.shares.retain(|share| share.conversation != conversation);
        take_offers(&mut book, |offer| offer.conversation == conversation)
    }

    /// The pane ended: its shares go for every chat, with the offers made for it.
    pub(super) fn pane_closed(&self, pane: PaneId) -> Vec<OfferId> {
        let mut book = self.lock();
        book.shares.retain(|share| share.pane != pane);
        take_offers(&mut book, |offer| offer.pane == pane)
    }

    /// The profile was removed: the shares bound to it go, with their offers.
    pub(super) fn profile_removed(&self, profile: &str) -> Vec<OfferId> {
        let mut book = self.lock();
        book.shares.retain(|share| share.key.profile != profile);
        take_offers(&mut book, |offer| offer.key.profile == profile)
    }
}

/// Removes the live offers `gone` selects and returns their ids.
fn take_offers(book: &mut Book, gone: impl Fn(&LiveOffer) -> bool) -> Vec<OfferId> {
    let mut taken = Vec::new();
    let mut kept = Vec::with_capacity(book.offers.len());
    for offer in book.offers.drain(..) {
        if gone(&offer) {
            taken.push(offer.id);
        } else {
            kept.push(offer);
        }
    }
    book.offers = kept;
    taken
}

// ---- the views ------------------------------------------------------------------------------------------------------

/// What the card needs to know about a pane before it asks: where the text would go, whether the user must share first,
/// whether it is shared, and which kinds the pane offers. No text.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ContextState {
    /// The provider profile the answer is for.
    pub profile: String,
    /// The model it is for.
    pub model: String,
    /// The host the text would go to, as a label.
    pub host: String,
    /// Where that host is.
    pub locality: Locality,
    /// Whether a send needs the pane to be shared first.
    pub required: bool,
    /// Whether the pane is shared for this chat on this route.
    pub shared: bool,
    /// The kinds the pane can offer now, in the order the card lists them.
    pub kinds: Vec<ContextKind>,
    /// Whether the chat file keeps the text of a context (the user's setting).
    pub persist: bool,
}

impl fmt::Debug for ContextState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContextState")
            .field("profile", &self.profile)
            .field("model", &self.model)
            .field("locality", &self.locality)
            .field("required", &self.required)
            .field("shared", &self.shared)
            .field("kinds", &self.kinds.len())
            .field("persist", &self.persist)
            .finish()
    }
}

/// How often a rule of the cleaner fired in one item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FindingView {
    /// The stable name of the rule.
    pub rule: String,
    /// How many times it fired.
    pub count: u32,
}

/// One item of an offer: its number, its kind, its label, its size and what the cleaner removed. No text.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ItemView {
    /// The number the page sends back to pick the item.
    pub id: u32,
    /// What the item is.
    pub kind: ContextKind,
    /// The cleaned label.
    pub label: String,
    /// The size of the cleaned text in bytes.
    pub bytes: u64,
    /// What the cleaner removed or masked.
    pub findings: Vec<FindingView>,
}

impl fmt::Debug for ItemView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ItemView")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("label", &Size(self.label.len()))
            .field("bytes", &self.bytes)
            .field("findings", &self.findings.len())
            .finish()
    }
}

/// An offer as the page sees it: the id to send back, where it would go, and one view per item. It holds no text; the
/// only way item text becomes bytes that leave the device is the approval of the gate.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct OfferView {
    /// The id the page sends back with the picked item numbers.
    pub offer: String,
    /// The provider profile the offer is for.
    pub profile: String,
    /// The model.
    pub model: String,
    /// The host the text would go to.
    pub host: String,
    /// Where that host is.
    pub locality: Locality,
    /// How long the offer can still be sent, in seconds.
    pub ttl_seconds: u64,
    /// The items, numbered from 1.
    pub items: Vec<ItemView>,
    /// The kinds that were read and were empty.
    pub empty: Vec<ContextKind>,
    /// The size of all items in bytes.
    pub bytes: u64,
    /// Whether the chat file keeps the text of a context.
    pub persist: bool,
}

impl OfferView {
    /// The view of `offer` for the route it was made on. Reads labels, sizes and findings; never text.
    pub(super) fn of(
        offer: &ContextOffer,
        profile: &str,
        model: &str,
        ttl_seconds: u64,
        empty: Vec<ContextKind>,
        persist: bool,
    ) -> OfferView {
        let items = offer
            .items()
            .iter()
            .map(|item| ItemView {
                id: item.id(),
                kind: item.kind(),
                label: item.label().to_owned(),
                bytes: item.bytes(),
                findings: item
                    .findings()
                    .iter()
                    .map(|finding| FindingView { rule: finding.rule.clone(), count: finding.count })
                    .collect(),
            })
            .collect();
        OfferView {
            offer: offer.id().as_str().to_owned(),
            profile: profile.to_owned(),
            model: model.to_owned(),
            host: offer.destination().host.clone(),
            locality: offer.destination().locality,
            ttl_seconds,
            items,
            empty,
            bytes: offer.bytes(),
            persist,
        }
    }
}

impl fmt::Debug for OfferView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OfferView")
            .field("profile", &self.profile)
            .field("model", &self.model)
            .field("locality", &self.locality)
            .field("ttl_seconds", &self.ttl_seconds)
            .field("items", &self.items.len())
            .field("empty", &self.empty.len())
            .field("bytes", &self.bytes)
            .field("persist", &self.persist)
            .finish()
    }
}
