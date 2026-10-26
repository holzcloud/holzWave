//! `wsh` on a host that is reached over SSH: the tunnel of one pane and what is answered through it.
//!
//! A pane that runs a saved SSH connection with `wsh` turned on gets a [`Tunnel`] (see [`tunnel_for`]): a Unix socket path
//! to forward back over the connection's own login, a token that every request must carry, and the three variables the
//! remote shell is started with. What arrives through the forwarded socket is answered by [`WshSink`], one forwarded
//! connection at a time, through the protocol crate's remote scope ([`holzwave_ipc::serve_remote`]).
//!
//! Who is asking is never read from a request. The sink is made for one pane and passes that pane on, so a process that
//! names another pane in its request is still answered for its own. What the process may ask is decided twice: by the
//! protocol crate (`ping`, `view`, `edit` and `setmeta`, in [`holzwave_ipc::Cmd::allowed_remote`]) and by
//! [`crate::state::AppCore::wsh_remote`], which has no branch for any command that decides, consents, approves or grants.
//!
//! The token and the socket path authorise a request, so neither is ever printed, logged, put in a notice or sent to the
//! page: [`WshSink`] prints its pane and nothing else, and the core's `Tunnel` prints neither.

use std::io::BufReader;
use std::sync::{Arc, Weak};

use holzwave_core::layout::PaneId;
use holzwave_core::pool::{ForwardSink, ForwardedStream};
use holzwave_core::profiles::Profile;
use holzwave_core::ssh::Tunnel;
use holzwave_ipc::{Cmd, Handler, REMOTE_REQUESTS, REMOTE_WINDOW, RateLimit, new_token};

use crate::state::AppCore;

/// The variable that names the socket on the host.
pub const SOCKET_VAR: &str = "HOLZWAVE_SOCKET";
/// The variable that holds the token every request carries.
pub const TOKEN_VAR: &str = "HOLZWAVE_TOKEN";
/// The variable that names the pane the shell runs in. The sink does not believe it (see the module documentation); `wsh`
/// sends it only because the local socket's protocol has the member.
pub const PANE_VAR: &str = "HOLZWAVE_PANE";

/// The socket path for a token of 32 lowercase hexadecimal characters: the form `holzwave_core::pool::valid_forward_path`
/// accepts.
pub fn forward_path(token: &str) -> String {
    format!("/tmp/holzwave-{token}.sock")
}

/// Where a pane's saved connection pointed when the pane started: the host, the port and the user. A saved connection can be
/// edited while a pane of it still runs (to another host, say); a request or an install that came from the pane of the old
/// host must then never be answered on the new one, so the pane's start records this and every later use compares it with
/// the connection as it is now.
#[derive(Clone, PartialEq, Eq)]
pub struct Target {
    host: String,
    port: u16,
    user: String,
}

impl Target {
    /// The target of a saved connection as it is now.
    pub fn of(profile: &Profile) -> Self {
        Self { host: profile.host.clone(), port: profile.port, user: profile.user.clone() }
    }

    /// Whether the saved connection still points where it did.
    pub fn is(&self, profile: &Profile) -> bool {
        *self == Self::of(profile)
    }
}

impl std::fmt::Debug for Target {
    // The user and the host name a person's account: only that something is recorded is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target").finish_non_exhaustive()
    }
}

/// The code that answers the forwarded connections of one pane. It holds the pane, the token and the budget of that pane
/// and only a weak reference to the core, so a forward that outlives the app asks nothing of it.
pub struct WshSink {
    core: Weak<AppCore>,
    pane: PaneId,
    token: String,
    /// Where the pane's connection pointed when the tunnel was made.
    target: Target,
    limit: RateLimit,
}

impl WshSink {
    pub fn new(core: Weak<AppCore>, pane: PaneId, token: String, target: Target) -> Self {
        Self { core, pane, token, target, limit: RateLimit::new(REMOTE_REQUESTS, REMOTE_WINDOW) }
    }
}

impl std::fmt::Debug for WshSink {
    // The token authorises requests: only the pane is printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WshSink").field("pane", &self.pane).finish_non_exhaustive()
    }
}

impl ForwardSink for WshSink {
    fn serve(&self, mut stream: ForwardedStream) {
        // An app that is gone answers nothing; the connection just ends.
        let Some(core) = self.core.upgrade() else { return };
        let handler = RemoteHandler { core, pane: self.pane, target: self.target.clone() };
        let mut reader = BufReader::new(&mut stream.reader);
        holzwave_ipc::serve_remote(&mut reader, &mut stream.writer, &self.token, self.pane, &handler, &self.limit);
    }
}

/// Answers a request of one pane's process. The pane the protocol crate passes is ignored: this handler was made for one
/// pane and acts for it alone.
struct RemoteHandler {
    core: Arc<AppCore>,
    pane: PaneId,
    target: Target,
}

impl Handler for RemoteHandler {
    fn handle(&self, _asked_for: Option<u64>, cmd: Cmd) -> Result<String, String> {
        self.core.wsh_remote(self.pane, &self.target, cmd)
    }
}

/// The tunnel of `pane`, or `None` unless the pane runs a saved SSH connection that has `wsh` turned on. A pane that shows
/// a file browser or a preview, a local pane and a Docker or WSL pane get none; neither does a pane when no random number
/// can be had for its token (a socket without a token is never made).
pub fn tunnel_for(core: &AppCore, pane: PaneId) -> Option<Tunnel> {
    let target = Target::of(&core.wsh_profile(pane).ok()?);
    // Two independent values: the one in the socket path (who can even find the socket) and the one in every request.
    let socket_token = new_token().ok()?;
    let token = new_token().ok()?;
    let path = forward_path(&socket_token);
    let env = vec![
        (SOCKET_VAR.to_owned(), path.clone()),
        (TOKEN_VAR.to_owned(), token.clone()),
        (PANE_VAR.to_owned(), pane.to_string()),
    ];
    let sink: Arc<dyn ForwardSink> = Arc::new(WshSink::new(core.downgrade(), pane, token, target));
    Some(Tunnel { path, env, sink })
}
