//! Handing a single file over between the phone's document dialog and the shell's folder, proved on the desktop with a fake
//! picker and a scratch folder: the name rules, the numbering, the limit, the jail of a copy out, the one-at-a-time rule
//! and the two commands as deny-by-default rows.
//!
//! Not proved here: the real system dialog, real providers (a cloud drive, a USB stick, another app) that report no size or
//! a wrong one, and a very large file on a phone. The fake picker stands in for all of it.

// The tests that need a Unix system are cfg-gated, so on a Windows check their helpers are unused.
#![cfg_attr(windows, allow(dead_code, unused_imports, unused_mut))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use holzwave_host::commands::handover::{HandoverOutcomeMsg, handover_in, handover_out};
use holzwave_host::commands::registry::{CAP_DEFAULT, COMMANDS, Mode, NOT_IN_BROWSER, Outcome, Scope, Shell, Web, lookup};
use holzwave_host::handover::{
    FOLDER, MAX_HANDOVER_BYTES, MAX_NAME_BYTES, Refusal, clean_name, export, import, open_regular, reopen_source,
    resolve_source, unique_create,
};
use holzwave_host::seams::{FilePicker, Host, NeutralHost, PickError, PickedIn, PickedOut, PrivateDirs};
use holzwave_host::state::{AppCore, Out, ShellSpec, Sink, Target};
use holzwave_host::windows::{CursorSpot, Place};
use serde_json::json;

const BUILD_RS: &str = include_str!("../../app/src-tauri/build.rs");
const CAPABILITY: &str = include_str!("../../app/src-tauri/capabilities/default.json");

// ---------------------------------------------------------------------------------------------------------------------
// Fixtures

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

/// What the fake dialog does when the person "chooses".
#[derive(Clone)]
enum Act {
    /// Writes `bytes` bytes of `fill` to the staging file and reports `name` and that size.
    Write { name: String, bytes: usize, fill: u8 },
    /// Writes a sparse file of `len` bytes and reports `reported` as its size.
    Sparse { name: String, len: u64, reported: u64 },
    Cancel,
    TooLarge { name: String, size: Option<u64> },
    Fail,
    Panic,
}

struct FakePicker {
    act: Mutex<Act>,
    /// The time the dialog stays open, so that a second call can arrive meanwhile.
    open_for: Mutex<Duration>,
    ins: AtomicUsize,
    outs: AtomicUsize,
    active: AtomicUsize,
    most_active: AtomicUsize,
    /// What the last copy out was handed: the real path and the suggested name.
    last_out: Mutex<Option<(PathBuf, String)>>,
    /// What a copy out answers.
    out_answer: Mutex<Result<PickedOut, PickError>>,
}

impl FakePicker {
    fn new(act: Act) -> Arc<FakePicker> {
        Arc::new(FakePicker {
            act: Mutex::new(act),
            open_for: Mutex::new(Duration::ZERO),
            ins: AtomicUsize::new(0),
            outs: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            most_active: AtomicUsize::new(0),
            last_out: Mutex::new(None),
            out_answer: Mutex::new(Ok(PickedOut { size: 0 })),
        })
    }

    fn writes(name: &str, bytes: usize) -> Arc<FakePicker> {
        FakePicker::new(Act::Write { name: name.to_owned(), bytes, fill: b'a' })
    }

    fn set(&self, act: Act) {
        *self.act.lock().expect("lock") = act;
    }

    fn enter(&self) {
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.most_active.fetch_max(now, Ordering::SeqCst);
        let wait = *self.open_for.lock().expect("lock");
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }

    fn leave(&self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

impl FilePicker for FakePicker {
    fn pick_in(&self, staging: &Path, max_bytes: u64) -> Result<PickedIn, PickError> {
        self.ins.fetch_add(1, Ordering::SeqCst);
        assert_eq!(max_bytes, MAX_HANDOVER_BYTES, "the picker is given the one limit");
        self.enter();
        let act = self.act.lock().expect("lock").clone();
        let result = match act {
            Act::Write { name, bytes, fill } => {
                fs::write(staging, vec![fill; bytes]).map_err(|_| PickError::Failed).map(|()| PickedIn { name, size: bytes as u64 })
            }
            Act::Sparse { name, len, reported } => {
                let file = fs::OpenOptions::new().write(true).open(staging).map_err(|_| PickError::Failed)?;
                file.set_len(len).map_err(|_| PickError::Failed)?;
                Ok(PickedIn { name, size: reported })
            }
            Act::Cancel => Err(PickError::Cancelled),
            Act::TooLarge { name, size } => {
                // A picker that stops early leaves some bytes behind, which the host must remove.
                let _ = fs::write(staging, b"partial");
                Err(PickError::TooLarge { name, size })
            }
            Act::Fail => {
                let _ = fs::write(staging, b"partial");
                Err(PickError::Failed)
            }
            Act::Panic => {
                let _ = fs::write(staging, b"partial");
                self.leave();
                panic!("the picker blew up");
            }
        };
        self.leave();
        result
    }

    fn save_out(&self, source: &Path, suggested_name: &str) -> Result<PickedOut, PickError> {
        self.outs.fetch_add(1, Ordering::SeqCst);
        self.enter();
        *self.last_out.lock().expect("lock") = Some((source.to_path_buf(), suggested_name.to_owned()));
        let answer = self.out_answer.lock().expect("lock").clone();
        self.leave();
        answer
    }
}

/// A host that is the neutral one in everything but its private folders and its document dialog.
struct PhoneHost {
    base: NeutralHost,
    dirs: Option<PrivateDirs>,
    picker: Option<Arc<FakePicker>>,
}

impl Host for PhoneHost {
    fn platform(&self) -> holzwave_host::seams::Platform {
        holzwave_host::seams::Platform::Android
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

    fn limits(&self) -> holzwave_host::seams::HostLimits {
        self.base.limits()
    }

    fn private_dirs(&self) -> Option<PrivateDirs> {
        self.dirs.clone()
    }

    fn file_picker(&self) -> Option<Arc<dyn FilePicker>> {
        self.picker.clone().map(|picker| picker as Arc<dyn FilePicker>)
    }
}

struct Scratch {
    root: PathBuf,
    home: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("holzwave-handover-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(root.join("cache")).expect("cache");
        fs::create_dir_all(root.join("data")).expect("data");
        Scratch { root, home }
    }

    fn handover(&self) -> PathBuf {
        self.home.join(FOLDER)
    }

    fn listing(&self, dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = match fs::read_dir(dir) {
            Ok(entries) => entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect(),
            Err(_) => vec![],
        };
        names.sort();
        names
    }

    fn core(&self, picker: Option<Arc<FakePicker>>) -> Arc<AppCore> {
        let host = PhoneHost {
            base: NeutralHost::new(),
            dirs: Some(PrivateDirs { home: self.home.clone(), cache: self.root.join("cache") }),
            picker,
        };
        AppCore::new_with_host(self.root.join("data"), Arc::new(Quiet), ShellSpec::new("/bin/cat", &[]), None, None, Arc::new(host))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn done(name: &str, size: u64, renamed: bool) -> HandoverOutcomeMsg {
    HandoverOutcomeMsg::Done { name: name.to_owned(), size, shown: format!("~/handover/{name}"), renamed }
}

/// A name that was taken: `name` is the chosen file's, `kept_as` the place it got.
fn numbered(name: &str, kept_as: &str, size: u64) -> HandoverOutcomeMsg {
    HandoverOutcomeMsg::Done { name: name.to_owned(), size, shown: format!("~/handover/{kept_as}"), renamed: true }
}

// ---------------------------------------------------------------------------------------------------------------------
// Test 1: clean_name

#[test]
fn clean_name_keeps_a_plain_name_and_removes_every_separator() {
    assert_eq!(clean_name("report.pdf"), "report.pdf");
    assert_eq!(clean_name("Report.PDF"), "Report.PDF", "the case is never changed");
    assert_eq!(clean_name("../../etc/passwd"), "etc_passwd");
    let mixed = clean_name("a/b\\c.txt");
    assert_eq!(mixed, "a_b_c.txt");
    assert!(!mixed.contains('/') && !mixed.contains('\\'));
    for hostile in ["/", "\\", "//", "..", "../..", "a/../../b", "..\\..\\x", "/etc/passwd", "C:\\Windows\\x.exe"] {
        let name = clean_name(hostile);
        assert!(!name.contains('/') && !name.contains('\\'), "{hostile:?} gave {name:?}");
        assert!(!name.starts_with('.') && !name.ends_with('.') && !name.ends_with(' '), "{hostile:?} gave {name:?}");
        assert_ne!(name, "..");
        assert!(!name.is_empty());
    }
}

#[test]
fn clean_name_trims_dots_and_spaces_at_the_ends_and_never_leaves_nothing() {
    assert_eq!(clean_name("  .hidden. "), "hidden");
    assert_eq!(clean_name("...a b..."), "a b");
    for nothing in ["", ".", "..", "...", " ", " . . ", "   ", "/", "\\", "./", " .. / . "] {
        assert_eq!(clean_name(nothing), "file", "{nothing:?}");
    }
}

#[test]
fn clean_name_replaces_control_and_direction_characters() {
    assert_eq!(clean_name("a\0b"), "a_b");
    assert_eq!(clean_name("a\nb"), "a_b");
    assert_eq!(clean_name("a\tb"), "a_b");
    assert_eq!(clean_name("a\u{7f}b"), "a_b");
    assert_eq!(clean_name("a\u{85}b"), "a_b");
    // The right-to-left override that makes `gpj.exe` read as `exe.jpg`, the isolates, a zero-width space, a mark.
    assert_eq!(clean_name("photo\u{202E}gpj.exe"), "photo_gpj.exe");
    assert_eq!(clean_name("a\u{2066}b\u{2069}c"), "a_b_c");
    assert_eq!(clean_name("a\u{200B}b"), "a_b");
    assert_eq!(clean_name("a\u{200F}b"), "a_b");
    assert_eq!(clean_name("a\u{FEFF}b"), "a_b");
    // A name that is only such characters is still a name.
    assert_eq!(clean_name("\n\t"), "__");
    // Characters that are fine stay: other scripts, an emoji, a space inside, a leading dash.
    assert_eq!(clean_name("Bericht Übersicht 2026.pdf"), "Bericht Übersicht 2026.pdf");
    assert_eq!(clean_name("日本語.txt"), "日本語.txt");
    assert_eq!(clean_name("-rf"), "-rf");
}

#[test]
fn clean_name_cuts_to_255_bytes_on_a_character_boundary_and_keeps_a_short_extension() {
    let long = format!("{}.pdf", "a".repeat(300));
    let cut = clean_name(&long);
    assert_eq!(cut.len(), MAX_NAME_BYTES);
    assert!(cut.ends_with(".pdf"));
    assert_eq!(&cut[..cut.len() - 4], "a".repeat(251));

    // An extension of 16 bytes after the dot is kept, one of 17 is cut like any other text.
    let sixteen = format!("{}.{}", "a".repeat(300), "e".repeat(16));
    let kept = clean_name(&sixteen);
    assert_eq!(kept.len(), MAX_NAME_BYTES);
    assert!(kept.ends_with(&format!(".{}", "e".repeat(16))));
    let seventeen = format!("{}.{}", "a".repeat(300), "e".repeat(17));
    let lost = clean_name(&seventeen);
    assert_eq!(lost.len(), MAX_NAME_BYTES);
    assert!(lost.starts_with(&"a".repeat(255)));

    // Characters of two, three and four bytes are never cut in the middle.
    for unit in ["é", "日", "😀"] {
        for extra in 0..4 {
            let text = format!("{}{}.txt", unit.repeat(120), "x".repeat(extra));
            let name = clean_name(&text);
            assert!(name.len() <= MAX_NAME_BYTES, "{unit} {extra}: {} bytes", name.len());
            assert!(name.is_char_boundary(name.len()));
            assert!(name.ends_with(".txt"), "{unit} {extra}");
            assert!(std::str::from_utf8(name.as_bytes()).is_ok());
        }
        let no_extension = unit.repeat(200);
        let name = clean_name(&no_extension);
        assert!(name.len() <= MAX_NAME_BYTES);
        assert!(name.chars().all(|c| c.to_string() == unit), "{unit}: a whole character is kept or dropped");
    }

    // The cut leaves no dot or space at the end of the stem.
    let ragged = format!("{}. . .pdf", "b".repeat(250));
    let name = clean_name(&ragged);
    assert!(name.len() <= MAX_NAME_BYTES && name.ends_with(".pdf"));
    assert!(!name.trim_end_matches(".pdf").ends_with(['.', ' ']), "{name:?}");
}

// ---------------------------------------------------------------------------------------------------------------------
// Test 2: unique names

#[test]
fn a_taken_name_gets_a_number_and_a_file_is_never_overwritten() {
    let scratch = Scratch::new("unique");
    let dir = scratch.home.clone();
    for (want, expect) in [
        ("report.pdf", "report.pdf"),
        ("report.pdf", "report-2.pdf"),
        ("report.pdf", "report-3.pdf"),
        ("notes", "notes"),
        ("notes", "notes-2"),
        ("archive.tar.gz", "archive.tar.gz"),
        ("archive.tar.gz", "archive.tar-2.gz"),
    ] {
        let (mut file, name) = unique_create(&dir, want).expect("a free name");
        assert_eq!(name, expect);
        use std::io::Write;
        file.write_all(expect.as_bytes()).expect("write");
    }
    assert_eq!(fs::read(dir.join("report.pdf")).expect("read"), b"report.pdf", "the first file is untouched after three");
    assert_eq!(fs::read(dir.join("report-2.pdf")).expect("read"), b"report-2.pdf");
}

#[test]
fn a_numbered_name_stays_within_255_bytes() {
    let scratch = Scratch::new("unique-long");
    let name = clean_name(&format!("{}.pdf", "z".repeat(400)));
    let (_first, one) = unique_create(&scratch.home, &name).expect("first");
    let (_second, two) = unique_create(&scratch.home, &name).expect("second");
    assert_eq!(one, name);
    assert_ne!(two, name);
    assert!(two.len() <= MAX_NAME_BYTES && two.ends_with("-2.pdf"), "{} bytes {two}", two.len());
}

#[cfg(unix)]
#[test]
fn a_dangling_link_in_the_way_is_a_taken_name_and_is_not_followed() {
    let scratch = Scratch::new("unique-link");
    let outside = scratch.root.join("outside.txt");
    std::os::unix::fs::symlink(&outside, scratch.home.join("trap.txt")).expect("link");
    let (_file, name) = unique_create(&scratch.home, "trap.txt").expect("a name");
    assert_eq!(name, "trap-2.txt");
    assert!(!outside.exists(), "nothing was created through the link");
}

#[test]
fn many_creations_at_once_each_get_a_name_of_their_own() {
    let scratch = Scratch::new("unique-race");
    let dir = Arc::new(scratch.home.clone());
    let handles: Vec<_> = (0..16)
        .map(|_| {
            let dir = dir.clone();
            std::thread::spawn(move || unique_create(&dir, "same.txt").expect("a name").1)
        })
        .collect();
    let mut names: Vec<String> = handles.into_iter().map(|handle| handle.join().expect("thread")).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 16, "no two calls got the same name");
}

// ---------------------------------------------------------------------------------------------------------------------
// Test 3: import

#[test]
fn an_import_lands_in_the_handover_folder_with_the_cleaned_name() {
    let scratch = Scratch::new("import");
    let picker = FakePicker::writes("report.pdf", 1_048_576);
    let core = scratch.core(Some(picker.clone()));

    assert_eq!(handover_in(&core), done("report.pdf", 1_048_576, false));
    let file = scratch.handover().join("report.pdf");
    assert_eq!(fs::metadata(&file).expect("the file").len(), 1_048_576);
    assert_eq!(scratch.listing(&scratch.handover()), ["report.pdf"], "the staging file is gone");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&file).expect("meta").permissions().mode() & 0o777, 0o600, "readable by the app user only");
        assert_eq!(fs::metadata(scratch.handover()).expect("meta").permissions().mode() & 0o777, 0o700);
    }

    // The same name again is numbered, and the first file is as it was.
    assert_eq!(handover_in(&core), numbered("report.pdf", "report-2.pdf", 1_048_576));
    assert_eq!(scratch.listing(&scratch.handover()), ["report-2.pdf", "report.pdf"]);
    assert_eq!(fs::read(&file).expect("read"), vec![b'a'; 1_048_576]);
    assert_eq!(picker.ins.load(Ordering::SeqCst), 2);
}

#[test]
fn the_name_of_a_provider_is_cleaned_before_it_names_anything() {
    let scratch = Scratch::new("import-hostile");
    let picker = FakePicker::writes("../../data/secrets.vault", 10);
    let core = scratch.core(Some(picker));
    let data_before = scratch.listing(&scratch.root.join("data"));
    assert_eq!(handover_in(&core), done("data_secrets.vault", 10, false));
    assert_eq!(scratch.listing(&scratch.handover()), ["data_secrets.vault"]);
    assert_eq!(scratch.listing(&scratch.root.join("data")), data_before, "nothing outside the folder");
    assert_eq!(scratch.listing(&scratch.home), ["handover"]);
}

#[test]
fn the_size_an_import_reports_is_the_size_that_was_copied_not_the_provider_claim() {
    let scratch = Scratch::new("import-size");
    let picker = FakePicker::new(Act::Sparse { name: "a.bin".into(), len: 2048, reported: 5 });
    let core = scratch.core(Some(picker));
    assert_eq!(handover_in(&core), done("a.bin", 2048, false));
}

#[test]
fn an_empty_file_is_a_file() {
    let scratch = Scratch::new("import-empty");
    let core = scratch.core(Some(FakePicker::writes("empty.txt", 0)));
    assert_eq!(handover_in(&core), done("empty.txt", 0, false));
    assert_eq!(fs::metadata(scratch.handover().join("empty.txt")).expect("file").len(), 0);
}

#[test]
fn staging_files_of_an_earlier_run_are_swept_and_other_files_are_left() {
    let scratch = Scratch::new("import-sweep");
    fs::create_dir_all(scratch.handover()).expect("dir");
    fs::write(scratch.handover().join(".incoming-deadbeef"), b"left by a killed app").expect("stale");
    fs::write(scratch.handover().join("mine.txt"), b"keep").expect("mine");
    let core = scratch.core(Some(FakePicker::writes("new.txt", 3)));
    assert_eq!(handover_in(&core), done("new.txt", 3, false));
    assert_eq!(scratch.listing(&scratch.handover()), ["mine.txt", "new.txt"]);
}

#[cfg(unix)]
#[test]
fn a_link_in_the_place_of_the_handover_folder_is_refused() {
    let scratch = Scratch::new("import-folder-link");
    let elsewhere = scratch.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("dir");
    std::os::unix::fs::symlink(&elsewhere, scratch.handover()).expect("link");
    let picker = FakePicker::writes("a.txt", 3);
    let core = scratch.core(Some(picker.clone()));
    assert_eq!(handover_in(&core), HandoverOutcomeMsg::Failed);
    assert_eq!(picker.ins.load(Ordering::SeqCst), 0, "the dialog is not shown");
    assert!(scratch.listing(&elsewhere).is_empty(), "nothing was put outside the home");
}

// ---------------------------------------------------------------------------------------------------------------------
// Test 4: limits and failures

#[test]
fn a_file_over_the_limit_is_refused_with_its_name_and_size_and_leaves_nothing() {
    assert_eq!(MAX_HANDOVER_BYTES, 268_435_456);
    let scratch = Scratch::new("limit");

    // The provider says the size before copying: the picker stops and answers too large.
    let picker = FakePicker::new(Act::TooLarge { name: "big.iso".into(), size: Some(612_000_000) });
    let core = scratch.core(Some(picker.clone()));
    assert_eq!(handover_in(&core), HandoverOutcomeMsg::TooLarge { name: "big.iso".into(), size: Some(612_000_000) });
    assert_eq!(scratch.listing(&scratch.handover()), Vec::<String>::new(), "nothing stays, not even the staging file");

    // Without a size, the name is still cleaned.
    picker.set(Act::TooLarge { name: "../big.iso".into(), size: None });
    assert_eq!(handover_in(&core), HandoverOutcomeMsg::TooLarge { name: "big.iso".into(), size: None });

    // A picker that reports a size over the limit but returns without error.
    picker.set(Act::Sparse { name: "huge.bin".into(), len: 10, reported: MAX_HANDOVER_BYTES + 1 });
    assert_eq!(
        handover_in(&core),
        HandoverOutcomeMsg::TooLarge { name: "huge.bin".into(), size: Some(MAX_HANDOVER_BYTES + 1) }
    );

    // A picker that says a small size and writes more than the limit: the host measures the file itself.
    picker.set(Act::Sparse { name: "liar.bin".into(), len: MAX_HANDOVER_BYTES + 1, reported: 100 });
    assert_eq!(
        handover_in(&core),
        HandoverOutcomeMsg::TooLarge { name: "liar.bin".into(), size: Some(MAX_HANDOVER_BYTES + 1) }
    );
    assert_eq!(scratch.listing(&scratch.handover()), Vec::<String>::new());

    // Exactly the limit is allowed.
    picker.set(Act::Sparse { name: "edge.bin".into(), len: MAX_HANDOVER_BYTES, reported: MAX_HANDOVER_BYTES });
    assert_eq!(handover_in(&core), done("edge.bin", MAX_HANDOVER_BYTES, false));
}

#[test]
fn a_failed_or_cancelled_dialog_changes_nothing_and_a_panic_is_caught() {
    let scratch = Scratch::new("failures");
    let picker = FakePicker::new(Act::Fail);
    let core = scratch.core(Some(picker.clone()));

    assert_eq!(handover_in(&core), HandoverOutcomeMsg::Failed);
    assert_eq!(scratch.listing(&scratch.handover()), Vec::<String>::new(), "the half-written staging file is removed");

    picker.set(Act::Panic);
    assert_eq!(handover_in(&core), HandoverOutcomeMsg::Failed);
    assert_eq!(scratch.listing(&scratch.handover()), Vec::<String>::new());

    picker.set(Act::Cancel);
    assert_eq!(handover_in(&core), HandoverOutcomeMsg::Cancelled);
    assert_eq!(scratch.listing(&scratch.handover()), Vec::<String>::new());

    // After every one of those the next import works: no turn was left held.
    picker.set(Act::Write { name: "ok.txt".into(), bytes: 4, fill: b'x' });
    assert_eq!(handover_in(&core), done("ok.txt", 4, false));
}

#[test]
fn no_answer_of_a_failure_holds_text() {
    let json = |outcome: &HandoverOutcomeMsg| serde_json::to_value(outcome).expect("json");
    assert_eq!(json(&HandoverOutcomeMsg::Failed), json!({ "kind": "failed" }));
    assert_eq!(json(&HandoverOutcomeMsg::Cancelled), json!({ "kind": "cancelled" }));
    assert_eq!(json(&HandoverOutcomeMsg::Unavailable), json!({ "kind": "unavailable" }));
    assert_eq!(
        json(&HandoverOutcomeMsg::TooLarge { name: "a.bin".into(), size: None }),
        json!({ "kind": "too_large", "name": "a.bin", "size": null })
    );
    assert_eq!(
        json(&numbered("a.bin", "a-2.bin", 9)),
        json!({ "kind": "done", "name": "a.bin", "size": 9, "shown": "~/handover/a-2.bin", "renamed": true })
    );
}

// ---------------------------------------------------------------------------------------------------------------------
// Test 5: the jail of a copy out

fn put(scratch: &Scratch, relative: &str, bytes: &[u8]) -> PathBuf {
    let path = scratch.home.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    fs::write(&path, bytes).expect("file");
    path
}

#[test]
fn a_copy_out_hands_the_picker_the_real_path_and_the_suggested_name() {
    let scratch = Scratch::new("export");
    let real = put(&scratch, "report.pdf", &vec![b'r'; 1_288_490]);
    let picker = FakePicker::writes("unused", 0);
    *picker.out_answer.lock().expect("lock") = Ok(PickedOut { size: 1_288_490 });
    let core = scratch.core(Some(picker.clone()));

    let answer = handover_out(&core, "report.pdf".into());
    assert_eq!(
        answer,
        HandoverOutcomeMsg::Done { name: "report.pdf".into(), size: 1_288_490, shown: "~/report.pdf".into(), renamed: false }
    );
    let (path, name) = picker.last_out.lock().expect("lock").clone().expect("the picker was called");
    assert_eq!(path, fs::canonicalize(&real).expect("canonical"));
    assert_eq!(name, "report.pdf");
    assert_eq!(picker.outs.load(Ordering::SeqCst), 1);

    // The forms of a path that mean the same file.
    for text in ["~/report.pdf", "./report.pdf", "dir/../report.pdf"] {
        let expected = if text.contains("..") { HandoverOutcomeMsg::Failed } else { answer.clone() };
        assert_eq!(handover_out(&core, text.into()), expected, "{text}");
    }
    // A file in a folder, and the file that an import made.
    put(&scratch, "notes/a b.txt", b"hello");
    *picker.out_answer.lock().expect("lock") = Ok(PickedOut { size: 5 });
    assert_eq!(
        handover_out(&core, "notes/a b.txt".into()),
        HandoverOutcomeMsg::Done { name: "a b.txt".into(), size: 5, shown: "~/notes/a b.txt".into(), renamed: false }
    );
}

#[test]
fn a_refused_path_answers_failed_and_the_picker_is_never_called() {
    let scratch = Scratch::new("jail");
    put(&scratch, "report.pdf", b"x");
    fs::create_dir_all(scratch.home.join("folder")).expect("dir");
    fs::write(scratch.root.join("outside.txt"), b"secret").expect("outside");
    fs::write(scratch.root.join("data").join("secrets.vault"), b"vault").expect("vault stand-in");
    let picker = FakePicker::writes("unused", 0);
    let core = scratch.core(Some(picker.clone()));

    let mut refused: Vec<String> = vec![
        "../outside.txt".into(),
        "folder/../../outside.txt".into(),
        "..".into(),
        "../data/secrets.vault".into(),
        "/etc/passwd".into(),
        format!("{}", scratch.root.join("outside.txt").display()),
        "".into(),
        "report.pdf\0".into(),
        "report\n.pdf".into(),
        "folder".into(),
        "~/".into(),
        "~".into(),
        ".".into(),
        "./".into(),
        "missing.txt".into(),
        "x".repeat(5000),
    ];
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(scratch.root.join("outside.txt"), scratch.home.join("link-to-file")).expect("link");
        symlink(&scratch.root, scratch.home.join("link-to-dir")).expect("link");
        symlink(scratch.root.join("data"), scratch.home.join("link-to-data")).expect("link");
        symlink("/etc/passwd", scratch.home.join("link-to-etc")).expect("link");
        symlink(scratch.home.join("missing"), scratch.home.join("dangling")).expect("link");
        for text in ["link-to-file", "link-to-dir/outside.txt", "link-to-data/secrets.vault", "link-to-etc", "dangling"] {
            refused.push(text.into());
        }
    }
    for text in &refused {
        let shown = if text.len() > 40 { format!("{}…", &text[..40]) } else { text.clone() };
        assert_eq!(handover_out(&core, text.clone()), HandoverOutcomeMsg::Failed, "{shown:?}");
    }
    assert_eq!(picker.outs.load(Ordering::SeqCst), 0, "no refused path reached the dialog");

    // And the same text through the plain function with the refusal kinds.
    assert_eq!(resolve_source(&scratch.home, "../outside.txt"), Err(Refusal::Text));
    assert_eq!(resolve_source(&scratch.home, "/etc/passwd"), Err(Refusal::Text));
    assert_eq!(resolve_source(&scratch.home, "folder"), Err(Refusal::NotAFile));
    assert_eq!(resolve_source(&scratch.home, "missing.txt"), Err(Refusal::Missing));
    #[cfg(unix)]
    {
        assert_eq!(resolve_source(&scratch.home, "link-to-file"), Err(Refusal::Outside));
        assert_eq!(resolve_source(&scratch.home, "link-to-data/secrets.vault"), Err(Refusal::Outside));
    }
}

#[cfg(unix)]
#[test]
fn a_link_that_stays_inside_the_folder_is_followed_to_its_real_file() {
    let scratch = Scratch::new("jail-inside");
    put(&scratch, "real/data.txt", b"abc");
    std::os::unix::fs::symlink(scratch.home.join("real"), scratch.home.join("alias")).expect("link");
    let source = resolve_source(&scratch.home, "alias/data.txt").expect("inside the folder");
    assert_eq!(source.shown, "~/real/data.txt", "the person reads the real place");
    assert_eq!(source.size, 3);
}

#[test]
fn a_file_over_the_limit_is_not_handed_to_the_dialog() {
    let scratch = Scratch::new("export-big");
    let big = put(&scratch, "big.bin", b"");
    fs::OpenOptions::new().write(true).open(&big).expect("open").set_len(MAX_HANDOVER_BYTES + 1).expect("sparse");
    let picker = FakePicker::writes("unused", 0);
    let core = scratch.core(Some(picker.clone()));
    assert_eq!(
        handover_out(&core, "big.bin".into()),
        HandoverOutcomeMsg::TooLarge { name: "big.bin".into(), size: Some(MAX_HANDOVER_BYTES + 1) }
    );
    assert_eq!(picker.outs.load(Ordering::SeqCst), 0);
}

#[test]
fn a_copy_out_that_the_person_leaves_or_that_fails_answers_so() {
    let scratch = Scratch::new("export-answers");
    put(&scratch, "a.txt", b"x");
    let picker = FakePicker::writes("unused", 0);
    let core = scratch.core(Some(picker.clone()));
    *picker.out_answer.lock().expect("lock") = Err(PickError::Cancelled);
    assert_eq!(handover_out(&core, "a.txt".into()), HandoverOutcomeMsg::Cancelled);
    *picker.out_answer.lock().expect("lock") = Err(PickError::Failed);
    assert_eq!(handover_out(&core, "a.txt".into()), HandoverOutcomeMsg::Failed);
    *picker.out_answer.lock().expect("lock") = Err(PickError::TooLarge { name: "a.txt".into(), size: Some(7) });
    assert_eq!(handover_out(&core, "a.txt".into()), HandoverOutcomeMsg::TooLarge { name: "a.txt".into(), size: Some(7) });
}

#[cfg(unix)]
#[test]
fn a_file_replaced_by_a_link_between_the_check_and_the_open_is_caught() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new("race");
    let path = put(&scratch, "swap.txt", b"mine");
    fs::write(scratch.root.join("outside.txt"), b"secret").expect("outside");

    let source = resolve_source(&scratch.home, "swap.txt").expect("a regular file");
    assert!(reopen_source(&source).is_ok(), "unchanged, it opens");

    // Replaced by a link to a file outside the folder.
    fs::remove_file(&path).expect("remove");
    symlink(scratch.root.join("outside.txt"), &path).expect("link");
    assert!(reopen_source(&source).is_err(), "a link in place of the file is refused");
    assert!(open_regular(&path).is_err(), "and a link is never opened as a regular file");

    // Replaced by another regular file: the same name, another file.
    fs::remove_file(&path).expect("remove");
    fs::write(&path, b"someone else's").expect("other");
    assert!(reopen_source(&source).is_err(), "another file under the same name is not the file that was checked");

    // Replaced by a directory.
    fs::remove_file(&path).expect("remove");
    fs::create_dir(&path).expect("dir");
    assert!(reopen_source(&source).is_err());
    assert!(open_regular(&path).is_err());
}

#[test]
fn open_regular_reads_a_regular_file_and_refuses_a_directory() {
    let scratch = Scratch::new("open-regular");
    let path = put(&scratch, "a.txt", b"12345");
    let (mut file, size) = open_regular(&path).expect("a regular file");
    assert_eq!(size, 5);
    let mut text = String::new();
    use std::io::Read;
    file.read_to_string(&mut text).expect("read");
    assert_eq!(text, "12345");
    assert!(open_regular(&scratch.home).is_err());
    assert!(open_regular(&scratch.home.join("missing")).is_err());
}

// ---------------------------------------------------------------------------------------------------------------------
// Test 6: one at a time

#[test]
fn a_second_hand_over_while_the_dialog_is_open_answers_failed_at_once() {
    let scratch = Scratch::new("exclusive");
    put(&scratch, "a.txt", b"x");
    let picker = FakePicker::writes("one.txt", 8);
    *picker.open_for.lock().expect("lock") = Duration::from_millis(700);
    let core = scratch.core(Some(picker.clone()));

    let first = {
        let core = core.clone();
        std::thread::spawn(move || handover_in(&core))
    };
    let begun = Instant::now();
    while picker.active.load(Ordering::SeqCst) == 0 {
        assert!(begun.elapsed() < Duration::from_secs(10), "the dialog never opened");
        std::thread::sleep(Duration::from_millis(5));
    }

    let asked = Instant::now();
    assert_eq!(handover_in(&core), HandoverOutcomeMsg::Failed, "a second copy in");
    assert_eq!(handover_out(&core, "a.txt".into()), HandoverOutcomeMsg::Failed, "a copy out meanwhile");
    assert!(asked.elapsed() < Duration::from_millis(500), "the second call did not wait for the dialog");
    assert_eq!(first.join().expect("thread"), done("one.txt", 8, false));
    assert_eq!(picker.most_active.load(Ordering::SeqCst), 1, "the dialog was never entered twice at once");
    assert_eq!(picker.outs.load(Ordering::SeqCst), 0);

    // Afterwards the turn is free again.
    *picker.open_for.lock().expect("lock") = Duration::ZERO;
    assert_eq!(handover_in(&core), numbered("one.txt", "one-2.txt", 8));
}

// ---------------------------------------------------------------------------------------------------------------------
// Test 7: no picker, nothing else touched

#[test]
fn a_host_with_no_picker_or_no_shell_folder_answers_unavailable() {
    assert!(NeutralHost::new().file_picker().is_none());

    let scratch = Scratch::new("unavailable");
    put(&scratch, "a.txt", b"x");
    let no_picker = scratch.core(None);
    assert_eq!(handover_in(&no_picker), HandoverOutcomeMsg::Unavailable);
    assert_eq!(handover_out(&no_picker, "a.txt".into()), HandoverOutcomeMsg::Unavailable);
    assert!(!scratch.handover().exists(), "no folder was made");

    // A picker but no folder for the shell (the desktop has none).
    let host = PhoneHost { base: NeutralHost::new(), dirs: None, picker: Some(FakePicker::writes("a", 1)) };
    let core = AppCore::new_with_host(scratch.root.join("data"), Arc::new(Quiet), ShellSpec::new("/bin/cat", &[]), None, None, Arc::new(host));
    assert_eq!(handover_in(&core), HandoverOutcomeMsg::Unavailable);
    assert_eq!(handover_out(&core, "a.txt".into()), HandoverOutcomeMsg::Unavailable);

    // And the plain core of a desktop test.
    let plain = AppCore::new(scratch.root.join("data"), Arc::new(Quiet), ShellSpec::new("/bin/cat", &[]));
    assert_eq!(handover_in(&plain), HandoverOutcomeMsg::Unavailable);
}

#[test]
fn the_module_names_no_file_of_the_app_but_the_handover_folder() {
    for (name, text) in [
        ("handover.rs", include_str!("../src/handover.rs")),
        ("commands/handover.rs", include_str!("../src/commands/handover.rs")),
    ] {
        for word in ["vault", "settings", "layout", "connections.json", "secrets", "keystore"] {
            assert!(!text.to_lowercase().contains(word), "{name} mentions {word}");
        }
        for forbidden in ["println!", "eprintln!", "dbg!", "unsafe", "log::", "tracing::"] {
            assert!(!text.contains(forbidden), "{name} contains {forbidden}");
        }
        // No label of the private planning notes in a public file.
        for label in ["D-0", "D-1", "D-2", "09-", "T-09"] {
            assert!(!text.contains(label), "{name} carries a planning label ({label})");
        }
    }
    let scratch = Scratch::new("folder");
    let core = scratch.core(Some(FakePicker::writes("a.txt", 1)));
    let data_before = scratch.listing(&scratch.root.join("data"));
    assert_eq!(handover_in(&core), done("a.txt", 1, false));
    assert_eq!(scratch.listing(&scratch.home), ["handover"], "the only thing made in the home is the handover folder");
    assert_eq!(scratch.listing(&scratch.root.join("data")), data_before, "the app's own folder was not touched");
}

#[test]
fn import_and_export_are_plain_functions_of_a_folder_and_a_picker() {
    // The same logic without a core: the folder is named by the caller and nothing else is read.
    let scratch = Scratch::new("plain");
    let picker = FakePicker::writes("p.txt", 2);
    assert_eq!(
        import(&scratch.home, picker.as_ref()),
        holzwave_host::handover::Outcome::Done { name: "p.txt".into(), size: 2, shown: "~/handover/p.txt".into(), renamed: false }
    );
    assert_eq!(export(&scratch.home, "handover/p.txt", picker.as_ref()), holzwave_host::handover::Outcome::Done {
        name: "p.txt".into(),
        size: 0,
        shown: "~/handover/p.txt".into(),
        renamed: false
    });
}

// ---------------------------------------------------------------------------------------------------------------------
// Test 8: the table

#[test]
fn both_commands_are_deny_by_default_rows_the_browser_does_not_have() {
    let (_dir, core) = {
        let scratch = Scratch::new("rows");
        (scratch.root.clone(), scratch.core(None))
    };
    for name in ["handover_in", "handover_out"] {
        let row = COMMANDS.iter().find(|row| row.name == name).unwrap_or_else(|| panic!("{name} is not in the table"));
        assert_eq!(
            (row.group, row.scope, row.cap, row.shell, row.web, row.mode),
            ("handover", Scope::None, CAP_DEFAULT, Shell::Generated, Web::Unavailable, Mode::Async),
            "{name}"
        );
        assert_eq!(row.answer.replace(' ', ""), "HandoverOutcomeMsg");
        assert_eq!(row.error, None, "{name} has no failure text: every failure is an outcome");
        assert!(BUILD_RS.contains(&format!("\"{name}\"")), "{name} is not in build.rs");
        assert!(CAPABILITY.contains(&format!("\"allow-{}\"", name.replace('_', "-"))), "{name} is not in the capability");
        for args in ["{}", "", r#"{"path":"a"}"#] {
            let outcome = lookup(name).expect("a row").call(&core, "main", args);
            assert_eq!(outcome, Outcome::Err(serde_json::to_string(NOT_IN_BROWSER).expect("json")), "{name} {args:?}");
        }
    }
    let args = |name: &str| -> Vec<(&str, &str)> { lookup(name).expect("row").args.iter().map(|a| (a.name, a.ty)).collect() };
    assert_eq!(args("handover_in"), Vec::<(&str, &str)>::new());
    assert_eq!(args("handover_out"), vec![("path", "String")]);
}
