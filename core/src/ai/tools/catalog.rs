//! The tools the agent can call: their names, their arguments with bounds, and the exact payload a decision covers.
//!
//! An argument struct is closed (`deny_unknown_fields`) and its bounds live in one table of [`Field`] rows. The same
//! table checks the arguments, so a value the model sends cannot be longer, larger or stranger than the table says.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::layout::PaneId;

use super::super::provider::Detail;
use super::super::types::Size;
use super::deny::Access;
use super::{MAX_PATH_BYTES, MAX_RESULT_BYTES, Resolved, ToolError};

/// The tools, by the name a model calls them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolName {
    /// Reads a slice of a file.
    ReadFile,
    /// Lists a directory.
    ListDir,
    /// Searches files for a literal text.
    Grep,
    /// Reads the text of a terminal pane.
    ReadPane,
    /// Writes a whole file.
    WriteFile,
    /// Replaces text inside a file.
    EditFile,
    /// Runs a command in a separate process.
    RunCommand,
}

impl ToolName {
    /// Every tool, in the order they are declared to a model.
    pub const ALL: [ToolName; 7] = [
        ToolName::ReadFile,
        ToolName::ListDir,
        ToolName::Grep,
        ToolName::ReadPane,
        ToolName::WriteFile,
        ToolName::EditFile,
        ToolName::RunCommand,
    ];

    /// The name a model calls the tool by. It is stable: saved conversations keep it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::ListDir => "list_dir",
            Self::Grep => "grep",
            Self::ReadPane => "read_pane",
            Self::WriteFile => "write_file",
            Self::EditFile => "edit_file",
            Self::RunCommand => "run_command",
        }
    }

    /// The tool called `name`, if there is one.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.as_str() == name)
    }

    /// What the tool does to the world.
    pub fn effect(self) -> Effect {
        match self {
            Self::ReadFile | Self::ListDir | Self::Grep | Self::ReadPane => Effect::Read,
            Self::WriteFile | Self::EditFile => Effect::Write,
            Self::RunCommand => Effect::Exec,
        }
    }

    /// Whether what the tool returns is untrusted content that taints the turn.
    pub fn taints_turn(self) -> bool {
        !matches!(self, Self::WriteFile | Self::EditFile)
    }
}

/// What a tool does to the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Reads and changes nothing.
    Read,
    /// Changes a file.
    Write,
    /// Runs a program.
    Exec,
}

impl Effect {
    /// Whether the tool changes something, which is what the taint rule protects.
    pub fn is_side_effect(self) -> bool {
        !matches!(self, Self::Read)
    }
}

/// How a text field treats line breaks. NUL, other control characters, DEL and C1 controls are refused in all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Newlines {
    /// No line break, carriage return or tab: the field is one line.
    Reject,
    /// Line breaks are allowed and the card shows them as `\n`.
    Escape,
    /// Line breaks are allowed and the card shows them as they are.
    Keep,
}

/// The kind of value a field holds, with its bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// Text, bounded in bytes.
    Text {
        /// The fewest bytes.
        min_bytes: usize,
        /// The most bytes.
        max_bytes: usize,
        /// The line break rule.
        newlines: Newlines,
    },
    /// A whole number in a range.
    Int {
        /// The smallest value.
        min: u64,
        /// The largest value.
        max: u64,
        /// The value used when the call leaves the field out.
        default: Option<u64>,
    },
    /// A flag.
    Flag {
        /// The value used when the call leaves the field out.
        default: bool,
    },
}

/// One argument of a tool.
#[derive(Clone, Copy, Debug)]
pub struct Field {
    /// The name in the arguments.
    pub name: &'static str,
    /// The kind of value and its bounds.
    pub kind: FieldKind,
    /// Whether the call has to give it.
    pub required: bool,
    /// What the model is told about it.
    pub doc: &'static str,
}

const MAX_PANE: u64 = u32::MAX as u64;
const MAX_OFFSET: u64 = 1 << 50;
const DEFAULT_OFFSET: u64 = 0;
const DEFAULT_LIMIT: u64 = MAX_RESULT_BYTES as u64;

const PANE: Field = Field {
    name: "pane",
    kind: FieldKind::Int { min: 0, max: MAX_PANE, default: None },
    required: false,
    doc: "The pane to act for. Leave it out to use the pane of this conversation.",
};

const READ_FILE_FIELDS: &[Field] = &[
    Field {
        name: "path",
        kind: FieldKind::Text { min_bytes: 1, max_bytes: MAX_PATH_BYTES, newlines: Newlines::Reject },
        required: true,
        doc: "The absolute path of the file to read.",
    },
    Field {
        name: "offset",
        kind: FieldKind::Int { min: 0, max: MAX_OFFSET, default: Some(DEFAULT_OFFSET) },
        required: false,
        doc: "The byte to start at.",
    },
    Field {
        name: "limit",
        kind: FieldKind::Int { min: 1, max: DEFAULT_LIMIT, default: Some(DEFAULT_LIMIT) },
        required: false,
        doc: "The most bytes to return.",
    },
    PANE,
];

/// Checks `args` against `fields`: the arguments are an object, every key is a field, every required field is there,
/// and every value has the right type, size and shape.
///
/// A failure names the field and the rule in a fixed phrase and never the value.
pub fn check_fields(fields: &[Field], args: &Value) -> Result<(), ToolError> {
    let bad = |text: String| ToolError::InvalidArguments(Detail::new(&text));
    let Some(object) = args.as_object() else {
        return Err(bad("the arguments must be a JSON object".to_owned()));
    };
    for key in object.keys() {
        if !fields.iter().any(|field| field.name == key) {
            return Err(bad(format!("unknown argument `{key}`")));
        }
    }
    for field in fields {
        let name = field.name;
        let value = match object.get(name) {
            None | Some(Value::Null) if field.required => return Err(bad(format!("`{name}` is required"))),
            None | Some(Value::Null) => continue,
            Some(value) => value,
        };
        match field.kind {
            FieldKind::Text { min_bytes, max_bytes, newlines } => {
                let Some(text) = value.as_str() else {
                    return Err(bad(format!("`{name}` must be text")));
                };
                if text.len() > max_bytes {
                    return Err(bad(format!("`{name}` is longer than {max_bytes} bytes")));
                }
                if text.len() < min_bytes {
                    return Err(bad(format!("`{name}` is shorter than {min_bytes} bytes")));
                }
                for c in text.chars() {
                    if matches!(c, '\n' | '\r' | '\t') {
                        if newlines == Newlines::Reject {
                            return Err(bad(format!("`{name}` must be on one line")));
                        }
                    } else if c.is_control() {
                        return Err(bad(format!("`{name}` contains a control character")));
                    }
                }
            }
            FieldKind::Int { min, max, .. } => {
                let Some(number) = value.as_u64() else {
                    return Err(bad(format!("`{name}` must be a whole number of zero or more")));
                };
                if number < min || number > max {
                    return Err(bad(format!("`{name}` must be between {min} and {max}")));
                }
            }
            FieldKind::Flag { .. } => {
                if !value.is_boolean() {
                    return Err(bad(format!("`{name}` must be true or false")));
                }
            }
        }
    }
    Ok(())
}

/// Decodes `args` into `T` after the table accepted them. The value is read in place, without a copy.
fn decode<'a, T: Deserialize<'a>>(fields: &[Field], args: &'a Value) -> Result<T, ToolError> {
    check_fields(fields, args)?;
    T::deserialize(args).map_err(|_| ToolError::InvalidArguments(Detail::new("the arguments do not fit the tool")))
}

/// Prints a value for `Debug` without its text: sizes for text, the value itself for numbers and flags.
trait Describe {
    fn describe(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result;
}

impl Describe for String {
    fn describe(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&Size(self.len()), f)
    }
}

impl Describe for u64 {
    fn describe(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl Describe for bool {
    fn describe(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl<T: Describe> Describe for Option<T> {
    fn describe(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            None => f.write_str("None"),
            Some(value) => {
                f.write_str("Some(")?;
                value.describe(f)?;
                f.write_str(")")
            }
        }
    }
}

struct Shown<'a>(&'a dyn Describe);

impl fmt::Debug for Shown<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.describe(f)
    }
}

/// Writes the `Debug` of an argument struct: its name, and each field as a size or a number, never text.
macro_rules! sized_debug {
    ($ty:ident: $($field:ident),+) => {
        impl fmt::Debug for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($ty))$(.field(stringify!($field), &Shown(&self.$field)))+.finish()
            }
        }
    };
}

/// The arguments of `read_file`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadFile {
    /// The absolute path of the file.
    pub path: String,
    /// The byte to start at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// The most bytes to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// The pane to act for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PaneId>,
}

sized_debug!(ReadFile: path, offset, limit, pane);

impl ReadFile {
    /// The byte to start at, with the default applied.
    pub fn offset(&self) -> u64 {
        self.offset.unwrap_or(DEFAULT_OFFSET)
    }

    /// The most bytes to return, with the default applied.
    pub fn limit(&self) -> u64 {
        self.limit.unwrap_or(DEFAULT_LIMIT)
    }
}

/// A call whose arguments passed the tables.
#[derive(Clone)]
pub enum Tool {
    /// `read_file`.
    ReadFile(ReadFile),
}

impl Tool {
    /// Decodes the call `name` with `args`. Unknown names, unknown arguments and every value outside its bounds are
    /// refused.
    pub fn parse(name: &str, args: &Value) -> Result<Tool, ToolError> {
        match ToolName::from_name(name) {
            Some(ToolName::ReadFile) => decode(READ_FILE_FIELDS, args).map(Tool::ReadFile),
            _ => Err(ToolError::UnknownTool(Detail::new(name))),
        }
    }

    /// Which tool this is.
    pub fn name(&self) -> ToolName {
        match self {
            Self::ReadFile(_) => ToolName::ReadFile,
        }
    }

    /// What the tool does to the world.
    pub fn effect(&self) -> Effect {
        self.name().effect()
    }

    /// How the tool uses its path: `None` for a tool without one.
    pub fn access(&self) -> Option<Access> {
        match self {
            Self::ReadFile(_) => Some(Access::Read),
        }
    }

    /// The pane the call names.
    pub fn pane(&self) -> Option<PaneId> {
        match self {
            Self::ReadFile(a) => a.pane,
        }
    }

    /// The path the call names, as the model wrote it.
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::ReadFile(a) => Some(&a.path),
        }
    }

    /// The working directory the call names, as the model wrote it.
    pub fn cwd(&self) -> Option<&str> {
        match self {
            Self::ReadFile(_) => None,
        }
    }

    /// The arguments in table order with the defaults applied. The `pane` argument is left out: the pane the call acts
    /// for is in the header of the card.
    pub(crate) fn arguments(&self) -> Vec<Item> {
        match self {
            Self::ReadFile(a) => vec![
                Item::line("path", a.path.clone()),
                Item::line("offset", a.offset().to_string()),
                Item::line("limit", a.limit().to_string()),
            ],
        }
    }
}

impl fmt::Debug for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadFile(a) => f.debug_tuple("ReadFile").field(a).finish(),
        }
    }
}

/// One line of the payload: the card shows it and the hash covers it.
#[derive(Clone)]
pub(crate) struct Item {
    pub name: &'static str,
    pub value: String,
    /// How the card shows line breaks of the value.
    pub newlines: Newlines,
}

impl Item {
    fn line(name: &'static str, value: String) -> Self {
        Self { name, value, newlines: Newlines::Reject }
    }
}

/// The list of fields a decision covers, in the order the card shows them: the tool, the pane, the machine, whether it
/// is remote, the tool's own arguments with defaults applied, then the path and working directory as resolved and the
/// stamp of the file.
pub(crate) fn payload(tool: &Tool, resolved: &Resolved) -> Vec<Item> {
    let mut items = vec![
        Item::line("tool", tool.name().as_str().to_owned()),
        Item::line("pane", resolved.pane().to_string()),
        Item::line("target", resolved.target().to_owned()),
        Item::line("remote", (resolved.fs() == super::deny::FsKind::Remote).to_string()),
    ];
    items.extend(tool.arguments());
    if let Some(path) = resolved.path() {
        items.push(Item::line("resolved_path", path.to_owned()));
    }
    if let Some(cwd) = resolved.cwd() {
        items.push(Item::line("resolved_cwd", cwd.to_owned()));
    }
    if let Some(stamp) = resolved.stamp() {
        items.push(Item::line("stamp", stamp.to_string()));
    }
    items
}

/// The bytes every payload hash starts with: the version of the layout and a NUL byte.
pub const HASH_PREFIX: &[u8] = b"holzwave.tool.v1\0";

/// SHA-256 over [`HASH_PREFIX`] and, for each pair in order, the length of the name as four bytes big-endian, the name,
/// the length of the value as four bytes big-endian and the value. Lengths in front of the parts mean that no two
/// different lists give the same bytes.
pub fn payload_hash(fields: &[(String, String)]) -> [u8; 32] {
    hash_pairs(fields.iter().map(|(name, value)| (name.as_str(), value.as_str())))
}

pub(crate) fn hash_items(items: &[Item]) -> [u8; 32] {
    hash_pairs(items.iter().map(|item| (item.name, item.value.as_str())))
}

fn hash_pairs<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(HASH_PREFIX);
    for (name, value) in pairs {
        for part in [name, value] {
            // A part is never near four gibibytes: the argument tables bound every field.
            let length = u32::try_from(part.len()).unwrap_or(u32::MAX);
            hasher.update(length.to_be_bytes());
            hasher.update(part.as_bytes());
        }
    }
    hasher.finalize().into()
}

/// Lower-case hexadecimal.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    out
}

/// Whether the card has to spell out the character: controls, direction marks, zero-width and tag characters.
fn is_hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

fn escape(text: &str, keep_lines: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' if keep_lines => out.push('\n'),
            '\t' if keep_lines => out.push('\t'),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if is_hidden(c) => out.push_str(&format!("\\u{{{:x}}}", u32::from(c))),
            c => out.push(c),
        }
    }
    out
}

/// `text` as the card shows a one-line value: a backslash is doubled, a line break, carriage return and tab are
/// written `\n`, `\r` and `\t`, and every other control, direction, zero-width, format and tag character is written
/// `\u{hex}`. Two different texts never give the same output.
pub fn visible(text: &str) -> String {
    escape(text, false)
}

/// Like [`visible`], but a line break and a tab stay as they are. A carriage return is still written `\r`.
pub fn visible_multiline(text: &str) -> String {
    escape(text, true)
}
