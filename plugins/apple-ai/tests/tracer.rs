//! The headless tracer: one text turn runs through `&dyn Provider` over the scripted backend into a collector, and every
//! refusal happens before the backend runs. The same path runs through the system model in `tests/live.rs`.

mod common;

use std::sync::Arc;

use holzwave_apple_ai::{AppleProvider, Availability, BackendScript, MODEL_ID, PROVIDER_ID, ScriptedBackend};
use holzwave_core::ai::profiles::RESERVED_IDS;
use holzwave_core::ai::provider::{AiError, CancelToken, Collector, Provider, StreamEvent};
use holzwave_core::ai::types::{Locality, StopReason, ToolSpec, Usage};

use common::{approved_for_provider, run, user_request, with_context};

fn provider_over(backend: ScriptedBackend) -> (AppleProvider, Arc<ScriptedBackend>) {
    let backend = Arc::new(backend);
    (AppleProvider::new(backend.clone()), backend)
}

#[test]
fn a_text_turn_streams_through_the_provider_into_a_collector() {
    let (provider, backend) =
        provider_over(ScriptedBackend::new().with_turn(BackendScript::new().delta("Hel").delta("lo ").delta("there.")));
    let provider: &dyn Provider = &provider;
    let req = user_request("Answer briefly.", "Say hello.");
    let mut events = Vec::new();
    let mut collector = Collector::new();

    let completion = provider
        .stream(
            &req,
            &mut |event| {
                events.push(event.clone());
                collector.push(event)
            },
            &CancelToken::new(),
        )
        .expect("the turn succeeds");

    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, ["Hel", "lo ", "there."]);
    assert_eq!(collector.text(), "Hello there.");
    let usages: Vec<Usage> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::Usage(usage) => Some(*usage),
            _ => None,
        })
        .collect();
    assert_eq!(usages.len(), 1, "exactly one usage event");
    assert!(usages[0].input_tokens > 0 && usages[0].output_tokens > 0, "{:?}", usages[0]);
    assert!(matches!(events.last(), Some(StreamEvent::Usage(_))), "usage comes last");
    assert_eq!(completion.stop_reason, StopReason::EndTurn);
    assert_eq!(completion.usage, usages[0]);
    assert!(completion.native.is_none());

    assert_eq!(backend.calls(), 1);
    let seen = backend.requests();
    assert_eq!(seen[0].instructions, "Answer briefly.");
    assert_eq!(seen[0].prompt, "Say hello.");
}

#[test]
fn a_request_that_declares_tools_is_refused_before_the_backend_runs() {
    let (provider, backend) = provider_over(ScriptedBackend::new().with_turn(BackendScript::reply("never")));
    let mut req = user_request("", "hello");
    req.tools.push(ToolSpec {
        name: "run".to_owned(),
        description: "runs a command".to_owned(),
        input_schema: Default::default(),
    });

    let (result, collector) = run(&provider, &req, &CancelToken::new());

    match result {
        Err(AiError::InvalidRequest(detail)) => assert_eq!(detail.as_str(), "tools_not_supported"),
        other => panic!("expected an invalid request, got {other:?}"),
    }
    assert_eq!(collector.text(), "");
    assert_eq!(backend.calls(), 0);

    let mut other_model = user_request("", "hello");
    other_model.model = "gpt-something".to_owned();
    match run(&provider, &other_model, &CancelToken::new()).0 {
        Err(AiError::InvalidRequest(detail)) => assert_eq!(detail.as_str(), "unknown_model"),
        other => panic!("expected an invalid request, got {other:?}"),
    }
    assert_eq!(backend.calls(), 0);

    // A cancel raised before the call is a cancel and never reaches the backend.
    let cancel = CancelToken::new();
    cancel.cancel();
    assert_eq!(run(&provider, &user_request("", "hello"), &cancel).0, Err(AiError::Cancelled));
    assert_eq!(backend.calls(), 0);
}

#[test]
fn an_unavailable_model_is_refused_before_the_backend_runs() {
    let states = [
        Availability::AppleIntelligenceNotEnabled,
        Availability::DeviceNotEligible,
        Availability::ModelNotReady,
        Availability::Other,
        Availability::OsTooOld,
        Availability::SdkLacksFramework,
        Availability::UnsupportedPlatform,
    ];
    for state in states {
        let (provider, backend) = provider_over(
            ScriptedBackend::new().with_availability(state).with_turn(BackendScript::reply("never")),
        );
        let (result, collector) = run(&provider, &user_request("", "hello"), &CancelToken::new());
        match result {
            Err(AiError::Unavailable(detail)) => assert_eq!(detail.as_str(), state.as_str(), "{state:?}"),
            other => panic!("{state:?}: expected unavailable, got {other:?}"),
        }
        assert_eq!(collector.text(), "");
        assert_eq!(backend.calls(), 0, "{state:?} reached the backend");
        assert_eq!(provider.availability(), state);
    }
}

#[test]
fn a_context_approved_for_another_provider_is_refused_before_the_backend_runs() {
    let (provider, backend) = provider_over(ScriptedBackend::new().with_turn(BackendScript::reply("never")));
    let foreign = approved_for_provider("other", "CANARY-foreign-5521");
    let req = with_context(user_request("", "what is this?"), foreign);

    let (result, collector) = run(&provider, &req, &CancelToken::new());

    assert!(matches!(result, Err(AiError::Policy(_))), "{result:?}");
    assert_eq!(collector.text(), "");
    assert_eq!(backend.calls(), 0);
}

#[test]
fn the_provider_id_is_the_reserved_profile_id() {
    assert_eq!(PROVIDER_ID, "apple");
    assert_eq!(MODEL_ID, "system");
    assert!(RESERVED_IDS.contains(&PROVIDER_ID));
    let (provider, _) = provider_over(ScriptedBackend::new());
    assert_eq!(provider.id(), PROVIDER_ID);
    let caps = provider.capabilities(MODEL_ID);
    assert!(!caps.tools && !caps.reasoning && !caps.vision);
    assert_eq!(caps.locality, Locality::OnDevice);
}
