//! The AI chat core of holzWave.
//!
//! Nothing in this module opens a socket by itself: a connection exists only while a request that the user started is
//! in flight. The HTTP client that reaches a provider is built in [`http`] and nowhere else, so the project policy test
//! can name that one file as the only first-party code that knows the client crate.

pub mod http;
pub mod provider;
pub mod scripted;
pub mod types;
