//! Talos Linux through `talosctl`: the argument lists of its verbs and the parsers of its answers.
//!
//! There is no Talos client in this crate and there will not be one: the supported client is the person's own
//! `talosctl`, so this module holds argument builders and parsers and nothing else. It starts no process, reads no file
//! and speaks no protocol; the generic engine runs the program that the detector found.
//!
//! What the module relies on, and has never seen the real tool print:
//!
//! - `talosctl get <resource> -o json` writes one indented JSON object per resource, back to back, with an extra
//!   `node` member and no enclosing array. [`read_objects`] reads such a stream and every shape it has not seen is
//!   treated as drift: a cell that shows `?`, a note that the output was cut, or the cleaned text of the output.
//! - The resources `members`, `services` and `machinestatus` carry the members that the views show, and `config info
//!   -o json` carries `context`, `nodes`, `endpoints` and, perhaps, `roles`. The role member and the whole JSON form of
//!   `config info` are assumptions taken from the documents.
//! - `config context <name>` writes the current context into the person's talosconfig. That the file is written, and
//!   which file, is also an assumption; the card says only `the talosconfig of this computer`.
//! - `config contexts` prints a plain table (`CURRENT`, `NAME`, `ENDPOINTS`, `NODES`) whose columns are aligned under
//!   the header and whose current row starts with `*`.
//!
//! The fixtures of these shapes are written, not recorded, and the tests say so wherever a check rests on one.
//!
//! The talosconfig reaches `talosctl` through its environment, set by the engine from local state, so no argument
//! list built here names a configuration file, an endpoint or an option that weakens the connection: the only flags
//! are the ones of the closed catalogue below. Talos' own role model is the defence in depth for read-only use: an
//! `os:reader` configuration cannot change a node, so the `info` view says in words what the role of the current
//! context allows, and the documents recommend a reader configuration.

use std::ffi::OsString;

use serde_json::Value;

use super::args::{flag_eq, ArgError, ContextName, NodeName};
use super::view::{self, RowBuilder, TableBuilder, ViewModel};
use super::{
    ArgKind, ArgSpec, CheckedSelection, Integration, IntegrationSpec, Output, ParseError, SelectKind, ToolId, Validated,
    Verb, VerbEffect, VersionProbe,
};
use crate::ai::redact;

/// The most objects of a stream that are read; a stream with more shows the first of them and says so.
pub const MAX_OBJECTS: usize = 2000;

/// The most bytes of a stream that are looked at. The runner caps the output of a tool well above this; a stream that
/// is longer is read up to here and treated as cut.
pub const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

/// The most bytes of the small outputs (`config info`, `config contexts`) that are parsed; a larger one is shown as
/// cleaned text.
const MAX_SMALL_BYTES: usize = 1024 * 1024;

/// The most bytes of an unreadable output that are looked at to be shown as text.
const MAX_FALLBACK_BYTES: usize = 512 * 1024;

/// The most entries of a list that a cell names before it says how many more there are.
const LIST_SHOWN: usize = 8;

/// What a cell shows when the output has no value for it or the value has the wrong type.
const UNKNOWN: &str = "?";

/// The longest version text that is kept.
const VERSION_MAX: usize = 64;

const NODE: ArgSpec = ArgSpec { name: "node", kind: ArgKind::Node, required: false };
const CONTEXT: ArgSpec = ArgSpec { name: "context", kind: ArgKind::Context, required: true };
const NODE_ARGS: &[ArgSpec] = &[NODE];
const CONTEXT_ARGS: &[ArgSpec] = &[CONTEXT];

const fn verb(
    id: &'static str,
    label: &'static str,
    effect: VerbEffect,
    output: Output,
    args: &'static [ArgSpec],
) -> Verb {
    Verb { id, label, effect, output, args, refresh_secs: None, exit_is_state: false }
}

/// The closed catalogue of verbs, in the order they are listed.
static VERBS: [Verb; 8] = [
    verb("contexts", "Contexts", VerbEffect::Read, Output::View, &[]),
    verb("nodes", "Nodes", VerbEffect::Read, Output::View, &[]),
    verb("services", "Services", VerbEffect::Read, Output::View, NODE_ARGS),
    verb("info", "Info", VerbEffect::Read, Output::View, &[]),
    verb("machine_status", "Machine status", VerbEffect::Read, Output::View, NODE_ARGS),
    verb("use_context", "Use context", VerbEffect::Local, Output::Done, CONTEXT_ARGS),
    verb("dmesg", "Kernel log", VerbEffect::Read, Output::Terminal, NODE_ARGS),
    verb("dashboard", "Dashboard", VerbEffect::Exec, Output::Terminal, NODE_ARGS),
];

/// The compiled-in description of `talosctl`.
pub static SPEC: IntegrationSpec = IntegrationSpec {
    id: ToolId::Talos,
    label: "Talos",
    binaries: &["talosctl"],
    windows_binaries: &["talosctl.exe"],
    fixed_dirs: &[
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/snap/bin",
        "/home/linuxbrew/.linuxbrew/bin",
        "$HOME/.local/bin",
        "$HOME/bin",
        "%LOCALAPPDATA%\\Microsoft\\WinGet\\Links",
        "%USERPROFILE%\\bin",
    ],
    version_probe: VersionProbe { argv: &["version", "--client"], parse: parse_version },
    env_set: &[],
    selects: &[SelectKind::Context, SelectKind::Node],
    verbs: &VERBS,
};

/// `talosctl`, as the engine sees it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Talos;

/// The one value of [`Talos`].
pub static TALOS: Talos = Talos;

/// The version in the answer of `talosctl version --client`: the text after `Tag:`. The tag is kept as the tool prints
/// it (`v1.12.0`); an answer with no such line, an empty tag or a tag that holds anything but letters, digits and
/// `.`, `+`, `-`, `_` gives `None`.
pub fn parse_version(output: &str) -> Option<String> {
    let tag = output.lines().find_map(|line| line.trim().strip_prefix("Tag:"))?.trim();
    let shape = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '_');
    (!tag.is_empty() && tag.len() <= VERSION_MAX && tag.chars().all(shape)).then(|| tag.to_owned())
}

// ---------------------------------------------------------------------------------------------------------------------
// The object stream.
// ---------------------------------------------------------------------------------------------------------------------

/// How a stream of objects ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEnd {
    /// Every byte was read and every value was an object.
    Complete,
    /// The stream stopped being a sequence of objects: it was cut off, a value was not JSON or a value was not an
    /// object. What was read before is kept.
    Broken,
    /// There were more than [`MAX_OBJECTS`] objects, or more than [`MAX_STREAM_BYTES`] bytes.
    Capped,
}

/// The objects of a stream, up to the first thing that was not one.
#[derive(Debug, Clone, PartialEq)]
pub struct Objects {
    /// The objects read, at most [`MAX_OBJECTS`].
    pub items: Vec<Value>,
    /// How the stream ended.
    pub end: StreamEnd,
}

/// Reads the output of `talosctl get -o json`: JSON objects one after the other, not an array. It reads at most
/// [`MAX_OBJECTS`] objects and [`MAX_STREAM_BYTES`] bytes and stops at the first value that is not an object, at the
/// first error and at the end, so a cut stream, a value that is not JSON and an endless one all give the objects read
/// so far and never an error.
pub fn read_objects(bytes: &[u8]) -> Objects {
    let (slice, cut) = match bytes.get(..MAX_STREAM_BYTES) {
        Some(head) => (head, true),
        None => (bytes, false),
    };
    let mut items = Vec::new();
    let mut stream = serde_json::Deserializer::from_slice(slice).into_iter::<Value>();
    let end = loop {
        let next = stream.next();
        if items.len() >= MAX_OBJECTS && next.is_some() {
            break StreamEnd::Capped;
        }
        match next {
            None if cut => break StreamEnd::Capped,
            None => break StreamEnd::Complete,
            Some(Ok(value @ Value::Object(_))) => items.push(value),
            Some(_) => break StreamEnd::Broken,
        }
    };
    Objects { items, end }
}

/// The text at `pointer`, when it is a non-empty string.
fn text<'a>(item: &'a Value, pointer: &str) -> Option<&'a str> {
    item.pointer(pointer).and_then(Value::as_str).filter(|text| !text.is_empty())
}

/// The strings of the array at `pointer`; the elements that are not strings are left out. `None` when the member is
/// absent or not an array.
fn strings<'a>(item: &'a Value, pointer: &str) -> Option<Vec<&'a str>> {
    let array = item.pointer(pointer)?.as_array()?;
    Some(array.iter().filter_map(Value::as_str).filter(|text| !text.is_empty()).collect())
}

/// The first entries of a list joined with a comma, and how many more there were. An empty list is `none`.
fn joined(list: &[&str]) -> String {
    if list.is_empty() {
        return String::from("none");
    }
    let shown = list.iter().take(LIST_SHOWN).copied().collect::<Vec<_>>().join(", ");
    match list.len().saturating_sub(LIST_SHOWN) {
        0 => shown,
        more => format!("{shown}, +{more} more"),
    }
}

/// The text of a cell: the value, or `?`.
fn cell(value: Option<&str>) -> &str {
    value.unwrap_or(UNKNOWN)
}

/// The cleaned, bounded text of an output that no parser knows. Secrets are redacted first: the output of a Talos
/// tool may sit next to a configuration, and a key or a certificate must never reach a model, however the shape came
/// out.
fn fallback(title: &str, output: &[u8]) -> ViewModel {
    let seen = output.get(..MAX_FALLBACK_BYTES).unwrap_or(output);
    let cleaned = redact::sanitise(&String::from_utf8_lossy(seen)).text;
    let safe = redact::redact(&cleaned).text;
    view::text_fallback(title, safe.as_bytes())
}

/// The table of a stream: one row per object, a note when the stream was cut off or was longer than is read, and the
/// text of the output when not one object could be read.
fn stream_table(
    title: &str,
    bytes: &[u8],
    columns: &[(&str, &str)],
    row: impl Fn(&Value, usize) -> RowBuilder,
) -> ViewModel {
    let objects = read_objects(bytes);
    if objects.items.is_empty() && objects.end != StreamEnd::Complete {
        return fallback(title, bytes);
    }
    let mut table = TableBuilder::new(title);
    for (id, label) in columns {
        table.column(id, label);
    }
    for (index, item) in objects.items.iter().enumerate() {
        table.push(row(item, index));
    }
    let model = table.finish();
    match objects.end {
        StreamEnd::Complete => model,
        StreamEnd::Broken => model.with_note(&format!(
            "The output was cut off; {} of the expected objects were read.",
            objects.items.len()
        )),
        StreamEnd::Capped => {
            model.with_note(&format!("More than {MAX_OBJECTS} objects were written; the first {MAX_OBJECTS} are shown."))
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The nodes of the cluster.
// ---------------------------------------------------------------------------------------------------------------------

fn nodes_view(bytes: &[u8]) -> ViewModel {
    stream_table(
        "Talos nodes",
        bytes,
        &[("node", "NODE"), ("hostname", "HOSTNAME"), ("type", "TYPE"), ("addresses", "ADDRESSES")],
        |item, index| {
            let hostname = text(item, "/spec/hostname");
            let addresses = strings(item, "/spec/addresses");
            let address_cell = addresses.as_deref().map(joined);
            let node = text(item, "/node");
            let key = hostname.or(node).map_or_else(|| format!("object-{}", index + 1), str::to_owned);
            RowBuilder::new(
                &key,
                &[
                    cell(node),
                    cell(hostname),
                    cell(text(item, "/spec/machineType")),
                    cell(address_cell.as_deref()),
                ],
            )
        },
    )
}

// ---------------------------------------------------------------------------------------------------------------------
// The engine's side.
// ---------------------------------------------------------------------------------------------------------------------

/// The flags that a verb takes from the selection, in the equals form so that a name stays one argument.
fn with_flags(base: &[&str], context: Option<&ContextName>, node: Option<&NodeName>) -> Vec<OsString> {
    let mut out: Vec<OsString> = base.iter().map(OsString::from).collect();
    if let Some(context) = context {
        out.push(flag_eq("context", context.as_str()));
    }
    if let Some(node) = node {
        out.push(flag_eq("nodes", node.as_str()));
    }
    out
}

impl Integration for Talos {
    fn spec(&self) -> &'static IntegrationSpec {
        &SPEC
    }

    fn argv(&self, verb: &Verb, _args: &Validated, selection: &CheckedSelection) -> Result<Vec<OsString>, ArgError> {
        let context = selection.context.as_ref();
        match verb.id {
            "nodes" => Ok(with_flags(&["get", "members", "-o", "json"], context, selection.node.as_ref())),
            _ => Err(ArgError::Unknown),
        }
    }

    fn view(&self, verb: &Verb, stdout: &[u8]) -> Result<ViewModel, ParseError> {
        match verb.id {
            "nodes" => Ok(nodes_view(stdout)),
            _ => Err(ParseError::Unrecognised),
        }
    }
}
