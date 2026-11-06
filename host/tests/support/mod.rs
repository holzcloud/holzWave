//! What the tests of the vault commands and of the locked answers share: a core whose host has a vault over the fake
//! keystore and the fake clock, a vault file that takes its time, and a sink that says nothing.
//!
//! Nothing here needs Android. The vault is the real `VaultStore` of the core, in a file of a scratch folder.

#![allow(dead_code)]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use holzwave_core::secrets::SecretStore;
use holzwave_core::vault::{Clock, DiskFiles, FakeClock, FakeKeystore, Keystore, VaultFiles, VaultStore};
use holzwave_host::seams::{Host, HostLimits, NeutralHost, Platform};
use holzwave_host::state::{AppCore, Out, ShellSpec, Sink, Target};
use holzwave_host::windows::{CursorSpot, Place};

/// A sink that drops everything.
pub struct Silent;

impl Sink for Silent {
    fn send(&self, _target: Target, _out: Out) {}

    fn close_window(&self, _label: &str) {}

    fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
        Ok(())
    }

    fn cursor_spot(&self, _source: &str) -> CursorSpot {
        CursorSpot::Unknown
    }
}

/// The file of the vault on disk, which waits `delay` milliseconds before it writes. A write of the vault is made while
/// an operation that may prompt holds the vault's operation lock and not its state lock, so a wait here stands for a held
/// prompt: the state must still answer meanwhile.
pub struct SlowFiles {
    pub delay: AtomicU64,
    /// Set when a write has begun, so a test can wait until the operation is in its wait.
    pub entered: AtomicBool,
    /// Whether every write fails, with a text that names a path (which an answer must never repeat).
    pub fail: AtomicBool,
}

/// The text a failed write carries; no answer of a command may hold it.
pub const WRITE_DETAIL: &str = "no space left on /data/user/0/ch.holzcloud.holzwave/files";

impl VaultFiles for SlowFiles {
    fn read(&self, path: &Path, max: u64) -> io::Result<Option<Vec<u8>>> {
        DiskFiles.read(path, max)
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.entered.store(true, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(io::Error::other(WRITE_DETAIL));
        }
        let ms = self.delay.load(Ordering::SeqCst);
        if ms > 0 {
            std::thread::sleep(Duration::from_millis(ms));
        }
        DiskFiles.write(path, bytes)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        DiskFiles.remove(path)
    }
}

/// A host that is the neutral one in everything but its secret store, which is the vault.
pub struct VaultHost {
    pub base: NeutralHost,
    pub vault: Arc<VaultStore>,
}

impl Host for VaultHost {
    fn platform(&self) -> Platform {
        self.base.platform()
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        self.vault.clone()
    }

    fn local_network(&self) -> Arc<dyn holzwave_host::local_network::Source> {
        self.base.local_network()
    }

    fn text_scale(&self) -> u32 {
        self.base.text_scale()
    }

    fn limits(&self) -> HostLimits {
        self.base.limits()
    }

    fn vault(&self) -> Option<Arc<VaultStore>> {
        Some(self.vault.clone())
    }
}

/// A core over a vault, with the handles a test needs to move the fakes.
pub struct Fixture {
    pub dir: PathBuf,
    pub core: Arc<AppCore>,
    pub vault: Arc<VaultStore>,
    pub keystore: Arc<FakeKeystore>,
    pub files: Arc<SlowFiles>,
    pub clock: Arc<FakeClock>,
}

impl Fixture {
    /// A core in a scratch folder of its own whose host has a new vault on a device with a screen lock.
    pub fn new(tag: &str) -> Fixture {
        Fixture::on(tag, FakeKeystore::new())
    }

    /// The same on `keystore`.
    pub fn on(tag: &str, keystore: FakeKeystore) -> Fixture {
        Fixture::prepared(tag, keystore, |_| ())
    }

    /// The same, with `setup` run on the scratch folder before the core reads it (an account of the sync, say).
    pub fn prepared(tag: &str, keystore: FakeKeystore, setup: impl FnOnce(&Path)) -> Fixture {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("holzwave-vault-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).expect("scratch dir");
        setup(&dir);
        let keystore = Arc::new(keystore);
        let files = Arc::new(SlowFiles { delay: AtomicU64::new(0), entered: AtomicBool::new(false), fail: AtomicBool::new(false) });
        let clock = Arc::new(FakeClock::new(1_000_000));
        let vault = Arc::new(VaultStore::open_with_files(
            dir.join("secrets.vault"),
            keystore.clone() as Arc<dyn Keystore>,
            clock.clone() as Arc<dyn Clock>,
            files.clone() as Arc<dyn VaultFiles>,
        ));
        let host: Arc<dyn Host> = Arc::new(VaultHost { base: NeutralHost::new(), vault: vault.clone() });
        let core = AppCore::new_with_host(dir.clone(), Arc::new(Silent), ShellSpec::new("/bin/cat", &[]), None, None, host);
        Fixture { dir, core, vault, keystore, files, clock }
    }

    /// A core over a host with no vault, in a scratch folder of its own.
    pub fn without_vault(tag: &str) -> (PathBuf, Arc<AppCore>) {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("holzwave-novault-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).expect("scratch dir");
        let core = AppCore::new(dir.clone(), Arc::new(Silent), ShellSpec::new("/bin/cat", &[]));
        (dir, core)
    }
}
