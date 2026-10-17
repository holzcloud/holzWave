//! The context gate joined to panes and files, proved headless against an in-process server and a source double, with
//! no outside network.
//!
//! A pane that a context source offers is shared for a chat, previewed into an offer, and sent with the offer id and
//! the picked item numbers only. What is proved: the server receives exactly the envelope the gate built for the
//! approved item, once, before the typed text; a key written on the screen and a span the engine drew hidden are
//! nowhere in the request, the page view, the events or the stored chat; consent is per pane and per route and ends
//! when the route, the chat, the pane or the profile ends; an offer can be spent only by its own chat, pane and
//! route, once; nothing reads a pane or looks up a name except the three explicit calls. Every wait is bounded.

mod common;

use std::collections::HashMap;
use std::fs;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use common::fake_http::{FakeHttp, Reply};
use holzwave_core::ai::gate::{
    ApprovedContext, Destination, Gate, MAX_SCOPE_BYTES, OfferId, OfferRequest, RequestItem,
};
use holzwave_core::ai::profiles::{AiConfig, KeySlot, KeyVault, Preset, ProfileInput};
use holzwave_core::ai::provider::AiError;
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::service::context::{ContextRefusal, ContextSource, Fetched, ReadRefusal};
use holzwave_core::ai::service::events::{AiEvent, AiEventKind, ChatPart, EndView};
use holzwave_core::ai::service::{AiService, AiSink, OfferView, SendRequest, ServiceError};
use holzwave_core::ai::store::{ConversationId, JsonDirStore};
use holzwave_core::ai::types::{ApiKey, ContextKind, Locality};
use holzwave_core::ai::wire::Wire;
use holzwave_core::layout::PaneId;
use holzwave_core::secrets::MemoryStore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The key of the profile. It appears in test code only: a hit anywhere else is a leak.
const KEY_CANARY: &str = "zq-ctxkey-8a40d6e2b1";
/// The text a user types.
const TEXT_CANARY: &str = "zq-share-text-6d2b94e1a7";
/// A key written on the screen.
const SCREEN_KEY: &str = "sk-ant-api03-zqshare7c41e90b3aXYZ0123456789";
/// Text the engine drew hidden.
const HIDDEN_CANARY: &str = "zq-hidden-3f9a1c";
/// The pane the tests share.
const PANE: PaneId = 7;

// ---- the rig ------------------------------------------------------------------------------------------------------

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Keeps every event in the order it arrived.
#[derive(Default)]
struct CollectSink {
    events: Mutex<Vec<AiEvent>>,
}

impl AiSink for CollectSink {
    fn emit(&self, event: &AiEvent) {
        lock(&self.events).push(event.clone());
    }
}

impl CollectSink {
    fn all(&self) -> Vec<AiEvent> {
        lock(&self.events).clone()
    }

    fn of(&self, chat: &ConversationId) -> Vec<AiEvent> {
        self.all().into_iter().filter(|event| event.conversation == chat.as_str()).collect()
    }

    fn ended(&self, chat: &ConversationId) -> usize {
        self.of(chat).iter().filter(|event| matches!(event.kind, AiEventKind::TurnEnded(_))).count()
    }

    fn wait_ended(&self, chat: &ConversationId, count: usize) -> bool {
        common::wait_until(Duration::from_secs(5), || self.ended(chat) >= count)
    }

    fn last_end(&self, chat: &ConversationId) -> EndView {
        self.of(chat)
            .into_iter()
            .rev()
            .find_map(|event| match event.kind {
                AiEventKind::TurnEnded(end) => Some(end),
                _ => None,
            })
            .expect("the chat has an ended turn")
    }
}

/// One item a pane gives, as the source double holds it.
#[derive(Clone)]
struct Item {
    label: String,
    text: String,
    hidden: Vec<Range<usize>>,
    cut: bool,
}

/// A context source over a table, counting its calls and able to answer a chosen refusal.
#[derive(Default)]
struct FakeSource {
    panes: Mutex<HashMap<PaneId, (Vec<ContextKind>, HashMap<ContextKind, Option<Item>>)>>,
    every_pane: Mutex<Option<Vec<ContextKind>>>,
    refusal: Mutex<Option<ReadRefusal>>,
    kinds_calls: AtomicUsize,
    reads: AtomicUsize,
}

impl FakeSource {
    fn offer(&self, pane: PaneId, kinds: &[ContextKind]) {
        lock(&self.panes).entry(pane).or_default().0 = kinds.to_vec();
    }

    fn give(&self, pane: PaneId, kind: ContextKind, item: Option<Item>) {
        lock(&self.panes).entry(pane).or_default().1.insert(kind, item);
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

impl ContextSource for FakeSource {
    fn kinds(&self, pane: PaneId) -> Vec<ContextKind> {
        self.kinds_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(kinds) = lock(&self.every_pane).clone() {
            return kinds;
        }
        lock(&self.panes).get(&pane).map(|(kinds, _)| kinds.clone()).unwrap_or_default()
    }

    fn read(&self, pane: PaneId, kind: ContextKind, _max_bytes: usize) -> Result<Option<Fetched>, ReadRefusal> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if let Some(refusal) = *lock(&self.refusal) {
            return Err(refusal);
        }
        let panes = lock(&self.panes);
        let item = panes.get(&pane).and_then(|(_, items)| items.get(&kind)).cloned().flatten();
        Ok(item.map(|item| Fetched::new(item.label, SourceText::with_hidden(item.text, item.hidden), item.cut)))
    }
}

/// The screen of pane 7: a command, a key on the screen, a span the engine drew hidden, and a prompt.
fn screen_text() -> String {
    format!("$ env\nANTHROPIC_API_KEY={SCREEN_KEY}\nnote: {HIDDEN_CANARY} end\n$ ")
}

fn screen_item() -> Item {
    let text = screen_text();
    let start = text.find(HIDDEN_CANARY).expect("the hidden text is on the screen");
    Item { label: "Screen of pane 7".to_owned(), text, hidden: vec![start..start + HIDDEN_CANARY.len()], cut: false }
}

/// A test rig: a scratch directory, a keychain, a sink, a chat store on disk and a source double.
struct Rig {
    dir: PathBuf,
    config_dir: PathBuf,
    secrets: Arc<MemoryStore>,
    sink: Arc<CollectSink>,
    store: Arc<JsonDirStore>,
    source: Arc<FakeSource>,
}

impl Rig {
    fn new(tag: &str) -> Rig {
        let dir = common::scratch_dir(tag);
        let config_dir = dir.join("config");
        fs::create_dir(&config_dir).expect("a config directory");
        let source = Arc::new(FakeSource::default());
        source.offer(PANE, &[ContextKind::Selection, ContextKind::Screen]);
        source.give(PANE, ContextKind::Selection, None);
        source.give(PANE, ContextKind::Screen, Some(screen_item()));
        Rig {
            store: Arc::new(JsonDirStore::new(dir.join("chats"))),
            dir,
            config_dir,
            secrets: Arc::new(MemoryStore::default()),
            sink: Arc::new(CollectSink::default()),
            source,
        }
    }

    fn vault(&self) -> KeyVault {
        KeyVault::new(self.secrets.clone())
    }

    fn chats_dir(&self) -> PathBuf {
        self.dir.join("chats")
    }

    /// Writes `ai.json` with the given profiles: (name, base address, model, key, plain http to the LAN allowed).
    fn profiles(&self, profiles: &[(&str, &str, &str, Option<&str>, bool)]) {
        let mut config = AiConfig::default();
        let vault = self.vault();
        for (name, base, model, key, lan) in profiles {
            let mut input = ProfileInput::new(*name, Preset::Custom);
            input.wire = Some(Wire::OpenAiChat);
            input.base_url = (*base).to_owned();
            input.model = (*model).to_owned();
            input.allow_lan_http = *lan;
            let id = config.upsert(input).expect("the profile is accepted");
            if let Some(key) = key {
                let key = ApiKey::new((*key).to_owned());
                config.set_key(&vault, &id, KeySlot::Provider, key).expect("the key is saved");
            }
        }
        config.save(&self.config_dir).expect("ai.json is written");
    }

    /// The profile `local` on the fake server.
    fn local(&self, fake: &FakeHttp) {
        self.profiles(&[("Local", &format!("{}/v1", fake.base()), "test-model", Some(KEY_CANARY), false)]);
    }

    fn builder(&self) -> holzwave_core::ai::service::ServiceBuilder {
        AiService::builder(self.store.clone(), self.sink.clone() as Arc<dyn AiSink>, &self.config_dir)
            .vault(self.vault())
            .context_source(self.source.clone() as Arc<dyn ContextSource>)
    }

    fn service(&self) -> AiService {
        self.builder().build()
    }

    /// Every file of the chat directory as text.
    fn stored_text(&self) -> String {
        let mut all = String::new();
        if let Ok(entries) = fs::read_dir(self.chats_dir()) {
            for entry in entries.flatten() {
                all.push_str(&fs::read_to_string(entry.path()).unwrap_or_default());
            }
        }
        all
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// One event of a Chat Completions stream.
fn chunk(delta: &Value, finish: Option<&str>) -> String {
    let body = json!({
        "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "test-model",
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }]
    });
    format!("data: {body}\n\n")
}

/// A Chat Completions answer: a role chunk, the content chunks `ok` and `.`, a finish chunk, a usage chunk, the marker.
fn answer() -> Reply {
    let usage = json!({
        "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "test-model", "choices": [],
        "usage": { "prompt_tokens": 11, "completion_tokens": 2, "total_tokens": 13 }
    });
    let chunks = vec![
        chunk(&json!({ "role": "assistant", "content": "" }), None),
        chunk(&json!({ "content": "ok" }), None),
        chunk(&json!({ "content": "." }), None),
        chunk(&json!({}), Some("stop")),
        format!("data: {usage}\n\n"),
        "data: [DONE]\n\n".to_owned(),
    ];
    Reply::sse_bytes(chunks.into_iter().map(String::into_bytes).collect())
}

fn fake_with(replies: usize) -> FakeHttp {
    FakeHttp::start((0..replies).map(|_| answer()).collect())
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The envelope a throwaway gate builds from the same cleaned item, and the size of that item: what the server must
/// receive.
fn expected_screen_envelope() -> (String, u64) {
    let item = screen_item();
    let gate = Gate::new();
    let destination = Destination { host: "127.0.0.1".to_owned(), locality: Locality::Loopback };
    let text = SourceText::with_hidden(item.text.clone(), item.hidden.clone());
    let request = RequestItem::new(ContextKind::Screen, &item.label, text);
    let offer = gate.offer(OfferRequest::new("local", destination, vec![request])).expect("the throwaway offer");
    (offer.envelope(&[1]).expect("the throwaway envelope"), offer.items()[0].bytes())
}

/// A chat id that is valid.
fn new_chat(service: &AiService) -> ConversationId {
    service.new_conversation().expect("a chat id")
}

fn context_error<T: std::fmt::Debug>(result: Result<T, ServiceError>) -> ContextRefusal {
    match result {
        Err(ServiceError::Context(refusal)) => refusal,
        other => panic!("expected a context refusal, got {other:?}"),
    }
}

/// Every key name that occurs anywhere in `value`.
fn json_keys(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                into.push(key.clone());
                json_keys(inner, into);
            }
        }
        Value::Array(items) => items.iter().for_each(|inner| json_keys(inner, into)),
        _ => {}
    }
}

fn view_of(service: &AiService, chat: &ConversationId, kinds: &[ContextKind]) -> OfferView {
    service.context_preview(chat, PANE, Some("local"), None, kinds).expect("a preview")
}

fn offer_id(view: &OfferView) -> OfferId {
    OfferId::parse(&view.offer).expect("the offer id has the form of one")
}

fn send_with_offer(
    service: &AiService,
    chat: &ConversationId,
    pane: PaneId,
    view: &OfferView,
    picked: &[u32],
) -> Result<(), ServiceError> {
    let request = SendRequest::new(chat.clone(), format!("explain {TEXT_CANARY}"), pane)
        .profile("local")
        .offer(offer_id(view), picked.to_vec());
    service.send(request).map(|_| ())
}

// ---- the tracer ---------------------------------------------------------------------------------------------------

#[test]
fn a_shared_pane_is_previewed_and_sent_and_the_server_receives_exactly_the_approved_bytes() {
    let fake = fake_with(1);
    let rig = Rig::new("ctx-tracer");
    rig.local(&fake);
    let service = rig.service();
    let chat = new_chat(&service);

    // Asking what a pane offers reads nothing and opens nothing.
    let state = service.context_state(&chat, PANE, Some("local"), None).expect("a state");
    assert!(state.required && !state.shared && !state.persist, "{state:?}");
    assert_eq!(state.locality, Locality::Loopback);
    assert!(state.host.starts_with("127.0.0.1"), "{}", state.host);
    assert_eq!(state.kinds, [ContextKind::Selection, ContextKind::Screen]);
    assert_eq!((state.profile.as_str(), state.model.as_str()), ("local", "test-model"));
    assert_eq!((fake.accepted(), rig.source.reads()), (0, 0));

    // A pane that is not shared is not read.
    let refused = service.context_preview(&chat, PANE, Some("local"), None, &[ContextKind::Screen]);
    assert_eq!(context_error(refused), ContextRefusal::NotShared);
    assert_eq!(rig.source.reads(), 0);

    // Share, preview: one screen item; the selection was empty.
    let state = service.context_share(&chat, PANE, Some("local"), None, true).expect("a share");
    assert!(state.shared);
    let view = view_of(&service, &chat, &[ContextKind::Selection, ContextKind::Screen]);
    let (envelope, cleaned_bytes) = expected_screen_envelope();
    assert_eq!(view.items.len(), 1, "{view:?}");
    let item = &view.items[0];
    assert_eq!((item.id, item.kind, item.label.as_str()), (1, ContextKind::Screen, "Screen of pane 7"));
    assert_eq!(view.empty, [ContextKind::Selection]);
    assert_eq!(view.ttl_seconds, 300);
    let findings: Vec<(&str, u32)> =
        item.findings.iter().map(|finding| (finding.rule.as_str(), finding.count)).collect();
    assert_eq!(findings, [("hidden_text", HIDDEN_CANARY.chars().count() as u32), ("anthropic_key", 1)]);
    assert_eq!(item.bytes, cleaned_bytes);
    assert_eq!(view.bytes, cleaned_bytes);
    let route = (view.profile.as_str(), view.model.as_str(), view.locality);
    assert_eq!(route, ("local", "test-model", Locality::Loopback));
    assert!(OfferId::parse(&view.offer).is_some());

    // The view carries no text of any kind.
    let shown = serde_json::to_string(&view).expect("the view serialises");
    for canary in [KEY_CANARY, TEXT_CANARY, SCREEN_KEY, HIDDEN_CANARY, "$ env", "ANTHROPIC_API_KEY"] {
        assert!(!shown.contains(canary), "{canary} is in the view: {shown}");
    }
    let mut keys = Vec::new();
    json_keys(&serde_json::to_value(&view).expect("a value"), &mut keys);
    for name in ["text", "envelope", "excerpt", "content"] {
        assert!(!keys.iter().any(|key| key == name), "the view has a member named {name}");
    }

    // The send names the offer and the item number, and the typed text.
    send_with_offer(&service, &chat, PANE, &view, &[1]).expect("the send is accepted");
    assert!(rig.sink.wait_ended(&chat, 1), "the turn ends");
    let end = rig.sink.last_end(&chat);
    assert_eq!((end.end.as_str(), end.saved), ("done", true), "{end:?}");

    // The server received one request, and the envelope is in it once, before the typed text, byte for byte.
    assert_eq!(fake.accepted(), 1);
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    let body = seen[0].body_text();
    let escaped = serde_json::to_string(&envelope).expect("the envelope escapes");
    let escaped = &escaped[1..escaped.len() - 1];
    assert_eq!(body.matches(escaped).count(), 1, "the envelope occurs exactly once");
    let at_envelope = body.find(escaped).expect("the envelope is in the request");
    assert_eq!(body.matches(TEXT_CANARY).count(), 1);
    assert!(body.find(TEXT_CANARY).expect("the typed text is in the request") > at_envelope, "the text is last");
    for canary in [SCREEN_KEY, HIDDEN_CANARY, KEY_CANARY] {
        assert!(!body.contains(canary), "{canary} is in the request");
    }

    // The event holds a context note with labels and sizes, and the hash of the envelope.
    let started = rig.sink.of(&chat).into_iter().find_map(|event| match event.kind {
        AiEventKind::TurnStarted { message, .. } => Some(message),
        _ => None,
    });
    let started = started.expect("a turn_started event");
    let note = started.parts.iter().find_map(|part| match part {
        ChatPart::Context { hash, items, provider, locality } => {
            Some((hash.clone(), items.clone(), provider.clone(), *locality))
        }
        _ => None,
    });
    let (hash, items, provider, locality) = note.expect("the message holds a context part");
    assert_eq!((provider.as_str(), locality), ("local", Locality::Loopback));
    assert_eq!(hash, sha256_hex(&envelope));
    assert_eq!(items.len(), 1);
    let noted = (items[0].kind, items[0].label.as_str(), items[0].bytes);
    assert_eq!(noted, (ContextKind::Screen, "Screen of pane 7", cleaned_bytes));
    let events = serde_json::to_string(&rig.sink.all()).expect("the events serialise");
    for canary in [SCREEN_KEY, HIDDEN_CANARY, KEY_CANARY, "ANTHROPIC_API_KEY"] {
        assert!(!events.contains(canary), "{canary} is in an event");
    }

    // The chat file holds the hash and the label and no text.
    let stored = rig.stored_text();
    assert!(stored.contains(&hash) && stored.contains("Screen of pane 7"), "the marker is stored");
    for canary in [SCREEN_KEY, HIDDEN_CANARY, KEY_CANARY, "$ env", "ANTHROPIC_API_KEY"] {
        assert!(!stored.contains(canary), "{canary} is in the stored chat");
    }

    // An offer is spent by one send.
    let again = send_with_offer(&service, &chat, PANE, &view, &[1]);
    assert_eq!(context_error(again), ContextRefusal::StaleOffer);
    assert_eq!(fake.accepted(), 1);
    assert_eq!(fake.seen().len(), 1);
    assert_eq!(service.gate().pending(), 0);
}

#[test]
fn a_pane_that_is_not_shared_can_be_neither_previewed_nor_sent() {
    let fake = fake_with(0);
    let rig = Rig::new("ctx-unshared");
    rig.local(&fake);
    rig.source.offer(9, &[]);
    let service = rig.service();
    let chat = new_chat(&service);

    let refused = service.context_preview(&chat, PANE, Some("local"), None, &[ContextKind::Screen]);
    assert_eq!(context_error(refused), ContextRefusal::NotShared);
    assert_eq!(rig.source.reads(), 0, "an unshared pane is never read");

    // A pane that offers nothing cannot be shared.
    let nothing = service.context_share(&chat, 9, Some("local"), None, true);
    assert_eq!(context_error(nothing), ContextRefusal::NothingToShare);
    assert!(!service.is_shared(&chat, 9));

    // Unsharing clears the record, voids the offer, and a send with that offer is stale.
    service.context_share(&chat, PANE, Some("local"), None, true).expect("a share");
    assert!(service.is_shared(&chat, PANE));
    let view = view_of(&service, &chat, &[ContextKind::Screen]);
    assert_eq!(service.gate().pending(), 1);
    let state = service.context_share(&chat, PANE, Some("local"), None, false).expect("an unshare");
    assert!(!state.shared && !service.is_shared(&chat, PANE));
    assert_eq!(service.gate().pending(), 0, "the offer is voided with the share");
    let stale = send_with_offer(&service, &chat, PANE, &view, &[1]);
    assert_eq!(context_error(stale), ContextRefusal::StaleOffer);
    assert_eq!(fake.accepted(), 0);

    // With no source in the builder no pane offers anything.
    let bare = AiService::builder(rig.store.clone(), rig.sink.clone() as Arc<dyn AiSink>, &rig.config_dir)
        .vault(rig.vault())
        .build();
    let none = bare.context_share(&chat, PANE, Some("local"), None, true);
    assert_eq!(context_error(none), ContextRefusal::NothingToShare);
}

#[test]
fn an_offer_belongs_to_its_chat_pane_and_route_and_is_spent_by_one_send() {
    let fake = fake_with(2);
    let rig = Rig::new("ctx-belonging");
    rig.local(&fake);
    rig.source.offer(8, &[ContextKind::Screen]);
    rig.source.give(8, ContextKind::Screen, Some(screen_item()));
    let service = rig.service();
    let chat = new_chat(&service);
    let other = new_chat(&service);
    service.context_share(&chat, PANE, Some("local"), None, true).expect("a share");
    let view = view_of(&service, &chat, &[ContextKind::Screen]);

    // Another chat, another pane: refused, and the offer is still usable.
    let foreign_chat = send_with_offer(&service, &other, PANE, &view, &[1]);
    assert_eq!(context_error(foreign_chat), ContextRefusal::StaleOffer);
    let foreign_pane = send_with_offer(&service, &chat, 8, &view, &[1]);
    assert_eq!(context_error(foreign_pane), ContextRefusal::StaleOffer);
    assert_eq!(service.gate().pending(), 1, "a refused attempt does not spend the offer");

    // Another model is another route: the share ends and the offer is voided.
    let request = SendRequest::new(chat.clone(), "hi".to_owned(), PANE)
        .profile("local")
        .model("other-model")
        .offer(offer_id(&view), vec![1]);
    assert_eq!(context_error(service.send(request).map(|_| ())), ContextRefusal::StaleOffer);
    assert_eq!(service.gate().pending(), 0);
    assert!(!service.is_shared(&chat, PANE));
    assert_eq!(fake.accepted(), 0);

    // Shared and previewed again: a bad pick list is refused before anything is stored and does not spend the offer.
    service.context_share(&chat, PANE, Some("local"), None, true).expect("a share");
    let view = view_of(&service, &chat, &[ContextKind::Screen]);
    assert_eq!(context_error(send_with_offer(&service, &chat, PANE, &view, &[])), ContextRefusal::BadPick);
    assert_eq!(context_error(send_with_offer(&service, &chat, PANE, &view, &[1; 9])), ContextRefusal::BadPick);
    assert_eq!(context_error(send_with_offer(&service, &chat, PANE, &view, &[2])), ContextRefusal::StaleOffer);
    assert!(service.snapshot(&chat).expect("a snapshot").messages.is_empty(), "nothing was stored");
    assert_eq!(service.gate().pending(), 1);

    // The contexts of the send and the offer count together against the limit of four.
    let gate = Gate::new();
    let mut attached: Vec<ApprovedContext> = Vec::new();
    for n in 0..4 {
        let destination = Destination { host: "127.0.0.1".to_owned(), locality: Locality::Loopback };
        let item = RequestItem::new(ContextKind::Selection, "selection", SourceText::plain(format!("attached {n}")));
        let offer = gate.offer(OfferRequest::new("local", destination, vec![item])).expect("an offer");
        attached.push(gate.approve(offer.id(), &[1], "local").expect("an approval"));
    }
    let request = SendRequest::new(chat.clone(), "hi".to_owned(), PANE)
        .profile("local")
        .context(attached)
        .offer(offer_id(&view), vec![1]);
    assert!(matches!(service.send(request), Err(ServiceError::ContextTooMany)));
    assert_eq!(service.gate().pending(), 1, "the offer was not spent");

    // The correct send succeeds once.
    send_with_offer(&service, &chat, PANE, &view, &[1]).expect("the send is accepted");
    assert!(rig.sink.wait_ended(&chat, 1));
    assert_eq!(rig.sink.last_end(&chat).end, "done");
    assert_eq!(fake.accepted(), 1);
    assert_eq!(context_error(send_with_offer(&service, &chat, PANE, &view, &[1])), ContextRefusal::StaleOffer);
    assert_eq!(service.gate().pending(), 0);
    assert!(service.is_shared(&chat, PANE), "the share stays: a pane is shared once");
}

#[test]
fn a_scoped_offer_is_approved_only_with_its_scope_and_the_plain_call_refuses_it() {
    let gate = Gate::new();
    let destination = || Destination { host: "127.0.0.1".to_owned(), locality: Locality::Loopback };
    let request = |scope: Option<&str>| {
        let item = RequestItem::new(ContextKind::Selection, "selection", SourceText::plain("some text"));
        let plain = OfferRequest::new("p", destination(), vec![item]);
        match scope {
            Some(scope) => plain.scoped(scope),
            None => plain,
        }
    };

    // The right scope spends the offer.
    let offer = gate.offer(request(Some("a"))).expect("an offer");
    assert!(gate.approve_scoped(offer.id(), &[1], "p", "a").is_ok());
    assert_eq!(gate.pending(), 0);

    // Another scope, and the plain call, are the same fixed refusal and leave the offer.
    let offer = gate.offer(request(Some("a"))).expect("an offer");
    let wrong = gate.approve_scoped(offer.id(), &[1], "p", "b");
    let plain = gate.approve(offer.id(), &[1], "p");
    let refusal = |result: Result<ApprovedContext, AiError>| match result {
        Err(AiError::Policy(detail)) => format!("{detail:?}"),
        other => panic!("expected a policy refusal, got {other:?}"),
    };
    assert_eq!(refusal(wrong), refusal(plain));
    assert_eq!(gate.pending(), 1, "a refusal leaves the offer");
    assert!(gate.approve_scoped(offer.id(), &[1], "other", "a").is_err(), "the provider still binds it");
    assert!(gate.approve_scoped(offer.id(), &[1], "p", "a").is_ok());

    // An offer with no scope is spent by the plain call only.
    let offer = gate.offer(request(None)).expect("an offer");
    assert!(matches!(gate.approve_scoped(offer.id(), &[1], "p", "a"), Err(AiError::Policy(_))));
    assert!(matches!(gate.approve_scoped(offer.id(), &[1], "p", ""), Err(AiError::Policy(_))));
    assert_eq!(gate.pending(), 1);
    assert!(gate.approve(offer.id(), &[1], "p").is_ok());

    // A scope that is too long is not an offer.
    let long = "s".repeat(MAX_SCOPE_BYTES + 1);
    assert!(matches!(gate.offer(request(Some(&long))), Err(AiError::InvalidRequest(_))));
    let longest = "s".repeat(MAX_SCOPE_BYTES);
    assert!(gate.offer(request(Some(&longest))).is_ok());

    // The id has one form: 32 lower-case hex digits.
    let valid = "0123456789abcdef0123456789abcdef";
    assert!(OfferId::parse(valid).is_some());
    let bad = [
        String::new(),
        valid[..31].to_owned(),
        format!("{valid}0"),
        valid.to_uppercase(),
        format!("g{}", &valid[1..]),
        format!(" {}", &valid[1..]),
        format!("{} ", &valid[..31]),
        "a".repeat(1024 * 1024),
    ];
    for text in &bad {
        assert!(OfferId::parse(text).is_none(), "{:?} is not an offer id", text.len());
    }
}
