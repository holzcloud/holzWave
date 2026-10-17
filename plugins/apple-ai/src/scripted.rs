//! A backend that plays a script: the injectable test double for availability, the window size, token counts, deltas,
//! stalls and errors. It works on every host, so everything the provider does is tested without a Mac.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use holzwave_core::ai::provider::{CancelToken, Flow};

use crate::Availability;
use crate::backend::{AppleError, Backend, GenerateRequest};

enum Step {
    Delta(String),
    Stall(Duration),
    Fail(AppleError),
    Panic,
}

/// The steps of one call to the model. Built with chained calls; one script is used up by each `generate`.
#[derive(Default)]
pub struct BackendScript {
    steps: Vec<Step>,
}

impl BackendScript {
    /// An empty script: the call ends cleanly with no text.
    pub fn new() -> Self {
        Self::default()
    }

    /// A script with one delta.
    pub fn reply(text: impl Into<String>) -> Self {
        Self::new().delta(text)
    }

    /// Adds a delta.
    pub fn delta(mut self, text: impl Into<String>) -> Self {
        self.steps.push(Step::Delta(text.into()));
        self
    }

    /// Adds a pause that a cancel ends early.
    pub fn stall(mut self, wait: Duration) -> Self {
        self.steps.push(Step::Stall(wait));
        self
    }

    /// Ends the call with `error`.
    pub fn fail(mut self, error: AppleError) -> Self {
        self.steps.push(Step::Fail(error));
        self
    }

    /// Makes the call panic.
    pub fn panic(mut self) -> Self {
        self.steps.push(Step::Panic);
        self
    }
}

struct Inner {
    availability: Availability,
    context_size: Option<u32>,
    exact_counts: bool,
    scripts: VecDeque<BackendScript>,
    requests: Vec<GenerateRequest>,
}

/// The model, played from scripts.
///
/// Availability defaults to [`Availability::Available`], the window to unknown, and exact token counts to off. A call
/// with no script left answers [`AppleError::Other`].
pub struct ScriptedBackend {
    inner: Mutex<Inner>,
    calls: AtomicUsize,
    count_calls: AtomicUsize,
    running: AtomicUsize,
    max_running: AtomicUsize,
}

impl Default for ScriptedBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Counts a call while it runs, also when it panics.
struct Running<'a>(&'a AtomicUsize);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ScriptedBackend {
    /// A backend that is available, with no window, no exact counts and no script.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                availability: Availability::Available,
                context_size: None,
                exact_counts: false,
                scripts: VecDeque::new(),
                requests: Vec::new(),
            }),
            calls: AtomicUsize::new(0),
            count_calls: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            max_running: AtomicUsize::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets what [`Backend::availability`] answers.
    pub fn with_availability(self, availability: Availability) -> Self {
        self.set_availability(availability);
        self
    }

    /// Changes the availability of a backend that is already shared, like the system changing it while the app runs.
    pub fn set_availability(&self, availability: Availability) {
        self.lock().availability = availability;
    }

    /// Sets the window the backend reports.
    pub fn with_context_size(self, size: Option<u32>) -> Self {
        self.lock().context_size = size;
        self
    }

    /// Turns the exact counter on: it answers the number of characters divided by four, rounded up.
    pub fn with_exact_counts(self, on: bool) -> Self {
        self.lock().exact_counts = on;
        self
    }

    /// Queues the script of the next call.
    pub fn with_turn(self, script: BackendScript) -> Self {
        self.lock().scripts.push_back(script);
        self
    }

    /// The requests the backend was called with, in order.
    pub fn requests(&self) -> Vec<GenerateRequest> {
        self.lock().requests.clone()
    }

    /// How many times `generate` was called.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// How many times `count_tokens` was called.
    pub fn count_calls(&self) -> usize {
        self.count_calls.load(Ordering::SeqCst)
    }

    /// The most calls to `generate` that ran at the same time.
    pub fn max_concurrent(&self) -> usize {
        self.max_running.load(Ordering::SeqCst)
    }
}

impl Backend for ScriptedBackend {
    fn availability(&self) -> Availability {
        self.lock().availability
    }

    fn context_size(&self) -> Option<u32> {
        self.lock().context_size
    }

    fn count_tokens(&self, text: &str) -> Option<u32> {
        self.count_calls.fetch_add(1, Ordering::SeqCst);
        if !self.lock().exact_counts {
            return None;
        }
        Some(u32::try_from(text.chars().count().div_ceil(4)).unwrap_or(u32::MAX))
    }

    fn generate(
        &self,
        req: &GenerateRequest,
        sink: &mut dyn FnMut(&str) -> Flow,
        cancel: &CancelToken,
    ) -> Result<(), AppleError> {
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        let _running = Running(&self.running);
        self.max_running.fetch_max(now, Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let script = {
            let mut inner = self.lock();
            inner.requests.push(req.clone());
            inner.scripts.pop_front()
        };
        if cancel.is_cancelled() {
            return Err(AppleError::Cancelled);
        }
        let Some(script) = script else { return Err(AppleError::Other) };
        for step in script.steps {
            if cancel.is_cancelled() {
                return Err(AppleError::Cancelled);
            }
            match step {
                Step::Delta(text) => {
                    if sink(&text) == Flow::Stop {
                        return Err(AppleError::Cancelled);
                    }
                }
                Step::Stall(wait) => {
                    if cancel.wait_cancelled(wait) {
                        return Err(AppleError::Cancelled);
                    }
                }
                Step::Fail(error) => return Err(error),
                Step::Panic => panic!("scripted panic inside the backend"),
            }
        }
        Ok(())
    }
}
