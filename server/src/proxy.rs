//! The edge of the listener: how the server is secured, who may connect, and whose word about the client address counts.
//!
//! [`Admission`] decides before any byte is read whether a connection may exist at all, and its [`Permit`] gives the slot back
//! when it is dropped. [`client_address`] says who the client is.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::limits;

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

/// The address of the client of a connection from `peer`.
pub fn client_address(
    _mode: Mode,
    peer: IpAddr,
    _forwarded_for: &[&str],
    _forwarded_proto: &[&str],
) -> Result<IpAddr, Refusal> {
    Ok(peer.to_canonical())
}

/// The address a connection is counted under: an IPv4 address as it is, an IPv6 address by its first 64 bits.
fn key_of(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => {
            let bits = u128::from(v6) & (u128::MAX << 64);
            IpAddr::V6(Ipv6Addr::from(bits))
        }
    }
}

struct Inner {
    max_total: u32,
    max_per_address: u32,
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
    key: IpAddr,
}

impl Admission {
    /// The caps of the limits module.
    pub fn with_defaults(_mode: Mode) -> Admission {
        Admission::new(limits::MAX_CONNECTIONS, limits::MAX_CONNECTIONS_PER_ADDRESS)
    }

    /// An admission with the given caps.
    pub fn new(max_total: u32, max_per_address: u32) -> Admission {
        Admission {
            inner: Arc::new(Inner {
                max_total,
                max_per_address,
                total: AtomicU32::new(0),
                per_address: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The connections that hold a slot now.
    pub fn open_connections(&self) -> u32 {
        self.inner.total.load(Ordering::Acquire)
    }

    /// A slot for a connection from `peer`, or the reason there is none.
    pub fn try_admit(&self, peer: IpAddr) -> Result<Permit, Refusal> {
        let inner = &self.inner;
        inner
            .total
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < inner.max_total).then_some(n + 1))
            .map_err(|_| Refusal::TooManyConnections)?;
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
        Ok(Permit { inner: Arc::clone(inner), key })
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut map = self.inner.per_address.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = map.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                map.remove(&self.key);
            }
        }
        drop(map);
        self.inner.total.fetch_sub(1, Ordering::AcqRel);
    }
}
