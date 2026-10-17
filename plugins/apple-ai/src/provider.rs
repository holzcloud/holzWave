//! [`AppleProvider`]: the Apple on-device model as a [`Provider`] of the AI core.
//!
//! One turn runs in this order: the consent check, the availability gate, the turn lock, the prompt, one call to the
//! backend, the usage report. The provider chooses no other provider, retries nothing, caches nothing and keeps no
//! session between requests.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::Duration;

use holzwave_core::ai::provider::{AiError, CancelToken, ChatRequest, Completion, Detail, Flow, Provider, StreamEvent};
use holzwave_core::ai::types::{Capabilities, Locality, StopReason, Usage};

use crate::backend::{AppleError, Backend, GenerateRequest};
use crate::Availability;
use crate::prompt::{Estimator, FALLBACK_CONTEXT_TOKENS, render};

/// The id the provider has in the registry and the one a context is approved for.
pub const PROVIDER_ID: &str = "apple";

/// The one model: Apple's default on-device system model.
pub const MODEL_ID: &str = "system";

/// How often a turn that waits for the turn lock looks at its cancel token.
const LOCK_POLL: Duration = Duration::from_millis(25);

/// The Apple on-device model as a provider. Chat only: it declares no tools and refuses a request that does.
pub struct AppleProvider {
    backend: Arc<dyn Backend>,
    /// One request at a time: the framework serves a session one request at a time and a second call is an error.
    turn: Mutex<()>,
}

impl fmt::Debug for AppleProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppleProvider").field("id", &PROVIDER_ID).finish()
    }
}

impl AppleProvider {
    /// A provider over `backend`.
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { backend, turn: Mutex::new(()) }
    }

    /// A provider over the system model of this Mac.
    #[cfg(target_os = "macos")]
    pub fn system() -> Self {
        Self::new(Arc::new(crate::native::SystemBackend::new()))
    }

    /// Whether the model can be used now. Asked from the backend on every call, never cached.
    pub fn availability(&self) -> Availability {
        self.backend.availability()
    }

    /// Takes the turn lock, looking at `cancel` while it waits. A lock that a panicking turn poisoned is recovered: the
    /// lock guards no data.
    fn take_turn(&self, cancel: &CancelToken) -> Result<MutexGuard<'_, ()>, AiError> {
        loop {
            match self.turn.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) => {
                    if cancel.wait_cancelled(LOCK_POLL) {
                        return Err(AiError::Cancelled);
                    }
                }
            }
        }
    }
}

impl Provider for AppleProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn capabilities(&self, _model: &str) -> Capabilities {
        Capabilities {
            tools: false,
            reasoning: false,
            vision: false,
            context_tokens: Some(FALLBACK_CONTEXT_TOKENS),
            max_output_tokens: Some(1024),
            locality: Locality::OnDevice,
        }
    }

    fn stream(
        &self,
        req: &ChatRequest,
        out: &mut dyn FnMut(StreamEvent) -> Flow,
        cancel: &CancelToken,
    ) -> Result<Completion, AiError> {
        req.check_consent(self.id())?;
        if cancel.is_cancelled() {
            return Err(AiError::Cancelled);
        }
        if !req.tools.is_empty() {
            return Err(AiError::InvalidRequest(Detail::new("tools_not_supported")));
        }
        if req.model != MODEL_ID {
            return Err(AiError::InvalidRequest(Detail::new("unknown_model")));
        }
        let state = self.backend.availability();
        if state != Availability::Available {
            return Err(AiError::Unavailable(Detail::new(state.as_str())));
        }
        let _turn = self.take_turn(cancel)?;
        let rendered = render(req)?;

        let request = GenerateRequest {
            instructions: rendered.instructions.clone(),
            prompt: rendered.prompt.clone(),
            max_response_tokens: rendered.max_response_tokens,
        };
        let mut reply_tokens = Estimator::new();
        let mut sink = |delta: &str| {
            reply_tokens.push(delta);
            out(StreamEvent::TextDelta(delta.to_owned()))
        };
        // The backend is called at most once per turn: nothing here retries.
        match self.backend.generate(&request, &mut sink, cancel) {
            Err(AppleError::Cancelled) => return Err(AiError::Cancelled),
            Err(_) if cancel.is_cancelled() => return Err(AiError::Cancelled),
            Err(error) => return Err(error.to_ai_error()),
            Ok(()) => {}
        }
        let usage = Usage { input_tokens: rendered.input_tokens, output_tokens: reply_tokens.total() };
        if out(StreamEvent::Usage(usage)) == Flow::Stop {
            return Err(AiError::Cancelled);
        }
        Ok(Completion { stop_reason: StopReason::EndTurn, usage, native: None })
    }
}
