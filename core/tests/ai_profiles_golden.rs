//! The golden of the provider dialog: the page keeps its own copy of the provider table, the refusal names, the key states
//! and the members of a profile view, and this test writes the real ones to `app/tests/fixtures/ai_profiles.json` so that a
//! renamed preset, reason or member fails here and in the page's unit tests in the same commit.
//!
//! The document is built from the real tables and from a configuration made in this test over a keychain in memory. No
//! network, no engine. `UPDATE_FIXTURES=1` rewrites the file; without it the file must equal what is built now.

use std::path::PathBuf;
use std::sync::Arc;

use holzwave_core::ai::http::Refusal;
use holzwave_core::ai::profiles::{
    AiConfig, DisclosureKind, KeyNeed, KeySlot, KeyStatus, KeyVault, PRESETS, Preset, ProfileError, ProfileInput,
    auth_header_for,
};
use holzwave_core::ai::types::ApiKey;
use holzwave_core::ai::wire::Wire;
use holzwave_core::secrets::MemoryStore;
use serde_json::{Map, Value, json};

/// The key of the first keyed profile. It appears in this file only: a hit anywhere else is a leak.
const CANARY: &str = "zq-golden-key-6c41e9b7";

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../app/tests/fixtures/ai_profiles.json")
}

fn update_fixtures() -> bool {
    std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
}

/// Every key state, in declaration order. The match has no wildcard arm, so a state the core adds does not compile here
/// until the golden (and with it the page) knows about it.
fn key_states() -> [KeyStatus; 7] {
    let all = [
        KeyStatus::NotSet,
        KeyStatus::Present,
        KeyStatus::Missing,
        KeyStatus::WrongAddress,
        KeyStatus::Damaged,
        KeyStatus::Unreadable,
        KeyStatus::NoSecureStore,
    ];
    for state in all {
        match state {
            KeyStatus::NotSet
            | KeyStatus::Present
            | KeyStatus::Missing
            | KeyStatus::WrongAddress
            | KeyStatus::Damaged
            | KeyStatus::Unreadable
            | KeyStatus::NoSecureStore => {}
        }
    }
    all
}

/// The profile errors that name a reason of their own, in declaration order. The one that wraps a refusal is left out:
/// its reason is the refusal's, listed under `refusals`.
fn profile_errors() -> [ProfileError; 9] {
    let all = [
        ProfileError::Id,
        ProfileError::ReservedId,
        ProfileError::Name,
        ProfileError::Model,
        ProfileError::Wire,
        ProfileError::PresetChange,
        ProfileError::NeedsAddress,
        ProfileError::TooMany,
        ProfileError::UnknownProfile,
    ];
    for error in all {
        match error {
            ProfileError::Id
            | ProfileError::ReservedId
            | ProfileError::Name
            | ProfileError::Model
            | ProfileError::Wire
            | ProfileError::PresetChange
            | ProfileError::NeedsAddress
            | ProfileError::TooMany
            | ProfileError::UnknownProfile
            | ProfileError::Address(_) => {}
        }
    }
    all
}

fn presets() -> Value {
    assert!(PRESETS.iter().zip(Preset::ALL).all(|(spec, preset)| spec.preset == preset), "the table follows Preset::ALL");
    let rows: Vec<Value> = Preset::ALL
        .iter()
        .map(|preset| {
            let spec = preset.spec();
            let mut address = Map::new();
            for wire in spec.wires {
                let rule = match spec.base_for(*wire) {
                    Some(base) if !base.contains('{') => "default",
                    _ => "required",
                };
                address.insert(wire.tag().to_owned(), json!(rule));
            }
            json!({
                "tag": preset.tag(),
                "label": spec.label,
                "wires": spec.wires.iter().map(|wire| wire.tag()).collect::<Vec<_>>(),
                "address": address,
                "key": match spec.key {
                    KeyNeed::Optional => "optional",
                    KeyNeed::Required => "required",
                },
                "gateway_token": spec.gateway_token,
                "disclosure": spec.disclosure.map(|kind| match kind {
                    DisclosureKind::Gateway => "gateway",
                    DisclosureKind::ThirdParty => "third_party",
                }),
            })
        })
        .collect();
    Value::Array(rows)
}

fn key(text: &str) -> ApiKey {
    ApiKey::new(text.to_owned())
}

/// A profile of `preset` named `name` on `wire` at `base` (empty: the default address of the type).
fn input(name: &str, preset: Preset, wire: Option<Wire>, base: &str) -> ProfileInput {
    let mut input = ProfileInput::new(name, preset);
    input.wire = wire;
    input.base_url = base.to_owned();
    input
}

/// The five profile views of one configuration made over a keychain in memory.
fn views() -> Value {
    let vault = KeyVault::new(Arc::new(MemoryStore::default()));
    let mut config = AiConfig::default();

    // A local server without a key.
    let local = config.upsert(input("Local", Preset::Ollama, None, "")).expect("an Ollama profile");

    // A named provider with a key.
    let claude = config.upsert(input("Claude", Preset::Anthropic, None, "")).expect("an Anthropic profile");
    config.set_key(&vault, &claude, KeySlot::Provider, key(CANARY)).expect("the first key");

    // A gateway with a key whose address is not acknowledged: its view carries a disclosure.
    let router = config.upsert(input("Router", Preset::OpenRouter, None, "")).expect("an OpenRouter profile");
    config.set_key(&vault, &router, KeySlot::Provider, key("zq-golden-router-1")).expect("the router key");

    // A gateway with both keys, an address whose two template segments are filled in, and the acknowledgement.
    let mut edge_input = input(
        "Edge",
        Preset::CloudflareGateway,
        Some(Wire::Anthropic),
        "https://gateway.ai.cloudflare.com/v1/account/gateway/anthropic",
    );
    edge_input.acknowledge_gateway = true;
    let edge = config.upsert(edge_input).expect("a Cloudflare profile");
    config.set_key(&vault, &edge, KeySlot::Provider, key("zq-golden-edge-1")).expect("the edge key");
    config.set_key(&vault, &edge, KeySlot::Gateway, key("zq-golden-edge-2")).expect("the gateway token");

    // A server of a third party with a key, whose address then changes: the key was filed for the old one.
    let moved = config
        .upsert(input("Moved", Preset::Custom, Some(Wire::OpenAiChat), "https://llm.example.org/v1"))
        .expect("a Custom profile");
    config.set_key(&vault, &moved, KeySlot::Provider, key("zq-golden-moved-1")).expect("the moved key");
    let mut again = input("Moved", Preset::Custom, Some(Wire::OpenAiChat), "https://other.example.net/v1");
    again.id = Some(moved.clone());
    config.upsert(again).expect("the address changes");

    assert_eq!(local, "local");
    let views = config.views(&vault);
    assert_eq!(views.len(), 5);
    serde_json::to_value(&views).expect("the views are JSON")
}

/// `value` with the members of every object in sorted order.
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().map(|(name, item)| (name, sorted(item))).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(list) => Value::Array(list.into_iter().map(sorted).collect()),
        other => other,
    }
}

fn document() -> Value {
    sorted(json!({
        "key_states": key_states().iter().map(|state| state.as_str()).collect::<Vec<_>>(),
        "presets": presets(),
        "profile_reasons": profile_errors().iter().map(|error| error.reason()).collect::<Vec<_>>(),
        "refusals": Refusal::ALL.iter().map(|refusal| refusal.as_str()).collect::<Vec<_>>(),
        "views": views(),
    }))
}

fn text_of(document: &Value) -> String {
    let mut text = serde_json::to_string_pretty(document).expect("the document is JSON");
    text.push('\n');
    text
}

#[test]
fn the_page_golden_matches_the_preset_table_and_the_profile_views() {
    let first = document();
    let second = document();
    assert_eq!(first, second, "the document is built the same way twice");

    let text = text_of(&first);
    assert!(!text.contains(CANARY), "the key is in the golden");
    for wire in Wire::ALL {
        let (header, _) = auth_header_for(wire);
        assert!(!text.to_ascii_lowercase().contains(header), "the header name {header} is in the golden");
    }

    let object = first.as_object().expect("the document is an object");
    let members: Vec<&str> = object.keys().map(String::as_str).collect();
    assert_eq!(members, ["key_states", "presets", "profile_reasons", "refusals", "views"]);
    let count = |name: &str| object[name].as_array().map_or(0, Vec::len);
    assert_eq!(count("presets"), 11);
    assert_eq!(count("refusals"), 18);
    assert_eq!(count("key_states"), 7);
    assert_eq!(count("views"), 5);
    assert_eq!(count("profile_reasons"), 9);

    // The states the views were built to show, so a change in the core's rules is seen here and not only as a diff.
    let views = object["views"].as_array().expect("views");
    let state = |at: usize, member: &str| views[at][member].as_str().map(str::to_owned);
    assert_eq!(state(0, "key").as_deref(), Some("not_set"));
    assert_eq!(state(1, "key").as_deref(), Some("present"));
    assert_eq!(state(2, "key").as_deref(), Some("present"));
    assert_eq!(state(3, "key").as_deref(), Some("present"));
    assert_eq!(state(4, "key").as_deref(), Some("wrong_address"));
    assert_eq!(state(3, "gateway_key").as_deref(), Some("present"));
    assert!(views[0]["disclosure"].is_null() && views[1]["disclosure"].is_null() && views[3]["disclosure"].is_null());
    assert_eq!(views[2]["disclosure"]["kind"], "gateway");
    assert_eq!(views[4]["disclosure"]["kind"], "third_party");
    assert_eq!(views[4]["disclosure"]["host"], "other.example.net");

    let path = golden_path();
    if update_fixtures() {
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the fixtures directory");
        std::fs::write(&path, text).expect("write ai_profiles.json");
        return;
    }
    let on_disk = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is missing ({e}); run with UPDATE_FIXTURES=1", path.display()));
    assert!(
        on_disk == text,
        "ai_profiles.json differs from the tables of the core; review the change and run with UPDATE_FIXTURES=1"
    );
}
