//! The check before a connection to the local network, at the three places that make one: an SSH login, a WebDAV request of
//! the sync and a request of the AI chat. A gate is installed for the whole process, so this file is its own test binary and
//! installs one gate for all its tests: one that refuses, and counts how often it was asked and for how long it was held.
//!
//! No address of the local network is ever dialled: the gate refuses first, so the tests prove both that the refusal is a plain
//! sentence (not a timeout) and that no connection was tried.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use holzwave_core::ai::http::{Call, Framing, Http, HttpConfig, NetworkPolicy, Target};
use holzwave_core::ai::provider::{AiError, CancelToken, Flow};
use holzwave_core::reach::{self, Access, Gate};
use holzwave_core::ssh::{ExecCancel, ExecLimits, SshAuth, SshExec, SshTarget};
use holzwave_core::sync::webdav::{Backend, WebDav};

struct Refusing {
    asked: AtomicU32,
    answer: Access,
}

impl Gate for Refusing {
    fn ask(&self) -> Access {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.answer
    }
}

static GATE: std::sync::OnceLock<Arc<Refusing>> = std::sync::OnceLock::new();

/// The one gate of this binary. It refuses with `Denied`.
fn gate() -> &'static Arc<Refusing> {
    GATE.get_or_init(|| {
        let gate = Arc::new(Refusing { asked: AtomicU32::new(0), answer: Access::Denied });
        assert!(reach::install(gate.clone()), "the gate was installed once");
        gate
    })
}

/// The tests count the asks of one shared gate, so they run one at a time.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    gate();
    LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn asked() -> u32 {
    gate().asked.load(Ordering::SeqCst)
}

fn plain(text: &str, host: &str) {
    assert!(text.contains(host), "{text}");
    assert!(text.contains("local network"), "{text}");
    assert!(text.contains("Android settings") && text.contains("Nearby devices"), "{text}");
    assert!(!text.to_ascii_lowercase().contains("timed out"), "{text}");
}

#[test]
fn an_ssh_login_to_a_private_address_is_refused_with_a_sentence_and_dials_nothing() {
    let _serial = serial();
    let dir = std::env::temp_dir().join(format!("hw-reach-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let known = dir.join("known_hosts");
    std::fs::write(&known, "").unwrap();
    let mut target = SshTarget::new("192.168.77.5", "holz", SshAuth::Password("pw".into()));
    target.known_hosts = Some(known);
    let exec = SshExec::new("private", target).expect("a pinned target");
    let before = asked();
    let started = std::time::Instant::now();
    let error = exec.exec("true", &ExecLimits::default(), &ExecCancel::default()).expect_err("refused");
    let text = error.to_string();
    plain(&text, "192.168.77.5");
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "an answer, not a timeout ({:?})", started.elapsed());
    assert_eq!(asked(), before + 1, "the gate was asked once");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_ssh_login_to_loopback_is_never_asked_about() {
    let _serial = serial();
    let mut target = SshTarget::new("127.0.0.1", "holz", SshAuth::Password("pw".into()));
    let dir = std::env::temp_dir().join(format!("hw-reach-lo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let known = dir.join("known_hosts");
    std::fs::write(&known, "").unwrap();
    target.known_hosts = Some(known);
    // Port 1 on the loopback address refuses at once; the error is the connection's own, and the gate was not asked.
    let mut target = target;
    target.port = 1;
    let exec = SshExec::new("loopback", target).expect("a pinned target");
    let before = asked();
    let error = exec.exec("true", &ExecLimits::default(), &ExecCancel::default()).expect_err("nothing listens there");
    assert!(!error.to_string().contains("local network"), "{error}");
    assert_eq!(asked(), before, "a loopback address does not need the permission");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_webdav_request_to_a_private_address_is_refused_with_a_sentence() {
    let _serial = serial();
    let dav = WebDav::new("https://192.168.77.6:8443/remote.php/dav/files/holz/holzwave", "holz", "app-password").expect("an endpoint");
    let before = asked();
    let error = dav.ensure("sync").expect_err("refused");
    plain(&error.to_string(), "192.168.77.6");
    assert_eq!(asked(), before + 1);
}

#[test]
fn a_webdav_request_to_a_name_in_the_local_domain_is_refused_with_a_sentence() {
    let _serial = serial();
    let dav = WebDav::new("https://nas.local/dav", "holz", "app-password").expect("an endpoint");
    let before = asked();
    let error = dav.ensure("sync").expect_err("refused");
    plain(&error.to_string(), "nas.local");
    assert_eq!(asked(), before + 1);
}

#[test]
fn a_chat_request_to_a_private_address_is_refused_with_a_sentence() {
    let _serial = serial();
    let http = Http::new(HttpConfig::standard());
    let target = Target::parse("https://192.168.77.7:11434", false).expect("accepted");
    let policy = NetworkPolicy::new(false, false);
    let call = Call::new(&target, policy, "/v1/chat", b"{}", Framing::Sse);
    let before = asked();
    let error = http.post_stream(&call, &mut |_| Flow::Continue, &CancelToken::new()).expect_err("refused");
    match &error {
        AiError::Unavailable(detail) => plain(&format!("{detail:?}"), "192.168.77.7"),
        other => panic!("not the plain refusal: {other:?}"),
    }
    assert_eq!(asked(), before + 1);
    assert_eq!(http.attempts(), 0, "no request was sent");
}
