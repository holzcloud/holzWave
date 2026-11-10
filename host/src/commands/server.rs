//! The plain functions of the commands for the link to a holzWave server: the state of the link, the sign-in with its wait,
//! Sign out, and the choice of the server as the transport of the sync. Each one calls the host and does nothing else; the
//! rules (the address, the one server per device, the token's custody) are in [`crate::server`] and [`crate::sync`].
//!
//! No answer holds the token: the views have no member for it, and every failure is a fixed sentence. The commands wait for the
//! network, so each is async, and the browser build does not have them (it is the server's own page).

use crate::server::{ServerView, SignInPoll, SignInView};
use crate::state::AppCore;
use crate::sync::SyncView;

/// The link to the server as the page reads it: the address, the state, the two switches of the server as the last answer said.
/// No request is made.
pub fn server_status(core: &AppCore) -> ServerView {
    core.server_status()
}

/// Ask the server who this device is (one request) and answer the link as it reads afterwards.
pub fn server_refresh(core: &AppCore) -> ServerView {
    core.server_refresh()
}

/// Start a sign-in at `address`: the code to show and the address to open in the system browser.
pub fn server_sign_in_start(core: &AppCore, address: String) -> Result<SignInView, String> {
    core.server_sign_in_start(&address)
}

/// One poll of the sign-in in progress: the state word and the seconds to wait.
pub fn server_sign_in_poll(core: &AppCore) -> Result<SignInPoll, String> {
    core.server_sign_in_poll()
}

/// Give up the sign-in in progress.
pub fn server_sign_in_cancel(core: &AppCore) -> ServerView {
    core.server_sign_in_cancel()
}

/// Sign this device out of the server: the token goes, the address stays.
pub fn server_sign_out(core: &AppCore) -> Result<ServerView, String> {
    core.server_sign_out()
}

/// Use the server this device is signed in to as the transport of the sync, with the person's passphrase, and run it once.
pub fn sync_use_server(core: &AppCore, passphrase: String) -> Result<SyncView, String> {
    core.sync_use_server(&passphrase)
}
