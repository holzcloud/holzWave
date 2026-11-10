//! Delivery of one worker's output to every browser tab attached to it.
//!
//! A worker's answer goes to the socket that asked, in the text the page reads, and its events go to every socket of the
//! person or to the one the worker names. The bodies are the worker's own JSON text and are put into the wrapper without
//! being read: the only checks are the size of a frame, the kind, the event name and that the text is UTF-8 (the frame
//! decoder does the last). This module also holds the rule that gives the size of a pane to one socket, the only place
//! where the arguments of a command are read, and then only for the pane number and the size.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;

use crate::limits;
use crate::supervisor::socket::{Outbound, Push, SocketCore, StreamPush};
use crate::wire::{Evt, FILE_SOCKET, Res, ResBody, Ses};

/// The event whose loss the page repairs by asking for a full frame.
pub const FRAME_EVENT: &str = "frame";

/// What the supervisor counts, shared by all workers.
#[derive(Default)]
pub(super) struct Counters {
    pub(super) dropped_unknown_socket: AtomicU64,
    pub(super) dropped_frames: AtomicU64,
    pub(super) dropped_synthetic: AtomicU64,
    pub(super) overflow_closes: AtomicU64,
    pub(super) worker_deaths: AtomicU64,
    pub(super) dropped_ctl_done: AtomicU64,
    pub(super) file_calls: AtomicU64,
    pub(super) dropped_file_answers: AtomicU64,
    pub(super) lost_streams: AtomicU64,
}

impl Counters {
    pub(super) fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    /// Queues a message that is not a frame event for `socket` and counts what the queue did with it. True when the queue
    /// overflowed and the socket is closed.
    pub(super) fn push(&self, socket: &SocketCore, message: Outbound) -> bool {
        self.pushed(socket.queue.push(message, false))
    }

    /// Counts what a queue did with a message. True for an overflow.
    fn pushed(&self, outcome: Push) -> bool {
        match outcome {
            Push::DroppedFrame => Counters::bump(&self.dropped_frames),
            Push::Overflow => {
                Counters::bump(&self.overflow_closes);
                return true;
            }
            Push::Queued | Push::Gone => {}
        }
        false
    }
}

// ---- what a worker may send ----------------------------------------------------------------------------------------

/// What was wrong with a frame of a worker. The words hold no byte of the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    /// The frame could not be read: a length, a kind, a note, a field or a text that the layout does not allow.
    Malformed,
    /// A kind that only the supervisor sends.
    WrongDirection,
    /// An event name that is empty, too long or outside the character set.
    BadEventName,
    /// A second `Ready`, or a frame before the first one.
    OutOfOrder,
}

impl Fault {
    /// A static word for the log.
    pub(super) fn word(self) -> &'static str {
        match self {
            Fault::Malformed => "malformed",
            Fault::WrongDirection => "wrong_direction",
            Fault::BadEventName => "bad_event_name",
            Fault::OutOfOrder => "out_of_order",
        }
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

/// An event name: 1 to 32 bytes of lower-case ASCII, digits, `-` and `_`.
pub(super) fn check_event_name(name: &str) -> Result<(), Fault> {
    let ok = !name.is_empty()
        && name.len() <= limits::EVENT_NAME_BYTES
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok { Ok(()) } else { Err(Fault::BadEventName) }
}

/// A command name: 1 to 64 bytes of lower-case ASCII, digits and `_`.
pub(super) fn check_command_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= limits::COMMAND_NAME_BYTES
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

// ---- the texts the page reads ------------------------------------------------------------------------------------------

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

/// The answer the supervisor gives itself, for a command it did not pass on: `null`.
pub(super) fn null_answer(request: u64) -> Outbound {
    Outbound::Text(wrapped(request, "ok", "null"))
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

/// The text of a command for the worker: `{"cmd":"<name>","args":<text>}`, the arguments as the page wrote them. Without
/// arguments the member is left out.
pub(super) fn command_json(command: &str, args: &str) -> String {
    let mut out = String::with_capacity(command.len() + args.len() + 20);
    out.push_str("{\"cmd\":\"");
    out.push_str(command);
    out.push('"');
    if !args.trim().is_empty() {
        out.push_str(",\"args\":");
        out.push_str(args);
    }
    out.push('}');
    out
}

// ---- delivery --------------------------------------------------------------------------------------------------------

/// Gives an answer to the socket that asked. An answer to a request the supervisor made up itself is dropped. The result
/// holds the sockets whose queue overflowed because of it.
pub(super) fn deliver_res(res: Res, sockets: &HashMap<u32, Arc<SocketCore>>, counters: &Counters) -> Vec<u32> {
    if res.request >= limits::SYNTHETIC_REQUEST_BASE {
        Counters::bump(&counters.dropped_synthetic);
        return Vec::new();
    }
    match sockets.get(&res.socket) {
        // A device socket speaks sessions, not commands: no answer to a command is for it.
        Some(socket) if !socket.device && counters.pushed(socket.queue.push(answer(res.request, res.body), false)) => {
            vec![socket.id]
        }
        Some(_) => Vec::new(),
        None => {
            Counters::bump(&counters.dropped_unknown_socket);
            Vec::new()
        }
    }
}

/// Whether an answer is the answer to a file control rather than to a command of a browser socket.
pub(super) fn is_file_answer(res: &Res) -> bool {
    res.socket == FILE_SOCKET
}

/// Gives the answer to a file control to the call that waits for it; an answer nobody waits for is counted and dropped.
pub(super) fn deliver_file_res(res: Res, waiter: Option<tokio::sync::oneshot::Sender<ResBody>>, counters: &Counters) {
    match waiter {
        Some(waiter) => {
            let _ = waiter.send(res.body);
        }
        None => Counters::bump(&counters.dropped_file_answers),
    }
}

/// Gives an event to every socket (target 0) or to the one that is named. A frame event that does not fit is dropped;
/// anything else that does not fit closes that socket only. The result holds the sockets whose queue overflowed.
pub(super) fn deliver_evt(evt: Evt, sockets: &HashMap<u32, Arc<SocketCore>>, counters: &Counters) -> Vec<u32> {
    let droppable = evt.name == FRAME_EVENT;
    let mut overflowed = Vec::new();
    if evt.target == 0 {
        let mut text = event_text(&evt.name, &evt.text);
        // The events of the window are for the pages; a device socket has none of them.
        let mut targets = sockets.values().filter(|socket| !socket.device).peekable();
        while let Some(socket) = targets.next() {
            // The last socket takes the text itself; the others get a copy.
            let message = if targets.peek().is_some() { text.clone() } else { std::mem::take(&mut text) };
            if counters.pushed(socket.queue.push(Outbound::Text(message), droppable)) {
                overflowed.push(socket.id);
            }
        }
        return overflowed;
    }
    match sockets.get(&evt.target) {
        Some(socket) if !socket.device => {
            if counters.pushed(socket.queue.push(Outbound::Text(event_text(&evt.name, &evt.text)), droppable)) {
                overflowed.push(socket.id);
            }
        }
        _ => Counters::bump(&counters.dropped_unknown_socket),
    }
    overflowed
}

/// What came of a frame about sessions that a worker wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Delivery {
    /// Queued, or dropped on purpose and counted.
    Done,
    /// The queue of this socket overflowed and was closed: the socket leaves its worker.
    Overflow(u32),
    /// The stream of this session ended for this socket for lack of room: the worker is to let the socket go from the session.
    Lost { socket: u32, session: u32 },
}

/// Gives an answer, an event or a chunk of a session to the device socket it names. An answer to a request the supervisor made
/// up itself is dropped. A frame for a socket that is not there, or that is not a device's, is counted and dropped. A chunk that
/// does not fit ends that stream only (see [`SocketCore`]'s queue); an answer or an event that does not fit closes that socket.
pub(super) fn deliver_ses(ses: Ses, sockets: &HashMap<u32, Arc<SocketCore>>, counters: &Counters) -> Delivery {
    let (socket, session, droppable_stream) = match &ses {
        Ses::Answer { request, .. } if *request >= limits::SYNTHETIC_REQUEST_BASE => {
            Counters::bump(&counters.dropped_synthetic);
            return Delivery::Done;
        }
        Ses::Answer { socket, .. } => (*socket, 0, false),
        Ses::Event { socket, session, .. } => (*socket, *session, false),
        Ses::Snapshot { socket, session, .. } | Ses::Live { socket, session, .. } => (*socket, *session, true),
        // A request is the supervisor's to send; the caller has refused it already.
        _ => return Delivery::Done,
    };
    let Some(target) = sockets.get(&socket).filter(|s| s.device) else {
        Counters::bump(&counters.dropped_unknown_socket);
        return Delivery::Done;
    };
    let message = Outbound::Ses(Box::new(ses));
    if droppable_stream {
        return match target.queue.push_stream(socket, session, message) {
            StreamPush::Queued | StreamPush::Gone => Delivery::Done,
            StreamPush::Dropped => {
                Counters::bump(&counters.dropped_frames);
                Delivery::Done
            }
            StreamPush::Lost => {
                Counters::bump(&counters.lost_streams);
                Delivery::Lost { socket, session }
            }
        };
    }
    if counters.pushed(target.queue.push(message, false)) { Delivery::Overflow(socket) } else { Delivery::Done }
}

// ---- the owner of the size of a pane ---------------------------------------------------------------------------------

/// The size of a pane as a page reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Size {
    cols: u32,
    rows: u32,
    cell_w: Option<u32>,
    cell_h: Option<u32>,
}

/// A `resize` that the supervisor makes itself, because the pane changed hands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Synthetic {
    /// The socket whose size it is, and the socket the command is made for.
    pub(super) socket: u32,
    pane: u64,
    size: Size,
}

impl Synthetic {
    /// The arguments of the `resize` command.
    pub(super) fn args(&self) -> String {
        let mut out = format!("{{\"pane\":{},\"cols\":{},\"rows\":{}", self.pane, self.size.cols, self.size.rows);
        if let Some(w) = self.size.cell_w {
            out.push_str(&format!(",\"cell_w\":{w}"));
        }
        if let Some(h) = self.size.cell_h {
            out.push_str(&format!(",\"cell_h\":{h}"));
        }
        out.push('}');
        out
    }
}

/// The arguments the size rule reads. Everything else in the text is skipped unread.
#[derive(Deserialize)]
struct PaneArgs {
    pane: u64,
    cols: Option<u32>,
    rows: Option<u32>,
    cell_w: Option<u32>,
    cell_h: Option<u32>,
}

impl PaneArgs {
    fn size(&self) -> Option<Size> {
        Some(Size { cols: self.cols?, rows: self.rows?, cell_w: self.cell_w, cell_h: self.cell_h })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rule {
    /// Not one of the seven, or the arguments cannot be read: nothing changes and the command goes on unchanged.
    Pass,
    /// Input from a socket to a pane.
    Input { pane: u64 },
    /// A `resize`.
    Resize { pane: u64, size: Size },
    /// A `start_pane`.
    Start { pane: u64, size: Size },
}

/// What to do with one command, decided before anything changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    rule: Rule,
    /// The command is answered `null` and not passed on.
    pub(super) answer: bool,
    /// A `resize` for the worker before the command.
    pub(super) before: Option<Synthetic>,
    /// A `resize` for the worker after the command.
    pub(super) after: Option<Synthetic>,
}

impl Plan {
    const PASS: Plan = Plan { rule: Rule::Pass, answer: false, before: None, after: None };
}

#[derive(Clone, Copy)]
struct Owned {
    socket: u32,
    stamp: u64,
}

#[derive(Clone, Copy)]
struct Remembered {
    size: Size,
    stamp: u64,
}

/// Who owns the size of each pane, and the sizes the sockets of one person last reported.
#[derive(Default)]
pub(super) struct Owners {
    owner: HashMap<u64, Owned>,
    sizes: HashMap<(u64, u32), Remembered>,
    clock: u64,
}

impl Owners {
    /// The panes that have an owner.
    pub(super) fn owned(&self) -> usize {
        self.owner.len()
    }

    /// The sizes that are remembered.
    pub(super) fn remembered(&self) -> usize {
        self.sizes.len()
    }

    /// Reads the arguments of the seven commands of the size rule. Any other command, and any text that cannot be read as
    /// a pane number with the size the command needs, passes unchanged and moves nothing.
    pub(super) fn assess(&self, socket: u32, command: &str, args: &str) -> Plan {
        let rule = match command {
            "key" | "text" | "paste" | "send_edit" | "mouse" => match read(args) {
                Some(parsed) => Rule::Input { pane: parsed.pane },
                None => return Plan::PASS,
            },
            "resize" | "start_pane" => match read(args).and_then(|p| p.size().map(|size| (p.pane, size))) {
                Some((pane, size)) if command == "resize" => Rule::Resize { pane, size },
                Some((pane, size)) => Rule::Start { pane, size },
                None => return Plan::PASS,
            },
            _ => return Plan::PASS,
        };
        let owner = |pane: u64| self.owner.get(&pane).map(|o| o.socket);
        match rule {
            Rule::Pass => Plan::PASS,
            Rule::Input { pane } => {
                let before = match owner(pane) {
                    Some(current) if current != socket => {
                        self.sizes.get(&(pane, socket)).map(|r| Synthetic { socket, pane, size: r.size })
                    }
                    _ => None,
                };
                Plan { rule, answer: false, before, after: None }
            }
            Rule::Resize { pane, .. } => {
                let answer = owner(pane).is_some_and(|current| current != socket);
                Plan { rule, answer, before: None, after: None }
            }
            Rule::Start { pane, size } => {
                let after = match owner(pane) {
                    Some(current) if current != socket => Some(Synthetic { socket, pane, size }),
                    _ => None,
                };
                Plan { rule, answer: false, before: None, after }
            }
        }
    }

    /// Makes the changes a plan decided on, once the command is queued.
    pub(super) fn commit(&mut self, socket: u32, plan: &Plan) {
        match plan.rule {
            Rule::Pass => {}
            Rule::Input { pane } => self.take(pane, socket),
            Rule::Resize { pane, size } => {
                self.remember(pane, socket, size);
                if !plan.answer {
                    self.take(pane, socket);
                }
            }
            Rule::Start { pane, size } => {
                self.remember(pane, socket, size);
                self.take(pane, socket);
            }
        }
    }

    /// `socket` owns `pane` from now on.
    fn take(&mut self, pane: u64, socket: u32) {
        if self.owner.get(&pane).is_some_and(|o| o.socket == socket) {
            return;
        }
        self.clock += 1;
        if !self.owner.contains_key(&pane) && self.owner.len() >= limits::OWNED_PANES {
            // The oldest owner is forgotten: that pane has no owner until a socket resizes or types in it.
            if let Some(oldest) = self.owner.iter().min_by_key(|(_, o)| o.stamp).map(|(pane, _)| *pane) {
                self.owner.remove(&oldest);
            }
        }
        self.owner.insert(pane, Owned { socket, stamp: self.clock });
    }

    /// Remembers the latest size of `socket` for `pane`.
    fn remember(&mut self, pane: u64, socket: u32, size: Size) {
        self.clock += 1;
        if !self.sizes.contains_key(&(pane, socket))
            && self.sizes.len() >= limits::REMEMBERED_SIZES
            && let Some(oldest) = self.sizes.iter().min_by_key(|(_, r)| r.stamp).map(|(key, _)| *key)
        {
            self.sizes.remove(&oldest);
        }
        self.sizes.insert((pane, socket), Remembered { size, stamp: self.clock });
    }

    /// A socket left: its sizes are forgotten, and each pane it owned goes to the socket with the latest size.
    pub(super) fn left(&mut self, socket: u32) -> Vec<Synthetic> {
        self.sizes.retain(|(_, s), _| *s != socket);
        let owned: Vec<u64> = self.owner.iter().filter(|(_, o)| o.socket == socket).map(|(pane, _)| *pane).collect();
        let mut best: HashMap<u64, (u64, u32, Size)> = HashMap::new();
        for ((pane, other), remembered) in &self.sizes {
            if self.owner.get(pane).is_some_and(|o| o.socket == socket)
                && best.get(pane).is_none_or(|(stamp, _, _)| remembered.stamp > *stamp)
            {
                best.insert(*pane, (remembered.stamp, *other, remembered.size));
            }
        }
        let mut handovers = Vec::new();
        for pane in owned {
            self.owner.remove(&pane);
            if let Some((_, other, size)) = best.get(&pane) {
                self.take(pane, *other);
                handovers.push(Synthetic { socket: *other, pane, size: *size });
            }
        }
        handovers
    }
}

/// The pane number and the sizes in a text of arguments, if it is JSON with a numeric `pane`.
fn read(args: &str) -> Option<PaneArgs> {
    serde_json::from_str(args).ok()
}
