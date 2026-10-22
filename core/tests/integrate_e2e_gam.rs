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

use holzwave_core::integrate::profiles::LocalFile;
use holzwave_core::integrate::service::{HostFacts, IntegrationService, RunAnswer};
use holzwave_core::integrate::{Selection, ToolId};
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

// ---------------------------------------------------------------------------------------------------------------------
// Sections, the domain and the JSON view.
// ---------------------------------------------------------------------------------------------------------------------

/// A listing of a configuration with three sections, the second one selected. Written, not recorded: the tool was only
/// ever recorded with an empty configuration directory, which has the one section `DEFAULT`.
const SECTIONS_TEXT: &str = "Config File: /path/to/gamcfg/gam.cfg\n  DEFAULT\n  lab\n  work *\n";

/// The users as `formatjson` prints them: the e-mail and one JSON object per row in the `JSON` column, quotes doubled.
/// Written: the shape is the tool's document, no real domain was ever read.
fn users_as_json() -> String {
    let objects = [
        r#"{"primaryEmail":"alice@example.com","name":{"fullName":"Alice Example"},"suspended":false,"orgUnitPath":"/"}"#,
        r#"{ "primaryEmail" : "carol@example.com" , "suspended" : true , "orgUnitPath" : "/Former" }"#,
    ];
    let mut out = String::from("primaryEmail,JSON\n");
    for object in objects {
        let email = object.split('"').nth(3).expect("an e-mail");
        out.push_str(&format!("{email},\"{}\"\n", object.replace('"', "\"\"")));
    }
    out
}

/// The domain document of the written fixture with two members the module does not know, one of them a secret.
fn domain_with_unknown_members() -> String {
    let mut document: Value = serde_json::from_str(&fixture("info_domain.json")).expect("the domain fixture is JSON");
    let members = document.as_object_mut().expect("an object");
    members.insert("oauthClientSecret".to_owned(), Value::from("SECRET-VALUE-0042"));
    members.insert("token".to_owned(), serde_json::json!({ "access": "TOKEN-VALUE-0042" }));
    serde_json::to_string_pretty(&document).expect("JSON")
}

/// The first row of the table `name` is built from, as a list of cells.
fn cells(value: &Value, row: usize) -> Vec<String> {
    value["view"]["rows"][row]["cells"]
        .as_array()
        .expect("cells")
        .iter()
        .map(|c| c.as_str().expect("text").to_owned())
        .collect()
}

fn case_sections_domain_and_json(rig: &Rig) {
    let service = rig.service();
    service.select("gam", Some(PANE), &section("work")).expect("a section for the pane");

    // The sections: the selected one is starred, a row has no action, and the list never selects or saves anything.
    // Written fixture: a configuration with several sections was never recorded (the reader is lenient on purpose).
    let fake = gam_printing(&rig.bin, SECTIONS_TEXT);
    let sections = service.run_view("gam", "sections", Some(PANE), &no_args()).expect("the sections are read");
    assert_eq!(argv_of(&fake), ["showsections"], "the listing is read from the tool itself, with no section chosen");
    let wire_sections = wire(&sections);
    assert_eq!(wire_sections["view"]["kind"], "table");
    let ids: Vec<&str> =
        wire_sections["view"]["columns"].as_array().expect("columns").iter().map(|c| c["id"].as_str().expect("id")).collect();
    assert_eq!(ids, ["section", "selected"]);
    assert_eq!(cells(&wire_sections, 0), ["DEFAULT", ""]);
    assert_eq!(cells(&wire_sections, 1), ["lab", ""]);
    assert_eq!(cells(&wire_sections, 2), ["work", "*"], "the selected section carries the star");
    for row in wire_sections["view"]["rows"].as_array().expect("rows") {
        assert_eq!(row["actions"].as_array().map(Vec::len), Some(0), "a section has no action: nothing switches the tool's default");
    }
    golden("gam_sections.json", &sections);

    // The domain: key-value rows of the members the module knows; the others are dropped, counted, and their values
    // are nowhere. Written fixture: the member names are an assumption from the tool's documents.
    let fake = gam_printing(&rig.bin, &domain_with_unknown_members());
    let domain = service.run_view("gam", "domain", Some(PANE), &no_args()).expect("the domain is read");
    assert_eq!(argv_of(&fake), ["select", "work", "info", "domain", "formatjson"]);
    let wire_domain = wire(&domain);
    assert_eq!(wire_domain["view"]["kind"], "kv");
    assert_eq!(wire_domain["view"]["note"], "2 other members are not shown.");
    let pairs: Vec<(&str, &str)> = wire_domain["view"]["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|r| (r["label"].as_str().expect("label"), r["value"].as_str().expect("value")))
        .collect();
    assert_eq!(pairs[0], ("Domain", "example.com"));
    assert!(pairs.contains(&("Alias", "alias-two.example.com, not verified")));
    let text = wire_domain.to_string();
    assert!(!text.contains("SECRET-VALUE-0042") && !text.contains("TOKEN-VALUE-0042"), "an unknown member never reaches the page");
    golden("gam_domain.json", &domain);

    // Output that is not JSON is shown as the plain text it is, never as a table of guesses.
    // (Another pane: an identical request within a second is served from the service's cache, by design.)
    let _fake = gam_printing(&rig.bin, "Domain: example.com\n");
    service.select("gam", Some(PANE + 1), &section("work")).expect("a section for the other pane");
    let odd = service.run_view("gam", "domain", Some(PANE + 1), &no_args()).expect("drift is a text, not a fault");
    assert_eq!(wire(&odd)["view"]["kind"], "text");

    // The users as JSON: the flag asks for `formatjson`, and the page gets one pretty, re-serialised array.
    let fake = gam_printing(&rig.bin, &users_as_json());
    let mut flag = no_args();
    flag.insert("as_json".to_owned(), "true".to_owned());
    let users = service.run_view("gam", "users", Some(PANE), &flag).expect("the users are read as JSON");
    let argv = argv_of(&fake);
    assert_eq!(argv.last().map(String::as_str), Some("formatjson"), "{argv:?}");
    assert_eq!(&argv[..5], ["select", "work", "config", "csv_output_row_limit", "2001"]);
    let wire_users = wire(&users);
    assert_eq!(wire_users["view"]["kind"], "text");
    assert_eq!(wire_users["view"]["format"], "json");
    assert_eq!(wire_users["view"]["truncated"], false);
    let shown = wire_users["view"]["text"].as_str().expect("text");
    let parsed: Value = serde_json::from_str(shown).expect("the text is one JSON document");
    assert_eq!(parsed.as_array().map(Vec::len), Some(2), "one array of the objects of the JSON column");
    assert!(shown.contains("\n  "), "pretty printed");
    assert!(!shown.contains("\"primaryEmail\" :"), "re-serialised, never the tool's own spacing");
    assert!(shown.len() <= 64 * 1024, "bounded: {} bytes", shown.len());
    golden("gam_users_json.json", &users);
}

#[test]
fn sections_domain_and_json_goldens_match() {
    case_sections_domain_and_json(&Rig::new());
}

// ---------------------------------------------------------------------------------------------------------------------
// A section belongs to a pane, and the tool's configuration directory is never written.
// ---------------------------------------------------------------------------------------------------------------------

/// Every path below `dir` and the bytes of every file in it, so that "unchanged" means: no file added, removed or changed.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).expect("list the directory").flatten() {
            let path = entry.path();
            let name = path.strip_prefix(base).expect("below the base").to_string_lossy().into_owned();
            if path.is_dir() {
                out.insert(format!("{name}/"), Vec::new());
                walk(base, &path, out);
            } else {
                out.insert(name, fs::read(&path).expect("read a file"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// A configuration directory of the tool as a person might have one: the file with the sections and the OAuth files.
fn scratch_config(rig: &Rig) -> PathBuf {
    let dir = rig.root.join("gamcfg");
    fs::create_dir(&dir).expect("the configuration directory");
    fs::write(dir.join("gam.cfg"), "[DEFAULT]\ncustomer_id = my_customer\n\n[work]\ndomain = example.com\n").expect("gam.cfg");
    fs::write(dir.join("oauth2.txt"), "{\"scratch\": true}\n").expect("oauth2.txt");
    fs::create_dir(dir.join("gamcache")).expect("the cache directory");
    fs::write(dir.join("gamcache").join("entry"), b"cached").expect("a cache file");
    dir
}

fn case_a_section_is_selected_per_pane(rig: &Rig) {
    let config = scratch_config(rig);
    let mut local = LocalFile::default();
    local.set_config_paths(ToolId::Gam, std::slice::from_ref(&config)).expect("a configuration path of this computer");
    local.save(&rig.data).expect("the machine-local file");
    let before = snapshot(&config);

    let fake = gam_printing(&rig.bin, &fixture("print_users.csv"));
    let service = rig.service();
    for (pane, choice) in [(3_u64, Some("work")), (4, Some("lab")), (5, None)] {
        let selection = choice.map_or_else(Selection::default, section);
        service.select("gam", Some(pane), &selection).expect("a selection for the pane");
        service.run_view("gam", "users", Some(pane), &no_args()).expect("the users are read for the pane");
    }

    let mut lists = rig.lists();
    lists.sort();
    assert_eq!(lists.len(), 3, "one start for each pane: {lists:?}");
    let runs = fs::read_to_string(format!("{}.runs", fake.path().display())).expect("the count of starts");
    assert_eq!(runs.lines().count(), 3, "and the record holds each of them");
    // The lists differ in the leading `select <section>` and in nothing else.
    let mut chosen = Vec::new();
    let mut rest = Vec::new();
    for list in &lists {
        if list[0] == "select" {
            chosen.push(list[1].clone());
            rest.push(list[2..].to_vec());
        } else {
            rest.push(list.clone());
        }
    }
    chosen.sort();
    assert_eq!(chosen, ["lab", "work"], "each pane that chose a section selects it, for that one process");
    assert!(rest.windows(2).all(|pair| pair[0] == pair[1]), "everything after the selection is the same: {rest:?}");
    assert_eq!(rest[0][..5], ["config", "csv_output_row_limit", "2001", "print", "users"]);

    // The machine-local configuration directory is the tool's directory for every start, and nothing in it changed.
    let env = fake.env();
    assert_eq!(env.get("GAMCFGDIR").map(String::as_str), config.to_str());
    assert_eq!(snapshot(&config), before, "no file of the configuration directory was added, removed or changed");
}

#[test]
fn a_section_is_selected_per_pane_and_the_configuration_directory_is_untouched() {
    case_a_section_is_selected_per_pane(&Rig::new());
}

// ---------------------------------------------------------------------------------------------------------------------
// The sweep: no argument list of any case holds a word that saves or writes.
// ---------------------------------------------------------------------------------------------------------------------

/// Makes the table of the cases of this file from their names, so that the sweep runs every one of them.
macro_rules! cases {
    ($($name:ident),+ $(,)?) => {
        const CASES: &[(&str, fn(&Rig))] = &[$((stringify!($name), $name)),+];
    };
}

cases![case_the_users_answer, case_sections_domain_and_json, case_a_section_is_selected_per_pane];

#[test]
fn no_recorded_argument_list_holds_a_save_or_write_word() {
    // Every case of the file is in the table: the sweep cannot miss one that was added later.
    let source = include_str!("integrate_e2e_gam.rs");
    let defined = source.lines().filter(|line| line.starts_with("fn case_")).count();
    assert_eq!(defined, CASES.len(), "every `fn case_...` of this file is listed in `cases!`");

    // Each case runs on a machine of its own; the lists its fake recorded are read back and checked together.
    let mut all: Vec<Vec<String>> = Vec::new();
    for (name, case) in CASES {
        let rig = Rig::new();
        case(&rig);
        let lists = rig.lists();
        assert!(!lists.is_empty(), "{name} recorded no argument list");
        all.extend(lists);
    }
    check_lists(&all);
    let words: usize = all.iter().map(Vec::len).sum();
    println!("swept {} argument lists, {words} words, against {} write words", all.len(), WRITE_WORDS.len());
    // The shapes that matter were among them, so a clean sweep over nothing cannot pass.
    let has = |word: &str| all.iter().any(|list| list.iter().any(|w| w == word));
    for word in ["select", "config", "print", "info", "showsections", "formatjson"] {
        assert!(has(word), "no recorded list holds {word}");
    }

    // The check itself bites: lists that save, write or misuse a meta command are refused.
    for bad in [
        &["select", "work", "save"][..],
        &["select", "work", "config", "csv_output_row_limit", "2001", "SAVE", "print", "users"],
        &["oauth", "create"],
        &["print", "users", "todrive"],
        &["config", "csv_output_row_limit", "5", "print", "users"],
        &["config", "customer_id", "x"],
        &["print", "users", "select", "work"],
        &["print", "users", "--config"],
    ] {
        let list: Vec<String> = bad.iter().map(|w| (*w).to_owned()).collect();
        assert!(std::panic::catch_unwind(|| check_list(&list)).is_err(), "{list:?} must be refused");
    }
}
