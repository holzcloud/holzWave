//! Shared helpers for the core integration tests.
//!
//! Include it from a test file with `mod common;`. Each test crate uses only a subset of the helpers, so dead code
//! is allowed here.
#![allow(dead_code)]

/// An in-process HTTP/1.1 server for the AI tests: chunked event streams, stalls, drops, error statuses, request capture.
pub mod fake_http;
/// The `wsh` stand-in of the shell tests, started once per test process so that no test waits for its first start.
#[cfg(unix)]
pub mod shell_stub;
/// The one in-process SSH server of the integration tests: logins by password and key, a shell, exec and a jump host.
pub mod ssh_server;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[cfg(feature = "ghostty")]
use holzwave_core::GhosttyEngine;

/// Build an engine with the given grid size and 1000 lines of scrollback.
///
/// Panics when the engine cannot be created, which is a test failure by design.
#[cfg(feature = "ghostty")]
pub fn engine(cols: u16, rows: u16) -> GhosttyEngine {
    GhosttyEngine::new(cols, rows, 1000).expect("engine")
}

/// Escape bytes as ASCII text so that assertions on byte streams read well: `ESC [ A` becomes the text `\x1b[A`.
pub fn ascii(bytes: &[u8]) -> String {
    bytes.escape_ascii().to_string()
}

/// A small deterministic random number generator (xorshift64).
///
/// Hand-rolled on purpose: invariant tests need reproducible sequences from a seed, not a dependency (D-03). A
/// failing test prints its seed and can be replayed exactly.
#[derive(Debug, Clone)]
pub struct Xorshift64 {
    state: u64,
}

impl Xorshift64 {
    /// Start a sequence from `seed`. The generator cannot run from an all-zero state, so a zero seed is replaced by
    /// a fixed non-zero constant.
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed },
        }
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// A number in `0..n`. Panics when `n` is zero.
    pub fn below(&mut self, n: usize) -> usize {
        assert!(n > 0, "below(0) has no result");
        (self.next_u64() % n as u64) as usize
    }

    /// True with the given probability in percent (0 never, 100 always).
    pub fn chance(&mut self, percent: u8) -> bool {
        self.below(100) < usize::from(percent)
    }
}

/// Create a fresh, empty scratch directory `holzwave-{tag}-{pid}-{n}` under the system temp directory.
///
/// The directory is made with `create_dir`, never `create_dir_all`: when the path already exists, the call panics
/// instead of silently reusing it, so a planted directory or symlink cannot redirect a test's writes. The caller
/// removes the directory at the end of the test.
pub fn scratch_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("holzwave-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir(&path)
        .unwrap_or_else(|e| panic!("scratch dir {} must not exist yet: {e}", path.display()));
    path
}

/// Poll `f` every 5 ms until it returns true or `timeout` has passed. Returns whether the condition was met.
///
/// `f` is evaluated once more at the deadline, so a condition that became true during the last sleep still counts.
pub fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if f() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The directory that holds the fixtures shared between the Rust tests and the TypeScript tests.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures")
}

/// True when the environment variable `UPDATE_FIXTURES` is `1`: tests that own a fixture rewrite it instead of
/// comparing against it.
pub fn update_fixtures() -> bool {
    std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
}
