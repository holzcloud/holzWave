//! The local shell of an Android pane, proved on the desktop with fakes: a host whose platform is `Platform::Android` and
//! whose private folders are a scratch folder, a pane that really starts a small program and reports what it was given,
//! and the rules that are pure functions of the core (`/system/bin/sh`, the system `PATH`).
//!
//! Not proved here: that `/system/bin/sh` of a real system is what the rules assume, and what it does with the locale
//! variables. Those need a device or an emulator.

// The tests that need a Unix system are cfg-gated, so on a Windows check their helpers are unused.
#![cfg_attr(windows, allow(dead_code, unused_imports, unused_mut))]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use holzwave_core::connections::{DOCKER_NOT_ON_ANDROID, WSL_ONLY_ON_WINDOWS};
use holzwave_core::platform::{Os, ProgramCheck, default_path, resolve_shell, search_dirs};
use holzwave_host::seams::{Host, HostLimits, NeutralHost, Platform, PrivateDirs};
use holzwave_host::state::{AppCore, Out, ShellSpec, Sink, Target};
use holzwave_host::windows::{CursorSpot, Place};

/// A sink that drops everything: the tests read what the child wrote to files.
struct Quiet;

impl Sink for Quiet {
    fn send(&self, _target: Target, _out: Out) {}

    fn close_window(&self, _label: &str) {}

    fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
        Ok(())
    }

    fn cursor_spot(&self, _source: &str) -> CursorSpot {
        CursorSpot::Unknown
    }
}

/// A host that is the neutral one in everything but its platform and its private folders.
struct Phone {
    base: NeutralHost,
    platform: Platform,
    dirs: Option<PrivateDirs>,
}

impl Host for Phone {
    fn platform(&self) -> Platform {
        self.platform
    }

    fn secrets(&self) -> Arc<dyn holzwave_core::secrets::SecretStore> {
        self.base.secrets()
    }

    fn local_network(&self) -> Arc<dyn holzwave_host::local_network::Source> {
        self.base.local_network()
    }

    fn text_scale(&self) -> u32 {
        self.base.text_scale()
    }

    fn limits(&self) -> HostLimits {
        self.base.limits()
    }

    fn private_dirs(&self) -> Option<PrivateDirs> {
        self.dirs.clone()
    }
}

/// A scratch folder with the private folders of a pretend app inside it: `data` for the core, `files/home` and `cache`
/// for the shell, and `out` for what the child reports.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("holzwave-android-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for sub in ["data", "files/home", "cache", "out"] {
            std::fs::create_dir_all(root.join(sub)).expect("scratch folder");
        }
        // The folders are named by their real path: a temp folder may be a link (macOS), and `pwd -P` of the child is not.
        Scratch { root: root.canonicalize().expect("canonical scratch folder") }
    }

    fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    fn out(&self) -> PathBuf {
        self.root.join("out")
    }

    fn dirs(&self) -> PrivateDirs {
        PrivateDirs { home: self.root.join("files/home"), cache: self.root.join("cache") }
    }

    /// The program that reports its directory and its environment into `out` and then waits.
    fn reporter(&self) -> ShellSpec {
        let out = self.out();
        let out = out.to_str().expect("utf-8 path");
        let script = format!("pwd -P > '{out}/cwd'; env > '{out}/env.tmp'; mv '{out}/env.tmp' '{out}/env'; sleep 30");
        ShellSpec::new("/bin/sh", &["-c", &script])
    }

    /// The pairs of the environment the child reported, after it has.
    fn env_of_child(&self) -> BTreeMap<String, String> {
        let file = self.out().join("env");
        let deadline = Instant::now() + Duration::from_secs(15);
        while !file.exists() {
            assert!(Instant::now() < deadline, "the child never reported its environment");
            std::thread::sleep(Duration::from_millis(10));
        }
        let text = std::fs::read_to_string(&file).expect("the environment");
        text.lines().filter_map(|line| line.split_once('=')).map(|(k, v)| (k.to_owned(), v.to_owned())).collect()
    }

    fn cwd_of_child(&self) -> PathBuf {
        // The environment is written last, so once it is there the directory is too.
        self.env_of_child();
        PathBuf::from(std::fs::read_to_string(self.out().join("cwd")).expect("the directory").trim_end())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn core_on(scratch: &Scratch, platform: Platform, dirs: Option<PrivateDirs>) -> Arc<AppCore> {
    let host: Arc<dyn Host> = Arc::new(Phone { base: NeutralHost::new(), platform, dirs });
    AppCore::new_with_host(scratch.data(), Arc::new(Quiet), scratch.reporter(), None, None, host)
}

fn focus(core: &AppCore) -> u64 {
    core.view_in("main").expect("the window main exists").focus
}

// ---- the private folder is where the shell lives ---------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn a_pane_on_android_starts_in_the_private_folder_with_home_and_tmpdir_named_by_the_host() {
    let scratch = Scratch::new("home");
    let core = core_on(&scratch, Platform::Android, Some(scratch.dirs()));
    core.start_pane(focus(&core), 80, 24, 0, 0).expect("the shell starts");

    assert_eq!(scratch.cwd_of_child(), scratch.dirs().home, "the start directory is the private folder");
    let env = scratch.env_of_child();
    assert_eq!(env.get("HOME").map(String::as_str), scratch.dirs().home.to_str());
    assert_eq!(env.get("TMPDIR").map(String::as_str), scratch.dirs().cache.to_str());
    assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"));
    assert_eq!(env.get("COLORTERM").map(String::as_str), Some("truecolor"));
    // The inherited PATH is untouched: the app adds no folder to it.
    assert_eq!(env.get("PATH").cloned(), std::env::var("PATH").ok());
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn a_directory_the_pane_asks_for_and_that_exists_is_kept() {
    let scratch = Scratch::new("asked");
    let asked = scratch.root.join("asked");
    std::fs::create_dir(&asked).expect("the directory");
    // The first pane of a fresh layout, and the session that names a directory for it.
    let first = core_on(&scratch, Platform::Android, Some(scratch.dirs()));
    let pane = focus(&first);
    first.shutdown();
    drop(first);
    std::fs::write(
        scratch.data().join("session.json"),
        format!(r#"{{"version": 1, "cwds": {{"{pane}": {:?}}}}}"#, asked.to_str().expect("utf-8 path")),
    )
    .expect("the session");

    let core = core_on(&scratch, Platform::Android, Some(scratch.dirs()));
    assert_eq!(focus(&core), pane, "the same pane comes back");
    core.start_pane(pane, 80, 24, 0, 0).expect("the shell starts");
    assert_eq!(scratch.cwd_of_child(), asked, "an existing directory is not replaced by the private folder");
    // HOME still names the private folder: the directory is where the shell is, not whose home it is.
    assert_eq!(scratch.env_of_child().get("HOME").map(String::as_str), scratch.dirs().home.to_str());
    core.shutdown();
}

#[cfg(unix)]
#[test]
fn a_host_that_names_no_folder_or_is_not_android_changes_nothing_about_a_pane() {
    for (tag, platform, dirs) in [("none", Platform::Android, None), ("linux", Platform::Linux, Some(()))] {
        let scratch = Scratch::new(tag);
        let dirs = dirs.map(|()| scratch.dirs());
        let core = core_on(&scratch, platform, dirs);
        core.start_pane(focus(&core), 80, 24, 0, 0).expect("the shell starts");
        let env = scratch.env_of_child();
        assert_eq!(env.get("TMPDIR"), std::env::var("TMPDIR").ok().as_ref(), "{tag}: TMPDIR is the inherited one, or none");
        assert_eq!(env.get("HOME"), std::env::var("HOME").ok().as_ref(), "{tag}: HOME is the inherited one");
        assert_ne!(scratch.cwd_of_child(), scratch.dirs().home, "{tag}: no private start directory");
        core.shutdown();
    }
}

// ---- nothing of the app leaks into the shell, and no wsh ------------------------------------------------------------

#[cfg(unix)]
#[test]
fn the_environment_of_an_android_shell_carries_no_secret_and_no_wsh_folder() {
    let scratch = Scratch::new("leak");
    let canary = "canary-9f3a1c-do-not-leak";
    let host: Arc<dyn Host> = Arc::new(Phone { base: NeutralHost::new(), platform: Platform::Android, dirs: Some(scratch.dirs()) });
    host.secrets().set("ssh:canary", canary).expect("a secret in the store");
    let core = AppCore::new_with_host(scratch.data(), Arc::new(Quiet), scratch.reporter(), None, None, host);
    let socket_dir = scratch.root.join("ipc");
    core.set_ipc(socket_dir.clone(), "token-1234".to_owned());
    core.start_pane(focus(&core), 80, 24, 0, 0).expect("the shell starts");

    let env = scratch.env_of_child();
    // The one rule that puts the app's socket there: the variables `wsh` would find the app by.
    let mut named: Vec<&str> = env.keys().filter(|k| k.starts_with("HOLZWAVE_")).map(String::as_str).collect();
    named.sort_unstable();
    assert_eq!(named, ["HOLZWAVE_IPC", "HOLZWAVE_PANE", "HOLZWAVE_TOKEN"]);
    for (name, value) in &env {
        assert!(!value.contains(canary), "{name} carries a secret");
        if name != "HOLZWAVE_IPC" {
            assert!(!value.contains(socket_dir.to_str().expect("utf-8 path")), "{name} carries the socket path");
        }
    }
    // No `wsh` folder in front of the PATH: a phone bundles none.
    assert_eq!(env.get("PATH").cloned(), std::env::var("PATH").ok());
    // And nothing on the command line of the child but what the script names: the secret is in no argument.
    let list = std::process::Command::new("ps").args(["-ww", "-eo", "args"]).output().expect("ps");
    let list = String::from_utf8_lossy(&list.stdout);
    assert!(!list.contains(canary) && !list.contains("token-1234"), "a secret is on a command line");
    core.shutdown();
}

// ---- what the shell is, from the pure rules ---------------------------------------------------------------------------

#[test]
fn the_android_shell_is_the_system_shell_unless_a_usable_absolute_setting_names_another() {
    let env = |_: &str| None::<String>;
    let nothing_exists = |_: &str| false;
    let usable = |path: &str| if path == "/system/bin/sh" || path == "/data/local/tmp/mksh" { ProgramCheck::Usable } else { ProgramCheck::Missing };

    let default = resolve_shell(Os::Android, "", &env, &nothing_exists, &usable);
    assert_eq!((default.launch.program.as_str(), default.unusable), ("/system/bin/sh", None));

    let kept = resolve_shell(Os::Android, "/data/local/tmp/mksh", &env, &nothing_exists, &usable);
    assert_eq!((kept.launch.program.as_str(), kept.unusable), ("/data/local/tmp/mksh", None));

    let fallback = resolve_shell(Os::Android, "/bin/bash", &env, &nothing_exists, &usable);
    assert_eq!(fallback.launch.program, "/system/bin/sh");
    let sentence = fallback.unusable.expect("a plain sentence");
    assert!(sentence.contains("/bin/bash") && sentence.contains("/system/bin/sh"), "{sentence}");
}

#[test]
fn an_android_search_path_takes_absolute_folders_only_and_an_empty_one_is_the_system_folders() {
    let path = |_: &str| Some("/system/bin:/vendor/bin:relative:".to_owned());
    assert_eq!(search_dirs(Os::Android, &path), ["/system/bin", "/vendor/bin"]);
    assert_eq!(default_path(Os::Android), Some("/system/bin:/system/xbin"));
}

// ---- what a phone does not have --------------------------------------------------------------------------------------

#[test]
fn a_phone_has_no_wsl_and_no_docker_connection_but_keeps_them_in_the_list() {
    use holzwave_core::connections::{Connections, SecretChange};
    use holzwave_core::profiles::{Kind, Profile};
    use holzwave_core::secrets::MemoryStore;

    let scratch = Scratch::new("dropped");
    let (connections, _) = Connections::load(&scratch.data(), Arc::new(MemoryStore::default()), None, &[1, 2]);
    let connections = connections.for_os(Os::from_name(Platform::Android.as_str()));
    let mut wsl = Profile::new("", "Box", "Ubuntu");
    wsl.kind = Kind::Wsl;
    let wsl = connections.save(wsl, SecretChange::Keep).expect("saved").profile.id;
    let mut docker = Profile::new("", "Web", "web-1");
    docker.kind = Kind::Docker;
    let docker = connections.save(docker, SecretChange::Keep).expect("saved").profile.id;

    assert!(connections.assign(1, &wsl).unwrap_err().to_string().contains(WSL_ONLY_ON_WINDOWS));
    assert!(connections.assign(2, &docker).unwrap_err().to_string().contains(DOCKER_NOT_ON_ANDROID));
    assert_eq!(connections.list().len(), 2, "the profiles stay in the list and in the sync");
}
