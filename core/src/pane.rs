//! The pane actor: one engine thread that owns the engine and the PTY child, a PTY reader thread feeding it, a PTY
//! writer thread, and the pacing of frames towards the shell. It lives in the core and knows nothing about Tauri
//! (D-19): the shell passes a sink closure and forwards the [`PaneEvent`]s it receives.
//!
//! # Threads
//!
//! ```text
//!   shell (any thread) --PaneHandle--> PaneQueue<Control> <--output-- reader thread <-- PTY master
//!                                            |
//!                                       engine thread  (owns the !Send engine, the Pty and the encoder)
//!                                            |  bytes for the child
//!                                      writer thread --> PTY master
//! ```
//!
//! - The **engine thread** is the only one that touches the engine. It pops control messages before output (a key or
//!   a close is never stuck behind a flood), feeds output, collects the engine's replies, and emits at most one
//!   frame per frame interval, and none while nothing changed.
//! - The **reader thread** does 64 KiB reads into the byte-bounded queue. At the end of the output it calls
//!   [`PaneQueue::finish`], which is ordered after every queued chunk: the last output of a short-lived shell is
//!   not lost.
//! - The **writer thread** owns the PTY writer behind an unbounded channel. The engine thread never writes to the
//!   PTY itself: a bounded queue plus a blocking write on the engine thread is a deadlock (RESEARCH Pitfall 6), for
//!   example a 4 MiB paste into `cat`. Replies to a program's own queries are dropped when the writer is far behind
//!   ([`REPLY_BACKLOG`]); what the user typed or pasted never is.
//!
//! # Ending
//!
//! A pane ends in one of three ways and all of them take the same path: [`PaneHandle::close`] (or the last handle
//! being dropped), the queue reporting the end of the output (the shell exited), or a failed start. The engine thread
//! closes the queue, kills and reaps the child, emits **one** [`PaneEvent::Exited`], and only then drops the writer
//! channel, so the PTY writer (whose `Drop` writes a newline and EOF) goes last. Nothing here can make the
//! application exit: that decision is the shell's, made from the `Exited` event.
//!
//! The sink runs on the engine thread. It may be called before [`PaneHandle::spawn`] has returned (a failed start
//! reports `Failed` first), so it must not wait for the handle.

use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::engine::{
    CopyOp, CopyState, DomKey, EngineEvent, EngineTheme, KeyEvent, MouseEvent, PasteOutcome, SearchOp, SearchState,
    SelectOp, TerminalEngine, WheelEvent, WheelOutcome, parse_osc7,
};
use crate::layout::PaneId;
use crate::pty::{Pty, SpawnOptions, process_cwd};
use crate::queue::{PaneQueue, Popped};
use crate::wire::{FrameEncoder, FrameMsg};
use crate::{Error, Result};

/// The default number of scrollback lines of a pane.
pub const DEFAULT_SCROLLBACK: usize = 10_000;

/// The default time between two frames of a pane.
pub const DEFAULT_FRAME_INTERVAL: Duration = Duration::from_millis(8);

/// Bytes of replies to a program's own queries that may wait for the writer thread. Past it, whole replies are
/// dropped (T-02-38): a program that floods queries while it does not read its input cannot grow memory without
/// limit, and a query nobody read the answer of is not worth keeping.
pub const REPLY_BACKLOG: usize = 256 * 1024;

/// The most bytes of one text input ([`PaneHandle::text`]) the actor accepts. It is what a person typed or an IME
/// committed, so this is generous.
pub const MAX_TEXT_BYTES: usize = 1 << 20;

/// The most bytes of one paste ([`PaneHandle::paste`]) the actor accepts (T-02-42).
pub const MAX_PASTE_BYTES: usize = 16 << 20;

/// How long a call that waits for the engine thread's answer waits at most.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// The largest cell size in pixels and the largest scroll step a caller can ask for; larger values are clamped.
const MAX_CELL_PX: u32 = 1024;
const MAX_SCROLL_ROWS: isize = 1_000_000;

/// The longest text of a search ([`SearchOp::Set`]) the actor accepts.
const MAX_SEARCH_BYTES: usize = 4096;

/// The longest `code` or `key` string of a [`DomKey`] the actor accepts. The longest real value is a named key.
const MAX_KEY_FIELD_BYTES: usize = 256;

/// The grid is clamped to this range before anything is built from it (T-02-41).
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 1;
const MAX_DIM: u16 = 1000;

/// How long `spawn` waits for the engine thread to report that the shell started. Starting a PTY takes
/// milliseconds; this only stops a caller (possibly the main thread) from hanging forever.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// The size of one read from the PTY.
const READ_CHUNK: usize = 64 * 1024;

/// How a pane is started.
#[derive(Clone, Debug)]
pub struct PaneSpec {
    pub program: String,
    pub args: Vec<String>,
    /// The directory the shell starts in. A directory that does not exist starts the shell in the home directory.
    pub cwd: Option<PathBuf>,
    pub cols: u16,
    pub rows: u16,
    /// The size of one cell in pixels, `(0, 0)` when unknown.
    pub cell_px: (u32, u32),
    pub scrollback: usize,
    pub option_as_alt: bool,
    pub theme: Option<EngineTheme>,
    /// The least time between two frames.
    pub frame_interval: Duration,
}

impl PaneSpec {
    /// Run `program` in a grid of `cols` x `rows` cells with 10 000 lines of scrollback and a frame every 8 ms.
    pub fn new(program: impl Into<String>, cols: u16, rows: u16) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            cols,
            rows,
            cell_px: (0, 0),
            scrollback: DEFAULT_SCROLLBACK,
            option_as_alt: false,
            theme: None,
            frame_interval: DEFAULT_FRAME_INTERVAL,
        }
    }

    /// The program's arguments, not including the program itself.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// The directory the shell starts in.
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }
}

/// What a pane tells its sink.
#[derive(Clone, Debug, PartialEq)]
pub enum PaneEvent {
    /// A frame for the webview: the rows that changed since the previous one.
    Frame(FrameMsg),
    /// The program set the window title (sanitised and bounded by the engine).
    Title(String),
    /// The working directory of the shell changed.
    Cwd(String),
    /// The shell ended and was reaped. Exactly once per pane that started, and the last event it sends.
    /// `code` is `None` when the exit status could not be read.
    Exited { code: Option<u32> },
    /// The pane could not start. Exactly once, and no other event follows.
    Failed(String),
}

/// A message from the shell to the engine thread.
enum Control {
    Key(DomKey),
    Text(String),
    /// The engine's answer travels back over `reply`; a dropped sender tells the caller the pane is gone.
    Paste { text: String, allow_unsafe: bool, reply: Sender<Result<PasteOutcome>> },
    Resize { cols: u16, rows: u16, cell_px: (u32, u32) },
    Scroll(isize),
    Mouse(MouseEvent),
    Wheel(WheelEvent),
    ScrollToBottom,
    RequestFull,
    SetTheme(EngineTheme),
    SetOptionAsAlt(bool),
    Select(SelectOp),
    SelectionText(Sender<Result<Option<String>>>),
    Search(SearchOp, Sender<Result<SearchState>>),
    CopyMode(CopyOp, Sender<Result<CopyState>>),
    /// The answer of a working-directory lookup, from the short-lived thread that ran it.
    CwdFound(Option<PathBuf>),
    Close,
}

/// What the shell keeps of a pane. The last clone being dropped closes the pane.
struct Shared {
    queue: Arc<PaneQueue<Control>>,
    pid: Option<u32>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // A pane nobody can reach any more must not keep a shell alive. Never blocks; false means it is gone.
        self.queue.push_control(Control::Close);
    }
}

/// A running pane. Cheap to clone and usable from any thread; every method returns at once (the engine thread does
/// the work), except the ones that wait for an answer and say so.
#[derive(Clone)]
pub struct PaneHandle {
    shared: Arc<Shared>,
}

impl PaneHandle {
    /// Start a pane with an engine built by `make_engine` and report everything it does to `sink`.
    ///
    /// The engine is not `Send`, so it is built inside the engine thread from the `Send` factory, which is also what
    /// lets a test pass a mock engine. Returns when the shell is running (or has failed to start: then the sink got
    /// one `Failed` event and the same reason is returned). Starting a PTY takes a few milliseconds, but do not call
    /// this on a thread that must never wait.
    pub fn spawn<E, F, S>(pane: PaneId, spec: PaneSpec, make_engine: F, sink: S) -> Result<PaneHandle>
    where
        E: TerminalEngine + 'static,
        F: FnOnce(&PaneSpec) -> Result<E> + Send + 'static,
        S: FnMut(PaneEvent) + Send + 'static,
    {
        let mut spec = spec;
        spec.cols = spec.cols.clamp(MIN_COLS, MAX_DIM);
        spec.rows = spec.rows.clamp(MIN_ROWS, MAX_DIM);
        spec.frame_interval = spec.frame_interval.max(Duration::from_millis(1));
        spec.cell_px = (spec.cell_px.0.min(MAX_CELL_PX), spec.cell_px.1.min(MAX_CELL_PX));

        let queue: Arc<PaneQueue<Control>> = Arc::new(PaneQueue::new());
        let (started_tx, started_rx) = mpsc::channel::<Result<Option<u32>>>();
        let engine_queue = queue.clone();
        let join = thread::Builder::new()
            .name(format!("hw-pane-{pane}-engine"))
            .spawn(move || engine_main::<E, F, S>(pane, spec, make_engine, sink, engine_queue, started_tx))
            .map_err(Error::Io)?;

        match started_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(pid)) => Ok(PaneHandle {
                shared: Arc::new(Shared { queue, pid, thread: Mutex::new(Some(join)) }),
            }),
            Ok(Err(e)) => {
                // The thread reported its failure and is on its way out.
                let _ = join.join();
                Err(e)
            }
            Err(_) => {
                queue.push_control(Control::Close);
                Err(Error::Pty("the pane did not start in time".into()))
            }
        }
    }

    /// [`PaneHandle::spawn`] with the libghostty engine, sized from the spec.
    #[cfg(feature = "ghostty")]
    pub fn spawn_ghostty<S>(pane: PaneId, spec: PaneSpec, sink: S) -> Result<PaneHandle>
    where
        S: FnMut(PaneEvent) + Send + 'static,
    {
        Self::spawn(
            pane,
            spec,
            |spec| crate::GhosttyEngine::new(spec.cols, spec.rows, spec.scrollback),
            sink,
        )
    }

    /// The operating system's id of the shell.
    pub fn pid(&self) -> Option<u32> {
        self.shared.pid
    }

    fn send(&self, control: Control) -> Result<()> {
        if self.shared.queue.push_control(control) {
            Ok(())
        } else {
            Err(Error::Pty("the pane is closed".into()))
        }
    }

    /// A key press or release, as the webview saw it. The engine encodes it against the terminal's live modes.
    /// Typing returns the viewport to the live screen.
    pub fn key(&self, key: DomKey) -> Result<()> {
        if key.code.len() > MAX_KEY_FIELD_BYTES || key.key.len() > MAX_KEY_FIELD_BYTES {
            return Err(Error::Pty("key description too long".into()));
        }
        self.send(Control::Key(key))
    }

    /// A mouse press, release or motion, in device pixels relative to the origin of the grid.
    ///
    /// The engine decides from the live terminal modes whether the program hears of it: nothing is written when the
    /// program did not ask for the mouse. A report is input but not typing: it neither clears the selection nor
    /// returns the viewport to the live screen. Handled ahead of queued output, like a key.
    pub fn mouse(&self, event: MouseEvent) -> Result<()> {
        self.send(Control::Mouse(event))
    }

    /// A wheel event: `rows` negative towards older output. The engine decides what it means (wheel reports for a
    /// program that tracks the mouse, arrow keys in the alternate screen, a local scroll otherwise; Shift always
    /// scrolls locally) and at most ten reports or arrow sequences come out of one event. Handled ahead of queued
    /// output, like a key.
    pub fn wheel(&self, event: WheelEvent) -> Result<()> {
        let rows = event.rows.clamp(-(MAX_SCROLL_ROWS as i32), MAX_SCROLL_ROWS as i32);
        self.send(Control::Wheel(WheelEvent { rows, ..event }))
    }

    /// Committed text (an IME commit, a dead-key result) as input. At most [`MAX_TEXT_BYTES`].
    pub fn text(&self, text: String) -> Result<()> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(Error::Pty("text too long".into()));
        }
        self.send(Control::Text(text))
    }

    /// Send a request and wait for the engine thread's answer, at most [`REPLY_TIMEOUT`].
    fn request<T>(&self, make: impl FnOnce(Sender<Result<T>>) -> Control) -> Result<T> {
        let (tx, rx) = mpsc::channel();
        self.send(make(tx))?;
        match rx.recv_timeout(REPLY_TIMEOUT) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => Err(Error::Pty("pane busy".into())),
            // The pane ended with the request still queued.
            Err(RecvTimeoutError::Disconnected) => Err(Error::Pty("the pane is closed".into())),
        }
    }

    /// Paste clipboard text the way the program asked for it (bracketed when it enabled mode 2004).
    ///
    /// Waits for the engine thread's answer for up to two seconds and gives `Error::Pty("pane busy")` after that,
    /// so it must not be called on the Tauri main thread. [`PasteOutcome::Rejected`] means the text could inject
    /// commands and nothing was written; the caller asks the user and repeats the call with `allow_unsafe`. Text
    /// over [`MAX_PASTE_BYTES`] is refused here.
    pub fn paste(&self, text: String, allow_unsafe: bool) -> Result<PasteOutcome> {
        if text.len() > MAX_PASTE_BYTES {
            return Err(Error::Pty(format!("paste too large (more than {MAX_PASTE_BYTES} bytes)")));
        }
        self.request(|reply| Control::Paste { text, allow_unsafe, reply })
    }

    /// Change the grid. The size is clamped to a sane range first; the PTY is resized with pixel dimensions and the
    /// next frame is a full one.
    pub fn resize(&self, cols: u16, rows: u16, cell_px: (u32, u32)) -> Result<()> {
        self.send(Control::Resize {
            cols: cols.clamp(MIN_COLS, MAX_DIM),
            rows: rows.clamp(MIN_ROWS, MAX_DIM),
            cell_px: (cell_px.0.min(MAX_CELL_PX), cell_px.1.min(MAX_CELL_PX)),
        })
    }

    /// Scroll the viewport by `rows`; negative is towards older output.
    pub fn scroll(&self, rows: isize) -> Result<()> {
        self.send(Control::Scroll(rows.clamp(-MAX_SCROLL_ROWS, MAX_SCROLL_ROWS)))
    }

    /// Make the next frame a full one: the webview reconnected or saw a gap in the sequence numbers.
    pub fn request_full(&self) -> Result<()> {
        self.send(Control::RequestFull)
    }

    /// The most output bytes that were ever queued between the PTY reader and the engine thread at once. It exists
    /// for tests and diagnostics: it is how the byte bound of the queue is observed from outside.
    pub fn queue_high_water(&self) -> usize {
        self.shared.queue.high_water()
    }

    /// Scroll back to the live screen at the bottom.
    pub fn scroll_to_bottom(&self) -> Result<()> {
        self.send(Control::ScrollToBottom)
    }

    /// Give the pane new colours. The next frame is a full one carrying the new defaults.
    pub fn set_theme(&self, theme: EngineTheme) -> Result<()> {
        self.send(Control::SetTheme(theme))
    }

    /// Whether the macOS Option key acts as Alt (an ESC prefix) instead of composing characters. It applies to the
    /// keys that follow.
    pub fn set_option_as_alt(&self, on: bool) -> Result<()> {
        self.send(Control::SetOptionAsAlt(on))
    }

    /// Change the selection. The selection itself lives in the engine and arrives in the frame header.
    pub fn select(&self, op: SelectOp) -> Result<()> {
        self.send(Control::Select(op))
    }

    /// The selected text, `None` when nothing (or only blanks) is selected. Waits for the answer like
    /// [`PaneHandle::paste`] does: never call it on the Tauri main thread.
    pub fn selection_text(&self) -> Result<Option<String>> {
        self.request(Control::SelectionText)
    }

    /// Change the search and return its state. Waits for the answer like [`PaneHandle::paste`] does. Search text
    /// over 4096 bytes is refused.
    pub fn search(&self, op: SearchOp) -> Result<SearchState> {
        if matches!(&op, SearchOp::Set(text) if text.len() > MAX_SEARCH_BYTES) {
            return Err(Error::Pty("search text too long".into()));
        }
        self.request(|reply| Control::Search(op, reply))
    }

    /// Run a copy-mode operation and return its state. Waits for the answer like [`PaneHandle::paste`] does.
    pub fn copy_mode(&self, op: CopyOp) -> Result<CopyState> {
        self.request(|reply| Control::CopyMode(op, reply))
    }

    /// End the pane: the shell is killed and reaped on the engine thread and one `Exited` event follows. Does not
    /// wait (killing takes about 200 ms); use [`PaneHandle::join`] to wait. Closing twice is fine.
    pub fn close(&self) {
        self.shared.queue.push_control(Control::Close);
    }

    /// Wait until the engine thread has ended, which is after `Exited` was delivered. `false` when it is still
    /// running after `timeout`.
    pub fn join(&self, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            {
                let mut slot = self.shared.thread.lock().unwrap_or_else(PoisonError::into_inner);
                match slot.as_ref() {
                    None => return true,
                    Some(handle) if handle.is_finished() => {
                        let handle = slot.take();
                        drop(slot);
                        if let Some(handle) = handle {
                            let _ = handle.join();
                        }
                        return true;
                    }
                    Some(_) => {}
                }
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return false;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
}

/// The path to the PTY writer thread. Everything for the child goes through it.
struct Writer {
    tx: Sender<Vec<u8>>,
    /// Bytes sent and not yet written.
    pending: Arc<AtomicUsize>,
}

impl Writer {
    /// Bytes the user typed or pasted: always queued.
    fn input(&self, bytes: Vec<u8>) {
        self.push(bytes);
    }

    /// An engine reply to a query the program made: dropped, whole, when the writer is [`REPLY_BACKLOG`] behind.
    fn reply(&self, bytes: Vec<u8>) {
        if self.pending.load(Ordering::Relaxed) >= REPLY_BACKLOG {
            return;
        }
        self.push(bytes);
    }

    fn push(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        let n = bytes.len();
        self.pending.fetch_add(n, Ordering::Relaxed);
        if self.tx.send(bytes).is_err() {
            self.pending.fetch_sub(n, Ordering::Relaxed);
        }
    }
}

/// Read the PTY into the queue until the end of the output.
///
/// After the queue was closed (the pane is being closed) the thread keeps reading and throws the output away until
/// the end or until `reaped` is set. That is not tidiness: a child that is being killed while its output is
/// unread cannot finish exiting on macOS, because closing a tty waits for its output to drain, and `Pty::close`
/// waits for the child. Draining is what lets a shell be closed in the middle of a flood.
fn reader_main(mut reader: Box<dyn Read + Send>, queue: Arc<PaneQueue<Control>>, reaped: Arc<AtomicBool>) {
    let mut buf = vec![0u8; READ_CHUNK];
    let mut draining = false;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if !draining && !queue.push_output(buf[..n].to_vec()) {
                    draining = true;
                }
                if draining && reaped.load(Ordering::Relaxed) {
                    return;
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            // macOS reports the end of a PTY as EIO; any error is the end of the output.
            Err(_) => break,
        }
    }
    // Not a control message: those are delivered before queued output and would drop the shell's last output.
    queue.finish();
}

fn writer_main(mut out: Box<dyn Write + Send>, rx: Receiver<Vec<u8>>, pending: Arc<AtomicUsize>) {
    for bytes in rx {
        let n = bytes.len();
        let written = out.write_all(&bytes).and_then(|()| out.flush()).is_ok();
        pending.fetch_sub(n, Ordering::Relaxed);
        if !written {
            break;
        }
    }
    // `out` drops here. The engine thread only drops its sender after the child was reaped, so the newline and EOF
    // the writer's Drop sends reach a PTY nobody is listening on.
}

/// Everything the engine thread owns.
struct Actor<E: TerminalEngine, S: FnMut(PaneEvent)> {
    engine: E,
    /// `None` once the child has been closed.
    pty: Option<Pty>,
    encoder: FrameEncoder,
    writer: Writer,
    queue: Arc<PaneQueue<Control>>,
    /// Set once the child was reaped: the reader thread may stop draining then.
    reaped: Arc<AtomicBool>,
    sink: S,
    option_as_alt: bool,
    interval: Duration,
    /// Something changed since the last frame.
    dirty: bool,
    last_emit: Option<Instant>,
    /// The engine has a selection that this actor made (or believes it made): input clears it.
    selecting: bool,
    /// The process id of the shell, for the working-directory lookup.
    pid: Option<u32>,
    cwd: CwdTracker,
}

impl<E: TerminalEngine, S: FnMut(PaneEvent)> Actor<E, S> {
    /// The main loop. Returns when the pane is to end.
    fn run(&mut self) {
        loop {
            let mut wait = match (self.dirty, self.last_emit) {
                (true, Some(last)) => self.interval.saturating_sub(last.elapsed()),
                (true, None) => Duration::ZERO,
                (false, _) => Duration::from_secs(3600),
            };
            if let Some(deadline) = self.cwd.deadline() {
                wait = wait.min(deadline.saturating_duration_since(Instant::now()));
            }
            match self.queue.pop(wait) {
                Popped::Control(Control::Close) | Popped::Closed => return,
                Popped::Control(control) => self.handle(control),
                Popped::Output(chunk) => self.feed(&chunk),
                Popped::Finished => {
                    // Everything the shell wrote was fed: show it before the pane goes.
                    if self.dirty {
                        self.emit_frame();
                    }
                    return;
                }
                Popped::Timeout => {}
            }
            if self.dirty && self.last_emit.is_none_or(|last| last.elapsed() >= self.interval) {
                self.emit_frame();
            }
            if self.cwd.begin(Instant::now()) {
                self.look_up_cwd();
            }
        }
    }

    /// Ask for the shell's working directory without waiting for it. The lookup starts a process that can stall
    /// (a stale network mount), so it runs on a thread of its own and its answer comes back as
    /// [`Control::CwdFound`]: keys, resizes and frames of the pane never wait for it (WR-04).
    fn look_up_cwd(&mut self) {
        let Some(pid) = self.pid else {
            self.report_cwd(None);
            return;
        };
        let queue = self.queue.clone();
        let started = thread::Builder::new().name("holzwave-cwd".into()).spawn(move || {
            // A closed queue means the pane is gone: nobody wants the answer.
            queue.push_control(Control::CwdFound(process_cwd(pid)));
        });
        if started.is_err() {
            self.report_cwd(None);
        }
    }

    /// The end of a lookup: report the directory when it differs from the last one reported.
    fn report_cwd(&mut self, found: Option<PathBuf>) {
        if let Some(dir) = self.cwd.finish(found) {
            (self.sink)(PaneEvent::Cwd(dir));
        }
    }

    fn handle(&mut self, control: Control) {
        match control {
            Control::Key(dom) => self.key(&dom),
            Control::Text(text) => self.text(&text),
            Control::Paste { text, allow_unsafe, reply } => {
                let outcome = self.engine.paste(&text, allow_unsafe);
                // A paste comes out of the same buffer as the answers to queries: take it now, so nothing that
                // happens later can pick it up again.
                let bytes = self.engine.take_replies();
                if matches!(outcome, Ok(PasteOutcome::Written)) {
                    self.clear_selection();
                    self.engine.scroll_to_bottom();
                    self.dirty = true;
                    self.writer.input(bytes);
                } else {
                    // Nothing was pasted; whatever else was in the buffer is an engine reply.
                    self.writer.reply(bytes);
                }
                let _ = reply.send(outcome);
            }
            Control::Resize { cols, rows, cell_px } => self.resize(cols, rows, cell_px),
            Control::Scroll(rows) => {
                self.engine.scroll(rows);
                self.dirty = true;
            }
            Control::Mouse(event) => {
                // Input for the program, not typing: the selection stays and the viewport stays where it is. The
                // reply of the program arrives as output.
                if let Ok(bytes) = self.engine.mouse(&event) {
                    self.writer.input(bytes);
                }
            }
            Control::Wheel(event) => match self.engine.wheel(&event) {
                Ok(WheelOutcome::Bytes(bytes)) => self.writer.input(bytes),
                Ok(WheelOutcome::Scrolled) => self.dirty = true,
                Ok(WheelOutcome::Nothing) | Err(_) => {}
            },
            Control::ScrollToBottom => {
                self.engine.scroll_to_bottom();
                self.dirty = true;
            }
            Control::CwdFound(found) => self.report_cwd(found),
            Control::RequestFull => {
                self.encoder.request_full();
                self.engine.request_full();
                self.dirty = true;
            }
            Control::SetTheme(theme) => {
                if self.engine.set_theme(&theme).is_ok() {
                    self.dirty = true;
                }
            }
            Control::SetOptionAsAlt(on) => {
                self.option_as_alt = on;
                self.engine.set_option_as_alt(on);
            }
            Control::Select(op) => {
                if self.engine.select(op).is_ok() {
                    self.selecting = !matches!(op, SelectOp::Clear);
                    self.dirty = true;
                }
            }
            Control::SelectionText(reply) => {
                let _ = reply.send(self.engine.selection_text());
            }
            Control::Search(op, reply) => {
                let state = self.engine.search(op);
                // The matches are part of the next frame.
                self.dirty = true;
                let _ = reply.send(state);
            }
            Control::CopyMode(op, reply) => {
                let state = self.engine.copy_mode(op);
                self.dirty = true;
                let _ = reply.send(state);
            }
            // `run` ends the loop on Close before it gets here.
            Control::Close => {}
        }
    }

    /// Program output into the engine; its answers to the program's queries go back through the writer.
    fn feed(&mut self, bytes: &[u8]) {
        self.engine.feed(bytes);
        let replies = self.engine.take_replies();
        self.writer.reply(replies);
        self.cwd.output(Instant::now());
        self.drain_events();
        self.dirty = true;
    }

    fn key(&mut self, dom: &DomKey) {
        let event = KeyEvent::from_dom(dom, self.option_as_alt);
        let Ok(bytes) = self.engine.encode_key(&event) else { return };
        // A modifier on its own, a release or a composing key encodes to nothing: it moves nothing.
        if bytes.is_empty() {
            return;
        }
        if !dom.release {
            // Typing replaces the selection and returns to the live screen, like every terminal.
            self.clear_selection();
            self.engine.scroll_to_bottom();
            self.dirty = true;
        }
        self.writer.input(bytes);
    }

    fn text(&mut self, text: &str) {
        let Ok(bytes) = self.engine.encode_text(text) else { return };
        if bytes.is_empty() {
            return;
        }
        self.clear_selection();
        self.engine.scroll_to_bottom();
        self.dirty = true;
        self.writer.input(bytes);
    }

    /// Input replaces the selection. Only asks the engine when this actor made one.
    fn clear_selection(&mut self) {
        if self.selecting {
            let _ = self.engine.select(SelectOp::Clear);
            self.selecting = false;
            self.dirty = true;
        }
    }

    fn resize(&mut self, cols: u16, rows: u16, cell_px: (u32, u32)) {
        let before = self.engine.size().ok();
        if self.engine.resize(cols, rows, cell_px).is_err() {
            return;
        }
        if let Some(pty) = &self.pty {
            // The child learns the size in cells and in pixels (programs that draw images read the latter).
            let px_w = cell_px.0.saturating_mul(u32::from(cols));
            let px_h = cell_px.1.saturating_mul(u32::from(rows));
            let _ = pty.resize_px(cols, rows, px_w, px_h);
        }
        if self.engine.size().ok() != before {
            // A different grid is a reset for the webview.
            self.encoder.request_full();
        }
        self.dirty = true;
    }

    fn drain_events(&mut self) {
        for event in self.engine.take_events() {
            match event {
                EngineEvent::Title(title) => (self.sink)(PaneEvent::Title(title)),
                EngineEvent::Pwd(raw) => {
                    // A program (a shell with integration, or something on the far side of ssh) announced its
                    // directory. Only a well-formed `file://` path counts; it is the fallback of the lookup.
                    if let Some((_host, path)) = parse_osc7(&raw) {
                        self.cwd.announced(path.to_string_lossy().into_owned(), Instant::now());
                    }
                }
            }
        }
    }

    /// Look at the engine and send a frame when something differs from the last one sent.
    ///
    /// The pacing clock is the last frame that was actually *sent*: an attempt that finds nothing new (a key that
    /// only moved the viewport it was already at, an update after output that left every row as it was) must not
    /// use up the interval, or the echo of that key, arriving a fraction of a millisecond later, would wait for it.
    fn emit_frame(&mut self) {
        self.dirty = false;
        match self.engine.update() {
            Ok(update) => {
                if let Some(frame) = self.encoder.encode(update) {
                    (self.sink)(PaneEvent::Frame(frame));
                    self.last_emit = Some(Instant::now());
                }
            }
            // No frame is the symptom of this; without a trace the pane just looks frozen.
            Err(e) => eprintln!("holzwave: pane update failed: {e}"),
        }
    }

    /// Close the child and report the end. The one place a pane ends.
    fn finish(&mut self) {
        // Refuse further input and release a reader blocked on a full queue.
        self.queue.close();
        // Kill and reap; the writer is dropped only after this. The reader thread keeps draining meanwhile.
        let code = self.pty.take().and_then(|pty| pty.close().ok());
        self.reaped.store(true, Ordering::Relaxed);
        (self.sink)(PaneEvent::Exited { code });
        // Callers waiting for an answer must not wait for their timeout: dropping what is left drops their
        // reply channels.
        while !matches!(self.queue.pop(Duration::ZERO), Popped::Closed | Popped::Timeout) {}
    }
}

impl<E: TerminalEngine, S: FnMut(PaneEvent)> Drop for Actor<E, S> {
    fn drop(&mut self) {
        // Only reached with a child still open when the engine thread unwinds (a panicking sink): never leave it.
        self.queue.close();
        if let Some(pty) = self.pty.take() {
            let _ = pty.close();
        }
        self.reaped.store(true, Ordering::Relaxed);
    }
}

/// Build the engine and the shell. Nothing is started when the engine cannot be built.
fn start<E, F>(spec: &PaneSpec, make_engine: F) -> Result<(E, Pty)>
where
    E: TerminalEngine,
    F: FnOnce(&PaneSpec) -> Result<E>,
{
    let mut engine = make_engine(spec)?;
    if spec.cell_px != (0, 0) {
        // An engine learns the size of a cell only through a resize. Without this, a size report the program asks
        // for before the first resize of the pane says 0 x 0 although the spec knew the real size.
        let _ = engine.resize(spec.cols, spec.rows, spec.cell_px);
    }
    engine.set_option_as_alt(spec.option_as_alt);
    if let Some(theme) = &spec.theme {
        // A theme that cannot be applied is not a reason to have no terminal.
        let _ = engine.set_theme(theme);
    }
    let mut options = SpawnOptions::new(spec.program.clone(), spec.cols, spec.rows)
        .args(spec.args.iter().cloned())
        .cell_px(spec.cell_px.0, spec.cell_px.1);
    if let Some(dir) = &spec.cwd {
        options = options.cwd(dir.clone());
    }
    let pty = Pty::spawn_with(&options)?;
    Ok((engine, pty))
}

fn engine_main<E, F, S>(
    pane: PaneId,
    spec: PaneSpec,
    make_engine: F,
    mut sink: S,
    queue: Arc<PaneQueue<Control>>,
    started: Sender<Result<Option<u32>>>,
) where
    E: TerminalEngine,
    F: FnOnce(&PaneSpec) -> Result<E>,
    S: FnMut(PaneEvent),
{
    let fail = |sink: &mut S, queue: &PaneQueue<Control>, error: Error| {
        queue.close();
        // The sink hears it before the caller of `spawn` gets the error.
        sink(PaneEvent::Failed(error.to_string()));
        let _ = started.send(Err(error));
    };

    let (engine, mut pty) = match start(&spec, make_engine) {
        Ok(started) => started,
        Err(e) => return fail(&mut sink, &queue, e),
    };
    let pid = pty.process_id();
    let (Some(reader), Some(writer)) = (pty.take_reader(), pty.take_writer()) else {
        let _ = pty.close();
        return fail(&mut sink, &queue, Error::Pty("the PTY has no reader or writer".into()));
    };

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let pending = Arc::new(AtomicUsize::new(0));
    let reader_queue = queue.clone();
    let reaped = Arc::new(AtomicBool::new(false));
    let reader_reaped = reaped.clone();
    let writer_pending = pending.clone();
    let threads = thread::Builder::new()
        .name(format!("hw-pane-{pane}-reader"))
        .spawn(move || reader_main(reader, reader_queue, reader_reaped))
        .and_then(|_| {
            thread::Builder::new()
                .name(format!("hw-pane-{pane}-writer"))
                .spawn(move || writer_main(writer, rx, writer_pending))
        });
    if let Err(e) = threads {
        let _ = pty.close();
        return fail(&mut sink, &queue, Error::Io(e));
    }

    let mut actor = Actor {
        engine,
        pty: Some(pty),
        encoder: FrameEncoder::new(pane),
        writer: Writer { tx, pending },
        queue,
        reaped,
        sink,
        option_as_alt: spec.option_as_alt,
        interval: spec.frame_interval,
        // The first frame goes out at once, before the shell has printed anything: the webview learns the grid, the
        // colours and the cursor of a pane that is still silent.
        dirty: true,
        last_emit: None,
        selecting: false,
        pid,
        cwd: CwdTracker::new(Instant::now()),
    };
    let _ = started.send(Ok(pid));
    actor.run();
    actor.finish();
}

/// How long the output must have been quiet before the working directory is looked up.
const CWD_IDLE: Duration = Duration::from_millis(150);

/// The least time between two working-directory lookups of one pane.
const CWD_MIN_GAP: Duration = Duration::from_secs(1);

/// Decides when to look up the shell's working directory and what to report. Pure: the caller passes the time and
/// the lookup, so the rules are unit-tested without a shell.
///
/// The lookup (`process_cwd`) can start a process, so it is rationed: it runs only when the output has been quiet
/// for [`CWD_IDLE`] (a prompt was drawn; a flood never gets one) and at most once per [`CWD_MIN_GAP`]. A result
/// that equals the last one reported is not reported again. One lookup is in flight at a time: [`begin`] starts
/// it and [`finish`] takes its answer, whenever that comes.
///
/// [`begin`]: CwdTracker::begin
/// [`finish`]: CwdTracker::finish
struct CwdTracker {
    /// When the next check is wanted: [`CWD_IDLE`] after the last output.
    due: Option<Instant>,
    last_lookup: Option<Instant>,
    /// A lookup was started and its answer has not come yet.
    in_flight: bool,
    /// What was last reported.
    reported: Option<String>,
    /// The last directory a program announced with OSC 7.
    osc7: Option<String>,
}

impl CwdTracker {
    /// A tracker that wants a first look [`CWD_IDLE`] after `now`, so that a program that never prints still gets
    /// its starting directory reported.
    fn new(now: Instant) -> Self {
        Self { due: Some(now + CWD_IDLE), last_lookup: None, in_flight: false, reported: None, osc7: None }
    }

    /// Output arrived: the check moves to [`CWD_IDLE`] after it.
    fn output(&mut self, now: Instant) {
        self.due = Some(now + CWD_IDLE);
    }

    /// A program announced its directory (OSC 7): remember it as the fallback and look soon.
    fn announced(&mut self, path: String, now: Instant) {
        self.osc7 = Some(path);
        self.due = Some(now);
    }

    /// When the next lookup may run, `None` when none is wanted or one is still in flight (its answer wakes the
    /// loop as a message).
    fn deadline(&self) -> Option<Instant> {
        if self.in_flight {
            return None;
        }
        let due = self.due?;
        Some(match self.last_lookup {
            Some(last) => due.max(last + CWD_MIN_GAP),
            None => due,
        })
    }

    /// Start a lookup if one is due at `now`: true means the caller runs it and passes the answer to
    /// [`finish`](Self::finish), whatever it is.
    fn begin(&mut self, now: Instant) -> bool {
        if self.deadline().is_none_or(|deadline| now < deadline) {
            return false;
        }
        self.due = None;
        self.last_lookup = Some(now);
        self.in_flight = true;
        true
    }

    /// The answer of the lookup that [`begin`](Self::begin) started. Returns the directory to report when it
    /// differs from the last one reported.
    fn finish(&mut self, found: Option<PathBuf>) -> Option<String> {
        self.in_flight = false;
        // The process is the truth; what the program announced is only for when the process cannot be read.
        let current = found.map(|p| p.to_string_lossy().into_owned()).or_else(|| self.osc7.clone())?;
        if self.reported.as_deref() == Some(current.as_str()) {
            return None;
        }
        self.reported = Some(current.clone());
        Some(current)
    }

    /// A lookup that is done in place: [`begin`](Self::begin) and [`finish`](Self::finish) in one call, with
    /// `lookup` called at most once and only when due. The rules are tested through it without a thread.
    #[cfg(test)]
    fn check(&mut self, now: Instant, lookup: impl FnOnce() -> Option<PathBuf>) -> Option<String> {
        if !self.begin(now) {
            return None;
        }
        self.finish(lookup())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn at(base: Instant, ms: u64) -> Instant {
        base + MS * ms as u32
    }

    #[test]
    fn nothing_is_looked_up_until_the_output_has_been_quiet_for_150_ms() {
        let base = Instant::now();
        let mut tracker = CwdTracker::new(base);
        tracker.output(at(base, 10));

        assert_eq!(tracker.check(at(base, 100), || Some("/a".into())), None, "only 90 ms after the output");
        assert_eq!(tracker.check(at(base, 170), || Some("/a".into())), Some("/a".to_string()));
    }

    #[test]
    fn more_output_pushes_the_check_back() {
        let base = Instant::now();
        let mut tracker = CwdTracker::new(base);
        tracker.output(at(base, 0));
        tracker.output(at(base, 100));
        tracker.output(at(base, 200));

        assert_eq!(tracker.check(at(base, 300), || Some("/a".into())), None, "100 ms after the last output");
        assert_eq!(tracker.check(at(base, 360), || Some("/a".into())), Some("/a".to_string()));
    }

    #[test]
    fn there_is_at_most_one_lookup_per_second_and_a_postponed_one_still_happens() {
        let base = Instant::now();
        let mut tracker = CwdTracker::new(base);
        let mut lookups = 0;
        tracker.output(at(base, 0));
        assert_eq!(
            tracker.check(at(base, 200), || {
                lookups += 1;
                Some("/a".into())
            }),
            Some("/a".to_string())
        );

        // New output and a changed directory 300 ms later: due at 450 ms, but a lookup ran at 200 ms.
        tracker.output(at(base, 300));
        assert_eq!(tracker.deadline(), Some(at(base, 1200)), "postponed to one second after the last lookup");
        assert_eq!(
            tracker.check(at(base, 500), || {
                lookups += 1;
                Some("/b".into())
            }),
            None
        );
        assert_eq!(lookups, 1, "no second lookup inside the same second");
        assert_eq!(
            tracker.check(at(base, 1250), || {
                lookups += 1;
                Some("/b".into())
            }),
            Some("/b".to_string())
        );
        assert_eq!(lookups, 2);
    }

    #[test]
    fn a_lookup_in_flight_is_not_started_again_and_does_not_wake_the_loop() {
        // The answer comes later, from another thread (WR-04): until it does, no second lookup starts and the
        // loop has no deadline to wake for, however much output arrives.
        let base = Instant::now();
        let mut tracker = CwdTracker::new(base);
        tracker.output(at(base, 0));
        assert!(tracker.begin(at(base, 200)));
        assert_eq!(tracker.deadline(), None);

        tracker.output(at(base, 300));
        assert_eq!(tracker.deadline(), None, "output during a lookup does not make the next one due yet");
        assert!(!tracker.begin(at(base, 5000)), "a lookup is still in flight");

        assert_eq!(tracker.finish(Some("/a".into())), Some("/a".to_string()));
        assert_eq!(tracker.deadline(), Some(at(base, 1200)), "the output of the meantime is looked at, a second later");
        assert!(tracker.begin(at(base, 1200)));
        assert_eq!(tracker.finish(Some("/a".into())), None, "an unchanged directory is not reported again");
    }

    #[test]
    fn a_lookup_that_found_nothing_ends_the_flight_and_falls_back_to_osc7() {
        let base = Instant::now();
        let mut tracker = CwdTracker::new(base);
        tracker.announced("/remote".into(), at(base, 0));
        assert!(tracker.begin(at(base, 0)));
        assert_eq!(tracker.finish(None), Some("/remote".to_string()));
        assert!(!tracker.begin(at(base, 10)), "nothing is due after the answer");
    }

    #[test]
    fn an_unchanged_directory_is_reported_once() {
        let base = Instant::now();
        let mut tracker = CwdTracker::new(base);
        tracker.output(at(base, 0));
        assert_eq!(tracker.check(at(base, 200), || Some("/a".into())), Some("/a".to_string()));
        tracker.output(at(base, 1300));
        assert_eq!(tracker.check(at(base, 1500), || Some("/a".into())), None);
    }

    #[test]
    fn the_last_osc7_value_is_the_fallback_when_the_lookup_yields_nothing() {
        let base = Instant::now();
        let mut tracker = CwdTracker::new(base);
        tracker.announced("/remote/project".into(), at(base, 0));
        assert_eq!(tracker.check(at(base, 10), || None), Some("/remote/project".to_string()));

        // A lookup that works beats what the program announced.
        tracker.announced("/remote/other".into(), at(base, 2000));
        assert_eq!(tracker.check(at(base, 2010), || Some("/local".into())), Some("/local".to_string()));
    }

    #[test]
    fn with_no_lookup_and_no_announcement_nothing_is_reported() {
        let base = Instant::now();
        let mut tracker = CwdTracker::new(base);
        tracker.output(at(base, 0));
        assert_eq!(tracker.check(at(base, 200), || None), None);
        assert_eq!(tracker.deadline(), None, "a check that ran is not due again until there is output");
    }
}
