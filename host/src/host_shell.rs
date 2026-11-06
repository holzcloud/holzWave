//! The host shell of a Flatpak, on the host side: the host that joins the stored choice of the person
//! ([`holzwave_core::host_shell`]) with what this Flatpak is allowed to do, the view the page reads, the setter, and the
//! two sentences a sandbox pane says on its first line.
//!
//! Inside a Flatpak a local pane starts in the sandbox, and on the host through `flatpak-spawn --host` only when the person
//! turned Host shell on **and** this Flatpak has the permission to talk to `org.freedesktop.Flatpak`. The permission is read
//! once, at construction, from the sandbox's own `/.flatpak-info` (the file does not change while the app runs), and any
//! doubt reads as missing. Outside a Flatpak nothing here reads a file or changes anything: the group does not apply.
//!
//! Every fact (the operating system, the environment, the existence of files, the text of `/.flatpak-info`, the data
//! directory) is passed in, so a test runs every case on any machine and needs no Flatpak. This file contains no
//! foreign-function code and starts no process: the launch itself is [`holzwave_core::platform::host_command`].

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use holzwave_core::host_shell::{
    FILE, HostShell, INFO_PATH, MAX_INFO_BYTES, State, VERSION, can_set, permission_in, state_of,
};
use holzwave_core::persist::read_limited;
use holzwave_core::platform::{Os, in_flatpak};
use serde::Serialize;

/// The line a new local pane shows while Host shell is `off` in a Flatpak. The words are the contract with the design
/// section of the Host shell group, and a test pins them against its text.
pub const NOTICE_OFF: &str = "holzWave: this pane runs inside the Flatpak sandbox. To use the tools on this computer, turn on Host shell in Settings.";
/// The line a new local pane shows while Host shell is `unavailable` in a Flatpak.
pub const NOTICE_UNAVAILABLE: &str = "holzWave: this pane runs inside the Flatpak sandbox. This Flatpak has no permission to run commands on the host; see the Flatpak page of the documentation.";

/// The sentence a pane says for `state`: one for `off` and one for `unavailable`. `on` says nothing (the pane runs where
/// the person asked), and outside a Flatpak there is no sandbox to speak of.
pub fn notice_for(state: State) -> Option<&'static str> {
    match state {
        State::Off => Some(NOTICE_OFF),
        State::Unavailable => Some(NOTICE_UNAVAILABLE),
        State::On | State::NotApplicable => None,
    }
}

/// The one notice string of a pane: the line about an unusable `shell` setting first, one space, then the sandbox sentence.
/// Either may be missing.
pub fn compose_notice(unusable: Option<String>, sandbox: Option<&str>) -> Option<String> {
    match (unusable, sandbox) {
        (Some(note), Some(sentence)) => Some(format!("{note} {sentence}")),
        (Some(note), None) => Some(note),
        (None, Some(sentence)) => Some(sentence.to_owned()),
        (None, None) => None,
    }
}

/// `notice` without the sandbox sentence: for a pane that turned out to run a saved SSH connection, which starts no local
/// shell and so is not in the sandbox in the sense of the sentence.
pub fn without_sandbox_notice(notice: Option<String>) -> Option<String> {
    let notice = notice?;
    for sentence in [NOTICE_OFF, NOTICE_UNAVAILABLE] {
        if notice == sentence {
            return None;
        }
        if let Some(rest) = notice.strip_suffix(sentence).and_then(|rest| rest.strip_suffix(' ')) {
            return Some(rest.to_owned());
        }
    }
    Some(notice)
}

/// The page's reading of the group, in the order of the wire: `applies`, `state`. `state` is one of `off`, `on` and
/// `unavailable`; the page chooses its own sentences by it. When `applies` is false (not in a Flatpak) the page draws
/// nothing, and `state` reads `unavailable`, the word a page that drew it anyway would treat as the safe one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostShellView {
    pub applies: bool,
    pub state: &'static str,
}

impl HostShellView {
    fn of(state: State) -> HostShellView {
        match state.as_str() {
            Some(word) => HostShellView { applies: true, state: word },
            None => HostShellView { applies: false, state: State::Unavailable.as_str().unwrap_or("unavailable") },
        }
    }
}

/// The facts about this process that the state is decided from. Read once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Facts {
    /// A Linux process that runs inside a Flatpak sandbox.
    in_flatpak: bool,
    /// The Flatpak may talk to `org.freedesktop.Flatpak` (always false outside one).
    permission: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The text of `/.flatpak-info`, bounded: a file above [`MAX_INFO_BYTES`], one that is not text and one that cannot be
/// read are `None`.
fn read_info_on_disk() -> Option<String> {
    let bytes = read_limited(Path::new(INFO_PATH), MAX_INFO_BYTES).ok().flatten()?;
    String::from_utf8(bytes).ok()
}

/// The choice of the person on this computer and the facts of this Flatpak, behind two commands' worth of methods.
pub struct HostShellHost {
    dir: PathBuf,
    facts: Mutex<Facts>,
    /// The loaded file. Locked briefly; held across the write of the file so two answers never interleave.
    stored: Mutex<HostShell>,
}

impl HostShellHost {
    /// A host over the data directory `dir`, deciding from this process's own facts.
    pub fn new(dir: &Path) -> HostShellHost {
        HostShellHost::with_facts(dir, Os::current(), &|key| std::env::var(key).ok(), &|path| Path::new(path).is_file(), &read_info_on_disk)
    }

    /// A host with the facts passed in: `os`, the environment and file existence that [`in_flatpak`] decides from, and the
    /// reader of the text of `/.flatpak-info`. Inside a Flatpak the permission is read once here and the file of the choice
    /// is loaded (a damaged one starts as off and its bytes are kept as `.bad`); outside one neither is touched.
    pub fn with_facts(
        dir: &Path,
        os: Os,
        env: &dyn Fn(&str) -> Option<String>,
        exists: &dyn Fn(&str) -> bool,
        read_info: &dyn Fn() -> Option<String>,
    ) -> HostShellHost {
        let inside = os == Os::Linux && in_flatpak(env, exists);
        let permission = inside && read_info().is_some_and(|text| permission_in(&text));
        let stored = if inside {
            let (stored, status) = HostShell::load(dir);
            if status != holzwave_core::layout::LoadStatus::Clean {
                eprintln!("holzwave: {FILE}: {status:?}; the host shell starts as off");
            }
            stored
        } else {
            HostShell::default()
        };
        HostShellHost { dir: dir.to_owned(), facts: Mutex::new(Facts { in_flatpak: inside, permission }), stored: Mutex::new(stored) }
    }

    /// Replaces the facts and reads the file again (tests only), so that one core can be a Flatpak with or without the
    /// permission.
    #[cfg(test)]
    pub fn use_facts(&self, in_flatpak: bool, permission: bool) {
        *lock(&self.facts) = Facts { in_flatpak, permission };
        *lock(&self.stored) = if in_flatpak { HostShell::load(&self.dir).0 } else { HostShell::default() };
    }

    /// The stored choice of the person.
    pub fn enabled(&self) -> bool {
        lock(&self.stored).enabled
    }

    /// The state of this computer.
    pub fn state(&self) -> State {
        let facts = *lock(&self.facts);
        state_of(facts.in_flatpak, facts.permission, self.enabled())
    }

    /// The state for a pane that is started now. `in_flatpak` is what the caller found in its own environment: the start
    /// of a pane passes its facts in, and the permission and the choice are the ones this host holds. Only `On` starts a
    /// pane on the host.
    pub fn state_for_pane(&self, in_flatpak: bool) -> State {
        let permission = lock(&self.facts).permission;
        state_of(in_flatpak, permission, self.enabled())
    }

    /// What the page shows now.
    pub fn view(&self) -> HostShellView {
        HostShellView::of(self.state())
    }

    /// The person pressed the switch. Refused, with the reason and nothing written, outside a Flatpak and in a Flatpak
    /// without the permission. Otherwise the choice is stored (the file is written only when it changes; a write that fails
    /// is returned and leaves the choice as it was) and the new view is returned. Panes that are already running are not
    /// touched: only a pane started afterwards reads the choice.
    pub fn set(&self, enabled: bool) -> Result<HostShellView, String> {
        let mut stored = lock(&self.stored);
        let facts = *lock(&self.facts);
        let state = state_of(facts.in_flatpak, facts.permission, stored.enabled);
        can_set(state).map_err(str::to_owned)?;
        if stored.enabled != enabled {
            let document = HostShell { version: VERSION, enabled };
            document.save(&self.dir).map_err(|e| format!("cannot save {FILE}: {e}"))?;
            *stored = document;
        }
        Ok(HostShellView::of(state_of(facts.in_flatpak, facts.permission, stored.enabled)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const INFO_TALK: &str = "[Application]\nname=ch.holzcloud.holzwave\n\n[Session Bus Policy]\norg.freedesktop.Flatpak=talk\n";
    const INFO_NONE: &str = "[Application]\nname=ch.holzcloud.holzwave\n\n[Session Bus Policy]\norg.freedesktop.Flatpak=none\n";

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("holzwave-hostshell-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path).expect("scratch dir");
            Scratch(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join(FILE)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A host over the facts of a process: `flatpak` is whether it is one, `info` the text of its `/.flatpak-info`.
    fn host(dir: &Scratch, os: Os, flatpak: bool, info: Option<&'static str>) -> HostShellHost {
        let env = move |key: &str| (flatpak && key == "FLATPAK_ID").then(|| "ch.holzcloud.holzwave".to_owned());
        HostShellHost::with_facts(&dir.0, os, &env, &|_| false, &move || info.map(str::to_owned))
    }

    #[test]
    fn outside_a_flatpak_the_group_does_not_apply_and_nothing_is_read_or_written() {
        for os in [Os::Macos, Os::Windows, Os::Linux] {
            let dir = Scratch::new("outside");
            // A file that says on, and a Flatpak info that grants the permission: neither is looked at.
            std::fs::write(dir.file(), "{\"version\": 1, \"host_shell\": true}\n").unwrap();
            let host = host(&dir, os, false, Some(INFO_TALK));
            assert_eq!(host.view(), HostShellView { applies: false, state: "unavailable" }, "{os:?}");
            assert_eq!(host.state(), State::NotApplicable);
            assert!(!host.enabled(), "{os:?}: the file is not read outside a Flatpak");
            assert_eq!(host.state_for_pane(false), State::NotApplicable);

            let before = std::fs::read(dir.file()).unwrap();
            let refused = host.set(true).expect_err("refused outside a Flatpak");
            assert!(refused.contains("only inside a Flatpak"), "{refused}");
            assert_eq!(std::fs::read(dir.file()).unwrap(), before, "{os:?}: nothing was written");
        }
        // A Flatpak variable on another operating system is not a Flatpak.
        let dir = Scratch::new("mac-flatpak-var");
        assert!(!host(&dir, Os::Macos, true, Some(INFO_TALK)).view().applies);
        assert!(!dir.file().exists());
    }

    #[test]
    fn the_view_has_exactly_the_three_words_of_the_spec() {
        let dir = Scratch::new("words");
        let words = |host: &HostShellHost| {
            let view = host.view();
            assert!(view.applies);
            view.state
        };
        assert_eq!(words(&host(&dir, Os::Linux, true, None)), "unavailable", "no info file");
        assert_eq!(words(&host(&dir, Os::Linux, true, Some(INFO_NONE))), "unavailable", "the permission is none");
        let allowed = host(&dir, Os::Linux, true, Some(INFO_TALK));
        assert_eq!(words(&allowed), "off");
        assert_eq!(allowed.set(true).expect("allowed").state, "on");
        assert_eq!(allowed.set(false).expect("allowed").state, "off");
        // The wire form: the fields in the order `applies`, `state`.
        let json = serde_json::to_string(&allowed.view()).unwrap();
        assert_eq!(json, "{\"applies\":true,\"state\":\"off\"}");
        for word in ["off", "on", "unavailable"] {
            assert!(State::from_word(word).is_some());
        }
    }

    #[test]
    fn the_setter_refuses_without_the_permission_and_writes_nothing() {
        for info in [None, Some(INFO_NONE), Some("[Session Bus Policy]\n\0org.freedesktop.Flatpak=talk")] {
            let dir = Scratch::new("no-permission");
            let host = host(&dir, Os::Linux, true, info);
            let refused = host.set(true).expect_err("refused");
            assert!(refused.contains("no permission"), "{refused}");
            assert!(!dir.file().exists(), "{info:?}: no file");
            assert!(!host.enabled());
            assert_eq!(host.view().state, "unavailable");
        }
    }

    #[test]
    fn the_setter_stores_the_choice_in_its_own_file_and_a_new_host_reads_it() {
        let dir = Scratch::new("store");
        let host_a = host(&dir, Os::Linux, true, Some(INFO_TALK));
        assert!(!dir.file().exists(), "starting writes nothing");
        assert_eq!(host_a.set(false).expect("allowed").state, "off");
        assert!(!dir.file().exists(), "no change, no write");
        host_a.set(true).expect("allowed");
        assert_eq!(std::fs::read_to_string(dir.file()).unwrap(), "{\n  \"version\": 1,\n  \"host_shell\": true\n}\n");
        assert_eq!(host(&dir, Os::Linux, true, Some(INFO_TALK)).view().state, "on", "the next start keeps it");
        // The same file in a Flatpak that lost the permission: kept, and not used.
        let revoked = host(&dir, Os::Linux, true, Some(INFO_NONE));
        assert_eq!(revoked.view().state, "unavailable");
        assert!(revoked.enabled());
        assert_eq!(revoked.state_for_pane(true), State::Unavailable);
    }

    #[test]
    fn a_damaged_file_starts_as_off_and_is_kept() {
        let dir = Scratch::new("damaged");
        std::fs::write(dir.file(), "{\"version\": 1, \"host_shell\": \"true\"}").unwrap();
        let host = host(&dir, Os::Linux, true, Some(INFO_TALK));
        assert_eq!(host.view().state, "off");
        assert!(dir.0.join("host_shell.json.bad").is_file());
    }

    #[test]
    fn a_write_that_fails_is_returned_and_leaves_the_choice() {
        let dir = Scratch::new("unwritable");
        let host = host(&dir, Os::Linux, true, Some(INFO_TALK));
        // The data directory is gone: the atomic write recreates parents, so make the target a directory instead.
        std::fs::create_dir(dir.file()).unwrap();
        let error = host.set(true).expect_err("cannot be saved");
        assert!(error.starts_with("cannot save host_shell.json"), "{error}");
        assert!(!host.enabled(), "the choice stays as it was");
    }

    #[test]
    fn the_notice_is_the_one_sentence_of_the_state_and_composes_in_one_line() {
        assert_eq!(notice_for(State::Off), Some(NOTICE_OFF));
        assert_eq!(notice_for(State::Unavailable), Some(NOTICE_UNAVAILABLE));
        assert_eq!(notice_for(State::On), None);
        assert_eq!(notice_for(State::NotApplicable), None);
        let shell = "holzWave: the shell setting names a program that is not there".to_owned();
        assert_eq!(compose_notice(None, None), None);
        assert_eq!(compose_notice(Some(shell.clone()), None), Some(shell.clone()));
        assert_eq!(compose_notice(None, Some(NOTICE_OFF)), Some(NOTICE_OFF.to_owned()));
        let both = compose_notice(Some(shell.clone()), Some(NOTICE_OFF)).unwrap();
        assert_eq!(both, format!("{shell} {NOTICE_OFF}"));
        // It comes apart again, which is what a saved SSH connection needs.
        assert_eq!(without_sandbox_notice(Some(both)), Some(shell.clone()));
        assert_eq!(without_sandbox_notice(Some(NOTICE_UNAVAILABLE.to_owned())), None);
        assert_eq!(without_sandbox_notice(Some(shell.clone())), Some(shell));
        assert_eq!(without_sandbox_notice(None), None);
        for sentence in [NOTICE_OFF, NOTICE_UNAVAILABLE] {
            assert!(!sentence.chars().any(char::is_control), "text, never an escape sequence");
            assert!(sentence.len() < holzwave_core::pane::MAX_NOTICE_BYTES, "one line the pane shows whole");
        }
    }

    #[test]
    fn the_words_and_the_sentences_are_those_of_the_design_section() {
        let design = include_str!("../../docs/design.md");
        let section = design.split_once("### 6.32 Host shell in a Flatpak").expect("the section exists").1;
        let section = section.split_once("\n### ").map_or(section, |(body, _)| body);
        for sentence in [NOTICE_OFF, NOTICE_UNAVAILABLE] {
            assert!(section.contains(&format!("`{sentence}`")), "the design section has no such notice: {sentence}");
        }
        for word in ["off", "on", "unavailable"] {
            assert!(section.contains(&format!("| `{word}` |")), "the state table has no row for {word}");
        }
        assert!(section.contains("`org.freedesktop.Flatpak`"), "the permission is named as the code reads it");
    }
}
