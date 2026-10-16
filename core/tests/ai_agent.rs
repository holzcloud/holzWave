//! The agent loop of the AI core, with no network, no terminal and no page.
//!
//! A scripted model and a fake host stand in for the provider and the shell. One `Agent::run_turn` call drives them
//! through the whole path: the model asks for a tool, the approval table proposes it and the loop emits the card, the
//! test plays the user and decides on the card's hash, the table re-checks and hands the host a ticket, the result is
//! cleaned and fed back, and the next round answers. Every wait is bounded, so a broken loop fails an assertion instead
//! of hanging the run.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use holzwave_core::ai::agent::{
    Agent, AgentEvent, Limit, Limits, STOPPED_RESULT, TRIMMED_RESULT, TurnEnd, TurnOutcome, TurnRequest,
    estimate_tokens,
};
use holzwave_core::ai::gate::{Destination, Gate, OfferRequest, RequestItem};
use holzwave_core::ai::provider::{AiError, CancelToken, ChatRequest, Detail};
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::scripted::{Script, ScriptedProvider};
use holzwave_core::ai::tools::{
    ApprovalTable, Decision, DenyPolicy, Pending, Resolved, Scope, Ticket, Tool, ToolError, ToolHost, ToolName,
    ToolOutput,
};
use holzwave_core::ai::store::NOT_STORED;
use holzwave_core::ai::types::{
    Capabilities, Consent, ContextKind, ContextNote, Locality, Message, NoteItem, Opaque, Part, Role, StopReason,
    ToolCall, ToolResult, Usage,
};
use holzwave_core::layout::PaneId;
use serde_json::{Value, json};

const NOTES: &str = "/srv/app/notes.txt";

// ---- the fake host ------------------------------------------------------------------------------------------------

/// One call the fake host executed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ran {
    call_id: String,
    path: Option<String>,
}

struct HostState {
    files: BTreeMap<String, String>,
    failing: BTreeSet<String>,
    available: BTreeSet<ToolName>,
    block: bool,
    ran: Vec<Ran>,
}

/// The shell, as far as the loop can tell: a remote machine called `build box` with a few files.
struct FakeHost {
    state: Mutex<HostState>,
}

impl FakeHost {
    /// A host that offers the four read tools.
    fn new() -> Self {
        let available =
            [ToolName::ReadFile, ToolName::ListDir, ToolName::Grep, ToolName::ReadPane].into_iter().collect();
        Self {
            state: Mutex::new(HostState {
                files: BTreeMap::new(),
                failing: BTreeSet::new(),
                available,
                block: false,
                ran: Vec::new(),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, HostState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn with_file(self, path: &str, text: &str) -> Self {
        self.state().files.insert(path.to_owned(), text.to_owned());
        self
    }

    /// A path whose read fails with a host error.
    fn failing(self, path: &str) -> Self {
        self.state().failing.insert(path.to_owned());
        self
    }

    /// The host offers all seven tools.
    #[allow(dead_code)]
    fn all_tools(self) -> Self {
        self.state().available = ToolName::ALL.into_iter().collect();
        self
    }

    /// `run` waits for the cancel token (at most five seconds) instead of answering.
    #[allow(dead_code)]
    fn blocking(self) -> Self {
        self.state().block = true;
        self
    }

    fn ran(&self) -> Vec<Ran> {
        self.state().ran.clone()
    }

    /// The ids of the calls the host executed, in order.
    fn runs(&self) -> Vec<String> {
        self.state().ran.iter().map(|ran| ran.call_id.clone()).collect()
    }
}

impl ToolHost for FakeHost {
    fn available(&self, tool: ToolName, _pane: PaneId) -> bool {
        self.state().available.contains(&tool)
    }

    fn resolve(&self, scope: &Scope, tool: &Tool) -> Result<Resolved, ToolError> {
        let mut resolved = Resolved::remote(scope.pane, "build box");
        if let Some(path) = tool.path() {
            resolved = resolved.with_path(path);
        }
        Ok(resolved)
    }

    fn run(&self, ticket: &Ticket, cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        let path = ticket.resolved().path().map(str::to_owned);
        let block = {
            let mut state = self.state();
            state.ran.push(Ran { call_id: ticket.call_id().to_owned(), path: path.clone() });
            state.block
        };
        if block && cancel.wait_cancelled(Duration::from_secs(5)) {
            return Err(ToolError::Cancelled);
        }
        let state = self.state();
        match ticket.tool() {
            Tool::ReadFile(_) => {
                let Some(path) = path else {
                    return Err(ToolError::Host(Detail::new("no path")));
                };
                if state.failing.contains(&path) {
                    return Err(ToolError::Host(Detail::new("read failed: device busy")));
                }
                match state.files.get(&path) {
                    Some(text) => Ok(ToolOutput::new(text.clone())),
                    None => Err(ToolError::Host(Detail::new("no such file"))),
                }
            }
            Tool::WriteFile(args) => Ok(ToolOutput::new(format!("wrote {} bytes", args.content.len()))),
            _ => Ok(ToolOutput::new("ok".to_owned())),
        }
    }
}

// ---- the world and the helpers ------------------------------------------------------------------------------------

struct World {
    provider: ScriptedProvider,
    host: FakeHost,
    table: ApprovalTable,
    limits: Limits,
}

impl World {
    fn new(host: FakeHost, provider: ScriptedProvider) -> Self {
        Self {
            provider,
            host,
            table: ApprovalTable::new(DenyPolicy::new(Vec::new())),
            limits: Limits::default(),
        }
    }

    fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    fn agent(&self) -> Agent<'_> {
        Agent::new(&self.provider, &self.host, &self.table).with_limits(self.limits.clone())
    }
}

fn scope(conversation: &str, turn: u64) -> Scope {
    Scope { conversation: conversation.to_owned(), turn, pane: 3 }
}

fn user(id: &str, text: &str) -> Message {
    Message::user_text(id, text, 1)
}

fn request(prefix: &str, messages: Vec<Message>) -> TurnRequest {
    TurnRequest {
        scope: scope("c1", 1),
        model: "scripted-model".to_owned(),
        system: "You help with a terminal.".to_owned(),
        messages,
        context: Vec::new(),
        id_prefix: prefix.to_owned(),
    }
}

fn native_blob() -> Opaque {
    Opaque { wire: "scripted".to_owned(), json: json!({ "blocks": ["a"] }) }
}

/// A script that asks for one call and stops for tool use.
fn call_script(call_id: &str, tool: &str, args: Value) -> Script {
    Script::new().tool_call(call_id, tool, args).stop(StopReason::ToolUse)
}

fn read_script(call_id: &str, path: &str) -> Script {
    call_script(call_id, "read_file", json!({ "path": path }))
}

fn allow_all(_: &Pending) -> Option<Decision> {
    Some(Decision::Allow)
}

fn deny_all(_: &Pending) -> Option<Decision> {
    Some(Decision::Deny)
}

struct Played {
    outcome: TurnOutcome,
    events: Vec<AgentEvent>,
}

impl Played {
    fn cards(&self) -> Vec<&Pending> {
        self.events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolProposed(pending) => Some(pending),
                _ => None,
            })
            .collect()
    }

    /// The `ToolResult` events as `(call id, content, is_error)`.
    fn result_events(&self) -> Vec<(String, String, bool)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolResult { call_id, content, is_error, .. } => {
                    Some((call_id.clone(), content.clone(), *is_error))
                }
                _ => None,
            })
            .collect()
    }

    fn labels(&self) -> Vec<String> {
        self.events
            .iter()
            .map(|event| match event {
                AgentEvent::RoundStarted { round } => format!("RoundStarted {round}"),
                AgentEvent::TextDelta(_) => "TextDelta".to_owned(),
                AgentEvent::ReasoningDelta(_) => "ReasoningDelta".to_owned(),
                AgentEvent::Trimmed { .. } => "Trimmed".to_owned(),
                AgentEvent::Usage(_) => "Usage".to_owned(),
                AgentEvent::Message(_) => "Message".to_owned(),
                AgentEvent::ToolProposed(_) => "ToolProposed".to_owned(),
                AgentEvent::ToolResult { .. } => "ToolResult".to_owned(),
            })
            .collect()
    }

    fn message_events(&self) -> Vec<&Message> {
        self.events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Message(message) => Some(message),
                _ => None,
            })
            .collect()
    }
}

/// Runs a turn, playing the user: `choose` answers every card (or leaves it open).
fn play(
    world: &World,
    turn: TurnRequest,
    cancel: &CancelToken,
    choose: &mut dyn FnMut(&Pending) -> Option<Decision>,
) -> Played {
    let agent = world.agent();
    let mut events = Vec::new();
    let outcome = {
        let mut sink = |event: AgentEvent| {
            if let AgentEvent::ToolProposed(pending) = &event
                && let Some(decision) = choose(pending)
            {
                world
                    .table
                    .decide(&pending.conversation, &pending.call_id, decision, &pending.hash)
                    .expect("the hash of the card is accepted");
            }
            events.push(event);
        };
        agent.run_turn(turn, &mut sink, cancel)
    };
    Played { outcome, events }
}

/// The transcript contract: every call is answered exactly once, by the tool message that directly follows its
/// assistant message, and no result stands without its call.
fn assert_valid_transcript(messages: &[Message]) {
    let mut asked: HashSet<String> = HashSet::new();
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        match message.role {
            Role::User => index += 1,
            Role::Tool => panic!("a tool message at {index} does not follow an assistant message that asked for tools"),
            Role::Assistant => {
                let calls: Vec<&ToolCall> = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::ToolCall(call) => Some(call),
                        _ => None,
                    })
                    .collect();
                index += 1;
                if calls.is_empty() {
                    continue;
                }
                let next = messages
                    .get(index)
                    .unwrap_or_else(|| panic!("calls of message {} have no tool message", message.id));
                assert_eq!(next.role, Role::Tool, "the message after {} must be the tool message", message.id);
                let results: Vec<&ToolResult> = next
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::ToolResult(result) => Some(result),
                        _ => None,
                    })
                    .collect();
                assert_eq!(results.len(), calls.len(), "one result for each call of {}", message.id);
                for call in &calls {
                    assert!(asked.insert(call.id.clone()), "call id {} is used twice", call.id);
                    let answers = results.iter().filter(|result| result.call_id == call.id).count();
                    assert_eq!(answers, 1, "call {} must be answered exactly once", call.id);
                }
                index += 1;
            }
        }
    }
}

/// The history plus what the turn produced.
fn transcript(history: &[Message], outcome: &TurnOutcome) -> Vec<Message> {
    history.iter().chain(outcome.messages.iter()).cloned().collect()
}

/// Checks the message events against the outcome and the transcript contract; the helper nearly every test ends with.
fn assert_consistent(history: &[Message], played: &Played) {
    let json = |message: &Message| serde_json::to_value(message).expect("a message serialises");
    let events: Vec<Value> = played.message_events().into_iter().map(json).collect();
    let produced: Vec<Value> = played.outcome.messages.iter().map(json).collect();
    assert_eq!(events, produced, "the Message events equal the produced messages one for one");
    assert_valid_transcript(&transcript(history, &played.outcome));
    // Every card is followed by a result event for the same call.
    let results = played.result_events();
    for card in played.cards() {
        assert!(results.iter().any(|(id, _, _)| *id == card.call_id), "card {} has no result event", card.call_id);
    }
}

fn tool_result_of(message: &Message) -> Vec<&ToolResult> {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect()
}

// ---- the tracer ---------------------------------------------------------------------------------------------------

/// The tracer: a scripted model reads a file, the user decides on the card's hash, the result goes back, and the
/// second round answers.
#[test]
fn a_scripted_model_reads_a_file_after_approval_and_answers() {
    let world = World::new(
        FakeHost::new().with_file(NOTES, "alpha beta"),
        ScriptedProvider::new("scripted")
            .with_turn(
                Script::new()
                    .text("Let me look. ")
                    .tool_call("call_1", "read_file", json!({ "path": NOTES }))
                    .usage(120, 15)
                    .stop(StopReason::ToolUse)
                    .native(native_blob()),
            )
            .with_turn(Script::new().text("The file ").text("says alpha beta.").usage(160, 9)),
    );
    let history = vec![user("u1", "What is in notes.txt?")];
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    let outcome = &played.outcome;

    assert_eq!(outcome.end, TurnEnd::Done(StopReason::EndTurn));
    assert_eq!(outcome.rounds, 2);
    assert_eq!(outcome.usage, Usage { input_tokens: 280, output_tokens: 24 });

    assert_eq!(outcome.messages.len(), 3);
    let ids: Vec<&str> = outcome.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["t1-1", "t1-2", "t1-3"]);
    let first = &outcome.messages[0];
    assert_eq!(first.role, Role::Assistant);
    assert_eq!(first.status, holzwave_core::ai::types::MessageStatus::Complete);
    assert_eq!(first.model.as_deref(), Some("scripted-model"));
    assert_eq!(first.native, Some(native_blob()));
    assert_eq!(first.parts.len(), 2);
    assert!(matches!(&first.parts[0], Part::Text { text } if text == "Let me look. "));
    assert!(matches!(&first.parts[1], Part::ToolCall(call) if call.id == "call_1" && call.name == "read_file"));
    let second = &outcome.messages[1];
    assert_eq!(second.role, Role::Tool);
    let results = tool_result_of(second);
    assert_eq!(results.len(), 1);
    let answer = (results[0].call_id.as_str(), results[0].content.as_str(), results[0].is_error);
    assert_eq!(answer, ("call_1", "alpha beta", false));
    let third = &outcome.messages[2];
    assert_eq!(third.role, Role::Assistant);
    assert_eq!(third.text(), "The file says alpha beta.");

    assert_eq!(
        played.labels(),
        [
            "RoundStarted 1",
            "TextDelta",
            "Usage",
            "Message",
            "ToolProposed",
            "ToolResult",
            "Message",
            "RoundStarted 2",
            "TextDelta",
            "TextDelta",
            "Usage",
            "Message"
        ]
    );
    let cards = played.cards();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].tool, ToolName::ReadFile);
    assert!(!cards[0].tainted && cards[0].taint.is_empty());
    assert_consistent(&history, &played);

    let requests = world.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].system, "You help with a terminal.");
    assert_eq!(requests[0].messages.len(), 1);
    let tools: Vec<&str> = requests[0].tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(tools, ["read_file", "list_dir", "grep", "read_pane"]);
    assert!(requests[0].context.is_empty());
    assert_eq!(requests[0].max_output_tokens, Some(4096), "the model's 4096 is below the default");
    assert_eq!(requests[1].messages.len(), 3);
    assert_eq!(requests[1].messages[1].native, Some(native_blob()), "the native turn rides along unchanged");
    assert_eq!(tool_result_of(&requests[1].messages[2])[0].content, "alpha beta");

    assert_eq!(world.host.ran(), [Ran { call_id: "call_1".to_owned(), path: Some(NOTES.to_owned()) }]);
    assert!(world.table.pending("c1").is_empty());
    assert!(world.table.taint_labels(&scope("c1", 1)).is_empty());
}

#[test]
fn a_denied_call_is_fed_back_and_the_turn_goes_on() {
    let world = World::new(
        FakeHost::new().with_file(NOTES, "alpha beta"),
        ScriptedProvider::new("scripted")
            .with_turn(read_script("call_1", NOTES))
            .with_turn(Script::reply("Understood, I will not read it.")),
    );
    let history = vec![user("u1", "What is in notes.txt?")];
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut deny_all);

    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
    assert_eq!(played.outcome.rounds, 2);
    assert!(world.host.runs().is_empty(), "a denied call never reaches the host");
    assert_eq!(played.result_events(), [("call_1".to_owned(), "The user denied this action.".to_owned(), true)]);
    assert_consistent(&history, &played);
}

#[test]
fn a_refused_or_failing_call_becomes_an_error_result_that_never_echoes_arguments() {
    // (a) an unknown argument gets no card.
    let world = World::new(
        FakeHost::new().with_file(NOTES, "alpha beta"),
        ScriptedProvider::new("scripted")
            .with_turn(call_script("call_1", "read_file", json!({ "path": NOTES, "mode": "CANARY-b27e" })))
            .with_turn(Script::reply("done")),
    );
    let history = vec![user("u1", "read it")];
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
    assert!(played.cards().is_empty(), "no card for a call the table refused");
    let results = played.result_events();
    assert_eq!(results.len(), 1);
    assert!(results[0].2, "an error result");
    assert!(!results[0].1.contains("CANARY-b27e"), "an argument is never echoed: {}", results[0].1);
    assert!(world.host.runs().is_empty());
    assert_consistent(&history, &played);

    // (b) the host fails on an allowed call.
    let world = World::new(
        FakeHost::new().failing("/srv/app/busy.txt"),
        ScriptedProvider::new("scripted")
            .with_turn(read_script("call_1", "/srv/app/busy.txt"))
            .with_turn(Script::reply("done")),
    );
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
    let results = played.result_events();
    assert_eq!(results.len(), 1);
    assert!(results[0].2);
    assert!(results[0].1.contains("device busy"), "the host's reason reaches the model: {}", results[0].1);
    assert_eq!(world.host.runs(), ["call_1"]);
    assert_consistent(&history, &played);

    // (c) the deny list refuses an environment file before any card exists.
    let world = World::new(
        FakeHost::new().with_file("/srv/app/.env", "TOKEN=1"),
        ScriptedProvider::new("scripted")
            .with_turn(read_script("call_1", "/srv/app/.env"))
            .with_turn(Script::reply("done")),
    );
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
    assert!(played.cards().is_empty());
    let results = played.result_events();
    assert_eq!(results.len(), 1);
    assert!(results[0].2);
    assert!(!results[0].1.contains("/srv/app/.env") && !results[0].1.contains("env_file"), "{}", results[0].1);
    assert!(world.host.runs().is_empty());
    assert_consistent(&history, &played);
}

#[test]
fn tool_results_are_sanitised_redacted_and_bounded_before_anyone_sees_them() {
    let mut text = String::from("\u{1b}[31mred \u{1b}[0m key: sk-ant-api03-CANARY7f3aQ0123456789abcdefABCDEF\n");
    for line in 0..1200 {
        text.push_str(&format!("filler line number {line} of the file under test\n"));
    }
    assert!(text.len() > 40 * 1024);
    let world = World::new(
        FakeHost::new().with_file(NOTES, &text),
        ScriptedProvider::new("scripted").with_turn(read_script("call_1", NOTES)).with_turn(Script::reply("done")),
    );
    let history = vec![user("u1", "read it")];
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));

    let (content, redactions) = played
        .events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolResult { content, redactions, .. } => Some((content.clone(), *redactions)),
            _ => None,
        })
        .expect("a result event");
    assert!(content.contains("red"), "the text survives");
    assert!(content.contains("[REDACTED:anthropic_key]"), "the key is redacted");
    assert!(!content.contains('\u{1b}'), "no escape byte reaches the model or the page");
    assert!(!content.contains("CANARY7f3a"), "no part of the key survives");
    assert!(content.len() <= 16_384, "bounded: {} bytes", content.len());
    assert!(redactions >= 3, "two escape sequences and a key: {redactions}");

    let requests = world.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(tool_result_of(&requests[1].messages[2])[0].content, content, "the model sees exactly the cleaned text");
    assert_eq!(tool_result_of(&played.outcome.messages[1])[0].content, content);
    assert_consistent(&history, &played);
}

#[test]
fn the_approval_table_is_clean_when_a_turn_ends_however_it_ends() {
    let history = vec![user("u1", "What is in notes.txt?")];

    // A turn that ends normally.
    let world = World::new(
        FakeHost::new().with_file(NOTES, "alpha beta"),
        ScriptedProvider::new("scripted").with_turn(read_script("call_1", NOTES)).with_turn(Script::reply("done")),
    );
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
    assert!(world.table.pending("c1").is_empty());
    assert!(world.table.taint_labels(&scope("c1", 1)).is_empty());
    assert_consistent(&history, &played);

    // A turn whose provider fails in the first round: the partial text stays, nothing is retried.
    let world = World::new(
        FakeHost::new(),
        ScriptedProvider::new("scripted")
            .with_turn(Script::new().text("part").fail(AiError::Overloaded))
            .with_turn(Script::reply("never served")),
    );
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Failed(AiError::Overloaded));
    assert_eq!(played.outcome.rounds, 1);
    assert_eq!(world.provider.calls(), 1, "the loop never retries a round");
    assert_eq!(played.outcome.messages.len(), 1);
    let message = &played.outcome.messages[0];
    assert_eq!(message.role, Role::Assistant);
    assert_eq!(message.text(), "part");
    assert_eq!(message.status, holzwave_core::ai::types::MessageStatus::Failed);
    assert!(!message.parts.iter().any(|part| matches!(part, Part::ToolCall(_))));
    assert!(world.table.pending("c1").is_empty());
    assert!(world.table.taint_labels(&scope("c1", 1)).is_empty());
    assert_consistent(&history, &played);
}

#[test]
fn a_turn_must_end_with_a_user_message_and_carry_an_id_prefix() {
    let assistant = Message::new("a1", Role::Assistant, vec![Part::Text { text: "hi".to_owned() }], 1);
    let long_prefix = "p".repeat(65);
    let cases: Vec<(&str, TurnRequest)> = vec![
        ("no messages", request("t1", Vec::new())),
        ("last message from the model", request("t1", vec![user("u1", "hello"), assistant])),
        ("empty prefix", request("", vec![user("u1", "hello")])),
        ("prefix too long", request(&long_prefix, vec![user("u1", "hello")])),
        ("prefix with a line feed", request("t\n1", vec![user("u1", "hello")])),
    ];
    for (name, turn) in cases {
        let world = World::new(FakeHost::new(), ScriptedProvider::new("scripted").with_turn(Script::reply("never")));
        let played = play(&world, turn, &CancelToken::new(), &mut allow_all);
        let end = &played.outcome.end;
        assert!(matches!(end, TurnEnd::Failed(AiError::InvalidRequest(_))), "{name}: {end:?}");
        assert_eq!(played.outcome.rounds, 0, "{name}");
        assert!(played.outcome.messages.is_empty() && played.events.is_empty(), "{name}");
        assert_eq!(world.provider.calls(), 0, "{name}");
    }
}

#[test]
fn the_step_budget_stops_a_model_that_keeps_asking_for_tools() {
    let mut provider = ScriptedProvider::new("scripted");
    for (index, path) in ["/a", "/b", "/c", "/d"].into_iter().enumerate() {
        provider = provider.with_turn(call_script(&format!("call_{index}"), "list_dir", json!({ "path": path })));
    }
    let world = World::new(FakeHost::new(), provider).with_limits(Limits { max_rounds: 3, ..Limits::default() });
    let history = vec![user("u1", "list everything")];
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);

    assert_eq!(played.outcome.end, TurnEnd::Limit(Limit::Steps));
    assert_eq!(played.outcome.rounds, 3);
    assert_eq!(played.cards().len(), 3);
    assert_eq!(played.outcome.messages.len(), 6);
    let roles: Vec<Role> = played.outcome.messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, [Role::Assistant, Role::Tool, Role::Assistant, Role::Tool, Role::Assistant, Role::Tool]);
    assert_eq!(world.provider.calls(), 3, "the fourth script is not consumed");
    assert_consistent(&history, &played);
}

// ---- budgets and guards -------------------------------------------------------------------------------------------

/// `bytes` bytes of plain words: nothing a redactor or a trim would treat specially.
fn plain_text(bytes: usize) -> String {
    let mut text = String::new();
    while text.len() < bytes {
        text.push_str("lorem ipsum dolor ");
    }
    text.truncate(bytes);
    text
}

fn capabilities(tools: bool, context_tokens: Option<u32>, max_output_tokens: Option<u32>) -> Capabilities {
    Capabilities {
        tools,
        reasoning: false,
        vision: false,
        context_tokens,
        max_output_tokens,
        locality: Locality::Loopback,
    }
}

fn ids_of(results: &[(String, String, bool)]) -> Vec<&str> {
    results.iter().map(|(id, _, _)| id.as_str()).collect()
}

#[test]
fn the_token_budget_uses_reported_usage_and_falls_back_to_an_estimate() {
    // Reported: 900 in and 200 out reach the budget of 1000, so the second round is never asked for.
    let world = World::new(
        FakeHost::new().with_file(NOTES, "alpha beta"),
        ScriptedProvider::new("scripted")
            .with_turn(read_script("call_1", NOTES).usage(900, 200))
            .with_turn(Script::reply("never asked")),
    )
    .with_limits(Limits { max_turn_tokens: 1000, ..Limits::default() });
    let history = vec![user("u1", "read it")];
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Limit(Limit::Tokens));
    assert_eq!(played.outcome.rounds, 1);
    assert_eq!(world.provider.calls(), 1);
    assert_consistent(&history, &played);

    // Not reported: the estimate of the request and the reply bytes exceeds 100 tokens.
    let world = World::new(
        FakeHost::new().with_file(NOTES, "alpha beta"),
        ScriptedProvider::new("scripted")
            .with_turn(read_script("call_1", NOTES))
            .with_turn(Script::reply("never asked")),
    )
    .with_limits(Limits { max_turn_tokens: 100, ..Limits::default() });
    let history = vec![user("u1", &plain_text(600))];
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Limit(Limit::Tokens));
    assert_eq!(played.outcome.rounds, 1);
    assert_eq!(world.provider.calls(), 1);
    assert_consistent(&history, &played);

    assert_eq!([0, 1, 2, 3, 4, 6, 7].map(estimate_tokens), [0, 1, 1, 1, 2, 2, 3]);
}

#[test]
fn the_tool_output_budget_of_a_turn_stops_further_calls() {
    let mut host = FakeHost::new();
    for index in 1..=3 {
        host = host.with_file(&format!("/srv/app/f{index}.txt"), &plain_text(600));
    }
    let world = World::new(
        host,
        ScriptedProvider::new("scripted")
            .with_turn(
                Script::new()
                    .tool_call("call_1", "read_file", json!({ "path": "/srv/app/f1.txt" }))
                    .tool_call("call_2", "read_file", json!({ "path": "/srv/app/f2.txt" }))
                    .tool_call("call_3", "read_file", json!({ "path": "/srv/app/f3.txt" }))
                    .stop(StopReason::ToolUse),
            )
            .with_turn(Script::reply("never asked")),
    )
    .with_limits(Limits { max_result_bytes: 1000, ..Limits::default() });
    let history = vec![user("u1", "read all three")];
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);

    assert_eq!(played.outcome.end, TurnEnd::Limit(Limit::ToolOutput));
    assert_eq!(world.host.runs(), ["call_1", "call_2"]);
    assert_eq!(played.cards().len(), 2, "the third call gets no card");
    let results = played.result_events();
    assert_eq!(ids_of(&results), ["call_1", "call_2", "call_3"]);
    assert_eq!(results[2].1, STOPPED_RESULT);
    assert!(results[2].2);
    assert_eq!(played.outcome.messages[1].status, holzwave_core::ai::types::MessageStatus::Complete);
    assert_consistent(&history, &played);
}

#[test]
fn an_endless_reply_is_cut_at_the_reply_limit() {
    let world = World::new(
        FakeHost::new(),
        ScriptedProvider::new("scripted").with_turn(
            Script::new()
                .text(plain_text(600))
                .text(plain_text(600))
                .text(plain_text(600))
                .tool_call("call_1", "list_dir", json!({ "path": "/a" })),
        ),
    )
    .with_limits(Limits { max_reply_bytes: 1024, ..Limits::default() });
    let history = vec![user("u1", "talk")];
    let cancel = CancelToken::new();
    let played = play(&world, request("t1", history.clone()), &cancel, &mut allow_all);

    assert_eq!(played.outcome.end, TurnEnd::Limit(Limit::ReplySize));
    assert!(!cancel.is_cancelled(), "the bound is the loop's, not the user's stop");
    assert_eq!(played.outcome.messages.len(), 1);
    let message = &played.outcome.messages[0];
    assert_eq!(message.text(), plain_text(600), "only the first 600 bytes were kept");
    assert_eq!(message.status, holzwave_core::ai::types::MessageStatus::Failed);
    assert!(!message.parts.iter().any(|part| matches!(part, Part::ToolCall(_))));
    let deltas = played.labels().iter().filter(|label| label.as_str() == "TextDelta").count();
    assert_eq!(deltas, 1, "the delta that passed the bound was not shown");
    assert!(played.cards().is_empty());
    assert_consistent(&history, &played);
}

#[test]
fn the_third_identical_call_stops_the_turn_before_its_card() {
    let history = vec![user("u1", "list /a again and again")];

    let list = |id: &str| call_script(id, "list_dir", json!({ "path": "/a" }));
    let world = World::new(
        FakeHost::new(),
        ScriptedProvider::new("scripted")
            .with_turn(list("call_1"))
            .with_turn(list("call_2"))
            .with_turn(list("call_3"))
            .with_turn(Script::reply("never asked")),
    );
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Limit(Limit::RepeatedCall));
    assert_eq!(played.outcome.rounds, 3);
    assert_eq!(played.cards().len(), 2, "the third call gets no card");
    assert_eq!(world.host.runs(), ["call_1", "call_2"]);
    let results = played.result_events();
    assert_eq!(results[2].1, STOPPED_RESULT);
    assert_consistent(&history, &played);

    // The same arguments under another tool are another call, and the key order of the arguments does not matter.
    let world = World::new(
        FakeHost::new().with_file("/a", "text"),
        ScriptedProvider::new("scripted")
            .with_turn(call_script("call_1", "list_dir", json!({ "path": "/a" })))
            .with_turn(call_script("call_2", "read_file", json!({ "path": "/a" })))
            .with_turn(call_script("call_3", "list_dir", json!({ "path": "/a" })))
            .with_turn(call_script("call_4", "read_file", json!({ "path": "/a" })))
            .with_turn(Script::reply("done")),
    );
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
    assert_eq!(played.outcome.rounds, 5);
    assert_consistent(&history, &played);

    let world = World::new(
        FakeHost::new(),
        ScriptedProvider::new("scripted")
            .with_turn(call_script("call_1", "list_dir", json!({ "path": "/a", "max_entries": 5 })))
            .with_turn(call_script("call_2", "list_dir", json!({ "max_entries": 5, "path": "/a" })))
            .with_turn(call_script("call_3", "list_dir", json!({ "path": "/a", "max_entries": 5 }))),
    );
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Limit(Limit::RepeatedCall), "argument key order is not a difference");
    assert_eq!(played.outcome.rounds, 3);
    assert_consistent(&history, &played);
}

#[test]
fn consecutive_tool_failures_stop_the_turn_and_a_success_resets_the_count() {
    let history = vec![user("u1", "read the files")];

    // Three missing files in a row.
    let world = World::new(
        FakeHost::new(),
        ScriptedProvider::new("scripted")
            .with_turn(read_script("call_1", "/m1"))
            .with_turn(read_script("call_2", "/m2"))
            .with_turn(read_script("call_3", "/m3"))
            .with_turn(Script::reply("never asked")),
    );
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Limit(Limit::ToolErrors));
    assert_eq!(played.outcome.rounds, 3);
    assert_eq!(world.host.runs().len(), 3);
    assert_consistent(&history, &played);

    // Fail, fail, succeed, fail, fail: the success reset the count.
    let world = World::new(
        FakeHost::new().with_file(NOTES, "alpha beta"),
        ScriptedProvider::new("scripted")
            .with_turn(read_script("call_1", "/m1"))
            .with_turn(read_script("call_2", "/m2"))
            .with_turn(read_script("call_3", NOTES))
            .with_turn(read_script("call_4", "/m3"))
            .with_turn(read_script("call_5", "/m4"))
            .with_turn(Script::reply("done")),
    );
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
    assert_eq!(played.outcome.rounds, 6);
    assert_consistent(&history, &played);

    // A denial between two failures neither resets nor counts: fail, deny, fail, fail makes three failures.
    let world = World::new(
        FakeHost::new(),
        ScriptedProvider::new("scripted")
            .with_turn(read_script("call_1", "/m1"))
            .with_turn(read_script("call_2", "/m2"))
            .with_turn(read_script("call_3", "/m3"))
            .with_turn(read_script("call_4", "/m4"))
            .with_turn(Script::reply("never asked")),
    );
    let mut choose = |pending: &Pending| {
        Some(if pending.call_id == "call_2" { Decision::Deny } else { Decision::Allow })
    };
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut choose);
    assert_eq!(played.outcome.end, TurnEnd::Limit(Limit::ToolErrors));
    assert_eq!(played.outcome.rounds, 4, "the denial was neither a failure nor a reset");
    assert_consistent(&history, &played);

    // A tool that does not exist counts as a failure.
    let world = World::new(
        FakeHost::new(),
        ScriptedProvider::new("scripted")
            .with_turn(call_script("call_1", "frobnicate", json!({ "n": 1 })))
            .with_turn(call_script("call_2", "frobnicate", json!({ "n": 2 })))
            .with_turn(call_script("call_3", "frobnicate", json!({ "n": 3 })))
            .with_turn(Script::reply("never asked")),
    );
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Limit(Limit::ToolErrors));
    assert_eq!(played.outcome.rounds, 3);
    assert!(played.cards().is_empty());
    assert_consistent(&history, &played);
}

#[test]
fn a_step_with_an_empty_or_repeated_call_id_fails_before_any_card() {
    let history = vec![user("u1", "do two things")];
    let batches = [
        ("a shared id", vec![("x", "/a"), ("x", "/b")]),
        ("an empty id", vec![("", "/a"), ("y", "/b")]),
    ];
    for (name, calls) in batches {
        let mut script = Script::new().text("I will do two things");
        for (id, path) in calls {
            script = script.tool_call(id, "list_dir", json!({ "path": path }));
        }
        let provider = ScriptedProvider::new("scripted").with_turn(script.stop(StopReason::ToolUse));
        let world = World::new(FakeHost::new(), provider);
        let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);

        let end = &played.outcome.end;
        assert!(matches!(end, TurnEnd::Failed(AiError::Protocol(_))), "{name}: {end:?}");
        assert_eq!(played.outcome.rounds, 1, "{name}");
        assert!(played.cards().is_empty() && world.host.runs().is_empty(), "{name}");
        assert_eq!(played.outcome.messages.len(), 1, "{name}");
        let message = &played.outcome.messages[0];
        assert_eq!(message.text(), "I will do two things", "{name}");
        assert_eq!(message.status, holzwave_core::ai::types::MessageStatus::Failed, "{name}");
        assert!(!message.parts.iter().any(|part| matches!(part, Part::ToolCall(_))), "{name}");
        assert_consistent(&history, &played);
    }
}

#[test]
fn a_tool_that_was_not_declared_is_refused_without_a_card() {
    let history = vec![user("u1", "write a file")];

    // A host that offers only the read tools: a model that asks for a write is refused.
    let world = World::new(
        FakeHost::new(),
        ScriptedProvider::new("scripted")
            .with_turn(call_script("call_1", "write_file", json!({ "path": "/srv/app/out.txt", "content": "x" })))
            .with_turn(Script::reply("done")),
    );
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
    assert!(played.cards().is_empty());
    assert!(world.host.runs().is_empty());
    assert_eq!(played.result_events(), [("call_1".to_owned(), ToolError::Unavailable.to_string(), true)]);
    assert_consistent(&history, &played);

    // A model without tool support gets none declared, and a call it makes anyway is refused the same way.
    let world = World::new(
        FakeHost::new().with_file(NOTES, "alpha beta"),
        ScriptedProvider::new("scripted")
            .with_capabilities(capabilities(false, Some(32768), Some(4096)))
            .with_turn(read_script("call_1", NOTES))
            .with_turn(Script::reply("done")),
    );
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
    assert!(world.provider.requests()[0].tools.is_empty());
    assert!(played.cards().is_empty());
    assert!(world.host.runs().is_empty());
    assert_eq!(played.result_events(), [("call_1".to_owned(), ToolError::Unavailable.to_string(), true)]);
    assert_consistent(&history, &played);
}

#[test]
fn calls_beyond_the_per_step_cap_are_answered_without_cards() {
    let world = World::new(
        FakeHost::new(),
        ScriptedProvider::new("scripted")
            .with_turn(
                Script::new()
                    .tool_call("call_1", "list_dir", json!({ "path": "/a" }))
                    .tool_call("call_2", "list_dir", json!({ "path": "/b" }))
                    .tool_call("call_3", "list_dir", json!({ "path": "/c" }))
                    .stop(StopReason::ToolUse),
            )
            .with_turn(Script::reply("done")),
    )
    .with_limits(Limits { max_calls_per_round: 2, ..Limits::default() });
    let history = vec![user("u1", "list three")];
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);

    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
    assert_eq!(played.outcome.rounds, 2);
    assert_eq!(played.cards().len(), 2);
    assert_eq!(world.host.runs(), ["call_1", "call_2"]);
    let results = played.result_events();
    assert_eq!(results[2], ("call_3".to_owned(), ToolError::TooMany.to_string(), true));
    assert_consistent(&history, &played);
}

/// One earlier exchange: a question, a `list_dir` call, its result of `result_bytes` bytes, and an answer.
fn exchange(index: usize, result_bytes: usize) -> Vec<Message> {
    let call_id = format!("old_{index}");
    let args = json!({ "path": format!("/old/{index}") });
    let call = ToolCall { id: call_id.clone(), name: "list_dir".to_owned(), args };
    let result = ToolResult { call_id, content: plain_text(result_bytes), is_error: false };
    vec![
        user(&format!("h{index}-u"), &format!("question {index}")),
        Message::new(format!("h{index}-a"), Role::Assistant, vec![Part::ToolCall(call)], 1),
        Message::new(format!("h{index}-t"), Role::Tool, vec![Part::ToolResult(result)], 1),
        Message::new(format!("h{index}-b"), Role::Assistant, vec![Part::Text { text: format!("answer {index}") }], 1),
    ]
}

/// The size of a request by the rule the loop uses: texts, call names and arguments, result contents, the tool
/// declarations and the system prompt.
fn measure(request: &ChatRequest) -> usize {
    let mut total = request.system.len();
    for tool in &request.tools {
        total += tool.description.len() + serde_json::to_string(&tool.input_schema).expect("a schema serialises").len();
    }
    for message in &request.messages {
        for part in &message.parts {
            total += match part {
                Part::Text { text } => text.len(),
                Part::ToolCall(call) => call.name.len() + serde_json::to_string(&call.args).expect("serialises").len(),
                Part::ToolResult(result) => result.content.len(),
                Part::Context(_) => 128,
            };
        }
    }
    total
}

fn trimmed_events(played: &Played) -> Vec<(u32, u32)> {
    played
        .events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Trimmed { results, exchanges } => Some((*results, *exchanges)),
            _ => None,
        })
        .collect()
}

#[test]
fn old_tool_output_is_trimmed_first_then_old_exchanges_and_the_trim_is_announced() {
    // A window of 3500 tokens with 500 reserved for the answer leaves (3500 - 500) * 3 = 9000 bytes.
    let small_window =
        || ScriptedProvider::new("scripted").with_capabilities(capabilities(true, Some(3500), Some(500)));

    // Six exchanges with 2000-byte results do not fit; the oldest results are replaced first. The very oldest result
    // is already the stored marker and is neither trimmed again nor counted.
    let mut history = Vec::new();
    for index in 0..6 {
        history.extend(exchange(index, 2000));
    }
    if let Part::ToolResult(result) = &mut history[2].parts[0] {
        NOT_STORED.clone_into(&mut result.content);
    }
    history.push(user("now", "and now?"));
    let before = serde_json::to_value(&history).expect("serialises");
    let world = World::new(FakeHost::new(), small_window().with_turn(Script::reply("ok")));
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);

    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
    let sent = &world.provider.requests()[0];
    assert_eq!(sent.messages.last().map(|m| m.text()), Some("and now?".to_owned()), "the current message is kept");
    let results: Vec<&str> = sent
        .messages
        .iter()
        .flat_map(|message| message.parts.iter())
        .filter_map(|part| match part {
            Part::ToolResult(result) => Some(result.content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 6, "every call keeps its result");
    assert_eq!(results[0], NOT_STORED, "a stored marker is left alone");
    let trimmed = results.iter().filter(|content| **content == TRIMMED_RESULT).count();
    assert!(trimmed > 0, "old results were trimmed");
    let first_whole = results.iter().position(|content| content.len() == 2000).expect("a newer result is whole");
    assert!(
        results[..first_whole].iter().all(|content| *content == TRIMMED_RESULT || *content == NOT_STORED),
        "the oldest results go first: {:?}",
        results.iter().map(|content| content.len()).collect::<Vec<_>>()
    );
    assert!(results[first_whole..].iter().all(|content| content.len() == 2000), "newer results are untouched");
    assert!(measure(sent) <= 9000, "the request fits: {} bytes", measure(sent));
    assert_valid_transcript(&sent.messages);
    let replaced = u32::try_from(trimmed).expect("small");
    assert_eq!(trimmed_events(&played), [(replaced, 0)], "announced once, counting only replaced results");
    assert_eq!(serde_json::to_value(&history).expect("serialises"), before, "the caller's history is unchanged");
    let labels = played.labels();
    assert_eq!(labels[0], "Trimmed", "the announcement precedes the round it applies to");

    // Forty small exchanges and no tool output: whole old exchanges go.
    let mut history = Vec::new();
    for index in 0..40 {
        history.push(user(&format!("h{index}-u"), &plain_text(300)));
        history.push(Message::new(
            format!("h{index}-b"),
            Role::Assistant,
            vec![Part::Text { text: plain_text(300) }],
            1,
        ));
    }
    history.push(user("now", "and now?"));
    let world = World::new(FakeHost::new(), small_window().with_turn(Script::reply("ok")));
    let played = play(&world, request("t2", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
    let sent = &world.provider.requests()[0];
    assert_eq!(sent.messages[0].role, Role::User, "the request starts at an exchange");
    assert_eq!(sent.messages.last().map(|m| m.text()), Some("and now?".to_owned()));
    assert!(sent.messages.len() < history.len());
    assert!(measure(sent) <= 9000, "{} bytes", measure(sent));
    let events = trimmed_events(&played);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, 0);
    assert!(events[0].1 > 0, "whole exchanges were removed");
    assert_eq!(usize::try_from(events[0].1).expect("small") * 2 + sent.messages.len(), history.len());

    // The current turn is never trimmed: what the model just read stays whole, and a request that then does not fit
    // fails instead. First learn the size of an empty request, then leave 2500 bytes of room on top of it.
    let probe = World::new(
        FakeHost::new(),
        ScriptedProvider::new("scripted")
            .with_capabilities(capabilities(true, Some(32768), Some(500)))
            .with_turn(Script::reply("ok")),
    );
    play(&probe, request("t3", vec![user("now", "read the big file")]), &CancelToken::new(), &mut allow_all);
    let base = measure(&probe.provider.requests()[0]);
    let window = u32::try_from((base + 2500).div_ceil(3)).expect("small") + 500;
    let world = World::new(
        FakeHost::new().with_file("/srv/app/big.txt", &plain_text(6000)),
        ScriptedProvider::new("scripted")
            .with_capabilities(capabilities(true, Some(window), Some(500)))
            .with_turn(read_script("call_1", "/srv/app/big.txt"))
            .with_turn(Script::reply("never asked")),
    );
    let history = vec![user("now", "read the big file")];
    let played = play(&world, request("t3", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Failed(AiError::ContextLength));
    assert_eq!(played.outcome.rounds, 1);
    assert_eq!(world.provider.calls(), 1);
    assert_eq!(tool_result_of(&played.outcome.messages[1])[0].content.len(), 6000, "the current result is whole");
    assert_consistent(&history, &played);

    // An exchange that holds a message with an approved context is not removed, whatever its age.
    let gate = Gate::new();
    let destination = Destination { host: "localhost".to_owned(), locality: Locality::Loopback };
    let item = RequestItem::new(ContextKind::Screen, "build pane", SourceText::plain("error: the linker failed"));
    let offer = gate.offer(OfferRequest::new("scripted", destination, vec![item])).expect("an offer");
    let approved = gate.approve(offer.id(), &[1], "scripted").expect("an approval");
    let note = ContextNote::for_approved(&approved, None);
    let mut history = vec![Message::new(
        "h0-u",
        Role::User,
        vec![Part::Context(note), Part::Text { text: plain_text(300) }],
        1,
    )];
    history.push(Message::new("h0-b", Role::Assistant, vec![Part::Text { text: plain_text(300) }], 1));
    for index in 1..40 {
        history.push(user(&format!("h{index}-u"), &plain_text(300)));
        let answer = vec![Part::Text { text: plain_text(300) }];
        history.push(Message::new(format!("h{index}-b"), Role::Assistant, answer, 1));
    }
    history.push(user("now", "and now?"));
    let world = World::new(FakeHost::new(), small_window().with_turn(Script::reply("ok")));
    let mut turn = request("t4", history.clone());
    turn.context = vec![approved.clone()];
    let played = play(&world, turn, &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
    let sent = &world.provider.requests()[0];
    assert_eq!(sent.messages[0].id, "h0-u", "the exchange of the approved context stays");
    assert!(sent.messages.len() < history.len(), "other old exchanges went");
    assert_eq!(sent.context.len(), 1);
}

#[test]
fn a_request_that_cannot_fit_fails_before_it_is_sent() {
    // (1500 - 500) * 3 = 3000 bytes of room; the current message alone is 10000.
    let world = World::new(
        FakeHost::new(),
        ScriptedProvider::new("scripted")
            .with_capabilities(capabilities(true, Some(1500), Some(500)))
            .with_turn(Script::reply("never asked")),
    );
    let history = vec![user("u1", &plain_text(10_000))];
    let played = play(&world, request("t1", history), &CancelToken::new(), &mut allow_all);

    assert_eq!(played.outcome.end, TurnEnd::Failed(AiError::ContextLength));
    assert_eq!(played.outcome.rounds, 0);
    assert!(played.outcome.messages.is_empty());
    assert_eq!(world.provider.calls(), 0, "nothing was sent");
}

#[test]
fn the_output_limit_is_always_set_and_never_above_what_the_model_allows() {
    let cases = [
        (capabilities(true, Some(32768), None), Limits::default(), 8192),
        (capabilities(true, Some(32768), Some(500)), Limits::default(), 500),
        (capabilities(true, Some(32768), Some(500)), Limits { max_output_tokens: 100, ..Limits::default() }, 100),
    ];
    for (caps, limits, expected) in cases {
        let world = World::new(
            FakeHost::new(),
            ScriptedProvider::new("scripted").with_capabilities(caps).with_turn(Script::reply("ok")),
        )
        .with_limits(limits);
        let played = play(&world, request("t1", vec![user("u1", "hi")]), &CancelToken::new(), &mut allow_all);
        assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));
        assert_eq!(world.provider.requests()[0].max_output_tokens, Some(expected));
    }
}

#[test]
fn stored_context_text_never_reaches_the_provider() {
    let note = ContextNote {
        hash: "h1".to_owned(),
        items: vec![NoteItem { kind: ContextKind::Screen, label: "build pane".to_owned(), bytes: 21 }],
        stored: Some("CANARY-e5a1 pane text".to_owned()),
        consented_to: Consent { provider: "scripted".to_owned(), locality: Locality::Loopback },
    };
    let earlier = Message::new(
        "h1-u",
        Role::User,
        vec![Part::Context(note.clone()), Part::Text { text: "what failed?".to_owned() }],
        1,
    );
    let history = vec![
        earlier,
        Message::new("h1-a", Role::Assistant, vec![Part::Text { text: "the linker".to_owned() }], 1),
        user("now", "and now?"),
    ];
    let world = World::new(FakeHost::new(), ScriptedProvider::new("scripted").with_turn(Script::reply("ok")));
    let played = play(&world, request("t1", history.clone()), &CancelToken::new(), &mut allow_all);
    assert_eq!(played.outcome.end, TurnEnd::Done(StopReason::EndTurn));

    let sent = &world.provider.requests()[0];
    let text = serde_json::to_string(&sent.messages).expect("serialises");
    assert!(!text.contains("CANARY-e5a1"), "the stored text left the loop: {text}");
    let Part::Context(sent_note) = &sent.messages[0].parts[0] else { panic!("the note stays in the request") };
    assert_eq!(sent_note.stored, None);
    assert_eq!(
        (&sent_note.hash, &sent_note.items, &sent_note.consented_to),
        (&note.hash, &note.items, &note.consented_to)
    );
    let Part::Context(kept) = &history[0].parts[0] else { panic!("the caller's note") };
    assert_eq!(kept.stored.as_deref(), Some("CANARY-e5a1 pane text"), "the caller's copy is unchanged");
}
