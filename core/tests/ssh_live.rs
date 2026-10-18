//! Live tests against a real OpenSSH server. Every test is ignored: it needs a running `sshd` and the keys for it, which
//! `scripts/ssh-loopback-check.sh` makes (a throwaway `sshd` as the current user on 127.0.0.1) and passes in through
//! these variables:
//!
//! - `HOLZWAVE_TEST_SSH_HOST`, `_PORT`, `_USER`: where to connect and as whom
//! - `HOLZWAVE_TEST_SSH_KNOWN_HOSTS`: a known hosts file that holds the server's host key (every login is strict)
//! - `HOLZWAVE_TEST_SSH_PASSPHRASE`: the passphrase of the keys named `..._PASS`
//! - `HOLZWAVE_TEST_SSH_KEY_ED25519`, `_KEY_ED25519_PASS`: an Ed25519 key in the OpenSSH format
//! - `HOLZWAVE_TEST_SSH_KEY_RSA_PEM`, `_KEY_RSA_PEM_PASS`: an RSA key in PEM format (PKCS#1)
//! - `HOLZWAVE_TEST_SSH_KEY_RSA_PKCS8`, `_KEY_RSA_PKCS8_PASS`: an RSA key in PKCS#8 format (the second one may be empty:
//!   the script could not make it and said so)
//! - `HOLZWAVE_TEST_SSH_AGENT_SOCK`: the socket of a throwaway `ssh-agent` that holds the Ed25519 and the RSA (PEM) key above,
//!   and `_AGENT_PUB_ED25519`, `_AGENT_PUB_RSA`, `_AGENT_PUB_NOT_HELD`: the public key files of those two keys and of a key
//!   the agent does not hold
//!
//! A test that cannot find a variable fails and names it. Each proof prints one line, so the output of the script says
//! what passed.

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::ssh_server::Out;
use holzwave_core::conn::{ConnState, Link};
use holzwave_core::files::{FileSource, SftpSource};
#[cfg(unix)]
use holzwave_core::pool::{Connection, ForwardError, ForwardSink, ForwardedStream};
use holzwave_core::ssh::{ExecCancel, ExecEnd, ExecLimits, HostKeyPolicy, SshAuth, SshExec, SshLink, SshTarget};

/// The seconds a wait for the real server may take.
const WAIT: u64 = 20;

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set; run this test through scripts/ssh-loopback-check.sh"))
}

/// The server of the script, with `auth`. Strict: the host key must be in the known hosts file already.
fn target(auth: SshAuth) -> SshTarget {
    let mut target = SshTarget::new(var("HOLZWAVE_TEST_SSH_HOST"), var("HOLZWAVE_TEST_SSH_USER"), auth);
    target.port = var("HOLZWAVE_TEST_SSH_PORT").parse().expect("HOLZWAVE_TEST_SSH_PORT is a port number");
    target.known_hosts = Some(PathBuf::from(var("HOLZWAVE_TEST_SSH_KNOWN_HOSTS")));
    target.host_key = HostKeyPolicy::Strict;
    target
}

fn key(name: &str, passphrase: bool) -> SshAuth {
    SshAuth::Key { path: var(name).into(), passphrase: passphrase.then(|| var("HOLZWAVE_TEST_SSH_PASSPHRASE")) }
}

/// Log a pane in and prove the shell runs: it types a sum and waits for the answer, which the typed line does not
/// contain.
fn prove_shell(target: SshTarget) {
    let mut link = SshLink::connect(target, 100, 30).unwrap();
    let mut out = Out::new(link.take_reader().unwrap());
    let mut writer = link.take_writer().unwrap();
    writer.write_all(b"echo LIVE_$((6*7))\n").unwrap();
    out.until("LIVE_42", WAIT);
    assert_eq!(link.state(), ConnState::Connected);
    Box::new(link).close().unwrap();
}

#[test]
#[ignore = "needs a real sshd: run scripts/ssh-loopback-check.sh"]
fn real_openssh_logins() {
    let keys = [
        ("an Ed25519 key in OpenSSH format", "HOLZWAVE_TEST_SSH_KEY_ED25519", false),
        ("an Ed25519 key in OpenSSH format with a passphrase", "HOLZWAVE_TEST_SSH_KEY_ED25519_PASS", true),
        ("an RSA key in PEM format (PKCS#1)", "HOLZWAVE_TEST_SSH_KEY_RSA_PEM", false),
        ("an RSA key in PEM format (PKCS#1) with a passphrase", "HOLZWAVE_TEST_SSH_KEY_RSA_PEM_PASS", true),
        ("an RSA key in PKCS#8 format", "HOLZWAVE_TEST_SSH_KEY_RSA_PKCS8", false),
        ("an RSA key in PKCS#8 format with a passphrase", "HOLZWAVE_TEST_SSH_KEY_RSA_PKCS8_PASS", true),
    ];
    for (what, name, passphrase) in keys {
        // The script leaves a variable empty when its tools could not make that key; it said so when it started.
        if std::env::var(name).is_ok_and(|path| path.is_empty()) {
            println!("live: NOT COVERED: {what} (the script could not make it)");
            continue;
        }
        prove_shell(target(key(name, passphrase)));
        println!("live: {what} logs in to a pane");
    }
}

fn agent(identity: Option<&str>) -> SshAuth {
    SshAuth::Agent { socket: Some(var("HOLZWAVE_TEST_SSH_AGENT_SOCK").into()), identity: identity.map(|name| var(name).into()) }
}

/// A real `ssh-agent` signs for a real `sshd`: no key file is given, the private keys are in the agent only. Without a
/// selector the agent's own order decides which key is tried first; with a public key file exactly that key is used (an
/// Ed25519 and an RSA key, so `rsa-sha2` signing through the agent is proven too); a public key the agent does not hold is
/// refused in words and never reaches the server.
#[test]
#[ignore = "needs a real sshd and a real ssh-agent: run scripts/ssh-loopback-check.sh"]
fn real_openssh_agent() {
    prove_shell(target(agent(None)));
    println!("live: a pane logs in through the real ssh-agent with no key file");
    prove_shell(target(agent(Some("HOLZWAVE_TEST_SSH_AGENT_PUB_ED25519"))));
    println!("live: the public key file of the Ed25519 key picks it and the agent signs");
    prove_shell(target(agent(Some("HOLZWAVE_TEST_SSH_AGENT_PUB_RSA"))));
    println!("live: the public key file of the RSA key picks it and the agent signs with rsa-sha2");

    let mut link = SshLink::connect(target(agent(Some("HOLZWAVE_TEST_SSH_AGENT_PUB_NOT_HELD"))), 100, 30).unwrap();
    let mut out = Out::new(link.take_reader().unwrap());
    out.until("the public key is not in the ssh-agent", WAIT);
    out.until("Press Enter to try again", WAIT);
    assert_eq!(link.state(), ConnState::Failed);
    assert!(!out.seen.contains(&var("HOLZWAVE_TEST_SSH_AGENT_SOCK")), "the socket path is in the message");
    Box::new(link).close().unwrap();
    println!("live: a public key the agent does not hold is refused in words");

    // The same server, strict exec, through the agent: the command runner logs in the same way.
    let exec = SshExec::new("Live host", target(agent(None))).unwrap();
    let result = exec.exec("echo AGENT_$((6*7))", &ExecLimits::default(), &ExecCancel::new()).unwrap();
    assert_eq!(String::from_utf8_lossy(&result.stdout).trim(), "AGENT_42");
    println!("live: one strict exec command ran through the agent and returned AGENT_42");
}

#[test]
#[ignore = "needs a real sshd: run scripts/ssh-loopback-check.sh"]
fn real_openssh_jump() {
    // The server is its own jump host: the pane asks it for a `direct-tcpip` channel to its own port and logs in a
    // second time through that channel.
    let mut behind = target(key("HOLZWAVE_TEST_SSH_KEY_ED25519", false));
    behind.jump = Some(Box::new(target(key("HOLZWAVE_TEST_SSH_KEY_RSA_PEM", false))));
    prove_shell(behind);
    println!("live: a pane reaches the server through a direct-tcpip channel of the same server");
}

#[test]
#[ignore = "needs a real sshd: run scripts/ssh-loopback-check.sh"]
fn real_openssh_sftp_and_exec() {
    let source = SftpSource::new("Live host", target(key("HOLZWAVE_TEST_SSH_KEY_ED25519", false))).unwrap();
    let home = source.home().unwrap();
    let listing = source.list(&home).unwrap();
    assert_eq!(listing.path, home);
    println!("live: sftp lists {} ({} entries)", home, listing.entries.len());
    drop(source);

    let exec = SshExec::new("Live host", target(key("HOLZWAVE_TEST_SSH_KEY_ED25519_PASS", true))).unwrap();
    let started = Instant::now();
    let result = exec.exec("echo LIVE_$((6*7))", &ExecLimits::default(), &ExecCancel::new()).unwrap();
    assert_eq!(String::from_utf8_lossy(&result.stdout).trim(), "LIVE_42");
    assert_eq!((result.status, result.end), (Some(0), ExecEnd::Finished));
    assert!(started.elapsed() < Duration::from_secs(WAIT), "the command took too long");
    println!("live: one strict exec command ran and returned LIVE_42");
}

/// A sink that answers nothing: the forward is the point, not what comes through it.
#[cfg(unix)]
struct Silent;

#[cfg(unix)]
impl ForwardSink for Silent {
    fn serve(&self, _stream: ForwardedStream) {}
}

/// A socket path of the form the forward accepts that no other run has used.
#[cfg(unix)]
fn fresh_socket_path() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    format!("/tmp/holzwave-{:032x}.sock", nanos ^ (u128::from(std::process::id()) << 96))
}

/// What the design of the tunnel rests on, shown against the real server: `sshd` makes the socket, an owner-only file, when
/// it accepts the forward; it refuses a path that is already there; and it leaves the file behind when the forward is cancelled
/// and when the login ends (the OpenSSH of this check does not unlink it), with nothing listening on it any more. That is why
/// every start of a shell asks for a path nobody has asked for before.
#[cfg(unix)]
#[test]
#[ignore = "needs a real sshd: run scripts/ssh-loopback-check.sh"]
fn real_openssh_forwards_a_socket_and_refuses_a_path_that_is_there() {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    let path = fresh_socket_path();
    let first = Connection::standalone("first", target(key("HOLZWAVE_TEST_SSH_KEY_ED25519", false))).unwrap();
    let second = Connection::standalone("second", target(key("HOLZWAVE_TEST_SSH_KEY_ED25519", false))).unwrap();
    let ask = |connection: &std::sync::Arc<Connection>| {
        let c = connection.clone();
        let path = path.clone();
        connection.block_on(async move { c.forward(&path, std::sync::Arc::new(Silent)).await })
    };

    let guard = ask(&first).expect("sshd accepts a Unix socket forward (AllowStreamLocalForwarding)");
    let meta = std::fs::metadata(&path).expect("sshd made the socket");
    assert!(meta.file_type().is_socket(), "{path} is a socket");
    assert_eq!(meta.permissions().mode() & 0o077, 0, "the socket is owner-only: {:o}", meta.permissions().mode());
    assert!(std::os::unix::net::UnixStream::connect(&path).is_ok(), "something listens while the forward is up");
    println!("live: sshd made the forwarded socket, owner-only, and it accepts connections");

    assert!(matches!(ask(&second), Err(ForwardError::Refused)), "a path that is still there is refused");
    println!("live: sshd refuses a socket path that exists");

    first.block_on(guard.cancel());
    // The forward is gone: nothing listens, so the socket reaches nothing. The file itself may stay (see above).
    let started = Instant::now();
    while std::os::unix::net::UnixStream::connect(&path).is_ok() {
        assert!(started.elapsed() < Duration::from_secs(WAIT), "the socket still accepts connections after the cancel");
        std::thread::sleep(Duration::from_millis(50));
    }
    let left = std::path::Path::new(&path).exists();
    println!("live: after the cancel nothing listens on the socket (the file {} behind)", if left { "stays" } else { "is gone, not left" });
    if left {
        // The same path would be refused again: a new start must ask for a new path, never for this one.
        assert!(matches!(ask(&second), Err(ForwardError::Refused)), "a file that stays still refuses the path");
        println!("live: the path of a file that stays is refused again, so every start asks for a new path");
        let _ = std::fs::remove_file(&path);
    }
}
