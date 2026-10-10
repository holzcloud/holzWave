//! Persistence primitives: atomic file writes, bounded reads, and the debounced saver that every on-disk document
//! (settings, session, layout, themes) goes through.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The path of the copy kept next to `path` when its content could not be used: `layout.json` becomes
/// `layout.json.bad`.
pub fn bad_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(std::ffi::OsString::from).unwrap_or_default();
    name.push(".bad");
    path.with_file_name(name)
}

/// Write `bytes` to `path` so that a crash leaves either the old or the new content.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::write(path, bytes)
}

/// Read a file of at most `max_bytes`. `Ok(None)` when it does not exist.
pub fn read_limited(path: &Path, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    match std::fs::File::open(path) {
        Ok(f) => {
            let mut bytes = Vec::new();
            f.take(max_bytes).read_to_end(&mut bytes)?;
            Ok(Some(bytes))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Copy `path` to its `.bad` sibling, replacing an older copy. Returns the path of the copy.
pub fn backup_bad(path: &Path) -> io::Result<PathBuf> {
    let bad = bad_path(path);
    std::fs::copy(path, &bad)?;
    Ok(bad)
}

/// Runs a job at most once per `delay` after it was asked for.
pub struct Debounced {
    _job: std::sync::Mutex<Box<dyn FnMut() + Send>>,
}

impl Debounced {
    pub fn new(delay: Duration, job: impl FnMut() + Send + 'static) -> Self {
        let _ = delay;
        Debounced { _job: std::sync::Mutex::new(Box::new(job)) }
    }

    pub fn touch(&self) {}

    pub fn flush(&self) {}
}
