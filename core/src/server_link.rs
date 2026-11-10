//! The link of this device to a holzWave server: the address and device id it keeps in `server_link.json`, the credential it
//! holds in the keychain, and the faults the link can have.
//!
//! A device signs in to a server once and keeps one credential, the device token. The token is a secret in the full sense:
//! it lives only in the operating system's keychain, only its SHA-256 is on the server, and in this process it is a
//! [`Token`], a type that is wiped when dropped, prints a marker instead of its text, and can be neither cloned nor
//! serialised, so no log line, `Debug` print, error text or file can carry it by accident. The one accessor that shows its text
//! builds the `Authorization` header value.
//!
//! [`Fault`] is the closed set of things that can go wrong with a link. Every case has one fixed sentence and none of them
//! holds text a server wrote: the server and the network are untrusted input, and what they say is never shown or logged.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::persist::{read_limited, write_atomic};
use crate::remote::ws::{UrlError, WsUrl};
use crate::secrets::SecretStore;
use crate::Error;

/// What a device token starts with (the server's `DEVICE_PREFIX`).
pub const TOKEN_PREFIX: &str = "hwd_";

/// The characters after the prefix: 32 random bytes written as base64url without padding.
pub const TOKEN_BODY_LEN: usize = 43;

/// What the `Debug` and `Display` text of a token print.
const TOKEN_MARKER: &str = "Token(..)";

/// The device token of this device, as the server issued it.
///
/// Not `Clone`, not `Serialize`, wiped on drop. It is only made from text that has exactly the shape of a token, which also
/// keeps anything that is not a plain header value out of a request.
pub struct Token(Zeroizing<String>);

impl Token {
    /// Reads a token. Anything but the prefix and exactly 43 characters of the base64url alphabet is refused, and the refusal
    /// is [`Fault::Protocol`] without the text.
    pub fn parse(text: &str) -> Result<Token, Fault> {
        let body = text.strip_prefix(TOKEN_PREFIX).ok_or(Fault::Protocol)?;
        if body.len() != TOKEN_BODY_LEN || !body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(Fault::Protocol);
        }
        Ok(Token(Zeroizing::new(text.to_owned())))
    }

    /// The value of the `Authorization` header (`Bearer <token>`), wiped when dropped. Call it only to build a request.
    pub fn bearer(&self) -> Zeroizing<String> {
        let mut value = Zeroizing::new(String::with_capacity(7 + self.0.len()));
        value.push_str("Bearer ");
        value.push_str(&self.0);
        value
    }
}

impl zeroize::ZeroizeOnDrop for Token {}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(TOKEN_MARKER)
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(TOKEN_MARKER)
    }
}

/// What can go wrong with the link to a server. A closed set: a fault says what kind of thing it was and nothing the server
/// wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The server does not accept the token any more (revoked, expired or never issued): the person signs in again.
    SignedOut,
    /// The server has the feature switched off (or does not have it at all).
    Off,
    /// The server could not be reached: no connection, no answer in time, or a connection that broke.
    Unreachable,
    /// The server answered, but refused: any other status than the two above.
    Refused,
    /// The server sent something this client does not accept: a frame, an answer or a text outside the rules.
    Protocol,
}

impl Fault {
    /// Every case, in the order of the declaration.
    pub const ALL: [Fault; 5] = [Fault::SignedOut, Fault::Off, Fault::Unreachable, Fault::Refused, Fault::Protocol];

    /// The one fixed sentence of this fault.
    pub fn sentence(self) -> &'static str {
        match self {
            Fault::SignedOut => "The server ended this sign-in. Sign in again.",
            Fault::Off => "This feature is off on the server.",
            Fault::Unreachable => "The server cannot be reached.",
            Fault::Refused => "The server refused the request.",
            Fault::Protocol => "The server sent something holzWave does not accept.",
        }
    }

    /// The fault of the status of an answer that is neither a success nor a case the caller names itself: 401 is a sign-in
    /// that ended, 404 a feature that is off, anything else a refusal.
    pub fn of_status(status: u16) -> Fault {
        match status {
            401 => Fault::SignedOut,
            404 => Fault::Off,
            _ => Fault::Refused,
        }
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.sentence())
    }
}

impl std::error::Error for Fault {}

impl From<Fault> for Error {
    /// The fault as the crate's error, with exactly the fixed sentence as its text (the same shape the WebDAV transport uses
    /// for its own sentences, so a caller that shows `to_string()` shows the sentence).
    fn from(fault: Fault) -> Error {
        Error::Io(std::io::Error::other(fault.sentence()))
    }
}

/// The address of a server: its origin, `scheme://host[:port]` and nothing after it. No user name, path, query or fragment.
///
/// [`Origin::parse`] accepts `https`, and `http` for a loopback host only (a server on this computer, or a test); the file of
/// the link ([`Origin::parse_https`]) accepts `https` alone. The rules of the host and the port are the WebSocket address's,
/// so the two cannot disagree about what a server's address is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    secure: bool,
    /// The host, an IPv6 address without brackets.
    host: String,
    port: u16,
}

impl Origin {
    /// An `https` origin, or an `http` one for a loopback host. A single trailing slash is allowed and dropped.
    pub fn parse(text: &str) -> Result<Origin, UrlError> {
        Origin::parse_with(text, true)
    }

    /// An `https` origin and nothing else.
    pub fn parse_https(text: &str) -> Result<Origin, UrlError> {
        Origin::parse_with(text, false)
    }

    fn parse_with(text: &str, loopback_http: bool) -> Result<Origin, UrlError> {
        let (scheme, rest) = text.split_once("://").ok_or(UrlError::Scheme)?;
        let ws_scheme = match scheme {
            "https" => "wss",
            "http" if loopback_http => "ws",
            _ => return Err(UrlError::Scheme),
        };
        let url = WsUrl::parse(&format!("{ws_scheme}://{rest}"))?;
        // An origin ends after the authority: the one path a WebSocket address always has is `/`, and it is all that is allowed.
        if rest.find('/').is_some_and(|i| &rest[i..] != "/") {
            return Err(UrlError::Path);
        }
        Ok(Origin { secure: url.secure(), host: url.host().to_owned(), port: url.port() })
    }

    /// `https` (true) or a loopback `http` (false).
    pub fn secure(&self) -> bool {
        self.secure
    }

    /// The host, an IPv6 address without brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The canonical text: `scheme://host[:port]`, the port only when it is not the default of the scheme, an IPv6 host in
    /// brackets, no trailing slash.
    pub fn to_text(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let port = if self.port == if self.secure { 443 } else { 80 } { String::new() } else { format!(":{}", self.port) };
        format!("{}://{host}{port}", if self.secure { "https" } else { "http" })
    }

    /// The WebSocket address of `path` on this server (`wss` for `https`, `ws` for the loopback `http`).
    pub fn socket_url(&self, path: &str) -> Result<WsUrl, UrlError> {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        WsUrl::parse(&format!("{}://{host}:{}{path}", if self.secure { "wss" } else { "ws" }, self.port))
    }
}

/// The file name inside the app data directory.
pub const FILE: &str = "server_link.json";
/// The format version that is written.
pub const VERSION: u32 = 1;
/// The largest `server_link.json` that is read. The file is a few hundred bytes.
pub const MAX_BYTES: u64 = 4096;
/// The longest device name, in characters.
pub const MAX_NAME: usize = 64;
/// The longest device id, in bytes.
pub const MAX_DEVICE_ID: usize = 32;
/// The keychain account the token lives under. The service name is the one of every holzWave entry.
pub const TOKEN_ACCOUNT: &str = "server:token";

/// Why `server_link.json` was refused. A closed set with fixed sentences: none of them repeats a value of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkFileError {
    /// The file cannot be read (it is not a regular file, or the system refused).
    Unreadable,
    /// The file is larger than [`MAX_BYTES`]; it was not read.
    TooLarge,
    /// The file is not text, not JSON, not an object of the known members, or a member has the wrong type.
    Damaged,
    /// The file is of a format version this build does not read (or names none).
    Version,
    /// The server address is not an `https` origin.
    Url,
    /// The device id is empty, too long or has a character outside letters, digits, `-` and `_`.
    DeviceId,
    /// The device name is empty, too long or has a control character.
    Name,
    /// The file could not be written; nothing was changed.
    NotWritten,
}

impl LinkFileError {
    pub fn sentence(self) -> &'static str {
        match self {
            LinkFileError::Unreadable => "The server link file cannot be read.",
            LinkFileError::TooLarge => "The server link file is larger than holzWave reads.",
            LinkFileError::Damaged => "The server link file is not valid.",
            LinkFileError::Version => "The server link file is of a version this holzWave does not read.",
            LinkFileError::Url => "The server address in the link file is not a valid https address.",
            LinkFileError::DeviceId => "The device id in the link file is not valid.",
            LinkFileError::Name => "The device name in the link file is not valid.",
            LinkFileError::NotWritten => "The server link file could not be written.",
        }
    }
}

impl fmt::Display for LinkFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.sentence())
    }
}

impl std::error::Error for LinkFileError {}

impl From<LinkFileError> for Error {
    fn from(e: LinkFileError) -> Error {
        Error::Io(std::io::Error::other(e.sentence()))
    }
}

/// The device id of the file as it is read: the server's id is a number, and a text is accepted as well.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawId {
    Text(String),
    Number(u64),
}

/// The members of `server_link.json` as they are read; an unknown member refuses the file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[allow(dead_code)]
    version: u32,
    url: String,
    device_id: RawId,
    name: String,
}

/// The members as they are written, in this order.
#[derive(Serialize)]
struct Out<'a> {
    version: u32,
    url: &'a str,
    device_id: &'a str,
    name: &'a str,
}

/// `server_link.json`: which server this device is linked to, and which device it is there.
///
/// The file is a fact of **this device** and is never synced (`settings.json` travels to every computer; a link to a server,
/// and the id of the device on it, must not): it sits beside `host_shell.json` and `window_frame.json`. One server per
/// device, so it holds one link. It holds no secret: the token is in the keychain ([`token_save`]). A value read from the
/// file is data and is never echoed into a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerLinkFile {
    url: String,
    device_id: String,
    name: String,
}

impl ServerLinkFile {
    /// A link to the server at `url` (an `https` origin, no path) as the device `device_id` called `name`. The address is
    /// kept in its canonical form.
    pub fn new(url: &str, device_id: &str, name: &str) -> Result<ServerLinkFile, LinkFileError> {
        ServerLinkFile::new_with(url, device_id, name, false)
    }

    /// [`ServerLinkFile::new`], and with `loopback_http` an `http` origin of a loopback host is accepted too. That is a server
    /// on this computer, which no product configuration has (a server's own address is `https`); it exists for the tests that
    /// run a real server on the loopback address, and the app never asks for it.
    pub fn new_with(url: &str, device_id: &str, name: &str, loopback_http: bool) -> Result<ServerLinkFile, LinkFileError> {
        let origin = if loopback_http { Origin::parse(url) } else { Origin::parse_https(url) }.map_err(|_| LinkFileError::Url)?;
        let id_ok = !device_id.is_empty()
            && device_id.len() <= MAX_DEVICE_ID
            && device_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !id_ok {
            return Err(LinkFileError::DeviceId);
        }
        let name_ok = !name.is_empty() && name.chars().count() <= MAX_NAME && !name.chars().any(char::is_control);
        if !name_ok {
            return Err(LinkFileError::Name);
        }
        Ok(ServerLinkFile { url: origin.to_text(), device_id: device_id.to_owned(), name: name.to_owned() })
    }

    /// The format version that is written.
    pub fn version(&self) -> u32 {
        VERSION
    }

    /// The server's address, `https://host[:port]`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The server's address as an [`Origin`].
    pub fn origin(&self) -> Result<Origin, UrlError> {
        // The address was checked when the link was made (`https`, or a loopback `http` where that was asked for).
        Origin::parse(&self.url)
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Reads the document: strict, so a file that is not exactly what this build writes is refused. A version other than the
    /// current one is [`LinkFileError::Version`] whatever else the file holds.
    pub fn from_json(text: &str) -> Result<ServerLinkFile, LinkFileError> {
        ServerLinkFile::from_json_with(text, false)
    }

    /// [`ServerLinkFile::from_json`] with the choice of [`ServerLinkFile::new_with`].
    pub fn from_json_with(text: &str, loopback_http: bool) -> Result<ServerLinkFile, LinkFileError> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|_| LinkFileError::Damaged)?;
        let serde_json::Value::Object(object) = &value else { return Err(LinkFileError::Damaged) };
        if object.get("version").and_then(serde_json::Value::as_u64) != Some(u64::from(VERSION)) {
            return Err(LinkFileError::Version);
        }
        let raw: Raw = serde_json::from_value(value).map_err(|_| LinkFileError::Damaged)?;
        let id = match raw.device_id {
            RawId::Text(text) => text,
            RawId::Number(number) => number.to_string(),
        };
        ServerLinkFile::new_with(&raw.url, &id, &raw.name, loopback_http)
    }

    /// The document as pretty printed JSON with a trailing newline.
    pub fn to_json(&self) -> String {
        let out = Out { version: VERSION, url: &self.url, device_id: &self.device_id, name: &self.name };
        let mut text = serde_json::to_string_pretty(&out).unwrap_or_default();
        text.push('\n');
        text
    }

    /// Reads `<dir>/server_link.json`. A missing file is no link (`Ok(None)`). A file that cannot be used is refused with the
    /// sentence of its fault and is left exactly as it is: the person's next sign-in replaces it.
    pub fn load(dir: &Path) -> Result<Option<ServerLinkFile>, LinkFileError> {
        ServerLinkFile::load_with(dir, false)
    }

    /// [`ServerLinkFile::load`] with the choice of [`ServerLinkFile::new_with`].
    pub fn load_with(dir: &Path, loopback_http: bool) -> Result<Option<ServerLinkFile>, LinkFileError> {
        let bytes = match read_limited(&dir.join(FILE), MAX_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => return Err(LinkFileError::TooLarge),
            Err(_) => return Err(LinkFileError::Unreadable),
        };
        let text = String::from_utf8(bytes).map_err(|_| LinkFileError::Damaged)?;
        ServerLinkFile::from_json_with(&text, loopback_http).map(Some)
    }

    /// Writes `<dir>/server_link.json` atomically (the parent directory is created): the old file or the new one, never a
    /// half. A failure changes nothing.
    pub fn save(&self, dir: &Path) -> Result<(), LinkFileError> {
        write_atomic(&dir.join(FILE), self.to_json().as_bytes()).map_err(|_| LinkFileError::NotWritten)
    }

    /// Removes `<dir>/server_link.json`. A file that is not there is not an error.
    pub fn clear(dir: &Path) -> Result<(), LinkFileError> {
        match std::fs::remove_file(dir.join(FILE)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(LinkFileError::NotWritten),
        }
    }
}

/// The token of this device from the keychain, `None` when there is none. A value under the account that is not a token (a
/// damaged entry, something else written there) is no token either: the person signs in again, and the value is not shown.
pub fn token_load(store: &dyn SecretStore) -> crate::Result<Option<Token>> {
    let Some(text) = store.get(TOKEN_ACCOUNT)? else { return Ok(None) };
    let text = Zeroizing::new(text);
    Ok(Token::parse(&text).ok())
}

/// Puts the token into the keychain under [`TOKEN_ACCOUNT`], replacing an older one. The token goes nowhere else.
pub fn token_save(store: &dyn SecretStore, token: &Token) -> crate::Result<()> {
    store.set(TOKEN_ACCOUNT, &token.0)
}

/// Removes the token from the keychain. This is what signing out does: the file of the link stays, so the next sign-in knows
/// which server it is for. Removing a token that is not there is not an error.
pub fn token_clear(store: &dyn SecretStore) -> crate::Result<()> {
    store.delete(TOKEN_ACCOUNT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

    #[test]
    fn an_origin_is_an_authority_and_nothing_after_it() {
        let o = Origin::parse("https://sync.example").unwrap();
        assert_eq!((o.secure(), o.host(), o.port(), o.to_text().as_str()), (true, "sync.example", 443, "https://sync.example"));
        assert_eq!(Origin::parse("https://sync.example/").unwrap(), o, "one trailing slash is dropped");
        assert_eq!(Origin::parse("https://sync.example:8443").unwrap().to_text(), "https://sync.example:8443");
        assert_eq!(Origin::parse("https://sync.example:443").unwrap().to_text(), "https://sync.example");
        assert_eq!(Origin::parse("http://[::1]:8080").unwrap().to_text(), "http://[::1]:8080");
        assert_eq!(Origin::parse("http://127.0.0.1").unwrap().to_text(), "http://127.0.0.1");
        for bad in ["https://sync.example/sync", "https://sync.example//", "https://sync.example/?x=1", "https://u:p@sync.example", "ftp://x", "wss://x", ""] {
            assert!(Origin::parse(bad).is_err(), "{bad}");
        }
        // The file of the link takes https alone.
        assert!(Origin::parse_https("https://sync.example").is_ok());
        assert_eq!(Origin::parse_https("http://127.0.0.1:8080"), Err(UrlError::Scheme));
        assert_eq!(Origin::parse("http://sync.example"), Err(UrlError::Insecure));
    }

    #[test]
    fn the_socket_address_of_an_origin_follows_its_scheme() {
        let secure = Origin::parse("https://sync.example:8443").unwrap().socket_url("/ws/device").unwrap();
        assert_eq!((secure.secure(), secure.host(), secure.port(), secure.path()), (true, "sync.example", 8443, "/ws/device"));
        let plain = Origin::parse("http://127.0.0.1:9000").unwrap().socket_url("/ws/device").unwrap();
        assert_eq!((plain.secure(), plain.port()), (false, 9000));
        let v6 = Origin::parse("https://[2001:db8::1]").unwrap().socket_url("/ws/device").unwrap();
        assert_eq!((v6.host(), v6.port()), ("2001:db8::1", 443));
    }

    // ---- the closed set of faults ----

    #[test]
    fn a_fault_is_one_of_five_cases_each_with_one_fixed_sentence() {
        // An exhaustive match: a sixth case does not compile until this test names it.
        fn index(fault: Fault) -> usize {
            match fault {
                Fault::SignedOut => 0,
                Fault::Off => 1,
                Fault::Unreachable => 2,
                Fault::Refused => 3,
                Fault::Protocol => 4,
            }
        }
        assert_eq!(Fault::ALL.len(), 5);
        for (i, fault) in Fault::ALL.into_iter().enumerate() {
            assert_eq!(index(fault), i, "ALL is in the order of the declaration");
        }
        let sentences: Vec<&str> = Fault::ALL.iter().map(|f| f.sentence()).collect();
        assert_eq!(
            sentences,
            [
                "The server ended this sign-in. Sign in again.",
                "This feature is off on the server.",
                "The server cannot be reached.",
                "The server refused the request.",
                "The server sent something holzWave does not accept.",
            ]
        );
        let mut unique = sentences.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 5, "no two faults share a sentence");
        for fault in Fault::ALL {
            // No server-provided text: the sentence is a constant, and the display and the crate error say exactly it.
            assert_eq!(fault.to_string(), fault.sentence());
            assert_eq!(Error::from(fault).to_string(), fault.sentence());
            assert!(!format!("{fault:?}").to_lowercase().contains("canary"));
        }
        assert_eq!((Fault::of_status(401), Fault::of_status(404), Fault::of_status(403), Fault::of_status(500)), (Fault::SignedOut, Fault::Off, Fault::Refused, Fault::Refused));
    }

    // ---- the token ----

    const TOKEN: &str = "hwd_canaryCANARY-0123456789_abcdefghijklmnopqrs";
    /// The part of the token that is random in a real one; no byte run of it may be in a file.
    const CANARY: &str = "canaryCANARY-0123456789_abcdefghijklmnopqrs";

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
    fn a_token_prints_a_marker_is_wiped_on_drop_and_can_be_neither_cloned_nor_serialised() {
        // The probe sees what it should: a type with the traits is seen, a type without is not.
        const { assert!(IsClone::<String>::YES && IsSerialize::<String>::YES, "the probe works") };
        const { assert!(!IsClone::<Token>::YES, "a token cannot be cloned") };
        const { assert!(!IsSerialize::<Token>::YES, "a token cannot be serialised") };
        wiped_on_drop::<Token>();

        let token = Token::parse(TOKEN).expect("a token");
        assert_eq!(format!("{token:?}"), "Token(..)");
        assert_eq!(format!("{token}"), "Token(..)");
        assert_eq!(format!("{token:#?}"), "Token(..)");
        assert_eq!(token.bearer().as_str(), format!("Bearer {TOKEN}"));
        // Only the shape of a token is a token.
        let body = &TOKEN[4..];
        for bad in [
            "".to_owned(),
            TOKEN.to_owned() + "x",
            TOKEN[..TOKEN.len() - 1].to_owned(),
            format!("hwg_{body}"),
            format!("HWD_{body}"),
            format!("hwd_{}", body.replace('-', "+")),
            format!("hwd_{}\n", &body[..42]),
            format!("hwd_{} ", &body[..42]),
            format!(" {TOKEN}"),
            format!("hwd_{}é", &body[..41]),
        ] {
            assert!(Token::parse(&bad).is_err(), "{bad:?}");
        }
        assert!(matches!(Token::parse("hwd_short"), Err(Fault::Protocol)));
    }

    #[test]
    fn the_token_is_saved_read_back_and_cleared_under_its_own_account() {
        let store = MemoryStore::default();
        assert!(token_load(&store).unwrap().is_none());
        let token = Token::parse(TOKEN).unwrap();
        token_save(&store, &token).unwrap();
        assert_eq!(TOKEN_ACCOUNT, "server:token");
        assert_eq!(store.get("server:token").unwrap().as_deref(), Some(TOKEN), "the account is the one the plan names");
        assert_eq!(token_load(&store).unwrap().unwrap().bearer().as_str(), format!("Bearer {TOKEN}"));
        // A second token replaces the first.
        let second = format!("hwd_{}", "B".repeat(43));
        token_save(&store, &Token::parse(&second).unwrap()).unwrap();
        assert_eq!(store.get("server:token").unwrap().as_deref(), Some(second.as_str()));
        token_clear(&store).unwrap();
        token_clear(&store).unwrap();
        assert!(token_load(&store).unwrap().is_none());
        // Something that is not a token under the account is no token, and is not shown.
        store.set("server:token", "not a token with a canary-secret").unwrap();
        assert!(token_load(&store).unwrap().is_none());
    }

    // ---- the file ----

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hw-server-link-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn link() -> ServerLinkFile {
        ServerLinkFile::new("https://sync.example:8443", "17", "Holz's laptop").unwrap()
    }

    #[test]
    fn the_link_file_round_trips_and_a_missing_file_is_no_link() {
        let dir = scratch("round");
        assert_eq!(ServerLinkFile::load(&dir), Ok(None), "a missing file is no link");
        let link = link();
        link.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert_eq!(
            text,
            "{\n  \"version\": 1,\n  \"url\": \"https://sync.example:8443\",\n  \"device_id\": \"17\",\n  \"name\": \"Holz's laptop\"\n}\n"
        );
        let back = ServerLinkFile::load(&dir).unwrap().unwrap();
        assert_eq!(back, link);
        assert_eq!((back.version(), back.url(), back.device_id(), back.name()), (1, "https://sync.example:8443", "17", "Holz's laptop"));
        assert_eq!(back.origin().unwrap().port(), 8443);
        // The id may be a number in a file (the server's id is one); it is read as its text.
        let numeric = r#"{"version":1,"url":"https://sync.example","device_id":42,"name":"n"}"#;
        assert_eq!(ServerLinkFile::from_json(numeric).unwrap().device_id(), "42");
        // A trailing slash on the address is accepted and dropped.
        let slash = r#"{"version":1,"url":"https://sync.example/","device_id":"a","name":"n"}"#;
        assert_eq!(ServerLinkFile::from_json(slash).unwrap().url(), "https://sync.example");
        // A second save replaces the file whole, with no leftover.
        let other = ServerLinkFile::new("https://other.example", "18", "phone").unwrap();
        other.save(&dir).unwrap();
        assert_eq!(ServerLinkFile::load(&dir).unwrap().unwrap(), other);
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, [FILE]);
        ServerLinkFile::clear(&dir).unwrap();
        ServerLinkFile::clear(&dir).unwrap();
        assert_eq!(ServerLinkFile::load(&dir), Ok(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_link_file_that_is_not_exactly_what_is_written_is_refused_with_a_fixed_sentence_and_left_alone() {
        let dir = scratch("refuse");
        let big_name = "n".repeat(65);
        let cases: Vec<(String, LinkFileError)> = vec![
            (r#"{"version":2,"url":"https://s.example","device_id":"1","name":"n"}"#.into(), LinkFileError::Version),
            (r#"{"url":"https://s.example","device_id":"1","name":"n"}"#.into(), LinkFileError::Version),
            (r#"{"version":"1","url":"https://s.example","device_id":"1","name":"n"}"#.into(), LinkFileError::Version),
            (r#"{"version":1,"url":"https://s.example","device_id":"1","name":"n","extra":true}"#.into(), LinkFileError::Damaged),
            (r#"{"version":1,"url":"https://s.example","device_id":"1"}"#.into(), LinkFileError::Damaged),
            (r#"{"version":1,"url":7,"device_id":"1","name":"n"}"#.into(), LinkFileError::Damaged),
            (r#"{"version":1,"url":"http://s.example","device_id":"1","name":"n"}"#.into(), LinkFileError::Url),
            (r#"{"version":1,"url":"http://127.0.0.1:8080","device_id":"1","name":"n"}"#.into(), LinkFileError::Url),
            (r#"{"version":1,"url":"https://s.example/path","device_id":"1","name":"n"}"#.into(), LinkFileError::Url),
            (r#"{"version":1,"url":"https://s.example?q=1","device_id":"1","name":"n"}"#.into(), LinkFileError::Url),
            (r#"{"version":1,"url":"https://user:pw@s.example","device_id":"1","name":"n"}"#.into(), LinkFileError::Url),
            (r#"{"version":1,"url":"https://s.example#f","device_id":"1","name":"n"}"#.into(), LinkFileError::Url),
            (r#"{"version":1,"url":"","device_id":"1","name":"n"}"#.into(), LinkFileError::Url),
            (r#"{"version":1,"url":"https://s.example","device_id":"","name":"n"}"#.into(), LinkFileError::DeviceId),
            (r#"{"version":1,"url":"https://s.example","device_id":"a b","name":"n"}"#.into(), LinkFileError::DeviceId),
            (format!(r#"{{"version":1,"url":"https://s.example","device_id":"{}","name":"n"}}"#, "9".repeat(33)), LinkFileError::DeviceId),
            (format!(r#"{{"version":1,"url":"https://s.example","device_id":"1","name":"{big_name}"}}"#), LinkFileError::Name),
            (r#"{"version":1,"url":"https://s.example","device_id":"1","name":""}"#.into(), LinkFileError::Name),
            (r#"{"version":1,"url":"https://s.example","device_id":"1","name":"a\nb"}"#.into(), LinkFileError::Name),
            ("not json".into(), LinkFileError::Damaged),
            ("[]".into(), LinkFileError::Damaged),
            ("".into(), LinkFileError::Damaged),
        ];
        for (text, expected) in &cases {
            std::fs::write(dir.join(FILE), text).unwrap();
            assert_eq!(ServerLinkFile::load(&dir), Err(*expected), "{text}");
            assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), *text, "a refused file is left exactly as it was");
        }
        // The name of 64 characters is the longest, counted in characters and not bytes.
        assert!(ServerLinkFile::new("https://s.example", "1", &"é".repeat(64)).is_ok());
        assert!(ServerLinkFile::new("https://s.example", "1", &"é".repeat(65)).is_err());
        // Bigger than the bound: refused unread. Not UTF-8: damaged.
        std::fs::write(dir.join(FILE), vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert_eq!(ServerLinkFile::load(&dir), Err(LinkFileError::TooLarge));
        std::fs::write(dir.join(FILE), [0xff, 0xfe]).unwrap();
        assert_eq!(ServerLinkFile::load(&dir), Err(LinkFileError::Damaged));
        // A directory where the file should be is unreadable, not a link.
        std::fs::remove_file(dir.join(FILE)).unwrap();
        std::fs::create_dir(dir.join(FILE)).unwrap();
        assert_eq!(ServerLinkFile::load(&dir), Err(LinkFileError::Unreadable));
        std::fs::remove_dir(dir.join(FILE)).unwrap();
        // The sentences are fixed and hold no value of the file.
        let all = [
            LinkFileError::Unreadable,
            LinkFileError::TooLarge,
            LinkFileError::Damaged,
            LinkFileError::Version,
            LinkFileError::Url,
            LinkFileError::DeviceId,
            LinkFileError::Name,
            LinkFileError::NotWritten,
        ];
        let mut sentences: Vec<_> = all.iter().map(|e| e.sentence()).collect();
        sentences.sort_unstable();
        sentences.dedup();
        assert_eq!(sentences.len(), all.len());
        for e in all {
            assert_eq!(Error::from(e).to_string(), e.sentence());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_link_that_cannot_be_made_writes_nothing_and_a_failed_write_leaves_the_old_file() {
        let dir = scratch("nothing");
        assert_eq!(ServerLinkFile::new("http://s.example", "1", "n"), Err(LinkFileError::Url));
        assert_eq!(ServerLinkFile::new("https://s.example/x", "1", "n"), Err(LinkFileError::Url));
        assert_eq!(ServerLinkFile::new("https://s.example", "a/b", "n"), Err(LinkFileError::DeviceId));
        assert_eq!(ServerLinkFile::new("https://s.example", "1", &"n".repeat(65)), Err(LinkFileError::Name));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "nothing was written");
        // A save that cannot happen (the folder is a file) changes nothing and says so in its fixed sentence.
        link().save(&dir).unwrap();
        let blocked = dir.join(FILE).join("inside");
        assert_eq!(link().save(&blocked), Err(LinkFileError::NotWritten));
        assert_eq!(ServerLinkFile::load(&dir).unwrap().unwrap(), link());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn signing_out_clears_the_token_only_and_the_file_stays() {
        let dir = scratch("signout");
        let store = MemoryStore::default();
        link().save(&dir).unwrap();
        token_save(&store, &Token::parse(TOKEN).unwrap()).unwrap();
        token_clear(&store).unwrap();
        assert!(token_load(&store).unwrap().is_none());
        assert_eq!(ServerLinkFile::load(&dir).unwrap().unwrap(), link(), "the link stays, so the next sign-in knows the server");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_byte_of_the_token_is_in_any_file_a_run_writes() {
        let dir = scratch("canary");
        let store = MemoryStore::default();
        let token = Token::parse(TOKEN).unwrap();
        // A whole life of the link: set up, sign in, read back, fail, sign out, link again.
        link().save(&dir).unwrap();
        token_save(&store, &token).unwrap();
        let loaded = token_load(&store).unwrap().unwrap();
        let _ = ServerLinkFile::load(&dir).unwrap();
        let _ = format!("{token:?} {token} {loaded:?} {:?}", link());
        let _ = ServerLinkFile::new("http://s.example", "1", "n");
        token_clear(&store).unwrap();
        link().save(&dir).unwrap();
        // Everything under the scratch directory, whatever its name.
        let mut seen = Vec::new();
        let mut stack = vec![dir.clone()];
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).unwrap() {
                let entry = entry.unwrap().path();
                if entry.is_dir() {
                    stack.push(entry);
                } else {
                    let bytes = std::fs::read(&entry).unwrap();
                    let text = String::from_utf8_lossy(&bytes);
                    assert!(!text.contains(CANARY) && !text.contains("hwd_"), "the token is in {}", entry.display());
                    seen.push(entry.file_name().unwrap().to_string_lossy().into_owned());
                }
            }
        }
        seen.sort();
        assert_eq!(seen, ["server_link.json"], "no other file, no temporary file");
        assert_eq!(store.get("server:token").unwrap(), None, "and signing out left nothing in the keychain stand-in");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_loopback_http_address_is_a_link_only_where_it_is_asked_for() {
        let dir = scratch("loopback");
        assert_eq!(ServerLinkFile::new("http://127.0.0.1:8080", "1", "n"), Err(LinkFileError::Url));
        let link = ServerLinkFile::new_with("http://127.0.0.1:8080", "1", "n", true).unwrap();
        assert_eq!((link.url(), link.origin().unwrap().secure()), ("http://127.0.0.1:8080", false));
        link.save(&dir).unwrap();
        assert_eq!(ServerLinkFile::load(&dir), Err(LinkFileError::Url), "the product reads https alone");
        assert_eq!(ServerLinkFile::load_with(&dir, true).unwrap().unwrap(), link);
        // Even where it is asked for, a plain address of another host is refused, and so is a path.
        assert_eq!(ServerLinkFile::new_with("http://sync.example", "1", "n", true), Err(LinkFileError::Url));
        assert_eq!(ServerLinkFile::new_with("http://127.0.0.1:8080/x", "1", "n", true), Err(LinkFileError::Url));
        assert!(ServerLinkFile::new_with("https://sync.example", "1", "n", true).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
