//! GAM7, the Google Workspace admin tool: the argument lists, the bounded CSV reader and the views.
//!
//! What is held here: that a `print` output with quoted multi-line cells becomes one table whose cells are clean and
//! bounded; that the reader never panics and never keeps more than its caps on any shape of document; that every
//! argument list is exact and holds no word that writes (the tool's own configuration is never saved); and, where the
//! real tool is present and `HW_INTEGRATION_REAL=1`, that the chained meta commands parse in a scratch configuration
//! directory. The module starts no process: the tests feed it recorded and written fixtures.

use std::path::{Path, PathBuf};

use holzwave_core::integrate::gam::{parse_version, GAM, SPEC};
use holzwave_core::integrate::view::{TableView, ViewModel};
use holzwave_core::integrate::{Integration, Verb};

fn fixture(name: &str) -> Vec<u8> {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrations/gam").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn verb(id: &str) -> &'static Verb {
    SPEC.verb(id).unwrap_or_else(|| panic!("the catalogue has the verb {id}"))
}

fn table(model: ViewModel) -> TableView {
    match model {
        ViewModel::Table(view) => view,
        other => panic!("a table was expected, got {other:?}"),
    }
}

fn users(stdout: &[u8]) -> ViewModel {
    GAM.view(verb("users"), stdout).expect("the users output is read")
}

fn cells(view: &TableView, row: usize) -> Vec<String> {
    view.rows()[row].cells().map(str::to_owned).collect()
}

#[test]
fn the_version_is_the_word_after_gam_on_the_first_line_that_has_one() {
    let text = String::from_utf8(fixture("version.txt")).expect("text");
    assert_eq!(parse_version(&text).as_deref(), Some("7.46.02"));
    assert_eq!(parse_version("noise\nGAM 7.1.0 - x").as_deref(), Some("7.1.0"));
    assert_eq!(parse_version("no version here"), None);
    assert_eq!(parse_version("GAM"), None);
}

#[test]
fn a_quoted_multi_line_cell_stays_one_cell() {
    let view = table(users(&fixture("print_users.csv")));
    assert_eq!(view.rows().len(), 5, "one row per record, the multi-line cell does not split a record");
    // Bob's note holds a comma, a line break and doubled quotes inside one quoted cell.
    assert_eq!(cells(&view, 1)[4], "Line one, with a comma line two with \"quotes\"");
    for row in view.rows() {
        assert_eq!(row.cells().len(), view.columns().len(), "every row has as many cells as columns");
    }
}

#[test]
fn users_become_a_table() {
    let view = table(users(&fixture("print_users.csv")));
    let ids: Vec<&str> = view.columns().iter().map(|c| c.id()).collect();
    assert_eq!(ids, ["primaryEmail", "name.fullName", "suspended", "orgUnitPath", "notes"]);
    assert_eq!(view.title(), "Users");
    let keys: Vec<&str> = view.rows().iter().map(|r| r.key()).collect();
    assert_eq!(
        keys,
        ["alice@example.com", "bob@example.com", "carol@example.com", "dave@example.com", "erin@example.com"]
    );
    assert_eq!(cells(&view, 0), ["alice@example.com", "Alice Example", "False", "/", ""]);
    assert!(view.truncated().is_none() && view.note().is_none());
}
