//! The native macOS menu: only what the operating system needs (D-01, D-14).
//!
//! Tauri installs its default menu on macOS unless told otherwise, and that menu owns Cmd+W (Close Window), which
//! would swallow the pane-close shortcut of the page. This menu is built item by item instead. It has the app menu
//! (About, Hide, Hide Others, Quit), the Edit menu (Cut, Copy, Paste, Select All, which have to exist so that the
//! clipboard commands reach the web view) and the Window menu (Minimize only). It has no Close Window item, no
//! full screen item and no item of its own: every app action stays in the page, so all platforms share one path.
//! Other platforms draw their own chrome and register no native menu at all.
#![cfg(target_os = "macos")]

use tauri::menu::{AboutMetadata, Menu, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Runtime};

/// The name shown in the menu bar and in the item labels.
const NAME: &str = "holzWave";

/// Build the menu of the app.
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let about = AboutMetadata {
        name: Some(NAME.to_owned()),
        version: Some(app.package_info().version.to_string()),
        ..Default::default()
    };
    let app_menu = Submenu::with_items(
        app,
        NAME,
        true,
        &[
            &PredefinedMenuItem::about(app, Some(&format!("About {NAME}")), Some(about))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, Some(&format!("Hide {NAME}")))?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, Some(&format!("Quit {NAME}")))?,
        ],
    )?;
    let edit_menu = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let window_menu = Submenu::with_items(app, "Window", true, &[&PredefinedMenuItem::minimize(app, None)?])?;
    Menu::with_items(app, &[&app_menu, &edit_menu, &window_menu])
}
