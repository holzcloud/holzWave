//! The plain functions of the commands for the history of typed commands, the sync of the settings and the Local Network
//! permission of macOS. The sync takes the password and the passphrase as arguments, stores them in the host's secret
//! store and never writes them into a file of the data directory, an answer or an error.

use holzwave_core::history::Hit;

use crate::host_shell::HostShellView;
use crate::window_frame::WindowFrameView;
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

/// The group `Host shell` of the Settings as the page needs it: `{applies, state}`, where `state` is `off`, `on` or
/// `unavailable` and `applies` is false outside a Flatpak (the page then draws nothing). It reads the Flatpak the core runs
/// in, so the browser build does not have it.
pub fn host_shell_status(core: &AppCore) -> HostShellView {
    core.host_shell().view()
}

/// The person pressed the switch `Host shell`. Takes one boolean and nothing else. Refused with the reason outside a Flatpak
/// and in a Flatpak without the permission to run commands on the host; otherwise the choice is stored on this computer
/// and the answer is the new view. Only panes started afterwards follow it. The browser build does not have it.
pub fn host_shell_set(core: &AppCore, enabled: bool) -> Result<HostShellView, String> {
    core.host_shell().set(enabled)
}

/// The group `Window frame` of the Settings as the page needs it: `{applies, frame, buttons, pending}`. `applies` is false
/// outside Windows and Linux (the page then draws nothing); the words are the stored choice and `pending` says that the
/// choice differs from the one this window was built with. The browser build does not have it.
pub fn window_frame_status(core: &AppCore) -> WindowFrameView {
    core.window_frame().view()
}

/// The person chose the window frame and the style of the window buttons. Takes two words and nothing else; each word is
/// checked against its table in the host before anything is written, and a word outside it is refused without being
/// echoed. Windows that are open keep their frame; the next start reads the choice. The browser build does not have it.
pub fn window_frame_set(core: &AppCore, frame: String, buttons: String) -> Result<WindowFrameView, String> {
    core.window_frame().set(&frame, &buttons)
}
