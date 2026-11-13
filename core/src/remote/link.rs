//! The link a pane runs on when it shows a session that lives on a holzWave server.
//!
//! The pane's own engine draws the session from the bytes the server sends, so scroll, selection and search stay with the pane.
//! This link is the byte path and the rules around it:
//!
//! * **Bytes.** The reader hands the pane, in order: a notice line, the reset that makes the engine empty, the snapshot the
//!   server sent with the attach, and then the live bytes. Notices (the words of the design record) travel the same path, so
//!   their order with the bytes is the order they happened in. The grid of the session is applied before the first byte of a
//!   snapshot ([`PaneSink::grid`]); a size that changes while the session runs is applied after the live bytes before it
//!   ([`PaneSink::grid_in_order`]), which the session wrote at the old size.
//! * **The writer rule.** Only the writer's typed bytes are sent. Typed bytes of a pane that watches, or that nobody is told
//!   writes, are dropped here and counted ([`RemoteView::dropped_input`]); the hub drops them again and the server a third
//!   time. Nothing typed is kept while the connection is down.
//! * **The grid rule.** The writer's size is the session's size: a resize of the writer's tile is sent, a watcher's is only
//!   recorded, and when a pane starts to write it sends its recorded tile size once if that is not the session's size. The
//!   pane's engine follows the size the server states.
//! * **A re-attach is the one mechanism** for a reconnect, a resync and older lines: the server sends the screen and a tail
//!   again, the engine is reset to nothing and fed it, so it shows exactly what a fresh pane would show. Older lines are a
//!   re-attach with a longer tail; the viewport keeps its distance from the bottom.
//! * **Ends.** A session that ended on the server ends the pane with its exit code and keeps `Session ended · exit N` on the
//!   screen. A failure that cannot be helped (a session that is not there, a token the server no longer takes) is one fixed
//!   sentence on the screen and the end of the pane with the status 255, as a lost SSH login is. Closing the pane only detaches.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::hub::{Cause, Hub, HubError, SubEvent, SubHandle, Subscription};
use super::proto::{MAX_TAIL, WriterView};
use crate::Result;
use crate::server_link::Fault;
use crate::conn::{ConnState, Link, RemoteControl, RemoteState, RemoteView, SharedState};
use crate::pane::{PaneSink, RemoteAttach};

/// What the pane says while the first attach is on its way.
pub const NOTICE_ATTACHING: &str = "Attaching…";
/// What the pane says while an attach with a longer tail is on its way.
pub const NOTICE_LOADING_OLDER: &str = "Loading older lines…";
/// What the pane says when the server asked for a fresh start, or the pane fell behind.
pub const NOTICE_RESYNCING: &str = "Resyncing…";
/// What the pane says while the connection is made again.
pub const NOTICE_RECONNECTING: &str = "Reconnecting…";
/// What the pane says when the tries are used.
pub const NOTICE_UNREACHABLE: &str = "Server unreachable. Press Enter to try again.";

/// The notice of a session that ended, with its status when it was readable.
pub fn notice_ended(code: Option<u32>) -> String {
    match code {
        Some(code) => format!("Session ended · exit {code}"),
        None => "Session ended".to_owned(),
    }
}

/// What the engine is fed before a snapshot: a full reset (`ESC c`), which leaves the terminal as new, with no screen, no
/// scrollback, no mode and no tab stop of what was shown before, and the size it has. It is the smallest sequence that makes a
/// pane that was fed anything show exactly what a fresh pane shows when it is fed the same snapshot; the test
/// `a_full_reset_before_the_snapshot_is_the_smallest_prefix_that_makes_an_engine_new` in `core/tests/remote_pane.rs` shows
/// what each smaller one leaves behind.
pub const RESET_BEFORE_SNAPSHOT: &[u8] = b"\x1bc";

/// How many rows of scrollback an attach asks for when the pane's spec did not say: the server's own default, which is also
/// what the doubling of [`RemoteControl::load_older`] starts from.
const DEFAULT_TAIL: u32 = 1000;

/// How long the pane says that input was dropped after the server last said so. The server says it at most once a second
/// while it lasts, so the state holds while input keeps being dropped and ends a few seconds after.
pub const INPUT_DROPPED_FOR: Duration = Duration::from_secs(5);

/// The exit status of a pane that ended because the link could not go on.
pub const EXIT_GAVE_UP: u32 = 255;

/// How long the reader waits for an event before it looks again. A pane that is closed wakes it at once.
const READ_WAIT: Duration = Duration::from_secs(1);

/// The text a pane shows for something the connection has to say: warn colour, on a line of its own, and never an escape
/// sequence (control characters become spaces).
pub fn notice(text: &str) -> Vec<u8> {
    let clean: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    format!("\r\n\x1b[33m{clean}\x1b[0m\r\n").into_bytes()
}

struct Inner {
    view: RemoteView,
    /// The size of the pane's tile in cells.
    tile: (u16, u16),
    /// The rows of scrollback the last attach asked for.
    tail: u32,
    /// The size of the snapshot of the last attach that was accepted, in bytes.
    snapshot: u32,
    /// An attach for older lines was asked for and has not been answered.
    older: bool,
    /// The status of the program, once the session ended.
    exit: Option<u32>,
    /// Until when the pane says that input was dropped ([`RemoteState::InputDropped`]).
    dropped_until: Option<Instant>,
}

/// Makes a new subscription to the same session, for a pane whose sign-in ended and that is asked to try again.
pub type Renew = Arc<dyn Fn() -> Subscription + Send + Sync>;

struct Shared {
    /// The handle of the subscription now: it is replaced when a pane that was signed out attaches again.
    handle: Mutex<SubHandle>,
    renew: Renew,
    /// The sign-in ended: the pane waits for Enter and then takes a new subscription (the token comes from the endpoint again).
    signed_out: AtomicBool,
    /// Enter was pressed in the signed-out state.
    again: AtomicBool,
    state: SharedState,
    inner: Mutex<Inner>,
    dropped: AtomicU64,
    closed: AtomicBool,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn handle(&self) -> SubHandle {
        self.handle.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn view(&self) -> RemoteView {
        let inner = self.lock();
        let mut view = inner.view.clone();
        // A state that is under way says more than the drop of a moment ago, and takes its place.
        if view.state == RemoteState::None && inner.dropped_until.is_some_and(|until| Instant::now() < until) {
            view.state = RemoteState::InputDropped;
        }
        drop(inner);
        view.dropped_input = self.dropped.load(Ordering::Relaxed);
        view
    }

    /// The older lines that were asked for do not come as asked: the request could not be sent, or the connection went before
    /// the answer. The pane may ask again, and the header no longer says they are loading. A reconnect still attaches with
    /// the longer tail.
    fn older_not_coming(&self) {
        let mut inner = self.lock();
        inner.older = false;
        if inner.view.state == RemoteState::LoadingOlder {
            inner.view.state = RemoteState::None;
        }
    }

    /// The pane starts to write: the session takes the size of its tile, once, when that is not the size it has.
    fn send_tile_if_needed(&self) {
        let inner = self.lock();
        if inner.tile != (inner.view.cols, inner.view.rows) {
            let (cols, rows) = inner.tile;
            drop(inner);
            let _ = self.handle().resize(cols, rows);
        }
    }
}

impl RemoteControl for Shared {
    fn view(&self) -> RemoteView {
        Shared::view(self)
    }

    fn takeover(&self) -> Result<()> {
        if self.signed_out.load(Ordering::SeqCst) {
            return Err(HubError::Fault(Fault::SignedOut).into());
        }
        if self.state.get() != ConnState::Connected {
            return Err(HubError::Fault(Fault::Unreachable).into());
        }
        Ok(self.handle().takeover()?)
    }

    fn load_older(&self) -> Result<()> {
        let tail = {
            let mut inner = self.lock();
            if !inner.view.more || inner.older || self.state.get() != ConnState::Connected {
                return Ok(());
            }
            inner.older = true;
            inner.view.state = RemoteState::LoadingOlder;
            inner.tail = inner.tail.max(1).saturating_mul(2).min(MAX_TAIL);
            inner.tail
        };
        self.handle().reattach(Some(tail), Cause::Older).inspect_err(|_| self.older_not_coming())?;
        Ok(())
    }

    fn tile(&self, cols: u16, rows: u16) {
        self.lock().tile = (cols, rows);
    }
}

/// A pane's link to a session on a server.
pub struct ServerLink {
    reader: Option<Box<dyn Read + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    shared: Arc<Shared>,
}

impl ServerLink {
    /// A link that shows `session` for a pane of `size` cells, on `sub`. Returns at once.
    pub fn new(sub: Subscription, tail: Option<u32>, size: (u16, u16), sink: PaneSink, renew: Renew) -> ServerLink {
        let session = sub.session();
        let state = SharedState::new(ConnState::Connecting);
        let view = RemoteView {
            session,
            writer: WriterView::Free,
            cols: size.0,
            rows: size.1,
            more: false,
            state: RemoteState::Attaching,
            dropped_input: 0,
        };
        let shared = Arc::new(Shared {
            handle: Mutex::new(sub.handle()),
            renew,
            signed_out: AtomicBool::new(false),
            again: AtomicBool::new(false),
            state,
            inner: Mutex::new(Inner {
                view,
                tile: size,
                tail: tail.unwrap_or(DEFAULT_TAIL),
                snapshot: 0,
                older: false,
                exit: None,
                dropped_until: None,
            }),
            dropped: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        });
        let reader = ServerReader {
            sub,
            shared: shared.clone(),
            sink,
            pending: notice(NOTICE_ATTACHING),
            pos: 0,
            snapshot_left: 0,
            restore: false,
            restore_after_attach: false,
            reconnect_noticed: false,
            ending: None,
        };
        let writer = ServerWriter { shared: shared.clone() };
        ServerLink { reader: Some(Box::new(reader)), writer: Some(Box::new(writer)), shared }
    }
}

/// What the reader does when its pending bytes are used up.
enum Ending {
    /// The session ended with this status: the pane ends.
    Exited(Option<u32>),
    /// The link could not go on: the pane ends with [`EXIT_GAVE_UP`].
    GaveUp,
    /// The pane was closed.
    Closed,
}

struct ServerReader {
    sub: Subscription,
    shared: Arc<Shared>,
    sink: PaneSink,
    /// Bytes for the pane that were not handed over yet, from `pos` on.
    pending: Vec<u8>,
    pos: usize,
    /// Bytes of the snapshot that are still to come.
    snapshot_left: u32,
    /// Put the viewport back once the snapshot is fed.
    restore: bool,
    /// The viewport was marked for older lines and the answer has not come yet: `restore` is set when it does.
    restore_after_attach: bool,
    /// The notice of a lost connection was shown for this loss.
    reconnect_noticed: bool,
    ending: Option<Ending>,
}

impl ServerReader {
    fn say(&mut self, text: &str) {
        self.pending.extend_from_slice(&notice(text));
    }

    /// Enter in the signed-out state: attach again on a new subscription.
    fn renew(&mut self) {
        self.shared.signed_out.store(false, Ordering::SeqCst);
        let sub = (self.shared.renew)();
        *self.shared.handle.lock().unwrap_or_else(PoisonError::into_inner) = sub.handle();
        self.sub = sub;
        self.shared.lock().view.state = RemoteState::Attaching;
        self.reconnect_noticed = false;
        self.say(NOTICE_ATTACHING);
    }

    fn event(&mut self, event: SubEvent) {
        match event {
            SubEvent::Attached { cols, rows, writer, more, snapshot, .. } => {
                // The size before the first byte of the snapshot.
                self.sink.grid(cols, rows);
                {
                    let mut inner = self.shared.lock();
                    let older = std::mem::take(&mut inner.older);
                    // A longer tail that brought nothing more has reached what the server keeps.
                    let grew = snapshot > inner.snapshot;
                    inner.snapshot = snapshot;
                    inner.view.cols = cols;
                    inner.view.rows = rows;
                    inner.view.writer = writer;
                    inner.view.more = more && (!older || grew);
                    inner.view.state = RemoteState::None;
                }
                self.reconnect_noticed = false;
                self.shared.state.set(ConnState::Connected);
                self.snapshot_left = snapshot;
                self.restore = std::mem::take(&mut self.restore_after_attach);
                self.pending.extend_from_slice(RESET_BEFORE_SNAPSHOT);
                if writer == WriterView::Writing {
                    self.shared.send_tile_if_needed();
                }
            }
            SubEvent::Snapshot(bytes) => {
                self.snapshot_left = self.snapshot_left.saturating_sub(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
                self.pending = bytes;
                self.pos = 0;
            }
            SubEvent::Live(bytes) => {
                self.pending = bytes;
                self.pos = 0;
            }
            SubEvent::Writer(writer) => {
                self.shared.lock().view.writer = writer;
                if writer == WriterView::Writing {
                    self.shared.send_tile_if_needed();
                }
            }
            SubEvent::Size(cols, rows) => {
                {
                    let mut inner = self.shared.lock();
                    inner.view.cols = cols;
                    inner.view.rows = rows;
                }
                // After the live bytes before it: they were written at the old size.
                self.sink.grid_in_order(cols, rows);
            }
            SubEvent::Exited(code) => {
                self.say(&notice_ended(code));
                self.ending = Some(Ending::Exited(code));
            }
            SubEvent::Reattaching(cause) => {
                let (word, state) = match cause {
                    Cause::Resync => (NOTICE_RESYNCING, RemoteState::Resyncing),
                    Cause::Older => (NOTICE_LOADING_OLDER, RemoteState::LoadingOlder),
                };
                self.shared.lock().view.state = state;
                if cause == Cause::Older {
                    // The distance from the bottom is read before the notice goes on the screen: the notice is not in the
                    // session and its rows must not count.
                    self.sink.mark_scroll();
                    self.restore_after_attach = true;
                }
                self.say(word);
            }
            SubEvent::Reconnecting { attempt, of, in_secs } => {
                self.shared.older_not_coming();
                self.shared.state.set(ConnState::Reconnecting { attempt, of, in_secs });
                if !self.reconnect_noticed {
                    self.reconnect_noticed = true;
                    self.say(NOTICE_RECONNECTING);
                }
            }
            SubEvent::GaveUp => {
                self.shared.older_not_coming();
                self.shared.state.set(ConnState::Failed);
                self.say(NOTICE_UNREACHABLE);
            }
            // Only the header says it: a line on the screen would move the session's own text, which the server's screen does
            // not have.
            SubEvent::InputDropped => self.shared.lock().dropped_until = Some(Instant::now() + INPUT_DROPPED_FOR),
            SubEvent::Lost(HubError::Closed) => self.ending = Some(Ending::Closed),
            SubEvent::Lost(error @ HubError::Fault(Fault::SignedOut)) => {
                // The server does not take the token any more. The pane stays and says so; after a new sign-in Enter attaches
                // again, with the token the endpoint has then.
                self.say(error.sentence());
                self.shared.older_not_coming();
                self.shared.state.set(ConnState::Failed);
                self.shared.signed_out.store(true, Ordering::SeqCst);
            }
            SubEvent::Lost(error) => {
                self.say(error.sentence());
                self.ending = Some(Ending::GaveUp);
            }
        }
    }
}

impl Read for ServerReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.pos < self.pending.len() {
                let n = buf.len().min(self.pending.len() - self.pos);
                buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            self.pending.clear();
            self.pos = 0;
            // Everything of the snapshot was handed over: the viewport goes back to where it was.
            if self.restore && self.snapshot_left == 0 {
                self.restore = false;
                self.sink.restore_scroll();
            }
            if let Some(ending) = self.ending.take() {
                // The pane has every byte there is to show; now it may end.
                let code = match ending {
                    Ending::Exited(code) => code.unwrap_or(0),
                    Ending::GaveUp => EXIT_GAVE_UP,
                    Ending::Closed => 0,
                };
                self.shared.lock().exit.get_or_insert(code);
                self.shared.state.set(ConnState::Closed);
                return Ok(0);
            }
            let signed_out = self.shared.signed_out.load(Ordering::SeqCst);
            match self.sub.recv(if signed_out { Duration::from_millis(50) } else { READ_WAIT }) {
                Some(event) => self.event(event),
                None => {
                    if self.shared.closed.load(Ordering::SeqCst) {
                        self.ending = Some(Ending::Closed);
                    } else if signed_out && self.shared.again.swap(false, Ordering::SeqCst) {
                        self.renew();
                    }
                }
            }
        }
    }
}

struct ServerWriter {
    shared: Arc<Shared>,
}

impl Write for ServerWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.shared.state.get() {
            ConnState::Connected => {
                if self.shared.lock().view.writer == WriterView::Writing {
                    let _ = self.shared.handle().input(buf);
                } else {
                    // A pane that watches, or that nobody is told writes: nothing is sent.
                    self.shared.dropped.fetch_add(buf.len() as u64, Ordering::Relaxed);
                }
            }
            // Enter tries once more; everything else typed is dropped.
            ConnState::Failed if buf.iter().any(|b| *b == b'\r' || *b == b'\n') => {
                self.shared.state.set(ConnState::Connecting);
                if self.shared.signed_out.load(Ordering::SeqCst) {
                    self.shared.again.store(true, Ordering::SeqCst);
                } else {
                    let _ = self.shared.handle().retry();
                }
            }
            // While the connection is down nothing typed is sent and none of it is kept: the session may have another writer
            // when the connection is back.
            _ => {}
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Link for ServerLink {
    fn take_reader(&mut self) -> Option<Box<dyn Read + Send>> {
        self.reader.take()
    }

    fn take_writer(&mut self) -> Option<Box<dyn Write + Send>> {
        self.writer.take()
    }

    fn process_id(&self) -> Option<u32> {
        None
    }

    fn resize_px(&self, cols: u16, rows: u16, _px_w: u32, _px_h: u32) -> Result<()> {
        // Only the writer sets the size of the session, and only when it differs from the one it has.
        let sends = {
            let inner = self.shared.lock();
            self.shared.state.get() == ConnState::Connected
                && inner.view.writer == WriterView::Writing
                && (cols, rows) != (inner.view.cols, inner.view.rows)
        };
        if sends {
            self.shared.handle().resize(cols, rows)?;
        }
        Ok(())
    }

    fn try_wait(&mut self) -> Result<Option<u32>> {
        Ok(self.shared.lock().exit)
    }

    fn close(self: Box<Self>) -> Result<u32> {
        // Detaches; the session goes on. The reader that waits is woken and ends.
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.handle().close();
        let code = self.shared.lock().exit.unwrap_or(0);
        self.shared.state.set(ConnState::Closed);
        Ok(code)
    }

    fn state(&self) -> ConnState {
        self.shared.state.get()
    }

    fn shared_state(&self) -> Option<SharedState> {
        Some(self.shared.state.clone())
    }

    fn remote_control(&self) -> Option<Arc<dyn RemoteControl>> {
        Some(self.shared.clone())
    }
}

impl RemoteAttach for Hub {
    fn link(&self, session: u32, tail: Option<u32>, size: (u16, u16), sink: PaneSink) -> Result<Box<dyn Link>> {
        let sub = self.attach(session, tail);
        let hub = self.clone();
        let renew: Renew = Arc::new(move || hub.attach(session, tail));
        Ok(Box::new(ServerLink::new(sub, tail, size, sink, renew)))
    }

    fn host(&self) -> String {
        Hub::host(self)
    }
}
