//! Names from a cluster, an account, a directory or a synced file, checked before they can be an argument.
//!
//! A name that starts with `-` becomes an option of the tool (`--server=...` territory), and a name with a control or a
//! direction character can spoof a card or a terminal. So every kind of name is a type with one constructor, `parse`,
//! and a function that builds an argument takes the type and never a `&str`. A flag that carries a name is built in the
//! equals form by [`flag_eq`], which keeps the name one argument. A refusal is a fixed sentence: it never holds a byte
//! of the rejected input, so a hostile name cannot reach a log, a card or a fault line through its error.

use std::ffi::OsString;
use std::fmt;

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

/// The checks every kind starts with, in this order: not empty, within `max` bytes, no hidden character, no leading dash.
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

/// The longest context name: an EKS context is an ARN and a kubeconfig allows what a file allows.
const CONTEXT_MAX: usize = 253;

/// A context of a kubeconfig or a talosconfig. Any printable text is accepted (colons, slashes, an at sign and an
/// inner space occur in real contexts); a leading dash, a leading or trailing space and every hidden character are not.
#[derive(Clone, PartialEq, Eq)]
pub struct ContextName(String);

impl ContextName {
    /// Checks `value` and keeps it as given.
    pub fn parse(value: &str) -> Result<Self, ArgError> {
        base(value, CONTEXT_MAX)?;
        if value.starts_with(' ') || value.ends_with(' ') {
            return Err(ArgError::Shape);
        }
        Ok(Self(value.to_owned()))
    }

    /// The validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ContextName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContextName(len={})", self.0.len())
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
