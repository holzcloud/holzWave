//! A locked vault does not touch a live SSH session: a pane that logged in while the vault was unlocked keeps carrying
//! traffic after the vault is locked, and a new login after the lock is refused with the locked text.
//!
//! The vault is the real `VaultStore` over the fake keystore, the server is the in-process one of `common::ssh_server`,
//! and the connection manager is the core's own, so the password is read from the vault exactly as a pane reads it.
//! The host's own tests of the locked answers (`host/tests/locked_paths.rs`) cannot start an SSH server, so the proof of
//! the live session is here. Not run: Android, a real Keystore, a real host.

mod common;

use std::io::Write;
use std::sync::Arc;

use common::ssh_server::{self, Options, Out};
use holzwave_core::conn::{ConnState, Link};
use holzwave_core::connections::{Connections, SecretChange};
use holzwave_core::profiles::{Auth, Profile};
use holzwave_core::secrets::SecretStore;
use holzwave_core::ssh::SshLink;
use holzwave_core::vault::{FakeClock, FakeKeystore, LOCKED_TEXT, VaultState, VaultStore};

#[test]
fn a_session_that_logged_in_while_unlocked_keeps_running_when_the_vault_locks() {
    let dir = common::scratch_dir("vault-live");
    let keystore = Arc::new(FakeKeystore::new());
    let clock = Arc::new(FakeClock::new(1_000));
    let vault = Arc::new(VaultStore::open(dir.join("secrets.vault"), keystore, clock));
    vault.unlock().expect("a new vault is created");

    let server = ssh_server::start(Options::password());
    let store: Arc<dyn SecretStore> = vault.clone();
    let (connections, _) = Connections::load(&dir, store, None, &[]);
    let mut profile = Profile::new("pw", "with password", "127.0.0.1");
    profile.port = server.addr.port();
    profile.user = "holz".to_owned();
    profile.auth = Auth::Password;
    connections.save(profile, SecretChange::Set { value: "pw".to_owned() }).expect("saved, with its password in the vault");

    // A login while unlocked: the password comes out of the vault and the shell answers.
    let target = connections.ssh_target("pw").expect("the target with the password from the vault");
    let mut link = SshLink::connect(target, 80, 24).expect("the login");
    let mut out = Out::new(link.take_reader().expect("a reader"));
    let mut writer = link.take_writer().expect("a writer");
    out.until("welcome 0", 10);
    writer.write_all(b"before").expect("typed");
    out.until("echo:before", 10);

    // The lock: the session is not asked anything and goes on carrying traffic.
    vault.lock();
    assert_eq!(vault.state(), VaultState::Locked);
    assert_eq!(link.state(), ConnState::Connected, "the lock did not end the session");
    writer.write_all(b"after").expect("typed after the lock");
    out.until("echo:after", 10);
    assert_eq!(link.state(), ConnState::Connected);
    assert_eq!(server.seen.logins.load(std::sync::atomic::Ordering::SeqCst), 1, "no login was made again");

    // A new login is a new read of the password, and the locked vault refuses it with its own text.
    let refused = connections.ssh_target("pw").expect_err("a locked vault gives no password");
    assert_eq!(refused.to_string(), LOCKED_TEXT);
    assert_eq!(server.seen.logins.load(std::sync::atomic::Ordering::SeqCst), 1);

    // After the unlock a new login works again, and the first session is still the same one.
    vault.unlock().expect("unlocked");
    let target = connections.ssh_target("pw").expect("the target again");
    let mut second = SshLink::connect(target, 80, 24).expect("the second login");
    let mut second_out = Out::new(second.take_reader().expect("a reader"));
    second_out.until("welcome 1", 10);
    writer.write_all(b"again").expect("typed on the first session");
    out.until("echo:again", 10);

    Box::new(second).close().expect("closed");
    Box::new(link).close().expect("closed");
    let _ = std::fs::remove_dir_all(&dir);
}
