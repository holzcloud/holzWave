//! The plain functions of the commands for saved connections, the questions of a login and the installation of `wsh`.
//! Secrets go to the store the host gives the core and nowhere else: no answer holds one, and a locked store answers with
//! the store's own error.

use holzwave_core::connections::{ConnView, ProfileView, SecretChange};
use holzwave_core::layout::PaneId;
use holzwave_core::profiles::Profile;

use crate::state::{AppCore, View};

/// The saved connections (SSH, WSL, Docker), each with whether a secret is saved for it. Asks the secret store.
pub fn connections_list(core: &AppCore) -> Vec<ProfileView> {
    core.connections_list()
}

/// Add or change a connection. `secret` says what to do with its password or key passphrase: `{kind: "keep"}`,
/// `{kind: "set", value}` or `{kind: "clear"}`. The answer is the connection as saved, or why it was refused.
pub fn connection_save(core: &AppCore, profile: Profile, secret: SecretChange) -> Result<ProfileView, String> {
    core.connection_save(profile, secret)
}

/// Remove a connection and its secret.
pub fn connection_delete(core: &AppCore, id: String) -> Result<(), String> {
    core.connection_delete(&id)
}

/// Open connection `id` in a new tab of the window `window`; the answer is the view with that tab shown.
pub fn connect(core: &AppCore, window: &str, id: String) -> Result<View, String> {
    core.connect_in(window, &id)
}

/// The connection state of every running pane that has a connection. Reads memory only.
pub fn pane_connections(core: &AppCore) -> Vec<ConnView> {
    core.pane_connections()
}

/// The person answered the question of a login (a one-time code, a PAM prompt): one answer per question, in order. True
/// when the question was waiting; an unknown, old or malformed id changes nothing. Nothing of the answers is kept or logged.
pub fn login_prompt_answer(core: &AppCore, id: String, answers: Vec<String>) -> bool {
    core.login_prompt_answer(&id, answers)
}

/// The person gave up on the question of a login. True when it was waiting.
pub fn login_prompt_cancel(core: &AppCore, id: String) -> bool {
    core.login_prompt_cancel(&id)
}

/// Put the bundled `wsh` on the host of the saved SSH connection the focused pane runs, verify it there and say the result
/// as a line on the pane. The page names the pane and never a host; the core resolves the connection, and nothing is
/// installed without this call. The answer is the sentence the pane shows.
pub fn wsh_install(core: &AppCore, pane: PaneId) -> Result<String, String> {
    core.wsh_install(pane)
}
