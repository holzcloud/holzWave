//! Nothing leaves the machine before a send, and nothing secret leaves with one.
//!
//! The service is built the way the shell builds it, over the in-process server, and every observation is made from
//! the outside: what the server accepted and received, what the stored chat holds, what a view says. This is test
//! code only. A failing assertion names the canary or the part (the target, a header name, the body), never the bytes.

mod common;
mod security;

use std::collections::BTreeSet;
use std::fs;
use std::time::Duration;

use holzwave_core::ai::gate::OfferId;
use holzwave_core::ai::redact::{SourceText, prepare};
use holzwave_core::ai::service::SendRequest;
use holzwave_core::ai::types::ContextKind;
use serde_json::Value;

use security::canaries::{api_key, canaries, leaks, marker, terminal_text};
use security::rig::{Entry, PANE, PROFILE, Rig, RigOptions, body_strings, captures, everything, everything_except_key};

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
