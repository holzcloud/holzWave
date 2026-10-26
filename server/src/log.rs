//! The log of the server: one line per event on standard error, with a fixed event name and a short list of fields.
//!
//! A line is `<RFC 3339 UTC time> <level> <name> key=value ...`. The event name and the field names are `&'static str`, so
//! they are written in the source and cannot be built from request data. A value is cut at [`MAX_VALUE_BYTES`] and has its
//! control characters escaped, so a value cannot add a line or hide a field. No function here takes a secret: a caller
//! passes only the facts it means to publish, and the key, a password or a token never goes through this file.

use std::io::Write;
use std::time::SystemTime;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// The longest value of a field in bytes; a longer value is cut and ends in `...`.
pub const MAX_VALUE_BYTES: usize = 200;

/// The most fields one line carries; further fields are dropped.
pub const MAX_FIELDS: usize = 12;

/// How important an event is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Something that happened as designed.
    Info,
    /// Something that was refused or degraded, and the server went on.
    Warn,
    /// Something that failed.
    Error,
}

impl Level {
    /// The word on the line.
    fn word(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// Writes one event to standard error. The line goes out in a single write, so lines of different threads do not mix.
pub fn event(level: Level, name: &'static str, fields: &[(&'static str, &str)]) {
    let mut text = line(SystemTime::now(), level, name, fields);
    text.push('\n');
    // A closed standard error is not worth failing for.
    let _ = std::io::stderr().lock().write_all(text.as_bytes());
}

/// The line for an event at `at`, without the newline.
pub fn line(at: SystemTime, level: Level, name: &'static str, fields: &[(&'static str, &str)]) -> String {
    let mut out = String::with_capacity(96);
    out.push_str(&timestamp(at));
    out.push(' ');
    out.push_str(level.word());
    out.push(' ');
    out.push_str(name);
    for (key, value) in fields.iter().take(MAX_FIELDS) {
        out.push(' ');
        out.push_str(key);
        out.push('=');
        push_value(&mut out, value);
    }
    out
}

/// RFC 3339 in UTC with whole seconds, such as `2024-02-29T12:00:00Z`.
fn timestamp(at: SystemTime) -> String {
    OffsetDateTime::from(at)
        .replace_nanosecond(0)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        // Only a year beyond 9999 cannot be written; a line still goes out.
        .unwrap_or_else(|| "0000-00-00T00:00:00Z".to_owned())
}

/// Appends a value: control characters, backslash and quote escaped, cut at [`MAX_VALUE_BYTES`], and in quotes when it is
/// empty or holds a space or an equals sign.
fn push_value(out: &mut String, value: &str) {
    let mut text = String::with_capacity(value.len().min(MAX_VALUE_BYTES) + 8);
    let mut cut = false;
    for c in value.chars() {
        let piece = match c {
            '\\' => "\\\\".to_owned(),
            '"' => "\\\"".to_owned(),
            '\n' => "\\n".to_owned(),
            '\r' => "\\r".to_owned(),
            '\t' => "\\t".to_owned(),
            c if c.is_control() => format!("\\u{{{:x}}}", u32::from(c)),
            c => c.to_string(),
        };
        if text.len() + piece.len() > MAX_VALUE_BYTES {
            cut = true;
            break;
        }
        text.push_str(&piece);
    }
    if cut {
        text.push_str("...");
    }
    if text.is_empty() || text.contains([' ', '=']) {
        out.push('"');
        out.push_str(&text);
        out.push('"');
    } else {
        out.push_str(&text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn the_time_stamp_is_rfc_3339_in_utc_with_whole_seconds() {
        assert_eq!(timestamp(at(0)), "1970-01-01T00:00:00Z");
        // A leap day, and a leap day in a century year that is a leap year.
        assert_eq!(timestamp(at(1_709_208_000)), "2024-02-29T12:00:00Z");
        assert_eq!(timestamp(at(951_782_400)), "2000-02-29T00:00:00Z");
        // The last second of a year and the first of the next.
        assert_eq!(timestamp(at(1_735_689_599)), "2024-12-31T23:59:59Z");
        assert_eq!(timestamp(at(1_735_689_600)), "2025-01-01T00:00:00Z");
        // A fraction of a second is dropped, not rounded.
        assert_eq!(timestamp(at(1_735_689_599) + Duration::from_millis(999)), "2024-12-31T23:59:59Z");
    }

    #[test]
    fn a_line_has_the_time_the_level_the_name_and_the_fields_in_order() {
        let text = line(at(0), Level::Warn, "listen.refused", &[("addr", "10.0.0.1"), ("count", "3")]);
        assert_eq!(text, "1970-01-01T00:00:00Z warn listen.refused addr=10.0.0.1 count=3");
        assert_eq!(line(at(0), Level::Info, "start", &[]), "1970-01-01T00:00:00Z info start");
    }

    #[test]
    fn a_value_cannot_add_a_line_or_a_field() {
        let text = line(at(0), Level::Info, "x", &[("v", "a\nb\r\tc\u{7f}"), ("w", "p q"), ("e", "")]);
        assert_eq!(text, "1970-01-01T00:00:00Z info x v=a\\nb\\r\\tc\\u{7f} w=\"p q\" e=\"\"");
        assert!(!text.contains('\n'));
        let quoted = line(at(0), Level::Info, "x", &[("v", "say \"hi\" \\")]);
        assert!(quoted.ends_with("v=\"say \\\"hi\\\" \\\\\""), "{quoted}");
    }

    #[test]
    fn a_value_is_cut_at_the_bound_and_a_line_at_the_field_limit() {
        let long = "a".repeat(MAX_VALUE_BYTES * 3);
        let text = line(at(0), Level::Info, "x", &[("v", &long)]);
        let value = text.rsplit("v=").next().expect("the field");
        assert_eq!(value, format!("{}...", "a".repeat(MAX_VALUE_BYTES)));
        // A multi-byte character is never split.
        let wide = "é".repeat(MAX_VALUE_BYTES);
        let text = line(at(0), Level::Info, "x", &[("v", &wide)]);
        let value = text.rsplit("v=").next().expect("the field");
        assert!(value.len() <= MAX_VALUE_BYTES + 3 && value.ends_with("..."));
        let many: Vec<(&'static str, &str)> = (0..MAX_FIELDS * 2).map(|_| ("k", "v")).collect();
        let text = line(at(0), Level::Info, "x", &many);
        assert_eq!(text.matches("k=v").count(), MAX_FIELDS);
    }
}
