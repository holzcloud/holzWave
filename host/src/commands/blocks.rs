//! The plain functions of the commands for file browsers, previews, the sidebar widgets and the system monitor.

use holzwave_core::blocks::{BlockKind, BlockView};
use holzwave_core::layout::PaneId;
use holzwave_core::monitor::Reading;

use crate::state::{AppCore, View, WidgetsView};

/// Open a file browser or a preview: `kind` is `files` or `preview`, `source` is `local` or a connection id, `placement` is
/// `tab`, `right` or `down`. An empty `path` is the start directory of the source (for a browser). The answer is the view of
/// the window `window` with the new pane focused.
pub fn block_open(
    core: &AppCore,
    window: &str,
    kind: BlockKind,
    source: String,
    path: String,
    placement: String,
) -> Result<View, String> {
    core.block_open_in(window, &placement, kind, &source, &path)
}

/// Every pane that shows a file browser or a preview. Reads memory only.
pub fn pane_blocks(core: &AppCore) -> Vec<BlockView> {
    core.blocks_list()
}

/// The pane's block now shows `path`.
pub fn block_update(core: &AppCore, pane: PaneId, path: String) -> Result<(), String> {
    core.block_update(pane, &path)
}

/// The widgets of the sidebar, their saved notes and what was wrong with a widget file.
pub fn widgets_list(core: &AppCore) -> WidgetsView {
    core.widgets_list()
}

/// Save the text of a note widget (an empty text forgets it).
pub fn widget_note_save(core: &AppCore, id: String, text: String) -> Result<(), String> {
    core.widget_note_save(&id, &text)
}

/// What the computer of a monitor pane is doing: CPU, memory, network and the busiest processes, or the word that it is not
/// a computer this monitor can read. The page names a pane and never an address: the pane's block names the source.
///
/// The list of processes belongs to the machine that runs the core. Where the host says that machine is shared
/// ([`Host::local_processes`](crate::seams::Host::local_processes) is false) the local reading carries the numbers and an
/// empty list; the reading of another computer, over a saved connection, is the person's own and is passed on as it is.
/// Which reading is local is decided by the pane's block, as the core always did.
pub fn monitor_sample(core: &AppCore, pane: PaneId) -> Result<Reading, String> {
    core.monitor_sample_with(pane, core.host().local_processes())
}
