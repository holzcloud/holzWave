//! The client of the device grant: how an app with no credential of its own gets a device token from a holzWave server, with a
//! person approving it in the system browser under the server's own login (RFC 8628, "device authorization grant").
//!
//! The steps are [`start`] (the server answers a code to show, and the address to open), [`poll`] (the app asks, at the pace the
//! server set, whether the person decided), [`me`] (who the server says this device is) and [`revoke_self`] (Sign out: the
//! device asks the server to end its own sign-in).
//!
//! The server and the network are untrusted input, as everywhere in the link to a server:
//!
//! * the address is checked before any request (`https`, or `http` for a loopback host only; no user name, path, query or
//!   fragment; a bound on its length) and the token is sent only to the origin it was issued for, because a [`Grant`] and the
//!   [`Client`] that serves it hold their origin and nothing else is ever a target;
//! * the agent follows no redirect, keeps no cookie, and every body is read with a hard bound and parsed strictly (closed sets of
//!   members, closed state words, the token's exact shape);
//! * every failure is a [`Fault`] of the link with its one fixed sentence; nothing the server wrote is repeated in an error;
//! * the device code and the token are secret types: not `Clone`, wiped on drop, printing a marker for `Debug` and `Display`;
//! * the wait has its own clock ([`Wait`]): a poll that comes before the server's interval is over is not sent at all, a
//!   `slow_down` makes the interval longer and nothing makes it shorter.
//!
//! The address the person is sent to is built here from the origin the app was pointed at, with the user code in the fragment (a
//! fragment is never sent to any server): the verification address the server states is not used, so a server cannot send the
//! person anywhere else.

use std::fmt;
use std::io::Read;
use std::time::{Duration, Instant};

use serde::Deserialize;
use ureq::Agent;
use ureq::http::{HeaderValue, Request};
use zeroize::Zeroizing;

use crate::remote::ws::UrlError;
use crate::server_link::{Fault, MAX_DEVICE_ID, MAX_NAME, Origin, TOKEN_BODY_LEN, Token};
use crate::sync::server::agent;

/// The most bytes of any answer that is read. The answers are a few hundred bytes; a server that says more is answering wrongly.
pub const MAX_ANSWER: u64 = 64 * 1024;

/// How long one request may take altogether.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// What a device code starts with (the server's grant code prefix).
const GRANT_PREFIX: &str = "hwg_";

/// The letters a user code is made of, as the server writes it (no vowels, no look-alikes).
const USER_CODE_ALPHABET: &str = "BCDFGHJKLMNPQRSTVWXZ";

/// The words the server accepts for the platform of a device.
pub const PLATFORMS: [&str; 5] = ["macos", "windows", "linux", "android", "other"];

/// The longest wait between two polls that is ever kept, in seconds (the server's own cap).
pub const MAX_INTERVAL: u64 = 60;

/// The most seconds a grant may be said to live: the server's is ten minutes.
const MAX_LIFE: u64 = 3600;

/// The longest text of a member that is only shown or kept, in bytes.
const MAX_TEXT: usize = 128;

/// The platform word of this build, for the grant (the server shows it to the person who approves).
pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    }
}

/// Why a step of the sign-in did not work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInError {
    /// The server address is not one that is used (see [`Origin::parse`]); nothing was sent.
    Address(UrlError),
    /// The platform will not let this app reach that address yet (the local-network permission); the sentence is the core's own.
    Blocked(String),
    /// The device name or platform is not valid; nothing was sent.
    Invalid,
    /// The link to the server failed, as one of the closed set of faults.
    Fault(Fault),
}

impl SignInError {
    /// The fault of the link, when it was one.
    pub fn fault(&self) -> Option<Fault> {
        match self {
            SignInError::Fault(fault) => Some(*fault),
            _ => None,
        }
    }

    /// The fixed sentence of this error.
    pub fn sentence(&self) -> String {
        match self {
            SignInError::Address(e) => e.sentence().to_owned(),
            SignInError::Blocked(text) => text.clone(),
            SignInError::Invalid => "The device name or platform is not valid.".to_owned(),
            SignInError::Fault(fault) => fault.sentence().to_owned(),
        }
    }
}

impl fmt::Display for SignInError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sentence())
    }
}

impl std::error::Error for SignInError {}

impl From<Fault> for SignInError {
    fn from(fault: Fault) -> SignInError {
        SignInError::Fault(fault)
    }
}

impl From<UrlError> for SignInError {
    fn from(e: UrlError) -> SignInError {
        SignInError::Address(e)
    }
}

impl From<SignInError> for crate::Error {
    fn from(e: SignInError) -> crate::Error {
        crate::Error::Io(std::io::Error::other(e.sentence()))
    }
}

/// The device code of a grant: the secret only this app holds, which it shows to the server at every poll. Not `Clone`, wiped on
/// drop, printing a marker.
pub struct DeviceCode(Zeroizing<String>);

impl DeviceCode {
    /// Reads a device code: the prefix and exactly 43 characters of the base64url alphabet, anything else is refused without the
    /// text.
    fn parse(text: &str) -> Result<DeviceCode, Fault> {
        let body = text.strip_prefix(GRANT_PREFIX).ok_or(Fault::Protocol)?;
        if body.len() != TOKEN_BODY_LEN || !body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(Fault::Protocol);
        }
        Ok(DeviceCode(Zeroizing::new(text.to_owned())))
    }

    /// The JSON body of a poll, wiped when dropped.
    fn poll_body(&self) -> Zeroizing<Vec<u8>> {
        let mut body = Zeroizing::new(Vec::with_capacity(self.0.len() + 20));
        body.extend_from_slice(br#"{"device_code":""#);
        body.extend_from_slice(self.0.as_bytes());
        body.extend_from_slice(br#""}"#);
        body
    }
}

impl zeroize::ZeroizeOnDrop for DeviceCode {}

impl fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceCode(..)")
    }
}

impl fmt::Display for DeviceCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceCode(..)")
    }
}

/// The pace of the wait, as plain numbers and instants so a test drives it without a clock.
///
/// The interval is the one the server stated, in seconds. A poll is allowed once the interval has passed since the last request
/// (the start of the grant counts as one). The interval only ever grows: a `slow_down` answer makes it longer, and an interval a
/// server states that is shorter than the one in force changes nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wait {
    interval: u64,
    last: Instant,
}

impl Wait {
    /// The wait of a grant that started `now`, at `interval` seconds (bounded to 1 to [`MAX_INTERVAL`]).
    pub fn new(interval: u64, now: Instant) -> Wait {
        Wait { interval: interval.clamp(1, MAX_INTERVAL), last: now }
    }

    /// The interval in force, in seconds.
    pub fn interval(&self) -> u64 {
        self.interval
    }

    /// How long until the next poll may be sent; zero when it may be sent now.
    pub fn ready_in(&self, now: Instant) -> Duration {
        (self.last + Duration::from_secs(self.interval)).saturating_duration_since(now)
    }

    /// Notes that a request was sent `now`.
    pub fn sent(&mut self, now: Instant) {
        self.last = now;
    }

    /// Takes an interval the server stated for a poll that goes on: never shorter than the one in force, never over the cap.
    pub fn adopt(&mut self, stated: u64) {
        self.interval = self.interval.max(stated.min(MAX_INTERVAL));
    }

    /// The server asked for slower polls: the interval it states when that is longer, else the one in force and five seconds more
    /// (RFC 8628). Never shorter, never over the cap.
    pub fn slow_down(&mut self, stated: Option<u64>) {
        let stated = stated.unwrap_or(0).min(MAX_INTERVAL);
        self.interval = if stated > self.interval { stated } else { (self.interval + 5).min(MAX_INTERVAL) };
    }
}

/// A sign-in that was started: the code to show, the address to open, and what the app needs to ask how it went.
///
/// It holds the origin it came from (the polls go there and nowhere else) and the secret device code. It prints nothing of
/// either.
pub struct Grant {
    client: Client,
    device_code: DeviceCode,
    user_code: String,
    open_url: String,
    expires_in: u64,
    wait: Wait,
}

impl Grant {
    /// The code the person sees and compares with the page: `XXXX-XXXX`.
    pub fn user_code(&self) -> &str {
        &self.user_code
    }

    /// The address to open in the system browser: the server's own origin with the user code in the fragment.
    pub fn open_url(&self) -> &str {
        &self.open_url
    }

    /// How many seconds the grant lives, as stated at the start.
    pub fn expires_in(&self) -> u64 {
        self.expires_in
    }

    /// The seconds the app must leave between two polls now.
    pub fn interval(&self) -> u64 {
        self.wait.interval()
    }

    /// The pace of the wait.
    pub fn wait(&self) -> &Wait {
        &self.wait
    }
}

impl fmt::Debug for Grant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Grant(..)")
    }
}

/// How a poll came out.
#[derive(Debug)]
pub enum Poll {
    /// Nobody decided yet (or the poll came too early and was not sent).
    Pending,
    /// The server asked for slower polls; the grant's interval is longer now.
    SlowDown,
    /// The person said no.
    Denied,
    /// The code ran out, or the server does not know it.
    Expired,
    /// The server refuses more devices (the person has too many, or too many grants are open).
    TooMany,
    /// The person approved. The token is the one credential of this device.
    Approved { token: Token, device_id: String, expires_at: String },
}

/// What the server says about this device and about its own switches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Me {
    pub device_id: String,
    pub name: String,
    pub platform: String,
    pub expires_at: String,
    /// The sync target is on.
    pub sync: bool,
    /// The remote sessions are on.
    pub remote: bool,
}

/// One answer: the status and, for a success, its bounded body (wiped when dropped: a poll's body can hold the token).
struct Answer {
    status: u16,
    body: Zeroizing<Vec<u8>>,
}

/// The client of one server, by its origin.
#[derive(Clone)]
pub struct Client {
    agent: Agent,
    origin: Origin,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").field("origin", &self.origin.to_text()).finish_non_exhaustive()
    }
}

/// The fault of a status that is not a success.
fn fault_of(status: u16) -> Fault {
    match status {
        401 => Fault::SignedOut,
        404 => Fault::Off,
        300..=399 | 500..=599 => Fault::Protocol,
        _ => Fault::Refused,
    }
}

impl Client {
    /// A client of the server at `origin` (`https://host[:port]`, or `http` for a loopback host). The address is checked here and
    /// nothing is sent to one that fails.
    pub fn new(origin: &str) -> Result<Client, SignInError> {
        Client::with_timeout(origin, REQUEST_TIMEOUT)
    }

    /// The same with another bound on one request (tests use a short one).
    pub fn with_timeout(origin: &str, timeout: Duration) -> Result<Client, SignInError> {
        let origin = Origin::parse(origin)?;
        Ok(Client { agent: agent(origin.secure(), timeout), origin })
    }

    /// The server's address in its canonical form.
    pub fn origin(&self) -> String {
        self.origin.to_text()
    }

    fn call(&self, method: &str, path: &str, bearer: Option<&Token>, body: Option<&[u8]>) -> Result<Answer, SignInError> {
        let mut req = Request::builder().method(method).uri(format!("{}{path}", self.origin.to_text()));
        if let Some(token) = bearer {
            let value = token.bearer();
            let mut header = HeaderValue::from_str(&value).map_err(|_| Fault::Protocol)?;
            header.set_sensitive(true);
            req = req.header("Authorization", header);
        }
        if body.is_some() {
            req = req.header("Content-Type", "application/json");
        }
        let req = req.body(body.unwrap_or_default().to_vec()).map_err(|_| Fault::Protocol)?;
        // A server on the local network needs the platform's permission on Android 17: asked here, when the request is made.
        crate::reach::ensure_blocking(self.origin.host(), self.origin.port()).map_err(SignInError::Blocked)?;
        let mut resp = self.agent.run(req).map_err(|_| Fault::Unreachable)?;
        let status = resp.status().as_u16();
        // The body of anything but a success is the server's words, and nothing here would use them.
        if !(200..300).contains(&status) {
            return Ok(Answer { status, body: Zeroizing::new(Vec::new()) });
        }
        let announced = resp.headers().get("Content-Length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
        if announced.is_some_and(|n| n > MAX_ANSWER) {
            return Err(Fault::Protocol.into());
        }
        let mut bytes = Zeroizing::new(Vec::new());
        resp.body_mut().as_reader().take(MAX_ANSWER + 1).read_to_end(&mut bytes).map_err(|_| Fault::Unreachable)?;
        if bytes.len() as u64 > MAX_ANSWER {
            return Err(Fault::Protocol.into());
        }
        Ok(Answer { status, body: bytes })
    }

    /// Starts a grant for a device called `name` on `platform` (a word of [`PLATFORMS`]).
    pub fn start(&self, name: &str, platform: &str) -> Result<Grant, SignInError> {
        self.start_at(name, platform, Instant::now())
    }

    /// [`Client::start`] with the instant the grant starts, so the pace of the wait can be driven by a test.
    pub fn start_at(&self, name: &str, platform: &str, now: Instant) -> Result<Grant, SignInError> {
        let name_ok = !name.is_empty() && name.chars().count() <= MAX_NAME && !name.chars().any(char::is_control);
        if !name_ok || !PLATFORMS.contains(&platform) {
            return Err(SignInError::Invalid);
        }
        let body = serde_json::to_vec(&serde_json::json!({ "name": name, "platform": platform })).map_err(|_| SignInError::Invalid)?;
        let answer = self.call("POST", "/api/device/code", None, Some(&body))?;
        if answer.status != 200 {
            return Err(fault_of(answer.status).into());
        }
        let mut raw: StartAnswer = serde_json::from_slice(&answer.body).map_err(|_| Fault::Protocol)?;
        let device_code = DeviceCode::parse(&raw.device_code)?;
        if !is_user_code(&raw.user_code) || raw.verification_uri.len() > MAX_TEXT || raw.verification_uri_complete.len() > MAX_TEXT * 2 {
            return Err(Fault::Protocol.into());
        }
        let in_range = |value: i64, max: u64| u64::try_from(value).ok().filter(|v| (1..=max).contains(v)).ok_or(Fault::Protocol);
        let expires_in = in_range(raw.expires_in, MAX_LIFE)?;
        let interval = in_range(raw.interval, MAX_INTERVAL)?;
        let open_url = format!("{}/#device={}", self.origin.to_text(), raw.user_code);
        Ok(Grant { client: self.clone(), device_code, user_code: std::mem::take(&mut raw.user_code), open_url, expires_in, wait: Wait::new(interval, now) })
    }

    /// Asks how the sign-in went. A poll that comes before the interval is over is not sent: the answer is [`Poll::Pending`].
    pub fn poll(&self, grant: &mut Grant) -> Result<Poll, SignInError> {
        self.poll_at(grant, Instant::now())
    }

    /// [`Client::poll`] at a given instant.
    pub fn poll_at(&self, grant: &mut Grant, now: Instant) -> Result<Poll, SignInError> {
        if !grant.wait.ready_in(now).is_zero() {
            return Ok(Poll::Pending);
        }
        grant.wait.sent(now);
        let body = grant.device_code.poll_body();
        let answer = self.call("POST", "/api/device/token", None, Some(&body))?;
        match answer.status {
            200 => {}
            // The server's own throttle: slow down, as it asks.
            429 => {
                grant.wait.slow_down(None);
                return Ok(Poll::SlowDown);
            }
            status => return Err(fault_of(status).into()),
        }
        let mut raw: PollAnswer = serde_json::from_slice(&answer.body).map_err(|_| Fault::Protocol)?;
        let token = raw.token.take().map(Zeroizing::new);
        let stated = match raw.interval {
            None => None,
            Some(value) => Some(u64::try_from(value).ok().filter(|v| (1..=MAX_INTERVAL).contains(v)).ok_or(Fault::Protocol)?),
        };
        if raw.state != "approved" && (token.is_some() || raw.device_id.is_some() || raw.expires_at.is_some()) {
            return Err(Fault::Protocol.into());
        }
        Ok(match raw.state.as_str() {
            "pending" => {
                if let Some(secs) = stated {
                    grant.wait.adopt(secs);
                }
                Poll::Pending
            }
            "slow_down" => {
                grant.wait.slow_down(stated);
                Poll::SlowDown
            }
            "denied" => Poll::Denied,
            "expired" => Poll::Expired,
            "too_many" => Poll::TooMany,
            "approved" => {
                let token = Token::parse(token.as_deref().ok_or(Fault::Protocol)?.as_str())?;
                let device_id = raw.device_id.take().ok_or(Fault::Protocol)?.text()?;
                let expires_at = raw.expires_at.take().ok_or(Fault::Protocol)?;
                if !plain_text(&expires_at) {
                    return Err(Fault::Protocol.into());
                }
                Poll::Approved { token, device_id, expires_at }
            }
            _ => return Err(Fault::Protocol.into()),
        })
    }

    /// Who the server says `token` is.
    pub fn me(&self, token: &Token) -> Result<Me, SignInError> {
        let answer = self.call("GET", "/api/device/me", Some(token), None)?;
        if answer.status != 200 {
            return Err(fault_of(answer.status).into());
        }
        let raw: MeAnswer = serde_json::from_slice(&answer.body).map_err(|_| Fault::Protocol)?;
        let device_id = raw.device.id.text()?;
        let name_ok = !raw.device.name.is_empty() && raw.device.name.chars().count() <= MAX_NAME && !raw.device.name.chars().any(char::is_control);
        let platform_ok = raw.device.platform.len() <= 32 && raw.device.platform.bytes().all(|b| b.is_ascii_lowercase());
        if !name_ok || !platform_ok || !plain_text(&raw.device.expires_at) {
            return Err(Fault::Protocol.into());
        }
        Ok(Me {
            device_id,
            name: raw.device.name,
            platform: raw.device.platform,
            expires_at: raw.device.expires_at,
            sync: raw.switches.sync,
            remote: raw.switches.remote,
        })
    }

    /// Asks the server to end the sign-in of the device that holds `token`.
    pub fn revoke_self(&self, token: &Token) -> Result<(), SignInError> {
        let answer = self.call("DELETE", "/api/device/me", Some(token), None)?;
        if answer.status == 204 || answer.status == 200 { Ok(()) } else { Err(fault_of(answer.status).into()) }
    }
}

/// Starts a grant at the server at `origin`. See [`Client::start`].
pub fn start(origin: &str, name: &str, platform: &str) -> Result<Grant, SignInError> {
    Client::new(origin)?.start(name, platform)
}

/// Polls a grant at the origin it was started at. See [`Client::poll`].
pub fn poll(grant: &mut Grant) -> Result<Poll, SignInError> {
    let client = grant.client.clone();
    client.poll(grant)
}

/// Who the server at `origin` says `token` is. See [`Client::me`].
pub fn me(origin: &str, token: &Token) -> Result<Me, SignInError> {
    Client::new(origin)?.me(token)
}

/// Asks the server at `origin` to end the sign-in of the device that holds `token`. See [`Client::revoke_self`].
pub fn revoke_self(origin: &str, token: &Token) -> Result<(), SignInError> {
    Client::new(origin)?.revoke_self(token)
}

/// Whether `text` is a user code: four letters of the alphabet, a dash, four letters.
fn is_user_code(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 9 && b[4] == b'-' && b.iter().enumerate().all(|(i, &c)| i == 4 || USER_CODE_ALPHABET.as_bytes().contains(&c))
}

/// Whether a text the server sent for a time is short, printable ASCII (it is kept and shown, never interpreted).
fn plain_text(text: &str) -> bool {
    !text.is_empty() && text.len() <= 40 && text.bytes().all(|b| b.is_ascii_graphic())
}

/// The id of a device: the server writes a number, and a text is accepted. It must be one the link file takes.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawId {
    Number(u64),
    Text(String),
}

impl RawId {
    fn text(self) -> Result<String, Fault> {
        let text = match self {
            RawId::Number(n) => n.to_string(),
            RawId::Text(text) => text,
        };
        let ok = !text.is_empty() && text.len() <= MAX_DEVICE_ID && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if ok { Ok(text) } else { Err(Fault::Protocol) }
    }
}

/// The answer of the start of a grant. Every member is named; the address the server states is read for its size only.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartAnswer {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: i64,
    interval: i64,
}

impl Drop for StartAnswer {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.device_code);
    }
}

/// The answer of a poll: a state word, and by state the interval or the token with the id and the end of the device.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PollAnswer {
    state: String,
    #[serde(default)]
    interval: Option<i64>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    device_id: Option<RawId>,
    #[serde(default)]
    expires_at: Option<String>,
}

impl Drop for PollAnswer {
    fn drop(&mut self) {
        if let Some(token) = self.token.as_mut() {
            zeroize::Zeroize::zeroize(token);
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MeDevice {
    id: RawId,
    name: String,
    platform: String,
    expires_at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MeSwitches {
    sync: bool,
    remote: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MeAnswer {
    device: MeDevice,
    switches: MeSwitches,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// A device token and a grant code with the shape of real ones and text that is easy to find in any output.
    const TOKEN: &str = "hwd_canaryCANARY-0123456789_abcdefghijklmnopqrs";
    const CODE: &str = "hwg_canaryCANARY-0123456789_abcdefghijklmnopqrs";
    const CANARY: &str = "canaryCANARY-0123456789_abcdefghijklmnopqrs";

    /// A peer that answers each connection with exactly the bytes a test gives it, and records every request it saw.
    struct Peer {
        port: u16,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Peer {
        /// Answers every request with `reply` (raw bytes: a status line, headers and a body).
        fn answering(reply: Vec<u8>) -> Peer {
            Peer::spawn(move |stream: &mut TcpStream| {
                let _ = stream.write_all(&reply);
            })
        }

        /// A peer that reads the request and then says nothing until the client gives up.
        fn silent() -> Peer {
            Peer::spawn(|stream: &mut TcpStream| {
                let mut sink = [0u8; 64];
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                while matches!(std::io::Read::read(stream, &mut sink), Ok(n) if n > 0) {}
            })
        }

        fn spawn(answer: impl Fn(&mut TcpStream) + Send + 'static) -> Peer {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let port = listener.local_addr().expect("address").port();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { return };
                    // Read the request head and its body so the client never sees a reset before the answer.
                    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                    let mut head = String::new();
                    let mut length = 0usize;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            break;
                        }
                        if line == "\r\n" {
                            break;
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                        head.push_str(&line);
                    }
                    let mut body = vec![0u8; length];
                    let _ = std::io::Read::read_exact(&mut reader, &mut body);
                    log.lock().unwrap().push(head);
                    answer(&mut stream);
                }
            });
            Peer { port, seen }
        }

        fn origin(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        fn requests(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
    }

    fn http(status: &str, extra: &str, body: &str) -> Vec<u8> {
        format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}", body.len()).into_bytes()
    }

    fn started(code: &str, user: &str) -> String {
        format!(
            r#"{{"device_code":"{code}","user_code":"{user}","verification_uri":"https://hw.example/","verification_uri_complete":"https://hw.example/#device={user}","expires_in":600,"interval":5}}"#
        )
    }

    fn quick(origin: &str) -> Client {
        Client::with_timeout(origin, Duration::from_millis(600)).expect("a client")
    }

    fn token() -> Token {
        Token::parse(TOKEN).expect("a token")
    }

    /// Every error a hostile answer can end in is a fixed sentence of the link and never mentions what the server sent.
    fn quiet(error: &SignInError) {
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains(CANARY) && !shown.contains("hwd_") && !shown.contains("hwg_"), "{shown}");
    }

    // ---- the pace of the wait ----

    #[test]
    fn the_wait_is_a_pure_clock_that_only_ever_grows() {
        let t0 = Instant::now();
        let mut wait = Wait::new(5, t0);
        assert_eq!(wait.interval(), 5);
        assert_eq!(wait.ready_in(t0), Duration::from_secs(5), "the start counts as a request");
        assert_eq!(wait.ready_in(t0 + Duration::from_secs(3)), Duration::from_secs(2));
        assert!(wait.ready_in(t0 + Duration::from_secs(5)).is_zero());
        wait.sent(t0 + Duration::from_secs(6));
        assert_eq!(wait.ready_in(t0 + Duration::from_secs(7)), Duration::from_secs(4));
        // A stated interval never shortens.
        wait.adopt(2);
        assert_eq!(wait.interval(), 5);
        wait.adopt(9);
        assert_eq!(wait.interval(), 9);
        // slow_down takes the stated interval when it is longer, else five seconds more, and never goes over the cap.
        wait.slow_down(Some(20));
        assert_eq!(wait.interval(), 20);
        wait.slow_down(Some(3));
        assert_eq!(wait.interval(), 25, "a shorter stated interval still slows the poll");
        wait.slow_down(None);
        assert_eq!(wait.interval(), 30);
        wait.slow_down(Some(10_000));
        assert_eq!(wait.interval(), MAX_INTERVAL);
        wait.slow_down(None);
        wait.adopt(u64::MAX);
        assert_eq!(wait.interval(), MAX_INTERVAL, "the cap holds");
        assert_eq!(Wait::new(0, t0).interval(), 1);
        assert_eq!(Wait::new(9999, t0).interval(), MAX_INTERVAL);
    }

    // ---- the address ----

    #[test]
    fn an_address_that_is_not_a_server_origin_is_refused_before_any_request() {
        let peer = Peer::answering(http("200 OK", "", "{}"));
        for (bad, error) in [
            ("http://example.org", UrlError::Insecure),
            ("ftp://example.org", UrlError::Scheme),
            ("example.org", UrlError::Scheme),
            ("https://example.org/path", UrlError::Path),
            ("https://example.org/?q=1", UrlError::Query),
            ("https://user:pw@example.org", UrlError::Userinfo),
            ("https://example.org/#frag", UrlError::Fragment),
        ] {
            let refused = Client::new(bad).expect_err(bad);
            assert_eq!(refused, SignInError::Address(error), "{bad}");
            assert_eq!(refused.to_string(), error.sentence());
        }
        let long = format!("https://{}.example.org", "a".repeat(600));
        assert_eq!(Client::new(&long).expect_err("too long"), SignInError::Address(UrlError::TooLong));
        // The free functions check the address as well, and a loopback http address is the one http that is allowed.
        assert!(matches!(start("http://example.org", "n", "linux"), Err(SignInError::Address(_))));
        assert!(matches!(me("https://example.org/x", &token()), Err(SignInError::Address(_))));
        assert!(Client::new(&peer.origin()).is_ok());
        assert_eq!(peer.requests(), 0, "no request went out");
        // A name or a platform the server would refuse is refused here too.
        let client = quick(&peer.origin());
        for (name, platform) in [("", "linux"), ("a\nb", "linux"), (&"n".repeat(65)[..], "linux"), ("name", "plan9"), ("name", "")] {
            assert_eq!(client.start(name, platform).expect_err("invalid"), SignInError::Invalid, "{name:?} {platform:?}");
        }
        assert_eq!(peer.requests(), 0);
    }

    // ---- a well-formed grant, as a scripted peer would send it ----

    #[test]
    fn a_grant_has_the_code_to_show_and_the_address_built_from_the_origin_it_was_started_at() {
        let peer = Peer::answering(http("200 OK", "", &started(CODE, "BCDF-GHJK")));
        let grant = quick(&peer.origin()).start("Holz's laptop", "macos").expect("a grant");
        assert_eq!(grant.user_code(), "BCDF-GHJK");
        // The address the server stated (a different host here) is not used: the person is sent where the app was pointed.
        assert_eq!(grant.open_url(), format!("http://127.0.0.1:{}/#device=BCDF-GHJK", peer.port));
        assert_eq!((grant.expires_in(), grant.interval()), (600, 5));
        let shown = format!("{grant:?}");
        assert_eq!(shown, "Grant(..)");
        let code = DeviceCode::parse(CODE).unwrap();
        assert_eq!((format!("{code:?}"), format!("{code}")), ("DeviceCode(..)".to_owned(), "DeviceCode(..)".to_owned()));
    }

    // ---- hostile and broken answers ----

    #[test]
    fn a_hostile_or_broken_answer_to_the_start_ends_as_a_fixed_fault_and_never_hangs() {
        let ok = started(CODE, "BCDF-GHJK");
        let user_change = |from: &str, to: &str| ok.replace(from, to);
        let cases: Vec<(&str, Vec<u8>, Fault)> = vec![
            ("not json", http("200 OK", "", "this is not json"), Fault::Protocol),
            ("an empty body", http("200 OK", "", ""), Fault::Protocol),
            ("an extra member", http("200 OK", "", &user_change("\"interval\":5", "\"interval\":5,\"extra\":1")), Fault::Protocol),
            ("a code of the wrong shape", http("200 OK", "", &user_change(CODE, "hwg_short")), Fault::Protocol),
            ("a token where the code goes", http("200 OK", "", &user_change(CODE, TOKEN)), Fault::Protocol),
            ("a user code of the wrong shape", http("200 OK", "", &user_change("BCDF-GHJK", "AEIO-UAEI")), Fault::Protocol),
            ("a user code without the dash", http("200 OK", "", &user_change("BCDF-GHJK", "BCDFXGHJK")), Fault::Protocol),
            ("an interval of zero", http("200 OK", "", &user_change("\"interval\":5", "\"interval\":0")), Fault::Protocol),
            ("an interval over the cap", http("200 OK", "", &user_change("\"interval\":5", "\"interval\":61")), Fault::Protocol),
            ("a negative life", http("200 OK", "", &user_change("\"expires_in\":600", "\"expires_in\":-1")), Fault::Protocol),
            ("a life of a year", http("200 OK", "", &user_change("\"expires_in\":600", "\"expires_in\":31536000")), Fault::Protocol),
            ("an answer over the bound", http("200 OK", "", &"x".repeat(MAX_ANSWER as usize + 1)), Fault::Protocol),
            ("a redirect", http("302 Found", "Location: https://evil.example/\r\n", ""), Fault::Protocol),
            ("a server error", http("503 Service Unavailable", "", "server-words-that-must-not-be-echoed"), Fault::Protocol),
            ("a refusal", http("403 Forbidden", "", "server-words-that-must-not-be-echoed"), Fault::Refused),
            ("a closed feature", http("404 Not Found", "", "server-words-that-must-not-be-echoed"), Fault::Off),
            ("nothing at all", Vec::new(), Fault::Unreachable),
        ];
        for (what, reply, expected) in cases {
            let peer = Peer::answering(reply);
            let error = quick(&peer.origin()).start("n", "linux").expect_err(what);
            assert_eq!(error, SignInError::Fault(expected), "{what}");
            quiet(&error);
            assert!(!error.to_string().contains("server-words"), "{what}");
        }
    }

    #[test]
    fn a_peer_that_announces_a_terabyte_or_sends_nothing_is_refused_or_given_up_on() {
        let announces = Peer::answering(b"HTTP/1.1 200 OK\r\nContent-Length: 1099511627776\r\nConnection: close\r\n\r\n".to_vec());
        assert_eq!(quick(&announces.origin()).start("n", "linux").expect_err("too big"), SignInError::Fault(Fault::Protocol));
        let silent = Peer::silent();
        let began = Instant::now();
        let error = quick(&silent.origin()).start("n", "linux").expect_err("a timeout");
        assert_eq!(error, SignInError::Fault(Fault::Unreachable));
        assert!(began.elapsed() < Duration::from_secs(4), "the client gave up by its own clock");
        // Nothing is listening at all.
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        assert_eq!(quick(&format!("http://127.0.0.1:{port}")).start("n", "linux").expect_err("refused"), SignInError::Fault(Fault::Unreachable));
    }

    fn a_grant_at(peer: &Peer) -> (Client, Grant, Instant) {
        let client = quick(&peer.origin());
        let t0 = Instant::now();
        let grant = Grant {
            client: client.clone(),
            device_code: DeviceCode::parse(CODE).unwrap(),
            user_code: "BCDF-GHJK".into(),
            open_url: String::new(),
            expires_in: 600,
            wait: Wait::new(5, t0),
        };
        (client, grant, t0)
    }

    fn poll_reply(body: &str) -> Peer {
        Peer::answering(http("200 OK", "", body))
    }

    #[test]
    fn a_poll_before_the_interval_is_not_sent_and_the_states_are_a_closed_set() {
        let peer = poll_reply(r#"{"state":"pending"}"#);
        let (client, mut grant, t0) = a_grant_at(&peer);
        // Too early: answered without a request.
        assert!(matches!(client.poll_at(&mut grant, t0 + Duration::from_secs(1)).unwrap(), Poll::Pending));
        assert_eq!(peer.requests(), 0, "the client's own clock held the poll back");
        assert!(matches!(client.poll_at(&mut grant, t0 + Duration::from_secs(5)).unwrap(), Poll::Pending));
        assert_eq!(peer.requests(), 1);
        // And again too early, counted from the poll that was sent.
        assert!(matches!(client.poll_at(&mut grant, t0 + Duration::from_secs(7)).unwrap(), Poll::Pending));
        assert_eq!(peer.requests(), 1);

        for (body, expect) in [
            (r#"{"state":"denied"}"#, "denied"),
            (r#"{"state":"expired"}"#, "expired"),
            (r#"{"state":"too_many"}"#, "too_many"),
            (r#"{"state":"pending","interval":5}"#, "pending"),
        ] {
            let peer = poll_reply(body);
            let (client, mut grant, t0) = a_grant_at(&peer);
            let got = client.poll_at(&mut grant, t0 + Duration::from_secs(5)).unwrap();
            let word = match got {
                Poll::Pending => "pending",
                Poll::Denied => "denied",
                Poll::Expired => "expired",
                Poll::TooMany => "too_many",
                other => panic!("{other:?}"),
            };
            assert_eq!(word, expect);
        }
    }

    #[test]
    fn a_slow_down_makes_the_interval_longer_by_what_the_server_says_and_never_shorter() {
        let peer = poll_reply(r#"{"state":"slow_down","interval":10}"#);
        let (client, mut grant, t0) = a_grant_at(&peer);
        assert!(matches!(client.poll_at(&mut grant, t0 + Duration::from_secs(5)).unwrap(), Poll::SlowDown));
        assert_eq!(grant.interval(), 10);
        // The next poll waits the new interval from the one that was sent.
        assert!(matches!(client.poll_at(&mut grant, t0 + Duration::from_secs(12)).unwrap(), Poll::Pending));
        assert_eq!(peer.requests(), 1);
        assert!(matches!(client.poll_at(&mut grant, t0 + Duration::from_secs(15)).unwrap(), Poll::SlowDown));
        assert_eq!(peer.requests(), 2);
        assert_eq!(grant.interval(), 15, "a stated interval that is not longer still adds five seconds");
        // A server that says a shorter interval with a pending state does not shorten it.
        let peer = poll_reply(r#"{"state":"pending","interval":2}"#);
        let (client, mut grant, t0) = a_grant_at(&peer);
        client.poll_at(&mut grant, t0 + Duration::from_secs(5)).unwrap();
        assert_eq!(grant.interval(), 5);
        // The server's own throttle is a slow down too.
        let peer = Peer::answering(http("429 Too Many Requests", "Retry-After: 30\r\n", "server-words-that-must-not-be-echoed"));
        let (client, mut grant, t0) = a_grant_at(&peer);
        assert!(matches!(client.poll_at(&mut grant, t0 + Duration::from_secs(5)).unwrap(), Poll::SlowDown));
        assert_eq!(grant.interval(), 10);
    }

    #[test]
    fn an_approved_poll_gives_the_token_and_every_malformed_answer_is_a_protocol_fault() {
        let approved = format!(r#"{{"state":"approved","token":"{TOKEN}","device_id":17,"expires_at":"2027-02-10T12:00:00Z"}}"#);
        let peer = poll_reply(&approved);
        let (client, mut grant, t0) = a_grant_at(&peer);
        match client.poll_at(&mut grant, t0 + Duration::from_secs(5)).unwrap() {
            Poll::Approved { token, device_id, expires_at } => {
                assert_eq!((device_id.as_str(), expires_at.as_str()), ("17", "2027-02-10T12:00:00Z"));
                assert_eq!(token.bearer().as_str(), format!("Bearer {TOKEN}"));
                assert_eq!(format!("{token:?}"), "Token(..)");
            }
            other => panic!("{other:?}"),
        }
        let text_id = approved.replace("17", "\"abc-1\"");
        let peer = poll_reply(&text_id);
        let (client, mut grant, t0) = a_grant_at(&peer);
        assert!(matches!(client.poll_at(&mut grant, t0 + Duration::from_secs(5)).unwrap(), Poll::Approved { device_id, .. } if device_id == "abc-1"));

        let bad: Vec<(&str, String)> = vec![
            ("a token of the wrong shape", approved.replace(TOKEN, "hwd_short")),
            ("a grant code as the token", approved.replace(TOKEN, CODE)),
            ("no token", r#"{"state":"approved","device_id":17,"expires_at":"x"}"#.to_owned()),
            ("no device id", format!(r#"{{"state":"approved","token":"{TOKEN}","expires_at":"x"}}"#)),
            ("a device id with a slash", approved.replace("17", "\"a/b\"")),
            ("a device id over the bound", approved.replace("17", &format!("\"{}\"", "9".repeat(33)))),
            ("a time with a space", approved.replace("2027-02-10T12:00:00Z", "2027 02 10")),
            ("a token in a pending state", format!(r#"{{"state":"pending","token":"{TOKEN}"}}"#)),
            ("a token in a denied state", format!(r#"{{"state":"denied","token":"{TOKEN}"}}"#)),
            ("a state word outside the set", r#"{"state":"approved_maybe"}"#.to_owned()),
            ("no state", r#"{"interval":5}"#.to_owned()),
            ("an extra member", format!(r#"{{"state":"pending","note":"{CANARY}"}}"#)),
            ("an interval of zero", r#"{"state":"slow_down","interval":0}"#.to_owned()),
            ("an interval over the cap", r#"{"state":"slow_down","interval":9999}"#.to_owned()),
            ("not json", "<html>".to_owned()),
        ];
        for (what, body) in bad {
            let peer = poll_reply(&body);
            let (client, mut grant, t0) = a_grant_at(&peer);
            let error = client.poll_at(&mut grant, t0 + Duration::from_secs(5)).expect_err(what);
            assert_eq!(error, SignInError::Fault(Fault::Protocol), "{what}");
            quiet(&error);
        }
    }

    #[test]
    fn who_am_i_and_sign_out_read_the_answers_strictly_and_send_the_token_as_a_header_only() {
        let me_body = r#"{"device":{"id":17,"name":"Holz's laptop","platform":"macos","expires_at":"2027-02-10T12:00:00Z"},"switches":{"sync":true,"remote":false}}"#;
        let peer = Peer::answering(http("200 OK", "", me_body));
        let got = quick(&peer.origin()).me(&token()).expect("me");
        assert_eq!(
            got,
            Me {
                device_id: "17".into(),
                name: "Holz's laptop".into(),
                platform: "macos".into(),
                expires_at: "2027-02-10T12:00:00Z".into(),
                sync: true,
                remote: false
            }
        );
        let seen = peer.seen.lock().unwrap().clone();
        assert!(seen[0].starts_with("GET /api/device/me HTTP/1.1"), "the token is not in the target: {}", seen[0]);
        assert!(seen[0].to_ascii_lowercase().contains(&format!("authorization: bearer {TOKEN}").to_ascii_lowercase()));
        assert!(!seen[0].lines().next().unwrap().contains(CANARY));
        for (what, body) in [
            ("an extra member", me_body.replace("\"remote\":false", "\"remote\":false,\"x\":1")),
            ("a switch that is not a bool", me_body.replace("true", "\"yes\"")),
            ("no name", me_body.replace("\"name\":\"Holz's laptop\",", "")),
            ("a name with a control character", me_body.replace("Holz's laptop", "a\\u0007b")),
            ("a platform in capitals", me_body.replace("macos", "MACOS")),
        ] {
            let peer = Peer::answering(http("200 OK", "", &body));
            assert_eq!(quick(&peer.origin()).me(&token()).expect_err(what), SignInError::Fault(Fault::Protocol), "{what}");
        }
        // A token the server refuses is a sign-in that ended, on both calls.
        let peer = Peer::answering(http("401 Unauthorized", "", "server-words-that-must-not-be-echoed"));
        assert_eq!(quick(&peer.origin()).me(&token()).expect_err("401"), SignInError::Fault(Fault::SignedOut));
        assert_eq!(quick(&peer.origin()).revoke_self(&token()).expect_err("401"), SignInError::Fault(Fault::SignedOut));
        let peer = Peer::answering(http("204 No Content", "", ""));
        quick(&peer.origin()).revoke_self(&token()).expect("revoked");
        assert!(peer.seen.lock().unwrap()[0].starts_with("DELETE /api/device/me HTTP/1.1"));
        let peer = Peer::answering(http("404 Not Found", "", ""));
        assert_eq!(quick(&peer.origin()).revoke_self(&token()).expect_err("404"), SignInError::Fault(Fault::Off));
    }

    // ---- custody ----

    trait NotImplemented {
        const YES: bool = false;
    }
    impl<T> NotImplemented for T {}

    struct IsClone<T>(std::marker::PhantomData<T>);
    impl<T: Clone> IsClone<T> {
        const YES: bool = true;
    }

    struct IsSerialize<T>(std::marker::PhantomData<T>);
    impl<T: serde::Serialize> IsSerialize<T> {
        const YES: bool = true;
    }

    fn wiped_on_drop<T: zeroize::ZeroizeOnDrop>() {}

    #[test]
    fn the_device_code_is_a_secret_type_that_cannot_be_cloned_or_serialised_and_is_wiped() {
        // The probe sees what it should: a type with the traits is seen, a type without is not.
        const { assert!(IsClone::<String>::YES && IsSerialize::<String>::YES) };
        const { assert!(!IsClone::<DeviceCode>::YES && !IsSerialize::<DeviceCode>::YES) };
        const { assert!(!IsClone::<Grant>::YES && !IsSerialize::<Grant>::YES) };
        const { assert!(!IsClone::<Token>::YES) };
        wiped_on_drop::<DeviceCode>();
        wiped_on_drop::<Token>();
        let code = DeviceCode::parse(CODE).unwrap();
        assert_eq!(code.poll_body().as_slice(), format!(r#"{{"device_code":"{CODE}"}}"#).as_bytes());
        for bad in ["", "hwg_", "hwd_canaryCANARY-0123456789_abcdefghijklmnopqrs", &format!("{CODE}x"), &format!(" {CODE}"), "hwg_é23456789012345678901234567890123456789012"] {
            assert!(DeviceCode::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_platform_word_is_one_the_server_accepts() {
        assert!(PLATFORMS.contains(&platform()));
    }
}
