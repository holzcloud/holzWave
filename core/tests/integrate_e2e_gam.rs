//! GAM7 end to end with a fake `gam` and no domain: the service over a scratch machine, the real tool module, the real CSV
//! reader, and the answers written as golden files that the page's tests read.
//!
//! The fake prints written fixtures and documents this file generates, records every argument list it was started with (one
//! file per start, next to the script) and the environment of its last start. What is held here is mostly negative or
//! bounded: GAM never saves its configuration, never reads more than its caps, never puts a raw control character on screen,
//! and is found where it lives even when no `PATH` names the place. The goldens under `app/tests/fixtures/integration/` are
//! the answers as the page receives them, with the time of the read normalised; `UPDATE_FIXTURES=1` rewrites them.
//!
//! Nothing here touches a real Google Workspace domain, a real `gam` or the network. The tests are Unix only.
#![cfg(unix)]

mod common;
#[path = "common/fake_cli.rs"]
mod fake_cli;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use holzwave_core::integrate::service::{HostFacts, IntegrationService, RunAnswer};
use holzwave_core::integrate::Selection;
use holzwave_core::secrets::MemoryStore;
use serde_json::Value;

use common::scratch_dir;
use fake_cli::FakeCli;

/// The time every golden carries as the time of the read.
const FETCHED: u64 = 1_760_000_000;

/// The pane the reads are made for.
const PANE: u64 = 7;

/// A scratch computer: a `bin` directory on the `PATH`, a home, a data directory, removed when the test ends.
struct Rig {
    root: PathBuf,
    bin: PathBuf,
    home: PathBuf,
    data: PathBuf,
    store: Arc<MemoryStore>,
}

impl Rig {
    fn new() -> Rig {
        let root = scratch_dir("gam-e2e");
        let (bin, home, data) = (root.join("bin"), root.join("home"), root.join("data"));
        for dir in [&bin, &home, &data] {
            fs::create_dir(dir).expect("a directory of the rig");
        }
        Rig { root, bin, home, data, store: Arc::new(MemoryStore::default()) }
    }

    fn facts(&self) -> HostFacts {
        HostFacts::for_tests(&self.root, std::slice::from_ref(&self.bin), &self.home, &self.data)
    }

    fn service(&self) -> IntegrationService {
        IntegrationService::new(&self.data, self.store.clone(), self.facts(), &[])
    }

    /// Every argument list a recording fake of this rig was started with, in no particular order.
    fn lists(&self) -> Vec<Vec<String>> {
        let mut found = Vec::new();
        collect_lists(&self.root, &mut found);
        found
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // Every test checks its own lists before its machine goes away; the sweep checks all of them together.
        if !std::thread::panicking() {
            check_lists(&self.lists());
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The recorded lists below `dir`: a file `gam.args.<pid>` holds the arguments of one start as NUL-terminated bytes.
fn collect_lists(dir: &Path, found: &mut Vec<Vec<String>>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            collect_lists(&path, found);
        } else if name.starts_with("gam.args.") {
            let bytes = fs::read(&path).expect("a recorded list");
            let mut parts: Vec<Vec<u8>> = bytes.split(|b| *b == 0).map(<[u8]>::to_vec).collect();
            if parts.last().is_some_and(Vec::is_empty) {
                parts.pop();
            }
            found.push(parts.into_iter().map(|p| String::from_utf8(p).expect("text arguments")).collect());
        }
    }
}

/// Makes a fake record every start in a file of its own: the rig's script sources `<name>.conf` before it records
/// anything, and a sourced file sees the script's arguments, so two more lines there write one list per start and count
/// the starts. Call it after the last builder method (a builder rewrites the file).
fn recording(fake: FakeCli) -> FakeCli {
    use std::io::Write;
    let path = format!("{}.conf", fake.path().display());
    let mut conf = fs::OpenOptions::new().append(true).open(path).expect("the settings of the fake");
    writeln!(conf, "for a in \"$@\"; do printf '%s\\0' \"$a\"; done > \"$d/$n.args.$$\"").expect("a line");
    writeln!(conf, "echo run >> \"$d/$n.runs\"").expect("a line");
    fake
}

/// A recording fake `gam` in `dir` that prints `text`.
fn gam_printing(dir: &Path, text: &str) -> FakeCli {
    recording(FakeCli::new(dir, "gam").prints(text))
}

/// The arguments of the last start of a fake.
fn argv_of(fake: &FakeCli) -> Vec<String> {
    fake.argv().into_iter().map(|a| String::from_utf8(a).expect("text arguments")).collect()
}

/// A recorded fixture of the tool, as text.
fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrations/gam").join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

fn no_args() -> BTreeMap<String, String> {
    BTreeMap::new()
}

fn section(name: &str) -> Selection {
    Selection { section: Some(name.to_owned()), ..Selection::default() }
}

// ---------------------------------------------------------------------------------------------------------------------
// The goldens.
// ---------------------------------------------------------------------------------------------------------------------

/// Sets every time of a read to [`FETCHED`]: the answer's and the model's.
fn normalise(value: &mut Value) {
    if let Some(answer) = value.as_object_mut() {
        if answer.get("fetched").is_some_and(Value::is_number) {
            answer.insert("fetched".to_owned(), Value::from(FETCHED));
        }
        if let Some(view) = answer.get_mut("view").and_then(Value::as_object_mut) {
            if view.get("fetched").is_some_and(Value::is_number) {
                view.insert("fetched".to_owned(), Value::from(FETCHED));
            }
        }
    }
}

/// The answer as the page receives it: the real types serialised, the time normalised.
fn wire(answer: &RunAnswer) -> Value {
    let mut value = serde_json::to_value(answer).expect("an answer is JSON");
    normalise(&mut value);
    value
}

fn golden_path(name: &str) -> PathBuf {
    common::fixtures_dir().join("integration").join(name)
}

/// Compares the answer with the golden file `name`, or writes the file with `UPDATE_FIXTURES=1`.
fn golden(name: &str, answer: &RunAnswer) {
    let mut text = serde_json::to_string_pretty(&wire(answer)).expect("pretty JSON");
    text.push('\n');
    let path = golden_path(name);
    if common::update_fixtures() {
        static WRITES: AtomicUsize = AtomicUsize::new(0);
        fs::create_dir_all(path.parent().expect("a directory")).expect("the fixtures directory");
        // The sweep runs the same cases as the tests on another thread: write beside the file and rename.
        let temporary = path.with_extension(format!("tmp{}-{}", std::process::id(), WRITES.fetch_add(1, Ordering::Relaxed)));
        fs::write(&temporary, &text).expect("write the golden");
        fs::rename(&temporary, &path).expect("put the golden in place");
        return;
    }
    let want = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("golden {name}: {e}; run with UPDATE_FIXTURES=1 and review the diff"));
    assert_eq!(text, want, "golden {name} differs from what the core builds now; review, then UPDATE_FIXTURES=1");
}

// ---------------------------------------------------------------------------------------------------------------------
// What no argument list may hold.
// ---------------------------------------------------------------------------------------------------------------------

/// The words that write the tool's state or a domain, or save a choice.
const WRITE_WORDS: &[&str] = &["save", "oauth", "create", "update", "delete", "add", "remove", "todrive"];

/// One list is clean: no write word at any place, no option-like word, `select` only first, and `config` only as the one
/// row-limit variable the module sets.
fn check_list(list: &[String]) {
    for (at, word) in list.iter().enumerate() {
        assert!(
            !WRITE_WORDS.iter().any(|w| word.eq_ignore_ascii_case(w)),
            "a write word {word:?} at {at} in {list:?}"
        );
        assert!(!word.starts_with('-'), "an option-like word {word:?} in {list:?}");
        match word.as_str() {
            "config" => assert_eq!(
                list.get(at..at + 3).map(<[String]>::to_vec),
                Some(vec!["config".to_owned(), "csv_output_row_limit".to_owned(), "2001".to_owned()]),
                "config is only ever the row limit: {list:?}"
            ),
            "select" => assert_eq!(at, 0, "select is only ever first: {list:?}"),
            _ => {}
        }
    }
}

fn check_lists(lists: &[Vec<String>]) {
    lists.iter().for_each(|list| check_list(list));
}

// ---------------------------------------------------------------------------------------------------------------------
// The tracer: the users answer.
// ---------------------------------------------------------------------------------------------------------------------

fn case_the_users_answer(rig: &Rig) {
    let fake = gam_printing(&rig.bin, &fixture("print_users.csv"));
    let service = rig.service();
    service.select("gam", Some(PANE), &section("work")).expect("a section for the pane");
    let answer = service.run_view("gam", "users", Some(PANE), &no_args()).expect("the users are read");

    let argv = argv_of(&fake);
    let head = ["select", "work", "config", "csv_output_row_limit", "2001", "print", "users", "fields"];
    assert_eq!(&argv[..head.len()], head, "the section and the row limit come before the command: {argv:?}");
    assert_eq!(argv.len(), head.len() + 1, "and the fields are the one last word: {argv:?}");
    assert!(!argv.iter().any(|a| a == "save"), "{argv:?}");

    // The multi-line quoted cell of the recorded shape is one cell on one line.
    let wire = wire(&answer);
    let rows = wire["view"]["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 5, "one row per user");
    for row in rows {
        for cell in row["cells"].as_array().expect("cells") {
            assert!(!cell.as_str().expect("text").contains('\n'), "a cell is on one line: {cell}");
        }
    }
    assert_eq!(rows[1]["cells"][4], "Line one, with a comma line two with \"quotes\"");
    assert_eq!(wire["selection"]["selection"]["section"], "work");
    golden("gam_users.json", &answer);
}

#[test]
fn the_users_answer_is_written_as_a_golden_and_matches_it() {
    case_the_users_answer(&Rig::new());
}
