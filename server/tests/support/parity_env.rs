//! The helpers of the parity and behaviour suites of the socket: the golden reader, the structural comparator, the builder of
//! benign and hostile arguments, the by-name sender, the screen rebuilder and the counting program.
//!
//! A suite pulls it in after the environment of the endpoint:
//!
//! ```text
//! #[path = "support/edge.rs"] mod edge;
//! #[path = "support/edge_tls.rs"] mod edge_tls;
//! #[path = "support/ws_env.rs"] mod ws_env;
//! #[path = "support/parity_env.rs"] mod parity_env;
//! ```
//!
//! Nothing here holds a list of commands: the table (`COMMANDS`), the golden file `app/tests/fixtures/protocol.json` and the
//! metadata of a row are read when a test runs. The pure functions have unit tests in this file; they run in every suite
//! that includes it and need no server.
#![allow(dead_code)]
#![cfg(feature = "in-process-workers")]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use holzwave_host::commands::registry::{COMMANDS, Command, Web, lookup};
use holzwave_server::worker::runtime::{Probe, TestHooks};
use serde_json::{Map, Value, json};
use tokio::time::Instant;

use crate::ws_env::{Got, Incoming, WsClient, WsEnv, WsOptions};

// ---------------------------------------------------------------------------------------------------------------------
// The golden file
// ---------------------------------------------------------------------------------------------------------------------

/// The golden file `app/tests/fixtures/protocol.json`, read once.
pub fn golden() -> &'static Value {
    static GOLDEN: OnceLock<Value> = OnceLock::new();
    GOLDEN.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures/protocol.json");
        serde_json::from_slice(&std::fs::read(path).expect("the golden file")).expect("the golden file is JSON")
    })
}

/// The frame fixture `app/tests/fixtures/frames.json`, read once.
pub fn frame_fixture() -> &'static Value {
    static FRAMES: OnceLock<Value> = OnceLock::new();
    FRAMES.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures/frames.json");
        serde_json::from_slice(&std::fs::read(path).expect("the frame fixture")).expect("the frame fixture is JSON")
    })
}

/// The names of the commands in the golden file.
pub fn golden_command_names() -> BTreeSet<String> {
    golden()["commands"].as_object().expect("the golden commands are an object").keys().cloned().collect()
}

/// The argument names of a command in the golden file, in order.
pub fn golden_command_args(name: &str) -> Option<Vec<String>> {
    golden()["commands"][name]
        .as_array()
        .map(|list| list.iter().map(|v| v.as_str().expect("an argument name").to_owned()).collect())
}

/// The names of the table.
pub fn table_names() -> BTreeSet<String> {
    COMMANDS.iter().map(|c| c.name.to_owned()).collect()
}

/// The names of the events in the golden file.
pub fn golden_event_names() -> BTreeSet<String> {
    golden()["events"].as_object().expect("the golden events are an object").keys().cloned().collect()
}

/// A member of the golden file by a dotted path (`answers.ai.ticket`, `view`, `events.title`).
pub fn golden_at(path: &str) -> Option<&'static Value> {
    path.split('.').try_fold(golden(), |value, key| value.get(key))
}

// ---------------------------------------------------------------------------------------------------------------------
// The structural comparison
// ---------------------------------------------------------------------------------------------------------------------

/// What the golden says about the shape of a value. A golden `null` is [`Schema::Any`]; the elements of a golden list are merged,
/// so a member that is absent from some of them is optional and a member that is `null` in some of them takes anything.
#[derive(Debug, Clone, PartialEq)]
enum Schema {
    Any,
    Bool,
    Number,
    Text,
    List(Box<Schema>),
    Tuple(Vec<Schema>),
    Object(std::collections::BTreeMap<String, (Schema, bool)>),
}

impl Schema {
    fn name(&self) -> &'static str {
        match self {
            Schema::Any => "anything",
            Schema::Bool => "a boolean",
            Schema::Number => "a number",
            Schema::Text => "a string",
            Schema::List(_) | Schema::Tuple(_) => "an array",
            Schema::Object(_) => "an object",
        }
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn merge(a: Schema, b: Schema) -> Schema {
    match (a, b) {
        (Schema::Bool, Schema::Bool) => Schema::Bool,
        (Schema::Number, Schema::Number) => Schema::Number,
        (Schema::Text, Schema::Text) => Schema::Text,
        (Schema::List(x), Schema::List(y)) => Schema::List(Box::new(merge(*x, *y))),
        (Schema::Tuple(x), Schema::Tuple(y)) if x.len() == y.len() => Schema::Tuple(x.into_iter().zip(y).map(|(p, q)| merge(p, q)).collect()),
        (Schema::Object(x), Schema::Object(y)) => {
            let mut out = std::collections::BTreeMap::new();
            for (key, (schema, required)) in &x {
                match y.get(key) {
                    Some((other, other_required)) => {
                        out.insert(key.clone(), (merge(schema.clone(), other.clone()), *required && *other_required));
                    }
                    None => {
                        out.insert(key.clone(), (schema.clone(), false));
                    }
                }
            }
            for (key, (schema, _)) in y {
                out.entry(key).or_insert((schema, false));
            }
            Schema::Object(out)
        }
        // A null in one instance, or two shapes that cannot be merged: anything goes.
        _ => Schema::Any,
    }
}

fn schema_of(golden: &Value) -> Schema {
    match golden {
        Value::Null => Schema::Any,
        Value::Bool(_) => Schema::Bool,
        Value::Number(_) => Schema::Number,
        Value::String(_) => Schema::Text,
        Value::Object(map) => Schema::Object(map.iter().map(|(k, v)| (k.clone(), (schema_of(v), true))).collect()),
        Value::Array(items) => {
            let schemas: Vec<Schema> = items.iter().map(schema_of).collect();
            // Elements of different kinds (a number and an object) are a fixed tuple; otherwise the elements are instances of one
            // shape.
            let kinds: BTreeSet<&'static str> = items.iter().filter(|v| !v.is_null()).map(kind).collect();
            if kinds.len() > 1 {
                Schema::Tuple(schemas)
            } else {
                Schema::List(Box::new(schemas.into_iter().reduce(merge).unwrap_or(Schema::Any)))
            }
        }
    }
}

fn check(actual: &Value, schema: &Schema, path: &str) -> Result<(), String> {
    match (actual, schema) {
        (_, Schema::Any) | (Value::Bool(_), Schema::Bool) | (Value::Number(_), Schema::Number) | (Value::String(_), Schema::Text) => Ok(()),
        (Value::Array(items), Schema::List(element)) => {
            items.iter().enumerate().try_for_each(|(i, item)| check(item, element, &format!("{path}[{i}]")))
        }
        (Value::Array(items), Schema::Tuple(parts)) => {
            if items.len() != parts.len() {
                return Err(format!("{path}: {} elements where the golden tuple has {}", items.len(), parts.len()));
            }
            items.iter().zip(parts).enumerate().try_for_each(|(i, (item, part))| check(item, part, &format!("{path}[{i}]")))
        }
        (Value::Object(map), Schema::Object(members)) => {
            let missing: Vec<&String> = members.iter().filter(|(k, (_, required))| *required && !map.contains_key(*k)).map(|(k, _)| k).collect();
            let extra: Vec<&String> = map.keys().filter(|k| !members.contains_key(*k)).collect();
            if !missing.is_empty() || !extra.is_empty() {
                return Err(format!("{path}: missing members {missing:?}, extra members {extra:?}"));
            }
            map.iter().try_for_each(|(key, value)| check(value, &members[key].0, &format!("{path}.{key}")))
        }
        (value, schema) => Err(format!("{path}: {} where the golden has {}", kind(value), schema.name())),
    }
}

/// Compares the structure of `actual` with the golden value: the same member names at every level (a member that some of the
/// golden instances of a list lack is optional), arrays by the shape of their golden elements (an empty golden array accepts any
/// array, and a golden array of elements of different kinds is a tuple compared position by position), scalars by JSON type, a
/// golden `null` accepts anything. The error names the path of the first difference and never holds a value.
pub fn same_shape(actual: &Value, golden: &Value) -> Result<(), String> {
    check(actual, &schema_of(golden), "$")
}

/// Compares `actual` with any of the golden values (one of a golden list of variants); the error of the closest is returned.
pub fn shape_matches_any(actual: &Value, golden: &Value) -> Result<(), String> {
    match golden {
        Value::Array(variants) if !variants.is_empty() => {
            let mut last = String::new();
            for variant in variants {
                match same_shape(actual, variant) {
                    Ok(()) => return Ok(()),
                    Err(e) => last = e,
                }
            }
            Err(last)
        }
        other => same_shape(actual, other),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------------------------------------------------

/// An id or an index that no layout of a new person holds, so that a benign call can never close, move or focus a real thing.
pub const NOWHERE: u64 = 4_000_000;

fn squash(ty: &str) -> String {
    ty.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The name inside `Option<..>` or `Vec<..>`, if `ty` is one.
fn inner<'a>(ty: &'a str, wrapper: &str) -> Option<&'a str> {
    ty.strip_prefix(wrapper)?.strip_prefix('<')?.strip_suffix('>')
}

/// A benign value for an argument type as the table spells it: strings are empty, counters and booleans are zero and false,
/// options are absent, lists and maps are empty, the ids that name a thing are [`NOWHERE`], and the enumerations take a value
/// that does nothing. A type the function does not know gives `null`, which the real decoder refuses with a plain answer.
pub fn benign_value(ty: &str) -> Option<Value> {
    let ty = squash(ty);
    if inner(&ty, "Option").is_some() {
        return None;
    }
    Some(match ty.as_str() {
        "String" => json!(""),
        "bool" => json!(false),
        "u8" | "u16" | "u32" | "u64" | "i32" | "i64" | "usize" => json!(0),
        "f32" | "f64" => json!(0.0),
        "PaneId" | "SplitId" => json!(NOWHERE),
        "Value" => Value::Null,
        "EditActionMsg" => json!("line_start"),
        "Edge" => json!("center"),
        "DropKind" => json!("pane"),
        "KeySlot" => json!("provider"),
        "MouseActionMsg" => json!("motion"),
        "MouseButtonMsg" => json!("none"),
        "BlockKind" => json!("files"),
        "Selection" => json!({}),
        "SelectOpMsg" | "SearchOpMsg" => json!({"kind": "clear"}),
        "CopyOpMsg" => json!({"kind": "exit"}),
        "SecretChange" => json!({"kind": "keep"}),
        "ProfileInputMsg" => golden_at("ops.ai_profile_input").and_then(|v| v.get(0)).cloned().unwrap_or(Value::Null),
        t if t.starts_with("Vec<") => json!([]),
        t if t.starts_with("BTreeMap<") => json!({}),
        _ => Value::Null,
    })
}

/// The index arguments are `usize` in the table: for those a benign value is also [`NOWHERE`], never `0`, because index `0`
/// is a real tab and a real workspace.
fn benign_for(arg: &str, ty: &str) -> Option<Value> {
    let squashed = squash(ty);
    if squashed == "usize" && arg != "limit" {
        return Some(json!(NOWHERE));
    }
    benign_value(ty)
}

/// The benign `args` object of a row, built from the names and types of the table. An optional argument is left out.
pub fn args_for(row: &Command) -> Value {
    let mut args = Map::new();
    for arg in row.args {
        if let Some(value) = benign_for(arg.name, arg.ty) {
            args.insert(arg.name.to_owned(), value);
        }
    }
    Value::Object(args)
}

/// The same object when the table has no types: every argument `null`.
pub fn args_without_types(names: &[&str]) -> Value {
    Value::Object(names.iter().map(|n| ((*n).to_owned(), Value::Null)).collect())
}

/// A string of path-traversal characters.
pub const TRAVERSAL: &str = "../../../../etc/passwd/..\\..\\%2e%2e/%00";

/// A value nested `depth` levels deep.
pub fn nested(depth: usize) -> Value {
    let mut value = json!(1);
    for _ in 0..depth {
        value = Value::Array(vec![value]);
    }
    value
}

/// The hostile replacements of one argument as JSON text: `null`, `true`, `-1`, `1.5`, `1e309`, the empty string, a string
/// with a NUL, path-traversal characters, an array nested 50 deep and an object. They are text because `1e309` is not a number
/// a `Value` can hold. The strings carry `canary` so that a test can search for it.
pub fn hostile_texts(canary: &str) -> Vec<(&'static str, String)> {
    let quote = |s: String| serde_json::to_string(&s).unwrap_or_else(|_| String::from("\"\""));
    vec![
        ("null", String::from("null")),
        ("true", String::from("true")),
        ("minus one", String::from("-1")),
        ("one and a half", String::from("1.5")),
        ("overflow", String::from("1e309")),
        ("empty string", String::from("\"\"")),
        ("nul string", quote(format!("{canary}\u{0}x"))),
        ("traversal", quote(format!("{canary}{TRAVERSAL}"))),
        ("nested 50", nested(50).to_string()),
        ("object", json!({"a": canary}).to_string()),
    ]
}

/// The text of the `args` object of `row` with the benign value everywhere except at argument `at`, which gets `text`
/// (JSON text). An optional argument that is not replaced is left out.
pub fn args_text_with(row: &Command, at: usize, text: &str) -> String {
    let mut parts = Vec::new();
    for (i, arg) in row.args.iter().enumerate() {
        let value = if i == at { Some(text.to_owned()) } else { benign_for(arg.name, arg.ty).map(|v| v.to_string()) };
        if let Some(value) = value {
            parts.push(format!("{}:{value}", serde_json::to_string(arg.name).unwrap_or_default()));
        }
    }
    format!("{{{}}}", parts.join(","))
}

/// The hostile replacements of the whole `args`, as the text to put after `"args":`.
pub fn hostile_whole_args() -> Vec<(&'static str, &'static str)> {
    vec![("null", "null"), ("array", "[]"), ("string", "\"x\""), ("number", "7")]
}

// ---------------------------------------------------------------------------------------------------------------------
// Sending by name
// ---------------------------------------------------------------------------------------------------------------------

/// Why a wait for an answer did not give one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Nothing with the id came in the time.
    Silent,
    /// The socket was closed (with the code, if there was one).
    Closed(Option<u16>),
}

/// A socket with the events it saw kept for the test. Every message that is not the wanted answer is kept in `events`.
pub struct Line {
    pub client: WsClient,
    /// Every event seen so far, as the whole message `{"event":..,"payload":..}`, in the order read.
    pub events: Vec<Value>,
    /// Every answer that was not the one waited for (late or unexpected).
    pub strays: Vec<Value>,
    next: u64,
    /// The close code, once seen.
    pub closed: Option<Option<u16>>,
}

/// How long a walk waits for one answer.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(10);

impl Line {
    pub fn new(client: WsClient) -> Line {
        Line { client, events: Vec::new(), strays: Vec::new(), next: 1, closed: None }
    }

    /// Sends the command `name` with `args` and returns the request number. The name must be in the table: the sender
    /// refuses any other string, so that the suite cannot reach the server with a name by accident.
    pub async fn send(&mut self, name: &str, args: &Value) -> u64 {
        assert!(lookup(name).is_some(), "refusing to send the name {name:?}: it is not in the command table");
        self.send_unchecked(name, args).await
    }

    /// Sends a registered name with `args` as text, exactly as given (it may be anything, even text that is not JSON).
    pub async fn send_args_text(&mut self, name: &str, args: &str) -> u64 {
        assert!(lookup(name).is_some(), "refusing to send the name {name:?}: it is not in the command table");
        let id = self.next;
        self.next += 1;
        let name = serde_json::to_string(name).unwrap_or_default();
        self.client.send_text(&format!("{{\"id\":{id},\"cmd\":{name},\"args\":{args}}}")).await;
        id
    }

    /// Sends any name. Only the tests of the unregistered names use it, and they say so.
    pub async fn send_unchecked(&mut self, name: &str, args: &Value) -> u64 {
        let id = self.next;
        self.next += 1;
        let text = json!({"id": id, "cmd": name, "args": args}).to_string();
        self.client.send_text(&text).await;
        id
    }

    /// Sends a text as it is, numbered by the caller.
    pub async fn send_text(&mut self, text: &str) {
        self.client.send_text(text).await;
    }

    /// The request number the next send will use.
    pub fn next_id(&self) -> u64 {
        self.next
    }

    fn keep(&mut self, value: Value) {
        if value.get("event").is_some() {
            self.events.push(value);
        } else {
            self.strays.push(value);
        }
    }

    /// Waits for the answer with `id`; every event read meanwhile is kept.
    pub async fn wait(&mut self, id: u64, within: Duration) -> Result<Value, Failure> {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.client.read(left).await {
                Got::Message(Incoming::Text(text)) => {
                    let value: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"));
                    if value.get("id").and_then(Value::as_u64) == Some(id) && value.get("event").is_none() {
                        return Ok(value);
                    }
                    self.keep(value);
                }
                Got::Message(Incoming::Binary(_) | Incoming::Ping(_) | Incoming::Pong(_)) => {}
                Got::Message(Incoming::Close(code, _)) => {
                    self.closed = Some(code);
                    return Err(Failure::Closed(code));
                }
                Got::Ended => {
                    self.closed = Some(None);
                    return Err(Failure::Closed(None));
                }
                Got::Quiet => return Err(Failure::Silent),
            }
        }
    }

    /// Sends a command by name and waits for its answer.
    pub async fn call(&mut self, name: &str, args: &Value) -> Result<Value, Failure> {
        let id = self.send(name, args).await;
        self.wait(id, ANSWER_WITHIN).await
    }

    /// Sends a command whose answer is raw bytes and returns them (the eight bytes of the request id taken off), or the
    /// text answer when the command was refused (`{"id":..,"err":..}`).
    pub async fn raw_call(&mut self, name: &str, args: &Value) -> Result<Vec<u8>, Value> {
        let id = self.send(name, args).await;
        let deadline = Instant::now() + ANSWER_WITHIN;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.client.read(left).await {
                Got::Message(Incoming::Binary(bytes)) if bytes.len() >= 8 && bytes[..8] == id.to_be_bytes() => return Ok(bytes[8..].to_vec()),
                Got::Message(Incoming::Text(text)) => {
                    let value: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"));
                    if value.get("id").and_then(Value::as_u64) == Some(id) && value.get("event").is_none() {
                        return Err(value);
                    }
                    self.keep(value);
                }
                Got::Message(Incoming::Close(..)) | Got::Ended | Got::Quiet => panic!("no answer to {name}"),
                Got::Message(_) => {}
            }
        }
    }

    /// Sends a command that must be answered `ok` and returns the result.
    pub async fn ok(&mut self, name: &str, args: &Value) -> Value {
        match self.call(name, args).await {
            Ok(answer) => {
                assert!(answer.get("ok").is_some(), "{name} was refused: {answer}");
                answer["ok"].clone()
            }
            Err(failure) => panic!("{name} got no answer: {failure:?}"),
        }
    }

    /// Reads for `span` and keeps everything (events and strays).
    pub async fn pump(&mut self, span: Duration) {
        let deadline = Instant::now() + span;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.client.read(left).await {
                Got::Message(Incoming::Text(text)) => {
                    let value: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"));
                    self.keep(value);
                }
                Got::Message(Incoming::Close(code, _)) => {
                    self.closed = Some(code);
                    return;
                }
                Got::Message(_) => {}
                Got::Ended => {
                    self.closed = Some(None);
                    return;
                }
                Got::Quiet => return,
            }
        }
    }

    /// Reads until `done` holds for the events kept so far, at most `within`. True when it held.
    pub async fn until(&mut self, within: Duration, done: impl Fn(&[Value]) -> bool) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if done(&self.events) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            match self.client.read(left.min(Duration::from_millis(200))).await {
                Got::Message(Incoming::Text(text)) => {
                    let value: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"));
                    self.keep(value);
                }
                Got::Message(Incoming::Close(code, _)) => {
                    self.closed = Some(code);
                    return done(&self.events);
                }
                Got::Message(_) => {}
                Got::Ended => {
                    self.closed = Some(None);
                    return done(&self.events);
                }
                Got::Quiet => {}
            }
        }
    }

    /// Reads until `count` answers that nobody waited for have come in (or `within` has passed) and returns them in the order
    /// they arrived; the events read meanwhile are kept.
    pub async fn answers(&mut self, count: usize, within: Duration) -> Vec<Value> {
        let deadline = Instant::now() + within;
        while self.strays.len() < count {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match self.client.read(left).await {
                Got::Message(Incoming::Text(text)) => {
                    let value: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"));
                    self.keep(value);
                }
                Got::Message(Incoming::Close(code, _)) => {
                    self.closed = Some(code);
                    break;
                }
                Got::Message(_) => {}
                Got::Ended => {
                    self.closed = Some(None);
                    break;
                }
                Got::Quiet => break,
            }
        }
        std::mem::take(&mut self.strays)
    }

    /// The events with the given name, in order.
    pub fn events_named(&self, name: &str) -> Vec<&Value> {
        self.events.iter().filter(|e| e["event"] == name).collect()
    }

    /// The `frame` payloads of one pane, in the order read.
    pub fn frames_of(&self, pane: u64) -> Vec<Value> {
        self.events
            .iter()
            .filter(|e| e["event"] == "frame" && e["payload"]["pane"].as_u64() == Some(pane))
            .map(|e| e["payload"].clone())
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------------------------------------------------

/// The rows the browser does not have, read from the table.
pub fn unavailable_rows() -> Vec<&'static Command> {
    COMMANDS.iter().filter(|c| c.web == Web::Unavailable).collect()
}

/// The groups of the table in the order they first appear.
pub fn groups() -> Vec<&'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    for command in COMMANDS {
        if !seen.contains(&command.group) {
            seen.push(command.group);
        }
    }
    seen
}

/// The row to try for a group: the first that the browser has and that takes no argument, or else the first the browser has.
pub fn row_for_group(group: &str) -> Option<&'static Command> {
    let mut rows = COMMANDS.iter().filter(|c| c.group == group && c.web != Web::Unavailable);
    let first = rows.next()?;
    if first.args.is_empty() {
        return Some(first);
    }
    COMMANDS.iter().find(|c| c.group == group && c.web != Web::Unavailable && c.args.is_empty()).or(Some(first))
}

// ---------------------------------------------------------------------------------------------------------------------
// A started environment with the worker's probe and log
// ---------------------------------------------------------------------------------------------------------------------

/// An environment with a probe on the commands the worker ran and a capture of the worker's log.
pub struct Probed {
    pub env: WsEnv,
    pub probe: Arc<Probe>,
    pub log: Arc<Mutex<Vec<String>>>,
}

impl Probed {
    /// Starts the real gate and the real worker with the probe and the log capture added to `options`.
    pub async fn start(mut options: WsOptions) -> Probed {
        let probe = Probe::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        let hooks = TestHooks { probe: Some(probe.clone()), log: Some(log.clone()), ..options.hooks.clone() };
        options.hooks = hooks;
        options.real_gate = true;
        let env = WsEnv::start_with(options).await;
        Probed { env, probe, log }
    }

    /// The number of commands the worker ran.
    pub fn ran(&self) -> usize {
        self.probe.calls().len()
    }

    /// The names of the commands the worker ran.
    pub fn ran_names(&self) -> Vec<String> {
        self.probe.calls().into_iter().map(|(name, _)| name).collect()
    }

    /// The lines the worker logged.
    pub fn log_lines(&self) -> Vec<String> {
        self.log.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// People, sockets and panes
// ---------------------------------------------------------------------------------------------------------------------

/// A new sign-in of `person` with a new socket whose first message (`hw-session`) has been read.
pub async fn fresh(probed: &Probed, person: &crate::ws_env::Person) -> Line {
    let signed = probed.env.sign_in_again(person);
    let mut line = Line::new(probed.env.connect_as(&signed).await);
    let first = line.until(crate::ws_env::PATIENCE, |events| events.iter().any(|e| e["event"] == "hw-session")).await;
    assert!(first, "the session message");
    line
}

/// The size of the panes the suites start.
pub fn pane_size() -> Value {
    json!({"cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16})
}

/// Starts a shell in the pane that has the focus and returns its id.
pub async fn start_focused_pane(line: &mut Line) -> u64 {
    let view = line.ok("get_layout", &json!({})).await;
    let pane = crate::ws_env::focus(&view);
    start_pane_with_size(line, pane).await;
    pane
}

/// Starts a shell in the layout pane `pane`.
pub async fn start_pane_with_size(line: &mut Line, pane: u64) {
    let mut args = pane_size();
    args["pane"] = json!(pane);
    line.ok("start_pane", &args).await;
}

/// Splits the focused pane, starts a shell in the new one and returns its id.
pub async fn split_and_start(line: &mut Line) -> u64 {
    let view = line.ok("split", &json!({"column": false})).await;
    let pane = crate::ws_env::focus(&view);
    start_pane_with_size(line, pane).await;
    pane
}

/// Types `text` into a pane.
pub async fn type_into(line: &mut Line, pane: u64, text: &str) {
    line.ok("text", &json!({"pane": pane, "text": text})).await;
}

/// The screen of a pane as the frames this socket has read so far rebuild it.
pub fn screen_of(line: &Line, pane: u64) -> Screen {
    Screen::rebuilt(&line.frames_of(pane))
}

/// Reads until the rebuilt screen of `pane` holds `needle`; true when it did within `within`.
pub async fn until_screen_has(line: &mut Line, pane: u64, needle: &str, within: Duration) -> bool {
    let needle = needle.to_owned();
    line.until(within, move |events| {
        let frames: Vec<Value> =
            events.iter().filter(|e| e["event"] == "frame" && e["payload"]["pane"].as_u64() == Some(pane)).map(|e| e["payload"].clone()).collect();
        Screen::rebuilt(&frames).rows.iter().any(|row| row.contains(&needle))
    })
    .await
}

/// Reads until the rebuilt screen of `pane` satisfies `wanted`; true when it did within `within`.
pub async fn until_screen(line: &mut Line, pane: u64, within: Duration, wanted: impl Fn(&[String]) -> bool) -> bool {
    line.until(within, move |events| {
        let frames: Vec<Value> =
            events.iter().filter(|e| e["event"] == "frame" && e["payload"]["pane"].as_u64() == Some(pane)).map(|e| e["payload"].clone()).collect();
        wanted(&Screen::rebuilt(&frames).rows)
    })
    .await
}

/// Whether a process with this id exists (`ps` answers for any process of the machine).
pub fn process_exists(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim() == pid.to_string())
        .unwrap_or(false)
}

/// The whole text of a pane (the screen and the scrollback) as the core reports it for a selection of everything.
pub async fn whole_text(line: &mut Line, pane: u64) -> String {
    line.ok("select", &json!({"pane": pane, "op": {"kind": "all"}})).await;
    let text = line.ok("selection_text", &json!({"pane": pane})).await;
    text.as_str().unwrap_or_default().to_owned()
}

// ---------------------------------------------------------------------------------------------------------------------
// The screen of a pane, rebuilt from its frames
// ---------------------------------------------------------------------------------------------------------------------

/// The rows of a pane's screen as text, rebuilt from `frame` payloads the way the page does: a full frame replaces the grid, a
/// partial frame replaces the rows it names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Screen {
    pub rows: Vec<String>,
    /// The `seq` of the last frame applied.
    pub seq: u64,
}

fn row_text(row: &Value) -> String {
    let mut cells: Vec<String> = Vec::new();
    for run in row["runs"].as_array().map(Vec::as_slice).unwrap_or_default() {
        for ch in run[0].as_str().unwrap_or_default().chars() {
            // A wide character fills two cells; the second is a NUL.
            cells.push(if ch == '\0' { String::new() } else { ch.to_string() });
        }
    }
    for extra in row["extras"].as_array().map(Vec::as_slice).unwrap_or_default() {
        if let (Some(x), Some(text)) = (extra[0].as_u64(), extra[1].as_str()) {
            if let Some(cell) = cells.get_mut(usize::try_from(x).unwrap_or(usize::MAX)) {
                cell.push_str(text);
            }
        }
    }
    cells.concat().trim_end().to_owned()
}

impl Screen {
    /// Applies the payload of one `frame` event.
    pub fn apply(&mut self, frame: &Value) {
        let rows = usize::try_from(frame["rows"].as_u64().unwrap_or(0)).unwrap_or(0);
        if frame["full"].as_bool().unwrap_or(false) || self.rows.len() != rows {
            self.rows = vec![String::new(); rows];
        }
        for changed in frame["changed"].as_array().map(Vec::as_slice).unwrap_or_default() {
            if let Some(y) = changed["y"].as_u64().and_then(|y| usize::try_from(y).ok()) {
                if let Some(slot) = self.rows.get_mut(y) {
                    *slot = row_text(changed);
                }
            }
        }
        self.seq = frame["seq"].as_u64().unwrap_or(self.seq);
    }

    /// The screen after all of `frames`.
    pub fn rebuilt(frames: &[Value]) -> Screen {
        let mut screen = Screen::default();
        for frame in frames {
            screen.apply(frame);
        }
        screen
    }

    /// The text of the screen without the empty rows at the end.
    pub fn text(&self) -> String {
        let mut rows: Vec<&str> = self.rows.iter().map(String::as_str).collect();
        while rows.last().is_some_and(|r| r.is_empty()) {
            rows.pop();
        }
        rows.join("\n")
    }
}

/// Convenience: the rows of a rebuilt screen.
pub fn rebuild_screen(frames: &[Value]) -> Vec<String> {
    Screen::rebuilt(frames).rows
}

/// Whether the `seq` of the payloads is strictly increasing.
pub fn strictly_increasing(frames: &[Value]) -> Result<(), String> {
    let mut last: Option<u64> = None;
    for frame in frames {
        let seq = frame["seq"].as_u64().ok_or_else(|| String::from("a frame without a number"))?;
        if let Some(before) = last {
            if seq <= before {
                return Err(format!("seq {seq} follows {before}"));
            }
        }
        last = Some(seq);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// Numbered lines
// ---------------------------------------------------------------------------------------------------------------------

/// Checks that `text` holds the lines `first..=last` (as whole lines) in ascending order, once each, with nothing between them
/// that is a number of the same kind. The error names the first gap, duplicate or reordering.
pub fn check_numbered(text: &str, prefix: &str, first: u64, last: u64) -> Result<(), String> {
    let mut want = first;
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix(prefix) else { continue };
        let Ok(n) = rest.parse::<u64>() else { continue };
        if n < first || n > last {
            continue;
        }
        if n != want {
            return Err(if n < want { format!("{prefix}{n} again where {prefix}{want} was due") } else { format!("{prefix}{want} missing before {prefix}{n}") });
        }
        want += 1;
    }
    if want != last + 1 {
        return Err(format!("{prefix}{want} is the first missing of {prefix}{first} to {prefix}{last}"));
    }
    Ok(())
}

/// The program of the counting pane: `lines` numbered lines `hw-n1` ... `hw-nN`, `pause` seconds apart, then `hw-done`.
pub fn counting_program(lines: u64, pause: &str) -> String {
    format!("i=1; while [ $i -le {lines} ]; do echo \"hw-n$i\"; i=$((i+1)); sleep {pause}; done; echo hw-done\r")
}

// ---------------------------------------------------------------------------------------------------------------------
// Pure tests
// ---------------------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod pure {
    use super::*;

    #[test]
    fn pure_shape_equal_objects_pass() {
        assert!(same_shape(&json!({"a": 1, "b": {"c": "x"}}), &json!({"a": 5, "b": {"c": "y"}})).is_ok());
    }

    #[test]
    fn pure_shape_missing_member_names_the_path() {
        let error = same_shape(&json!({"a": {}}), &json!({"a": {"b": 1}})).unwrap_err();
        assert!(error.contains("$.a") && error.contains("\"b\""), "{error}");
    }

    #[test]
    fn pure_shape_extra_member_fails() {
        let error = same_shape(&json!({"a": 1, "z": 2}), &json!({"a": 1})).unwrap_err();
        assert!(error.contains("extra") && error.contains("\"z\""), "{error}");
    }

    #[test]
    fn pure_shape_scalars_compare_by_type() {
        assert!(same_shape(&json!("x"), &json!("y")).is_ok());
        assert!(same_shape(&json!(true), &json!(false)).is_ok());
        let error = same_shape(&json!("1"), &json!(1)).unwrap_err();
        assert!(error.contains("a string") && error.contains("a number"), "{error}");
    }

    #[test]
    fn pure_shape_integers_and_floats_are_both_numbers() {
        assert!(same_shape(&json!(1), &json!(2.5)).is_ok());
        assert!(same_shape(&json!(2.5), &json!(1)).is_ok());
    }

    #[test]
    fn pure_shape_a_golden_null_accepts_anything() {
        for value in [json!(null), json!(1), json!("x"), json!([1, 2]), json!({"a": 1})] {
            assert!(same_shape(&value, &json!(null)).is_ok(), "{value}");
        }
    }

    #[test]
    fn pure_shape_a_null_where_the_golden_has_a_value_fails() {
        assert!(same_shape(&json!(null), &json!(1)).is_err());
        assert!(same_shape(&json!(null), &json!({"a": 1})).is_err());
    }

    #[test]
    fn pure_shape_arrays_compare_every_element_with_the_first_golden_element() {
        assert!(same_shape(&json!([{"a": 1}, {"a": 2}]), &json!([{"a": 0}])).is_ok());
        let error = same_shape(&json!([{"a": 1}, {"b": 2}]), &json!([{"a": 0}])).unwrap_err();
        assert!(error.contains("$[1]"), "{error}");
    }

    #[test]
    fn pure_shape_a_member_that_is_null_in_one_golden_instance_is_optional() {
        let golden = json!([{"chord": "Cmd+D"}, {"chord": null}]);
        assert!(same_shape(&json!([{"chord": "Cmd+K"}, {"chord": null}, {"chord": "x"}]), &golden).is_ok());
        let error = same_shape(&json!([{"chord": "x", "extra": 7}]), &golden).unwrap_err();
        assert!(error.contains("$[0]") && error.contains("extra"), "{error}");
    }

    #[test]
    fn pure_shape_an_empty_array_is_accepted_and_an_empty_golden_accepts_any_array() {
        assert!(same_shape(&json!([]), &json!([{"a": 1}])).is_ok());
        assert!(same_shape(&json!([1, "x", {}]), &json!([])).is_ok());
    }

    #[test]
    fn pure_shape_a_tuple_is_compared_by_position() {
        let golden = json!([3, {"x": 0.0}]);
        assert!(same_shape(&json!([9, {"x": 1.5}]), &golden).is_ok());
        assert!(same_shape(&json!([9]), &golden).is_err());
        assert!(same_shape(&json!([{"x": 1.5}, 9]), &golden).is_err());
    }

    #[test]
    fn pure_shape_an_array_where_an_object_is_due_fails() {
        let error = same_shape(&json!([1]), &json!({"a": 1})).unwrap_err();
        assert!(error.contains("an array") && error.contains("an object"), "{error}");
    }

    #[test]
    fn pure_shape_an_error_never_holds_a_value() {
        let error = same_shape(&json!({"a": "SECRET-CANARY"}), &json!({"a": 1})).unwrap_err();
        assert!(!error.contains("SECRET-CANARY"), "{error}");
    }

    #[test]
    fn pure_shape_any_of_the_variants_passes_when_one_matches() {
        let variants = json!([{"a": 1}, {"b": 2}]);
        assert!(shape_matches_any(&json!({"b": 0}), &variants).is_ok());
        assert!(shape_matches_any(&json!({"c": 0}), &variants).is_err());
    }

    #[test]
    fn pure_golden_is_read_and_names_the_commands() {
        let names = golden_command_names();
        assert!(names.contains("get_layout"), "the golden holds the layout command");
        assert_eq!(golden_command_args("rename_pane").as_deref(), Some(&["pane".to_owned(), "name".to_owned()][..]));
        assert!(golden_at("answers.ai.ticket").is_some() && golden_at("answers.nope").is_none());
    }

    #[test]
    fn pure_args_are_built_from_the_names_and_types_of_a_row() {
        let row = lookup("rename_pane").expect("a row");
        let args = args_for(row);
        assert_eq!(args["name"], json!(""));
        assert_eq!(args["pane"], json!(NOWHERE));
        let optional = lookup("start_pane").expect("a row");
        let args = args_for(optional);
        assert!(args.get("cell_w").is_none() && args.get("cols") == Some(&json!(0)), "{args}");
        assert_eq!(args_for(lookup("get_layout").expect("a row")), json!({}));
        assert_eq!(args_without_types(&["a", "b"]), json!({"a": null, "b": null}));
    }

    #[test]
    fn pure_every_row_gets_an_object_of_exactly_its_arguments_or_fewer() {
        for row in COMMANDS {
            let args = args_for(row);
            let object = args.as_object().expect("an object");
            for key in object.keys() {
                assert!(row.args.iter().any(|a| a.name == key), "{} got {key}", row.name);
            }
            for arg in row.args {
                let optional = squash(arg.ty).starts_with("Option<");
                assert_eq!(object.contains_key(arg.name), !optional, "{}.{}", row.name, arg.name);
            }
        }
    }

    #[test]
    fn pure_hostile_values_cover_the_named_kinds() {
        let values = hostile_texts("CANARY");
        assert_eq!(values.len(), 10);
        assert!(values.iter().any(|(_, v)| v == "null"));
        assert!(values.iter().any(|(_, v)| v == "1e309"));
        assert!(values.iter().any(|(_, v)| v.contains("\\u0000") && v.contains("CANARY")));
        assert!(values.iter().any(|(_, v)| v.starts_with('{')));
        for (label, text) in &values {
            if *label != "overflow" {
                assert!(serde_json::from_str::<Value>(text).is_ok(), "{label}: {text}");
            }
        }
        assert_eq!(nested(50).to_string().matches('[').count(), 50);
        assert_eq!(hostile_whole_args().len(), 4);
    }

    #[test]
    fn pure_the_args_text_replaces_one_argument_and_keeps_the_others_benign() {
        let row = lookup("rename_pane").expect("a row");
        let text = args_text_with(row, 1, "1e309");
        assert_eq!(text, format!("{{\"pane\":{NOWHERE},\"name\":1e309}}"));
        let optional = lookup("start_pane").expect("a row");
        let text = args_text_with(optional, 3, "-1");
        assert!(text.contains("\"cell_w\":-1") && !text.contains("cell_h"), "{text}");
    }

    #[test]
    fn pure_the_sender_refuses_a_name_that_is_not_in_the_table_and_the_lookup_is_exact() {
        for name in ["get_layoutx", "GET_LAYOUT", "get_layout ", "", "../get_layout", "__proto__"] {
            assert!(lookup(name).is_none(), "{name:?}");
        }
        assert!(lookup("get_layout").is_some());
    }

    #[test]
    fn pure_the_screen_is_rebuilt_from_the_fixture_step_by_step() {
        let fixture = frame_fixture();
        let mut screen = Screen::default();
        for step in fixture["steps"].as_array().expect("steps") {
            screen.apply(&step["frame"]);
            let want: Vec<String> = step["grid"].as_array().expect("grid").iter().map(|r| r.as_str().expect("row").to_owned()).collect();
            assert_eq!(screen.rows, want, "after the step {}", step["name"]);
        }
    }

    #[test]
    fn pure_a_partial_frame_replaces_only_its_rows_and_a_full_frame_replaces_all() {
        let mut screen = Screen::default();
        let full = json!({"seq": 1, "full": true, "rows": 3, "changed": [
            {"y": 0, "runs": [["one  ", 5, 0, 0, 0]]}, {"y": 1, "runs": [["two  ", 5, 0, 0, 0]]}, {"y": 2, "runs": []}]});
        screen.apply(&full);
        assert_eq!(screen.rows, ["one", "two", ""]);
        screen.apply(&json!({"seq": 2, "full": false, "rows": 3, "changed": [{"y": 2, "runs": [["x", 1, 0, 0, 0]]}]}));
        assert_eq!(screen.rows, ["one", "two", "x"]);
        screen.apply(&json!({"seq": 3, "full": true, "rows": 2, "changed": [{"y": 0, "runs": [["z", 1, 0, 0, 0]]}]}));
        assert_eq!(screen.rows, ["z", ""]);
        assert_eq!(screen.seq, 3);
    }

    #[test]
    fn pure_seq_must_strictly_increase() {
        assert!(strictly_increasing(&[json!({"seq": 1}), json!({"seq": 2}), json!({"seq": 9})]).is_ok());
        assert!(strictly_increasing(&[json!({"seq": 1}), json!({"seq": 1})]).is_err());
        assert!(strictly_increasing(&[json!({"seq": 3}), json!({"seq": 2})]).is_err());
    }

    #[test]
    fn pure_numbered_lines_find_a_gap_a_duplicate_and_a_reordering() {
        let good: String = (1..=5).map(|i| format!("hw-n{i}\n")).collect();
        assert!(check_numbered(&good, "hw-n", 1, 5).is_ok());
        assert!(check_numbered("hw-n1\nhw-n3\n", "hw-n", 1, 3).unwrap_err().contains("hw-n2 missing"));
        assert!(check_numbered("hw-n1\nhw-n1\nhw-n2\n", "hw-n", 1, 2).unwrap_err().contains("again"));
        assert!(check_numbered("hw-n2\nhw-n1\n", "hw-n", 1, 2).is_err());
        assert!(check_numbered("hw-n1\nhw-n2\n", "hw-n", 1, 3).unwrap_err().contains("hw-n3"));
        assert!(check_numbered("noise\nhw-nX\nhw-n1\n", "hw-n", 1, 1).is_ok());
    }

    #[test]
    fn pure_the_groups_are_read_from_the_table_and_each_has_a_row() {
        let all = groups();
        assert!(all.len() >= 2, "{all:?}");
        for group in all {
            let row = row_for_group(group).expect("a row for the group");
            assert_eq!(row.group, group);
            assert_ne!(row.web, Web::Unavailable);
        }
    }

    #[test]
    fn pure_the_counting_program_is_plain_sh() {
        let program = counting_program(300, "0.02");
        assert!(program.contains("-le 300") && program.contains("hw-done") && program.ends_with('\r'));
    }
}
