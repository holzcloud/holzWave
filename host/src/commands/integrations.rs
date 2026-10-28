//! The plain functions of the commands of the external tools (`kubectl`, `az` and the others). The page sends facts (a
//! tool, a verb, a pane, names, a selection) and the core builds everything else: no command takes a program, an argument
//! list or a shell string. A refusal is the object `{code, reason, text}` ([`IntegrationFault`]). Every tool runs as the
//! person, in the person's own home, exactly as a shell in a pane would.

use holzwave_core::integrate::service::ToolView;

use crate::state::AppCore;

/// The four tools and where each stands on this computer: found (with its path and version), not found (with the places
/// that were looked in), unusable or unsupported in this build, and the verbs each has. It looks for the programs and asks
/// a found one for its version.
pub fn integration_tools(core: &AppCore) -> Vec<ToolView> {
    core.integration_tools()
}
