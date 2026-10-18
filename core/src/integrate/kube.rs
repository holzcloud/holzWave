//! Kubernetes through `kubectl`: the argument lists of its verbs and the parsers of its answers.
//!
//! `kubectl` is run as a program by the engine of this module's neighbours; no Kubernetes client is written here, so
//! its auth plugins, proxies and the exec transport stay `kubectl`'s own work. This file holds only argument builders
//! and parsers: it starts no process, reads no file and keeps no state. Every argument list is built from validated
//! names ([`super::args`]), a name that follows an option travels in the equals form, and every other name follows `--`.
//!
//! What `kubectl` prints is another party's text. Each answer is read by a typed reader that keeps only the members a
//! table shows. A member that is not there or has another type is a `?` cell and not an error, an unknown member is
//! skipped without being kept, and a struct is read from a JSON object only (a derived reader would also take an array
//! of the member values in order). The credential members of a kubeconfig have no field in any type here, and a
//! kubeconfig that cannot be read is never shown as text, because `kubectl` leaves the environment values of an exec
//! plugin in clear in `config view`.

use std::ffi::OsString;
use std::fmt;
use std::marker::PhantomData;

use serde::de::value::MapAccessDeserializer;
use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

use super::args::ArgError;
use super::run::MAX_STDOUT;
use super::view::{text_view, RowBuilder, TableBuilder, TextFormat, ViewModel};
use super::{
    ArgKind, ArgSpec, CheckedSelection, Integration, IntegrationSpec, Output, ParseError, SelectKind, ToolId, Validated,
    Verb, VerbEffect, VersionProbe,
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
    let valid = !version.is_empty() && version.chars().count() <= VERSION_MAX && version.chars().all(|c| c.is_ascii_graphic());
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
        let cells = [if current { "*" } else { "" }, &entry.name, &entry.cluster, &entry.user, entry.namespace().unwrap_or("")];
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

impl Integration for Kube {
    fn spec(&self) -> &'static IntegrationSpec {
        &SPEC
    }

    fn argv(&self, verb: &Verb, _args: &Validated, _selection: &CheckedSelection) -> Result<Vec<OsString>, ArgError> {
        let verb = own(verb)?;
        match verb.id {
            "contexts" => Ok(["config", "view", "-o", "json"].map(OsString::from).to_vec()),
            _ => Err(ArgError::Unknown),
        }
    }

    fn view(&self, verb: &Verb, stdout: &[u8]) -> Result<ViewModel, ParseError> {
        let verb = own(verb).map_err(|_| ParseError::Unrecognised)?;
        match verb.id {
            "contexts" => match parse_contexts(stdout) {
                Ok(data) => Ok(contexts_view(&data)),
                Err(ParseError::Unrecognised) => Ok(unreadable_config()),
                Err(other) => Err(other),
            },
            _ => Err(ParseError::Unrecognised),
        }
    }
}
