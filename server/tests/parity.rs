//! Parity of the socket with the desktop: every command of the host's table is reachable by name through a signed-in
//! WebSocket, with the desktop's answers and events; nothing runs without a session; hostile arguments end in plain answers.
//!
//! The tracer tests come first; the filter `tracer` selects exactly them. Everything here needs the `in-process-workers`
//! feature, and the page must have been built (`npm --prefix app run build`) because the listener embeds nothing else.
//!
//! The table (`COMMANDS`), the golden file and the metadata of a row are read when a test runs; no test holds a copy of the
//! command list or a count of it.
#![cfg(feature = "in-process-workers")]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/parity_env.rs"]
mod parity_env;
#[path = "support/ws_env.rs"]
mod ws_env;

use holzwave_host::commands::registry::{COMMANDS, NOT_IN_BROWSER, lookup};
use holzwave_server::http::ws::{ANSWER_TOO_LARGE, ANSWER_UNKNOWN};
use holzwave_host::commands::registry::{Command, Web};
use parity_env::{
    Failure, Line, Probed, fresh, start_focused_pane, args_for, args_text_with, golden, golden_at, golden_command_args, golden_command_names, golden_event_names,
    groups, hostile_texts, hostile_whole_args, row_for_group, same_shape, shape_matches_any, table_names, unavailable_rows,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use ws_env::{PATIENCE, Person, WsOptions};

/// A person who is signed in, with a socket whose first message (`hw-session`) has been read.
async fn signed_in(probed: &Probed, name: &str) -> (ws_env::Person, Line) {
    let person = probed.env.person(name);
    let mut line = Line::new(probed.env.connect_as(&person.signed).await);
    assert!(line.until(PATIENCE, |events| events.iter().any(|e| e["event"] == "hw-session")).await, "the session message");
    (person, line)
}

fn err_text(answer: &Value) -> &str {
    answer["err"].as_str().unwrap_or_else(|| panic!("an error string was due: {answer}"))
}

// ---- the tracer ---------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tracer_one_command_of_each_group_answers_with_the_id_of_the_request() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let mut report = Vec::new();
    for group in groups() {
        let row = row_for_group(group).expect("a row of the group the browser has");
        let id = line.send(row.name, &args_for(row)).await;
        let answer = line.wait(id, parity_env::ANSWER_WITHIN).await.unwrap_or_else(|f| panic!("{} ({group}): {f:?}", row.name));
        assert_eq!(answer["id"].as_u64(), Some(id), "{} answers with the id of its request", row.name);
        let kind = if answer.get("ok").is_some() {
            "ok"
        } else {
            assert!(answer.get("err").is_some(), "{} gave neither ok nor err: {answer}", row.name);
            assert_ne!(answer["err"], json!(ANSWER_UNKNOWN), "{} was not found by the endpoint", row.name);
            "err"
        };
        report.push(format!("{group}: {} -> {kind}", row.name));
    }
    println!("tracer groups:\n{}", report.join("\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_the_layout_the_settings_and_the_app_info_have_the_shape_of_the_golden() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    for (command, member) in [("get_layout", "view"), ("get_settings", "settings_view"), ("app_info", "app_info")] {
        let result = line.ok(command, &json!({})).await;
        let golden = golden_at(member).unwrap_or_else(|| panic!("the golden has no {member}"));
        if let Err(difference) = same_shape(&result, golden) {
            panic!("{command} against the golden {member}: {difference}");
        }
        println!("tracer shape: {command} matches {member}");
    }
    let info = line.ok("app_info", &json!({})).await;
    assert_eq!(info["os"], "web", "the page is told it runs in the browser build");
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_command_the_browser_does_not_have_answers_the_sentence_of_the_host_and_runs_nothing() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let row = *unavailable_rows().first().expect("the table marks at least one row unavailable in the browser");
    let before = probed.ran();
    let answer = line.call(row.name, &args_for(row)).await.expect("an answer");
    assert_eq!(err_text(&answer), NOT_IN_BROWSER, "{}", row.name);
    assert_eq!(probed.ran(), before, "{} ran nothing", row.name);
    println!("tracer unavailable: {} -> {NOT_IN_BROWSER}", row.name);
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_name_that_is_not_in_the_table_is_answered_unknown_and_nothing_is_forwarded() {
    let probed = Probed::start(WsOptions::default()).await;
    let (alice, mut line) = signed_in(&probed, "alice").await;
    assert!(lookup("not_a_command").is_none(), "the probe name must not be in the table");
    let before = (probed.ran(), probed.env.counts(&alice));
    let id = line.send_unchecked("not_a_command", &json!({})).await;
    let answer = line.wait(id, parity_env::ANSWER_WITHIN).await.expect("an answer");
    assert_eq!(answer["id"].as_u64(), Some(id));
    assert_eq!(err_text(&answer), ANSWER_UNKNOWN);
    let after = (probed.ran(), probed.env.counts(&alice));
    assert_eq!(before, after, "the unknown name changed nothing the server counts");
    println!("tracer refusal: not_a_command -> {ANSWER_UNKNOWN}; forwarded {} -> {}, sockets {} -> {}, workers {} -> {}", before.0, after.0, before.1.sockets, after.1.sockets, before.1.workers, after.1.workers);
}

#[tokio::test(flavor = "multi_thread")]
#[should_panic(expected = "get_layoutx")]
async fn tracer_the_sender_refuses_a_name_that_is_not_in_the_table() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let _ = line.send("get_layoutx", &json!({})).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_an_upgrade_without_a_cookie_is_closed_4401_and_a_person_who_never_signed_in_has_no_worker() {
    let probed = Probed::start(WsOptions::default()).await;
    let bob = probed.env.person("bob");
    let before = probed.env.counts(&bob);
    let mut client = probed.env.connect(None, Some(edge::ORIGIN)).await;
    let closed = client.close_code(PATIENCE).await.expect("a close message");
    assert_eq!(closed.0, 4401, "{closed:?}");
    let after = probed.env.counts(&bob);
    assert_eq!(before, after, "the refused upgrade changed nothing");
    assert_eq!((after.sockets, after.workers), (0, 0), "no socket and no worker for a person who never signed in");
    assert_eq!(probed.env.totals(), (0, 0), "no worker and no socket on the server");
    assert_eq!(probed.ran(), 0);
    println!("tracer refusal: no cookie -> {closed:?}; counts before {before:?} after {after:?}");
}

// ---- helpers of the walks -----------------------------------------------------------------------------------------------

/// Fails with every problem at once, one per line.
fn nothing_wrong(what: &str, problems: &[String]) {
    assert!(problems.is_empty(), "{what}: {} problems\n{}", problems.len(), problems.join("\n"));
}

/// How a row was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ok,
    Err,
    Unavailable,
}

struct Walked {
    row: &'static Command,
    kind: Kind,
    answer: Value,
}

/// Sends every row of the table by name with its benign arguments and collects the answers and the problems.
async fn walk(line: &mut Line) -> (Vec<Walked>, Vec<String>) {
    let mut walked = Vec::new();
    let mut problems = Vec::new();
    for row in COMMANDS {
        let id = line.send(row.name, &args_for(row)).await;
        match line.wait(id, parity_env::ANSWER_WITHIN).await {
            Ok(answer) => {
                let kind = if answer.get("ok").is_some() {
                    Kind::Ok
                } else if answer.get("err") == Some(&json!(NOT_IN_BROWSER)) {
                    Kind::Unavailable
                } else if answer.get("err").is_some() {
                    if answer["err"] == json!(ANSWER_UNKNOWN) {
                        problems.push(format!("{}: answered {ANSWER_UNKNOWN}", row.name));
                    }
                    Kind::Err
                } else {
                    problems.push(format!("{}: neither ok nor err", row.name));
                    Kind::Err
                };
                walked.push(Walked { row, kind, answer });
            }
            Err(Failure::Silent) => problems.push(format!("{}: no answer in {:?}", row.name, parity_env::ANSWER_WITHIN)),
            Err(Failure::Closed(code)) => {
                problems.push(format!("{}: the socket was closed ({code:?})", row.name));
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    (walked, problems)
}

// ---- names --------------------------------------------------------------------------------------------------------------

#[test]
fn names_the_table_and_the_golden_hold_the_same_names() {
    let table = table_names();
    let golden = golden_command_names();
    let only_table: Vec<&String> = table.difference(&golden).collect();
    let only_golden: Vec<&String> = golden.difference(&table).collect();
    assert!(only_table.is_empty() && only_golden.is_empty(), "only in the table: {only_table:?}; only in the golden: {only_golden:?}");
    println!("names: the table and the golden hold {} names each", table.len());
}

#[test]
fn names_the_table_holds_no_name_twice() {
    let mut seen = BTreeSet::new();
    for row in COMMANDS {
        assert!(seen.insert(row.name), "{} is in the table twice", row.name);
    }
}

#[test]
fn names_the_argument_names_of_each_row_equal_the_golden_in_order() {
    let mut problems = Vec::new();
    for row in COMMANDS {
        let table: Vec<&str> = row.args.iter().map(|a| a.name).collect();
        match golden_command_args(row.name) {
            Some(golden) if golden.iter().map(String::as_str).collect::<Vec<_>>() == table => {}
            Some(golden) => problems.push(format!("{}: the table has {table:?}, the golden {golden:?}", row.name)),
            None => problems.push(format!("{}: not in the golden", row.name)),
        }
    }
    nothing_wrong("argument names", &problems);
}

// ---- the walk -----------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn walk_every_row_of_the_table_is_answered_once_with_its_id_in_time() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let (walked, problems) = walk(&mut line).await;
    nothing_wrong("the walk", &problems);
    assert_eq!(walked.len(), COMMANDS.len(), "every row was answered");
    let count = |kind: Kind| walked.iter().filter(|w| w.kind == kind).count();
    println!(
        "walk: {} rows sent by name over the socket, {} ok, {} err, {} unavailable in the browser",
        walked.len(),
        count(Kind::Ok),
        count(Kind::Err),
        count(Kind::Unavailable)
    );
    assert!(line.closed.is_none(), "the socket stayed open");
}

#[tokio::test(flavor = "multi_thread")]
async fn walk_the_answers_by_kind_add_up_to_the_table_and_the_worker_ran_what_the_browser_has() {
    let probed = Probed::start(WsOptions::default()).await;
    let (alice, mut line) = signed_in(&probed, "alice").await;
    let (walked, problems) = walk(&mut line).await;
    nothing_wrong("the walk", &problems);
    let unavailable = walked.iter().filter(|w| w.kind == Kind::Unavailable).count();
    assert_eq!(unavailable, unavailable_rows().len(), "the answers of the sentence are the rows the table marks");
    assert_eq!(probed.ran(), COMMANDS.len() - unavailable, "the worker ran every row the browser has and nothing else");
    assert_eq!(probed.env.counts(&alice).workers, 1, "one worker for the person");
    assert_eq!(probed.env.supervisor.stats().worker_deaths, 0, "no worker was declared dead");
}

// ---- shapes -------------------------------------------------------------------------------------------------------------

/// Which side of a row a golden member describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Answer,
    Error,
}

/// A golden member and the commands that return it: the rows whose answer (or error) type is `ty`, or only those named.
struct Pair {
    member: &'static str,
    side: Side,
    ty: &'static str,
    only: &'static [&'static str],
    /// The golden member is a list of variants of the answer and not a list of elements.
    variants: bool,
    /// Why the benign answer of the web host is not compared with this member (a member of the desktop's system).
    exempt: Option<&'static str>,
}

const fn pair(member: &'static str, side: Side, ty: &'static str, variants: bool) -> Pair {
    Pair { member, side, ty, only: &[], variants, exempt: None }
}

const fn only(member: &'static str, side: Side, ty: &'static str, variants: bool, only: &'static [&'static str]) -> Pair {
    Pair { member, side, ty, only, variants, exempt: None }
}

/// The pairing list: each member of the golden file with the type the table gives the commands that return it.
const PAIRS: &[Pair] = &[
    pair("view", Side::Answer, "View", false),
    pair("settings_view", Side::Answer, "SettingsView", false),
    pair("app_info", Side::Answer, "AppInfo", false),
    pair("paste", Side::Answer, "PasteOutcomeMsg", true),
    pair("answers.search", Side::Answer, "SearchMsg", true),
    pair("answers.copy_mode", Side::Answer, "CopyStateMsg", true),
    pair("answers.release_outside", Side::Answer, "Released", true),
    pair("answers.local_network", Side::Answer, "holzwave_host::local_network::LocalNetworkView", true),
    pair("answers.host_shell", Side::Answer, "holzwave_host::host_shell::HostShellView", true),
    pair("answers.window_frame", Side::Answer, "holzwave_host::window_frame::WindowFrameView", true),
    pair("answers.monitor", Side::Answer, "holzwave_core::monitor::Reading", true),
    pair("answers.ai.ask_pending", Side::Answer, "Vec<AskView>", false),
    pair("answers.ai.context_state", Side::Answer, "ContextState", false),
    pair("answers.ai.conversations", Side::Answer, "ConversationList", false),
    pair("answers.ai.offer", Side::Answer, "OfferView", false),
    pair("answers.ai.profiles", Side::Answer, "AiProfiles", false),
    Pair {
        member: "answers.ai.profiles_on_device",
        side: Side::Answer,
        ty: "AiProfiles",
        only: &["ai_profiles"],
        variants: false,
        exempt: Some("the on-device model of the desktop's system; the web host has none"),
    },
    pair("answers.ai.removal", Side::Answer, "RemovalView", false),
    pair("answers.ai.ticket", Side::Answer, "TicketView", false),
    pair("answers.ai.fault", Side::Error, "AiFault", true),
    only("answers.ai.ask_fault", Side::Error, "AiFault", false, &["ai_ask_answer"]),
    pair("answers.integration.card", Side::Answer, "Card", false),
    pair("answers.integration.confirm", Side::Answer, "ConfirmAnswer", true),
    pair("answers.integration.context", Side::Answer, "ContextAnswer", false),
    pair("answers.integration.launch", Side::Answer, "LaunchAnswer", false),
    pair("answers.integration.profiles", Side::Answer, "ProfilesView", false),
    pair("answers.integration.run", Side::Answer, "RunAnswer", false),
    pair("answers.integration.tools", Side::Answer, "Vec<ToolView>", false),
    pair("answers.integration.fault", Side::Error, "IntegrationFault", false),
    only("answers.integration.profile_fault", Side::Error, "IntegrationFault", false, &[
        "integration_profile_save",
        "integration_profile_remove",
        "integration_secret_set",
        "integration_secret_clear",
    ]),
    only("answers.integration.nothing", Side::Answer, "()", false, &["integration_cancel"]),
];

fn squash(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The rows a pair names, read from the table.
fn rows_of(pair: &Pair) -> Vec<&'static Command> {
    COMMANDS
        .iter()
        .filter(|row| {
            let ty = match pair.side {
                Side::Answer => Some(squash(row.answer)),
                Side::Error => row.error.map(squash),
            };
            ty.as_deref() == Some(pair.ty) && (pair.only.is_empty() || pair.only.contains(&row.name))
        })
        .collect()
}

/// The members of the golden file that describe an answer or an error: `view`, `settings_view`, `app_info`, `paste` and the
/// members of `answers` (the members of a family such as `ai` one level down).
fn golden_members() -> Vec<String> {
    let mut members: Vec<String> = ["view", "settings_view", "app_info", "paste"].iter().map(|m| (*m).to_owned()).collect();
    for (name, value) in golden()["answers"].as_object().expect("answers") {
        match value {
            Value::Object(family) => members.extend(family.keys().map(|k| format!("answers.{name}.{k}"))),
            _ => members.push(format!("answers.{name}")),
        }
    }
    members
}

#[test]
fn shapes_every_member_of_the_golden_has_a_pair() {
    let paired: BTreeSet<&str> = PAIRS.iter().map(|p| p.member).collect();
    let unpaired: Vec<String> = golden_members().into_iter().filter(|m| !paired.contains(m.as_str())).collect();
    assert!(unpaired.is_empty(), "golden members with no pair: {unpaired:?}");
    println!("shapes: {} golden members, {} pairs", golden_members().len(), PAIRS.len());
}

#[test]
fn shapes_every_pair_names_a_member_of_the_golden_and_commands_of_the_table() {
    let members: BTreeSet<String> = golden_members().into_iter().collect();
    let mut problems = Vec::new();
    for pair in PAIRS {
        if !members.contains(pair.member) {
            problems.push(format!("{}: not a member of the golden", pair.member));
        }
        for name in pair.only {
            match lookup(name) {
                None => problems.push(format!("{}: names {name}, which is not in the table", pair.member)),
                Some(row) => {
                    let ty = match pair.side {
                        Side::Answer => Some(squash(row.answer)),
                        Side::Error => row.error.map(squash),
                    };
                    if ty.as_deref() != Some(pair.ty) {
                        problems.push(format!("{}: {name} does not return {}", pair.member, pair.ty));
                    }
                }
            }
        }
        if rows_of(pair).is_empty() {
            problems.push(format!("{}: no row of the table returns {}", pair.member, pair.ty));
        }
    }
    nothing_wrong("pairs", &problems);
}

/// Compares an answer with a golden member the way the pair says.
fn compare(pair: &Pair, value: &Value) -> Result<(), String> {
    let golden = golden_at(pair.member).ok_or_else(|| format!("{} is not in the golden", pair.member))?;
    if pair.variants { shape_matches_any(value, golden) } else { same_shape(value, golden) }
}

/// The benign call of every row that a pair names, compared with its member: an `ok` answer by shape, an error by its kind.
/// The result lists, for every member, the rows that answered `ok` (compared) and those that did not (and why).
async fn pair_walk(line: &mut Line) -> (BTreeMap<&'static str, Vec<String>>, Vec<String>) {
    let mut compared: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut problems = Vec::new();
    for pair in PAIRS {
        for row in rows_of(pair) {
            let answer = line.call(row.name, &args_for(row)).await.unwrap_or_else(|f| panic!("{}: {f:?}", row.name));
            let entry = compared.entry(pair.member).or_default();
            if let Some(reason) = pair.exempt {
                entry.push(format!("{} not compared: {reason}", row.name));
                continue;
            }
            match pair.side {
                Side::Answer => match answer.get("ok") {
                    Some(value) => match compare(pair, value) {
                        Ok(()) => entry.push(format!("{} compared", row.name)),
                        Err(difference) => problems.push(format!("{} against {}: {difference}", row.name, pair.member)),
                    },
                    None => entry.push(format!("{} answered an error with the benign arguments", row.name)),
                },
                Side::Error => match answer.get("err") {
                    Some(value) => match compare(pair, value) {
                        Ok(()) => entry.push(format!("{} compared", row.name)),
                        Err(difference) => problems.push(format!("{} error against {}: {difference}", row.name, pair.member)),
                    },
                    None => entry.push(format!("{} answered ok with the benign arguments", row.name)),
                },
            }
        }
    }
    (compared, problems)
}

#[tokio::test(flavor = "multi_thread")]
async fn shapes_every_paired_command_that_answers_has_the_shape_of_its_golden_member() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let (compared, problems) = pair_walk(&mut line).await;
    for (member, rows) in &compared {
        println!("shapes: {member}: {}", rows.join("; "));
    }
    nothing_wrong("shapes", &problems);
}

#[tokio::test(flavor = "multi_thread")]
async fn shapes_an_error_is_a_string_except_the_faults_of_the_assistant_and_the_integrations() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let (walked, problems) = walk(&mut line).await;
    nothing_wrong("the walk", &problems);
    let fault_types: BTreeSet<String> =
        COMMANDS.iter().filter_map(|row| row.error).map(squash).filter(|ty| ty != "String").collect();
    let mut problems = Vec::new();
    for w in walked.iter().filter(|w| w.kind == Kind::Err) {
        let error = &w.answer["err"];
        match w.row.error.map(squash) {
            Some(ty) if ty != "String" => {
                let golden_faults = if ty == "AiFault" { "answers.ai.fault" } else { "answers.integration.fault" };
                let golden = golden_at(golden_faults).expect("a golden fault");
                if let Err(difference) = shape_matches_any(error, golden) {
                    problems.push(format!("{}: {difference}", w.row.name));
                }
            }
            _ => {
                if !error.is_string() {
                    problems.push(format!("{}: an error that is not a string", w.row.name));
                }
            }
        }
    }
    nothing_wrong("errors", &problems);
    println!("shapes: the error types that are not strings: {fault_types:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn shapes_a_real_pane_answers_paste_search_and_copy_mode_as_the_golden() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let pane = start_focused_pane(&mut line).await;
    let mut checks = Vec::new();
    for (command, args, member) in [
        ("paste", json!({"pane": pane, "text": "", "allow_unsafe": false}), "paste"),
        ("paste_cue", json!({"pane": pane}), "paste"),
        ("search", json!({"pane": pane, "op": {"kind": "set", "needle": "find"}}), "answers.search"),
        ("search", json!({"pane": pane, "op": {"kind": "clear"}}), "answers.search"),
        ("copy_mode", json!({"pane": pane, "op": {"kind": "enter"}}), "answers.copy_mode"),
        ("copy_mode", json!({"pane": pane, "op": {"kind": "exit"}}), "answers.copy_mode"),
    ] {
        let result = line.ok(command, &args).await;
        let golden = golden_at(member).expect("a golden member");
        if let Err(difference) = shape_matches_any(&result, golden) {
            panic!("{command} against {member}: {difference}");
        }
        if member == "paste" {
            assert!(golden.as_array().expect("the words").contains(&result), "{command} said {result}");
        }
        checks.push(format!("{command} -> {member}"));
    }
    println!("shapes with a real pane: {}", checks.join("; "));
}

#[tokio::test(flavor = "multi_thread")]
async fn shapes_a_system_monitor_pane_answers_the_reading_of_the_golden() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let opened = line.call("block_open", &json!({"kind": "monitor", "source": "local", "path": "", "placement": "tab"})).await;
    let view = opened.expect("an answer");
    let view = match view.get("ok") {
        Some(view) => view.clone(),
        None => {
            println!("shapes: monitor_sample is not exercised: the monitor pane did not open ({view})");
            return;
        }
    };
    let pane = ws_env::focus(&view);
    let reading = line.ok("monitor_sample", &json!({"pane": pane})).await;
    let golden = golden_at("answers.monitor").expect("a golden member");
    if let Err(difference) = shape_matches_any(&reading, golden) {
        panic!("monitor_sample against answers.monitor: {difference}");
    }
    println!("shapes: monitor_sample on a monitor pane matches answers.monitor (state {})", reading["state"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn shapes_the_assistant_answers_that_need_no_network_have_the_golden_shape() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let pane = start_focused_pane(&mut line).await;
    let profile = line.ok("ai_profile_save", &json!({"input": golden_at("ops.ai_profile_input").expect("ops")[0]})).await;
    let profile = profile.as_str().expect("the id of the profile").to_owned();
    let conversation = line.ok("ai_conversation_new", &json!({})).await.as_str().expect("an id").to_owned();
    let mut report = Vec::new();
    for (command, args, member) in [
        ("ai_context_state", json!({"conversation": conversation, "pane": pane, "profile": profile}), "answers.ai.context_state"),
        (
            "ai_context_preview",
            json!({"conversation": conversation, "pane": pane, "profile": profile, "kinds": ["screen"]}),
            "answers.ai.offer",
        ),
        ("ai_profiles", json!({}), "answers.ai.profiles"),
        ("ai_conversation_list", json!({}), "answers.ai.conversations"),
        ("ai_profile_remove", json!({"profile": profile}), "answers.ai.removal"),
    ] {
        let answer = line.call(command, &args).await.expect("an answer");
        match answer.get("ok") {
            Some(result) => {
                let golden = golden_at(member).expect("a golden member");
                if let Err(difference) = same_shape(result, golden) {
                    panic!("{command} against {member}: {difference}");
                }
                report.push(format!("{command} -> {member}: compared"));
            }
            None => report.push(format!("{command} -> {member}: not compared, it answered {}", answer["err"])),
        }
    }
    println!("shapes of the assistant: {}", report.join("; "));
}

#[test]
fn shapes_the_members_that_need_a_desktop_window_a_provider_or_a_tool_have_a_reason() {
    // Members whose success answer cannot be provoked without a window, a provider on the network or an installed tool. The
    // list is checked against the table: every command named here is in it.
    let reasons: &[(&str, &[&str], &str)] = &[
        ("answers.release_outside", &["release_outside"], "a drag between desktop windows; the browser has no such command"),
        ("answers.local_network", &["local_network_status", "local_network_answer"], "the permission of a desktop; the browser has no such command"),
        ("answers.host_shell", &["host_shell_status", "host_shell_set"], "the sandbox of a Flatpak desktop; the browser has no such command"),
        ("answers.window_frame", &["window_frame_status", "window_frame_set"], "a desktop window frame; the browser has no such command"),
        ("answers.ai.ticket", &["ai_send"], "a turn opens a connection to the provider"),
        ("answers.ai.profiles_on_device", &["ai_profiles"], "an on-device model of the desktop system"),
        ("answers.integration.card", &["integration_prepare"], "needs an installed tool and a verb that writes"),
        ("answers.integration.confirm", &["integration_confirm"], "needs a card of a tool that would run"),
        ("answers.integration.launch", &["integration_launch"], "needs an installed tool; it would start a process"),
        ("answers.integration.run", &["integration_run"], "needs an installed tool; it would run a process"),
    ];
    let mut problems = Vec::new();
    for (member, commands, reason) in reasons {
        if !PAIRS.iter().any(|p| p.member == *member) {
            problems.push(format!("{member}: not in the pairing list"));
        }
        for command in *commands {
            if lookup(command).is_none() {
                problems.push(format!("{member}: {command} is not in the table"));
            }
        }
        println!("not provoked: {member} ({}): {reason}", commands.join(", "));
    }
    nothing_wrong("exempt members", &problems);
}

// ---- the set the browser cannot offer -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn unavailable_every_flagged_row_answers_the_sentence_and_runs_nothing() {
    let probed = Probed::start(WsOptions::default()).await;
    let (alice, mut line) = signed_in(&probed, "alice").await;
    let rows = unavailable_rows();
    let mut problems = Vec::new();
    let before = (probed.ran(), probed.env.counts(&alice));
    for row in &rows {
        // The arguments do not matter: nothing reads them.
        for args in [args_for(row), json!({}), json!({"anything": [1, 2, 3]})] {
            match line.call(row.name, &args).await {
                Ok(answer) if answer.get("err") == Some(&json!(NOT_IN_BROWSER)) => {}
                other => problems.push(format!("{}: {other:?}", row.name)),
            }
        }
    }
    nothing_wrong("unavailable rows", &problems);
    assert_eq!(probed.ran(), before.0, "no unavailable row reached the code of a command");
    assert_eq!(probed.env.counts(&alice), before.1);
    println!("unavailable: {}", rows.iter().map(|r| r.name).collect::<Vec<_>>().join(", "));
    println!("unavailable: {} rows read from the table", rows.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn unavailable_no_other_row_answers_the_sentence() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let (walked, problems) = walk(&mut line).await;
    nothing_wrong("the walk", &problems);
    let mut wrong = Vec::new();
    for w in &walked {
        let flagged = w.row.web == Web::Unavailable;
        let sentence = w.answer.get("err") == Some(&json!(NOT_IN_BROWSER));
        if flagged != sentence {
            wrong.push(format!("{}: flagged {flagged}, answered the sentence {sentence}", w.row.name));
        }
    }
    nothing_wrong("the unavailable set", &wrong);
}

// ---- names that are not in the table ------------------------------------------------------------------------------------

/// The names the endpoint must refuse, with the reason each one is interesting.
fn unregistered() -> Vec<(String, &'static str)> {
    let real = COMMANDS.first().expect("a row").name;
    vec![
        (format!("{real}x"), "a name and one more letter"),
        (real.to_ascii_uppercase(), "the wrong case"),
        (format!("{real} "), "a trailing space"),
        (real[..real.len() - 1].to_owned(), "a prefix"),
        (String::new(), "an empty name"),
        ("a".repeat(65), "65 characters"),
        (format!("../{real}"), "path-like"),
        ("__proto__".to_owned(), "a name of another system"),
        ("constructor".to_owned(), "a name of another system"),
        (format!("{real}\u{0}"), "a NUL"),
        ("window_ready.".to_owned(), "a real name and a dot"),
    ]
}

#[test]
fn unregistered_none_of_the_names_is_in_the_table() {
    for (name, why) in unregistered() {
        assert!(lookup(&name).is_none(), "{why}: {name:?} is in the table");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unregistered_each_name_is_answered_unknown_and_starts_nothing() {
    let probed = Probed::start(WsOptions::default()).await;
    let (alice, line) = signed_in(&probed, "alice").await;
    let before = (probed.ran(), probed.env.counts(&alice), probed.env.launcher.launches());
    let mut results = Vec::new();
    let mut problems = Vec::new();
    // Every name is a violation of the socket; a socket may take a few, so the names go out over several.
    for chunk in unregistered().chunks(5) {
        let mut socket = fresh(&probed, &alice).await;
        for (name, why) in chunk {
            let id = socket.send_unchecked(name, &json!({})).await;
            match socket.wait(id, parity_env::ANSWER_WITHIN).await {
                Ok(answer) if answer["err"] == json!(ANSWER_UNKNOWN) => results.push(format!("{why}: {ANSWER_UNKNOWN}")),
                Ok(answer) => problems.push(format!("{why}: {answer}")),
                Err(failure) => problems.push(format!("{why}: {failure:?}")),
            }
        }
        // The next command works on the same socket.
        let view = socket.call("get_layout", &json!({})).await;
        if !view.as_ref().is_ok_and(|a| a.get("ok").is_some()) {
            problems.push(format!("a normal command after the names: {view:?}"));
        }
    }
    drop(line);
    nothing_wrong("unregistered names", &problems);
    let after = (probed.ran(), probed.env.launcher.launches());
    assert_eq!(probed.ran_names().iter().filter(|n| **n != "get_layout").count(), 0, "only the normal commands ran");
    assert_eq!(after.1, before.2, "no worker was started");
    println!("unregistered: {}", results.join("; "));
}

#[tokio::test(flavor = "multi_thread")]
async fn unregistered_a_name_without_an_id_costs_a_violation_and_gets_no_answer() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    // A name that is not a string cannot be read as a command and there is no id to answer.
    line.send_text(r#"{"cmd":"get_layout","args":{}}"#).await;
    line.send_text(r#"{"id":1,"cmd":7,"args":{}}"#).await;
    line.pump(Duration::from_millis(300)).await;
    assert!(line.closed.is_none(), "the socket stays open");
    let view = line.call("get_layout", &json!({})).await.expect("a normal command works afterwards");
    assert!(view.get("ok").is_some());
}

// ---- events -------------------------------------------------------------------------------------------------------------

/// What `provoke_events` saw.
struct Provoked {
    line: Line,
    pane: u64,
}

/// A shell in a second pane prints a title (OSC 0), a working directory (OSC 7), a command end (OSC 133) and a bell, the layout
/// is changed by a rename, a setting is set to the value it has and the shell ends. Returns the socket with every event kept.
async fn provoke_events(probed: &Probed) -> Provoked {
    let (_, mut line) = signed_in(probed, "alice").await;
    let first = start_focused_pane(&mut line).await;
    let split = line.ok("split", &json!({"column": false})).await;
    let pane = ws_env::focus(&split);
    assert_ne!(pane, first);
    line.ok("start_pane", &json!({"pane": pane, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16})).await;
    let settings = line.ok("get_settings", &json!({})).await;
    let (key, value) = settings["settings"]
        .as_object()
        .and_then(|all| all.iter().find(|(_, v)| v.is_boolean()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .expect("a boolean setting");
    line.ok("set_setting", &json!({"key": key, "value": !value.as_bool().unwrap_or(false)})).await;
    line.ok("integration_tools", &json!({})).await;
    line.ok("integration_context", &json!({"refresh": false})).await;
    line.ok("rename_pane", &json!({"pane": pane, "name": "renamed"})).await;
    let program = "printf '\\033]0;hw-title\\007'; printf '\\033]7;file://localhost/tmp\\007'; printf '\\033]133;D;3\\007'; printf '\\007'; echo hw-printed\r";
    line.ok("text", &json!({"pane": pane, "text": program})).await;
    let shown = line.until(Duration::from_secs(15), |events| {
        events.iter().any(|e| e["event"] == "frame" && e["payload"].to_string().contains("hw-printed"))
    })
    .await;
    if !shown {
        for e in &line.events {
            let t = e.to_string();
            println!("EVENT {}", &t[..t.len().min(300)]);
        }
    }
    assert!(shown, "the shell printed");
    line.pump(Duration::from_millis(300)).await;
    line.ok("text", &json!({"pane": pane, "text": "exit\r"})).await;
    let ended = line.until(Duration::from_secs(15), |events| events.iter().any(|e| e["event"] == "pane-exit")).await;
    assert!(ended, "the shell ended");
    line.pump(Duration::from_millis(300)).await;
    Provoked { line, pane }
}

/// The names of events a page may be sent: the keys of the golden `events` and the names the golden lists, plus `hw-session`.
fn accepted_event_names() -> BTreeSet<String> {
    let mut names = golden_event_names();
    names.extend(golden_at("events.names").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned));
    names.insert(String::from("hw-session"));
    names
}

#[tokio::test(flavor = "multi_thread")]
async fn events_every_name_seen_is_in_the_golden_or_is_one_of_the_three_of_the_web() {
    let probed = Probed::start(WsOptions::default()).await;
    let provoked = provoke_events(&probed).await;
    let accepted = accepted_event_names();
    let seen: BTreeSet<String> = provoked.line.events.iter().filter_map(|e| e["event"].as_str()).map(str::to_owned).collect();
    let unknown: Vec<&String> = seen.iter().filter(|n| !accepted.contains(*n)).collect();
    assert!(unknown.is_empty(), "events the desktop does not have: {unknown:?}");
    for must in ["frame", "layout", "hw-session"] {
        assert!(seen.contains(must), "{must} was not seen");
    }
    let listed = golden_at("events.names").and_then(Value::as_array).expect("the golden lists the names");
    for must in ["frame", "layout"] {
        assert!(listed.iter().any(|n| n == must), "the golden list of names holds {must}");
    }
    println!("events seen: {seen:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn events_the_payloads_have_the_members_of_the_golden_shapes() {
    let probed = Probed::start(WsOptions::default()).await;
    let provoked = provoke_events(&probed).await;
    let mut problems = Vec::new();
    let mut checked: BTreeMap<String, usize> = BTreeMap::new();
    for event in &provoked.line.events {
        let name = event["event"].as_str().unwrap_or("?");
        let payload = &event["payload"];
        let result = match name {
            "frame" => {
                let frames: Vec<Value> = parity_env::frame_fixture()["steps"]
                    .as_array()
                    .expect("steps")
                    .iter()
                    .map(|step| step["frame"].clone())
                    .collect();
                shape_matches_any(payload, &Value::Array(frames))
            }
            "layout" => same_shape(payload, golden_at("view").expect("the golden view")),
            "hw-session" => same_shape(payload, &json!({"state": "ok", "seconds": 1})),
            other => match golden_at(&format!("events.{other}")) {
                Some(golden) => shape_matches_any(payload, golden),
                None => Err(String::from("no golden shape")),
            },
        };
        *checked.entry(name.to_owned()).or_default() += 1;
        if let Err(difference) = result {
            problems.push(format!("{name}: {difference}"));
        }
    }
    nothing_wrong("event payloads", &problems);
    println!("events payloads compared: {checked:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn events_a_settings_change_sends_the_golden_settings_event() {
    let probed = Probed::start(WsOptions::default()).await;
    let provoked = provoke_events(&probed).await;
    let settings = provoked.line.events_named("settings");
    assert!(!settings.is_empty(), "set_setting sent no settings event");
    let golden = golden_at("events.settings").expect("the golden settings event");
    for event in settings {
        same_shape(&event["payload"], golden).unwrap_or_else(|d| panic!("settings event: {d}"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn events_a_frame_has_the_members_of_the_fixture_and_the_numbers_of_its_pane() {
    let probed = Probed::start(WsOptions::default()).await;
    let provoked = provoke_events(&probed).await;
    let frames = provoked.line.frames_of(provoked.pane);
    assert!(!frames.is_empty(), "the pane sent frames");
    assert_eq!(frames[0]["full"], json!(true), "the first frame of a pane is a full one");
    parity_env::strictly_increasing(&frames).unwrap_or_else(|d| panic!("{d}"));
    let fixture_keys: BTreeSet<String> =
        parity_env::frame_fixture()["steps"][0]["frame"].as_object().expect("a frame").keys().cloned().collect();
    for frame in &frames {
        let keys: BTreeSet<String> = frame.as_object().expect("a frame").keys().cloned().collect();
        assert_eq!(keys, fixture_keys, "the members of a frame");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn events_the_names_that_were_not_provoked_are_listed_with_the_reason() {
    let probed = Probed::start(WsOptions::default()).await;
    let provoked = provoke_events(&probed).await;
    let seen: BTreeSet<String> = provoked.line.events.iter().filter_map(|e| e["event"].as_str()).map(str::to_owned).collect();
    let reasons: BTreeMap<&str, &str> = BTreeMap::from([
        ("ai", "an assistant turn that opens a connection to a provider"),
        ("ai-ask", "an assistant request that waits for the person's consent"),
        ("drag-end", "a drag between desktop windows"),
        ("drag-hover", "a drag between desktop windows"),
        ("incoming-drop", "a drag between desktop windows"),
        ("history-open", "a menu of the desktop window"),
        ("login-prompt", "a login prompt of a remote shell"),
        ("login-prompt-end", "a login prompt of a remote shell"),
        ("names", "the golden lists the names; it is not an event"),
    ]);
    let mut missing = Vec::new();
    for name in accepted_event_names() {
        if seen.contains(&name) || name == "hw-session" {
            continue;
        }
        match reasons.get(name.as_str()) {
            Some(reason) => println!("events not provoked: {name}: {reason}"),
            None => missing.push(name),
        }
    }
    assert!(missing.is_empty(), "events neither provoked nor explained: {missing:?}");
    let still: Vec<&&str> = reasons.keys().filter(|n| seen.contains(**n)).collect();
    println!("events with a reason that were provoked after all: {still:?}");
}

// ---- no session, no command ---------------------------------------------------------------------------------------------

/// The text of a request for `row` with its benign arguments.
fn request_text(row: &Command) -> String {
    json!({"id": 1, "cmd": row.name, "args": args_for(row)}).to_string()
}

/// The layout (without its counter of views made) and the settings a person gets from a new socket.
async fn answers_of(probed: &Probed, person: &Person) -> (Value, Value) {
    let mut line = fresh(probed, person).await;
    let mut layout = line.ok("get_layout", &json!({})).await;
    layout.as_object_mut().expect("a view").remove("rev");
    let settings = line.ok("get_settings", &json!({})).await;
    (layout, settings)
}

/// A person with a pane that has a name, and another person who never opens a socket.
struct Pair2 {
    alice: Person,
    bob: Person,
    layout: Value,
    settings: Value,
}

async fn two_people(probed: &Probed) -> Pair2 {
    let alice = probed.env.person("alice");
    let bob = probed.env.person("bob");
    // No shell runs in this layout, so nothing but a command can make an event: an event after the sign-out is then a leak.
    let mut line = fresh(probed, &alice).await;
    let view = line.ok("get_layout", &json!({})).await;
    line.ok("rename_pane", &json!({"pane": ws_env::focus(&view), "name": "mine"})).await;
    drop(line);
    let (layout, settings) = answers_of(probed, &alice).await;
    Pair2 { alice, bob, layout, settings }
}

/// Everything the server counts, to be the same after the attempts.
fn held(probed: &Probed, who: &Pair2) -> (usize, usize, (usize, usize), ws_env::Counts, ws_env::Counts) {
    (probed.ran(), probed.env.launcher.launches(), probed.env.totals(), probed.env.counts(&who.alice), probed.env.counts(&who.bob))
}

/// After the attempts: nothing ran, no worker started for bob, and the answers of alice are the ones from before.
async fn nothing_changed(
    probed: &Probed,
    who: &Pair2,
    before: &(usize, usize, (usize, usize), ws_env::Counts, ws_env::Counts),
    own_calls: usize,
    own_audit_lines: usize,
) {
    let after = held(probed, who);
    assert_eq!(after.0, before.0 + own_calls, "no command was forwarded to a worker but the {own_calls} the test made itself");
    assert_eq!(after.1, before.1, "no worker was started");
    assert_eq!(after.3.audit_lines, before.3.audit_lines + own_audit_lines, "no audit line was written but the {own_audit_lines} of the sign-outs and expiries");
    assert_eq!(after.4.workers, 0, "the person who never signed in has no worker");
    assert!(
        probed.env.launcher.requests().iter().all(|(uid, _)| *uid != who.bob.key.uid),
        "no worker was requested for the person who never signed in"
    );
    let (layout, settings) = answers_of(probed, &who.alice).await;
    assert_eq!(layout, who.layout, "the layout is byte for byte what it was");
    assert_eq!(settings, who.settings, "the settings are byte for byte what they were");
    assert_eq!(layout.to_string(), who.layout.to_string());
}

/// Sends every row on a socket that the server closes at once and returns what did not close with `expect`.
async fn refused_for_every_row(probed: &Probed, cookie: Option<&str>, origin: Option<&str>, expect: u16) -> Vec<String> {
    let mut problems = Vec::new();
    for row in COMMANDS {
        let mut client = probed.env.connect(cookie, origin).await;
        client.send_text(&request_text(row)).await;
        match client.close_code(PATIENCE).await {
            Some((code, _)) if code == expect => {}
            other => problems.push(format!("{}: closed with {other:?}, {expect} was due", row.name)),
        }
    }
    problems
}

#[tokio::test(flavor = "multi_thread")]
async fn no_session_without_a_cookie_no_row_runs_and_the_upgrade_is_closed_4401() {
    let probed = Probed::start(WsOptions::default()).await;
    let who = two_people(&probed).await;
    let before = held(&probed, &who);
    let problems = refused_for_every_row(&probed, None, Some(edge::ORIGIN), 4401).await;
    nothing_wrong("no cookie", &problems);
    nothing_changed(&probed, &who, &before, 0, 0).await;
    println!("no session (a) no cookie: {} rows; counts alice {:?} -> {:?}", COMMANDS.len(), before.3, probed.env.counts(&who.alice));
}

#[tokio::test(flavor = "multi_thread")]
async fn no_session_a_live_cookie_with_a_foreign_origin_runs_no_row_and_is_closed_4403() {
    let probed = Probed::start(WsOptions::default()).await;
    let who = two_people(&probed).await;
    let before = held(&probed, &who);
    let mut problems = refused_for_every_row(&probed, Some(&who.alice.signed.cookie), Some("https://evil.example"), 4403).await;
    problems.extend(refused_for_every_row(&probed, Some(&who.bob.signed.cookie), Some("https://evil.example"), 4403).await);
    problems.extend(refused_for_every_row(&probed, Some(&who.bob.signed.cookie), None, 4403).await);
    nothing_wrong("foreign origin", &problems);
    nothing_changed(&probed, &who, &before, 0, 0).await;
    println!("no session (b) foreign origin: {} rows x 3; counts alice {:?} -> {:?}", COMMANDS.len(), before.3, probed.env.counts(&who.alice));
}

#[tokio::test(flavor = "multi_thread")]
async fn no_session_an_expired_cookie_runs_no_row_and_is_closed_4401() {
    let probed = Probed::start(WsOptions::default()).await;
    let who = two_people(&probed).await;
    let before = held(&probed, &who);
    probed.env.test.clock.advance(edge::IDLE + 60);
    let mut problems = refused_for_every_row(&probed, Some(&who.alice.signed.cookie), Some(edge::ORIGIN), 4401).await;
    problems.extend(refused_for_every_row(&probed, Some(&who.bob.signed.cookie), Some(edge::ORIGIN), 4401).await);
    nothing_wrong("expired session", &problems);
    // The one thing an expired session leaves is its own `session.expired` line, once for each of the two people however many
    // rows were tried with the cookie.
    nothing_changed(&probed, &who, &before, 0, 2).await;
    println!("no session (c) expired: {} rows x 2; counts alice {:?} -> {:?}", COMMANDS.len(), before.3, probed.env.counts(&who.alice));
}

#[tokio::test(flavor = "multi_thread")]
async fn no_session_an_open_socket_right_after_sign_out_runs_no_row_and_gets_no_event() {
    let probed = Probed::start(WsOptions::default()).await;
    let who = two_people(&probed).await;
    let before = held(&probed, &who);
    let mut problems = Vec::new();
    let mut own_calls = 0;
    let mut signed_out = 0;
    for row in COMMANDS {
        let signed = probed.env.sign_in_again(&who.alice);
        let mut line = Line::new(probed.env.connect_as(&signed).await);
        if !line.until(PATIENCE, |events| events.iter().any(|e| e["event"] == "hw-session")).await {
            problems.push(format!("{}: no session message", row.name));
            continue;
        }
        // The replay of titles and directories comes with the attach; a command answered after it means it has arrived.
        own_calls += 1;
        if line.call("get_layout", &json!({})).await.is_err() {
            problems.push(format!("{}: the socket gave no answer before the sign-out", row.name));
            continue;
        }
        let request = edge_tls::Request::new("POST", "/auth/logout")
            .header("Origin", edge::ORIGIN)
            .header("Content-Type", "application/json")
            .header("X-CSRF-Token", &signed.csrf)
            .cookie(&signed.cookie);
        let request = edge_tls::Request { body: b"{}".to_vec(), ..request };
        let reply = probed.env.plain(request).await;
        if reply.status / 100 != 2 {
            problems.push(format!("{}: the sign-out was answered {}", row.name, reply.status));
            continue;
        }
        signed_out += 1;
        let audit = probed.env.counts(&who.alice).audit_lines;
        let seen = line.events.len();
        line.send_text(&request_text(row)).await;
        line.pump(Duration::from_secs(5)).await;
        if line.closed != Some(Some(4401)) {
            problems.push(format!("{}: closed with {:?}, 4401 was due", row.name, line.closed));
        }
        let late: Vec<String> = line.events[seen..]
            .iter()
            .filter(|e| e["event"] != "hw-session")
            .map(|e| e["event"].as_str().unwrap_or("?").to_owned())
            .collect();
        if !late.is_empty() {
            problems.push(format!("{}: events after the sign-out: {late:?}", row.name));
        }
        if let Some(answer) = line.strays.iter().find(|a| a.get("ok").is_some()) {
            problems.push(format!("{}: answered ok after the sign-out: {answer}", row.name));
        }
        if probed.env.counts(&who.alice).audit_lines != audit {
            problems.push(format!("{}: an audit line was written after the sign-out", row.name));
        }
    }
    nothing_wrong("sign-out", &problems);
    nothing_changed(&probed, &who, &before, own_calls, signed_out).await;
    println!("no session (d) sign-out: {} rows; counts alice {:?} -> {:?}", COMMANDS.len(), before.3, probed.env.counts(&who.alice));
}

// ---- hostile arguments --------------------------------------------------------------------------------------------------

const CANARY: &str = "HW-CANARY-9d41f6c2";

/// How many requests one socket carries before the walk takes a new one (the rate bucket is never what is tested).
const REQUESTS_PER_SOCKET: usize = 250;

/// How many violations one socket takes (the budget closes the socket at the ninth in a minute).
const VIOLATIONS_PER_SOCKET: usize = 6;

/// One answer of the hostile walk.
struct Heard {
    command: &'static str,
    argument: &'static str,
    label: String,
    answer: Value,
}

/// The sockets of the hostile walk: a new one after a few hundred requests or a few violations, so that neither the rate nor
/// the violation budget of the endpoint is the thing under test.
struct Hostile<'a> {
    probed: &'a Probed,
    person: &'a Person,
    line: Line,
    sent: usize,
    violations: usize,
    sockets: usize,
    worst: Duration,
    heard: Vec<Heard>,
    problems: Vec<String>,
}

impl<'a> Hostile<'a> {
    async fn start(probed: &'a Probed, person: &'a Person) -> Hostile<'a> {
        let line = fresh(probed, person).await;
        Hostile { probed, person, line, sent: 0, violations: 0, sockets: 1, worst: Duration::ZERO, heard: Vec::new(), problems: Vec::new() }
    }

    async fn rotate_if_needed(&mut self, violation: bool) {
        if self.sent >= REQUESTS_PER_SOCKET || (violation && self.violations >= VIOLATIONS_PER_SOCKET) || self.line.closed.is_some() {
            self.line = fresh(self.probed, self.person).await;
            self.sockets += 1;
            self.sent = 0;
            self.violations = 0;
        }
    }

    /// Sends `args` (JSON text or not) to a row and records the answer, the latency and any problem.
    async fn request(&mut self, row: &'static Command, argument: &'static str, label: String, args: &str, violation: bool) {
        self.rotate_if_needed(violation).await;
        self.sent += 1;
        self.violations += usize::from(violation);
        let started = tokio::time::Instant::now();
        let id = self.line.send_args_text(row.name, args).await;
        match self.line.wait(id, parity_env::ANSWER_WITHIN).await {
            Ok(answer) => {
                self.worst = self.worst.max(started.elapsed());
                if answer.get("ok").is_none() && answer.get("err").is_none() {
                    self.problems.push(format!("{} / {label}: neither ok nor err", row.name));
                }
                self.heard.push(Heard { command: row.name, argument, label, answer });
            }
            Err(failure) => {
                self.problems.push(format!("{} / {label}: {failure:?}", row.name));
            }
        }
        tokio::time::sleep(Duration::from_millis(3)).await;
    }

    /// The state of the worker after a row: it is still one worker, nobody was declared dead, and a normal command works.
    async fn check_alive(&mut self, row: &str) -> Option<usize> {
        let stats = self.probed.env.supervisor.stats();
        if stats.workers != 1 || stats.worker_deaths != 0 || stats.overflow_closes != 0 {
            self.problems.push(format!("after {row}: workers {}, deaths {}, overflow closes {}", stats.workers, stats.worker_deaths, stats.overflow_closes));
        }
        self.rotate_if_needed(false).await;
        match self.line.call("get_layout", &json!({})).await {
            // The panes of every workspace: the view lists only those of the one in front.
            Ok(answer) => answer["ok"]["workspaces"].as_array().map(|all| all.iter().filter_map(|w| w["panes"].as_u64()).sum::<u64>() as usize),
            Err(failure) => {
                self.problems.push(format!("after {row}: a normal command got {failure:?}"));
                None
            }
        }
    }
}

/// The errors of the desktop's own core and host that quote the argument they refuse back to the person who sent it:
/// `(command, argument)`. They are the desktop's sentences (its own tests pin them), only the sender sees them, and the
/// host bounds the length of two of them. They are reported to the owner; the test lets exactly these through, fails on any
/// other echo and fails when an entry stops echoing, so the list can only get shorter.
const KNOWN_ECHOES: &[(&str, &str)] = &[
    ("file_grant", "source"),
    ("files_list", "source"),
    ("file_open", "source"),
    ("file_stat", "source"),
    ("file_save", "source"),
    ("block_open", "source"),
    ("connect", "id"),
    ("connection_delete", "id"),
    ("set_setting", "key"),
    ("reset_setting", "key"),
];

/// Commands whose `ok` answer is made of what they were given: a view with the names of panes, tabs and workspaces, and the
/// rendering of a text.
fn returns_what_it_was_given(command: &str) -> bool {
    lookup(command).is_some_and(|row| row.group == "layout" || command == "markdown_render")
}

/// The canary in answers: an error or a command that does not return what it was given may not hold it, except the known echoes.
/// The result is the new echoes and the known ones that did not echo.
fn echoes(heard: &[Heard]) -> (Vec<String>, Vec<String>) {
    let mut new = Vec::new();
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    for h in heard.iter().filter(|h| h.answer.to_string().contains(CANARY)) {
        let is_error = h.answer.get("err").is_some();
        if !is_error && returns_what_it_was_given(h.command) {
            continue;
        }
        if is_error && KNOWN_ECHOES.contains(&(h.command, h.argument)) {
            seen.insert((h.command, h.argument));
            continue;
        }
        new.push(format!("{} / {}: {}", h.command, h.label, h.answer));
    }
    let stale = KNOWN_ECHOES.iter().filter(|e| !seen.contains(*e)).map(|(c, a)| format!("{c} / {a} no longer echoes")).collect();
    (new, stale)
}

fn log_problems(probed: &Probed) -> Vec<String> {
    probed
        .log_lines()
        .into_iter()
        .filter(|line| {
            let low = line.to_ascii_lowercase();
            line.contains(CANARY) || low.contains("panic") || low.contains("internal error")
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_every_declared_argument_replaced_in_turn_ends_in_a_plain_answer() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut run = Hostile::start(&probed, &alice).await;
    let mut panes = run.check_alive("the start").await.expect("the panes");
    let mut variants = 0;
    for row in COMMANDS {
        let ok_before = run.heard.iter().filter(|h| h.answer.get("ok").is_some()).count();
        for (at, arg) in row.args.iter().enumerate() {
            for (label, text) in hostile_texts(CANARY) {
                let args = args_text_with(row, at, &text);
                variants += 1;
                // A number the JSON reader cannot hold makes the whole message invalid: it is a violation of the socket.
                run.request(row, arg.name, label.to_owned(), &args, label == "overflow").await;
            }
        }
        let accepted = run.heard.iter().filter(|h| h.answer.get("ok").is_some()).count() > ok_before;
        if let Some(now) = run.check_alive(row.name).await {
            if now < panes {
                run.problems.push(format!("{}: the layout lost panes ({panes} -> {now})", row.name));
            }
            if now > panes && !accepted {
                run.problems.push(format!("{}: the layout gained panes ({panes} -> {now}) although every answer was an error", row.name));
            }
            panes = now;
        }
    }
    let (leaks, stale) = echoes(&run.heard);
    let mut problems = std::mem::take(&mut run.problems);
    problems.extend(leaks.iter().map(|l| format!("echo: {l}")));
    problems.extend(stale.iter().map(|l| format!("stale: {l}")));
    problems.extend(log_problems(&probed).into_iter().map(|l| format!("log: {l}")));
    println!(
        "hostile: {variants} variants over {} sockets, worst latency {:?}, {} answered ok, {} answered err",
        run.sockets,
        run.worst,
        run.heard.iter().filter(|h| h.answer.get("ok").is_some()).count(),
        run.heard.iter().filter(|h| h.answer.get("err").is_some()).count()
    );
    nothing_wrong("hostile arguments", &problems);
    assert!(run.worst < parity_env::ANSWER_WITHIN, "a request took {:?}", run.worst);
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_the_whole_args_replaced_by_null_an_array_a_string_or_a_number_ends_in_a_plain_answer() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut run = Hostile::start(&probed, &alice).await;
    let mut variants = 0;
    for row in COMMANDS {
        for (label, text) in hostile_whole_args() {
            variants += 1;
            // None of the four is an object or absent, so each is a violation of the socket (the sockets are changed in time).
            run.request(row, "", format!("args = {label}"), text, true).await;
        }
    }
    run.check_alive("the end").await;
    let mut problems = std::mem::take(&mut run.problems);
    problems.extend(log_problems(&probed).into_iter().map(|l| format!("log: {l}")));
    // What each replacement came to: an object, or no `args` at all, is the only thing the endpoint passes on; `null` is neither.
    let mut outcomes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for h in &run.heard {
        let text = h.answer.get("err").map_or_else(|| String::from("ok"), |e| e.to_string().chars().take(60).collect());
        outcomes.entry(h.label.clone()).or_default().insert(text);
    }
    for (label, texts) in &outcomes {
        let shown: Vec<&String> = texts.iter().take(4).collect();
        println!("hostile whole args {label}: {} distinct answers, e.g. {shown:?}", texts.len());
    }
    for label in ["args = null", "args = array", "args = string", "args = number"] {
        let texts = &outcomes[label];
        assert!(texts.len() == 1 && texts.iter().all(|t| t.contains("The message is not valid.")), "{label}: {texts:?}");
    }
    println!("hostile whole args: {variants} variants over {} sockets, worst latency {:?}", run.sockets, run.worst);
    nothing_wrong("hostile arguments", &problems);
}

/// The text of a request for `name` that is exactly `total` bytes long.
fn request_of_length(name: &str, total: usize) -> String {
    let base = format!(r#"{{"id":1,"cmd":"{name}","args":{{"p":""}}}}"#);
    assert!(total >= base.len(), "{total} is shorter than the smallest request");
    format!(r#"{{"id":1,"cmd":"{name}","args":{{"p":"{}"}}}}"#, "x".repeat(total - base.len()))
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_a_message_over_the_cap_of_its_command_is_refused_and_one_at_the_cap_is_not() {
    let probed = Probed::start(WsOptions::default()).await;
    let alice = probed.env.person("alice");
    let mut caps: Vec<usize> = COMMANDS.iter().map(|c| c.cap).collect();
    caps.sort_unstable();
    caps.dedup();
    let largest = *caps.last().expect("a cap");
    let mut report = Vec::new();
    let mut problems = Vec::new();
    for cap in caps {
        let row = COMMANDS.iter().find(|c| c.cap == cap && c.web != Web::Unavailable).expect("a row of the class");
        // At the cap: the command is read and answered (an unknown member is refused by the command, not by the size).
        let mut line = fresh(&probed, &alice).await;
        line.client.send_text(&request_of_length(row.name, cap)).await;
        match line.wait(1, Duration::from_secs(30)).await {
            Ok(answer) if answer["err"] != json!(ANSWER_TOO_LARGE) => report.push(format!("{cap} bytes ({}): admitted", row.name)),
            other => problems.push(format!("{} at its cap {cap}: {other:?}", row.name)),
        }
        // One over: a command with a smaller cap than the largest is answered; one at the largest cap closes the socket.
        let mut line = fresh(&probed, &alice).await;
        line.client.send_text(&request_of_length(row.name, cap + 1)).await;
        if cap < largest {
            match line.wait(1, Duration::from_secs(30)).await {
                Ok(answer) if answer["err"] == json!(ANSWER_TOO_LARGE) => report.push(format!("{} bytes ({}): refused with the sentence", cap + 1, row.name)),
                other => problems.push(format!("{} one over its cap {cap}: {other:?}", row.name)),
            }
        } else {
            match line.client.close_code(Duration::from_secs(30)).await {
                Some((1009, _)) => report.push(format!("{} bytes ({}): the socket is closed 1009", cap + 1, row.name)),
                other => problems.push(format!("{} one over the largest cap {cap}: {other:?}", row.name)),
            }
        }
    }
    println!("hostile caps: {}", report.join("; "));
    nothing_wrong("caps", &problems);
    let mut line = fresh(&probed, &alice).await;
    assert!(line.call("get_layout", &json!({})).await.expect("an answer").get("ok").is_some(), "a normal command works afterwards");
    assert!(log_problems(&probed).is_empty());
}

/// One pixel (red 0, green 0, blue 0, alpha 255) shown in one cell with the id 5, typed into the shell of the pane.
const TYPED_PICTURE: &str = "printf '\\033_Ga=T,f=32,s=1,v=1,i=5,c=1,r=1;AAAA/w==\\033\\\\'; echo hw-pic-done\r";

#[tokio::test(flavor = "multi_thread")]
async fn images_a_picture_arrives_as_a_placement_and_its_pixels_arrive_as_raw_bytes_over_the_socket() {
    let probed = Probed::start(WsOptions::default()).await;
    let (_, mut line) = signed_in(&probed, "alice").await;
    let pane = start_focused_pane(&mut line).await;
    line.ok("text", &json!({"pane": pane, "text": TYPED_PICTURE})).await;
    let shown = line
        .until(Duration::from_secs(15), |events| {
            events.iter().any(|e| e["event"] == "frame" && e["payload"]["images"].as_array().is_some_and(|list| !list.is_empty()))
        })
        .await;
    assert!(shown, "no frame carried the placement");

    // The placement is one array of sixteen numbers, the first is the id and the second the generation.
    let frames = line.frames_of(pane);
    let with = frames.iter().find(|f| f["images"].as_array().is_some_and(|list| !list.is_empty())).expect("a frame with images");
    let placement = with["images"][0].as_array().expect("an array");
    assert_eq!(placement.len(), 16);
    assert!(placement.iter().all(Value::is_number));
    assert_eq!(placement[0], json!(5));
    let generation = placement[1].as_u64().expect("a generation");
    // The pixels: a 16-byte header and the four bytes, as a binary message of the socket.
    let args = json!({"pane": pane, "image": 5, "generation": generation, "offset": 0});
    let bytes = line.raw_call("image_data", &args).await.expect("raw bytes");
    assert_eq!(bytes, [0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 255]);

    // A picture that is not there is refused with the one text, over the same socket.
    for (image, generation) in [(5, generation + 1), (6, generation), (5, 0)] {
        let refused = line.raw_call("image_data", &json!({"pane": pane, "image": image, "generation": generation, "offset": 0})).await;
        assert_eq!(refused.expect_err("refused")["err"], json!("no such image"));
    }

    // The switch has no pane and answers nothing.
    assert_eq!(line.ok("set_image_animation", &json!({"enabled": false})).await, Value::Null);
    assert!(log_problems(&probed).is_empty());
}
