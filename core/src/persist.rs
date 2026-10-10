//! Persistence primitives: atomic file writes, bounded reads, and the debounced saver that every on-disk document
//! (settings, session, layout, themes) goes through.
//!
//! std only. The three functions work on paths; [`Debounced`] decouples "something changed" (cheap, called from
//! a pointer move) from "write it to disk" (a serialise and an fsync).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// `path` with `suffix` appended to its file name: `layout.json` and `.tmp` give `layout.json.tmp`.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(std::ffi::OsString::from).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

/// The path of the copy kept next to `path` when its content could not be used: `layout.json` becomes
/// `layout.json.bad`.
pub fn bad_path(path: &Path) -> PathBuf {
    sibling(path, ".bad")
}

/// Write `bytes` to `path` so that a crash or power loss leaves either the old or the new content, never a
/// truncated file: the bytes go to `<path>.tmp` next to it, are flushed to disk (`sync_all`), and only then
/// renamed over the target.
///
/// Missing parent directories are created. On failure nothing is left behind and the error is returned; the
/// caller decides whether to log it, nobody may ignore it silently.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let tmp = sibling(path, ".tmp");
    let written = (|| {
        // A leftover from an earlier crash goes first, and `create_new` then refuses anything planted in the
        // meantime (a symlink would otherwise redirect the write).
        match fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    let result = written.and_then(|()| fs::rename(&tmp, path));
    match &result {
        Ok(()) => {
            // Best effort: make the rename itself durable. Not every platform can open a directory.
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                if let Ok(dir) = File::open(dir) {
                    let _ = dir.sync_all();
                }
            }
        }
        Err(_) => {
            let _ = fs::remove_file(&tmp);
        }
    }
    result
}

/// Read a file of at most `max_bytes`.
///
/// `Ok(None)` when it does not exist. A file that is larger than the limit is an `InvalidData` error and is
/// not read at all: the length is checked first, and the read itself is bounded again in case the file grows.
/// Anything that is not a regular file (a directory, a pipe that would block) is an `InvalidInput` error.
pub fn read_limited(path: &Path, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a regular file", path.display())));
    }
    let too_large = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is larger than the limit of {max_bytes} bytes", path.display()),
        )
    };
    if meta.len() > max_bytes {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    File::open(path)?.take(max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(too_large());
    }
    Ok(Some(bytes))
}

/// Copy `path` to its `.bad` sibling ([`bad_path`]), replacing an older copy. Returns the path of the copy.
///
/// Called before a damaged file can be overwritten by the next save, so the user keeps what they had.
pub fn backup_bad(path: &Path) -> io::Result<PathBuf> {
    let bad = bad_path(path);
    fs::copy(path, &bad)?;
    Ok(bad)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

type Job = Box<dyn FnMut() + Send>;

/// A job that panics is logged and does not take the saver down with it.
fn run(job: &mut Job) {
    if catch_unwind(AssertUnwindSafe(|| job())).is_err() {
        eprintln!("holzwave: a save job panicked");
    }
}

struct State {
    /// When the pending run is due; `None` when nothing is pending.
    due: Option<Instant>,
    shutdown: bool,
}

struct Shared {
    delay: Duration,
    state: Mutex<State>,
    wake: Condvar,
    /// Held for the whole run of the job, by the worker and by `flush` alike, so they never overlap.
    /// Lock order is always `job` before `state`.
    job: Mutex<Job>,
}

/// Runs a save job on its own thread, at most once per `delay`, however often it is asked for.
///
/// `touch` is cheap and may be called from anywhere, any number of times. The first `touch` while nothing is
/// pending schedules one run `delay` later; touches before it fires add nothing. The deadline is not pushed
/// back by later touches, so a drag that never pauses still writes every `delay`.
///
/// `flush` (and dropping the value) runs a pending job right away on the calling thread, after waiting for a
/// run that is already in progress, so that a flush on exit returns only when everything is on disk.
pub struct Debounced {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Debounced {
    pub fn new(delay: Duration, job: impl FnMut() + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            delay,
            state: Mutex::new(State { due: None, shutdown: false }),
            wake: Condvar::new(),
            job: Mutex::new(Box::new(job)),
        });
        let for_worker = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("holzwave-saver".into())
            .spawn(move || worker(&for_worker))
            .map_err(|e| eprintln!("holzwave: cannot start the saver thread: {e}"))
            .ok();
        Debounced { shared, worker }
    }

    /// Ask for the job to run. Cheap; coalesces with every touch that is already pending.
    pub fn touch(&self) {
        let mut state = lock(&self.shared.state);
        if state.due.is_none() {
            state.due = Some(Instant::now() + self.shared.delay);
            self.shared.wake.notify_one();
        }
    }

    /// Run a pending job now, on this thread. Returns after any run in progress has finished; does nothing
    /// when nothing is pending.
    pub fn flush(&self) {
        let mut job = lock(&self.shared.job);
        let pending = lock(&self.shared.state).due.take().is_some();
        if pending {
            run(&mut job);
        }
    }
}

impl Drop for Debounced {
    fn drop(&mut self) {
        self.flush();
        lock(&self.shared.state).shutdown = true;
        self.shared.wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn worker(shared: &Shared) {
    loop {
        {
            let mut state = lock(&shared.state);
            loop {
                if state.shutdown {
                    return;
                }
                match state.due {
                    None => state = shared.wake.wait(state).unwrap_or_else(PoisonError::into_inner),
                    Some(at) => {
                        let now = Instant::now();
                        if now >= at {
                            break;
                        }
                        state = shared.wake.wait_timeout(state, at - now).unwrap_or_else(PoisonError::into_inner).0;
                    }
                }
            }
        }
        // Take the job lock before the pending flag: a `flush` that arrives now waits for this run instead of
        // finding the flag already cleared and returning before the write has happened.
        let mut job = lock(&shared.job);
        let due = {
            let mut state = lock(&shared.state);
            if state.shutdown {
                return;
            }
            match state.due {
                Some(at) if Instant::now() >= at => {
                    state.due = None;
                    true
                }
                _ => false,
            }
        };
        if due {
            run(&mut job);
        }
    }
}
