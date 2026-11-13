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
//! # Taps
//!
//! A tap is a second reader of the bytes the engine is fed: [`PaneHandle::attach_tap`] takes a VT snapshot of the screen
//! ([`TerminalEngine::snapshot_vt`]) and registers the tap in one step of the engine thread, so no byte of the program
//! falls between the snapshot and the stream and none is in both. A client that feeds the snapshot and then every
//! [`TapMsg::Bytes`] to an engine of its own ends up with the terminal of the pane. Each tap has a queue bounded in bytes
//! that the engine thread never waits for: a tap that cannot keep up is removed and told so ([`TapEnd::Overflow`]).
//!
//! # Ending
//!
//! A pane ends in one of four ways and all of them take the same path: [`PaneHandle::close`] (or the last handle
//! being dropped), the queue reporting the end of the output (the shell exited), the exit poll finding the shell
//! exited although the output never ended (Windows), or a failed start. The engine thread
//! closes the queue, kills and reaps the child, emits **one** [`PaneEvent::Exited`], and only then drops the writer
//! channel, so the PTY writer (whose `Drop` writes a newline and EOF) goes last. Nothing here can make the
//! application exit: that decision is the shell's, made from the `Exited` event.
//!
//! The sink runs on the engine thread. It may be called before [`PaneHandle::spawn`] has returned (a failed start
//! reports `Failed` first), so it must not wait for the handle.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::attention::Gate;
use crate::engine::{
    CopyOp, CopyState, DomKey, EngineEvent, EngineTheme, ImageChunk, KeyEvent, MAX_IMAGE_CHUNK_BYTES, Mods, MouseEvent,
    PasteOutcome, SearchOp, SearchState, SelectOp, TerminalEngine, WheelEvent, WheelOutcome, parse_osc7,
};
use crate::layout::PaneId;
use crate::platform::{self, Os, ShellLaunch};
use crate::conn::{ConnState, Link, RemoteControl, RemoteView, SharedState};
use crate::remote::proto::WriterView;
use crate::pty::{Pty, SpawnOptions, process_cwd};
use crate::pool::Connection;
use crate::ssh::{SshLink, SshTarget, Tunnel};
use crate::queue::{PaneQueue, Popped};
use crate::textread::{MAX_READ_BYTES, MAX_READ_ROWS, TextRead, TextRegion};
use crate::wire::{FrameEncoder, FrameMsg};
use crate::winshell::announced_path;
use crate::{Error, Result};

/// The default number of scrollback lines of a pane.
pub const DEFAULT_SCROLLBACK: usize = 10_000;

/// The default memory of a pane for pictures (the Kitty graphics protocol), 64 MiB. It is the default of the
/// `image_memory_mib` setting and, with the `ghostty` feature, the same number as the engine's own default.
pub const DEFAULT_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// The most memory a pane may be given for pictures, 512 MiB: the top of the `image_memory_mib` range. A spec that asks
/// for more is cut to it when the pane starts, so a caller that did not read the setting cannot ask for terabytes.
pub const MAX_IMAGE_BYTES: u64 = 512 * 1024 * 1024;

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
pub const MIN_COLS: u16 = 2;
pub const MIN_ROWS: u16 = 1;
pub const MAX_DIM: u16 = 1000;

/// How long `spawn` waits for the engine thread to report that the shell started. Starting a PTY takes
/// milliseconds; this only stops a caller (possibly the main thread) from hanging forever.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// The size of one read from the PTY.
const READ_CHUNK: usize = 64 * 1024;

/// How long a pane whose shell was found exited by the poll keeps feeding the output that is still on its way, so
/// that the shell's last words are in the last frame. The output pipe has no end to wait for, so this is a bound.
const EXIT_GRACE: Duration = Duration::from_millis(150);

/// How long a pane whose output ended waits for the child's exit status to become readable before it holds the pane
/// without one ([`PaneSpec::linger`]). The status is there a few milliseconds after the output ends; this only bounds a
/// child that closed its terminal and runs on.
const STATUS_SETTLE: Duration = Duration::from_millis(500);

/// What a held pane does to the terminal modes before its notice: the program's mouse and focus reporting end, so that
/// the person can select and copy the error line with the mouse. Nothing else of the screen changes.
const HELD_MODES_RESET: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1006l";

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
    /// The bytes of pictures the engine keeps for this pane, per screen; `0` keeps none. The default is
    /// [`DEFAULT_IMAGE_BYTES`].
    pub image_bytes: u64,
    /// Whether animated pictures may move from the start. `true` (the default) leaves the engine as it is; `false` pauses
    /// them before the shell is started, so a pane opened while the page asks for reduced motion never plays one frame.
    /// [`PaneHandle::set_image_animation`] changes it later.
    pub image_animation: bool,
    pub option_as_alt: bool,
    pub theme: Option<EngineTheme>,
    /// The least time between two frames.
    pub frame_interval: Duration,
    /// How often the pane asks the shell whether it has exited, besides waiting for the end of its output. `None`
    /// does not ask: the idle pane then wakes for nothing. The default is [`platform::exit_poll`] for this OS, which
    /// is `Some` on Windows only: a pseudo console before Windows 11 24H2 never ends its output pipe.
    pub exit_poll: Option<Duration>,
    /// One line the pane shows before the shell's own output (and so first): what the app has to tell about this
    /// start, such as an unusable `shell` setting that was replaced. It goes to the screen only, never to the shell.
    pub notice: Option<String>,
    /// Start the shell on the host through `flatpak-spawn --host` ([`platform::host_command`]): the program, the
    /// arguments and the directory of the spec are wrapped, and the child process's own working directory is not set.
    /// `false` (the default) everywhere except inside a Flatpak, and there only when the person turned Host shell on and
    /// the Flatpak has the permission to run commands on the host: in every other case a Flatpak pane starts in the sandbox.
    pub host_spawn: bool,
    /// Ask the operating system for the shell's working directory ([`process_cwd`]). `true` (the default) everywhere
    /// except in host-spawn mode, where the process in the sandbox is `flatpak-spawn` and its directory means
    /// nothing: the directory then comes from the shell's OSC 7 reports only.
    pub cwd_lookup: bool,
    /// Run the pane on another machine over SSH instead of a local shell. `program`, `args`, `cwd` and `host_spawn`
    /// are then not used. The pane's own thread reconnects when the connection is lost.
    pub ssh: Option<SshTarget>,
    /// The connection of a saved SSH profile that this pane runs on, shared with the file browser and the commands of
    /// the same profile (one login). When it is set the shell takes a channel of it; otherwise `ssh` is connected on a
    /// connection of the pane's own. [`crate::connections::Connections::apply`] sets both.
    pub ssh_connection: Option<Arc<Connection>>,
    /// The tunnel of an SSH pane that runs on `ssh_connection`: the socket forwarded back from the host and the
    /// environment its shell gets ([`SshLink::attach_with`]). `None` (the default) starts a plain shell; it is not used
    /// without `ssh_connection`. Its `Debug` prints neither the socket path nor the token.
    pub tunnel: Option<Tunnel>,
    /// No process at all ([`crate::conn::NullLink`]): the page draws this pane's content itself (a file browser, a
    /// preview). The pane is otherwise a pane like any other: it has a header, can be split, moved and closed.
    pub blank: bool,
    /// Extra environment variables of the local shell (`HOLZWAVE_IPC`, `HOLZWAVE_TOKEN`, `HOLZWAVE_PANE`: what `wsh` finds the
    /// app by). Not used for an SSH pane: a remote host reaches the app through `tunnel`.
    pub env: Vec<(String, String)>,
    /// Hold the pane when its program ends instead of closing it at once. `Some(time)`: a program that ends with a
    /// status that is not zero, with no readable status, or within `time` of the start, leaves the pane open with one
    /// notice line (`[holzWave] exited with status N. Press Enter to close.`), one [`PaneEvent::Held`], and no input
    /// but Enter and a close request; the pane then ends through the one `Exited`. A program that ran longer than
    /// `time` and ended with status 0 closes the pane at once, as it does without this. `None` (the default) never
    /// holds: the pane ends with its program. It is for a pane whose program is a tool and whose last line explains a
    /// failure: a pane that vanished with it would leave nothing to read.
    pub linger: Option<Duration>,
    /// Names the program does not inherit from the app's environment (see [`SpawnOptions::env_remove`]): applied before
    /// `env`, so a name that is in both is set. Empty (the default) removes nothing. Not used for an SSH pane.
    pub env_remove: Vec<String>,
    /// The pane runs a program and keeps its engine but draws for no page: it encodes no frame and emits no
    /// [`PaneEvent::Frame`], and an idle pane sets no timer for frames or pictures. It still emits `Title`, `Cwd`, `Bell`,
    /// `CommandEnd`, `Held`, `Exited` and `Failed`. It is the pane of a session that lives on a server and is seen only
    /// through taps ([`PaneHandle::attach_tap`]); the only pane that accepts raw input bytes ([`PaneHandle::write_raw`]).
    /// `false` by default.
    pub headless: bool,
    /// Show a session that lives on a holzWave server instead of running a program: the pane's own engine draws what the
    /// server sends ([`RemoteTarget`]). `program`, `args`, `cwd`, `ssh` and `host_spawn` are then not used, as for SSH.
    /// `None` by default.
    pub remote: Option<RemoteTarget>,
}

/// What builds the link of a pane that shows a session on a server. The hub of a device ([`crate::remote::hub::Hub`]) is the
/// one implementation of this crate; nothing in the pane names a socket.
pub trait RemoteAttach: Send + Sync {
    /// The link of a pane for `session`: the screen and `tail` rows of scrollback (the server's default when `None`), then live
    /// bytes. `size` is the grid of the pane, in cells; `sink` is how the link reaches the pane's engine thread. Returns at
    /// once: connecting happens on a thread of the implementation and its progress is the link's state.
    fn link(&self, session: u32, tail: Option<u32>, size: (u16, u16), sink: PaneSink) -> Result<Box<dyn Link>>;

    /// The name of the server, for a debug print. Never a token and never more than the host.
    fn host(&self) -> String;
}

/// A session on a server, as a pane's spec names it.
#[derive(Clone)]
pub struct RemoteTarget {
    /// The number of the session on the server.
    pub session: u32,
    /// How many rows of scrollback come with the screen; `None` is the server's default.
    pub tail: Option<u32>,
    /// What builds the link.
    pub attach: Arc<dyn RemoteAttach>,
}

impl std::fmt::Debug for RemoteTarget {
    // A spec derives `Debug`: a debug print shows the session and the host of the server and nothing that could be used to reach
    // it as someone else.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteTarget").field("session", &self.session).field("host", &self.attach.host()).finish()
    }
}

/// How a link to a session on a server reaches the engine thread of its pane: what the pane does not learn from the bytes.
/// It is made by [`PaneHandle::spawn`], which owns the pane's queue, and handed to the link when the pane starts, so the code
/// that starts a pane wires nothing.
#[derive(Clone)]
pub struct PaneSink {
    queue: Arc<PaneQueue<Control>>,
}

impl std::fmt::Debug for PaneSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PaneSink")
    }
}

/// How long [`PaneSink::settle`] waits for the engine thread to catch up with the output before it gives up.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

impl PaneSink {
    /// The grid of the session is `cols` by `rows`: the engine takes that size (clamped), without telling the link, so that a
    /// watcher follows the session and a writer's own resize is not sent back. Handled before output that is still queued,
    /// which is what a size that comes before the snapshot needs.
    pub fn grid(&self, cols: u16, rows: u16) {
        self.queue.push_control(Control::Grid { cols: cols.clamp(MIN_COLS, MAX_DIM), rows: rows.clamp(MIN_ROWS, MAX_DIM) });
    }

    /// Remember how far the viewport is from the bottom of the screen, once every byte queued so far has been fed.
    pub fn mark_scroll(&self) {
        self.settle();
        self.queue.push_control(Control::MarkScroll);
    }

    /// Put the viewport back where [`PaneSink::mark_scroll`] saw it, measured from the bottom, once every byte queued so far
    /// has been fed.
    pub fn restore_scroll(&self) {
        self.settle();
        self.queue.push_control(Control::RestoreScroll);
    }

    /// Waits until the engine thread has taken all the output that is queued, or the pane is gone, at most
    /// [`SETTLE_TIMEOUT`]. A control that is pushed after this is handled after that output, because the engine thread does
    /// one thing at a time.
    fn settle(&self) {
        let deadline = Instant::now() + SETTLE_TIMEOUT;
        while self.queue.queued_bytes() > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
    }
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
            image_bytes: DEFAULT_IMAGE_BYTES,
            image_animation: true,
            option_as_alt: false,
            theme: None,
            frame_interval: DEFAULT_FRAME_INTERVAL,
            exit_poll: platform::exit_poll(Os::current()),
            notice: None,
            host_spawn: false,
            cwd_lookup: true,
            ssh: None,
            ssh_connection: None,
            tunnel: None,
            blank: false,
            env: Vec::new(),
            linger: None,
            env_remove: Vec::new(),
            headless: false,
            remote: None,
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
    /// The program rang the bell (BEL). At most one per [`crate::attention::GAP`] per pane.
    Bell,
    /// A command ended: the `D` mark of OSC 133 with its exit status (`None` when the mark carried none). At most one
    /// per [`crate::attention::GAP`] per pane.
    CommandEnd { exit: Option<u32> },
    /// The shell ended and was reaped. Exactly once per pane that started, and the last event it sends.
    /// `code` is `None` when the exit status could not be read.
    Exited { code: Option<u32> },
    /// The program ended and the pane stays ([`PaneSpec::linger`]): its last output and a notice line are on the screen
    /// and the pane ends, with one `Exited`, on Enter or when it is closed. At most once per pane, and always before
    /// `Exited`. `code` is `None` when the exit status could not be read.
    Held { code: Option<u32> },
    /// The pane could not start. Exactly once, and no other event follows.
    Failed(String),
}

/// The least bytes a tap's queue may be given: one read of the PTY, so that a chunk always fits an empty queue.
pub const MIN_TAP_QUEUE_BYTES: usize = READ_CHUNK;

/// The most bytes a tap's queue may be given; a larger request is cut to it.
pub const MAX_TAP_QUEUE_BYTES: usize = 16 * 1024 * 1024;

/// The most taps one pane keeps. Past it an attach is refused, so a caller that never detaches cannot make the engine
/// thread copy every chunk without a bound.
pub const MAX_TAPS: usize = 32;

/// The most bytes one [`PaneHandle::write_raw`] call may hand to the program.
pub const MAX_RAW_BYTES: usize = 64 * 1024;

/// The most bytes that may wait for the program of a headless pane: raw input on its way to the PTY writer and not yet
/// written, together with the engine's own replies. A [`PaneHandle::write_raw`] call that would go past it is refused
/// whole, so a sender whose program does not read its input cannot grow the memory of the process without a bound.
pub const MAX_RAW_BACKLOG: usize = 1024 * 1024;

/// The text of the error of a [`PaneHandle::write_raw`] call that [`MAX_RAW_BACKLOG`] refused, so that a caller can tell it from a
/// pane that is closed and tell its sender.
pub const RAW_BACKLOG_FULL: &str = "the program is not reading its input";

/// The identity of a tap, for [`PaneHandle::detach_tap`]. Unique in the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TapId(u64);

impl TapId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        TapId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Why a tap ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapEnd {
    /// The reader did not keep up: its queue would have passed its bound, so the tap was removed and what was queued is
    /// gone. The bytes after the last one it got were lost, so what it built from them is wrong: the caller attaches
    /// again and starts from a fresh snapshot.
    Overflow,
    /// The pane ended, or the tap was detached. Every byte that was queued before the end is delivered first.
    Closed,
}

/// What a tap yields.
#[derive(Debug, PartialEq, Eq)]
pub enum TapMsg {
    /// Bytes the engine was fed, in order, as the reader of the PTY handed them over (at most 64 KiB each).
    Bytes(Vec<u8>),
    /// The end. Yielded once the queue is empty and again on every later call.
    End(TapEnd),
}

#[derive(Default)]
struct TapState {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    end: Option<TapEnd>,
}

struct TapShared {
    state: Mutex<TapState>,
    ready: Condvar,
    /// The receiver was dropped: nobody wants the bytes, the engine thread removes the tap.
    receiver_gone: AtomicBool,
}

impl TapShared {
    fn lock(&self) -> std::sync::MutexGuard<'_, TapState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The engine thread's end of a tap.
struct TapSink {
    id: TapId,
    shared: Arc<TapShared>,
    bound: usize,
}

impl TapSink {
    /// Hand over a chunk without waiting. `false` means the tap is finished (overflow or no receiver) and is removed.
    fn push(&self, chunk: &[u8]) -> bool {
        if self.shared.receiver_gone.load(Ordering::Relaxed) {
            return false;
        }
        let mut state = self.shared.lock();
        if state.end.is_some() {
            return false;
        }
        if state.bytes + chunk.len() > self.bound {
            // The queue is dropped with the tap: its bytes are of no use to a reader that has to start over.
            state.chunks.clear();
            state.bytes = 0;
            state.end = Some(TapEnd::Overflow);
        } else {
            state.bytes += chunk.len();
            state.chunks.push_back(chunk.to_vec());
        }
        self.shared.ready.notify_one();
        state.end.is_none()
    }

    /// The pane ended or the tap was detached: what is queued stays readable, then the end follows.
    fn close(&self, discard: bool) {
        let mut state = self.shared.lock();
        if state.end.is_none() {
            state.end = Some(TapEnd::Closed);
        }
        if discard {
            state.chunks.clear();
            state.bytes = 0;
        }
        self.shared.ready.notify_one();
    }
}

/// The reader of a tap. Dropping it detaches the tap on the engine thread's next chunk; [`PaneHandle::detach_tap`] does it
/// at once.
pub struct TapReceiver {
    shared: Arc<TapShared>,
}

impl TapReceiver {
    /// The next message, waiting at most `timeout` (`Duration::ZERO` does not wait); `None` when nothing came in time.
    pub fn recv(&self, timeout: Duration) -> Option<TapMsg> {
        let deadline = Instant::now().checked_add(timeout);
        let mut state = self.shared.lock();
        loop {
            if let Some(chunk) = state.chunks.pop_front() {
                state.bytes -= chunk.len();
                return Some(TapMsg::Bytes(chunk));
            }
            if let Some(end) = state.end {
                return Some(TapMsg::End(end));
            }
            let left = match deadline {
                Some(deadline) => deadline.saturating_duration_since(Instant::now()),
                None => Duration::from_secs(3600),
            };
            if left.is_zero() {
                return None;
            }
            state = self.shared.ready.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

impl Drop for TapReceiver {
    fn drop(&mut self) {
        self.shared.receiver_gone.store(true, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for TapReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TapReceiver").finish_non_exhaustive()
    }
}

/// What [`PaneHandle::attach_tap`] returns: the state of the terminal at one instant and the bytes that follow it.
#[derive(Debug)]
pub struct Tap {
    /// The VT bytes of the scrollback tail and the screen ([`TerminalEngine::snapshot_vt`]). Feed them to an engine of
    /// `size` first, and then every [`TapMsg::Bytes`].
    pub snapshot: Vec<u8>,
    /// The grid `(cols, rows)` of the pane at the instant of the snapshot: the client sets its own grid to it before it
    /// feeds the snapshot.
    pub size: (u16, u16),
    /// The rows of scrollback that existed at the instant of the snapshot, above the screen. A snapshot that brought fewer
    /// rows than this left older ones out: asking again with a larger tail brings them.
    pub history_rows: usize,
    pub id: TapId,
    pub rx: TapReceiver,
}

/// A line-editing step of the shell's line editor. The page names one of these, never the bytes: the core owns the
/// bytes, so the page cannot send arbitrary input through this path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditAction {
    WordBack,
    WordForward,
    LineStart,
    LineEnd,
    DeleteWordBack,
    DeleteToLineStart,
}

impl EditAction {
    /// What the shell receives. These are the readline and zsh defaults that Terminal.app and iTerm send for the same
    /// keys: ESC b and ESC f move by word, Ctrl+A and Ctrl+E go to the ends of the line, ESC DEL kills the word
    /// before the cursor and Ctrl+U kills to the start of the line.
    pub fn bytes(self) -> &'static [u8] {
        match self {
            EditAction::WordBack => b"\x1bb",
            EditAction::WordForward => b"\x1bf",
            EditAction::LineStart => b"\x01",
            EditAction::LineEnd => b"\x05",
            EditAction::DeleteWordBack => b"\x1b\x7f",
            EditAction::DeleteToLineStart => b"\x15",
        }
    }
}

/// The most bytes of a notice [`PaneHandle::notice`] shows.
pub const MAX_NOTICE_BYTES: usize = 512;

/// The bytes that show `text` as one warn-colour line of its own: control characters become spaces, so the text can never be
/// an escape sequence, and the line is cut at [`MAX_NOTICE_BYTES`] on a character boundary.
fn notice_line(text: &str) -> Vec<u8> {
    let mut clean = String::with_capacity(text.len().min(MAX_NOTICE_BYTES));
    for c in text.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if clean.len() + c.len_utf8() > MAX_NOTICE_BYTES {
            break;
        }
        clean.push(c);
    }
    format!("\r\n\x1b[33m{clean}\x1b[0m\r\n").into_bytes()
}

/// Whether a key closes a held pane: Enter, or the one on the numeric pad, pressed once with no modifier.
fn is_enter(key: &DomKey) -> bool {
    matches!(key.code.as_str(), "Enter" | "NumpadEnter")
        && key.mods == Mods::NONE
        && !key.release
        && !key.repeat
        && !key.composing
}

/// A message from the shell to the engine thread.
enum Control {
    Key(DomKey),
    Edit(EditAction),
    Text(String),
    /// One line for the screen only (never the shell): what the app has to tell about an act of the person.
    Notice(String),
    /// The engine's answer travels back over `reply`; a dropped sender tells the caller the pane is gone.
    Paste { text: String, allow_unsafe: bool, reply: Sender<Result<PasteOutcome>> },
    /// The empty bracketed paste that tells a program something that is not text was pasted.
    PasteCue { reply: Sender<Result<PasteOutcome>> },
    Resize { cols: u16, rows: u16, cell_px: (u32, u32) },
    Scroll(isize),
    Mouse(MouseEvent),
    Wheel(WheelEvent),
    ScrollToBottom,
    RequestFull,
    SetTheme(EngineTheme),
    SetOptionAsAlt(bool),
    /// Whether pictures may animate: the page's reduced-motion fact, handed to the engine.
    SetImageAnimation(bool),
    Select(SelectOp),
    SelectionText(Sender<Result<Option<String>>>),
    /// Read a region of the text; the engine's answer travels back over `reply`.
    ReadText { region: TextRegion, max_bytes: usize, reply: Sender<Result<Option<TextRead>>> },
    Search(SearchOp, Sender<Result<SearchState>>),
    CopyMode(CopyOp, Sender<Result<CopyState>>),
    /// Read a piece of an image's pixels; `Ok(None)` when the image or its generation is gone.
    ImageChunk { id: u32, generation: u64, offset: usize, max: usize, reply: Sender<Result<Option<ImageChunk>>> },
    /// The answer of a working-directory lookup, from the short-lived thread that ran it.
    CwdFound(Option<PathBuf>),
    /// Take the snapshot and register the tap, with no output fed between the two.
    AttachTap { tail_rows: usize, max_bytes: usize, queue_bytes: usize, id: TapId, reply: Sender<Result<Tap>> },
    DetachTap(TapId),
    /// Bytes for the program, verbatim; honoured by a headless pane only.
    WriteRaw(Vec<u8>),
    /// The grid of the session a remote pane shows: the engine follows it, the link is not told.
    Grid { cols: u16, rows: u16 },
    /// Remember the distance of the viewport from the bottom.
    MarkScroll,
    /// Put the viewport back at the remembered distance from the bottom.
    RestoreScroll,
    Close,
}

/// What the shell keeps of a pane. The last clone being dropped closes the pane.
struct Shared {
    queue: Arc<PaneQueue<Control>>,
    pid: Option<u32>,
    thread: Mutex<Option<JoinHandle<()>>>,
    /// The state of the pane's connection; empty for a local shell.
    conn: Arc<Mutex<Option<SharedState>>>,
    /// The control surface of a pane that shows a session on a server; empty for every other pane.
    remote: Arc<Mutex<Option<Arc<dyn RemoteControl>>>>,
    /// [`PaneSpec::headless`]: the only kind of pane that takes [`PaneHandle::write_raw`].
    headless: bool,
    /// How many times a timer woke the engine thread, see [`PaneHandle::timer_wakes`].
    timer_wakes: Arc<AtomicU64>,
    /// The bytes on their way to the PTY writer and not yet written; the same counter as the writer's own, see
    /// [`PaneHandle::raw_backlog`].
    writer_pending: Arc<AtomicUsize>,
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
        Self::spawn_wrapped(pane, spec, make_engine, sink, None)
    }

    /// [`PaneHandle::spawn`] with the PTY reader passed through `wrap` first. Nothing but the tests of the exit poll
    /// passes one: they hold the end of the output back, which is what a Windows pseudo console does after its shell
    /// ended and what no Unix PTY can be made to do (see [`ReaderWrap`]).
    fn spawn_wrapped<E, F, S>(
        pane: PaneId,
        spec: PaneSpec,
        make_engine: F,
        sink: S,
        wrap: Option<ReaderWrap>,
    ) -> Result<PaneHandle>
    where
        E: TerminalEngine + 'static,
        F: FnOnce(&PaneSpec) -> Result<E> + Send + 'static,
        S: FnMut(PaneEvent) + Send + 'static,
    {
        let mut spec = spec;
        spec.cols = spec.cols.clamp(MIN_COLS, MAX_DIM);
        spec.rows = spec.rows.clamp(MIN_ROWS, MAX_DIM);
        spec.frame_interval = spec.frame_interval.max(Duration::from_millis(1));
        spec.exit_poll = spec.exit_poll.map(|poll| poll.max(Duration::from_millis(1)));
        spec.cell_px = (spec.cell_px.0.min(MAX_CELL_PX), spec.cell_px.1.min(MAX_CELL_PX));
        spec.image_bytes = spec.image_bytes.min(MAX_IMAGE_BYTES);

        let queue: Arc<PaneQueue<Control>> = Arc::new(PaneQueue::new());
        let (started_tx, started_rx) = mpsc::channel::<Result<Option<u32>>>();
        let conn: Arc<Mutex<Option<SharedState>>> = Arc::new(Mutex::new(None));
        let engine_conn = conn.clone();
        let remote: Arc<Mutex<Option<Arc<dyn RemoteControl>>>> = Arc::new(Mutex::new(None));
        let engine_remote = remote.clone();
        let engine_queue = queue.clone();
        let headless = spec.headless;
        let timer_wakes = Arc::new(AtomicU64::new(0));
        let engine_timer_wakes = timer_wakes.clone();
        let writer_pending = Arc::new(AtomicUsize::new(0));
        let engine_pending = writer_pending.clone();
        let join = thread::Builder::new()
            .name(format!("hw-pane-{pane}-engine"))
            .spawn(move || {
                engine_main::<E, F, S>(
                    pane,
                    spec,
                    make_engine,
                    sink,
                    engine_queue,
                    started_tx,
                    wrap,
                    engine_conn,
                    engine_remote,
                    engine_timer_wakes,
                    engine_pending,
                )
            })
            .map_err(Error::Io)?;

        match started_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(pid)) => Ok(PaneHandle {
                shared: Arc::new(Shared {
                    queue,
                    pid,
                    thread: Mutex::new(Some(join)),
                    conn,
                    remote,
                    headless,
                    timer_wakes,
                    writer_pending,
                }),
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
            |spec| crate::GhosttyEngine::with_image_limit(spec.cols, spec.rows, spec.scrollback, spec.image_bytes),
            sink,
        )
    }

    /// The state of the pane's connection: [`ConnState::Local`] for a shell on this machine.
    pub fn conn_state(&self) -> ConnState {
        let slot = self.shared.conn.lock().unwrap_or_else(PoisonError::into_inner);
        slot.as_ref().map_or(ConnState::Local, SharedState::get)
    }

    fn remote(&self) -> Option<Arc<dyn RemoteControl>> {
        self.shared.remote.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// What a pane that shows a session on a server shows besides its screen: who writes, the grid of the session, whether
    /// older lines exist and what it is busy with. `None` for every other pane.
    pub fn remote_view(&self) -> Option<RemoteView> {
        self.remote().map(|remote| remote.view())
    }

    /// Take the right to write in the session this pane shows. The server tells every pane of the session; the view of this
    /// one changes when it has. An error for a pane that does not show a session on a server.
    pub fn takeover(&self) -> Result<()> {
        self.remote().ok_or_else(|| Error::Pty("this pane does not show a session on a server".into()))?.takeover()
    }

    /// Ask the server for older lines than the pane has, and keep the screen where it is (the position is measured from the
    /// bottom). Does nothing when the server has no older lines or a request is on its way. An error for a pane that does not
    /// show a session on a server.
    pub fn load_older(&self) -> Result<()> {
        self.remote().ok_or_else(|| Error::Pty("this pane does not show a session on a server".into()))?.load_older()
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

    /// A line-editing step (word or line jump, delete word or line) as the fixed bytes of [`EditAction::bytes`].
    /// Typing: it returns the viewport to the live screen. The page sends it only for the normal screen; a program
    /// on the alternate screen gets the key through [`PaneHandle::key`] instead.
    pub fn edit(&self, action: EditAction) -> Result<()> {
        self.send(Control::Edit(action))
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

    /// Show one line on the pane's screen, in the warn colour like the lines a connection says: what the app has to tell about
    /// something the person asked for. It goes to the screen only and never to the shell, control characters in it become
    /// spaces so it can never be an escape sequence, and at most [`MAX_NOTICE_BYTES`] of it are shown.
    pub fn notice(&self, text: &str) -> Result<()> {
        self.send(Control::Notice(text.to_owned()))
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

    /// Tell the program that something that is not text was pasted: an empty bracketed paste when it turned on
    /// bracketed paste, [`PasteOutcome::Empty`] and nothing written otherwise. Like [`PaneHandle::paste`] it waits for
    /// the engine thread's answer and must not be called on the Tauri main thread; a written cue clears the selection
    /// and returns the view to the live screen, as a paste does.
    pub fn paste_cue(&self) -> Result<PasteOutcome> {
        self.request(|reply| Control::PasteCue { reply })
    }

    /// Change the grid. The size is clamped to a sane range first; the PTY is resized with pixel dimensions and the
    /// next frame is a full one.
    ///
    /// On a pane that shows a session on a server the size is the pane's tile: it is recorded, and it changes the engine and the
    /// session only while the pane holds the right to write (a watcher follows the size of the session, never the other way
    /// round). When the pane takes over, the recorded size is sent once.
    pub fn resize(&self, cols: u16, rows: u16, cell_px: (u32, u32)) -> Result<()> {
        let (cols, rows) = (cols.clamp(MIN_COLS, MAX_DIM), rows.clamp(MIN_ROWS, MAX_DIM));
        if let Some(remote) = self.remote() {
            remote.tile(cols, rows);
            if remote.view().writer != WriterView::Writing {
                return Ok(());
            }
        }
        self.send(Control::Resize { cols, rows, cell_px: (cell_px.0.min(MAX_CELL_PX), cell_px.1.min(MAX_CELL_PX)) })
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

    /// Whether animated pictures may move. `false` pauses every animation on its shown frame (the page's reduced-motion
    /// fact) and the pane stops waking for them; `true` resumes them from that frame and makes the next frame ask the
    /// engine's clock again. Ordered with the other controls of the pane.
    pub fn set_image_animation(&self, enabled: bool) -> Result<()> {
        self.send(Control::SetImageAnimation(enabled))
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

    /// The text of a region of the pane, at most `max_bytes` bytes (and never more than [`MAX_READ_BYTES`]).
    ///
    /// `Ok(None)` when there is nothing to read and, without a round trip, for a bound of 0. The engine's own thread
    /// does the work; this call waits for the answer like [`PaneHandle::paste`] does, at most two seconds and
    /// `Error::Pty("pane busy")` after that, so it must not be called on the Tauri main thread. The text is untrusted
    /// program output, neither sanitised nor redacted: whoever passes it on is responsible for that.
    pub fn read_text(&self, region: TextRegion, max_bytes: usize) -> Result<Option<TextRead>> {
        if max_bytes == 0 {
            return Ok(None);
        }
        let max_bytes = max_bytes.min(MAX_READ_BYTES);
        let region = match region {
            TextRegion::LastRows(0) => return Ok(None),
            TextRegion::LastRows(n) => TextRegion::LastRows(n.min(MAX_READ_ROWS)),
            other => other,
        };
        self.request(|reply| Control::ReadText { region, max_bytes, reply })
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

    /// A piece of the pixels of image `id` as straight RGBA, at most `max` bytes (and never more than
    /// [`MAX_IMAGE_CHUNK_BYTES`]) from `offset` on. `Ok(None)` when the image is gone, still being received, or no
    /// longer has `generation`: the caller then asks the placement list of the next frame again. Waits for the answer
    /// like [`PaneHandle::paste`] does (at most two seconds), so it must not be called on the Tauri main thread. A read
    /// marks nothing dirty and writes nothing to the PTY. `Err` when the pane is closed or busy.
    pub fn image_chunk(&self, id: u32, generation: u64, offset: usize, max: usize) -> Result<Option<ImageChunk>> {
        let max = max.min(MAX_IMAGE_CHUNK_BYTES);
        self.request(|reply| Control::ImageChunk { id, generation, offset, max, reply })
    }

    /// Attach a tap: the state of the terminal now and every byte the engine is fed from then on, taken in one step of the
    /// engine thread, so that a client that feeds both to an engine of its own never misses or repeats a byte.
    ///
    /// `tail_rows` is how many rows of scrollback come with the screen (clamped to what exists, `0` for the screen only).
    /// `max_bytes` bounds the snapshot: a larger one is refused, as an error, and no tap is registered. `queue_bytes`
    /// bounds the bytes that wait for the reader, cut to [`MIN_TAP_QUEUE_BYTES`]..=[`MAX_TAP_QUEUE_BYTES`]: a reader
    /// that falls further behind is removed ([`TapEnd::Overflow`]) and the engine thread never waits for it. At most
    /// [`MAX_TAPS`] taps per pane. Waits for the engine thread's answer like [`PaneHandle::paste`] does, so it must not
    /// be called on the Tauri main thread.
    pub fn attach_tap(&self, tail_rows: usize, max_bytes: usize, queue_bytes: usize) -> Result<Tap> {
        let queue_bytes = queue_bytes.clamp(MIN_TAP_QUEUE_BYTES, MAX_TAP_QUEUE_BYTES);
        let id = TapId::next();
        self.request(|reply| Control::AttachTap { tail_rows, max_bytes, queue_bytes, id, reply })
    }

    /// Write `bytes` to the program as they are, the way a person's typing arrives, with no key encoder in between. Only a
    /// pane that was started headless ([`PaneSpec::headless`]) takes them: any other pane refuses with an error and writes
    /// nothing, and so does a call of more than [`MAX_RAW_BYTES`]. Returns at once; the engine thread queues the bytes
    /// for the PTY writer thread. Unlike a person's own input at this machine, raw input is bounded: a call that would
    /// take the bytes waiting for the program past [`MAX_RAW_BACKLOG`] is refused whole and writes nothing, because the
    /// sender is on the network and a program that does not read its input would otherwise keep every byte in memory.
    ///
    /// This is the one place where bytes enter a pane from outside, so who may call it is part of its contract: the
    /// session host of a server, after it has checked that the sender holds the write right of the session. The page
    /// can never reach it: it has no row in the command table and the host does not export it. The bytes are not
    /// looked at; they are as trusted as the holder of the write right.
    pub fn write_raw(&self, bytes: &[u8]) -> Result<()> {
        if !self.shared.headless {
            return Err(Error::Pty("raw input is for headless panes only".into()));
        }
        if bytes.len() > MAX_RAW_BYTES {
            return Err(Error::Pty(format!("raw input is limited to {MAX_RAW_BYTES} bytes per call")));
        }
        let n = bytes.len();
        let pending = &self.shared.writer_pending;
        // Counted here, before the bytes are queued, so that what waits in the control queue counts as well; the engine
        // thread hands them to the writer without counting them again.
        if pending
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |waiting| {
                waiting.checked_add(n).filter(|&total| total <= MAX_RAW_BACKLOG)
            })
            .is_err()
        {
            return Err(Error::Pty(RAW_BACKLOG_FULL.into()));
        }
        let sent = self.send(Control::WriteRaw(bytes.to_vec()));
        if sent.is_err() {
            pending.fetch_sub(n, Ordering::Relaxed);
        }
        sent
    }

    /// The bytes on their way to the program that are not written yet: raw input and the engine's replies. At most
    /// [`MAX_RAW_BACKLOG`] of it is raw input. It exists for tests and diagnostics, as [`PaneHandle::timer_wakes`] does.
    pub fn raw_backlog(&self) -> usize {
        self.shared.writer_pending.load(Ordering::Relaxed)
    }

    /// How many times a timer woke the engine thread with nothing queued for it: the next frame, an animated picture, the
    /// lookup of the working directory, the exit poll. A message or a chunk of output is not counted. It exists for tests
    /// and diagnostics, as [`PaneHandle::queue_high_water`] does: it is how the idle behaviour of a pane is observed from
    /// outside (a pane that draws for no page, [`PaneSpec::headless`], sets no timer for frames).
    pub fn timer_wakes(&self) -> u64 {
        self.shared.timer_wakes.load(Ordering::Relaxed)
    }

    /// End a tap now: nothing more is handed to it, what was queued is dropped and its reader gets [`TapEnd::Closed`].
    /// The other taps are not affected. An id that is gone is ignored.
    pub fn detach_tap(&self, id: TapId) {
        let _ = self.send(Control::DetachTap(id));
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

    /// Raw input of a headless pane: [`PaneHandle::write_raw`] counted the bytes in `pending` already.
    fn counted(&self, bytes: Vec<u8>) {
        let n = bytes.len();
        if n == 0 {
            return;
        }
        if self.tx.send(bytes).is_err() {
            self.pending.fetch_sub(n, Ordering::Relaxed);
        }
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

/// A function that wraps the PTY reader of a pane, see [`PaneHandle::spawn_wrapped`]. It is a seam for tests: on a Unix
/// PTY the end of the output always comes with the exit of the shell (the kernel hangs the terminal up when its
/// controlling process exits, whatever else holds it open), so the case the exit poll exists for cannot be made
/// with a real child and has to be made by holding the end back.
type ReaderWrap = Box<dyn FnOnce(Box<dyn Read + Send>) -> Box<dyn Read + Send> + Send>;

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
    pty: Option<Box<dyn Link>>,
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
    /// [`PaneSpec::cwd_lookup`]: false reports nothing from the process, so only OSC 7 tells the directory.
    cwd_lookup: bool,
    cwd: CwdTracker,
    /// The poll interval of [`PaneSpec::exit_poll`] and when the child was last asked.
    exit_poll: Option<Duration>,
    last_exit_check: Instant,
    /// The OS whose path rules an announced directory follows, and the machine's own name (`COMPUTERNAME`, read
    /// once when the pane starts; empty off Windows) that a Windows prompt names as the host of its report.
    os: Os,
    local_host: String,
    /// [`PaneSpec::linger`] and when the pane started, for the rule of the held exit. `started` is also the zero of the
    /// clock that animated pictures run on ([`Actor::now_ms`]).
    linger: Option<Duration>,
    started: Instant,
    /// When the engine wants the next frame of an animated picture, in milliseconds on the clock of `started`. `None`
    /// when nothing animates (or animation is paused): the pane then never wakes for pictures.
    image_due: Option<u64>,
    /// The exited program is held (see [`Actor::hold`]): no picture animates any more.
    holding: bool,
    /// Lets one bell through per [`crate::attention::GAP`], so a program that rings in a loop cannot flood the window.
    bell_gate: Gate,
    /// The same for the end of a command.
    end_gate: Gate,
    /// [`PaneSpec::headless`]: no frame is wanted, so none is built and nothing wakes for one.
    headless: bool,
    /// The taps ([`PaneHandle::attach_tap`]), in the order they were attached.
    taps: Vec<TapSink>,
    /// Timer wake-ups of the main loop, for [`PaneHandle::timer_wakes`].
    timer_wakes: Arc<AtomicU64>,
    /// The size of one cell in pixels as the pane last learned it, for a size that comes from the session ([`PaneSink::grid`]).
    cell_px: (u32, u32),
    /// Where the viewport was in the scrollable area at the last frame, for [`PaneSink::mark_scroll`].
    last_bar: crate::engine::Scrollbar,
    /// The distance of the viewport from the bottom that [`PaneSink::mark_scroll`] remembered, in rows.
    anchor: Option<u64>,
}

impl<E: TerminalEngine, S: FnMut(PaneEvent)> Actor<E, S> {
    /// The main loop. Returns when the pane is to end.
    fn run(&mut self) {
        loop {
            let mut wait = match (self.frame_wanted(), self.last_emit) {
                (true, Some(last)) => self.interval.saturating_sub(last.elapsed()),
                (true, None) => Duration::ZERO,
                (false, _) => Duration::from_secs(3600),
            };
            if let Some(deadline) = self.cwd.deadline() {
                wait = wait.min(deadline.saturating_duration_since(Instant::now()));
            }
            // An animation wakes the pane only for the time the engine named, and only when no frame is waiting anyway:
            // a dirty pane already waits for its frame interval.
            if let Some(due) = self.image_due.filter(|_| !self.frame_wanted()) {
                wait = wait.min(Duration::from_millis(due.saturating_sub(self.now_ms())));
            }
            if let Some(poll) = self.exit_poll {
                wait = wait.min(poll.saturating_sub(self.last_exit_check.elapsed()));
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
                    self.hold_if_wanted(None);
                    return;
                }
                // Woken by a timer with nothing queued: the next frame, a picture, the directory lookup or the exit poll.
                Popped::Timeout => {
                    self.timer_wakes.fetch_add(1, Ordering::Relaxed);
                }
            }
            // The clock has passed the due time of an animation: a frame is due, built by the rule below.
            if self.image_due.is_some_and(|due| self.now_ms() >= due) {
                self.dirty = true;
            }
            if self.dirty && self.last_emit.is_none_or(|last| last.elapsed() >= self.interval) {
                self.emit_frame();
            }
            if self.cwd.begin(Instant::now()) {
                self.look_up_cwd();
            }
            // Asked on the clock and not on a timeout: output that keeps arriving makes every wait end early.
            if self.exit_poll.is_some_and(|poll| self.last_exit_check.elapsed() >= poll) {
                self.last_exit_check = Instant::now();
                if let Some(code) = self.child_status() {
                    if !self.drain_after_exit() {
                        self.hold_if_wanted(Some(code));
                    }
                    return;
                }
            }
        }
    }

    /// Whether a frame is due: the screen changed and the pane draws for a page.
    fn frame_wanted(&self) -> bool {
        self.dirty && !self.headless
    }

    /// The exit status of the shell if it has exited. Does not block; a child that cannot be asked counts as running.
    /// The child is not closed: a second ask gives the same status.
    fn child_status(&mut self) -> Option<u32> {
        self.pty.as_mut().and_then(|pty| pty.try_wait().ok().flatten())
    }

    /// The status of a child whose output has ended, which is readable a few milliseconds later at the latest: asked
    /// every few milliseconds for [`STATUS_SETTLE`], `None` when it never became readable.
    fn settled_status(&mut self) -> Option<u32> {
        let deadline = Instant::now() + STATUS_SETTLE;
        loop {
            if let Some(code) = self.child_status() {
                return Some(code);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// The program ended and the output was shown: with [`PaneSpec::linger`] set, keep the pane when the status is not
    /// zero, is unknown, or the pane is younger than the linger time. `known` is the status the caller already has.
    fn hold_if_wanted(&mut self, known: Option<u32>) {
        let Some(linger) = self.linger else { return };
        let code = known.or_else(|| self.settled_status());
        if code == Some(0) && self.started.elapsed() >= linger {
            return;
        }
        self.hold(code);
    }

    /// The held exit: one notice line on the screen, one [`PaneEvent::Held`], then nothing but a close request and Enter
    /// end the pane. Resizes, themes, scrolling, search and the like work as before; everything that would reach the
    /// program (keys, text, pastes, mouse reports) is dropped, because there is no program, and so is output. The caller
    /// ends the pane through [`Actor::finish`], which sends the one `Exited`.
    fn hold(&mut self, code: Option<u32>) {
        self.holding = true;
        self.image_due = None;
        let line = match code {
            Some(code) => format!("[holzWave] exited with status {code}. Press Enter to close."),
            None => "[holzWave] exited. Press Enter to close.".to_owned(),
        };
        let mut bytes = HELD_MODES_RESET.to_vec();
        bytes.extend_from_slice(&notice_line(&line));
        self.feed(&bytes);
        self.emit_frame();
        (self.sink)(PaneEvent::Held { code });
        loop {
            let wait = match (self.frame_wanted(), self.last_emit) {
                (true, Some(last)) => self.interval.saturating_sub(last.elapsed()),
                (true, None) => Duration::ZERO,
                (false, _) => Duration::from_secs(3600),
            };
            match self.queue.pop_held(wait) {
                Popped::Control(Control::Close) | Popped::Closed => return,
                Popped::Control(Control::Key(key)) if is_enter(&key) => return,
                Popped::Control(control) => self.handle_held(control),
                // Output after the end (a pseudo console that still flushes) and the end itself are of no interest.
                Popped::Output(_) | Popped::Finished | Popped::Timeout => {}
            }
            if self.dirty && self.last_emit.is_none_or(|last| last.elapsed() >= self.interval) {
                self.emit_frame();
            }
        }
    }

    /// A control message of a held pane: what would reach the program is dropped, the rest is as in a live pane.
    fn handle_held(&mut self, control: Control) {
        match control {
            Control::Key(_) | Control::Edit(_) | Control::Text(_) | Control::Mouse(_) | Control::CwdFound(_) => {}
            // The bytes were counted when they were queued; dropped here, they no longer wait.
            Control::WriteRaw(bytes) => {
                self.writer.pending.fetch_sub(bytes.len(), Ordering::Relaxed);
            }
            Control::Paste { reply, .. } => {
                let _ = reply.send(Err(Error::Pty("the program has ended".into())));
            }
            // A wheel scrolls the screen locally or does nothing; bytes for the program are dropped.
            Control::Wheel(event) => {
                if matches!(self.engine.wheel(&event), Ok(WheelOutcome::Scrolled)) {
                    self.dirty = true;
                }
            }
            other => self.handle(other),
        }
    }

    /// The shell exited but the output did not end (a Windows pseudo console before 24H2): feed what is still queued
    /// or arriving for [`EXIT_GRACE`], show the last frame, and go. The caller then ends the pane like the end of the
    /// output does, so the reader keeps draining while the child is closed and the one `Exited` follows. Returns true
    /// when a request to close the pane was met on the way (it is answered by ending, so the pane is not held then).
    fn drain_after_exit(&mut self) -> bool {
        let deadline = Instant::now() + EXIT_GRACE;
        loop {
            match self.queue.pop(deadline.saturating_duration_since(Instant::now())) {
                Popped::Output(chunk) => self.feed(&chunk),
                // Answered, so that a caller waiting for a reply does not wait out its timeout; nothing else is.
                Popped::Control(Control::Close) | Popped::Closed => return true,
                Popped::Control(control) => self.handle(control),
                Popped::Finished | Popped::Timeout => break,
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        if self.dirty {
            self.emit_frame();
        }
        false
    }

    /// Ask for the shell's working directory without waiting for it. The lookup starts a process that can stall
    /// (a stale network mount), so it runs on a thread of its own and its answer comes back as
    /// [`Control::CwdFound`]: keys, resizes and frames of the pane never wait for it (WR-04).
    fn look_up_cwd(&mut self) {
        let Some(pid) = self.pid.filter(|_| self.cwd_lookup) else {
            // No process to ask (or none worth asking, see `PaneSpec::cwd_lookup`): the tracker falls back to OSC 7.
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
            Control::Edit(action) => self.edit(action),
            Control::Text(text) => self.text(&text),
            Control::Notice(text) => self.feed(&notice_line(&text)),
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
            Control::PasteCue { reply } => {
                let outcome = self.engine.paste_cue();
                let bytes = self.engine.take_replies();
                if matches!(outcome, Ok(PasteOutcome::Written)) {
                    self.clear_selection();
                    self.engine.scroll_to_bottom();
                    self.dirty = true;
                    self.writer.input(bytes);
                } else {
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
            Control::SetImageAnimation(on) => {
                self.engine.set_image_animation(on);
                if on {
                    // The next frame asks the clock again: an animation that was paused has a due time now.
                    self.dirty = true;
                } else {
                    // Paused: the engine moves no frame, so there is nothing to wake for.
                    self.image_due = None;
                }
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
            Control::ReadText { region, max_bytes, reply } => {
                // A read marks nothing dirty and writes nothing to the PTY.
                let _ = reply.send(self.engine.read_text(region, max_bytes));
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
            Control::ImageChunk { id, generation, offset, max, reply } => {
                // A read marks nothing dirty and writes nothing to the PTY; a caller that is gone costs nothing.
                let _ = reply.send(Ok(self.engine.image_chunk(id, generation, offset, max)));
            }
            Control::AttachTap { tail_rows, max_bytes, queue_bytes, id, reply } => {
                let answer = self.attach_tap(tail_rows, max_bytes, queue_bytes, id);
                // A caller that stopped waiting (its call timed out) must not leave a tap behind.
                if reply.send(answer).is_err() {
                    self.taps.retain(|tap| tap.id != id);
                }
            }
            // The handle refused a pane that is not headless; the engine thread does not trust that alone.
            Control::WriteRaw(bytes) => {
                if self.headless {
                    self.writer.counted(bytes);
                } else {
                    self.writer.pending.fetch_sub(bytes.len(), Ordering::Relaxed);
                }
            }
            Control::DetachTap(id) => {
                if let Some(at) = self.taps.iter().position(|tap| tap.id == id) {
                    self.taps.remove(at).close(true);
                }
            }
            Control::Grid { cols, rows } => self.follow_grid(cols, rows),
            Control::MarkScroll => {
                // The viewport is read from the last frame: bring it up to date first.
                self.emit_frame();
                let bar = self.last_bar;
                self.anchor = Some(bar.total.saturating_sub(bar.offset.saturating_add(bar.len)));
            }
            Control::RestoreScroll => {
                if let Some(rows) = self.anchor.take().filter(|rows| *rows > 0) {
                    self.engine.scroll_to_bottom();
                    self.engine.scroll(-isize::try_from(rows).unwrap_or(isize::MAX));
                    self.dirty = true;
                }
            }
            // `run` ends the loop on Close before it gets here.
            Control::Close => {}
        }
    }

    /// Program output into the engine; its answers to the program's queries go back through the writer.
    fn feed(&mut self, bytes: &[u8]) {
        self.engine.feed(bytes);
        self.fan_out(bytes);
        let replies = self.engine.take_replies();
        self.writer.reply(replies);
        self.cwd.output(Instant::now());
        self.drain_events();
        self.dirty = true;
    }

    /// Hand the bytes the engine was just fed to every tap, in order. A tap that is finished by this (it fell behind, or
    /// nobody reads it) is removed; the engine thread never waits for any of them.
    fn fan_out(&mut self, bytes: &[u8]) {
        if !self.taps.is_empty() {
            self.taps.retain(|tap| tap.push(bytes));
        }
    }

    /// Take a snapshot and register a tap for it, between two feeds of this thread.
    fn attach_tap(&mut self, tail_rows: usize, max_bytes: usize, queue_bytes: usize, id: TapId) -> Result<Tap> {
        // Taps whose reader has gone do not count against the bound.
        self.taps.retain(|tap| !tap.shared.receiver_gone.load(Ordering::Relaxed));
        if self.taps.len() >= MAX_TAPS {
            return Err(Error::Pty("too many taps on this pane".into()));
        }
        let snapshot = self.engine.snapshot_vt(tail_rows, max_bytes)?;
        let size = self.engine.size()?;
        let history_rows = self.engine.scrollback_rows();
        let shared = Arc::new(TapShared {
            state: Mutex::new(TapState::default()),
            ready: Condvar::new(),
            receiver_gone: AtomicBool::new(false),
        });
        self.taps.push(TapSink { id, shared: shared.clone(), bound: queue_bytes });
        Ok(Tap { snapshot, size, history_rows, id, rx: TapReceiver { shared } })
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

    fn edit(&mut self, action: EditAction) {
        self.clear_selection();
        self.engine.scroll_to_bottom();
        self.dirty = true;
        self.writer.input(action.bytes().to_vec());
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

    /// The grid of a session on a server: the engine takes that size and nothing else changes. The link is not told: a
    /// watcher follows the session, and the size the writer sent is the one that came back.
    fn follow_grid(&mut self, cols: u16, rows: u16) {
        let before = self.engine.size().ok();
        if before == Some((cols, rows)) {
            return;
        }
        if self.engine.resize(cols, rows, self.cell_px).is_err() {
            return;
        }
        if self.engine.size().ok() != before {
            self.encoder.request_full();
        }
        self.dirty = true;
    }

    fn resize(&mut self, cols: u16, rows: u16, cell_px: (u32, u32)) {
        self.cell_px = cell_px;
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
                EngineEvent::Bell => {
                    if self.bell_gate.pass(Instant::now()) {
                        (self.sink)(PaneEvent::Bell);
                    }
                }
                EngineEvent::CommandEnd { exit } => {
                    if self.end_gate.pass(Instant::now()) {
                        (self.sink)(PaneEvent::CommandEnd { exit });
                    }
                }
                EngineEvent::Pwd(raw) => {
                    // A program (a shell with integration, or something on the far side of ssh) announced its
                    // directory. Only a well-formed `file://` path counts, and on Windows only a drive path of this
                    // machine ([`announced_path`]); it is the fallback of the lookup, and on Windows the only source.
                    if let Some((host, path)) = parse_osc7(&raw) {
                        if let Some(dir) = announced_path(self.os, &host, &path, &self.local_host) {
                            self.cwd.announced(dir.to_string_lossy().into_owned(), Instant::now());
                        }
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
        // A pane that draws for no page builds no frame and asks no picture clock: nothing in it is waiting for one.
        if self.headless {
            return;
        }
        // The engine's clock for animated pictures runs once per frame, never per chunk of output, and not at all in a
        // held pane. What it returns is when the next frame is due.
        self.image_due = if self.holding { None } else { self.engine.tick_images(self.now_ms()) };
        match self.engine.update() {
            Ok(update) => {
                self.last_bar = update.scrollbar;
                if let Some(frame) = self.encoder.encode(update) {
                    (self.sink)(PaneEvent::Frame(frame));
                    self.last_emit = Some(Instant::now());
                }
            }
            // No frame is the symptom of this; without a trace the pane just looks frozen.
            Err(e) => eprintln!("holzwave: pane update failed: {e}"),
        }
    }

    /// Milliseconds since the pane started, on a monotonic clock: the time that animated pictures are given and the
    /// time their due moments are compared with.
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Close the child and report the end. The one place a pane ends.
    fn finish(&mut self) {
        // Refuse further input and release a reader blocked on a full queue.
        self.queue.close();
        // Kill and reap; the writer is dropped only after this. The reader thread keeps draining meanwhile.
        let code = self.pty.take().and_then(|pty| pty.close().ok());
        self.reaped.store(true, Ordering::Relaxed);
        // Every tap gets the end after the bytes that were queued for it.
        for tap in self.taps.drain(..) {
            tap.close(false);
        }
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
fn start<E, F>(spec: &PaneSpec, make_engine: F, sink: PaneSink) -> Result<(E, Box<dyn Link>)>
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
    if !spec.image_animation {
        // Before the shell exists, so before the first picture it could send.
        engine.set_image_animation(false);
    }
    if let Some(theme) = &spec.theme {
        // A theme that cannot be applied is not a reason to have no terminal.
        let _ = engine.set_theme(theme);
    }
    if let Some(notice) = &spec.notice {
        // Before the shell exists, so before anything it prints. Control characters are made harmless: the line is
        // text, never an escape sequence. It is drawn in the warn colour, as the line of `PaneHandle::notice` is (the
        // design record 6.32 says so for the line that tells a Flatpak pane runs in the sandbox); the colour is reset
        // before the shell's own output.
        let line: String = notice.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        engine.feed(format!("\x1b[33m{line}\x1b[0m\r\n").as_bytes());
    }
    if spec.blank {
        return Ok((engine, Box::new(crate::conn::NullLink::new())));
    }
    if let Some(target) = &spec.remote {
        // A session on a server: the link is built by what the spec names and returns at once (it connects on a thread of
        // its own), so starting the pane never waits for the network.
        let link = target.attach.link(target.session, target.tail, (spec.cols, spec.rows), sink)?;
        return Ok((engine, link));
    }
    if let Some(connection) = &spec.ssh_connection {
        let link = SshLink::attach_with(connection.clone(), spec.cols, spec.rows, spec.tunnel.clone())?;
        return Ok((engine, Box::new(link)));
    }
    if let Some(target) = &spec.ssh {
        let link = SshLink::connect(target.clone(), spec.cols, spec.rows)?;
        return Ok((engine, Box::new(link)));
    }
    let (program, args, cwd) = spawn_command(spec, &|key| std::env::var_os(key).map(|v| v.to_string_lossy().into_owned()));
    let pty = Pty::spawn_with(&spawn_options(spec, program, args, cwd))?;
    Ok((engine, Box::new(pty)))
}

/// The options the local program of `spec` is started with: the command from [`spawn_command`], the grid, the extra
/// variables of the spec and the names it removes.
fn spawn_options(spec: &PaneSpec, program: String, args: Vec<String>, cwd: Option<PathBuf>) -> SpawnOptions {
    let mut options = SpawnOptions::new(program, spec.cols, spec.rows).args(args).cell_px(spec.cell_px.0, spec.cell_px.1);
    if let Some(dir) = cwd {
        options = options.cwd(dir);
    }
    for (key, value) in &spec.env {
        options = options.env(key, value);
    }
    options.env_remove(spec.env_remove.iter().cloned())
}

/// The program, the arguments and the working directory the child process is started with. As the spec says them, except
/// in host-spawn mode ([`PaneSpec::host_spawn`]): then the program is `flatpak-spawn`, the directory and the
/// variables the child would get ([`SpawnOptions::child_env`], looked up in `env`) are arguments of it, and the process's
/// own directory is not set. Pure apart from `env`, so the rule is unit-tested without a Flatpak.
fn spawn_command(spec: &PaneSpec, env: &dyn Fn(&str) -> Option<String>) -> (String, Vec<String>, Option<PathBuf>) {
    if !spec.host_spawn {
        return (spec.program.clone(), spec.args.clone(), spec.cwd.clone());
    }
    let child_env = SpawnOptions::new(spec.program.clone(), spec.cols, spec.rows).child_env(env);
    let launch = ShellLaunch { program: spec.program.clone(), args: spec.args.clone() };
    // A directory that is not UTF-8 cannot be an argument here: the pane starts without one.
    let wrapped = platform::host_command(&launch, spec.cwd.as_deref().and_then(|dir| dir.to_str()), &child_env);
    (wrapped.program, wrapped.args, None)
}

#[allow(clippy::too_many_arguments)]
fn engine_main<E, F, S>(
    pane: PaneId,
    spec: PaneSpec,
    make_engine: F,
    mut sink: S,
    queue: Arc<PaneQueue<Control>>,
    started: Sender<Result<Option<u32>>>,
    wrap: Option<ReaderWrap>,
    conn: Arc<Mutex<Option<SharedState>>>,
    remote: Arc<Mutex<Option<Arc<dyn RemoteControl>>>>,
    timer_wakes: Arc<AtomicU64>,
    pending: Arc<AtomicUsize>,
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

    let (engine, mut pty) = match start(&spec, make_engine, PaneSink { queue: queue.clone() }) {
        Ok(started) => started,
        Err(e) => return fail(&mut sink, &queue, e),
    };
    let pid = pty.process_id();
    *conn.lock().unwrap_or_else(PoisonError::into_inner) = pty.shared_state();
    *remote.lock().unwrap_or_else(PoisonError::into_inner) = pty.remote_control();
    let (Some(reader), Some(writer)) = (pty.take_reader(), pty.take_writer()) else {
        let _ = pty.close();
        return fail(&mut sink, &queue, Error::Pty("the PTY has no reader or writer".into()));
    };

    let reader = match wrap {
        Some(wrap) => wrap(reader),
        None => reader,
    };

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
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
        cwd_lookup: spec.cwd_lookup,
        cwd: CwdTracker::new(Instant::now()),
        exit_poll: spec.exit_poll,
        last_exit_check: Instant::now(),
        os: Os::current(),
        local_host: match Os::current() {
            Os::Windows => std::env::var("COMPUTERNAME").unwrap_or_default(),
            _ => String::new(),
        },
        linger: spec.linger,
        started: Instant::now(),
        image_due: None,
        holding: false,
        bell_gate: Gate::new(),
        end_gate: Gate::new(),
        headless: spec.headless,
        taps: Vec::new(),
        timer_wakes,
        cell_px: spec.cell_px,
        last_bar: crate::engine::Scrollbar::default(),
        anchor: None,
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

    // ---- the remote slot and its sink ----

    struct Host;

    impl RemoteAttach for Host {
        fn link(&self, _: u32, _: Option<u32>, _: (u16, u16), _: PaneSink) -> Result<Box<dyn Link>> {
            unreachable!("not started in these tests")
        }

        fn host(&self) -> String {
            "hw.example.test".into()
        }
    }

    #[test]
    fn a_spec_has_no_remote_target_and_a_debug_print_of_one_shows_the_session_and_the_host_only() {
        assert!(PaneSpec::new("/bin/sh", 80, 24).remote.is_none());
        let target = RemoteTarget { session: 7, tail: Some(500), attach: Arc::new(Host) };
        let printed = format!("{target:?}");
        assert!(printed.contains("session: 7") && printed.contains("hw.example.test"), "{printed}");
        assert!(!printed.contains("500"), "the tail is not part of what a print shows: {printed}");
    }

    #[test]
    fn the_sink_hands_the_engine_the_grid_clamped_and_the_scroll_marks_in_the_order_they_were_given() {
        let queue: Arc<PaneQueue<Control>> = Arc::new(PaneQueue::new());
        let sink = PaneSink { queue: queue.clone() };
        sink.grid(0, 65_535);
        sink.mark_scroll();
        sink.restore_scroll();
        let mut got = Vec::new();
        while let Popped::Control(control) = queue.pop(MS) {
            got.push(control);
        }
        assert!(matches!(got[0], Control::Grid { cols: MIN_COLS, rows: MAX_DIM }), "the first control is not the clamped grid");
        assert!(matches!(got[1], Control::MarkScroll) && matches!(got[2], Control::RestoreScroll), "{} controls", got.len());
    }

    #[test]
    fn a_mark_waits_until_the_bytes_before_it_have_been_taken_by_the_engine() {
        let queue: Arc<PaneQueue<Control>> = Arc::new(PaneQueue::new());
        let sink = PaneSink { queue: queue.clone() };
        assert!(queue.push_output(vec![0; 100]));
        let taker = {
            let queue = queue.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(50));
                assert!(matches!(queue.pop(Duration::from_secs(1)), Popped::Output(_)));
            })
        };
        let started = Instant::now();
        sink.mark_scroll();
        assert!(started.elapsed() >= Duration::from_millis(40), "the mark did not wait for the output");
        taker.join().unwrap();
        assert!(matches!(queue.pop(Duration::from_secs(1)), Popped::Control(Control::MarkScroll)));
    }

    // ---- spawn_command: what the child is started with ----

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn a_pane_spec_without_the_new_fields_builds_the_same_spawn_options() {
        let mut spec = PaneSpec::new("/bin/zsh", 80, 24).args(["-l"]).cwd("/home/a");
        spec.cell_px = (9, 18);
        spec.env = vec![("HOLZWAVE_PANE".into(), "3".into())];
        assert_eq!((spec.linger, spec.env_remove.len()), (None, 0), "the new fields default to nothing");

        let (program, args, cwd) = spawn_command(&spec, &no_env);
        let built = spawn_options(&spec, program, args, cwd);
        // What `start` built before the fields existed, written out.
        let before = SpawnOptions::new("/bin/zsh", 80, 24)
            .args(["-l"])
            .cell_px(9, 18)
            .cwd("/home/a")
            .env("HOLZWAVE_PANE", "3");
        let (built_text, before_text) = (format!("{built:?}"), format!("{before:?}"));
        assert_eq!(built_text.replace(", env_remove: []", ""), before_text.replace(", env_remove: []", ""));
        assert_eq!(built.child_env(no_env), before.child_env(no_env));

        // With removals the same options carry them, and nothing else changes.
        spec.env_remove = vec!["HOME".into()];
        let (program, args, cwd) = spawn_command(&spec, &no_env);
        let removing = spawn_options(&spec, program, args, cwd);
        assert_eq!(removing.child_env(no_env), before.child_env(no_env));
        assert_ne!(format!("{removing:?}"), before_text);
    }

    #[test]
    fn a_new_spec_has_the_default_image_memory_and_it_is_the_engines_default() {
        assert_eq!(PaneSpec::new("/bin/sh", 80, 24).image_bytes, DEFAULT_IMAGE_BYTES);
        assert_eq!(DEFAULT_IMAGE_BYTES, 64 * 1024 * 1024);
        assert_eq!(MAX_IMAGE_BYTES, 512 * 1024 * 1024, "the top of the image_memory_mib range");
        #[cfg(feature = "ghostty")]
        assert_eq!(DEFAULT_IMAGE_BYTES, crate::images::DEFAULT_IMAGE_BYTES);
    }

    /// What the engine factory of a pane is handed: the image memory of the spec, cut to [`MAX_IMAGE_BYTES`].
    #[cfg(all(feature = "ghostty", unix))]
    #[test]
    fn the_engine_factory_gets_the_image_memory_of_the_spec_cut_to_the_top_of_the_range() {
        for (asked, expected) in [(0, 0), (5 * 1024 * 1024, 5 * 1024 * 1024), (u64::MAX, MAX_IMAGE_BYTES)] {
            let seen = Arc::new(Mutex::new(None));
            let seen_in = seen.clone();
            let mut spec = PaneSpec::new("/bin/sh", 80, 24).args(["-c", "sleep 30"]);
            spec.image_bytes = asked;
            let pane = PaneHandle::spawn(
                1,
                spec,
                move |spec| {
                    *seen_in.lock().unwrap() = Some(spec.image_bytes);
                    crate::GhosttyEngine::with_image_limit(spec.cols, spec.rows, spec.scrollback, spec.image_bytes)
                },
                |_| {},
            )
            .expect("the pane starts");
            assert_eq!(*seen.lock().unwrap(), Some(expected), "asked for {asked}");
            pane.close();
            assert!(pane.join(Duration::from_secs(5)));
        }
    }

    #[test]
    fn a_spec_without_the_host_flag_is_started_as_it_says() {
        let spec = PaneSpec::new("/bin/zsh", 80, 24).args(["-l"]).cwd("/home/a");
        assert!(!spec.host_spawn && spec.cwd_lookup, "the defaults: nothing wrapped, the directory looked up");
        assert_eq!(
            spawn_command(&spec, &no_env),
            ("/bin/zsh".to_string(), vec!["-l".to_string()], Some(PathBuf::from("/home/a")))
        );
    }

    #[test]
    fn a_spec_with_the_host_flag_is_wrapped_and_the_process_directory_is_not_set() {
        let mut spec = PaneSpec::new("sh", 80, 24).args(["-c", "exec zsh"]).cwd("/home/a b");
        spec.host_spawn = true;
        let (program, args, cwd) = spawn_command(&spec, &no_env);
        assert_eq!(program, "flatpak-spawn");
        assert_eq!(cwd, None, "the directory is an argument of flatpak-spawn, not the sandbox process's");
        let separator = args.iter().position(|a| a == "--").expect("a separator");
        assert_eq!(&args[..3], ["--host", "--watch-bus", "--directory=/home/a b"]);
        assert_eq!(&args[separator + 1..], ["sh", "-c", "exec zsh"]);
        // The variables the child would get travel as options of flatpak-spawn: it does not forward its own.
        for expected in ["--env=TERM=xterm-256color", "--env=COLORTERM=truecolor", "--env=TERM_PROGRAM=holzWave"] {
            assert!(args[..separator].iter().any(|a| a == expected), "{expected} missing in {args:?}");
        }
        assert!(
            args[..separator].iter().any(|a| a.starts_with("--env=LC_CTYPE=")) || cfg!(windows),
            "no locale in the environment of the app: the host shell gets a UTF-8 one"
        );
    }

    #[test]
    fn the_host_wrapping_leaves_the_locale_alone_when_the_app_has_one() {
        let mut spec = PaneSpec::new("sh", 80, 24);
        spec.host_spawn = true;
        let env = |key: &str| (key == "LANG").then(|| "de_DE.UTF-8".to_string());
        let (_, args, _) = spawn_command(&spec, &env);
        assert!(!args.iter().any(|a| a.starts_with("--env=LC_CTYPE")), "{args:?}");
    }

    #[test]
    fn a_directory_that_is_not_text_starts_the_host_pane_without_one() {
        #[cfg(unix)]
        {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;
            let mut spec = PaneSpec::new("sh", 80, 24);
            spec.host_spawn = true;
            spec.cwd = Some(PathBuf::from(OsString::from_vec(vec![b'/', b't', 0xff, b'x'])));
            let (_, args, _) = spawn_command(&spec, &no_env);
            assert!(!args.iter().any(|a| a.starts_with("--directory=")), "{args:?}");
        }
    }

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

/// The exit poll against a shell whose output does not end. The end is held back by wrapping the reader (see
/// [`ReaderWrap`]); the shell, the PTY and the engine are real.
#[cfg(all(test, feature = "ghostty", unix))]
mod exit_poll_tests {
    use super::*;
    use crate::wire::RowMsg;
    use std::sync::atomic::AtomicBool;

    const WAIT: Duration = Duration::from_secs(5);

    /// What a Windows pseudo console does before 24H2 once its shell has ended: the reader is never given the end
    /// of the output. After the real end it waits until `release` is set, or, in `flood` mode, keeps delivering
    /// output without a pause, so that the pane's wait for a message never runs into a timeout.
    struct OutlivesShell {
        inner: Box<dyn Read + Send>,
        release: Arc<AtomicBool>,
        flood: bool,
        ended: bool,
    }

    impl Read for OutlivesShell {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.ended {
                match self.inner.read(buf) {
                    Ok(n) if n > 0 => return Ok(n),
                    // The real end, or the macOS EIO: swallowed.
                    _ => self.ended = true,
                }
            }
            loop {
                if self.release.load(Ordering::Relaxed) {
                    return Ok(0);
                }
                if self.flood {
                    // No pause: the queue is never empty, so the engine thread's wait never times out.
                    let text = b"flood\r\n".repeat(64);
                    buf[..text.len()].copy_from_slice(&text);
                    return Ok(text.len());
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    #[derive(Clone, Default)]
    struct Log(Arc<Mutex<Vec<PaneEvent>>>);

    impl Log {
        fn sink(&self) -> impl FnMut(PaneEvent) + Send + 'static {
            let log = self.0.clone();
            move |event| log.lock().unwrap().push(event)
        }

        fn events(&self) -> Vec<PaneEvent> {
            self.0.lock().unwrap().clone()
        }

        fn exits(&self) -> Vec<Option<u32>> {
            self.events()
                .into_iter()
                .filter_map(|e| match e {
                    PaneEvent::Exited { code } => Some(code),
                    _ => None,
                })
                .collect()
        }

        fn has_text(&self, needle: &str) -> bool {
            let text = |row: &RowMsg| row.runs.iter().flat_map(|run| run.0.chars()).collect::<String>();
            self.events().iter().any(|e| match e {
                PaneEvent::Frame(f) => f.changed.iter().any(|row| text(row).contains(needle)),
                _ => false,
            })
        }
    }

    fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        condition()
    }

    /// Start `/bin/sh -c script` in a pane whose output outlives the shell until the returned flag is set.
    fn start(script: &str, exit_poll: Option<Duration>, flood: bool) -> (PaneHandle, Log, Arc<AtomicBool>) {
        let log = Log::default();
        let release = Arc::new(AtomicBool::new(false));
        let held = release.clone();
        let mut spec = PaneSpec::new("/bin/sh", 80, 24).args(["-c", script]);
        spec.exit_poll = exit_poll;
        let wrap: ReaderWrap = Box::new(move |inner| {
            Box::new(OutlivesShell { inner, release: held, flood, ended: false }) as Box<dyn Read + Send>
        });
        let pane = PaneHandle::spawn_wrapped(
            1,
            spec,
            |spec| crate::GhosttyEngine::with_image_limit(spec.cols, spec.rows, spec.scrollback, spec.image_bytes),
            log.sink(),
            Some(wrap),
        )
        .expect("the pane starts");
        (pane, log, release)
    }

    #[test]
    fn a_pane_ends_when_its_shell_exits_although_a_child_keeps_the_pty_open() {
        let started = Instant::now();
        let (pane, log, release) = start("exit 3", Some(Duration::from_millis(50)), false);

        assert!(wait_until(WAIT, || !log.exits().is_empty()), "the poll never ended the pane: {:?}", log.events());
        assert!(started.elapsed() < Duration::from_secs(2), "ended after {:?}", started.elapsed());
        assert_eq!(log.exits(), vec![Some(3)]);
        // Nothing follows the one Exited.
        let events = log.events();
        assert!(matches!(events.last(), Some(PaneEvent::Exited { code: Some(3) })), "{events:?}");
        thread::sleep(Duration::from_millis(300));
        assert_eq!(log.events().len(), events.len(), "an event arrived after Exited");
        assert!(pane.join(Duration::from_secs(3)));
        release.store(true, Ordering::Relaxed);
    }

    #[test]
    fn without_a_poll_such_a_pane_stays_open_until_it_is_closed() {
        // The control of the test above: the hold works, and nothing but the poll ends the pane.
        let (pane, log, release) = start("exit 3", None, false);
        thread::sleep(Duration::from_millis(800));
        assert!(log.exits().is_empty(), "ended without a poll: {:?}", log.events());

        pane.close();
        assert!(wait_until(WAIT, || !log.exits().is_empty()), "close did not end it");
        assert_eq!(log.exits(), vec![Some(3)]);
        assert!(pane.join(Duration::from_secs(3)));
        release.store(true, Ordering::Relaxed);
    }

    #[test]
    fn the_exit_poll_runs_while_output_keeps_arriving() {
        // Every wait of the engine thread ends with output and never with a timeout: the poll must not depend on a
        // quiet moment.
        let started = Instant::now();
        let (pane, log, release) = start("exit 4", Some(Duration::from_millis(50)), true);

        let ended = wait_until(WAIT, || !log.exits().is_empty());
        assert!(ended, "a flood starved the poll: {} events", log.events().len());
        assert!(started.elapsed() < Duration::from_secs(2), "ended after {:?}", started.elapsed());
        assert_eq!(log.exits(), vec![Some(4)]);
        assert!(log.has_text("flood"), "the output of the flood was not fed");
        release.store(true, Ordering::Relaxed);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(log.exits().len(), 1);
        assert!(pane.join(Duration::from_secs(3)));
    }

    #[test]
    fn output_written_right_before_the_exit_is_fed_before_a_polled_pane_ends() {
        for round in 0..3 {
            let (pane, log, release) = start("printf last-words; exit 2", Some(Duration::from_millis(50)), false);

            assert!(wait_until(WAIT, || !log.exits().is_empty()), "round {round}: no Exited: {:?}", log.events());
            let events = log.events();
            let exited = events.iter().position(|e| matches!(e, PaneEvent::Exited { .. })).unwrap();
            let framed = events.iter().position(|e| match e {
                PaneEvent::Frame(f) => {
                    f.changed.iter().any(|row| row.runs.iter().any(|run| run.0.contains("last-words")))
                }
                _ => false,
            });
            match framed {
                Some(i) => assert!(i < exited, "round {round}: the frame came after Exited: {events:?}"),
                None => panic!("round {round}: the last output never reached a frame: {events:?}"),
            }
            assert_eq!(log.exits(), vec![Some(2)]);
            assert!(pane.join(Duration::from_secs(3)));
            release.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn a_pane_the_poll_ended_gives_no_second_exited_when_it_is_closed_afterwards() {
        let (pane, log, release) = start("exit 3", Some(Duration::from_millis(50)), false);
        assert!(wait_until(WAIT, || !log.exits().is_empty()), "no Exited: {:?}", log.events());
        pane.close();
        thread::sleep(Duration::from_millis(300));
        assert_eq!(log.exits(), vec![Some(3)]);
        assert!(pane.join(Duration::from_secs(3)));
        release.store(true, Ordering::Relaxed);
    }

    #[test]
    fn a_running_shell_is_not_taken_for_an_exited_one_whatever_the_poll() {
        let (pane, log, release) = start("sleep 5", Some(Duration::from_millis(10)), false);
        thread::sleep(Duration::from_millis(500));
        assert!(log.exits().is_empty(), "{:?}", log.events());
        pane.close();
        assert!(wait_until(WAIT, || !log.exits().is_empty()));
        assert_eq!(log.exits().len(), 1);
        assert!(pane.join(Duration::from_secs(3)));
        release.store(true, Ordering::Relaxed);
    }
}

/// The animation clock of the pane against a stand-in engine that only counts: when the pane asks the engine for the
/// time of the next frame of a picture, when it wakes up for it, and when it asks nothing. The shell and the PTY are
/// real; the clock is the real monotonic one, so the timing bounds are generous and the counts are exact.
#[cfg(all(test, unix))]
mod image_clock_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// What the stand-in engine saw, shared with the test.
    #[derive(Default)]
    struct Seen {
        /// The `now_ms` of every call of `tick_images`, in order.
        ticks: Mutex<Vec<u64>>,
        updates: AtomicUsize,
        feeds: AtomicUsize,
        /// Every call that matters for order, as text.
        order: Mutex<Vec<String>>,
    }

    impl Seen {
        fn ticks(&self) -> Vec<u64> {
            self.ticks.lock().unwrap().clone()
        }

        fn order(&self) -> Vec<String> {
            self.order.lock().unwrap().clone()
        }
    }

    /// What the engine answers to `tick_images`.
    #[derive(Clone, Copy)]
    enum Mode {
        /// Nothing animates.
        Never,
        /// The first call is answered with a due time 50 ms later, every other call with `None`.
        Once50,
        /// A picture animates for good: the next frame is always `ms` after the call, until it is paused.
        Every(u64),
    }

    struct ClockEngine {
        seen: Arc<Seen>,
        mode: Mode,
        paused: bool,
        calls: usize,
        size: (u16, u16),
        reset: bool,
    }

    impl ClockEngine {
        fn new(seen: Arc<Seen>, mode: Mode) -> Self {
            Self { seen, mode, paused: false, calls: 0, size: (80, 24), reset: true }
        }
    }

    impl TerminalEngine for ClockEngine {
        fn feed(&mut self, _bytes: &[u8]) {
            self.seen.feeds.fetch_add(1, Ordering::Relaxed);
        }

        fn resize(&mut self, cols: u16, rows: u16, _cell_px: (u32, u32)) -> Result<()> {
            self.size = (cols, rows);
            self.reset = true;
            Ok(())
        }

        fn size(&self) -> Result<(u16, u16)> {
            Ok(self.size)
        }

        fn scroll(&mut self, _rows: isize) {}

        fn scroll_to_bottom(&mut self) {}

        fn screen(&mut self) -> Result<crate::engine::Screen> {
            Ok(crate::engine::Screen::default())
        }

        fn set_option_as_alt(&mut self, on: bool) {
            self.seen.order.lock().unwrap().push(format!("option_as_alt {on}"));
        }

        fn encode_key(&mut self, _event: &KeyEvent) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }

        fn encode_text(&mut self, text: &str) -> Result<Vec<u8>> {
            Ok(text.as_bytes().to_vec())
        }

        fn paste(&mut self, _text: &str, _allow_unsafe: bool) -> Result<PasteOutcome> {
            Ok(PasteOutcome::Written)
        }

        fn take_replies(&mut self) -> Vec<u8> {
            Vec::new()
        }

        fn update(&mut self) -> Result<crate::engine::Update> {
            let n = self.seen.updates.fetch_add(1, Ordering::Relaxed) as u64;
            let reset = std::mem::take(&mut self.reset);
            // Every update differs from the one before (the scrollbar counts), so every one is a frame that is sent
            // and that paces the next, as the updates of a real engine do while output changes the screen.
            Ok(crate::engine::Update {
                full: reset,
                reset,
                scrollbar: crate::engine::Scrollbar { total: n + 24, offset: 0, len: 24 },
                cols: self.size.0,
                rows: self.size.1,
                ..crate::engine::Update::default()
            })
        }

        fn request_full(&mut self) {
            self.reset = true;
        }

        fn tick_images(&mut self, now_ms: u64) -> Option<u64> {
            self.seen.ticks.lock().unwrap().push(now_ms);
            let call = self.calls;
            self.calls += 1;
            if self.paused {
                return None;
            }
            match self.mode {
                Mode::Never => None,
                Mode::Once50 => (call == 0).then_some(now_ms + 50),
                Mode::Every(ms) => Some(now_ms + ms),
            }
        }

        fn set_image_animation(&mut self, enabled: bool) {
            self.paused = !enabled;
            self.seen.order.lock().unwrap().push(format!("image_animation {enabled}"));
        }
    }

    fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(2));
        }
        condition()
    }

    /// A pane on a silent child (`sleep`) with the stand-in engine, and what the sink saw.
    fn start(mode: Mode, linger: Option<Duration>, script: &str) -> (PaneHandle, Arc<Seen>, Arc<Mutex<Vec<PaneEvent>>>) {
        let seen = Arc::new(Seen::default());
        let engine_seen = seen.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = events.clone();
        let mut spec = PaneSpec::new("/bin/sh", 80, 24).args(["-c", script]);
        spec.linger = linger;
        let pane = PaneHandle::spawn(
            1,
            spec,
            move |_| Ok(ClockEngine::new(engine_seen, mode)),
            move |event| sink_events.lock().unwrap().push(event),
        )
        .expect("the pane starts");
        (pane, seen, events)
    }

    fn end(pane: &PaneHandle) {
        pane.close();
        assert!(pane.join(Duration::from_secs(5)));
    }

    #[test]
    fn a_due_time_wakes_a_silent_pane_once_and_then_nothing_more_is_asked() {
        let (pane, seen, _events) = start(Mode::Once50, None, "sleep 30");
        // The first frame goes out at once and asks the clock; the answer was 50 ms later; the pane wakes for it
        // although no output arrives; the second answer is `None`.
        assert!(wait_until(Duration::from_secs(3), || seen.ticks().len() >= 2), "the pane never woke up for the picture");
        thread::sleep(Duration::from_millis(250));
        let ticks = seen.ticks();
        assert_eq!(ticks.len(), 2, "no more calls after `None`: {ticks:?}");
        assert!(ticks[0] < 1000, "the first call is at the start of the pane: {ticks:?}");
        assert!(ticks[1] >= ticks[0] + 50, "the second call is not before the due time: {ticks:?}");
        assert!(ticks[1] < ticks[0] + 1500, "the second call is about at the due time: {ticks:?}");
        assert_eq!(seen.updates.load(Ordering::Relaxed), 2, "one frame was built per call");
        end(&pane);
    }

    #[test]
    fn a_pane_whose_engine_never_animates_asks_once_for_its_first_frame_and_never_wakes_for_pictures() {
        let (pane, seen, _events) = start(Mode::Never, None, "sleep 30");
        assert!(wait_until(Duration::from_secs(3), || !seen.ticks().is_empty()));
        thread::sleep(Duration::from_millis(300));
        assert_eq!(seen.ticks().len(), 1, "no wake: {:?}", seen.ticks());
        assert_eq!(seen.updates.load(Ordering::Relaxed), 1);
        end(&pane);
    }

    #[test]
    fn the_clock_is_asked_once_per_frame_before_the_update_with_milliseconds_since_the_start() {
        let (pane, seen, _events) = start(Mode::Every(20), None, "sleep 30");
        assert!(wait_until(Duration::from_secs(3), || seen.ticks().len() >= 5));
        let ticks = seen.ticks();
        assert!(ticks.windows(2).all(|pair| pair[0] <= pair[1]), "the clock goes forward: {ticks:?}");
        assert!(ticks[0] < 1000, "milliseconds since the pane started, not since the epoch: {ticks:?}");
        // One call per frame: the calls and the updates go together (the update follows the call at once).
        end(&pane);
        let (calls, updates) = (seen.ticks().len(), seen.updates.load(Ordering::Relaxed));
        assert_eq!(calls, updates, "one `tick_images` per `update`");
    }

    #[test]
    fn switching_the_animation_off_reaches_the_engine_in_order_and_stops_every_wake() {
        let (pane, seen, _events) = start(Mode::Every(20), None, "sleep 30");
        assert!(wait_until(Duration::from_secs(3), || seen.ticks().len() >= 3));
        // The start itself told the engine the option setting of the spec; what follows is what the test sent.
        seen.order.lock().unwrap().clear();
        // Ordered with the other controls of the pane.
        pane.set_option_as_alt(true).unwrap();
        pane.set_image_animation(false).unwrap();
        pane.set_option_as_alt(false).unwrap();
        assert!(wait_until(Duration::from_secs(3), || seen.order().len() >= 3));
        assert_eq!(seen.order(), ["option_as_alt true", "image_animation false", "option_as_alt false"]);
        // The engine answers `None` while paused and the pane has dropped its due time: it asks nothing more.
        thread::sleep(Duration::from_millis(100));
        let calls = seen.ticks().len();
        thread::sleep(Duration::from_millis(300));
        assert_eq!(seen.ticks().len(), calls, "a paused animation costs no wake");
        end(&pane);
    }

    #[test]
    fn a_spec_that_asks_for_no_animation_pauses_the_engine_before_the_shell_runs() {
        let seen = Arc::new(Seen::default());
        let engine_seen = seen.clone();
        let mut spec = PaneSpec::new("/bin/sh", 80, 24).args(["-c", "sleep 30"]);
        spec.image_animation = false;
        let pane = PaneHandle::spawn(1, spec, move |_| Ok(ClockEngine::new(engine_seen, Mode::Every(20))), |_| {})
            .expect("the pane starts");
        // The engine heard it at the start, and the pane never wakes for a picture: the engine answers `None`.
        assert_eq!(seen.order(), ["option_as_alt false", "image_animation false"]);
        thread::sleep(Duration::from_millis(300));
        assert_eq!(seen.ticks().len(), 1, "one call for the first frame and no wake: {:?}", seen.ticks());
        end(&pane);

        // The default leaves the engine alone.
        let (pane, seen, _events) = start(Mode::Never, None, "sleep 30");
        assert_eq!(seen.order(), ["option_as_alt false"]);
        end(&pane);
    }

    #[test]
    fn switching_the_animation_on_makes_the_next_frame_ask_the_clock_again() {
        let (pane, seen, _events) = start(Mode::Never, None, "sleep 30");
        assert!(wait_until(Duration::from_secs(3), || seen.ticks().len() == 1));
        thread::sleep(Duration::from_millis(100));
        assert_eq!(seen.ticks().len(), 1);
        seen.order.lock().unwrap().clear();

        pane.set_image_animation(true).unwrap();
        assert!(wait_until(Duration::from_secs(3), || seen.ticks().len() == 2), "no frame after the switch");
        assert_eq!(seen.order(), ["image_animation true"]);
        end(&pane);
    }

    #[test]
    fn a_burst_of_output_chunks_asks_the_clock_once_per_frame_and_not_once_per_chunk() {
        let (pane, seen, _events) = start(Mode::Every(1000), None, "sleep 30");
        assert!(wait_until(Duration::from_secs(3), || !seen.ticks().is_empty()));
        let (ticks_before, updates_before) = (seen.ticks().len(), seen.updates.load(Ordering::Relaxed));
        for _ in 0..10_000 {
            assert!(pane.shared.queue.push_output(vec![b'x']), "the pane is gone");
        }
        assert!(wait_until(Duration::from_secs(10), || seen.feeds.load(Ordering::Relaxed) == 10_000), "not all fed");
        thread::sleep(Duration::from_millis(100));
        // Counted after the pane has ended: a frame in progress has asked the clock and not yet updated.
        end(&pane);
        let ticks = seen.ticks().len() - ticks_before;
        let frames = seen.updates.load(Ordering::Relaxed) - updates_before;
        assert!(ticks <= frames, "{ticks} calls for {frames} frames");
        assert!(frames < 1000, "{frames} frames for 10000 chunks: frames are paced, not per chunk");
    }

    #[test]
    fn a_held_pane_makes_no_tick_even_when_the_engine_would_animate_for_good() {
        // The shell ends at once with a status that is not 0, so the pane is held with its notice line.
        let (pane, seen, events) = start(Mode::Every(10), Some(Duration::from_secs(60)), "exit 3");
        assert!(
            wait_until(Duration::from_secs(5), || events.lock().unwrap().iter().any(|e| matches!(e, PaneEvent::Held { .. }))),
            "the pane was never held"
        );
        let at_hold = seen.ticks().len();
        // Controls of a held pane are still met, and the switch to `true` marks the pane dirty: still no tick.
        pane.set_image_animation(true).unwrap();
        pane.request_full().unwrap();
        thread::sleep(Duration::from_millis(300));
        assert_eq!(seen.ticks().len(), at_hold, "a held pane asked the clock");
        assert!(seen.updates.load(Ordering::Relaxed) > at_hold, "the held pane still builds its frames");
        end(&pane);
    }
}

#[cfg(test)]
mod tap_queue_tests {
    use super::*;

    fn pair(bound: usize) -> (TapSink, TapReceiver) {
        let shared = Arc::new(TapShared {
            state: Mutex::new(TapState::default()),
            ready: Condvar::new(),
            receiver_gone: AtomicBool::new(false),
        });
        (TapSink { id: TapId::next(), shared: shared.clone(), bound }, TapReceiver { shared })
    }

    fn queued(rx: &TapReceiver) -> usize {
        rx.shared.lock().bytes
    }

    #[test]
    fn a_chunk_that_would_pass_the_bound_ends_the_tap_with_overflow_and_drops_what_was_queued() {
        let (tap, rx) = pair(100);
        assert!(tap.push(&[1; 60]));
        assert!(tap.push(&[2; 40]), "exactly the bound fits");
        assert_eq!(queued(&rx), 100);
        assert!(!tap.push(&[3]), "one byte over ends the tap");
        assert_eq!(queued(&rx), 0, "the queue is dropped with the tap, so the memory is freed at once");
        assert_eq!(rx.recv(Duration::ZERO), Some(TapMsg::End(TapEnd::Overflow)));
        assert_eq!(rx.recv(Duration::ZERO), Some(TapMsg::End(TapEnd::Overflow)), "the end repeats");
        assert!(!tap.push(&[4]), "a finished tap takes nothing");
    }

    #[test]
    fn what_the_reader_takes_makes_room_and_the_held_bytes_never_pass_the_bound() {
        let (tap, rx) = pair(64);
        for round in 0..1000u32 {
            assert!(tap.push(&[7; 40]), "round {round}");
            assert!(queued(&rx) <= 64);
            assert_eq!(rx.recv(Duration::ZERO), Some(TapMsg::Bytes(vec![7; 40])));
        }
        assert_eq!(rx.recv(Duration::ZERO), None, "nothing is waiting");
    }

    #[test]
    fn a_closed_tap_yields_its_queued_bytes_first_and_a_detached_one_yields_none() {
        let (tap, rx) = pair(100);
        assert!(tap.push(b"abc"));
        tap.close(false);
        assert_eq!(rx.recv(Duration::ZERO), Some(TapMsg::Bytes(b"abc".to_vec())));
        assert_eq!(rx.recv(Duration::ZERO), Some(TapMsg::End(TapEnd::Closed)));

        let (tap, rx) = pair(100);
        assert!(tap.push(b"abc"));
        tap.close(true);
        assert_eq!(rx.recv(Duration::ZERO), Some(TapMsg::End(TapEnd::Closed)), "detached: at once");
    }

    #[test]
    fn a_dropped_receiver_ends_the_tap_on_the_next_chunk() {
        let (tap, rx) = pair(100);
        assert!(tap.push(b"a"));
        drop(rx);
        assert!(!tap.push(b"b"));
    }

    #[test]
    fn a_waiting_reader_is_woken_by_a_chunk() {
        let (tap, rx) = pair(100);
        let reader = thread::spawn(move || rx.recv(Duration::from_secs(10)));
        thread::sleep(Duration::from_millis(50));
        assert!(tap.push(b"wake"));
        assert_eq!(reader.join().unwrap(), Some(TapMsg::Bytes(b"wake".to_vec())));
    }
}
