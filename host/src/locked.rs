//! The sentence of a locked secret store, and how a refusal that came from it is told apart from every other store error.
//!
//! The core turns any failure of the secret store into one fixed phrase of its own (`the system keychain refused the
//! request`), because the store's message may be about the machine. A store that is locked (the server's encrypted store
//! before the person unlocked it) is not a failure of the machine, and the page must say so: it shows the sentence
//! `secrets are locked` and offers the unlock. The host cannot read the cause out of the core's answer, so it wraps the
//! store it gives to the core in [`Watched`]: when a call to the store fails with the locked error, the wrapper notes it
//! for the thread that made the call, and the command that made the call reads the note with [`watching`]. The wrapper
//! passes every call and every answer through unchanged, keeps no copy of an account or a secret, and has no output.

use std::cell::Cell;
use std::sync::Arc;

use holzwave_core::secrets::SecretStore;
use holzwave_core::{Error, Result};

/// What a locked store says, and what the page shows for it. The text of the server's store error is exactly this.
pub const SECRETS_LOCKED: &str = "secrets are locked";

thread_local! {
    /// Whether a call to a watched store on this thread failed because the store is locked, since the last [`watching`].
    static SAW_LOCKED: Cell<bool> = const { Cell::new(false) };
}

/// Whether `error` is the error of a locked store: an I/O error whose text is exactly [`SECRETS_LOCKED`].
pub fn is_locked(error: &Error) -> bool {
    matches!(error, Error::Io(_)) && error.to_string() == SECRETS_LOCKED
}

/// A secret store that notes, for the calling thread, that it was locked. See the module text.
pub(crate) struct Watched {
    inner: Arc<dyn SecretStore>,
}

impl Watched {
    /// `store`, watched.
    pub(crate) fn wrap(store: Arc<dyn SecretStore>) -> Arc<dyn SecretStore> {
        Arc::new(Watched { inner: store })
    }
}

/// Notes a locked refusal in `result` and passes the result on.
fn note<T>(result: Result<T>) -> Result<T> {
    if let Err(error) = &result
        && is_locked(error)
    {
        SAW_LOCKED.with(|saw| saw.set(true));
    }
    result
}

impl SecretStore for Watched {
    fn get(&self, account: &str) -> Result<Option<String>> {
        note(self.inner.get(account))
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        note(self.inner.set(account, secret))
    }

    fn delete(&self, account: &str) -> Result<()> {
        note(self.inner.delete(account))
    }
}

/// Run `work` and say whether a watched store refused a call of this thread because it is locked while it ran. The work
/// must make its calls on this thread, which the core's key and secret operations do.
pub(crate) fn watching<T>(work: impl FnOnce() -> T) -> (T, bool) {
    SAW_LOCKED.with(|saw| saw.set(false));
    let result = work();
    (result, SAW_LOCKED.with(|saw| saw.replace(false)))
}

#[cfg(test)]
mod tests {
    use super::*;

    use holzwave_core::secrets::MemoryStore;

    /// A store that is locked or not, and says so the way the server's store does.
    struct Switch {
        inner: MemoryStore,
        locked: std::sync::atomic::AtomicBool,
    }

    #[derive(Debug)]
    struct Locked;

    impl std::fmt::Display for Locked {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(SECRETS_LOCKED)
        }
    }

    impl std::error::Error for Locked {}

    impl Switch {
        fn check(&self) -> Result<()> {
            if self.locked.load(std::sync::atomic::Ordering::SeqCst) {
                Err(Error::Io(std::io::Error::other(Locked)))
            } else {
                Ok(())
            }
        }
    }

    impl SecretStore for Switch {
        fn get(&self, account: &str) -> Result<Option<String>> {
            self.check()?;
            self.inner.get(account)
        }

        fn set(&self, account: &str, secret: &str) -> Result<()> {
            self.check()?;
            self.inner.set(account, secret)
        }

        fn delete(&self, account: &str) -> Result<()> {
            self.check()?;
            self.inner.delete(account)
        }
    }

    fn switch(locked: bool) -> Arc<Switch> {
        Arc::new(Switch { inner: MemoryStore::default(), locked: std::sync::atomic::AtomicBool::new(locked) })
    }

    #[test]
    fn only_the_locked_error_is_told_apart() {
        assert!(is_locked(&Error::Io(std::io::Error::other(Locked))));
        assert!(!is_locked(&Error::Io(std::io::Error::other("keychain: no such device"))));
        assert!(!is_locked(&Error::Unsupported(SECRETS_LOCKED.to_owned())), "the text of another kind of error is no lock");
        assert!(!is_locked(&Error::Pty("other".to_owned())));
    }

    #[test]
    fn a_watched_store_passes_everything_through_and_notes_a_lock_for_the_calling_thread_only() {
        let store = switch(false);
        let watched = Arc::new(Watched { inner: store.clone() });

        let (answer, saw) = watching(|| {
            watched.set("ai:a", "one").expect("set");
            watched.get("ai:a").expect("get")
        });
        assert_eq!((answer.as_deref(), saw), (Some("one"), false));

        store.locked.store(true, std::sync::atomic::Ordering::SeqCst);
        let (answer, saw) = watching(|| watched.get("ai:a"));
        assert!(answer.is_err_and(|error| is_locked(&error)), "the error comes through as it was");
        assert!(saw);
        // The note is read once and is not carried into the next piece of work.
        assert!(!watching(|| ()).1);
        // Another thread's lock is not this thread's.
        let other = watched.clone();
        let (_, saw) = watching(|| std::thread::spawn(move || drop(other.get("ai:a"))).join().expect("joined"));
        assert!(!saw, "a call on another thread notes nothing here");
        // A different failure is not a lock.
        store.locked.store(false, std::sync::atomic::Ordering::SeqCst);
        let (answer, saw) = watching(|| watched.set("", "x"));
        assert!(answer.is_err());
        assert!(!saw);
    }
}
