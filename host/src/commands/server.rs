//! The plain functions of the commands for the link to a holzWave server: the state of the link, the sign-in with its wait,
//! Sign out, the choice of the server as the transport of the sync, and the sessions of the server run as panes. Each one
//! calls the host and does nothing else; the rules (the address, the one server per device, the token's custody, what a
//! pane of a session is) are in [`crate::server`], [`crate::sync`] and [`crate::sessions`].
//!
//! No answer holds the token: the views have no member for it, and every failure is a fixed sentence. The commands wait for the
//! network, so each is async. The browser build (the server's own page) has no link and no sign-in, so it does not have the
//! commands of the link; it has the six commands of the sessions, which its worker answers from its own sessions.

use holzwave_core::layout::PaneId;

use crate::server::{ServerView, SignInPoll, SignInView};
use crate::sessions::SessionRow;
use crate::state::{AppCore, View};
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

/// The sessions of the person on the server, oldest first. An error is a fixed sentence; nothing the server wrote is one.
pub fn server_sessions(core: &AppCore) -> Result<Vec<SessionRow>, String> {
    core.server_sessions()
}

/// Make a session on the server and show it in a new tab of `window`: the view with that tab shown. Nothing is made when the
/// person has no room for another pane.
pub fn server_session_new(core: &AppCore, window: &str) -> Result<View, String> {
    core.server_session_new_in(window)
}

/// Show the session `session` of the server in a new tab of `window`: the view with that tab shown. A number that is not one
/// of the person's sessions is the one `not found` sentence, and no tab is made.
pub fn server_session_open(core: &AppCore, window: &str, session: u32) -> Result<View, String> {
    core.server_session_open_in(window, session)
}

/// End the program of a session on the server. The panes that show it say so and stay until they are closed.
pub fn server_session_end(core: &AppCore, session: u32) -> Result<(), String> {
    core.server_session_end(session)
}

/// Take the right to write in the session `pane` shows; the pane's line in `pane_connections` says so once the server has.
pub fn pane_takeover(core: &AppCore, pane: PaneId) -> Result<(), String> {
    core.pane_takeover(pane)
}

/// Ask the server for older lines of the session `pane` shows.
pub fn pane_load_older(core: &AppCore, pane: PaneId) -> Result<(), String> {
    core.pane_load_older(pane)
}
