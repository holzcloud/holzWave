//! Whether the certificate verifier of the platform is ready to judge a chain.
//!
//! On Android the verifier that both network clients use (`rustls-platform-verifier`) has to be handed the Android context
//! once before the first request; without it every HTTPS request would fail, and a verifier that was never set up must never
//! be replaced by one that accepts. The app sets the flag in [`global`] when that hand-over worked. The two places that start
//! an HTTPS request on Android (a sync run and the send of an AI chat) ask [`require`] first and fail with the fixed word
//! [`NOT_READY`] when it was not, so a missing set-up is a visible failure and never a hang, a crash or an acceptance.
//!
//! The type is compiled on every platform and tested on the desktop. The two hooks that ask it exist in Android builds only
//! (a `cfg(target_os = "android")` line each, in `sync.rs` and `ai_host.rs`); on every other platform the flag is never
//! asked and the clients behave as they always did.

use std::sync::atomic::{AtomicBool, Ordering};

/// The fixed word of a request that was refused because the verifier is not set up. It is the whole failure: no text of the
/// system and no address goes with it.
pub const NOT_READY: &str = "tls_not_ready";

/// A flag that is false until it is set, and set once.
#[derive(Debug, Default)]
pub struct TlsReady(AtomicBool);

impl TlsReady {
    /// A flag that is not set.
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// The verifier was set up. Setting it again changes nothing.
    pub fn set(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether the verifier was set up.
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// `Ok` once the flag is set, the fixed word [`NOT_READY`] before.
    pub fn require(&self) -> Result<(), &'static str> {
        if self.is_set() { Ok(()) } else { Err(NOT_READY) }
    }
}

static GLOBAL: TlsReady = TlsReady::new();

/// The flag of this process.
pub fn global() -> &'static TlsReady {
    &GLOBAL
}

/// [`TlsReady::require`] on the flag of this process: what the hooks of an Android build call.
pub fn require() -> Result<(), &'static str> {
    GLOBAL.require()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_is_false_until_it_is_set_and_stays_set() {
        let flag = TlsReady::new();
        assert!(!flag.is_set());
        flag.set();
        assert!(flag.is_set());
        flag.set();
        assert!(flag.is_set(), "setting twice changes nothing");
    }

    #[test]
    fn require_gives_the_fixed_word_before_the_flag_is_set_and_ok_after() {
        let flag = TlsReady::new();
        assert_eq!(flag.require(), Err("tls_not_ready"));
        assert_eq!(NOT_READY, "tls_not_ready");
        flag.set();
        assert_eq!(flag.require(), Ok(()));
    }
}
