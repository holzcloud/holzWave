//! The context gate: the only path from private text to a provider.
//!
//! Terminal content, files and tool output leave the device only after the user has seen exactly what would be sent and
//! has agreed to send it to one named provider. That rule is a type here, not a convention: an [`ApprovedContext`]
//! cannot be built outside this module, so a request can carry context only if the gate produced it.
//!
//! This file holds the types. The code that reads the terminal, redacts secrets and asks the user builds a
//! [`ContextOffer`] and, once the user agrees, turns it into an [`ApprovedContext`] with the private constructor of this
//! module. An offer is not an approval, so building one is harmless.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::types::{Consent, ContextKind, ContextNote, Locality, NoteItem, Size};

/// Names an offer, so an approval can refer to the offer the user saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OfferId(u64);

impl OfferId {
    /// An id with this number.
    pub fn new(number: u64) -> Self {
        Self(number)
    }

    /// The number.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// What the secret scan found in an item: the rule that matched and how many times.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// The name of the rule, for example the kind of key it recognises.
    pub rule: String,
    /// How many matches were masked.
    pub count: u32,
}

/// Where an offer would go.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Destination {
    /// The host name the provider is reached at, as a label for the user.
    pub host: String,
    /// Where that host is, as the HTTP policy resolved it.
    pub locality: Locality,
}

/// One piece of text offered to a provider.
///
/// The text is private and has no getter: a page-facing view of an offer is built from the label, the size and the
/// findings, so the text cannot be serialised by accident. `Debug` prints the same.
#[derive(Clone)]
pub struct OfferItem {
    id: u32,
    kind: ContextKind,
    label: String,
    text: String,
    findings: Vec<Finding>,
}

impl OfferItem {
    /// An item. `text` is what would be sent, after redaction.
    pub fn new(
        id: u32,
        kind: ContextKind,
        label: impl Into<String>,
        text: impl Into<String>,
        findings: Vec<Finding>,
    ) -> Self {
        Self { id, kind, label: label.into(), text: text.into(), findings }
    }

    /// The id of the item within its offer.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// What the item is.
    pub fn kind(&self) -> ContextKind {
        self.kind
    }

    /// The label the user sees.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The size of the text in bytes.
    pub fn bytes(&self) -> u64 {
        self.text.len() as u64
    }

    /// What the secret scan masked.
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }
}

impl fmt::Debug for OfferItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OfferItem")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("label", &self.label)
            .field("text", &Size(self.text.len()))
            .field("findings", &self.findings)
            .finish()
    }
}

/// What the user is asked to share: items, a provider and a destination. It carries no consent.
#[derive(Clone)]
pub struct ContextOffer {
    id: OfferId,
    provider: String,
    destination: Destination,
    items: Vec<OfferItem>,
}

impl ContextOffer {
    /// An offer of `items` to `provider` at `destination`.
    pub fn new(id: OfferId, provider: impl Into<String>, destination: Destination, items: Vec<OfferItem>) -> Self {
        Self { id, provider: provider.into(), destination, items }
    }

    /// The id of the offer.
    pub fn id(&self) -> OfferId {
        self.id
    }

    /// The provider the offer is for.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Where it would go.
    pub fn destination(&self) -> &Destination {
        &self.destination
    }

    /// The items.
    pub fn items(&self) -> &[OfferItem] {
        &self.items
    }

    /// The size of all items in bytes.
    pub fn bytes(&self) -> u64 {
        self.items.iter().map(OfferItem::bytes).sum()
    }
}

impl fmt::Debug for ContextOffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContextOffer")
            .field("id", &self.id)
            .field("provider", &self.provider)
            .field("destination", &self.destination)
            .field("items", &self.items)
            .finish()
    }
}

/// One item of an approved context, without its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedItem {
    /// What the item is.
    pub kind: ContextKind,
    /// The label the user saw.
    pub label: String,
    /// The size of the text that was approved.
    pub bytes: u64,
}

struct Approved {
    provider: String,
    locality: Locality,
    hash: [u8; 32],
    items: Vec<ApprovedItem>,
    envelope: String,
}

/// Terminal context the user approved for one provider: proof of consent that a request carries.
///
/// There is no way to make one outside this module. The fields are private, the constructor is private, and the type
/// has no `Default`, no `Deserialize`, no `Serialize` and no `From`. Cloning an existing value creates no new consent,
/// so a turn may reuse its context in every round.
///
/// The hash is the SHA-256 of the envelope bytes and is computed by the constructor, so it can never disagree with the
/// bytes it names. A message names its context by that hash, and an adapter sends the envelope of the approved context
/// with that hash and nothing else.
///
/// Each of these is a compile error from outside the crate. A struct literal:
///
/// ```compile_fail
/// use holzwave_core::ai::gate::ApprovedContext;
/// fn forge() -> ApprovedContext {
///     ApprovedContext { inner: todo!() }
/// }
/// ```
///
/// The constructor:
///
/// ```compile_fail
/// use holzwave_core::ai::gate::ApprovedContext;
/// use holzwave_core::ai::types::Locality;
/// fn forge() -> ApprovedContext {
///     ApprovedContext::mint("p".to_owned(), Locality::Cloud, Vec::new(), String::new())
/// }
/// ```
///
/// A default value:
///
/// ```compile_fail
/// use holzwave_core::ai::gate::ApprovedContext;
/// fn forge() -> ApprovedContext {
///     ApprovedContext::default()
/// }
/// ```
///
/// Reading one from data:
///
/// ```compile_fail
/// use holzwave_core::ai::gate::ApprovedContext;
/// fn forge(json: &str) -> ApprovedContext {
///     serde_json::from_str::<ApprovedContext>(json).unwrap()
/// }
/// ```
///
/// Writing one as data:
///
/// ```compile_fail
/// use holzwave_core::ai::gate::ApprovedContext;
/// fn leak(context: &ApprovedContext) -> String {
///     serde_json::to_string(context).unwrap()
/// }
/// ```
///
/// The examples above use the same imports as this one, which compiles: a value can be received and read, and a request
/// carries a list of them, so a typo cannot make the failing ones pass.
///
/// ```
/// use holzwave_core::ai::gate::ApprovedContext;
/// use holzwave_core::ai::provider::ChatRequest;
///
/// fn names_provider(context: &ApprovedContext) -> &str {
///     context.provider()
/// }
///
/// let mut request = ChatRequest::new("model");
/// request.context = Vec::<ApprovedContext>::new();
/// assert!(request.context.iter().map(names_provider).next().is_none());
/// ```
#[derive(Clone)]
pub struct ApprovedContext {
    inner: Arc<Approved>,
}

impl ApprovedContext {
    // The approve step of the gate is the only caller; until it exists only the tests call it.
    #[cfg_attr(not(test), allow(dead_code))]
    fn mint(provider: String, locality: Locality, items: Vec<ApprovedItem>, envelope: String) -> Self {
        let hash: [u8; 32] = Sha256::digest(envelope.as_bytes()).into();
        Self { inner: Arc::new(Approved { provider, locality, hash, items, envelope }) }
    }

    /// The provider the user approved this context for.
    pub fn provider(&self) -> &str {
        &self.inner.provider
    }

    /// Where that provider runs.
    pub fn locality(&self) -> Locality {
        self.inner.locality
    }

    /// The SHA-256 of the envelope bytes.
    pub fn hash(&self) -> &[u8; 32] {
        &self.inner.hash
    }

    /// The hash in lower-case hex: how a message names its context.
    pub fn hash_hex(&self) -> String {
        use std::fmt::Write;
        self.inner.hash.iter().fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
    }

    /// The items, described.
    pub fn items(&self) -> &[ApprovedItem] {
        &self.inner.items
    }

    /// The exact payload the user saw and approved.
    pub fn envelope(&self) -> &str {
        &self.inner.envelope
    }

    /// The size of the envelope in bytes.
    pub fn bytes(&self) -> usize {
        self.inner.envelope.len()
    }
}

impl fmt::Debug for ApprovedContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApprovedContext")
            .field("provider", &self.inner.provider)
            .field("locality", &self.inner.locality)
            .field("hash", &self.hash_hex())
            .field("items", &self.inner.items)
            .field("envelope", &Size(self.inner.envelope.len()))
            .finish()
    }
}

impl ContextNote {
    /// The record of `context` for a message: its hash, its items described, who it was approved for, and `stored` as
    /// given. `None` is the redacted marker and the default; `stored` is never sent to a provider.
    pub fn for_approved(context: &ApprovedContext, stored: Option<String>) -> Self {
        Self {
            hash: context.hash_hex(),
            items: context
                .items()
                .iter()
                .map(|item| NoteItem { kind: item.kind, label: item.label.clone(), bytes: item.bytes })
                .collect(),
            stored,
            consented_to: Consent { provider: context.provider().to_owned(), locality: context.locality() },
        }
    }
}

#[cfg(test)]
mod testing {
    use super::*;

    /// An approved context with one selection item, for the tests of this crate.
    pub(crate) fn approved(provider: &str, locality: Locality, label: &str, envelope: &str) -> ApprovedContext {
        let item = ApprovedItem { kind: ContextKind::Selection, label: label.to_owned(), bytes: envelope.len() as u64 };
        ApprovedContext::mint(provider.to_owned(), locality, vec![item], envelope.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::testing::approved;
    use super::*;
    use crate::ai::provider::{AiError, CancelToken, ChatRequest, Flow, Provider, StreamEvent};
    use crate::ai::scripted::{Script, ScriptedProvider};
    use crate::ai::types::{Message, Part, Role};

    const CANARY: &str = "CANARY-7f3a";
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn message_with_note(id: &str, context: &ApprovedContext) -> Message {
        Message::new(
            id,
            Role::User,
            vec![
                Part::Context(ContextNote::for_approved(context, None)),
                Part::Text { text: "explain this".to_owned() },
            ],
            1,
        )
    }

    fn request_with(context: &ApprovedContext, notes: usize) -> ChatRequest {
        let mut request = ChatRequest::new("model");
        request.context.push(context.clone());
        for n in 0..notes {
            request.messages.push(message_with_note(&format!("m{n}"), context));
        }
        request
    }

    fn is_policy(result: Result<(), AiError>) -> bool {
        matches!(result, Err(AiError::Policy(_)))
    }

    fn serve(provider: &ScriptedProvider, request: &ChatRequest) -> (Vec<StreamEvent>, Result<(), AiError>) {
        let mut events = Vec::new();
        let result = provider
            .stream(
                request,
                &mut |event| {
                    events.push(event);
                    Flow::Continue
                },
                &CancelToken::new(),
            )
            .map(|_| ());
        (events, result)
    }

    #[test]
    fn an_approved_context_reports_its_provider_items_and_envelope() {
        let context = approved("p", Locality::Cloud, "selection", "hello");

        assert_eq!(context.provider(), "p");
        assert_eq!(context.locality(), Locality::Cloud);
        assert_eq!(context.envelope(), "hello");
        assert_eq!(context.bytes(), 5);
        assert_eq!(
            context.items(),
            [ApprovedItem { kind: ContextKind::Selection, label: "selection".to_owned(), bytes: 5 }]
        );
        let clone = context.clone();
        assert_eq!(clone.hash(), context.hash(), "a clone is the same consent");
    }

    #[test]
    fn the_hash_is_the_sha256_of_the_envelope() {
        let context = approved("p", Locality::Loopback, "selection", "abc");

        assert_eq!(context.hash_hex(), ABC_SHA256);
        assert_eq!(context.hash_hex().len(), 64);
        assert_eq!(context.hash()[0], 0xba);
        assert_eq!(context.hash()[31], 0xad);
        assert_ne!(approved("p", Locality::Loopback, "selection", "abd").hash(), context.hash());
    }

    #[test]
    fn a_note_made_for_an_approved_context_records_hash_provider_and_locality() {
        let context = approved("p", Locality::Lan, "selection", "abc");

        let note = ContextNote::for_approved(&context, Some("kept".to_owned()));

        assert_eq!(note.hash, ABC_SHA256);
        assert_eq!(note.items, [NoteItem { kind: ContextKind::Selection, label: "selection".to_owned(), bytes: 3 }]);
        assert_eq!(note.consented_to, Consent { provider: "p".to_owned(), locality: Locality::Lan });
        assert_eq!(note.stored.as_deref(), Some("kept"));
        assert_eq!(ContextNote::for_approved(&context, None).stored, None);
    }

    #[test]
    fn a_request_finds_its_approved_context_by_hash() {
        let context = approved("p", Locality::Cloud, "selection", "abc");
        let request = request_with(&context, 1);

        assert_eq!(request.approved_for(ABC_SHA256).map(|found| found.provider()), Some("p"));
        assert!(request.approved_for(&"0".repeat(64)).is_none());
        assert!(ChatRequest::new("model").approved_for(ABC_SHA256).is_none());
    }

    #[test]
    fn a_context_for_another_provider_is_refused_before_any_event() {
        let foreign = approved("q", Locality::Cloud, "selection", "abc");
        let provider = ScriptedProvider::new("p").with_turn(Script::reply("never sent"));
        let request = request_with(&foreign, 1);

        assert!(is_policy(request.check_consent("p")));
        let (events, result) = serve(&provider, &request);

        assert!(events.is_empty(), "no event for a refused request");
        assert!(matches!(result, Err(AiError::Policy(_))), "{result:?}");
        assert!(provider.requests().is_empty(), "the request was not recorded");
        assert_eq!(provider.turns_left(), 1, "the script was not consumed");
    }

    #[test]
    fn a_context_for_this_provider_reaches_the_recorded_request() {
        let context = approved("p", Locality::Loopback, "selection", "abc");
        let provider = ScriptedProvider::new("p").with_turn(Script::reply("seen"));
        let mut request = request_with(&context, 1);
        // A note of an earlier turn that no approved context in this request has is history, and fine.
        request.messages.insert(0, message_with_note("old", &approved("p", Locality::Loopback, "screen", "older")));

        assert_eq!(request.check_consent("p"), Ok(()));
        let (events, result) = serve(&provider, &request);

        assert_eq!(result, Ok(()));
        assert_eq!(events, [StreamEvent::TextDelta("seen".to_owned())]);
        let recorded = provider.requests();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].context.len(), 1);
        assert_eq!(recorded[0].context[0].hash_hex(), ABC_SHA256);
    }

    #[test]
    fn an_approved_context_that_no_message_references_is_refused() {
        let context = approved("p", Locality::Cloud, "selection", "abc");
        let provider = ScriptedProvider::new("p").with_turn(Script::reply("never sent"));
        let mut request = request_with(&context, 0);
        request.messages.push(Message::user_text("u1", "no note here", 1));

        assert!(is_policy(request.check_consent("p")));
        let (events, result) = serve(&provider, &request);
        assert!(events.is_empty());
        assert!(matches!(result, Err(AiError::Policy(_))), "{result:?}");
        assert_eq!(provider.turns_left(), 1);
    }

    #[test]
    fn two_notes_for_one_approved_context_are_refused() {
        let context = approved("p", Locality::Cloud, "selection", "abc");

        assert!(is_policy(request_with(&context, 2).check_consent("p")));
        assert_eq!(request_with(&context, 1).check_consent("p"), Ok(()));

        // The same envelope approved twice is one hash named by two contexts, which is also refused.
        let mut doubled = request_with(&context, 1);
        doubled.context.push(context.clone());
        assert!(is_policy(doubled.check_consent("p")));
    }

    #[test]
    fn approved_context_and_offer_debug_show_sizes_and_labels_never_the_text() {
        let context = approved("p", Locality::Cloud, "selection", CANARY);
        let item = OfferItem::new(
            1,
            ContextKind::File,
            "notes.txt",
            CANARY,
            vec![Finding { rule: "api_key".to_owned(), count: 2 }],
        );
        let offer = ContextOffer::new(
            OfferId::new(7),
            "p",
            Destination { host: "api.example.org".to_owned(), locality: Locality::Cloud },
            vec![item.clone()],
        );
        let size = format!("<{} bytes>", CANARY.len());

        for shown in [
            format!("{context:?}"),
            format!("{context:#?}"),
            format!("{item:?}"),
            format!("{item:#?}"),
            format!("{offer:?}"),
            format!("{offer:#?}"),
        ] {
            assert!(!shown.contains(CANARY), "the text reached a Debug line: {shown}");
            assert!(shown.contains(&size), "no byte size in {shown}");
        }
        let shown = format!("{context:?}");
        assert!(shown.contains(&context.hash_hex()) && shown.contains("\"p\"") && shown.contains("selection"));
        assert!(format!("{item:?}").contains("notes.txt") && format!("{item:?}").contains("api_key"));
        assert_eq!(offer.bytes(), CANARY.len() as u64);
        assert_eq!(json!(offer.id()), json!(7));
    }
}
