//! The command table and its registry: what a call through [`lookup`] answers, how strictly it reads its arguments, what
//! the caps are, what the browser build does with a command it does not have, and that the table still describes the
//! desktop surface as it was recorded before the table existed.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use holzwave_host::commands::registry::{
    COMMANDS, CAP_DEFAULT, CAP_INPUT, CAP_PASTE, Command, ERROR_TEXT_LIMIT, Mode, NOT_IN_BROWSER, Outcome, Scope, Shell,
    Web, lookup,
};
use holzwave_host::state::{AppCore, Out, ShellSpec, Sink, Target};
use holzwave_host::windows::{CursorSpot, Place};

// ---- a core to call against -----------------------------------------------------------------------------------------

/// A sink that counts what the core sends and records the windows it was asked to open.
#[derive(Default)]
struct Counting {
    events: AtomicUsize,
    opened: Mutex<Vec<String>>,
}

impl Sink for Counting {
    fn send(&self, _target: Target, _out: Out) {
        self.events.fetch_add(1, Ordering::SeqCst);
    }

    fn close_window(&self, _label: &str) {}

    fn open_window(&self, label: &str, _place: &Place) -> Result<(), String> {
        self.opened.lock().unwrap_or_else(PoisonError::into_inner).push(label.to_owned());
        Ok(())
    }

    fn cursor_spot(&self, _source: &str) -> CursorSpot {
        CursorSpot::Unknown
    }
}

/// A core in a scratch directory of its own, with a program that ends at once so that no test depends on a shell.
struct World {
    dir: PathBuf,
    core: Arc<AppCore>,
    sink: Arc<Counting>,
}

impl World {
    fn new(tag: &str) -> World {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("holzwave-commands-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).expect("scratch dir");
        let sink = Arc::new(Counting::default());
        let core = AppCore::new(dir.clone(), sink.clone(), ShellSpec::new("/bin/cat", &[]));
        World { dir, core, sink }
    }

    /// Call the command `name` for the window `main`.
    fn call(&self, name: &str, args: &str) -> Outcome {
        self.call_in("main", name, args)
    }

    fn call_in(&self, window: &str, name: &str, args: &str) -> Outcome {
        lookup(name).unwrap_or_else(|| panic!("{name} is not a row")).call(&self.core, window, args)
    }

    /// The events the core has sent so far.
    fn events(&self) -> usize {
        self.sink.events.load(Ordering::SeqCst)
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.core.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The JSON text of a string value.
fn json_string(text: &str) -> String {
    serde_json::to_string(text).expect("a string is JSON")
}

fn error_text(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Err(json) => serde_json::from_str::<String>(json).unwrap_or_else(|_| panic!("{json} is not a JSON string")),
        other => panic!("expected an error, got {other:?}"),
    }
}

// ---- the table and the lookup ---------------------------------------------------------------------------------------

#[test]
fn lookup_finds_a_row_by_name_and_nothing_else() {
    let row = lookup("get_layout").expect("get_layout is a row");
    assert_eq!(row.name, "get_layout");
    assert_eq!(row.group, "layout");
    assert_eq!(row.scope, Scope::Window);
    assert_eq!(row.mode, Mode::Sync);
    assert_eq!(row.cap, 64 * 1024);
    assert_eq!(row.web, Web::Same);
    assert_eq!(row.shell, Shell::Generated);
    assert!(row.args.is_empty());
    assert!(lookup("nope").is_none());
    assert!(lookup("").is_none());
    assert!(lookup("GET_LAYOUT").is_none(), "names are exact");
    assert!(lookup("get_layout ").is_none(), "names are exact");
}

#[test]
fn the_slice_is_the_rows_and_lookup_finds_each_in_it() {
    let names: Vec<&str> = COMMANDS.iter().map(|row| row.name).collect();
    assert!(names.starts_with(&["get_layout"]), "the order is the order of the table: {names:?}");
    for row in COMMANDS {
        let found = lookup(row.name).expect("every row is found");
        assert!(std::ptr::eq(found, row), "{} is found at its own place", row.name);
    }
}

#[test]
fn a_row_says_its_arguments_and_answer_as_the_table_spells_them() {
    let paste = lookup("paste").expect("paste");
    let args: Vec<(&str, &str)> = paste.args.iter().map(|arg| (arg.name, arg.ty)).collect();
    assert_eq!(args, [("pane", "PaneId"), ("text", "String"), ("allow_unsafe", "bool")]);
    assert_eq!(paste.answer, "PasteOutcomeMsg");
    assert_eq!(paste.error, Some("String"));
    assert_eq!((paste.scope, paste.mode, paste.cap), (Scope::None, Mode::Async, CAP_PASTE));
    let text = lookup("selection_text").expect("selection_text");
    assert_eq!(text.answer.replace(' ', ""), "Option<String>");
}

#[test]
fn a_command_admits_up_to_its_cap_and_no_more() {
    let small = lookup("selection_text").expect("selection_text");
    assert!(small.admits(0));
    assert!(small.admits(CAP_DEFAULT));
    assert!(!small.admits(CAP_DEFAULT + 1));
    let large = lookup("paste").expect("paste");
    assert!(large.admits(CAP_PASTE));
    assert!(!large.admits(CAP_PASTE + 1));
    assert!(CAP_INPUT < CAP_DEFAULT && CAP_DEFAULT < CAP_PASTE);
}

// ---- a call answers what the plain function answers --------------------------------------------------------------------

#[test]
fn get_layout_answers_the_json_of_the_view_the_core_gives() {
    let world = World::new("get-layout");

    let outcome = world.call("get_layout", "{}");

    // Every view the core builds carries the next revision number: the view the core gives afterwards is the same bytes
    // with the number one higher.
    let Outcome::Ok(text) = outcome else { panic!("get_layout answered {outcome:?}") };
    let own = world.core.view_in("main").expect("the window main exists");
    assert_eq!(own.rev, 2);
    assert!(text.starts_with(r#"{"rev":1,"#), "{text}");
    let own_text = serde_json::to_string(&own).expect("a view is JSON");
    assert_eq!(text.replacen(r#"{"rev":1,"#, r#"{"rev":2,"#, 1), own_text);
}

#[test]
fn get_layout_for_a_window_that_does_not_exist_answers_the_core_error() {
    let world = World::new("no-window");

    let outcome = world.call_in("nope", "get_layout", "{}");

    assert_eq!(outcome, Outcome::Err(json_string("no such window")));
}

#[test]
fn paste_to_a_pane_that_does_not_exist_answers_the_error_of_the_core() {
    let world = World::new("paste-error");
    let own = world.core.paste(4711, "x".into(), false).expect_err("no such pane");

    let outcome = world.call("paste", r#"{"pane":4711,"text":"x","allow_unsafe":false}"#);

    assert_eq!(outcome, Outcome::Err(json_string(&own)));
}

#[test]
fn selection_text_for_a_pane_that_does_not_exist_answers_the_error_of_the_core() {
    let world = World::new("selection-error");
    let own = world.core.selection_text(4711).expect_err("no such pane");

    assert_eq!(world.call("selection_text", r#"{"pane":4711}"#), Outcome::Err(json_string(&own)));
}

#[cfg(unix)]
#[test]
fn paste_and_selection_text_on_a_running_pane_answer_what_the_plain_functions_answer() {
    use holzwave_host::commands::terminal;

    let world = World::new("running");
    let pane = world.core.view_in("main").expect("the window main exists").focus;
    world.core.start_pane(pane, 80, 24, 0, 0).expect("cat starts");

    let direct = terminal::paste(&world.core, pane, "x".into(), false).expect("pasted");
    let through = world.call("paste", &format!(r#"{{"pane":{pane},"text":"x","allow_unsafe":false}}"#));

    assert_eq!(through, Outcome::Ok(serde_json::to_string(&direct).expect("JSON")));
    assert_eq!(through, Outcome::Ok(r#""written""#.into()));
    assert_eq!(world.call("selection_text", &format!(r#"{{"pane":{pane}}}"#)), Outcome::Ok("null".into()));
    // A null is no value for a field that is required.
    assert!(matches!(world.call("selection_text", r#"{"pane":null}"#), Outcome::Err(_)));
}

// ---- the arguments are read strictly ------------------------------------------------------------------------------------

#[test]
fn an_unknown_field_is_refused_by_name_and_nothing_runs() {
    let world = World::new("unknown-field");
    let before = world.events();

    let outcome = world.call("paste", r#"{"pane":4711,"text":"x","allow_unsafe":false,"extra":1}"#);

    let text = error_text(&outcome);
    assert!(text.contains("extra"), "the error names the field: {text}");
    assert!(text.contains("unknown"), "{text}");
    assert_eq!(world.events(), before, "the plain function did not run");
    // Had it run, the answer would have been the error of the core for a pane that does not exist.
    let own = world.core.paste(4711, "x".into(), false).expect_err("no such pane");
    assert_ne!(text, own);
}

#[test]
fn a_missing_field_is_refused_by_name() {
    let world = World::new("missing-field");

    let outcome = world.call("paste", r#"{"pane":1,"text":"x"}"#);

    let text = error_text(&outcome);
    assert!(text.contains("allow_unsafe") && text.contains("missing"), "{text}");
}

#[test]
fn a_row_without_arguments_refuses_any_field() {
    let world = World::new("no-arguments");

    let text = error_text(&world.call("get_layout", r#"{"x":1}"#));

    assert!(text.contains('x') && text.contains("unknown"), "{text}");
}

#[test]
fn a_value_of_the_wrong_type_is_refused_without_repeating_it() {
    let world = World::new("wrong-type");
    let secret = "hunter2-hunter2-hunter2";

    let outcome = world.call("selection_text", &format!(r#"{{"pane":"{secret}"}}"#));

    let text = error_text(&outcome);
    assert!(!text.contains(secret), "the error repeats the value: {text}");
    assert!(text.contains("u64"), "the error says what was expected: {text}");
}

#[test]
fn text_that_is_not_json_is_refused() {
    let world = World::new("not-json");

    for args in ["", "{", "[]", "null", "{} {}", r#"{"pane":1}x"#, "\u{0}"] {
        let outcome = world.call("selection_text", args);
        assert!(matches!(outcome, Outcome::Err(_)), "{args:?} answered {outcome:?}");
    }
}

#[test]
fn an_error_text_is_cut_to_the_limit_and_never_holds_a_long_value() {
    let world = World::new("bounded");
    let long = "A".repeat(100 * 1024);

    let as_field = error_text(&world.call("paste", &format!(r#"{{"pane":1,"text":"x","allow_unsafe":false,"{long}":1}}"#)));
    let as_value = error_text(&world.call("paste", &format!(r#"{{"pane":"{long}","text":"x","allow_unsafe":false}}"#)));

    assert!(as_field.chars().count() <= ERROR_TEXT_LIMIT, "{} characters", as_field.chars().count());
    assert!(as_value.chars().count() <= ERROR_TEXT_LIMIT, "{} characters", as_value.chars().count());
    assert!(!as_value.contains("AAAAAAAAAA"), "the value is not in the error: {as_value}");
}

// ---- the recorded surface -----------------------------------------------------------------------------------------------

const SURFACE: &str = include_str!("fixtures/desktop-commands.txt");
const GOLDEN: &str = include_str!("../../app/tests/fixtures/protocol.json");

/// The recorded line of a command: name, window scope, async flag, argument names, answer and error type text without
/// whitespace.
fn surface_line(row: &Command) -> String {
    let strip = |text: &str| text.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let scope = if row.scope == Scope::Window { "window" } else { "none" };
    let mode = if row.mode == Mode::Async { "async" } else { "sync" };
    let args: Vec<&str> = row.args.iter().map(|arg| arg.name).collect();
    format!(
        "{}|{scope}|{mode}|{}|{}|{}",
        row.name,
        args.join(","),
        strip(row.answer),
        row.error.map_or_else(|| "-".to_owned(), strip)
    )
}

fn surface_lines() -> Vec<(&'static str, &'static str)> {
    SURFACE
        .lines()
        .map(|line| (line.split('|').next().expect("a name"), line))
        .collect()
}

#[test]
fn surface_file_lists_every_golden_command() {
    let golden: serde_json::Value = serde_json::from_str(GOLDEN).expect("the golden is JSON");
    let golden_names: BTreeSet<&str> =
        golden["commands"].as_object().expect("the golden has commands").keys().map(String::as_str).collect();
    let lines = surface_lines();
    let surface_names: BTreeSet<&str> = lines.iter().map(|(name, _)| *name).collect();

    assert_eq!(lines.len(), 119, "one line for each command that was declared");
    assert_eq!(surface_names.len(), lines.len(), "no command twice");
    assert_eq!(surface_names, golden_names);
    for (_, line) in &lines {
        assert_eq!(line.split('|').count(), 6, "{line}");
    }
}

#[test]
fn table_rows_match_the_recorded_surface() {
    let lines = surface_lines();
    for row in COMMANDS {
        let recorded = lines
            .iter()
            .find(|(name, _)| *name == row.name)
            .unwrap_or_else(|| panic!("{} is a row but is not in the recorded surface", row.name))
            .1;
        assert_eq!(surface_line(row), recorded, "the row {} differs from the desktop as it was", row.name);
    }
}

const COMMANDS_RS: &str = include_str!("../../app/src-tauri/src/commands.rs");

/// The names of the wrappers that are still written out in `commands.rs`.
fn written_wrappers() -> BTreeSet<&'static str> {
    COMMANDS_RS
        .lines()
        .filter_map(|line| line.strip_prefix("pub fn "))
        .filter_map(|rest| rest.split('(').next())
        .collect()
}

#[test]
fn table_and_text_cover_the_golden() {
    let written = written_wrappers();
    for (name, _) in surface_lines() {
        assert!(
            lookup(name).is_some() || written.contains(name),
            "{name} is a command of the desktop but neither a row of the table nor a wrapper in commands.rs"
        );
    }
}

#[test]
fn table_has_no_duplicate_names() {
    let names: BTreeSet<&str> = COMMANDS.iter().map(|row| row.name).collect();
    assert_eq!(names.len(), COMMANDS.len());
}

#[test]
fn a_hand_written_row_has_its_wrapper_in_the_app_and_a_generated_row_has_none() {
    let written = written_wrappers();
    for row in COMMANDS {
        assert_eq!(
            written.contains(row.name),
            row.shell == Shell::Hand,
            "{}: a generated row has no wrapper in commands.rs and a hand row has one",
            row.name
        );
    }
}

#[test]
fn the_sentence_for_a_missing_command_is_the_one_the_page_shows() {
    assert_eq!(NOT_IN_BROWSER, "Not available in the browser.");
}
