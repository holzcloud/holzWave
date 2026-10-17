//! Names from a cluster, an account, a directory or a synced file, checked before they can be an argument.
//!
//! A name that starts with `-` becomes an option of the tool (`--server=...` territory), and a name with a control or a
//! direction character can spoof a card or a terminal. So every kind of name is a type with one constructor, `parse`,
//! and a function that builds an argument takes the type and never a `&str`. A flag that carries a name is built in the
//! equals form by [`flag_eq`], which keeps the name one argument. A refusal is a fixed sentence: it never holds a byte
//! of the rejected input, so a hostile name cannot reach a log, a card or a fault line through its error.

use std::ffi::OsString;
use std::fmt;
use std::net::IpAddr;

use crate::ai::tools::is_hidden;

/// Why a value was refused. No variant carries text, so no error can hold the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgError {
    /// The value is the empty string.
    Empty,
    /// The value is longer than its kind allows.
    TooLong,
    /// The value starts with `-` and would be read as an option.
    LeadingDash,
    /// The value holds a control, direction, zero-width, format, tag or other invisible character.
    Hidden,
    /// The value is made of the wrong characters or has the wrong form for its kind.
    Shape,
}

impl ArgError {
    /// The fixed sentence of the refusal.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "the value is empty",
            Self::TooLong => "the value is longer than allowed",
            Self::LeadingDash => "the value starts with a dash",
            Self::Hidden => "the value holds a control or invisible character",
            Self::Shape => "the value has the wrong form for its kind",
        }
    }
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for ArgError {}

/// Whether a name may never hold the character: everything the approval card has to spell out (control, direction,
/// zero-width, format and tag characters) and every space-like character other than the plain space, which looks like
/// one and is not.
pub(crate) fn never_in_a_name(c: char) -> bool {
    is_hidden(c) || (c.is_whitespace() && c != ' ')
}

/// The checks every kind starts with, in this order: not empty, within `max` bytes, no hidden character, no leading
/// dash.
fn base(value: &str, max: usize) -> Result<(), ArgError> {
    if value.is_empty() {
        return Err(ArgError::Empty);
    }
    if value.len() > max {
        return Err(ArgError::TooLong);
    }
    if value.chars().any(never_in_a_name) {
        return Err(ArgError::Hidden);
    }
    if value.starts_with('-') {
        return Err(ArgError::LeadingDash);
    }
    Ok(())
}

/// Declares a name kind: a private field, `as_str`, and a `Debug` that prints the length only. The kind has no
/// `Default` and no `From<&str>`; its `impl` block below holds the one constructor, `parse`.
macro_rules! name_kind {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            /// The validated text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "(len={})"), self.0.len())
            }
        }
    };
}

/// The longest context name: an EKS context is an ARN and a kubeconfig allows what a file allows.
const CONTEXT_MAX: usize = 253;
/// The longest DNS-1123 label (a namespace, a container, one part of a pod or node name).
const LABEL_MAX: usize = 63;
/// The longest DNS-1123 subdomain (a pod or a host name).
const SUBDOMAIN_MAX: usize = 253;
/// A GUID in its text form: 8-4-4-4-12 hexadecimal digits.
const GUID_LEN: usize = 36;
const SECTION_MAX: usize = 64;
const PROFILE_MAX: usize = 32;
const ENV_MAX: usize = 64;
/// The most lines a log or a read may ask for.
const LINES_MAX: u16 = 5000;

name_kind! {
    /// A context of a kubeconfig or a talosconfig. Any printable text is accepted (colons, slashes, an at sign and an
    /// inner space occur in real contexts); a leading dash, a leading or trailing space and every hidden character are
    /// not.
    ContextName
}

impl ContextName {
    /// Checks `value` and keeps it as given.
    pub fn parse(value: &str) -> Result<Self, ArgError> {
        base(value, CONTEXT_MAX)?;
        if value.starts_with(' ') || value.ends_with(' ') {
            return Err(ArgError::Shape);
        }
        Ok(Self(value.to_owned()))
    }
}

/// One DNS-1123 label: lower-case letters, digits and `-`, starting and ending with a letter or a digit. Inside a
/// longer name an empty label is a malformed name, so it is a shape error and not an empty value.
fn dns_label(label: &str) -> Result<(), ArgError> {
    let bytes = label.as_bytes();
    let (Some(&first), Some(&last)) = (bytes.first(), bytes.last()) else {
        return Err(ArgError::Shape);
    };
    if bytes.len() > LABEL_MAX {
        return Err(ArgError::TooLong);
    }
    let edge = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    if edge(first) && edge(last) && bytes.iter().all(|&b| edge(b) || b == b'-') {
        Ok(())
    } else {
        Err(ArgError::Shape)
    }
}

/// A DNS-1123 subdomain: labels separated by `.`, at most 253 bytes in all.
fn dns_subdomain(value: &str) -> Result<(), ArgError> {
    base(value, SUBDOMAIN_MAX)?;
    value.split('.').try_for_each(dns_label)
}

name_kind! {
    /// A Kubernetes namespace: one DNS-1123 label of at most 63 bytes.
    Namespace
}

impl Namespace {
    /// Checks `value` and keeps it as given.
    pub fn parse(value: &str) -> Result<Self, ArgError> {
        base(value, LABEL_MAX)?;
        dns_label(value)?;
        Ok(Self(value.to_owned()))
    }
}

name_kind! {
    /// A container of a pod: one DNS-1123 label of at most 63 bytes.
    ContainerName
}

impl ContainerName {
    /// Checks `value` and keeps it as given.
    pub fn parse(value: &str) -> Result<Self, ArgError> {
        base(value, LABEL_MAX)?;
        dns_label(value)?;
        Ok(Self(value.to_owned()))
    }
}

name_kind! {
    /// A pod: a DNS-1123 subdomain of at most 253 bytes.
    PodName
}

impl PodName {
    /// Checks `value` and keeps it as given.
    pub fn parse(value: &str) -> Result<Self, ArgError> {
        dns_subdomain(value)?;
        Ok(Self(value.to_owned()))
    }
}

name_kind! {
    /// A node: an IPv4 or IPv6 address literal (read by the standard library's own parser) or a DNS-1123 subdomain
    /// whose last label is not all digits, so that `1.2.3.4.5` is not taken for a host name.
    NodeName
}

impl NodeName {
    /// Checks `value` and keeps it as given.
    pub fn parse(value: &str) -> Result<Self, ArgError> {
        base(value, SUBDOMAIN_MAX)?;
        if value.parse::<IpAddr>().is_err() {
            dns_subdomain(value)?;
            let last = value.rsplit('.').next().unwrap_or_default();
            if last.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ArgError::Shape);
            }
        }
        Ok(Self(value.to_owned()))
    }
}

name_kind! {
    /// An Azure subscription by its GUID (never by its display name), kept in lower case.
    SubscriptionId
}

impl SubscriptionId {
    /// Checks `value` (8-4-4-4-12 hexadecimal digits, either case) and keeps it in lower case.
    pub fn parse(value: &str) -> Result<Self, ArgError> {
        base(value, GUID_LEN)?;
        let bytes = value.as_bytes();
        let grouped = bytes.len() == GUID_LEN
            && bytes.iter().enumerate().all(|(at, &b)| match at {
                8 | 13 | 18 | 23 => b == b'-',
                _ => b.is_ascii_hexdigit(),
            });
        if !grouped {
            return Err(ArgError::Shape);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

name_kind! {
    /// A section of a GAM configuration file: 1 to 64 of letters, digits, `_`, `.` and `-`, not starting with `.`.
    SectionName
}

impl SectionName {
    /// Checks `value` and keeps it as given.
    pub fn parse(value: &str) -> Result<Self, ArgError> {
        base(value, SECTION_MAX)?;
        let charset = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-');
        if value.starts_with('.') || !value.bytes().all(charset) {
            return Err(ArgError::Shape);
        }
        Ok(Self(value.to_owned()))
    }
}

name_kind! {
    /// The id of an integration profile: 1 to 32 of lower-case letters, digits, `_` and `-`, not starting with `-`.
    ProfileId
}

impl ProfileId {
    /// Checks `value` and keeps it as given.
    pub fn parse(value: &str) -> Result<Self, ArgError> {
        base(value, PROFILE_MAX)?;
        if !value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-')) {
            return Err(ArgError::Shape);
        }
        Ok(Self(value.to_owned()))
    }
}

name_kind! {
    /// The name of an environment variable: `[A-Z_][A-Z0-9_]*`, at most 64 characters.
    EnvName
}

impl EnvName {
    /// Checks `value` and keeps it as given.
    pub fn parse(value: &str) -> Result<Self, ArgError> {
        base(value, ENV_MAX)?;
        let mut bytes = value.bytes();
        let first_ok = bytes.next().is_some_and(|b| b.is_ascii_uppercase() || b == b'_');
        if !first_ok || !bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
            return Err(ArgError::Shape);
        }
        Ok(Self(value.to_owned()))
    }
}

/// A line count: decimal digits only (no sign, no space, no leading zero), 1 to 5000.
pub fn lines(value: &str) -> Result<u16, ArgError> {
    base(value, 4)?;
    if !value.bytes().all(|b| b.is_ascii_digit()) || value.starts_with('0') {
        return Err(ArgError::Shape);
    }
    match value.parse::<u16>() {
        Ok(count) if (1..=LINES_MAX).contains(&count) => Ok(count),
        _ => Err(ArgError::Shape),
    }
}

/// A flag value: exactly `true` or `false`.
pub fn flag_value(value: &str) -> Result<bool, ArgError> {
    base(value, 5)?;
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ArgError::Shape),
    }
}

/// The argument `--<name>=<value>`: one element, so the value cannot become a second argument or an option of its own.
///
/// `value` is only ever the `as_str()` of a validated type; a caller never passes text it did not validate.
pub fn flag_eq(name: &'static str, value: &str) -> OsString {
    let mut out = OsString::with_capacity(name.len() + value.len() + 3);
    out.push("--");
    out.push(name);
    out.push("=");
    out.push(value);
    out
}
