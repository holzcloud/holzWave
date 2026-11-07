//! The check before a connection to a device on the local network.
//!
//! Android 17 lets an app reach a private address (a NAS, a home server, another computer on the same Wi-Fi) only after the
//! person has granted a runtime permission, and a connection made without it does not fail: it times out. So a connection
//! to such an address asks the platform first, at the moment it is needed and not at the start of the app, and a refusal is
//! a plain sentence instead of a timeout.
//!
//! This file has the rule and nothing of Android: [`is_local`] says which addresses count, [`Gate`] is what the platform
//! implements (the Android shell installs one with [`install`]), and [`ensure`] is what a connection calls. Where no gate is
//! installed (every other platform) [`ensure`] returns at once, before it resolves or spawns anything, so the desktop and
//! the browser build pay nothing for it.
//!
//! The gate is asked only for an address that counts as local: a host name is resolved first (bounded), and a loopback or a
//! public address never reaches the gate. A name that cannot be resolved is left to the connection, which reports its own
//! error.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// What the platform answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// The app may reach the local network.
    Granted,
    /// The person said no, or the system did, or the answer could not be had.
    Denied,
    /// The question could not be asked because the app has no window the person can see (a reconnect in the background).
    Background,
}

/// The platform's answer to "may this app reach the local network", asked when a connection to such an address is about to
/// be made. The call blocks, possibly for as long as a person takes to answer a dialog, so it runs on a thread that may block.
pub trait Gate: Send + Sync + 'static {
    fn ask(&self) -> Access;
}

static GATE: OnceLock<Arc<dyn Gate>> = OnceLock::new();

/// Installs the gate of the platform, once per process; `false` when one was installed already.
pub fn install(gate: Arc<dyn Gate>) -> bool {
    GATE.set(gate).is_ok()
}

/// The longest a host name may take to resolve here before the connection is left to find out for itself.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(15);

/// Whether the address is one that Android's local-network permission covers: the private IPv4 ranges (10/8, 172.16/12,
/// 192.168/16), the shared range of carrier-grade NAT (100.64/10, where mesh VPNs live), link-local addresses (169.254/16,
/// fe80::/10), the unique local IPv6 range (fc00::/7) and multicast. Loopback and every public address are not.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(address) => is_local_v4(address),
        IpAddr::V6(address) => match address.to_ipv4_mapped() {
            Some(mapped) => is_local_v4(mapped),
            None => is_local_v6(address),
        },
    }
}

fn is_local_v4(address: Ipv4Addr) -> bool {
    let [a, b, _, _] = address.octets();
    a == 10
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (224..=239).contains(&a)
}

fn is_local_v6(address: Ipv6Addr) -> bool {
    let first = address.segments()[0];
    (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80 || (first & 0xff00) == 0xff00
}

/// A name in the `.local` domain, which is answered by multicast on the local network and nowhere else.
pub fn is_local_name(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    host.len() > ".local".len() && host[host.len() - ".local".len()..].eq_ignore_ascii_case(".local")
}

/// The sentence a refused connection shows. It names the host and what to do, and nothing the system said.
pub fn refusal_text(host: &str, access: Access) -> String {
    let host: String = host.chars().filter(|c| !c.is_control()).take(100).collect();
    match access {
        Access::Background => format!(
            "{host} is on your local network, and holzWave can only ask for the permission to reach it while it is on screen. \
             Open holzWave and try again"
        ),
        _ => format!(
            "holzWave is not allowed to reach {host}, which is on your local network. Allow Nearby devices for holzWave in the \
             Android settings (Apps, holzWave, Permissions), then try again"
        ),
    }
}

/// Whether `host` needs the permission: an address that counts, a `.local` name, or a name that resolves to one.
fn needs_permission(host: &str, port: u16, resolve: &dyn Fn(&str, u16) -> Vec<IpAddr>) -> bool {
    let bare = host.trim_matches(['[', ']']);
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return is_local(ip);
    }
    if is_local_name(bare) {
        return true;
    }
    resolve(bare, port).into_iter().any(is_local)
}

/// The decision, with the gate and the resolver as arguments so that it can be tried without a platform.
fn decide(gate: &dyn Gate, host: &str, port: u16, resolve: &dyn Fn(&str, u16) -> Vec<IpAddr>) -> Result<(), String> {
    if !needs_permission(host, port, resolve) {
        return Ok(());
    }
    match gate.ask() {
        Access::Granted => Ok(()),
        other => Err(refusal_text(host, other)),
    }
}

fn system_resolve(host: &str, port: u16) -> Vec<IpAddr> {
    (host, port).to_socket_addrs().map(|found| found.map(|a| a.ip()).collect()).unwrap_or_default()
}

/// Blocking: may a connection to `host` be made now? `Ok` where no gate is installed, for an address that does not count,
/// and when the permission is held (asking for it first when it is not). `Err` is the sentence to show.
pub fn ensure_blocking(host: &str, port: u16) -> Result<(), String> {
    match GATE.get() {
        None => Ok(()),
        Some(gate) => decide(gate.as_ref(), host, port, &system_resolve),
    }
}

/// [`ensure_blocking`] for a task on the runtime: the lookup and the question run on a blocking thread.
pub async fn ensure(host: &str, port: u16) -> Result<(), String> {
    if GATE.get().is_none() {
        return Ok(());
    }
    let host = host.to_owned();
    // The lookup is bounded; the question to the person is not (a dialog waits for as long as the person does).
    let needs = {
        let host = host.clone();
        let lookup = tokio::task::spawn_blocking(move || needs_permission(&host, port, &system_resolve));
        match tokio::time::timeout(RESOLVE_TIMEOUT, lookup).await {
            Ok(Ok(needs)) => needs,
            _ => false,
        }
    };
    if !needs {
        return Ok(());
    }
    require(&host).await
}

/// Asks the gate now, for a host that is already known to be on the local network (a caller that has resolved the name
/// itself). `Ok` where no gate is installed.
pub async fn require(host: &str) -> Result<(), String> {
    let Some(gate) = GATE.get() else { return Ok(()) };
    let gate = gate.clone();
    let answer = tokio::task::spawn_blocking(move || gate.ask()).await.unwrap_or(Access::Denied);
    match answer {
        Access::Granted => Ok(()),
        other => Err(refusal_text(host.trim_matches(['[', ']']), other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn the_private_link_local_shared_and_multicast_ranges_are_local() {
        for text in [
            "10.0.0.1", "10.255.255.255", "172.16.0.1", "172.31.255.255", "192.168.0.1", "192.168.255.254", "100.64.0.1",
            "100.127.255.255", "169.254.1.1", "224.0.0.251", "239.255.255.250", "fc00::1", "fd12:3456::1", "fe80::1", "ff02::fb",
            "::ffff:192.168.1.5",
        ] {
            assert!(is_local(ip(text)), "{text} should be local");
        }
    }

    #[test]
    fn loopback_and_public_addresses_are_not_local() {
        for text in [
            "127.0.0.1", "127.255.255.254", "8.8.8.8", "172.15.255.255", "172.32.0.1", "192.167.1.1", "192.169.1.1", "100.63.255.255",
            "100.128.0.1", "11.0.0.1", "9.255.255.255", "::1", "2001:4860:4860::8888", "fb00::1", "::ffff:8.8.8.8", "::",
        ] {
            assert!(!is_local(ip(text)), "{text} should not be local");
        }
    }

    #[test]
    fn a_local_domain_name_is_local_and_a_lookalike_is_not() {
        assert!(is_local_name("printer.local"));
        assert!(is_local_name("Printer.LOCAL"));
        assert!(is_local_name("printer.local."));
        assert!(!is_local_name("local"));
        assert!(!is_local_name(".local"));
        assert!(!is_local_name("example.com"));
        assert!(!is_local_name("notlocal"));
        assert!(!is_local_name("printer.localhost"));
        assert!(!is_local_name("printer.local.example.com"));
    }

    /// A gate that answers as told and counts how often it was asked.
    struct Scripted {
        answer: Access,
        asked: Mutex<u32>,
    }

    impl Scripted {
        fn new(answer: Access) -> Scripted {
            Scripted { answer, asked: Mutex::new(0) }
        }
        fn asked(&self) -> u32 {
            *self.asked.lock().unwrap()
        }
    }

    impl Gate for Scripted {
        fn ask(&self) -> Access {
            *self.asked.lock().unwrap() += 1;
            self.answer
        }
    }

    fn no_lookup(_: &str, _: u16) -> Vec<IpAddr> {
        panic!("an address that is a literal needs no lookup");
    }

    #[test]
    fn an_address_that_is_not_local_never_asks() {
        let gate = Scripted::new(Access::Denied);
        assert_eq!(decide(&gate, "127.0.0.1", 22, &no_lookup), Ok(()));
        assert_eq!(decide(&gate, "8.8.8.8", 443, &no_lookup), Ok(()));
        assert_eq!(decide(&gate, "[::1]", 443, &no_lookup), Ok(()));
        assert_eq!(gate.asked(), 0, "the platform was not bothered");
    }

    #[test]
    fn a_local_address_asks_and_a_grant_lets_it_through() {
        let gate = Scripted::new(Access::Granted);
        assert_eq!(decide(&gate, "192.168.1.20", 22, &no_lookup), Ok(()));
        assert_eq!(decide(&gate, "[fd00::5]", 22, &no_lookup), Ok(()));
        assert_eq!(decide(&gate, "nas.local", 22, &|_, _| panic!("a .local name needs no lookup")), Ok(()));
        assert_eq!(gate.asked(), 3);
    }

    #[test]
    fn a_refusal_is_a_plain_sentence_that_names_the_host_and_the_way_out() {
        let gate = Scripted::new(Access::Denied);
        let text = decide(&gate, "192.168.1.20", 22, &no_lookup).unwrap_err();
        assert!(text.contains("192.168.1.20"), "{text}");
        assert!(text.contains("local network"), "{text}");
        assert!(text.contains("Android settings") && text.contains("Nearby devices"), "{text}");
        assert!(!text.contains("timed out") && !text.contains("timeout"), "{text}");
        let background = decide(&Scripted::new(Access::Background), "10.0.0.2", 22, &no_lookup).unwrap_err();
        assert!(background.contains("on screen") && background.contains("10.0.0.2"), "{background}");
    }

    #[test]
    fn a_name_is_resolved_and_asks_only_when_it_resolves_to_a_local_address() {
        let gate = Scripted::new(Access::Denied);
        let local = |_: &str, _: u16| vec![ip("203.0.113.9"), ip("192.168.7.7")];
        assert!(decide(&gate, "nas.example.net", 443, &local).is_err());
        assert_eq!(gate.asked(), 1);
        let public = |_: &str, _: u16| vec![ip("203.0.113.9")];
        assert_eq!(decide(&gate, "example.net", 443, &public), Ok(()));
        let nothing = |_: &str, _: u16| Vec::new();
        assert_eq!(decide(&gate, "missing.example.net", 443, &nothing), Ok(()), "the connection reports its own error");
        assert_eq!(gate.asked(), 1);
    }

    #[test]
    fn a_host_text_cannot_inject_control_characters_into_the_sentence() {
        let text = refusal_text("192.168.1.2\r\nfake line\x1b[31m", Access::Denied);
        assert!(!text.chars().any(char::is_control), "{text:?}");
    }

    #[test]
    fn with_no_gate_installed_nothing_is_asked_or_resolved() {
        // No test of this crate installs a gate (it is process wide), so the answer is the platform's default everywhere.
        assert_eq!(ensure_blocking("192.168.1.20", 22), Ok(()));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        assert_eq!(runtime.block_on(ensure("nas.local", 22)), Ok(()));
    }
}
