//! The readiness flag of the platform certificate verifier, and the two places that ask it.
//!
//! The type is proved on the desktop. The hooks exist in Android builds only, so what a desktop test can show about them is
//! their text: each is one line behind a `cfg(target_os = "android")` attribute, there are exactly three (one in the sync run,
//! two in the AI send), and no other file of the crate asks the flag, so on every other platform the clients are untouched.
//!
//! Not run here: the hooks themselves (they are compiled for Android only; the emulator run shows `tls_not_ready` is absent
//! when the set-up worked and the clients then reach the network).

use std::fs;
use std::path::PathBuf;

use holzwave_host::tls_ready::{NOT_READY, TlsReady, global};

fn source(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The lines of `text` that ask the flag, each with the line before it.
fn hooks(text: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains("tls_ready::require()"))
        .map(|(at, line)| (lines[at.saturating_sub(1)].trim().to_owned(), line.trim().to_owned()))
        .collect()
}

#[test]
fn the_flag_is_false_until_set_and_require_gives_the_fixed_word() {
    let flag = TlsReady::new();
    assert!(!flag.is_set());
    assert_eq!(flag.require(), Err("tls_not_ready"));
    assert_eq!(NOT_READY, "tls_not_ready");
    flag.set();
    flag.set();
    assert!(flag.is_set());
    assert_eq!(flag.require(), Ok(()));
}

#[test]
fn a_desktop_process_never_sets_the_flag_of_the_process() {
    assert!(!global().is_set());
    assert_eq!(holzwave_host::tls_ready::require(), Err(NOT_READY));
}

#[test]
fn the_sync_run_asks_the_flag_in_one_android_only_line() {
    let found = hooks(&source("src/sync.rs"));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "#[cfg(target_os = \"android\")]", "the hook is gated to Android: {found:?}");
    assert!(found[0].1.contains("map_err(Fault::from)?;"), "a refusal is the failure of the run: {found:?}");
    // It sits in the function that builds the client, before the app password is read.
    let text = source("src/sync.rs");
    let once = &text[text.find("fn once(").expect("fn once")..];
    let hook = once.find("tls_ready::require()").expect("the hook is in once");
    let secret = once.find("self.secrets.get(PASSWORD)").expect("once reads the password");
    assert!(hook < secret, "the check comes before the password is read");
}

#[test]
fn the_ai_send_asks_the_flag_in_one_android_only_line_each() {
    let found = hooks(&source("src/ai_host.rs"));
    assert_eq!(found.len(), 2, "the send and the send with an offer: {found:?}");
    for (before, line) in &found {
        assert_eq!(before, "#[cfg(target_os = \"android\")]", "{line}");
        assert!(line.contains("map_err(AiFault::tls_not_ready)?;"), "{line}");
    }
}

#[test]
fn no_other_file_of_the_crate_asks_the_flag() {
    let mut files = Vec::new();
    for entry in fs::read_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")).expect("src") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            for inner in fs::read_dir(&path).expect("dir") {
                files.push(inner.expect("entry").path());
            }
        } else {
            files.push(path);
        }
    }
    for path in files.into_iter().filter(|p| p.extension().is_some_and(|e| e == "rs")) {
        let name = path.file_name().expect("name").to_string_lossy().into_owned();
        if matches!(name.as_str(), "sync.rs" | "ai_host.rs" | "tls_ready.rs") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read");
        // The module declaration of lib.rs is the one mention a file may have besides the three.
        assert!(!text.replace("pub mod tls_ready;", "").contains("tls_ready"), "{name} mentions the flag: the clients ask it in two places only");
    }
}
