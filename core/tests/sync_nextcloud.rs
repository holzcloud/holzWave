//! The sync against a real Nextcloud: the part of row S1 of `docs/test-plan.md` that a script can repeat.
//!
//! Every test here is `#[ignore]` and reads its server from the environment, so a plain `cargo test` never touches a
//! network. `sh scripts/nextcloud-live-test.sh` starts the official Nextcloud image in Docker, sets the variables and
//! runs `cargo test --test sync_nextcloud -- --ignored`:
//!
//! * `HOLZWAVE_TEST_NEXTCLOUD_URL`: the folder to sync into, for example
//!   `http://127.0.0.1:8080/remote.php/dav/files/holz/holzwave-live-1760000000` (every test adds its own suffix, so the
//!   tests never see each other's files);
//! * `HOLZWAVE_TEST_NEXTCLOUD_USER` and `HOLZWAVE_TEST_NEXTCLOUD_PASSWORD`: the account and its app password;
//! * `HOLZWAVE_TEST_NEXTCLOUD_TLS_URL`: the same folder through a proxy whose certificate authority no system trusts.
//!
//! The tests use the public API of `holzwave_core::sync` as it is. A secret is never printed: failure messages name the
//! step and what the server answered (status and a short reason), nothing else.
//!
//! What this proves that the loopback server of the engine tests cannot: the ETags, the conditional writes, the
//! multistatus listing and the refusals of a real server, and the refusal of an untrusted certificate. What stays the
//! owner's: two real computers, their own server and its app-password policy, a private CA in the system store, the
//! keychain prompts.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use holzwave_core::persist::write_atomic;
use holzwave_core::profiles::{Auth, Profile, Profiles};
use holzwave_core::secrets::{MemoryStore, SecretStore};
use holzwave_core::sync::crypto::{Kdf, Key};
use holzwave_core::sync::engine::{ConfigSource, Syncer, promote_incoming};
use holzwave_core::sync::webdav::{Backend, Cond, Entry, WebDav};
use holzwave_core::Result;

const PASSPHRASE: &str = "correct horse battery";
/// A fast derivation: the folder is made with it, so every device of a test pays only this.
const KDF: Kdf = Kdf { m_kib: 8 * 1024, t: 1, p: 1 };

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is not set: run `sh scripts/nextcloud-live-test.sh`, which starts a Nextcloud and sets it")
    })
}

/// The server folder of one test: the environment's folder plus a suffix of this test.
fn endpoint(suffix: &str) -> String {
    format!("{}-{suffix}", env("HOLZWAVE_TEST_NEXTCLOUD_URL"))
}

fn webdav(suffix: &str) -> WebDav {
    WebDav::new(&endpoint(suffix), &env("HOLZWAVE_TEST_NEXTCLOUD_USER"), &env("HOLZWAVE_TEST_NEXTCLOUD_PASSWORD"))
        .expect("the endpoint of the environment is a valid WebDAV folder")
}

/// A device: its own folder and its own secret store, the same server. The folder is removed when the device is dropped.
struct Device {
    dir: PathBuf,
    secrets: Arc<MemoryStore>,
}

impl Device {
    fn new(tag: &str) -> Device {
        let dir = std::env::temp_dir().join(format!("hw-nc-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Device { dir, secrets: Arc::new(MemoryStore::default()) }
    }

    fn connect(&self, suffix: &str, pass: &str) -> Result<Syncer<WebDav, ConfigSource>> {
        Syncer::connect_with(webdav(suffix), ConfigSource::new(&self.dir, self.secrets.clone()), &self.dir, pass, KDF)
    }

    /// An edit by the user, after the device started again (staged downloads applied).
    fn put(&self, name: &str, text: &str) {
        promote_incoming(&self.dir);
        write_atomic(&self.dir.join(name), text.as_bytes()).unwrap();
    }

    /// What the device has after its next start: staged downloads are moved into place first.
    fn text(&self, name: &str) -> Option<String> {
        promote_incoming(&self.dir);
        fs::read_to_string(self.dir.join(name)).ok()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn names(entries: Vec<Entry>) -> Vec<String> {
    let mut out: Vec<String> = entries.into_iter().map(|e| e.name).collect();
    out.sort();
    out
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[test]
#[ignore = "needs a real Nextcloud: run `sh scripts/nextcloud-live-test.sh`"]
fn real_nextcloud_two_devices_converge() {
    let suffix = "converge";
    let (a, b) = (Device::new("conv-a"), Device::new("conv-b"));
    fs::create_dir_all(a.dir.join("themes")).unwrap();
    a.put("settings.json", "{\"font_size\":14,\"marker\":\"plaintext-canary-7f3\"}");
    a.put("layout.json", "{\"version\":1,\"marker\":\"layout-canary-9c1\"}");
    a.put("themes/dark.json", "{\"name\":\"dark\",\"marker\":\"theme-canary-2b8\"}");
    let mut profiles = Profiles::default();
    let mut web = Profile::new("web", "Web", "web.example");
    web.user = "deploy".into();
    web.auth = Auth::Password;
    profiles.upsert(web).unwrap();
    profiles.save(&a.dir).unwrap();
    a.secrets.set("ssh:web", "hunter2-canary-hunter2").unwrap();

    let report = a.connect(suffix, PASSPHRASE).expect("device A connects to the new folder").sync_once().expect("A syncs");
    assert!(report.errors.is_empty(), "A: {report:?}");
    for want in ["settings.json", "layout.json", "themes/dark.json", "connections.json", "secrets.json"] {
        assert!(report.uploaded.contains(&want.to_owned()), "A did not upload {want}: {report:?}");
    }

    // The server holds meta.json at the top, the blobs in files/, and nothing readable.
    let dav = webdav(suffix);
    assert_eq!(names(dav.list("").unwrap()), vec!["meta.json"], "the folder holds meta.json and nothing else at its top");
    let files = names(dav.list("files").unwrap());
    assert_eq!(
        files,
        vec![
            "connections.json.hwx",
            "layout.json.hwx",
            "secrets.json.hwx",
            "settings.json.hwx",
            "themes.dark.json.hwx"
        ],
        "files/ holds the five blobs, named by the logical names the format documents"
    );
    for name in &files {
        let (blob, _) = dav.get(&format!("files/{name}")).unwrap().expect("listed, so it exists");
        for word in ["canary", "font_size", "hunter2", "web.example", "deploy", "dark"] {
            assert!(!contains(&blob, word), "files/{name} shows {word:?} in clear on the server");
        }
    }
    let (meta, _) = dav.get("meta.json").unwrap().expect("meta.json exists");
    assert!(!contains(&meta, PASSPHRASE), "meta.json must not hold the passphrase");

    // Device B opens the same folder with the same passphrase and gets everything.
    let report = b.connect(suffix, PASSPHRASE).expect("device B opens the folder").sync_once().expect("B syncs");
    assert!(report.errors.is_empty(), "B: {report:?}");
    assert_eq!(report.downloaded.len(), 5, "B downloads the five files: {report:?}");
    for name in ["settings.json", "layout.json", "themes/dark.json", "connections.json"] {
        assert_eq!(b.text(name), fs::read_to_string(a.dir.join(name)).ok(), "{name} differs on B after the restart");
    }
    assert_eq!(b.secrets.get("ssh:web").unwrap().as_deref(), Some("hunter2-canary-hunter2"), "B has the saved password");
    assert!(!b.dir.join("secrets.json").exists(), "the secrets never become a file");

    // A second run on B changes nothing and writes nothing.
    let again = b.connect(suffix, PASSPHRASE).unwrap().sync_once().unwrap();
    assert!(again.uploaded.is_empty() && again.downloaded.is_empty() && again.conflicts.is_empty(), "{again:?}");
}

// ---- what the client does not show: the raw answers of the server ------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn basic(user: &str, password: &str) -> String {
    let bytes = format!("{user}:{password}").into_bytes();
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    format!("Basic {out}")
}

struct Raw {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Raw {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// One request with the account's credentials, to see the status and the headers the client hides. Loopback `http` only.
fn raw(suffix: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Raw {
    use ureq::http::Request;
    let agent: ureq::Agent = ureq::config::Config::builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .allow_non_standard_methods(true)
        .timeout_global(Some(std::time::Duration::from_secs(60)))
        .build()
        .into();
    let url = if path.is_empty() { endpoint(suffix) } else { format!("{}/{path}", endpoint(suffix)) };
    let mut req = Request::builder().method(method).uri(url).header(
        "Authorization",
        basic(&env("HOLZWAVE_TEST_NEXTCLOUD_USER"), &env("HOLZWAVE_TEST_NEXTCLOUD_PASSWORD")),
    );
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut resp = agent.run(req.body(body.to_vec()).unwrap()).unwrap_or_else(|e| panic!("{method} {path}: {e}"));
    let status = resp.status().as_u16();
    let headers = resp.headers().iter().map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("?").to_owned())).collect();
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    Raw { status, headers, body }
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// The plain text of a blob of the folder (a file or a backup), decrypted the way a device does: the key from the
/// folder's own `meta.json` and the passphrase. `name` is the logical name the blob was sealed under.
fn decrypt(dav: &WebDav, name: &str, blob: &[u8]) -> String {
    let (meta, _) = dav.get("meta.json").unwrap().expect("meta.json exists");
    let meta: serde_json::Value = serde_json::from_slice(&meta).unwrap();
    let n = |k: &str| u32::try_from(meta[k].as_u64().unwrap()).unwrap();
    let key = Key::derive(PASSPHRASE, &unhex(meta["salt"].as_str().unwrap()), Kdf { m_kib: n("m_kib"), t: n("t"), p: n("p") }).unwrap();
    let (_, plain) = key.open(name, blob).unwrap_or_else(|e| panic!("the blob of {name} does not open: {e}"));
    String::from_utf8(plain.to_vec()).unwrap()
}

/// Non-repeating text from a fixed seed (xorshift), so a size test does not depend on a compressor being absent.
fn noise(len: usize) -> String {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            B64[(x >> 32) as usize % 64] as char
        })
        .collect()
}

#[test]
#[ignore = "needs a real Nextcloud: run `sh scripts/nextcloud-live-test.sh`"]
fn real_nextcloud_etags_and_preconditions() {
    let suffix = "etags";
    let dav = webdav(suffix);
    dav.ensure("").expect("creating the folder");
    dav.ensure("").expect("creating it again is not an error (the server answers 405)");
    let path = "probe.bin";

    let first = dav.put(path, b"one", &Cond::Absent).expect("a write to a new name");
    assert!(!first.is_empty(), "the server returned no ETag for a new file");
    let listed = dav.list("").unwrap().into_iter().find(|e| e.name == path).expect("the new file is listed");
    assert_eq!(listed.etag, first, "the ETag a PUT returns equals the one the listing shows");

    let e = dav.put(path, b"two", &Cond::Absent).expect_err("`If-None-Match: *` on an existing name");
    assert!(e.to_string().contains("precondition failed"), "an existing name under Cond::Absent: {e}");
    let e = dav.put(path, b"two", &Cond::Match("0123456789abcdef0123456789abcdef".into())).expect_err("a stale `If-Match`");
    assert!(e.to_string().contains("precondition failed"), "a made-up ETag under Cond::Match: {e}");
    assert_eq!(dav.get(path).unwrap().unwrap().0, b"one", "a refused write changed nothing");

    // A real Nextcloud derives a file's ETag from the modification time in whole seconds: two writes within one second
    // can share an ETag. Record what this server does, then leave the second boundary behind so the checks below see
    // distinct versions. (A device that lost a race inside one second would not be told: see the SUMMARY of this run.)
    let same_second = dav.put(path, b"two", &Cond::Match(first.clone())).expect("the current ETag is accepted");
    println!(
        "ETag after a second write within the same second: {}",
        if same_second == first { "UNCHANGED (known difference: a write inside one second of the previous one is not seen by If-Match)" } else { "changed" }
    );
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let second = dav.put(path, b"three", &Cond::Match(same_second.clone())).expect("the current ETag is accepted");
    assert_ne!(second, same_second, "a write a second later has a new ETag");
    let (bytes, got) = dav.get(path).unwrap().expect("the file exists");
    assert_eq!((bytes.as_slice(), got.as_str()), (b"three".as_slice(), second.as_str()), "GET returns the last write and its ETag");
    assert!(dav.get("no-such-file.bin").unwrap().is_none(), "a name that does not exist is Ok(None)");
    assert!(dav.list("no-such-folder").unwrap().is_empty(), "an absent folder lists as empty");

    // What the client hides: the raw ETag forms and the raw statuses of the refusals.
    let put = raw(suffix, "PUT", path, &[("Content-Type", "application/octet-stream")], b"four");
    println!("raw PUT answer: status {} ETag {:?} OC-ETag {:?}", put.status, put.header("ETag"), put.header("OC-ETag"));
    let get = raw(suffix, "GET", path, &[], &[]);
    println!("raw GET answer: status {} ETag {:?} OC-ETag {:?}", get.status, get.header("ETag"), get.header("OC-ETag"));
    let prop = raw(
        suffix,
        "PROPFIND",
        "",
        &[("Depth", "1"), ("Content-Type", "application/xml")],
        br#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:getetag/><d:resourcetype/></d:prop></d:propfind>"#,
    );
    let etag_tags: Vec<&str> = prop.body.split("<d:getetag>").skip(1).filter_map(|p| p.split("</d:getetag>").next()).collect();
    println!("raw PROPFIND: status {} getetag values {:?}", prop.status, etag_tags);
    assert_eq!(prop.status, 207, "PROPFIND answers 207 Multi-Status");
    let none_match = raw(suffix, "PUT", path, &[("If-None-Match", "*")], b"x");
    let stale = raw(suffix, "PUT", path, &[("If-Match", "\"0123456789abcdef0123456789abcdef\"")], b"x");
    println!("raw refusals: If-None-Match * on an existing name: {}; a stale If-Match: {}", none_match.status, stale.status);
    assert_eq!((none_match.status, stale.status), (412, 412), "both refusals are 412 Precondition Failed");
    println!("raw MKCOL of an existing folder: {}", raw(suffix, "MKCOL", "", &[], &[]).status);
}

#[test]
#[ignore = "needs a real Nextcloud: run `sh scripts/nextcloud-live-test.sh`"]
fn real_nextcloud_a_conflict_keeps_the_loser() {
    let suffix = "conflict";
    let (a, b) = (Device::new("conf-a"), Device::new("conf-b"));
    let dav = webdav(suffix);
    a.put("settings.json", "base");
    a.connect(suffix, PASSPHRASE).unwrap().sync_once().unwrap();
    b.connect(suffix, PASSPHRASE).unwrap().sync_once().unwrap();
    assert_eq!(b.text("settings.json").as_deref(), Some("base"));

    // Both change the file. A is first, B is second and finds that both sides changed. A real Nextcloud's ETag moves
    // with the modification time in whole seconds, so A's write must come a second after the base to be seen as a change.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    a.put("settings.json", "from a");
    b.put("settings.json", "from b");
    let report = a.connect(suffix, PASSPHRASE).unwrap().sync_once().unwrap();
    assert_eq!(report.uploaded, vec!["settings.json"], "A is first, so it is no conflict for A: {report:?}");
    // B's write comes a second after A's, or the two writes would share one ETag and A would never see B's version.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let report = b.connect(suffix, PASSPHRASE).unwrap().sync_once().unwrap();
    assert_eq!(report.conflicts, vec!["settings.json"], "B finds both sides changed: {report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    // B's stamp is as high in the counter and later in time, so B wins and A's version goes to backups/ on the server.
    assert_eq!(report.uploaded, vec!["settings.json"], "{report:?}");
    let backups: Vec<String> = names(dav.list("backups").unwrap());
    assert_eq!(backups.len(), 1, "exactly one backup of the loser: {backups:?}");
    assert!(backups[0].starts_with("settings.json."), "{backups:?}");
    let (loser, _) = dav.get(&format!("backups/{}", backups[0])).unwrap().expect("the backup exists");
    assert_eq!(decrypt(&dav, "settings.json", &loser), "from a", "the backup holds the version that lost");
    let (winner, _) = dav.get("files/settings.json.hwx").unwrap().expect("the file exists");
    assert_eq!(decrypt(&dav, "settings.json", &winner), "from b", "the server's file is the winner");
    assert!(!b.dir.join("sync-conflicts").exists(), "B won, so it kept nothing locally");
    println!("conflict, the remote loses: backups/{}", backups[0]);

    // A runs again and ends with the winner's content.
    let report = a.connect(suffix, PASSPHRASE).unwrap().sync_once().unwrap();
    assert_eq!(report.downloaded, vec!["settings.json"], "{report:?}");
    assert_eq!(a.text("settings.json").as_deref(), Some("from b"));

    // The other direction: the server's counter is higher, so the local version loses and is kept next to the files.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    for text in ["v1", "v2"] {
        a.put("settings.json", text);
        a.connect(suffix, PASSPHRASE).unwrap().sync_once().unwrap();
    }
    b.put("settings.json", "b only");
    let report = b.connect(suffix, PASSPHRASE).unwrap().sync_once().unwrap();
    assert_eq!(report.conflicts, vec!["settings.json"], "{report:?}");
    assert_eq!(report.downloaded, vec!["settings.json"], "the higher counter wins: {report:?}");
    assert_eq!(b.text("settings.json").as_deref(), Some("v2"));
    let kept: Vec<_> = fs::read_dir(b.dir.join("sync-conflicts")).expect("the local loser is kept").flatten().collect();
    assert_eq!(kept.len(), 1, "exactly one kept local version");
    assert!(kept[0].file_name().to_string_lossy().starts_with("settings.json."));
    assert_eq!(fs::read_to_string(kept[0].path()).unwrap(), "b only");
    assert_eq!(names(dav.list("backups").unwrap()).len(), 1, "the server's backups did not grow");
    println!("conflict, the local loses: sync-conflicts/{}", kept[0].file_name().to_string_lossy());
}

#[test]
#[ignore = "needs a real Nextcloud: run `sh scripts/nextcloud-live-test.sh`"]
fn real_nextcloud_refuses_wrong_credentials() {
    let suffix = "credentials";
    let (a, b) = (Device::new("cred-a"), Device::new("cred-b"));
    a.put("layout.json", "{}");
    a.connect(suffix, PASSPHRASE).expect("the right credentials work").sync_once().unwrap();

    // At most two wrong attempts at the server in the whole test: its brute-force protection slows repeated failures.
    let (wrong, real) = ("wrong-app-password-0000-1111-2222", env("HOLZWAVE_TEST_NEXTCLOUD_PASSWORD"));
    let user = env("HOLZWAVE_TEST_NEXTCLOUD_USER");
    let source = || ConfigSource::new(&b.dir, b.secrets.clone());
    let backend = WebDav::new(&endpoint(suffix), &user, wrong).unwrap();
    let e = Syncer::connect_with(backend, source(), &b.dir, PASSPHRASE, KDF).err().expect("a wrong app password is refused");
    let text = e.to_string();
    println!("wrong app password: {text}");
    assert!(text.contains("refused the user name or app password"), "the refusal is in words: {text}");
    assert!(!text.contains(wrong) && !text.contains(&real), "the refusal names no secret");
    let backend = WebDav::new(&endpoint(suffix), "nobody-here", wrong).unwrap();
    let e = Syncer::connect_with(backend, source(), &b.dir, PASSPHRASE, KDF).err().expect("an unknown account is refused");
    let text = e.to_string();
    println!("unknown account: {text}");
    assert!(text.contains("refused the user name or app password") && !text.contains(wrong) && !text.contains(&real), "{text}");

    // A wrong passphrase with the right account: refused before any file is touched (the server sees a valid login).
    let e = b.connect(suffix, "a different passphrase").err().expect("a wrong passphrase is refused");
    let text = e.to_string();
    println!("wrong passphrase: {text}");
    assert!(text.contains("the passphrase is wrong for this sync folder"), "{text}");
    assert!(!text.contains("a different passphrase") && !text.contains(PASSPHRASE), "the refusal names no passphrase");
    assert!(b.text("layout.json").is_none(), "nothing was written by the refused attempts");

    // The right credentials still work afterwards.
    let report = b.connect(suffix, PASSPHRASE).expect("the right credentials still work").sync_once().unwrap();
    assert_eq!(report.downloaded, vec!["layout.json"], "{report:?}");
}

#[test]
#[ignore = "needs a real Nextcloud: run `sh scripts/nextcloud-live-test.sh`"]
fn real_nextcloud_size_and_listing() {
    // A settings file of 3 MiB, below the 4 MiB limit of the sync, goes through byte for byte.
    let suffix = "size";
    let (a, b) = (Device::new("size-a"), Device::new("size-b"));
    let big = format!("{{\"noise\":\"{}\"}}", noise(3 * 1024 * 1024));
    a.put("settings.json", &big);
    let report = a.connect(suffix, PASSPHRASE).unwrap().sync_once().unwrap();
    assert_eq!(report.uploaded, vec!["settings.json"], "{report:?}");
    let report = b.connect(suffix, PASSPHRASE).unwrap().sync_once().unwrap();
    assert_eq!(report.downloaded, vec!["settings.json"], "{report:?}");
    assert!(b.text("settings.json").as_deref() == Some(big.as_str()), "the 3 MiB file differs after the round trip");

    // Over the limit: the engine refuses it and it never reaches the server.
    let over = "size-over";
    let c = Device::new("size-c");
    c.put("settings.json", &noise(4 * 1024 * 1024 + 100));
    let report = c.connect(over, PASSPHRASE).unwrap().sync_once().unwrap();
    println!("a file over the limit: {:?}", report.errors);
    assert!(report.uploaded.is_empty() && report.errors.iter().any(|(n, e)| n == "settings.json" && e.contains("larger than the limit")), "{report:?}");
    assert!(webdav(over).list("files").unwrap().is_empty(), "nothing over the limit reached the server");

    // Thirty themes, and the longest name the format allows.
    let many = "many";
    let (d, e) = (Device::new("many-a"), Device::new("many-b"));
    fs::create_dir_all(d.dir.join("themes")).unwrap();
    let mut expected: Vec<String> = Vec::new();
    for i in 0..30 {
        d.put(&format!("themes/t-{i:02}.json"), &format!("{{\"name\":\"t-{i:02}\"}}"));
        expected.push(format!("themes.t-{i:02}.json.hwx"));
    }
    // A remote name is `themes.` + the name + `.json.hwx` and may be 128 characters at most: 112 characters of name.
    let longest = "x".repeat(112);
    d.put(&format!("themes/{longest}.json"), "{\"long\":true}");
    expected.push(format!("themes.{longest}.json.hwx"));
    expected.sort();
    // A name the engine accepts (128 characters) but whose remote name is longer than the transport allows.
    let too_long = "y".repeat(128);
    d.put(&format!("themes/{too_long}.json"), "{\"long\":\"too\"}");
    let report = d.connect(many, PASSPHRASE).unwrap().sync_once().unwrap();
    println!("a theme of 128 characters: errors {:?}", report.errors);
    assert_eq!(report.uploaded.len(), 31, "thirty themes and the longest allowed one go up: {report:?}");
    let listed = names(webdav(many).list("files").unwrap());
    assert_eq!(listed, expected, "the multistatus of a real server lists every one of the 31 names");
    let report = e.connect(many, PASSPHRASE).unwrap().sync_once().unwrap();
    assert_eq!(report.downloaded.len(), 31, "{report:?}");
    for i in 0..30 {
        let name = format!("themes/t-{i:02}.json");
        assert_eq!(e.text(&name).as_deref(), Some(format!("{{\"name\":\"t-{i:02}\"}}").as_str()), "{name}");
    }
    assert_eq!(e.text(&format!("themes/{longest}.json")).as_deref(), Some("{\"long\":true}"));
}

#[test]
#[ignore = "needs a real Nextcloud and a TLS proxy: run `sh scripts/nextcloud-live-test.sh`"]
fn real_nextcloud_untrusted_certificate_is_refused() {
    let suffix = "tls";
    // The proxy is not what broke it: the same folder works over the loopback `http` address in the same run.
    let plain = webdav(suffix);
    plain.ensure("").expect("the folder works over loopback http");
    plain.put("probe.txt", b"reachable", &Cond::Any).expect("a write over loopback http");
    assert_eq!(names(plain.list("").unwrap()), vec!["probe.txt"]);

    // Through the proxy, whose certificate comes from an authority the system does not trust.
    let tls_url = format!("{}-{suffix}", env("HOLZWAVE_TEST_NEXTCLOUD_TLS_URL"));
    assert!(tls_url.starts_with("https://"), "the TLS address is https");
    let dav = WebDav::new(&tls_url, &env("HOLZWAVE_TEST_NEXTCLOUD_USER"), &env("HOLZWAVE_TEST_NEXTCLOUD_PASSWORD"))
        .expect("an https address is valid; the certificate is judged on the first request");
    for (what, e) in [("ensure", dav.ensure("").expect_err("ensure over an untrusted certificate")), ("list", dav.list("").expect_err("list over an untrusted certificate"))] {
        let text = e.to_string();
        println!("untrusted certificate, {what}: {text}");
        assert!(
            ["certificate", "TLS", "tls", "UnknownIssuer", "unknown issuer", "Unknown issuer"].iter().any(|w| text.contains(w)),
            "{what}: the refusal names no certificate or TLS problem: {text}"
        );
        // A handshake that failed before the first request sends no credential: it is not an answer of the account's server.
        assert!(!text.contains("refused the user name or app password") && !text.contains("401"), "{what}: {text}");
    }
    let syncer = Syncer::connect_with(
        dav,
        ConfigSource::new(std::env::temp_dir(), Arc::new(MemoryStore::default())),
        &std::env::temp_dir(),
        PASSPHRASE,
        KDF,
    );
    assert!(syncer.is_err(), "the sync does not start over an untrusted certificate");
}
