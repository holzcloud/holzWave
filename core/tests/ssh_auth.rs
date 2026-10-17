//! Public-key login, the words of a key that cannot be used, and the jump-host path, against the in-process SSH server
//! of `common::ssh_server`. No Zig and no real host are needed.
//!
//! Every login test uses a server that knows the public key only (no password), so a password cannot be what let a
//! test pass. A key is generated at run time into a private folder that the test removes at its end.
//!
//! The key tests need no network beyond loopback and no agent. The RSA key is generated once, in the test that uses it;
//! it takes a few seconds in a dev build.
//!
//! The PEM formats (PKCS#1 and PKCS#8, with and without a passphrase) are not written here, because the crates that
//! write them are not dependencies of the core; `scripts/ssh-loopback-check.sh` proves them against a real OpenSSH.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};

use common::ssh_server::{self, Options, Out};
use holzwave_core::conn::{ConnState, Link};
use holzwave_core::ssh::{ExecCancel, ExecEnd, ExecLimits, SshAuth, SshExec, SshLink};

/// A private folder (mode 0700 on Unix) that is removed when the test ends, also when it fails.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = common::scratch_dir(tag);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Connect a pane with `auth`; its output is read by a helper thread.
fn pane(server: &ssh_server::Server, auth: SshAuth) -> (SshLink, Out) {
    pane_to(server.target(auth))
}

fn pane_to(target: holzwave_core::ssh::SshTarget) -> (SshLink, Out) {
    let mut link = SshLink::connect(target, 80, 24).unwrap();
    let out = Out::new(link.take_reader().unwrap());
    (link, out)
}

fn key_auth(path: PathBuf, passphrase: Option<&str>) -> SshAuth {
    SshAuth::Key { path, passphrase: passphrase.map(str::to_string) }
}

#[test]
fn an_ed25519_key_logs_in_to_a_pane_and_to_exec() {
    let scratch = Scratch::new("auth-ed25519");
    let key = ssh_server::ed25519();
    let path = ssh_server::write_key(scratch.path(), "id_ed25519", &key, None);
    let server = ssh_server::start(Options::key(key.public_key().clone()));

    let (mut link, mut out) = pane(&server, key_auth(path.clone(), None));
    let mut writer = link.take_writer().unwrap();
    out.until("welcome 0", 10);
    assert_eq!(link.state(), ConnState::Connected);
    writer.write_all(b"hi").unwrap();
    out.until("echo:hi", 10);

    let known = scratch.path().join("known_hosts");
    server.trust(&known);
    let exec = SshExec::new("Key host", server.exec_target(key_auth(path, None), &known)).unwrap();
    let result = exec.exec("echo ok", &ExecLimits::default(), &ExecCancel::new()).unwrap();
    assert_eq!(result.stdout, b"ran:echo ok\r\n");
    assert_eq!((result.status, result.end), (Some(0), ExecEnd::Finished));

    assert_eq!(server.seen.refused_logins.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(server.seen.logins.load(std::sync::atomic::Ordering::SeqCst) >= 2, "the pane and the exec logged in");
    assert_eq!(server.seen.commands(), vec!["echo ok".to_string()]);
    Box::new(link).close().unwrap();
}

#[test]
fn a_key_the_server_does_not_know_is_refused_in_words() {
    let scratch = Scratch::new("auth-unknown");
    let known = ssh_server::ed25519();
    let other = ssh_server::ed25519();
    let path = ssh_server::write_key(scratch.path(), "id_other", &other, None);
    let server = ssh_server::start(Options::key(known.public_key().clone()));

    let (link, mut out) = pane(&server, key_auth(path, None));
    out.until("refused the login", 10);
    out.until("Press Enter to try again", 10);
    assert_eq!(link.state(), ConnState::Failed);
    assert_eq!(server.seen.logins.load(std::sync::atomic::Ordering::SeqCst), 0);
    Box::new(link).close().unwrap();
}

#[test]
fn the_password_login_still_works_on_the_shared_server() {
    let server = ssh_server::start(Options::password());
    let (link, mut out) = pane(&server, SshAuth::Password("pw".into()));
    out.until("welcome 0", 10);
    assert_eq!(link.state(), ConnState::Connected);
    Box::new(link).close().unwrap();
}

/// The text of the pane up to the line that waits for Enter: the whole reason a connection failed.
fn failure_line(out: &mut Out) -> String {
    out.until("Press Enter to try again", 10);
    out.seen.clone()
}

/// No login was tried: the key was refused on this side, before the server was asked anything.
fn assert_no_login(server: &ssh_server::Server) {
    use std::sync::atomic::Ordering::SeqCst;
    assert_eq!(server.seen.logins.load(SeqCst), 0);
    assert_eq!(server.seen.refused_logins.load(SeqCst), 0, "the server was never asked to judge a key");
}

#[test]
fn an_ed25519_key_with_a_passphrase_logs_in() {
    let scratch = Scratch::new("auth-passphrase");
    let key = ssh_server::ed25519();
    let path = ssh_server::write_key(scratch.path(), "id_ed25519", &key, Some("open sesame"));
    let server = ssh_server::start(Options::key(key.public_key().clone()));

    let (link, mut out) = pane(&server, key_auth(path, Some("open sesame")));
    out.until("welcome 0", 10);
    assert_eq!(link.state(), ConnState::Connected);
    Box::new(link).close().unwrap();
}

#[test]
fn a_key_that_needs_a_passphrase_and_gets_none_says_so() {
    let scratch = Scratch::new("auth-nopass");
    let key = ssh_server::ed25519();
    let path = ssh_server::write_key(scratch.path(), "id_ed25519", &key, Some("open sesame"));
    let server = ssh_server::start(Options::key(key.public_key().clone()));

    let (link, mut out) = pane(&server, key_auth(path.clone(), None));
    let line = failure_line(&mut out);
    assert!(line.contains("the key is protected by a passphrase; save the passphrase in the connection"), "{line:?}");
    assert!(line.contains(&path.display().to_string()), "the line names the key file: {line:?}");
    assert!(!line.contains("cryptographic error"), "{line:?}");
    assert_eq!(link.state(), ConnState::Failed);
    assert_no_login(&server);
    Box::new(link).close().unwrap();
}

#[test]
fn a_wrong_passphrase_says_the_passphrase_is_wrong() {
    let scratch = Scratch::new("auth-wrongpass");
    let key = ssh_server::ed25519();
    let path = ssh_server::write_key(scratch.path(), "id_ed25519", &key, Some("the right one"));
    let server = ssh_server::start(Options::key(key.public_key().clone()));

    let auth = key_auth(path.clone(), Some("the wrong one"));
    let target = server.target(auth);
    assert!(!format!("{target:?}").contains("the wrong one"), "the target never prints a passphrase");
    let (link, mut out) = pane_to(target);
    let line = failure_line(&mut out);
    assert!(
        line.contains(&format!("the key {} cannot be used: the passphrase is wrong or the key file is damaged", path.display())),
        "{line:?}"
    );
    assert!(!line.contains("cryptographic error"), "{line:?}");
    assert!(!line.contains("the wrong one") && !line.contains("the right one"), "no secret in the line: {line:?}");
    assert_eq!(link.state(), ConnState::Failed);
    assert_no_login(&server);
    Box::new(link).close().unwrap();
}

#[test]
fn an_rsa_key_in_openssh_format_logs_in() {
    let scratch = Scratch::new("auth-rsa");
    // Generated once: a few seconds in a dev build.
    let started = std::time::Instant::now();
    let key = ssh_server::rsa(2048);
    eprintln!("rsa 2048 generated in {:?}", started.elapsed());
    let path = ssh_server::write_key(scratch.path(), "id_rsa", &key, None);
    let server = ssh_server::start(Options::key(key.public_key().clone()));

    let (link, mut out) = pane(&server, key_auth(path, None));
    out.until("welcome 0", 30);
    assert_eq!(link.state(), ConnState::Connected);
    assert_eq!(server.seen.refused_logins.load(std::sync::atomic::Ordering::SeqCst), 0);
    Box::new(link).close().unwrap();
}

#[test]
fn a_missing_key_file_and_a_damaged_key_file_are_told_apart() {
    let scratch = Scratch::new("auth-files");
    let server = ssh_server::start(Options::key(ssh_server::ed25519().public_key().clone()));

    let missing = scratch.path().join("no_such_key");
    let (link, mut out) = pane(&server, key_auth(missing, None));
    let line = failure_line(&mut out);
    assert!(line.contains("the key file cannot be read"), "{line:?}");
    Box::new(link).close().unwrap();

    let damaged = scratch.path().join("damaged");
    ssh_server::write_private(&damaged, b"this is text, not a key\n");
    let (link, mut out) = pane(&server, key_auth(damaged.clone(), None));
    let line = failure_line(&mut out);
    assert!(line.contains("the key file is damaged"), "{line:?}");
    assert!(!line.contains("passphrase"), "no passphrase was given, so none is mentioned: {line:?}");
    Box::new(link).close().unwrap();

    let (link, mut out) = pane(&server, key_auth(damaged, Some("some passphrase")));
    let line = failure_line(&mut out);
    assert!(line.contains("the passphrase is wrong or the key file is damaged"), "{line:?}");
    assert!(!line.contains("some passphrase"), "{line:?}");
    Box::new(link).close().unwrap();
    assert_no_login(&server);
}
