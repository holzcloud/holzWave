//! The in-app Kotlin plugin: `HolzwavePlugin` in `gen/android`, registered with Tauri's own plugin builder and no
//! `tauri-plugin-*` crate. Rust calls it through [`handle`]; it has no command of its own on the Rust side and registers no
//! invoke handler, so the page has no way to call it: an invoke for it is refused like any other command that is not in the
//! table. Everything that goes through it is started by Rust code in this crate.
//!
//! The vault, the file dialog and the certificate check of later changes use the same plugin class and the same handle.

use std::sync::OnceLock;

use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::DynRuntime;

/// The name Tauri knows the plugin by. It is the key of the plugin in the plugin manager of the Kotlin side.
const NAME: &str = "holzwave-android";
/// The package of the Kotlin class (the application id, which is also the namespace of the project).
const PACKAGE: &str = "ch.holzcloud.holzwave";
/// The Kotlin class, in [`PACKAGE`]. The Rust side finds it by this name, so the build of the release keeps it (see the rules
/// in `gen/android/app/proguard-rules.pro`).
const CLASS: &str = "HolzwavePlugin";

static HANDLE: OnceLock<PluginHandle<DynRuntime>> = OnceLock::new();

/// The handle of the Kotlin plugin, once the app has registered it. Calls through it block until Kotlin answers, so they are
/// made from a worker thread and never from the thread of the window system.
pub fn handle() -> Option<&'static PluginHandle<DynRuntime>> {
    HANDLE.get()
}

/// The plugin for the builder of the app.
pub fn init() -> TauriPlugin<DynRuntime> {
    Builder::<DynRuntime>::new(NAME)
        .setup(|_app, api| {
            let handle = api.register_android_plugin(PACKAGE, CLASS)?;
            // Set once: a second registration (a restarted activity) keeps the first handle, which is the one the poller has.
            let _ = HANDLE.set(handle);
            Ok(())
        })
        .build()
}
