//! Kubernetes through `kubectl`: the argument lists of its verbs and the parsers of its answers.
//!
//! `kubectl` is run as a program by the engine of this module's neighbours; no Kubernetes client is written here, so
//! its auth plugins, proxies and the exec transport stay `kubectl`'s own work. This file holds argument builders,
//! parsers and the overlay kubeconfig of a shell pane: it starts no process and keeps no state, and the only files it
//! touches are the overlays below. Every argument list is built from validated names ([`super::args`]): a name that
//! follows an option travels in the equals form, and every other name follows `--`.
//!
//! What `kubectl` prints is another party's text. Each answer is read by a typed reader that keeps only the members a
//! table shows. A member that is not there or has another type is a `?` cell and not an error, an unknown member is
//! skipped without being kept, and a struct is read from a JSON object only (a derived reader would also take an array
//! of the member values in order). The credential members of a kubeconfig have no field in any type here, and a
//! kubeconfig that cannot be read is never shown as text, because `kubectl` leaves the environment values of an exec
//! plugin in clear in `config view`.
//!
//! A shell pane gets its own current context without a copy of anything secret: a small overlay file, written by
//! [`write_overlay`], names one context, its cluster, its user and its namespace and nothing else, and
//! [`kubeconfig_value`] lists it first in `KUBECONFIG`, ahead of the person's own files. `kubectl` merges the list and
//! the first file that names a thing wins, so the overlay's current context and namespace apply to the pane while the
//! person's files are never edited by a selection. Two facts belong with that: an exec plugin or a shell start-up file
//! can still change a context (and a start-up file that exports `KUBECONFIG` replaces the pane's value), so the pane's
//! chip says what holzWave set and not what is certain; and the tool's own files change only through the two local
//! verbs that a card confirms. Overlays live in directories of mode 0700 as files of mode 0600, are replaced
//! atomically, and [`sweep_overlays`] removes the ones whose pane is gone.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::value::MapAccessDeserializer;
use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

use super::args::{flag_eq, ArgError, ContextName, Namespace};
use super::run::MAX_STDOUT;
use super::view::{text_fallback, text_view, RowBuilder, TableBuilder, TextFormat, Tone, ViewModel};
use crate::layout::PaneId;
use crate::persist::write_atomic_private;
use crate::platform::{self, Os};

use super::{
    ArgKind, ArgSpec, CheckedSelection, DefaultRead, Integration, IntegrationSpec, LaunchProgram, Output, ParseError,
    SelectKind, Selection, ToolId, Validated, Verb, VerbEffect, VersionProbe,
};

/// The most bytes of one answer that are read; the runner caps standard output below this.
const MAX_INPUT: usize = MAX_STDOUT;

/// The most items of one list that are kept, whatever the list holds; the rest are counted and dropped.
const LIST_KEPT: usize = 10_000;

/// The most items of a list inside an item (the containers of a pod) that are kept.
const INNER_KEPT: usize = 256;

/// The most characters of a version that are kept.
const VERSION_MAX: usize = 64;

// ---------------------------------------------------------------------------------------------------------------------
// The spec.
// ---------------------------------------------------------------------------------------------------------------------

const NO_ARGS: &[ArgSpec] = &[];
const POD: ArgSpec = ArgSpec { name: "pod", kind: ArgKind::Pod, required: true };
const CONTAINER: ArgSpec = ArgSpec { name: "container", kind: ArgKind::Container, required: false };

/// The verbs, in the order they are listed. Each is found by its id only.
static VERBS: [Verb; 9] = [
    Verb {
        id: "contexts",
        label: "Contexts",
        effect: VerbEffect::Read,
        output: Output::View,
        args: NO_ARGS,
        refresh_secs: None,
        exit_is_state: false,
    },
    Verb {
        id: "namespaces",
        label: "Namespaces",
        effect: VerbEffect::Read,
        output: Output::View,
        args: NO_ARGS,
        refresh_secs: None,
        exit_is_state: false,
    },
    Verb {
        id: "pods",
        label: "Pods",
        effect: VerbEffect::Read,
        output: Output::View,
        args: NO_ARGS,
        refresh_secs: Some(10),
        exit_is_state: false,
    },
    Verb {
        id: "use_context",
        label: "Use context",
        effect: VerbEffect::Local,
        output: Output::Done,
        args: &[ArgSpec { name: "context", kind: ArgKind::Context, required: true }],
        refresh_secs: None,
        exit_is_state: false,
    },
    Verb {
        id: "set_namespace",
        label: "Set namespace",
        effect: VerbEffect::Local,
        output: Output::Done,
        args: &[ArgSpec { name: "namespace", kind: ArgKind::Namespace, required: true }],
        refresh_secs: None,
        exit_is_state: false,
    },
    Verb {
        id: "delete_pod",
        label: "Delete pod",
        effect: VerbEffect::Write,
        output: Output::Done,
        args: &[POD],
        refresh_secs: None,
        exit_is_state: false,
    },
    Verb {
        id: "exec",
        label: "Exec",
        effect: VerbEffect::Exec,
        output: Output::Terminal,
        args: &[POD, CONTAINER],
        refresh_secs: None,
        exit_is_state: false,
    },
    Verb {
        id: "logs",
        label: "Logs",
        effect: VerbEffect::Read,
        output: Output::Terminal,
        args: &[POD, CONTAINER, ArgSpec { name: "lines", kind: ArgKind::Lines, required: false }],
        refresh_secs: None,
        exit_is_state: false,
    },
    Verb {
        id: "shell",
        label: "Shell",
        effect: VerbEffect::Read,
        output: Output::Terminal,
        args: NO_ARGS,
        refresh_secs: None,
        exit_is_state: false,
    },
];

/// The description of `kubectl`: its names, where it is searched when the `PATH` does not have it, how its version is
/// asked for, and the nine verbs. It sets no environment of its own: the file the person chose reaches `kubectl`
/// through the environment of the run.
pub static SPEC: IntegrationSpec = IntegrationSpec {
    id: ToolId::Kube,
    label: "Kubernetes",
    binaries: &["kubectl"],
    windows_binaries: &["kubectl.exe"],
    fixed_dirs: &[
        // macOS: Homebrew, the installers of the project, a person's own bin directory, Docker Desktop's copy.
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "$HOME/.local/bin",
        "$HOME/bin",
        "$HOME/.docker/bin",
        // Linux: the distribution, Snap and Homebrew on Linux (the person's own directories are listed above).
        "/usr/bin",
        "/snap/bin",
        "/home/linuxbrew/.linuxbrew/bin",
        // Windows: winget's links, Docker Desktop's copy and a person's own bin directory.
        "%LOCALAPPDATA%\\Microsoft\\WinGet\\Links",
        "%ProgramFiles%\\Docker\\Docker\\resources\\bin",
        "%USERPROFILE%\\bin",
    ],
    version_probe: VersionProbe { argv: &["version", "--client", "-o", "json"], parse: parse_version_text },
    env_set: &[],
    selects: &[SelectKind::Context, SelectKind::Namespace],
    verbs: &VERBS,
};

/// The verb with this id; the ids used below are all in [`VERBS`].
fn own_verb(id: &str) -> Option<&'static Verb> {
    SPEC.verb(id)
}

fn parse_version_text(text: &str) -> Option<String> {
    parse_version(text)
}

// ---------------------------------------------------------------------------------------------------------------------
// Readers: JSON objects read for what a table shows, and nothing else.
// ---------------------------------------------------------------------------------------------------------------------

/// What one member of a JSON object turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Member<T> {
    /// The member is not there, or is `null`.
    Missing,
    /// The member is there with another type than the reader wants.
    Wrong,
    /// The member, read.
    Is(T),
}

impl<T> Default for Member<T> {
    fn default() -> Self {
        Self::Missing
    }
}

impl<T> Member<T> {
    fn is(&self) -> Option<&T> {
        if let Self::Is(value) = self { Some(value) } else { None }
    }
}

/// What a type takes from a JSON value. Everything it does not take is `Wrong`, and is skipped without being kept.
trait Accept<'de>: Sized {
    fn string(_: &str) -> Option<Self> {
        None
    }

    fn unsigned(_: u64) -> Option<Self> {
        None
    }

    fn boolean(_: bool) -> Option<Self> {
        None
    }

    fn object<A: MapAccess<'de>>(map: A) -> Result<Option<Self>, A::Error> {
        skip_map(map)?;
        Ok(None)
    }
}

fn skip_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
    Ok(())
}

impl<'de> Accept<'de> for String {
    fn string(text: &str) -> Option<Self> {
        Some(text.to_owned())
    }
}

impl<'de> Accept<'de> for u64 {
    fn unsigned(number: u64) -> Option<Self> {
        Some(number)
    }
}

impl<'de> Accept<'de> for bool {
    fn boolean(flag: bool) -> Option<Self> {
        Some(flag)
    }
}

/// Makes a struct a member type that is read from a JSON object only: `object` hands the map to the derived reader,
/// which never sees the positional array form because only `visit_map` leads here.
macro_rules! object_members {
    ($($name:ident),* $(,)?) => {
        $(impl<'de> Accept<'de> for $name {
            fn object<A: MapAccess<'de>>(map: A) -> Result<Option<Self>, A::Error> {
                Self::deserialize(MapAccessDeserializer::new(map)).map(Some)
            }
        })*
    };
}

struct MemberReader<T>(PhantomData<T>);

impl<'de, T: Accept<'de>> Visitor<'de> for MemberReader<T> {
    type Value = Member<T>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(T::boolean(value).map_or(Member::Wrong, Member::Is))
    }

    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
        Ok(Member::Wrong)
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(T::unsigned(value).map_or(Member::Wrong, Member::Is))
    }

    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
        Ok(Member::Wrong)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(T::string(value).map_or(Member::Wrong, Member::Is))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Member::Missing)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Member::Missing)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Member::Wrong)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        Ok(T::object(map)?.map_or(Member::Wrong, Member::Is))
    }
}

impl<'de, T: Accept<'de>> Deserialize<'de> for Member<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(MemberReader(PhantomData))
    }
}

/// The shape of a list member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// The member is not there.
    Missing,
    /// The member is `null`, which is how `kubectl` writes an empty list of a kubeconfig.
    Null,
    /// The member is an array.
    List,
    /// The member is something else.
    Wrong,
}

/// A list member: at most `CAP` items are kept, the others are counted, and an item that is not an object is counted as
/// unreadable. A list of empty objects cannot make the reader allocate more than `CAP` items.
#[derive(Debug)]
struct Seq<T, const CAP: usize> {
    shape: Shape,
    items: Vec<T>,
    total: usize,
    unreadable: usize,
}

impl<T, const CAP: usize> Default for Seq<T, CAP> {
    fn default() -> Self {
        Self { shape: Shape::Missing, items: Vec::new(), total: 0, unreadable: 0 }
    }
}

impl<T, const CAP: usize> Seq<T, CAP> {
    /// Whether every item of the list was read and kept.
    fn complete(&self) -> bool {
        self.shape == Shape::List && self.unreadable == 0 && self.total == self.items.len()
    }
}

struct SeqReader<T, const CAP: usize>(PhantomData<T>);

impl<'de, T: Accept<'de>, const CAP: usize> Visitor<'de> for SeqReader<T, CAP> {
    type Value = Seq<T, CAP>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
        Ok(Seq { shape: Shape::Wrong, ..Seq::default() })
    }

    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
        Ok(Seq { shape: Shape::Wrong, ..Seq::default() })
    }

    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
        Ok(Seq { shape: Shape::Wrong, ..Seq::default() })
    }

    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
        Ok(Seq { shape: Shape::Wrong, ..Seq::default() })
    }

    fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
        Ok(Seq { shape: Shape::Wrong, ..Seq::default() })
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Seq { shape: Shape::Null, ..Seq::default() })
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Seq { shape: Shape::Null, ..Seq::default() })
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut out = Seq { shape: Shape::List, ..Seq::default() };
        while let Some(item) = seq.next_element::<Member<T>>()? {
            out.total += 1;
            match item {
                Member::Is(value) if out.items.len() < CAP => out.items.push(value),
                Member::Is(_) => {}
                Member::Missing | Member::Wrong => out.unreadable += 1,
            }
        }
        Ok(out)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        skip_map(map)?;
        Ok(Seq { shape: Shape::Wrong, ..Seq::default() })
    }
}

impl<'de, T: Accept<'de>, const CAP: usize> Deserialize<'de> for Seq<T, CAP> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(SeqReader::<T, CAP>(PhantomData))
    }
}

/// Reads a whole answer as one object of type `T`. An empty answer, one over the cap and one that is no JSON object of
/// that shape are errors without any text of the answer in them.
fn read_document<T: for<'de> Accept<'de>>(bytes: &[u8]) -> Result<T, ParseError> {
    if bytes.len() > MAX_INPUT {
        return Err(ParseError::TooLarge);
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(ParseError::Empty);
    }
    match serde_json::from_slice::<Member<T>>(bytes) {
        Ok(Member::Is(document)) => Ok(document),
        Ok(_) | Err(_) => Err(ParseError::Unrecognised),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The version.
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct VersionDocument {
    #[serde(rename = "clientVersion", default)]
    client: Member<ClientVersion>,
}

#[derive(Deserialize)]
struct ClientVersion {
    #[serde(rename = "gitVersion", default)]
    git: Member<String>,
}

object_members!(VersionDocument, ClientVersion);

/// The client's git version (`v1.36.1`) from `version --client -o json`, or `None` when the text is no such answer, is
/// empty or longer than 64 characters, or holds anything but visible ASCII.
pub fn parse_version(text: &str) -> Option<String> {
    let document: VersionDocument = read_document(text.as_bytes()).ok()?;
    let version = document.client.is()?.git.is()?;
    let valid = !version.is_empty()
        && version.chars().count() <= VERSION_MAX
        && version.chars().all(|c| c.is_ascii_graphic());
    valid.then(|| version.clone())
}

// ---------------------------------------------------------------------------------------------------------------------
// The contexts of `config view -o json`.
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct ConfigDocument {
    #[serde(rename = "current-context", default)]
    current: Member<String>,
    #[serde(default)]
    contexts: Seq<NamedContext, LIST_KEPT>,
}

#[derive(Deserialize)]
struct NamedContext {
    #[serde(default)]
    name: Member<String>,
    #[serde(default)]
    context: Member<ContextBody>,
}

#[derive(Deserialize)]
struct ContextBody {
    #[serde(default)]
    cluster: Member<String>,
    #[serde(default)]
    user: Member<String>,
    #[serde(default)]
    namespace: Member<String>,
}

object_members!(ConfigDocument, NamedContext, ContextBody);

/// One context of a kubeconfig, by names only: the context, the cluster and the user it refers to, and its namespace.
/// The members that hold a credential have no field here.
#[derive(Clone, PartialEq, Eq)]
pub struct ContextEntry {
    name: String,
    cluster: String,
    user: String,
    namespace: Option<String>,
}

impl ContextEntry {
    /// An entry from its names. Nothing is checked here: [`write_overlay`] validates what it writes.
    pub fn new(name: &str, cluster: &str, user: &str, namespace: Option<&str>) -> Self {
        Self {
            name: name.to_owned(),
            cluster: cluster.to_owned(),
            user: user.to_owned(),
            namespace: namespace.map(str::to_owned),
        }
    }

    /// The name of the context.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The name of its cluster, or an empty text when the context names none.
    pub fn cluster(&self) -> &str {
        &self.cluster
    }

    /// The name of its user, or an empty text when the context names none.
    pub fn user(&self) -> &str {
        &self.user
    }

    /// Its namespace, when it has one.
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }
}

impl fmt::Debug for ContextEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContextEntry(name len={})", self.name.len())
    }
}

/// The contexts of a kubeconfig and the current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextsData {
    current: Option<String>,
    contexts: Vec<ContextEntry>,
    omitted: usize,
}

impl ContextsData {
    /// The name of the current context, when there is one.
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// The contexts, in the order `kubectl` printed them (at most 10,000).
    pub fn contexts(&self) -> &[ContextEntry] {
        &self.contexts
    }

    /// How many contexts were left out because the list was longer than is read.
    pub fn omitted(&self) -> usize {
        self.omitted
    }

    /// The context with exactly this name.
    pub fn context(&self, name: &str) -> Option<&ContextEntry> {
        self.contexts.iter().find(|entry| entry.name == name)
    }
}

/// Reads `config view -o json`: the current context and, for every context, its name, cluster, user and namespace.
/// An empty answer is `Empty`, one over the cap `TooLarge`. Anything that is not the shape of a kubeconfig is
/// `Unrecognised`: not JSON, not an object, a `contexts` member that is missing or is no list, an entry without a
/// name, a member of a name that is no text. A `null` list is no contexts, which is how `kubectl` prints an empty
/// kubeconfig. No error holds a byte of the answer.
pub fn parse_contexts(bytes: &[u8]) -> Result<ContextsData, ParseError> {
    let document: ConfigDocument = read_document(bytes)?;
    let current = match &document.current {
        Member::Missing => None,
        Member::Is(name) if name.is_empty() => None,
        Member::Is(name) => Some(name.clone()),
        Member::Wrong => return Err(ParseError::Unrecognised),
    };
    let list = &document.contexts;
    match list.shape {
        Shape::Null => return Ok(ContextsData { current, contexts: Vec::new(), omitted: 0 }),
        Shape::List if list.unreadable == 0 => {}
        Shape::Missing | Shape::List | Shape::Wrong => return Err(ParseError::Unrecognised),
    }
    let mut contexts = Vec::with_capacity(list.items.len());
    for item in &list.items {
        let Member::Is(name) = &item.name else { return Err(ParseError::Unrecognised) };
        let (cluster, user, namespace) = match &item.context {
            Member::Is(body) => (names(&body.cluster)?, names(&body.user)?, names(&body.namespace)?),
            Member::Missing => (None, None, None),
            Member::Wrong => return Err(ParseError::Unrecognised),
        };
        contexts.push(ContextEntry {
            name: name.clone(),
            cluster: cluster.unwrap_or_default(),
            user: user.unwrap_or_default(),
            namespace: namespace.filter(|text| !text.is_empty()),
        });
    }
    Ok(ContextsData { current, contexts, omitted: list.total - list.items.len() })
}

/// The text of a name member: absent is `None`, a name that is no text is an error.
fn names(member: &Member<String>) -> Result<Option<String>, ParseError> {
    match member {
        Member::Missing => Ok(None),
        Member::Is(text) => Ok(Some(text.clone())),
        Member::Wrong => Err(ParseError::Unrecognised),
    }
}

/// The table of contexts: CURRENT (a star), NAME, CLUSTER, USER and NAMESPACE, a `use_context` action on each row whose
/// name validates.
fn contexts_view(data: &ContextsData) -> ViewModel {
    let mut table = TableBuilder::new("Contexts");
    table.column("current", "CURRENT").column("name", "NAME").column("cluster", "CLUSTER");
    table.column("user", "USER").column("namespace", "NAMESPACE");
    let use_context = own_verb("use_context");
    for entry in &data.contexts {
        let current = data.current.as_deref() == Some(entry.name.as_str());
        let star = if current { "*" } else { "" };
        let cells = [star, &entry.name, &entry.cluster, &entry.user, entry.namespace().unwrap_or("")];
        let mut row = RowBuilder::new(&entry.name, &cells);
        if let Some(verb) = use_context {
            row.action(verb, &[("context", &entry.name)]);
        }
        table.push(row);
    }
    let model = table.finish();
    if data.omitted > 0 {
        model.with_note(&format!("{} more contexts were not read.", data.omitted))
    } else {
        model
    }
}

/// What is shown for a kubeconfig answer that cannot be read: one fixed sentence and none of the answer, because
/// `config view` leaves the environment values of an exec plugin in clear.
fn unreadable_config() -> ViewModel {
    text_view("Contexts", "The kubeconfig summary is not in a shape that is known.", TextFormat::Plain)
        .with_note("It is not shown as text because a kubeconfig can hold credentials.")
}

// ---------------------------------------------------------------------------------------------------------------------
// The integration.
// ---------------------------------------------------------------------------------------------------------------------

/// `kubectl` as an [`Integration`].
#[derive(Debug, Clone, Copy)]
pub struct Kube;

/// The one value of [`Kube`].
pub static KUBE: Kube = Kube;

/// The verb of this spec that equals `verb`: a verb that another spec lists, even under the same id, is refused.
fn own(verb: &Verb) -> Result<&'static Verb, ArgError> {
    SPEC.verb(verb.id).filter(|listed| *listed == verb).ok_or(ArgError::Unknown)
}

impl Kube {
    /// The view model of the standard output of `verb`, with the ages of the rows computed against `now`. [`view`]
    /// calls it with the system clock; a test passes a fixed time.
    ///
    /// [`view`]: Integration::view
    pub fn view_at(&self, verb: &Verb, stdout: &[u8], now: SystemTime) -> Result<ViewModel, ParseError> {
        let verb = own(verb).map_err(|_| ParseError::Unrecognised)?;
        match verb.id {
            "contexts" => match parse_contexts(stdout) {
                Ok(data) => Ok(contexts_view(&data)),
                Err(ParseError::Unrecognised) => Ok(unreadable_config()),
                Err(other) => Err(other),
            },
            "namespaces" => namespaces_view(stdout, epoch_seconds(now)),
            "pods" => pods_view(stdout, epoch_seconds(now)),
            _ => Err(ParseError::Unrecognised),
        }
    }
}

impl Integration for Kube {
    fn spec(&self) -> &'static IntegrationSpec {
        &SPEC
    }

    fn argv(&self, verb: &Verb, args: &Validated, selection: &CheckedSelection) -> Result<Vec<OsString>, ArgError> {
        let verb = own(verb)?;
        match verb.id {
            "contexts" => Ok(["config", "view", "-o", "json"].map(OsString::from).to_vec()),
            "namespaces" => Ok(read_list("namespaces", selection, false)),
            "pods" => Ok(read_list("pods", selection, true)),
            "use_context" => {
                let context = args.context().ok_or(ArgError::Missing)?;
                Ok(vec!["config".into(), "use-context".into(), "--".into(), context.as_str().into()])
            }
            "set_namespace" => set_namespace(args, selection),
            "delete_pod" => delete_pod(args, selection),
            "exec" => exec(args, selection),
            "logs" => logs(args, selection),
            // The launch of the shell is the person's own shell with the overlay in its environment: no arguments.
            "shell" => Ok(Vec::new()),
            _ => Err(ArgError::Unknown),
        }
    }

    fn view(&self, verb: &Verb, stdout: &[u8]) -> Result<ViewModel, ParseError> {
        self.view_at(verb, stdout, SystemTime::now())
    }

    fn read_default(&self, verb: &Verb, stdout: &[u8]) -> Option<DefaultRead> {
        if own(verb).ok()?.id != "contexts" {
            return None;
        }
        let data = parse_contexts(stdout).ok()?;
        let current = data.current().and_then(|name| data.context(name));
        let selection = Selection {
            context: data.current().map(str::to_owned),
            namespace: current.and_then(ContextEntry::namespace).map(str::to_owned),
            ..Selection::default()
        };
        Some(DefaultRead { selection, signed_out: false, contexts: data.contexts().to_vec() })
    }

    fn changes(&self, verb: &Verb) -> Option<&'static str> {
        match own(verb).ok()?.id {
            "use_context" | "set_namespace" => {
                Some("the kubeconfig files named by KUBECONFIG, by default ~/.kube/config")
            }
            _ => None,
        }
    }

    fn launch_program(&self, verb: &Verb) -> LaunchProgram {
        if own(verb).is_ok_and(|own| own.id == "shell") { LaunchProgram::UserShell } else { LaunchProgram::Tool }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Argument lists.
// ---------------------------------------------------------------------------------------------------------------------

/// The time limit of a read, which a hung cluster cannot get past.
const REQUEST_TIMEOUT: &str = "--request-timeout=10s";

/// `--context=<name>` when the selection has a context. The name is validated, and the equals form keeps it one
/// argument, so it cannot become an option of its own.
fn context_flag(selection: &CheckedSelection) -> Option<OsString> {
    selection.context.as_ref().map(|context| flag_eq("context", context.as_str()))
}

/// `--namespace=<name>` when the selection has a namespace.
fn namespace_flag(selection: &CheckedSelection) -> Option<OsString> {
    selection.namespace.as_ref().map(|namespace| flag_eq("namespace", namespace.as_str()))
}

/// `get <what> -o json` with the selection and the time limit. A list of namespaces is not scoped to a namespace.
fn read_list(what: &str, selection: &CheckedSelection, namespaced: bool) -> Vec<OsString> {
    let mut argv: Vec<OsString> = ["get", what, "-o", "json"].map(OsString::from).to_vec();
    argv.extend(context_flag(selection));
    if namespaced {
        argv.extend(namespace_flag(selection));
    }
    argv.push(REQUEST_TIMEOUT.into());
    argv
}

/// The one string a shell reads in this module: it is the last argument of `exec`, run by `sh` inside the pod, and it
/// picks `bash` when the container has it and `sh` when it has not. Nothing is interpolated into it, no name and no
/// value; the pod, the container, the context and the namespace are separate arguments of `kubectl`.
pub const POD_SHELL: &str = "command -v bash >/dev/null 2>&1 && exec bash || exec sh";

/// The lines of a log that are asked for when the call names no number.
const LOG_LINES: u16 = 200;

fn container_flag(args: &Validated) -> Option<OsString> {
    args.container().map(|container| flag_eq("container", container.as_str()))
}

/// `config set-context`: the namespace of the context the person selected, or of the current context when there is no
/// selection. It rewrites the kubeconfig, which is why the verb is a local one behind a card.
fn set_namespace(args: &Validated, selection: &CheckedSelection) -> Result<Vec<OsString>, ArgError> {
    let namespace = args.namespace().ok_or(ArgError::Missing)?;
    let mut argv: Vec<OsString> = vec!["config".into(), "set-context".into()];
    match &selection.context {
        Some(context) => {
            argv.push(flag_eq("namespace", namespace.as_str()));
            argv.push("--".into());
            argv.push(context.as_str().into());
        }
        None => {
            argv.push("--current".into());
            argv.push(flag_eq("namespace", namespace.as_str()));
        }
    }
    Ok(argv)
}

/// `delete pod`, without waiting for the pod to go, within the time limit.
fn delete_pod(args: &Validated, selection: &CheckedSelection) -> Result<Vec<OsString>, ArgError> {
    let pod = args.pod().ok_or(ArgError::Missing)?;
    let mut argv: Vec<OsString> = vec!["delete".into(), "pod".into()];
    argv.extend(context_flag(selection));
    argv.extend(namespace_flag(selection));
    argv.extend(["--wait=false", REQUEST_TIMEOUT, "--"].map(OsString::from));
    argv.push(pod.as_str().into());
    Ok(argv)
}

/// `exec` with a terminal in a pod: the pod is the one positional before `--`, and after it comes the constant that
/// picks the shell inside the pod.
fn exec(args: &Validated, selection: &CheckedSelection) -> Result<Vec<OsString>, ArgError> {
    let pod = args.pod().ok_or(ArgError::Missing)?;
    let mut argv: Vec<OsString> = ["exec", "-i", "-t"].map(OsString::from).to_vec();
    argv.extend(context_flag(selection));
    argv.extend(namespace_flag(selection));
    argv.extend(container_flag(args));
    argv.push(pod.as_str().into());
    argv.extend(["--", "sh", "-c", POD_SHELL].map(OsString::from));
    Ok(argv)
}

/// `logs --follow` with the last lines first: 200 unless the call names a number.
fn logs(args: &Validated, selection: &CheckedSelection) -> Result<Vec<OsString>, ArgError> {
    let pod = args.pod().ok_or(ArgError::Missing)?;
    let tail = args.lines().unwrap_or(LOG_LINES);
    let mut argv: Vec<OsString> = vec!["logs".into(), "--follow".into(), flag_eq("tail", &tail.to_string())];
    argv.extend(context_flag(selection));
    argv.extend(namespace_flag(selection));
    argv.extend(container_flag(args));
    argv.push("--".into());
    argv.push(pod.as_str().into());
    Ok(argv)
}

// ---------------------------------------------------------------------------------------------------------------------
// Time.
// ---------------------------------------------------------------------------------------------------------------------

/// Whole seconds since the Unix epoch; a time before it is 0.
fn epoch_seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
}

/// Seconds since the Unix epoch of a `YYYY-MM-DDTHH:MM:SSZ` time, the form `kubectl` writes, or `None` for any other
/// text. The date is checked, not only the shape.
fn parse_timestamp(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':'), (19, b'Z')];
    let shape = bytes.len() == 20 && separators.iter().all(|(at, c)| bytes[*at] == *c);
    if !shape {
        return None;
    }
    let number = |from: usize, to: usize| -> Option<i64> {
        let digits = text.get(from..to)?;
        if digits.bytes().all(|b| b.is_ascii_digit()) { digits.parse().ok() } else { None }
    };
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => 28 + i64::from(leap),
        _ => return None,
    };
    if !(1..=month_days).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    // Days from 1970-01-01 to the civil date (the proleptic Gregorian calendar).
    let shifted_year = if month <= 2 { year - 1 } else { year };
    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// The age of a thing created at `created` as `45s`, `7m`, `3h` or `12d`; a creation time in the future is `0s`.
fn age(created: &Member<String>, now: i64) -> String {
    match created.is().and_then(|text| parse_timestamp(text)) {
        Some(created) => {
            let seconds = now.saturating_sub(created).max(0);
            match seconds {
                0..=59 => format!("{seconds}s"),
                60..=3599 => format!("{}m", seconds / 60),
                3600..=86_399 => format!("{}h", seconds / 3600),
                _ => format!("{}d", seconds / 86_400),
            }
        }
        None => "?".to_owned(),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Lists of namespaces and pods.
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NamespaceList {
    #[serde(default)]
    items: Seq<NamespaceItem, LIST_KEPT>,
}

#[derive(Deserialize)]
struct NamespaceItem {
    #[serde(default)]
    metadata: Member<ObjectMeta>,
    #[serde(default)]
    status: Member<NamespaceStatus>,
}

#[derive(Deserialize)]
struct NamespaceStatus {
    #[serde(default)]
    phase: Member<String>,
}

/// The members of `metadata` that a table shows. Labels and annotations have no field, so they are skipped unread.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObjectMeta {
    #[serde(default)]
    name: Member<String>,
    #[serde(default)]
    creation_timestamp: Member<String>,
    #[serde(default)]
    deletion_timestamp: Member<String>,
}

#[derive(Deserialize)]
struct PodList {
    #[serde(default)]
    items: Seq<PodItem, LIST_KEPT>,
}

#[derive(Deserialize)]
struct PodItem {
    #[serde(default)]
    metadata: Member<ObjectMeta>,
    #[serde(default)]
    spec: Member<PodSpec>,
    #[serde(default)]
    status: Member<PodStatus>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PodSpec {
    #[serde(default)]
    node_name: Member<String>,
    #[serde(default)]
    containers: Seq<ContainerSpec, INNER_KEPT>,
}

#[derive(Deserialize)]
struct ContainerSpec {
    #[serde(default)]
    name: Member<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PodStatus {
    #[serde(default)]
    phase: Member<String>,
    #[serde(default)]
    container_statuses: Seq<ContainerStatus, INNER_KEPT>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContainerStatus {
    #[serde(default)]
    ready: Member<bool>,
    #[serde(default)]
    restart_count: Member<u64>,
    #[serde(default)]
    state: Member<ContainerState>,
}

#[derive(Deserialize)]
struct ContainerState {
    #[serde(default)]
    waiting: Member<StateReason>,
    #[serde(default)]
    terminated: Member<StateReason>,
    #[serde(default)]
    running: Member<Anything>,
}

#[derive(Deserialize)]
struct StateReason {
    #[serde(default)]
    reason: Member<String>,
}

/// An object whose members are all skipped: its presence is what is read.
#[derive(Deserialize)]
struct Anything {}

object_members!(
    NamespaceList,
    NamespaceItem,
    NamespaceStatus,
    ObjectMeta,
    PodList,
    PodItem,
    PodSpec,
    ContainerSpec,
    PodStatus,
    ContainerStatus,
    ContainerState,
    StateReason,
    Anything,
);

/// The items of a list member as rows to show: a `null` list is empty, an array is its items, and anything else (a
/// member that is missing or is no array) is no list at all.
fn items_of<T, const CAP: usize>(list: &Seq<T, CAP>) -> Option<&[T]> {
    match list.shape {
        Shape::Null => Some(&[]),
        Shape::List => Some(&list.items),
        Shape::Missing | Shape::Wrong => None,
    }
}

/// The sentence under a table about the items that were not shown as rows.
fn list_note<T, const CAP: usize>(list: &Seq<T, CAP>) -> Option<String> {
    let mut parts = Vec::new();
    if list.unreadable > 0 {
        parts.push(format!("{} entries could not be read.", list.unreadable));
    }
    let kept_or_unreadable = list.items.len() + list.unreadable;
    if list.total > kept_or_unreadable {
        parts.push(format!("{} more entries were not read.", list.total - kept_or_unreadable));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn with_list_note<T, const CAP: usize>(model: ViewModel, list: &Seq<T, CAP>) -> ViewModel {
    match list_note(list) {
        Some(note) => model.with_note(&note),
        None => model,
    }
}

fn text_of(member: &Member<String>) -> &str {
    member.is().map_or("?", String::as_str)
}

fn namespaces_view(stdout: &[u8], now: i64) -> Result<ViewModel, ParseError> {
    let list: NamespaceList = match read_document(stdout) {
        Ok(list) => list,
        Err(ParseError::Unrecognised) => return Ok(text_fallback("Namespaces", stdout)),
        Err(other) => return Err(other),
    };
    let Some(items) = items_of(&list.items) else { return Ok(text_fallback("Namespaces", stdout)) };
    let mut table = TableBuilder::new("Namespaces");
    table.column("name", "NAME").column("status", "STATUS").column("age", "AGE");
    let set_namespace = own_verb("set_namespace");
    for item in items {
        let meta = item.metadata.is();
        let name = meta.and_then(|meta| meta.name.is());
        let phase = item.status.is().map_or("?", |status| text_of(&status.phase));
        let created = meta.map_or_else(|| "?".to_owned(), |meta| age(&meta.creation_timestamp, now));
        let shown = name.map_or("?", String::as_str);
        let mut row = RowBuilder::new(shown, &[shown, phase, &created]);
        row.tone(if phase == "Terminating" { Tone::Warn } else { Tone::Normal });
        if let (Some(name), Some(verb)) = (name, set_namespace) {
            row.action(verb, &[("namespace", name)]);
        }
        table.push(row);
    }
    Ok(with_list_note(table.finish(), &list.items))
}

/// The statuses of the containers as a slice when every one was read: a pod that has none yet (a `null` or missing
/// member) has an empty slice, and a status that is no object, a list that was cut or one that holds an unreadable
/// entry has none.
fn container_statuses(status: &Member<PodStatus>) -> Option<&[ContainerStatus]> {
    let status = match status {
        Member::Missing => return Some(&[]),
        Member::Wrong => return None,
        Member::Is(status) => status,
    };
    match status.container_statuses.shape {
        Shape::Missing | Shape::Null => Some(&[]),
        Shape::List if status.container_statuses.complete() => Some(&status.container_statuses.items),
        Shape::List | Shape::Wrong => None,
    }
}

/// `ready/total`, or `?` when either number is not known. The total is the number of containers in the spec, else the
/// number of statuses when the spec lists none.
fn ready_cell(spec: &Member<PodSpec>, status: &Member<PodStatus>) -> String {
    let statuses = container_statuses(status);
    let total = match spec {
        Member::Wrong => None,
        Member::Is(spec) => match spec.containers.shape {
            Shape::List if spec.containers.complete() => Some(spec.containers.items.len()),
            Shape::Missing | Shape::Null => statuses.map(<[ContainerStatus]>::len),
            Shape::List | Shape::Wrong => None,
        },
        Member::Missing => statuses.map(<[ContainerStatus]>::len),
    };
    let ready = statuses.and_then(|statuses| {
        statuses.iter().try_fold(0_usize, |ready, status| match &status.ready {
            Member::Is(true) => Some(ready + 1),
            Member::Is(false) | Member::Missing => Some(ready),
            Member::Wrong => None,
        })
    });
    match (ready, total) {
        (Some(ready), Some(total)) => format!("{ready}/{total}"),
        _ => "?".to_owned(),
    }
}

/// The restarts of all containers, or `?`.
fn restarts_cell(status: &Member<PodStatus>) -> String {
    let total = container_statuses(status).and_then(|statuses| {
        statuses.iter().try_fold(0_u64, |sum, status| match &status.restart_count {
            Member::Is(count) => Some(sum.saturating_add(*count)),
            Member::Missing => Some(sum),
            Member::Wrong => None,
        })
    });
    total.map_or_else(|| "?".to_owned(), |total| total.to_string())
}

fn reason_of(state: &Member<StateReason>) -> Option<&str> {
    state.is().and_then(|state| state.reason.is()).map(String::as_str).filter(|reason| !reason.is_empty())
}

/// What a person expects to read as the status of a pod, as `kubectl` shows it: the waiting or terminated reason of
/// the first container that has one (a `Completed` container beside a running one is not a completed pod), else
/// `Terminating` when the pod is being deleted, else the phase.
fn pod_status(meta: Option<&ObjectMeta>, status: Option<&PodStatus>) -> Option<String> {
    let containers: &[ContainerStatus] = status.map_or(&[][..], |status| &status.container_statuses.items);
    let reason = containers.iter().find_map(|container| {
        let state = container.state.is()?;
        reason_of(&state.waiting).or_else(|| reason_of(&state.terminated))
    });
    if let Some(reason) = reason {
        let running = containers
            .iter()
            .any(|container| container.state.is().is_some_and(|state| state.running.is().is_some()));
        return Some(if reason == "Completed" && running { "Running" } else { reason }.to_owned());
    }
    if meta.is_some_and(|meta| meta.deletion_timestamp.is().is_some_and(|stamp| !stamp.is_empty())) {
        return Some("Terminating".to_owned());
    }
    status?.phase.is().filter(|phase| !phase.is_empty()).cloned()
}

fn pod_tone(status: &str) -> Tone {
    match status {
        "Pending" | "ContainerCreating" | "PodInitializing" | "Terminating" | "Unknown" | "?" => Tone::Warn,
        "CrashLoopBackOff" | "Error" | "OOMKilled" | "ImagePullBackOff" | "ErrImagePull" | "Failed"
        | "CreateContainerConfigError" | "CreateContainerError" | "InvalidImageName" | "RunContainerError"
        | "DeadlineExceeded" | "Evicted" | "ContainerCannotRun" => Tone::Danger,
        _ => Tone::Normal,
    }
}

/// The node of a pod: empty while it is not scheduled, `?` when the member is no text.
fn node_cell(spec: &Member<PodSpec>) -> &str {
    match spec {
        Member::Missing => "",
        Member::Wrong => "?",
        Member::Is(spec) => match &spec.node_name {
            Member::Missing => "",
            Member::Is(node) => node,
            Member::Wrong => "?",
        },
    }
}

fn pods_view(stdout: &[u8], now: i64) -> Result<ViewModel, ParseError> {
    let list: PodList = match read_document(stdout) {
        Ok(list) => list,
        Err(ParseError::Unrecognised) => return Ok(text_fallback("Pods", stdout)),
        Err(other) => return Err(other),
    };
    let Some(items) = items_of(&list.items) else { return Ok(text_fallback("Pods", stdout)) };
    let mut table = TableBuilder::new("Pods");
    table.column("name", "NAME").column("ready", "READY").column("status", "STATUS");
    table.column("restarts", "RESTARTS").column("age", "AGE").column("node", "NODE");
    let (exec, logs, delete) = (own_verb("exec"), own_verb("logs"), own_verb("delete_pod"));
    for item in items {
        let (meta, spec, status) = (item.metadata.is(), item.spec.is(), item.status.is());
        let name = meta.and_then(|meta| meta.name.is());
        let shown = name.map_or("?", String::as_str);
        let state = pod_status(meta, status).unwrap_or_else(|| "?".to_owned());
        let created = meta.map_or_else(|| "?".to_owned(), |meta| age(&meta.creation_timestamp, now));
        let ready = ready_cell(&item.spec, &item.status);
        let restarts = restarts_cell(&item.status);
        let cells = [shown, &ready, &state, &restarts, &created, node_cell(&item.spec)];
        let mut row = RowBuilder::new(shown, &cells);
        row.tone(pod_tone(&state));
        if let Some(pod) = name {
            // The first container travels with Exec and Logs; a pod that names none gets its default container.
            let first = spec.and_then(|spec| spec.containers.items.first()).and_then(|container| container.name.is());
            let mut with_container = vec![("pod", pod.as_str())];
            if let Some(container) = first {
                with_container.push(("container", container.as_str()));
            }
            for verb in [exec, logs].into_iter().flatten() {
                row.action(verb, &with_container);
            }
            if let Some(verb) = delete {
                row.action(verb, &[("pod", pod.as_str())]);
            }
        }
        table.push(row);
    }
    Ok(with_list_note(table.finish(), &list.items))
}

// ---------------------------------------------------------------------------------------------------------------------
// The overlay kubeconfig of a shell pane.
// ---------------------------------------------------------------------------------------------------------------------

/// The most entries of the person's own `KUBECONFIG` list that follow the overlay.
const LIST_ENTRIES_MAX: usize = 64;

/// The longest entry of the person's list that is kept.
const LIST_ENTRY_MAX: usize = 4096;

/// Why an overlay, a `KUBECONFIG` value or a sweep was refused. No variant holds a name or a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayError {
    /// A name of the entry did not validate (see [`ArgError`]).
    Name(ArgError),
    /// The overlay's path is not an absolute path of the system, or holds the list separator or a control character.
    Path,
    /// A directory of the overlays is a link or is not a directory, so nothing is written or removed there.
    Unsafe,
    /// The file system refused.
    Io(io::ErrorKind),
}

impl fmt::Display for OverlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(error) => write!(f, "a name of the context is refused: {}", error.as_str()),
            Self::Path => f.write_str("the overlay path is not an absolute path that can be listed"),
            Self::Unsafe => f.write_str("a directory of the overlays is a link or not a directory"),
            Self::Io(kind) => write!(f, "the overlay files could not be used: {kind}"),
        }
    }
}

impl std::error::Error for OverlayError {}

impl From<ArgError> for OverlayError {
    fn from(error: ArgError) -> Self {
        Self::Name(error)
    }
}

impl From<io::Error> for OverlayError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

/// The directory of the overlays below the data directory `dir`.
fn overlay_dir(dir: &Path) -> PathBuf {
    dir.join("run").join("kube")
}

/// Where the overlay of `pane` lives: `<dir>/run/kube/<pane>.json`.
pub fn overlay_path(dir: &Path, pane: PaneId) -> PathBuf {
    overlay_dir(dir).join(format!("{pane}.json"))
}

/// Makes `path` a directory for its owner only. A link, or anything that is not a directory, is refused: nothing is
/// written through it.
fn private_dir(path: &Path) -> Result<(), OverlayError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(OverlayError::Unsafe),
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_private(path)?,
        Err(error) => return Err(error.into()),
    }
    narrow(path)
}

#[cfg(unix)]
fn create_private(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

/// Sets the mode of the directory to 0700 whatever the umask made of it, after checking again that it is no link.
#[cfg(unix)]
fn narrow(path: &Path) -> Result<(), OverlayError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        return Err(OverlayError::Unsafe);
    }
    if meta.permissions().mode() & 0o7777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The profile of a user on Windows decides who can read a directory below it; there is no mode to set.
#[cfg(not(unix))]
fn narrow(path: &Path) -> Result<(), OverlayError> {
    if fs::symlink_metadata(path)?.is_dir() { Ok(()) } else { Err(OverlayError::Unsafe) }
}

/// Writes the overlay of `pane` below the data directory `dir` and returns its path. The file is JSON (which `kubectl`
/// reads like YAML) and holds names only: `current-context` and one context with its cluster, user and namespace.
/// Every name is checked first, so a name that could be read as an option or holds a hidden character writes nothing.
/// The file is written atomically with mode 0600 in directories of mode 0700; a second call replaces it. A cluster or a
/// user the context does not name is left out, and so is a namespace it does not have.
pub fn write_overlay(dir: &Path, pane: PaneId, entry: &ContextEntry) -> Result<PathBuf, OverlayError> {
    let name = ContextName::parse(&entry.name)?;
    let cluster = if entry.cluster.is_empty() { None } else { Some(ContextName::parse(&entry.cluster)?) };
    let user = if entry.user.is_empty() { None } else { Some(ContextName::parse(&entry.user)?) };
    let namespace = entry.namespace.as_deref().map(Namespace::parse).transpose()?;

    let mut body = serde_json::Map::new();
    if let Some(cluster) = &cluster {
        body.insert("cluster".to_owned(), cluster.as_str().into());
    }
    if let Some(user) = &user {
        body.insert("user".to_owned(), user.as_str().into());
    }
    if let Some(namespace) = &namespace {
        body.insert("namespace".to_owned(), namespace.as_str().into());
    }
    let document = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Config",
        "current-context": name.as_str(),
        "contexts": [{ "name": name.as_str(), "context": body }],
    });
    let mut bytes = serde_json::to_vec_pretty(&document).map_err(|_| OverlayError::Io(io::ErrorKind::InvalidData))?;
    bytes.push(b'\n');

    fs::create_dir_all(dir)?;
    private_dir(&dir.join("run"))?;
    private_dir(&overlay_dir(dir))?;
    let path = overlay_path(dir, pane);
    write_atomic_private(&path, &bytes)?;
    Ok(path)
}

/// Whether one entry of the person's own list may follow the overlay: an absolute path of the system, without a
/// control character and of a bounded length. On Windows that is a drive path, so a UNC path, a device path and a
/// drive-relative path are not.
fn listable(os: Os, entry: &str) -> bool {
    entry.len() <= LIST_ENTRY_MAX && platform::is_absolute_path(os, entry) && !entry.chars().any(char::is_control)
}

/// The value of `KUBECONFIG` for a pane: the overlay first, then the person's own files, joined with `:` (Unix) or `;`
/// (Windows). The person's files are `user_list` (their `KUBECONFIG`, when it is set and not empty) or else
/// `<home>/.kube/config`; a relative, empty, control-character, UNC or device entry is dropped, at most 64 entries are
/// kept, and a list with nothing left gives the default. An `overlay` that is not absolute for `os`, or holds the
/// separator or a control character, is refused, so the list can only grow by the entries checked here.
pub fn kubeconfig_value(overlay: &str, os: Os, user_list: Option<&str>, home: &str) -> Result<String, OverlayError> {
    let separator = if os == Os::Windows { ';' } else { ':' };
    if !listable(os, overlay) || overlay.contains(separator) {
        return Err(OverlayError::Path);
    }
    let mut files: Vec<String> = user_list
        .map(|list| list.split(separator).filter(|entry| listable(os, entry)).take(LIST_ENTRIES_MAX).map(str::to_owned).collect())
        .unwrap_or_default();
    if files.is_empty() && listable(os, home) && !home.contains(separator) {
        files.push(platform::join_path(os, &platform::join_path(os, home, ".kube"), "config"));
    }
    let mut value = overlay.to_owned();
    for file in &files {
        value.push(separator);
        value.push_str(file);
    }
    Ok(value)
}

/// The pane a file name stands for: `<number>.json` with the number written the way [`overlay_path`] writes it.
fn pane_of(file_name: &str) -> Option<PaneId> {
    let digits = file_name.strip_suffix(".json")?;
    let pane: PaneId = digits.parse().ok()?;
    (pane.to_string() == digits).then_some(pane)
}

/// Removes the overlay of every pane that is not in `live` and returns how many it removed. Only regular files named
/// `<number>.json` go: a directory, a link (which is never followed), a temporary file and anything else stay. A
/// directory that does not exist is nothing to sweep. A file that cannot be removed does not stop the sweep; the first
/// such error is returned when it ends.
pub fn sweep_overlays(dir: &Path, live: &[PaneId]) -> Result<usize, OverlayError> {
    let kube_dir = overlay_dir(dir);
    for directory in [dir.join("run"), kube_dir.clone()] {
        match fs::symlink_metadata(&directory) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(OverlayError::Unsafe),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.into()),
        }
    }
    let mut removed = 0;
    let mut failure = None;
    for entry in fs::read_dir(&kube_dir)? {
        let Ok(entry) = entry else { continue };
        let file_name = entry.file_name();
        let Some(pane) = file_name.to_str().and_then(pane_of) else { continue };
        if live.contains(&pane) {
            continue;
        }
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            _ => continue,
        }
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => failure = failure.or(Some(OverlayError::from(error))),
        }
    }
    failure.map_or(Ok(removed), Err)
}
