//! GAM7, the Google Workspace admin tool: the argument lists, the bounded CSV reader and the views.
//!
//! What is held here: that a `print` output with quoted multi-line cells becomes one table whose cells are clean and
//! bounded; that the reader never panics and never keeps more than its caps on any shape of document; that every
//! argument list is exact and holds no word that writes (the tool's own configuration is never saved); and, where the
//! real tool is present and `HW_INTEGRATION_REAL=1`, that the chained meta commands parse in a scratch configuration
//! directory. The module starts no process: the tests feed it recorded and written fixtures.

use std::path::{Path, PathBuf};

use holzwave_core::integrate::gam::{domain_json_text, parse_version, GAM, MAX_INPUT, SPEC};
use holzwave_core::integrate::view::{
    TableView, TextFormat, ViewModel, MAX_CELL, MAX_COLUMNS, MAX_ROWS, MAX_TEXT,
};
use holzwave_core::integrate::{Integration, ParseError, Verb};

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

// ---------------------------------------------------------------------------------------------------------------------
// The caps and the hostile shapes.
// ---------------------------------------------------------------------------------------------------------------------

/// Every string a model holds, so that a test can look at all of them.
fn every_text(model: &ViewModel) -> Vec<String> {
    let value = serde_json::to_value(model).expect("a model is JSON");
    let mut out = Vec::new();
    fn walk(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::String(text) => out.push(text.clone()),
            serde_json::Value::Array(items) => items.iter().for_each(|item| walk(item, out)),
            serde_json::Value::Object(map) => {
                for (key, item) in map {
                    out.push(key.clone());
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    walk(&value, &mut out);
    out
}

/// No raw control character other than the line feed and the tab of a text block, and none of the characters that
/// spoof (direction, zero width, tag).
fn assert_clean(model: &ViewModel, label: &str) {
    let in_text_block = matches!(model, ViewModel::Text(_));
    for text in every_text(model) {
        for c in text.chars() {
            let allowed_break = in_text_block && matches!(c, '\n' | '\t');
            assert!(!c.is_control() || allowed_break, "{label}: a control character {:?} in {text:?}", c as u32);
            let spoof = matches!(c as u32, 0x202A..=0x202E | 0x2066..=0x2069 | 0x200B..=0x200F | 0xFEFF | 0xE0000..=0xE007F);
            assert!(!spoof, "{label}: a hidden character U+{:04X} in {text:?}", c as u32);
        }
    }
    model.check().unwrap_or_else(|e| panic!("{label}: the model breaks a rule of its contract: {e}"));
}

fn csv_of(columns: usize, rows: usize) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(&(0..columns).map(|c| format!("c{c}")).collect::<Vec<_>>().join(","));
    out.push('\n');
    for r in 0..rows {
        out.push_str(&(0..columns).map(|c| format!("v{r}x{c}")).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out.into_bytes()
}

#[test]
fn wide_and_long_documents_are_cut_with_notes() {
    // 70 columns keep 64 and say so.
    let view = table(users(&csv_of(70, 3)));
    assert_eq!(view.columns().len(), MAX_COLUMNS);
    assert_eq!(view.note(), Some("First 64 of 70 columns."));
    assert_eq!(cells(&view, 2).len(), MAX_COLUMNS);
    assert_eq!(cells(&view, 2)[63], "v2x63", "the 64th column is the last one kept");

    // 3000 rows keep 2000 and the model says that more exist.
    let view = table(users(&csv_of(2, 3000)));
    assert_eq!(view.rows().len(), MAX_ROWS);
    let cut = view.truncated().expect("a cut table says so");
    assert_eq!(cut.shown() as usize, MAX_ROWS);
    assert_eq!(view.note(), Some("More rows exist than are shown."));
    assert_eq!(view.rows()[MAX_ROWS - 1].key(), "row 2000", "no primary e-mail column: the key is the row number");

    // The edge: 2000 rows are all the rows, 2001 prove that there are more (GAM's own limit gives the same note).
    assert!(table(users(&csv_of(2, 2000))).truncated().is_none());
    let view = table(users(&csv_of(2, 2001)));
    assert_eq!(view.truncated().map(|t| t.shown() as usize), Some(MAX_ROWS));
    assert_eq!(view.note(), Some("More rows exist than are shown."));

    // The reader stops at the 2001st record: what follows it is not looked at (ragged rows after it are not counted).
    let mut doc = csv_of(2, 2001);
    doc.extend(std::iter::repeat_n(*b"ragged,only,three\n", 100).flatten());
    let view = table(users(&doc));
    assert_eq!(view.note(), Some("More rows exist than are shown."));

    // A cell of 5000 characters is cut at 4096 bytes with an ellipsis; so is one of a megabyte.
    for size in [5000_usize, 1 << 20] {
        let doc = format!("a,b\nx,{}\n", "y".repeat(size));
        let view = table(users(doc.as_bytes()));
        let cell = &cells(&view, 0)[1];
        assert!(cell.len() <= MAX_CELL, "{size}: {} bytes", cell.len());
        assert!(cell.ends_with('\u{2026}'), "{size}: the cut says so");
        assert_eq!(cells(&view, 0)[0], "x");
    }
    // A multi-byte character is never cut in the middle.
    let doc = format!("a\n{}\n", "\u{4e2d}".repeat(3000));
    let view = table(users(doc.as_bytes()));
    assert!(cells(&view, 0)[0].len() <= MAX_CELL);

    // An output above the cap is refused, not read.
    let big = vec![b'a'; MAX_INPUT + 1];
    assert_eq!(GAM.view(verb("users"), &big).err(), Some(ParseError::TooLarge));
}

#[test]
fn hostile_csv_shapes_give_clean_tables_and_never_panic() {
    let mut blob: Vec<u8> = vec![0xff, 0xfe];
    blob.extend((0..=255_u8).cycle().take(5000));
    blob.extend_from_slice(b"\n\xff\xfe,\x00\n");
    let docs: Vec<(&str, Vec<u8>)> = vec![
        ("bom", b"\xEF\xBB\xBFprimaryEmail,name\na@example.com,A\n".to_vec()),
        ("crlf", b"primaryEmail,name\r\na@example.com,A\r\nb@example.com,B\r\n".to_vec()),
        ("short row", b"primaryEmail,name,x\na@example.com\nb@example.com,B,y\n".to_vec()),
        ("long row", b"primaryEmail,name\na@example.com,A,extra,more\n".to_vec()),
        ("empty field", b"primaryEmail,name\n,\na@example.com,\n".to_vec()),
        ("escaped quote", b"primaryEmail,name\na@example.com,\"He said \"\"hi\"\"\"\n".to_vec()),
        ("duplicate header", b"name,name,name\n1,2,3\n".to_vec()),
        ("empty header name", b"a,,b\n1,2,3\n".to_vec()),
        ("nul", b"primaryEmail,name\na@example.com,A\x00B\n".to_vec()),
        ("escape", b"primaryEmail,name\na@example.com,\x1b[31mred\x1b[0m \x1b]0;title\x07ok\n".to_vec()),
        ("direction", "primaryEmail,name\na@example.com,\u{202e}evil\u{200b}x\n".as_bytes().to_vec()),
        ("only a header", b"primaryEmail,name\n".to_vec()),
        ("header without a newline", b"primaryEmail,name".to_vec()),
        ("empty", Vec::new()),
        ("blank lines", b"\n\n\r\n".to_vec()),
        ("unterminated quote", b"a,b\n1,\"never closed\n2,3\n".to_vec()),
        ("lone carriage returns", b"a,b\r1,2\r3,4\r".to_vec()),
        ("quote in the middle", b"a,b\n1,x\"y\"z\n".to_vec()),
        ("only commas", b",,,,\n,,,,\n".to_vec()),
        ("binary", blob),
    ];
    for (label, doc) in &docs {
        let model = GAM.view(verb("users"), doc).unwrap_or_else(|e| panic!("{label}: refused: {e}"));
        assert_clean(&model, label);
    }

    // The documents whose result is worth looking at.
    let view = table(users(&docs[0].1));
    assert_eq!(view.columns()[0].id(), "primaryEmail", "a leading byte order mark is not part of the first name");
    assert_eq!(view.rows()[0].key(), "a@example.com");
    let view = table(users(&docs[1].1));
    assert_eq!(view.rows().len(), 2, "CRLF line ends");
    let view = table(users(&docs[2].1));
    assert_eq!(cells(&view, 0), ["a@example.com", "", ""], "a short row is padded");
    assert_eq!(view.note(), Some("1 rows had another number of fields."));
    let view = table(users(&docs[3].1));
    assert_eq!(cells(&view, 0), ["a@example.com", "A"], "a long row is cut to the header");
    let view = table(users(&docs[5].1));
    assert_eq!(cells(&view, 0)[1], "He said \"hi\"");
    let view = table(users(&docs[6].1));
    let ids: Vec<&str> = view.columns().iter().map(|c| c.id()).collect();
    assert_eq!(ids, ["name", "name#2", "name#3"], "a duplicate header name is made unique");
    let view = table(users(&docs[7].1));
    let ids: Vec<&str> = view.columns().iter().map(|c| c.id()).collect();
    assert_eq!(ids, ["a", "column 2", "b"]);
    let view = table(users(&docs[8].1));
    assert_eq!(cells(&view, 0)[1], "AB", "a NUL is dropped");
    let view = table(users(&docs[9].1));
    assert_eq!(cells(&view, 0)[1], "red ok", "an escape sequence goes with its payload");
    let view = table(users(&docs[10].1));
    assert_eq!(cells(&view, 0)[1], "evilx");
    let view = table(users(&docs[11].1));
    assert!(view.rows().is_empty() && view.columns().len() == 2, "only a header: an empty table");
    let view = table(users(&docs[13].1));
    assert!(view.rows().is_empty() && view.columns().is_empty(), "empty input: an empty table");
    assert_eq!(view.note(), Some("GAM printed no rows."));
    let view = table(users(&docs[14].1));
    assert!(view.rows().is_empty() && view.columns().is_empty(), "blank lines: an empty table");
    // A header that is not text is the plain-text model, not a table of garbage.
    assert!(matches!(users(&docs[19].1), ViewModel::Text(_)), "a binary blob shows as text");
    // The truncated recording of a real shape (cut inside a quoted cell) still reads.
    let view = table(users(&fixture("print_users_truncated.csv")));
    assert!(!view.rows().is_empty());
    assert_clean(&ViewModel::Table(view), "truncated fixture");
}

// ---------------------------------------------------------------------------------------------------------------------
// The JSON views.
// ---------------------------------------------------------------------------------------------------------------------

/// A CSV as `formatjson` prints it: the e-mail and one JSON object per row in the `JSON` column, quotes doubled.
fn json_csv(objects: &[&str]) -> Vec<u8> {
    let mut out = String::from("primaryEmail,JSON\n");
    for (at, object) in objects.iter().enumerate() {
        out.push_str(&format!("u{at}@example.com,\"{}\"\n", object.replace('"', "\"\"")));
    }
    out.into_bytes()
}

fn text_of(model: ViewModel) -> (String, bool, TextFormat, Option<String>) {
    match model {
        ViewModel::Text(view) => {
            (view.text().to_owned(), view.truncated(), view.format(), view.note().map(str::to_owned))
        }
        other => panic!("a text model was expected, got {other:?}"),
    }
}

#[test]
fn the_json_views_are_re_serialised_and_bounded() {
    let raw = r#"{"primaryEmail":"a@example.com","name":{"fullName":"A  B"},"suspended":false}"#;
    let second = r#"{ "primaryEmail" : "b@example.com" , "orgUnitPath" : "/Staff" }"#;
    let (text, truncated, format, note) = text_of(users(&json_csv(&[raw, second])));
    assert_eq!(format, TextFormat::Json);
    assert!(!truncated);
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("the text is one JSON document");
    assert_eq!(parsed.as_array().map(Vec::len), Some(2), "one array of the objects of the JSON column");
    assert_eq!(parsed[0]["name"]["fullName"], "A  B");
    assert!(text.contains("\n  "), "pretty printed");
    assert!(!text.contains(raw) && !text.contains(second), "re-serialised, never the raw bytes");
    assert_eq!(note.as_deref(), Some("2 objects shown."));

    // A cell that is not JSON, one that is not an object and one that is too deep are skipped and counted.
    let deep = format!("{}1{}", "{\"a\":".repeat(300), "}".repeat(300));
    let (text, _, _, note) = text_of(users(&json_csv(&[raw, "not json", "42", deep.as_str(), "{\"cut\":"])));
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(parsed.as_array().map(Vec::len), Some(1));
    assert_eq!(note.as_deref(), Some("1 objects shown. 4 cells skipped, they are not JSON objects."));

    // Nothing valid: an empty array and the count.
    let (text, _, _, note) = text_of(users(&json_csv(&["nope"])));
    assert_eq!(text, "[]");
    assert_eq!(note.as_deref(), Some("0 objects shown. 1 cells skipped, they are not JSON objects."));

    // A row with no cell in the JSON column is skipped too.
    let doc = b"primaryEmail,JSON\nshort@example.com\n";
    let (text, _, _, _) = text_of(users(doc));
    assert_eq!(text, "[]");

    // Hostile text inside a JSON string is re-serialised as an escape and cleaned: no raw control or spoofing character.
    let hostile = "{\"n\":\"\\u001b[31mred\\u001b[0m \u{202e}x\u{200b}y\"}";
    let model = users(&json_csv(&[hostile]));
    assert_clean(&model, "hostile json");

    // A large document is bounded at 64 KiB with `truncated` set; the cut text is still only characters.
    let filler = format!("{{\"k\":\"{}\"}}", "z".repeat(3000));
    let many: Vec<&str> = (0..100).map(|_| filler.as_str()).collect();
    let (text, truncated, _, _) = text_of(users(&json_csv(&many)));
    assert!(truncated, "more than 64 KiB of objects is cut and says so");
    assert!(text.len() <= MAX_TEXT, "{} bytes", text.len());
    // A single object over the cap of a cell is read all the same (the cap of a JSON cell is the text cap).
    let big = format!("{{\"k\":\"{}\"}}", "q".repeat(20_000));
    let (text, truncated, _, _) = text_of(users(&json_csv(&[big.as_str()])));
    assert!(!truncated && text.contains("qqqq"));
    // One over the cap of the text is skipped.
    let huge = format!("{{\"k\":\"{}\"}}", "q".repeat(MAX_TEXT + 10));
    let (text, _, _, note) = text_of(users(&json_csv(&[huge.as_str()])));
    assert_eq!(text, "[]");
    assert_eq!(note.as_deref(), Some("0 objects shown. 1 cells skipped, they are not JSON objects."));
}

#[test]
fn the_domain_is_a_key_value_view() {
    // The fixture is written, not recorded: its member names are an assumption from the tool's documents.
    let model = GAM.view(verb("domain"), &fixture("info_domain.json")).expect("the domain is read");
    let ViewModel::Kv(view) = &model else { panic!("a key-value model, got {model:?}") };
    let rows: Vec<(&str, &str)> = view.rows().iter().map(|r| (r.label(), r.value())).collect();
    assert_eq!(
        rows,
        [
            ("Domain", "example.com"),
            ("Created", "2020-03-01T09:00:00.000Z"),
            ("Verified", "yes"),
            ("Alias", "alias-one.example.com, verified"),
            ("Alias", "alias-two.example.com, not verified"),
        ]
    );
    assert_eq!(view.title(), "Domain");
    assert!(view.note().is_none() && view.actions().is_empty());
    assert_clean(&model, "domain");

    // Members the module does not know are dropped and counted; their values are nowhere in the model.
    let doc = br#"{"customerDomain":"example.com","oauthClientSecret":"SECRET-VALUE","token":{"a":"TOKEN-VALUE"},"isVerified":false}"#;
    let model = GAM.view(verb("domain"), doc).expect("read");
    let ViewModel::Kv(view) = &model else { panic!("kv") };
    assert_eq!(view.note(), Some("2 other members are not shown."));
    assert!(every_text(&model).iter().all(|t| !t.contains("SECRET") && !t.contains("TOKEN")));
    assert_eq!(view.rows().iter().find(|r| r.label() == "Verified").map(|r| r.tone()), Some(holzwave_core::integrate::view::Tone::Warn));

    // Hostile values are cleaned.
    let doc = "{\"customerDomain\":\"ex\\u001b[31mam\u{202e}ple.com\"}".as_bytes();
    assert_clean(&GAM.view(verb("domain"), doc).expect("read"), "hostile domain");

    // Output that is not the known shape is shown as text: not JSON, an array (never read by position), a wrong type,
    // no known member, the truncated recording, a document over the cap that is parsed.
    for (label, doc) in [
        ("not json", b"Domain: example.com".to_vec()),
        ("array", br#"["example.com","2020",true,[]]"#.to_vec()),
        ("wrong type", br#"{"customerDomain":42}"#.to_vec()),
        ("wrong alias", br#"{"customerDomain":"x","domainAliases":["a"]}"#.to_vec()),
        ("no known member", br#"{"other":1}"#.to_vec()),
        ("truncated", fixture("info_domain_truncated.json")),
        ("scalar", b"42".to_vec()),
        ("large", format!("{{\"customerDomain\":\"{}\"}}", "a".repeat(2 << 20)).into_bytes()),
    ] {
        let model = GAM.view(verb("domain"), &doc).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert!(matches!(model, ViewModel::Text(_)), "{label}: a text model");
        assert_clean(&model, label);
    }
    assert_eq!(GAM.view(verb("domain"), b"").err(), Some(ParseError::Empty));

    // The JSON text on request holds the typed members only, re-serialised.
    let (text, _, format, _) = text_of(domain_json_text(&doc_with_secret()).expect("read"));
    assert_eq!(format, TextFormat::Json);
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(parsed["customerDomain"], "example.com");
    assert!(parsed.get("oauthClientSecret").is_none() && !text.contains("SECRET"));
    assert!(domain_json_text(b"not json").is_err());
}

fn doc_with_secret() -> Vec<u8> {
    br#"{"customerDomain":"example.com","oauthClientSecret":"SECRET-VALUE","domainAliases":[{"domainAliasName":"a.example.com","verified":true,"extra":1}]}"#.to_vec()
}
