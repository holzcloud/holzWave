//! The client side of the byte bridge to a holzWave server.
//!
//! [`ws`] is the WebSocket client: one synchronous connection that carries opaque text and binary messages, over TLS from the
//! operating system's trust store, with the device token in an `Authorization` header, bounded messages and frames, and a
//! read timeout that leaves the connection usable. [`proto`] is the vocabulary that runs over it: the requests, the answers,
//! the events and the binary frames of the sessions, shared by the server and the client. [`hub`] is the one socket of a device
//! and what rides on it (requests, subscriptions, beat, reconnect); [`link`] is the link a pane runs on to show a session.

pub mod hub;
pub mod link;
pub mod proto;
pub mod ws;
