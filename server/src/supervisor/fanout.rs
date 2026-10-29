//! Delivery of one worker's output to every browser tab attached to it.
//!
//! A worker's answer goes to the socket that asked, in the text the page reads, and its events go to every socket of the
//! person or to the one the worker names. The bodies are the worker's own JSON text and are put into the wrapper without
//! being read.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::supervisor::socket::{Outbound, SocketCore};
use crate::wire::{Evt, Res, ResBody};

/// What the supervisor counts, shared by all workers.
#[derive(Default)]
pub(super) struct Counters {
    pub(super) dropped_unknown_socket: AtomicU64,
}

impl Counters {
    pub(super) fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// The text of an event for the page: `{"event":"<name>","payload":<text>}`.
pub(super) fn event_text(name: &str, payload: &str) -> String {
    let mut out = String::with_capacity(name.len() + payload.len() + 24);
    out.push_str("{\"event\":\"");
    out.push_str(name);
    out.push_str("\",\"payload\":");
    out.push_str(payload);
    out.push('}');
    out
}

/// What the page gets for an answer: text for a result or an error, bytes for a binary result.
pub(super) fn answer(request: u64, body: ResBody) -> Outbound {
    match body {
        ResBody::Ok(text) => Outbound::Text(wrapped(request, "ok", &text)),
        ResBody::Err(text) => Outbound::Text(wrapped(request, "err", &text)),
        ResBody::Binary(bytes) => {
            let mut out = Vec::with_capacity(8 + bytes.len());
            out.extend_from_slice(&request.to_be_bytes());
            out.extend_from_slice(&bytes);
            Outbound::Binary(out)
        }
    }
}

fn wrapped(request: u64, member: &str, body: &str) -> String {
    let mut out = String::with_capacity(body.len() + member.len() + 32);
    out.push_str("{\"id\":");
    out.push_str(&request.to_string());
    out.push_str(",\"");
    out.push_str(member);
    out.push_str("\":");
    out.push_str(body);
    out.push('}');
    out
}

/// Gives an answer to the socket that asked.
pub(super) fn deliver_res(res: Res, sockets: &HashMap<u32, Arc<SocketCore>>, counters: &Counters) {
    match sockets.get(&res.socket) {
        Some(socket) => {
            socket.queue.push(answer(res.request, res.body), false);
        }
        None => Counters::bump(&counters.dropped_unknown_socket),
    }
}

/// Gives an event to every socket (target 0) or to the one that is named.
pub(super) fn deliver_evt(evt: Evt, sockets: &HashMap<u32, Arc<SocketCore>>, counters: &Counters) {
    if evt.target == 0 {
        let text = event_text(&evt.name, &evt.text);
        for socket in sockets.values() {
            socket.queue.push(Outbound::Text(text.clone()), false);
        }
        return;
    }
    match sockets.get(&evt.target) {
        Some(socket) => {
            socket.queue.push(Outbound::Text(event_text(&evt.name, &evt.text)), false);
        }
        None => Counters::bump(&counters.dropped_unknown_socket),
    }
}
