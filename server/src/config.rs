//! The strict JSON configuration of the server: every key, its default, and the refusal of unknown keys.
//!
//! One file, named by `HOLZWAVE_SERVER_CONFIG` (default `/etc/holzwave/server.json`). Unknown keys are refused at every
//! level, durations are strings such as `30m`, and secrets are never values in the file, only paths of files. There is no
//! key that serves without a sign-in, no default administrator and no plain-HTTP public listener: such keys do not exist, so
//! a file that names one is refused as unknown.
//!
//! [`prepare`] is the one function that turns the environment and the file into a [`Prepared`] server configuration. The
//! `serve` and `check-config` commands call it before anything else, so a configuration the server must refuse is refused in
//! one place, with one sentence, before a socket exists. File-system questions go through the [`Probe`] trait, so the tests
//! can run every refusal without real certificates.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer};

use crate::keyfile::{self, ServerKey, shown};
use crate::limits::{self, Limits, LimitsConfig};

/// The variable that names the server key file; required.
pub const ENV_KEY_FILE: &str = "HOLZWAVE_SERVER_KEY_FILE";

/// The variable that names the configuration file.
pub const ENV_CONFIG: &str = "HOLZWAVE_SERVER_CONFIG";

/// The variable that replaces `data_dir` of the file.
pub const ENV_DATA_DIR: &str = "HOLZWAVE_DATA_DIR";

/// Where the configuration file is when `HOLZWAVE_SERVER_CONFIG` is not set.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/holzwave/server.json";

/// The default of `listen`.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:8443";

/// The default of `data_dir`.
pub const DEFAULT_DATA_DIR: &str = "/data";

/// The default of `runtime_dir`.
pub const DEFAULT_RUNTIME_DIR: &str = "/run/holzwave";

/// The default of `tls.directory_url` in the `acme` mode: the production directory of Let's Encrypt.
pub const DEFAULT_ACME_DIRECTORY: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// The default of `workers.helper_path`.
pub const DEFAULT_HELPER_PATH: &str = "/usr/local/bin/holzwave-spawn-helper";

/// The default of `workers.worker_path`: the server binary itself, run with the `worker` subcommand.
pub const DEFAULT_WORKER_PATH: &str = "/usr/local/bin/holzwave-server";

/// The default of `upload.dir`, relative to a person's home.
pub const DEFAULT_UPLOAD_DIR: &str = "uploads";

/// The largest configuration file that is read, in bytes.
pub const MAX_CONFIG_BYTES: usize = 1 << 20;

/// The longest duration a setting can be written as: ten years.
pub const MAX_DURATION_SECS: u64 = 3650 * 86_400;

/// The most entries of `tls.trusted_proxies`.
pub const MAX_TRUSTED_PROXIES: usize = 256;

/// The most names of `tls.domains`: one certificate holds ten names at most.
pub const MAX_ACME_DOMAINS: usize = crate::acme::MAX_DOMAINS;

/// The most entries of `auth.oidc`.
pub const MAX_OIDC_PROVIDERS: usize = 32;

/// The environment as the server reads it: the variables that start with `HOLZWAVE_`.
pub type Env = BTreeMap<String, String>;

// ---------------------------------------------------------------------------------------------------------------------
// The refusal
// ---------------------------------------------------------------------------------------------------------------------

/// A configuration the server refuses to run with: one sentence that says what is wrong and what to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(String);

impl Refusal {
    /// A refusal with this sentence. Control characters are replaced, so a sentence is always one line.
    pub fn new(sentence: impl Into<String>) -> Refusal {
        Refusal(sentence.into().chars().map(|c| if c.is_control() { ' ' } else { c }).collect())
    }

    /// The sentence.
    pub fn sentence(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

impl From<keyfile::PrivateFileError> for Refusal {
    fn from(error: keyfile::PrivateFileError) -> Refusal {
        Refusal::new(error.sentence())
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Durations
// ---------------------------------------------------------------------------------------------------------------------

/// A duration as the configuration spells it. Only this type is read from text; the rest of the server uses [`Duration`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dur(pub Duration);

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Dur, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Dur;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a duration written as a string such as \"90s\", \"30m\", \"24h\" or \"7d\"")
            }
            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Dur, E> {
                parse_duration(text).map(Dur).map_err(E::custom)
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

/// The field reader for a [`Duration`] written as a string.
fn duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    Dur::deserialize(deserializer).map(|d| d.0)
}

/// Reads `90s`, `30m`, `24h` or `7d`: digits and one unit letter, nothing else. A bare number, a sign, a fraction, a second
/// unit and a value above [`MAX_DURATION_SECS`] are errors.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let wrong = || format!("{text:?} is not a duration; write a number and a unit, such as 90s, 30m, 24h or 7d");
    let split = text.len().checked_sub(1).ok_or_else(wrong)?;
    if !text.is_char_boundary(split) {
        return Err(wrong());
    }
    let (digits, unit) = text.split_at(split);
    if digits.is_empty() || digits.len() > 12 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(wrong());
    }
    let unit_secs: u64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(wrong()),
    };
    let count: u64 = digits.parse().map_err(|_| wrong())?;
    let total = count.checked_mul(unit_secs).filter(|t| *t <= MAX_DURATION_SECS);
    total.map(Duration::from_secs).ok_or_else(|| format!("{text:?} is longer than ten years"))
}

/// The largest unit that spells a duration exactly, which [`parse_duration`] reads back: 86400 seconds are `1d`, 5400 are `90m`.
pub fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs != 0 && secs.is_multiple_of(86_400) {
        format!("{}d", secs / 86_400)
    } else if secs != 0 && secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs != 0 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The public origin
// ---------------------------------------------------------------------------------------------------------------------

/// The address people type in the browser, reduced to what the server compares with: the host, the port, and the exact string
/// an `Origin` header must equal. The passkey relying party is the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    host: String,
    port: u16,
    ip: bool,
    text: String,
}

impl Origin {
    /// The scheme; it is always `https`.
    pub fn scheme(&self) -> &'static str {
        "https"
    }

    /// The host in lower case, without brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port; 443 when the address names none.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Whether the host is an IP address and not a name.
    pub fn host_is_ip(&self) -> bool {
        self.ip
    }

    /// The exact value of an `Origin` header for this server, such as `https://hw.example.org:8443`.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The exact value of a `Host` header for this server: the host, with the port unless it is 443.
    pub fn authority(&self) -> &str {
        &self.text["https://".len()..]
    }
}

/// Reads `public_url`: `https`, a host, an optional port, and at most a closing slash. A path, a query, a fragment, user
/// information, a plain `http` address and a host that is not a valid name or address are refused.
pub fn public_origin(url: &str) -> Result<Origin, Refusal> {
    let bad = |why: &str| Refusal::new(format!("public_url {url:?} is not usable: {why}"));
    let rest = match url.get(..8) {
        Some(head) if head.eq_ignore_ascii_case("https://") => &url[8..],
        _ if url.get(..7).is_some_and(|head| head.eq_ignore_ascii_case("http://")) => {
            return Err(bad("it must start with https:// because passkeys and sign-in cookies need TLS"));
        }
        _ => return Err(bad("it must start with https://")),
    };
    if rest.chars().any(|c| c.is_control() || c.is_whitespace() || c == '\\') {
        return Err(bad("it holds a space, a control character or a backslash"));
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if tail.contains('?') {
        return Err(bad("it must not have a query"));
    }
    if tail.contains('#') {
        return Err(bad("it must not have a fragment"));
    }
    if !tail.is_empty() && tail != "/" {
        return Err(bad("it must not have a path"));
    }
    if authority.contains('@') {
        return Err(bad("it must not carry user information"));
    }

    let (host_text, port_text) = if let Some(inner) = authority.strip_prefix('[') {
        let close = inner.find(']').ok_or_else(|| bad("the address in brackets is not closed"))?;
        let after = &inner[close + 1..];
        let port = match after {
            "" => None,
            _ => Some(after.strip_prefix(':').ok_or_else(|| bad("only a port may follow the bracketed address"))?),
        };
        (&inner[..close], port)
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    let bracketed = authority.starts_with('[');

    let port = match port_text {
        None => 443,
        Some(p) if !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()) => {
            match p.parse::<u16>() {
                Ok(n) if n != 0 => n,
                _ => return Err(bad("the port must be between 1 and 65535")),
            }
        }
        Some(_) => return Err(bad("the port must be a number between 1 and 65535")),
    };

    let host = host_text.to_ascii_lowercase();
    let ip = if bracketed {
        Ipv6Addr::from_str(&host).map_err(|_| bad("the address in brackets is not an IPv6 address"))?;
        true
    } else if Ipv4Addr::from_str(&host).is_ok() {
        true
    } else {
        if !valid_dns_name(&host) {
            return Err(bad(
                "the host is not a valid name (use letters, digits and hyphens; write international names in punycode)",
            ));
        }
        false
    };

    let shown_host = if bracketed { format!("[{host}]") } else { host.clone() };
    let text = if port == 443 { format!("https://{shown_host}") } else { format!("https://{shown_host}:{port}") };
    Ok(Origin { host, port, ip, text })
}

/// Whether `host` (already lower case) is a name a certificate can be issued for: dot-separated labels of letters, digits and
/// hyphens, none empty, none longer than 63, none starting or ending with a hyphen, and a last label that is not a number.
pub fn valid_dns_name(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let label_ok = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    let mut labels = host.split('.').peekable();
    let mut last_is_number = false;
    while let Some(label) = labels.next() {
        if !label_ok(label) {
            return false;
        }
        if labels.peek().is_none() {
            last_is_number = label.bytes().all(|b| b.is_ascii_digit());
        }
    }
    !last_is_number
}

// ---------------------------------------------------------------------------------------------------------------------
// Trusted proxies
// ---------------------------------------------------------------------------------------------------------------------

/// An address or a network, such as `10.0.0.0/8` or `fd00::1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Reads `a.b.c.d`, `a.b.c.d/n`, an IPv6 address or `addr/n`. The prefix must fit the family and must not be 0, which
    /// would trust every address. Host bits below the prefix are cleared.
    pub fn parse(text: &str) -> Result<Cidr, String> {
        let wrong = || format!("{text:?} is not an address or a network such as 10.0.0.1 or 10.0.0.0/8");
        let (addr_text, prefix_text) = match text.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (text, None),
        };
        let addr = IpAddr::from_str(addr_text).map_err(|_| wrong())?.to_canonical();
        let width: u8 = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix_text {
            None => width,
            Some(p) if !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()) => {
                p.parse::<u8>().map_err(|_| wrong())?
            }
            Some(_) => return Err(wrong()),
        };
        if prefix > width {
            return Err(format!("{text:?} has a prefix longer than {width} bits"));
        }
        if prefix == 0 {
            return Err(format!("{text:?} would trust every address; name the networks of the proxies"));
        }
        Ok(Cidr { addr: mask(addr, prefix), prefix })
    }

    /// Whether `ip` lies inside the network. An IPv4 address that arrives as an IPv4-mapped IPv6 address counts as IPv4.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        ip.is_ipv4() == self.addr.is_ipv4() && mask(ip, self.prefix) == self.addr
    }
}

/// `ip` with every bit after the first `prefix` bits cleared.
fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let keep = if prefix == 0 { 0 } else { u32::MAX << (32 - u32::from(prefix.min(32))) };
            IpAddr::V4(Ipv4Addr::from(bits & keep))
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let keep = if prefix == 0 { 0 } else { u128::MAX << (128 - u32::from(prefix.min(128))) };
            IpAddr::V6(Ipv6Addr::from(bits & keep))
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------------------------------------------------

/// The configuration file as written. Everything is optional or has a default except what makes the server reachable.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The address people type in the browser. Required.
    pub public_url: Option<String>,
    /// The address and port the server listens on.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Where the server keeps its database and the homes of the people.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// Where the server keeps files that live as long as the process, such as the health socket.
    #[serde(default = "default_runtime_dir")]
    pub runtime_dir: PathBuf,
    /// How the connection is secured. Required.
    pub tls: Option<Tls>,
    /// Who may sign in. At least one method must be on.
    #[serde(default)]
    pub auth: Auth,
    /// How long a session lives.
    #[serde(default)]
    pub sessions: Sessions,
    /// Overrides of the named limits.
    #[serde(default)]
    pub limits: LimitsConfig,
    /// The workers that run a person's terminals.
    #[serde(default)]
    pub workers: Workers,
    /// Uploads.
    #[serde(default)]
    pub upload: Upload,
    /// The WebSocket.
    #[serde(default)]
    pub transport: Transport,
    /// What the operator allows of the device features.
    #[serde(default)]
    pub device_access: DeviceAccess,
    /// The devices that sign in to the server.
    #[serde(default)]
    pub devices: Devices,
    /// What a person may keep on the server as a sync target.
    #[serde(default)]
    pub sync: SyncTarget,
    /// The sessions that live on the server for devices to attach to.
    #[serde(default)]
    pub remote: Remote,
}

fn default_listen() -> String {
    DEFAULT_LISTEN.to_owned()
}

fn default_data_dir() -> PathBuf {
    PathBuf::from(DEFAULT_DATA_DIR)
}

fn default_runtime_dir() -> PathBuf {
    PathBuf::from(DEFAULT_RUNTIME_DIR)
}

/// The `tls` section. The `mode` key chooses one of three shapes.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum Tls {
    /// A certificate and a key from files.
    Files {
        /// The certificate chain in PEM.
        cert_file: Option<PathBuf>,
        /// The private key in PEM.
        key_file: Option<PathBuf>,
    },
    /// A certificate that the server gets and renews through ACME.
    Acme {
        /// The ACME directory; Let's Encrypt when not given.
        directory_url: Option<String>,
        /// The contact for the ACME account, such as `mailto:admin@example.org`.
        contact: Option<String>,
        /// The names the certificate covers.
        #[serde(default)]
        domains: Vec<String>,
        /// An extra trusted root for a private ACME directory.
        ca_file: Option<PathBuf>,
        /// The operator's agreement to the terms of service of the certificate authority. There is no default: without
        /// `true` the server does not start.
        terms_agreed: Option<bool>,
    },
    /// Plain HTTP behind a proxy that ends TLS; only the listed addresses may connect.
    Proxy {
        /// The addresses and networks of the proxies. Required and not empty.
        #[serde(default)]
        trusted_proxies: Vec<String>,
    },
}

/// The `auth` section: every method is off until the file turns it on.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Auth {
    /// Sign in with a passkey.
    pub passkey: bool,
    /// Sign in with a password and a TOTP code.
    pub password_totp: bool,
    /// Sign in through an OpenID Connect provider.
    pub oidc: Vec<Oidc>,
}

/// One OpenID Connect provider.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Oidc {
    /// A short name for the provider in the sign-in route: lower-case letters, digits, `-` and `_`.
    pub id: String,
    /// The name people see on the button.
    pub name: String,
    /// The issuer URL; `https`.
    pub issuer: String,
    /// The client id the provider gave the server.
    pub client_id: String,
    /// The file that holds the client secret; the secret is never a value in this file.
    pub client_secret_file: PathBuf,
    /// If not empty, only addresses at these domains may sign in.
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// If not empty, only members of these groups may sign in.
    #[serde(default)]
    pub allowed_groups: Vec<String>,
    /// The claim that holds the groups; `groups` when not given.
    pub groups_claim: Option<String>,
}

/// The `sessions` section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Sessions {
    /// How long a session may sit unused.
    #[serde(deserialize_with = "duration")]
    pub idle: Duration,
    /// How long a session may live in total.
    #[serde(deserialize_with = "duration")]
    pub absolute: Duration,
    /// How long a fresh sign-in counts for sensitive actions.
    #[serde(deserialize_with = "duration")]
    pub step_up: Duration,
}

impl Default for Sessions {
    fn default() -> Sessions {
        Sessions {
            idle: limits::SESSION_IDLE,
            absolute: limits::SESSION_ABSOLUTE,
            step_up: limits::SESSION_STEP_UP,
        }
    }
}

/// How workers are started.
#[cfg(feature = "in-process-workers")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkersMode {
    /// Through the spawn helper, as a person's own user. The only mode of a release build.
    #[default]
    Helper,
    /// Inside the server's own process, for tests and the development host.
    InProcess,
}

/// The `workers` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Workers {
    /// How workers are started; this key exists only in a build with the `in-process-workers` feature.
    #[cfg(feature = "in-process-workers")]
    pub mode: WorkersMode,
    /// The first user id of the range workers run as.
    pub uid_min: u32,
    /// The last user id of the range workers run as.
    pub uid_max: u32,
    /// How long a worker lives after its person's last socket closed.
    #[serde(deserialize_with = "duration")]
    pub idle_timeout: Duration,
    /// The most panes one worker holds.
    pub max_panes: u32,
    /// The scrollback lines of a pane.
    pub max_scrollback_lines: u32,
    /// The most MiB of pictures one pane may keep, whatever the person's own setting says; 0 turns pictures off.
    pub max_image_mib: u32,
    /// The `RLIMIT_NPROC` of a worker.
    pub nproc: u64,
    /// The `RLIMIT_NOFILE` of a worker.
    pub nofile: u64,
    /// The spawn helper program.
    pub helper_path: PathBuf,
    /// The program a worker runs.
    pub worker_path: PathBuf,
}

impl Default for Workers {
    fn default() -> Workers {
        Workers {
            #[cfg(feature = "in-process-workers")]
            mode: WorkersMode::default(),
            uid_min: limits::WORKER_UID_MIN,
            uid_max: limits::WORKER_UID_MAX,
            idle_timeout: limits::WORKER_IDLE_TIMEOUT,
            max_panes: limits::MAX_PANES,
            max_scrollback_lines: limits::MAX_SCROLLBACK_LINES,
            max_image_mib: limits::MAX_IMAGE_MIB,
            nproc: limits::WORKER_NPROC,
            nofile: limits::WORKER_NOFILE,
            helper_path: PathBuf::from(DEFAULT_HELPER_PATH),
            worker_path: PathBuf::from(DEFAULT_WORKER_PATH),
        }
    }
}

/// The `upload` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Upload {
    /// The directory inside a person's home that receives uploads; relative, without `..`.
    pub dir: PathBuf,
    /// The largest upload in bytes.
    pub max_bytes: u64,
}

impl Default for Upload {
    fn default() -> Upload {
        Upload { dir: PathBuf::from(DEFAULT_UPLOAD_DIR), max_bytes: limits::UPLOAD_BYTES }
    }
}

/// Whether the WebSocket compresses messages. A value that this build cannot honour is refused by [`prepare`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Compression {
    /// No compression. The default: the measurement did not pass its rule.
    #[default]
    Off,
    /// Per-message deflate.
    Deflate,
}

/// The `transport` section.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Transport {
    /// Whether the WebSocket compresses messages.
    pub compression: Compression,
}

/// The `device_access` section: the operator's ceiling over the two switches an administrator can turn on. A key that is not
/// written leaves its feature open; `false` keeps it off for good, whatever the switch says. The section never turns a
/// feature on: both switches are off until an administrator turns them on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DeviceAccess {
    /// Whether the server may be a sync target.
    pub sync: Option<bool>,
    /// Whether sessions may live on the server for devices to attach to.
    pub remote: Option<bool>,
}

impl DeviceAccess {
    /// The ceiling these keys make: a feature is allowed unless its key says `false`.
    pub fn ceiling(self) -> crate::http::features::Ceiling {
        crate::http::features::Ceiling { sync: self.sync.unwrap_or(true), remote: self.remote.unwrap_or(true) }
    }
}

/// The `devices` section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Devices {
    /// How long a device may go unseen before its token ends.
    #[serde(deserialize_with = "duration")]
    pub idle: Duration,
}

impl Default for Devices {
    fn default() -> Devices {
        Devices { idle: limits::DEVICE_IDLE }
    }
}

/// The `sync` section: what one person may keep on the server when it is a sync target. The target itself is off until an
/// administrator turns it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SyncTarget {
    /// The most MiB of encrypted files one person keeps.
    pub quota_mib: u32,
    /// The most files one person keeps, the metadata file and the backups included.
    pub max_files: u32,
    /// How many of the newest backups stay.
    pub backups_keep: u32,
    /// The most bytes one encrypted file may have.
    pub max_blob_bytes: u32,
}

impl Default for SyncTarget {
    fn default() -> SyncTarget {
        SyncTarget {
            quota_mib: limits::SYNC_QUOTA_MIB,
            max_files: limits::SYNC_MAX_FILES,
            backups_keep: limits::SYNC_BACKUPS_KEEP,
            max_blob_bytes: limits::SYNC_MAX_BLOB_BYTES,
        }
    }
}

impl SyncTarget {
    /// The bounds the sync routes hold a person to.
    pub fn bounds(&self) -> crate::sync_target::Bounds {
        let to_usize = |n: u32| usize::try_from(n).unwrap_or(usize::MAX);
        crate::sync_target::Bounds {
            quota_bytes: u64::from(self.quota_mib) * 1024 * 1024,
            max_files: to_usize(self.max_files),
            backups_keep: to_usize(self.backups_keep),
            max_blob_bytes: to_usize(self.max_blob_bytes),
        }
    }
}

/// The `remote` section: what a device may ask of a session that lives on the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Remote {
    /// The rows of scrollback that come with the screen when a device attaches and does not say how many it wants.
    pub attach_tail_lines: u32,
    /// The most rows of scrollback any attach brings; the session's own scrollback is the other limit.
    pub history_max_lines: u32,
    /// The largest snapshot a session sends, in bytes; a larger one is refused.
    pub snapshot_max_bytes: u32,
    /// How long the writer of a session may be silent before the right to write is free.
    #[serde(deserialize_with = "duration")]
    pub writer_timeout: Duration,
}

impl Default for Remote {
    fn default() -> Remote {
        Remote {
            attach_tail_lines: limits::REMOTE_ATTACH_TAIL_LINES,
            history_max_lines: limits::REMOTE_HISTORY_MAX_LINES,
            snapshot_max_bytes: limits::REMOTE_SNAPSHOT_MAX_BYTES,
            writer_timeout: limits::REMOTE_WRITER_TIMEOUT,
        }
    }
}

impl Remote {
    /// The numbers a worker's session host runs with. The writer timeout is bounded by the key's own range, which is far
    /// below what a `u32` of milliseconds holds.
    pub fn session_limits(&self) -> crate::worker::sessions::SessionLimits {
        crate::worker::sessions::SessionLimits {
            attach_tail_lines: self.attach_tail_lines,
            history_max_lines: self.history_max_lines,
            snapshot_max_bytes: self.snapshot_max_bytes,
            writer_timeout_ms: u32::try_from(self.writer_timeout.as_millis()).unwrap_or(u32::MAX),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The file system, as far as `prepare` needs it
// ---------------------------------------------------------------------------------------------------------------------

/// The questions `prepare` asks the system, so that a test can answer them without real files.
pub trait Probe {
    /// The bytes of the configuration file, at most one byte more than [`MAX_CONFIG_BYTES`].
    fn read_config(&self, path: &Path) -> std::io::Result<Vec<u8>>;

    /// Whether `path` is a regular file the server can open for reading.
    fn readable(&self, path: &Path) -> bool;

    /// The user id the server runs as.
    fn own_uid(&self) -> u32;

    /// The server key from the file at `path`; the strict reader of the key module.
    fn load_key(&self, path: &Path) -> Result<ServerKey, Refusal>;
}

/// The real file system.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealProbe;

impl Probe for RealProbe {
    fn read_config(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        let file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn readable(&self, path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|m| m.is_file()) && std::fs::File::open(path).is_ok()
    }

    fn own_uid(&self) -> u32 {
        rustix::process::getuid().as_raw()
    }

    fn load_key(&self, path: &Path) -> Result<ServerKey, Refusal> {
        ServerKey::load(path).map_err(Refusal::from)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// prepare
// ---------------------------------------------------------------------------------------------------------------------

/// How the connection is secured, after the checks.
#[derive(Debug, Clone)]
pub enum TlsSetup {
    /// A certificate and a key from files; both can be read.
    Files {
        /// The certificate chain.
        cert_file: PathBuf,
        /// The private key.
        key_file: PathBuf,
    },
    /// A certificate from ACME.
    Acme {
        /// The ACME directory URL.
        directory_url: String,
        /// The contact of the account.
        contact: String,
        /// The names of the certificate.
        domains: Vec<String>,
        /// An extra trusted root for the directory.
        ca_file: Option<PathBuf>,
    },
    /// Plain HTTP from the listed proxies only.
    Proxy {
        /// The proxies.
        trusted_proxies: Vec<Cidr>,
    },
}

/// A configuration that passed every check, with its key. This is all `serve` needs to start.
#[derive(Debug)]
pub struct Prepared {
    /// The configuration file that was read.
    pub config_path: PathBuf,
    /// The public origin.
    pub origin: Origin,
    /// The address the server listens on.
    pub listen: SocketAddr,
    /// The data directory.
    pub data_dir: PathBuf,
    /// The runtime directory.
    pub runtime_dir: PathBuf,
    /// The TLS setup.
    pub tls: TlsSetup,
    /// The sign-in methods; at least one is on.
    pub auth: Auth,
    /// The session times.
    pub sessions: Sessions,
    /// The limits in force.
    pub limits: Limits,
    /// The workers.
    pub workers: Workers,
    /// The upload settings.
    pub upload: Upload,
    /// The WebSocket settings.
    pub transport: Transport,
    /// What the operator allows of the device features.
    pub device_access: DeviceAccess,
    /// The devices.
    pub devices: Devices,
    /// The remote sessions.
    pub remote: Remote,
    /// What a person may keep on the server as a sync target.
    pub sync: SyncTarget,
    /// The server key.
    pub key: ServerKey,
}

/// Reads the environment and the configuration file and returns the server configuration, or the one sentence that says why
/// the server must not start. The order of the checks is: the key variable, the file, its syntax, the public URL, the listen
/// address and directories, TLS, the sign-in methods, the workers, the other settings, and last the key file.
pub fn prepare(env: &Env, probe: &dyn Probe) -> Result<Prepared, Refusal> {
    let key_path = match env.get(ENV_KEY_FILE).filter(|v| !v.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => {
            return Err(Refusal::new(format!(
                "{ENV_KEY_FILE} is not set; create a key file with `holzwave-server keygen --out FILE` and name it in that variable"
            )));
        }
    };
    let config_path = env
        .get(ENV_CONFIG)
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH), PathBuf::from);

    let mut config = read_config(&config_path, probe)?;
    if let Some(dir) = env.get(ENV_DATA_DIR).filter(|v| !v.is_empty()) {
        config.data_dir = PathBuf::from(dir);
    }

    let origin = match config.public_url.as_deref() {
        None | Some("") => {
            return Err(Refusal::new(
                "public_url is not set; name the address people type in the browser, such as https://holzwave.example.org",
            ));
        }
        Some(url) => public_origin(url)?,
    };
    if config.auth.passkey && origin.host_is_ip() {
        return Err(Refusal::new(format!(
            "public_url {:?} has an IP address as its host, but passkeys need a name; use a host name or turn auth.passkey off",
            origin.as_str()
        )));
    }

    let listen = SocketAddr::from_str(&config.listen).map_err(|_| {
        Refusal::new(format!("listen {:?} is not an address and port such as 0.0.0.0:8443", config.listen))
    })?;
    for (key, dir) in [("data_dir", &config.data_dir), ("runtime_dir", &config.runtime_dir)] {
        if !dir.is_absolute() {
            return Err(Refusal::new(format!("{key} {} must be an absolute path", shown(dir))));
        }
    }

    let tls = check_tls(config.tls.as_ref(), origin.host(), probe)?;
    check_auth(&config.auth, probe)?;
    check_workers(&config.workers, probe.own_uid())?;
    let sessions = check_sessions(&config.sessions)?;
    let limits = Limits::resolve(&config.limits)?;
    let upload = check_upload(&config.upload)?;
    let devices = check_devices(&config.devices)?;
    let remote = check_remote(&config.remote)?;
    let sync = check_sync(&config.sync)?;
    crate::http::transport::check(config.transport.compression).map_err(Refusal::new)?;

    let key = probe.load_key(&key_path)?;

    Ok(Prepared {
        config_path,
        origin,
        listen,
        data_dir: config.data_dir,
        runtime_dir: config.runtime_dir,
        tls,
        auth: config.auth,
        sessions,
        limits,
        workers: config.workers,
        upload,
        transport: config.transport,
        device_access: config.device_access,
        devices,
        remote,
        sync,
        key,
    })
}

/// Reads and parses the configuration file.
fn read_config(path: &Path, probe: &dyn Probe) -> Result<Config, Refusal> {
    let bytes = probe.read_config(path).map_err(|e| {
        Refusal::new(format!(
            "the configuration file {} cannot be read ({}); name the file in {ENV_CONFIG}",
            shown(path),
            e.kind()
        ))
    })?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(Refusal::new(format!(
            "the configuration file {} is larger than {MAX_CONFIG_BYTES} bytes",
            shown(path)
        )));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Refusal::new(format!("the configuration file {} is not UTF-8 text", shown(path))))?;
    serde_json::from_str(text)
        .map_err(|e| Refusal::new(format!("the configuration file {} is not valid: {e}", shown(path))))
}

/// The TLS section: present, one of the three modes, and complete.
fn check_tls(tls: Option<&Tls>, public_host: &str, probe: &dyn Probe) -> Result<TlsSetup, Refusal> {
    let Some(tls) = tls else {
        return Err(Refusal::new(
            "the configuration has no tls section; choose a mode: \"files\", \"acme\" or \"proxy\" (the server has no plain-HTTP mode)",
        ));
    };
    match tls {
        Tls::Files { cert_file, key_file } => {
            let (Some(cert_file), Some(key_file)) = (cert_file, key_file) else {
                return Err(Refusal::new("tls mode \"files\" needs cert_file and key_file"));
            };
            if !probe.readable(cert_file) {
                return Err(Refusal::new(format!(
                    "the certificate file {} cannot be read; check tls.cert_file",
                    shown(cert_file)
                )));
            }
            if !probe.readable(key_file) {
                return Err(Refusal::new(format!(
                    "the certificate key file {} cannot be read; check tls.key_file",
                    shown(key_file)
                )));
            }
            Ok(TlsSetup::Files { cert_file: cert_file.clone(), key_file: key_file.clone() })
        }
        Tls::Acme { directory_url, contact, domains, ca_file, terms_agreed } => {
            let directory = directory_url.as_deref().unwrap_or(DEFAULT_ACME_DIRECTORY);
            crate::acme::check_terms(*terms_agreed, directory)?;
            crate::acme::check_domains(domains, Some(public_host))?;
            let contact = contact.as_deref().map(str::trim).filter(|c| !c.is_empty()).ok_or_else(|| {
                Refusal::new("tls mode \"acme\" needs tls.contact, an address such as mailto:admin@example.org")
            })?;
            crate::acme::normalise_contact(contact)?;
            crate::acme::check_directory(directory)?;
            if let Some(ca) = ca_file
                && !probe.readable(ca)
            {
                return Err(Refusal::new(format!("the ACME root file {} cannot be read; check tls.ca_file", shown(ca))));
            }
            Ok(TlsSetup::Acme {
                directory_url: directory.to_owned(),
                contact: contact.to_owned(),
                domains: domains.clone(),
                ca_file: ca_file.clone(),
            })
        }
        Tls::Proxy { trusted_proxies } => {
            if trusted_proxies.is_empty() {
                return Err(Refusal::new(
                    "tls mode \"proxy\" needs tls.trusted_proxies, the addresses of the proxies allowed to connect; the list is empty",
                ));
            }
            if trusted_proxies.len() > MAX_TRUSTED_PROXIES {
                return Err(Refusal::new(format!("tls.trusted_proxies lists more than {MAX_TRUSTED_PROXIES} entries")));
            }
            let parsed = trusted_proxies
                .iter()
                .map(|entry| Cidr::parse(entry).map_err(|e| Refusal::new(format!("tls.trusted_proxies entry {e}"))))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(TlsSetup::Proxy { trusted_proxies: parsed })
        }
    }
}

/// At least one sign-in method is on, and each OpenID Connect provider is complete.
fn check_auth(auth: &Auth, probe: &dyn Probe) -> Result<(), Refusal> {
    if !auth.passkey && !auth.password_totp && auth.oidc.is_empty() {
        return Err(Refusal::new(
            "no sign-in method is on; turn on auth.passkey or auth.password_totp, or add a provider to auth.oidc (the server never runs without sign-in)",
        ));
    }
    if auth.oidc.len() > MAX_OIDC_PROVIDERS {
        return Err(Refusal::new(format!("auth.oidc lists more than {MAX_OIDC_PROVIDERS} providers")));
    }
    let mut seen = Vec::new();
    for provider in &auth.oidc {
        let id = &provider.id;
        let id_ok = !id.is_empty()
            && id.len() <= 32
            && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if !id_ok {
            return Err(Refusal::new(format!(
                "auth.oidc id {id:?} must be 1 to 32 lower-case letters, digits, hyphens or underscores"
            )));
        }
        if seen.contains(&id) {
            return Err(Refusal::new(format!("auth.oidc id {id:?} is used twice")));
        }
        seen.push(id);
        if provider.name.trim().is_empty() || provider.name.chars().count() > 64 {
            return Err(Refusal::new(format!("auth.oidc {id:?} needs a name of 1 to 64 characters")));
        }
        if !provider.issuer.starts_with("https://") || provider.issuer.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(Refusal::new(format!("auth.oidc {id:?}: issuer must be an https address")));
        }
        if provider.client_id.trim().is_empty() {
            return Err(Refusal::new(format!("auth.oidc {id:?} needs a client_id")));
        }
        if !probe.readable(&provider.client_secret_file) {
            return Err(Refusal::new(format!(
                "auth.oidc {id:?}: the client secret file {} cannot be read; check client_secret_file",
                shown(&provider.client_secret_file)
            )));
        }
    }
    Ok(())
}

/// The user id range and the numeric settings of the workers.
fn check_workers(workers: &Workers, own_uid: u32) -> Result<(), Refusal> {
    if workers.uid_min < limits::WORKER_UID_FLOOR {
        return Err(Refusal::new(format!(
            "workers.uid_min {} is below {}, which could name a system account; use a range of {} or more",
            workers.uid_min,
            limits::WORKER_UID_FLOOR,
            limits::WORKER_UID_FLOOR
        )));
    }
    if workers.uid_min > workers.uid_max {
        return Err(Refusal::new(format!(
            "workers.uid_min {} is above workers.uid_max {}; the range is reversed",
            workers.uid_min, workers.uid_max
        )));
    }
    if workers.uid_max == u32::MAX {
        return Err(Refusal::new("workers.uid_max is the largest user id, which the kernel reserves"));
    }
    if (workers.uid_min..=workers.uid_max).contains(&own_uid) {
        return Err(Refusal::new(format!(
            "the worker range {}..{} contains the user id the server itself runs as ({own_uid}); choose a range without it",
            workers.uid_min, workers.uid_max
        )));
    }
    limits::WORKER_IDLE_SPEC.check(workers.idle_timeout)?;
    limits::MAX_PANES_SPEC.check(workers.max_panes)?;
    limits::SCROLLBACK_SPEC.check(workers.max_scrollback_lines)?;
    limits::IMAGE_SPEC.check(workers.max_image_mib)?;
    limits::NPROC_SPEC.check(workers.nproc)?;
    limits::NOFILE_SPEC.check(workers.nofile)?;
    for (key, path) in [("helper_path", &workers.helper_path), ("worker_path", &workers.worker_path)] {
        if !path.is_absolute() {
            return Err(Refusal::new(format!("workers.{key} {} must be an absolute path", shown(path))));
        }
    }
    Ok(())
}

/// The session times, each inside its bounds and in a sensible order.
fn check_sessions(sessions: &Sessions) -> Result<Sessions, Refusal> {
    limits::SESSION_IDLE_SPEC.check(sessions.idle)?;
    limits::SESSION_ABSOLUTE_SPEC.check(sessions.absolute)?;
    limits::SESSION_STEP_UP_SPEC.check(sessions.step_up)?;
    if sessions.idle > sessions.absolute {
        return Err(Refusal::new(format!(
            "sessions.idle {} is longer than sessions.absolute {}",
            format_duration(sessions.idle),
            format_duration(sessions.absolute)
        )));
    }
    Ok(*sessions)
}

/// The idle time of a device token, inside its bounds.
fn check_devices(devices: &Devices) -> Result<Devices, Refusal> {
    limits::DEVICE_IDLE_SPEC.check(devices.idle)?;
    Ok(*devices)
}

/// The four numbers of the sync target, each inside its bounds.
fn check_sync(sync: &SyncTarget) -> Result<SyncTarget, Refusal> {
    limits::SYNC_QUOTA_SPEC.check(sync.quota_mib)?;
    limits::SYNC_FILES_SPEC.check(sync.max_files)?;
    limits::SYNC_BACKUPS_SPEC.check(sync.backups_keep)?;
    limits::SYNC_BLOB_SPEC.check(sync.max_blob_bytes)?;
    Ok(*sync)
}

/// The four numbers of the remote sessions, each inside its bounds, and a tail that is not larger than the history cap.
fn check_remote(remote: &Remote) -> Result<Remote, Refusal> {
    limits::ATTACH_TAIL_SPEC.check(remote.attach_tail_lines)?;
    limits::HISTORY_MAX_SPEC.check(remote.history_max_lines)?;
    limits::SNAPSHOT_MAX_SPEC.check(remote.snapshot_max_bytes)?;
    limits::WRITER_TIMEOUT_SPEC.check(remote.writer_timeout)?;
    if remote.attach_tail_lines > remote.history_max_lines {
        return Err(Refusal::new(format!(
            "remote.attach_tail_lines {} is larger than remote.history_max_lines {}; the default tail cannot exceed the cap",
            remote.attach_tail_lines, remote.history_max_lines
        )));
    }
    Ok(*remote)
}

/// The upload size, and a directory that stays inside the home.
fn check_upload(upload: &Upload) -> Result<Upload, Refusal> {
    limits::UPLOAD_SPEC.check(upload.max_bytes)?;
    let inside = !upload.dir.as_os_str().is_empty()
        && upload.dir.components().all(|c| matches!(c, Component::Normal(_)));
    if !inside {
        return Err(Refusal::new(format!(
            "upload.dir {} must be a relative path inside the home, without \"..\" or a leading slash",
            shown(&upload.dir)
        )));
    }
    Ok(upload.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_a_number_and_a_unit() {
        assert_eq!(parse_duration("30m"), Ok(Duration::from_secs(1800)));
        assert_eq!(parse_duration("24h"), Ok(Duration::from_secs(86_400)));
        assert_eq!(parse_duration("7d"), Ok(Duration::from_secs(604_800)));
        assert_eq!(parse_duration("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_duration("0s"), Ok(Duration::ZERO));
        assert_eq!(parse_duration("3650d"), Ok(Duration::from_secs(MAX_DURATION_SECS)));
    }

    #[test]
    fn durations_refuse_everything_else() {
        for text in [
            "", "30", "-1m", "+1m", "1x", "m", "1.5h", "1h30m", " 1m", "1m ", "1 m", "1M", "١m", "1é", "é",
            "999999999999d", "3651d", "99999999999999999999s",
        ] {
            assert!(parse_duration(text).is_err(), "{text:?} must be refused");
        }
    }

    #[test]
    fn durations_are_shown_in_the_largest_exact_unit_and_read_back() {
        for (secs, text) in [(0, "0s"), (45, "45s"), (60, "1m"), (90 * 60, "90m"), (3600, "1h"), (86_400, "1d"), (86_401, "86401s")] {
            assert_eq!(format_duration(Duration::from_secs(secs)), text);
            assert_eq!(parse_duration(text), Ok(Duration::from_secs(secs)));
        }
    }

    #[test]
    fn the_public_origin_keeps_what_an_origin_header_holds() {
        let o = public_origin("https://hw.example.test").expect("plain");
        assert_eq!((o.scheme(), o.host(), o.port()), ("https", "hw.example.test", 443));
        assert_eq!((o.as_str(), o.authority(), o.host_is_ip()), ("https://hw.example.test", "hw.example.test", false));

        let o = public_origin("https://hw.example.test:8443/").expect("port and closing slash");
        assert_eq!((o.port(), o.as_str(), o.authority()), (8443, "https://hw.example.test:8443", "hw.example.test:8443"));

        // The default port is not part of an Origin header, and the case of the host is not either.
        assert_eq!(public_origin("HTTPS://HW.Example.TEST:443").expect("case and default port").as_str(), "https://hw.example.test");
        assert_eq!(public_origin("https://localhost").expect("one label").host(), "localhost");

        let v4 = public_origin("https://192.0.2.7:8443").expect("ipv4");
        assert!(v4.host_is_ip() && v4.as_str() == "https://192.0.2.7:8443");
        let v6 = public_origin("https://[2001:db8::1]:8443").expect("ipv6");
        assert!(v6.host_is_ip() && v6.host() == "2001:db8::1" && v6.as_str() == "https://[2001:db8::1]:8443");
        assert_eq!(v6.authority(), "[2001:db8::1]:8443");
    }

    #[test]
    fn the_public_origin_refuses_what_is_not_an_origin() {
        for url in [
            "",
            "hw.example.test",
            "http://hw.example.test",
            "ftp://hw.example.test",
            "https://",
            "https://hw.example.test/path",
            "https://hw.example.test/x/",
            "https://hw.example.test/?q=1",
            "https://hw.example.test?q=1",
            "https://hw.example.test#frag",
            "https://hw.example.test/#frag",
            "https://user@hw.example.test",
            "https://user:pw@hw.example.test",
            "https://hw.example.test:0",
            "https://hw.example.test:65536",
            "https://hw.example.test:",
            "https://hw.example.test:80a",
            "https://hw.example.test:1:2",
            "https://hw example.test",
            "https://hw.example.test\\x",
            "https://-hw.example.test",
            "https://hw-.example.test",
            "https://hw..example.test",
            "https://hw.example.test.",
            "https://hw_x.example.test",
            "https://hé.example.test",
            "https://1.2.3",
            "https://[::1",
            "https://[hw.example.test]",
            "https://[::1]x",
            "https://[::1]:",
        ] {
            assert!(public_origin(url).is_err(), "{url:?} must be refused");
        }
    }

    #[test]
    fn a_network_contains_its_addresses_and_only_those() {
        let net = Cidr::parse("10.0.0.0/8").expect("net");
        assert!(net.contains("10.1.2.3".parse().unwrap()) && net.contains("10.255.255.255".parse().unwrap()));
        assert!(!net.contains("11.0.0.1".parse().unwrap()) && !net.contains("::1".parse().unwrap()));
        assert!(net.contains("::ffff:10.1.2.3".parse().unwrap()), "an IPv4-mapped address is IPv4");

        let one = Cidr::parse("192.0.2.5").expect("single");
        assert!(one.contains("192.0.2.5".parse().unwrap()) && !one.contains("192.0.2.6".parse().unwrap()));
        assert!(Cidr::parse("10.1.2.3/8").expect("host bits").contains("10.9.9.9".parse().unwrap()));

        let v6 = Cidr::parse("fd00::/8").expect("v6");
        assert!(v6.contains("fd12::1".parse().unwrap()) && !v6.contains("fe80::1".parse().unwrap()));
        assert!(Cidr::parse("::1").expect("v6 single").contains("::1".parse().unwrap()));
    }

    #[test]
    fn a_bad_network_is_refused() {
        for text in ["", "10.0.0.0/", "10.0.0.0/33", "::/129", "10.0.0.0/-1", "10.0.0.0/8/8", "10.0.0/8", "proxy.example", "0.0.0.0/0", "::/0", "10.0.0.0/ 8", "10.0.0.0/999"] {
            assert!(Cidr::parse(text).is_err(), "{text:?} must be refused");
        }
    }
}
