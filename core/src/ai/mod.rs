//! The AI chat core of holzWave.
//!
//! Nothing in this module opens a socket by itself: a connection exists only while a request that the user started is
//! in flight. The HTTP client that reaches a provider is built in [`http`] and nowhere else, so the project policy test
//! can name that one file as the only first-party code that knows the client crate.
//!
//! - [`types`]: the message model, capabilities, the provider-native turn and the API key type.
//! - [`provider`]: the blocking [`Provider`] trait, its events, the [`CancelToken`] and the errors.
//! - [`scripted`]: a provider that plays a script, the test double of every AI test.
//! - [`gate`]: the context gate; an [`ApprovedContext`] can only be made there.
//! - [`http`]: the one place the HTTP client is built.
//! - [`agent`]: the agent loop that drives a turn.
//! - [`profiles`]: provider profiles, presets and key accounts.
//! - [`redact`]: secret redaction and the sanitiser for text that leaves the device.
//! - [`service`]: the facade the app shell holds.
//! - [`sse`]: the bounded server-sent-events decoder.
//! - [`store`]: conversation persistence.
//! - [`tools`]: tool definitions, the approval state machine and the deny list.
//! - [`wire`]: the wire adapters, one file per dialect.

pub mod agent;
pub mod gate;
pub mod http;
pub mod profiles;
pub mod provider;
pub mod redact;
pub mod scripted;
pub mod service;
pub mod sse;
pub mod store;
pub mod tools;
pub mod types;
pub mod wire;

pub use gate::{ApprovedContext, ContextOffer};
pub use provider::{AiError, CancelToken, ChatRequest, Collector, Completion, ErrorClass, Flow, Provider, StreamEvent};
pub use scripted::{Script, ScriptedProvider, Step};
pub use types::{
    ApiKey, Capabilities, ContextKind, Locality, Message, Opaque, Part, Role, StopReason, ToolCall, ToolResult, ToolSpec,
    Usage,
};
