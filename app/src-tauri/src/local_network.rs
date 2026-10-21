//! The shell's side of the Local Network permission of macOS: the host that joins the stored choice of the person
//! ([`holzwave_core::local_network`]) with a probe of the system, and the view the page reads.
//!
//! The probe is itself a local network operation, so it can make the system show its own question. The host therefore
//! probes only when the person pressed `Allow` (the choice is `requested`), and a denial is reported only by a status read,
//! never by the call that asked: the system may refuse an operation before the person answered (TN3179). [`Source`] is the
//! seam: the macOS source sits over the bridge crate `holzwave-local-network`, and the tests put a scripted one in its
//! place. Windows and Linux have no source: `supported` is false there, nothing is stored, nothing is probed and the page
//! draws nothing. This file contains no foreign-function code.
//!
//! Nothing here runs at construction, at launch or from a timer: the page asks when a person acts (the first start of the
//! main window, the Settings dialog opening, the window gaining focus while the dialog is open, a button).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use holzwave_core::local_network::{Choice, FILE, LocalNetwork, Reading, State, VERSION, state_after_request, state_of};
use serde::Serialize;

/// How long a status read browses. The Swift side settles for 500 ms inside this window.
pub const STATUS_WINDOW: Duration = Duration::from_millis(1500);
/// How long the probe after `Allow` browses: longer, because the system question may be on its way.
pub const ANSWER_WINDOW: Duration = Duration::from_millis(3000);

/// The page's reading of the permission, in the order of the wire: `supported`, `notice_due`, `state`. `state` is one of
/// `allowed`, `denied`, `not_asked` and `unknown`; the page chooses its own sentences by it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LocalNetworkView {
    /// This system has the privacy setting (macOS 15 or later). Everything else is false or `unknown` where it is not.
    pub supported: bool,
    /// The notice has not been answered on this computer, on a system that has the setting.
    pub notice_due: bool,
    pub state: &'static str,
}

impl LocalNetworkView {
    /// The view of a system that has no such setting.
    pub fn unsupported() -> LocalNetworkView {
        LocalNetworkView { supported: false, notice_due: false, state: State::Unknown.as_str() }
    }
}

/// Where the system's answer comes from. Implemented over the bridge crate on macOS and by a scripted double in tests.
pub(crate) trait Source: Send + Sync {
    /// Whether this system has the Local Network privacy setting.
    fn applies(&self) -> bool;

    /// One bounded browse of the local network. Touches the network; may raise the system question. Blocks for at most
    /// `window`.
    fn probe(&self, window: Duration) -> Reading;
}

/// The source of this platform: over the bridge crate on macOS, none elsewhere (no notice, no row, no file).
pub(crate) fn system() -> Option<Arc<dyn Source>> {
    #[cfg(target_os = "macos")]
    {
        Some(Arc::new(SystemSource))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The source over the bridge crate, which holds the only foreign calls.
#[cfg(target_os = "macos")]
struct SystemSource;

#[cfg(target_os = "macos")]
impl Source for SystemSource {
    fn applies(&self) -> bool {
        holzwave_local_network::applies()
    }

    fn probe(&self, window: Duration) -> Reading {
        match holzwave_local_network::probe(window) {
            holzwave_local_network::Reading::Allowed => Reading::Allowed,
            holzwave_local_network::Reading::Denied => Reading::Denied,
            holzwave_local_network::Reading::Unknown => Reading::Unknown,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The choice of the person on this computer and the probe of the system, behind two commands' worth of methods.
pub struct LocalNetworkHost {
    dir: PathBuf,
    /// The loaded file. Locked briefly; held across the write of the file so two answers never interleave.
    stored: Mutex<LocalNetwork>,
    /// The source; replaced only by tests.
    source: Mutex<Option<Arc<dyn Source>>>,
    /// Held for the whole of a probe, so only one browse runs at a time.
    probing: Mutex<()>,
}

impl LocalNetworkHost {
    /// A host over the data directory `dir`. A damaged file starts as `unasked` (the bytes are kept as `.bad`). Nothing is
    /// probed here.
    pub(crate) fn new(dir: &Path, source: Option<Arc<dyn Source>>) -> LocalNetworkHost {
        let (stored, status) = LocalNetwork::load(dir);
        if status != holzwave_core::layout::LoadStatus::Clean {
            eprintln!("holzwave: {FILE}: {status:?}; the choice starts as {}", stored.choice.as_str());
        }
        LocalNetworkHost { dir: dir.to_owned(), stored: Mutex::new(stored), source: Mutex::new(source), probing: Mutex::new(()) }
    }

    /// Replaces the source (tests only).
    #[cfg(test)]
    pub(crate) fn use_source(&self, source: Arc<dyn Source>) {
        *lock(&self.source) = Some(source);
    }

    /// The source when this system has the setting, else `None`.
    fn applicable(&self) -> Option<Arc<dyn Source>> {
        lock(&self.source).clone().filter(|source| source.applies())
    }

    /// One probe, alone.
    fn probe(&self, source: &dyn Source, window: Duration) -> Reading {
        let _alone = lock(&self.probing);
        source.probe(window)
    }

    /// The stored choice.
    pub fn choice(&self) -> Choice {
        lock(&self.stored).choice
    }

    /// What the page shows now. The network is touched only when the choice is `requested` (it then answers `allowed`,
    /// `denied` or `unknown`); with `unasked` or `declined` the answer is `not_asked` and nothing was probed.
    pub fn view(&self) -> LocalNetworkView {
        let Some(source) = self.applicable() else { return LocalNetworkView::unsupported() };
        let choice = self.choice();
        let state = state_of(choice, || self.probe(source.as_ref(), STATUS_WINDOW));
        LocalNetworkView { supported: true, notice_due: choice == Choice::Unasked, state: state.as_str() }
    }

    /// The person answered the notice (or pressed `Allow` in the row). The choice is stored first; when the file cannot be
    /// written the error is returned and nothing is probed. `allow` then probes once with the longer window and answers
    /// `allowed` or `unknown`, never `denied` (the system may refuse before the person has answered; a status read says
    /// `denied`). A decline probes nothing and never replaces a request.
    pub fn answer(&self, allow: bool) -> Result<LocalNetworkView, String> {
        let Some(source) = self.applicable() else { return Err("not supported on this system".to_owned()) };
        let choice = {
            let mut stored = lock(&self.stored);
            let next = stored.choice.answered(allow);
            if next != stored.choice {
                let document = LocalNetwork { version: VERSION, choice: next };
                document.save(&self.dir).map_err(|e| format!("cannot save {FILE}: {e}"))?;
                *stored = document;
            }
            next
        };
        let state = if allow {
            state_after_request(self.probe(source.as_ref(), ANSWER_WINDOW))
        } else if choice == Choice::Requested {
            State::Unknown
        } else {
            State::NotAsked
        };
        Ok(LocalNetworkView { supported: true, notice_due: choice == Choice::Unasked, state: state.as_str() })
    }
}

/// A source for the tests: what it applies to and what it reads are set by the test, and the probes are counted.
#[cfg(test)]
pub(crate) struct ScriptedSource {
    applies: bool,
    reading: Mutex<Reading>,
    windows: Mutex<Vec<Duration>>,
}

#[cfg(test)]
impl ScriptedSource {
    pub(crate) fn new(applies: bool, reading: Reading) -> ScriptedSource {
        ScriptedSource { applies, reading: Mutex::new(reading), windows: Mutex::new(Vec::new()) }
    }

    /// Changes what the next probe reads.
    pub(crate) fn set(&self, reading: Reading) {
        *lock(&self.reading) = reading;
    }

    /// How many times the network was probed.
    pub(crate) fn probes(&self) -> usize {
        lock(&self.windows).len()
    }

    /// The windows of the probes, oldest first.
    pub(crate) fn windows(&self) -> Vec<Duration> {
        lock(&self.windows).clone()
    }
}

#[cfg(test)]
impl Source for ScriptedSource {
    fn applies(&self) -> bool {
        self.applies
    }

    fn probe(&self, window: Duration) -> Reading {
        lock(&self.windows).push(window);
        *lock(&self.reading)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("holzwave-ln-{tag}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path).expect("scratch dir");
            Dir(path)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn host(dir: &Dir, applies: bool, reading: Reading) -> (LocalNetworkHost, Arc<ScriptedSource>) {
        let source = Arc::new(ScriptedSource::new(applies, reading));
        (LocalNetworkHost::new(&dir.0, Some(source.clone())), source)
    }

    #[test]
    fn a_fresh_host_shows_the_notice_and_has_not_touched_the_network() {
        let dir = Dir::new("fresh");
        let (host, source) = host(&dir, true, Reading::Allowed);

        let view = host.view();

        assert_eq!(view, LocalNetworkView { supported: true, notice_due: true, state: "not_asked" });
        assert_eq!(source.probes(), 0, "reading the state before Allow must not touch the network");
        assert!(!dir.0.join(FILE).exists(), "reading writes nothing");
    }

    #[test]
    fn the_view_has_a_fixed_key_order() {
        let view = LocalNetworkView { supported: true, notice_due: false, state: "denied" };
        assert_eq!(
            serde_json::to_string(&view).expect("serialises"),
            r#"{"supported":true,"notice_due":false,"state":"denied"}"#
        );
    }

    #[test]
    fn not_now_stores_declined_without_a_probe_and_the_next_host_sees_it() {
        let dir = Dir::new("decline");
        let (first, source) = host(&dir, true, Reading::Allowed);

        let view = first.answer(false).expect("answers");

        assert_eq!(view, LocalNetworkView { supported: true, notice_due: false, state: "not_asked" });
        assert_eq!(source.probes(), 0);
        assert_eq!(first.choice(), Choice::Declined);
        let text = fs::read_to_string(dir.0.join(FILE)).expect("file");
        assert!(text.contains("\"declined\""), "{text}");

        let (second, second_source) = host(&dir, true, Reading::Allowed);
        assert_eq!(second.view(), LocalNetworkView { supported: true, notice_due: false, state: "not_asked" });
        assert_eq!(second_source.probes(), 0, "a declined choice is never probed");
    }

    #[test]
    fn allow_stores_requested_probes_once_and_never_answers_denied() {
        let dir = Dir::new("allow");
        let (allowed_host, allowed_source) = host(&dir, true, Reading::Allowed);
        let view = allowed_host.answer(true).expect("answers");
        assert_eq!(view, LocalNetworkView { supported: true, notice_due: false, state: "allowed" });
        assert_eq!(allowed_source.probes(), 1);
        assert_eq!(allowed_source.windows(), [ANSWER_WINDOW]);
        assert_eq!(allowed_host.choice(), Choice::Requested);
        assert!(fs::read_to_string(dir.0.join(FILE)).expect("file").contains("\"requested\""));

        let other = Dir::new("allow-denied");
        let (denied_host, denied_source) = host(&other, true, Reading::Denied);
        let view = denied_host.answer(true).expect("answers");
        assert_eq!(view.state, "unknown", "a denial right after the request is not reported");
        assert_eq!(denied_source.probes(), 1);
    }

    #[test]
    fn after_a_request_every_status_read_probes_once_and_may_say_denied() {
        let dir = Dir::new("status");
        let (host, source) = host(&dir, true, Reading::Unknown);
        host.answer(true).expect("answers");
        assert_eq!(source.probes(), 1);

        source.set(Reading::Denied);
        assert_eq!(host.view(), LocalNetworkView { supported: true, notice_due: false, state: "denied" });
        assert_eq!(source.probes(), 2);
        assert_eq!(source.windows()[1], STATUS_WINDOW);

        source.set(Reading::Allowed);
        assert_eq!(host.view().state, "allowed");
        assert_eq!(source.probes(), 3);
    }

    #[test]
    fn not_now_after_a_request_leaves_the_request() {
        let dir = Dir::new("keep");
        let (host, source) = host(&dir, true, Reading::Allowed);
        host.answer(true).expect("answers");
        assert_eq!(source.probes(), 1);

        let view = host.answer(false).expect("answers");

        assert_eq!(host.choice(), Choice::Requested);
        assert_eq!(source.probes(), 1, "a decline probes nothing");
        assert!(!view.notice_due);
        assert!(fs::read_to_string(dir.0.join(FILE)).expect("file").contains("\"requested\""));
    }

    #[test]
    fn allow_after_not_now_is_the_way_back() {
        let dir = Dir::new("back");
        let (host, source) = host(&dir, true, Reading::Allowed);
        host.answer(false).expect("answers");
        assert_eq!(host.choice(), Choice::Declined);

        let view = host.answer(true).expect("answers");

        assert_eq!(host.choice(), Choice::Requested);
        assert_eq!(view.state, "allowed");
        assert_eq!(source.probes(), 1);
    }

    #[test]
    fn a_system_without_the_setting_and_a_platform_without_a_source_are_unsupported() {
        let dir = Dir::new("old");
        let (host, source) = host(&dir, false, Reading::Allowed);
        assert_eq!(host.view(), LocalNetworkView { supported: false, notice_due: false, state: "unknown" });
        assert!(host.answer(true).is_err());
        assert!(host.answer(false).is_err());
        assert_eq!(source.probes(), 0);
        assert!(!dir.0.join(FILE).exists(), "nothing is stored where nothing is asked");

        let none = LocalNetworkHost::new(&dir.0, None);
        assert_eq!(none.view(), LocalNetworkView::unsupported());
        assert!(none.answer(true).is_err());
        assert!(!dir.0.join(FILE).exists());
    }

    #[test]
    fn an_unwritable_data_directory_makes_allow_an_error_with_no_probe() {
        let dir = Dir::new("unwritable");
        // A regular file where the data directory should be: nothing can be created below it.
        let blocked = dir.0.join("not-a-directory");
        fs::write(&blocked, "x").expect("file");
        let source = Arc::new(ScriptedSource::new(true, Reading::Allowed));
        let host = LocalNetworkHost::new(&blocked, Some(source.clone()));

        let result = host.answer(true);

        assert!(result.as_ref().is_err_and(|e| e.contains(FILE)), "{result:?}");
        assert_eq!(source.probes(), 0, "nothing is probed when the choice cannot be kept");
        assert_eq!(host.choice(), Choice::Unasked, "the memory follows the file");
    }

    #[test]
    fn a_hostile_file_starts_as_unasked() {
        let dir = Dir::new("hostile");
        fs::write(dir.0.join(FILE), "{\"choice\": \"requested\u{0}\"").expect("file");
        let (host, source) = host(&dir, true, Reading::Allowed);

        assert_eq!(host.view(), LocalNetworkView { supported: true, notice_due: true, state: "not_asked" });
        assert_eq!(source.probes(), 0);
        assert!(dir.0.join("local_network.json.bad").is_file(), "the damaged bytes are kept");
    }

    #[test]
    fn the_system_source_exists_only_on_macos() {
        assert_eq!(system().is_some(), cfg!(target_os = "macos"));
    }
}
