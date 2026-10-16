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

use holzwave_core::ai::agent::{Agent, AgentEvent, Limit, Limits, TurnEnd, TurnOutcome, TurnRequest};
use holzwave_core::ai::provider::{AiError, CancelToken, Detail};
use holzwave_core::ai::scripted::{Script, ScriptedProvider};
use holzwave_core::ai::tools::{
    ApprovalTable, Decision, DenyPolicy, Pending, Resolved, Scope, Ticket, Tool, ToolError, ToolHost, ToolName,
    ToolOutput,
};
use holzwave_core::ai::types::{Message, Opaque, Part, Role, StopReason, ToolCall, ToolResult, Usage};
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
