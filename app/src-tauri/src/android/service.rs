//! The foreground service that keeps shells and connections alive while the app is in the background, as the Rust side
//! drives it: the counts of the live sessions go to the Kotlin service when they change, and the service's one action,
//! `Stop all`, comes back and ends every session.
//!
//! The rule of what is told when ([`Changes`]) has no Android API in it and is tested on the desktop. The thread that applies
//! it and the listener of `Stop all` exist on Android only. Nothing here holds a secret: the only things that cross are two
//! numbers one way and an event without a payload the other way.
//!
//! The route of `Stop all` is the plugin's own event API and needs no JNI code of ours: the service triggers the event
//! `stopAll` on the Kotlin plugin, the plugin sends it down the channel that this module registered through the plugin's
//! `registerListener` command, and the channel's closure ends the sessions on a thread of its own.

/// What the service was last told, so that it is told again only when the counts differ. A service that was never started
/// has been told `(0, 0)`, so a core with no session tells it nothing.
#[derive(Debug, Default)]
pub struct Changes {
    told: (usize, usize),
}

impl Changes {
    /// The counts to tell the service now: `Some` when they differ from the ones it was last told, `None` when it is up to date.
    pub fn pending(&self, now: (usize, usize)) -> Option<(usize, usize)> {
        (now != self.told).then_some(now)
    }

    /// The service has taken `counts`. Not called when the call failed, so the next pass tries again.
    pub fn delivered(&mut self, counts: (usize, usize)) {
        self.told = counts;
    }
}

#[cfg(target_os = "android")]
pub use run::start;

#[cfg(target_os = "android")]
mod run {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use holzwave_host::state::AppCore;
    use serde_json::json;
    use tauri::ipc::Channel;

    use super::Changes;
    use crate::android::plugin;

    /// How often the live sessions are counted. Two seconds is quick enough for a notification and costs a pass over a small map.
    const POLL: std::time::Duration = std::time::Duration::from_secs(2);

    /// The command of the Kotlin plugin that takes the counts.
    const UPDATE: &str = "serviceUpdate";
    /// The event the service triggers on the plugin for `Stop all`.
    const STOP_ALL_EVENT: &str = "stopAll";

    /// Raised while `stop_all` runs, so that the poller does not tell the service the counts of a half-stopped app (which
    /// would show the notification again for an instant).
    static STOPPING: AtomicBool = AtomicBool::new(false);

    /// The channel for the event `stopAll`. Its closure runs on the thread that calls into Rust from Kotlin, so it only starts
    /// the work on a thread of its own.
    fn stop_all_channel(core: Arc<AppCore>) -> Channel<serde_json::Value> {
        Channel::new(move |_message| {
            let core = core.clone();
            // Raised here, not on the new thread, so no pass of the poller falls between the event and the stop.
            STOPPING.store(true, Ordering::SeqCst);
            let spawned = std::thread::Builder::new().name("hw-stop-all".into()).spawn(move || {
                core.stop_all();
                STOPPING.store(false, Ordering::SeqCst);
            });
            if let Err(e) = spawned {
                STOPPING.store(false, Ordering::SeqCst);
                eprintln!("holzwave: Stop all could not start its thread: {e}");
            }
            Ok(())
        })
    }

    /// Start the poller. Called once from the app's `setup`, after the core exists; it returns at once. The thread registers
    /// the listener for `Stop all` (retrying each pass until the Kotlin plugin answers) and then tells the service the counts
    /// whenever they change. Every call into Kotlin is made here, on this thread, never on the thread of the window system.
    pub fn start(core: Arc<AppCore>) {
        let spawned = std::thread::Builder::new().name("hw-service".into()).spawn(move || {
            let mut changes = Changes::default();
            let mut listening = false;
            loop {
                std::thread::sleep(POLL);
                let Some(handle) = plugin::handle() else { continue };
                if !listening {
                    let listener = json!({ "event": STOP_ALL_EVENT, "handler": stop_all_channel(core.clone()) });
                    match handle.run_mobile_plugin::<()>("registerListener", listener) {
                        Ok(()) => listening = true,
                        Err(e) => {
                            eprintln!("holzwave: the Stop all listener was not registered: {e}");
                            continue;
                        }
                    }
                }
                if STOPPING.load(Ordering::SeqCst) {
                    continue;
                }
                let now = core.session_counts();
                let Some((shells, connections)) = changes.pending(now) else { continue };
                match handle.run_mobile_plugin::<()>(UPDATE, json!({ "shells": shells, "connections": connections })) {
                    Ok(()) => changes.delivered((shells, connections)),
                    Err(e) => eprintln!("holzwave: the service was not updated: {e}"),
                }
            }
        });
        if let Err(e) = spawned {
            eprintln!("holzwave: the session service could not start its thread: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_that_was_never_started_is_told_nothing_for_no_sessions() {
        let changes = Changes::default();
        assert_eq!(changes.pending((0, 0)), None);
    }

    #[test]
    fn counts_are_told_when_they_change_and_not_again_while_they_stay() {
        let mut changes = Changes::default();
        assert_eq!(changes.pending((1, 0)), Some((1, 0)));
        changes.delivered((1, 0));
        assert_eq!(changes.pending((1, 0)), None, "the same counts are not told twice");
        assert_eq!(changes.pending((2, 0)), Some((2, 0)));
        assert_eq!(changes.pending((1, 1)), Some((1, 1)), "a shell turning into a connection is a change");
        changes.delivered((1, 1));
        assert_eq!(changes.pending((0, 0)), Some((0, 0)), "the end of the last session is told, so the service can stop");
        changes.delivered((0, 0));
        assert_eq!(changes.pending((0, 0)), None);
    }

    #[test]
    fn a_call_that_failed_is_tried_again_on_the_next_pass() {
        let changes = Changes::default();
        let first = changes.pending((3, 1));
        assert_eq!(first, Some((3, 1)));
        // The call failed: `delivered` was not called, so the counts are still pending.
        assert_eq!(changes.pending((3, 1)), Some((3, 1)));
    }
}
