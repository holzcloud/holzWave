//! Talos end to end: a fake `talosctl` that prints recorded or written documents, the integration service over a
//! scratch computer, and the answers written as golden files that the page tests read back.
//!
//! No real cluster, no real `talosctl`, no network. `talosctl` was not installed where the module was written, so every
//! document the fake prints is a fixture that is `written` from the tool's documents (see the manifest of the fixtures),
//! and every assertion that rests on such a document says so in a comment line that starts with `ASSUMPTION`. The claim
//! of this file is "everything around the module works for Talos", never "the module reads what the real tool prints".
//!
//! The goldens are the real types of the core, serialised as the page receives them, with `fetched` set to a fixed number
//! and the id of a card's ticket to a fixed one. `UPDATE_FIXTURES=1` rewrites them; without it each file must equal what is
//! built now. Nothing here edits a golden by hand.
#![cfg(unix)]

mod common;
#[path = "common/fake_cli.rs"]
mod fake_cli;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use holzwave_core::integrate::args::{ContextName, ProfileId};
use holzwave_core::integrate::context::RowState;
use holzwave_core::integrate::profiles::IntegrationProfile;
use holzwave_core::integrate::service::{Done, FaultCode, HostFacts, IntegrationService, RunAnswer};
use holzwave_core::integrate::view::{Tone, ViewModel};
use holzwave_core::integrate::{Selection, ToolId, VerbEffect};
use holzwave_core::secrets::MemoryStore;
use serde::Serialize;
use serde_json::Value;

use common::scratch_dir;
use fake_cli::FakeCli;

/// The time every golden says it was read.
const FETCHED: u64 = 1_760_000_000;

/// The id every golden card has for its ticket.
const TICKET: &str = "0123456789abcdef0123456789abcdef";

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
        let root = scratch_dir("e2e-talos");
        let (bin, home, data) = (root.join("bin"), root.join("home"), root.join("data"));
        for dir in [&bin, &home, &data] {
            std::fs::create_dir(dir).expect("a directory of the rig");
        }
        Rig { root, bin, home, data, store: Arc::new(MemoryStore::default()) }
    }

    fn facts(&self) -> HostFacts {
        HostFacts::for_tests(&self.root, std::slice::from_ref(&self.bin), &self.home, &self.data)
    }

    fn service(&self) -> IntegrationService {
        IntegrationService::new(&self.data, self.store.clone(), self.facts(), &[])
    }

    fn service_over(&self, facts: HostFacts) -> IntegrationService {
        IntegrationService::new(&self.data, self.store.clone(), facts, &[])
    }

    /// A fake `talosctl` that prints `text`.
    fn talosctl(&self, text: &str) -> FakeCli {
        FakeCli::new(&self.bin, "talosctl").prints(text)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A recorded or written document of the tool, as text.
fn fixture(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrations").join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {relative}: {e}"))
}

fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn argv_of(fake: &FakeCli) -> Vec<String> {
    fake.argv().into_iter().map(|a| String::from_utf8(a).expect("text arguments")).collect()
}

/// A service over the rig whose pane 3 talks to `talos-prod`. A read is answered from the service's cache for a second
/// (single flight), so a test that changes what the fake prints and asks for the same view again needs a new service.
fn fresh(rig: &Rig) -> IntegrationService {
    let service = rig.service();
    service.select("talos", Some(3), &in_context("talos-prod")).expect("a selection");
    service
}

/// The selection of a pane that talks to this context.
fn in_context(context: &str) -> Selection {
    Selection { context: Some(context.into()), ..Selection::default() }
}

// ---- the goldens ---------------------------------------------------------------------------------------------------

fn golden_path(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures/integration").join(file)
}

fn update_fixtures() -> bool {
    std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
}

/// `value` with the members of every object in sorted order (so that a feature of `serde_json` somewhere in the
/// workspace cannot change the file), every `fetched` number fixed and the ticket of a card fixed.
fn normalised(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .into_iter()
                .map(|(name, item)| {
                    let item = match (name.as_str(), &item) {
                        ("fetched", Value::Number(_)) => Value::from(FETCHED),
                        ("ticket", Value::String(_)) => Value::from(TICKET),
                        _ => normalised(item),
                    };
                    (name, item)
                })
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(list) => Value::Array(list.into_iter().map(normalised).collect()),
        other => other,
    }
}

/// Writes `file` when `UPDATE_FIXTURES=1`, else requires it to equal the serialised `value`.
fn check_golden(file: &str, value: &impl Serialize) -> Value {
    let value = normalised(serde_json::to_value(value).expect("the answer is JSON"));
    let mut text = serde_json::to_string_pretty(&value).expect("pretty JSON");
    text.push('\n');
    let path = golden_path(file);
    if update_fixtures() {
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the fixture directory");
        std::fs::write(&path, &text).expect("write the golden");
        return value;
    }
    let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("{} cannot be read ({error}); run this test with UPDATE_FIXTURES=1 to write it", path.display())
    });
    assert_eq!(on_disk, text, "{file} is stale; run this test with UPDATE_FIXTURES=1 and review the diff");
    value
}

// ---- the tracer ----------------------------------------------------------------------------------------------------

#[test]
fn the_nodes_answer_is_written_as_a_golden_and_matches_it() {
    let rig = Rig::new();
    // ASSUMPTION: the three-object stream of get_members.json is written from the documents, not recorded.
    let fake = rig.talosctl(&fixture("talosctl/get_members.json"));
    let service = rig.service();
    service.select("talos", Some(3), &in_context("talos-prod")).expect("a selection");

    let answer: RunAnswer = service.run_view("talos", "nodes", Some(3), &args(&[])).expect("the nodes are read");
    assert_eq!(argv_of(&fake), ["get", "members", "-o", "json", "--context=talos-prod"], "the exact argument list");
    let ViewModel::Table(table) = &answer.view else { panic!("nodes is a table, got {:?}", answer.view) };
    assert_eq!(table.rows().len(), 3, "one row for each member of the stream");
    assert_eq!(answer.selection.selection.context.as_deref(), Some("talos-prod"));

    let golden = check_golden("talos_nodes.json", &answer);
    assert_eq!(golden["fetched"], FETCHED);
    assert_eq!(golden["view"]["kind"], "table");
    let columns: Vec<&str> =
        golden["view"]["columns"].as_array().expect("columns").iter().map(|c| c["label"].as_str().expect("a label")).collect();
    assert_eq!(columns, ["NODE", "HOSTNAME", "TYPE", "ADDRESSES"]);
}

// ---- the other views, the roles, the stream drift and the Local verb ---------------------------------------------

/// The table of an answer, or a failure that says what it was.
fn table_of(answer: &RunAnswer) -> &holzwave_core::integrate::view::TableView {
    let ViewModel::Table(table) = &answer.view else { panic!("a table, got {:?}", answer.view) };
    table
}

/// The cells of every row of a table.
fn cells_of(answer: &RunAnswer) -> Vec<Vec<String>> {
    table_of(answer).rows().iter().map(|row| row.cells().map(str::to_owned).collect()).collect()
}

/// The value and the tone of a labelled row of a key-value view.
fn kv_row(answer: &RunAnswer, label: &str) -> Option<(String, Tone)> {
    let ViewModel::Kv(view) = &answer.view else { panic!("a key-value view, got {:?}", answer.view) };
    view.rows().iter().find(|row| row.label() == label).map(|row| (row.value().to_owned(), row.tone()))
}

/// `config_info.json` with the `roles` member replaced (or removed): a variant that is written, like the original.
fn info_with_roles(roles: Option<Value>) -> String {
    let mut document: Value = serde_json::from_str(&fixture("talosctl/config_info.json")).expect("the info document");
    let object = document.as_object_mut().expect("an object");
    match roles {
        Some(roles) => object.insert("roles".to_owned(), roles),
        None => object.remove("roles"),
    };
    serde_json::to_string_pretty(&document).expect("JSON")
}

#[test]
fn services_status_contexts_and_info_goldens_match() {
    let rig = Rig::new();
    let service = rig.service();
    service.select("talos", Some(3), &in_context("talos-prod")).expect("a selection");

    // ASSUMPTION: get_services.json is written from the documents; the words `running`, `healthy` and the member names
    // behind them are the module's reading of it.
    let fake = rig.talosctl(&fixture("talosctl/get_services.json"));
    let services = service.run_view("talos", "services", Some(3), &args(&[])).expect("the services are read");
    assert_eq!(argv_of(&fake), ["get", "services", "-o", "json", "--context=talos-prod"]);
    let columns: Vec<&str> = table_of(&services).columns().iter().map(|c| c.label()).collect();
    assert_eq!(columns, ["NODE", "SERVICE", "STATE", "HEALTH"]);
    let unhealthy: Vec<_> = table_of(&services).rows().iter().filter(|row| row.cells().nth(3) == Some("unhealthy")).collect();
    assert_eq!(unhealthy.len(), 1, "the written stream holds one unhealthy service: {:?}", cells_of(&services));
    assert_eq!(unhealthy[0].tone(), Tone::Warn, "an unhealthy service is warn");
    for row in table_of(&services).rows().iter().filter(|row| row.cells().nth(3) == Some("healthy")) {
        assert_ne!(row.tone(), Tone::Warn, "a healthy service is not warn: {:?}", row.cells().collect::<Vec<_>>());
    }
    check_golden("talos_services.json", &services);

    // ASSUMPTION: get_machinestatus.json is written; one booting node with an unmet condition.
    let fake = rig.talosctl(&fixture("talosctl/get_machinestatus.json"));
    let status = service.run_view("talos", "machine_status", Some(3), &args(&[])).expect("the machine status is read");
    assert_eq!(argv_of(&fake), ["get", "machinestatus", "-o", "json", "--context=talos-prod"]);
    let columns: Vec<&str> = table_of(&status).columns().iter().map(|c| c.label()).collect();
    assert_eq!(columns, ["NODE", "STAGE", "READY"]);
    let not_ready: Vec<_> = table_of(&status).rows().iter().filter(|row| row.cells().nth(2) == Some("no")).collect();
    assert_eq!(not_ready.len(), 1, "one node is not ready: {:?}", cells_of(&status));
    assert_eq!(not_ready[0].tone(), Tone::Warn);
    check_golden("talos_machine_status.json", &status);

    // ASSUMPTION: config_contexts.txt is a written table (`CURRENT NAME ENDPOINTS NODES`, `*` on the current row).
    let fake = rig.talosctl(&fixture("talosctl/config_contexts.txt"));
    let contexts = service.run_view("talos", "contexts", Some(3), &args(&[])).expect("the contexts are read");
    assert_eq!(argv_of(&fake), ["config", "contexts"], "the contexts of the talosconfig, not of the pane");
    let rows = table_of(&contexts).rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].cells().next(), Some("*"), "the current context is marked");
    assert_eq!(rows[1].cells().next(), Some(""), "and the other is not");
    for row in rows {
        assert_eq!(row.actions().len(), 1, "every context row can be made the default");
        let action = &row.actions()[0];
        assert_eq!((action.verb(), action.effect()), ("use_context", VerbEffect::Local));
        let name = row.cells().nth(1).expect("a name");
        assert_eq!(action.args().collect::<Vec<_>>(), [("context", name)], "the action carries the row's own name");
    }
    check_golden("talos_contexts.json", &contexts);

    // ASSUMPTION: config_info.json, and above all its `roles` member, is a guess from the documents.
    let fake = rig.talosctl(&fixture("talosctl/config_info.json"));
    let reader = service.run_view("talos", "info", Some(3), &args(&[])).expect("the info is read");
    assert_eq!(argv_of(&fake), ["config", "info", "-o", "json", "--context=talos-prod"]);
    check_golden("talos_info.json", &reader);
    let _ = rig.talosctl(&info_with_roles(Some(serde_json::json!(["os:admin"]))));
    let admin = fresh(&rig).run_view("talos", "info", Some(3), &args(&[])).expect("the admin info is read");
    check_golden("talos_info_admin.json", &admin);
}

#[test]
fn the_role_is_said_in_words() {
    let rig = Rig::new();
    let read = |roles: Option<Value>| {
        let _ = rig.talosctl(&info_with_roles(roles));
        rig.service().run_view("talos", "info", None, &args(&[])).expect("the info is read")
    };
    // ASSUMPTION: the `roles` member is a guess from the documents. Each case below asserts what the module says for
    // the guess, so a real capture that differs replaces the fixture and not the rule.
    let words = |roles: Value| kv_row(&read(Some(roles)), "Access");

    assert_eq!(words(serde_json::json!(["os:reader"])), Some(("read-only (os:reader)".to_owned(), Tone::Normal)));
    assert_eq!(words(serde_json::json!(["os:admin"])), Some(("full access (os:admin)".to_owned(), Tone::Warn)));
    // A set that names the admin role is full access whatever else it holds, in any case and with any padding.
    assert_eq!(words(serde_json::json!(["os:reader", "os:admin"])), Some(("full access (os:admin)".to_owned(), Tone::Warn)));
    assert_eq!(words(serde_json::json!([" OS:Admin "])), Some(("full access (os:admin)".to_owned(), Tone::Warn)));
    assert_eq!(words(serde_json::json!("os:reader, os:admin")), Some(("full access (os:admin)".to_owned(), Tone::Warn)));
    assert_eq!(words(serde_json::json!(["os:operator"])), Some(("limited (os:operator)".to_owned(), Tone::Normal)));
    // A role the module does not know is shown as the raw list, never as read-only; an unreadable member claims nothing.
    assert_eq!(words(serde_json::json!(["os:reader", "os:backup"])), Some(("os:reader, os:backup".to_owned(), Tone::Normal)));
    let odd = read(Some(serde_json::json!(7)));
    assert_eq!(kv_row(&odd, "Access"), None, "a roles member that is not a list claims nothing");
    assert_eq!(kv_row(&odd, "Roles").map(|(value, _)| value).as_deref(), Some("?"));

    // ASSUMPTION of the written fixture: when the output has no `roles` member there is no Roles row and no Access row.
    let none = read(None);
    assert_eq!(kv_row(&none, "Roles"), None, "no roles member, no Roles row");
    assert_eq!(kv_row(&none, "Access"), None, "and nothing said about access");
    assert!(kv_row(&none, "Context").is_some(), "the rest of the view is still there");
}

#[test]
fn a_truncated_stream_shows_the_rows_and_the_note() {
    let rig = Rig::new();
    let nodes = |rig: &Rig| fresh(rig).run_view("talos", "nodes", Some(3), &args(&[]));

    // ASSUMPTION: the cut streams are written variants of get_members.json, not what a real tool printed when it died.
    let _ = rig.talosctl(&fixture("talosctl/get_members_truncated.json"));
    let truncated = nodes(&rig).expect("rows and a note, not a fault");
    let rows = cells_of(&truncated);
    assert!(!rows.is_empty() && rows.len() < 3, "the rows read before the cut: {rows:?}");
    let note = table_of(&truncated).note().expect("the core says the output was cut");
    assert_eq!(note, format!("The output was cut off; {} of the expected objects were read.", rows.len()));
    check_golden("talos_nodes_truncated.json", &truncated);

    // A stream whose second object is not JSON ends the same way: the first row and the same kind of note.
    let _ = rig.talosctl(&fixture("talosctl/get_members_second_not_json.json"));
    let second = nodes(&rig).expect("the first row and a note");
    assert_eq!(cells_of(&second).len(), 1);
    assert!(table_of(&second).note().is_some_and(|note| note.starts_with("The output was cut off")));

    // An unknown shape (no object at all) is the cleaned text of the output, with the core's note.
    let _ = rig.talosctl("WARNING: this is not what talosctl printed before\n");
    let unknown = nodes(&rig).expect("the text model, not a fault");
    let ViewModel::Text(text) = &unknown.view else { panic!("the text model, got {:?}", unknown.view) };
    assert!(text.text().contains("not what talosctl printed before"));
    assert!(text.note().is_some(), "with its note");
    check_golden("talos_unknown_shape.json", &unknown);

    // A list with nothing in it prints nothing, and that is an empty table, not a fault (the page says it is empty). A
    // document that is not a list and is empty is an error line.
    let _ = rig.talosctl("");
    let empty = nodes(&rig).expect("an empty list is an answer");
    assert!(table_of(&empty).rows().is_empty() && table_of(&empty).note().is_none(), "{:?}", empty.view);
    // A document that is not a list and is empty is the text model with an empty text and the note, never a panic.
    let none = fresh(&rig).run_view("talos", "info", Some(3), &args(&[])).expect("an empty document is an answer");
    let ViewModel::Text(text) = &none.view else { panic!("the text model, got {:?}", none.view) };
    assert!(text.text().is_empty() && text.note().is_some(), "{:?}", none.view);
    // No answer is the line of a tool that failed: what it said, cleaned.
    let _ = FakeCli::new(&rig.bin, "talosctl").prints_stderr("rpc error: code = Unavailable desc = connection refused\n").exits(1);
    let failed = nodes(&rig).expect_err("the tool failed");
    assert_eq!(failed.code, FaultCode::Failed);
    assert!(failed.text.starts_with("Talos reported an error: rpc error"), "{}", failed.text);
}

/// Forgets what a fake recorded, so that `started` says whether it ran since.
fn forget_record(fake: &FakeCli) {
    for ext in ["argv", "env"] {
        let _ = std::fs::remove_file(format!("{}.{ext}", fake.path().display()));
    }
}

#[test]
fn use_context_asks_first_with_the_files_name() {
    let rig = Rig::new();
    let service = rig.service();
    // ASSUMPTION A1: `config context <name>` writes the current context into the talosconfig; the card says only
    // `the talosconfig of this computer` and the stale mark after the run rests on the same guess.
    let fake = rig.talosctl(&fixture("talosctl/config_contexts.txt"));
    service.run_view("talos", "contexts", None, &args(&[])).expect("the default read");
    let before = service.context_rows(None, false);
    let talos = before.iter().find(|row| row.tool == ToolId::Talos).expect("a Talos row");
    assert_eq!(talos.context.as_deref(), Some("demo-prod"), "the tool's own context, from the table");
    assert!(!talos.stale);

    let fake = {
        forget_record(&fake);
        rig.talosctl("Switched.\n")
    };
    forget_record(&fake);
    let card = service.prepare("talos", "use_context", None, &args(&[("context", "demo-lab")])).expect("a card");
    assert!(!fake.started(), "a card starts nothing");
    assert_eq!(card.argv, ["config", "context", "--", "demo-lab"], "the name stands behind `--`");
    assert_eq!((card.tool, card.effect, card.program.as_str()), (ToolId::Talos, VerbEffect::Local, "talosctl"));
    assert!(card.target.contains(&("context".to_owned(), "demo-lab".to_owned())), "{:?}", card.target);
    let changes = card.target.iter().find(|(label, _)| label == "changes").expect("the card names what it changes");
    assert_eq!(changes.1, "the talosconfig of this computer");
    assert_eq!(card.guard.production, None, "no profile pins a context, so no production guard");
    check_golden("talos_card_use_context.json", &card);

    let done = service.confirm(&card.ticket, &card.hash).expect("the confirm runs it once");
    assert!(matches!(done, Done::Ran { .. }), "a Local verb runs, got {done:?}");
    assert_eq!(argv_of(&fake), card.argv, "what ran is exactly what the card showed");
    let after = service.context_rows(None, false);
    let talos = after.iter().find(|row| row.tool == ToolId::Talos).expect("a Talos row");
    assert!(talos.stale, "the tool's own context may have changed, so the default is marked");
    assert_eq!(talos.context.as_deref(), Some("demo-prod"), "the value is kept");
    assert_eq!(talos.state, RowState::Found);
    let again = service.confirm(&card.ticket, &card.hash).expect_err("a ticket approves once");
    assert_eq!(again.code, FaultCode::Ticket);

    // A profile that pins the context as production makes the card of a pane on it say so; another pane's card does not.
    let mut profile = IntegrationProfile::new(ProfileId::parse("talos-prod").expect("an id"), ToolId::Talos, "Talos production");
    profile.context = Some(ContextName::parse("talos-prod").expect("a name"));
    profile.production = true;
    service.save_profile(profile).expect("a profile");
    service.select("talos", Some(4), &in_context("talos-prod")).expect("a selection");
    service.select("talos", Some(5), &in_context("talos-stg")).expect("a selection");
    let guarded = service.prepare("talos", "use_context", Some(4), &args(&[("context", "demo-lab")])).expect("a card");
    assert_eq!(guarded.guard.production.as_deref(), Some("Talos production"));
    assert!(guarded.fields.contains(&("guard".to_owned(), "production".to_owned())));
    let calm = service.prepare("talos", "use_context", Some(5), &args(&[("context", "demo-lab")])).expect("a card");
    assert_eq!(calm.guard.production, None);
}

// ---- the terminals, the config path variable and the node actions ------------------------------------------------

/// The node every card and launch below talks to: the address of the first member of the written members fixture.
const NODE: &str = "192.0.2.11";

fn os(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

/// A machine whose environment holds a variable of the app and a loader variable, and every such name the test process
/// has itself (a test run from a holzWave pane has the app's socket variables, which is the case the removal is for).
fn dirty_facts(rig: &Rig) -> HostFacts {
    let mut facts = rig.facts().with_env("HOLZWAVE_TOKEN", "app-secret").with_env("LD_PRELOAD", "/tmp/x.so");
    for (name, value) in std::env::vars_os() {
        let (Some(name), Some(value)) = (name.to_str(), value.to_str()) else { continue };
        if name.starts_with("HOLZWAVE_") || name.starts_with("DYLD_") || name.starts_with("LD_") {
            facts = facts.with_env(name, value);
        }
    }
    facts
}

/// `integration_local.json` of this computer with a talosconfig path; returns the path. It must be written before the
/// service is made, which reads the file once.
fn write_local(rig: &Rig) -> String {
    let path = rig.home.join(".talos").join("config").to_string_lossy().into_owned();
    let document = serde_json::json!({ "version": 1, "binaries": {}, "config_paths": { "talos": [path] } });
    std::fs::write(rig.data.join("integration_local.json"), document.to_string()).expect("the local file");
    path
}

#[test]
fn dmesg_launches_without_a_ticket_and_dashboard_asks() {
    let rig = Rig::new();
    let fake = rig.talosctl("");
    let service = rig.service_over(dirty_facts(&rig));
    service.select("talos", Some(3), &in_context("talos-prod")).expect("a selection");
    let node = args(&[("node", NODE)]);

    // A read terminal launches at once: no card, no ticket, and nothing is started by the service.
    let dmesg = service.launch("talos", "dmesg", Some(3), &node).expect("dmesg launches");
    assert_eq!(dmesg.program, fake.path());
    assert_eq!(dmesg.args, os(&["dmesg", "--follow", "--context=talos-prod", "--nodes=192.0.2.11"]), "context before node");
    assert!(!fake.started(), "a launch only describes the program");
    // A node that is not a node never becomes an argument: the launch is refused before it exists.
    for hostile in ["--talosconfig=/etc/passwd", "-e", "$(touch /tmp/never)", "192.0.2.11/24", ""] {
        let fault = service.launch("talos", "dmesg", Some(3), &args(&[("node", hostile)])).expect_err(hostile);
        assert_eq!(fault.code, FaultCode::Refused, "{hostile:?}");
        assert!(!fault.text.contains("touch"), "the refusal never repeats the input: {}", fault.text);
    }

    // The dashboard changes nothing but opens a session, so it asks: no launch without the card.
    let fault = service.launch("talos", "dashboard", Some(3), &node).expect_err("not a launch without a card");
    assert_eq!(fault.code, FaultCode::Refused);
    let card = service.prepare("talos", "dashboard", Some(3), &node).expect("a card");
    assert_eq!((card.tool, card.effect, card.program.as_str()), (ToolId::Talos, VerbEffect::Exec, "talosctl"));
    assert_eq!(card.argv, ["dashboard", "--context=talos-prod", "--nodes=192.0.2.11"]);
    assert_eq!(card.guard.production, None, "no profile pins the context, so no production guard");
    assert!(card.target.contains(&("context".to_owned(), "talos-prod".to_owned())), "{:?}", card.target);
    assert!(card.target.contains(&("node".to_owned(), NODE.to_owned())), "{:?}", card.target);
    assert!(!fake.started(), "a card starts nothing");
    check_golden("talos_card_dashboard.json", &card);

    let done = service.confirm(&card.ticket, &card.hash).expect("the confirm gives a launch");
    let Done::Launch(launch) = done else { panic!("an exec verb launches, got {done:?}") };
    assert!(!fake.started(), "the confirm started nothing: the pane does");
    assert_eq!(launch.args, os(&["dashboard", "--context=talos-prod", "--nodes=192.0.2.11"]));
    assert_eq!(launch.args, card.argv.iter().map(OsString::from).collect::<Vec<_>>(), "what launches is what the card showed");
    let names: Vec<&String> = launch.env.iter().map(|(name, _)| name).collect();
    assert!(names.iter().all(|name| !name.starts_with("HOLZWAVE_")), "no variable of the app: {names:?}");
    assert!(launch.env_remove.iter().any(|name| name == "HOLZWAVE_TOKEN"), "the pane removes what it inherits: {:?}", launch.env_remove);
    assert!(launch.env_remove.iter().any(|name| name == "LD_PRELOAD"), "{:?}", launch.env_remove);
    let again = service.confirm(&card.ticket, &card.hash).expect_err("a ticket approves once");
    assert_eq!(again.code, FaultCode::Ticket);
}

#[test]
fn the_local_config_path_becomes_talosconfig() {
    let rig = Rig::new();
    let talosconfig = write_local(&rig);
    let fake = rig.talosctl(&fixture("talosctl/get_members.json"));
    let service = rig.service_over(dirty_facts(&rig));
    assert!(service.notes().is_empty(), "the local file is read without a note: {:?}", service.notes());
    service.select("talos", Some(3), &in_context("talos-prod")).expect("a selection");

    // A read: the path reaches the tool as its own variable, and the app's variables do not.
    service.run_view("talos", "nodes", Some(3), &args(&[])).expect("the nodes are read");
    let env = fake.env();
    assert_eq!(env.get("TALOSCONFIG"), Some(&talosconfig), "the machine-local path is the tool's variable");
    assert!(env.keys().all(|name| !name.starts_with("HOLZWAVE_")), "{:?}", env.keys().collect::<Vec<_>>());
    assert!(!env.contains_key("LD_PRELOAD"), "nor does a loader variable");
    assert!(argv_of(&fake).iter().all(|arg| !arg.contains(&talosconfig) && !arg.starts_with("--talosconfig")), "the path is never an argument");

    // The two terminals carry it in the pane's environment, once.
    let node = args(&[("node", NODE)]);
    let dmesg = service.launch("talos", "dmesg", Some(3), &node).expect("dmesg launches");
    let dashboard = {
        let card = service.prepare("talos", "dashboard", Some(3), &node).expect("a card");
        let Done::Launch(launch) = service.confirm(&card.ticket, &card.hash).expect("a launch") else { panic!("a launch") };
        launch
    };
    for (what, launch) in [("dmesg", &dmesg), ("dashboard", &dashboard)] {
        let found: Vec<&(String, String)> = launch.env.iter().filter(|(name, _)| name == "TALOSCONFIG").collect();
        assert_eq!(found.len(), 1, "{what}: exactly one TALOSCONFIG");
        assert_eq!(found[0].1, talosconfig, "{what}");
        assert!(launch.args.iter().all(|arg| !arg.to_string_lossy().contains("talosconfig")), "{what}: {:?}", launch.args);
    }

    // Without the file nothing sets the variable: the tool reads its own default.
    let plain = Rig::new();
    let fake = plain.talosctl(&fixture("talosctl/get_members.json"));
    plain.service().run_view("talos", "nodes", None, &args(&[])).expect("the nodes are read");
    assert!(!fake.env().contains_key("TALOSCONFIG"), "no path, no variable");
}

#[test]
fn node_rows_carry_actions_only_for_valid_names() {
    // A stream of members written for this test from the first object of get_members.json (a written variant): the
    // second member has a hostile node and a markup host name, the third a valid address and a shell-syntax host name,
    // the fourth a mask and an option as names.
    let template = serde_json::Deserializer::from_str(&fixture("talosctl/get_members.json"))
        .into_iter::<Value>()
        .next()
        .expect("a first object")
        .expect("JSON");
    let members = |entries: &[(&str, &str)]| -> String {
        entries
            .iter()
            .map(|(node, host)| {
                let mut item = template.clone();
                item["node"] = serde_json::json!(node);
                item["spec"]["hostname"] = serde_json::json!(host);
                item["spec"]["addresses"] = serde_json::json!([node]);
                serde_json::to_string_pretty(&item).expect("JSON")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let hostile_markup = "<img src=x onerror=alert(1)>";
    let rig = Rig::new();
    let _ = rig.talosctl(&members(&[
        (NODE, "node-1.example.com"),
        ("--talosconfig=/etc/passwd", hostile_markup),
        ("192.0.2.13", "$(touch /tmp/never)"),
        ("10.0.4.12/24", "-e"),
    ]));
    let answer = fresh(&rig).run_view("talos", "nodes", Some(3), &args(&[])).expect("the nodes are read");
    let rows = table_of(&answer).rows();
    assert_eq!(rows.len(), 4);

    let shape = |row: &holzwave_core::integrate::view::Row| -> Vec<(String, String, VerbEffect, Vec<(String, String)>)> {
        row.actions()
            .iter()
            .map(|a| {
                let args = a.args().map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
                (a.verb().to_owned(), a.label().to_owned(), a.effect(), args)
            })
            .collect()
    };
    let offered = |node: &str| -> Vec<(String, String, VerbEffect, Vec<(String, String)>)> {
        [
            ("services", "Services", VerbEffect::Read),
            ("machine_status", "Machine status", VerbEffect::Read),
            // The real label of the verb; the page shows it as written and adds the ellipsis to the Exec one.
            ("dmesg", "Kernel log", VerbEffect::Read),
            ("dashboard", "Dashboard", VerbEffect::Exec),
        ]
        .into_iter()
        .map(|(verb, label, effect)| (verb.to_owned(), label.to_owned(), effect, vec![("node".to_owned(), node.to_owned())]))
        .collect()
    };
    assert_eq!(shape(&rows[0]), offered(NODE), "a valid row offers exactly these four, in this order");
    assert!(rows[1].actions().is_empty(), "a hostile node and a markup host name: no action");
    assert_eq!(shape(&rows[2]), offered("192.0.2.13"), "a valid address with a hostile host name: the address only");
    assert!(rows[3].actions().is_empty(), "a mask and an option: no action");
    // The hostile text is still shown, as text; it only has no button and never an argument.
    assert_eq!(rows[1].cells().nth(1), Some(hostile_markup));
    for row in rows {
        for action in row.actions() {
            assert!(action.args().all(|(_, value)| !value.contains("touch") && !value.starts_with('-') && !value.contains('<')));
        }
    }
    check_golden("talos_nodes_hostile.json", &answer);
}

// ---- the dashboard in a real pane (needs the engine) --------------------------------------------------------------

#[cfg(feature = "ghostty")]
mod pane {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;
    use common::wait_until;
    use holzwave_core::engine::{DomKey, Mods};
    use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
    use holzwave_core::wire::{FrameMsg, RowMsg};

    const WAIT: Duration = Duration::from_secs(10);

    /// Collects everything a pane sends to its sink.
    #[derive(Clone, Default)]
    pub struct Probe {
        log: Arc<Mutex<Vec<PaneEvent>>>,
    }

    impl Probe {
        pub fn sink(&self) -> impl FnMut(PaneEvent) + Send + 'static {
            let log = self.log.clone();
            move |event| log.lock().unwrap().push(event)
        }

        pub fn events(&self) -> Vec<PaneEvent> {
            self.log.lock().unwrap().clone()
        }

        pub fn has_text(&self, needle: &str) -> bool {
            let rows = |f: &FrameMsg| f.changed.iter().any(|r| row_text(r).contains(needle));
            self.events().into_iter().any(|e| matches!(e, PaneEvent::Frame(f) if rows(&f)))
        }

        pub fn helds(&self) -> Vec<Option<u32>> {
            self.events().into_iter().filter_map(|e| if let PaneEvent::Held { code } = e { Some(code) } else { None }).collect()
        }

        pub fn exits(&self) -> Vec<Option<u32>> {
            self.events().into_iter().filter_map(|e| if let PaneEvent::Exited { code } = e { Some(code) } else { None }).collect()
        }
    }

    fn row_text(row: &RowMsg) -> String {
        row.runs.iter().flat_map(|run| run.0.chars()).filter(|c| *c != '\0').collect()
    }

    /// The launch that a confirmed dashboard card gives, on a rig whose `talosctl` is `fake`.
    pub fn dashboard_launch(rig: &Rig) -> holzwave_core::integrate::run::Launch {
        let service = rig.service_over(dirty_facts(rig));
        service.select("talos", Some(3), &in_context("talos-prod")).expect("a selection");
        let card = service.prepare("talos", "dashboard", Some(3), &args(&[("node", NODE)])).expect("a card");
        let Done::Launch(launch) = service.confirm(&card.ticket, &card.hash).expect("a launch") else { panic!("a launch") };
        launch
    }

    fn spawn(launch: &holzwave_core::integrate::run::Launch, probe: &Probe) -> PaneHandle {
        // The size, the cell and the scrollback are the pane's; the program, the arguments and the environment are the launch's.
        let mut spec = PaneSpec::new("/bin/zsh", 100, 24);
        spec.env = vec![("HOLZWAVE_TOKEN".into(), "app-token".into())];
        launch.apply_to(&mut spec);
        PaneHandle::spawn_ghostty(1, spec, probe.sink()).expect("the pane starts")
    }

    #[test]
    fn dashboard_runs_the_exact_argument_list_in_a_real_pane() {
        let rig = Rig::new();
        let talosconfig = write_local(&rig);
        let fake = rig.talosctl("dashboard drawn\n");
        let launch = dashboard_launch(&rig);
        assert!(!fake.started(), "nothing ran before the pane");

        let probe = Probe::default();
        let pane = spawn(&launch, &probe);
        // A quick success is held for the linger time, so the notice and the record are both there to read.
        assert!(wait_until(WAIT, || !probe.helds().is_empty()), "the pane was not held: {:?}", probe.events());
        assert!(probe.has_text("dashboard drawn"), "the tool's own words are on screen");

        let argv: Vec<String> = fake.argv().iter().map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        assert_eq!(argv, ["dashboard", "--context=talos-prod", "--nodes=192.0.2.11"], "the exact list, nothing added by the pane");
        let seen = fake.env();
        let names: Vec<&String> = seen.keys().collect();
        assert!(!names.iter().any(|name| name.starts_with("HOLZWAVE_")), "no variable of the app, not even the one the spec had: {names:?}");
        assert!(!seen.contains_key("LD_PRELOAD"), "{names:?}");
        assert_eq!(seen.get("TALOSCONFIG"), Some(&talosconfig), "the machine-local path is the tool's variable");
        let path = seen.get("PATH").expect("the rebuilt PATH");
        assert!(path.starts_with(&format!("{}:", rig.bin.display())), "the tool's directory first: {path}");
        pane.close();
        assert!(pane.join(Duration::from_secs(5)));
    }

    #[test]
    fn a_failing_dashboard_is_held_until_enter() {
        let rig = Rig::new();
        let fake = FakeCli::new(&rig.bin, "talosctl").prints_stderr("rpc error: code = Unavailable desc = connection refused\n").exits(1);
        let launch = dashboard_launch(&rig);
        let probe = Probe::default();
        let pane = spawn(&launch, &probe);

        assert!(wait_until(WAIT, || !probe.helds().is_empty()), "no Held event: {:?}", probe.events());
        assert!(probe.has_text("rpc error: code = Unavailable"), "the tool's line is on screen");
        assert!(wait_until(WAIT, || probe.has_text("[holzWave] exited with status 1. Press Enter to close.")), "the notice line is on screen");
        assert_eq!(probe.helds(), [Some(1)], "one Held with the tool's status");
        assert!(probe.exits().is_empty(), "no Exited yet");
        assert_eq!(fake.argv().len(), 3, "it did run, with the card's list");

        // Waiting, a letter and a modified Enter close nothing.
        std::thread::sleep(Duration::from_millis(1200));
        let key = |code: &str, name: &str, mods: Mods| DomKey { code: code.into(), key: name.into(), mods, release: false, ..DomKey::default() };
        for press in [key("KeyA", "a", Mods::NONE), key("Enter", "Enter", Mods::SHIFT), key("Escape", "Escape", Mods::NONE)] {
            let _ = pane.key(press);
        }
        std::thread::sleep(Duration::from_millis(500));
        assert!(probe.exits().is_empty(), "the pane ended without Enter: {:?}", probe.events());

        pane.key(key("Enter", "Enter", Mods::NONE)).expect("the held pane takes a key");
        assert!(wait_until(WAIT, || !probe.exits().is_empty()), "Enter did not end the pane: {:?}", probe.events());
        assert!(pane.join(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(probe.exits(), [Some(1)], "exactly one Exited, with the tool's status");
    }
}
