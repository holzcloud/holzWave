//! Native integrations: the person's own command-line tools (Kubernetes, Talos, Azure, GAM) as blocks and launches.
//!
//! The design in one paragraph: every tool is a compiled-in typed description (an [`IntegrationSpec`] with a closed
//! list of [`Verb`]s), run by one generic engine as an argument list and never through a shell. Every name that can
//! reach an argument (a context, a namespace, a pod, a node, a subscription, a section) comes from a cluster, an
//! account, a directory or a synced file, so it is another party's text: [`args`] turns it into a type that cannot be
//! built any other way than by validating it, and [`Validated`] and [`Selection`] hold only such types.
//!
//! Module map:
//!
//! - this file: the types every tool shares ([`ToolId`], [`VerbEffect`], [`Output`], [`ArgKind`], [`ArgSpec`],
//!   [`Verb`], [`IntegrationSpec`], [`Selection`], [`Validated`]).
//! - [`args`]: the validators for names, the equals-form flag builder and the fixed refusal sentences.
//! - [`detect`]: finding a tool on this computer: the override, the `PATH`, the login shell's `PATH`, fixed places.
//! - [`run`]: running a tool with a scrubbed environment, bounded output, a time limit and cancellation.
//! - [`view`]: the view model the output becomes and the cleaning of text that came from a cluster or an account.
//! - [`profiles`]: the synced profile file and the machine-local file.
//! - [`context`]: the selection per pane and its application to a run.
//! - [`ticket`]: prepare and confirm for verbs that change something.
//! - [`service`]: the service the app shell holds.
//! - [`kube`], [`talos`], [`azure`], [`gam`]: one module per tool, with its argument lists and parsers.

pub mod args;
pub mod azure;
pub mod context;
pub mod detect;
pub mod gam;
pub mod kube;
pub mod profiles;
pub mod run;
pub mod service;
pub mod talos;
pub mod ticket;
pub mod view;

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

use args::{
    flag_value, lines, ArgError, ContainerName, ContextName, Namespace, NodeName, PodName, ProfileId, SectionName,
    SubscriptionId,
};

/// The most bytes any one argument value may have, checked before its kind is looked at.
const VALUE_MAX: usize = 4096;

/// The four tools. The wire names are the lower-case ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolId {
    /// Kubernetes, through `kubectl`.
    Kube,
    /// Talos Linux, through `talosctl`.
    Talos,
    /// The Azure command-line tool.
    Azure,
    /// GAM7 for Google Workspace.
    Gam,
}

impl ToolId {
    /// Every tool, in the order the person sees them.
    pub const ALL: [ToolId; 4] = [ToolId::Kube, ToolId::Talos, ToolId::Azure, ToolId::Gam];

    /// The lower-case name: `kube`, `talos`, `azure`, `gam`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kube => "kube",
            Self::Talos => "talos",
            Self::Azure => "azure",
            Self::Gam => "gam",
        }
    }

    /// The tool with exactly this name; another word, another case and any padding are refused.
    pub fn parse(name: &str) -> Result<Self, ArgError> {
        Self::ALL.into_iter().find(|tool| tool.as_str() == name).ok_or(ArgError::Unknown)
    }

    /// The selection fields this tool takes besides the profile. [`Selection::check`] refuses every other field.
    pub fn selects(self) -> &'static [SelectKind] {
        match self {
            Self::Kube => &[SelectKind::Context, SelectKind::Namespace],
            Self::Talos => &[SelectKind::Context, SelectKind::Node],
            Self::Azure => &[SelectKind::Subscription],
            Self::Gam => &[SelectKind::Section],
        }
    }
}

/// What a verb does to the world, which decides whether the person sees a card first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerbEffect {
    /// Reads and changes nothing: it only shows what the tool reports.
    Read,
    /// Changes the tool's own configuration file on this computer, such as the current context.
    Local,
    /// Changes a cluster or an account: the person has to confirm it, and twice in a production context.
    Write,
    /// Opens a session with the person's credentials, such as a shell in a pod or a sign-in.
    Exec,
}

/// Where the result of a verb goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    /// A table, a key-value list or a text in an integration block.
    View,
    /// A terminal block that runs the tool interactively.
    Terminal,
    /// Nothing to show: the verb is done when the tool exits.
    Done,
}

/// The kind of one argument of a verb: the validator its value goes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgKind {
    /// A [`ContextName`].
    Context,
    /// A [`Namespace`].
    Namespace,
    /// A [`PodName`].
    Pod,
    /// A [`ContainerName`].
    Container,
    /// A [`NodeName`].
    Node,
    /// A [`SubscriptionId`].
    Subscription,
    /// A [`SectionName`].
    Section,
    /// A line count from 1 to 5000.
    Lines,
    /// `true` or `false`.
    Flag,
    /// Exactly one of the listed words.
    Choice(&'static [&'static str]),
}

/// One argument a verb declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ArgSpec {
    /// The name the page sends it under.
    pub name: &'static str,
    /// How its value is validated.
    pub kind: ArgKind,
    /// Whether a call without it is refused.
    pub required: bool,
}

/// One verb of a tool: the closed catalogue holds no other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Verb {
    /// The id the page names it by.
    pub id: &'static str,
    /// The label the person reads.
    pub label: &'static str,
    /// What it does to the world.
    pub effect: VerbEffect,
    /// Where its result goes.
    pub output: Output,
    /// The arguments it declares; a call with any other is refused.
    pub args: &'static [ArgSpec],
    /// How often a shown view refreshes by itself, or `None` for on request only.
    pub refresh_secs: Option<u32>,
    /// A non-zero exit with an empty standard output is a normal state that the parser reports, as for the account
    /// command of a signed-out Azure tool, and not a failure.
    pub exit_is_state: bool,
}

/// What the person's context of a tool can be set to; the selection fields of a tool are a subset of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectKind {
    /// A context of a kubeconfig or talosconfig.
    Context,
    /// A Kubernetes namespace.
    Namespace,
    /// An Azure subscription.
    Subscription,
    /// A section of a GAM configuration.
    Section,
    /// A Talos node.
    Node,
}

/// How to ask a tool for its version and read the answer.
#[derive(Debug, Clone, Copy)]
pub struct VersionProbe {
    /// The arguments after the program.
    pub argv: &'static [&'static str],
    /// The version in the tool's answer, or `None` when the answer holds none.
    pub parse: fn(&str) -> Option<String>,
}

/// The compiled-in description of one tool.
#[derive(Debug, Clone, Copy)]
pub struct IntegrationSpec {
    /// Which tool it is.
    pub id: ToolId,
    /// The name the person reads.
    pub label: &'static str,
    /// The program names searched on macOS and Linux.
    pub binaries: &'static [&'static str],
    /// The file names searched on Windows.
    pub windows_binaries: &'static [&'static str],
    /// Directories searched after the `PATH`, with `~` and variables spelled the way the detector reads them.
    pub fixed_dirs: &'static [&'static str],
    /// How the version is read.
    pub version_probe: VersionProbe,
    /// Environment variables set for every run of this tool.
    pub env_set: &'static [(&'static str, &'static str)],
    /// The selection fields the tool takes.
    pub selects: &'static [SelectKind],
    /// The verbs, in the order they are listed.
    pub verbs: &'static [Verb],
}

impl IntegrationSpec {
    /// The verb with exactly this id, or `None`: an id the spec does not list is never run.
    pub fn verb(&self, id: &str) -> Option<&'static Verb> {
        let verbs: &'static [Verb] = self.verbs;
        verbs.iter().find(|verb| verb.id == id)
    }
}

/// What a pane has selected, as the page and the files hold it: plain text, not yet validated. It is read from a JSON
/// object only: a member that is not a field, a repeated member and the positional array form that a derived reader
/// would also take are all refused, and [`Selection::check`] validates each value by its kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Selection {
    /// The id of the integration profile.
    pub profile: Option<String>,
    /// A kubeconfig or talosconfig context.
    pub context: Option<String>,
    /// A Kubernetes namespace.
    pub namespace: Option<String>,
    /// An Azure subscription id.
    pub subscription: Option<String>,
    /// A GAM section.
    pub section: Option<String>,
    /// A Talos node.
    pub node: Option<String>,
}

impl<'de> Deserialize<'de> for Selection {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Members;

        impl<'de> Visitor<'de> for Members {
            type Value = Selection;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object with the members of a selection")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Selection, A::Error> {
                let mut out = Selection::default();
                let mut seen = [false; 6];
                while let Some(key) = map.next_key::<String>()? {
                    let (at, slot) = match key.as_str() {
                        "profile" => (0, &mut out.profile),
                        "context" => (1, &mut out.context),
                        "namespace" => (2, &mut out.namespace),
                        "subscription" => (3, &mut out.subscription),
                        "section" => (4, &mut out.section),
                        "node" => (5, &mut out.node),
                        // The message is fixed: the member name is another party's text.
                        _ => return Err(de::Error::custom("a selection holds a member that is not a field")),
                    };
                    if std::mem::replace(&mut seen[at], true) {
                        return Err(de::Error::custom("a selection holds a member twice"));
                    }
                    *slot = map.next_value()?;
                }
                Ok(out)
            }
        }

        deserializer.deserialize_map(Members)
    }
}

/// A [`Selection`] after its values went through their validators.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckedSelection {
    /// The profile.
    pub profile: Option<ProfileId>,
    /// The context.
    pub context: Option<ContextName>,
    /// The namespace.
    pub namespace: Option<Namespace>,
    /// The subscription.
    pub subscription: Option<SubscriptionId>,
    /// The section.
    pub section: Option<SectionName>,
    /// The node.
    pub node: Option<NodeName>,
}

impl Selection {
    /// Validates every field that is set. A field the tool does not select is refused before its value is looked at;
    /// every tool takes a profile.
    pub fn check(&self, tool: ToolId) -> Result<CheckedSelection, ArgError> {
        let takes = |kind: SelectKind| tool.selects().contains(&kind);
        let allowed = |field: &Option<String>, kind: SelectKind| match field {
            Some(_) if !takes(kind) => Err(ArgError::NotForTool),
            _ => Ok(()),
        };
        allowed(&self.context, SelectKind::Context)?;
        allowed(&self.namespace, SelectKind::Namespace)?;
        allowed(&self.subscription, SelectKind::Subscription)?;
        allowed(&self.section, SelectKind::Section)?;
        allowed(&self.node, SelectKind::Node)?;
        Ok(CheckedSelection {
            profile: self.profile.as_deref().map(ProfileId::parse).transpose()?,
            context: self.context.as_deref().map(ContextName::parse).transpose()?,
            namespace: self.namespace.as_deref().map(Namespace::parse).transpose()?,
            subscription: self.subscription.as_deref().map(SubscriptionId::parse).transpose()?,
            section: self.section.as_deref().map(SectionName::parse).transpose()?,
            node: self.node.as_deref().map(NodeName::parse).transpose()?,
        })
    }
}

/// One validated argument value. A choice keeps the listed word, not the text it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Context(ContextName),
    Namespace(Namespace),
    Pod(PodName),
    Container(ContainerName),
    Node(NodeName),
    Subscription(SubscriptionId),
    Section(SectionName),
    Lines(u16),
    Flag(bool),
    Choice(&'static str),
}

impl Value {
    fn parse(kind: ArgKind, text: &str) -> Result<Self, ArgError> {
        Ok(match kind {
            ArgKind::Context => Self::Context(ContextName::parse(text)?),
            ArgKind::Namespace => Self::Namespace(Namespace::parse(text)?),
            ArgKind::Pod => Self::Pod(PodName::parse(text)?),
            ArgKind::Container => Self::Container(ContainerName::parse(text)?),
            ArgKind::Node => Self::Node(NodeName::parse(text)?),
            ArgKind::Subscription => Self::Subscription(SubscriptionId::parse(text)?),
            ArgKind::Section => Self::Section(SectionName::parse(text)?),
            ArgKind::Lines => Self::Lines(lines(text)?),
            ArgKind::Flag => Self::Flag(flag_value(text)?),
            ArgKind::Choice(words) => Self::Choice(words.iter().find(|word| **word == text).ok_or(ArgError::Shape)?),
        })
    }
}

/// The argument bag of one verb call after it went through the verb's declaration: only declared arguments, every
/// required one present, every value validated by its kind. It holds validated types and no loose text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validated {
    values: Vec<(&'static str, Value)>,
}

impl Validated {
    /// Checks `pairs` (argument name to text) against `verb`: an argument the verb does not declare is `Unknown`, a
    /// required one that is absent is `Missing`, a value over 4096 bytes is `TooLong` before its kind is looked at, and
    /// a value that fails its kind is refused with that kind's reason.
    pub fn parse(verb: &Verb, pairs: &BTreeMap<String, String>) -> Result<Self, ArgError> {
        if pairs.keys().any(|name| !verb.args.iter().any(|spec| spec.name == name.as_str())) {
            return Err(ArgError::Unknown);
        }
        let mut values = Vec::with_capacity(verb.args.len());
        for spec in verb.args {
            match pairs.get(spec.name) {
                None if spec.required => return Err(ArgError::Missing),
                None => {}
                Some(text) if text.len() > VALUE_MAX => return Err(ArgError::TooLong),
                Some(text) => values.push((spec.name, Value::parse(spec.kind, text)?)),
            }
        }
        Ok(Self { values })
    }

    fn first<'a, T>(&'a self, pick: impl Fn(&'a Value) -> Option<T>) -> Option<T> {
        self.values.iter().find_map(|(_, value)| pick(value))
    }

    fn named(&self, name: &str) -> Option<&Value> {
        self.values.iter().find(|(declared, _)| *declared == name).map(|(_, value)| value)
    }

    /// The pod, when the verb declared one and it was given.
    pub fn pod(&self) -> Option<&PodName> {
        self.first(|v| if let Value::Pod(x) = v { Some(x) } else { None })
    }

    /// The container.
    pub fn container(&self) -> Option<&ContainerName> {
        self.first(|v| if let Value::Container(x) = v { Some(x) } else { None })
    }

    /// The line count.
    pub fn lines(&self) -> Option<u16> {
        self.first(|v| if let Value::Lines(x) = v { Some(*x) } else { None })
    }

    /// The flag with this argument name.
    pub fn flag(&self, name: &str) -> Option<bool> {
        if let Some(Value::Flag(x)) = self.named(name) { Some(*x) } else { None }
    }

    /// The listed word chosen for the argument with this name.
    pub fn choice(&self, name: &str) -> Option<&'static str> {
        if let Some(Value::Choice(x)) = self.named(name) { Some(*x) } else { None }
    }

    /// The context.
    pub fn context(&self) -> Option<&ContextName> {
        self.first(|v| if let Value::Context(x) = v { Some(x) } else { None })
    }

    /// The namespace.
    pub fn namespace(&self) -> Option<&Namespace> {
        self.first(|v| if let Value::Namespace(x) = v { Some(x) } else { None })
    }

    /// The node.
    pub fn node(&self) -> Option<&NodeName> {
        self.first(|v| if let Value::Node(x) = v { Some(x) } else { None })
    }

    /// The subscription.
    pub fn subscription(&self) -> Option<&SubscriptionId> {
        self.first(|v| if let Value::Subscription(x) = v { Some(x) } else { None })
    }

    /// The section.
    pub fn section(&self) -> Option<&SectionName> {
        self.first(|v| if let Value::Section(x) = v { Some(x) } else { None })
    }
}
