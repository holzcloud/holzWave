//! The named limits of the server: every size, count and time that bounds what a client or a worker may do.
//!
//! Each limit has a constant with its default, a documented meaning and a lower and an upper bound. The `limits` section of
//! the configuration may override the ones listed in [`Limits`]; the sections for the sessions, the workers and the upload
//! hold the rest, and their bounds are the [`Spec`] constants below. `prepare` in the configuration module refuses a value
//! outside its bounds, so no setting can turn a limit off by being huge or zero.

use std::time::Duration;

use serde::Deserialize;

use crate::config::{Dur, Refusal, format_duration};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

const fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

const fn minutes(n: u64) -> Duration {
    Duration::from_secs(n * 60)
}

const fn hours(n: u64) -> Duration {
    Duration::from_secs(n * 3600)
}

const fn days(n: u64) -> Duration {
    Duration::from_secs(n * 86_400)
}

// ---------------------------------------------------------------------------------------------------------------------
// Settings outside the `limits` section: sessions, workers, upload
// ---------------------------------------------------------------------------------------------------------------------

/// How long a session may sit unused before it ends.
pub const SESSION_IDLE: Duration = minutes(30);

/// How long a session may live in total, however busy it is.
pub const SESSION_ABSOLUTE: Duration = hours(24);

/// How long after a fresh sign-in a sensitive action is allowed without asking again.
pub const SESSION_STEP_UP: Duration = minutes(5);

/// The first user id a worker may run as; ids below 1000 belong to the system.
pub const WORKER_UID_MIN: u32 = 20_000;

/// The last user id a worker may run as.
pub const WORKER_UID_MAX: u32 = 29_999;

/// The lowest first user id of the worker range that the configuration accepts.
pub const WORKER_UID_FLOOR: u32 = 1000;

/// How long a person's worker lives after the last socket of that person closed.
pub const WORKER_IDLE_TIMEOUT: Duration = hours(24);

/// The most panes one person's worker holds at once.
pub const MAX_PANES: u32 = 64;

/// The scrollback lines of a pane in a worker.
pub const MAX_SCROLLBACK_LINES: u32 = 10_000;

/// The `RLIMIT_NPROC` of a worker: the most processes one person may have.
pub const WORKER_NPROC: u64 = 1024;

/// The `RLIMIT_NOFILE` of a worker: the most open files one person may have.
pub const WORKER_NOFILE: u64 = 4096;

/// The largest file a person may upload, in bytes.
pub const UPLOAD_BYTES: u64 = 256 * MIB;

// ---------------------------------------------------------------------------------------------------------------------
// The supervisor: sockets, queues and waits. These are fixed by the design and are not configuration keys.
// ---------------------------------------------------------------------------------------------------------------------

/// The frames the supervisor may have queued for one worker (commands, not counting the few control frames).
pub const WORKER_QUEUE_FRAMES: usize = 1024;

/// The bytes, length prefixes included, the supervisor may have queued for one worker.
pub const WORKER_QUEUE_BYTES: usize = 32 * 1024 * 1024;

/// How many control frames (attach, detach, shutdown, key controls) may queue beyond [`WORKER_QUEUE_FRAMES`] for a
/// worker that has stopped reading.
pub const WORKER_QUEUE_CONTROL_RESERVE: usize = 256;

/// The most sizes the supervisor remembers for panes that another socket owns, over all panes of one person.
pub const REMEMBERED_SIZES: usize = 512;

/// The most panes the supervisor remembers an owner of, for one person. The oldest entry is forgotten first, and a pane
/// without an owner is claimed by the next socket that resizes it.
pub const OWNED_PANES: usize = 2048;

/// The most control frames that wait for the answer of a worker at once.
pub const PENDING_CONTROLS: usize = 64;

/// The most people the supervisor keeps a record of at once.
pub const ACCOUNT_SLOTS: usize = 65_536;

/// How long a new worker may take to say it is ready; after it the worker is killed and the attach fails.
pub const WORKER_READY_TIMEOUT: Duration = secs(10);

/// How long a worker may take to close its link after it was asked to shut down; after it everything of the person is
/// killed.
pub const WORKER_SHUTDOWN_TIMEOUT: Duration = secs(10);

/// How long the answer to a control frame (an unlock, a lock, a slot change) may take.
pub const WORKER_CONTROL_TIMEOUT: Duration = secs(10);

/// How long a worker that wipes a store or a home may take before it counts as failed.
pub const WORKER_WIPE_TIMEOUT: Duration = secs(60);

/// How long one write of a frame to a worker may take before the worker counts as stuck and its link is closed.
pub const WORKER_WRITE_TIMEOUT: Duration = secs(30);

/// How long the spawn helper may take to answer one request.
pub const HELPER_REQUEST_TIMEOUT: Duration = secs(10);

/// How long a newly started spawn helper may take to answer its first request.
pub const HELPER_START_TIMEOUT: Duration = secs(10);

/// The most requests to the spawn helper that wait for an answer at once.
pub const HELPER_PENDING_REQUESTS: usize = 256;

/// How often the idle rule looks at the workers. The rule itself reads the injected clock.
pub const SWEEP_INTERVAL: Duration = secs(10);

/// The first request number the supervisor makes up itself (for the size of a pane that changes hands). The page's own
/// numbers stay below it, and a command that carries a number at or above it is refused.
pub const SYNTHETIC_REQUEST_BASE: u64 = 1 << 60;

/// The longest event name a worker may send, in bytes.
pub const EVENT_NAME_BYTES: usize = 32;

/// The longest command name a page may send, in bytes.
pub const COMMAND_NAME_BYTES: usize = 64;

/// A setting with its default and its lower and upper bound, both included.
#[derive(Debug, Clone, Copy)]
pub struct Spec<T> {
    /// The configuration key, such as `workers.nproc`.
    pub key: &'static str,
    /// The value used when the configuration does not name one.
    pub default: T,
    /// The smallest accepted value.
    pub min: T,
    /// The largest accepted value.
    pub max: T,
}

/// The idle time of a session.
pub const SESSION_IDLE_SPEC: Spec<Duration> = Spec::new("sessions.idle", SESSION_IDLE, minutes(1), days(30));
/// The total life of a session.
pub const SESSION_ABSOLUTE_SPEC: Spec<Duration> = Spec::new("sessions.absolute", SESSION_ABSOLUTE, minutes(5), days(90));
/// The window of a fresh sign-in.
pub const SESSION_STEP_UP_SPEC: Spec<Duration> = Spec::new("sessions.step_up", SESSION_STEP_UP, secs(30), hours(1));
/// The idle time of a worker.
pub const WORKER_IDLE_SPEC: Spec<Duration> = Spec::new("workers.idle_timeout", WORKER_IDLE_TIMEOUT, minutes(1), days(90));
/// The pane cap.
pub const MAX_PANES_SPEC: Spec<u32> = Spec::new("workers.max_panes", MAX_PANES, 1, 1024);
/// The scrollback cap.
pub const SCROLLBACK_SPEC: Spec<u32> = Spec::new("workers.max_scrollback_lines", MAX_SCROLLBACK_LINES, 100, 1_000_000);
/// The process cap of a worker.
pub const NPROC_SPEC: Spec<u64> = Spec::new("workers.nproc", WORKER_NPROC, 8, 4_194_304);
/// The open-file cap of a worker.
pub const NOFILE_SPEC: Spec<u64> = Spec::new("workers.nofile", WORKER_NOFILE, 16, 1_048_576);
/// The upload cap.
pub const UPLOAD_SPEC: Spec<u64> = Spec::new("upload.max_bytes", UPLOAD_BYTES, KIB, 16 * GIB);

impl<T: Copy> Spec<T> {
    /// A setting.
    pub const fn new(key: &'static str, default: T, min: T, max: T) -> Spec<T> {
        Spec { key, default, min, max }
    }
}

impl<T: Knob> Spec<T> {
    /// `value` when it lies inside the bounds, else the refusal.
    pub fn check(&self, value: T) -> Result<T, Refusal> {
        check(self.key, value, self.min, self.max)
    }

    /// The row of this setting for a table of defaults.
    pub fn row(&self) -> Row {
        Row { key: self.key, default: self.default.show(), min: self.min.show(), max: self.max.show() }
    }
}

/// One line of the table of defaults and bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The configuration key.
    pub key: &'static str,
    /// The default, as written in a configuration.
    pub default: String,
    /// The smallest accepted value.
    pub min: String,
    /// The largest accepted value.
    pub max: String,
}

// ---------------------------------------------------------------------------------------------------------------------
// The shared check
// ---------------------------------------------------------------------------------------------------------------------

/// A kind of value a limit can hold.
pub trait Knob: Copy + PartialOrd {
    /// How the value is written in the configuration file.
    type Cfg: for<'de> Deserialize<'de> + Copy + std::fmt::Debug;

    /// The value from its configuration form.
    fn from_cfg(cfg: Self::Cfg) -> Self;

    /// The value as a person reads it.
    fn show(self) -> String;
}

impl Knob for u32 {
    type Cfg = u32;
    fn from_cfg(cfg: u32) -> u32 {
        cfg
    }
    fn show(self) -> String {
        self.to_string()
    }
}

impl Knob for u64 {
    type Cfg = u64;
    fn from_cfg(cfg: u64) -> u64 {
        cfg
    }
    fn show(self) -> String {
        self.to_string()
    }
}

impl Knob for Duration {
    type Cfg = Dur;
    fn from_cfg(cfg: Dur) -> Duration {
        cfg.0
    }
    fn show(self) -> String {
        format_duration(self)
    }
}

/// `value` when `min <= value <= max`, else the refusal that names `key` and both bounds.
pub fn check<T: Knob>(key: &str, value: T, min: T, max: T) -> Result<T, Refusal> {
    if value < min || value > max {
        return Err(Refusal::new(format!(
            "{key} is {} but must be between {} and {}",
            value.show(),
            min.show(),
            max.show()
        )));
    }
    Ok(value)
}

// ---------------------------------------------------------------------------------------------------------------------
// The `limits` section
// ---------------------------------------------------------------------------------------------------------------------

/// Defines the constants, the configuration section that overrides them, the resolved struct and the table of rows from one
/// list, so that a default, its bounds and its documentation exist in exactly one place.
macro_rules! limits {
    ($( $(#[$doc:meta])* $field:ident, $konst:ident: $ty:ty = $default:expr, $min:expr, $max:expr; )*) => {
        $( $(#[$doc])* pub const $konst: $ty = $default; )*

        /// The `limits` section of the configuration: any limit may be overridden, inside its bounds.
        #[derive(Debug, Clone, Copy, Default, Deserialize)]
        #[serde(deny_unknown_fields, default)]
        pub struct LimitsConfig {
            $( $(#[$doc])* pub $field: Option<<$ty as Knob>::Cfg>, )*
        }

        /// The limits in force: the defaults with the overrides of the configuration applied.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Limits {
            $( $(#[$doc])* pub $field: $ty, )*
        }

        impl Limits {
            /// The defaults.
            pub const DEFAULT: Limits = Limits { $( $field: $konst, )* };

            /// The defaults with `config` applied; a value outside its bounds, or a pair that contradicts each other, is refused.
            pub fn resolve(config: &LimitsConfig) -> Result<Limits, Refusal> {
                let limits = Limits {
                    $( $field: check(
                        concat!("limits.", stringify!($field)),
                        config.$field.map(<$ty as Knob>::from_cfg).unwrap_or($konst),
                        $min,
                        $max,
                    )?, )*
                };
                limits.check_pairs()?;
                Ok(limits)
            }

            /// One row per limit: key, default and bounds.
            pub fn rows() -> Vec<Row> {
                vec![ $( Row {
                    key: concat!("limits.", stringify!($field)),
                    default: <$ty as Knob>::show($konst),
                    min: <$ty as Knob>::show($min),
                    max: <$ty as Knob>::show($max),
                }, )* ]
            }
        }
    };
}

limits! {
    /// How long a connection may take to finish its TLS handshake.
    handshake_timeout, HANDSHAKE_TIMEOUT: Duration = secs(10), secs(1), secs(60);
    /// How long a client may take to send the header of a request.
    header_timeout, HEADER_TIMEOUT: Duration = secs(10), secs(1), secs(60);
    /// The most connections open at once, from everywhere.
    max_connections, MAX_CONNECTIONS: u32 = 1024, 1, 65_536;
    /// The most connections open at once from one address.
    max_connections_per_address, MAX_CONNECTIONS_PER_ADDRESS: u32 = 64, 1, 65_536;
    /// The largest JSON body of an API route, in bytes.
    api_body_bytes, API_BODY_BYTES: u64 = 64 * KIB, KIB, MIB;
    /// The largest message the socket accepts from the page, in bytes: the largest command cap.
    ws_message_bytes, WS_MESSAGE_BYTES: u64 = 8 * MIB, 64 * KIB, 64 * MIB;
    /// The largest message of every command except the paste family, in bytes.
    ws_command_bytes, WS_COMMAND_BYTES: u64 = 64 * KIB, 4 * KIB, 8 * MIB;
    /// The largest message of the small commands (key, mouse, wheel, resize), in bytes.
    ws_small_command_bytes, WS_SMALL_COMMAND_BYTES: u64 = 4 * KIB, 256, 64 * KIB;
    /// The bytes waiting to be sent to one socket before the socket is closed.
    ws_queue_bytes, WS_QUEUE_BYTES: u64 = 4 * MIB, 64 * KIB, 256 * MIB;
    /// The most sockets one person may have open at once.
    sockets_per_person, SOCKETS_PER_PERSON: u32 = 8, 1, 64;
    /// The commands per second one socket may send over a longer time.
    commands_per_second, COMMANDS_PER_SECOND: u32 = 400, 1, 100_000;
    /// The commands one socket may send in a burst before the rate applies.
    command_burst, COMMAND_BURST: u32 = 800, 1, 1_000_000;
    /// How often the server pings an open socket.
    ping_interval, PING_INTERVAL: Duration = secs(25), secs(1), minutes(10);
    /// How often the server checks that the session behind an open socket is still valid.
    session_recheck, SESSION_RECHECK: Duration = secs(30), secs(1), minutes(10);
    /// The most Argon2 verifications that run at once, which bounds the memory a flood of sign-ins can take.
    argon2_concurrency, ARGON2_CONCURRENCY: u32 = 2, 1, 64;
    /// The wrong passwords per account within the window before the account is locked.
    password_attempts_per_account, PASSWORD_ATTEMPTS_PER_ACCOUNT: u32 = 5, 1, 1000;
    /// The wrong passwords per address within the window before the address is locked.
    password_attempts_per_address, PASSWORD_ATTEMPTS_PER_ADDRESS: u32 = 20, 1, 10_000;
    /// The window in which wrong passwords are counted.
    attempt_window, ATTEMPT_WINDOW: Duration = minutes(15), minutes(1), hours(24);
    /// How long an account or an address stays locked.
    lock_duration, LOCK_DURATION: Duration = minutes(15), minutes(1), hours(24);
    /// How long a half-finished sign-in (the password was right, the second step is still to come) stays valid.
    pending_signin, PENDING_SIGNIN: Duration = minutes(5), secs(30), hours(1);
}

impl Limits {
    /// The pairs of limits that must not contradict each other.
    fn check_pairs(&self) -> Result<(), Refusal> {
        let ordered = |small: (&str, u64), large: (&str, u64)| {
            if small.1 > large.1 {
                Err(Refusal::new(format!("{} must not be larger than {}", small.0, large.0)))
            } else {
                Ok(())
            }
        };
        ordered(
            ("limits.max_connections_per_address", u64::from(self.max_connections_per_address)),
            ("limits.max_connections", u64::from(self.max_connections)),
        )?;
        ordered(("limits.ws_command_bytes", self.ws_command_bytes), ("limits.ws_message_bytes", self.ws_message_bytes))?;
        ordered(
            ("limits.ws_small_command_bytes", self.ws_small_command_bytes),
            ("limits.ws_command_bytes", self.ws_command_bytes),
        )?;
        ordered(
            ("limits.commands_per_second", u64::from(self.commands_per_second)),
            ("limits.command_burst", u64::from(self.command_burst)),
        )?;
        Ok(())
    }
}

/// Every setting that has a default and bounds: the `limits` section and the settings of the other sections.
pub fn all_rows() -> Vec<Row> {
    let mut rows = Limits::rows();
    rows.extend([
        SESSION_IDLE_SPEC.row(),
        SESSION_ABSOLUTE_SPEC.row(),
        SESSION_STEP_UP_SPEC.row(),
        WORKER_IDLE_SPEC.row(),
        MAX_PANES_SPEC.row(),
        SCROLLBACK_SPEC.row(),
        NPROC_SPEC.row(),
        NOFILE_SPEC.row(),
        UPLOAD_SPEC.row(),
    ]);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_values_of_the_contract() {
        let d = Limits::DEFAULT;
        assert_eq!(d.handshake_timeout, secs(10));
        assert_eq!(d.header_timeout, secs(10));
        assert_eq!((d.max_connections, d.max_connections_per_address), (1024, 64));
        assert_eq!(d.api_body_bytes, 64 * 1024);
        assert_eq!((d.ws_message_bytes, d.ws_command_bytes, d.ws_small_command_bytes), (8 << 20, 64 << 10, 4 << 10));
        assert_eq!(d.ws_queue_bytes, 4 << 20);
        assert_eq!(d.sockets_per_person, 8);
        assert_eq!((d.commands_per_second, d.command_burst), (400, 800));
        assert_eq!((d.ping_interval, d.session_recheck), (secs(25), secs(30)));
        assert_eq!(d.argon2_concurrency, 2);
        assert_eq!((d.password_attempts_per_account, d.password_attempts_per_address), (5, 20));
        assert_eq!((d.attempt_window, d.lock_duration, d.pending_signin), (minutes(15), minutes(15), minutes(5)));
        assert_eq!((MAX_PANES, WORKER_IDLE_TIMEOUT, UPLOAD_BYTES), (64, hours(24), 256 << 20));
    }

    #[test]
    fn every_default_lies_inside_its_bounds_and_the_defaults_resolve() {
        assert_eq!(Limits::resolve(&LimitsConfig::default()).expect("the defaults resolve"), Limits::DEFAULT);
        for spec in [
            SESSION_IDLE_SPEC,
            SESSION_ABSOLUTE_SPEC,
            SESSION_STEP_UP_SPEC,
            WORKER_IDLE_SPEC,
        ] {
            spec.check(spec.default).unwrap_or_else(|e| panic!("{e}"));
        }
        MAX_PANES_SPEC.check(MAX_PANES).expect("max_panes");
        SCROLLBACK_SPEC.check(MAX_SCROLLBACK_LINES).expect("scrollback");
        NPROC_SPEC.check(WORKER_NPROC).expect("nproc");
        NOFILE_SPEC.check(WORKER_NOFILE).expect("nofile");
        UPLOAD_SPEC.check(UPLOAD_BYTES).expect("upload");
        for row in all_rows() {
            assert!(!row.default.is_empty() && !row.min.is_empty() && !row.max.is_empty(), "{row:?}");
        }
    }

    #[test]
    fn a_value_outside_its_bounds_is_refused_at_both_ends() {
        let config: LimitsConfig = serde_json::from_str(r#"{"max_connections": 0}"#).expect("parses");
        let error = Limits::resolve(&config).expect_err("zero connections");
        assert_eq!(error.sentence(), "limits.max_connections is 0 but must be between 1 and 65536");
        let config: LimitsConfig = serde_json::from_str(r#"{"max_connections": 65537}"#).expect("parses");
        assert!(Limits::resolve(&config).is_err());
        let config: LimitsConfig = serde_json::from_str(r#"{"handshake_timeout": "0s"}"#).expect("parses");
        let error = Limits::resolve(&config).expect_err("zero seconds");
        assert!(error.sentence().contains("limits.handshake_timeout is 0s but must be between 1s and 1m"), "{error}");
        let config: LimitsConfig = serde_json::from_str(r#"{"lock_duration": "7d"}"#).expect("parses");
        assert!(Limits::resolve(&config).is_err());
    }

    #[test]
    fn an_override_inside_the_bounds_replaces_the_default_and_a_contradiction_is_refused() {
        let config: LimitsConfig =
            serde_json::from_str(r#"{"max_connections": 100, "max_connections_per_address": 10, "ping_interval": "1m"}"#)
                .expect("parses");
        let limits = Limits::resolve(&config).expect("resolves");
        assert_eq!((limits.max_connections, limits.max_connections_per_address), (100, 10));
        assert_eq!(limits.ping_interval, minutes(1));
        assert_eq!(limits.sockets_per_person, SOCKETS_PER_PERSON, "the rest keeps its default");

        for (json, word) in [
            (r#"{"max_connections": 10, "max_connections_per_address": 20}"#, "max_connections_per_address"),
            (r#"{"ws_message_bytes": 65536, "ws_command_bytes": 131072}"#, "ws_command_bytes"),
            (r#"{"ws_command_bytes": 4096, "ws_small_command_bytes": 8192}"#, "ws_small_command_bytes"),
            (r#"{"commands_per_second": 500, "command_burst": 100}"#, "commands_per_second"),
        ] {
            let config: LimitsConfig = serde_json::from_str(json).expect("parses");
            let error = Limits::resolve(&config).expect_err(json);
            assert!(error.sentence().contains(word) && error.sentence().contains("must not be larger"), "{error}");
        }
    }

    #[test]
    fn an_unknown_limit_and_a_wrong_type_do_not_parse() {
        assert!(serde_json::from_str::<LimitsConfig>(r#"{"unlimited": true}"#).is_err());
        assert!(serde_json::from_str::<LimitsConfig>(r#"{"max_connections": "many"}"#).is_err());
        assert!(serde_json::from_str::<LimitsConfig>(r#"{"max_connections": -1}"#).is_err());
        assert!(serde_json::from_str::<LimitsConfig>(r#"{"handshake_timeout": 10}"#).is_err());
    }
}
