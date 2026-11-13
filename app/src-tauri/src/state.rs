//! The test-only home of the protocol golden: the check that what the webview depends on (the commands, their argument
//! names and the shape of every payload) is generated from the real Rust types and read by the TypeScript tests too.
//!
//! It stays in the app crate because it builds the wire types of the command wrappers (`commands.rs`) and reads the
//! source text of the wrappers and of `lib.rs`. The state it samples is the host's.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use holzwave_core::engine::PasteOutcome;
use holzwave_core::keybindings::Keybindings;
use holzwave_core::layout::{Layout, PaneId};
use holzwave_core::settings::Settings;
use holzwave_core::ssh::PromptLine;
use holzwave_core::theme::ThemeRegistry;
use holzwave_core::wire::FrameMsg;
use serde::Serialize;

use holzwave_host::ai_host::AiPayload;
use holzwave_host::integrate_host::{ContextAnswer, IntegrationEvent, IntegrationFault};
use holzwave_host::login_prompt::PromptEnd;
use holzwave_host::windows::DropKind;

use holzwave_host::state::*;

/// The protocol golden (`app/tests/fixtures/protocol.json`): what the webview depends on, generated from the real
/// Rust types and read by the TypeScript tests too, so that a renamed argument or a changed payload fails on both
/// sides at once.
///
/// `UPDATE_FIXTURES=1` rewrites the file, as for the frame golden of the core.
#[cfg(test)]
mod protocol {
    use super::*;
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use holzwave_host::ai_device::{DeviceState, DeviceView};
    use holzwave_host::host_shell::HostShellView;
    use holzwave_host::window_frame::WindowFrameView;
    use holzwave_host::server::{ServerView, SignInPoll, SignInView};
    use holzwave_host::sessions::SessionRow;
    use holzwave_host::local_network::LocalNetworkView;
    use holzwave_core::local_network::State as LnState;
    use holzwave_host::ai_host::{AiFault, AiProfiles, ConversationList, ProfileInputMsg, RemovalView, TicketView};
    use crate::commands::{
        CopyMotionMsg, CopyOpMsg, CopyStateMsg, MouseActionMsg, MouseButtonMsg, PasteOutcomeMsg, SearchOpMsg,
        SelectModeMsg, SelectOpMsg,
    };
    use holzwave_core::ai::profiles::{KeyError, KeySlot, KeyStatus, Preset, ProfileView, Removal};
    use holzwave_core::ai::service::events::{AiEvent, AiEventKind};
    use holzwave_core::ai::service::{
        AskEvent, AskFault, AskView, ContextState, FindingView, ItemView, OfferView, ServiceError,
    };
    use holzwave_core::ai::service::ask::InputView;
    use holzwave_core::ai::store::{ConversationId, ConversationSummary};
    use holzwave_core::ai::tools::Decision;
    use holzwave_core::ai::types::{ContextKind, Locality};
    use holzwave_core::ai::wire::Wire;
    use holzwave_host::integrate_host::IntegrationChange;
    use holzwave_core::integrate::ToolId;
    use holzwave_core::layout::Axis;
    use holzwave_core::wire::{ModesMsg, SearchMsg};
    use holzwave_host::commands::registry::COMMANDS;

    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(PoisonError::into_inner)
    }

    const COMMANDS_SRC: &str = include_str!("commands.rs");
    const LIB_SRC: &str = include_str!("lib.rs");
    const DESKTOP_TABLE_SRC: &str = include_str!("desktop_table.rs");

    /// Serialises the writers of the golden: both tests write the same bytes in update mode.
    static GOLDEN: Mutex<()> = Mutex::new(());

    fn golden_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/protocol.json")
    }

    fn update_fixtures() -> bool {
        std::env::var("UPDATE_FIXTURES").is_ok_and(|v| v == "1")
    }

    /// One `#[tauri::command]` function of `commands.rs`.
    struct CommandSource {
        name: String,
        /// The attribute carries `rename_all = "snake_case"`.
        snake_case: bool,
        /// The argument names in order, State, AppHandle and WebviewWindow parameters left out: Tauri fills those in,
        /// the page never sends them.
        args: Vec<String>,
    }

    /// Split `text` at the commas that are not inside brackets, parentheses or angle brackets.
    fn split_top_level(text: &str) -> Vec<&str> {
        let mut parts = Vec::new();
        let (mut depth, mut start) = (0i32, 0usize);
        for (i, c) in text.char_indices() {
            match c {
                '(' | '[' | '<' | '{' => depth += 1,
                ')' | ']' | '>' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(&text[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        parts.push(&text[start..]);
        parts
    }

    /// Read the command functions out of the source text: the name and the argument names of every function that
    /// follows a `#[tauri::command` attribute.
    fn parse_commands(source: &str) -> Vec<CommandSource> {
        let mut found = Vec::new();
        let mut rest = source;
        while let Some(at) = rest.find("#[tauri::command") {
            rest = &rest[at + "#[tauri::command".len()..];
            let attr_end = rest.find(']').expect("the attribute is closed");
            let snake_case = rest[..attr_end].contains("rename_all = \"snake_case\"");
            rest = &rest[attr_end..];

            let fn_at = rest.find("fn ").expect("a function follows the attribute");
            let after = &rest[fn_at + 3..];
            let open = after.find('(').expect("an argument list");
            let name = after[..open].trim().to_owned();

            // The matching close parenthesis of the argument list.
            let (mut depth, mut close) = (0i32, None);
            for (i, c) in after[open..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(open + i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let list = &after[open + 1..close.expect("the argument list is closed")];
            let args = split_top_level(list)
                .into_iter()
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .filter_map(|a| {
                    let (arg, ty) = a.split_once(':').expect("name: Type");
                    let ty = ty.trim();
                    if ty.starts_with("State<") || ty.contains("AppHandle") || ty.contains("WebviewWindow") {
                        return None;
                    }
                    Some(arg.trim().to_owned())
                })
                .collect();
            found.push(CommandSource { name, snake_case, args });
        }
        found
    }

    fn sample_view() -> View {
        let (mut layout, _) = Layout::new();
        layout.split_focused(Axis::Row);
        layout.new_tab();
        layout.new_tab();
        layout.select_tab(0);
        layout.new_workspace("notes");
        layout.select_workspace(0);
        let view = View::of(&layout, 1, 1);
        assert_eq!(view.workspaces.len(), 2);
        assert_eq!(view.tabs.len(), 3);
        assert_eq!(view.panes.len(), 2);
        assert_eq!(view.dividers.len(), 1);
        view
    }

    #[test]
    fn the_view_lists_the_ids_of_the_panes_of_every_tab_and_workspace() {
        let view = sample_view();
        for w in &view.workspaces {
            assert_eq!(w.pane_ids.len(), w.panes, "{}", w.name);
        }
        for (i, t) in view.tabs.iter().enumerate() {
            assert_eq!(t.pane_ids.len(), t.panes, "tab {i}");
        }
        // The pane of a tab that is not shown is named too, so the page can map it to its tab.
        let shown: Vec<PaneId> = view.panes.iter().map(|(p, _)| *p).collect();
        let tab_ids: Vec<PaneId> = view.tabs.iter().flat_map(|t| t.pane_ids.clone()).collect();
        assert!(shown.iter().all(|p| view.tabs[0].pane_ids.contains(p)), "the shown tab holds the shown panes");
        assert_eq!(tab_ids.len(), 4, "two panes in the first tab, one in each other tab");
        let mut unique = tab_ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), tab_ids.len(), "a pane is in one tab");
        // The active workspace holds all the panes of its tabs; the other one holds its own.
        assert_eq!(view.workspaces[0].pane_ids.len(), 4);
        assert!(view.workspaces[0].pane_ids.iter().all(|p| tab_ids.contains(p)));
        assert_eq!(view.workspaces[1].pane_ids.len(), 1);
        assert!(!view.workspaces[1].pane_ids.iter().any(|p| tab_ids.contains(p)));
    }

    fn to_value<T: Serialize>(value: &T) -> Value {
        serde_json::to_value(value).expect("serialises")
    }

    /// Every value serialises, and deserialises back to itself: the page sends what the Rust side reads.
    fn ops<T>(values: Vec<T>) -> Value
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        Value::Array(
            values
                .into_iter()
                .map(|op| {
                    let json = to_value(&op);
                    let back: T = serde_json::from_value(json.clone()).expect("reads back");
                    assert_eq!(back, op, "{json} must read back as the same op");
                    json
                })
                .collect(),
        )
    }

    /// The reasons an undo is refused and the fixed sentence of each, from the real fault. The match has no wildcard arm, so a
    /// reason that is added to the shell does not compile here until the golden (and with it the page) knows it.
    fn undo_reasons() -> Value {
        use holzwave_host::ai_write::UndoFault;
        let position = |fault: UndoFault| match fault {
            UndoFault::UnknownChange => 0,
            UndoFault::Created => 1,
            UndoFault::FileChanged => 2,
            UndoFault::Denied => 3,
            UndoFault::Unreadable => 4,
        };
        let all = [
            UndoFault::UnknownChange,
            UndoFault::Created,
            UndoFault::FileChanged,
            UndoFault::Denied,
            UndoFault::Unreadable,
        ];
        let mut out = serde_json::Map::new();
        for (at, fault) in all.into_iter().enumerate() {
            assert_eq!(position(fault), at, "the list holds every reason, in order");
            out.insert(fault.reason().to_owned(), Value::String(fault.to_string()));
        }
        Value::Object(out)
    }

    /// A card of `wsh ai` as the core builds it: a cloud destination, piped text counted with one finding.
    fn sample_ask() -> AskView {
        AskView {
            ask: "0123456789abcdef0123456789abcdef".into(),
            seal: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            pane: 3,
            prompt: "why does this fail".into(),
            prompt_findings: Vec::new(),
            input: Some(InputView {
                lines: 142,
                bytes: 6254,
                sent_bytes: 5939,
                findings: vec![FindingView { rule: "anthropic_key".into(), count: 1 }],
            }),
            profile: "claude".into(),
            model: "claude-sonnet".into(),
            host: "api.anthropic.com".into(),
            locality: Locality::Cloud,
        }
    }

    /// The answers of the integration commands, written by the real types: the recorded output of a fake `kubectl` goes
    /// through the real parser, and the tool entries are the real compiled-in descriptions.
    /// The answer of `pane_connections`: a pane of a saved connection (no remote member), then panes of server sessions in
    /// every word of who writes and of what the pane is busy with.
    fn pane_connection_samples() -> Value {
        use holzwave_core::conn::{ConnState, RemoteState, RemoteView};
        use holzwave_core::connections::{ConnView, RemoteConn};
        use holzwave_core::remote::proto::WriterView;
        let profile = holzwave_core::profiles::Profile::new("web-1", "Web", "web.example.org");
        let saved = ConnView::new(4, &profile, &ConnState::Reconnecting { attempt: 2, of: 5, in_secs: 3 }).expect("a connection");
        let remote = |pane, state: &ConnState, writer, phase| {
            let view = RemoteView { session: 7, writer, cols: 120, rows: 40, more: true, state: phase, dropped_input: 0 };
            ConnView::remote(pane, "sync.example.test", state, RemoteConn::of(&view))
        };
        to_value(&vec![
            saved,
            remote(9, &ConnState::Connected, WriterView::Writing, RemoteState::None),
            remote(10, &ConnState::Connected, WriterView::Watching, RemoteState::LoadingOlder),
            remote(11, &ConnState::Connecting, WriterView::Free, RemoteState::Attaching),
            remote(12, &ConnState::Reconnecting { attempt: 1, of: 5, in_secs: 3 }, WriterView::Free, RemoteState::Resyncing),
            remote(13, &ConnState::Failed, WriterView::Free, RemoteState::None),
            remote(14, &ConnState::Connected, WriterView::Writing, RemoteState::InputDropped),
        ])
    }

    fn integration_samples() -> Value {
        use holzwave_host::integrate_host::{ConfirmAnswer, ConfirmKind, LaunchAnswer, PaneAnswer};
        use holzwave_core::integrate::ticket::{Card, Guard};
        use holzwave_core::integrate::VerbEffect;
        use holzwave_core::integrate::context::{ContextRow, RowState};
        use holzwave_core::integrate::service::{
            Fault, FaultCode, ProfileView, ProfilesView, RunAnswer, SecretView, SelectionView, ToolState, ToolView,
        };
        use holzwave_core::integrate::{kube, Integration, Selection};

        let spec = &kube::SPEC;
        let verb = spec.verb("contexts").expect("the default verb of Kubernetes");
        let recorded = include_str!("../../../core/tests/fixtures/integrations/kubectl/config_view.json");
        let view = kube::KUBE.view(verb, recorded.as_bytes()).expect("the recorded kubeconfig is a table");
        let selection = Selection { context: Some("dev".into()), namespace: Some("default".into()), ..Selection::default() };
        let found = ToolView {
            id: ToolId::Kube,
            label: spec.label,
            state: ToolState::Found,
            path: Some("/opt/homebrew/bin/kubectl".into()),
            place: Some("path"),
            binary: None,
            reason: None,
            version: Some("1.36.1".into()),
            searched: Vec::new(),
            verbs: spec.verbs,
            selects: spec.selects,
        };
        let missing = ToolView {
            id: ToolId::Talos,
            label: holzwave_core::integrate::spec_of(ToolId::Talos).label,
            state: ToolState::NotFound,
            path: None,
            place: None,
            binary: None,
            reason: None,
            version: None,
            searched: vec!["/usr/local/bin".into(), "/opt/homebrew/bin".into()],
            verbs: holzwave_core::integrate::spec_of(ToolId::Talos).verbs,
            selects: holzwave_core::integrate::spec_of(ToolId::Talos).selects,
        };
        let refused = Fault { code: FaultCode::Refused, reason: "tool", text: "The tool is not known.".into() };
        json!({
            "tools": [to_value(&found), to_value(&missing)],
            "run": to_value(&RunAnswer {
                view: view.with_fetched(1_800_000_000),
                selection: SelectionView { selection, production: Some("Production".into()) },
                fetched: 1_800_000_000,
            }),
            "fault": to_value(&IntegrationFault::from(refused)),
            "card": to_value(&Card {
                ticket: "0123456789abcdef0123456789abcdef".into(),
                tool: ToolId::Kube,
                verb: "delete_pod".into(),
                effect: VerbEffect::Write,
                program: "kubectl".into(),
                argv: ["delete", "pod", "--wait=false", "--request-timeout=10s", "--", "api-0"].map(String::from).to_vec(),
                target: vec![
                    ("context".into(), "prod".into()),
                    ("namespace".into(), "payments".into()),
                    ("pod".into(), "api-0".into()),
                ],
                guard: Guard { production: Some("Production".into()) },
                fields: [
                    ("tool", "kube"),
                    ("verb", "delete_pod"),
                    ("effect", "write"),
                    ("program", "kubectl"),
                    ("arg 1", "delete"),
                    ("arg 2", "pod"),
                    ("arg 3", "--wait=false"),
                    ("arg 4", "--request-timeout=10s"),
                    ("arg 5", "--"),
                    ("arg 6", "api-0"),
                    ("context", "prod"),
                    ("namespace", "payments"),
                    ("pod", "api-0"),
                    ("guard", "production"),
                    ("production_name", "Production"),
                ]
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .to_vec(),
                hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                expires_in_secs: 120,
            }),
            "confirm": [
                to_value(&ConfirmAnswer {
                    kind: ConfirmKind::Ran,
                    pane: None,
                    note: "pod \"api-0\" deleted".into(),
                    view: None,
                }),
                to_value(&ConfirmAnswer {
                    kind: ConfirmKind::Launched,
                    pane: Some(3),
                    note: "Exec in pod api-0 (Production)".into(),
                    view: Some(sample_view()),
                }),
            ],
            "launch": to_value(&LaunchAnswer { pane: 3, view: sample_view() }),
            "profiles": to_value(&ProfilesView {
                profiles: vec![
                    ProfileView {
                        id: "prod".into(),
                        tool: ToolId::Kube,
                        name: "Production".into(),
                        context: Some("prod-cluster".into()),
                        namespace: None,
                        subscription: None,
                        section: None,
                        node: None,
                        production: true,
                        isolated: false,
                        env: [("AWS_PROFILE".to_owned(), "prod".to_owned())].into_iter().collect(),
                        secret_env: vec![
                            SecretView { name: "AWS_SECRET_ACCESS_KEY".into(), has_value: true },
                            SecretView { name: "AWS_SESSION_TOKEN".into(), has_value: false },
                        ],
                    },
                    ProfileView {
                        id: "work".into(),
                        tool: ToolId::Azure,
                        name: "Work".into(),
                        context: None,
                        namespace: None,
                        subscription: Some("00000000-0000-0000-0000-000000000000".into()),
                        section: None,
                        node: None,
                        production: false,
                        isolated: true,
                        env: std::collections::BTreeMap::new(),
                        secret_env: Vec::new(),
                    },
                ],
                notes: vec!["integrations.json was not used: it has a member or a value that is not allowed (line 3, column 9)".into()],
            }),
            "profile_fault": to_value(&IntegrationFault::from(Fault {
                code: FaultCode::Profile,
                reason: "invalid",
                text: "Profile prod: context is not valid.".into(),
            })),
            "nothing": to_value(&()),
            "context": to_value(&ContextAnswer {
                rows: vec![
                    ContextRow {
                        tool: ToolId::Kube,
                        state: RowState::Found,
                        context: Some("dev".into()),
                        namespace: Some("default".into()),
                        subscription: None,
                        section: None,
                        node: None,
                        production: None,
                        stale: false,
                    },
                    ContextRow {
                        tool: ToolId::Azure,
                        state: RowState::SignedOut,
                        context: None,
                        namespace: None,
                        subscription: None,
                        section: None,
                        node: None,
                        production: None,
                        stale: true,
                    },
                ],
                panes: vec![PaneAnswer {
                    pane: 3,
                    tool: ToolId::Kube,
                    view: "pods".into(),
                    context: Some("dev".into()),
                    namespace: Some("default".into()),
                    production: Some("Production".into()),
                    held: true,
                    status: Some(1),
                }],
            }),
        })
    }

    fn samples() -> Value {
        let frame = FrameMsg {
            pane: 3,
            seq: 1,
            full: true,
            cols: 80,
            rows: 24,
            fg: 0xffffff,
            bg: 0,
            cursor: None,
            scrollbar: (0, 24, 24),
            selection: Vec::new(),
            matches: Vec::new(),
            search: None,
            copy_cursor: None,
            modes: ModesMsg { alt: false, bracketed_paste: false, mouse: false },
            images: None,
            changed: Vec::new(),
        };
        let view = sample_view();
        let settings_view = SettingsView::new(
            &Settings::default(),
            &ThemeRegistry::builtin(),
            // By name, not the host's table: the golden is what the Mac produces and must read the same on every host.
            &Keybindings::default_for("macos"),
            vec!["settings.json: font_size 9999 is outside 8..=40; 40 is used".into()],
        );
        let events = [
            Out::Frame(frame),
            Out::Layout(view.clone()),
            Out::Title { pane: 3, title: "vim README.md".into() },
            Out::Cwd { pane: 3, cwd: "/Users/holz/Projects/holzWave".into() },
            Out::PaneExit { pane: 3, code: Some(0) },
            Out::Settings(settings_view.clone()),
            Out::IncomingDrop { kind: DropKind::Pane, pane: 3, x: 412.5, y: 96.0 },
            Out::DragHover { kind: DropKind::Pane, pane: 3, x: 412.5, y: 96.0 },
            Out::DragEnd,
            Out::Ai(AiPayload::new(AiEvent {
                conversation: "chat-1".into(),
                turn: 5,
                seq: 2,
                kind: AiEventKind::Delta { text: "Hel".into() },
            })),
            Out::AiAsk(AskEvent::Waiting { view: sample_ask() }),
            Out::HistoryOpen { pane: 3 },
            Out::LoginPrompt {
                id: "0123456789abcdef0123456789abcdef".into(),
                connection: "Talos".into(),
                host: "talos.example".into(),
                name: "Two-factor login".into(),
                instructions: "Enter your password and the code of your token.".into(),
                prompts: vec![
                    PromptLine { text: "Password:".into(), echo: false },
                    PromptLine { text: "Code:".into(), echo: true },
                ],
            },
            Out::LoginPromptEnd { id: "0123456789abcdef0123456789abcdef".into(), outcome: PromptEnd::Answered },
            Out::Bell { pane: 3 },
            Out::CommandEnd { pane: 3, code: Some(3) },
            Out::Integration(IntegrationEvent {
                seq: 7,
                kind: IntegrationChange::Context,
                tool: Some(ToolId::Kube),
                pane: Some(3),
                code: None,
            }),
        ];
        let names: Vec<&str> = events.iter().map(Out::name).collect();
        let payload = |i: usize| to_value(&events[i]);

        let mut motions = Vec::new();
        for motion in [
            CopyMotionMsg::Left,
            CopyMotionMsg::Right,
            CopyMotionMsg::Up,
            CopyMotionMsg::Down,
            CopyMotionMsg::LineStart,
            CopyMotionMsg::LineEnd,
            CopyMotionMsg::Top,
            CopyMotionMsg::Bottom,
            CopyMotionMsg::PageUp,
            CopyMotionMsg::PageDown,
            CopyMotionMsg::WordForward,
            CopyMotionMsg::WordBack,
        ] {
            motions.push(CopyOpMsg::Move { motion });
        }
        let mut copy_ops = vec![CopyOpMsg::Enter, CopyOpMsg::Exit, CopyOpMsg::Toggle, CopyOpMsg::Yank];
        copy_ops.extend(motions);

        json!({
            "view": to_value(&view),
            "events": {
                "names": names,
                "title": payload(2),
                "cwd": payload(3),
                "pane-exit": [
                    payload(4),
                    to_value(&Out::PaneExit { pane: 3, code: None }),
                ],
                "settings": payload(5),
                "incoming-drop": [
                    payload(6),
                    to_value(&Out::IncomingDrop { kind: DropKind::Tab, pane: 4, x: 40.0, y: 12.0 }),
                ],
                "drag-hover": [
                    payload(7),
                    to_value(&Out::DragHover { kind: DropKind::Tab, pane: 4, x: 40.0, y: 12.0 }),
                ],
                "drag-end": payload(8),
                "ai": payload(9),
                "ai-ask": payload(10),
                "history-open": payload(11),
                "login-prompt": payload(12),
                "login-prompt-end": [
                    payload(13),
                    to_value(&Out::LoginPromptEnd { id: "0123456789abcdef0123456789abcdef".into(), outcome: PromptEnd::Cancelled }),
                    to_value(&Out::LoginPromptEnd { id: "0123456789abcdef0123456789abcdef".into(), outcome: PromptEnd::TimedOut }),
                ],
                "bell": payload(14),
                "command-end": [
                    payload(15),
                    to_value(&Out::CommandEnd { pane: 3, code: None }),
                ],
                "integration": [
                    payload(16),
                    to_value(&Out::Integration(IntegrationEvent {
                        seq: 8,
                        kind: IntegrationChange::Held,
                        tool: Some(ToolId::Kube),
                        pane: Some(3),
                        code: Some(1),
                    })),
                    to_value(&Out::Integration(IntegrationEvent {
                        seq: 9,
                        kind: IntegrationChange::Tools,
                        tool: None,
                        pane: None,
                        code: None,
                    })),
                    to_value(&Out::Integration(IntegrationEvent {
                        seq: 10,
                        kind: IntegrationChange::Profiles,
                        tool: None,
                        pane: None,
                        code: None,
                    })),
                ],
            },
            "settings_view": to_value(&settings_view),
            "app_info": to_value(&AppInfo {
                version: "0.0.1-beta2".into(),
                os: "macos".into(),
                hostname: "holz-mac.local".into(),
            }),
            "mouse": {
                "actions": ops(vec![MouseActionMsg::Press, MouseActionMsg::Release, MouseActionMsg::Motion]),
                "buttons": ops(vec![
                    MouseButtonMsg::Left,
                    MouseButtonMsg::Middle,
                    MouseButtonMsg::Right,
                    MouseButtonMsg::None,
                ]),
            },
            "ops": {
                "select": ops(vec![
                    SelectOpMsg::Begin { x: 3, y: 4, mode: SelectModeMsg::Cell },
                    SelectOpMsg::Begin { x: 3, y: 4, mode: SelectModeMsg::Word },
                    SelectOpMsg::Begin { x: 3, y: 4, mode: SelectModeMsg::Line },
                    SelectOpMsg::Extend { x: 10, y: 4 },
                    SelectOpMsg::All,
                    SelectOpMsg::Clear,
                ]),
                "search": ops(vec![
                    SearchOpMsg::Set { needle: "find".into() },
                    SearchOpMsg::Next,
                    SearchOpMsg::Prev,
                    SearchOpMsg::Clear,
                ]),
                "copy_mode": ops(copy_ops),
                "drop_kind": ops(vec![DropKind::Pane, DropKind::Tab]),
                "ai_profile_input": ops(vec![
                    ProfileInputMsg {
                        id: None,
                        name: "Claude".into(),
                        preset: Preset::Anthropic,
                        wire: None,
                        base_url: String::new(),
                        model: String::new(),
                        allow_lan_http: false,
                        acknowledge_gateway: false,
                    },
                    ProfileInputMsg {
                        id: Some("local".into()),
                        name: "Local".into(),
                        preset: Preset::Ollama,
                        wire: Some(Wire::OpenAiChat),
                        base_url: "http://127.0.0.1:11434".into(),
                        model: "llama3".into(),
                        allow_lan_http: true,
                        acknowledge_gateway: true,
                    },
                ]),
                "key_slot": ops(vec![KeySlot::Provider, KeySlot::Gateway]),
                "context_kind": ops(vec![
                    ContextKind::Selection,
                    ContextKind::Screen,
                    ContextKind::LastCommand,
                    ContextKind::File,
                    ContextKind::Preview,
                ]),
                "ai_decision": ops(vec![Decision::Allow, Decision::Deny]),
                "ai_undo": undo_reasons(),
            },
            "paste": [
                to_value(&PasteOutcomeMsg::from(PasteOutcome::Written)),
                to_value(&PasteOutcomeMsg::from(PasteOutcome::Empty)),
                to_value(&PasteOutcomeMsg::from(PasteOutcome::Rejected)),
            ],
            "answers": {
                "search": [
                    to_value(&SearchMsg { total: 3, current: Some(1) }),
                    to_value(&SearchMsg { total: 0, current: None }),
                ],
                "copy_mode": [
                    to_value(&CopyStateMsg { active: true, selecting: false, yanked: None }),
                    to_value(&CopyStateMsg { active: false, selecting: false, yanked: Some("text".into()) }),
                ],
                "monitor": [
                    to_value(&holzwave_core::monitor::Reading::Live(holzwave_core::monitor::Snapshot {
                        cpu: 37.5,
                        cores: 4,
                        mem_used: 7_561_216_000,
                        mem_total: 16_777_216_000,
                        net_rx: 2_100_000,
                        net_tx: 500_000,
                        uptime: 5402,
                        procs: vec![holzwave_core::monitor::Proc { pid: 842, name: "node server".into(), cpu: 200.0, mem: 204_800_000 }],
                    })),
                    to_value(&holzwave_core::monitor::Reading::Unsupported { system: "Darwin".into() }),
                ],
                "release_outside": [
                    to_value(&Released::TornOff { label: "window-12".into() }),
                    to_value(&Released::Handed { to: "main".into() }),
                    to_value(&Released::Cancelled),
                ],
                "local_network": [
                    to_value(&LocalNetworkView { supported: true, notice_due: true, state: LnState::NotAsked.as_str() }),
                    to_value(&LocalNetworkView { supported: true, notice_due: false, state: LnState::Allowed.as_str() }),
                    to_value(&LocalNetworkView { supported: true, notice_due: false, state: LnState::Denied.as_str() }),
                    to_value(&LocalNetworkView { supported: true, notice_due: false, state: LnState::Unknown.as_str() }),
                    to_value(&LocalNetworkView::unsupported()),
                ],
                "host_shell": [
                    to_value(&HostShellView { applies: false, state: "unavailable" }),
                    to_value(&HostShellView { applies: true, state: "off" }),
                    to_value(&HostShellView { applies: true, state: "on" }),
                    to_value(&HostShellView { applies: true, state: "unavailable" }),
                ],
                "window_frame": [
                    to_value(&WindowFrameView { applies: false, frame: "auto".into(), buttons: "auto".into(), pending: false }),
                    to_value(&WindowFrameView { applies: true, frame: "auto".into(), buttons: "auto".into(), pending: false }),
                    to_value(&WindowFrameView { applies: true, frame: "system".into(), buttons: "round".into(), pending: true }),
                    to_value(&WindowFrameView { applies: true, frame: "own".into(), buttons: "flat".into(), pending: false }),
                ],
                "server": [
                    to_value(&ServerView { state: "signed_out", url: String::new(), name: "holzWave on Linux".into(), locked: false, sync_on: false, remote_on: false, error: None }),
                    to_value(&ServerView { state: "signing_in", url: "https://sync.example.test".into(), name: "Laptop".into(), locked: false, sync_on: false, remote_on: false, error: None }),
                    to_value(&ServerView { state: "signed_in", url: "https://sync.example.test".into(), name: "Laptop".into(), locked: false, sync_on: true, remote_on: true, error: None }),
                    to_value(&ServerView { state: "unreachable", url: "https://sync.example.test".into(), name: "Laptop".into(), locked: true, sync_on: false, remote_on: false, error: Some("The server cannot be reached.".into()) }),
                ],
                "sign_in": [to_value(&SignInView { code: "BCDF-GHJK".into(), url: "https://sync.example.test/#device=hwg_x".into(), expires_in: 600, interval: 5 })],
                "sign_in_poll": [
                    to_value(&SignInPoll { state: "pending", interval: 5 }),
                    to_value(&SignInPoll { state: "approved", interval: 5 }),
                    to_value(&SignInPoll { state: "denied", interval: 5 }),
                    to_value(&SignInPoll { state: "expired", interval: 5 }),
                    to_value(&SignInPoll { state: "too_many", interval: 5 }),
                    to_value(&SignInPoll { state: "off", interval: 5 }),
                    to_value(&SignInPoll { state: "unreachable", interval: 10 }),
                ],
                "sessions": to_value(&vec![
                    SessionRow { id: 3, title: "vim notes.md".into(), cwd: "/home/alice/notes".into(), cols: 120, rows: 40, started: 1_762_771_200, attached: 2, held: true },
                    SessionRow { id: 9, title: String::new(), cwd: String::new(), cols: 80, rows: 24, started: 1_762_774_800, attached: 0, held: false },
                ]),
                "pane_connections": pane_connection_samples(),
                "integration": integration_samples(),
                "ai": {
                    "ticket": to_value(&TicketView { conversation: "chat-1".into(), turn: 5 }),
                    "ask_pending": [to_value(&sample_ask())],
                    "ask_fault": to_value(&AiFault::from_ask(&AskFault::SealMismatch)),
                    "fault": [
                        to_value(&AiFault::from_service(&ServiceError::Key(KeyError::NoSuchSlot))),
                        to_value(&AiFault::no_such_pane()),
                    ],
                    "conversations": to_value(&ConversationList {
                        chats: vec![ConversationSummary {
                            id: ConversationId::parse("chat-1").expect("a chat id"),
                            title: "What is a pty?".into(),
                            created: 1_800_000_000,
                            updated: 1_800_000_060,
                            profile: Some("claude".into()),
                            model: Some("claude-sonnet".into()),
                        }],
                        notes: vec!["chat-2: the file cannot be used".into()],
                    }),
                    "profiles": to_value(&AiProfiles {
                        profiles: vec![ProfileView {
                            id: "claude".into(),
                            name: "Claude".into(),
                            preset: Preset::Anthropic,
                            wire: Wire::Anthropic,
                            base_url: "https://api.anthropic.com".into(),
                            model: "claude-sonnet".into(),
                            allow_lan_http: false,
                            host: "api.anthropic.com".into(),
                            key: KeyStatus::Present,
                            gateway_key: None,
                            disclosure: None,
                        }],
                        notes: Vec::new(),
                        on_device: None,
                    }),
                    "profiles_on_device": to_value(&AiProfiles {
                        profiles: Vec::new(),
                        notes: Vec::new(),
                        on_device: Some(DeviceView::of(DeviceState {
                            state: "apple_intelligence_not_enabled",
                            advice: "enable_apple_intelligence",
                        })),
                    }),
                    "removal": to_value(&RemovalView::from(Removal { removed: true, key_deleted: true })),
                    "context_state": to_value(&ContextState {
                        profile: "local".into(),
                        model: "llama3".into(),
                        host: "127.0.0.1:11434".into(),
                        locality: Locality::Loopback,
                        required: true,
                        shared: false,
                        kinds: vec![ContextKind::Selection, ContextKind::Screen, ContextKind::LastCommand],
                        persist: false,
                    }),
                    "offer": to_value(&OfferView {
                        offer: "0123456789abcdef0123456789abcdef".into(),
                        profile: "local".into(),
                        model: "llama3".into(),
                        host: "127.0.0.1:11434".into(),
                        locality: Locality::Loopback,
                        ttl_seconds: 300,
                        items: vec![ItemView {
                            id: 1,
                            kind: ContextKind::Screen,
                            label: "Screen of pane 3".into(),
                            bytes: 412,
                            findings: vec![FindingView { rule: "anthropic_key".into(), count: 1 }],
                        }],
                        empty: vec![ContextKind::Selection],
                        bytes: 412,
                        persist: false,
                    }),
                },
            },
        })
    }

    /// The command table of the golden: Rust name to ordered argument names. A command the shell writes from the host's
    /// command table comes from its row, the others from the text of `commands.rs`; a command that is both (its wrapper is
    /// hand-written because it needs the window shell) must name the same arguments in both places.
    fn command_table() -> Value {
        let mut table: BTreeMap<String, Vec<String>> =
            parse_commands(COMMANDS_SRC).into_iter().map(|c| (c.name, c.args)).collect();
        for row in COMMANDS {
            let args: Vec<String> = row.args.iter().map(|arg| arg.name.to_owned()).collect();
            if let Some(written) = table.get(row.name) {
                assert_eq!(written, &args, "the wrapper of {} in commands.rs and its row name different arguments", row.name);
            }
            table.insert(row.name.to_owned(), args);
        }
        to_value(&table)
    }

    fn document() -> Value {
        let mut doc = samples();
        doc.as_object_mut().expect("object").insert("commands".into(), command_table());
        doc
    }

    /// Update mode writes the whole document; compare mode checks the part `pick` selects.
    fn check(pick: impl Fn(&Value) -> Value, what: &str) {
        let _guard = lock(&GOLDEN);
        let doc = document();
        let path = golden_path();
        if update_fixtures() {
            let mut text = serde_json::to_string_pretty(&doc).expect("json");
            text.push('\n');
            std::fs::create_dir_all(path.parent().expect("dir")).expect("fixtures dir");
            std::fs::write(&path, text).expect("write protocol.json");
            return;
        }
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is missing ({e}); run with UPDATE_FIXTURES=1", path.display()));
        let on_disk: Value = serde_json::from_str(&text).expect("protocol.json is JSON");
        assert!(
            pick(&on_disk) == pick(&doc),
            "{what} differ from protocol.json; review the change and run with UPDATE_FIXTURES=1"
        );
    }

    #[test]
    fn protocol_samples_match_golden() {
        check(
            |doc| {
                let mut rest = doc.clone();
                rest.as_object_mut().expect("object").remove("commands");
                rest
            },
            "the payload samples",
        );
    }

    #[test]
    fn protocol_commands_match_source() {
        let commands = parse_commands(COMMANDS_SRC);
        // Every command is a row of the table now; `commands.rs` holds only the wrappers that need the window shell.
        assert!(COMMANDS.len() >= 20, "the table has only {} commands", COMMANDS.len());
        assert_eq!(
            commands.len(),
            COMMANDS.iter().filter(|row| row.shell == holzwave_host::commands::registry::Shell::Hand).count(),
            "the scanner found {} wrappers in commands.rs",
            commands.len()
        );
        for command in &commands {
            assert!(
                command.snake_case,
                "{} lacks rename_all = \"snake_case\": the page would have to send camelCase",
                command.name
            );
            assert!(
                crate::desktop_table::HANDLED.contains(&command.name.as_str()),
                "{} is not registered in the handler the table writes",
                command.name
            );
        }
        assert!(
            !commands.iter().any(|c| c.name == "input") && !COMMANDS.iter().any(|row| row.name == "input"),
            "the raw input command must not exist: keys, text and paste go through the engine encoder (D-05)"
        );
        // A row is written once, in the table: its wrapper is either generated or in `commands.rs`, never both and never
        // neither (the build already fails for a hand-written wrapper that is missing).
        for row in COMMANDS {
            let written = commands.iter().any(|c| c.name == row.name);
            assert_eq!(
                written,
                row.shell == holzwave_host::commands::registry::Shell::Hand,
                "{}: a generated row has no wrapper in commands.rs, and a hand row has one",
                row.name
            );
        }
        for name in ["note_window", "placement"] {
            assert!(
                !commands.iter().any(|c| c.name == name) && !COMMANDS_SRC.contains(&format!("fn {name}")),
                "{name} is a method of AppCore for the window event handlers, not a command"
            );
        }
        // The arguments the page was promised.
        let table = command_table();
        assert_eq!(table["paste"], json!(["pane", "text", "allow_unsafe"]));
        assert_eq!(table["start_pane"], json!(["pane", "cols", "rows", "cell_w", "cell_h"]));
        assert_eq!(table["resize"], json!(["pane", "cols", "rows", "cell_w", "cell_h"]));
        assert_eq!(
            table["key"],
            json!(["pane", "code", "key", "mods", "repeat", "release", "composing"])
        );
        // The calling window is not an argument: Tauri fills it in, so the page sends the same names as before.
        assert_eq!(table["split"], json!(["column"]));
        assert_eq!(table["get_layout"], json!([]));
        assert_eq!(table["close_pane"], json!(["pane"]));
        // The commands between windows carry pane ids and nothing about where the pointer is.
        assert_eq!(table["tear_off_pane"], json!(["pane", "at_cursor"]));
        assert_eq!(table["tear_off_tab"], json!(["pane", "at_cursor"]));
        assert_eq!(table["release_outside"], json!(["pane", "kind"]));
        assert_eq!(table["attach_pane"], json!(["pane", "target", "edge", "workspace"]));
        assert_eq!(table["attach_tab"], json!(["pane", "workspace"]));
        assert_eq!(table["window_ready"], json!([]));

        check(|doc| doc["commands"].clone(), "the command argument names");
    }

    #[test]
    fn the_commands_are_registered_by_the_table_and_by_no_list_in_lib_rs() {
        let code = |source: &str| -> String {
            source.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
        };
        let (lib, table) = (code(LIB_SRC), code(DESKTOP_TABLE_SRC));
        assert!(!lib.contains("generate_handler"), "lib.rs lists commands by hand");
        assert!(lib.contains("desktop_table::handler()"), "lib.rs installs the handler of the table");
        // The wrappers that stay in commands.rs are named by the table's expansion, once each, from their rows.
        assert!(table.contains("crate::commands::$name"), "the expansion names the hand wrappers from their rows");
        for command in parse_commands(COMMANDS_SRC) {
            let row = COMMANDS.iter().find(|row| row.name == command.name).expect("a wrapper of commands.rs is a row");
            assert_eq!(row.shell, holzwave_host::commands::registry::Shell::Hand, "{}", command.name);
        }
    }

    #[test]
    fn the_scanner_reads_generics_and_skips_state_and_handles() {
        let source = r#"
            #[tauri::command(async, rename_all = "snake_case")]
            pub fn demo(core: State<Arc<AppCore>>, app: AppHandle, window: WebviewWindow, pane: PaneId, map: HashMap<String, Vec<u8>>) -> Result<(), String> {
                Ok(())
            }
            #[tauri::command]
            pub async fn camel(core: State<'_, Arc<AppCore>>, allow_unsafe: bool) {}
        "#;

        let found = parse_commands(source);

        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "demo");
        assert!(found[0].snake_case);
        assert_eq!(found[0].args, vec!["pane", "map"]);
        assert_eq!(found[1].name, "camel");
        assert!(!found[1].snake_case, "a bare attribute is camelCase");
        assert_eq!(found[1].args, vec!["allow_unsafe"]);
    }
}
