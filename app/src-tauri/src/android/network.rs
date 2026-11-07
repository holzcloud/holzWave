//! The permission to reach the local network, seen from Rust: [`PermissionGate`] is the [`Gate`] that the core's connections
//! ask before they dial a private address (`holzwave_core::reach`), and it asks the Kotlin plugin, which shows the system's own
//! dialog when the permission is not held yet.
//!
//! Android 17 lets an app that targets it reach a device on the local network only with the runtime permission
//! `ACCESS_LOCAL_NETWORK`; without it a connection does not fail, it times out. The question is asked here, when the first
//! such connection is made, and never at the start of the app.
//!
//! The gate has no Android API in it: it asks through a function that answers one word, so the rules (a grant is remembered, a
//! refusal is asked again by the next connection, a dialog that is busy with another question is waited for, an answer that cannot
//! be read is a refusal) are proved on the desktop with a scripted function. On Android the function is the plugin call.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use holzwave_core::reach::{Access, Gate};

/// The command of the Kotlin plugin.
pub const ENSURE: &str = "localNetworkEnsure";

/// How many times a busy answer (another system dialog is open) is waited out, and for how long each time.
const BUSY_TRIES: u32 = 240;
const BUSY_PAUSE: Duration = Duration::from_millis(500);

/// What the plugin said, by its one word. `None` is a call that failed or an answer that was not one of the words.
fn access_of(word: Option<&str>) -> Option<Access> {
    match word? {
        "granted" => Some(Access::Granted),
        "denied" => Some(Access::Denied),
        "background" => Some(Access::Background),
        _ => None,
    }
}

/// The gate. `ask` is one call into the plugin: the word of its answer, or `None` when the call failed.
pub struct PermissionGate<F: Fn() -> Option<String> + Send + Sync + 'static> {
    ask: F,
    pause: Duration,
    /// Set by a grant, for good: an app whose permission is taken away is stopped by the system, so it is never stale.
    granted: AtomicBool,
    /// One question at a time: a second connection waits for the first one's dialog and then finds the answer.
    one_at_a_time: Mutex<()>,
}

impl<F: Fn() -> Option<String> + Send + Sync + 'static> PermissionGate<F> {
    pub fn new(ask: F) -> Self {
        Self::with_pause(ask, BUSY_PAUSE)
    }

    fn with_pause(ask: F, pause: Duration) -> Self {
        PermissionGate { ask, pause, granted: AtomicBool::new(false), one_at_a_time: Mutex::new(()) }
    }
}

impl<F: Fn() -> Option<String> + Send + Sync + 'static> Gate for PermissionGate<F> {
    fn ask(&self) -> Access {
        if self.granted.load(Ordering::Acquire) {
            return Access::Granted;
        }
        let _turn = self.one_at_a_time.lock().unwrap_or_else(PoisonError::into_inner);
        if self.granted.load(Ordering::Acquire) {
            return Access::Granted;
        }
        for _ in 0..BUSY_TRIES {
            let word = (self.ask)();
            if word.as_deref() == Some("busy") {
                std::thread::sleep(self.pause);
                continue;
            }
            return match access_of(word.as_deref()) {
                Some(Access::Granted) => {
                    self.granted.store(true, Ordering::Release);
                    Access::Granted
                }
                Some(other) => other,
                None => Access::Denied,
            };
        }
        Access::Denied
    }
}

#[cfg(target_os = "android")]
pub use run::install;

#[cfg(target_os = "android")]
mod run {
    use std::sync::Arc;

    use serde::Deserialize;

    use super::{ENSURE, PermissionGate};
    use crate::android::plugin;

    #[derive(Deserialize)]
    struct Answer {
        state: String,
    }

    /// Installs the gate for the connections of the core. Called once from the app's `setup`.
    pub fn install() {
        let gate = PermissionGate::new(|| {
            let handle = plugin::handle()?;
            match handle.run_mobile_plugin::<Answer>(ENSURE, ()) {
                Ok(answer) => Some(answer.state),
                Err(e) => {
                    eprintln!("holzwave: the local network permission could not be asked: {e}");
                    None
                }
            }
        });
        if !holzwave_core::reach::install(Arc::new(gate)) {
            eprintln!("holzwave: a gate for the local network was installed already");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::sync::{Arc, Mutex};

    /// A function that answers the words it is given, one for each call (the last one repeats), and counts the calls.
    fn scripted(words: &[Option<&str>]) -> (impl Fn() -> Option<String> + Send + Sync + 'static, Arc<AtomicU32>) {
        let words: Vec<Option<String>> = words.iter().map(|w| w.map(str::to_owned)).collect();
        let calls = Arc::new(AtomicU32::new(0));
        let counter = calls.clone();
        let next = Mutex::new(0usize);
        (
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut at = next.lock().unwrap();
                let word = words[(*at).min(words.len() - 1)].clone();
                *at += 1;
                word
            },
            calls,
        )
    }

    fn gate(words: &[Option<&str>]) -> (PermissionGate<impl Fn() -> Option<String> + Send + Sync + 'static>, Arc<AtomicU32>) {
        let (ask, calls) = scripted(words);
        (PermissionGate::with_pause(ask, Duration::from_millis(1)), calls)
    }

    #[test]
    fn a_grant_is_remembered_and_the_plugin_is_not_asked_again() {
        let (gate, calls) = gate(&[Some("granted")]);
        assert_eq!(gate.ask(), Access::Granted);
        assert_eq!(gate.ask(), Access::Granted);
        assert_eq!(gate.ask(), Access::Granted);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_refusal_is_asked_again_by_the_next_connection() {
        let (gate, calls) = gate(&[Some("denied"), Some("granted")]);
        assert_eq!(gate.ask(), Access::Denied, "the first connection is refused");
        assert_eq!(gate.ask(), Access::Granted, "the person allowed it in the settings meanwhile");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_busy_dialog_is_waited_for() {
        let (gate, calls) = gate(&[Some("busy"), Some("busy"), Some("granted")]);
        assert_eq!(gate.ask(), Access::Granted);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_dialog_that_stays_busy_is_a_refusal_in_the_end() {
        let (gate, calls) = gate(&[Some("busy")]);
        assert_eq!(gate.ask(), Access::Denied);
        assert_eq!(calls.load(Ordering::SeqCst), BUSY_TRIES);
    }

    #[test]
    fn the_background_is_told_apart_and_is_not_remembered() {
        let (gate, _) = gate(&[Some("background"), Some("granted")]);
        assert_eq!(gate.ask(), Access::Background);
        assert_eq!(gate.ask(), Access::Granted);
    }

    #[test]
    fn a_call_that_failed_or_an_unknown_word_is_a_refusal_and_never_a_grant() {
        for word in [None, Some(""), Some("GRANTED"), Some("yes"), Some("granted "), Some("true")] {
            let (gate, _) = gate(&[word]);
            assert_eq!(gate.ask(), Access::Denied, "{word:?}");
        }
    }

    #[test]
    fn two_connections_make_one_question() {
        let (ask, calls) = scripted(&[Some("granted")]);
        let gate = Arc::new(PermissionGate::with_pause(ask, Duration::from_millis(1)));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let gate = gate.clone();
                std::thread::spawn(move || gate.ask())
            })
            .collect();
        for thread in threads {
            assert_eq!(thread.join().unwrap(), Access::Granted);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the second to eighth connection found the answer");
    }
}
