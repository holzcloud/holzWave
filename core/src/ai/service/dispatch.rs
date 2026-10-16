//! Which adapter speaks to which wire.
//!
//! A provider that sends through the network is built in one way only: a profile is resolved ([`AiConfig::resolve`]),
//! the adapter of its wire is chosen here, and [`Resolved::into_provider`] puts them behind the HTTP chokepoint. The
//! service never builds a provider from a bare endpoint, so the key scope, the gateway acknowledgement and the request
//! options of a preset cannot be skipped.
//!
//! [`AiConfig::resolve`]: crate::ai::profiles::AiConfig::resolve
//! [`Resolved::into_provider`]: crate::ai::profiles::Resolved::into_provider

use crate::ai::http::Origin;
use crate::ai::wire::anthropic::AnthropicAdapter;
use crate::ai::wire::gemini::GeminiAdapter;
use crate::ai::wire::ollama::OllamaAdapter;
use crate::ai::wire::openai_chat::OpenAiChatAdapter;
use crate::ai::wire::openai_responses::OpenAiResponsesAdapter;
use crate::ai::wire::{Wire, WireAdapter};

/// The adapter of `wire`. The Anthropic adapter chooses its flavor from the origin: only the vendor's own host gets the
/// first-party request, every other address gets the core fields only. The Ollama adapter asks for its default window.
pub fn adapter_for(wire: Wire, origin: &Origin) -> Box<dyn WireAdapter> {
    match wire {
        Wire::OpenAiChat => Box::new(OpenAiChatAdapter::new()),
        Wire::OpenAiResponses => Box::new(OpenAiResponsesAdapter::new()),
        Wire::Anthropic => Box::new(AnthropicAdapter::for_origin(origin)),
        Wire::Gemini => Box::new(GeminiAdapter::new()),
        Wire::Ollama => Box::new(OllamaAdapter::new()),
    }
}
