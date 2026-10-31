//! A scratch directory for a test: a fresh directory under the system temporary directory, removed when the value drops.
//!
//! The directory is made with `create_dir`, never `create_dir_all`, and with mode 0700. A name that is already taken makes
//! the test fail instead of reusing it, so a planted directory or link cannot redirect what a test writes. Tests pull this
//! file in with `#[path = "support/scratch.rs"] mod scratch;` and use the part they need.
#![allow(dead_code)]

use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A directory that exists for the life of the value.
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// A new, empty directory whose name holds `tag`, the process id, a counter and random bytes.
    pub fn new(tag: &str) -> Scratch {
        Scratch::under(&std::env::temp_dir(), &format!("holzwave-server-{tag}"))
    }

    /// A new directory with a short path, for a Unix socket: the path of a socket has a hard limit of about a hundred bytes,
    /// which the long per-user temporary directory of some systems exceeds.
    pub fn short(tag: &str) -> Scratch {
        Scratch::under(Path::new("/tmp"), &format!("hw-{tag}"))
    }

    fn under(base: &Path, stem: &str) -> Scratch {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut random = [0u8; 6];
        getrandom::fill(&mut random).expect("the operating system gives random bytes");
        let random: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let path = base.join(format!("{stem}-{}-{n}-{random}", std::process::id()));
        DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap_or_else(|e| panic!("scratch directory {} must not exist yet: {e}", path.display()));
        Scratch { path }
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A path inside the directory.
    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// Writes `contents` to a new file called `name` with exactly `mode` (the umask does not reduce it) and returns its path.
    pub fn write_file(&self, name: &str, contents: &[u8], mode: u32) -> PathBuf {
        let path = self.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", path.display()));
        file.set_permissions(Permissions::from_mode(mode)).expect("set the mode of the scratch file");
        file.write_all(contents).expect("write the scratch file");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A test that changed the mode of a directory it made must not leave the removal stuck.
        let _ = std::fs::set_permissions(&self.path, Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
