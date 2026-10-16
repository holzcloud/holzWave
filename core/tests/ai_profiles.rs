//! Provider profiles, their keys and the file they live in, with no outside network.
//!
//! A profile is saved with a key in a test secret store, the file is read back, and the profile is resolved into the
//! endpoint of the HTTP chokepoint and driven against `FakeHttp`, a server in this process. What is proved is the
//! boundary around a key: where it is stored (one keychain entry per slot, never a file), what leaves (a status, never
//! text), which address it may go to (the one it was saved for) and what happens when anything is missing, damaged or
//! hostile (nothing is sent). The dialect under test is `EchoAdapter`, a tiny one defined here, so the wire adapters
//! are not involved. Every wait is bounded.

mod common;

use std::collections::BTreeSet;
use std::fmt::Debug;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use common::fake_http::{FakeHttp, Reply};
use holzwave_core::ai::http::{Frame, Http, HttpConfig, Refusal};
use holzwave_core::ai::profiles::{
    AiConfig, FILE, FORMAT_VERSION, KeyError, KeySlot, KeyStatus, KeyVault, MAX_BYTES, MAX_ID, MAX_KEY_BYTES,
    MAX_MODEL, MAX_NAME, MAX_PROFILES, Preset, Profile, ProfileError, ProfileInput, Removal, ResolveError,
};
use holzwave_core::ai::provider::{
    AiError, CancelToken, ChatRequest, Collector, Completion, Detail, Flow, Provider, StreamEvent,
};
use holzwave_core::ai::sse::StreamEnd;
use holzwave_core::ai::types::{ApiKey, Capabilities, Locality, Message, Role, StopReason, Usage};
use holzwave_core::ai::wire::{Wire, WireAdapter, WireRequest, WireTurn};
use holzwave_core::layout::LoadStatus;
use holzwave_core::secrets::{MemoryStore, SecretStore};
use serde_json::{Value, json};

// ---- the test dialect and the helpers -------------------------------------------------------------------

/// The key of every test. It appears in test code only: a hit anywhere else is a leak.
const CANARY: &str = "zq-canary-5d1e8a07c3f2b9";

/// A dialect with one JSON line as the request and one text delta per frame as the answer.
struct EchoAdapter {
    wire: Wire,
}

impl EchoAdapter {
    fn new(wire: Wire) -> Self {
        Self { wire }
    }
}

struct EchoTurn {
    done: bool,
}

impl WireAdapter for EchoAdapter {
    fn wire(&self) -> Wire {
        self.wire
    }

    fn capabilities(&self, _model: &str, locality: Locality) -> Capabilities {
        Capabilities::conservative(locality)
    }

    fn request(&self, req: &ChatRequest, _caps: &Capabilities) -> Result<WireRequest, AiError> {
        let text: Vec<String> = req.messages.iter().filter(|m| m.role == Role::User).map(Message::text).collect();
        let body = json!({ "model": req.model, "text": text.join("\n") });
        Ok(WireRequest {
            path: "/v1/lines".to_owned(),
            headers: Vec::new(),
            body: serde_json::to_vec(&body).expect("a body"),
        })
    }

    fn turn(&self) -> Box<dyn WireTurn> {
        Box::new(EchoTurn { done: false })
    }
}

impl WireTurn for EchoTurn {
    fn frame(&mut self, frame: Frame<'_>, out: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<Flow, AiError> {
        match frame {
            Frame::Event(event) if event.is_done() => {
                self.done = true;
                Ok(Flow::Continue)
            }
            Frame::Event(event) => Ok(out(StreamEvent::TextDelta(event.data().to_owned()))),
            Frame::Line(_) => Err(AiError::Protocol(Detail::new("this dialect reads events only"))),
        }
    }

    fn finish(
        self: Box<Self>,
        end: StreamEnd,
        _out: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<Completion, AiError> {
        if end.is_clean() && self.done {
            Ok(Completion { stop_reason: StopReason::EndTurn, usage: Usage::default(), native: None })
        } else {
            Err(AiError::Protocol(Detail::new("the stream ended without its terminal event")))
        }
    }
}

/// A secret store that counts its calls and can list what it holds without counting.
struct CountingStore {
    inner: MemoryStore,
    accounts: Mutex<BTreeSet<String>>,
    gets: AtomicUsize,
    sets: AtomicUsize,
    deletes: AtomicUsize,
}

impl CountingStore {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MemoryStore::default(),
            accounts: Mutex::new(BTreeSet::new()),
            gets: AtomicUsize::new(0),
            sets: AtomicUsize::new(0),
            deletes: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.gets.load(Ordering::SeqCst) + self.sets.load(Ordering::SeqCst) + self.deletes.load(Ordering::SeqCst)
    }

    /// Every entry, without counting as a call.
    fn entries(&self) -> Vec<(String, String)> {
        let accounts = self.accounts.lock().unwrap_or_else(PoisonError::into_inner).clone();
        accounts
            .into_iter()
            .filter_map(|account| self.inner.get(&account).ok().flatten().map(|secret| (account, secret)))
            .collect()
    }

    /// Replaces an entry behind the back of the code under test.
    fn plant(&self, account: &str, secret: &str) {
        self.accounts.lock().unwrap_or_else(PoisonError::into_inner).insert(account.to_owned());
        self.inner.set(account, secret).expect("plant an entry");
    }
}

impl SecretStore for CountingStore {
    fn get(&self, account: &str) -> holzwave_core::Result<Option<String>> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.inner.get(account)
    }

    fn set(&self, account: &str, secret: &str) -> holzwave_core::Result<()> {
        self.sets.fetch_add(1, Ordering::SeqCst);
        self.accounts.lock().unwrap_or_else(PoisonError::into_inner).insert(account.to_owned());
        self.inner.set(account, secret)
    }

    fn delete(&self, account: &str) -> holzwave_core::Result<()> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        self.accounts.lock().unwrap_or_else(PoisonError::into_inner).remove(account);
        self.inner.delete(account)
    }
}

/// A secret store whose every call fails, as a keychain without a running service does.
#[derive(Default)]
struct FailingStore {
    calls: AtomicUsize,
}

impl FailingStore {
    fn fail<T>(&self) -> holzwave_core::Result<T> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(holzwave_core::Error::Unsupported("keychain: no secret service".into()))
    }
}

impl SecretStore for FailingStore {
    fn get(&self, _: &str) -> holzwave_core::Result<Option<String>> {
        self.fail()
    }

    fn set(&self, _: &str, _: &str) -> holzwave_core::Result<()> {
        self.fail()
    }

    fn delete(&self, _: &str) -> holzwave_core::Result<()> {
        self.fail()
    }
}

/// How many files below `dir` hold `needle`.
fn scan_dir_for(dir: &Path, needle: &str) -> usize {
    let mut found = 0;
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found += scan_dir_for(&path, needle);
        } else if let Ok(bytes) = fs::read(&path)
            && bytes.windows(needle.len()).any(|window| window == needle.as_bytes())
        {
            found += 1;
        }
    }
    found
}

/// A Custom profile named `Local` that speaks the chat wire at `base`.
fn input(base: &str) -> ProfileInput {
    let mut input = ProfileInput::new("Local", Preset::Custom);
    input.wire = Some(Wire::OpenAiChat);
    input.base_url = base.to_owned();
    input
}

fn key(text: &str) -> ApiKey {
    ApiKey::new(text.to_owned())
}

fn request(text: &str) -> ChatRequest {
    let mut request = ChatRequest::new("m");
    request.messages.push(Message::user_text("u1", text, 1));
    request
}

/// What a run delivered.
struct Run {
    events: Vec<StreamEvent>,
    text: String,
    result: Result<Completion, AiError>,
}

fn run(provider: &dyn Provider, request: &ChatRequest) -> Run {
    let mut events = Vec::new();
    let mut collector = Collector::new();
    let result = provider.stream(
        request,
        &mut |event| {
            events.push(event.clone());
            collector.push(event)
        },
        &CancelToken::new(),
    );
    Run { events, text: collector.text().to_owned(), result }
}

fn http() -> Arc<Http> {
    Arc::new(Http::new(HttpConfig::standard()))
}

/// A server with one event-stream reply that says `Hello`.
fn hello_server() -> FakeHttp {
    FakeHttp::start(vec![Reply::sse(&["data: Hel\n\n", "data: lo\n\n", "data: [DONE]\n\n"])])
}

/// Resolves `id` and sends one request through the chokepoint.
fn send(config: &AiConfig, id: &str, vault: &KeyVault) -> Result<Run, ResolveError> {
    let resolved = config.resolve(id, vault, false)?;
    let provider = resolved.into_provider(Box::new(EchoAdapter::new(Wire::OpenAiChat)), http());
    Ok(run(&provider, &request("hello")))
}

/// Debug and pretty Debug of a value, to look for a key in.
fn shown<T: Debug>(value: &T) -> String {
    format!("{value:?}\n{value:#?}")
}

fn valid_json(id: &str) -> Value {
    json!({
        "id": id, "name": format!("Server {id}"), "preset": "custom", "wire": "openai_chat",
        "base_url": "http://127.0.0.1:1234/v1", "model": "", "allow_lan_http": false,
        "key_set": false, "gateway_key_set": false
    })
}

fn document(profiles: Vec<Value>) -> String {
    json!({ "version": 1, "profiles": profiles, "acknowledged": [] }).to_string()
}

// ---- the tracer -----------------------------------------------------------------------------------------

#[test]
fn a_profile_and_its_key_are_saved_resolved_and_sent_to_its_own_address() {
    let dir = common::scratch_dir("ai-profiles-tracer");
    let fake = hello_server();
    let store = CountingStore::new();
    let vault = KeyVault::new(store.clone());

    let mut config = AiConfig::default();
    let id = config.upsert(input(&format!("{}/v1", fake.base()))).expect("a profile");
    assert_eq!(id, "local");
    config.set_key(&vault, &id, KeySlot::Provider, key(CANARY)).expect("the key is saved");
    config.save(&dir).expect("the file is saved");

    let (loaded, status) = AiConfig::load(&dir);
    assert_eq!(status, LoadStatus::Clean);
    assert_eq!(loaded.profiles(), config.profiles(), "the file is read back equal");

    let resolved = loaded.resolve(&id, &vault, false).expect("the profile resolves");
    let scope = format!("http://127.0.0.1:{}", fake.port());
    assert_eq!(resolved.scope(), scope);
    assert_eq!(resolved.wire(), Wire::OpenAiChat);
    let provider = resolved.into_provider(Box::new(EchoAdapter::new(Wire::OpenAiChat)), http());
    let outcome = run(&provider, &request("hello"));

    outcome.result.as_ref().expect("the turn completes");
    assert_eq!(
        outcome.events,
        [StreamEvent::TextDelta("Hel".to_owned()), StreamEvent::TextDelta("lo".to_owned())],
        "the closure sees the deltas in order"
    );
    assert_eq!(outcome.text, "Hello");

    let seen = fake.seen();
    assert_eq!(seen.len(), 1, "exactly one request");
    let bearer = format!("Bearer {CANARY}");
    assert_eq!(seen[0].header("authorization"), Some(bearer.as_str()));
    for (name, value) in &seen[0].headers {
        assert!(name == "authorization" || !value.contains(CANARY), "the key is in the header {name}");
    }
    assert!(!seen[0].target.contains(CANARY), "the key is in the target");
    assert!(!seen[0].body_text().contains(CANARY), "the key is in the body");

    let profile = loaded.get(&id).expect("the profile");
    assert_eq!(vault.status(profile, KeySlot::Provider), KeyStatus::Present);
    let entries = store.entries();
    assert_eq!(entries.len(), 1, "one entry in the keychain: {entries:?}");
    assert_eq!(entries[0].0, "ai:local");
    let record: Value = serde_json::from_str(&entries[0].1).expect("the entry is one JSON record");
    assert_eq!(record, json!({ "v": 1, "scope": scope, "key": CANARY }));
    assert_eq!(entries[0].1, format!(r#"{{"v":1,"scope":"{scope}","key":"{CANARY}"}}"#), "compact, in this order");

    println!(
        "tracer: a_profile_and_its_key_are_saved_resolved_and_sent_to_its_own_address ok, text {:?}, record {}",
        outcome.text,
        entries[0].1.replace(CANARY, "<key>")
    );
    fs::remove_dir_all(&dir).expect("remove scratch directory");
}

#[test]
fn a_key_is_refused_for_an_address_it_was_not_set_for() {
    let dir = common::scratch_dir("ai-profiles-scope");
    let first = hello_server();
    let second = hello_server();
    let store = CountingStore::new();
    let vault = KeyVault::new(store.clone());

    let mut config = AiConfig::default();
    let id = config.upsert(input(&format!("{}/v1", first.base()))).expect("a profile");
    config.set_key(&vault, &id, KeySlot::Provider, key(CANARY)).expect("the key is saved");
    send(&config, &id, &vault).expect("resolves").result.expect("the first send works");
    assert_eq!(first.accepted(), 1);

    // The address changes to another port: the key stays where it was filed and is not used.
    let mut changed = input(&format!("{}/v1", second.base()));
    changed.id = Some(id.clone());
    config.upsert(changed).expect("the address is changed");
    let profile = config.get(&id).expect("the profile");
    assert_eq!(vault.status(profile, KeySlot::Provider), KeyStatus::WrongAddress);
    let error = config.resolve(&id, &vault, false).expect_err("the key belongs to another address");
    assert_eq!(error, ResolveError::KeyScope(KeySlot::Provider));
    assert_eq!(error.as_str(), "key_scope");
    assert_eq!(first.accepted(), 1, "nothing more reached the first server");
    assert_eq!(second.accepted(), 0, "nothing reached the new address");

    // A hand-edited file that names the second server gives the same refusal.
    let mut original = input(&format!("{}/v1", first.base()));
    original.id = Some(id.clone());
    config.upsert(original).expect("the address is restored");
    config.save(&dir).expect("saved");
    let text = fs::read_to_string(dir.join(FILE)).expect("the file");
    let edited = text.replace(&first.base(), &second.base());
    assert_ne!(edited, text, "the edit changed the file");
    fs::write(dir.join(FILE), edited).expect("hand edit");
    let (mut edited, status) = AiConfig::load(&dir);
    assert_eq!(status, LoadStatus::Clean);
    assert_eq!(edited.resolve(&id, &vault, false).expect_err("refused").as_str(), "key_scope");
    assert_eq!(second.accepted(), 0, "the edited address never saw the key");

    // Entering the key again binds it to the new address, and the second server receives it.
    edited.set_key(&vault, &id, KeySlot::Provider, key(CANARY)).expect("entered again");
    let outcome = send(&edited, &id, &vault).expect("resolves");
    outcome.result.expect("the send works");
    let bearer = format!("Bearer {CANARY}");
    assert_eq!(second.seen()[0].header("authorization"), Some(bearer.as_str()));
    assert_eq!(first.accepted(), 1, "the first server is not contacted again");
    fs::remove_dir_all(&dir).expect("remove scratch directory");
}

// ---- the file -------------------------------------------------------------------------------------------

#[test]
fn the_file_is_versioned_pretty_printed_and_pinned() {
    let dir = common::scratch_dir("ai-profiles-form");
    assert_eq!(FORMAT_VERSION, 1);
    assert_eq!(
        AiConfig::default().to_json(),
        "{\n  \"version\": 1,\n  \"profiles\": [],\n  \"acknowledged\": []\n}\n",
        "the empty form"
    );

    let store = CountingStore::new();
    let vault = KeyVault::new(store.clone());
    let mut config = AiConfig::default();
    let mut first = input("http://127.0.0.1:1234/v1");
    first.model = "m1".to_owned();
    config.upsert(first).expect("first");
    let mut second = ProfileInput::new("Claude", Preset::Anthropic);
    second.base_url = "https://api.example.test".to_owned();
    let claude = config.upsert(second).expect("second");
    config.set_key(&vault, &claude, KeySlot::Provider, key(CANARY)).expect("a key");

    let expected = r#"{
  "version": 1,
  "profiles": [
    {
      "id": "local",
      "name": "Local",
      "preset": "custom",
      "wire": "openai_chat",
      "base_url": "http://127.0.0.1:1234/v1",
      "model": "m1",
      "allow_lan_http": false,
      "key_set": false,
      "gateway_key_set": false
    },
    {
      "id": "claude",
      "name": "Claude",
      "preset": "anthropic",
      "wire": "anthropic",
      "base_url": "https://api.example.test",
      "model": "",
      "allow_lan_http": false,
      "key_set": true,
      "gateway_key_set": false
    }
  ],
  "acknowledged": []
}
"#;
    assert_eq!(config.to_json(), expected);
    assert!(!config.to_json().ends_with("\n\n"), "one trailing newline");
    config.save(&dir).expect("saved");
    assert_eq!(fs::read_to_string(dir.join(FILE)).expect("the file"), expected, "the file is the document");
    let (back, status) = AiConfig::load(&dir);
    assert_eq!(status, LoadStatus::Clean);
    assert_eq!(back, config);

    // A bare version loads clean and empty; a field this build does not know is ignored at every level.
    let (empty, status) = AiConfig::from_json(r#"{"version":1}"#);
    assert_eq!(status, LoadStatus::Clean);
    assert!(empty.profiles().is_empty());
    let mut extra = valid_json("one");
    extra["color"] = json!("red");
    let text = json!({ "version": 1, "profiles": [extra], "acknowledged": [], "theme": { "x": 1 } }).to_string();
    let (read, status) = AiConfig::from_json(&text);
    assert_eq!(status, LoadStatus::Clean, "unknown fields are ignored");
    assert_eq!(read.profiles().len(), 1);
    fs::remove_dir_all(&dir).expect("remove scratch directory");
}

#[test]
fn a_missing_file_is_clean_and_an_oversize_one_is_moved_aside() {
    let dir = common::scratch_dir("ai-profiles-size");
    let (config, status) = AiConfig::load(&dir);
    assert_eq!(status, LoadStatus::Clean, "a missing file");
    assert!(config.profiles().is_empty());

    // A file of exactly the limit is read.
    let mut exact = document(vec![valid_json("one")]).into_bytes();
    exact.resize(usize::try_from(MAX_BYTES).expect("fits"), b' ');
    fs::write(dir.join(FILE), &exact).expect("a file of the limit");
    let (config, status) = AiConfig::load(&dir);
    assert_eq!(status, LoadStatus::Clean, "a file of exactly the limit is read");
    assert_eq!(config.profiles().len(), 1);

    // One byte more is not read: it is moved aside, and the reason names the limit.
    let file = fs::File::create(dir.join(FILE)).expect("create");
    file.set_len(MAX_BYTES + 1).expect("a sparse file");
    drop(file);
    let (config, status) = AiConfig::load(&dir);
    assert!(config.profiles().is_empty());
    match status {
        LoadStatus::Reset(reason) => assert!(reason.contains(&MAX_BYTES.to_string()), "{reason}"),
        other => panic!("expected Reset, got {other:?}"),
    }
    assert!(!dir.join(FILE).exists(), "the oversize file was moved");
    let bad = fs::metadata(dir.join("ai.json.bad")).expect("the file is kept");
    assert_eq!(bad.len(), MAX_BYTES + 1);

    // A later save writes a fresh file next to the kept one.
    AiConfig::default().save(&dir).expect("saved");
    assert!(dir.join(FILE).exists());
    assert_eq!(fs::metadata(dir.join("ai.json.bad")).expect("still kept").len(), MAX_BYTES + 1);
    fs::remove_dir_all(&dir).expect("remove scratch directory");
}

#[test]
fn a_damaged_or_newer_file_is_kept_as_bad_and_reset_with_a_reason() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("invalid UTF-8", [b"\xff\xfe{\"version\":1} ".as_slice(), CANARY.as_bytes()].concat()),
        ("not JSON", format!("not json {CANARY}").into_bytes()),
        ("an array", format!("[\"{CANARY}\"]").into_bytes()),
        ("no version", format!("{{\"profiles\":[],\"note\":\"{CANARY}\"}}").into_bytes()),
        ("a version that is a string", format!("{{\"version\":\"1\",\"note\":\"{CANARY}\"}}").into_bytes()),
        ("a newer version", format!("{{\"version\":2,\"profiles\":[{{\"name\":\"{CANARY}\"}}]}}").into_bytes()),
    ];
    for (label, bytes) in cases {
        let dir = common::scratch_dir("ai-profiles-damage");
        fs::write(dir.join(FILE), &bytes).expect("the damaged file");

        let (config, status) = AiConfig::load(&dir);

        assert!(config.profiles().is_empty(), "{label}: the app starts empty");
        let LoadStatus::Reset(reason) = status else { panic!("{label}: expected Reset, got {status:?}") };
        assert!(!reason.is_empty() && !reason.contains(CANARY), "{label}: the reason quotes content: {reason}");
        assert_eq!(fs::read(dir.join("ai.json.bad")).expect("kept"), bytes, "{label}: kept byte for byte");
        if label == "a newer version" {
            assert!(reason.contains('2') && reason.contains('1'), "the reason names both numbers: {reason}");
        }

        // A later save writes a fresh file and leaves the kept copy alone.
        AiConfig::default().save(&dir).expect("saved");
        assert_eq!(AiConfig::load(&dir).1, LoadStatus::Clean, "{label}");
        assert_eq!(fs::read(dir.join("ai.json.bad")).expect("kept"), bytes, "{label}: the copy survives the save");
        fs::remove_dir_all(&dir).expect("remove scratch directory");
    }
}

#[test]
fn a_hostile_file_loses_only_its_bad_profiles_each_with_a_note() {
    let long_model = "m".repeat(MAX_MODEL + 1);
    let hostile: Vec<Value> = vec![
        json!({ "id": "BadId", "name": CANARY }),
        json!({ "id": "apple", "name": CANARY }),
        json!({ "id": "v01", "name": CANARY }),
        {
            let mut p = valid_json("ctl");
            p["name"] = json!(format!("bad\u{7}name {CANARY}"));
            p
        },
        {
            let mut p = valid_json("longmodel");
            p["model"] = json!(long_model);
            p
        },
        {
            let mut p = valid_json("meta");
            p["base_url"] = json!(format!("https://169.254.169.254/{CANARY}"));
            p
        },
        {
            let mut p = valid_json("creds");
            p["base_url"] = json!(format!("https://user:{CANARY}@h.example"));
            p
        },
        {
            let mut p = valid_json("wire");
            p["preset"] = json!("anthropic");
            p["wire"] = json!("ollama");
            p["name"] = json!(CANARY);
            p
        },
        {
            let mut p = valid_json("preset");
            p["preset"] = json!("no-such-preset");
            p["name"] = json!(CANARY);
            p
        },
    ];
    let mut entries = Vec::new();
    let mut hostile_at = Vec::new();
    for i in 0..33 {
        entries.push(valid_json(&format!("v{:02}", i + 1)));
        if let Some(bad) = hostile.get(i) {
            entries.push(bad.clone());
            hostile_at.push(entries.len());
        }
    }
    let over_limit_at = entries.len();
    let text = json!({
        "version": 1,
        "profiles": entries,
        "acknowledged": ["http://127.0.0.1:9", format!("not a scope {CANARY}"), 5, "http://127.0.0.1:9"]
    })
    .to_string();

    let (config, status) = AiConfig::from_json(&text);

    let kept: Vec<&str> = config.profiles().iter().map(|p| p.id.as_str()).collect();
    let expected: Vec<String> = (1..=MAX_PROFILES).map(|i| format!("v{i:02}")).collect();
    assert_eq!(kept, expected.iter().map(String::as_str).collect::<Vec<_>>(), "the valid ones survive, in order");
    let LoadStatus::Repaired(notes) = status else { panic!("expected Repaired, got {status:?}") };
    for number in hostile_at.iter().chain([&over_limit_at]) {
        let prefix = format!("profile #{number}: ");
        assert_eq!(
            notes.iter().filter(|note| note.starts_with(&prefix)).count(),
            1,
            "one note for #{number}: {notes:?}"
        );
    }
    assert_eq!(
        notes.len(),
        hostile_at.len() + 1 + 2,
        "one note per dropped profile and one per bad address: {notes:?}"
    );
    assert!(notes.iter().all(|note| !note.contains(CANARY)), "a note quotes content: {notes:?}");
    assert!(config.is_acknowledged("http://127.0.0.1:9"), "the valid address is kept once");
}

// ---- the address rule, names and bounds -----------------------------------------------------------------

#[test]
fn base_addresses_are_checked_at_the_settings_boundary_with_stable_reasons() {
    let rows: [(&str, bool, &str); 14] = [
        ("https://169.254.169.254", false, "metadata"),
        ("https://[fe80::1]/", false, "link_local"),
        ("https://user:pw@h.example", false, "credentials"),
        ("https://h.example/?a=1", false, "query"),
        ("ftp://h.example", false, "scheme"),
        ("http://api.example.com", false, "plain_http"),
        ("http://192.168.1.5:1234", false, "plain_http"),
        ("", false, "needs_address"),
        ("   ", false, "needs_address"),
        ("https://h.example/{account}/x", false, "needs_address"),
        ("https://h.example/x}", false, "needs_address"),
        ("https://h.example/%7bx", false, "needs_address"),
        ("https://h.example/%7Dx", false, "needs_address"),
        ("https://[fe80::1]/{x}", false, "needs_address"),
    ];
    for (address, lan, reason) in rows {
        let mut config = AiConfig::default();
        let mut form = input(address);
        form.allow_lan_http = lan;
        let error = config.upsert(form).expect_err(address);
        assert_eq!(error.reason(), reason, "{address:?}: {error}");
        assert!(config.profiles().is_empty(), "{address:?}: nothing is kept");
    }

    // Every refusal of the chokepoint keeps its own stable name.
    let mut config = AiConfig::default();
    let long = format!("https://h.example/{}", "a".repeat(2049 - "https://h.example/".len()));
    assert_eq!(long.chars().count(), 2049);
    let error = config.upsert(input(&long)).expect_err("too long");
    assert_eq!(error, ProfileError::Address(Refusal::TooLong));
    assert_eq!(error.reason(), "too_long");

    // The opt-in lets plain http reach a local-network address, and nothing else.
    let mut form = input("http://192.168.1.5:1234");
    form.allow_lan_http = true;
    assert_eq!(config.upsert(form).expect("allowed with the opt-in"), "local");
    assert!(config.get("local").expect("kept").allow_lan_http);

    // The reason of an address error is a refusal name, and no text holds the input.
    let hostile = format!("ftp://{}", "h".repeat(294));
    assert_eq!(hostile.chars().count(), 300);
    let error = AiConfig::default().upsert(input(&hostile)).expect_err("a scheme");
    for text in [error.to_string(), format!("{error:?}"), error.reason().to_owned()] {
        assert!(!text.contains("hhhhhhhh"), "the error repeats the input: {text}");
    }
    for refusal in Refusal::ALL {
        assert_eq!(ProfileError::Address(refusal).reason(), refusal.as_str());
    }
}

#[test]
fn ids_names_and_models_are_bounded_and_the_reserved_id_is_refused() {
    let mut config = AiConfig::default();
    let base = "http://127.0.0.1:1234/v1";

    // Ids: 1 to 40 characters of a-z 0-9 - _.
    let too_long = "a".repeat(MAX_ID + 1);
    for bad in ["", "UPPER", "a b", "ä", "a.b", "a/b", too_long.as_str()] {
        let mut form = input(base);
        form.id = Some(bad.to_owned());
        assert_eq!(config.upsert(form), Err(ProfileError::Id), "{bad:?}");
    }
    let mut form = input(base);
    form.id = Some("apple".to_owned());
    assert_eq!(config.upsert(form), Err(ProfileError::ReservedId));
    let mut form = input(base);
    form.id = Some("valid-id_1".to_owned());
    assert_eq!(config.upsert(form), Err(ProfileError::UnknownProfile), "a missing id is not a new profile");

    // Names: 1 to 80 characters, no control character.
    for bad in ["", "   ", "a\nb", "a\u{7f}b", &"n".repeat(MAX_NAME + 1)] {
        let mut form = input(base);
        form.name = bad.to_owned();
        assert_eq!(config.upsert(form), Err(ProfileError::Name), "{bad:?}");
    }
    let mut form = input(base);
    form.name = "n".repeat(MAX_NAME);
    assert!(config.upsert(form).is_ok(), "80 characters are fine");

    // Models: at most 200 characters, no control character, empty is fine.
    for bad in ["a\tb", &"m".repeat(MAX_MODEL + 1)] {
        let mut form = input(base);
        form.model = bad.to_owned();
        assert_eq!(config.upsert(form), Err(ProfileError::Model), "{bad:?}");
    }
    let mut form = input(base);
    form.name = "Empty model".to_owned();
    form.model = String::new();
    assert!(config.upsert(form).is_ok());
    let mut form = input(base);
    form.name = "Long model".to_owned();
    form.model = "m".repeat(MAX_MODEL);
    assert!(config.upsert(form).is_ok());

    // A wire the preset does not speak, and a change of preset.
    let mut form = ProfileInput::new("Claude", Preset::Anthropic);
    form.base_url = "https://api.example.test".to_owned();
    form.wire = Some(Wire::Ollama);
    assert_eq!(config.upsert(form), Err(ProfileError::Wire));
    let mut form = input(base);
    form.id = Some("empty-model".to_owned());
    form.preset = Preset::LmStudio;
    assert_eq!(config.upsert(form), Err(ProfileError::PresetChange));

    // Fresh ids.
    let mut config = AiConfig::default();
    let mut id_for = |name: &str| {
        let mut form = input(base);
        form.name = name.to_owned();
        config.upsert(form).expect(name)
    };
    assert_eq!(id_for("My Local Server!"), "my-local-server");
    assert_eq!(id_for("My Local Server!"), "my-local-server-2");
    assert_eq!(id_for("My Local Server!"), "my-local-server-3");
    assert_eq!(id_for("???"), "provider");
    assert_eq!(id_for("Apple"), "apple-2", "the reserved id is never handed out");
    let long_id = id_for(&"x".repeat(MAX_NAME));
    assert!(long_id.len() <= MAX_ID - 3, "room is left for a suffix: {long_id}");
    assert_eq!(id_for(&"x".repeat(MAX_NAME)), format!("{long_id}-2"));
    assert!(id_for("  Trim  ").starts_with("trim"));

    // The count: a 33rd new profile is refused while an existing one can still be updated.
    let mut config = AiConfig::default();
    for number in 0..MAX_PROFILES {
        let mut form = input(base);
        form.name = format!("P{number}");
        config.upsert(form).expect("within the limit");
    }
    let mut form = input(base);
    form.name = "One too many".to_owned();
    assert_eq!(config.upsert(form), Err(ProfileError::TooMany));
    let mut form = input(base);
    form.id = Some("p0".to_owned());
    form.name = "Renamed".to_owned();
    assert_eq!(config.upsert(form), Ok("p0".to_owned()), "an update is not a new profile");
    assert_eq!(config.get("p0").expect("kept").name, "Renamed");
    assert_eq!(config.profiles().len(), MAX_PROFILES);
}

// ---- keys -----------------------------------------------------------------------------------------------

#[test]
fn a_key_lives_only_in_the_secret_store_and_never_in_a_file_a_format_or_an_error() {
    let dir = common::scratch_dir("ai-profiles-canary");
    let fake = hello_server();
    let store = CountingStore::new();
    let vault = KeyVault::new(store.clone());
    let mut config = AiConfig::default();
    let id = config.upsert(input(&format!("{}/v1", fake.base()))).expect("a profile");
    config.set_key(&vault, &id, KeySlot::Provider, key(CANARY)).expect("a key");
    config.save(&dir).expect("saved");
    let (loaded, status) = AiConfig::load(&dir);
    send(&loaded, &id, &vault).expect("resolves").result.expect("the send works");

    assert_eq!(scan_dir_for(&dir, CANARY), 0, "the key is in a file");
    assert_eq!(store.entries().iter().filter(|(_, secret)| secret.contains(CANARY)).count(), 1, "one entry holds it");
    assert!(!config.to_json().contains(CANARY));

    let profile = loaded.get(&id).expect("the profile");
    let resolved = loaded.resolve(&id, &vault, false).expect("resolves");
    let removal = Removal { removed: true, key_deleted: true };
    let mut lines = vec![
        shown(&config),
        shown(&loaded),
        shown(profile),
        shown(&input("http://127.0.0.1:1")),
        shown(&vault),
        shown(&resolved),
        shown(&removal),
        shown(&status),
        shown(&vault.status(profile, KeySlot::Provider)),
        serde_json::to_string(profile).expect("serialise"),
    ];

    // Every error type, produced while the key is in the store; the rejected key is the canary itself.
    let mut other = loaded.clone();
    let bad =
        other.set_key(&vault, &id, KeySlot::Provider, key(&format!("{CANARY} with a space"))).expect_err("invalid");
    assert!(matches!(bad, KeyError::Invalid(_)));
    let missing = other.set_key(&vault, "nobody", KeySlot::Provider, key(CANARY)).expect_err("unknown");
    let no_slot = other.set_key(&vault, &id, KeySlot::Gateway, key(CANARY)).expect_err("no such slot");
    let unavailable = other.set_key(&KeyVault::unavailable(), &id, KeySlot::Provider, key(CANARY)).expect_err("none");
    let failing = KeyVault::new(Arc::new(FailingStore::default()));
    let failed = other.set_key(&failing, &id, KeySlot::Provider, key(CANARY)).expect_err("fails");
    for error in [bad, missing, no_slot, unavailable, failed] {
        lines.push(shown(&error));
        lines.push(error.to_string());
    }
    store.plant("ai:local", &format!("{{\"v\":2,\"scope\":\"http://127.0.0.1:1\",\"key\":\"{CANARY}\"}}"));
    let damaged = loaded.resolve(&id, &vault, false).expect_err("damaged");
    store.plant("ai:local", &format!("{{\"v\":1,\"scope\":\"http://127.0.0.1:1\",\"key\":\"{CANARY}\"}}"));
    let scope = loaded.resolve(&id, &vault, false).expect_err("another address");
    for error in [damaged, scope, ResolveError::Keychain, ResolveError::NoSecureStore] {
        lines.push(shown(&error));
        lines.push(error.to_string());
        lines.push(shown(&AiError::from(error)));
    }
    lines.push(shown(&vault.status(profile, KeySlot::Provider)));
    for (index, line) in lines.iter().enumerate() {
        assert!(!line.contains(CANARY), "the key is in output #{index}: {line}");
    }
    fs::remove_dir_all(&dir).expect("remove scratch directory");
}

#[test]
fn without_a_secret_store_a_key_cannot_be_saved_and_a_keyless_profile_still_works() {
    let dir = common::scratch_dir("ai-profiles-nostore");
    let fake = hello_server();
    let base = format!("{}/v1", fake.base());

    // No store at all.
    let mut config = AiConfig::default();
    let id = config.upsert(input(&base)).expect("a keyless profile");
    let vault = KeyVault::unavailable();
    assert!(!vault.is_available());
    assert_eq!(config.set_key(&vault, &id, KeySlot::Provider, key(CANARY)), Err(KeyError::NoSecureStore));
    assert!(!config.get(&id).expect("profile").key_set, "no marker without a key");
    send(&config, &id, &vault).expect("a keyless profile resolves without a store").result.expect("and works");

    // A profile that says it has a key, with no store to hold it.
    let text = {
        let profile = serde_json::to_value(Profile {
            id: "claude".to_owned(),
            name: "Claude".to_owned(),
            preset: Preset::Anthropic,
            wire: Wire::Anthropic,
            base_url: "https://api.example.test".to_owned(),
            model: String::new(),
            allow_lan_http: false,
            key_set: true,
            gateway_key_set: false,
        })
        .expect("a profile");
        document(vec![profile])
    };
    let (marked, status) = AiConfig::from_json(&text);
    assert_eq!(status, LoadStatus::Clean);
    let profile = marked.get("claude").expect("claude");
    assert_eq!(vault.status(profile, KeySlot::Provider), KeyStatus::NoSecureStore);
    assert_eq!(marked.resolve("claude", &vault, false).expect_err("needs a key").as_str(), "no_secure_store");

    // A store whose calls all fail: nothing is saved, nowhere, and a second try fails the same way.
    let failing = Arc::new(FailingStore::default());
    let broken = KeyVault::new(failing.clone());
    assert!(broken.is_available());
    assert_eq!(config.set_key(&broken, &id, KeySlot::Provider, key(CANARY)), Err(KeyError::Store));
    assert_eq!(config.set_key(&broken, &id, KeySlot::Provider, key(CANARY)), Err(KeyError::Store));
    assert_eq!(failing.calls.load(Ordering::SeqCst), 2, "each try asked the store and nothing else");
    assert!(!config.get(&id).expect("profile").key_set);
    config.save(&dir).expect("saved");
    assert_eq!(scan_dir_for(&dir, CANARY), 0, "no file holds the key");
    assert_eq!(broken.status(marked.get("claude").expect("claude"), KeySlot::Provider), KeyStatus::Unreadable);
    assert_eq!(marked.resolve("claude", &broken, false).expect_err("fails").as_str(), "keychain");

    // A keyless profile never touches the store, whatever the store does.
    let before = failing.calls.load(Ordering::SeqCst);
    send(&config, &id, &broken).expect("resolves with a failing store").result.expect("works");
    assert_eq!(failing.calls.load(Ordering::SeqCst), before, "a keyless profile did not touch the failing store");
    let counting = CountingStore::new();
    send(&config, &id, &KeyVault::new(counting.clone())).expect("resolves").result.expect("works");
    assert_eq!(counting.calls(), 0, "a keyless profile did not touch the store");
    fs::remove_dir_all(&dir).expect("remove scratch directory");
}

#[test]
fn a_key_that_cannot_be_part_of_a_header_is_refused_and_one_with_edge_white_space_is_trimmed() {
    let store = CountingStore::new();
    let vault = KeyVault::new(store.clone());
    let mut config = AiConfig::default();
    let id = config.upsert(input("http://127.0.0.1:1234/v1")).expect("a profile");

    let too_long = "k".repeat(MAX_KEY_BYTES + 1);
    let quotes = "\"".repeat(MAX_KEY_BYTES);
    let bad = [
        "",
        "   \t\n",
        "ab\ncd",
        "ab\rcd",
        "ab cd",
        "ab\u{e9}cd",
        "ab\u{7f}cd",
        "ab\0cd",
        "ab\u{2028}cd",
        too_long.as_str(),
    ];
    for text in bad {
        let error = config.set_key(&vault, &id, KeySlot::Provider, key(text)).expect_err(text);
        assert!(matches!(error, KeyError::Invalid(_)), "{text:?}: {error:?}");
        assert!(!error.to_string().is_empty());
    }
    // A key whose record would not fit the store (every character is escaped) is refused as well.
    assert!(matches!(config.set_key(&vault, &id, KeySlot::Provider, key(&quotes)), Err(KeyError::Invalid(_))));
    assert_eq!(store.calls(), 0, "nothing reached the store");
    assert!(store.entries().is_empty());
    assert!(!config.get(&id).expect("profile").key_set);

    // Edge white space is trimmed before storing.
    config.set_key(&vault, &id, KeySlot::Provider, key("  \t sk-value \n")).expect("trimmed");
    let entry = &store.entries()[0].1;
    let record: Value = serde_json::from_str(entry).expect("a record");
    assert_eq!(record["key"], json!("sk-value"));

    // A key of exactly the limit is accepted and its record stays under the limit of the store.
    let exact = "k".repeat(MAX_KEY_BYTES);
    config.set_key(&vault, &id, KeySlot::Provider, key(&exact)).expect("the longest key");
    let entry = &store.entries()[0].1;
    assert!(entry.len() < holzwave_core::secrets::MAX_LEN, "the record is {} bytes", entry.len());
    assert_eq!(store.entries().len(), 1, "the new key replaced the old one");
}

#[test]
fn a_damaged_key_record_stops_the_send() {
    let fake = hello_server();
    let store = CountingStore::new();
    let vault = KeyVault::new(store.clone());
    let mut config = AiConfig::default();
    let id = config.upsert(input(&format!("{}/v1", fake.base()))).expect("a profile");
    config.set_key(&vault, &id, KeySlot::Provider, key(CANARY)).expect("a key");
    let scope = format!("http://127.0.0.1:{}", fake.port());

    let records = [
        "not a record".to_owned(),
        "[]".to_owned(),
        format!(r#"{{"v":2,"scope":"{scope}","key":"k"}}"#),
        format!(r#"{{"v":1,"scope":"{scope}","key":"k","extra":1}}"#),
        r#"{"v":1,"scope":"ftp://h:1","key":"k"}"#.to_owned(),
        r#"{"v":1,"scope":"not a scope","key":"k"}"#.to_owned(),
        format!(r#"{{"v":1,"scope":"{scope}","key":""}}"#),
        format!(r#"{{"v":1,"scope":"{scope}"}}"#),
        String::new(),
    ];
    for record in records {
        store.plant("ai:local", &record);
        let profile = config.get(&id).expect("the profile");
        assert_eq!(vault.status(profile, KeySlot::Provider), KeyStatus::Damaged, "{record}");
        assert_eq!(config.resolve(&id, &vault, false).expect_err(&record), ResolveError::KeyDamaged(KeySlot::Provider));
    }
    assert_eq!(fake.accepted(), 0, "nothing was sent");
    let error: AiError = ResolveError::KeyDamaged(KeySlot::Provider).into();
    assert!(matches!(error, AiError::Auth(_)), "{error:?}");
}

#[test]
fn a_required_key_that_was_never_set_stops_the_send_without_touching_the_store() {
    let store = CountingStore::new();
    let vault = KeyVault::new(store.clone());
    let mut config = AiConfig::default();
    let mut claude = ProfileInput::new("Claude", Preset::Anthropic);
    claude.base_url = "https://api.example.test".to_owned();
    let id = config.upsert(claude).expect("a profile");

    let error = config.resolve(&id, &vault, false).expect_err("a key is needed");
    assert_eq!(error, ResolveError::KeyMissing(KeySlot::Provider));
    assert_eq!(error.as_str(), "key_missing");
    assert_eq!(store.calls(), 0, "no key was ever set, so the store was not asked");
    assert!(matches!(AiError::from(error), AiError::Auth(_)));
    assert_eq!(vault.status(config.get(&id).expect("profile"), KeySlot::Provider), KeyStatus::NotSet);
    assert_eq!(store.calls(), 0);

    // A marker set by hand with no entry behind it.
    let mut profile = serde_json::to_value(config.get(&id).expect("profile")).expect("a value");
    profile["key_set"] = json!(true);
    let (marked, status) = AiConfig::from_json(&document(vec![profile]));
    assert_eq!(status, LoadStatus::Clean);
    assert_eq!(vault.status(marked.get(&id).expect("profile"), KeySlot::Provider), KeyStatus::Missing);
    assert_eq!(marked.resolve(&id, &vault, false).expect_err("no entry"), ResolveError::KeyMissing(KeySlot::Provider));
}

#[test]
fn removing_a_profile_removes_its_keys() {
    let store = CountingStore::new();
    let vault = KeyVault::new(store.clone());
    let mut config = AiConfig::default();
    let id = config.upsert(input("http://127.0.0.1:1234/v1")).expect("a profile");
    config.set_key(&vault, &id, KeySlot::Provider, key(CANARY)).expect("a key");
    assert_eq!(store.entries().len(), 1);

    assert_eq!(config.remove(&id, &vault), Removal { removed: true, key_deleted: true });
    assert!(config.get(&id).is_none(), "the profile is gone");
    assert!(store.entries().is_empty(), "ai:local is gone from the store");

    // A keychain that fails does not keep the profile, and the result says the key may be left behind.
    let id = config.upsert(input("http://127.0.0.1:1234/v1")).expect("a profile");
    config.set_key(&vault, &id, KeySlot::Provider, key(CANARY)).expect("a key");
    let failing = KeyVault::new(Arc::new(FailingStore::default()));
    assert_eq!(config.remove(&id, &failing), Removal { removed: true, key_deleted: false });
    assert!(config.get(&id).is_none());

    // An unknown id; a profile that never had a key does not touch the store.
    assert_eq!(config.remove("nobody", &vault), Removal { removed: false, key_deleted: false });
    let id = config.upsert(input("http://127.0.0.1:1234/v1")).expect("a profile");
    let calls = store.calls();
    assert_eq!(config.remove(&id, &vault), Removal { removed: true, key_deleted: true });
    assert_eq!(store.calls(), calls, "a keyless profile left the store alone");

    // Clearing a key leaves the profile.
    let id = config.upsert(input("http://127.0.0.1:1234/v1")).expect("a profile");
    config.set_key(&vault, &id, KeySlot::Provider, key(CANARY)).expect("a key");
    config.clear_key(&vault, &id, KeySlot::Provider).expect("cleared");
    assert!(config.get(&id).is_some() && !config.get(&id).expect("profile").key_set);
    assert!(store.entries().is_empty());
    assert_eq!(config.clear_key(&vault, "nobody", KeySlot::Provider), Err(KeyError::UnknownProfile));
    assert_eq!(config.clear_key(&vault, &id, KeySlot::Gateway), Err(KeyError::NoSuchSlot));
}

// ---- quiet construction ---------------------------------------------------------------------------------

#[test]
fn loading_saving_and_resolving_open_no_socket() {
    let dir = common::scratch_dir("ai-profiles-quiet");
    let fake = hello_server();
    let store = CountingStore::new();
    let vault = KeyVault::new(store.clone());
    assert_eq!(store.calls(), 0, "constructing a vault makes no store call");

    let mut config = AiConfig::default();
    let id = config.upsert(input(&format!("{}/v1", fake.base()))).expect("a profile");
    config.save(&dir).expect("saved");
    let (loaded, _) = AiConfig::load(&dir);
    let resolved = loaded.resolve(&id, &vault, false).expect("resolves");
    let provider = resolved.into_provider(Box::new(EchoAdapter::new(Wire::OpenAiChat)), http());
    assert_eq!(provider.id(), "local");

    assert!(!fake.wait_accepted(1, Duration::from_millis(300)), "something connected to the server");
    assert_eq!(fake.accepted(), 0);
    assert_eq!(store.calls(), 0, "a keyless profile never touches the store");
    fs::remove_dir_all(&dir).expect("remove scratch directory");
}
