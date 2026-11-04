//! The plain functions of the commands for the settings, the app information, the text scale, the Markdown preview and
//! the links. The settings are the person's own (`settings.json` of the data directory of the core), so what one person
//! sets never reaches another.

use holzwave_core::layout::PaneId;
use serde_json::Value;

use crate::state::{AppCore, AppInfo, SettingsView};

/// The settings, the themes, the theme in force and the key bindings. Reads memory only.
pub fn get_settings(core: &AppCore) -> SettingsView {
    core.get_settings()
}

/// Set one setting (`key` is a field of `settings.json`, `value` its JSON value). Validated, written atomically, pushed to
/// every live pane and announced as the `settings` event; the answer is the new view, or the reason why nothing changed.
pub fn set_setting(core: &AppCore, key: String, value: Value) -> Result<SettingsView, String> {
    core.set_setting(&key, value)
}

/// Restore the default of one setting.
pub fn reset_setting(core: &AppCore, key: String) -> Result<SettingsView, String> {
    core.reset_setting(&key)
}

/// Re-read `settings.json` and `themes/` when a file changed; does nothing otherwise.
pub fn reload_settings(core: &AppCore) -> SettingsView {
    core.reload_settings()
}

/// The text size the system asks for, in percent (100 to 300; 100 where there is no such setting or it cannot be read).
/// The host decides: the desktop asks the operating system, the server answers 100 because the browser scales text itself.
pub fn os_text_scale(core: &AppCore) -> u32 {
    core.host().text_scale()
}

/// The version, the platform the host names and the host name.
pub fn app_info(core: &AppCore) -> AppInfo {
    core.app_info()
}

/// Render the Markdown of a chat reply or a preview as HTML, with the sanitiser the previews use: an allow-list of tags and
/// of link schemes, no images, no styles, no scripts. Answers nothing when the text is larger than the renderer takes. It
/// holds no state, so the core is not used.
pub fn markdown_render(_core: &AppCore, text: String) -> Option<String> {
    holzwave_core::preview::render_markdown(&text)
}

/// Open a link with the platform opener. Only links up to 2048 bytes whose scheme is in the `link_schemes` setting are
/// opened. A `file` link is shown in the file manager and never opened, and only when `pane` names a pane that runs on
/// this computer; `pane` means nothing for other links, and the calls that omit it keep working. It starts a process on
/// the machine that runs the core, so the browser build does not have it.
pub fn open_url(core: &AppCore, url: String, pane: Option<PaneId>) -> Result<(), String> {
    core.open_url(&url, pane)
}

/// Reveal a file or folder in the file manager and never run it. `path` is absolute, `~/...` or relative to `base` (the
/// pane's working directory); it has to exist. It starts a process on the machine that runs the core, so the browser build
/// does not have it.
pub fn reveal_path(core: &AppCore, path: String, base: Option<String>) -> Result<(), String> {
    core.reveal_path(&path, base.as_deref())
}
