//! Nothing leaves the machine before a send, and nothing secret leaves with one.
//!
//! The service is built the way the shell builds it, over the in-process server, and every observation is made from
//! the outside: what the server accepted and received, what the stored chat holds, what a view says. This is test
//! code only. A failing assertion names the canary or the part (the target, a header name, the body), never the bytes.

mod common;
mod security;

use std::collections::BTreeSet;
use std::fs;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use common::fake_http::FakeHttp;
use holzwave_core::ai::gate::{Destination, Gate, OfferId, OfferRequest, RequestItem};
use holzwave_core::ai::http::{Http, HttpConfig};
use holzwave_core::ai::profiles::{AiConfig, KeySlot, KeyVault, Preset, ProfileInput};
use holzwave_core::ai::provider::{AiError, CancelToken, ChatRequest, Flow, Provider};
use holzwave_core::ai::redact::{SourceText, prepare};
use holzwave_core::ai::scripted::Script;
use holzwave_core::ai::service::{AskFault, AskInput, AskLimits, AskVerdict, SendRequest, ServiceError};
use holzwave_core::ai::store::NOT_STORED;
use holzwave_core::ai::tools::{Decision, DenyPolicy};
use holzwave_core::ai::types::{ApiKey, ContextKind, ContextNote, Locality, Message, Part, Role};
use holzwave_core::ai::wire::Wire;
use holzwave_core::ai::wire::openai_chat::OpenAiChatAdapter;
use holzwave_core::secrets::{MemoryStore, SecretStore};
use holzwave_core::settings::{AiLocalContext, Settings};
use holzwave_core::sync::engine::{ConfigSource, Source, allowed};
use serde_json::{Value, json};

use security::canaries::{api_key, canaries, file_value, leaks, marker, terminal_text};
use security::rig::{
    AskOutput, Entry, PANE, PROFILE, RecordingResolver, Rig, RigOptions, body_strings, captures, chat_reply,
    everything, everything_except_key, tool_call_reply,
};
use security::world::{GateHost, PANE_A, Tree};

/// The wait that stands for "nothing happened".
const QUIET: Duration = Duration::from_millis(300);

/// The rules the canary set must trigger, written out here and not read from the gate.
const SECRET_RULES: [&str; 14] = [
    "private_key",
    "anthropic_key",
    "openai_key",
    "google_api_key",
    "github_token",
    "aws_access_key",
    "slack_token",
    "jwt",
    "authorization",
    "bearer_token",
    "cookie",
    "url_credentials",
    "secret_assignment",
    "high_entropy",
];

/// The two entries pane 7 offers: a screen full of canary secrets and a selection that holds only a marker.
fn pane_entries() -> Vec<Entry> {
    vec![
        Entry::new(PANE, ContextKind::Screen, "Screen of pane 7", terminal_text(&canaries())),
        Entry::new(PANE, ContextKind::Selection, "Selection of pane 7", marker("selection")),
    ]
}

/// Every file below `dir`, as bytes.
fn files_below(dir: &std::path::Path) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(files_below(&path));
            } else if let Ok(bytes) = fs::read(&path) {
                out.push(bytes);
            }
        }
    }
    out
}

#[test]
fn the_canary_set_covers_the_secret_rules_of_the_gate() {
    let set = canaries();
    assert!(set.len() >= 17, "the set has {} canaries", set.len());
    let rules: BTreeSet<&str> = set.iter().map(|canary| canary.rule).collect();
    for rule in SECRET_RULES {
        assert!(rules.contains(rule), "no canary triggers the rule {rule}");
    }
    let needles: BTreeSet<&str> = set.iter().map(|canary| canary.needle.as_str()).collect();
    assert_eq!(needles.len(), set.len(), "two canaries share a needle");
    for canary in &set {
        assert!(canary.needle.len() >= 20, "the needle of {} is too short", canary.name);
        // Each canary, alone, makes the gate report the rule it names and removes its needle.
        let cleaned = prepare(ContextKind::Screen, &SourceText::plain(canary.line.clone()));
        assert!(cleaned.hits.iter().any(|hit| hit.rule == canary.rule), "the gate does not report {}", canary.name);
        assert!(leaks(cleaned.text.as_bytes(), std::slice::from_ref(canary)).is_empty(), "{} survives", canary.name);
    }
    // The key is no canary shape and the terminal text holds every canary.
    let key = api_key();
    assert!(!needles.contains(key.as_str()));
    let page = terminal_text(&set);
    assert!(page.len() > 4096 && page.len() < 131_072, "the page is {} bytes", page.len());
    for canary in &set {
        assert!(page.contains(&canary.line), "the page lacks the line of {}", canary.name);
    }
    assert!(!page.contains(&key));
}

#[test]
fn a_shared_pane_full_of_canary_secrets_reaches_the_server_only_after_the_send_and_never_with_a_secret() {
    let set = canaries();
    let raw = terminal_text(&set);
    let rig = Rig::new(RigOptions { source: pane_entries(), ..RigOptions::default() });
    let (service, fake) = (rig.service(), rig.fake());
    let (picked_marker, unpicked_marker) = (marker("screen"), marker("selection"));

    // (1) Building the service and the idle acts open nothing.
    let chat = rig.chat();
    assert!(!service.profiles().is_empty());
    assert!(service.config_notes().is_empty());
    assert!(service.list_conversations().expect("a listing").chats.is_empty());
    std::thread::sleep(QUIET);
    assert_eq!((fake.accepted(), fake.seen().len()), (0, 0), "a connection before any act of the user");

    // (2) The acts of the user around a send make no request either.
    let state = service.context_state(&chat, PANE, Some(PROFILE), None).expect("a state");
    assert!(state.required && !state.shared, "the pane starts unshared");
    let state = service.context_share(&chat, PANE, Some(PROFILE), None, true).expect("a share");
    assert!(state.shared);
    let view = service
        .context_preview(&chat, PANE, Some(PROFILE), None, &[ContextKind::Screen, ContextKind::Selection])
        .expect("a preview");
    std::thread::sleep(QUIET);
    assert_eq!((fake.accepted(), fake.seen().len()), (0, 0), "a connection before the send");
    assert_eq!(view.items.len(), 2);

    // (3) The views hold no canary and no marker text, and the findings name every rule of the set.
    let shown = [
        serde_json::to_vec(&view).expect("the offer view serialises"),
        serde_json::to_vec(&state).expect("the state serialises"),
    ];
    for (index, text) in shown.iter().enumerate() {
        assert!(leaks(text, &set).is_empty(), "a canary is in view {index}");
        for marker_text in [&picked_marker, &unpicked_marker] {
            assert!(!String::from_utf8_lossy(text).contains(marker_text.as_str()), "a marker is in view {index}");
        }
    }
    let screen = view.items.iter().find(|item| item.kind == ContextKind::Screen).expect("the screen item");
    let selection = view.items.iter().find(|item| item.kind == ContextKind::Selection).expect("the selection item");
    let reported: BTreeSet<&str> = screen.findings.iter().map(|finding| finding.rule.as_str()).collect();
    for canary in &set {
        assert!(reported.contains(canary.rule), "the preview does not name the rule of {}", canary.name);
    }

    // (4) One send with only the screen item picked.
    let offer = OfferId::parse(&view.offer).expect("an offer id");
    let request = SendRequest::new(chat.clone(), "explain this screen".to_owned(), PANE)
        .profile(PROFILE)
        .offer(offer, vec![screen.id]);
    service.send(request).expect("the send is accepted");
    let ended = rig.sink().wait_ended(chat.as_str(), Duration::from_secs(10)).expect("the turn ends");
    assert_eq!(ended["end"], "done", "the turn ended otherwise");

    // (5) The server saw one request on one connection, with no canary and no unpicked marker in it.
    assert_eq!((fake.accepted(), fake.seen().len()), (1, 1), "connections and requests after the send");
    let all = everything(fake);
    assert!(leaks(&all, &set).is_empty(), "a canary reached the server: {:?}", leaks(&all, &set));
    let text = String::from_utf8_lossy(&all).into_owned();
    assert!(!text.contains(&unpicked_marker), "the marker of the unpicked item reached the server");
    let request = &captures(fake)[0];
    let cleaned = prepare(ContextKind::Screen, &SourceText::plain(raw)).text;
    let strings = body_strings(request);
    let envelope = strings.iter().find(|text| text.contains(&cleaned)).expect("the cleaned screen is in the body");
    for rule in SECRET_RULES {
        assert!(envelope.contains(&format!("[REDACTED:{rule}]")), "no typed placeholder for {rule}");
    }
    assert!(envelope.contains(&picked_marker), "the picked screen text is in the body");
    assert!(strings.iter().any(|text| text.contains("explain this screen")), "the typed text is in the body");

    // The key is in the header of its wire and nowhere else.
    let key = api_key();
    let authorization = request.headers.iter().find(|(name, _)| name == "authorization").map(|(_, value)| value.clone());
    assert_eq!(authorization, Some(format!("Bearer {key}")), "the header authorization");
    let carrying: Vec<&str> =
        request.headers.iter().filter(|(_, value)| value.contains(&key)).map(|(name, _)| name.as_str()).collect();
    assert_eq!(carrying, ["authorization"], "the headers that carry the key");
    assert!(!request.target.contains(&key), "the key is in the target");
    assert!(!String::from_utf8_lossy(&request.body).contains(&key), "the key is in the body");
    let rest = String::from_utf8_lossy(&everything_except_key(fake, "openai_chat")).into_owned();
    assert!(!rest.contains(&key), "the key is somewhere but its header");

    // (6) The stored chat keeps a marker for the context and the typed text, and nothing else of the screen.
    let (_, chats, _) = rig.dirs();
    let stored = files_below(&chats);
    assert!(!stored.is_empty(), "the chat was not stored");
    let joined: Vec<u8> = stored.concat();
    let stored_text = String::from_utf8_lossy(&joined).into_owned();
    assert!(leaks(&joined, &set).is_empty(), "a canary is in the stored chat");
    assert!(!stored_text.contains(&key), "the key is in the stored chat");
    assert!(!stored_text.contains(&unpicked_marker), "the unpicked marker is in the stored chat");
    assert!(!stored_text.contains(&picked_marker), "the screen text is in the stored chat");
    assert!(stored_text.contains("explain this screen"), "the typed text is not in the stored chat");
    assert!(stored_text.contains("Screen of pane 7"), "the label of the note is not stored");
    let parsed: Value = serde_json::from_slice(&stored[0]).expect("the chat file is JSON");
    assert!(parsed.to_string().contains(&screen.bytes.to_string()), "the size of the note is not stored");
    assert!(selection.bytes > 0);
}

// ---- helpers of the full egress gate ------------------------------------------------------------------------------

/// The settings of a test: the defaults with one change.
fn settings(change: impl FnOnce(&mut Settings)) -> Settings {
    let mut settings = Settings::default();
    change(&mut settings);
    settings
}

/// The input of the profile `Local` on `fake`, with this model.
fn local_input(fake: &FakeHttp, model: &str) -> ProfileInput {
    let mut input = ProfileInput::new("Local", Preset::Custom);
    input.wire = Some(Wire::OpenAiChat);
    input.base_url = format!("{}/v1", fake.base());
    input.model = model.to_owned();
    input.acknowledge_gateway = true;
    input
}

/// Sends one message to the profile `local` and waits for the end of the turn.
fn say(rig: &Rig, chat: &holzwave_core::ai::store::ConversationId, text: &str) -> Value {
    let before = rig.sink().ended(chat.as_str());
    rig.service()
        .send(SendRequest::new(chat.clone(), text.to_owned(), PANE).profile(PROFILE))
        .expect("the send is accepted");
    assert!(common::wait_until(Duration::from_secs(10), || rig.sink().ended(chat.as_str()) > before), "the turn ends");
    rig.sink().wait_ended(chat.as_str(), Duration::from_secs(1)).expect("an end")
}

fn context_reason(result: Result<impl std::fmt::Debug, ServiceError>) -> &'static str {
    match result {
        Err(ServiceError::Context(refusal)) => refusal.reason(),
        other => panic!("expected a context refusal, got {other:?}"),
    }
}

// ---- idle: no socket, no lookup -----------------------------------------------------------------------------------

#[test]
fn a_fully_configured_service_opens_no_socket_and_makes_no_lookup_until_a_send() {
    let resolver = Arc::new(RecordingResolver::default());
    let rig = Rig::new(RigOptions {
        profile_per_preset: true,
        local_profile: false,
        resolver: Some(resolver.clone()),
        source: pane_entries(),
        ..RigOptions::default()
    });
    let (service, fake) = (rig.service(), rig.fake());
    let quiet = |step: &str| {
        assert!(resolver.lookups().is_empty(), "a lookup after {step}");
        assert_eq!((fake.accepted(), fake.seen().len()), (0, 0), "a connection after {step}");
    };
    quiet("the build and the profiles of every preset");

    let chat = rig.chat();
    quiet("new_conversation");
    service.list_conversations().expect("a listing");
    quiet("list_conversations");
    service.snapshot(&chat).expect("a snapshot");
    quiet("snapshot");
    let views = service.profiles();
    assert_eq!(views.len(), 11, "one profile for each preset");
    quiet("profiles");
    assert!(service.config_notes().is_empty());
    quiet("config_notes");
    assert_eq!(service.gate().pending(), 0);
    assert!(service.approvals().pending(chat.as_str()).is_empty());
    quiet("gate and approvals");
    for local_only in [false, true] {
        for persist in [false, true] {
            for context in [AiLocalContext::Ask, AiLocalContext::Auto] {
                service.apply_settings(&settings(|s| {
                    s.ai_local_only = local_only;
                    s.ai_persist_context = persist;
                    s.ai_local_context = context;
                }));
                quiet("apply_settings");
            }
        }
    }
    service.apply_settings(&Settings::default());
    std::thread::sleep(QUIET);
    quiet("a pause");

    // The only lookup is the one a user act names: the host of the profile the act is about.
    let cloud = views.iter().find(|view| view.id == "anthropic").expect("the profile of the Anthropic preset");
    let state = service.context_state(&chat, PANE, Some("anthropic"), None).expect("a state");
    assert_eq!(state.locality, Locality::Cloud);
    assert_eq!(resolver.lookups(), [cloud.host.clone()], "the lookups after the user act");
    let state = service.context_state(&chat, PANE, Some("custom"), None).expect("a state");
    assert_eq!(state.locality, Locality::Loopback);
    assert_eq!(resolver.lookups().len(), 1, "a loopback address needs no lookup");
    assert_eq!((fake.accepted(), fake.seen().len()), (0, 0), "a user act is no request");
    assert_eq!(service.shutdown(Duration::from_secs(2)), 0);

    // A service with no profile and no key leaves both at zero as well.
    let bare_resolver = Arc::new(RecordingResolver::default());
    let bare = Rig::new(RigOptions {
        local_profile: false,
        resolver: Some(bare_resolver.clone()),
        ..RigOptions::default()
    });
    assert!(bare.service().profiles().is_empty());
    let chat = bare.chat();
    let refused = bare.service().send(SendRequest::new(chat, "hello".to_owned(), PANE));
    assert!(matches!(refused, Err(ServiceError::NoProfile)), "a send with no profile is refused");
    std::thread::sleep(QUIET);
    assert!(bare_resolver.lookups().is_empty());
    assert_eq!((bare.fake().accepted(), bare.fake().seen().len()), (0, 0));
}

// ---- profile and key acts never probe ------------------------------------------------------------------------------

#[test]
fn profile_and_key_acts_never_probe_the_server() {
    let rig = Rig::new(RigOptions { local_profile: false, ..RigOptions::default() });
    let (service, fake) = (rig.service(), rig.fake());
    let quiet = |step: &str| assert_eq!(fake.accepted(), 0, "a connection after {step}");

    let id = service.save_profile(local_input(fake, "test-model")).expect("a profile");
    quiet("save_profile");
    service.profiles();
    service.set_key(&id, KeySlot::Provider, ApiKey::new(api_key())).expect("a key");
    quiet("set_key");
    service.profiles();
    let mut changed = local_input(fake, "other-model");
    changed.id = Some(id.clone());
    service.save_profile(changed).expect("a changed profile");
    quiet("save_profile with another model");
    service.profiles();
    service.clear_key(&id, KeySlot::Provider).expect("a cleared key");
    quiet("clear_key");
    service.profiles();
    service.set_key(&id, KeySlot::Provider, ApiKey::new(api_key())).expect("a key");
    quiet("set_key again");
    service.remove_profile(&id).expect("a removal");
    quiet("remove_profile");
    assert!(service.profiles().is_empty());
    std::thread::sleep(QUIET);
    quiet("a pause");

    // The control: the same acts and one message make exactly one request.
    let id = service.save_profile(local_input(fake, "test-model")).expect("a profile");
    service.set_key(&id, KeySlot::Provider, ApiKey::new(api_key())).expect("a key");
    let chat = rig.chat();
    let ended = say(&rig, &chat, "hello");
    assert_eq!(ended["end"], "done");
    assert_eq!((fake.accepted(), fake.seen().len()), (1, 1), "one message is one request");
}

// ---- only the picked items leave, once ------------------------------------------------------------------------------

#[test]
fn only_the_picked_items_leave_once_and_a_second_turn_carries_no_context_text() {
    let set = canaries();
    let rig = Rig::new(RigOptions { source: pane_entries(), ..RigOptions::default() });
    let (service, fake) = (rig.service(), rig.fake());
    let chat = rig.chat();
    service.context_share(&chat, PANE, Some(PROFILE), None, true).expect("a share");
    let view = service
        .context_preview(&chat, PANE, Some(PROFILE), None, &[ContextKind::Screen, ContextKind::Selection])
        .expect("a preview");
    let screen = view.items.iter().find(|item| item.kind == ContextKind::Screen).expect("the screen item").id;
    let offer = OfferId::parse(&view.offer).expect("an offer id");
    let first = SendRequest::new(chat.clone(), "explain this screen".to_owned(), PANE)
        .profile(PROFILE)
        .offer(offer.clone(), vec![screen]);
    service.send(first).expect("the send is accepted");
    assert!(common::wait_until(Duration::from_secs(10), || rig.sink().ended(chat.as_str()) == 1));
    let ended = say(&rig, &chat, "second question");
    assert_eq!(ended["end"], "done");

    assert_eq!((fake.accepted(), fake.seen().len()), (2, 2), "two turns are two requests");
    let requests = captures(fake);
    let (unpicked, screen_marker) = (marker("selection"), marker("screen"));
    let second = String::from_utf8_lossy(&requests[1].body).into_owned();
    let strings = body_strings(&requests[1]);
    assert!(strings.iter().any(|text| text.contains("ok.")), "the first answer is in the second request");
    assert!(strings.iter().any(|text| text.contains("second question")), "the typed text is in the second request");
    assert!(second.contains("Screen of pane 7"), "the label of the earlier note is in the second request");
    assert!(leaks(&everything(fake), &set).is_empty(), "a canary reached the server");
    assert!(!second.contains(&unpicked), "the unpicked marker is in the second request");
    for line in [screen_marker.as_str(), "Compiling holzwave-core", "uname -a"] {
        assert!(!second.contains(line), "a line of the screen text is in the second request");
    }

    // An offer is spent by one send.
    let again = SendRequest::new(chat.clone(), "again".to_owned(), PANE).profile(PROFILE).offer(offer, vec![screen]);
    assert_eq!(context_reason(service.send(again)), "stale_offer");
    std::thread::sleep(QUIET);
    assert_eq!(fake.seen().len(), 2, "a refused send made a request");
}

// ---- a request that names context nobody approved ---------------------------------------------------------------------

/// A provider built the way the service builds it, over `server`.
fn built_provider(server: &FakeHttp) -> impl Provider {
    let vault = KeyVault::new(Arc::new(MemoryStore::default()));
    let mut config = AiConfig::default();
    let id = config.upsert(local_input(server, "test-model")).expect("a profile");
    config.set_key(&vault, &id, KeySlot::Provider, ApiKey::new(api_key())).expect("a key");
    let resolved = config.resolve(&id, &vault, false).expect("the profile resolves");
    resolved.into_provider(Box::new(OpenAiChatAdapter::new()), Arc::new(Http::new(HttpConfig::standard())))
}

/// The approval of `text` for `provider`, made by a gate of its own.
fn approval(provider: &str, text: &str) -> holzwave_core::ai::gate::ApprovedContext {
    let gate = Gate::new();
    let destination = Destination { host: "127.0.0.1".to_owned(), locality: Locality::Loopback };
    let item = RequestItem::new(ContextKind::Screen, "Screen of pane 7", SourceText::plain(text));
    let offer = gate.offer(OfferRequest::new(provider, destination, vec![item])).expect("an offer");
    gate.approve(offer.id(), &[1], provider).expect("an approval")
}

fn request_of(notes: Vec<Part>, context: Vec<holzwave_core::ai::gate::ApprovedContext>) -> ChatRequest {
    let mut request = ChatRequest::new("test-model");
    let mut parts = notes;
    parts.push(Part::Text { text: "explain".to_owned() });
    request.messages = vec![Message::new("m1", Role::User, parts, 1)];
    request.context = context;
    request
}

fn stream(provider: &impl Provider, request: &ChatRequest) -> Result<(), AiError> {
    provider.stream(request, &mut |_| Flow::Continue, &CancelToken::new()).map(|_| ())
}

#[test]
fn a_request_that_names_context_nobody_approved_is_refused_before_any_connection() {
    let fake = FakeHttp::start(vec![chat_reply("ok.")]);
    let provider = built_provider(&fake);
    let approved_text = marker("approved");
    let note_text = marker("note");
    let policy = |result: Result<(), AiError>, what: &str| {
        assert!(matches!(result, Err(AiError::Policy(_))), "{what} was not refused as a policy error");
        assert_eq!(fake.accepted(), 0, "{what} opened a connection");
    };

    // (a) An approval that no message names.
    let local = approval("local", &approved_text);
    policy(stream(&provider, &request_of(Vec::new(), vec![local.clone()])), "an approval no message names");
    // (b) A note that matches an approval made for another provider.
    let other = approval("other", &approved_text);
    let note = Part::Context(ContextNote::for_approved(&other, Some(note_text.clone())));
    policy(stream(&provider, &request_of(vec![note], vec![other])), "an approval made for another provider");
    // (c) An approval for this provider whose note names another hash.
    let mut foreign = ContextNote::for_approved(&local, Some(note_text.clone()));
    foreign.hash = "ab".repeat(32);
    policy(stream(&provider, &request_of(vec![Part::Context(foreign)], vec![local.clone()])), "a note with another hash");
    let nothing: Vec<u8> = everything(&fake);
    assert!(nothing.is_empty() && !String::from_utf8_lossy(&nothing).contains(&note_text));

    // The control: a note and an approval that match stream, and the request holds the approved text and not the
    // stored text of the note.
    let note = Part::Context(ContextNote::for_approved(&local, Some(note_text.clone())));
    stream(&provider, &request_of(vec![note], vec![local.clone()])).expect("a matching note and approval stream");
    assert_eq!((fake.accepted(), fake.seen().len()), (1, 1));
    let sent = String::from_utf8_lossy(&everything(&fake)).into_owned();
    assert!(sent.contains(&approved_text), "the approved text is in the request");
    assert!(!sent.contains(&note_text), "the stored text of the note reached the server");

    // A note whose hash no approval of the request has is history: one marker line stands for it and the stored text
    // never travels.
    let mut history = ContextNote::for_approved(&local, Some(note_text.clone()));
    history.hash = "cd".repeat(32);
    stream(&provider, &request_of(vec![Part::Context(history)], Vec::new())).expect("a history note streams");
    let sent = String::from_utf8_lossy(&everything(&fake)).into_owned();
    assert_eq!(fake.seen().len(), 2);
    assert!(!sent.contains(&note_text), "the stored text of a history note reached the server");
}

// ---- a share ends with every change of route and every close ------------------------------------------------------------

#[test]
fn a_pane_share_ends_on_every_change_of_route_and_on_every_close() {
    type Step = fn(&Rig, &holzwave_core::ai::store::ConversationId, &FakeHttp);
    let scenarios: Vec<(&str, Step, Step)> = vec![
        (
            "another profile",
            |rig, chat, _| {
                rig.service().context_state(chat, PANE, Some("other"), None).expect("a state");
            },
            |_, _, _| {},
        ),
        (
            "another model",
            |rig, chat, _| {
                rig.service().context_state(chat, PANE, Some(PROFILE), Some("other-model")).expect("a state");
            },
            |_, _, _| {},
        ),
        (
            "another address",
            |rig, chat, second| {
                let mut moved = local_input(second, "test-model");
                moved.id = Some(PROFILE.to_owned());
                rig.service().save_profile(moved).expect("a moved profile");
                rig.service().context_state(chat, PANE, Some(PROFILE), None).expect("a state");
            },
            |rig, _, _| {
                let mut back = local_input(rig.fake(), "test-model");
                back.id = Some(PROFILE.to_owned());
                rig.service().save_profile(back).expect("the profile is back");
            },
        ),
        ("chat closed", |rig, chat, _| rig.service().chat_closed(chat), |_, _, _| {}),
        (
            "chat deleted",
            |rig, chat, _| {
                rig.service().delete_conversation(chat).expect("a deletion");
            },
            |_, _, _| {},
        ),
        (
            "profile removed",
            |rig, _, _| {
                rig.service().remove_profile(PROFILE).expect("a removal");
            },
            |rig, _, _| {
                let id = rig.service().save_profile(local_input(rig.fake(), "test-model")).expect("a profile");
                rig.service().set_key(&id, KeySlot::Provider, ApiKey::new(api_key())).expect("a key");
            },
        ),
        (
            "pane closed",
            |rig, _, _| {
                rig.service().pane_closed(PANE);
            },
            |_, _, _| {},
        ),
    ];
    for (name, away, back) in scenarios {
        let rig = Rig::new(RigOptions { source: pane_entries(), ..RigOptions::default() });
        let second = FakeHttp::start(vec![chat_reply("ok.")]);
        let service = rig.service();
        let mut other = local_input(rig.fake(), "test-model");
        other.name = "Other".to_owned();
        let other = service.save_profile(other).expect("a second profile");
        service.set_key(&other, KeySlot::Provider, ApiKey::new(api_key())).expect("a key");
        let chat = rig.chat();
        service.context_share(&chat, PANE, Some(PROFILE), None, true).expect("a share");
        assert!(service.is_shared(&chat, PANE), "{name}: the share did not start");
        let view = service.context_preview(&chat, PANE, Some(PROFILE), None, &[ContextKind::Screen]).expect("a preview");
        let offer = OfferId::parse(&view.offer).expect("an offer id");

        away(&rig, &chat, &second);
        back(&rig, &chat, &second);
        assert!(!service.is_shared(&chat, PANE), "{name}: the share survived");
        let preview = service.context_preview(&chat, PANE, Some(PROFILE), None, &[ContextKind::Screen]);
        assert_eq!(context_reason(preview), "not_shared", "{name}: the preview of an unshared pane");
        let send = SendRequest::new(chat.clone(), "hi".to_owned(), PANE).profile(PROFILE).offer(offer, vec![1]);
        let refused = context_reason(service.send(send));
        assert!(refused == "stale_offer" || refused == "not_shared", "{name}: the send with the old offer: {refused}");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!((rig.fake().seen().len(), second.accepted()), (0, 0), "{name}: a request was made");

        // The control: sharing again restores the preview.
        service.context_share(&chat, PANE, Some(PROFILE), None, true).expect("a share");
        let view = service.context_preview(&chat, PANE, Some(PROFILE), None, &[ContextKind::Screen]);
        assert!(view.is_ok_and(|view| !view.items.is_empty()), "{name}: sharing again did not restore the preview");
    }
}

// ---- tool results ---------------------------------------------------------------------------------------------------

#[test]
fn a_tool_result_with_secrets_is_cleaned_before_it_goes_back_to_the_model() {
    let set = canaries();
    let scratch = security::world::Scratch::new("egress-tree");
    let tree = Tree::new(scratch.path());
    let host = Arc::new(GateHost::new(&tree, &[PANE_A]));
    let leaky = tree.path("proj/leaky.txt");
    let rig = Rig::new(RigOptions {
        replies: vec![tool_call_reply("call_1", "read_file", &json!({ "path": leaky })), chat_reply("done")],
        tool_host: Some(host.clone()),
        deny: Some(DenyPolicy::new(tree.protected())),
        ..RigOptions::default()
    });
    let (service, fake) = (rig.service(), rig.fake());
    let chat = rig.chat();
    service.send(SendRequest::new(chat.clone(), "read the file".to_owned(), PANE_A).profile(PROFILE)).expect("send");

    // The test plays the page: it waits for the card and decides with the hash on the card.
    let proposed = || rig.sink().events(chat.as_str()).into_iter().find(|event| event["kind"] == "tool_proposed");
    assert!(common::wait_until(Duration::from_secs(10), || proposed().is_some()), "no card was proposed");
    let card = proposed().expect("a card");
    let (call_id, hash) = (card["pending"]["call_id"].as_str().unwrap_or(""), card["pending"]["hash"].as_str().unwrap_or(""));
    service.approvals().decide(chat.as_str(), call_id, Decision::Allow, hash).expect("the decision is taken");
    let ended = rig.sink().wait_ended(chat.as_str(), Duration::from_secs(10)).expect("the turn ends");
    assert_eq!(ended["end"], "done");

    let requests = captures(fake);
    assert_eq!(requests.len(), 2, "two rounds are two requests");
    let first: Value = serde_json::from_slice(&requests[0].body).expect("a JSON body");
    let declared = first["tools"].as_array().cloned().unwrap_or_default();
    assert!(declared.iter().any(|tool| tool["function"]["name"] == "read_file"), "request 1 does not declare read_file");
    let second = String::from_utf8_lossy(&requests[1].body).into_owned();
    let found = leaks(&requests[1].body, &set);
    assert!(found.is_empty(), "a canary is in the tool result: {found:?}");
    assert!(!second.contains(&file_value()), "the file value is in the tool result");
    let strings = body_strings(&requests[1]);
    for rule in SECRET_RULES {
        assert!(
            strings.iter().any(|text| text.contains(&format!("[REDACTED:{rule}]"))),
            "no typed placeholder for {rule} in the tool result"
        );
    }
    let result = rig.sink().events(chat.as_str()).into_iter().find(|event| event["kind"] == "tool_result").expect("a result");
    assert!(result["redactions"].as_u64().unwrap_or(0) > 0, "the result event reports no redaction");
    assert_eq!(host.run_count(), 1, "the host ran the one approved call");

    let (_, chats, _) = rig.dirs();
    let stored: Vec<u8> = files_below(&chats).concat();
    let stored_text = String::from_utf8_lossy(&stored).into_owned();
    assert!(stored_text.contains(NOT_STORED), "the stored result is not the fixed text");
    assert!(leaks(&stored, &set).is_empty() && !stored_text.contains(&file_value()), "a secret is in the stored chat");
}

// ---- wsh ai asks --------------------------------------------------------------------------------------------------------

/// An ask running on its own thread.
struct AskRun {
    handle: JoinHandle<(Result<(), AskFault>, AskOutput)>,
    cancel: CancelToken,
}

impl AskRun {
    fn finished(&self) -> bool {
        self.handle.is_finished()
    }

    fn end(self) -> (Result<(), AskFault>, AskOutput) {
        assert!(common::wait_until(Duration::from_secs(10), || self.handle.is_finished()), "the ask did not end");
        self.handle.join().expect("the ask thread did not panic")
    }
}

fn start_ask(rig: &Rig, pane: holzwave_core::layout::PaneId, prompt: &str, input: &str) -> AskRun {
    let (asks, service) = (rig.asks().clone(), rig.service().clone());
    let cancel = CancelToken::new();
    let token = cancel.clone();
    let (prompt, input) = (prompt.to_owned(), input.to_owned());
    let handle = thread::Builder::new()
        .name("gate-ask".to_owned())
        .spawn(move || {
            let mut out = AskOutput::default();
            let result = asks.run(&service, AskInput { pane, prompt, input }, &mut out, &token);
            (result, out)
        })
        .expect("a thread");
    AskRun { handle, cancel }
}

fn card_of(rig: &Rig) -> (String, String) {
    let card = rig.notes().wait_waiting(Duration::from_secs(10)).expect("a card waits");
    (card["view"]["ask"].as_str().unwrap_or("").to_owned(), card["view"]["seal"].as_str().unwrap_or("").to_owned())
}

fn reason_of(result: Result<(), AskFault>) -> &'static str {
    result.expect_err("the ask ends with a fault").reason()
}

#[test]
fn a_wsh_ask_sends_nothing_until_the_person_answers_and_declares_no_tool() {
    let set = canaries();
    let piped = terminal_text(&set);
    let rig = Rig::scripted(Locality::Cloud, vec![Script::reply("The build fails.").usage(1, 1)]);
    let run = start_ask(&rig, 3, "why does this fail", &piped);
    let card = rig.notes().wait_waiting(Duration::from_secs(10)).expect("a card waits");
    assert_eq!(card["view"]["locality"], "cloud");
    let shown = card.to_string();
    assert!(leaks(shown.as_bytes(), &set).is_empty(), "a canary is on the card");
    assert!(!shown.contains(&marker("screen")), "piped text is on the card");
    let findings: BTreeSet<String> = card["view"]["input"]["findings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|finding| finding["rule"].as_str().map(str::to_owned))
        .collect();
    for rule in SECRET_RULES {
        assert!(findings.contains(rule), "the card does not name {rule}");
    }
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(rig.provider().calls(), 0, "the provider saw a request before the answer");
    assert!(!run.finished(), "the ask returned before the answer");

    let (ask, seal) = (card["view"]["ask"].as_str().unwrap_or(""), card["view"]["seal"].as_str().unwrap_or(""));
    rig.asks().settle(ask, AskVerdict::Send, seal).expect("the answer is taken");
    let (result, out) = run.end();
    result.expect("the ask ends ok");
    assert_eq!(out.texts.concat(), "The build fails.");
    let requests = rig.provider().requests();
    assert_eq!(requests.len(), 1, "one ask is one request");
    assert!(requests[0].tools.is_empty(), "an ask declared a tool");
    assert_eq!(requests[0].context.len(), 1);
    let envelope = requests[0].context[0].envelope();
    assert!(leaks(envelope.as_bytes(), &set).is_empty(), "a canary is in the envelope");
    for rule in SECRET_RULES {
        assert!(envelope.contains(&format!("[REDACTED:{rule}]")), "no typed placeholder for {rule}");
    }
    let last = requests[0].messages.iter().rev().find(|message| message.role == Role::User).expect("a user message");
    assert_eq!(last.text(), "why does this fail", "the last user message is the cleaned prompt");
    assert!(common::wait_until(Duration::from_secs(10), || {
        rig.service().list_conversations().is_ok_and(|listing| listing.chats.is_empty())
    }));
}

#[test]
fn a_refused_or_unanswered_ask_costs_the_provider_nothing() {
    let rig = Rig::scripted(Locality::Cloud, vec![Script::reply("fine").usage(1, 1)]);
    let asks = rig.asks();

    // A refusal: refused, no request, and the same pane rests.
    let first = start_ask(&rig, 3, "explain this", "some text\n");
    let (ask, seal) = card_of(&rig);
    asks.settle(&ask, AskVerdict::Refuse, &seal).expect("the refusal is taken");
    assert_eq!(reason_of(first.end().0), "refused");
    assert_eq!(rig.provider().calls(), 0, "a refusal sent a request");
    let again = start_ask(&rig, 3, "explain this", "some text\n");
    assert_eq!(reason_of(again.end().0), "cooling_down");
    assert_eq!(rig.notes().waiting().len(), 1, "a second card was shown");

    // A caller that leaves while the ask waits: gone, no request, and a late answer is unknown.
    let leaving = start_ask(&rig, 5, "explain that", "other text\n");
    assert!(common::wait_until(Duration::from_secs(10), || rig.notes().waiting().len() == 2));
    let late = rig.notes().waiting()[1]["view"]["ask"].as_str().unwrap_or("").to_owned();
    let late_seal = rig.notes().waiting()[1]["view"]["seal"].as_str().unwrap_or("").to_owned();
    leaving.cancel.cancel();
    assert_eq!(reason_of(leaving.end().0), "gone");
    assert_eq!(rig.provider().calls(), 0, "a withdrawn ask sent a request");
    let unknown = asks.settle(&late, AskVerdict::Send, &late_seal).expect_err("a late answer is refused");
    assert_eq!(unknown.reason(), "unknown_ask");

    // A wrong seal is refused and the ask keeps waiting; the right seal sends (the positive control).
    let waiting = start_ask(&rig, 4, "explain more", "more text\n");
    assert!(common::wait_until(Duration::from_secs(10), || rig.notes().waiting().len() == 3));
    let card = rig.notes().waiting()[2].clone();
    let (ask, seal) = (card["view"]["ask"].as_str().unwrap_or("").to_owned(), card["view"]["seal"].as_str().unwrap_or("").to_owned());
    let wrong = asks.settle(&ask, AskVerdict::Send, &"0".repeat(64)).expect_err("a wrong seal is refused");
    assert_eq!(wrong.reason(), "seal_mismatch");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!waiting.finished() && rig.provider().calls() == 0, "a wrong seal released the ask");
    asks.settle(&ask, AskVerdict::Send, &seal).expect("the right seal is taken");
    let (result, out) = waiting.end();
    result.expect("the ask ends ok");
    assert_eq!(out.texts.concat(), "fine");
    assert_eq!(rig.provider().calls(), 1, "only the answered ask sent a request");

    // An unanswered ask ends timed out with no request.
    let limits = AskLimits { wait: Duration::from_millis(300), ..AskLimits::default() };
    let slow = Rig::scripted_with(Locality::Cloud, Vec::new(), RigOptions { asks: Some(limits), ..RigOptions::default() });
    let silent = start_ask(&slow, 3, "explain this", "some text\n");
    assert_eq!(reason_of(silent.end().0), "timed_out");
    assert_eq!(slow.provider().calls(), 0, "an unanswered ask sent a request");
}

#[test]
fn local_only_refuses_an_ask_that_would_leave_the_device_before_any_card() {
    let rig = Rig::scripted(Locality::Cloud, vec![Script::reply("fine").usage(1, 1)]);
    rig.service().apply_settings(&settings(|s| s.ai_local_only = true));
    let refused = start_ask(&rig, 3, "explain this", "some text\n");
    let fault = refused.end().0.expect_err("a cloud ask is refused");
    assert_eq!(fault.reason(), "context");
    assert_eq!(format!("{fault:?}"), "AskFault::Context(local_only)");
    assert!(rig.notes().waiting().is_empty(), "a card was shown for an ask that cannot leave");
    assert_eq!(rig.provider().calls(), 0);
    assert!(rig.service().list_conversations().expect("a listing").chats.is_empty(), "a conversation was made");

    // The control: with the setting off the same ask waits.
    rig.service().apply_settings(&Settings::default());
    let waiting = start_ask(&rig, 4, "explain this", "some text\n");
    assert!(rig.notes().wait_waiting(Duration::from_secs(10)).is_some(), "the ask does not wait with the setting off");
    waiting.cancel.cancel();
    assert_eq!(reason_of(waiting.end().0), "gone");
}

#[test]
fn a_piped_corpus_through_a_loopback_ask_reaches_the_server_cleaned() {
    let set = canaries();
    let piped = terminal_text(&set);

    // By default the ask waits, and the server sees nothing until the answer.
    let rig = Rig::new(RigOptions { asks: Some(AskLimits::default()), ..RigOptions::default() });
    let run = start_ask(&rig, 3, "why does this fail", &piped);
    let (ask, seal) = card_of(&rig);
    std::thread::sleep(QUIET);
    assert_eq!(rig.fake().accepted(), 0, "a connection before the answer");
    rig.asks().settle(&ask, AskVerdict::Send, &seal).expect("the answer is taken");
    run.end().0.expect("the ask ends ok");
    let check = |rig: &Rig, what: &str| {
        assert_eq!((rig.fake().accepted(), rig.fake().seen().len()), (1, 1), "{what}: connections and requests");
        let request = &captures(rig.fake())[0];
        assert!(leaks(&everything(rig.fake()), &set).is_empty(), "{what}: a canary reached the server");
        let body: Value = serde_json::from_slice(&request.body).expect("a JSON body");
        assert!(body["tools"].as_array().is_none_or(Vec::is_empty), "{what}: the ask declared a tool");
        let strings = body_strings(request);
        for rule in SECRET_RULES {
            assert!(
                strings.iter().any(|text| text.contains(&format!("[REDACTED:{rule}]"))),
                "{what}: no typed placeholder for {rule}"
            );
        }
    };
    check(&rig, "after the answer");

    // With `auto` the ask streams at once.
    let auto = Rig::new(RigOptions {
        asks: Some(AskLimits::default()),
        local_context_auto: true,
        ..RigOptions::default()
    });
    let run = start_ask(&auto, 3, "why does this fail", &piped);
    let (result, out) = run.end();
    result.expect("the ask ends ok");
    assert_eq!(out.texts.concat(), "ok.");
    assert!(auto.notes().waiting().is_empty(), "a card was shown for a destination on this computer");
    check(&auto, "with auto");
}

// ---- nothing the assistant writes is synced ---------------------------------------------------------------------------------

#[test]
fn nothing_the_assistant_writes_is_in_the_sync_allow_list() {
    for name in ["ai.json", "ai.json.tmp", "chats", "chats/c1.json", "chats/c1.json.tmp", "chats/c1.json.bad"] {
        assert!(!allowed(name), "{name} is synced");
    }
    for name in ["settings.json", "themes/dark.json"] {
        assert!(allowed(name), "{name} is not synced (the positive control)");
    }

    // After a full session: the profile, the key, a chat.
    let rig = Rig::new(RigOptions { source: pane_entries(), ..RigOptions::default() });
    let chat = rig.chat();
    assert_eq!(say(&rig, &chat, "hello")["end"], "done");
    let (_, chats, config) = rig.dirs();
    assert!(config.join("ai.json").is_file(), "the session left no ai.json");
    assert!(!files_below(&chats).is_empty(), "the session left no chat file");
    // The same files in the folder the sync reads, as a hostile layout would have them.
    fs::create_dir(config.join("chats")).expect("a chats folder");
    fs::write(config.join("chats").join("c1.json"), "{}").expect("a chat file");
    fs::write(config.join("chats").join("c1.json.tmp"), "{}").expect("a temporary file");
    fs::write(config.join("settings.json"), "{}").expect("the settings");

    let store: Arc<dyn SecretStore> = rig.vault().clone();
    let source = ConfigSource::new(&config, store);
    let names = source.names().expect("the names");
    assert!(names.contains(&"settings.json".to_owned()), "the positive control is not listed: {names:?}");
    for name in &names {
        assert!(name != "ai.json" && !name.starts_with("chats"), "{name} is listed by the sync");
    }
    match source.read("secrets.json").expect("a read") {
        None => {}
        Some(bytes) => assert!(!String::from_utf8_lossy(&bytes).contains("ai:"), "an ai key is in the synced secrets"),
    }
    for name in ["ai.json", "chats/c1.json"] {
        assert!(source.write(name, b"{}").is_err(), "{name} can be written by the sync");
    }
    let hostile = br#"{"ai:local":"x"}"#;
    assert!(source.write("secrets.json", hostile).is_err(), "an ai account can be written by the sync");
    // The key is still in the vault and nothing the sync did changed it.
    assert!(rig.vault().get("ai:local").expect("a read").is_some_and(|record| record.contains(&api_key())));
}
