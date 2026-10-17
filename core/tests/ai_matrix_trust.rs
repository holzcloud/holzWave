//! The provider matrix, consent and keys: a context approved for another provider is refused on every provider before
//! any connection, the approved envelope goes out once and the stored text never, an earlier context note is treated by
//! one contract and one wording on every dialect, a key stays in its own header and dies with its address, and nothing a
//! built provider prints holds a key or text. No outside network.
//!
//! Approved contexts are made through the public gate; there is no other way to make one.

mod common;
#[path = "ai_matrix/streams.rs"]
mod streams;
#[path = "ai_matrix/support.rs"]
mod support;

use std::sync::Arc;

use common::fake_http::{FakeHttp, Reply};
use holzwave_core::ai::gate::{ApprovedContext, Destination, Gate, OfferRequest, RequestItem};
use holzwave_core::ai::http::{Http, HttpConfig, Origin};
use holzwave_core::ai::profiles::{KeySlot, ResolveError};
use holzwave_core::ai::provider::{AiError, ChatRequest};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::scripted::{Script, ScriptedProvider};
use holzwave_core::ai::service::dispatch::adapter_for;
use holzwave_core::ai::types::{Consent, ContextKind, ContextNote, Locality, Message, NoteItem, Part, Role};
use holzwave_core::ai::wire::Wire;
use serde_json::Value;
use streams::stream;
use support::{
    CANARY_STORED, CANARY_TEXT, Cell, GATEWAY_HEADER, Keys, RigOptions, Scenario, Side, assert_clean, canary_gateway_key,
    canary_key, cell, cells, chat_request, count_in, header_names, hostile, is_unsafe_char, json_body, key_header, reply_for,
    rig, run, texts_of, try_rig, try_rig_with, upsert_again,
};

/// A context text of the envelope rows.
const CONTEXT_TEXT: &str = "CANARY-CONTEXT-TEXT-77e1";

/// A context approved through the public gate for `provider`: one offer, every item picked.
fn mint(provider: &str, items: &[(ContextKind, &str, &str)]) -> ApprovedContext {
    let gate = Gate::new();
    let destination = Destination { host: "127.0.0.1".to_owned(), locality: Locality::Loopback };
    let items = items.iter().map(|(kind, label, text)| RequestItem::new(*kind, *label, SourceText::plain(*text))).collect();
    let offer = gate.offer(OfferRequest::new(provider, destination, items)).expect("the gate makes an offer");
    let picked: Vec<u32> = offer.items().iter().map(|item| item.id()).collect();
    gate.approve(offer.id(), &picked, provider).expect("the gate approves every item")
}

/// The request of a cell that carries `approved` before the user's text.
fn with_context(cell: &Cell, approved: ApprovedContext, stored: Option<&str>) -> ChatRequest {
    let mut request = chat_request(cell, "hello");
    let note = ContextNote::for_approved(&approved, stored.map(str::to_owned));
    request.messages[0].parts.insert(0, Part::Context(note));
    request.context.push(approved);
    request
}

fn keys_of(cell: &Cell) -> Keys {
    if cell.cloudflare() { Keys::Both } else { Keys::Provider }
}

fn text_reply(cell: &Cell) -> Reply {
    reply_for(cell.wire, vec![stream(cell.wire, Scenario::Text).bytes])
}

#[test]
fn a_context_approved_for_another_provider_is_refused_on_every_provider_before_any_connection() {
    for cell in cells() {
        let built = rig(&cell, vec![text_reply(&cell)], keys_of(&cell), HttpConfig::standard());
        let foreign = mint("someone-else", &[(ContextKind::Screen, "Screen of pane 1", CANARY_TEXT)]);
        let result = run(&built.provider, &with_context(&cell, foreign, None));
        let error = result.result.err().unwrap_or_else(|| panic!("{}: a foreign context was accepted", cell.name));
        assert_eq!(error.class().as_str(), "policy", "{}: {error}", cell.name);
        assert!(result.events.is_empty(), "{}: an event was delivered", cell.name);
        assert_eq!(built.fake.accepted(), 0, "{}: a connection was opened", cell.name);
        assert_eq!(built.http.attempts(), 0, "{}: a request was sent", cell.name);
        assert_clean(cell.name, &error, &[CANARY_TEXT, "Screen of pane 1"]);

        // The same request approved for the provider itself goes through.
        let own = mint(&built.id, &[(ContextKind::Screen, "Screen of pane 1", CANARY_TEXT)]);
        let result = run(&built.provider, &with_context(&cell, own, None));
        assert!(result.result.is_ok(), "{}: the own context was refused: {:?}", cell.name, result.result.as_ref().err());
        assert_eq!(built.fake.accepted(), 1, "{}: one connection for the own context", cell.name);
    }

    // The scripted provider holds the same rule.
    let scripted = ScriptedProvider::new("scripted").with_turn(Script::reply("ok"));
    let mut request = ChatRequest::new("model");
    let foreign = mint("someone-else", &[(ContextKind::Screen, "Screen of pane 1", CANARY_TEXT)]);
    let note = ContextNote::for_approved(&foreign, None);
    request.messages.push(Message::new("u1", Role::User, vec![Part::Context(note), Part::Text { text: "hi".to_owned() }], 1));
    request.context.push(foreign);
    let result = run(&scripted, &request);
    let error = result.result.err().expect("a foreign context is refused");
    assert_eq!(error.class().as_str(), "policy", "{error}");
    assert!(scripted.requests().is_empty() && scripted.calls() == 0, "the scripted provider recorded a request");
    assert_clean("scripted", &error, &[CANARY_TEXT, "Screen of pane 1"]);
}

#[test]
fn the_approved_envelope_reaches_every_wire_once_and_the_stored_text_never() {
    for cell in cells() {
        let built = rig(&cell, vec![text_reply(&cell)], keys_of(&cell), HttpConfig::standard());
        let approved = mint(
            &built.id,
            &[(ContextKind::Screen, "Screen of pane 1", CONTEXT_TEXT), (ContextKind::File, "notes.txt", "second item text")],
        );
        let envelope = approved.envelope().to_owned();
        let result = run(&built.provider, &with_context(&cell, approved, Some(CANARY_STORED)));
        assert!(result.result.is_ok(), "{}: the turn failed: {:?}", cell.name, result.result.as_ref().err());
        let seen = built.fake.seen();
        let text = seen[0].body_text();
        let body = json_body(cell.name, &seen[0]);
        let user = texts_of(cell.wire, &body, Side::User);
        let system = texts_of(cell.wire, &body, Side::System);
        let in_user: usize = user.iter().map(|part| count_in(part, &envelope)).sum();
        assert_eq!(in_user, 1, "{}: the envelope occurs once on the user side", cell.name);
        assert_eq!(system.iter().map(|part| count_in(part, "holzwave-context")).sum::<usize>(), 0, "{}: system side", cell.name);
        assert_eq!(count_in(&text, CONTEXT_TEXT), 1, "{}: the context text occurs once in the body", cell.name);
        assert_eq!(count_in(&text, CANARY_STORED), 0, "{}: the stored text was sent", cell.name);
        let joined = user.join("\n");
        let at = joined.find(&envelope).expect("the envelope");
        assert!(joined[at + envelope.len()..].contains("hello"), "{}: the user's own text follows the envelope", cell.name);
    }
}

// ---- the marker line --------------------------------------------------------------------------------------

/// The four items of an earlier note. The label of the last one has a hostile tail of 300 characters.
fn marker_note() -> ContextNote {
    let escape = char::from_u32(0x1b).expect("escape");
    let override_ = char::from_u32(0x202e).expect("override");
    let zero_width = char::from_u32(0x200b).expect("zero-width space");
    let tag = char::from_u32(0xe0041).expect("tag character");
    let mut tail = format!("{escape}[31m{override_}{zero_width}{tag}\nnext line ");
    while tail.chars().count() < 300 {
        tail.push('t');
    }
    let label = format!("Selection of the pane {tail}");
    ContextNote {
        hash: "00".repeat(32),
        items: vec![
            NoteItem { kind: ContextKind::Screen, label: "Screen of pane 1".to_owned(), bytes: 120 },
            NoteItem { kind: ContextKind::LastCommand, label: "make test".to_owned(), bytes: 4096 },
            NoteItem { kind: ContextKind::File, label: "notes.txt".to_owned(), bytes: 40 },
            NoteItem { kind: ContextKind::Selection, label, bytes: 77 },
        ],
        stored: Some(CANARY_STORED.to_owned()),
        consented_to: Consent { provider: "earlier".to_owned(), locality: Locality::Cloud },
    }
}

/// The request of a message that holds the note and then the text `explain`.
fn marker_request(cell: &Cell, approved: Option<ApprovedContext>) -> ChatRequest {
    let mut request = ChatRequest::new(cell.model);
    request.system = support::SYSTEM_TEXT.to_owned();
    let note = match &approved {
        Some(context) => ContextNote::for_approved(context, Some(CANARY_STORED.to_owned())),
        None => marker_note(),
    };
    request.messages.push(Message::new(
        "u1",
        Role::User,
        vec![Part::Context(note), Part::Text { text: "explain".to_owned() }],
        1,
    ));
    request.context.extend(approved);
    request
}

/// The body of the pure request of a wire.
fn body_of(wire: Wire, request: &ChatRequest) -> (String, Value) {
    let origin = Origin::from_scope("http://127.0.0.1:1").expect("origin");
    let adapter = adapter_for(wire, &origin);
    let caps = adapter.capabilities(&request.model, Locality::Loopback);
    let built = adapter.request(request, &caps).unwrap_or_else(|error| panic!("{}: refused: {error}", wire.tag()));
    let text = String::from_utf8_lossy(&built.body).into_owned();
    let value = serde_json::from_str(&text).expect("a JSON body");
    (text, value)
}

/// The lines of the user side that are one bracketed line.
fn bracketed(user: &[String]) -> Vec<String> {
    user.iter()
        .flat_map(|part| part.lines())
        .map(str::trim)
        .filter(|line| line.starts_with('[') && line.ends_with(']'))
        .map(str::to_owned)
        .collect()
}

/// Checks the contract for the line of an earlier note on one wire and returns the line.
fn check_marker(wire: Wire, cell: &Cell) -> String {
    let tag = wire.tag();
    let request = marker_request(cell, None);
    let (text, body) = body_of(wire, &request);
    let user = texts_of(wire, &body, Side::User);
    let system = texts_of(wire, &body, Side::System);

    // M1 one line.
    let lines = bracketed(&user);
    assert_eq!(lines.len(), 1, "{tag}: M1 exactly one bracketed line: {lines:?}");
    let line = lines[0].clone();
    assert!(!line.contains('\n') && !line.contains('\r'), "{tag}: M1 a line feed in the line");

    // M2 role and place: in the user text, before the user's own text, never on the system side.
    let joined = user.join("\n");
    assert!(joined.find(&line) < joined.find("explain"), "{tag}: M2 the line comes before the user's text");
    assert!(system.iter().all(|part| !part.contains(&line) && !part.contains('[')), "{tag}: M2 the system side holds a bracket");

    // M3 facts in note order.
    let mut from = 0;
    for lead in ["Screen of pane 1", "make test", "notes.txt", "Selection of the pane"] {
        let at = line[from..].find(lead).unwrap_or_else(|| panic!("{tag}: M3 {lead:?} is missing or out of order in {line}"));
        from += at + lead.len();
    }
    let runs: Vec<&str> = line.split(|c: char| !c.is_ascii_digit()).filter(|run| !run.is_empty()).collect();
    let mut next = 0;
    for count in ["120", "4096", "40", "77"] {
        let at = runs[next..].iter().position(|run| *run == count).unwrap_or_else(|| panic!("{tag}: M3 byte count {count} is missing or out of order: {runs:?}"));
        next += at + 1;
    }

    // M4 cleaning and bounds.
    assert!(!line.chars().any(is_unsafe_char), "{tag}: M4 an unsafe character in the line: {line:?}");
    assert!(!line.chars().any(|c| ('\u{7f}'..='\u{9f}').contains(&c)), "{tag}: M4 a C1 control in the line");
    let longest = line.split(|c| c != 't').map(str::len).max().unwrap_or(0);
    assert!(longest <= 240, "{tag}: M4 a label contributes {longest} characters");
    assert!(line.chars().count() <= 2048, "{tag}: M4 the line has {} characters", line.chars().count());

    // M5 exclusion.
    assert!(!text.contains("holzwave-context") && !text.contains("holzwave-item"), "{tag}: M5 the envelope grammar is in the body");
    assert!(!text.contains(CANARY_STORED), "{tag}: M5 the stored text is in the body");

    // M6 deterministic.
    let (again, _) = body_of(wire, &request);
    assert_eq!(text, again, "{tag}: M6 the same request gave other bytes");

    // M7 approval flips it.
    let approved = mint(
        "earlier",
        &[(ContextKind::Screen, "Screen of pane 1", CONTEXT_TEXT), (ContextKind::File, "notes.txt", "second item text")],
    );
    let envelope = approved.envelope().to_owned();
    let (with_approval, with_body) = body_of(wire, &marker_request(cell, Some(approved)));
    let with_user = texts_of(wire, &with_body, Side::User);
    assert_eq!(with_user.iter().map(|part| count_in(part, &envelope)).sum::<usize>(), 1, "{tag}: M7 the envelope occurs once");
    assert!(bracketed(&with_user).is_empty(), "{tag}: M7 a marker line next to the envelope");
    assert!(!with_approval.contains(CANARY_STORED), "{tag}: M7 the stored text is in the body");
    line
}

#[test]
fn an_unapproved_note_becomes_one_marker_line_that_every_wire_treats_alike() {
    let custom = ["custom_chat", "custom_ollama", "custom_anthropic", "custom_responses", "custom_gemini"].map(cell);
    let mut lines = Vec::new();
    for cell in &custom {
        let line = check_marker(cell.wire, cell);
        println!("marker line {}: {line}", cell.wire.tag());
        lines.push((cell.wire, line));
    }
    // M8 sameness: one wording on every dialect.
    for (wire, line) in &lines {
        assert_eq!(line, &lines[0].1, "{}: M8 the line differs from the Chat Completions line", wire.tag());
    }
    // A label that holds a secret reaches no dialect: every wire cleans it the same way.
    let secret = "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let leaky = ContextNote {
        hash: "00".repeat(32),
        items: vec![NoteItem { kind: ContextKind::File, label: format!("deploy key {secret}"), bytes: 9 }],
        stored: None,
        consented_to: Consent { provider: "earlier".to_owned(), locality: Locality::Cloud },
    };
    for cell in &custom {
        let mut request = ChatRequest::new(cell.model);
        request.messages.push(Message::new("u1", Role::User, vec![Part::Context(leaky.clone()), Part::Text { text: "explain".to_owned() }], 1));
        let (text, _) = body_of(cell.wire, &request);
        assert!(!text.contains(secret), "{}: a secret in a label reached the request", cell.wire.tag());
    }
    // A note with far more items than a gate allows still gives one line within the bound.
    let many = ContextNote {
        hash: "00".repeat(32),
        items: (0..60)
            .map(|index| NoteItem { kind: ContextKind::File, label: format!("file number {index} {}", "n".repeat(150)), bytes: index })
            .collect(),
        stored: None,
        consented_to: Consent { provider: "earlier".to_owned(), locality: Locality::Cloud },
    };
    for cell in &custom {
        let mut request = ChatRequest::new(cell.model);
        request.messages.push(Message::new("u1", Role::User, vec![Part::Context(many.clone()), Part::Text { text: "explain".to_owned() }], 1));
        let (_, body) = body_of(cell.wire, &request);
        let lines = bracketed(&texts_of(cell.wire, &body, Side::User));
        assert_eq!(lines.len(), 1, "{}: one line for a large note", cell.wire.tag());
        assert!(lines[0].chars().count() <= 2048, "{}: the line has {} characters", cell.wire.tag(), lines[0].chars().count());
        assert!(lines[0].ends_with(" more]"), "{}: the line says how many items were left out: {}", cell.wire.tag(), lines[0]);
    }
    // The wording of a clean note, pinned.
    let clean = ContextNote {
        hash: "00".repeat(32),
        items: vec![
            NoteItem { kind: ContextKind::Screen, label: "Screen of pane 1".to_owned(), bytes: 120 },
            NoteItem { kind: ContextKind::LastCommand, label: "make \"test\"".to_owned(), bytes: 4096 },
        ],
        stored: None,
        consented_to: Consent { provider: "earlier".to_owned(), locality: Locality::Cloud },
    };
    for cell in &custom {
        let mut request = ChatRequest::new(cell.model);
        request.messages.push(Message::new("u1", Role::User, vec![Part::Context(clean.clone()), Part::Text { text: "explain".to_owned() }], 1));
        let (_, body) = body_of(cell.wire, &request);
        let user = texts_of(cell.wire, &body, Side::User);
        assert_eq!(
            bracketed(&user),
            vec![
                "[terminal context from an earlier message is not sent again: screen \"Screen of pane 1\" (120 bytes); last_command \"make 'test'\" (4096 bytes)]"
                    .to_owned()
            ],
            "{}: the pinned wording",
            cell.wire.tag()
        );
    }
}

// ---- keys -------------------------------------------------------------------------------------------------

#[test]
fn a_key_is_sent_in_its_own_header_only_and_a_changed_address_refuses_it_on_every_cell() {
    for cell in cells() {
        let built = rig(&cell, vec![text_reply(&cell)], keys_of(&cell), HttpConfig::standard());
        let result = run(&built.provider, &chat_request(&cell, "hello"));
        assert!(result.result.is_ok(), "{}: the turn failed", cell.name);
        let seen = built.fake.seen();
        let request = &seen[0];
        let (key_name, prefix) = key_header(cell.wire);
        let holding: Vec<&str> =
            request.headers.iter().filter(|(_, value)| value.contains(canary_key())).map(|(name, _)| name.as_str()).collect();
        assert_eq!(holding, vec![key_name], "{}: the key is in its own header only", cell.name);
        assert_eq!(request.header(key_name), Some(format!("{prefix}{}", canary_key()).as_str()), "{}", cell.name);
        assert!(!request.target.contains(canary_key()) && !request.body_text().contains(canary_key()), "{}: key leak", cell.name);
        let gateway: Vec<&str> =
            request.headers.iter().filter(|(_, value)| value.contains(canary_gateway_key())).map(|(name, _)| name.as_str()).collect();
        if cell.cloudflare() {
            assert_eq!(gateway, vec![GATEWAY_HEADER], "{}: the gateway token is in its own header only", cell.name);
        } else {
            assert!(gateway.is_empty(), "{}: a gateway token was sent", cell.name);
        }

        // The profile moves to another address: the key is refused, and nothing reaches the new server.
        let mut built = built;
        let second = FakeHttp::start(vec![text_reply(&cell)]);
        let moved = format!("{}{}", second.base(), cell.base_suffix);
        upsert_again(&mut built.config, &built.id, &cell, &moved, false);
        assert_eq!(
            built.config.resolve(&built.id, &built.vault, false).err(),
            Some(ResolveError::KeyScope(KeySlot::Provider)),
            "{}: the provider key after the address changed",
            cell.name
        );
        assert_eq!(second.accepted(), 0, "{}: the new server was reached", cell.name);
        support::store_keys(&mut built.config, &built.vault, &built.id, Keys::Provider, &cell);
        if cell.cloudflare() {
            assert_eq!(
                built.config.resolve(&built.id, &built.vault, false).err(),
                Some(ResolveError::KeyScope(KeySlot::Gateway)),
                "{}: the gateway token after the address changed",
                cell.name
            );
            support::store_keys(&mut built.config, &built.vault, &built.id, Keys::GatewayOnly, &cell);
        }
        let resolved = built
            .config
            .resolve(&built.id, &built.vault, false)
            .unwrap_or_else(|error| panic!("{}: the keys entered again were refused: {error}", cell.name));
        let origin = Origin::from_scope(resolved.scope()).expect("origin");
        let provider = resolved.into_provider(adapter_for(cell.wire, &origin), Arc::new(Http::new(HttpConfig::standard())));
        let result = run(&provider, &chat_request(&cell, "hello"));
        assert!(result.result.is_ok(), "{}: the turn at the new address failed", cell.name);
        let seen = second.seen();
        assert_eq!(seen.len(), 1, "{}: the new server saw one request", cell.name);
        assert_eq!(seen[0].header(key_name), Some(format!("{prefix}{}", canary_key()).as_str()), "{}: the key at the new address", cell.name);
        assert_eq!(header_names(&seen[0]).iter().filter(|name| seen[0].header(name).is_some_and(|value| value.contains(canary_key()))).count(), 1, "{}", cell.name);
    }
}

#[test]
fn no_built_provider_prints_a_key_or_text_in_debug_or_in_an_error() {
    for cell in cells() {
        let options = RigOptions::keyed(keys_of(&cell));
        let fake = FakeHttp::start(vec![text_reply(&cell)]);
        let vault = support::test_vault();
        let mut config = holzwave_core::ai::profiles::AiConfig::default();
        let address = format!("{}{}", fake.base(), cell.base_suffix);
        let id = support::save_profile(&mut config, &vault, &cell, &address, &options);

        let resolved = config.resolve(&id, &vault, false).expect("resolved");
        let http = Arc::new(Http::new(HttpConfig::standard()));
        let mut shown = vec![format!("{resolved:?}"), format!("{resolved:#?}"), format!("{http:?}"), format!("{http:#?}")];
        let origin = Origin::from_scope(resolved.scope()).expect("origin");
        let provider = resolved.into_provider(adapter_for(cell.wire, &origin), Arc::clone(&http));
        shown.push(format!("{provider:?}"));
        shown.push(format!("{provider:#?}"));
        let views = serde_json::to_string(&config.views(&vault)).expect("views serialise");
        let views_pretty = serde_json::to_string_pretty(&config.views(&vault)).expect("views serialise");
        shown.push(views.clone());
        shown.push(views_pretty);
        for text in &shown {
            for canary in [canary_key(), canary_gateway_key(), CANARY_TEXT] {
                assert!(!text.contains(canary), "{}: a canary is in a printed value: {text}", cell.name);
            }
        }
        for name in ["authorization", "x-api-key", "x-goog-api-key", GATEWAY_HEADER] {
            assert!(!views.contains(name), "{}: the views name the header {name}", cell.name);
        }

        // The errors of the failing paths.
        let mut errors: Vec<AiError> = Vec::new();
        if cell.key == support::KeyRule::Required {
            let missing = try_rig(&cell, vec![Reply::Close], Keys::None).err().expect("a missing key");
            assert_eq!(missing, ResolveError::KeyMissing(KeySlot::Provider));
            shown_error(&cell, &format!("{missing:?} {missing}"));
            errors.push(AiError::from(missing));
        }
        let foreign = mint("someone-else", &[(ContextKind::Screen, "Screen of pane 1", CANARY_TEXT)]);
        let refusal = run(&provider, &with_context(&cell, foreign, Some(CANARY_STORED)));
        errors.push(refusal.result.err().expect("a foreign context is refused"));
        let mut moved = RigOptions::keyed(keys_of(&cell));
        moved.origin = Some(format!("http://127.0.0.1:{}", support::closed_port()));
        let rebuilt = try_rig_with(&cell, Vec::new(), moved).expect("a rig");
        let mut rebuilt = rebuilt;
        let other = FakeHttp::start(Vec::new());
        upsert_again(&mut rebuilt.config, &rebuilt.id, &cell, &format!("{}{}", other.base(), cell.base_suffix), false);
        let scope = rebuilt.config.resolve(&rebuilt.id, &rebuilt.vault, false).err().expect("a key for another address");
        shown_error(&cell, &format!("{scope:?} {scope}"));
        errors.push(AiError::from(scope));
        for error in &errors {
            assert_clean(cell.name, error, &[canary_key(), canary_gateway_key(), CANARY_TEXT, CANARY_STORED, "Screen of pane 1"]);
        }
    }
}

fn shown_error(cell: &Cell, shown: &str) {
    for canary in [canary_key(), canary_gateway_key(), CANARY_TEXT] {
        assert!(!shown.contains(canary), "{}: a canary is in an error: {shown}", cell.name);
    }
    let _ = hostile;
}
