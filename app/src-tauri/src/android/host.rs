//! [`AndroidHost`]: the platform answers of the Android app.
//!
//! - The platform is Android, so the page takes the Linux key table and the phone and tablet layouts.
//! - The private folders are two folders inside the app's own storage: `home` (the shell's home and start directory) in
//!   the data directory, and the app's cache directory (the shell's `TMPDIR`). `home` is a folder of its own beside the
//!   app's settings, layout, sync and vault files, never the data directory itself, so that a program a person starts in
//!   a pane does not find those files next to its own and a hand-over of files can be limited to it. Both folders are made
//!   before they are named, `home` with mode 0700.
//! - The secret store is a [`LockedStore`]: until the vault of the platform keystore is wired in, this build keeps no
//!   secret at all.
//! - Everything else is what a platform without the feature answers: no permission source for the local network, a text
//!   size of 100, no limits, and the address of the file scheme from `files_protocol` (on Android the `http://holzfile.localhost/`
//!   form).

use std::io;
use std::path::Path;
use std::sync::Arc;

use holzwave_core::secrets::SecretStore;
use holzwave_core::vault::LOCKED_TEXT;
use holzwave_core::{Error, Result};
use holzwave_host::local_network::{Source, Unsupported};
use holzwave_host::seams::{Host, HostLimits, Platform, PrivateDirs};

/// The name of the shell's own folder inside the app's data directory.
pub const HOME_FOLDER: &str = "home";

/// A secret store that is locked for good: every call answers the locked error and nothing is kept.
///
/// This is the honest store of the first Android build. The platform keystore and the vault that wraps its key are not
/// part of it yet, so no password, passphrase or key can be saved, and none is held in memory or in a file. The page
/// shows what it shows for a locked store: `secrets are locked`. A later build gives the host the vault in its place.
#[derive(Default)]
pub struct LockedStore;

/// The error of a locked store, in the shape the host's `is_locked` matches: an I/O error whose text is exactly the
/// locked sentence.
fn locked() -> Error {
    Error::Io(io::Error::other(LOCKED_TEXT))
}

impl SecretStore for LockedStore {
    fn get(&self, _account: &str) -> Result<Option<String>> {
        Err(locked())
    }

    fn set(&self, _account: &str, _secret: &str) -> Result<()> {
        Err(locked())
    }

    fn delete(&self, _account: &str) -> Result<()> {
        Err(locked())
    }
}

/// The host of the Android app.
pub struct AndroidHost {
    dirs: PrivateDirs,
    secrets: Arc<dyn SecretStore>,
}

impl AndroidHost {
    /// The host for an app whose data directory is `data_dir` and whose cache directory is `cache_dir`. Both folders it
    /// names exist when this returns: `data_dir/home` (mode 0700) and `cache_dir`. Fails when one cannot be made, and
    /// the app does not start without them.
    pub fn new(data_dir: &Path, cache_dir: &Path) -> io::Result<AndroidHost> {
        for dir in [data_dir, cache_dir] {
            if !dir.is_absolute() {
                return Err(io::Error::other(format!("the private folder {} is not an absolute path", dir.display())));
            }
        }
        let home = data_dir.join(HOME_FOLDER);
        make_private_dir(&home)?;
        std::fs::create_dir_all(cache_dir)?;
        Ok(AndroidHost { dirs: PrivateDirs { home, cache: cache_dir.to_owned() }, secrets: Arc::new(LockedStore) })
    }
}

/// Make `dir` and its missing parents, and set the mode of `dir` itself to 0700 (owner only) on Unix.
fn make_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

impl Host for AndroidHost {
    fn platform(&self) -> Platform {
        Platform::Android
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        self.secrets.clone()
    }

    fn local_network(&self) -> Arc<dyn Source> {
        crate::local_network::system().unwrap_or_else(|| Arc::new(Unsupported))
    }

    fn text_scale(&self) -> u32 {
        100
    }

    fn limits(&self) -> HostLimits {
        HostLimits::default()
    }

    /// The address of the app's own file scheme, which Android serves as `http://<scheme>.localhost/` like Windows.
    fn file_url(&self, token: &str) -> String {
        crate::files_protocol::url_of(token)
    }

    fn private_dirs(&self) -> Option<PrivateDirs> {
        Some(self.dirs.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holzwave_host::locked::is_locked;

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("holzwave-android-host-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_host_is_android_and_names_the_folders_it_made() {
        let scratch = Scratch::new("folders");
        let data = scratch.0.join("files");
        let cache = scratch.0.join("cache");
        let host = AndroidHost::new(&data, &cache).expect("a host");
        assert_eq!(host.platform(), Platform::Android);
        let dirs = host.private_dirs().expect("the folders");
        assert_eq!(dirs.home, data.join("home"));
        assert_eq!(dirs.cache, cache);
        assert!(dirs.home.is_dir() && dirs.cache.is_dir(), "both exist before they are named");
        assert_ne!(dirs.home, data, "the shell's home is not the folder of the app's own files");
        assert!(dirs.home.starts_with(&data) && dirs.home != data);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dirs.home).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "the home folder is the owner's alone");
        }
        // Starting again over the same folders changes nothing and fails nowhere.
        let again = AndroidHost::new(&data, &cache).expect("a second host");
        assert_eq!(again.private_dirs(), host.private_dirs());
    }

    #[test]
    fn a_relative_folder_is_refused() {
        let scratch = Scratch::new("relative");
        assert!(AndroidHost::new(Path::new("files"), &scratch.0.join("cache")).is_err());
        assert!(AndroidHost::new(&scratch.0.join("files"), Path::new("cache")).is_err());
        assert!(!Path::new("files").exists() && !Path::new("cache").exists(), "nothing was made for a bad path");
    }

    #[test]
    fn the_store_answers_locked_to_every_call_and_keeps_nothing() {
        let store = LockedStore;
        for result in [store.get("ssh:a").map(|_| ()), store.set("ssh:a", "canary-secret"), store.delete("ssh:a")] {
            let error = result.expect_err("a locked store refuses");
            assert!(is_locked(&error), "{error}");
            assert_eq!(error.to_string(), "secrets are locked");
        }
        // The secret was not kept: a later read is still the locked error, never the value.
        assert!(is_locked(&store.get("ssh:a").expect_err("still locked")));
    }

    #[test]
    fn the_host_hands_out_one_locked_store_and_the_other_answers_of_a_platform_without_the_feature() {
        let scratch = Scratch::new("answers");
        let host = AndroidHost::new(&scratch.0.join("files"), &scratch.0.join("cache")).expect("a host");
        assert!(Arc::ptr_eq(&host.secrets(), &host.secrets()));
        assert!(is_locked(&host.secrets().set("a", "b").expect_err("locked")));
        assert_eq!(host.text_scale(), 100);
        assert_eq!(host.limits(), HostLimits::default());
        assert!(host.local_processes());
        assert_eq!(host.file_url("abc"), crate::files_protocol::url_of("abc"), "the one definition of the address");
        if cfg!(target_os = "android") {
            assert_eq!(host.file_url("abc"), "http://holzfile.localhost/abc");
        }
    }
}
