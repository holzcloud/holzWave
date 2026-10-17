//! The system backend: the Rust side of the stream ABI to the Swift package (macOS only).
//!
//! This is the second and last home of foreign calls in the crate, next to the module of `lib.rs` that asks for the
//! availability. Functions of the Swift package cross the ABI here (`hw_afm_generate` and `hw_afm_cancel`; see the
//! module documentation of the backend for the others as they are added). Only reply text and an ASCII case label come back: no framework error text, no payload and no
//! prompt.
//!
//! Ownership of a call's state follows one pattern. The state sits behind an `Arc`; one strong reference is handed to
//! Swift as an opaque pointer and the callback only borrows it. The waiter releases that reference after the terminal
//! event has been delivered and its callback has returned (and a short grace period has passed to catch an event that
//! must not come), and **leaks** it when the call is abandoned, so that a late callback still finds live memory.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use holzwave_core::ai::provider::{CancelToken, Flow};

use crate::Availability;
use crate::backend::{AppleError, Backend, GenerateRequest, MAX_INPUT_BYTES};
use crate::pump::{Event, Next, Pump, PumpLimits};

unsafe extern "C" {
    fn hw_afm_generate(
        instructions: *const u8,
        instructions_len: usize,
        prompt: *const u8,
        prompt_len: usize,
        max_response_tokens: i32,
        on_event: extern "C" fn(*mut c_void, i32, *const u8, usize),
        user_data: *mut c_void,
    ) -> i64;
    fn hw_afm_cancel(handle: i64);
}

/// Event kinds of the stream ABI.
const KIND_SNAPSHOT: i32 = 1;
const KIND_END: i32 = 2;
const KIND_ERROR: i32 = 3;

/// How often the loop looks at the clock and the cancel token when no event arrives.
const POLL: Duration = Duration::from_millis(20);
/// How long a released call waits for an event that must not come, so that a late one is counted.
const GRACE: Duration = Duration::from_millis(20);
/// How long the waiter gives a callback that has delivered its last event to return.
const RETURN_WAIT: Duration = Duration::from_millis(200);

/// Events that arrived after a terminal one, over all calls of the process.
static LATE_EVENTS: AtomicU64 = AtomicU64::new(0);

/// The number of callbacks that arrived after the terminal event of their call. The Swift side promises none, so the
/// live tests assert that this stays zero.
#[doc(hidden)]
pub fn late_events() -> u64 {
    LATE_EVENTS.load(Ordering::SeqCst)
}

/// Asks the model to stop the call with `handle`. Harmless for an unknown or a finished handle. It exists for tests.
#[doc(hidden)]
pub fn cancel_handle(handle: i64) {
    // SAFETY: the function takes a plain integer and looks the handle up under a lock; an unknown handle does nothing.
    unsafe { hw_afm_cancel(handle) }
}

/// Shared between the waiter and the callback of one call.
struct Shared {
    tx: Mutex<Sender<Event>>,
    /// Set when the callback begins to handle the model's own end or error event.
    terminal: AtomicBool,
    /// Set as the very last action of the callback that handled the terminal event.
    returned: AtomicBool,
    max_reply_bytes: usize,
}

/// Called by Swift on a thread of its concurrency pool. It only borrows `user_data`, never panics and never blocks.
extern "C" fn on_event(user_data: *mut c_void, kind: i32, bytes: *const u8, len: usize) {
    // SAFETY: `user_data` is the pointer `generate` got from `Arc::into_raw`. The reference it stands for is released
    // only after the terminal event has been handled and its callback has returned, or it is leaked; the callback
    // never frees it.
    let shared = unsafe { &*(user_data as *const Shared) };
    if shared.terminal.load(Ordering::SeqCst) {
        LATE_EVENTS.fetch_add(1, Ordering::SeqCst);
        return;
    }
    let is_terminal = kind != KIND_SNAPSHOT;
    if is_terminal {
        shared.terminal.store(true, Ordering::SeqCst);
    }
    let event = if kind == KIND_SNAPSHOT {
        snapshot_event(shared, bytes, len)
    } else if kind == KIND_END {
        Event::End
    } else if kind == KIND_ERROR {
        match copy_text(bytes, len, 64) {
            Some(label) => Event::Error(AppleError::from_label(&label)),
            None => Event::Error(AppleError::Other),
        }
    } else {
        Event::Error(AppleError::Other)
    };
    let sender = shared.tx.lock().unwrap_or_else(PoisonError::into_inner);
    // The receiver is gone when the call was abandoned; the event then has nowhere to go.
    let _ = sender.send(event);
    drop(sender);
    if is_terminal {
        // Nothing touches the state after this store.
        shared.returned.store(true, Ordering::Release);
    }
}

fn snapshot_event(shared: &Shared, bytes: *const u8, len: usize) -> Event {
    if len > shared.max_reply_bytes {
        return Event::Oversize;
    }
    match copy_text(bytes, len, shared.max_reply_bytes) {
        Some(text) => Event::Snapshot(text),
        None => Event::Malformed,
    }
}

/// Copies `len` bytes as text; `None` for a null pointer with a length, a length over `max` or invalid UTF-8.
fn copy_text(bytes: *const u8, len: usize, max: usize) -> Option<String> {
    if len == 0 {
        return Some(String::new());
    }
    if bytes.is_null() || len > max {
        return None;
    }
    // SAFETY: Swift passes a pointer to `len` readable bytes that stay valid for the duration of the callback; the
    // bytes are copied before the callback returns.
    let slice = unsafe { std::slice::from_raw_parts(bytes, len) };
    String::from_utf8(slice.to_vec()).ok()
}

/// The system language model of this Mac, through the Swift package.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemBackend;

impl SystemBackend {
    /// The backend of this Mac. It holds no state: every call asks the system.
    pub fn new() -> Self {
        Self
    }
}

impl Backend for SystemBackend {
    fn availability(&self) -> Availability {
        crate::probe()
    }

    fn generate(
        &self,
        req: &GenerateRequest,
        sink: &mut dyn FnMut(&str) -> Flow,
        cancel: &CancelToken,
    ) -> Result<(), AppleError> {
        if req.instructions.len() > MAX_INPUT_BYTES || req.prompt.len() > MAX_INPUT_BYTES {
            return Err(AppleError::ContextExceeded);
        }
        if cancel.is_cancelled() {
            return Err(AppleError::Cancelled);
        }
        let limits = PumpLimits::standard();
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            tx: Mutex::new(tx),
            terminal: AtomicBool::new(false),
            returned: AtomicBool::new(false),
            max_reply_bytes: limits.max_reply_bytes,
        });
        let raw = Arc::into_raw(Arc::clone(&shared)) as *mut c_void;
        let max_tokens = i32::try_from(req.max_response_tokens).unwrap_or(i32::MAX).clamp(1, 8192);
        // SAFETY: both buffers are valid for their lengths during the call and Swift copies them before it returns;
        // `on_event` has the C signature the Swift side expects; `raw` stays valid until the reference it stands for is
        // released below or leaked.
        let handle = unsafe {
            hw_afm_generate(
                req.instructions.as_ptr(),
                req.instructions.len(),
                req.prompt.as_ptr(),
                req.prompt.len(),
                max_tokens,
                on_event,
                raw,
            )
        };
        if handle <= 0 {
            // The call was refused: the callback is never called, so the reference is ours to release.
            // SAFETY: `raw` came from `Arc::into_raw` above and Swift keeps no copy of it.
            drop(unsafe { Arc::from_raw(raw as *const Shared) });
            return Err(AppleError::Other);
        }

        let mut pump = Pump::new(limits, Instant::now());
        let mut next = Next::Wait;
        let result = loop {
            match next {
                Next::Done(result) => break result,
                Next::Cancel => {
                    // SAFETY: plain integer argument; a finished or unknown handle is harmless.
                    unsafe { hw_afm_cancel(handle) };
                    next = Next::Wait;
                }
                Next::Deliver(text) => {
                    if sink(&text) == Flow::Stop {
                        pump.stop_requested();
                    }
                    next = pump.tick(Instant::now(), cancel.is_cancelled());
                }
                Next::Wait => {
                    next = match rx.recv_timeout(POLL) {
                        Ok(event) => {
                            let after = pump.event(event, Instant::now());
                            if after == Next::Wait && cancel.is_cancelled() {
                                pump.tick(Instant::now(), true)
                            } else {
                                after
                            }
                        }
                        Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                            pump.tick(Instant::now(), cancel.is_cancelled())
                        }
                    };
                }
            }
        };

        release(&shared, raw);
        result
    }
}

/// Releases the callback's reference to `shared` when the model has delivered its last event and the callback has
/// returned; otherwise leaks it (the call was abandoned, and a late callback must find live memory).
fn release(shared: &Arc<Shared>, raw: *mut c_void) {
    if !shared.terminal.load(Ordering::SeqCst) {
        return;
    }
    let deadline = Instant::now() + RETURN_WAIT;
    while !shared.returned.load(Ordering::Acquire) {
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    // An event after the terminal one is counted by the callback; give one the chance to show itself.
    std::thread::sleep(GRACE);
    // SAFETY: `raw` came from `Arc::into_raw` in `generate` and has not been released; the terminal callback has
    // returned and the Swift side sends nothing after a terminal event.
    drop(unsafe { Arc::from_raw(raw as *const Shared) });
}
