//! The wire adapters, one file per dialect, and their shared types.
//!
//! An adapter speaks one provider dialect and nothing else: it turns a [`ChatRequest`] into a path, headers and a JSON
//! body ([`WireAdapter::request`], a pure function with no I/O), and it turns the frames of the answer into
//! [`StreamEvent`]s ([`WireTurn`]). Everything that touches the network is the HTTP chokepoint's: [`HttpProvider`]
//! drives any adapter through it, checks consent first, and implements [`Provider`], so an adapter never opens a socket,
//! resolves a name, handles a key or decides where a request may go.
//!
//! - [`anthropic`], [`gemini`], [`ollama`], [`openai_chat`] and [`openai_responses`] are the dialects.
//! - [`Wire`] names them with tags that are written to profile files and to stored chats, so they never change.

pub mod anthropic;
pub mod gemini;
pub mod ollama;
pub mod openai_chat;
pub mod openai_responses;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

use super::http::{Call, Frame, Framing, Header, Http, NetworkPolicy, Outcome, Rejection, SecretHeader, Target, status_error};
use super::provider::{AiError, CancelToken, ChatRequest, Completion, Detail, Flow, Provider, StreamEvent};
use super::sse::StreamEnd;
use super::types::{Capabilities, Locality, Size};

/// The dialects a provider can speak.
///
/// The tags are stable: they are written to profile files and to the `wire` of the provider-native turn of a stored chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Wire {
    /// OpenAI Chat Completions, also spoken by many gateways and local servers.
    #[serde(rename = "openai_chat")]
    OpenAiChat,
    /// OpenAI Responses.
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    /// Anthropic Messages.
    #[serde(rename = "anthropic")]
    Anthropic,
    /// Gemini generateContent.
    #[serde(rename = "gemini")]
    Gemini,
    /// The Ollama native chat API.
    #[serde(rename = "ollama")]
    Ollama,
}

impl Wire {
    /// Every dialect, in the order of the tags.
    pub const ALL: [Wire; 5] = [Wire::OpenAiChat, Wire::OpenAiResponses, Wire::Anthropic, Wire::Gemini, Wire::Ollama];

    /// The stable tag.
    pub fn tag(self) -> &'static str {
        match self {
            Wire::OpenAiChat => "openai_chat",
            Wire::OpenAiResponses => "openai_responses",
            Wire::Anthropic => "anthropic",
            Wire::Gemini => "gemini",
            Wire::Ollama => "ollama",
        }
    }

    /// The dialect with this tag.
    pub fn from_tag(tag: &str) -> Option<Wire> {
        Wire::ALL.into_iter().find(|wire| wire.tag() == tag)
    }

    /// How the answer of this dialect is cut into frames: Ollama sends one JSON record per line, the rest send
    /// server-sent events.
    pub fn framing(self) -> Framing {
        match self {
            Wire::Ollama => Framing::Ndjson,
            _ => Framing::Sse,
        }
    }
}

/// Where a provider is and how it is reached: the dialect, the base address, the network policy, the headers it needs
/// and the key.
///
/// It is not `Clone`, because the key is not. `Debug` prints the dialect, the origin and counts.
pub struct Endpoint {
    wire: Wire,
    target: Target,
    policy: NetworkPolicy,
    headers: Vec<Header>,
    secrets: Vec<SecretHeader>,
}

impl Endpoint {
    /// An endpoint with no header and no key.
    pub fn new(wire: Wire, target: Target, policy: NetworkPolicy) -> Self {
        Self { wire, target, policy, headers: Vec::new(), secrets: Vec::new() }
    }

    /// Adds a header that is sent with every request.
    #[must_use]
    pub fn with_header(mut self, header: Header) -> Self {
        self.headers.push(header);
        self
    }

    /// Adds a key header. The key is sent to the origin it was filed for and nowhere else.
    #[must_use]
    pub fn with_secret(mut self, secret: SecretHeader) -> Self {
        self.secrets.push(secret);
        self
    }

    /// The dialect.
    pub fn wire(&self) -> Wire {
        self.wire
    }

    /// The base address.
    pub fn target(&self) -> &Target {
        &self.target
    }

    /// The network policy.
    pub fn policy(&self) -> NetworkPolicy {
        self.policy
    }

    /// Text from the provider that is going to be shown, with every key of this endpoint masked and the rest scrubbed
    /// and bounded by [`Detail::new`]. An adapter maps the error body of a provider through this.
    pub fn scrub(&self, text: &str) -> Detail {
        let masked = self.secrets.iter().fold(text.to_owned(), |masked, secret| secret.redact_in(&masked));
        Detail::new(&masked)
    }
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint")
            .field("wire", &self.wire)
            .field("origin", self.target.origin())
            .field("headers", &self.headers.len())
            .field("secrets", &self.secrets.len())
            .finish()
    }
}

/// What an adapter asks the chokepoint to send: a path below the base address, extra headers and the JSON body.
pub struct WireRequest {
    /// The path, starting with `/`, appended to the base path of the endpoint. It may carry a query. It never carries a
    /// key.
    pub path: String,
    /// Headers of this dialect that are not secret, for example `anthropic-version`.
    pub headers: Vec<Header>,
    /// The JSON body.
    pub body: Vec<u8>,
}

impl fmt::Debug for WireRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WireRequest")
            .field("path", &Size(self.path.len()))
            .field("headers", &self.headers.len())
            .field("body", &Size(self.body.len()))
            .finish()
    }
}

/// One provider dialect.
pub trait WireAdapter: Send + Sync {
    /// The dialect this adapter speaks.
    fn wire(&self) -> Wire;

    /// What `model` can do at a destination of class `locality`.
    fn capabilities(&self, model: &str, locality: Locality) -> Capabilities;

    /// The request for `req`. A pure function of its arguments: no I/O, no clock, no randomness. A [`Part::Context`]
    /// is rendered by the rule of the provider contract (the approved envelope, or one marker line for history).
    ///
    /// [`Part::Context`]: super::types::Part::Context
    fn request(&self, req: &ChatRequest, caps: &Capabilities) -> Result<WireRequest, AiError>;

    /// A fresh reader for the answer of one turn.
    fn turn(&self) -> Box<dyn WireTurn>;

    /// The error of an answer that was not a success, when the adapter can read the provider's own error body. Text
    /// from the body goes through [`Endpoint::scrub`]. `None` falls back to the status: [`status_error`].
    fn rejection(&self, _rejection: &Rejection, _endpoint: &Endpoint) -> Option<AiError> {
        None
    }
}

/// The reader of one answer.
pub trait WireTurn: Send {
    /// Takes one frame and reports what it says through `out`. The answer of `out` is passed on: [`Flow::Stop`] ends the
    /// turn. An `Err` ends it with that error.
    fn frame(&mut self, frame: Frame<'_>, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError>;

    /// Ends the turn. The adapter combines `end` with the terminal event of its dialect: only a clean end after the
    /// terminal event is a completed turn. Tool calls are emitted here, from `ToolAssembler::finish`, and nowhere else.
    fn finish(
        self: Box<Self>,
        end: StreamEnd,
        out: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<Completion, AiError>;
}

/// No class known yet.
const LOCALITY_UNKNOWN: u8 = 0;

fn encode(locality: Locality) -> u8 {
    match locality {
        Locality::Loopback => 1,
        Locality::Lan => 2,
        Locality::Cloud => 3,
        Locality::OnDevice => 4,
    }
}

fn decode(code: u8) -> Option<Locality> {
    match code {
        1 => Some(Locality::Loopback),
        2 => Some(Locality::Lan),
        3 => Some(Locality::Cloud),
        4 => Some(Locality::OnDevice),
        _ => None,
    }
}

/// A [`Provider`] that speaks one dialect through the HTTP chokepoint.
///
/// `stream` checks consent as its first statement, then asks the adapter for the request and sends it. Constructing a
/// provider opens no socket and starts no thread.
pub struct HttpProvider {
    id: String,
    endpoint: Endpoint,
    adapter: Box<dyn WireAdapter>,
    http: Arc<Http>,
    locality: AtomicU8,
}

impl HttpProvider {
    /// A provider with the id a context is approved for.
    pub fn new(id: impl Into<String>, endpoint: Endpoint, adapter: Box<dyn WireAdapter>, http: Arc<Http>) -> Self {
        Self { id: id.into(), endpoint, adapter, http, locality: AtomicU8::new(LOCALITY_UNKNOWN) }
    }

    /// The endpoint.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Where the provider is: the class of the addresses of the last request, else the class of an IP-literal address,
    /// else the cloud. The cloud is the answer that asks for the most care.
    pub fn locality(&self) -> Locality {
        decode(self.locality.load(Ordering::Relaxed))
            .or_else(|| self.endpoint.target.literal_locality())
            .unwrap_or(Locality::Cloud)
    }

    /// Resolves the host and returns its class, sending nothing. This performs a name lookup, so it is for an explicit
    /// user action only (the consent offer that names the destination).
    pub fn classify(&self, cancel: &CancelToken) -> Result<Locality, AiError> {
        let resolved = self.http.classify(&self.endpoint.target, &self.endpoint.policy, cancel)?;
        self.locality.store(encode(resolved.locality), Ordering::Relaxed);
        Ok(resolved.locality)
    }
}

impl fmt::Debug for HttpProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpProvider")
            .field("id", &self.id)
            .field("endpoint", &self.endpoint)
            .field("locality", &self.locality())
            .finish()
    }
}

impl Provider for HttpProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self, model: &str) -> Capabilities {
        self.adapter.capabilities(model, self.locality())
    }

    fn stream(
        &self,
        req: &ChatRequest,
        out: &mut dyn FnMut(StreamEvent) -> Flow,
        cancel: &CancelToken,
    ) -> Result<Completion, AiError> {
        req.check_consent(&self.id)?;
        if cancel.is_cancelled() {
            return Err(AiError::Cancelled);
        }
        let caps = self.adapter.capabilities(&req.model, self.locality());
        let wire_request = self.adapter.request(req, &caps)?;
        let mut call = Call::new(
            &self.endpoint.target,
            self.endpoint.policy,
            &wire_request.path,
            &wire_request.body,
            self.endpoint.wire.framing(),
        );
        for header in self.endpoint.headers.iter().chain(&wire_request.headers) {
            call = call.header(header);
        }
        for secret in &self.endpoint.secrets {
            call = call.secret(secret);
        }
        let mut turn = self.adapter.turn();
        let mut failure: Option<AiError> = None;
        let exchange = {
            let mut on_frame = |frame: Frame<'_>| match turn.frame(frame, out) {
                Ok(flow) => flow,
                Err(error) => {
                    failure = Some(error);
                    Flow::Stop
                }
            };
            self.http.post_stream(&call, &mut on_frame, cancel)
        };
        // The error that made the turn stop is the answer, not the `Cancelled` that stopping produces.
        if let Some(error) = failure {
            return Err(error);
        }
        let exchange = exchange?;
        self.locality.store(encode(exchange.locality), Ordering::Relaxed);
        match exchange.outcome {
            Outcome::Rejected(rejection) => {
                Err(self.adapter.rejection(&rejection, &self.endpoint).unwrap_or_else(|| status_error(&rejection)))
            }
            Outcome::Streamed(end) => turn.finish(end, out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::gate::{Destination, Gate, OfferRequest, RequestItem};
    use crate::ai::provider::ErrorClass;
    use crate::ai::redact::SourceText;
    use crate::ai::types::{ContextKind, ContextNote, Message, Part, Role};

    /// An adapter that is never asked to do anything: the test fails if the chokepoint is reached.
    struct Idle;

    struct IdleTurn;

    impl WireTurn for IdleTurn {
        fn frame(&mut self, _: Frame<'_>, _: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
            Ok(Flow::Continue)
        }

        fn finish(
            self: Box<Self>,
            _: StreamEnd,
            _: &mut dyn FnMut(StreamEvent) -> Flow,
        ) -> Result<Completion, AiError> {
            Err(AiError::Protocol(Detail::new("never reached")))
        }
    }

    impl WireAdapter for Idle {
        fn wire(&self) -> Wire {
            Wire::OpenAiChat
        }

        fn capabilities(&self, _: &str, locality: Locality) -> Capabilities {
            Capabilities::conservative(locality)
        }

        fn request(&self, _: &ChatRequest, _: &Capabilities) -> Result<WireRequest, AiError> {
            Ok(WireRequest { path: "/v1/lines".to_owned(), headers: Vec::new(), body: b"{}".to_vec() })
        }

        fn turn(&self) -> Box<dyn WireTurn> {
            Box::new(IdleTurn)
        }
    }

    #[test]
    fn consent_is_checked_before_any_connection_is_attempted() {
        // The base address is a port nobody listens on, so a missing consent check would show up as a network error.
        let target = Target::parse("http://127.0.0.1:9", false).expect("a loopback address");
        let endpoint = Endpoint::new(Wire::OpenAiChat, target, NetworkPolicy::new(false, false));
        let http = Arc::new(Http::new(super::super::http::HttpConfig::standard()));
        let provider = HttpProvider::new("mine", endpoint, Box::new(Idle), http.clone());

        let gate = Gate::new();
        let destination = Destination { host: "localhost".to_owned(), locality: Locality::Loopback };
        let item = RequestItem::new(ContextKind::Screen, "pane", SourceText::plain("hello"));
        let offer = gate.offer(OfferRequest::new("other", destination, vec![item])).expect("an offer");
        let context = gate.approve(offer.id(), &[1], "other").expect("an approval for another provider");
        let mut request = ChatRequest::new("model");
        request.messages.push(Message::new(
            "u1",
            Role::User,
            vec![Part::Context(ContextNote::for_approved(&context, None)), Part::Text { text: "hi".to_owned() }],
            1,
        ));
        request.context.push(context);

        let result = provider.stream(&request, &mut |_| Flow::Continue, &CancelToken::new());

        let error = result.expect_err("a foreign context is refused");
        assert_eq!(error.class(), ErrorClass::Policy, "{error:?}");
        assert_eq!(http.attempts(), 0, "nothing was sent");
    }
}
