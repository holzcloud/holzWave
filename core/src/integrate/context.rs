//! The selection of a context, namespace, subscription, section or node for a pane, and how it is applied to a run.
//!
//! There are three tiers of context, and this module is the first two:
//!
//! 1. **Flags per command.** Every command carries its own selection in its arguments (`--context=...`,
//!    `--namespace=...`, `--subscription=...`, `select <section>`), so two panes can talk to two clusters at the same
//!    time and a selection changes nothing on disk. [`resolve`] turns what a pane selected, plus the pins of a profile,
//!    into the validated selection the argument builders take.
//! 2. **An overlay per pane.** A shell pane cannot carry flags, so it gets a small credential-free file that names the
//!    current context and nothing else, and a variable that lists it first ([`apply_env`]).
//! 3. **An explicit Local verb** changes the tool's own configuration, behind a card. No function here calls one, and
//!    none writes below a tool's own directory.
//!
//! No process is started and no tool is asked in this module: the defaults of a tool (its own current selection) are
//! stored by the service when it has read them, and this module is state, resolution and application. A profile's
//! secret values never pass through it; it reports the names only.

use std::fmt;

use super::args::{ArgError, ProfileId};
use super::profiles::{IntegrationProfile, IntegrationProfiles};
use super::{CheckedSelection, SelectKind, Selection, ToolId};

/// Why a selection could not be resolved. Each variant is a fixed sentence that holds no byte of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextError {
    /// The selection names a profile that does not exist.
    UnknownProfile,
    /// The profile belongs to another tool.
    WrongTool,
    /// A field of the selection was refused by its validator, or is not a field of the tool.
    Selection(ArgError),
}

impl ContextError {
    /// The fixed sentence of the refusal.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownProfile => "the profile does not exist",
            Self::WrongTool => "the profile belongs to another tool",
            Self::Selection(error) => error.as_str(),
        }
    }
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for ContextError {}

impl From<ArgError> for ContextError {
    fn from(error: ArgError) -> Self {
        Self::Selection(error)
    }
}

/// A selection turned into what a run needs.
#[derive(Clone)]
pub struct Resolved {
    /// The tool.
    pub tool: ToolId,
    /// The selection after the pins of the profile filled the fields it left empty. An explicit field wins.
    pub selection: CheckedSelection,
    /// The profile the selection named, if any.
    pub profile: Option<ProfileId>,
    /// The display name of the profile that makes the selected context production, or `None` when it is not.
    pub production: Option<String>,
    /// The plain environment of the profile, in name order. Values are the person's own and not secret.
    pub env_plain: Vec<(String, String)>,
    /// The names, never the values, of the profile's secret variables. The service resolves the values for a run.
    pub secret_names: Vec<String>,
    /// Whether the profile asks for a configuration directory of its own (Azure and GAM).
    pub isolated: bool,
}

impl fmt::Debug for Resolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolved")
            .field("tool", &self.tool)
            .field("selection", &self.selection)
            .field("profile", &self.profile.as_ref().map(ProfileId::as_str))
            .field("production", &self.production.is_some())
            .field("env_plain", &self.env_plain.len())
            .field("secret_names", &self.secret_names.len())
            .field("isolated", &self.isolated)
            .finish()
    }
}

/// The selection fields of `profile` that `tool` takes, as a selection of their own.
fn pins(tool: ToolId, profile: &IntegrationProfile) -> CheckedSelection {
    let pinned = profile.selection();
    let takes = |kind: SelectKind| tool.selects().contains(&kind);
    CheckedSelection {
        profile: pinned.profile,
        context: pinned.context.filter(|_| takes(SelectKind::Context)),
        namespace: pinned.namespace.filter(|_| takes(SelectKind::Namespace)),
        subscription: pinned.subscription.filter(|_| takes(SelectKind::Subscription)),
        section: pinned.section.filter(|_| takes(SelectKind::Section)),
        node: pinned.node.filter(|_| takes(SelectKind::Node)),
    }
}

/// Resolves `selection` for `tool` against `profiles`.
///
/// Every field that is set is validated first (a field the tool does not select is refused). When the selection names
/// a profile, that profile must exist and be one of this tool's, and the fields the selection leaves empty are filled
/// from what the profile pins; a field the selection sets itself always wins, because the person's click is the more
/// recent intention. Nothing is written and no process is started.
///
/// # Errors
///
/// A fixed sentence: a field that failed its validator, a profile that does not exist, or a profile of another tool.
pub fn resolve(profiles: &IntegrationProfiles, tool: ToolId, selection: &Selection) -> Result<Resolved, ContextError> {
    let mut checked = selection.check(tool)?;
    let mut out = Resolved {
        tool,
        selection: CheckedSelection::default(),
        profile: None,
        production: None,
        env_plain: Vec::new(),
        secret_names: Vec::new(),
        isolated: false,
    };
    if let Some(id) = checked.profile.clone() {
        let profile = profiles.get(id.as_str()).ok_or(ContextError::UnknownProfile)?;
        if profile.tool != tool {
            return Err(ContextError::WrongTool);
        }
        let pinned = pins(tool, profile);
        checked.context = checked.context.or(pinned.context);
        checked.namespace = checked.namespace.or(pinned.namespace);
        checked.subscription = checked.subscription.or(pinned.subscription);
        checked.section = checked.section.or(pinned.section);
        checked.node = checked.node.or(pinned.node);
        out.profile = Some(id);
        out.env_plain = profile.env.iter().map(|(name, value)| (name.clone(), value.clone())).collect();
        out.secret_names = profile.secret_env.clone();
        out.isolated = profile.isolated;
    }
    out.selection = checked;
    Ok(out)
}
