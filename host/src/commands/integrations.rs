//! The plain functions of the commands of the external tools (`kubectl`, `az` and the others). The page sends facts (a
//! tool, a verb, a pane, names, a selection) and the core builds everything else: no command takes a program, an argument
//! list or a shell string. A refusal is the object `{code, reason, text}` ([`IntegrationFault`]). Every tool runs as the
//! person, in the person's own home, exactly as a shell in a pane would.

use std::collections::BTreeMap;

use holzwave_core::integrate::service::{ProfilesView, RunAnswer, SelectionView, ToolView};
use holzwave_core::integrate::ticket::Card;
use holzwave_core::integrate::Selection;
use holzwave_core::layout::PaneId;
use serde_json::Value;

use crate::integrate_host::{ConfirmAnswer, ContextAnswer, IntegrationFault, LaunchAnswer};
use crate::state::AppCore;

/// The four tools and where each stands on this computer: found (with its path and version), not found (with the places
/// that were looked in), unusable or unsupported in this build, and the verbs each has. It looks for the programs and asks
/// a found one for its version.
pub fn integration_tools(core: &AppCore) -> Vec<ToolView> {
    core.integration_tools()
}

/// One read of a verb that only reads and shows a view, for `pane` (or for the tool's own selection when `pane` is null).
/// `args` is an object of strings; the core checks each value by the kind of the verb's argument. The answer is the view
/// model, the selection the read used and the time it was read; a refusal is the fault object. It starts the tool, so it
/// is async.
pub fn integration_run(
    core: &AppCore,
    tool: String,
    verb: String,
    pane: Option<PaneId>,
    args: BTreeMap<String, String>,
) -> Result<RunAnswer, IntegrationFault> {
    core.integration_run(&tool, &verb, pane, &args)
}

/// The selection of a pane (or the pick of the tool, which new panes start from, when `pane` is null): the context, the
/// namespace, the subscription, the section, the node and the profile, as far as the tool takes them. The core checks it
/// against the tool and its profile and answers it as it will be used (the pins of the profile filled in) with the name of
/// the production profile, or `null`. A selection the tool refuses is the fault object and changes nothing. One
/// `integration` event follows a selection that was accepted.
pub fn integration_select(
    core: &AppCore,
    tool: String,
    pane: Option<PaneId>,
    selection: Selection,
) -> Result<SelectionView, IntegrationFault> {
    core.integration_select(&tool, pane, &selection)
}

/// What the tools are set to and which panes have a selection: `{rows, panes}`. `rows` has one entry for each tool (its
/// state, and what is selected for `pane` or for the tool), `panes` one for each pane with an integration selection (an
/// integration block, or the terminal a launch opened), with the status of its tool when the pane holds the tool's last
/// words. With `refresh` the tools are looked for again and their default verbs are read, which starts them, so it is async.
pub fn integration_context(core: &AppCore, pane: Option<PaneId>, refresh: bool) -> ContextAnswer {
    core.integration_context(pane, refresh)
}

/// The card for a verb that changes something or opens a session: the ticket, the tool, the verb, what it does to the
/// world, the program's name and every argument spelled out, the target, the guard line, every field the hash covers and
/// the hash. Nothing is started. A verb that only reads is refused (it needs no card). The page shows the card as it is
/// and answers it with `integration_confirm` or closes it with `integration_cancel`.
pub fn integration_prepare(
    core: &AppCore,
    tool: String,
    verb: String,
    pane: Option<PaneId>,
    args: BTreeMap<String, String>,
) -> Result<Card, IntegrationFault> {
    core.integration_prepare(&tool, &verb, pane, &args)
}

/// The person's press on a card: the ticket and the hash of the card they were shown. It is the one integration command
/// that decides, and the one that names a ticket and a hash together; it exists for the webview only, and the local socket
/// of `wsh` has no counterpart. A verb that changes a file, a cluster or an account runs once (`kind` `ran`, with the
/// tool's own first line of output as `note`); an exec verb opens a pane (`kind` `launched`, with `pane`, its name as
/// `note` and the new layout as `view`) that the page starts. A ticket that is unknown, old, answered with another hash,
/// or no longer what the card showed is the fault object with the code `ticket`, and nothing runs.
pub fn integration_confirm(core: &AppCore, window: &str, ticket: String, hash: String) -> Result<ConfirmAnswer, IntegrationFault> {
    core.integration_confirm(window, &ticket, &hash)
}

/// The person closed a card without answering it. The ticket is dropped; a ticket that is not there is no problem.
pub fn integration_cancel(core: &AppCore, ticket: String) -> Result<(), IntegrationFault> {
    core.integration_cancel(&ticket);
    Ok(())
}

/// A terminal for a verb that only reads and opens one (logs, kernel messages, a shell): a pane in a tab or beside the
/// focused one (`placement` is `tab`, `right` or `down`), with the launch registered before the page starts it. The answer
/// is the new `pane` and the layout it is in, as `block_open` answers. A verb that changes something or opens a session
/// with the person's credentials is refused here and needs a card. It may read the tool once for the shell's overlay, so it
/// is async.
pub fn integration_launch(
    core: &AppCore,
    window: &str,
    tool: String,
    verb: String,
    pane: Option<PaneId>,
    args: BTreeMap<String, String>,
    placement: String,
) -> Result<LaunchAnswer, IntegrationFault> {
    core.integration_launch(window, &tool, &verb, pane, &args, &placement)
}

/// Makes `path` the program of `tool`. The person chooses which `kubectl` runs and not which program: the path must be
/// absolute, its file name must be one of the tool's own names, and the file must be one the core finds usable (an
/// executable file that no one but the person's group can replace). Answers the four tools again; a path that does not
/// pass is the fault object. It asks the tool for its version, so it is async.
pub fn integration_binary_set(core: &AppCore, tool: String, path: String) -> Result<Vec<ToolView>, IntegrationFault> {
    core.integration_binary_set(&tool, &path)
}

/// Forgets the program the person chose for `tool`; the search of the core applies again. Answers the four tools again.
pub fn integration_binary_clear(core: &AppCore, tool: String) -> Result<Vec<ToolView>, IntegrationFault> {
    core.integration_binary_clear(&tool)
}

/// The profiles and the notes about the files that could not be used. A profile lists its secret variables by name, each
/// with whether the secret store has a value (`has_value`): a yes or a no, never the value. It asks the store, so it is
/// async.
pub fn integration_profiles(core: &AppCore) -> ProfilesView {
    core.integration_profiles()
}

/// Adds a profile or replaces the one with the same id. `profile` has the shape of the profiles file: `secret_env` is a
/// list of names and `env` an object of plain values; the rules of the loader decide. A profile that does not pass is the
/// fault object with the code `profile`, naming the profile and the field and never a value. Answers the profiles again.
pub fn integration_profile_save(core: &AppCore, profile: Value) -> Result<ProfilesView, IntegrationFault> {
    core.integration_profile_save(profile)
}

/// Removes a profile and the values of its secret variables from the secret store. Answers the profiles again.
pub fn integration_profile_remove(core: &AppCore, id: String) -> Result<ProfilesView, IntegrationFault> {
    core.integration_profile_remove(&id)
}

/// Stores the value of one secret variable of a profile in the secret store. It is the one integration command that takes
/// a value, and it answers nothing: a value goes in and never comes out. The name must be one the profile declares in
/// `secret_env` and the value 1 to 1024 bytes; anything else is the fault object with the code `profile`. A store that is
/// locked says `secrets are locked`.
pub fn integration_secret_set(core: &AppCore, profile: String, name: String, value: String) -> Result<(), IntegrationFault> {
    core.integration_secret_set(&profile, &name, value)
}

/// Removes the value of one secret variable of a profile from the secret store. Answers nothing.
pub fn integration_secret_clear(core: &AppCore, profile: String, name: String) -> Result<(), IntegrationFault> {
    core.integration_secret_clear(&profile, &name)
}
