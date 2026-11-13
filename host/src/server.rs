//! The link of this device to a holzWave server, on the host side: the address and device id in `server_link.json`, the device
//! token in the keychain, the sign-in that gets one, and the view the page reads.
//!
//! The rules of the token and of the wait are in the core ([`holzwave_core::server_link`], [`holzwave_core::server_signin`]); this
//! file keeps the facts of one device:
//!
//! * **One server per device.** The link file holds one address. While a token is kept, a sign-in at another address is refused
//!   until `Sign out`.
//! * **The token is only in the keychain** (`server:token`). It is read for one request and dropped; the view and every answer of
//!   this file have no member that could hold it, and every failure is a fixed sentence.
//! * **Sign out forgets the token and keeps the address**, so the next sign-in goes straight to the code. It asks the server to
//!   end this device's sign-in first, best effort: a server that cannot be reached does not keep the person signed in here.
//! * **A token the server refuses is forgotten.** The view then reads as not signed in.
//! * **A locked secret store** (the Android vault) is `locked`: no request is made, nothing is changed, and the next call after
//!   the unlock works.
//!
//! Every network call here blocks, up to the bound of the core's agent, and a certificate check by the platform's verifier can
//! take long: callers run them off the main thread. No lock of this file is held across a network call.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use holzwave_core::remote::hub::{Endpoint, Hub, HubOptions};
use holzwave_core::secrets::SecretStore;
use holzwave_core::server_link::{Fault, LinkFileError, Origin, ServerLinkFile, Token, token_clear, token_load, token_save};
use holzwave_core::server_signin::{self as signin, Client, Grant, Poll, SignInError};
use serde::Serialize;

use crate::locked::{SECRETS_LOCKED, is_locked};

/// The most characters of the device name that is sent.
const MAX_NAME: usize = holzwave_core::server_link::MAX_NAME;

/// What a refusal to sign in at a second server says.
pub const OTHER_SERVER: &str = "This device is signed in to another server. Sign out first.";
/// What a token that cannot be put into the keychain says.
pub const NOT_KEPT: &str = "The sign-in could not be saved in the keychain.";
/// What a token that cannot be taken out of the keychain says.
pub const NOT_FORGOTTEN: &str = "The sign-in could not be removed from the keychain.";
/// What a keychain that cannot be read says.
pub const KEYCHAIN: &str = "The keychain could not be read.";
/// What a sync that needs the server says while this device is not signed in.
pub const NOT_SIGNED_IN: &str = "Sign in to the server first.";

/// What the page reads of the link. No member holds a secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ServerView {
    /// `signed_out`, `signing_in`, `signed_in` or `unreachable`.
    pub state: &'static str,
    /// The server's address, `https://host[:port]`; empty when this device never linked one. Kept after `Sign out`.
    pub url: String,
    /// The name of this device on the server (before a sign-in, the name that would be sent).
    pub name: String,
    /// The secret store is locked: the token cannot be read, so nothing is asked of the server until the unlock.
    pub locked: bool,
    /// The sync target is on at the server, as the last answer said.
    pub sync_on: bool,
    /// The remote sessions are on at the server, as the last answer said.
    pub remote_on: bool,
    /// The fixed sentence of the last failure, if there is one.
    pub error: Option<String>,
}

/// What the page needs to show a sign-in that started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SignInView {
    /// The code the person compares with the page the server shows, `XXXX-XXXX`.
    pub code: String,
    /// The address to open in the system browser (through `open_url`).
    pub url: String,
    /// Seconds the code lives.
    pub expires_in: u64,
    /// Seconds to leave between two polls.
    pub interval: u64,
}

/// How the wait stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SignInPoll {
    /// `pending`, `approved`, `denied`, `expired`, `too_many`, `off` or `unreachable`.
    pub state: &'static str,
    /// Seconds to leave before the next poll.
    pub interval: u64,
}

/// Why the credential cannot be had now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The secret store is locked.
    Locked,
    /// This device holds no sign-in.
    SignedOut,
    /// The keychain cannot be read.
    Keychain,
}

impl Unavailable {
    /// The fixed sentence.
    pub fn sentence(self) -> &'static str {
        match self {
            Unavailable::Locked => SECRETS_LOCKED,
            Unavailable::SignedOut => NOT_SIGNED_IN,
            Unavailable::Keychain => KEYCHAIN,
        }
    }
}

/// What the last request to the server found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    Unknown,
    Reachable,
    Unreachable,
}

/// What the last answers said, kept only for the view.
struct Seen {
    reach: Reach,
    sync_on: bool,
    remote_on: bool,
    error: Option<String>,
}

impl Seen {
    const NONE: Seen = Seen { reach: Reach::Unknown, sync_on: false, remote_on: false, error: None };
}

/// A sign-in that is waiting for the person.
struct Pending {
    grant: Grant,
    origin: String,
    name: String,
}

/// Why an approved sign-in was not kept.
enum NotKept {
    /// It was cancelled or replaced while its poll was out.
    Stale,
    /// The fixed sentence of a keychain or file failure.
    Failed(String),
}

struct State {
    link: Option<ServerLinkFile>,
    link_error: Option<LinkFileError>,
    pending: Option<Pending>,
    /// Counts the sign-ins started and the ones ended, so that a poll that comes back after either puts nothing back.
    epoch: u64,
    seen: Seen,
}

/// What is in the keychain under the account of the token, without the token.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kept {
    Present,
    Absent,
    Locked,
    Unreadable,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The name this device is called on a server: the computer's name, cleaned and cut to the limit, or the product name and the
/// platform when no name can be read. `os` is spelled as `std::env::consts::OS` spells it.
pub fn device_name_from(computer: &str, os: &str) -> String {
    let cleaned: String = computer.chars().filter(|c| !c.is_control()).collect();
    let cut: String = cleaned.trim().chars().take(MAX_NAME).collect();
    let cut = cut.trim().to_owned();
    if !cut.is_empty() {
        return cut;
    }
    let platform = match os {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        "android" => "Android",
        "ios" => "iOS",
        _ => "another system",
    };
    format!("holzWave on {platform}")
}

/// The name of this device, from what the platform offers without starting a program.
pub fn device_name() -> String {
    let os = std::env::consts::OS;
    let computer = crate::state::computer_name(os);
    device_name_from(&computer, os)
}

/// The request check of an Android build: the platform's certificate verifier is set up. Nothing elsewhere.
fn network_ready() -> Result<(), String> {
    #[cfg(target_os = "android")]
    crate::tls_ready::require().map_err(|_| Fault::Unreachable.sentence().to_owned())?;
    Ok(())
}

/// The sentence of a failure of the sign-in client.
fn sentence(error: &SignInError) -> String {
    error.sentence()
}

pub struct ServerRuntime {
    dir: PathBuf,
    secrets: Arc<dyn SecretStore>,
    name: String,
    /// Accept a loopback `http` address: for a test that runs a real server on this computer. The app never sets it.
    plain_loopback: bool,
    state: Mutex<State>,
    /// The one connection of this device to the sessions of the server, made when something first asks for it.
    hub: OnceLock<Hub>,
    hub_options: HubOptions,
}

/// What the hub is told about the server: the address of the link file and, for every connection it makes, the token as the
/// keychain holds it then. It keeps neither, so one hub lives through a sign-out and a new sign-in.
struct LinkEndpoint(Weak<ServerRuntime>);

impl Endpoint for LinkEndpoint {
    fn origin(&self) -> Option<Origin> {
        let runtime = self.0.upgrade()?;
        let link = lock(&runtime.state).link.clone()?;
        link.origin().ok()
    }

    fn token(&self) -> Result<Token, Fault> {
        let runtime = self.0.upgrade().ok_or(Fault::SignedOut)?;
        network_ready().map_err(|_| Fault::Unreachable)?;
        // A store that is locked or cannot be read gives no token: the pane waits as a signed-out pane does and tries again
        // on Enter, with whatever the store holds then.
        runtime.token_and_origin().map(|(token, _)| token).map_err(|_| Fault::SignedOut)
    }
}

impl ServerRuntime {
    /// The link of the device whose data directory is `dir`, with its token in `secrets`. A link file that cannot be used is left
    /// as it is and the view says so; the next sign-in replaces it.
    pub fn new(dir: PathBuf, secrets: Arc<dyn SecretStore>) -> ServerRuntime {
        ServerRuntime::build(dir, secrets, false)
    }

    /// [`ServerRuntime::new`] for a server on this computer over plain `http`. Meant for the tests that run a real server on the
    /// loopback address: a product server is always `https`, and the app calls [`ServerRuntime::new`].
    pub fn new_for_loopback(dir: PathBuf, secrets: Arc<dyn SecretStore>) -> ServerRuntime {
        ServerRuntime::build(dir, secrets, true)
    }

    fn build(dir: PathBuf, secrets: Arc<dyn SecretStore>, plain_loopback: bool) -> ServerRuntime {
        let (link, link_error) = match ServerLinkFile::load_with(&dir, plain_loopback) {
            Ok(link) => (link, None),
            Err(e) => (None, Some(e)),
        };
        ServerRuntime {
            dir,
            secrets,
            name: device_name(),
            plain_loopback,
            state: Mutex::new(State { link, link_error, pending: None, epoch: 0, seen: Seen::NONE }),
            hub: OnceLock::new(),
            hub_options: HubOptions::default(),
        }
    }

    /// The same runtime with other settings for its hub (the pauses between two tries). Meant for tests; it has to be called
    /// before the hub is first asked for.
    pub fn with_hub_options(mut self, options: HubOptions) -> ServerRuntime {
        self.hub_options = options;
        self
    }

    /// The one hub of this device: the connection that lists, makes and ends the sessions of the server and that every pane
    /// of a session is attached through. Nothing is connected until something is asked of it; it asks this runtime for the
    /// address and a fresh token whenever it connects.
    pub fn hub(self: &Arc<Self>) -> Hub {
        self.hub.get_or_init(|| Hub::new(Arc::new(LinkEndpoint(Arc::downgrade(self))), self.hub_options.clone())).clone()
    }

    /// Ends the connection of the hub, when there is one: its panes show the signed-out state and no session is ended.
    fn drop_connection(&self) {
        if let Some(hub) = self.hub.get() {
            hub.disconnect();
        }
    }

    /// The host of the server this device is linked to, for the name of a row: the host and nothing more. Empty when no
    /// server is known.
    pub fn host_name(&self) -> String {
        let link = lock(&self.state).link.clone();
        link.and_then(|link| link.origin().ok()).map(|origin| origin.host().to_owned()).unwrap_or_default()
    }

    /// A runtime that sends `name` as the name of this device. For tests.
    #[cfg(test)]
    fn named(mut self, name: &str) -> ServerRuntime {
        self.name = name.to_owned();
        self
    }

    fn kept(&self) -> Kept {
        match token_load(&*self.secrets) {
            Ok(Some(_)) => Kept::Present,
            Ok(None) => Kept::Absent,
            Err(e) if is_locked(&e) => Kept::Locked,
            Err(_) => Kept::Unreadable,
        }
    }

    /// What the page reads now, with no request to the server: the link file, the keychain and what the last answer said.
    pub fn view(&self) -> ServerView {
        let kept = self.kept();
        let state = lock(&self.state);
        self.compose(&state, kept)
    }

    fn compose(&self, state: &State, kept: Kept) -> ServerView {
        let locked = kept == Kept::Locked;
        let failure = |state: &State| {
            state
                .seen
                .error
                .clone()
                .or_else(|| state.link_error.map(|e| e.sentence().to_owned()))
                .or_else(|| (kept == Kept::Unreadable).then(|| KEYCHAIN.to_owned()))
        };
        if let Some(pending) = &state.pending {
            return ServerView {
                state: "signing_in",
                url: pending.origin.clone(),
                name: pending.name.clone(),
                locked,
                sync_on: false,
                remote_on: false,
                error: None,
            };
        }
        match &state.link {
            Some(link) if matches!(kept, Kept::Present | Kept::Locked) => ServerView {
                state: if state.seen.reach == Reach::Unreachable { "unreachable" } else { "signed_in" },
                url: link.url().to_owned(),
                name: link.name().to_owned(),
                locked,
                sync_on: state.seen.sync_on,
                remote_on: state.seen.remote_on,
                error: failure(state),
            },
            Some(link) => ServerView {
                state: "signed_out",
                url: link.url().to_owned(),
                name: link.name().to_owned(),
                locked,
                sync_on: false,
                remote_on: false,
                error: failure(state),
            },
            None => ServerView {
                state: "signed_out",
                url: String::new(),
                name: self.name.clone(),
                locked,
                sync_on: false,
                remote_on: false,
                error: failure(state),
            },
        }
    }

    /// Asks the server who this device is (one request): the state of the link and of the server's two switches. A token the
    /// server refuses is forgotten. Blocks on the network. With the store locked, or with no sign-in, no request is made.
    pub fn refresh(&self) -> ServerView {
        let Some(link) = lock(&self.state).link.clone() else { return self.view() };
        let token = match token_load(&*self.secrets) {
            Ok(Some(token)) => token,
            _ => return self.view(),
        };
        if let Err(sentence) = network_ready() {
            lock(&self.state).seen = Seen { reach: Reach::Unreachable, error: Some(sentence), ..Seen::NONE };
            return self.view();
        }
        let answer = signin::me(link.url(), &token);
        match answer {
            Ok(me) => {
                lock(&self.state).seen = Seen { reach: Reach::Reachable, sync_on: me.sync, remote_on: me.remote, error: None };
            }
            Err(SignInError::Fault(Fault::SignedOut)) => self.token_refused(&token),
            // The server has both device features off: the sign-in is not harmed, there is just nothing to use.
            Err(SignInError::Fault(Fault::Off)) => {
                lock(&self.state).seen = Seen { reach: Reach::Reachable, sync_on: false, remote_on: false, error: None };
            }
            Err(other) => {
                lock(&self.state).seen = Seen { reach: Reach::Unreachable, error: Some(sentence(&other)), ..Seen::NONE };
            }
        }
        drop(token);
        self.view()
    }

    /// The server refused `refused`: it is forgotten here, unless another sign-in replaced it in the meantime. The address stays.
    pub fn token_refused(&self, refused: &Token) {
        let same = matches!(token_load(&*self.secrets), Ok(Some(current)) if current.bearer().as_str() == refused.bearer().as_str());
        if same {
            let _ = token_clear(&*self.secrets);
            self.drop_connection();
        }
        lock(&self.state).seen = Seen::NONE;
    }

    /// The credential of this device and the address it is for, for a caller that makes a request (the sync run, the session
    /// link). The token is the caller's to drop.
    pub fn token_and_origin(&self) -> Result<(Token, String), Unavailable> {
        let link = lock(&self.state).link.clone().ok_or(Unavailable::SignedOut)?;
        match token_load(&*self.secrets) {
            Ok(Some(token)) => Ok((token, link.url().to_owned())),
            Ok(None) => Err(Unavailable::SignedOut),
            Err(e) if is_locked(&e) => Err(Unavailable::Locked),
            Err(_) => Err(Unavailable::Keychain),
        }
    }

    /// Starts a sign-in at `address`: the server answers a code and the address to open. Nothing is written yet and the keychain is
    /// not touched; the grant lives in memory until the person decides, the wait ends or [`ServerRuntime::sign_in_cancel`] is
    /// called. Blocks on the network.
    pub fn sign_in_start(&self, address: &str) -> Result<SignInView, String> {
        if self.kept() == Kept::Locked {
            return Err(SECRETS_LOCKED.to_owned());
        }
        let address = address.trim();
        let origin = if self.plain_loopback { Origin::parse(address) } else { Origin::parse_https(address) }.map_err(|e| e.sentence().to_owned())?;
        let text = origin.to_text();
        // One server per device: while a token is kept, only the same address may sign in again.
        let linked = lock(&self.state).link.clone();
        if let Some(link) = &linked
            && link.url() != text
            && self.kept() == Kept::Present
        {
            return Err(OTHER_SERVER.to_owned());
        }
        network_ready()?;
        let client = Client::new(&text).map_err(|e| sentence(&e))?;
        let grant = client.start(&self.name, signin::platform()).map_err(|e| sentence(&e))?;
        let view = SignInView {
            code: grant.user_code().to_owned(),
            url: grant.open_url().to_owned(),
            expires_in: grant.expires_in(),
            interval: grant.interval(),
        };
        let mut state = lock(&self.state);
        state.epoch += 1;
        state.pending = Some(Pending { grant, origin: text, name: self.name.clone() });
        Ok(view)
    }

    /// One poll of the sign-in in progress. The client holds a poll back that comes before the server's interval, so a page that
    /// polls too often costs nothing. On approval the token goes into the keychain and the link file is written. Blocks on the
    /// network. With no sign-in in progress the answer is `expired`.
    pub fn sign_in_poll(&self) -> Result<SignInPoll, String> {
        let (mut pending, epoch) = {
            let mut state = lock(&self.state);
            match state.pending.take() {
                Some(pending) => (pending, state.epoch),
                None => return Ok(SignInPoll { state: "expired", interval: 0 }),
            }
        };
        let answer = network_ready().map_err(SignInError::Blocked).and_then(|()| signin::poll(&mut pending.grant));
        let interval = pending.grant.interval();
        // What goes back into the state, unless the sign-in was cancelled or replaced while the request was out.
        let keep = |pending: Pending| {
            let mut state = lock(&self.state);
            if state.epoch == epoch && state.pending.is_none() {
                state.pending = Some(pending);
            }
        };
        match answer {
            Ok(Poll::Pending | Poll::SlowDown) => {
                keep(pending);
                Ok(SignInPoll { state: "pending", interval })
            }
            Ok(Poll::Denied) => Ok(SignInPoll { state: "denied", interval }),
            Ok(Poll::Expired) => Ok(SignInPoll { state: "expired", interval }),
            Ok(Poll::TooMany) => Ok(SignInPoll { state: "too_many", interval }),
            Ok(Poll::Granted { token, device_id, .. }) => match self.keep_sign_in(&pending, &token, &device_id, epoch) {
                Ok(replaced) => {
                    // A sign-in this device held before (a new sign-in at the same server) is ended on the server, best
                    // effort, so the old device does not stay there for its idle time and count against the person's cap.
                    if let Some((old, url)) = replaced
                        && network_ready().is_ok()
                    {
                        let _ = signin::revoke_self(&url, &old);
                    }
                    Ok(SignInPoll { state: "approved", interval })
                }
                Err(why) => {
                    // Not kept: the person cancelled, signed out or started another sign-in while the request was out, or the
                    // keychain or the file could not take it. The device the server just made would stay there unused, so it
                    // is ended there, best effort.
                    if network_ready().is_ok() {
                        let _ = signin::revoke_self(&pending.origin, &token);
                    }
                    match why {
                        NotKept::Stale => Ok(SignInPoll { state: "expired", interval }),
                        NotKept::Failed(sentence) => Err(sentence),
                    }
                }
            },
            Err(SignInError::Fault(Fault::Off)) => Ok(SignInPoll { state: "off", interval }),
            Err(SignInError::Fault(Fault::Unreachable)) => {
                keep(pending);
                Ok(SignInPoll { state: "unreachable", interval })
            }
            Err(other) => {
                keep(pending);
                Err(sentence(&other))
            }
        }
    }

    /// Writes what an approved sign-in comes to: the token into the keychain, then the link file; if the file cannot be written
    /// the token is taken out again, so a failure leaves no half. The device that is in the file is the one the server made.
    ///
    /// Nothing is written when the sign-in of `epoch` was cancelled or replaced while its poll was out ([`NotKept::Stale`]).
    /// The check and the writes happen under the lock of the state, which a cancel, a sign-out and a new sign-in also take to
    /// end the sign-in in progress, so none of them can come between the two (the writes are local: no network call).
    ///
    /// Answers the token this device held before and the address it was for, when there was one: the caller ends it on the
    /// server. A failure puts that token back, so the device stays signed in as it was.
    fn keep_sign_in(&self, pending: &Pending, token: &Token, device_id: &str, epoch: u64) -> Result<Option<(Token, String)>, NotKept> {
        let link = ServerLinkFile::new_with(&pending.origin, device_id, &pending.name, self.plain_loopback)
            .map_err(|e| NotKept::Failed(e.sentence().to_owned()))?;
        let mut state = lock(&self.state);
        if state.epoch != epoch || state.pending.is_some() {
            return Err(NotKept::Stale);
        }
        // The sign-in this one replaces. A keychain that cannot be read here fails the save below.
        let old = token_load(&*self.secrets).ok().flatten().filter(|old| old.bearer().as_str() != token.bearer().as_str());
        let old_url = state.link.as_ref().map(|link| link.url().to_owned());
        token_save(&*self.secrets, token)
            .map_err(|e| NotKept::Failed(if is_locked(&e) { SECRETS_LOCKED.to_owned() } else { NOT_KEPT.to_owned() }))?;
        if let Err(e) = link.save(&self.dir) {
            let _ = match &old {
                Some(old) => token_save(&*self.secrets, old),
                None => token_clear(&*self.secrets),
            };
            return Err(NotKept::Failed(e.sentence().to_owned()));
        }
        state.link = Some(link);
        state.link_error = None;
        state.pending = None;
        state.epoch += 1;
        state.seen = Seen::NONE;
        Ok(old.zip(old_url))
    }

    /// Drops the sign-in in progress. Nothing was written, so nothing is undone.
    pub fn sign_in_cancel(&self) -> ServerView {
        {
            let mut state = lock(&self.state);
            state.pending = None;
            state.epoch += 1;
        }
        self.view()
    }

    /// Signs this device out: the server is asked to end this device's sign-in (best effort), the token is forgotten and the
    /// address stays. With the store locked nothing is changed, because the token cannot be reached.
    pub fn sign_out(&self) -> Result<ServerView, String> {
        // Read under the lock that ends the sign-in in progress: an approval that is being kept now is either in the keychain
        // already, and is the token signed out here, or finds the sign-in ended and is not kept.
        let token = {
            let mut state = lock(&self.state);
            let token = match token_load(&*self.secrets) {
                Ok(token) => token,
                Err(e) if is_locked(&e) => return Err(SECRETS_LOCKED.to_owned()),
                Err(_) => return Err(KEYCHAIN.to_owned()),
            };
            state.pending = None;
            state.epoch += 1;
            token
        };
        let link = lock(&self.state).link.clone();
        if let (Some(token), Some(link)) = (&token, &link)
            && network_ready().is_ok()
        {
            // Whatever the server answers, this device forgets the token below.
            let _ = signin::revoke_self(link.url(), token);
        }
        drop(token);
        token_clear(&*self.secrets).map_err(|e| if is_locked(&e) { SECRETS_LOCKED.to_owned() } else { NOT_FORGOTTEN.to_owned() })?;
        lock(&self.state).seen = Seen::NONE;
        // The sessions of the server are not ended: the panes that show them say that the sign-in is gone.
        self.drop_connection();
        Ok(self.view())
    }
}

/// A server that speaks the device routes and the sync routes in memory, for the tests of this crate.
#[cfg(test)]
pub(crate) mod fake {
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use holzwave_core::sync::webdav::sha256_hex;

    pub const TOKEN: &str = "hwd_canaryCANARY-0123456789_abcdefghijklmnopqrs";
    pub const CODE: &str = "hwg_canaryCANARY-0123456789_abcdefghijklmnopqrs";

    /// How the fake answers a poll.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Grant {
        Pending,
        Approved,
        Denied,
        Expired,
        TooMany,
        /// The feature is off: every route is a 404.
        Off,
    }

    #[derive(Clone)]
    pub struct Fake {
        pub origin: String,
        pub grant: Arc<Mutex<Grant>>,
        /// Whether the token is still accepted.
        pub accepts: Arc<AtomicBool>,
        pub requests: Arc<Mutex<Vec<String>>>,
        pub files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
        pub revokes: Arc<AtomicUsize>,
        pub sync_on: Arc<AtomicBool>,
        /// When set, the server stops answering (every connection is closed unanswered).
        pub down: Arc<AtomicBool>,
        /// How long the answer to a poll is held back, in milliseconds: a poll that is still out while the person acts.
        pub poll_delay: Arc<AtomicU64>,
        /// The token an approval issues; [`TOKEN`] unless a test needs a second one. Both are accepted.
        pub issue: Arc<Mutex<String>>,
        /// The tokens a device ended its own sign-in with, in order.
        pub revoked: Arc<Mutex<Vec<String>>>,
    }

    fn respond(stream: &mut TcpStream, status: &str, extra: &str, body: &[u8]) {
        let head = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n", body.len());
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
    }

    impl Fake {
        pub fn start() -> Fake {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = listener.local_addr().expect("address").port();
            let fake = Fake {
                origin: format!("http://127.0.0.1:{port}"),
                grant: Arc::new(Mutex::new(Grant::Pending)),
                accepts: Arc::new(AtomicBool::new(true)),
                requests: Arc::new(Mutex::new(Vec::new())),
                files: Arc::new(Mutex::new(BTreeMap::new())),
                revokes: Arc::new(AtomicUsize::new(0)),
                sync_on: Arc::new(AtomicBool::new(true)),
                down: Arc::new(AtomicBool::new(false)),
                poll_delay: Arc::new(AtomicU64::new(0)),
                issue: Arc::new(Mutex::new(TOKEN.to_owned())),
                revoked: Arc::new(Mutex::new(Vec::new())),
            };
            let shared = fake.clone();
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { return };
                    let shared = shared.clone();
                    thread::spawn(move || serve(stream, &shared));
                }
            });
            fake
        }

        pub fn count(&self, prefix: &str) -> usize {
            self.requests.lock().unwrap().iter().filter(|line| line.starts_with(prefix)).count()
        }

        pub fn total(&self) -> usize {
            self.requests.lock().unwrap().len()
        }
    }

    fn serve(mut stream: TcpStream, fake: &Fake) {
        let Fake { grant, accepts, requests, files, revokes, sync_on, down, poll_delay, issue, revoked, .. } = fake;
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let mut parts = line.split_whitespace();
            let (method, target) = (parts.next().unwrap_or("").to_owned(), parts.next().unwrap_or("").to_owned());
            let mut headers: Vec<(String, String)> = Vec::new();
            let mut length = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 {
                    return;
                }
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_owned());
                    if k == "content-length" {
                        length = v.parse().unwrap_or(0);
                    }
                    headers.push((k, v));
                }
            }
            let mut body = vec![0u8; length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            // The body of the start of a grant is kept with its line, so a test can read the name that was sent.
            let line = if target == "/api/device/code" { format!("{method} {target} {}", String::from_utf8_lossy(&body)) } else { format!("{method} {target}") };
            requests.lock().unwrap().push(line);
            if down.load(Ordering::SeqCst) {
                return;
            }
            let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
            let bearer = header("authorization").and_then(|v| v.strip_prefix("Bearer ").map(str::to_owned));
            let issued = issue.lock().unwrap().clone();
            let authed = bearer.as_ref().is_some_and(|b| *b == TOKEN || *b == issued)
                && accepts.load(Ordering::SeqCst)
                && !revoked.lock().unwrap().contains(bearer.as_ref().unwrap());
            let mode = *grant.lock().unwrap();
            match (method.as_str(), target.as_str()) {
                _ if mode == Grant::Off && target.starts_with("/api/device/") && !target.ends_with("/me") => respond(&mut stream, "404 Not Found", "", b""),
                ("POST", "/api/device/code") => {
                    let started = format!(
                        r#"{{"device_code":"{CODE}","user_code":"BCDF-GHJK","verification_uri":"https://hw.example/","verification_uri_complete":"https://hw.example/#device=BCDF-GHJK","expires_in":600,"interval":1}}"#
                    );
                    respond(&mut stream, "200 OK", "", started.as_bytes());
                }
                ("POST", "/api/device/token") => {
                    thread::sleep(std::time::Duration::from_millis(poll_delay.load(Ordering::SeqCst)));
                    let answer = match mode {
                        Grant::Pending | Grant::Off => r#"{"state":"pending"}"#.to_owned(),
                        Grant::Denied => r#"{"state":"denied"}"#.to_owned(),
                        Grant::Expired => r#"{"state":"expired"}"#.to_owned(),
                        Grant::TooMany => r#"{"state":"too_many"}"#.to_owned(),
                        Grant::Approved => format!(r#"{{"state":"approved","token":"{issued}","device_id":17,"expires_at":"2027-02-10T12:00:00Z"}}"#),
                    };
                    respond(&mut stream, "200 OK", "", answer.as_bytes());
                }
                ("GET", "/api/device/me") if authed => {
                    let on = sync_on.load(Ordering::SeqCst);
                    let me = format!(
                        r#"{{"device":{{"id":17,"name":"Holz's laptop","platform":"macos","expires_at":"2027-02-10T12:00:00Z"}},"switches":{{"sync":{on},"remote":false}}}}"#
                    );
                    respond(&mut stream, "200 OK", "", me.as_bytes());
                }
                ("DELETE", "/api/device/me") if authed => {
                    revokes.fetch_add(1, Ordering::SeqCst);
                    revoked.lock().unwrap().extend(bearer.clone());
                    respond(&mut stream, "204 No Content", "", b"");
                }
                (_, path) if path.starts_with("/api/device/me") => respond(&mut stream, "401 Unauthorized", "", b""),
                (_, path) if path.starts_with("/sync/v1/") && !authed => respond(&mut stream, "401 Unauthorized", "", b""),
                (_, path) if path.starts_with("/sync/v1/") && !sync_on.load(Ordering::SeqCst) => respond(&mut stream, "404 Not Found", "", b""),
                ("POST", path) if path.starts_with("/sync/v1/ensure/") => respond(&mut stream, "200 OK", "", b"{}"),
                ("GET", path) if path.starts_with("/sync/v1/list/") => {
                    let dir = path.trim_start_matches("/sync/v1/list/");
                    let prefix = if dir == "-" { String::new() } else { format!("{dir}/") };
                    let files = files.lock().unwrap();
                    let items: Vec<String> = files
                        .iter()
                        .filter(|(name, _)| name.starts_with(&prefix) && !name[prefix.len()..].contains('/') && name.len() > prefix.len())
                        .map(|(name, bytes)| {
                            let sum = sha256_hex(bytes);
                            format!(r#"{{"name":"{}","etag":"{sum}","checksum":"{sum}"}}"#, &name[prefix.len()..])
                        })
                        .collect();
                    respond(&mut stream, "200 OK", "", format!("[{}]", items.join(",")).as_bytes());
                }
                ("GET", path) if path.starts_with("/sync/v1/blob/") => {
                    let name = path.trim_start_matches("/sync/v1/blob/");
                    match files.lock().unwrap().get(name) {
                        Some(bytes) => respond(&mut stream, "200 OK", &format!("ETag: \"{}\"\r\n", sha256_hex(bytes)), bytes),
                        None => respond(&mut stream, "404 Not Found", "", b""),
                    }
                }
                ("PUT", path) if path.starts_with("/sync/v1/blob/") => {
                    let name = path.trim_start_matches("/sync/v1/blob/").to_owned();
                    let mut files = files.lock().unwrap();
                    let current = files.get(&name).map(|bytes| sha256_hex(bytes));
                    let refused = match (header("if-none-match").as_deref(), header("if-match")) {
                        (Some("*"), _) => current.is_some(),
                        (_, Some(etag)) => current.as_deref() != Some(etag.trim_matches('"')),
                        _ => false,
                    };
                    if refused {
                        respond(&mut stream, "412 Precondition Failed", "", b"");
                    } else {
                        let etag = sha256_hex(&body);
                        files.insert(name, body);
                        respond(&mut stream, "200 OK", &format!("ETag: \"{etag}\"\r\n"), b"{}");
                    }
                }
                _ => respond(&mut stream, "404 Not Found", "", b""),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{CODE, Fake, Grant as Mode, TOKEN};
    use super::*;
    use holzwave_core::secrets::MemoryStore;
    use holzwave_core::server_link::TOKEN_ACCOUNT;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// A store that can be locked, as the Android vault is: every call answers the locked error while it is.
    struct Lockable {
        inner: MemoryStore,
        locked: AtomicBool,
    }

    impl Lockable {
        fn new() -> Arc<Lockable> {
            Arc::new(Lockable { inner: MemoryStore::default(), locked: AtomicBool::new(false) })
        }

        fn lock(&self, on: bool) {
            self.locked.store(on, Ordering::SeqCst);
        }

        fn check(&self) -> holzwave_core::Result<()> {
            if self.locked.load(Ordering::SeqCst) {
                Err(holzwave_core::Error::Io(std::io::Error::other(SECRETS_LOCKED)))
            } else {
                Ok(())
            }
        }
    }

    impl SecretStore for Lockable {
        fn get(&self, account: &str) -> holzwave_core::Result<Option<String>> {
            self.check()?;
            self.inner.get(account)
        }

        fn set(&self, account: &str, secret: &str) -> holzwave_core::Result<()> {
            self.check()?;
            self.inner.set(account, secret)
        }

        fn delete(&self, account: &str) -> holzwave_core::Result<()> {
            self.check()?;
            self.inner.delete(account)
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hw-server-rt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn runtime(tag: &str) -> (ServerRuntime, Arc<Lockable>, PathBuf) {
        let dir = scratch(tag);
        let store = Lockable::new();
        (ServerRuntime::new_for_loopback(dir.clone(), store.clone()).named("Holz's laptop"), store, dir)
    }

    /// Signs the runtime in at the fake: start, wait out the server's one-second interval, approve, poll.
    fn sign_in(rt: &ServerRuntime, fake: &Fake) {
        *fake.grant.lock().unwrap() = Mode::Pending;
        rt.sign_in_start(&fake.origin).expect("a sign-in starts");
        *fake.grant.lock().unwrap() = Mode::Approved;
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(rt.sign_in_poll().expect("a poll").state, "approved");
    }

    #[test]
    fn a_fresh_install_is_signed_out_and_a_sign_in_keeps_nothing_until_it_is_approved() {
        let (rt, store, dir) = runtime("fresh");
        let fake = Fake::start();
        let view = rt.view();
        assert_eq!((view.state, view.url.as_str(), view.name.as_str(), view.locked), ("signed_out", "", "Holz's laptop", false));
        assert_eq!(rt.sign_in_poll().unwrap().state, "expired", "nothing is waiting");

        let started = rt.sign_in_start(&fake.origin).expect("started");
        assert_eq!((started.code.as_str(), started.expires_in, started.interval), ("BCDF-GHJK", 600, 1));
        assert_eq!(started.url, format!("{}/#device=BCDF-GHJK", fake.origin));
        assert_eq!(rt.view().state, "signing_in");
        assert_eq!(rt.view().url, fake.origin);
        // Only in memory: no file, no keychain entry.
        assert!(!dir.join("server_link.json").exists());
        assert_eq!(store.get(TOKEN_ACCOUNT).unwrap(), None);
        // A poll before the server's interval is over is held back by the client's own clock.
        assert_eq!(rt.sign_in_poll().unwrap().state, "pending");
        assert_eq!(fake.count("POST /api/device/token"), 0);
        // Cancelling drops the grant: nothing was written, so nothing is undone.
        let view = rt.sign_in_cancel();
        assert_eq!(view.state, "signed_out");
        assert_eq!(rt.sign_in_poll().unwrap().state, "expired");
        assert!(!dir.join("server_link.json").exists());

        // The name that is sent is the device's name.
        let names: Vec<String> = fake.requests.lock().unwrap().iter().filter(|l| l.starts_with("POST /api/device/code")).cloned().collect();
        assert!(names[0].contains(r#""name":"Holz's laptop""#), "{names:?}");

        // Approval: the token goes to the keychain under its account, the address and id to the file.
        rt.sign_in_start(&fake.origin).unwrap();
        *fake.grant.lock().unwrap() = Mode::Approved;
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(rt.sign_in_poll().unwrap().state, "approved");
        assert_eq!(store.get("server:token").unwrap().as_deref(), Some(TOKEN));
        let file = std::fs::read_to_string(dir.join("server_link.json")).unwrap();
        assert!(file.contains(&fake.origin) && file.contains("\"device_id\": \"17\"") && file.contains("Holz's laptop"), "{file}");
        assert!(!file.contains(TOKEN) && !file.contains("hwd_"), "no token in the file");
        let view = rt.view();
        assert_eq!((view.state, view.url.as_str(), view.name.as_str()), ("signed_in", fake.origin.as_str(), "Holz's laptop"));

        // A restart reads the same: a second runtime on the same directory and store.
        let again = ServerRuntime::new_for_loopback(dir.clone(), store.clone());
        assert_eq!(again.view(), view);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_end_of_the_wait_is_a_state_of_its_own() {
        let (rt, _store, dir) = runtime("ends");
        let fake = Fake::start();
        for (mode, word) in [(Mode::Denied, "denied"), (Mode::Expired, "expired"), (Mode::TooMany, "too_many")] {
            *fake.grant.lock().unwrap() = Mode::Pending;
            rt.sign_in_start(&fake.origin).unwrap();
            *fake.grant.lock().unwrap() = mode;
            std::thread::sleep(Duration::from_millis(1100));
            assert_eq!(rt.sign_in_poll().unwrap().state, word);
            // The wait ended: nothing is waiting any more, and nothing was kept.
            assert_eq!(rt.sign_in_poll().unwrap().state, "expired");
            assert_eq!(rt.view().state, "signed_out");
        }
        // The server has the feature off: the start itself says so, in the fixed sentence of the link.
        *fake.grant.lock().unwrap() = Mode::Off;
        assert_eq!(rt.sign_in_start(&fake.origin).unwrap_err(), Fault::Off.sentence());
        // Nobody is there: the start says it cannot be reached.
        drop(fake);
        let gone = Fake::start();
        let origin = gone.origin.clone();
        gone.down.store(true, Ordering::SeqCst);
        assert_eq!(rt.sign_in_start(&origin).unwrap_err(), Fault::Unreachable.sentence());
        assert!(!dir.join("server_link.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_poll_that_loses_the_server_keeps_the_wait_and_a_poll_that_meets_off_ends_it() {
        let (rt, _store, dir) = runtime("poll-faults");
        let fake = Fake::start();
        rt.sign_in_start(&fake.origin).unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        fake.down.store(true, Ordering::SeqCst);
        assert_eq!(rt.sign_in_poll().unwrap().state, "unreachable");
        assert_eq!(rt.view().state, "signing_in", "the wait goes on");
        fake.down.store(false, Ordering::SeqCst);
        *fake.grant.lock().unwrap() = Mode::Off;
        // The 404 of the closed feature ends the wait.
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(rt.sign_in_poll().unwrap().state, "off");
        assert_eq!(rt.view().state, "signed_out", "the wait ended");
        assert_eq!(rt.sign_in_poll().unwrap().state, "expired");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Starts a sign-in at `fake` that the server approves, and polls it on another thread with the answer held back; `meanwhile`
    /// runs while that poll is out. Returns the poll's answer.
    fn poll_out_while(rt: &ServerRuntime, fake: &Fake, meanwhile: impl FnOnce()) -> SignInPoll {
        rt.sign_in_start(&fake.origin).expect("a sign-in starts");
        *fake.grant.lock().unwrap() = Mode::Approved;
        std::thread::sleep(Duration::from_millis(1100));
        fake.poll_delay.store(400, Ordering::SeqCst);
        let polls = fake.count("POST /api/device/token");
        std::thread::scope(|scope| {
            let poll = scope.spawn(|| rt.sign_in_poll());
            while fake.count("POST /api/device/token") == polls {
                std::thread::sleep(Duration::from_millis(5));
            }
            meanwhile();
            poll.join().unwrap().expect("a poll")
        })
    }

    #[test]
    fn an_approval_that_comes_back_after_cancel_is_not_kept_and_the_device_it_made_is_ended() {
        let (rt, store, dir) = runtime("late-cancel");
        let fake = Fake::start();
        let answer = poll_out_while(&rt, &fake, || {
            rt.sign_in_cancel();
        });
        assert_eq!(answer.state, "expired");
        assert_eq!(store.get(TOKEN_ACCOUNT).unwrap(), None, "nothing in the keychain");
        assert!(!dir.join("server_link.json").exists(), "no link file");
        assert_eq!(rt.view().state, "signed_out");
        assert_eq!(fake.revokes.load(Ordering::SeqCst), 1, "the device the server made is ended there");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_approval_that_comes_back_after_sign_out_does_not_sign_the_device_in_again() {
        let (rt, store, dir) = runtime("late-out");
        let fake = Fake::start();
        sign_in(&rt, &fake);
        // A second sign-in at the same server, and the person signs out while its poll is out.
        let answer = poll_out_while(&rt, &fake, || {
            rt.sign_out().expect("signed out");
        });
        assert_eq!(answer.state, "expired");
        assert_eq!(store.get(TOKEN_ACCOUNT).unwrap(), None, "the token is not written again");
        assert_eq!(rt.view().state, "signed_out");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_approval_from_one_server_that_comes_back_after_a_sign_in_at_another_began_keeps_neither_link_nor_token() {
        let (rt, store, dir) = runtime("late-other");
        let (first, second) = (Fake::start(), Fake::start());
        let answer = poll_out_while(&rt, &first, || {
            rt.sign_in_cancel();
            rt.sign_in_start(&second.origin).expect("the sign-in at the other server starts");
        });
        assert_eq!(answer.state, "expired");
        assert_eq!(store.get(TOKEN_ACCOUNT).unwrap(), None);
        assert!(!dir.join("server_link.json").exists());
        // The sign-in the person chose is still waiting, at the server they chose.
        let view = rt.view();
        assert_eq!((view.state, view.url.as_str()), ("signing_in", second.origin.as_str()));
        *second.grant.lock().unwrap() = Mode::Approved;
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(rt.sign_in_poll().unwrap().state, "approved");
        assert_eq!(rt.view().url, second.origin);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A second token of the shape the client requires, for a second sign-in.
    const SECOND: &str = "hwd_secondSECOND-0123456789_abcdefghijklmnopqrs";

    #[test]
    fn a_new_sign_in_at_the_same_server_ends_the_sign_in_it_replaces_there() {
        let (rt, store, dir) = runtime("again");
        let fake = Fake::start();
        sign_in(&rt, &fake);
        *fake.issue.lock().unwrap() = SECOND.to_owned();
        sign_in(&rt, &fake);
        assert_eq!(store.get(TOKEN_ACCOUNT).unwrap().as_deref(), Some(SECOND), "the new token is kept");
        assert_eq!(*fake.revoked.lock().unwrap(), vec![TOKEN.to_owned()], "the old device is ended, and only that one");
        assert_eq!(rt.refresh().state, "signed_in");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_approval_the_keychain_cannot_take_ends_the_device_the_server_made_for_it() {
        let (rt, store, dir) = runtime("not-kept");
        let fake = Fake::start();
        // The store is locked between the start and the approval: the token cannot be kept.
        *fake.grant.lock().unwrap() = Mode::Pending;
        rt.sign_in_start(&fake.origin).unwrap();
        *fake.grant.lock().unwrap() = Mode::Approved;
        store.lock(true);
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(rt.sign_in_poll().unwrap_err(), SECRETS_LOCKED);
        store.lock(false);
        assert_eq!(*fake.revoked.lock().unwrap(), vec![TOKEN.to_owned()], "the device the server made is ended");
        assert_eq!(store.get(TOKEN_ACCOUNT).unwrap(), None);
        assert!(!dir.join("server_link.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_file_that_cannot_be_written_ends_the_new_device_and_keeps_the_sign_in_before_it() {
        use std::os::unix::fs::PermissionsExt;
        let (rt, store, dir) = runtime("file-fails");
        let fake = Fake::start();
        sign_in(&rt, &fake);
        *fake.issue.lock().unwrap() = SECOND.to_owned();
        *fake.grant.lock().unwrap() = Mode::Pending;
        rt.sign_in_start(&fake.origin).unwrap();
        *fake.grant.lock().unwrap() = Mode::Approved;
        std::thread::sleep(Duration::from_millis(1100));
        let set = |mode: u32| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        set(0o500);
        let polled = rt.sign_in_poll();
        set(0o700);
        assert!(polled.is_err(), "a file that cannot be written fails the sign-in");
        assert_eq!(store.get(TOKEN_ACCOUNT).unwrap().as_deref(), Some(TOKEN), "the token before it is back");
        assert!(dir.join("server_link.json").exists());
        assert_eq!(*fake.revoked.lock().unwrap(), vec![SECOND.to_owned()], "the new device is ended, the old one is not");
        assert_eq!(rt.refresh().state, "signed_in", "the old sign-in still works");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sign_out_asks_the_server_forgets_only_the_token_and_keeps_the_address() {
        let (rt, store, dir) = runtime("out");
        let fake = Fake::start();
        sign_in(&rt, &fake);
        let view = rt.sign_out().expect("signed out");
        assert_eq!(fake.revokes.load(Ordering::SeqCst), 1, "the server was asked to end this device's sign-in");
        assert_eq!((view.state, view.url.as_str()), ("signed_out", fake.origin.as_str()));
        assert_eq!(store.get(TOKEN_ACCOUNT).unwrap(), None);
        assert!(dir.join("server_link.json").exists(), "the file stays");

        // Best effort: a server that cannot be reached does not keep this device signed in.
        let fake = Fake::start();
        let (rt, store, dir2) = runtime("out-down");
        sign_in(&rt, &fake);
        fake.down.store(true, Ordering::SeqCst);
        let view = rt.sign_out().expect("signed out");
        assert_eq!(view.state, "signed_out");
        assert_eq!(store.get(TOKEN_ACCOUNT).unwrap(), None);
        assert_eq!(fake.revokes.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn a_token_the_server_refuses_is_forgotten_and_an_unreachable_server_changes_nothing() {
        let (rt, store, dir) = runtime("refresh");
        let fake = Fake::start();
        sign_in(&rt, &fake);
        // The server says who this device is: the sync switch is on, the remote one is off.
        let view = rt.refresh();
        assert_eq!((view.state, view.sync_on, view.remote_on, view.error), ("signed_in", true, false, None));
        // Unreachable: the token stays and the state says so.
        fake.down.store(true, Ordering::SeqCst);
        let view = rt.refresh();
        assert_eq!(view.state, "unreachable");
        assert_eq!(view.error.as_deref(), Some(Fault::Unreachable.sentence()));
        assert!(store.get(TOKEN_ACCOUNT).unwrap().is_some());
        fake.down.store(false, Ordering::SeqCst);
        assert_eq!(rt.refresh().state, "signed_in", "it recovers with the next answer");
        // Refused: the token is forgotten, the address stays, and the link reads as not signed in.
        fake.accepts.store(false, Ordering::SeqCst);
        let view = rt.refresh();
        assert_eq!((view.state, view.url.as_str()), ("signed_out", fake.origin.as_str()));
        assert_eq!(store.get(TOKEN_ACCOUNT).unwrap(), None);
        assert!(dir.join("server_link.json").exists());
        // No sign-in, no request.
        let before = fake.total();
        rt.refresh();
        assert_eq!(fake.total(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_locked_store_is_said_without_a_request_and_the_next_call_after_the_unlock_works() {
        let (rt, store, dir) = runtime("locked");
        let fake = Fake::start();
        sign_in(&rt, &fake);
        store.lock(true);
        let before = fake.total();
        let view = rt.refresh();
        assert!(view.locked);
        assert_eq!(view.state, "signed_in");
        assert_eq!(fake.total(), before, "no request while the store is locked");
        assert_eq!(rt.sign_in_start(&fake.origin).unwrap_err(), SECRETS_LOCKED);
        assert_eq!(rt.sign_out().unwrap_err(), SECRETS_LOCKED);
        assert_eq!(rt.token_and_origin().unwrap_err(), Unavailable::Locked);
        assert_eq!(fake.total(), before);
        store.lock(false);
        let view = rt.refresh();
        assert!(!view.locked && view.sync_on, "{view:?}");
        assert!(fake.total() > before);
        // The view and its serial form never hold the token or any part of it.
        let shown = format!("{view:?} {}", serde_json::to_string(&view).unwrap());
        assert!(!shown.contains(TOKEN) && !shown.contains("hwd_") && !shown.contains(&TOKEN[4..]), "{shown}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_server_per_device_until_sign_out_and_only_an_https_origin_in_the_product() {
        let (rt, _store, dir) = runtime("one");
        let first = Fake::start();
        let second = Fake::start();
        sign_in(&rt, &first);
        assert_eq!(rt.sign_in_start(&second.origin).unwrap_err(), OTHER_SERVER);
        assert_eq!(second.total(), 0, "the other server was not asked");
        // The same server again is allowed (a new device token replaces the old one).
        rt.sign_in_start(&first.origin).expect("the same server");
        rt.sign_in_cancel();
        rt.sign_out().unwrap();
        rt.sign_in_start(&second.origin).expect("after Sign out another server is allowed");
        rt.sign_in_cancel();

        // The product takes https origins alone: loopback http is for tests, and the rules of an address hold.
        let product = ServerRuntime::new(scratch("product"), Lockable::new());
        for bad in ["http://127.0.0.1:1", "http://sync.example", "https://sync.example/path", "https://u:p@sync.example", "sync.example", ""] {
            let sentence = product.sign_in_start(bad).unwrap_err();
            assert!(sentence.starts_with("The ") || sentence.contains("address"), "{bad}: {sentence}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_device_name_is_the_computers_name_cleaned_to_the_limit_with_a_fallback() {
        assert_eq!(device_name_from("Holz's laptop", "linux"), "Holz's laptop");
        assert_eq!(device_name_from("  my\u{7}-pc\n ", "windows"), "my-pc");
        assert_eq!(device_name_from(&"n".repeat(100), "linux"), "n".repeat(64));
        assert_eq!(device_name_from(&format!("{}  tail", "é".repeat(63)), "linux").chars().count(), 63, "cut at 64 characters and trimmed");
        assert_eq!(device_name_from("", "macos"), "holzWave on macOS");
        assert_eq!(device_name_from("\u{7}\n", "windows"), "holzWave on Windows");
        assert_eq!(device_name_from("   ", "linux"), "holzWave on Linux");
        assert_eq!(device_name_from("", "android"), "holzWave on Android");
        assert_eq!(device_name_from("", "freebsd"), "holzWave on another system");
        // The name the link file takes: never refused for its length or a control character.
        for computer in ["", "x", &"y".repeat(500), "a\tb"] {
            let name = device_name_from(computer, "linux");
            assert!(ServerLinkFile::new("https://s.example", "1", &name).is_ok(), "{computer:?}");
        }
        assert!(ServerLinkFile::new("https://s.example", "1", &device_name()).is_ok());
    }

    #[test]
    fn nothing_of_the_token_or_the_code_is_in_the_files_or_in_any_error() {
        let (rt, store, dir) = runtime("canary");
        let fake = Fake::start();
        let mut texts: Vec<String> = Vec::new();
        sign_in(&rt, &fake);
        texts.push(format!("{:?}", rt.view()));
        fake.accepts.store(false, Ordering::SeqCst);
        texts.push(format!("{:?}", rt.refresh()));
        fake.down.store(true, Ordering::SeqCst);
        texts.extend(rt.sign_in_start(&fake.origin).err());
        texts.push(format!("{:?}", rt.sign_out()));
        for name in std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()) {
            texts.push(std::fs::read_to_string(&name).unwrap_or_default());
        }
        for text in texts {
            assert!(!text.contains(TOKEN) && !text.contains(CODE) && !text.contains(&TOKEN[4..]) && !text.contains("hwd_") && !text.contains("hwg_"), "{text}");
        }
        assert_eq!(store.get(TOKEN_ACCOUNT).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
