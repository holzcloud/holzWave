//! The install of the bundled `wsh` on a host, against the shared in-process SSH server: the steps, the swap that never
//! removes an old file first, the hosts that cannot take it, and the sentences (no path, no secret).

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use common::ssh_server::{self, Options, Server};
use holzwave_core::pool::Connection;
use holzwave_core::ssh::SshAuth;
use holzwave_core::wshinstall::{self, Bundled, Installed};

const VERSION: &str = "1.2.3";
const UNAME: &str = "uname -sm";
/// The check the install runs on the host, written out here so a change of that text is a change of a test.
const CHECK: &str = "\"$HOME/.holzwave/bin/wsh\" version 2>/dev/null";
const OLD: &str = "wsh 0.0.1\n";
const CURRENT: &str = "wsh 1.2.3\n";
const PASSWORD: &str = "pw";

/// A scratch directory that is removed when the test ends, also when it fails.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn x86_64() -> Vec<u8> {
    let mut bytes = b"\x7fELF-x86_64-stub".to_vec();
    bytes.extend((0..4000u32).map(|n| (n % 251) as u8));
    bytes
}

fn aarch64() -> Vec<u8> {
    b"\x7fELF-aarch64-stub".to_vec()
}

fn both() -> Bundled {
    Bundled { x86_64: Some(x86_64()), aarch64: Some(aarch64()) }
}

struct Rig {
    _dir: Scratch,
    dir: PathBuf,
    root: PathBuf,
    server: Server,
    conn: Arc<Connection>,
}

impl Rig {
    /// A host that says `uname` and whose `wsh version` answers `checks` in turn.
    fn new(tag: &str, uname: &str, checks: &[(&str, u32)]) -> Self {
        Self::with(tag, Options::password(), uname, checks)
    }

    fn with(tag: &str, options: Options, uname: &str, checks: &[(&str, u32)]) -> Self {
        let dir = common::scratch_dir(tag);
        let root = dir.join("root");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let options = options.sftp(root.clone()).exec_script(UNAME, &[(uname, 0)]).exec_script(CHECK, checks);
        Self::start(dir, root, options)
    }

    fn start(dir: PathBuf, root: PathBuf, options: Options) -> Self {
        let server = ssh_server::start(options);
        let known = dir.join("known_hosts");
        server.trust(&known);
        let conn = Connection::standalone("Test host", server.exec_target(SshAuth::Password(PASSWORD.into()), &known)).unwrap();
        Rig { _dir: Scratch(dir.clone()), dir, root, server, conn }
    }

    fn install(&self, bundled: &Bundled) -> Result<Installed, String> {
        wshinstall::install(&self.conn, bundled, VERSION)
    }

    fn wsh(&self) -> PathBuf {
        self.root.join(".holzwave/bin/wsh")
    }

    fn ops(&self) -> Vec<String> {
        self.server.seen.sftp_ops()
    }
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn an_install_uploads_verifies_and_says_so() {
    let rig = Rig::new("install", "Linux x86_64\n", &[("", 127), (CURRENT, 0)]);
    let result = rig.install(&both()).unwrap();
    assert_eq!(result, Installed::Installed(rig.wsh().to_string_lossy().into_owned()));
    assert_eq!(std::fs::read(rig.wsh()).unwrap(), x86_64(), "exactly the bundled bytes of this architecture");
    #[cfg(unix)]
    {
        assert_eq!(mode(&rig.wsh()), 0o755);
        assert_eq!(mode(&rig.root.join(".holzwave")), 0o700);
        assert_eq!(mode(&rig.root.join(".holzwave/bin")), 0o700);
    }
    let names: Vec<_> = std::fs::read_dir(rig.root.join(".holzwave/bin")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, ["wsh"], "no temporary file is left");
    let seen = &rig.server.seen;
    assert_eq!(seen.logins.load(Ordering::SeqCst), 1, "one login: the exec and the SFTP share the pooled connection");
    assert_eq!(seen.sftp_sessions.load(Ordering::SeqCst), 1, "one SFTP session");
    assert_eq!(seen.commands(), [UNAME, CHECK, CHECK], "the system, the check before, the check after: fixed texts");
    assert_eq!(seen.shells.load(Ordering::SeqCst), 0, "nothing is typed into a shell");
}

#[test]
fn only_paths_below_the_install_folder_are_touched() {
    let rig = Rig::new("below", "Linux x86_64\n", &[("", 127), (CURRENT, 0)]);
    rig.install(&both()).unwrap();
    let ops = rig.ops();
    assert!(!ops.is_empty());
    for op in &ops {
        for path in op.split_whitespace().filter(|w| w.starts_with('/')) {
            assert!(path == "/.holzwave" || path.starts_with("/.holzwave/bin"), "{op}");
        }
    }
    // Everything of the root: only the install folder came into being.
    let top: Vec<_> = std::fs::read_dir(&rig.root).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(top, [".holzwave"]);
}

#[test]
fn an_up_to_date_wsh_is_not_uploaded_again() {
    let rig = Rig::new("current", "Linux x86_64\n", &[(CURRENT, 0)]);
    std::fs::create_dir_all(rig.root.join(".holzwave/bin")).unwrap();
    std::fs::write(rig.wsh(), "already there").unwrap();
    let result = rig.install(&both()).unwrap();
    assert_eq!(result, Installed::AlreadyCurrent(rig.wsh().to_string_lossy().into_owned()));
    assert_eq!(std::fs::read(rig.wsh()).unwrap(), b"already there");
    assert!(rig.ops().is_empty(), "no write, no folder, no rename: {:?}", rig.ops());
    assert_eq!(rig.server.seen.commands(), [UNAME, CHECK]);
}

/// The position of the first operation that starts with `prefix`.
fn at(ops: &[String], prefix: &str) -> Option<usize> {
    ops.iter().position(|op| op.starts_with(prefix))
}

#[test]
fn another_version_is_replaced_without_losing_the_old_file_first() {
    for refuse_overwrite in [false, true] {
        let options = if refuse_overwrite { Options::password().sftp_refuse_overwrite() } else { Options::password() };
        let rig = Rig::with(&format!("replace-{refuse_overwrite}"), options, "Linux x86_64\n", &[(OLD, 0), (CURRENT, 0)]);
        std::fs::create_dir_all(rig.root.join(".holzwave/bin")).unwrap();
        std::fs::write(rig.wsh(), "old program").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(rig.wsh(), std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert_eq!(rig.install(&both()), Ok(Installed::Installed(rig.wsh().to_string_lossy().into_owned())), "refuse={refuse_overwrite}");
        assert_eq!(std::fs::read(rig.wsh()).unwrap(), x86_64());
        #[cfg(unix)]
        assert_eq!(mode(&rig.wsh()), 0o755, "the mode is the install's, not the old file's");
        let ops = rig.ops();
        // The old file is never removed: the only removal is of the copy the swap itself makes aside.
        assert!(ops.iter().filter(|op| op.starts_with("remove ")).all(|op| op.ends_with("wsh.holzwave-old")), "{ops:?}");
        let tmp_in = at(&ops, "rename /.holzwave/bin/wsh.holzwave-tmp /.holzwave/bin/wsh").expect("the new file is renamed into place");
        if refuse_overwrite {
            let aside = at(&ops, "rename /.holzwave/bin/wsh /.holzwave/bin/wsh.holzwave-old").expect("the old file waits aside");
            let in_place = ops.iter().rposition(|op| op == "rename /.holzwave/bin/wsh.holzwave-tmp /.holzwave/bin/wsh").unwrap();
            assert!(aside < in_place, "the old file moved aside before the new one took its place: {ops:?}");
            let removed = at(&ops, "remove /.holzwave/bin/wsh.holzwave-old");
            assert!(removed.is_none_or(|r| r > in_place || r < aside), "the copy aside goes only after the new file is in place: {ops:?}");
            assert!(tmp_in <= in_place);
            let names: Vec<_> = std::fs::read_dir(rig.root.join(".holzwave/bin")).unwrap().map(|e| e.unwrap().file_name()).collect();
            assert_eq!(names, ["wsh"], "neither the temporary file nor the copy aside stays");
        } else {
            assert!(at(&ops, "remove ").is_none(), "an overwriting rename needs no removal at all: {ops:?}");
        }
    }
}

#[test]
fn a_host_that_is_not_linux_is_told_so() {
    let cases: [(&str, &str); 6] = [
        ("Darwin arm64\n", "Darwin"),
        ("Linux armv7l\n", "armv7l"),
        ("", "did not say"),
        ("garbled\n", "did not say"),
        ("Linux x86_64 extra\n", "did not say"),
        ("FreeBSD amd64\n", "FreeBSD"),
    ];
    for (uname, word) in cases {
        let rig = Rig::new("not-linux", uname, &[("", 127)]);
        let sentence = rig.install(&both()).unwrap_err();
        assert!(sentence.contains(word), "{uname:?} -> {sentence}");
        assert_eq!(rig.server.seen.sftp_sessions.load(Ordering::SeqCst), 0, "nothing is uploaded, no SFTP is even opened: {uname:?}");
        assert!(rig.ops().is_empty());
        assert!(!rig.root.join(".holzwave").exists());
    }
}

#[test]
fn a_system_word_with_control_characters_is_cleaned() {
    let rig = Rig::new("cleaned", "Dar\u{1b}[2Jwin/;$(id) arm64\n", &[("", 127)]);
    let sentence = rig.install(&both()).unwrap_err();
    assert!(sentence.contains("Dar2Jwinid"), "{sentence}");
    assert!(!sentence.chars().any(char::is_control) && !sentence.contains(['$', ';', '(', '/']), "{sentence}");
}

#[test]
fn a_command_that_fails_is_not_taken_for_an_answer() {
    let dir = common::scratch_dir("uname-fails");
    let root = dir.join("root");
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    // The answer looks right but the command ended with a failure: it is not an answer.
    let options = Options::password().sftp(root.clone()).exec_script(UNAME, &[("Linux x86_64\n", 1)]);
    let rig = Rig::start(dir, root, options);
    assert_eq!(rig.install(&both()).unwrap_err(), wshinstall::NO_ANSWER);
    assert!(rig.ops().is_empty());
}

#[test]
fn a_build_without_the_binaries_is_told_so() {
    let rig = Rig::new("no-binary", "Linux x86_64\n", &[("", 127)]);
    let only_arm = Bundled { x86_64: None, aarch64: Some(aarch64()) };
    let sentence = rig.install(&only_arm).unwrap_err();
    assert!(sentence.contains("carries no wsh for x86_64"), "{sentence}");
    assert!(rig.ops().is_empty() && !rig.root.join(".holzwave").exists());
    let none = rig.install(&Bundled::default()).unwrap_err();
    assert!(none.contains("carries no wsh"), "{none}");

    // The other architecture picks the other file.
    let rig = Rig::new("arm", "Linux aarch64\n", &[("", 127), (CURRENT, 0)]);
    rig.install(&only_arm).unwrap();
    assert_eq!(std::fs::read(rig.wsh()).unwrap(), aarch64());
    // `arm64` and `amd64` are the same processors under their other names.
    let rig = Rig::new("amd", "Linux amd64\n", &[("", 127), (CURRENT, 0)]);
    rig.install(&both()).unwrap();
    assert_eq!(std::fs::read(rig.wsh()).unwrap(), x86_64());
}

#[test]
fn a_bundled_file_that_is_not_a_linux_program_is_never_uploaded() {
    let rig = Rig::new("not-elf", "Linux x86_64\n", &[("", 127)]);
    let bundled = Bundled { x86_64: Some(b"#!/bin/sh\nrm -rf ~\n".to_vec()), aarch64: None };
    assert_eq!(rig.install(&bundled).unwrap_err(), wshinstall::NOT_ELF);
    assert!(rig.ops().is_empty());
}

#[test]
fn a_failed_upload_leaves_no_temporary_file() {
    for (tag, options) in [("fail-write", Options::password().sftp_fail_writes()), ("deny-upload", Options::password().sftp_deny_upload())] {
        let rig = Rig::with(tag, options, "Linux x86_64\n", &[(OLD, 0)]);
        std::fs::create_dir_all(rig.root.join(".holzwave/bin")).unwrap();
        std::fs::write(rig.wsh(), "old program").unwrap();
        let sentence = rig.install(&both()).unwrap_err();
        assert_eq!(sentence, wshinstall::REFUSED, "{tag}");
        let names: Vec<_> = std::fs::read_dir(rig.root.join(".holzwave/bin")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["wsh"], "{tag}: no wsh.holzwave-tmp remains");
        assert_eq!(std::fs::read(rig.wsh()).unwrap(), b"old program", "{tag}: the existing wsh is intact");
        assert!(at(&rig.ops(), "open-write /.holzwave/bin/wsh.holzwave-tmp").is_some(), "{tag}: the upload was tried aside");
    }
}

#[test]
fn a_failed_verification_is_an_error() {
    let rig = Rig::new("unverified", "Linux x86_64\n", &[("", 127)]);
    let sentence = rig.install(&both()).unwrap_err();
    assert_eq!(sentence, wshinstall::NOT_VERIFIED);
    assert!(sentence.contains("could not be verified"));
    assert_eq!(std::fs::read(rig.wsh()).unwrap(), x86_64(), "the file is there; the sentence says it could not be verified");
    // Another version on the host after the upload is not verification either.
    let rig = Rig::new("wrong-version", "Linux x86_64\n", &[("", 127), (OLD, 0)]);
    assert_eq!(rig.install(&both()).unwrap_err(), wshinstall::NOT_VERIFIED);
    // And a version line with a failing status.
    let rig = Rig::new("status", "Linux x86_64\n", &[("", 127), (CURRENT, 1)]);
    assert_eq!(rig.install(&both()).unwrap_err(), wshinstall::NOT_VERIFIED);
}

#[test]
fn a_host_without_sftp_is_told_so() {
    let dir = common::scratch_dir("no-sftp");
    let root = dir.join("root");
    std::fs::create_dir(&root).unwrap();
    let options = Options::password().exec_script(UNAME, &[("Linux x86_64\n", 0)]).exec_script(CHECK, &[("", 127)]);
    let rig = Rig::start(dir, root, options);
    assert_eq!(rig.install(&both()).unwrap_err(), wshinstall::NO_SFTP);
}

#[test]
fn the_sentences_hold_no_path_of_the_app_and_no_secret() {
    let mut texts: Vec<String> = Vec::new();
    let scenarios: Vec<(Options, &str, Vec<(&str, u32)>, Bundled)> = vec![
        (Options::password(), "Darwin arm64\n", vec![("", 127)], both()),
        (Options::password(), "Linux armv7l\n", vec![("", 127)], both()),
        (Options::password(), "", vec![("", 127)], both()),
        (Options::password(), "Linux x86_64\n", vec![("", 127)], Bundled::default()),
        (Options::password(), "Linux x86_64\n", vec![("", 127)], Bundled { x86_64: Some(b"nope".to_vec()), aarch64: None }),
        (Options::password(), "Linux x86_64\n", vec![("", 127)], both()),
        (Options::password().sftp_fail_writes(), "Linux x86_64\n", vec![("", 127)], both()),
        (Options::password().sftp_deny_upload(), "Linux x86_64\n", vec![("", 127)], both()),
    ];
    let mut secrets = vec![PASSWORD.to_owned()];
    for (n, (options, uname, checks, bundled)) in scenarios.into_iter().enumerate() {
        let rig = Rig::with(&format!("words-{n}"), options, uname, &checks);
        secrets.push(rig.dir.to_string_lossy().into_owned());
        secrets.push(rig.root.to_string_lossy().into_owned());
        if let Err(sentence) = rig.install(&bundled) {
            texts.push(sentence);
        }
    }
    let ok = Rig::new("words-ok", "Linux x86_64\n", &[("", 127), (CURRENT, 0)]);
    secrets.push(ok.dir.to_string_lossy().into_owned());
    let result = ok.install(&both()).unwrap();
    // The result holds the path on the host (not of this computer); the sentence the app makes of it does not.
    assert!(matches!(result, Installed::Installed(_)));
    assert!(texts.len() >= 8, "every scenario ended in a sentence: {texts:?}");
    for text in &texts {
        for secret in &secrets {
            assert!(!text.contains(secret.as_str()), "{text:?} holds {secret:?}");
        }
        assert!(!text.contains("/tmp") && !text.contains("/var/") && !text.contains("/Users"), "{text:?}");
        assert!(!text.contains("uname") && !text.contains("$HOME"), "no command line: {text:?}");
        assert!(!text.chars().any(char::is_control), "{text:?}");
        assert!(text.starts_with("wsh was "), "{text:?}");
    }
}
