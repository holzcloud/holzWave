//! Which adapter speaks to which wire.
//!
//! A provider that sends through the network is built in one way only: a profile is resolved ([`AiConfig::resolve`]),
//! the adapter of its wire is chosen here, and [`Resolved::into_provider`] puts them behind the HTTP chokepoint. The
//! service never builds a provider from a bare endpoint, so the key scope, the gateway acknowledgement and the request
//! options of a preset cannot be skipped.
//!
//! A provider that does not come from a profile (the scripted double of the tests, the on-device model of the shell) is
//! registered by id in the [`Registry`] and is used as it is, before any profile of that id.
//!
//! [`AiConfig::resolve`]: crate::ai::profiles::AiConfig::resolve
//! [`Resolved::into_provider`]: crate::ai::profiles::Resolved::into_provider

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use crate::ai::http::Origin;
use crate::ai::provider::Provider;
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

/// The providers that were registered by id.
#[derive(Default)]
pub struct Registry {
    entries: Mutex<Vec<(String, Arc<dyn Provider>)>>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Registers a provider. Returns false, and keeps the one that is there, when the id is taken.
    pub fn register(&self, id: &str, provider: Arc<dyn Provider>) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.iter().any(|(known, _)| known == id) {
            return false;
        }
        entries.push((id.to_owned(), provider));
        true
    }

    /// The provider registered under `id`.
    pub fn get(&self, id: &str) -> Option<Arc<dyn Provider>> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.iter().find(|(known, _)| known == id).map(|(_, provider)| Arc::clone(provider))
    }

    /// Whether a provider is registered under `id`.
    pub fn contains(&self, id: &str) -> bool {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.iter().any(|(known, _)| known == id)
    }

    /// How many providers are registered.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Whether no provider is registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry").field("providers", &self.len()).finish()
    }
}
