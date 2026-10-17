use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use super::*;
use crate::secrets::MemoryStore;
use crate::sync::webdav::WebDav;

/// How often a server's ETags move: with every write, or once per (virtual) second like Nextcloud's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Granularity {
    #[default]
    PerWrite,
    Second,
}

/// An in-memory WebDAV server: the verbs the sync uses, ETags, and the conditions on a write. With
/// [`Granularity::Second`] the ETag of a file is the virtual second of its last write, and with `checksums` the server
/// keeps the `OC-Checksum` of a write and reports it in the listing, as Nextcloud does.
#[derive(Default)]
struct Store {
    files: BTreeMap<String, (Vec<u8>, u64)>,
    dirs: BTreeSet<String>,
    next: u64,
    puts: u32,
    gets: u32,
    granularity: Granularity,
    checksums: bool,
    /// The virtual clock of [`Granularity::Second`]; it moves only through [`Server::set_second`].
    clock: u64,
    /// The checksum stored with each file (a write without the header clears it).
    sums: BTreeMap<String, String>,
    /// The reads of names under `files/`.
    blob_gets: u32,
    /// The writes of a checksum server whose `OC-Checksum` was missing or did not match the body.
    bad_sums: u32,
    /// A part of a path whose next read is answered with 500.
    fail_get: Option<String>,
    /// A file of `files/` that disappears right after the next listing of that folder.
    vanish: Option<String>,
}

/// What a test counts on the server: reads of blobs, writes, and writes with a wrong or missing checksum header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Counts {
    blob_gets: u32,
    puts: u32,
    bad_sums: u32,
}

struct Server {
    url: String,
    store: Arc<Mutex<Store>>,
}

impl Server {
    /// Move the virtual clock of a one-second server.
    fn set_second(&self, second: u64) {
        self.store.lock().unwrap().clock = second;
    }

    fn counts(&self) -> Counts {
        let s = self.store.lock().unwrap();
        Counts { blob_gets: s.blob_gets, puts: s.puts, bad_sums: s.bad_sums }
    }
}

fn serve(stream: TcpStream, store: Arc<Mutex<Store>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut out = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (method, path) = (parts.next().unwrap_or("").to_owned(), parts.next().unwrap_or("").to_owned());
        let mut headers = BTreeMap::new();
        loop {
            let mut h = String::new();
            reader.read_line(&mut h).unwrap();
            if h.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
            }
        }
        let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body).unwrap();
        let rel = path.trim_start_matches("/dav").trim_matches('/').to_owned();
        let mut s = store.lock().unwrap();
        let (status, extra, payload): (u16, String, Vec<u8>) = if headers.get("authorization").map(String::as_str) != Some("Basic dTpw") {
            (401, String::new(), Vec::new())
        } else {
            match method.as_str() {
                "MKCOL" => {
                    if s.dirs.insert(rel) { (201, String::new(), vec![]) } else { (405, String::new(), vec![]) }
                }
                "GET" => {
                    s.gets += 1;
                    if rel.starts_with("files/") {
                        s.blob_gets += 1;
                    }
                    if s.fail_get.as_deref().is_some_and(|part| rel.contains(part)) {
                        s.fail_get = None;
                        (500, String::new(), vec![])
                    } else {
                        match s.files.get(&rel) {
                            Some((b, e)) => (200, format!("ETag: \"v{e}\"\r\n"), b.clone()),
                            None => (404, String::new(), vec![]),
                        }
                    }
                }
                "PUT" => {
                    let current = s.files.get(&rel).map(|(_, e)| format!("\"v{e}\""));
                    let ok = match (headers.get("if-none-match"), headers.get("if-match")) {
                        (Some(_), _) => current.is_none(),
                        (_, Some(m)) => current.as_deref() == Some(m.as_str()),
                        _ => true,
                    };
                    let sum = headers.get("oc-checksum").cloned();
                    let right = format!("sha256:{}", digest(&body));
                    if ok && s.checksums && sum.as_deref().is_some_and(|v| v != right) {
                        // A header that does not match the body is refused, as a server that checks it does.
                        s.bad_sums += 1;
                        (400, String::new(), vec![])
                    } else if ok {
                        s.next += 1;
                        s.puts += 1;
                        let e = if s.granularity == Granularity::Second { s.clock } else { s.next };
                        let created = current.is_none();
                        if s.checksums {
                            match sum {
                                Some(v) => {
                                    s.sums.insert(rel.clone(), v);
                                }
                                None => {
                                    s.bad_sums += 1;
                                    s.sums.remove(&rel);
                                }
                            }
                        }
                        s.files.insert(rel, (body, e));
                        (if created { 201 } else { 204 }, format!("ETag: \"v{e}\"\r\n"), vec![])
                    } else {
                        (412, String::new(), vec![])
                    }
                }
                "PROPFIND" => {
                    if !s.dirs.contains(&rel) && !rel.is_empty() {
                        (404, String::new(), vec![])
                    } else {
                        let prefix = if rel.is_empty() { String::new() } else { format!("{rel}/") };
                        let mut xml = String::from(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">"#);
                        xml.push_str(&format!("<d:response><d:href>/dav/{prefix}</d:href><d:propstat><d:prop><d:getetag>\"d\"</d:getetag><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>"));
                        for (p, (_, e)) in &s.files {
                            if let Some(name) = p.strip_prefix(&prefix) {
                                if !name.contains('/') {
                                    let sum = s.sums.get(p).filter(|_| s.checksums).map_or(String::new(), |v| format!("<oc:checksums><oc:checksum>{v}</oc:checksum></oc:checksums>"));
                                    xml.push_str(&format!("<d:response><d:href>/dav/{p}</d:href><d:propstat><d:prop><d:getetag>&quot;v{e}&quot;</d:getetag><d:resourcetype/>{sum}</d:prop></d:propstat></d:response>"));
                                }
                            }
                        }
                        xml.push_str("</d:multistatus>");
                        if rel == "files" {
                            if let Some(gone) = s.vanish.take() {
                                s.files.remove(&format!("files/{gone}"));
                            }
                        }
                        (207, "Content-Type: application/xml\r\n".into(), xml.into_bytes())
                    }
                }
                _ => (405, String::new(), vec![]),
            }
        };
        drop(s);
        let head = format!("HTTP/1.1 {status} X\r\n{extra}Content-Length: {}\r\nConnection: keep-alive\r\n\r\n", payload.len());
        if out.write_all(head.as_bytes()).and_then(|()| out.write_all(&payload)).is_err() {
            return;
        }
    }
}

fn server() -> Server {
    start(Granularity::PerWrite, false)
}

/// A server whose ETags move once per virtual second (the clock starts at 1000 and moves through
/// [`Server::set_second`]); `checksums` makes it keep and report the `OC-Checksum` of a write, like Nextcloud.
fn server_coarse(checksums: bool) -> Server {
    start(Granularity::Second, checksums)
}

fn start(granularity: Granularity, checksums: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/dav", listener.local_addr().unwrap().port());
    let store = Arc::new(Mutex::new(Store { granularity, checksums, clock: 1000, ..Store::default() }));
    let shared = store.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let store = shared.clone();
            std::thread::spawn(move || serve(stream, store));
        }
    });
    Server { url, store }
}

/// A device: its own folder and its own secret store, the same server.
struct Device {
    dir: PathBuf,
    secrets: Arc<MemoryStore>,
}

fn device(tag: &str) -> Device {
    let dir = std::env::temp_dir().join(format!("hw-sync-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    Device { dir, secrets: Arc::new(MemoryStore::default()) }
}

fn connect(server: &Server, d: &Device, pass: &str) -> Result<Syncer<WebDav, ConfigSource>> {
    let backend = WebDav::new(&server.url, "u", "p")?;
    Syncer::connect_with(backend, ConfigSource::new(&d.dir, d.secrets.clone()), &d.dir, pass, Kdf { m_kib: 8 * 1024, t: 1, p: 1 })
}

/// What the device has after its next start: staged downloads are moved into place first.
fn text(d: &Device, name: &str) -> Option<String> {
    promote_incoming(&d.dir);
    fs::read_to_string(d.dir.join(name)).ok()
}

/// An edit by the user, after the device started again (staged downloads applied).
fn put(d: &Device, name: &str, text: &str) {
    promote_incoming(&d.dir);
    write_atomic(&d.dir.join(name), text.as_bytes()).unwrap();
}

#[test]
fn a_file_goes_to_the_server_encrypted_and_to_a_second_device() {
    let srv = server();
    let (a, b) = (device("a1"), device("b1"));
    put(&a, "settings.json", "{\"font\":14}");
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.uploaded, vec!["settings.json"]);
    {
        let store = srv.store.lock().unwrap();
        let blob = &store.files["files/settings.json.hwx"].0;
        assert!(!blob.windows(4).any(|w| w == b"font"), "the server sees no content");
        assert!(store.files.contains_key("meta.json"));
    }
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.downloaded, vec!["settings.json"]);
    assert_eq!(text(&b, "settings.json").as_deref(), Some("{\"font\":14}"));
    // A second run changes nothing and writes nothing.
    let puts = srv.store.lock().unwrap().puts;
    let again = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(again, Report::default());
    assert_eq!(srv.store.lock().unwrap().puts, puts);
}

#[test]
fn a_wrong_passphrase_is_refused_before_any_file_is_touched() {
    let srv = server();
    let (a, b) = (device("a2"), device("b2"));
    put(&a, "layout.json", "{}");
    connect(&srv, &a, "right").unwrap().sync_once().unwrap();
    let e = connect(&srv, &b, "wrong").err().expect("refused");
    assert!(e.to_string().contains("passphrase is wrong"), "{e}");
    assert!(text(&b, "layout.json").is_none());
}

#[test]
fn the_wrong_app_password_says_so() {
    let srv = server();
    let a = device("a3");
    let backend = WebDav::new(&srv.url, "u", "nope").unwrap();
    let e = Syncer::connect_with(backend, ConfigSource::new(&a.dir, a.secrets.clone()), &a.dir, "pw", Kdf { m_kib: 8 * 1024, t: 1, p: 1 }).err().unwrap();
    assert!(e.to_string().contains("refused the user name or app password"), "{e}");
}

#[test]
fn a_change_on_one_side_follows_to_the_other_both_ways() {
    let srv = server();
    let (a, b) = (device("a4"), device("b4"));
    put(&a, "settings.json", "one");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    put(&b, "settings.json", "two");
    assert_eq!(connect(&srv, &b, "pw").unwrap().sync_once().unwrap().uploaded, vec!["settings.json"]);
    assert_eq!(connect(&srv, &a, "pw").unwrap().sync_once().unwrap().downloaded, vec!["settings.json"]);
    assert_eq!(text(&a, "settings.json").as_deref(), Some("two"));
}

#[test]
fn when_both_sides_changed_the_newer_wins_and_the_other_version_is_kept() {
    let srv = server();
    let (a, b) = (device("a5"), device("b5"));
    put(&a, "settings.json", "base");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    // A uploads first; B, which changed the same file meanwhile, syncs second and has the later clock.
    put(&a, "settings.json", "from a");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    put(&b, "settings.json", "from b");
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.conflicts, vec!["settings.json"]);
    assert_eq!(report.uploaded, vec!["settings.json"]);
    {
        let store = srv.store.lock().unwrap();
        assert!(store.files.keys().any(|k| k.starts_with("backups/settings.json.")), "the replaced server version is kept");
    }
    // A takes B's version, and keeps its own copy next to the files.
    put(&a, "settings.json", "a again");
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.conflicts, vec!["settings.json"]);
    let kept: Vec<_> = fs::read_dir(a.dir.join("sync-conflicts")).map(|r| r.flatten().collect()).unwrap_or_default();
    // Either side may have won the tie on the clock; whichever lost is kept.
    let server_has_backup = srv.store.lock().unwrap().files.keys().filter(|k| k.starts_with("backups/")).count() >= 1;
    assert!(!kept.is_empty() || server_has_backup);
}

#[test]
fn a_higher_counter_beats_a_later_clock() {
    let srv = server();
    let (a, b) = (device("a6"), device("b6"));
    put(&a, "settings.json", "v0");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    // The server's file is written twice by A (counter 3); B changed once (would be counter 2).
    for t in ["v1", "v2"] {
        put(&a, "settings.json", t);
        connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    }
    put(&b, "settings.json", "b only");
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.conflicts, vec!["settings.json"]);
    assert_eq!(report.downloaded, vec!["settings.json"], "the remote counter is higher: it wins");
    assert_eq!(text(&b, "settings.json").as_deref(), Some("v2"));
    let kept = fs::read_dir(b.dir.join("sync-conflicts")).unwrap().flatten().next().unwrap();
    assert_eq!(fs::read_to_string(kept.path()).unwrap(), "b only");
}

#[test]
fn themes_sync_and_secrets_travel_only_inside_the_encrypted_payload() {
    let srv = server();
    let (a, b) = (device("a7"), device("b7"));
    fs::create_dir_all(a.dir.join("themes")).unwrap();
    put(&a, "themes/dark.json", "{\"name\":\"dark\"}");
    let mut profiles = Profiles::default();
    profiles.upsert({ let mut p = crate::profiles::Profile::new("web", "Web", "h"); p.user = "u".into(); p.auth = crate::profiles::Auth::Password; p }).unwrap();
    profiles.save(&a.dir).unwrap();
    a.secrets.set("ssh:web", "hunter2hunter2").unwrap();
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    {
        let store = srv.store.lock().unwrap();
        assert!(store.files.values().all(|(b, _)| !b.windows(8).any(|w| w == b"hunter2h")), "no secret in clear on the server");
        assert!(store.files.contains_key("files/secrets.json.hwx"));
    }
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert!(report.downloaded.contains(&"themes/dark.json".to_owned()));
    assert!(!b.dir.join("themes").exists(), "a download is staged, the running app's files are not touched");
    assert_eq!(text(&b, "themes/dark.json").as_deref(), Some("{\"name\":\"dark\"}"));
    assert_eq!(b.secrets.get("ssh:web").unwrap().as_deref(), Some("hunter2hunter2"));
    assert!(!b.dir.join("secrets.json").exists(), "the secrets never become a file");
    assert!(fs::read_dir(&b.dir).unwrap().flatten().all(|e| !fs::read(e.path()).map_or(false, |c| c.windows(8).any(|w| w == b"hunter2h"))));
}

#[test]
fn a_staged_download_counts_as_the_local_version_and_is_not_sent_back() {
    let srv = server();
    let (a, b) = (device("a10"), device("b10"));
    put(&a, "settings.json", "from a");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    write_atomic(&b.dir.join("settings.json"), b"local edit made before the download was applied").unwrap();
    let puts = srv.store.lock().unwrap().puts;
    // The next run sees B's staged version as local; it does not upload the stale file.
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert!(report.uploaded.is_empty(), "{report:?}");
    assert_eq!(srv.store.lock().unwrap().puts, puts);
    // Applying replaces the file.
    assert_eq!(promote_incoming(&b.dir), vec!["settings.json"]);
    assert_eq!(text(&b, "settings.json").as_deref(), Some("from a"));
}

/// The files in `sync-conflicts/` of a device: name and content.
fn kept_copies(d: &Device) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = fs::read_dir(d.dir.join("sync-conflicts"))
        .map(|r| r.flatten().map(|e| (e.file_name().to_string_lossy().into_owned(), fs::read_to_string(e.path()).unwrap_or_default())).collect())
        .unwrap_or_default();
    out.sort();
    out
}

#[test]
fn a_local_edit_made_after_a_download_is_kept_before_the_download_is_applied() {
    let srv = server();
    let (a, b) = (device("a12"), device("b12"));
    put(&a, "settings.json", "from a");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    // B stages the download and has no live file; then the person writes one, and B starts again.
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    write_atomic(&b.dir.join("settings.json"), b"local edit").unwrap();
    let promoted = promote_incoming_with_report(&b.dir);
    assert_eq!(promoted, Promoted { applied: vec!["settings.json".into()], kept: vec!["settings.json".into()], skipped: vec![], ..Promoted::default() });
    assert_eq!(fs::read_to_string(b.dir.join("settings.json")).unwrap(), "from a");
    let copies = kept_copies(&b);
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert!(copies[0].0.starts_with("settings.json."), "{copies:?}");
    assert_eq!(copies[0].1, "local edit");
    let notice: serde_json::Value = serde_json::from_slice(&fs::read(b.dir.join("sync-kept.json")).unwrap()).unwrap();
    assert_eq!(notice, serde_json::json!({"kept": ["settings.json"]}));
    // The first run after the start says so through the conflicts, and removes the notice.
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.conflicts, vec!["settings.json"], "{report:?}");
    assert!(!b.dir.join("sync-kept.json").exists());
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert!(report.conflicts.is_empty(), "{report:?}");
}

/// B has `name` live with `first`, A uploads it and B's next run stages it. Returns the two devices.
fn staged_over(srv: &Server, tag: &str, name: &str, first: &str, then: &str) -> (Device, Device) {
    let (a, b) = (device(&format!("a{tag}")), device(&format!("b{tag}")));
    put(&a, name, first);
    connect(srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(text(&b, name).as_deref(), Some(first));
    put(&a, name, then);
    connect(srv, &a, "pw").unwrap().sync_once().unwrap();
    assert_eq!(connect(srv, &b, "pw").unwrap().sync_once().unwrap().downloaded, vec![name.to_owned()]);
    (a, b)
}

#[test]
fn a_download_over_an_unedited_file_keeps_no_copy() {
    let srv = server();
    let (_a, b) = staged_over(&srv, "13", "settings.json", "v1", "v2");
    let promoted = promote_incoming_with_report(&b.dir);
    assert_eq!(promoted, Promoted { applied: vec!["settings.json".into()], kept: vec![], skipped: vec![], ..Promoted::default() });
    assert_eq!(fs::read_to_string(b.dir.join("settings.json")).unwrap(), "v2");
    assert!(!b.dir.join("sync-conflicts").exists() && !b.dir.join("sync-kept.json").exists());
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert!(report.conflicts.is_empty(), "{report:?}");
}

#[test]
fn a_second_download_before_the_start_keeps_the_first_base() {
    let srv = server();
    let (a, b) = staged_over(&srv, "14", "settings.json", "v1", "v2");
    put(&a, "settings.json", "v3");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert_eq!(connect(&srv, &b, "pw").unwrap().sync_once().unwrap().downloaded, vec!["settings.json"]);
    let promoted = promote_incoming_with_report(&b.dir);
    assert_eq!((promoted.applied.len(), promoted.kept.len()), (1, 0), "the live file still was v1: {promoted:?}");
    assert_eq!(fs::read_to_string(b.dir.join("settings.json")).unwrap(), "v3");
    assert!(!b.dir.join("sync-conflicts").exists());
    // With an edit between the two downloads the first base still stands, so the edit is copied.
    put(&a, "settings.json", "v4");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    write_atomic(&b.dir.join("settings.json"), b"edit").unwrap();
    put(&a, "settings.json", "v5");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    let promoted = promote_incoming_with_report(&b.dir);
    assert_eq!(promoted.kept, vec!["settings.json"], "{promoted:?}");
    assert_eq!(fs::read_to_string(b.dir.join("settings.json")).unwrap(), "v5");
    let copies = kept_copies(&b);
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(copies[0].1, "edit");
}

#[test]
fn a_name_in_a_folder_is_copied_under_a_flat_name() {
    let srv = server();
    let (a, b) = (device("a15"), device("b15"));
    put(&a, "themes/dark.json", "{\"name\":\"dark\"}");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    write_atomic(&b.dir.join("themes").join("dark.json"), b"mine").unwrap();
    let promoted = promote_incoming_with_report(&b.dir);
    assert_eq!(promoted.kept, vec!["themes/dark.json"], "{promoted:?}");
    let copies = kept_copies(&b);
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert!(copies[0].0.starts_with("themes.dark.json."), "{copies:?}");
    assert_eq!(copies[0].1, "mine");
    assert_eq!(fs::read_to_string(b.dir.join("themes").join("dark.json")).unwrap(), "{\"name\":\"dark\"}");
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.conflicts, vec!["themes/dark.json"], "{report:?}");
}

#[test]
fn a_copy_that_cannot_be_written_leaves_the_download_staged_and_the_live_file_alone() {
    let srv = server();
    let (a, b) = (device("a16"), device("b16"));
    put(&a, "settings.json", "from a");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    write_atomic(&b.dir.join("settings.json"), b"my edit").unwrap();
    fs::write(b.dir.join("sync-conflicts"), "in the way").unwrap();
    let promoted = promote_incoming_with_report(&b.dir);
    assert_eq!(promoted, Promoted { applied: vec![], kept: vec![], skipped: vec!["settings.json".into()], ..Promoted::default() });
    assert_eq!(fs::read_to_string(b.dir.join("settings.json")).unwrap(), "my edit", "the live file is untouched");
    assert!(b.dir.join("sync-incoming").join("settings.json").is_file(), "the download waits");
    assert!(b.dir.join("sync-incoming").join(".live-at-stage.json").is_file(), "its base waits with it");
    assert!(!b.dir.join("sync-kept.json").exists(), "nothing was kept, so nothing is reported");
    // The next start, with the way free, applies the download and keeps the copy.
    fs::remove_file(b.dir.join("sync-conflicts")).unwrap();
    let promoted = promote_incoming_with_report(&b.dir);
    assert_eq!(promoted, Promoted { applied: vec!["settings.json".into()], kept: vec!["settings.json".into()], skipped: vec![], ..Promoted::default() });
    assert_eq!(fs::read_to_string(b.dir.join("settings.json")).unwrap(), "from a");
    assert_eq!(kept_copies(&b).iter().map(|c| c.1.as_str()).collect::<Vec<_>>(), vec!["my edit"]);
    assert!(!b.dir.join("sync-incoming").exists());
}

#[test]
fn a_file_that_only_exists_after_staging_counts_as_edited() {
    let srv = server();
    let (a, b) = (device("a17"), device("b17"));
    put(&a, "settings.json", "from a");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    let base: serde_json::Value = serde_json::from_slice(&fs::read(b.dir.join("sync-incoming").join(".live-at-stage.json")).unwrap()).unwrap();
    assert_eq!(base, serde_json::json!({"settings.json": null}), "no live file at staging is a null");
    write_atomic(&b.dir.join("settings.json"), b"appeared later").unwrap();
    assert_eq!(promote_incoming_with_report(&b.dir).kept, vec!["settings.json"]);
}

#[test]
fn the_secrets_are_never_staged_copied_or_noticed() {
    let srv = server();
    let d = device("a18");
    let source = ConfigSource::new(&d.dir, d.secrets.clone());
    source.write(SECRETS, br#"{"ssh:ok":"hunter2hunter2"}"#).unwrap();
    assert!(!d.dir.join("sync-incoming").exists(), "the secrets never become a staged file");
    // Whatever is planted, a promote applies and copies nothing for them.
    fs::create_dir_all(d.dir.join("sync-incoming")).unwrap();
    fs::write(d.dir.join("sync-incoming").join("secrets.json"), "{\"ssh:ok\":\"planted\"}").unwrap();
    fs::write(d.dir.join("secrets.json"), "live").unwrap();
    let promoted = promote_incoming_with_report(&d.dir);
    assert_eq!(promoted, Promoted::default());
    assert_eq!(fs::read_to_string(d.dir.join("secrets.json")).unwrap(), "live");
    assert!(!d.dir.join("sync-conflicts").exists() && !d.dir.join("sync-kept.json").exists() && !d.dir.join("sync-incoming").exists());
    // A notice that names them reports nothing.
    fs::write(d.dir.join("sync-kept.json"), r#"{"kept":["secrets.json"]}"#).unwrap();
    let report = connect(&srv, &d, "pw").unwrap().sync_once().unwrap();
    assert!(report.conflicts.is_empty(), "{report:?}");
}

#[test]
fn a_damaged_or_oversized_sidecar_and_notice_are_ignored() {
    let srv = server();
    let (a, b) = (device("a19"), device("b19"));
    put(&a, "settings.json", "from a");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    write_atomic(&b.dir.join("settings.json"), b"my edit").unwrap();
    let sidecar = b.dir.join("sync-incoming").join(".live-at-stage.json");
    // Every damaged form means "no base": the differing live file is copied, nothing panics, the promote goes on.
    let big = format!("{{\"settings.json\":\"{}\"}}", "a".repeat(300 * 1024));
    for (i, damaged) in ["not json at all".to_owned(), r#"{"settings.json": 5}"#.to_owned(), r#"["settings.json"]"#.to_owned(), big].into_iter().enumerate() {
        let incoming = b.dir.join("sync-incoming");
        fs::create_dir_all(&incoming).unwrap();
        fs::write(incoming.join("settings.json"), format!("download {i}")).unwrap();
        fs::write(&sidecar, damaged).unwrap();
        fs::write(b.dir.join("settings.json"), format!("edit {i}")).unwrap();
        let promoted = promote_incoming_with_report(&b.dir);
        assert_eq!(promoted.kept, vec!["settings.json"], "case {i}: {promoted:?}");
        assert_eq!(fs::read_to_string(b.dir.join("settings.json")).unwrap(), format!("download {i}"));
        assert!(kept_copies(&b).iter().any(|c| c.1 == format!("edit {i}")), "case {i}");
    }
    // A sidecar that names things that are not synced does not make them synced.
    fs::create_dir_all(b.dir.join("sync-incoming")).unwrap();
    fs::write(&sidecar, r#"{"../x":null,"secrets.json":null,"other.json":"x"}"#).unwrap();
    assert_eq!(promote_incoming_with_report(&b.dir), Promoted::default());
    // A damaged notice never blocks a run and never names a file that is not synced.
    for damaged in ["garbage".to_owned(), r#"{"kept":5}"#.to_owned(), format!("{{\"kept\":[\"settings.json\"],\"pad\":\"{}\"}}", "a".repeat(300 * 1024))] {
        fs::write(b.dir.join("sync-kept.json"), damaged).unwrap();
        let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
        assert!(report.conflicts.is_empty(), "{report:?}");
        assert!(!b.dir.join("sync-kept.json").exists());
    }
    fs::write(b.dir.join("sync-kept.json"), r#"{"kept":["../x","secrets.json","other.json","themes/a.b.json","settings.json","settings.json"]}"#).unwrap();
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.conflicts, vec!["settings.json"], "{report:?}");
}

#[test]
fn promoting_ignores_names_that_are_not_synced() {
    let d = device("a11");
    let incoming = d.dir.join("sync-incoming");
    fs::create_dir_all(incoming.join("themes")).unwrap();
    fs::write(incoming.join("evil.sh"), "x").unwrap();
    fs::write(incoming.join("secrets.json"), "{}").unwrap();
    fs::write(incoming.join("themes").join("a.b.json"), "x").unwrap();
    fs::write(incoming.join("layout.json"), "{}").unwrap();
    assert_eq!(promote_incoming(&d.dir), vec!["layout.json"]);
    assert!(!d.dir.join("evil.sh").exists() && !d.dir.join("secrets.json").exists() && !incoming.exists());
}

#[test]
fn a_hostile_server_cannot_write_outside_the_synced_names() {
    assert!(allowed("settings.json"));
    assert!(allowed("themes/dark-2.json") && allowed("widgets/deploy.json") && allowed("widget_notes.json"));
    for bad in ["../x", "themes/../x.json", "themes/a.b.json", "themes/.json", "other.json", "themes/x.json/y", "/etc/passwd", "widgets/a.b.json", "other/a.json", "widgets/../x.json"] {
        assert!(!allowed(bad), "{bad}");
    }
    assert_eq!(logical_name("themes.dark.json.hwx").as_deref(), Some("themes/dark.json"));
    assert_eq!(logical_name("widgets.deploy.json.hwx").as_deref(), Some("widgets/deploy.json"));
    assert_eq!(logical_name("evil.hwx"), None);
    assert_eq!(logical_name("settings.json"), None);
    // The secrets file accepts only connection accounts.
    let d = device("a8");
    let source = ConfigSource::new(&d.dir, d.secrets.clone());
    assert!(source.write(SECRETS, br#"{"keychain:other":"x"}"#).is_err());
    assert!(source.write(SECRETS, br#"{"ssh:../x":"x"}"#).is_err());
    assert!(source.write(SECRETS, br#"{"ssh:ok":"x"}"#).is_ok());
}

#[test]
fn a_file_swapped_on_the_server_for_another_names_blob_is_not_applied() {
    let srv = server();
    let (a, b) = (device("a9"), device("b9"));
    put(&a, "settings.json", "s");
    put(&a, "layout.json", "{}");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    {
        let mut store = srv.store.lock().unwrap();
        let layout = store.files["files/layout.json.hwx"].0.clone();
        store.files.get_mut("files/settings.json.hwx").unwrap().0 = layout;
    }
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert!(report.errors.iter().any(|(n, e)| n == "settings.json" && e.contains("could not be decrypted")), "{report:?}");
    assert!(text(&b, "settings.json").is_none());
    assert_eq!(text(&b, "layout.json").as_deref(), Some("{}"), "the other files still sync");
}

/// Stage `name` with `text` in the staging folder of `d`, the way a download does (no record of a live hash).
fn stage(d: &Device, name: &str, text: &str) {
    write_atomic(&d.dir.join("sync-incoming").join(name), text.as_bytes()).unwrap();
}

#[test]
fn promote_where_moves_only_what_is_wanted() {
    let d = device("w1");
    stage(&d, "themes/dark.json", "{\"name\":\"dark\"}");
    stage(&d, "settings.json", "{\"font_size\":20}");
    stage(&d, "widget_notes.json", "{}");
    let promoted = promote_where(&d.dir, &is_unowned);
    assert_eq!(promoted, Promoted { applied: vec!["themes/dark.json".into(), "widget_notes.json".into()], kept: vec![], skipped: vec![], ..Promoted::default() });
    assert!(d.dir.join("themes").join("dark.json").is_file() && d.dir.join("widget_notes.json").is_file());
    assert!(!d.dir.join("settings.json").exists(), "an owned file is not applied");
    let incoming = d.dir.join("sync-incoming");
    assert!(incoming.join("settings.json").is_file(), "it stays staged");
    assert!(!incoming.join("themes").join("dark.json").exists());
    assert!(incoming.is_dir(), "the staging folder stays while a staged name is left");
    // The rest follows with everything wanted, and the folder goes.
    let promoted = promote_where(&d.dir, &|_| true);
    assert_eq!(promoted.applied, vec!["settings.json"]);
    assert_eq!(fs::read_to_string(d.dir.join("settings.json")).unwrap(), "{\"font_size\":20}");
    assert!(!incoming.exists());
}

#[test]
fn the_record_of_a_name_that_waits_waits_with_it() {
    let srv = server();
    let (a, b) = (device("wa2"), device("wb2"));
    put(&a, "settings.json", "first");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(text(&b, "settings.json").as_deref(), Some("first"));
    put(&a, "settings.json", "from a");
    put(&a, "themes/dark.json", "{\"name\":\"dark\"}");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    let incoming = b.dir.join("sync-incoming");
    assert!(incoming.join("settings.json").is_file() && incoming.join("themes").join("dark.json").is_file());
    let promoted = promote_where(&b.dir, &is_unowned);
    assert_eq!(promoted.applied, vec!["themes/dark.json"]);
    let record: BTreeMap<String, Option<String>> = serde_json::from_slice(&fs::read(incoming.join(STAGE_BASE)).unwrap()).unwrap();
    assert_eq!(record.keys().cloned().collect::<Vec<_>>(), vec!["settings.json"], "{record:?}");
    // The start-up promote then still sees that the live settings.json was not touched since staging: no copy.
    let promoted = promote_incoming_with_report(&b.dir);
    assert_eq!(promoted, Promoted { applied: vec!["settings.json".into()], kept: vec![], skipped: vec![], ..Promoted::default() });
    assert_eq!(fs::read_to_string(b.dir.join("settings.json")).unwrap(), "from a");
    assert!(!incoming.exists());
}

#[test]
fn promote_incoming_is_promote_where_with_everything() {
    let d = device("w3");
    stage(&d, "layout.json", "{}");
    stage(&d, "themes/a.json", "{}");
    stage(&d, "settings.json", "{}");
    let via_wrapper = promote_incoming_with_report(&d.dir);
    let d2 = device("w4");
    stage(&d2, "layout.json", "{}");
    stage(&d2, "themes/a.json", "{}");
    stage(&d2, "settings.json", "{}");
    assert_eq!(via_wrapper, promote_where(&d2.dir, &|_| true));
    assert_eq!(via_wrapper.applied, vec!["layout.json", "settings.json", "themes/a.json"]);
}

#[test]
fn the_unowned_names_are_themes_widgets_and_the_notes_and_nothing_the_app_rewrites() {
    for yes in ["themes/dark.json", "widgets/deploy.json", "widget_notes.json"] {
        assert!(is_unowned(yes), "{yes}");
    }
    for no in ["layout.json", "settings.json", "connections.json", "secrets.json", "themes/a.b.json", "themes/../settings.json", "evil.json", "widgets/x.txt", ""] {
        assert!(!is_unowned(no), "{no}");
    }
}

// ---- the layout and the pane assignments that belong to it --------------------------------------------------------

/// Save connections with these ids (key login, no secret) in the folder of `d`.
fn save_connections(d: &Device, ids: &[&str]) {
    let mut profiles = Profiles::default();
    for id in ids {
        let mut p = crate::profiles::Profile::new(*id, *id, "host.example");
        p.user = "u".into();
        p.key_path = "~/.ssh/id_ed25519".into();
        profiles.upsert(p).unwrap();
    }
    profiles.save(&d.dir).unwrap();
}

/// Two devices that know the same saved connections `web-1` and `db` (synced earlier), each with its own pane files.
/// A runs `web-1` in its pane 2 and has a layout with the panes 1, 2 and 3; B shows a local preview in its pane 2 and
/// runs `db` in its pane 3 and has no layout yet. A has synced, B has not synced the layout.
fn pair(srv: &Server, tag: &str) -> (Device, Device) {
    let (a, b) = (device(&format!("a-{tag}")), device(&format!("b-{tag}")));
    save_connections(&a, &["web-1", "db"]);
    connect(srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(promote_incoming(&b.dir), vec!["connections.json"]);
    write_atomic(&b.dir.join(PANES_FILE), br#"{"3":"db"}"#).unwrap();
    write_atomic(&b.dir.join(BLOCKS_FILE), br#"{"2":{"kind":"preview","source":"local","path":"/tmp/b-notes.md"}}"#).unwrap();
    write_atomic(&a.dir.join("layout.json"), br#"{"panes":[1,2,3]}"#).unwrap();
    write_atomic(&a.dir.join(PANES_FILE), br#"{"2":"web-1"}"#).unwrap();
    (a, b)
}

fn json(d: &Device, file: &str) -> serde_json::Value {
    serde_json::from_slice(&fs::read(d.dir.join(file)).unwrap()).unwrap()
}

/// The unit a device would send now, as JSON.
fn unit_of(d: &Device) -> serde_json::Value {
    serde_json::from_slice(&live_unit(&d.dir).unwrap().expect("a layout exists")).unwrap()
}

/// The counter the device remembers for `name`.
fn counter(d: &Device, name: &str) -> u64 {
    json(d, "sync_state.json")["files"][name]["counter"].as_u64().unwrap()
}

/// The defect of the old behaviour, turned around: a layout adopted from another device brings that device's pane
/// assignments that make sense here (the pane that runs `web-1`) and drops this device's own, in one step, and the
/// replaced files are kept. Before the unit existed the same two directories gave the opposite on every line: B's pane 2
/// stayed a local preview, B's pane 3 stayed on `db` and `profile_of(2)` was `None`.
#[test]
fn an_adopted_layout_brings_its_own_pane_assignments_and_drops_this_devices() {
    let srv = server();
    let (a, b) = pair(&srv, "adopt");
    assert_eq!(connect(&srv, &a, "pw").unwrap().sync_once().unwrap().uploaded, vec!["layout.json"]);
    assert_eq!(connect(&srv, &b, "pw").unwrap().sync_once().unwrap().downloaded, vec!["layout.json"]);
    let promoted = promote_incoming_with_report(&b.dir);
    assert_eq!(promoted, Promoted { applied: vec!["layout.json".into()], kept: vec![], skipped: vec![], ..Promoted::default() });
    assert_eq!(json(&b, "layout.json"), serde_json::json!({"panes": [1, 2, 3]}));

    let panes = [1, 2, 3];
    let (connections, _) = crate::connections::Connections::load(&b.dir, b.secrets.clone(), None, &panes);
    let blocks = crate::blocks::Blocks::load(&b.dir, None, &panes);
    assert_eq!(connections.profile_of(2).map(|p| p.id), Some("web-1".to_owned()), "A's connected pane comes up on B");
    assert!(connections.profile_of(3).is_none(), "B's own pane-3 assignment is gone");
    assert!(blocks.block_of(2).is_none(), "B's local preview is gone");
    assert_eq!(json(&b, PANES_FILE), serde_json::json!({"2": "web-1"}));
    assert_eq!(json(&b, BLOCKS_FILE), serde_json::json!({}));
    // A's files are untouched.
    assert_eq!(fs::read_to_string(a.dir.join(PANES_FILE)).unwrap(), r#"{"2":"web-1"}"#);
    assert_eq!(fs::read_to_string(a.dir.join("layout.json")).unwrap(), r#"{"panes":[1,2,3]}"#);
    // What was replaced is kept, under the file name and the time.
    let copies = kept_copies(&b);
    assert_eq!(copies.len(), 2, "{copies:?}");
    assert!(copies[0].0.starts_with("pane_blocks.json.") && copies[0].1.contains("/tmp/b-notes.md"), "{copies:?}");
    assert!(copies[1].0.starts_with("pane_connections.json.") && copies[1].1 == r#"{"3":"db"}"#, "{copies:?}");
    // The adoption leaves B level with the server: a run sends nothing and finds no conflict.
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report, Report::default());
}

#[test]
fn a_device_that_never_adopts_keeps_its_assignments_and_building_the_unit_changes_nothing() {
    let srv = server();
    let (a, _b) = pair(&srv, "keep");
    write_atomic(&a.dir.join(BLOCKS_FILE), br#"{"1":{"kind":"files","source":"local","path":"/home/a"}}"#).unwrap();
    let before: Vec<Vec<u8>> = ["layout.json", PANES_FILE, BLOCKS_FILE].iter().map(|f| fs::read(a.dir.join(f)).unwrap()).collect();
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    let after: Vec<Vec<u8>> = ["layout.json", PANES_FILE, BLOCKS_FILE].iter().map(|f| fs::read(a.dir.join(f)).unwrap()).collect();
    assert_eq!(before, after, "the files of the sending device are byte for byte as they were");
    assert!(!a.dir.join("sync-conflicts").exists() && !a.dir.join("sync-incoming").exists());
}

#[test]
fn a_local_only_block_is_not_shared() {
    let srv = server();
    let (a, b) = pair(&srv, "local");
    write_atomic(
        &a.dir.join(BLOCKS_FILE),
        br#"{
            "1": {"kind":"files","source":"local","path":"/home/a-secret-folder"},
            "2": {"kind":"preview","source":"local","path":"/home/a-secret-folder/notes.md"},
            "3": {"kind":"chat","source":"local","path":"a-chat-id"},
            "4": {"kind":"monitor","source":"local","path":""},
            "5": {"kind":"files","source":"web-1","path":"/srv/www"}
        }"#,
    )
    .unwrap();
    let unit = unit_of(&a);
    assert_eq!(unit["blocks"].as_object().unwrap().keys().cloned().collect::<Vec<_>>(), vec!["4", "5"], "{unit}");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    {
        // The server holds an encrypted blob only; the unit bytes themselves hold nothing local.
        let bytes = ConfigSource::new(&a.dir, a.secrets.clone()).read("layout.json").unwrap().unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("a-secret-folder") && !String::from_utf8_lossy(&bytes).contains("a-chat-id"));
    }
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    promote_incoming(&b.dir);
    let blocks = crate::blocks::Blocks::load(&b.dir, None, &[1, 2, 3, 4, 5]);
    assert!((1..=3).all(|pane| blocks.block_of(pane).is_none()), "no local block arrives");
    assert_eq!(blocks.block_of(4).map(|b| (b.kind, b.source)), Some((crate::blocks::BlockKind::Monitor, "local".to_owned())));
    assert_eq!(blocks.block_of(5).map(|b| (b.kind, b.source, b.path)), Some((crate::blocks::BlockKind::Files, "web-1".to_owned(), "/srv/www".to_owned())));
    // Even a unit that carries a local block does not bring it in: what is shared is what `portable` accepts.
    let hostile = br#"{"unit":1,"layout":{},"blocks":{"1":{"kind":"files","source":"local","path":"/etc"},"2":{"kind":"files","source":"web-1","path":"/srv"}}}"#;
    ConfigSource::new(&b.dir, b.secrets.clone()).write("layout.json", hostile).unwrap();
    promote_incoming(&b.dir);
    assert_eq!(json(&b, BLOCKS_FILE).as_object().unwrap().keys().cloned().collect::<Vec<_>>(), vec!["2"]);
}

#[test]
fn a_change_of_the_assignments_alone_is_a_change() {
    let srv = server();
    let (a, b) = pair(&srv, "change");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    promote_incoming(&b.dir);
    let first = counter(&a, "layout.json");
    assert_eq!(connect(&srv, &a, "pw").unwrap().sync_once().unwrap(), Report::default(), "nothing changed, nothing sent");
    // Only `pane_connections.json` of A changes: the layout file is untouched.
    write_atomic(&a.dir.join(PANES_FILE), br#"{"1":"db","2":"web-1"}"#).unwrap();
    assert_eq!(connect(&srv, &a, "pw").unwrap().sync_once().unwrap().uploaded, vec!["layout.json"]);
    assert_eq!(counter(&a, "layout.json"), first + 1);
    assert_eq!(connect(&srv, &b, "pw").unwrap().sync_once().unwrap().downloaded, vec!["layout.json"]);
    promote_incoming(&b.dir);
    assert_eq!(json(&b, PANES_FILE), serde_json::json!({"1": "db", "2": "web-1"}));
}

#[test]
fn the_staged_unit_counts_as_the_local_version() {
    let srv = server();
    let (a, b) = pair(&srv, "staged");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    let sent = ConfigSource::new(&a.dir, a.secrets.clone()).read("layout.json").unwrap().unwrap();
    let staged = fs::read(b.dir.join("sync-incoming").join("layout.json")).unwrap();
    assert_eq!(staged, sent, "the staged unit is stored verbatim");
    assert_eq!(ConfigSource::new(&b.dir, b.secrets.clone()).read("layout.json").unwrap().unwrap(), sent, "and read back as the local version");
    let puts = srv.store.lock().unwrap().puts;
    assert_eq!(connect(&srv, &b, "pw").unwrap().sync_once().unwrap(), Report::default(), "nothing goes back");
    assert_eq!(srv.store.lock().unwrap().puts, puts);
    // After the apply B's live state builds the same canonical unit as A's, so it is still level with the server.
    promote_incoming(&b.dir);
    assert_eq!(live_unit(&b.dir).unwrap().unwrap(), sent);
    assert_eq!(connect(&srv, &b, "pw").unwrap().sync_once().unwrap(), Report::default());
    assert_eq!(srv.store.lock().unwrap().puts, puts);
}

#[test]
fn the_same_content_on_both_sides_is_not_a_conflict() {
    let srv = server();
    let (a, b) = (device("a-same"), device("b-same"));
    for d in [&a, &b] {
        write_atomic(&d.dir.join("layout.json"), br#"{"panes":[1]}"#).unwrap();
    }
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    // B changed the same file in the same way before it ever synced: both sides are "changed" and equal.
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report, Report::default(), "{report:?}");
    assert!(!b.dir.join("sync-conflicts").exists());
    assert!(!srv.store.lock().unwrap().files.keys().any(|k| k.starts_with("backups/")));
    assert_eq!(connect(&srv, &b, "pw").unwrap().sync_once().unwrap(), Report::default());
}

#[test]
fn a_legacy_plain_layout_is_accepted_and_clears_the_local_assignments() {
    let d = device("legacy");
    write_atomic(&d.dir.join("layout.json"), br#"{"old":true}"#).unwrap();
    write_atomic(&d.dir.join(PANES_FILE), br#"{"3":"db"}"#).unwrap();
    write_atomic(&d.dir.join(BLOCKS_FILE), br#"{"2":{"kind":"preview","source":"local","path":"/tmp/n.md"}}"#).unwrap();
    // What an older build uploaded: the layout alone, no `unit` member.
    ConfigSource::new(&d.dir, d.secrets.clone()).write("layout.json", br#"{"panes":[7,8]}"#).unwrap();
    let promoted = promote_incoming_with_report(&d.dir);
    assert_eq!(promoted.applied, vec!["layout.json"]);
    assert_eq!(json(&d, "layout.json"), serde_json::json!({"panes": [7, 8]}));
    assert_eq!((json(&d, PANES_FILE), json(&d, BLOCKS_FILE)), (serde_json::json!({}), serde_json::json!({})));
    let names: Vec<String> = kept_copies(&d).into_iter().map(|c| c.0.split('.').next().unwrap_or_default().to_owned()).collect();
    assert!(names.contains(&"pane_connections".to_owned()) && names.contains(&"pane_blocks".to_owned()), "{names:?}");
}

#[test]
fn a_hostile_unit_is_refused_before_it_is_staged() {
    let d = device("hostile");
    let source = ConfigSource::new(&d.dir, d.secrets.clone());
    let many = |n: usize| (0..n).map(|i| format!("\"{i}\":\"db\"")).collect::<Vec<_>>().join(",");
    let many_blocks = |n: usize| (0..n).map(|i| format!("\"{i}\":{{\"kind\":\"files\",\"source\":\"db\",\"path\":\"/\"}}")).collect::<Vec<_>>().join(",");
    let huge = format!("{{\"unit\":1,\"layout\":{{\"x\":\"{}\"}}}}", "a".repeat(MAX_FILE as usize));
    let bad: Vec<(&str, String)> = vec![
        ("not json", "layout".into()),
        ("not an object", "[1,2]".into()),
        ("layout is an array", r#"{"unit":1,"layout":[1]}"#.into()),
        ("layout is a number", r#"{"unit":1,"layout":5}"#.into()),
        ("layout missing", r#"{"unit":1}"#.into()),
        ("unit 2", r#"{"unit":2,"layout":{}}"#.into()),
        ("unit is text", r#"{"unit":"1","layout":{}}"#.into()),
        ("unknown member", r#"{"unit":1,"layout":{},"extra":1}"#.into()),
        ("pane key is text", r#"{"unit":1,"layout":{},"connections":{"x":"db"}}"#.into()),
        ("pane key is negative", r#"{"unit":1,"layout":{},"connections":{"-1":"db"}}"#.into()),
        ("block pane key is text", r#"{"unit":1,"layout":{},"blocks":{"x":{"kind":"files","source":"db","path":"/"}}}"#.into()),
        ("connection id with a slash", r#"{"unit":1,"layout":{},"connections":{"1":"a/b"}}"#.into()),
        ("connection id is dots", r#"{"unit":1,"layout":{},"connections":{"1":".."}}"#.into()),
        ("connection id escapes", r#"{"unit":1,"layout":{},"connections":{"1":"../x"}}"#.into()),
        ("connection id over 64 bytes", format!("{{\"unit\":1,\"layout\":{{}},\"connections\":{{\"1\":\"{}\"}}}}", "a".repeat(65))),
        ("connection id is a number", r#"{"unit":1,"layout":{},"connections":{"1":5}}"#.into()),
        ("block with an empty source", r#"{"unit":1,"layout":{},"blocks":{"1":{"kind":"files","source":"","path":"/"}}}"#.into()),
        ("block source over 64 bytes", format!("{{\"unit\":1,\"layout\":{{}},\"blocks\":{{\"1\":{{\"kind\":\"files\",\"source\":\"{}\",\"path\":\"/\"}}}}}}", "a".repeat(65))),
        ("block path with a NUL", r#"{"unit":1,"layout":{},"blocks":{"1":{"kind":"files","source":"db","path":"/a\u0000b"}}}"#.into()),
        ("block path over 4096 bytes", format!("{{\"unit\":1,\"layout\":{{}},\"blocks\":{{\"1\":{{\"kind\":\"files\",\"source\":\"db\",\"path\":\"/{}\"}}}}}}", "a".repeat(4096))),
        ("block of an unknown kind", r#"{"unit":1,"layout":{},"blocks":{"1":{"kind":"shell","source":"db","path":"/"}}}"#.into()),
        ("4097 connections", format!("{{\"unit\":1,\"layout\":{{}},\"connections\":{{{}}}}}", many(4097))),
        ("4097 blocks", format!("{{\"unit\":1,\"layout\":{{}},\"blocks\":{{{}}}}}", many_blocks(4097))),
        ("over the file limit", huge),
    ];
    for (what, bytes) in &bad {
        assert!(source.write("layout.json", bytes.as_bytes()).is_err(), "{what} must be refused");
        assert!(!d.dir.join("sync-incoming").exists(), "{what}: nothing is staged");
    }
    // The bounds are exact: 4096 entries and a 64-byte id still pass, a plain layout object passes.
    for good in [
        format!("{{\"unit\":1,\"layout\":{{}},\"connections\":{{{}}}}}", many(4096)),
        format!("{{\"unit\":1,\"layout\":{{}},\"blocks\":{{{}}}}}", many_blocks(4096)),
        format!("{{\"unit\":1,\"layout\":{{}},\"connections\":{{\"1\":\"{}\"}}}}", "a".repeat(64)),
        r#"{"unit":1,"layout":{"a":1}}"#.to_owned(),
        r#"{"anything":"as a plain layout"}"#.to_owned(),
    ] {
        source.write("layout.json", good.as_bytes()).unwrap();
        assert_eq!(fs::read(d.dir.join("sync-incoming").join("layout.json")).unwrap(), good.as_bytes(), "staged verbatim");
    }
}

#[test]
fn a_planted_staged_layout_that_is_not_one_is_removed_and_not_applied() {
    let d = device("planted");
    write_atomic(&d.dir.join("layout.json"), br#"{"mine":1}"#).unwrap();
    write_atomic(&d.dir.join(PANES_FILE), br#"{"3":"db"}"#).unwrap();
    for planted in ["not json", r#"{"unit":1,"layout":5}"#, r#"{"unit":1,"layout":{},"connections":{"1":"../x"}}"#] {
        stage(&d, "layout.json", planted);
        assert_eq!(promote_incoming_with_report(&d.dir), Promoted::default(), "{planted}");
        assert_eq!(json(&d, "layout.json"), serde_json::json!({"mine": 1}));
        assert_eq!(json(&d, PANES_FILE), serde_json::json!({"3": "db"}), "the live pane file is untouched");
        assert!(!d.dir.join("sync-incoming").exists());
    }
}

#[test]
fn a_failed_backup_stops_the_adoption() {
    let d = device("nobackup");
    write_atomic(&d.dir.join("layout.json"), br#"{"mine":1}"#).unwrap();
    write_atomic(&d.dir.join(PANES_FILE), br#"{"3":"db"}"#).unwrap();
    write_atomic(&d.dir.join(BLOCKS_FILE), br#"{"2":{"kind":"preview","source":"local","path":"/tmp/n.md"}}"#).unwrap();
    let unit = br#"{"unit":1,"layout":{"theirs":1},"connections":{"2":"web-1"}}"#;
    ConfigSource::new(&d.dir, d.secrets.clone()).write("layout.json", unit).unwrap();
    fs::write(d.dir.join("sync-conflicts"), "in the way").unwrap();
    let live: Vec<Vec<u8>> = ["layout.json", PANES_FILE, BLOCKS_FILE].iter().map(|f| fs::read(d.dir.join(f)).unwrap()).collect();
    let promoted = promote_incoming_with_report(&d.dir);
    assert_eq!(promoted, Promoted { applied: vec![], kept: vec![], skipped: vec!["layout.json".into()], ..Promoted::default() });
    let after: Vec<Vec<u8>> = ["layout.json", PANES_FILE, BLOCKS_FILE].iter().map(|f| fs::read(d.dir.join(f)).unwrap()).collect();
    assert_eq!(live, after, "every live file is untouched");
    assert_eq!(fs::read(d.dir.join("sync-incoming").join("layout.json")).unwrap(), unit, "the unit stays staged");
    // With the way free the next start applies it.
    fs::remove_file(d.dir.join("sync-conflicts")).unwrap();
    let promoted = promote_incoming_with_report(&d.dir);
    assert_eq!(promoted.applied, vec!["layout.json"]);
    assert_eq!(json(&d, PANES_FILE), serde_json::json!({"2": "web-1"}));
    assert_eq!(json(&d, "layout.json"), serde_json::json!({"theirs": 1}));
    assert!(!d.dir.join("sync-incoming").exists());
}

#[test]
fn the_unit_is_applied_in_one_order() {
    let unit = br#"{"unit":1,"layout":{"theirs":[1,2]},"connections":{"2":"web-1"},"blocks":{"1":{"kind":"monitor","source":"local","path":""}}}"#;
    let adopt = |tag: &str, prepare: &dyn Fn(&Device)| -> Device {
        let d = device(tag);
        write_atomic(&d.dir.join("layout.json"), br#"{"mine":1}"#).unwrap();
        write_atomic(&d.dir.join(PANES_FILE), br#"{"3":"db"}"#).unwrap();
        prepare(&d);
        ConfigSource::new(&d.dir, d.secrets.clone()).write("layout.json", unit).unwrap();
        promote_incoming_with_report(&d.dir);
        d
    };
    let files = |d: &Device| -> Vec<serde_json::Value> { ["layout.json", PANES_FILE, BLOCKS_FILE].iter().map(|f| json(d, f)).collect() };
    let once = adopt("order-1", &|_| {});
    assert_eq!(files(&once), vec![serde_json::json!({"theirs": [1, 2]}), serde_json::json!({"2": "web-1"}), serde_json::json!({"1": {"kind": "monitor", "source": "local", "path": ""}})]);
    // The same input again leaves the same files and keeps no new copy: the step can be repeated.
    let copies = kept_copies(&once).len();
    ConfigSource::new(&once.dir, once.secrets.clone()).write("layout.json", unit).unwrap();
    let again = promote_incoming_with_report(&once.dir);
    assert_eq!(again, Promoted { applied: vec!["layout.json".into()], kept: vec![], skipped: vec![], ..Promoted::default() });
    assert_eq!(files(&once), files(&adopt("order-2", &|_| {})));
    assert_eq!(kept_copies(&once).len(), copies);
    // A crash after the pane files were written and before the layout was: the next start finishes the step.
    let crashed = adopt("order-3", &|d| {
        // The state the crash leaves: the staged unit is still there, the pane files are already the new ones.
        write_atomic(&d.dir.join(PANES_FILE), br#"{"2": "web-1"}"#).unwrap();
        write_atomic(&d.dir.join(BLOCKS_FILE), br#"{"1": {"kind": "monitor", "source": "local", "path": ""}}"#).unwrap();
    });
    assert_eq!(files(&crashed), files(&once));
}

#[test]
fn a_layout_that_is_not_json_is_reported_by_name_and_the_other_files_still_sync() {
    let srv = server();
    let a = device("a-bad-layout");
    put(&a, "layout.json", "not a layout");
    put(&a, "settings.json", "{}");
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert!(report.errors.iter().any(|(n, e)| n == "layout.json" && e.contains("layout.json is not valid JSON")), "{report:?}");
    assert_eq!(report.uploaded, vec!["settings.json"]);
}

#[test]
fn the_unit_is_canonical_whatever_the_order_of_the_files() {
    let d = device("canonical");
    write_atomic(&d.dir.join("layout.json"), br#"{"z": 1, "a": {"y": [3, {"q": 1, "b": 2}], "b": null}}"#).unwrap();
    write_atomic(&d.dir.join(PANES_FILE), br#"{"10": "db", "2": "web-1", "02": "db", "+3": "db", "x": "db"}"#).unwrap();
    let bytes = live_unit(&d.dir).unwrap().unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.contains(r#""layout":{"a":{"b":null,"y":[3,{"b":2,"q":1}]},"z":1}"#), "{text}");
    assert!(text.contains(r#""connections":{"10":"db","2":"web-1"}"#), "only the one spelling of a pane id goes: {text}");
    // The same files in another order and another spelling give the same bytes.
    write_atomic(&d.dir.join("layout.json"), br#"{"a":{"b":null,"y":[3,{"b":2,"q":1}]},"z":1}"#).unwrap();
    write_atomic(&d.dir.join(PANES_FILE), br#"{"2":"web-1","10":"db"}"#).unwrap();
    assert_eq!(live_unit(&d.dir).unwrap().unwrap(), bytes);
}

#[test]
fn a_layout_edited_after_the_download_was_staged_is_copied_before_the_unit_replaces_it() {
    let srv = server();
    let (a, b) = pair(&srv, "edited");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    // The person arranged panes after the download was staged and before the app was started again.
    write_atomic(&b.dir.join("layout.json"), br#"{"arranged":"by hand"}"#).unwrap();
    let promoted = promote_incoming_with_report(&b.dir);
    assert_eq!(promoted, Promoted { applied: vec!["layout.json".into()], kept: vec!["layout.json".into()], skipped: vec![], ..Promoted::default() });
    let copies = kept_copies(&b);
    assert!(copies.iter().any(|c| c.0.starts_with("layout.json.") && c.1 == r#"{"arranged":"by hand"}"#), "{copies:?}");
    assert_eq!(json(&b, "layout.json"), serde_json::json!({"panes": [1, 2, 3]}));
    // The first run after the start lists it among the conflicts, as for any other file.
    assert_eq!(connect(&srv, &b, "pw").unwrap().sync_once().unwrap().conflicts, vec!["layout.json"]);
    // A download over an unedited layout keeps no copy of the layout.
    write_atomic(&a.dir.join("layout.json"), br#"{"panes":[1,2,3,4]}"#).unwrap();
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert!(promote_incoming_with_report(&b.dir).kept.is_empty());
}

// ---- deletions: a tombstone is a version like any other --------------------------------------------------------------

/// The plaintext the server's blob for `name` opens to, as device `d` (which holds the key) reads it.
fn server_plain(srv: &Server, d: &Device, name: &str) -> Vec<u8> {
    let syncer = connect(srv, d, "pw").unwrap();
    let blob = srv.store.lock().unwrap().files[&format!("files/{}", remote_file(name))].0.clone();
    syncer.key.open(name, &blob).unwrap().1.to_vec()
}

/// The `deleted` flag and the hash the device remembers for `name`.
fn remembered(d: &Device, name: &str) -> (bool, String) {
    let state = json(d, "sync_state.json");
    (state["files"][name]["deleted"].as_bool().unwrap_or(false), state["files"][name]["hash"].as_str().unwrap_or("?").to_owned())
}

/// Two devices that hold `themes/dark.json` after a sync, and the server.
fn with_theme(tag: &str) -> (Server, Device, Device) {
    let srv = server();
    let (a, b) = (device(&format!("a-{tag}")), device(&format!("b-{tag}")));
    fs::create_dir_all(a.dir.join("themes")).unwrap();
    put(&a, "themes/dark.json", "{\"name\":\"dark\"}");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(text(&b, "themes/dark.json").as_deref(), Some("{\"name\":\"dark\"}"));
    (srv, a, b)
}

#[test]
fn a_theme_deleted_on_one_device_is_removed_on_the_other_and_stays_deleted() {
    let (srv, a, b) = with_theme("del");
    fs::remove_file(a.dir.join("themes/dark.json")).unwrap();
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.deleted, vec!["themes/dark.json"], "{report:?}");
    assert_eq!(server_plain(&srv, &a, "themes/dark.json"), TOMBSTONE);
    assert_eq!(remembered(&a, "themes/dark.json"), (true, String::new()));

    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.removed, vec!["themes/dark.json"], "{report:?}");
    assert!(!b.dir.join("themes/dark.json").exists(), "the file is gone");
    let copies = kept_copies(&b);
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert!(copies[0].0.starts_with("themes.dark.json."), "{copies:?}");
    assert_eq!(copies[0].1, "{\"name\":\"dark\"}");
    assert_eq!(remembered(&b, "themes/dark.json"), (true, String::new()));

    // Nothing is left to do on either side, and the file does not come back.
    let puts = srv.store.lock().unwrap().puts;
    for d in [&a, &b, &a, &b] {
        let report = connect(&srv, d, "pw").unwrap().sync_once().unwrap();
        assert_eq!(report, Report::default());
    }
    assert_eq!(srv.store.lock().unwrap().puts, puts, "nothing is sent again");
    assert!(text(&a, "themes/dark.json").is_none() && text(&b, "themes/dark.json").is_none());
}

#[test]
fn the_marker_is_not_a_valid_theme_or_widget_file() {
    assert!(is_tombstone(TOMBSTONE));
    assert_eq!(TOMBSTONE, b"\0holzwave:deleted");
    assert!(!is_tombstone(b"holzwave:deleted") && !is_tombstone(b"{}") && !is_tombstone(b""));
    assert!(serde_json::from_slice::<serde_json::Value>(TOMBSTONE).is_err());
    // An old client stages the marker as an ordinary file; the loaders leave it out with a note and the rest still loads.
    let d = device("marker");
    fs::create_dir_all(d.dir.join("themes")).unwrap();
    fs::create_dir_all(d.dir.join("widgets")).unwrap();
    fs::write(d.dir.join("themes/gone.json"), TOMBSTONE).unwrap();
    fs::write(d.dir.join("widgets/gone.json"), TOMBSTONE).unwrap();
    let good = r#"{"title":"T","kind":"list","items":[{"label":"a","command":"ls"}]}"#;
    fs::write(d.dir.join("widgets/good.json"), good).unwrap();
    let (registry, warnings) = crate::theme::ThemeRegistry::load(&d.dir);
    assert!(warnings.iter().any(|w| w.contains("themes/gone.json")), "{warnings:?}");
    assert!(!registry.contains("gone"));
    let (defs, notes) = crate::widgets::load(&d.dir);
    assert!(notes.iter().any(|n| n.contains("widgets/gone.json")), "{notes:?}");
    assert!(defs.iter().any(|w| w.id == "good"), "the valid widget still loads");
}

/// A file of the device `d` that `name` names, written without going through the staging of a download.
fn write_live(d: &Device, name: &str, text: &str) {
    write_atomic(&d.dir.join(name), text.as_bytes()).unwrap();
}

#[test]
fn an_edit_beats_a_delete_on_the_other_device() {
    let (srv, a, b) = with_theme("edit-del");
    fs::remove_file(a.dir.join("themes/dark.json")).unwrap();
    write_live(&b, "themes/dark.json", "{\"name\":\"edited by b\"}");
    assert_eq!(connect(&srv, &a, "pw").unwrap().sync_once().unwrap().deleted, vec!["themes/dark.json"]);
    let tombstone_counter = counter(&a, "themes/dark.json");
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.uploaded, vec!["themes/dark.json"], "{report:?}");
    assert_eq!(report.conflicts, vec!["themes/dark.json"], "the run that decided it says so: {report:?}");
    assert!(report.removed.is_empty(), "{report:?}");
    assert!(counter(&b, "themes/dark.json") > tombstone_counter, "the edit is newer than the tombstone");
    assert_eq!(fs::read_to_string(b.dir.join("themes/dark.json")).unwrap(), "{\"name\":\"edited by b\"}");
    // A's next run brings the edited file back; neither side lost it.
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.downloaded, vec!["themes/dark.json"], "{report:?}");
    assert_eq!(text(&a, "themes/dark.json").as_deref(), Some("{\"name\":\"edited by b\"}"));
    assert_eq!(connect(&srv, &b, "pw").unwrap().sync_once().unwrap(), Report::default());
}

#[test]
fn an_edit_beats_a_delete_the_other_way() {
    let (srv, a, b) = with_theme("del-edit");
    fs::remove_file(a.dir.join("themes/dark.json")).unwrap();
    write_live(&b, "themes/dark.json", "{\"name\":\"edited by b\"}");
    // B runs first and uploads its edit; A, which deleted, then finds the server changed.
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.downloaded, vec!["themes/dark.json"], "{report:?}");
    assert_eq!(report.conflicts, vec!["themes/dark.json"], "{report:?}");
    assert!(report.deleted.is_empty() && report.uploaded.is_empty(), "no tombstone is sent: {report:?}");
    assert_eq!(text(&a, "themes/dark.json").as_deref(), Some("{\"name\":\"edited by b\"}"));
    assert_eq!(server_plain(&srv, &a, "themes/dark.json"), b"{\"name\":\"edited by b\"}");
}

#[test]
fn a_file_recreated_after_a_deletion_supersedes_the_tombstone() {
    let (srv, a, b) = with_theme("recreate");
    fs::remove_file(a.dir.join("themes/dark.json")).unwrap();
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert_eq!(connect(&srv, &b, "pw").unwrap().sync_once().unwrap().removed, vec!["themes/dark.json"]);
    write_live(&a, "themes/dark.json", "{\"name\":\"again\"}");
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.uploaded, vec!["themes/dark.json"], "{report:?}");
    assert!(report.deleted.is_empty() && report.errors.is_empty(), "{report:?}");
    assert_eq!(remembered(&a, "themes/dark.json").0, false);
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.downloaded, vec!["themes/dark.json"], "{report:?}");
    assert!(report.conflicts.is_empty(), "a recreation is not a conflict: {report:?}");
    assert_eq!(text(&b, "themes/dark.json").as_deref(), Some("{\"name\":\"again\"}"));
    // And it can be deleted again.
    fs::remove_file(b.dir.join("themes/dark.json")).unwrap();
    assert_eq!(connect(&srv, &b, "pw").unwrap().sync_once().unwrap().deleted, vec!["themes/dark.json"]);
    assert_eq!(connect(&srv, &a, "pw").unwrap().sync_once().unwrap().removed, vec!["themes/dark.json"]);
}

#[test]
fn a_device_that_never_had_the_file_records_the_tombstone_and_creates_nothing() {
    let (srv, a, _b) = with_theme("never");
    fs::remove_file(a.dir.join("themes/dark.json")).unwrap();
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    let c = device("c-never");
    let report = connect(&srv, &c, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report, Report::default(), "no file, no error");
    assert!(!c.dir.join("themes").exists() && !c.dir.join("sync-incoming").exists() && !c.dir.join("sync-conflicts").exists());
    assert_eq!(remembered(&c, "themes/dark.json"), (true, String::new()));
    // Later runs ask nothing about it: the only read is the sync folder's own meta file.
    let gets = srv.store.lock().unwrap().gets;
    assert_eq!(connect(&srv, &c, "pw").unwrap().sync_once().unwrap(), Report::default());
    assert_eq!(srv.store.lock().unwrap().gets - gets, 1, "meta.json only");
}

#[test]
fn a_read_error_is_not_a_deletion() {
    let (srv, a, _b) = with_theme("readerr");
    let file = a.dir.join("themes/dark.json");
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap(); // a directory in the file's place cannot be read as a file
    let puts = srv.store.lock().unwrap().puts;
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert!(report.deleted.is_empty() && report.uploaded.is_empty(), "{report:?}");
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert_eq!(report.errors[0].0, "themes/dark.json");
    assert_eq!(srv.store.lock().unwrap().puts, puts);
    assert_eq!(server_plain(&srv, &a, "themes/dark.json"), b"{\"name\":\"dark\"}");
    assert_eq!(remembered(&a, "themes/dark.json").0, false);
}

#[cfg(unix)]
#[test]
fn an_unreadable_folder_is_not_a_deletion() {
    use std::os::unix::fs::PermissionsExt;
    let (srv, a, _b) = with_theme("noperm");
    let folder = a.dir.join("themes");
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::read_dir(&folder).is_ok(); // a superuser reads it anyway
    let puts = srv.store.lock().unwrap().puts;
    let result = connect(&srv, &a, "pw").unwrap().sync_once();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
    let report = result.unwrap();
    if !readable {
        assert!(report.deleted.is_empty() && report.uploaded.is_empty(), "{report:?}");
        assert_eq!(report.errors.iter().filter(|e| e.0 == "themes/dark.json").count(), 1, "{report:?}");
        assert_eq!(srv.store.lock().unwrap().puts, puts);
    }
    assert_eq!(remembered(&a, "themes/dark.json").0, false);
}

#[test]
fn a_missing_folder_deletes_nothing() {
    let (srv, a, _b) = with_theme("nofolder");
    let puts = srv.store.lock().unwrap().puts;
    fs::remove_dir_all(a.dir.join("themes")).unwrap();
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert!(report.deleted.is_empty() && report.uploaded.is_empty(), "{report:?}");
    assert_eq!(report.errors.len(), 1, "one error for the folder: {report:?}");
    assert!(report.errors[0].1.contains("themes"), "{report:?}");
    assert_eq!(srv.store.lock().unwrap().puts, puts, "the server keeps everything");
    assert_eq!(remembered(&a, "themes/dark.json").0, false);
    // The folder comes back: the sync is normal again, and a real deletion is a deletion.
    fs::create_dir_all(a.dir.join("themes")).unwrap();
    write_live(&a, "themes/dark.json", "{\"name\":\"dark\"}");
    assert_eq!(connect(&srv, &a, "pw").unwrap().sync_once().unwrap(), Report::default());
    fs::remove_file(a.dir.join("themes/dark.json")).unwrap();
    assert_eq!(connect(&srv, &a, "pw").unwrap().sync_once().unwrap().deleted, vec!["themes/dark.json"]);
}

/// A has `count` themes `themes/t01.json`, `t02`, ... synced to the server, B has them too.
fn with_themes(tag: &str, count: usize) -> (Server, Device, Device) {
    let srv = server();
    let (a, b) = (device(&format!("a-{tag}")), device(&format!("b-{tag}")));
    fs::create_dir_all(a.dir.join("themes")).unwrap();
    for i in 1..=count {
        write_live(&a, &format!("themes/t{i:02}.json"), &format!("{{\"name\":\"t{i}\"}}"));
    }
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    promote_incoming(&b.dir);
    (srv, a, b)
}

#[test]
fn at_most_five_tombstones_are_sent_per_run() {
    let (srv, a, _b) = with_themes("five", 6);
    for i in 1..=6 {
        fs::remove_file(a.dir.join(format!("themes/t{i:02}.json"))).unwrap();
    }
    let puts = srv.store.lock().unwrap().puts;
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert!(report.deleted.is_empty() && report.uploaded.is_empty(), "a mass deletion is stopped whole: {report:?}");
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert!(report.errors[0].1.contains("themes") && report.errors[0].1.contains('5'), "{report:?}");
    assert_eq!(srv.store.lock().unwrap().puts, puts);
    // The person puts one back (a smaller batch): the five that remain are sent.
    write_live(&a, "themes/t06.json", "{\"name\":\"t6\"}");
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.deleted.len(), 5, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    // The last one goes in a later run.
    fs::remove_file(a.dir.join("themes/t06.json")).unwrap();
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.deleted, vec!["themes/t06.json"], "{report:?}");
}

#[test]
fn at_most_twenty_files_are_removed_per_run() {
    let (srv, a, b) = with_themes("twenty", 21);
    // A deletes in batches of five, as the cap on sending asks.
    for batch in [1..=5, 6..=10, 11..=15, 16..=20, 21..=21] {
        for i in batch {
            fs::remove_file(a.dir.join(format!("themes/t{i:02}.json"))).unwrap();
        }
        let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
        assert!(report.errors.is_empty(), "{report:?}");
    }
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.removed.len(), MAX_DELETES_APPLIED, "{report:?}");
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert!(report.errors[0].1.contains("themes") && report.errors[0].1.contains("20"), "{report:?}");
    let left = fs::read_dir(b.dir.join("themes")).unwrap().count();
    assert_eq!(left, 1, "the rest waits");
    // The next run removes more.
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(fs::read_dir(b.dir.join("themes")).unwrap().count(), 0);
    assert_eq!(kept_copies(&b).len(), 21, "every removed file was copied first");
}

#[test]
fn a_failed_copy_stops_the_removal() {
    let (srv, a, b) = with_theme("copyfail");
    fs::remove_file(a.dir.join("themes/dark.json")).unwrap();
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    fs::write(b.dir.join("sync-conflicts"), b"in the way").unwrap(); // a file where the folder of the copies belongs
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert!(report.removed.is_empty(), "{report:?}");
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert_eq!(report.errors[0].0, "themes/dark.json");
    assert_eq!(fs::read_to_string(b.dir.join("themes/dark.json")).unwrap(), "{\"name\":\"dark\"}", "the live file stays");
    assert_eq!(remembered(&b, "themes/dark.json").0, false);
    // The next run tries again.
    fs::remove_file(b.dir.join("sync-conflicts")).unwrap();
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.removed, vec!["themes/dark.json"], "{report:?}");
    assert!(!b.dir.join("themes/dark.json").exists());
}

#[test]
fn a_fixed_file_is_never_deleted_by_the_sync() {
    let srv = server();
    let (a, b) = (device("a-fixed"), device("b-fixed"));
    write_live(&b, "settings.json", "mine");
    // A tombstone put under each fixed name by hand (as a hostile or buggy client could).
    let planter = connect(&srv, &a, "pw").unwrap();
    for name in ["settings.json", "layout.json", "connections.json", "widget_notes.json"] {
        planter.upload(name, TOMBSTONE, 1, None).unwrap();
    }
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    let mut failed: Vec<&str> = report.errors.iter().map(|e| e.0.as_str()).collect();
    failed.sort_unstable();
    assert_eq!(failed, vec!["connections.json", "layout.json", "settings.json", "widget_notes.json"], "{report:?}");
    assert!(report.errors.iter().all(|e| e.1.contains("never deletes")), "{report:?}");
    assert_eq!(fs::read_to_string(b.dir.join("settings.json")).unwrap(), "mine", "the file stays");
    assert!(!b.dir.join("layout.json").exists() && !b.dir.join("connections.json").exists() && !b.dir.join("sync-incoming").exists());
    assert!(report.removed.is_empty() && report.downloaded.is_empty() && report.uploaded.is_empty());
    // The file itself refuses too.
    let source = ConfigSource::new(&b.dir, b.secrets.clone());
    assert!(source.delete("settings.json").is_err() && source.delete("secrets.json").is_err() && source.delete("../x.json").is_err());
    assert_eq!(fs::read_to_string(b.dir.join("settings.json")).unwrap(), "mine");
}

#[test]
fn a_locally_missing_fixed_file_still_takes_the_servers_version() {
    let srv = server();
    let (a, b) = (device("a-fixed2"), device("b-fixed2"));
    write_live(&a, "settings.json", "one");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(text(&b, "settings.json").as_deref(), Some("one"));
    fs::remove_file(b.dir.join("settings.json")).unwrap();
    // Deleting it here sends nothing: a fixed file has no tombstone.
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report, Report::default());
    write_live(&a, "settings.json", "two");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.downloaded, vec!["settings.json"], "{report:?}");
    assert_eq!(text(&b, "settings.json").as_deref(), Some("two"));
}

#[test]
fn a_staged_copy_of_a_deleted_name_goes_too() {
    let (srv, a, b) = with_theme("staged");
    write_live(&a, "themes/dark.json", "{\"name\":\"v2\"}");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    // B's download of v2 is staged; the live file is still v1.
    assert_eq!(connect(&srv, &b, "pw").unwrap().sync_once().unwrap().downloaded, vec!["themes/dark.json"]);
    assert!(b.dir.join("sync-incoming/themes/dark.json").exists());
    fs::remove_file(a.dir.join("themes/dark.json")).unwrap();
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.removed, vec!["themes/dark.json"], "{report:?}");
    assert!(!b.dir.join("themes/dark.json").exists() && !b.dir.join("sync-incoming/themes/dark.json").exists());
    assert!(promote_incoming(&b.dir).is_empty(), "nothing is left to apply, so nothing comes back");
    assert!(!b.dir.join("themes/dark.json").exists());
    let copies = kept_copies(&b);
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(copies[0].1, "{\"name\":\"dark\"}", "the live file was copied");
}

// ---- a connection that another device deleted -----------------------------------------------------------------------

/// Stage `bytes` as the download of `connections.json` on `d`, as a run would.
fn stage_connections(d: &Device, bytes: &[u8]) {
    write_atomic(&d.dir.join(INCOMING).join("connections.json"), bytes).unwrap();
}

#[test]
fn a_connection_deleted_on_another_device_is_gone_here() {
    let srv = server();
    let (a, b) = (device("a-conn"), device("b-conn"));
    save_connections(&a, &["web", "db"]);
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    let first = promote_incoming_with_report(&b.dir);
    assert_eq!(first, Promoted { applied: vec!["connections.json".into()], ..Promoted::default() }, "nothing was held before");
    // A deletes `web`; the deletion travels as the edit of one file.
    save_connections(&a, &["db"]);
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    let promoted = promote_incoming_with_report(&b.dir);
    assert_eq!(promoted.applied, vec!["connections.json"]);
    assert_eq!(promoted.dropped_profiles, vec!["web"]);
    let (now, _) = Profiles::load(&b.dir);
    assert_eq!(now.items.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["db"]);
}

#[test]
fn a_corrupt_connections_file_drops_nothing() {
    let valid_none = br#"{"version":1,"items":[{"id":"web","name":"Web","host":"","user":"u"}]}"#;
    for (label, bytes) in [
        ("not JSON", &b"this is not json"[..]),
        ("not UTF-8", &[0xff, 0xfe, 0x00][..]),
        ("another version", br#"{"version":2,"items":[]}"#),
        ("the wrong shape", br#"{"items":"web"}"#),
        ("a list of invalid profiles only", &valid_none[..]),
    ] {
        let d = device("corrupt-conn");
        save_connections(&d, &["web", "db"]);
        stage_connections(&d, bytes);
        let promoted = promote_incoming_with_report(&d.dir);
        assert_eq!(promoted.applied, vec!["connections.json"], "{label}: applied as before");
        assert!(promoted.dropped_profiles.is_empty(), "{label}: {promoted:?}");
    }
}

#[test]
fn a_profile_that_is_still_there_is_never_dropped() {
    let d = device("kept-conn");
    save_connections(&d, &["web", "db"]);
    // `web` stays with other fields; `db` stays but is invalid now (it has no host): neither counts as removed.
    stage_connections(
        &d,
        br#"{"version":1,"items":[{"id":"web","name":"Other","host":"new.example","user":"x","auth":"password"},{"id":"db","name":"Db","host":"","user":"u"}]}"#,
    );
    assert!(promote_incoming_with_report(&d.dir).dropped_profiles.is_empty());
    // A list that really lacks one drops exactly that one, sorted.
    save_connections(&d, &["web", "db", "cache"]);
    stage_connections(&d, br#"{"version":1,"items":[{"id":"db","name":"Db","host":"h","user":"u","key_path":"~/.ssh/k"}]}"#);
    assert_eq!(promote_incoming_with_report(&d.dir).dropped_profiles, vec!["cache", "web"]);
    // No live list before: nothing is known, nothing is dropped.
    let fresh = device("fresh-conn");
    stage_connections(&fresh, br#"{"version":1,"items":[]}"#);
    assert!(promote_incoming_with_report(&fresh.dir).dropped_profiles.is_empty());
}

// ---- a server whose ETag moves once per second -------------------------------------------------------------------------

/// The texts of the replaced versions of `name` that the server keeps in `backups/`, opened with the folder's key.
fn backed_up(srv: &Server, d: &Device, name: &str) -> Vec<String> {
    let syncer = connect(srv, d, "pw").unwrap();
    let flat = format!("backups/{}.", name.replace('/', "."));
    let blobs: Vec<Vec<u8>> = srv.store.lock().unwrap().files.iter().filter(|(k, _)| k.starts_with(&flat)).map(|(_, v)| v.0.clone()).collect();
    let mut texts: Vec<String> = blobs.iter().map(|b| String::from_utf8(syncer.key.open(name, b).unwrap().1.to_vec()).unwrap()).collect();
    texts.sort();
    texts
}

/// Two devices level on `settings.json` = `base`, all inside one second of the server.
fn level_pair(srv: &Server, tag: &str) -> (Device, Device) {
    let (a, b) = (device(&format!("a-{tag}")), device(&format!("b-{tag}")));
    put(&a, "settings.json", "base");
    connect(srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(text(&b, "settings.json").as_deref(), Some("base"));
    (a, b)
}

/// A uploads `from a`; B, which changed the same file to `from b` meanwhile, syncs in the same second. Nothing may be
/// lost: B finds the conflict and one of the versions is kept where a person can reach it.
fn same_second_overwrite(srv: &Server, tag: &str) -> (Device, Device) {
    let (a, b) = level_pair(srv, tag);
    put(&a, "settings.json", "from a");
    assert_eq!(connect(srv, &a, "pw").unwrap().sync_once().unwrap().uploaded, vec!["settings.json"]);
    put(&b, "settings.json", "from b");
    let report = connect(srv, &b, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.conflicts, vec!["settings.json"], "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(backed_up(srv, &b, "settings.json"), vec!["from a"], "A's version is kept on the server");
    assert_eq!(server_plain(srv, &b, "settings.json"), b"from b");
    (a, b)
}

#[test]
#[ignore = "fails until the fix of the next commit"]
fn a_write_in_the_same_second_as_the_last_seen_version_is_a_conflict_not_an_overwrite() {
    let srv = server_coarse(true);
    let (a, _b) = same_second_overwrite(&srv, "ss1");
    // A's next run sees B's version, although the ETag did not move, because the checksum did.
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.downloaded, vec!["settings.json"], "{report:?}");
    assert_eq!(text(&a, "settings.json").as_deref(), Some("from b"));
}

/// B changes a file in the same second in which A last saw it written; the version B wrote is `v2`.
fn same_second_second_writer(srv: &Server, tag: &str) -> (Device, Device) {
    let (a, b) = (device(&format!("a-{tag}")), device(&format!("b-{tag}")));
    put(&a, "settings.json", "v1");
    connect(srv, &a, "pw").unwrap().sync_once().unwrap();
    connect(srv, &b, "pw").unwrap().sync_once().unwrap();
    put(&b, "settings.json", "v2");
    assert_eq!(connect(srv, &b, "pw").unwrap().sync_once().unwrap().uploaded, vec!["settings.json"]);
    (a, b)
}

#[test]
#[ignore = "fails until the fix of the next commit"]
fn a_version_written_in_the_same_second_as_the_last_seen_one_is_downloaded() {
    let srv = server_coarse(true);
    let (a, _b) = same_second_second_writer(&srv, "ss2");
    let report = connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    assert_eq!(report.downloaded, vec!["settings.json"], "{report:?}");
    assert_eq!(text(&a, "settings.json").as_deref(), Some("v2"));
}
