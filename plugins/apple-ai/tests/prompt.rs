//! What reaches the model, and how much of it: the budget, the estimate, the rendering of the approved envelope and of the
//! history, the trimming, the system text cap, the reply limit, the capabilities and the advice per availability state.

mod common;

use std::cell::Cell;
use std::sync::Arc;

use holzwave_apple_ai::{
    Advice, AppleProvider, Availability, BackendScript, Budget, Estimator, FALLBACK_CONTEXT_TOKENS, Rendered,
    ScriptedBackend, estimate_tokens, render,
};
use holzwave_core::ai::provider::{AiError, CancelToken, ChatRequest, Provider, StreamEvent};
use holzwave_core::ai::types::{ContextNote, Message, Opaque, Part, Role, ToolCall, ToolResult, Usage};

use common::{approved, history, run, user_request, with_context};

fn no_exact(_: &str) -> Option<u32> {
    None
}

fn roomy() -> Budget {
    Budget::for_context(4096)
}

fn rendered(req: &ChatRequest) -> Rendered {
    render(req, &roomy(), &no_exact).expect("the request renders")
}

#[test]
fn the_budget_for_a_4096_window_reserves_reply_and_frame() {
    assert_eq!(Budget::for_context(4096), Budget { context: 4096, reply: 1024, margin: 128, input: 2944, system: 300 });
    let large = Budget::for_context(8192);
    assert_eq!((large.reply, large.input, large.system), (1024, 7040, 300));
    assert_eq!(FALLBACK_CONTEXT_TOKENS, 4096);
}

#[test]
fn an_unknown_window_uses_the_documented_size() {
    let unknown = AppleProvider::new(Arc::new(ScriptedBackend::new().with_context_size(None)));
    assert_eq!(unknown.input_budget_tokens(), 2944);
    assert_eq!(unknown.capabilities("system").context_tokens, Some(4096));

    let known = AppleProvider::new(Arc::new(ScriptedBackend::new().with_context_size(Some(8192))));
    assert_eq!(known.input_budget_tokens(), 7040);
}

#[test]
fn a_window_too_small_for_any_input_leaves_no_room() {
    let small = Budget::for_context(512);
    assert_eq!((small.reply, small.margin, small.input, small.system), (256, 128, 128, 32));
    assert_eq!(Budget::for_context(384).input, 0);
    assert_eq!(Budget::for_context(0).input, 0);
    assert_eq!(Budget::for_context(0).system, 0);

    // With no room even a one-word message is refused, and before any backend call.
    let err = render(&user_request("", "hi there"), &Budget::for_context(384), &no_exact).unwrap_err();
    assert_eq!(err, AiError::ContextLength);
}

#[test]
fn the_estimate_is_conservative_for_ascii_and_counts_other_scalars_once() {
    assert_eq!(estimate_tokens(""), 0);
    assert_eq!(estimate_tokens("a"), 1);
    assert_eq!(estimate_tokens("abcdef"), 2);
    assert_eq!(estimate_tokens("0123456789"), 4);
    assert_eq!(estimate_tokens("\u{65e5}\u{672c}\u{8a9e}"), 3);
    assert_eq!(estimate_tokens("a\u{e9}"), 2);

    // Apple counts three to four characters per token for English: the estimate never goes below the four-per-token mark.
    let english = "The quick brown fox jumps over the lazy dog while the build runs in the other pane.";
    assert!(estimate_tokens(english) >= english.chars().count().div_ceil(4) as u32);

    // Text that arrives in pieces is estimated like the whole.
    let mut pieces = Estimator::new();
    for piece in ["The quick brown ", "fox j\u{fc}mps", " over ", "the lazy dog"] {
        pieces.push(piece);
    }
    assert_eq!(pieces.total(), estimate_tokens("The quick brown fox j\u{fc}mps over the lazy dog"));
}

#[test]
fn a_single_turn_prompt_is_the_user_text_alone() {
    let out = rendered(&user_request("Answer briefly.", "Say hello."));
    assert_eq!(out.instructions, "Answer briefly.");
    assert_eq!(out.prompt, "Say hello.");
}

#[test]
fn earlier_turns_are_labelled_oldest_first_under_a_fixed_heading() {
    let req = history(
        user_request("", "three"),
        &[(Role::User, "one"), (Role::Assistant, "two"), (Role::Tool, "never shown")],
    );
    let out = rendered(&req);
    assert_eq!(out.prompt, "Earlier in this conversation:\nUser: one\nAssistant: two\n\nCurrent message:\nthree");
}

#[test]
fn an_approved_envelope_reaches_the_prompt_byte_for_byte() {
    let context = approved("line one\nCANARY-envelope-4d2a  two spaces\n\tand a tab");
    let envelope = context.envelope().to_owned();
    let req = with_context(user_request("", "what is this?"), context);

    let out = rendered(&req);

    assert!(out.prompt.contains(&envelope), "the envelope is one contiguous substring");
    assert_eq!(out.prompt, format!("{envelope}\nwhat is this?"));
    assert!(!out.instructions.contains("CANARY"));
}

#[test]
fn tool_calls_and_tool_results_never_reach_the_prompt() {
    let mut req = user_request("", "now what?");
    let call = ToolCall { id: "CANARY-call-id".to_owned(), name: "CANARY-tool-name".to_owned(), args: Default::default() };
    let result = ToolResult { call_id: "CANARY-call-id".to_owned(), content: "CANARY-tool-result".to_owned(), is_error: false };
    let mut earlier = Message::new(
        "a1",
        Role::Assistant,
        vec![Part::Text { text: "I will look.".to_owned() }, Part::ToolCall(call.clone())],
        0,
    );
    earlier.native = Some(Opaque { wire: "CANARY-native-wire".to_owned(), json: Default::default() });
    req.messages.insert(0, earlier);
    req.messages.insert(1, Message::new("t1", Role::Tool, vec![Part::ToolResult(result.clone())], 0));
    // Tool parts inside a user message are skipped too, and a message with nothing else is dropped.
    req.messages.insert(2, Message::new("u0", Role::User, vec![Part::ToolResult(result)], 0));
    req.messages.last_mut().unwrap().parts.push(Part::ToolCall(call));

    let out = rendered(&req);

    assert!(!out.prompt.contains("CANARY") && !out.instructions.contains("CANARY"), "{:?}", out);
    assert_eq!(out.prompt, "Earlier in this conversation:\nAssistant: I will look.\n\nCurrent message:\nnow what?");
}

#[test]
fn stored_context_text_is_never_sent() {
    let old = approved("CANARY-old-envelope-1");
    let now = approved("CANARY-now-envelope-2");
    let mut req = user_request("", "and now?");
    // An earlier message kept its text; it is history and the request does not approve it again.
    let kept = ContextNote::for_approved(&old, Some("CANARY-stored-text".to_owned()));
    req.messages.insert(0, Message::new("old", Role::User, vec![Part::Context(kept), Part::Text { text: "see above".to_owned() }], 0));
    // The current message names its own approved context, and its note also holds a stored copy.
    let note = ContextNote::for_approved(&now, Some("CANARY-stored-again".to_owned()));
    req.messages.last_mut().unwrap().parts.insert(0, Part::Context(note));
    let envelope = now.envelope().to_owned();
    req.context.push(now);

    let out = rendered(&req);

    assert!(out.prompt.contains(&envelope));
    for secret in ["CANARY-stored-text", "CANARY-stored-again", "CANARY-old-envelope-1"] {
        assert!(!out.prompt.contains(secret) && !out.instructions.contains(secret), "{secret} was sent");
    }
}

#[test]
fn an_unapproved_context_note_is_one_marker_line() {
    let old = approved("CANARY-old-envelope-9");
    let bytes = old.items()[0].bytes;
    let mut req = user_request("", "continue");
    req.messages.insert(
        0,
        Message::new(
            "old",
            Role::User,
            vec![Part::Context(ContextNote::for_approved(&old, None)), Part::Text { text: "explain".to_owned() }],
            0,
        ),
    );

    let out = rendered(&req);

    let marker = format!("[terminal context from an earlier message is not sent again: selection \"selection\" ({bytes} bytes)]");
    assert_eq!(out.prompt, format!("Earlier in this conversation:\nUser: {marker}\nexplain\n\nCurrent message:\ncontinue"));
    assert!(!out.prompt.contains("CANARY"));
}

#[test]
fn the_oldest_turns_are_dropped_first_and_whole() {
    let tight = Budget { context: 4096, reply: 1024, margin: 128, input: 200, system: 50 };
    let filler = "x".repeat(150);
    let turns: Vec<(Role, String)> = (1..=8)
        .map(|n| (if n % 2 == 1 { Role::User } else { Role::Assistant }, format!("TURN-{n} {filler} END-{n}")))
        .collect();
    let borrowed: Vec<(Role, &str)> = turns.iter().map(|(role, text)| (*role, text.as_str())).collect();
    let req = history(user_request("", "latest"), &borrowed);

    let out = render(&req, &tight, &no_exact).expect("the newest turns fit");

    assert!(estimate_tokens(&out.instructions) + estimate_tokens(&out.prompt) <= tight.input);
    let kept: Vec<usize> = (1..=8).filter(|n| out.prompt.contains(&format!("TURN-{n} "))).collect();
    assert!(!kept.is_empty() && kept.len() < 8, "{kept:?}");
    // The kept turns are the newest ones, without a gap, and each is whole.
    assert_eq!(kept, (9 - kept.len()..=8).collect::<Vec<_>>());
    for n in &kept {
        assert!(out.prompt.contains(&format!("TURN-{n} {filler} END-{n}")), "turn {n} was cut");
    }
    assert!(out.prompt.ends_with("Current message:\nlatest"));

    // At most the 40 newest turns are considered, however large the window is.
    let many: Vec<(Role, String)> = (1..=50).map(|n| (Role::User, format!("T-{n:02}"))).collect();
    let borrowed: Vec<(Role, &str)> = many.iter().map(|(role, text)| (*role, text.as_str())).collect();
    let req = history(user_request("", "latest"), &borrowed);
    let out = render(&req, &Budget::for_context(100_000), &no_exact).unwrap();
    assert!(!out.prompt.contains("T-10") && out.prompt.contains("T-11") && out.prompt.contains("T-50"));
}

#[test]
fn a_last_message_that_cannot_fit_is_a_context_length_error() {
    let long = "a".repeat(20_000);
    let req = history(user_request("", &long), &[(Role::User, "earlier")]);
    assert_eq!(render(&req, &roomy(), &no_exact).unwrap_err(), AiError::ContextLength);

    // An approved envelope is never cut to make it fit.
    let big = approved(&"word ".repeat(4000));
    let with_big = with_context(user_request("", "summarise"), big);
    assert_eq!(render(&with_big, &roomy(), &no_exact).unwrap_err(), AiError::ContextLength);

    // Through the provider the backend is never called.
    let backend = Arc::new(ScriptedBackend::new().with_turn(BackendScript::reply("never")));
    let provider = AppleProvider::new(backend.clone());
    let (result, collector) = run(&provider, &with_big, &CancelToken::new());
    assert_eq!(result.unwrap_err(), AiError::ContextLength);
    assert_eq!(collector.text(), "");
    assert_eq!(backend.calls(), 0);
}

#[test]
fn a_long_system_prompt_is_cut_at_a_line_boundary_keeping_the_head() {
    let system: String = (1..=100).map(|n| format!("Rule {n:03}: keep the answer short and exact.\n")).collect();
    let budget = roomy();
    assert!(estimate_tokens(&system) > budget.system);

    let out = rendered(&user_request(&system, "hi"));

    assert!(system.starts_with(&out.instructions), "the head is kept");
    assert!(system[out.instructions.len()..].starts_with('\n'), "the cut is after a complete line");
    assert!(estimate_tokens(&out.instructions) <= budget.system);
    let next_line_end = system[out.instructions.len() + 1..].find('\n').unwrap() + out.instructions.len() + 2;
    assert!(estimate_tokens(&system[..next_line_end]) > budget.system, "the next line would not have fit");
    assert!(out.instructions.starts_with("Rule 001:"));

    // A system text that fits is unchanged, a trailing line feed included.
    let short = "Answer briefly.\nUse plain words.\n";
    assert_eq!(rendered(&user_request(short, "hi")).instructions, short);
}

#[test]
fn the_exact_count_is_asked_only_when_the_estimate_does_not_fit() {
    let fits = Cell::new(0);
    let counter = |text: &str| {
        fits.set(fits.get() + 1);
        Some(text.chars().count().div_ceil(4) as u32)
    };
    render(&user_request("", "short"), &roomy(), &counter).unwrap();
    assert_eq!(fits.get(), 0, "the estimate fits: no exact count");

    // The estimate (110) says no, the exact count (83) says yes.
    let budget = Budget { context: 4096, reply: 1024, margin: 128, input: 100, system: 25 };
    let text = "a".repeat(330);
    let calls = Cell::new(0);
    let counter = |text: &str| {
        calls.set(calls.get() + 1);
        Some(text.chars().count().div_ceil(4) as u32)
    };
    let out = render(&user_request("", &text), &budget, &counter).expect("the exact count lets it through");
    assert!(calls.get() >= 1);
    assert_eq!(out.input_tokens, 83);
    assert_eq!(render(&user_request("", &text), &budget, &no_exact).unwrap_err(), AiError::ContextLength);

    // Turns are dropped by the exact count: more history fits than the estimate alone would keep.
    let turns: Vec<(Role, String)> = (1..=6).map(|n| (Role::User, format!("T{n} {}", "b".repeat(120)))).collect();
    let borrowed: Vec<(Role, &str)> = turns.iter().map(|(role, text)| (*role, text.as_str())).collect();
    let req = history(user_request("", "now"), &borrowed);
    let wide = Budget { context: 4096, reply: 1024, margin: 128, input: 120, system: 30 };
    let by_estimate = render(&req, &wide, &no_exact).unwrap();
    let by_exact = render(&req, &wide, &counter).unwrap();
    let kept = |out: &Rendered| (1..=6).filter(|n| out.prompt.contains(&format!("T{n} "))).count();
    assert!(kept(&by_exact) > kept(&by_estimate), "{} against {}", kept(&by_exact), kept(&by_estimate));

    // Through the provider and the scripted counter: zero calls for a small request, some for a tight one.
    let small = ScriptedBackend::new().with_exact_counts(true).with_turn(BackendScript::reply("ok"));
    let small = Arc::new(small);
    let small_provider = AppleProvider::new(small.clone());
    assert!(run(&small_provider, &user_request("", "hi"), &CancelToken::new()).0.is_ok());
    assert_eq!(small.count_calls(), 0);

    let tight = ScriptedBackend::new().with_exact_counts(true).with_context_size(Some(600)).with_turn(BackendScript::reply("ok"));
    let tight = Arc::new(tight);
    let tight_provider = AppleProvider::new(tight.clone());
    let (result, _) = run(&tight_provider, &user_request("", &"a".repeat(700)), &CancelToken::new());
    assert!(result.is_ok(), "{result:?}");
    assert!(tight.count_calls() >= 1);
}

#[test]
fn the_reply_limit_is_the_smaller_of_the_request_and_the_budget() {
    let budget = Budget::for_context(4096);
    let limit = |max: Option<u32>| {
        let mut req = user_request("", "hi");
        req.max_output_tokens = max;
        render(&req, &budget, &no_exact).unwrap().max_response_tokens
    };
    assert_eq!(limit(None), 1024);
    assert_eq!(limit(Some(100)), 100);
    assert_eq!(limit(Some(5000)), 1024);
    assert_eq!(limit(Some(0)), 1);
    let mut req = user_request("", "hi");
    req.max_output_tokens = None;
    assert_eq!(render(&req, &Budget::for_context(1000), &no_exact).unwrap().max_response_tokens, 256);

    // The provider hands the limit to the backend.
    let backend = Arc::new(ScriptedBackend::new().with_turn(BackendScript::reply("ok")));
    let provider = AppleProvider::new(backend.clone());
    let mut req = user_request("", "hi");
    req.max_output_tokens = Some(77);
    assert!(run(&provider, &req, &CancelToken::new()).0.is_ok());
    assert_eq!(backend.requests()[0].max_response_tokens, 77);
}

#[test]
fn capabilities_say_no_tools_on_device_with_the_runtime_window() {
    use holzwave_core::ai::types::Locality;
    for (window, context, reply) in [(Some(8192), 8192, 1024), (None, 4096, 1024), (Some(1000), 1000, 256)] {
        let provider = AppleProvider::new(Arc::new(ScriptedBackend::new().with_context_size(window)));
        let caps = provider.capabilities("system");
        assert_eq!(caps.context_tokens, Some(context));
        assert_eq!(caps.max_output_tokens, Some(reply));
        assert!(!caps.tools && !caps.reasoning && !caps.vision);
        assert_eq!(caps.locality, Locality::OnDevice);
    }
}

#[test]
fn advice_covers_every_availability_with_stable_keys() {
    let table = [
        (Availability::Available, Advice::None, "none"),
        (Availability::AppleIntelligenceNotEnabled, Advice::EnableAppleIntelligence, "enable_apple_intelligence"),
        (Availability::ModelNotReady, Advice::WaitForModel, "wait_for_model"),
        (Availability::DeviceNotEligible, Advice::NotSupported, "not_supported"),
        (Availability::UnsupportedPlatform, Advice::NotSupported, "not_supported"),
        (Availability::OsTooOld, Advice::UpdateMacos, "update_macos"),
        (Availability::SdkLacksFramework, Advice::NotInThisBuild, "not_in_this_build"),
        (Availability::Other, Advice::Unknown, "unknown"),
    ];
    for (state, advice, key) in table {
        assert_eq!(state.advice(), advice, "{state:?}");
        assert_eq!(advice.as_str(), key);
    }
    let keys: std::collections::BTreeSet<&str> = table.iter().map(|(_, advice, _)| advice.as_str()).collect();
    assert_eq!(keys.len(), 7);
    assert!(keys.iter().all(|key| key.chars().all(|c| c.is_ascii_lowercase() || c == '_')));
}

#[test]
fn rendered_debug_shows_sizes_never_text() {
    let req = user_request("CANARY-system-3b7e", "CANARY-user-text-81c4");
    let out = rendered(&req);
    for shown in [format!("{out:?}"), format!("{out:#?}")] {
        assert!(!shown.contains("CANARY"), "{shown}");
        assert!(shown.contains("<18 bytes>") && shown.contains("<21 bytes>"), "{shown}");
    }
}

#[test]
fn usage_reports_the_input_count_and_an_output_estimate() {
    let reply = "Hello there, this is the reply.";
    let backend = Arc::new(ScriptedBackend::new().with_turn(BackendScript::new().delta("Hello there, ").delta("this is the reply.")));
    let provider = AppleProvider::new(backend);
    let req = user_request("Answer briefly.", "Say hello.");
    let expected_input = estimate_tokens("Answer briefly.") + estimate_tokens("Say hello.");

    let mut usages = Vec::new();
    let completion = provider
        .stream(
            &req,
            &mut |event| {
                if let StreamEvent::Usage(usage) = event {
                    usages.push(usage);
                }
                holzwave_core::ai::Flow::Continue
            },
            &CancelToken::new(),
        )
        .unwrap();

    assert_eq!(usages.len(), 1);
    assert_eq!(usages[0], Usage { input_tokens: expected_input, output_tokens: estimate_tokens(reply) });
    assert_eq!(completion.usage, *usages.last().unwrap());

    // When the exact count had to decide, the input figure is the exact one.
    let backend = Arc::new(ScriptedBackend::new().with_exact_counts(true).with_context_size(Some(600)).with_turn(BackendScript::reply("ok")));
    let provider = AppleProvider::new(backend);
    let (result, _) = run(&provider, &user_request("", &"a".repeat(700)), &CancelToken::new());
    assert_eq!(result.unwrap().usage.input_tokens, 175);
}
