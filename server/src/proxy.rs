//! The edge of the listener: how the server is secured, who may connect, and whose word about the client address counts.
//!
//! [`Admission`] decides before any byte is read whether a connection may exist at all, and its [`Permit`] gives the slot back
//! when it is dropped. [`TrustedProxies::client_address`] says who the client is: the forwarded headers count only in the
//! proxy mode and only from a trusted proxy, and only the right side of the address list is believed.
//!
//! Header values come in as `&str` slices from the caller, so this module does not depend on the HTTP crates.

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::config::{Cidr, MAX_TRUSTED_PROXIES};
use crate::limits::{self, Limits};

/// The most entries of an `X-Forwarded-For` list that are read.
pub const MAX_FORWARDED_ENTRIES: usize = 16;

/// The most bytes of `X-Forwarded-For` (all lines together) that are read.
pub const MAX_FORWARDED_BYTES: usize = 1024;

/// The bits of an IPv6 address that name a client for the per-address cap: one /64 is one subscriber.
const COUNTED_PREFIX_BITS: u32 = 64;

/// How the listener is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// TLS from a certificate file and a key file.
    Files,
    /// TLS from a certificate that ACME issues.
    Acme,
    /// Plain HTTP from the trusted proxies only.
    Proxy,
}

/// Why a connection or a forwarded header was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The server holds its maximum of connections.
    TooManyConnections,
    /// One address holds its maximum of connections.
    TooManyFromAddress,
    /// The peer is not a trusted proxy.
    UntrustedPeer,
    /// The forwarded scheme is missing or is not exactly `https`.
    NotSecure,
    /// The forwarded address list is not usable.
    BadForwardedFor,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refusal::TooManyConnections => "the server holds its maximum of connections",
            Refusal::TooManyFromAddress => "this address holds its maximum of connections",
            Refusal::UntrustedPeer => "the peer is not a trusted proxy",
            Refusal::NotSecure => "the proxy did not say that the request came in over https",
            Refusal::BadForwardedFor => "the forwarded address list is not usable",
        })
    }
}

impl std::error::Error for Refusal {}

/// A mistake in the configuration of the proxies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyError {
    /// The proxy mode needs at least one trusted proxy.
    NoTrustedProxies,
    /// More entries than the configuration allows.
    TooManyTrustedProxies,
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProxyError::NoTrustedProxies => {
                f.write_str("the proxy mode needs tls.trusted_proxies, the addresses of the proxies allowed to connect")
            }
            ProxyError::TooManyTrustedProxies => {
                write!(f, "tls.trusted_proxies lists more than {MAX_TRUSTED_PROXIES} entries")
            }
        }
    }
}

impl std::error::Error for ProxyError {}

/// The proxies whose word about the client counts.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies {
    list: Vec<Cidr>,
}

impl TrustedProxies {
    /// The proxies of the configuration. An empty list is refused: the proxy mode has no use for it and every other mode
    /// uses [`TrustedProxies::none`].
    pub fn new(list: &[Cidr]) -> Result<TrustedProxies, ProxyError> {
        if list.is_empty() {
            return Err(ProxyError::NoTrustedProxies);
        }
        if list.len() > MAX_TRUSTED_PROXIES {
            return Err(ProxyError::TooManyTrustedProxies);
        }
        Ok(TrustedProxies { list: list.to_vec() })
    }

    /// No proxy: the list of the modes that serve TLS themselves.
    pub fn none() -> TrustedProxies {
        TrustedProxies { list: Vec::new() }
    }

    /// Whether `ip` is a trusted proxy. An IPv4-mapped IPv6 address is the IPv4 address.
    pub fn contains(&self, ip: IpAddr) -> bool {
        self.list.iter().any(|net| net.contains(ip))
    }

    /// The address of the client of a connection from `peer`.
    ///
    /// `forwarded_for` holds the values of all `X-Forwarded-For` header lines and `forwarded_proto` those of all
    /// `X-Forwarded-Proto` lines, as the caller found them. Both are ignored completely unless the mode is
    /// [`Mode::Proxy`] and `peer` is a trusted proxy; for any other peer the answer is the peer itself (the proxy mode
    /// refuses such a peer at [`Admission::try_admit`]).
    ///
    /// For a trusted proxy the scheme must be given exactly once and be `https`, otherwise the answer is
    /// [`Refusal::NotSecure`]. A missing address list means the proxy itself is the client. A list is at most
    /// [`MAX_FORWARDED_BYTES`] bytes and [`MAX_FORWARDED_ENTRIES`] entries, and every entry must be an IP address, otherwise
    /// the answer is [`Refusal::BadForwardedFor`]. The client is the right-most entry that is not a trusted proxy, so a
    /// part of the list that a client wrote is never believed; when every entry is a trusted proxy, it is the left-most.
    pub fn client_address(
        &self,
        mode: Mode,
        peer: IpAddr,
        forwarded_for: &[&str],
        forwarded_proto: &[&str],
    ) -> Result<IpAddr, Refusal> {
        let peer = peer.to_canonical();
        if mode != Mode::Proxy || !self.contains(peer) {
            return Ok(peer);
        }
        match forwarded_proto {
            [one] if one.trim().eq_ignore_ascii_case("https") => {}
            _ => return Err(Refusal::NotSecure),
        }
        if forwarded_for.is_empty() {
            return Ok(peer);
        }
        if forwarded_for.iter().map(|line| line.len()).sum::<usize>() > MAX_FORWARDED_BYTES {
            return Err(Refusal::BadForwardedFor);
        }
        let mut entries: Vec<IpAddr> = Vec::new();
        for entry in forwarded_for.iter().flat_map(|line| line.split(',')) {
            if entries.len() == MAX_FORWARDED_ENTRIES {
                return Err(Refusal::BadForwardedFor);
            }
            let ip = entry.trim().parse::<IpAddr>().map_err(|_| Refusal::BadForwardedFor)?;
            entries.push(ip.to_canonical());
        }
        let client = entries.iter().rev().find(|ip| !self.contains(**ip)).or_else(|| entries.first());
        client.copied().ok_or(Refusal::BadForwardedFor)
    }
}

/// The address a connection is counted under: an IPv4 address as it is, an IPv6 address by its first 64 bits.
fn key_of(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => {
            let bits = u128::from(v6) & (u128::MAX << (128 - COUNTED_PREFIX_BITS));
            IpAddr::V6(Ipv6Addr::from(bits))
        }
    }
}

struct Inner {
    max_total: u32,
    max_per_address: u32,
    /// In the proxy mode: the only peers that may connect. They count in the total and not per address.
    proxies: Option<TrustedProxies>,
    total: AtomicU32,
    per_address: Mutex<HashMap<IpAddr, u32>>,
}

/// Limits the connections, before any byte is read.
#[derive(Clone)]
pub struct Admission {
    inner: Arc<Inner>,
}

/// A connection's slot. Dropping it gives the slot back.
pub struct Permit {
    inner: Arc<Inner>,
    /// The address the slot is counted under; `None` for a trusted proxy.
    key: Option<IpAddr>,
}

impl Admission {
    /// The caps of the limits module.
    pub fn with_defaults(mode: Mode, trusted: TrustedProxies) -> Result<Admission, ProxyError> {
        Admission::new(mode, trusted, limits::MAX_CONNECTIONS, limits::MAX_CONNECTIONS_PER_ADDRESS)
    }

    /// The caps of the resolved limits.
    pub fn from_limits(mode: Mode, trusted: TrustedProxies, limits: &Limits) -> Result<Admission, ProxyError> {
        Admission::new(mode, trusted, limits.max_connections, limits.max_connections_per_address)
    }

    /// An admission with the given caps. In the proxy mode `trusted` must not be empty, and only its peers may connect.
    pub fn new(
        mode: Mode,
        trusted: TrustedProxies,
        max_total: u32,
        max_per_address: u32,
    ) -> Result<Admission, ProxyError> {
        let proxies = if mode == Mode::Proxy {
            if trusted.list.is_empty() {
                return Err(ProxyError::NoTrustedProxies);
            }
            Some(trusted)
        } else {
            None
        };
        Ok(Admission {
            inner: Arc::new(Inner {
                max_total,
                max_per_address,
                proxies,
                total: AtomicU32::new(0),
                per_address: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// The connections that hold a slot now.
    pub fn open_connections(&self) -> u32 {
        self.inner.total.load(Ordering::Acquire)
    }

    /// The addresses that hold a slot now (the table has no entry for an address at zero).
    pub fn tracked_addresses(&self) -> usize {
        self.inner.per_address.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// A slot for a connection from `peer`, or the reason there is none.
    ///
    /// In the proxy mode only a trusted proxy gets a slot, and it is not counted per address, because every client arrives
    /// from it. Otherwise an IPv4 address is counted as it is and an IPv6 address by its first 64 bits.
    pub fn try_admit(&self, peer: IpAddr) -> Result<Permit, Refusal> {
        let inner = &self.inner;
        let per_address = match &inner.proxies {
            Some(proxies) if proxies.contains(peer) => false,
            Some(_) => return Err(Refusal::UntrustedPeer),
            None => true,
        };
        inner
            .total
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < inner.max_total).then_some(n + 1))
            .map_err(|_| Refusal::TooManyConnections)?;
        if !per_address {
            return Ok(Permit { inner: Arc::clone(inner), key: None });
        }
        let key = key_of(peer);
        let mut map = inner.per_address.lock().unwrap_or_else(PoisonError::into_inner);
        let count = map.entry(key).or_insert(0);
        if *count >= inner.max_per_address {
            if *count == 0 {
                map.remove(&key);
            }
            drop(map);
            inner.total.fetch_sub(1, Ordering::AcqRel);
            return Err(Refusal::TooManyFromAddress);
        }
        *count += 1;
        Ok(Permit { inner: Arc::clone(inner), key: Some(key) })
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(key) = self.key {
            let mut map = self.inner.per_address.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(count) = map.get_mut(&key) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    map.remove(&key);
                }
            }
        }
        self.inner.total.fetch_sub(1, Ordering::AcqRel);
    }
}
