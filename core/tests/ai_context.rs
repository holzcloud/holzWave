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
use std::net::IpAddr;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use common::fake_http::{FakeHttp, Reply};
use holzwave_core::ai::gate::{
    ApprovedContext, Destination, Gate, MAX_SCOPE_BYTES, OfferId, OfferRequest, RequestItem,
};
use holzwave_core::ai::http::{Http, HttpConfig, Resolver};
use holzwave_core::ai::profiles::{AiConfig, KeySlot, KeyVault, Preset, ProfileInput};
use holzwave_core::ai::provider::{AiError, Provider};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::scripted::{Script, ScriptedProvider};
use holzwave_core::ai::service::context::{
    CUT_NOTE, ContextRefusal, ContextSource, Fetched, MAX_PREVIEW_KINDS, MAX_SHARES, ReadRefusal, share_required,
};
use holzwave_core::ai::service::events::{AiEvent, AiEventKind, ChatPart, EndView};
use holzwave_core::ai::service::{AiService, AiSink, OfferView, SendRequest, ServiceError};
use holzwave_core::ai::store::{ConversationId, JsonDirStore};
use holzwave_core::ai::types::{ApiKey, Capabilities, ContextKind, Locality};
use holzwave_core::ai::wire::Wire;
use holzwave_core::layout::PaneId;
use holzwave_core::secrets::MemoryStore;
use holzwave_core::settings::{AiLocalContext, Settings};
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

    fn refuse_with(&self, refusal: Option<ReadRefusal>) {
        *lock(&self.refusal) = refusal;
    }

    fn every_pane_offers(&self, kinds: &[ContextKind]) {
        *lock(&self.every_pane) = Some(kinds.to_vec());
    }

    fn kinds_calls(&self) -> usize {
        self.kinds_calls.load(Ordering::SeqCst)
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
            input.acknowledge_gateway = true;
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

// ---- more of the rig ----------------------------------------------------------------------------------------------

/// Answers one set of addresses for the first `flip_after` lookups and another set afterwards, and counts the lookups.
struct FlipResolver {
    first: Vec<IpAddr>,
    later: Vec<IpAddr>,
    flip_after: usize,
    calls: AtomicUsize,
}

impl FlipResolver {
    fn new(first: &str, later: &str, flip_after: usize) -> Arc<FlipResolver> {
        let parse = |text: &str| vec![text.parse::<IpAddr>().expect("an address")];
        Arc::new(FlipResolver { first: parse(first), later: parse(later), flip_after, calls: AtomicUsize::new(0) })
    }

    fn lookups(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Resolver for FlipResolver {
    fn lookup(&self, _host: &str) -> std::io::Result<Vec<IpAddr>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(if n < self.flip_after { self.first.clone() } else { self.later.clone() })
    }
}

impl Rig {
    /// A service whose chokepoint asks `resolver` for every host name.
    fn service_resolving(&self, resolver: &Arc<FlipResolver>) -> AiService {
        let http = Http::with_resolver(HttpConfig::standard(), resolver.clone() as Arc<dyn Resolver>);
        self.builder().http(Arc::new(http)).build()
    }
}

fn settings(change: impl FnOnce(&mut Settings)) -> Settings {
    let mut settings = Settings::default();
    change(&mut settings);
    settings
}

fn register_device(service: &AiService, id: &str, locality: Locality) {
    let provider = ScriptedProvider::new(id)
        .with_capabilities(Capabilities::conservative(locality))
        .with_turn(Script::reply("ok"));
    service.register_provider(id, Arc::new(provider) as Arc<dyn Provider>).expect("a registered provider");
}

fn screen_of(pane: PaneId) -> Item {
    Item { label: format!("Screen of pane {pane}"), ..screen_item() }
}

/// Shares `pane` for `chat` on `profile` and previews its screen.
fn share_and_preview(service: &AiService, chat: &ConversationId, pane: PaneId, profile: &str) -> OfferView {
    service.context_share(chat, pane, Some(profile), None, true).expect("a share");
    service.context_preview(chat, pane, Some(profile), None, &[ContextKind::Screen]).expect("a preview")
}

// ---- consent ends with the route, the chat, the pane and the profile -----------------------------------------------

#[test]
fn a_change_of_provider_model_or_address_ends_the_share_and_voids_the_offer() {
    let fake = fake_with(1);
    let moved = fake_with(0);
    let rig = Rig::new("ctx-reset");
    let base = format!("{}/v1", fake.base());
    rig.profiles(&[
        ("Local", &base, "test-model", Some(KEY_CANARY), false),
        ("Second", &base, "test-model", Some("zq-second-key-17c3e9"), false),
    ]);
    let service = rig.service();
    let chat = new_chat(&service);

    // Another model: the share is gone, and a later call without a model does not bring it back.
    share_and_preview(&service, &chat, PANE, "local");
    let reads_before = rig.source.reads();
    assert_eq!(service.gate().pending(), 1);
    let other = service.context_state(&chat, PANE, Some("local"), Some("other-model")).expect("a state");
    assert!(!other.shared && other.model == "other-model");
    let again = service.context_state(&chat, PANE, Some("local"), None).expect("a state");
    assert!(!again.shared, "a reset, not a shadow");
    assert_eq!(service.gate().pending(), 0);

    // Another provider: a plain send on it ends the share of the first.
    service.context_share(&chat, PANE, Some("local"), None, true).expect("a share");
    assert!(service.is_shared(&chat, PANE));
    let request = SendRequest::new(chat.clone(), format!("hello {TEXT_CANARY}"), PANE).profile("second");
    service.send(request).expect("a plain send");
    assert!(rig.sink.wait_ended(&chat, 1));
    assert!(!service.is_shared(&chat, PANE));
    assert_eq!(fake.accepted(), 1);
    let sent = fake.seen()[0].body_text();
    assert!(!sent.contains("<holzwave-context"), "the plain send carries no context");

    // Another base address: the profile moves to a second server and the share does not follow.
    service.context_share(&chat, PANE, Some("local"), None, true).expect("a share");
    let mut input = ProfileInput::new("Local", Preset::Custom);
    input.id = Some("local".to_owned());
    input.base_url = format!("{}/v1", moved.base());
    input.model = "test-model".to_owned();
    service.save_profile(input).expect("the profile moves");
    let state = service.context_state(&chat, PANE, Some("local"), Some("test-model")).expect("a state");
    assert!(!state.shared, "a changed address asks again");
    assert!(!service.is_shared(&chat, PANE));

    assert_eq!(rig.source.reads(), reads_before, "no reset reads a pane");
}

#[test]
fn closing_the_chat_or_the_pane_or_deleting_the_chat_ends_the_share_and_voids_the_offer() {
    let fake = fake_with(0);
    let rig = Rig::new("ctx-close");
    let base = format!("{}/v1", fake.base());
    rig.profiles(&[("Local", &base, "test-model", None, false), ("Second", &base, "test-model", None, false)]);
    rig.source.offer(8, &[ContextKind::Screen]);
    rig.source.give(8, ContextKind::Screen, Some(screen_of(8)));
    let service = rig.service();
    let (a, b) = (new_chat(&service), new_chat(&service));
    let preview = |chat: &ConversationId, pane: PaneId| {
        service.context_preview(chat, pane, Some("local"), None, &[ContextKind::Screen]).expect("a preview")
    };
    let share = |chat: &ConversationId, pane: PaneId| {
        service.context_share(chat, pane, Some("local"), None, true).expect("a share");
    };

    // The chat closes: both of its shares and its offer go, the other chat keeps everything.
    share(&a, 7);
    share(&a, 8);
    share(&b, 7);
    let offer_a = preview(&a, 7);
    let offer_b = preview(&b, 7);
    assert_eq!(service.gate().pending(), 2);
    service.chat_closed(&a);
    assert!(!service.is_shared(&a, 7) && !service.is_shared(&a, 8) && service.is_shared(&b, 7));
    assert_eq!(service.gate().pending(), 1);
    assert_eq!(context_error(send_with_offer(&service, &a, 7, &offer_a, &[1])), ContextRefusal::StaleOffer);
    assert_eq!(service.gate().pending(), 1, "the offer of the other chat lives");
    drop(offer_b);

    // The pane closes: its shares go for every chat, with the offers made for it.
    share(&a, 7);
    share(&a, 8);
    share(&b, 7);
    let offer_a = preview(&a, 8);
    let offer_b = preview(&b, 7);
    assert_eq!(service.gate().pending(), 2);
    service.pane_closed(7);
    assert!(!service.is_shared(&a, 7) && !service.is_shared(&b, 7));
    assert!(service.is_shared(&a, 8), "the other pane stays shared");
    assert_eq!(service.gate().pending(), 1);
    assert_eq!(context_error(send_with_offer(&service, &b, 7, &offer_b, &[1])), ContextRefusal::StaleOffer);
    assert_eq!(service.gate().pending(), 1, "the offer of the open pane lives");

    // The chat is deleted: its share and its offer go.
    service.delete_conversation(&a).expect("a delete");
    assert!(!service.is_shared(&a, 8));
    assert_eq!(service.gate().pending(), 0);
    assert_eq!(context_error(send_with_offer(&service, &a, 8, &offer_a, &[1])), ContextRefusal::StaleOffer);

    // The profile is removed: the shares bound to it go, the ones bound to another profile stay.
    let (x, y) = (new_chat(&service), new_chat(&service));
    service.context_share(&x, 7, Some("local"), None, true).expect("a share");
    service.context_share(&y, 7, Some("second"), None, true).expect("a share");
    service.context_preview(&x, 7, Some("local"), None, &[ContextKind::Screen]).expect("a preview");
    service.context_preview(&y, 7, Some("second"), None, &[ContextKind::Screen]).expect("a preview");
    assert_eq!(service.gate().pending(), 2);
    assert!(service.remove_profile("local").expect("a removal").removed);
    assert!(!service.is_shared(&x, 7) && service.is_shared(&y, 7));
    assert_eq!(service.gate().pending(), 1);
}

#[test]
fn local_and_on_device_destinations_ask_by_default_and_the_setting_skips_the_share_only_for_them() {
    let fake = fake_with(2);
    let rig = Rig::new("ctx-local");
    rig.profiles(&[
        ("Local", &format!("{}/v1", fake.base()), "test-model", Some(KEY_CANARY), false),
        ("Near", "http://192.168.50.20:11434/v1", "m", None, true),
        ("Far", "https://203.0.113.7/v1", "m", None, false),
    ]);
    let service = rig.service();
    register_device(&service, "device", Locality::OnDevice);
    let chat = new_chat(&service);
    let required = |profile: &str| service.context_state(&chat, PANE, Some(profile), None).expect("a state").required;

    // The default asks everywhere.
    assert!(required("local") && required("device") && required("near") && required("far"));
    assert!(share_required(Locality::Loopback, false) && share_required(Locality::OnDevice, false));
    assert!(!share_required(Locality::Loopback, true) && !share_required(Locality::OnDevice, true));
    assert!(share_required(Locality::Lan, true) && share_required(Locality::Cloud, true));

    // "auto" skips the ask for a local destination and for nothing else.
    service.apply_settings(&settings(|s| s.ai_local_context = AiLocalContext::Auto));
    assert!(!required("local") && !required("device"));
    assert!(required("near") && required("far"));
    for profile in ["near", "far"] {
        let refused = service.context_preview(&chat, PANE, Some(profile), None, &[ContextKind::Screen]);
        assert_eq!(context_error(refused), ContextRefusal::NotShared, "{profile} still asks");
        service.context_share(&chat, PANE, Some(profile), None, true).expect("a share");
        let view = service.context_preview(&chat, PANE, Some(profile), None, &[ContextKind::Screen]);
        assert!(view.is_ok(), "{profile} previews once it is shared");
    }
    assert_eq!(fake.accepted(), 0, "no connection is made to either address");

    // Under "auto" a local destination previews without a share and sends; switching back to "ask" between the preview
    // and the send refuses the send, with the offer spent and nothing stored.
    let view = view_of(&service, &chat, &[ContextKind::Screen]);
    assert!(!service.is_shared(&chat, PANE));
    service.apply_settings(&settings(|s| s.ai_local_context = AiLocalContext::Ask));
    let refused = send_with_offer(&service, &chat, PANE, &view, &[1]);
    assert_eq!(context_error(refused), ContextRefusal::NotShared);
    assert_eq!(service.gate().pending(), 0, "the offer was spent");
    assert!(service.snapshot(&chat).expect("a snapshot").messages.is_empty(), "nothing was stored");
    assert_eq!(fake.accepted(), 0);

    service.apply_settings(&settings(|s| s.ai_local_context = AiLocalContext::Auto));
    let view = view_of(&service, &chat, &[ContextKind::Screen]);
    send_with_offer(&service, &chat, PANE, &view, &[1]).expect("the send is accepted");
    assert!(rig.sink.wait_ended(&chat, 1));
    assert_eq!(rig.sink.last_end(&chat).end, "done");
    assert_eq!(fake.accepted(), 1);
}

#[test]
fn local_only_refuses_a_destination_that_leaves_the_device() {
    let fake = fake_with(0);
    let rig = Rig::new("ctx-only");
    rig.profiles(&[
        ("Local", &format!("{}/v1", fake.base()), "test-model", None, false),
        ("Near", "http://192.168.50.20:11434/v1", "m", None, true),
        ("Far", "https://203.0.113.7/v1", "m", None, false),
    ]);
    let service = rig.service();
    register_device(&service, "device", Locality::OnDevice);
    register_device(&service, "cloudy", Locality::Cloud);
    let chat = new_chat(&service);
    let kinds = [ContextKind::Screen];

    service.apply_settings(&settings(|s| s.ai_local_only = true));
    for profile in ["far", "near", "cloudy"] {
        let state = service.context_state(&chat, PANE, Some(profile), None);
        assert_eq!(context_error(state), ContextRefusal::LocalOnly, "{profile}");
        let share = service.context_share(&chat, PANE, Some(profile), None, true);
        assert_eq!(context_error(share), ContextRefusal::LocalOnly, "{profile}");
        let preview = service.context_preview(&chat, PANE, Some(profile), None, &kinds);
        assert_eq!(context_error(preview), ContextRefusal::LocalOnly, "{profile}");
    }
    for profile in ["local", "device"] {
        service.context_state(&chat, PANE, Some(profile), None).expect("a local state");
        service.context_share(&chat, PANE, Some(profile), None, true).expect("a local share");
        service.context_preview(&chat, PANE, Some(profile), None, &kinds).expect("a local preview");
    }
    assert_eq!(rig.source.reads(), 2, "only the two local previews read a pane");

    // Off again: the destinations that were refused are allowed.
    service.apply_settings(&settings(|s| s.ai_local_only = false));
    for profile in ["far", "near", "cloudy"] {
        service.context_state(&chat, PANE, Some(profile), None).expect("allowed again");
    }
    assert_eq!(fake.accepted(), 0);
}

#[test]
fn the_destination_is_checked_again_when_the_turn_starts() {
    let fake = fake_with(0);
    let rig = Rig::new("ctx-recheck");
    rig.profiles(&[("Llm", "https://llm.example.test/v1", "test-model", Some(KEY_CANARY), false)]);
    let resolver = FlipResolver::new("192.168.1.5", "93.184.216.34", 3);
    let service = rig.service_resolving(&resolver);
    let chat = new_chat(&service);

    // The three explicit calls look the name up once each, and the destination is on the local network.
    let state = service.context_state(&chat, PANE, Some("llm"), None).expect("a state");
    assert_eq!(state.locality, Locality::Lan);
    service.context_share(&chat, PANE, Some("llm"), None, true).expect("a share");
    let view = service.context_preview(&chat, PANE, Some("llm"), None, &[ContextKind::Screen]).expect("a preview");
    assert_eq!(view.locality, Locality::Lan);
    assert_eq!(resolver.lookups(), 3);

    // The name now points to the internet: the turn that carries the context fails before any request.
    let request = SendRequest::new(chat.clone(), format!("explain {TEXT_CANARY}"), PANE)
        .profile("llm")
        .offer(offer_id(&view), vec![1]);
    service.send(request).expect("the send is accepted");
    assert!(rig.sink.wait_ended(&chat, 1));
    let end = rig.sink.last_end(&chat);
    assert_eq!((end.end.as_str(), end.rounds), ("failed", 0), "{end:?}");
    let error = end.error.expect("an error");
    assert_eq!(error.class, "policy");
    for text in ["93.184", "192.168", "example.test"] {
        assert!(!error.text.contains(text), "the text names an address: {}", error.text);
    }
    assert_eq!(resolver.lookups(), 4, "the turn looked the name up once more");
    assert_eq!(rig.sink.ended(&chat), 1);
    assert_eq!(rig.stored_text(), "", "no chat file was written");
    assert_eq!(fake.accepted(), 0);
    assert_eq!(service.gate().pending(), 0);
}

#[test]
fn only_the_explicit_calls_read_a_pane_or_look_up_a_name() {
    let fake = fake_with(1);
    let rig = Rig::new("ctx-quiet");
    rig.profiles(&[
        ("Local", &format!("{}/v1", fake.base()), "test-model", Some(KEY_CANARY), false),
        ("Llm", "https://llm.example.test/v1", "test-model", None, false),
    ]);
    let resolver = FlipResolver::new("192.168.1.5", "192.168.1.5", usize::MAX);
    let service = rig.service_resolving(&resolver);
    let chat = new_chat(&service);
    let quiet = |what: &str| {
        assert_eq!(resolver.lookups(), 0, "{what} looked a name up");
        assert_eq!(rig.source.kinds_calls(), 0, "{what} asked a pane for its kinds");
        assert_eq!(rig.source.reads(), 0, "{what} read a pane");
    };

    // Building the service, the idle calls and a plain send do none of it.
    quiet("building the service");
    let _ = service.profiles();
    let _ = service.snapshot(&chat);
    let _ = service.list_conversations();
    let _ = service.new_conversation();
    service.apply_settings(&Settings::default());
    let _ = service.gate().pending();
    let _ = service.approvals();
    let _ = service.is_shared(&chat, PANE);
    service.chat_closed(&chat);
    let _ = service.pane_closed(PANE);
    quiet("the idle calls");
    service.send(SendRequest::new(chat.clone(), "hello".to_owned(), PANE).profile("local")).expect("a plain send");
    assert!(rig.sink.wait_ended(&chat, 1));
    quiet("a plain send");
    assert_eq!(fake.accepted(), 1);

    // Each explicit call does its own work and no more.
    service.context_state(&chat, PANE, Some("llm"), None).expect("a state");
    assert_eq!((resolver.lookups(), rig.source.reads(), rig.source.kinds_calls()), (1, 0, 1));
    service.context_share(&chat, PANE, Some("llm"), None, true).expect("a share");
    assert_eq!((resolver.lookups(), rig.source.reads()), (2, 0));
    service
        .context_preview(&chat, PANE, Some("llm"), None, &[ContextKind::Selection, ContextKind::Screen])
        .expect("a preview");
    assert_eq!((resolver.lookups(), rig.source.reads()), (3, 2));
    assert_eq!(fake.accepted(), 1, "none of them opened a connection");

    // A command of the shell can run on the thread of an async runtime, where the chokepoint refuses a blocking call:
    // the calls look the destination up on a thread of their own and still work there.
    let runtime = tokio::runtime::Builder::new_current_thread().build().expect("a runtime");
    let inside = runtime.block_on(async { service.context_state(&chat, PANE, Some("llm"), None) });
    assert!(inside.is_ok(), "{inside:?}");
    assert_eq!(resolver.lookups(), 4);
}

#[test]
fn previews_are_bounded_one_live_offer_per_chat_and_every_refusal_has_a_fixed_reason() {
    let fake = fake_with(0);
    let rig = Rig::new("ctx-bounds");
    rig.local(&fake);
    let service = rig.service();
    let chat = new_chat(&service);
    service.context_share(&chat, PANE, Some("local"), None, true).expect("a share");
    let preview = |kinds: &[ContextKind]| service.context_preview(&chat, PANE, Some("local"), None, kinds);
    let reads = || rig.source.reads();

    // The kinds asked for are checked first.
    assert_eq!(context_error(preview(&[])), ContextRefusal::BadKinds);
    let six = [
        ContextKind::Selection,
        ContextKind::Screen,
        ContextKind::LastCommand,
        ContextKind::File,
        ContextKind::Preview,
        ContextKind::Selection,
    ];
    assert_eq!(six.len(), MAX_PREVIEW_KINDS + 1);
    assert_eq!(context_error(preview(&six)), ContextRefusal::BadKinds);
    assert_eq!(context_error(preview(&[ContextKind::Screen, ContextKind::Screen])), ContextRefusal::BadKinds);
    assert_eq!(context_error(preview(&[ContextKind::File])), ContextRefusal::UnsupportedKind);
    assert_eq!(reads(), 0, "no refusal so far read the pane");

    // Nothing there is nothing to share.
    assert_eq!(context_error(preview(&[ContextKind::Selection])), ContextRefusal::NothingToShare);

    // Each way a source can refuse has its own stable reason.
    let reasons = [
        (ReadRefusal::PaneGone, "pane_gone"),
        (ReadRefusal::Unsupported, "unsupported_kind"),
        (ReadRefusal::Denied, "denied"),
        (ReadRefusal::NotText, "not_text"),
        (ReadRefusal::Unreadable, "unreadable"),
        (ReadRefusal::Busy, "busy"),
    ];
    for (refusal, reason) in reasons {
        rig.source.refuse_with(Some(refusal));
        let seen = context_error(preview(&[ContextKind::Screen]));
        assert_eq!((seen.reason(), refusal.reason()), (reason, reason));
    }
    rig.source.refuse_with(None);

    // A read that was cut at its limit says so in the label.
    rig.source.give(PANE, ContextKind::Screen, Some(Item { cut: true, ..screen_item() }));
    let view = preview(&[ContextKind::Screen]).expect("a preview");
    assert!(view.items[0].label.ends_with(CUT_NOTE), "{}", view.items[0].label);
    rig.source.give(PANE, ContextKind::Screen, Some(screen_item()));

    // A chat holds one live offer: a new preview voids the old one.
    let first = preview(&[ContextKind::Screen]).expect("a preview");
    let second = preview(&[ContextKind::Screen]).expect("a preview");
    assert_eq!(service.gate().pending(), 1);
    assert_eq!(context_error(send_with_offer(&service, &chat, PANE, &first, &[1])), ContextRefusal::StaleOffer);
    assert_ne!(first.offer, second.offer);

    // The 13 reasons are distinct, lower-case and the documented ones; no phrase names a pane, a path or a host.
    let names: Vec<&str> = ContextRefusal::ALL.iter().map(|refusal| refusal.reason()).collect();
    assert_eq!(
        names,
        [
            "not_shared",
            "stale_offer",
            "bad_pick",
            "bad_kinds",
            "unsupported_kind",
            "nothing_to_share",
            "pane_gone",
            "denied",
            "not_text",
            "unreadable",
            "busy",
            "destination",
            "local_only"
        ]
    );
    for refusal in ContextRefusal::ALL {
        let phrase = refusal.to_string();
        assert!(!phrase.chars().any(|c| c.is_ascii_digit() || c == '/' || c == '\\' || c == ':'), "{phrase}");
        assert!(refusal.reason().chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        let fault = ServiceError::Context(refusal);
        assert_eq!((fault.code(), fault.to_string()), ("context", phrase));
    }

    // At most MAX_SHARES shares are remembered: the oldest goes first.
    rig.source.every_pane_offers(&[ContextKind::Screen]);
    let others = new_chat(&service);
    let panes: Vec<PaneId> = (1000..1000 + MAX_SHARES as PaneId + 44).collect();
    for pane in &panes {
        service.context_share(&others, *pane, Some("local"), None, true).expect("a share");
    }
    let kept = panes.iter().filter(|pane| service.is_shared(&others, **pane)).count();
    assert_eq!(kept, MAX_SHARES);
    assert!(!service.is_shared(&others, panes[0]) && service.is_shared(&others, panes[panes.len() - 1]));
}

#[test]
fn the_stored_chat_keeps_a_marker_and_no_view_or_debug_line_holds_context_text() {
    let fake = fake_with(3);
    let rig = Rig::new("ctx-marker");
    rig.local(&fake);
    let service = rig.service();
    let chat = new_chat(&service);
    let canaries = [KEY_CANARY, SCREEN_KEY, HIDDEN_CANARY, "$ env", "ANTHROPIC_API_KEY"];

    // With the switch off the file keeps the label, the size and the hash, and no text.
    service.context_share(&chat, PANE, Some("local"), None, true).expect("a share");
    let state = service.context_state(&chat, PANE, Some("local"), None).expect("a state");
    let view = view_of(&service, &chat, &[ContextKind::Screen]);
    send_with_offer(&service, &chat, PANE, &view, &[1]).expect("the send is accepted");
    assert!(rig.sink.wait_ended(&chat, 1));
    let stored = rig.stored_text();
    assert!(stored.contains("Screen of pane 7") && stored.contains(&format!("\"bytes\":{}", view.items[0].bytes)));
    assert!(stored.contains("\"hash\""));
    for canary in canaries {
        assert!(!stored.contains(canary), "{canary} is in the stored chat");
    }
    assert!(!stored.contains("[REDACTED:anthropic_key]"), "not even the cleaned text is stored");

    // The same content cannot be sent twice in one chat: the refusal comes before anything is stored.
    let view = view_of(&service, &chat, &[ContextKind::Screen]);
    let again = send_with_offer(&service, &chat, PANE, &view, &[1]);
    assert!(matches!(again, Err(ServiceError::ContextDuplicate)), "{again:?}");
    assert_eq!(service.snapshot(&chat).expect("a snapshot").messages.len(), 2, "nothing was added to the chat");

    // With the switch on the next context turn stores the cleaned text once, and the follow-up does not carry it.
    service.apply_settings(&settings(|s| s.ai_persist_context = true));
    let changed = Item { text: format!("{}\nand a later line", screen_text()), ..screen_item() };
    rig.source.give(PANE, ContextKind::Screen, Some(changed));
    let view = view_of(&service, &chat, &[ContextKind::Screen]);
    assert!(view.persist);
    send_with_offer(&service, &chat, PANE, &view, &[1]).expect("the send is accepted");
    assert!(rig.sink.wait_ended(&chat, 2));
    let stored = rig.stored_text();
    assert_eq!(stored.matches("[REDACTED:anthropic_key]").count(), 1, "the cleaned text is stored once");
    for canary in [KEY_CANARY, SCREEN_KEY, HIDDEN_CANARY] {
        assert!(!stored.contains(canary), "{canary} is in the stored chat");
    }
    service.send(SendRequest::new(chat.clone(), "and then".to_owned(), PANE).profile("local")).expect("a follow-up");
    assert!(rig.sink.wait_ended(&chat, 3));
    let seen = fake.seen();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[1].body_text().matches("[REDACTED:anthropic_key]").count(), 1, "the approved turn carries it once");
    assert!(!seen[2].body_text().contains("[REDACTED:anthropic_key]"), "the follow-up does not carry it");

    // No view, event, snapshot or Debug line holds a canary or the screen text.
    let fetched = Fetched::new("label", SourceText::with_hidden(screen_text(), vec![0..1]), true);
    let pending = SendRequest::new(chat.clone(), format!("explain {TEXT_CANARY}"), PANE)
        .profile("local")
        .offer(offer_id(&view), vec![1]);
    let mut shown = vec![
        serde_json::to_string(&state).expect("json"),
        serde_json::to_string(&view).expect("json"),
        serde_json::to_string(&rig.sink.all()).expect("json"),
        serde_json::to_string(&service.snapshot(&chat).expect("a snapshot")).expect("json"),
        format!("{state:?}{state:#?}{view:?}{view:#?}{fetched:?}{fetched:#?}{pending:?}{pending:#?}"),
    ];
    for refusal in ContextRefusal::ALL {
        shown.push(format!("{refusal:?}{refusal:#?}{}", ServiceError::Context(refusal)));
        shown.push(format!("{:?}", ServiceError::Context(refusal)));
    }
    for refusal in [ReadRefusal::PaneGone, ReadRefusal::Busy, ReadRefusal::Denied] {
        shown.push(format!("{refusal:?}{refusal:#?}"));
    }
    let typed_only_in_requests = [SCREEN_KEY, HIDDEN_CANARY, KEY_CANARY, "$ env", "ANTHROPIC_API_KEY"];
    for text in &shown {
        for canary in typed_only_in_requests {
            assert!(!text.contains(canary), "{canary} is in a view or a Debug line");
        }
    }
    assert!(!shown[4].contains(TEXT_CANARY), "a request prints its size, not the text");
}
