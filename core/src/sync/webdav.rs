//! WebDAV, the transport of the sync (Nextcloud and anything else that speaks it): the few verbs the sync needs, and
//! nothing of its own on the server. `https` is required except for a loopback address, redirects are not followed (so
//! the app password is never sent to another host), every answer is bounded, and a server that answers wrongly is an
//! error, never a panic.

use std::io::{self, Read};
use std::time::Duration;

use quick_xml::Reader;
use quick_xml::events::Event;
use sha2::{Digest, Sha256};
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
    /// The SHA-256 the server holds for the file, 64 lowercase hex characters; `None` when the server reports none (or only
    /// other algorithms). A hint: the sync reads the file when it differs from what it recorded, and the file decides.
    pub checksum: Option<String>,
}

/// The longest a path segment may be (see [`safe_segment`]); every name on the server is made to fit.
pub const MAX_SEGMENT: usize = 128;

/// The SHA-256 of `bytes` as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    Sha256::digest(bytes).iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
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
    !s.is_empty() && s.len() <= MAX_SEGMENT && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
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
        // The folder itself has an empty path: name it in words instead of leaving a gap in the text.
        let shown = if path.is_empty() { "the sync folder" } else { path };
        let mut resp = self.agent.run(req).map_err(|e| err(format!("{method} {shown}: {e}")))?;
        let status = resp.status();
        let etag = resp.headers().get("ETag").or_else(|| resp.headers().get("OC-ETag")).and_then(|v| v.to_str().ok()).map(clean_etag);
        let mut bytes = Vec::new();
        resp.body_mut().as_reader().take(limit + 1).read_to_end(&mut bytes).map_err(|e| err(format!("{method} {shown}: {e}")))?;
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
        // `oc:checksums` is the content hash a Nextcloud-like server keeps; a server that does not know it leaves it out.
        const BODY: &[u8] = br#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:prop><d:getetag/><d:resourcetype/><oc:checksums/></d:prop></d:propfind>"#;
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
        // The checksum of the sealed blob (never of a plaintext) lets a server that keeps it tell two versions apart that
        // share an ETag, because the ETag of such a server moves once per second.
        let mut headers =
            vec![("Content-Type", "application/octet-stream".to_owned()), ("OC-Checksum", format!("sha256:{}", sha256_hex(bytes)))];
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
    // The text inside `checksums` and `checksum` elements of the current response, and how deep inside them the reader is.
    let (mut sums, mut in_sums) = (String::new(), 0u32);
    let mut out = Vec::new();
    loop {
        let event = reader.read_event().map_err(|e| bad(&e))?;
        // An empty element has no end event: it must not leave the reader "inside" it.
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(e) | Event::Empty(e) => {
                match e.local_name().as_ref() {
                    "response" => {
                        (href, etag, collection) = (String::new(), String::new(), false);
                        (sums, in_sums) = (String::new(), 0);
                    }
                    "href" => (field, text) = (Some("href"), String::new()),
                    "getetag" => (field, text) = (Some("etag"), String::new()),
                    "checksums" | "checksum" if !empty => {
                        in_sums += 1;
                        sums.push(' ');
                    }
                    "collection" => collection = true,
                    _ => {}
                }
            }
            Event::Text(t) => {
                let content = t.xml_content(XmlVersion::Implicit1_0);
                if in_sums > 0 {
                    sums.extend(content.chars().take(MAX_CHECKSUM_TEXT.saturating_sub(sums.len())));
                }
                text.push_str(&content);
            }
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
                    ("checksums" | "checksum", _) => {
                        in_sums = in_sums.saturating_sub(1);
                        sums.push(' ');
                    }
                    _ => {}
                }
                if e.local_name().as_ref() == "response" && !collection && !etag.is_empty() {
                    let last = href.trim_end_matches('/').rsplit('/').next().unwrap_or("");
                    let name = percent_decode(last);
                    if safe_segment(&name) {
                        out.push(Entry { name, etag: etag.clone(), checksum: sha256_of(&sums) });
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// The most text of `checksums` elements that is kept for one file: a hostile server cannot make the reader hold more.
const MAX_CHECKSUM_TEXT: usize = 4096;

/// The SHA-256 a `oc:checksums` text names, as 64 lowercase hex characters. The text is a space separated list of
/// `<algorithm>:<hex>` (`SHA1:… sha256:… MD5:…`); the algorithm is compared ignoring case, a token over 200 characters or
/// one whose hex is not exactly 64 characters is not a checksum, and only the first 16 tokens are looked at. Anything
/// else is `None`: a bad checksum is no reason to drop the file.
fn sha256_of(text: &str) -> Option<String> {
    text.split_whitespace().take(16).filter(|t| t.len() <= 200).find_map(|token| {
        let (algorithm, hex) = token.split_once(':')?;
        (algorithm.eq_ignore_ascii_case("sha256") && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hex.to_ascii_lowercase())
    })
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
    fn a_multistatus_checksum_is_read_leniently_and_bounded() {
        let hex = "0123456789abcdef".repeat(4);
        let upper = hex.to_ascii_uppercase();
        let entry = |props: &str| {
            let xml = format!(
                r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>/dav/files/a.hwx</d:href><d:propstat><d:prop><d:getetag>"e1"</d:getetag><d:resourcetype/>{props}</d:prop></d:propstat></d:response></d:multistatus>"#
            );
            let got = parse_multistatus(xml.as_bytes()).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(got.len(), 1, "the entry is listed whatever its checksum looks like");
            assert_eq!((got[0].name.as_str(), got[0].etag.as_str()), ("a.hwx", "e1"));
            got[0].checksum.clone()
        };
        let wrap = |inner: &str| format!("<oc:checksums><oc:checksum>{inner}</oc:checksum></oc:checksums>");
        assert_eq!(entry(&wrap(&format!("sha256:{hex}"))), Some(hex.clone()));
        assert_eq!(entry(&wrap(&format!("SHA256:{upper}"))), Some(hex.clone()), "returned lowercase");
        assert_eq!(entry(&wrap(&format!("SHA1:{} sha256:{hex} MD5:{}", "a".repeat(40), "b".repeat(32)))), Some(hex.clone()));
        assert_eq!(entry(""), None);
        assert_eq!(entry("<oc:checksums/>"), None);
        assert_eq!(entry("<oc:checksums></oc:checksums>"), None);
        assert_eq!(entry(&wrap(&format!("SHA1:{} MD5:{}", "a".repeat(40), "b".repeat(32)))), None, "only other algorithms");
        assert_eq!(entry(&wrap(&format!("sha256:{}", &hex[..63]))), None, "too short");
        assert_eq!(entry(&wrap(&format!("sha256:{hex}0"))), None, "too long");
        assert_eq!(entry(&wrap(&format!("sha256:{}", "g".repeat(64)))), None, "not hex");
        assert_eq!(entry(&wrap(&format!("sha256:{}", "a".repeat(5000)))), None, "hostile length");
        assert_eq!(entry(&wrap(&format!("{} sha256:{hex}", "x".repeat(5000)))), None, "the work is bounded");
        assert_eq!(entry(&wrap(&format!("{} sha256:{hex}", "a ".repeat(40)))), None, "only the first tokens are looked at");
        assert_eq!(entry(&format!("<oc:checksums>sha256:{hex}</oc:checksums>")), Some(hex.clone()), "text inside checksums");
        assert_eq!(entry(&format!("<oc:checksums><x:other xmlns:x=\"urn:x\">1</x:other><oc:checksum>sha256:{hex}</oc:checksum></oc:checksums>")), Some(hex.clone()));
        assert_eq!(entry(&wrap("")), None);
        // A checksum belongs to its own file: the next file has none.
        let xml = format!(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:response><d:href>/dav/files/a.hwx</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag>{}</d:prop></d:propstat></d:response><d:response><d:href>/dav/files/b.hwx</d:href><d:propstat><d:prop><d:getetag>"2"</d:getetag></d:prop></d:propstat></d:response></d:multistatus>"#,
            wrap(&format!("sha256:{hex}"))
        );
        let got = parse_multistatus(xml.as_bytes()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(got.iter().map(|e| e.checksum.clone()).collect::<Vec<_>>(), vec![Some(hex), None]);
    }

    #[test]
    fn a_failure_of_the_sync_folder_is_named_in_words() {
        // A port that refuses connections: bind one, then let it go.
        let port = std::net::TcpListener::bind("127.0.0.1:0").map(|l| l.local_addr().map_or(0, |a| a.port())).unwrap_or(0);
        let Ok(dav) = WebDav::new(&format!("http://127.0.0.1:{port}/dav"), "u", "p") else { panic!("a loopback endpoint is accepted") };
        let Err(ensure) = dav.ensure("") else { panic!("nothing listens on the port") };
        let Err(list) = dav.list("") else { panic!("nothing listens on the port") };
        for text in [ensure.to_string(), list.to_string()] {
            assert!(text.contains("the sync folder"), "{text}");
            assert!(!text.contains("MKCOL :") && !text.contains("PROPFIND :"), "{text}");
        }
        // A path below the folder is shown as it is.
        let Err(file) = dav.get("files/settings.json.hwx") else { panic!("nothing listens on the port") };
        assert!(file.to_string().contains("GET files/settings.json.hwx:"), "{file}");
    }

    #[test]
    fn sha256_hex_is_the_lowercase_digest() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn percent_decoding_is_total() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
    }
}
