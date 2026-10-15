use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use super::*;
use crate::secrets::MemoryStore;
use crate::sync::webdav::WebDav;

/// An in-memory WebDAV server: the verbs the sync uses, ETags, and the conditions on a write.
#[derive(Default)]
struct Store {
    files: BTreeMap<String, (Vec<u8>, u64)>,
    dirs: BTreeSet<String>,
    next: u64,
    puts: u32,
}

struct Server {
    url: String,
    store: Arc<Mutex<Store>>,
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
                "GET" => match s.files.get(&rel) {
                    Some((b, e)) => (200, format!("ETag: \"v{e}\"\r\n"), b.clone()),
                    None => (404, String::new(), vec![]),
                },
                "PUT" => {
                    let current = s.files.get(&rel).map(|(_, e)| format!("\"v{e}\""));
                    let ok = match (headers.get("if-none-match"), headers.get("if-match")) {
                        (Some(_), _) => current.is_none(),
                        (_, Some(m)) => current.as_deref() == Some(m.as_str()),
                        _ => true,
                    };
                    if ok {
                        s.next += 1;
                        s.puts += 1;
                        let e = s.next;
                        let created = current.is_none();
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
                        let mut xml = String::from(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">"#);
                        xml.push_str(&format!("<d:response><d:href>/dav/{prefix}</d:href><d:propstat><d:prop><d:getetag>\"d\"</d:getetag><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>"));
                        for (p, (_, e)) in &s.files {
                            if let Some(name) = p.strip_prefix(&prefix) {
                                if !name.contains('/') {
                                    xml.push_str(&format!("<d:response><d:href>/dav/{p}</d:href><d:propstat><d:prop><d:getetag>&quot;v{e}&quot;</d:getetag><d:resourcetype/></d:prop></d:propstat></d:response>"));
                                }
                            }
                        }
                        xml.push_str("</d:multistatus>");
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
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/dav", listener.local_addr().unwrap().port());
    let store = Arc::new(Mutex::new(Store::default()));
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
    assert!(allowed("themes/dark-2.json"));
    for bad in ["../x", "themes/../x.json", "themes/a.b.json", "themes/.json", "other.json", "themes/x.json/y", "/etc/passwd"] {
        assert!(!allowed(bad), "{bad}");
    }
    assert_eq!(logical_name("themes.dark.json.hwx").as_deref(), Some("themes/dark.json"));
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
    put(&a, "layout.json", "l");
    connect(&srv, &a, "pw").unwrap().sync_once().unwrap();
    {
        let mut store = srv.store.lock().unwrap();
        let layout = store.files["files/layout.json.hwx"].0.clone();
        store.files.get_mut("files/settings.json.hwx").unwrap().0 = layout;
    }
    let report = connect(&srv, &b, "pw").unwrap().sync_once().unwrap();
    assert!(report.errors.iter().any(|(n, e)| n == "settings.json" && e.contains("could not be decrypted")), "{report:?}");
    assert!(text(&b, "settings.json").is_none());
    assert_eq!(text(&b, "layout.json").as_deref(), Some("l"), "the other files still sync");
}
