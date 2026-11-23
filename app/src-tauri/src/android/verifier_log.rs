//! The warnings of the platform certificate verifier, written to logcat.
//!
//! On Android the verifier asks the Java side whether a certificate is trusted, and when the answer is no it writes the
//! system's own reason through the `log` facade (for example that a certificate does not name a revocation responder). The app
//! installed no backend for that facade, so the reason was lost and a refused certificate left nothing to read. This module
//! is that backend, and only that: it lets the records of the verifier's target through at warn and error, prints each as one
//! plain bounded line on standard error (which reaches logcat the way the set-up lines of `tls.rs` do), and drops everything
//! else, including every other crate's records and everything below warn.
//!
//! The messages are the system's words about a certificate, never a secret of the app: the verifier sees a certificate chain and
//! a host name, not a token, a password or a key. A certificate field can still be echoed in a message, so every control
//! character becomes a space (one record is one line) and the line is cut at [`MAX_LINE`] characters.
//!
//! The line builder is plain functions and is tested on the desktop. The `log::Log` implementation and [`install`] exist on
//! Android only.

/// The longest line, in characters, that is printed.
const MAX_LINE: usize = 512;

/// The log target of the verifier crate. Its records carry the module path, so the nested modules match by the prefix.
const TARGET: &str = "rustls_platform_verifier";

/// The two severities that are printed; a record below warn has none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warn,
}

impl Severity {
    fn word(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warn => "warn",
        }
    }
}

/// Whether a record of `target` is the verifier's: the crate's own target, or a module below it.
fn is_verifier_target(target: &str) -> bool {
    target == TARGET || target.strip_prefix(TARGET).is_some_and(|rest| rest.starts_with("::"))
}

/// The one line for a record, or `None` when the record is not printed (another target, or below warn). The line holds no
/// control character and has at most [`MAX_LINE`] characters; it is cut on a character boundary.
pub fn line(target: &str, severity: Option<Severity>, message: &str) -> Option<String> {
    let severity = severity?;
    if !is_verifier_target(target) {
        return None;
    }
    let head = format!("holzwave: {TARGET} {}: ", severity.word());
    Some(head.chars().chain(message.chars().map(|c| if c.is_control() { ' ' } else { c })).take(MAX_LINE).collect())
}

#[cfg(target_os = "android")]
mod backend {
    use super::{Severity, is_verifier_target, line};
    use log::{Level, LevelFilter, Log, Metadata, Record};

    struct VerifierLogger;

    fn severity_of(level: Level) -> Option<Severity> {
        match level {
            Level::Error => Some(Severity::Error),
            Level::Warn => Some(Severity::Warn),
            _ => None,
        }
    }

    impl Log for VerifierLogger {
        fn enabled(&self, metadata: &Metadata) -> bool {
            severity_of(metadata.level()).is_some() && is_verifier_target(metadata.target())
        }

        fn log(&self, record: &Record) {
            if let Some(text) = line(record.target(), severity_of(record.level()), &record.args().to_string()) {
                eprintln!("{text}");
            }
        }

        fn flush(&self) {}
    }

    static LOGGER: VerifierLogger = VerifierLogger;

    /// Installs the backend once. A second call, or a logger some other code installed first, changes nothing.
    pub fn install() {
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(LevelFilter::Warn);
        }
    }
}

#[cfg(target_os = "android")]
pub use backend::install;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_warnings_of_the_verifier_become_one_bounded_plain_line() {
        for target in ["rustls_platform_verifier", "rustls_platform_verifier::verification::android"] {
            assert_eq!(
                line(target, Some(Severity::Warn), "certificate was revoked: x").as_deref(),
                Some("holzwave: rustls_platform_verifier warn: certificate was revoked: x")
            );
            assert_eq!(
                line(target, Some(Severity::Error), "failed").as_deref(),
                Some("holzwave: rustls_platform_verifier error: failed")
            );
            assert_eq!(line(target, None, "debug detail"), None, "a record below warn is not printed");
        }
        for target in ["ureq", "rustls", "rustls_platform_verifier_android", "holzwave_core", ""] {
            assert_eq!(line(target, Some(Severity::Error), "x"), None, "{target:?} is not the verifier's target");
        }
    }

    #[test]
    fn a_line_holds_no_control_character_and_is_cut_on_a_character_boundary() {
        let text = line("rustls_platform_verifier", Some(Severity::Warn), "a\nb\tc\u{1b}[31md\r\0e").unwrap();
        assert_eq!(text, "holzwave: rustls_platform_verifier warn: a b c [31md  e");
        assert!(!text.chars().any(char::is_control));

        let long = "é".repeat(2000);
        let text = line("rustls_platform_verifier", Some(Severity::Warn), &long).unwrap();
        assert_eq!(text.chars().count(), MAX_LINE);
        assert!(text.starts_with("holzwave: rustls_platform_verifier warn: éé"));
        assert!(text.ends_with('é'));
    }
}
