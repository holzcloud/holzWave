//! Public-key login, the words of a key that cannot be used, and the jump-host path, against the in-process SSH server
//! of `common::ssh_server`. No Zig and no real host are needed.
//!
//! Every login test uses a server that knows the public key only (no password), so a password cannot be what let a
//! test pass. A key is generated at run time into a private folder that the test removes at its end.
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
