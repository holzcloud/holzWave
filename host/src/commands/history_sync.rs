//! The plain functions of the commands for the history of typed commands, the sync of the settings and the Local Network
//! permission of macOS. The sync takes the password and the passphrase as arguments, stores them in the host's secret
//! store and never writes them into a file of the data directory, an answer or an error.

use holzwave_core::history::Hit;

use crate::local_network::LocalNetworkView;
use crate::state::AppCore;
use crate::sync::SyncView;

/// Typed commands that match `query` (fuzzy), newest and best first; an empty query lists the newest. The search is the
/// history of the host the focused pane of the window `window` runs on: the page sends no host.
pub fn history_search(core: &AppCore, window: &str, query: String, limit: usize) -> Vec<Hit> {
    core.history_search_in(window, &query, limit)
}

/// Forget every use of one command; the answer is how many entries went.
pub fn history_delete(core: &AppCore, cmd: String) -> Result<usize, String> {
    core.history_delete(&cmd)
}

/// Forget the whole history.
pub fn history_clear(core: &AppCore) -> Result<(), String> {
    core.history_clear()
}

/// The state of the sync.
pub fn sync_status(core: &AppCore) -> SyncView {
    core.sync_status()
}

/// Set up the sync for an account (the first run makes the folder on a new server) or why it was refused.
pub fn sync_save(core: &AppCore, endpoint: String, user: String, password: String, passphrase: String) -> Result<SyncView, String> {
    core.sync_save(&endpoint, &user, &password, &passphrase)
}

/// Run the sync now.
pub fn sync_now(core: &AppCore) -> Result<SyncView, String> {
    core.sync_now()
}

/// Turn the sync off and forget the account.
pub fn sync_off(core: &AppCore) -> SyncView {
    core.sync_off()
}

/// The Local Network permission of macOS as the page needs it: `{supported, notice_due, state}`. It reads the permission of
/// the computer that runs the core, so the browser build does not have it.
pub fn local_network_status(core: &AppCore) -> LocalNetworkView {
    core.local_network().view()
}

/// The person answered the notice of the Local Network permission. `allow` true stores the choice and makes the app touch
/// the network once, which makes macOS show its own question; false stores `Not now`. The browser build does not have it.
pub fn local_network_answer(core: &AppCore, allow: bool) -> Result<LocalNetworkView, String> {
    core.local_network().answer(allow)
}

/// Open System Settings at Privacy & Security, Local Network (macOS only). Takes nothing from the page. The browser build
/// does not have it.
pub fn local_network_open_settings(core: &AppCore) -> Result<(), String> {
    core.open_local_network_settings()
}
