//! The `wsh` stand-in that the shell tests put first on the PATH of their shells.
//!
//! The script appends the arguments of every call to the file that the variable [`LOG_ENV`] names, one line per call, so
//! each test has its own log and the script itself keeps no state. One script serves all the tests of a test process, and
//! it is started once, here, before any shell can start it.
//!
//! The reason is the first start of a new executable. On macOS a new file (a script counts) is assessed by `syspolicyd`
//! before it runs for the first time: that takes 0.1 to 0.2 s on an idle machine and seconds (a timeout of about 5.5 s
//! was measured) while builds create executables and keep the daemon busy. A script of its own per test, started for the
//! first time by the shell in the background, was late by seconds under load, and a test that gave the key 3 s to call it
//! failed with "the stub was called 0 times". Started once and blocking, the cost is paid outside of every timed wait, and
//! each later start, by any test, takes milliseconds: a call that does not come is then really missing.

use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

/// The environment variable that names the file the stub appends the arguments of its calls to. A call without it (the
/// first start below) is discarded.
pub const LOG_ENV: &str = "HOLZWAVE_STUB_LOG";

/// How long the first start of the stub may take before the test panics.
const FIRST_START: Duration = Duration::from_secs(120);

/// The directory with the `wsh` script, shared by the tests of one test process.
pub struct ShellStub {
    dir: PathBuf,
}

impl ShellStub {
    /// The stub of this test process: made and started once on the first call, while other callers wait, and removed when
    /// the last test that holds it is done (a later test then makes it again).
    pub fn shared() -> Arc<ShellStub> {
        static SHARED: Mutex<Weak<ShellStub>> = Mutex::new(Weak::new());
        let mut shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(stub) = shared.upgrade() {
            return stub;
        }
        let stub = Arc::new(ShellStub::create());
        *shared = Arc::downgrade(&stub);
        stub
    }

    /// The directory to put first on the PATH of a shell. It holds one file, `wsh`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn create() -> ShellStub {
        // The value owns the directory from here on, so that a panic below removes it.
        let stub = ShellStub { dir: super::scratch_dir("shell-stub") };
        let script = stub.dir.join("wsh");
        std::fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"${{{LOG_ENV}:-/dev/null}}\"\n")).expect("write the stub");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("make the stub executable");
        let began = Instant::now();
        let mut child = loop {
            // Another test may fork while the file is still open for writing in its child until that child execs: the start
            // is refused as "text file busy" for that moment.
            match Command::new(&script).arg("first start").env_remove(LOG_ENV).spawn() {
                Ok(child) => break child,
                Err(e) if e.kind() == ErrorKind::ExecutableFileBusy && began.elapsed() < FIRST_START => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => panic!("could not start the stub {}: {e}", script.display()),
            }
        };
        loop {
            match child.try_wait().expect("wait for the stub") {
                Some(status) => {
                    assert!(status.success(), "the stub {} ended with {status} on its first start", script.display());
                    return stub;
                }
                None if began.elapsed() > FIRST_START => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("the first start of the new executable {} did not finish in {} s (macOS assesses every new executable once)", script.display(), FIRST_START.as_secs());
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}

impl Drop for ShellStub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
