//! Helpers shared by the test files of the crate. Every test file declares `mod common;` and uses what it needs.
#![allow(dead_code)]

use holzwave_core::ai::gate::{ApprovedContext, Destination, Gate, OfferRequest, RequestItem};
use holzwave_core::ai::provider::{AiError, CancelToken, ChatRequest, Collector, Completion, Provider};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::types::{ContextKind, ContextNote, Locality, Message, Part, Role};

/// The id and model of the provider under test.
pub const PROVIDER: &str = holzwave_apple_ai::PROVIDER_ID;
pub const MODEL: &str = holzwave_apple_ai::MODEL_ID;

/// A context approved for `provider`, obtained the way the consent flow obtains it: an offer through the public gate and
/// an approval of every item.
pub fn approved_for_provider(provider: &str, text: &str) -> ApprovedContext {
    let gate = Gate::new();
    let destination = Destination { host: "on-device".to_owned(), locality: Locality::OnDevice };
    let item = RequestItem::new(ContextKind::Selection, "selection", SourceText::plain(text));
    let offer = gate.offer(OfferRequest::new(provider, destination, vec![item])).expect("the gate makes an offer");
    let picked: Vec<u32> = offer.items().iter().map(|item| item.id()).collect();
    gate.approve(offer.id(), &picked, provider).expect("the gate approves every item")
}

/// A context approved for the Apple provider.
pub fn approved(text: &str) -> ApprovedContext {
    approved_for_provider(PROVIDER, text)
}

/// A request for the system model with a system text and one user message.
pub fn user_request(system: &str, text: &str) -> ChatRequest {
    let mut req = ChatRequest::new(MODEL);
    req.system = system.to_owned();
    req.messages.push(Message::user_text("m-last", text, 1));
    req
}

/// Adds `context` to the request and a note that names it before the text of the last user message.
pub fn with_context(mut req: ChatRequest, context: ApprovedContext) -> ChatRequest {
    let note = Part::Context(ContextNote::for_approved(&context, None));
    let last = req.messages.last_mut().expect("the request has a message");
    last.parts.insert(0, note);
    req.context.push(context);
    req
}

/// Puts earlier turns before the last message: `(role, text)` oldest first.
pub fn history(mut req: ChatRequest, turns: &[(Role, &str)]) -> ChatRequest {
    let last = req.messages.pop().expect("the request has a message");
    for (index, (role, text)) in turns.iter().enumerate() {
        req.messages.push(Message::new(format!("m-{index}"), *role, vec![Part::Text { text: (*text).to_owned() }], 0));
    }
    req.messages.push(last);
    req
}

/// Drives `stream` through the trait object into a collector.
pub fn run(provider: &dyn Provider, req: &ChatRequest, cancel: &CancelToken) -> (Result<Completion, AiError>, Collector) {
    let mut collector = Collector::new();
    let result = provider.stream(req, &mut |event| collector.push(event), cancel);
    (result, collector)
}
