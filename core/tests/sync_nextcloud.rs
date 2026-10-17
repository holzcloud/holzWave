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
use holzwave_core::sync::crypto::Kdf;
use holzwave_core::sync::engine::{ConfigSource, Syncer, promote_incoming};
use holzwave_core::sync::webdav::{Backend, WebDav};
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

fn names(entries: Vec<holzwave_core::sync::webdav::Entry>) -> Vec<String> {
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
