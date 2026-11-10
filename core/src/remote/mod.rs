//! The client side of the byte bridge to a holzWave server.
//!
//! [`ws`] is the WebSocket client: one synchronous connection that carries opaque text and binary messages, over TLS from the
//! operating system's trust store, with the device token in an `Authorization` header, bounded messages and frames, and a
//! read timeout that leaves the connection usable. The vocabulary of the sessions that run over it is not here.

pub mod ws;
