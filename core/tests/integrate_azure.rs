//! The Azure command-line tool as an integration: the argument lists of its verbs, the sign-in state, the parsers of
//! its answers and the hostile text that an account can put into them.
//!
//! No process is started here and no account is contacted. The answers of the tool are the files of
//! `core/tests/fixtures/integrations/az` (some recorded, some written, see the manifest next to them) and texts built
//! in the tests; hostile characters are built from code points so that this file holds none of them.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;

use holzwave_core::integrate::args::SubscriptionId;
use holzwave_core::integrate::azure::{login_state, parse_version, LoginState, AZURE, SPEC};
use holzwave_core::integrate::view::{KvView, TableView, TextFormat, Tone, ViewModel, MAX_CELL, MAX_KEY, MAX_ROWS};
use holzwave_core::integrate::{
    CheckedSelection, Integration, ParseError, Selection, ToolId, Validated, Verb, VerbEffect,
};

const SUBSCRIPTION: &str = "00000000-0000-4000-8000-000000000002";

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrations/az").join(name);
    fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn verb(id: &str) -> &'static Verb {
    SPEC.verb(id).unwrap_or_else(|| panic!("the catalogue has no verb {id}"))
}

fn selection(subscription: Option<&str>) -> CheckedSelection {
    Selection { subscription: subscription.map(str::to_owned), ..Selection::default() }
        .check(ToolId::Azure)
        .expect("a valid selection")
}

fn bare(id: &str) -> Validated {
    Validated::parse(verb(id), &BTreeMap::new()).expect("a verb without arguments")
}

fn argv(id: &str, subscription: Option<&str>) -> Vec<String> {
    AZURE
        .argv(verb(id), &bare(id), &selection(subscription))
        .expect("an argument list")
        .into_iter()
        .map(|arg: OsString| arg.into_string().expect("an argument that is text"))
        .collect()
}

fn kv(model: ViewModel) -> KvView {
    match model {
        ViewModel::Kv(view) => view,
        other => panic!("a key-value model was expected, got {other:?}"),
    }
}

fn value_of<'a>(view: &'a KvView, label: &str) -> &'a str {
    view.rows()
        .iter()
        .find(|row| row.label() == label)
        .unwrap_or_else(|| panic!("the view has no row {label}"))
        .value()
}

fn labels(view: &KvView) -> Vec<&str> {
    view.rows().iter().map(|row| row.label()).collect()
}

#[test]
fn signed_out_is_a_state_and_offers_login() {
    let account = verb("account");
    assert!(account.exit_is_state, "a signed-out account command exits non-zero and that is a state");
    assert_eq!(argv("account", None), ["account", "show", "-o", "json", "--only-show-errors"]);
    assert_eq!(
        argv("account", Some(SUBSCRIPTION)),
        ["account", "show", "-o", "json", "--only-show-errors", &format!("--subscription={SUBSCRIPTION}")]
    );

    let view = kv(AZURE.view(account, b"").expect("empty output is a state"));
    assert_eq!(value_of(&view, "State"), "Signed out");
    assert_eq!(view.rows()[0].tone(), Tone::Warn, "the word is the cue, the tone is the second one");
    assert_eq!(view.rows().len(), 1, "nothing else is known when signed out");
    assert_eq!(view.actions().len(), 1);
    let login = &view.actions()[0];
    assert_eq!(login.verb(), "login");
    assert_eq!(login.effect(), VerbEffect::Exec);
    assert_eq!(login.args().count(), 0);

    // Blank output is the same state; a tool that wrote only white space has signed nothing in.
    assert_eq!(kv(AZURE.view(account, b" \r\n\t\n").expect("blank output")).actions().len(), 1);
    assert_eq!(login_state(b""), LoginState::SignedOut);
    assert_eq!(login_state(b" \r\n"), LoginState::SignedOut);
}

#[test]
fn signed_in_shows_the_account_and_no_login() {
    let view = kv(AZURE.view(verb("account"), &fixture("account_show.json")).expect("the recorded account"));
    assert_eq!(
        labels(&view),
        ["State", "User", "Subscription", "Subscription ID", "Tenant ID", "Environment", "Subscription state"]
    );
    assert_eq!(value_of(&view, "State"), "Signed in");
    assert_eq!(view.rows()[0].tone(), Tone::Normal);
    assert_eq!(value_of(&view, "User"), "user@example.com");
    assert_eq!(value_of(&view, "Subscription"), "Item 1");
    assert_eq!(value_of(&view, "Subscription ID"), SUBSCRIPTION);
    assert_eq!(value_of(&view, "Tenant ID"), "00000000-0000-4000-8000-000000000001");
    assert_eq!(value_of(&view, "Environment"), "AzureCloud");
    assert_eq!(value_of(&view, "Subscription state"), "Enabled");
    assert!(view.actions().is_empty(), "a signed-in account has no login button");

    match login_state(&fixture("account_show.json")) {
        LoginState::SignedIn { user, subscription_id, subscription_name } => {
            assert_eq!(user, "user@example.com");
            assert_eq!(subscription_id.as_str(), SUBSCRIPTION);
            assert_eq!(subscription_name, "Item 1");
        }
        other => panic!("signed in was expected, got {other:?}"),
    }
    assert_eq!(login_state(b"this is not json"), LoginState::Unknown);
    assert_eq!(login_state(b"[1, 2"), LoginState::Unknown);
    assert_eq!(login_state(&fixture("account_show_signed_out.stderr.txt")), LoginState::Unknown);
    assert_eq!(SubscriptionId::parse(SUBSCRIPTION).expect("a guid").as_str(), SUBSCRIPTION);
}

#[test]
fn the_spec_turns_telemetry_off_and_nothing_else() {
    assert_eq!(SPEC.id, ToolId::Azure);
    assert_eq!(SPEC.label, "Azure");
    assert_eq!(SPEC.env_set, [("AZURE_CORE_COLLECT_TELEMETRY", "false")]);
    assert_eq!(SPEC.binaries, ["az"]);
    assert_eq!(SPEC.windows_binaries, ["az.cmd", "az.exe"]);
    assert_eq!(SPEC.selects, ToolId::Azure.selects());
    assert_eq!(SPEC.version_probe.argv, ["version", "-o", "json"]);
    let version = String::from_utf8(fixture("version.json")).expect("text");
    assert_eq!((SPEC.version_probe.parse)(&version).as_deref(), Some("2.90.0"));
    assert_eq!(parse_version("{\"azure-cli-core\": \"2.90.0\"}"), None, "only the member of the tool itself");
    assert_eq!(parse_version("2.90.0"), None, "not JSON");
    let ids: Vec<&str> = SPEC.verbs.iter().map(|verb| verb.id).collect();
    assert_eq!(ids, ["account", "subscriptions", "groups", "set_subscription", "login"]);
    assert!(std::ptr::eq(AZURE.spec(), &SPEC), "the integration describes itself with the spec");
}

// ---------------------------------------------------------------------------------------------------------------------
// Subscriptions and resource groups.
// ---------------------------------------------------------------------------------------------------------------------

/// A scalar value from its code point: the hostile characters of these tests are built, never typed.
fn ch(code: u32) -> char {
    char::from_u32(code).expect("a scalar value")
}

fn table(model: ViewModel) -> TableView {
    match model {
        ViewModel::Table(view) => view,
        other => panic!("a table was expected, got {other:?}"),
    }
}

fn cells(view: &TableView, row: usize) -> Vec<&str> {
    view.rows()[row].cells().collect()
}

fn column_labels(view: &TableView) -> Vec<&str> {
    view.columns().iter().map(|column| column.label()).collect()
}

/// A guid for the number `n`.
fn guid(n: usize) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

/// The text of a model is made only of characters a person can see, and every cell and key is within its cap.
fn assert_clean(model: &ViewModel) {
    fn walk(value: &serde_json::Value, found: &mut Vec<String>) {
        match value {
            serde_json::Value::String(text) => found.push(text.clone()),
            serde_json::Value::Array(items) => items.iter().for_each(|item| walk(item, found)),
            serde_json::Value::Object(members) => members.values().for_each(|member| walk(member, found)),
            _ => {}
        }
    }
    let mut texts = Vec::new();
    walk(&serde_json::to_value(model).expect("a model serialises"), &mut texts);
    assert!(model.check().is_ok(), "a model that breaks a rule of the schema");
    for text in texts {
        assert!(text.len() <= 65536, "a text over the largest cap");
        let is_text_block = matches!(model, ViewModel::Text(_));
        for c in text.chars() {
            let hidden = c.is_control() && !(is_text_block && matches!(c, '\n' | '\t'))
                || matches!(c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x2069 | 0xFEFF);
            assert!(!hidden, "a control, direction or zero-width character in a model: U+{:04X}", c as u32);
        }
    }
}

fn subscriptions_of(documents: &[u8]) -> ViewModel {
    AZURE.view(verb("subscriptions"), documents).expect("a model")
}

fn groups_of(documents: &[u8]) -> ViewModel {
    AZURE.view(verb("groups"), documents).expect("a model")
}

fn subscription_json(id: &str, name: &str, state: &str, default: bool) -> String {
    serde_json::json!({
        "cloudName": "AzureCloud",
        "homeTenantId": guid(1),
        "id": id,
        "isDefault": default,
        "managedByTenants": [],
        "name": name,
        "state": state,
        "tenantId": guid(1),
        "user": { "name": "user@example.com", "type": "user" },
    })
    .to_string()
}

#[test]
fn subscriptions_become_a_table_with_the_default_marked() {
    assert_eq!(argv("subscriptions", None), ["account", "list", "-o", "json", "--only-show-errors"]);
    assert_eq!(
        argv("subscriptions", Some(SUBSCRIPTION)),
        ["account", "list", "-o", "json", "--only-show-errors"],
        "a list shows every subscription, so a selection does not narrow it"
    );
    let view = table(subscriptions_of(&fixture("account_list.json")));
    assert_eq!(column_labels(&view), ["Default", "Name", "ID", "State", "Tenant"]);
    assert_eq!(view.rows().len(), 3);
    assert_eq!(cells(&view, 0), ["*", "Item 1", SUBSCRIPTION, "Enabled", &guid(1)]);
    assert_eq!(cells(&view, 1), ["", "Item 2", &guid(3), "Enabled", &guid(1)]);
    assert_eq!(view.rows()[0].key(), SUBSCRIPTION, "the key is the id");
    for row in view.rows() {
        assert_eq!(row.tone(), Tone::Normal);
        assert_eq!(row.actions().len(), 1, "a valid guid has a button");
        let action = &row.actions()[0];
        assert_eq!(action.verb(), "set_subscription");
        assert_eq!(action.effect(), VerbEffect::Local, "changing the tool's own file is Local, behind a card");
        assert_eq!(action.args().collect::<Vec<_>>(), [("subscription", row.key())]);
    }
    assert!(view.truncated().is_none());

    // A subscription that is not enabled is told in words and in the tone; only the default carries the star.
    let documents = format!(
        "[{}, {}, {}]",
        subscription_json(&guid(2), "Prod", "Enabled", true),
        subscription_json(&guid(3), "Old", "Disabled", false),
        subscription_json(&guid(4), "Late", "Warned", false)
    );
    let view = table(subscriptions_of(documents.as_bytes()));
    assert_eq!(cells(&view, 1)[3], "Disabled");
    assert_eq!(view.rows()[1].tone(), Tone::Warn);
    assert_eq!(cells(&view, 2)[3], "Warned");
    assert_eq!(view.rows()[2].tone(), Tone::Warn);
    assert_eq!(view.rows()[0].tone(), Tone::Normal);
    assert_eq!(cells(&view, 0)[0], "*");
    assert_eq!(cells(&view, 1)[0], "");

    // An upper-case guid is shown and offered in the one form the validator keeps.
    let upper = subscription_json(&guid(0xABC).to_uppercase(), "Upper", "Enabled", false);
    let view = table(subscriptions_of(format!("[{upper}]").as_bytes()));
    assert_eq!(cells(&view, 0)[2], guid(0xABC));
    assert_eq!(view.rows()[0].actions()[0].args().collect::<Vec<_>>(), [("subscription", guid(0xABC).as_str())]);

    // A row whose id is not a guid is shown and has no button: nothing hostile becomes an argument.
    let not_guids = [
        "not-a-guid",
        "--subscription=x",
        "{00000000-0000-4000-8000-000000000002}",
        "00000000-0000-4000-8000-000000000002 ",
    ];
    for id in not_guids {
        let documents = format!("[{}]", subscription_json(id, "Odd", "Enabled", false));
        let view = table(subscriptions_of(documents.as_bytes()));
        assert_eq!(view.rows().len(), 1, "the row is still shown");
        assert!(view.rows()[0].actions().is_empty(), "no button for the id {id:?}");
        assert_clean(&ViewModel::Table(view));
    }

    // An empty list is an empty table, and no output at all is no answer.
    assert!(table(subscriptions_of(b"[]")).rows().is_empty());
    assert_eq!(AZURE.view(verb("subscriptions"), b"").err(), Some(ParseError::Empty));
    assert_eq!(AZURE.view(verb("subscriptions"), b"  \n").err(), Some(ParseError::Empty));
}

#[test]
fn groups_become_a_table_and_never_read_tag_values() {
    assert_eq!(argv("groups", None), ["group", "list", "-o", "json", "--only-show-errors"]);
    assert_eq!(
        argv("groups", Some(SUBSCRIPTION)),
        ["group", "list", "-o", "json", "--only-show-errors", &format!("--subscription={SUBSCRIPTION}")]
    );
    let view = table(groups_of(&fixture("group_list.json")));
    assert_eq!(column_labels(&view), ["Name", "Location", "State", "Tags"]);
    assert_eq!(cells(&view, 0), ["rg-demo-one", "westeurope", "Succeeded", "1"]);
    assert_eq!(cells(&view, 1), ["rg-demo-two", "northeurope", "Succeeded", "0"]);
    assert_eq!(cells(&view, 2), ["rg-demo-three", "westeurope", "Deleting", "0"], "tags of null are no tags");
    assert_eq!(view.rows()[0].tone(), Tone::Normal);
    assert_eq!(view.rows()[2].tone(), Tone::Warn, "an unsettled state is told in the tone and in the word");
    assert!(view.rows().iter().all(|row| row.actions().is_empty()), "a group has no verb");

    // The tag keys and values are another party's text and are never read into a model.
    let documents = serde_json::json!([{
        "id": "/subscriptions/x/resourceGroups/g",
        "location": "westeurope",
        "name": "g",
        "properties": { "provisioningState": "Succeeded" },
        "tags": {
            "CANARY-TAG-KEY": "CANARY-TAG-VALUE", "owner": "CANARY-OWNER", "n": 5, "deep": { "a": ["CANARY-DEEP"] }
        },
    }])
    .to_string();
    let model = groups_of(documents.as_bytes());
    let json = serde_json::to_string(&model).expect("a model serialises");
    assert!(!json.contains("CANARY"), "a tag key or value reached the model: {json}");
    assert_eq!(cells(&table(model), 0)[3], "4");

    // A tags member of another type is not a count.
    for tags in ["\"text\"", "[1, 2]", "7", "true"] {
        let documents = format!(
            r#"[{{"name": "g", "location": "l", "properties": {{"provisioningState": "Succeeded"}}, "tags": {tags}}}]"#
        );
        assert_eq!(cells(&table(groups_of(documents.as_bytes())), 0)[3], "?", "tags: {tags}");
    }
}

#[test]
fn drift_truncation_and_hostile_names_give_clean_models() {
    // Documents that are no list of objects give the cleaned text, with the note that says so, and never a panic.
    for verb_id in ["subscriptions", "groups"] {
        let kinds: [&[u8]; 9] = [
            b"[1, 2, 3]",
            b"null",
            b"{}",
            b"{\"value\": []}",
            b"this is not json",
            b"[{\"id\": ",
            b"[[\"a\"]]",
            b"\"a string\"",
            b"[] trailing",
        ];
        for bytes in kinds {
            let model = AZURE.view(verb(verb_id), bytes).expect("drift is a text model, not an error");
            match &model {
                ViewModel::Text(text) => {
                    assert_eq!(text.format(), TextFormat::Plain);
                    assert!(text.note().is_some_and(|note| note.contains("not in the expected shape")), "{verb_id}");
                }
                other => panic!("{verb_id}: a text model was expected, got {other:?}"),
            }
            assert_clean(&model);
        }
    }

    // The truncated recordings are cut in the middle of a value.
    for (verb_id, name) in [("subscriptions", "account_list_truncated.json"), ("groups", "group_list_truncated.json")] {
        let model = AZURE.view(verb(verb_id), &fixture(name)).expect("a model");
        assert!(matches!(model, ViewModel::Text(_)), "{name} is a text model");
        assert_clean(&model);
    }

    // Unknown members are skipped, members of the wrong type read as absent and show as a question mark.
    let documents = br#"[
        {"id": "00000000-0000-4000-8000-000000000002", "name": "A", "state": "Enabled", "tenantId": "t",
         "isDefault": true, "brandNew": {"x": [1, 2, {"y": null}]},
         "user": {"name": "u", "type": "user", "secret": "CANARY-USER-SECRET"}},
        {"id": "00000000-0000-4000-8000-000000000003", "name": 5, "state": {"a": 1}, "tenantId": [1],
         "isDefault": "yes"},
        {"id": "00000000-0000-4000-8000-000000000004"},
        {}
    ]"#;
    let model = subscriptions_of(documents);
    assert_clean(&model);
    assert!(!serde_json::to_string(&model).expect("a model serialises").contains("CANARY"));
    let view = table(model);
    assert_eq!(view.rows().len(), 4);
    assert_eq!(cells(&view, 0)[..2], ["*", "A"]);
    assert_eq!(cells(&view, 1), ["", "?", "00000000-0000-4000-8000-000000000003", "?", "?"]);
    assert_eq!(cells(&view, 2), ["", "?", "00000000-0000-4000-8000-000000000004", "?", "?"]);
    assert_eq!(cells(&view, 3), ["", "?", "?", "?", "?"]);
    assert_eq!(view.rows()[1].tone(), Tone::Normal, "an unknown state is not a warning");
    assert!(view.rows()[3].actions().is_empty());
    let groups = br#"[
        {"name": 7, "location": null, "properties": [1], "tags": {}},
        {"id": 1},
        {"name": "ok", "properties": {"provisioningState": 3}}
    ]"#;
    let view = table(groups_of(groups));
    assert_eq!(cells(&view, 0), ["?", "?", "?", "0"]);
    assert_eq!(cells(&view, 1), ["?", "?", "?", "?"]);
    assert_eq!(cells(&view, 2), ["ok", "?", "?", "?"]);
    assert_eq!(view.rows().len(), 3, "rows with the same key stay rows");
    let keys: std::collections::HashSet<&str> = view.rows().iter().map(|row| row.key()).collect();
    assert_eq!(keys.len(), 3, "keys stay unique");

    // A document with a byte order mark is read; a document of invalid UTF-8 is text, not a panic.
    let mut bom = b"\xEF\xBB\xBF".to_vec();
    bom.extend_from_slice(&fixture("account_list.json"));
    assert_eq!(table(subscriptions_of(&bom)).rows().len(), 3);
    let bytes: &[u8] = b"[{\"id\": \"\xFF\xFE\"}]";
    assert_clean(&subscriptions_of(bytes));

    // Names that spoof or flood: an escape sequence, a right-to-left override, a line break, 5000 characters.
    let escape = format!("{}[31mred{}[0m{}]0;title{}", ch(0x1B), ch(0x1B), ch(0x1B), ch(7));
    let rlo = format!("evil{}gnp.exe", ch(0x202E));
    let lines = format!("one{}two{}three", ch(0x0A), ch(0x0D));
    let zero = format!("a{}b{}c", ch(0x200B), ch(0x2066));
    let long = "x".repeat(5000);
    let mut subs = Vec::new();
    let mut groups = Vec::new();
    for (n, name) in [&escape, &rlo, &lines, &zero, &long].into_iter().enumerate() {
        subs.push(subscription_json(&guid(n + 10), name, name, false));
        groups.push(serde_json::json!({
            "name": name, "location": name, "properties": { "provisioningState": name }, "tags": {}
        }));
    }
    let model = subscriptions_of(format!("[{}]", subs.join(",")).as_bytes());
    assert_clean(&model);
    let view = table(model);
    for row in view.rows() {
        assert!(row.key().len() <= MAX_KEY);
        assert!(row.cells().all(|cell| cell.len() <= MAX_CELL));
    }
    assert_eq!(cells(&view, 0)[1], "red");
    assert!(cells(&view, 1)[1].starts_with("evil"));
    assert_eq!(cells(&view, 2)[1], "one two three");
    assert_eq!(cells(&view, 3)[1], "abc");
    assert!(cells(&view, 4)[1].len() <= MAX_CELL && cells(&view, 4)[1].ends_with('\u{2026}'));
    let model = groups_of(serde_json::Value::Array(groups).to_string().as_bytes());
    assert_clean(&model);
    assert_eq!(cells(&table(model), 0)[0], "red");

    // The name that holds an option or a path has no effect but being text.
    let hostile = ["--subscription=evil", "-o table", "../../etc/passwd", "a&b|c>d", "%PATH%", "$(id)", "`id`"];
    for name in hostile {
        let documents = format!("[{}]", subscription_json(&guid(99), name, "Enabled", false));
        let view = table(subscriptions_of(documents.as_bytes()));
        assert_eq!(cells(&view, 0)[1], name);
        assert_eq!(view.rows()[0].actions()[0].args().collect::<Vec<_>>(), [("subscription", guid(99).as_str())]);
    }
}

#[test]
fn a_long_subscription_list_is_cut_with_a_note() {
    let items: Vec<String> =
        (0..3000).map(|n| subscription_json(&guid(n + 1), &format!("Sub {n}"), "Enabled", n == 0)).collect();
    let documents = format!("[{}]", items.join(","));
    let model = subscriptions_of(documents.as_bytes());
    assert_clean(&model);
    let view = table(model);
    assert_eq!(view.rows().len(), MAX_ROWS);
    let truncated = view.truncated().expect("a cut table says so");
    assert_eq!(truncated.shown(), 2000);
    assert!(truncated.note().contains("2000 of 3000"), "the note counts: {}", truncated.note());
    assert_eq!(cells(&view, 0)[0], "*", "the order of the tool is kept");

    let groups: Vec<String> = (0..3000)
        .map(|n| {
            serde_json::json!({
                "name": format!("rg-{n}"), "location": "l", "tags": {},
                "properties": { "provisioningState": "Succeeded" }
            })
            .to_string()
        })
        .collect();
    let view = table(groups_of(format!("[{}]", groups.join(",")).as_bytes()));
    assert_eq!(view.rows().len(), MAX_ROWS);
    assert!(view.truncated().is_some());

    // A document over what is read at all is no answer.
    let huge = vec![b' '; 9 * 1024 * 1024];
    let mut bytes = b"[".to_vec();
    bytes.extend_from_slice(&huge);
    bytes.push(b']');
    assert_eq!(AZURE.view(verb("subscriptions"), &bytes).err(), Some(ParseError::TooLarge));
    assert_eq!(AZURE.view(verb("groups"), &bytes).err(), Some(ParseError::TooLarge));
}

#[test]
fn an_array_is_no_account() {
    // A derived reader would take an array as the member values in order; no answer of the tool is one.
    let guid_text = serde_json::json!([SUBSCRIPTION, "name", "Enabled"]).to_string();
    assert_eq!(login_state(guid_text.as_bytes()), LoginState::Unknown);
    let view = AZURE.view(verb("account"), guid_text.as_bytes()).expect("a model");
    assert!(matches!(view, ViewModel::Text(_)));
    assert_eq!(login_state(b"{}"), LoginState::Unknown, "an object without a subscription is not signed in");
    assert_eq!(login_state(b"{\"id\": \"x\"}"), LoginState::Unknown);
    assert_eq!(parse_version("[\"2.90.0\"]"), None);
}

// ---------------------------------------------------------------------------------------------------------------------
// The Local and Exec verbs, the batch-file charset and the hostile subscriptions.
// ---------------------------------------------------------------------------------------------------------------------

fn with_subscription(id: &str, text: &str) -> Result<Validated, holzwave_core::integrate::args::ArgError> {
    let pairs = BTreeMap::from([("subscription".to_owned(), text.to_owned())]);
    Validated::parse(verb(id), &pairs)
}

fn texts(list: Vec<OsString>) -> Vec<String> {
    list.into_iter().map(|arg| arg.into_string().expect("an argument that is text")).collect()
}

/// Every argument list the module can build from `guid`: every verb, with and without a selected subscription.
fn every_list(guid: &str) -> Vec<(String, Vec<OsString>)> {
    let mut lists = Vec::new();
    for known in SPEC.verbs {
        for chosen in [None, Some(guid)] {
            let args = if known.args.is_empty() { bare(known.id) } else { with_subscription(known.id, guid).expect("a guid") };
            let list = AZURE.argv(known, &args, &selection(chosen)).expect("an argument list");
            lists.push((format!("{} (selection {chosen:?})", known.id), list));
        }
    }
    lists
}

#[test]
fn the_local_and_exec_verbs_build_their_exact_argument_lists() {
    let set = verb("set_subscription");
    assert_eq!(set.effect, VerbEffect::Local, "it writes the tool's own configuration, so it goes through a card");
    let login = verb("login");
    assert_eq!(login.effect, VerbEffect::Exec);

    let args = with_subscription("set_subscription", SUBSCRIPTION).expect("a guid");
    let list = texts(AZURE.argv(set, &args, &selection(None)).expect("an argument list"));
    assert_eq!(list, ["account", "set", &format!("--subscription={SUBSCRIPTION}"), "--only-show-errors"]);

    // The argument names the subscription; a selection never replaces it, and an upper-case guid is written in lower case.
    let other = "00000000-0000-4000-8000-0000000000AB";
    let args = with_subscription("set_subscription", other).expect("a guid");
    let list = texts(AZURE.argv(set, &args, &selection(Some(SUBSCRIPTION))).expect("an argument list"));
    assert_eq!(list, ["account", "set", "--subscription=00000000-0000-4000-8000-0000000000ab", "--only-show-errors"]);

    // The sign-in has no flag: no tenant, no device-code switch; the terminal shows what the tool prints.
    assert_eq!(argv("login", None), ["login"]);
    assert_eq!(argv("login", Some(SUBSCRIPTION)), ["login"]);

    // The subscription argument is required.
    assert!(AZURE.argv(set, &bare("login"), &selection(None)).is_err(), "no subscription, no argument list");
    assert!(with_subscription("login", SUBSCRIPTION).is_err(), "the sign-in declares no argument");

    // The six lists of the reads and the two others, as built.
    assert_eq!(argv("subscriptions", Some(SUBSCRIPTION)), ["account", "list", "-o", "json", "--only-show-errors"]);
    assert_eq!(
        argv("groups", Some(SUBSCRIPTION)),
        ["group", "list", "-o", "json", "--only-show-errors", &format!("--subscription={SUBSCRIPTION}")]
    );
    assert_eq!(argv("groups", None), ["group", "list", "-o", "json", "--only-show-errors"]);

    // A verb of another description with the id of one of ours is refused.
    let mut forged = *set;
    forged.effect = VerbEffect::Read;
    assert!(AZURE.argv(&forged, &args, &selection(None)).is_err());
}

#[test]
fn a_hostile_subscription_never_changes_the_az_argument_list() {
    let bad_guid_only = [
        "My Subscription".to_owned(),
        format!("{{{SUBSCRIPTION}}}"),
        format!("{SUBSCRIPTION} "),
        format!(" {SUBSCRIPTION}"),
        format!("{SUBSCRIPTION}\n"),
        format!("{SUBSCRIPTION}\u{0}"),
        "--subscription=x".to_owned(),
        format!("--subscription={SUBSCRIPTION}"),
        format!("-{SUBSCRIPTION}"),
        format!("{SUBSCRIPTION}0"),
        SUBSCRIPTION[..35].to_owned(),
        SUBSCRIPTION.replace('-', ""),
        SUBSCRIPTION.replace("0000", "zzzz"),
        format!("{SUBSCRIPTION}{}", ch(0x1b)),
        format!("{}{SUBSCRIPTION}", ch(0x202e)),
        format!("{}{}", &SUBSCRIPTION[..35], ch(0x200b)),
        format!("{SUBSCRIPTION}&calc"),
        format!("{SUBSCRIPTION}%PATH%"),
        format!("{SUBSCRIPTION}^"),
        format!("{SUBSCRIPTION}\" --debug"),
        "x".repeat(5000),
        String::new(),
    ];
    assert_eq!(SUBSCRIPTION.len(), 36);
    for text in &bad_guid_only {
        assert!(with_subscription("set_subscription", text).is_err(), "{text:?} must not be an argument");
        let chosen = Selection { subscription: Some(text.clone()), ..Selection::default() };
        assert!(chosen.check(ToolId::Azure).is_err(), "{text:?} must not be a selection");
    }
    // 37 and 35 characters.
    assert!(with_subscription("set_subscription", &format!("{SUBSCRIPTION}a")).is_err());
    assert!(with_subscription("set_subscription", &SUBSCRIPTION[1..]).is_err());
    // A hostile value for a verb without that argument is refused as unknown.
    for known in ["account", "subscriptions", "groups", "login"] {
        assert!(with_subscription(known, "--subscription=x").is_err());
    }
    // The only text that is let through is a guid, and it ends up as one element of the exact shape.
    let args = with_subscription("set_subscription", &SUBSCRIPTION.to_uppercase()).expect("a guid in upper case");
    let list = texts(AZURE.argv(verb("set_subscription"), &args, &selection(None)).expect("an argument list"));
    assert_eq!(list.len(), 4);
    assert_eq!(list[2], format!("--subscription={SUBSCRIPTION}"));
}

#[test]
fn every_argument_list_passes_the_batch_file_charset() {
    use holzwave_core::integrate::run::check_batch_args;
    use holzwave_core::platform::Os;

    let mut count = 0;
    for guid in [SUBSCRIPTION, "ABCDEF01-2345-4789-8ABC-DEF012345678", "ffffffff-ffff-4fff-8fff-ffffffffffff"] {
        for (what, list) in every_list(guid) {
            assert!(check_batch_args("az.cmd", &list, Os::Windows).is_ok(), "{what}: {list:?}");
            assert!(check_batch_args("C:\\Tools\\AZ.CMD", &list, Os::Windows).is_ok(), "{what}: {list:?}");
            assert!(!list.iter().any(|arg| arg.is_empty()), "{what} holds an empty argument");
            count += 1;
        }
    }
    assert_eq!(count, 3 * SPEC.verbs.len() * 2, "every verb and both selections were swept");
    // The sweep would catch an argument outside the charset.
    let spaced = [OsString::from("--subscription=my sub")];
    assert!(check_batch_args("az.cmd", &spaced, Os::Windows).is_err());
}

#[test]
fn no_argument_list_holds_a_token_clear_refresh_or_verbose_flag() {
    let guid_flag = format!("--subscription={SUBSCRIPTION}");
    let allowed = [
        "account",
        "show",
        "list",
        "set",
        "group",
        "login",
        "-o",
        "json",
        "--only-show-errors",
        guid_flag.as_str(),
    ];
    let never = [
        "get-access-token",
        "clear",
        "--refresh",
        "-v",
        "--verbose",
        "--debug",
        "--query",
        "--output=table",
        "--output",
        "--tenant",
        "--use-device-code",
        "--service-principal",
        "--password",
        "-p",
        "--yes",
        "-y",
        "delete",
        "logout",
        "rest",
    ];
    for (what, list) in every_list(SUBSCRIPTION) {
        let list = texts(list);
        for arg in &list {
            assert!(allowed.contains(&arg.as_str()), "{what}: {arg:?} is not in the catalogue of the module");
            assert!(!never.contains(&arg.as_str()), "{what}: {arg:?}");
        }
        // Every read asks for JSON and errors only; the two others never carry the format.
        let is_read = list.first().is_some_and(|first| first == "account" || first == "group")
            && list.get(1).is_some_and(|second| second == "show" || second == "list");
        if is_read {
            assert!(list.windows(2).any(|pair| pair == ["-o", "json"]), "{what}");
            assert!(list.iter().any(|arg| arg == "--only-show-errors"), "{what}");
        }
    }
}
