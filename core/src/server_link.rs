//! The link of this device to a holzWave server: the credential it holds and the faults the link can have.
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

use zeroize::Zeroizing;

use crate::Error;
use crate::remote::ws::{UrlError, WsUrl};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
