//! GAM7, the Google Workspace admin tool: the argument lists, the bounded CSV reader and the views.
//!
//! What is held here: that a `print` output with quoted multi-line cells becomes one table whose cells are clean and
//! bounded; that the reader never panics and never keeps more than its caps on any shape of document; that every
//! argument list is exact and holds no word that writes (the tool's own configuration is never saved); and, where the
//! real tool is present and `HW_INTEGRATION_REAL=1`, that the chained meta commands parse in a scratch configuration
//! directory. The module starts no process: the tests feed it recorded and written fixtures.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use holzwave_core::integrate::args::{ArgError, SectionName};
use holzwave_core::integrate::gam::{domain_json_text, parse_version, GAM, MAX_INPUT, ROW_LIMIT, SPEC};
use holzwave_core::integrate::view::{
    TableView, TextFormat, Tone, ViewModel, MAX_CELL, MAX_COLUMNS, MAX_ROWS, MAX_TEXT,
};
use holzwave_core::integrate::{Integration, ParseError, Selection, ToolId, Validated, Verb, VerbEffect};

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
    assert_eq!(view.rows().iter().find(|r| r.label() == "Verified").map(|r| r.tone()), Some(Tone::Warn));

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

// ---------------------------------------------------------------------------------------------------------------------
// The sections.
// ---------------------------------------------------------------------------------------------------------------------

fn sections(text: &str) -> ViewModel {
    GAM.view(verb("sections"), text.as_bytes()).expect("the sections output is read")
}

#[test]
fn sections_are_read_leniently() {
    // The fixture is recorded: an empty configuration directory still has a selected section.
    let view = table(sections(&String::from_utf8(fixture("showsections.txt")).expect("text")));
    let ids: Vec<&str> = view.columns().iter().map(|c| c.id()).collect();
    assert_eq!(ids, ["section", "selected"]);
    assert_eq!(view.title(), "Sections");
    assert_eq!(view.rows().len(), 1);
    assert_eq!(cells(&view, 0), ["DEFAULT", "*"]);
    assert_eq!(view.rows()[0].key(), "DEFAULT");
    assert!(view.rows()[0].actions().is_empty(), "there is no verb that switches the default of the tool");

    // The mark may lead or trail, the line ends may be CRLF, blank lines do not matter.
    let view = table(sections("Config File: /x/gam.cfg\r\n\r\n  alpha\r\n  * beta\r\n  gamma *\r\n  delta\r\n"));
    let rows: Vec<Vec<String>> = (0..view.rows().len()).map(|r| cells(&view, r)).collect();
    assert_eq!(rows, [["alpha", ""], ["beta", "*"], ["gamma", "*"], ["delta", ""]]);

    // A path with spaces in the header line is only a header line.
    let view = table(sections("Config File: /my files/gam.cfg\n  one *\n"));
    assert_eq!(cells(&view, 0), ["one", "*"]);

    // Any other shape is the plain-text model, never a table of guesses.
    for (label, text) in [
        ("no header line", "  alpha *\n"),
        ("no section", "Config File: /x/gam.cfg\n"),
        ("a name that is not indented", "Config File: /x/gam.cfg\nalpha\n"),
        ("a name with a space", "Config File: /x/gam.cfg\n  my section\n"),
        ("a star inside a name", "Config File: /x/gam.cfg\n  al*pha\n"),
        ("only a star", "Config File: /x/gam.cfg\n  *\n"),
        ("a sentence", "Error: no such file\n"),
    ] {
        let model = sections(text);
        assert!(matches!(model, ViewModel::Text(_)), "{label}: a text model");
        assert_clean(&model, label);
    }
    assert_eq!(GAM.view(verb("sections"), b"").err(), Some(ParseError::Empty));
    assert_eq!(GAM.view(verb("sections"), b" \n\n").err(), Some(ParseError::Empty));

    // Hostile names are cleaned, and a huge listing is cut at the row cap and says so.
    let model = sections("Config File: /x\n  a\u{202e}b\u{1b}[31m *\n");
    assert_clean(&model, "hostile name");
    let mut long = String::from("Config File: /x\n");
    for n in 0..3000 {
        long.push_str(&format!("  s{n}\n"));
    }
    let view = table(sections(&long));
    assert_eq!(view.rows().len(), MAX_ROWS);
    assert!(view.truncated().is_some());
    // Output over the text cap is not parsed.
    let big = format!("Config File: /x\n{}", "  a\n".repeat(MAX_TEXT / 4));
    assert!(matches!(sections(&big), ViewModel::Text(_)));
    // Bytes that are not text give the text model.
    assert!(matches!(GAM.view(verb("sections"), b"Config File: x\n  \xff\xfe\n"), Ok(ViewModel::Text(_))));
}

// ---------------------------------------------------------------------------------------------------------------------
// The argument lists.
// ---------------------------------------------------------------------------------------------------------------------

fn selection(section: Option<&str>) -> Selection {
    Selection { section: section.map(str::to_owned), ..Selection::default() }
}

/// The argument list of a verb with its arguments and the selected section, or why it was refused.
fn argv(id: &str, pairs: &[(&str, &str)], section: Option<&str>) -> Result<Vec<String>, ArgError> {
    let verb = verb(id);
    let pairs: BTreeMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    let args = Validated::parse(verb, &pairs)?;
    let checked = selection(section).check(ToolId::Gam)?;
    let list = GAM.argv(verb, &args, &checked)?;
    Ok(list.into_iter().map(|a| a.into_string().expect("every argument is text")).collect())
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

#[test]
fn the_argument_lists_put_select_and_the_row_limit_before_the_command() {
    let fields = "primaryemail,name,suspended,orgunitpath,lastlogintime";
    assert_eq!(
        argv("users", &[], Some("work")),
        Ok(strings(&["select", "work", "config", "csv_output_row_limit", "2001", "print", "users", "fields", fields]))
    );
    assert_eq!(
        argv("users", &[("as_json", "true")], Some("work")),
        Ok(strings(&[
            "select", "work", "config", "csv_output_row_limit", "2001", "print", "users", "fields", fields,
            "formatjson"
        ]))
    );
    assert_eq!(
        argv("users", &[("as_json", "false")], None),
        Ok(strings(&["config", "csv_output_row_limit", "2001", "print", "users", "fields", fields]))
    );
    assert_eq!(argv("domain", &[], Some("work")), Ok(strings(&["select", "work", "info", "domain", "formatjson"])));
    assert_eq!(argv("domain", &[], None), Ok(strings(&["info", "domain", "formatjson"])));
    assert_eq!(argv("sections", &[], Some("work")), Ok(strings(&["showsections"])));
    assert_eq!(argv("sections", &[], None), Ok(strings(&["showsections"])));
    // The row limit is the reader's cap plus one.
    assert_eq!(ROW_LIMIT, MAX_ROWS + 1);

    // Every verb is a read that shows a view, and the catalogue is the three of the outline.
    let ids: Vec<&str> = SPEC.verbs.iter().map(|v| v.id).collect();
    assert_eq!(ids, ["sections", "domain", "users"]);
    assert!(SPEC.verbs.iter().all(|v| v.effect == VerbEffect::Read && v.refresh_secs.is_none()));
    assert_eq!(SPEC.binaries, ["gam"]);
    assert_eq!(SPEC.windows_binaries, ["gam.exe"]);
    assert!(SPEC.fixed_dirs.contains(&"$HOME/bin/gam7") && SPEC.fixed_dirs.contains(&"C:\\GAM7"));
    assert_eq!(SPEC.version_probe.argv, ["version"]);

    // An argument the verb does not declare, a value that is not a flag, a verb that is not the catalogue's.
    assert_eq!(argv("users", &[("save", "true")], None), Err(ArgError::Unknown));
    assert_eq!(argv("users", &[("as_json", "yes")], None), Err(ArgError::Shape));
    assert_eq!(argv("domain", &[("as_json", "true")], None), Err(ArgError::Unknown));
    let mut forged = *verb("users");
    forged.effect = VerbEffect::Write;
    let args = Validated::parse(&forged, &BTreeMap::new()).expect("no arguments");
    let checked = selection(None).check(ToolId::Gam).expect("empty");
    assert_eq!(GAM.argv(&forged, &args, &checked), Err(ArgError::Unknown));
    let mut other = *verb("users");
    other.id = "delete_user";
    let args = Validated::parse(&other, &BTreeMap::new()).expect("no arguments");
    assert_eq!(GAM.argv(&other, &args, &checked), Err(ArgError::Unknown));
    // A selection field the tool does not select never reaches the list.
    let wrong = Selection { context: Some("dev".to_owned()), ..Selection::default() };
    assert_eq!(wrong.check(ToolId::Gam), Err(ArgError::NotForTool));
}

/// The words that write the tool's state or a domain, or save a choice.
const WRITE_WORDS: &[&str] = &["save", "oauth", "create", "update", "delete", "add", "remove", "todrive"];

/// Section names that an attacker, a typo or an odd file could produce.
fn section_corpus() -> Vec<String> {
    let mut out: Vec<String> = [
        "work", "default", "DEFAULT", "a-b_c.d", "x", "-x", "--config", "-", "save", "SAVE", "Save", "sAvE", "verify",
        "Verify", "default save", "work save", "work verify", "a b", ".hidden", "..", "a/b", "a;b", "a\nb", "a\0b",
        "a\u{1b}[31m", "a\u{202e}b", "a\u{200b}b", "\u{feff}a", "a\u{a0}b", "a\tb", "", " ", "oauth", "create",
        "update", "delete", "add", "remove", "todrive", "config", "select", "formatjson", "print", "users", "ADD",
        "ToDrive", "save=true", "a,b", "a\"b", "a'b", "$(id)", "`id`", "a=b", "\u{e9}",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    out.push("a".repeat(64));
    out.push("a".repeat(65));
    out.push("a".repeat(5000));
    out
}

#[test]
fn a_hostile_section_never_changes_the_gam_argument_list() {
    // A section named like a keyword of the tool is refused: the tool reads the word after the name as `save` or
    // `verify`, and a name that is a write word would put that word in the list.
    for word in WRITE_WORDS.iter().chain(&["verify", "config", "select"]) {
        for spelled in [(*word).to_owned(), word.to_ascii_uppercase(), format!("{}{}", &word[..1].to_ascii_uppercase(), &word[1..])] {
            assert!(SectionName::parse(&spelled).is_err(), "{spelled} must not be a section");
        }
    }
    // Words that only look like them are names.
    for fine in ["saved", "my-save", "verify2", "work", "a.b"] {
        assert!(SectionName::parse(fine).is_ok(), "{fine}");
    }
    for section in section_corpus() {
        for id in ["users", "domain", "sections"] {
            match argv(id, &[], Some(&section)) {
                Err(error) => assert!(matches!(error, ArgError::Shape | ArgError::Hidden | ArgError::LeadingDash | ArgError::TooLong | ArgError::Empty), "{section:?}: {error:?}"),
                Ok(list) => {
                    // Only a plain word can be there, once, right after `select`, and the list does not end with `save`.
                    if id == "sections" {
                        assert_eq!(list, ["showsections"]);
                    } else {
                        assert_eq!(&list[..2], ["select", section.as_str()], "{section:?}");
                        assert!(!list.iter().skip(2).any(|a| a == &section) || list.iter().filter(|a| *a == &section).count() >= 1);
                        assert!(section.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')));
                        assert!(!section.starts_with('-') && !section.starts_with('.'));
                    }
                    assert!(!list.last().is_some_and(|last| last.eq_ignore_ascii_case("save")), "{section:?}");
                }
            }
        }
    }
    // The ones the plan names.
    assert!(argv("users", &[], Some("save")).is_err());
    assert!(argv("users", &[], Some("default save")).is_err());
    assert!(argv("users", &[], Some("-x")).is_err());
    assert!(argv("users", &[], Some(&"a".repeat(65))).is_err());
    assert!(argv("users", &[], Some(&"a".repeat(64))).is_ok());
}

#[test]
fn no_argument_list_holds_a_write_or_save_word() {
    let mut checked = 0;
    let sections: Vec<Option<String>> =
        std::iter::once(None).chain(section_corpus().into_iter().map(Some)).collect();
    for id in ["sections", "domain", "users"] {
        let flags: &[&[(&str, &str)]] = if id == "users" {
            &[&[], &[("as_json", "true")], &[("as_json", "false")]]
        } else {
            &[&[]]
        };
        for pairs in flags {
            for section in &sections {
                let Ok(list) = argv(id, pairs, section.as_deref()) else { continue };
                checked += 1;
                for (at, word) in list.iter().enumerate() {
                    assert!(
                        !WRITE_WORDS.iter().any(|w| word.eq_ignore_ascii_case(w)),
                        "{id} {section:?}: {word:?} at {at} in {list:?}"
                    );
                    assert!(!word.starts_with('-'), "{id}: no option-like word, {list:?}");
                }
                // `config` only ever as the one row-limit variable, `select` only first.
                let configs: Vec<usize> = list.iter().enumerate().filter(|(_, w)| *w == "config").map(|(i, _)| i).collect();
                match id {
                    "users" => {
                        assert_eq!(configs.len(), 1, "{list:?}");
                        let at = configs[0];
                        assert_eq!(list[at..at + 3], ["config", "csv_output_row_limit", "2001"], "{list:?}");
                    }
                    _ => assert!(configs.is_empty(), "{list:?}"),
                }
                let selects: Vec<usize> = list.iter().enumerate().filter(|(_, w)| *w == "select").map(|(i, _)| i).collect();
                assert!(selects.is_empty() || selects == [0], "{list:?}");
                // The first word that is not a meta command is a command the module knows.
                let command = match id {
                    "sections" => "showsections",
                    "domain" => "info",
                    _ => "print",
                };
                assert!(list.iter().any(|w| w == command), "{list:?}");
            }
        }
    }
    assert!(checked > 100, "the sweep looked at {checked} lists");
}

// ---------------------------------------------------------------------------------------------------------------------
// The real tool, offline and in a scratch configuration directory (gated).
// ---------------------------------------------------------------------------------------------------------------------

/// GAM is a shell alias on many machines: `HW_GAM`, then the `PATH`, then `~/bin/gam7/gam`.
fn find_gam() -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let executable = |path: &Path| {
        path.is_file() && std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    };
    if let Some(path) = std::env::var_os("HW_GAM").map(PathBuf::from).filter(|p| p.is_absolute() && executable(p)) {
        return Some(path);
    }
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("gam"))
        .find(|candidate| executable(candidate));
    on_path.or_else(|| {
        let path = PathBuf::from(std::env::var_os("HOME")?).join("bin/gam7/gam");
        executable(&path).then_some(path)
    })
}

#[test]
fn the_chained_meta_commands_parse_in_a_real_gam() {
    if std::env::var("HW_INTEGRATION_REAL").ok().as_deref() != Some("1") {
        println!("skipped: set HW_INTEGRATION_REAL=1 to run the real GAM in a scratch directory");
        return;
    }
    let Some(gam) = find_gam() else {
        println!("skipped: GAM is not installed here");
        return;
    };
    let scratch = std::env::temp_dir().join(format!("holzwave-gam-chain-{}", std::process::id()));
    let (config, home) = (scratch.join("cfg"), scratch.join("home"));
    std::fs::create_dir_all(&config).expect("scratch config directory");
    std::fs::create_dir_all(&home).expect("scratch home directory");
    // Only the scratch directory is ever the tool's configuration; the real one of the person is never named.
    let run = |args: &[&str]| {
        std::process::Command::new(&gam)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("GAMCFGDIR", &config)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("start GAM")
    };
    let snapshot = || -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(&config)
            .expect("list the scratch directory")
            .map(|entry| {
                let entry = entry.expect("entry");
                let path = entry.path();
                let bytes = if path.is_file() { std::fs::read(&path).unwrap_or_default() } else { Vec::new() };
                (entry.file_name().to_string_lossy().into_owned(), bytes)
            })
            .collect()
    };
    // The first run initialises the scratch directory (it writes `gam.cfg` itself): the state to compare against.
    let first = run(&["version"]);
    assert!(first.status.success(), "gam version: {}", String::from_utf8_lossy(&first.stderr));
    let before = snapshot();
    assert!(before.contains_key("gam.cfg"), "the first run wrote the scratch gam.cfg");

    // The row limit is a meta command in front of a command, applied to this process only.
    let chained = run(&["config", "csv_output_row_limit", "5", "version"]);
    println!("config ... version: {:?} {}", chained.status.code(), String::from_utf8_lossy(&chained.stderr));
    assert!(chained.status.success(), "the chained meta command parses: {}", String::from_utf8_lossy(&chained.stderr));
    assert!(String::from_utf8_lossy(&chained.stdout).starts_with("GAM "), "and the command after it runs");

    // A section that does not exist is refused and named.
    let missing = run(&["select", "zzz", "version"]);
    let message = format!("{}{}", String::from_utf8_lossy(&missing.stdout), String::from_utf8_lossy(&missing.stderr));
    println!("select zzz version: {:?} {message}", missing.status.code());
    assert!(!missing.status.success(), "an unknown section is an error");
    assert!(message.contains("zzz"), "the error names the section: {message}");

    // The existing one is selected for the one process, and the tool's files are byte for byte what they were.
    let selected = run(&["select", "default", "config", "csv_output_row_limit", "5", "version"]);
    assert!(selected.status.success(), "select and config chain: {}", String::from_utf8_lossy(&selected.stderr));
    assert_eq!(snapshot(), before, "no meta command in front of a command wrote the configuration directory");
    std::fs::remove_dir_all(&scratch).ok();
}
