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
/// (single flight, 08-17), so a test that changes what the fake prints and asks for the same view again needs a new service.
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
