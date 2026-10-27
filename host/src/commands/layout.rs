//! The plain functions of the layout, window, tab and workspace commands. Each takes the core, the label of the calling
//! window (the key of its layout; the page never sends it) and the arguments of the command, and answers the new view.

use crate::state::{AppCore, View};

/// The layout of the calling window. An unknown window is the error `no such window`.
pub fn get_layout(core: &AppCore, window: &str) -> Result<View, String> {
    core.view_in(window)
}
