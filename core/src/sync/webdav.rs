//! WebDAV, the transport of the sync (Nextcloud and anything else that speaks it): the few verbs the sync needs, and
//! nothing of its own on the server. `https` is required except for a loopback address, redirects are not followed (so
//! the app password is never sent to another host), every answer is bounded, and a server that answers wrongly is an
//! error, never a panic.

use std::io::{self, Read};
use std::time::Duration;

use quick_xml::Reader;
use quick_xml::events::Event;
use ureq::config::Config;
use ureq::http::{Request, StatusCode};
use ureq::tls::{RootCerts, TlsConfig};
use ureq::Agent;

use super::crypto::MAX_BLOB;
use crate::{Error, Result};

/// A condition on a write, so two devices cannot overwrite each other unseen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cond {
    /// The name must not exist yet (`If-None-Match: *`).
    Absent,
    /// The name must still have this ETag (`If-Match`).
    Match(String),
    /// Overwrite whatever is there.
    Any,
}

/// The reason a conditional write was refused: somebody else wrote first.
pub const CONFLICT: &str = "precondition failed";

/// One entry of a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub etag: String,
}

/// What the sync needs from a server. `path` is relative to the sync folder and made of safe segments.
pub trait Backend: Send {
    /// Create the folder (and its parents) if it is missing.
    fn ensure(&self, dir: &str) -> Result<()>;
    /// The files of `dir`, with their ETags; an absent folder is empty.
    fn list(&self, dir: &str) -> Result<Vec<Entry>>;
    /// The content and ETag of `path`, `None` if it does not exist.
    fn get(&self, path: &str) -> Result<Option<(Vec<u8>, String)>>;
    /// Write `bytes` if `cond` holds; the new ETag. A failed condition is an error whose text is [`CONFLICT`].
    fn put(&self, path: &str, bytes: &[u8], cond: &Cond) -> Result<String>;
}

fn err(m: impl Into<String>) -> Error {
    Error::Io(io::Error::other(m.into()))
}

pub struct WebDav {
    agent: Agent,
    base: String,
    auth: String,
}

/// Base64 (RFC 4648) for the `Authorization` header.
fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn is_loopback(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h == "localhost" || h == "127.0.0.1" || h == "::1"
}

/// A path segment is letters, digits and `._-` only: nothing a server or a file name can use to leave the folder.
pub fn safe_segment(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn safe_path(path: &str) -> Result<()> {
    if path.is_empty() || !path.split('/').all(safe_segment) {
        return Err(err(format!("unsafe sync path {path:?}")));
    }
    Ok(())
}

impl WebDav {
    /// `endpoint` is the folder's URL (for Nextcloud `https://host/remote.php/dav/files/<user>/holzwave`); the user and
    /// the app password are the account's.
    pub fn new(endpoint: &str, user: &str, password: &str) -> Result<Self> {
        let (scheme, rest) = endpoint.split_once("://").ok_or_else(|| err("the endpoint is not a URL"))?;
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host_only = host.rsplit('@').next().unwrap_or("");
        let name = if host_only.starts_with('[') {
            host_only.split(']').next().unwrap_or("")
        } else {
            host_only.split(':').next().unwrap_or("")
        };
        if host.contains('@') || name.is_empty() {
            return Err(err("the endpoint must not carry a user name or password; they are asked for separately"));
        }
        match scheme {
            "https" => {}
            "http" if is_loopback(name) => {}
            _ => return Err(err("the endpoint must be an https address")),
        }
        if endpoint.contains(['?', '#']) || user.is_empty() || user.len() > 256 || password.len() > 1024 {
            return Err(err("the endpoint or the account is not valid"));
        }
        let config: Config = Config::builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .allow_non_standard_methods(true)
            .https_only(!is_loopback(name))
            .timeout_global(Some(Duration::from_secs(60)))
            .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
            .user_agent("holzWave-sync")
            .build();
        Ok(Self {
            agent: config.into(),
            base: endpoint.trim_end_matches('/').to_owned(),
            auth: format!("Basic {}", base64(format!("{user}:{password}").as_bytes())),
        })
    }

    fn url(&self, path: &str) -> String {
        if path.is_empty() { self.base.clone() } else { format!("{}/{path}", self.base) }
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &[u8],
        limit: u64,
    ) -> Result<(StatusCode, Option<String>, Vec<u8>)> {
        let mut req = Request::builder().method(method).uri(self.url(path)).header("Authorization", &self.auth);
        for (k, v) in headers {
            req = req.header(*k, v.as_str());
        }
        let req = req.body(body.to_vec()).map_err(|e| err(format!("request: {e}")))?;
        let mut resp = self.agent.run(req).map_err(|e| err(format!("{method} {path}: {e}")))?;
        let status = resp.status();
        let etag = resp.headers().get("ETag").or_else(|| resp.headers().get("OC-ETag")).and_then(|v| v.to_str().ok()).map(clean_etag);
        let mut bytes = Vec::new();
        resp.body_mut().as_reader().take(limit + 1).read_to_end(&mut bytes).map_err(|e| err(format!("{method} {path}: {e}")))?;
        if bytes.len() as u64 > limit {
            return Err(err("the server's answer is larger than the sync accepts"));
        }
        Ok((status, etag, bytes))
    }

    fn fail(&self, what: &str, status: StatusCode) -> Error {
        match status.as_u16() {
            401 | 403 => err(format!("{what}: the server refused the user name or app password ({status})")),
            507 => err(format!("{what}: the server has no space left")),
            _ => err(format!("{what}: the server answered {status}")),
        }
    }
}

/// An ETag without the weak marker and the quotes, which servers add and drop inconsistently.
fn clean_etag(v: &str) -> String {
    v.trim().trim_start_matches("W/").trim_matches('"').to_owned()
}

impl Backend for WebDav {
    fn ensure(&self, dir: &str) -> Result<()> {
        let mut path = String::new();
        // The endpoint folder itself, then each level below it. MKCOL on an existing folder answers 405.
        let mut levels = vec![String::new()];
        for seg in dir.split('/').filter(|s| !s.is_empty()) {
            if !safe_segment(seg) {
                return Err(err("unsafe sync folder"));
            }
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(seg);
            levels.push(path.clone());
        }
        for level in levels {
            let (status, _, _) = self.send("MKCOL", &level, &[], &[], 64 * 1024)?;
            if !(status.is_success() || status.as_u16() == 405 || status.as_u16() == 301) {
                return Err(self.fail("creating the sync folder", status));
            }
        }
        Ok(())
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        if !dir.is_empty() {
            safe_path(dir)?;
        }
        const BODY: &[u8] = br#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:getetag/><d:resourcetype/></d:prop></d:propfind>"#;
        let (status, _, bytes) =
            self.send("PROPFIND", dir, &[("Depth", "1".into()), ("Content-Type", "application/xml".into())], BODY, 4 * 1024 * 1024)?;
        if status.as_u16() == 404 {
            return Ok(Vec::new());
        }
        if status.as_u16() != 207 {
            return Err(self.fail("listing the sync folder", status));
        }
        parse_multistatus(&bytes)
    }

    fn get(&self, path: &str) -> Result<Option<(Vec<u8>, String)>> {
        safe_path(path)?;
        let (status, etag, bytes) = self.send("GET", path, &[], &[], MAX_BLOB as u64)?;
        match status.as_u16() {
            404 => Ok(None),
            200 => Ok(Some((bytes, etag.unwrap_or_default()))),
            _ => Err(self.fail(&format!("reading {path}"), status)),
        }
    }

    fn put(&self, path: &str, bytes: &[u8], cond: &Cond) -> Result<String> {
        safe_path(path)?;
        let mut headers = vec![("Content-Type", "application/octet-stream".to_owned())];
        match cond {
            Cond::Absent => headers.push(("If-None-Match", "*".into())),
            Cond::Match(etag) => headers.push(("If-Match", format!("\"{etag}\""))),
            Cond::Any => {}
        }
        let (status, etag, _) = self.send("PUT", path, &headers, bytes, 64 * 1024)?;
        match status.as_u16() {
            412 => Err(err(CONFLICT)),
            200 | 201 | 204 => match etag {
                Some(e) => Ok(e),
                // Not every server returns the new ETag with the PUT: ask for it.
                None => self
                    .list(path.rsplit_once('/').map_or("", |(d, _)| d))?
                    .into_iter()
                    .find(|e| path.rsplit('/').next() == Some(e.name.as_str()))
                    .map(|e| e.etag)
                    .ok_or_else(|| err("the server did not report the new version of the file")),
            },
            _ => Err(self.fail(&format!("writing {path}"), status)),
        }
    }
}

/// The files in a `207 Multi-Status` answer. Folders are skipped; a name that is not a safe segment is dropped.
fn parse_multistatus(xml: &[u8]) -> Result<Vec<Entry>> {
    use quick_xml::XmlVersion;
    let bad = |e: &dyn std::fmt::Display| err(format!("the server's listing is not XML: {e}"));
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let (mut href, mut etag, mut collection) = (String::new(), String::new(), false);
    let mut field: Option<&'static str> = None;
    let mut text = String::new();
    let mut out = Vec::new();
    loop {
        match reader.read_event().map_err(|e| bad(&e))? {
            Event::Start(e) | Event::Empty(e) => {
                match e.local_name().as_ref() {
                    "response" => (href, etag, collection) = (String::new(), String::new(), false),
                    "href" => (field, text) = (Some("href"), String::new()),
                    "getetag" => (field, text) = (Some("etag"), String::new()),
                    "collection" => collection = true,
                    _ => {}
                }
            }
            Event::Text(t) => text.push_str(&t.xml_content(XmlVersion::Implicit1_0)),
            Event::GeneralRef(r) => match &*r {
                "quot" => text.push('"'),
                "amp" => text.push('&'),
                "lt" => text.push('<'),
                "gt" => text.push('>'),
                "apos" => text.push('\''),
                _ => {}
            },
            Event::End(e) => {
                match (e.local_name().as_ref(), field.take()) {
                    ("href", Some("href")) => href = std::mem::take(&mut text),
                    ("getetag", Some("etag")) => etag = clean_etag(&std::mem::take(&mut text)),
                    _ => {}
                }
                if e.local_name().as_ref() == "response" && !collection && !etag.is_empty() {
                    let last = href.trim_end_matches('/').rsplit('/').next().unwrap_or("");
                    let name = percent_decode(last);
                    if safe_segment(&name) {
                        out.push(Entry { name, etag: etag.clone() });
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn only_https_or_loopback_is_accepted_and_no_credentials_in_the_url() {
        assert!(WebDav::new("https://cloud.example/remote.php/dav/files/u/hw", "u", "p").is_ok());
        assert!(WebDav::new("http://127.0.0.1:8080/dav", "u", "p").is_ok());
        assert!(WebDav::new("http://[::1]:8080/dav", "u", "p").is_ok());
        assert!(WebDav::new("http://cloud.example/dav", "u", "p").is_err());
        assert!(WebDav::new("https://u:p@cloud.example/dav", "u", "p").is_err());
        assert!(WebDav::new("https://cloud.example/dav?x=1", "u", "p").is_err());
        assert!(WebDav::new("ftp://cloud.example/dav", "u", "p").is_err());
        assert!(WebDav::new("cloud.example/dav", "u", "p").is_err());
    }

    #[test]
    fn paths_cannot_leave_the_folder() {
        for bad in ["", "..", "a/../b", "a//b", "/a", "a b", "a%2e", "a\\b"] {
            assert!(safe_path(bad).is_err(), "{bad:?}");
        }
        assert!(safe_path("files/settings.json.hwx").is_ok());
    }

    #[test]
    fn a_multistatus_lists_files_with_clean_etags_and_skips_folders_and_odd_names() {
        let xml = br#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
 <d:response><d:href>/dav/hw/files/</d:href><d:propstat><d:prop><d:getetag>"dir1"</d:getetag><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
 <d:response><d:href>/dav/hw/files/settings.json.hwx</d:href><d:propstat><d:prop><d:getetag>&quot;abc123&quot;</d:getetag><d:resourcetype/></d:prop></d:propstat></d:response>
 <d:response><d:href>/dav/hw/files/themes.My%20Theme.hwx</d:href><d:propstat><d:prop><d:getetag>W/"w1"</d:getetag><d:resourcetype/></d:prop></d:propstat></d:response>
 <D:response xmlns:D="DAV:"><D:href>/dav/hw/files/layout.json.hwx</D:href><D:propstat><D:prop><D:getetag>"l9"</D:getetag></D:prop></D:propstat></D:response>
</d:multistatus>"#;
        let got = parse_multistatus(xml).unwrap();
        let names: Vec<_> = got.iter().map(|e| (e.name.as_str(), e.etag.as_str())).collect();
        assert_eq!(names, vec![("settings.json.hwx", "abc123"), ("layout.json.hwx", "l9")]);
        assert!(parse_multistatus(b"<a><b></a>").is_err());
    }

    #[test]
    fn percent_decoding_is_total() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
    }
}
