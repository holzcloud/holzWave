//! The seam between the provider and the model: the [`Backend`] trait, what a call asks for, and the errors a call can
//! end with.
//!
//! The provider talks to the model only through [`Backend`], so a test injects availability, the window size, a script
//! of deltas and every error on any host. The system backend (macOS only) and the scripted one implement it.

use std::fmt;

use holzwave_core::ai::provider::{AiError, CancelToken, Detail, Flow};

use crate::Availability;

/// The longest text, in bytes, that goes into one call: the instructions and the prompt each. A longer one is refused
/// as [`AppleError::ContextExceeded`] before any foreign call. The same number bounds one reply.
pub const MAX_INPUT_BYTES: usize = 65_536;

/// What one call to the model asks for.
///
/// `Debug` prints sizes and the token limit, never the text: the instructions and the prompt hold what the user
/// approved.
#[derive(Clone)]
pub struct GenerateRequest {
    /// The system text. Empty for none.
    pub instructions: String,
    /// The prompt: the conversation folded into one text, ending with the current message.
    pub prompt: String,
    /// The most tokens the reply may have.
    pub max_response_tokens: u32,
}

impl fmt::Debug for GenerateRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GenerateRequest")
            .field("instructions", &format_args!("<{} bytes>", self.instructions.len()))
            .field("prompt", &format_args!("<{} bytes>", self.prompt.len()))
            .field("max_response_tokens", &self.max_response_tokens)
            .finish()
    }
}

/// How a call to the model ended when it did not end well.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppleError {
    /// The caller stopped the call, or the consumer answered stop.
    Cancelled,
    /// The request does not fit the window of the model.
    ContextExceeded,
    /// The safety controls of the model refused the request or the reply.
    Guardrail,
    /// The model declined to answer.
    Refusal,
    /// The system rate limit was reached.
    RateLimited,
    /// The model already serves another request.
    Concurrent,
    /// The model files are not on the device (yet).
    AssetsUnavailable,
    /// The language or locale of the request is not supported.
    UnsupportedLanguage,
    /// The call took too long.
    Timeout,
    /// The model is not available.
    Unavailable,
    /// Any other case, including cases a later system adds.
    Other,
    /// The stream rewrote text that was already delivered.
    Inconsistent,
    /// The reply is larger than the bound.
    ReplyTooLarge,
}

/// The longest case label the stream ABI carries, in letters.
const MAX_LABEL: usize = 48;

impl AppleError {
    /// Every kind, in a fixed order.
    pub const ALL: [AppleError; 13] = [
        AppleError::Cancelled,
        AppleError::ContextExceeded,
        AppleError::Guardrail,
        AppleError::Refusal,
        AppleError::RateLimited,
        AppleError::Concurrent,
        AppleError::AssetsUnavailable,
        AppleError::UnsupportedLanguage,
        AppleError::Timeout,
        AppleError::Unavailable,
        AppleError::Other,
        AppleError::Inconsistent,
        AppleError::ReplyTooLarge,
    ];

    /// Maps the case label that the Swift side sends. The label is checked first (1 to 48 ASCII letters); the eleven
    /// documented labels match exactly and case-sensitively, and everything else is [`AppleError::Other`]. The label
    /// only chooses a class and is never shown.
    pub fn from_label(label: &str) -> AppleError {
        if label.is_empty() || label.len() > MAX_LABEL || !label.bytes().all(|b| b.is_ascii_alphabetic()) {
            return AppleError::Other;
        }
        match label {
            "cancelled" => AppleError::Cancelled,
            "unavailable" => AppleError::Unavailable,
            "exceededContextWindowSize" | "contextSizeExceeded" => AppleError::ContextExceeded,
            "guardrailViolation" => AppleError::Guardrail,
            "refusal" => AppleError::Refusal,
            "rateLimited" => AppleError::RateLimited,
            "concurrentRequests" => AppleError::Concurrent,
            "assetsUnavailable" => AppleError::AssetsUnavailable,
            "unsupportedLanguageOrLocale" => AppleError::UnsupportedLanguage,
            "timeout" => AppleError::Timeout,
            _ => AppleError::Other,
        }
    }

    /// A stable snake_case key. It is the only text an error of this crate carries.
    pub fn as_str(self) -> &'static str {
        match self {
            AppleError::Cancelled => "cancelled",
            AppleError::ContextExceeded => "context_exceeded",
            AppleError::Guardrail => "guardrail",
            AppleError::Refusal => "refusal",
            AppleError::RateLimited => "rate_limited",
            AppleError::Concurrent => "concurrent_requests",
            AppleError::AssetsUnavailable => "assets_unavailable",
            AppleError::UnsupportedLanguage => "unsupported_language",
            AppleError::Timeout => "timeout",
            AppleError::Unavailable => "unavailable",
            AppleError::Other => "other",
            AppleError::Inconsistent => "inconsistent_stream",
            AppleError::ReplyTooLarge => "reply_too_large",
        }
    }

    /// The error of the AI core for this kind. Context overflow, the safety controls and the rate limit are first-class
    /// errors, and none of them is retried by anything in this crate. The only text is the key of the kind.
    pub fn to_ai_error(self) -> AiError {
        let key = || Detail::new(self.as_str());
        match self {
            AppleError::Cancelled => AiError::Cancelled,
            AppleError::ContextExceeded => AiError::ContextLength,
            AppleError::Guardrail | AppleError::Refusal => AiError::ContentFilter,
            AppleError::RateLimited => AiError::RateLimited { retry_after: None },
            AppleError::Concurrent => AiError::Overloaded,
            AppleError::AssetsUnavailable | AppleError::Unavailable => AiError::Unavailable(key()),
            AppleError::UnsupportedLanguage => AiError::InvalidRequest(key()),
            AppleError::Timeout => AiError::Timeout,
            AppleError::Other | AppleError::Inconsistent | AppleError::ReplyTooLarge => AiError::Protocol(key()),
        }
    }
}

/// The model, as the provider sees it.
///
/// Implementations are `Send + Sync` because the provider is shared. `generate` blocks, never retries, and returns
/// `Err(AppleError::Cancelled)` as soon as `cancel` is set or the sink answers [`Flow::Stop`].
pub trait Backend: Send + Sync {
    /// Whether the model can be used right now. Asked on every turn and never cached.
    fn availability(&self) -> Availability;

    /// The context window of the model in tokens, when it can be read.
    fn context_size(&self) -> Option<u32> {
        None
    }

    /// The exact number of tokens of `text`, when the model can count.
    fn count_tokens(&self, _text: &str) -> Option<u32> {
        None
    }

    /// Runs one request and delivers the reply as deltas, in order.
    fn generate(
        &self,
        req: &GenerateRequest,
        sink: &mut dyn FnMut(&str) -> Flow,
        cancel: &CancelToken,
    ) -> Result<(), AppleError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_map_to_the_documented_kinds() {
        let table = [
            ("cancelled", AppleError::Cancelled),
            ("unavailable", AppleError::Unavailable),
            ("exceededContextWindowSize", AppleError::ContextExceeded),
            ("contextSizeExceeded", AppleError::ContextExceeded),
            ("guardrailViolation", AppleError::Guardrail),
            ("refusal", AppleError::Refusal),
            ("rateLimited", AppleError::RateLimited),
            ("concurrentRequests", AppleError::Concurrent),
            ("assetsUnavailable", AppleError::AssetsUnavailable),
            ("unsupportedLanguageOrLocale", AppleError::UnsupportedLanguage),
            ("timeout", AppleError::Timeout),
        ];
        for (label, kind) in table {
            assert_eq!(AppleError::from_label(label), kind, "{label}");
        }
    }

    #[test]
    fn an_unknown_or_hostile_label_is_other() {
        let long = "a".repeat(MAX_LABEL + 1);
        let hostile = [
            "",
            "decodingFailure",
            "Cancelled",
            "GUARDRAILVIOLATION",
            "guardrail violation",
            "guardrailViolation ",
            "guardrail1",
            "guardrail/Violation",
            "guardrail\u{e9}",
            "\u{202e}refusal",
            long.as_str(),
        ];
        for label in hostile {
            assert_eq!(AppleError::from_label(label), AppleError::Other, "{label:?}");
        }
        // The longest allowed label is still only matched by the table.
        assert_eq!(AppleError::from_label(&"a".repeat(MAX_LABEL)), AppleError::Other);
    }

    #[test]
    fn every_kind_maps_to_one_ai_error_class() {
        use holzwave_core::ai::ErrorClass;
        let expected = [
            (AppleError::Cancelled, ErrorClass::Cancelled),
            (AppleError::ContextExceeded, ErrorClass::ContextLength),
            (AppleError::Guardrail, ErrorClass::ContentFilter),
            (AppleError::Refusal, ErrorClass::ContentFilter),
            (AppleError::RateLimited, ErrorClass::RateLimit),
            (AppleError::Concurrent, ErrorClass::Overload),
            (AppleError::AssetsUnavailable, ErrorClass::Unavailable),
            (AppleError::UnsupportedLanguage, ErrorClass::InvalidRequest),
            (AppleError::Timeout, ErrorClass::Timeout),
            (AppleError::Unavailable, ErrorClass::Unavailable),
            (AppleError::Other, ErrorClass::Protocol),
            (AppleError::Inconsistent, ErrorClass::Protocol),
            (AppleError::ReplyTooLarge, ErrorClass::Protocol),
        ];
        assert_eq!(expected.len(), AppleError::ALL.len());
        let mut keys = std::collections::BTreeSet::new();
        for (kind, class) in expected {
            let error = kind.to_ai_error();
            assert_eq!(error.class(), class, "{kind:?}");
            assert!(keys.insert(kind.as_str()), "the key {} is not unique", kind.as_str());
            // The only text an error holds is the key of its kind.
            let text = error.to_string();
            let detailed = matches!(
                kind,
                AppleError::AssetsUnavailable
                    | AppleError::Unavailable
                    | AppleError::UnsupportedLanguage
                    | AppleError::Other
                    | AppleError::Inconsistent
                    | AppleError::ReplyTooLarge
            );
            if detailed {
                assert!(text.ends_with(kind.as_str()), "{kind:?}: {text}");
            }
        }
        assert_eq!(AiError::RateLimited { retry_after: None }, AppleError::RateLimited.to_ai_error());
    }

    #[test]
    fn generate_request_debug_shows_sizes_never_text() {
        let request = GenerateRequest {
            instructions: "CANARY-instructions-91c2".to_owned(),
            prompt: "CANARY-prompt-77aa".to_owned(),
            max_response_tokens: 512,
        };
        for shown in [format!("{request:?}"), format!("{request:#?}")] {
            assert!(!shown.contains("CANARY"), "{shown}");
            assert!(shown.contains("<24 bytes>") && shown.contains("<18 bytes>") && shown.contains("512"), "{shown}");
        }
    }
}
