//! The two switches of the device features, the operator's ceiling over them, and the gate that makes a feature that is off
//! look like a path that does not exist.
//!
//! - **Two switches, both off.** `sync` (the server as a sync target) and `remote` (sessions that live on the server) are kept
//!   in the `server_settings` table and cached here. A switch that was never set is off. [`Features::apply`] writes the store
//!   first and the cache second, under one lock, so the cache never shows a state the store does not hold.
//! - **A ceiling above them.** The configuration may say that a feature is not allowed at all ([`Ceiling`]). A switch is on only
//!   when it is stored as on and the ceiling allows it; the ceiling is the operator's word, the switch the administrator's.
//! - **The gate.** [`GATES`] is the one table of the paths that belong to a feature. The edge of the router asks it before any
//!   credential is read, and answers a gated path whose feature is off with the answer of a path that is not there, so a probe
//!   cannot tell a disabled feature from an absent one.

use std::borrow::Cow;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::watch;

use crate::device::DEVICE_IDLE_DEFAULT;
use crate::log::{self, Level};
use crate::store::{Store, StoreError};

/// One of the two features an administrator can switch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    /// The server as a sync target.
    Sync,
    /// Terminal sessions that live on the server and that a device attaches to.
    Remote,
}

impl Feature {
    /// Both features, in the order the answers list them.
    pub const ALL: [Feature; 2] = [Feature::Sync, Feature::Remote];

    /// The key of the switch in the store and the member of the answers and requests.
    pub fn key(self) -> &'static str {
        match self {
            Feature::Sync => "sync",
            Feature::Remote => "remote",
        }
    }

    fn index(self) -> usize {
        match self {
            Feature::Sync => 0,
            Feature::Remote => 1,
        }
    }
}

/// What the operator allows. A feature the ceiling does not allow stays off, whatever an administrator says; a ceiling that
/// is not written is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ceiling {
    /// Whether the sync target may be on.
    pub sync: bool,
    /// Whether remote sessions may be on.
    pub remote: bool,
}

impl Ceiling {
    /// Nothing is forbidden by the configuration.
    pub const OPEN: Ceiling = Ceiling { sync: true, remote: true };

    /// Whether the ceiling lets `feature` be on.
    pub fn allows(self, feature: Feature) -> bool {
        match feature {
            Feature::Sync => self.sync,
            Feature::Remote => self.remote,
        }
    }
}

impl Default for Ceiling {
    fn default() -> Ceiling {
        Ceiling::OPEN
    }
}

/// The settings of the device features that come from the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceSettings {
    /// What the operator allows.
    pub ceiling: Ceiling,
    /// How long a device may go unseen before its token ends.
    pub idle: Duration,
}

impl Default for DeviceSettings {
    fn default() -> DeviceSettings {
        DeviceSettings {
            ceiling: Ceiling::OPEN,
            idle: Duration::from_secs(u64::try_from(DEVICE_IDLE_DEFAULT).unwrap_or(0)),
        }
    }
}

/// Which switches are on, as they work now (a switch that is stored as on but that the ceiling forbids reads as off).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Switches {
    /// The sync target.
    pub sync: bool,
    /// Remote sessions.
    pub remote: bool,
}

impl Switches {
    /// Whether `feature` is on.
    pub fn get(self, feature: Feature) -> bool {
        match feature {
            Feature::Sync => self.sync,
            Feature::Remote => self.remote,
        }
    }

    /// Whether at least one feature is on.
    pub fn any(self) -> bool {
        self.sync || self.remote
    }
}

/// Which switches a call of [`Features::apply`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Changed {
    /// The sync switch changed, to this value.
    pub sync: Option<bool>,
    /// The remote switch changed, to this value.
    pub remote: Option<bool>,
}

impl Changed {
    /// The changes in the order of [`Feature::ALL`].
    pub fn list(self) -> Vec<(Feature, bool)> {
        [(Feature::Sync, self.sync), (Feature::Remote, self.remote)]
            .into_iter()
            .filter_map(|(feature, value)| value.map(|v| (feature, v)))
            .collect()
    }
}

/// What a path under a prefix of [`GATES`] needs before it is served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// At least one of the two features is on.
    Any,
    /// This feature is on.
    Feature(Feature),
}

/// The paths that belong to a feature, as prefixes of whole path segments. A path under a prefix is answered as a path that is
/// not there while its gate is closed. This is the one place: a route of the device features is mounted under one of these.
pub const GATES: [(&str, Gate); 3] = [
    ("/api/device/", Gate::Any),
    ("/sync/v1/", Gate::Feature(Feature::Sync)),
    ("/ws/device", Gate::Feature(Feature::Remote)),
];

/// The gate a path is under, if any. The path is read the way a person or a scanner could spell it: letters of either case,
/// repeated slashes, a trailing slash, dot segments and percent-escapes all count as the plain path.
pub fn gate_of(path: &str) -> Option<Gate> {
    GATES.iter().find(|(prefix, _)| is_under(path, prefix)).map(|(_, gate)| *gate)
}

/// Whether `path` lies under `prefix` as whole segments (`/api/device/` holds `/api/device` and `/api/device/me`, never
/// `/api/devices`).
pub fn is_under(path: &str, prefix: &str) -> bool {
    if path.contains('%') || path.contains("..") {
        let plain = resolve(path);
        return under(plain.iter().map(String::as_str), prefix);
    }
    under(segments(path), prefix)
}

fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|segment| !segment.is_empty() && *segment != ".")
}

fn under<'a>(path: impl Iterator<Item = &'a str>, prefix: &str) -> bool {
    let mut path = path;
    for wanted in segments(prefix) {
        match path.next() {
            Some(segment) if segment.eq_ignore_ascii_case(wanted) => {}
            _ => return false,
        }
    }
    true
}

/// The segments of a path after the percent-escapes are decoded, the dot segments are resolved and the empty ones dropped.
fn resolve(path: &str) -> Vec<String> {
    let decoded = decode(path);
    let mut out: Vec<String> = Vec::new();
    for segment in decoded.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other.to_owned()),
        }
    }
    out
}

fn decode(path: &str) -> Cow<'_, str> {
    if !path.contains('%') {
        return Cow::Borrowed(path);
    }
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let pair = if bytes[i] == b'%' && i + 2 < bytes.len() { hex_pair(bytes[i + 1], bytes[i + 2]) } else { None };
        match pair {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    Cow::Owned(String::from_utf8_lossy(&out).into_owned())
}

fn hex_pair(high: u8, low: u8) -> Option<u8> {
    let digit = |b: u8| char::from(b).to_digit(16);
    u8::try_from(digit(high)? * 16 + digit(low)?).ok()
}

/// The switches, the ceiling and the announcement of a change.
pub struct Features {
    store: Arc<Store>,
    ceiling: Ceiling,
    stored: [AtomicBool; 2],
    writers: Mutex<()>,
    announce: watch::Sender<Switches>,
}

impl fmt::Debug for Features {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Features(..)")
    }
}

impl Features {
    /// Reads the two switches from the store.
    pub fn load(store: Arc<Store>, ceiling: Ceiling) -> Result<Features, StoreError> {
        let (sync, remote) = store.read(|tx| Ok((tx.setting_get(Feature::Sync.key())?, tx.setting_get(Feature::Remote.key())?)))?;
        Ok(Features::with(store, ceiling, sync, remote))
    }

    /// The switches of the store, or both off when the store cannot be read: a feature that cannot be confirmed as on is off.
    pub fn load_or_off(store: Arc<Store>, ceiling: Ceiling) -> Features {
        match Features::load(store.clone(), ceiling) {
            Ok(features) => features,
            Err(_) => {
                log::event(Level::Error, "features.load_failed", &[]);
                Features::with(store, ceiling, false, false)
            }
        }
    }

    fn with(store: Arc<Store>, ceiling: Ceiling, sync: bool, remote: bool) -> Features {
        let (announce, _) = watch::channel(Switches {
            sync: sync && ceiling.sync,
            remote: remote && ceiling.remote,
        });
        Features { store, ceiling, stored: [AtomicBool::new(sync), AtomicBool::new(remote)], writers: Mutex::new(()), announce }
    }

    /// The ceiling of the configuration.
    pub fn ceiling(&self) -> Ceiling {
        self.ceiling
    }

    /// Whether the switch of `feature` is stored as on, whatever the ceiling says.
    pub fn stored(&self, feature: Feature) -> bool {
        self.stored[feature.index()].load(Ordering::SeqCst)
    }

    /// Whether `feature` is on: stored as on and allowed by the ceiling.
    pub fn is_on(&self, feature: Feature) -> bool {
        self.ceiling.allows(feature) && self.stored(feature)
    }

    /// Whether at least one feature is on.
    pub fn any_on(&self) -> bool {
        Feature::ALL.into_iter().any(|feature| self.is_on(feature))
    }

    /// Which features are on now.
    pub fn effective(&self) -> Switches {
        Switches { sync: self.is_on(Feature::Sync), remote: self.is_on(Feature::Remote) }
    }

    /// Whether `gate` lets a request through.
    pub fn passes(&self, gate: Gate) -> bool {
        match gate {
            Gate::Any => self.any_on(),
            Gate::Feature(feature) => self.is_on(feature),
        }
    }

    /// Whether `path` is under a gate that is closed now.
    pub fn is_closed(&self, path: &str) -> bool {
        gate_of(path).is_some_and(|gate| !self.passes(gate))
    }

    /// A receiver that sees the features that are on, and wakes when that changes. A change of a stored switch that the
    /// ceiling hides does not wake it.
    pub fn subscribe(&self) -> watch::Receiver<Switches> {
        self.announce.subscribe()
    }

    /// Sets the switches named, writing the store first and the cache second, in one transaction. A switch that already has the
    /// value is left alone and is not in the answer. This blocks on the database: call it off the async threads.
    pub fn apply(&self, sync: Option<bool>, remote: Option<bool>) -> Result<Changed, StoreError> {
        let _writer = self.writers.lock().unwrap_or_else(PoisonError::into_inner);
        let wanted = [(Feature::Sync, sync), (Feature::Remote, remote)];
        let todo: Vec<(Feature, bool)> = wanted
            .into_iter()
            .filter_map(|(feature, value)| value.filter(|v| self.stored(feature) != *v).map(|v| (feature, v)))
            .collect();
        if todo.is_empty() {
            return Ok(Changed::default());
        }
        self.store.write(|tx| {
            for (feature, value) in &todo {
                tx.setting_set(feature.key(), *value)?;
            }
            Ok(())
        })?;
        let mut changed = Changed::default();
        for (feature, value) in todo {
            self.stored[feature.index()].store(value, Ordering::SeqCst);
            match feature {
                Feature::Sync => changed.sync = Some(value),
                Feature::Remote => changed.remote = Some(value),
            }
        }
        let now = self.effective();
        self.announce.send_if_modified(|seen| {
            if *seen == now {
                false
            } else {
                *seen = now;
                true
            }
        });
        Ok(changed)
    }

    /// Sets one switch; answers whether it changed.
    pub fn set(&self, feature: Feature, on: bool) -> Result<bool, StoreError> {
        let changed = match feature {
            Feature::Sync => self.apply(Some(on), None)?,
            Feature::Remote => self.apply(None, Some(on))?,
        };
        Ok(!changed.list().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_under_a_prefix_by_whole_segments() {
        for path in ["/api/device/me", "/api/device", "/api/device/", "/api/device/a/b", "/sync/v1/list/-", "/ws/device"] {
            assert!(gate_of(path).is_some(), "{path}");
        }
        for path in ["/api/devices", "/api/devicex/me", "/api", "/", "/sync/v2/list", "/sync", "/ws", "/ws/devices", "/api/me/device"] {
            assert!(gate_of(path).is_none(), "{path}");
        }
    }

    #[test]
    fn a_spelling_that_could_reach_a_gated_path_is_gated() {
        for path in [
            "/api/device/me/",
            "/API/Device/me",
            "//api//device/me",
            "/api/device//me",
            "/api/./device/me",
            "/api/x/../device/me",
            "/api/%64evice/me",
            "/%61pi/device/me",
            "/api%2Fdevice/me",
            "/ws//device",
            "/WS/DEVICE",
            "/sync/V1/blob/meta.json",
        ] {
            assert!(gate_of(path).is_some(), "{path}");
        }
    }

    #[test]
    fn each_prefix_names_its_gate() {
        assert_eq!(gate_of("/api/device/me"), Some(Gate::Any));
        assert_eq!(gate_of("/sync/v1/list/files"), Some(Gate::Feature(Feature::Sync)));
        assert_eq!(gate_of("/ws/device"), Some(Gate::Feature(Feature::Remote)));
    }

    #[test]
    fn a_bad_escape_is_kept_as_it_is() {
        assert_eq!(decode("/a%zz/b%4"), "/a%zz/b%4");
        assert_eq!(decode("/a%41b"), "/aAb");
    }
}
