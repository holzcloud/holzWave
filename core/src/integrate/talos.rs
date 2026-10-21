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
//!
//! A name never becomes an option: a node or a context is a validated type, a flag that carries one is built in the
//! equals form, and the context of `use_context` stands behind `--`. A row of the `nodes` table offers its buttons only
//! for a name that passes the node check.

use std::ffi::OsString;

use serde_json::Value;

use super::args::{flag_eq, ArgError, ContextName, NodeName};
use super::view::{self, KvBuilder, RowBuilder, TableBuilder, Tone, ViewModel};
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

/// What the `info` view says of a role set that holds only `os:reader`.
pub const ACCESS_READ_ONLY: &str = "read-only (os:reader)";

/// What the `info` view says of a role set that includes `os:admin`.
pub const ACCESS_FULL: &str = "full access (os:admin)";

/// What the `info` view says of a role set that includes `os:operator` and not `os:admin`.
pub const ACCESS_LIMITED: &str = "limited (os:operator)";

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

/// The non-empty strings of an array; the elements that are not strings are left out. Empty when `value` is absent or
/// is not an array.
fn strings(value: Option<&Value>) -> Vec<&str> {
    let array = value.and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    array.iter().filter_map(Value::as_str).filter(|text| !text.is_empty()).collect()
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

/// The cell of a list member: its strings joined, `none` for an empty array, `?` for a member that is absent, is not
/// an array or holds elements but no string.
fn list_cell(value: Option<&Value>) -> String {
    let names = strings(value);
    match value.and_then(Value::as_array) {
        Some(array) if !array.is_empty() && names.is_empty() => String::from(UNKNOWN),
        Some(_) => joined(&names),
        None => String::from(UNKNOWN),
    }
}

/// The text of a single-valued member, or `?`.
fn text_cell(value: &Value) -> &str {
    value.as_str().filter(|text| !text.is_empty()).unwrap_or(UNKNOWN)
}

fn flag(item: &Value, pointer: &str) -> Option<bool> {
    item.pointer(pointer).and_then(Value::as_bool)
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
            let address_cell = list_cell(item.pointer("/spec/addresses"));
            let node = text(item, "/node");
            let key = hostname.or(node).map_or_else(|| format!("object-{}", index + 1), str::to_owned);
            let mut row = RowBuilder::new(
                &key,
                &[
                    cell(node),
                    cell(hostname),
                    cell(text(item, "/spec/machineType")),
                    &address_cell,
                ],
            );
            // The buttons name the address that the tool reported, or else the host name, and only when the name
            // passes the node check: a hostile name gives a row with no buttons, never a button with a bad argument.
            if let Some(target) = [node, hostname].into_iter().flatten().find(|name| NodeName::parse(name).is_ok()) {
                for id in ["services", "machine_status", "dmesg", "dashboard"] {
                    if let Some(verb) = SPEC.verb(id) {
                        row.action(verb, &[("node", target)]);
                    }
                }
            }
            row
        },
    )
}

// ---------------------------------------------------------------------------------------------------------------------
// Services and machine status.
// ---------------------------------------------------------------------------------------------------------------------

fn services_view(bytes: &[u8]) -> ViewModel {
    stream_table(
        "Talos services",
        bytes,
        &[("node", "NODE"), ("service", "SERVICE"), ("state", "STATE"), ("health", "HEALTH")],
        |item, index| {
            let node = text(item, "/node");
            let service = text(item, "/metadata/id");
            let running = flag(item, "/spec/running");
            let healthy = flag(item, "/spec/healthy");
            // A service that reports no health says so. `unknown` is a member that is believed to exist; where it is
            // absent the member reads as no claim.
            let health_unknown = flag(item, "/spec/unknown") == Some(true);
            let state = match running {
                Some(true) => "running",
                Some(false) => "stopped",
                None => UNKNOWN,
            };
            let health = match (healthy, health_unknown) {
                (Some(false), false) => "unhealthy",
                (Some(true), false) => "healthy",
                _ => "unknown",
            };
            let key = match (node, service) {
                (None, None) => format!("object-{}", index + 1),
                _ => format!("{}/{}", cell(node), cell(service)),
            };
            let mut row = RowBuilder::new(&key, &[cell(node), cell(service), state, health]);
            // The output does not say which service is expected to run, so none is `danger`. A service that says it is
            // unhealthy is a warning even when it is stopped: nothing that reports a fault is dimmed.
            if health == "unhealthy" {
                row.tone(Tone::Warn);
            } else if running == Some(false) {
                row.tone(Tone::Dim);
            }
            row
        },
    )
}

fn machine_status_view(bytes: &[u8]) -> ViewModel {
    stream_table(
        "Talos machine status",
        bytes,
        &[("node", "NODE"), ("stage", "STAGE"), ("ready", "READY")],
        |item, index| {
            let node = text(item, "/node");
            let ready = flag(item, "/spec/status/ready");
            let key = node.map_or_else(|| format!("object-{}", index + 1), str::to_owned);
            let ready_word = match ready {
                Some(true) => "yes",
                Some(false) => "no",
                None => UNKNOWN,
            };
            let mut row = RowBuilder::new(&key, &[cell(node), cell(text(item, "/spec/stage")), ready_word]);
            if ready == Some(false) {
                row.tone(Tone::Warn);
            }
            row
        },
    )
}

// ---------------------------------------------------------------------------------------------------------------------
// The current context and its role.
// ---------------------------------------------------------------------------------------------------------------------

/// The roles of a `roles` member and whether every element of it could be read: an array of strings, or one text of
/// roles separated by commas or spaces. Each role is cleaned like a cell, so a role cannot be spelled with characters
/// that the page would not show.
fn read_roles(roles: &Value) -> (Vec<String>, bool) {
    let clean = |text: &str| view::clean_cell(text);
    match roles {
        Value::Array(items) => {
            let names = items.iter().filter_map(Value::as_str).map(clean).filter(|name| !name.is_empty()).collect();
            (names, items.iter().all(Value::is_string))
        }
        Value::String(text) => {
            let split = |c: char| c == ',' || c.is_whitespace();
            (text.split(split).map(clean).filter(|name| !name.is_empty()).collect(), true)
        }
        _ => (Vec::new(), false),
    }
}

/// What a role set allows, in words and with a tone, or `None` when nothing is claimed. A set that names the admin
/// role is full access whatever else it holds, in any case and with any padding; read-only is only said of a set that
/// was read in full and holds nothing but `os:reader`.
fn access(names: &[String], readable: bool) -> Option<(String, Tone)> {
    let has = |role: &str| names.iter().any(|name| name.trim().eq_ignore_ascii_case(role));
    if has("os:admin") {
        Some((String::from(ACCESS_FULL), Tone::Warn))
    } else if has("os:operator") {
        Some((String::from(ACCESS_LIMITED), Tone::Normal))
    } else if !readable || names.is_empty() {
        None
    } else if names.iter().all(|name| name == "os:reader") {
        Some((String::from(ACCESS_READ_ONLY), Tone::Normal))
    } else {
        Some((names.join(", "), Tone::Normal))
    }
}

fn info_view(bytes: &[u8]) -> ViewModel {
    const TITLE: &str = "Talos context";
    let parsed = if bytes.len() <= MAX_SMALL_BYTES { serde_json::from_slice::<Value>(bytes).ok() } else { None };
    let Some(Value::Object(object)) = parsed else {
        return fallback(TITLE, bytes);
    };
    if !["context", "nodes", "endpoints", "roles"].iter().any(|member| object.contains_key(*member)) {
        return fallback(TITLE, bytes);
    }
    let mut list = KvBuilder::new(TITLE);
    if let Some(context) = object.get("context") {
        list.row("Context", text_cell(context), Tone::Normal);
    }
    for (member, label) in [("nodes", "Nodes"), ("endpoints", "Endpoints")] {
        if let Some(value) = object.get(member) {
            list.row(label, &list_cell(Some(value)), Tone::Normal);
        }
    }
    if let Some(roles) = object.get("roles") {
        let (names, readable) = read_roles(roles);
        let shown: Vec<&str> = names.iter().map(String::as_str).collect();
        let roles_text = match (shown.is_empty(), readable) {
            (true, true) => String::from("none"),
            (true, false) => String::from(UNKNOWN),
            _ => joined(&shown),
        };
        list.row("Roles", &roles_text, Tone::Normal);
        if let Some((words, tone)) = access(&names, readable) {
            list.row("Access", &words, tone);
        }
    }
    list.finish()
}

// ---------------------------------------------------------------------------------------------------------------------
// The contexts of the talosconfig.
// ---------------------------------------------------------------------------------------------------------------------

/// The columns of the plain table of `config contexts`, in order.
const CONTEXT_HEADER: [&str; 4] = ["CURRENT", "NAME", "ENDPOINTS", "NODES"];

/// The words of a header line and the column where each starts. A word ends at two spaces or at the end of the line.
fn header_columns(line: &[char]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < line.len() {
        if line[at] == ' ' {
            at += 1;
            continue;
        }
        let start = at;
        let mut end = at;
        while at < line.len() && !(line[at] == ' ' && line.get(at + 1).is_none_or(|next| *next == ' ')) {
            at += 1;
            end = at;
        }
        out.push((start, line[start..end].iter().collect()));
    }
    out
}

/// The text of one column of a line, trimmed: from `from` to `to`, or to the end of the line.
fn column_text(line: &[char], from: usize, to: Option<usize>) -> String {
    let end = to.map_or(line.len(), |to| to.min(line.len()));
    if from >= end {
        return String::new();
    }
    line[from..end].iter().collect::<String>().trim().to_owned()
}

/// The table of `config contexts`, or `None` when the output is not that table: a header that is not exactly the four
/// words, a row that is not laid out under its columns, a mark in the first column that is not `*`, a row with no name,
/// a tab, or text that is not UTF-8.
fn parse_contexts(bytes: &[u8]) -> Option<ViewModel> {
    if bytes.len() > MAX_SMALL_BYTES {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    if text.contains('\t') {
        return None;
    }
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let header: Vec<char> = lines.next()?.chars().collect();
    let columns = header_columns(&header);
    if columns.len() != CONTEXT_HEADER.len() || columns.iter().zip(CONTEXT_HEADER).any(|((_, word), want)| word != want) {
        return None;
    }
    let starts: Vec<usize> = columns.iter().map(|(start, _)| *start).collect();
    let switch = SPEC.verb("use_context");
    let mut table = TableBuilder::new("Talos contexts");
    for (id, label) in [("current", "CURRENT"), ("name", "NAME"), ("endpoints", "ENDPOINTS"), ("nodes", "NODES")] {
        table.column(id, label);
    }
    for line in lines {
        let line: Vec<char> = line.chars().collect();
        // Every column after the first starts after a space: a row that runs into the next column is not laid out
        // under this header, and guessing where its cells end would put one context's name in another's column.
        if starts.iter().skip(1).any(|&at| line.get(at.wrapping_sub(1)).is_some_and(|c| !c.is_whitespace())) {
            return None;
        }
        let part = |n: usize| column_text(&line, starts[n], starts.get(n + 1).copied());
        let current = match part(0).as_str() {
            "*" => "*",
            "" => "",
            _ => return None,
        };
        let name = part(1);
        if name.is_empty() {
            return None;
        }
        let mut row = RowBuilder::new(&name, &[current, &name, &part(2), &part(3)]);
        if let Some(verb) = switch {
            row.action(verb, &[("context", &name)]);
        }
        table.push(row);
    }
    Some(table.finish())
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

    fn argv(&self, verb: &Verb, args: &Validated, selection: &CheckedSelection) -> Result<Vec<OsString>, ArgError> {
        let context = selection.context.as_ref();
        // The node of the verb's own argument first, the node of the selection second.
        let node = args.node().or(selection.node.as_ref());
        match verb.id {
            "contexts" => Ok(with_flags(&["config", "contexts"], None, None)),
            "info" => Ok(with_flags(&["config", "info", "-o", "json"], context, None)),
            "nodes" => Ok(with_flags(&["get", "members", "-o", "json"], context, node)),
            "services" => Ok(with_flags(&["get", "services", "-o", "json"], context, node)),
            "machine_status" => Ok(with_flags(&["get", "machinestatus", "-o", "json"], context, node)),
            // The context is named after `--`, so it cannot be read as an option, and nothing of the selection is used:
            // the verb changes the person's own current context and no other.
            "use_context" => {
                let context = args.context().ok_or(ArgError::Missing)?;
                Ok(vec!["config".into(), "context".into(), "--".into(), context.as_str().into()])
            }
            "dmesg" => Ok(with_flags(&["dmesg", "--follow"], context, node)),
            "dashboard" => Ok(with_flags(&["dashboard"], context, node)),
            _ => Err(ArgError::Unknown),
        }
    }

    fn view(&self, verb: &Verb, stdout: &[u8]) -> Result<ViewModel, ParseError> {
        match verb.id {
            "contexts" => Ok(parse_contexts(stdout).unwrap_or_else(|| fallback("Talos contexts", stdout))),
            "info" => Ok(info_view(stdout)),
            "nodes" => Ok(nodes_view(stdout)),
            "services" => Ok(services_view(stdout)),
            "machine_status" => Ok(machine_status_view(stdout)),
            _ => Err(ParseError::Unrecognised),
        }
    }
}
