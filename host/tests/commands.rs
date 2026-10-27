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
use holzwave_host::seams::{Host, HostLimits, NeutralHost, Platform};
use holzwave_host::state::{AppCore, Out, ShellSpec, Sink, Target};
use holzwave_host::windows::{CursorSpot, Place};

// ---- a core to call against -----------------------------------------------------------------------------------------

/// A sink that counts what the core sends, remembers the name and the window of each event, and records the windows it
/// was asked to open.
#[derive(Default)]
struct Counting {
    events: AtomicUsize,
    log: Mutex<Vec<(String, &'static str)>>,
    opened: Mutex<Vec<String>>,
}

impl Sink for Counting {
    fn send(&self, target: Target, out: Out) {
        self.events.fetch_add(1, Ordering::SeqCst);
        let window = match target {
            Target::All => "*".to_owned(),
            Target::Window(label) => label.to_string(),
        };
        self.log.lock().unwrap_or_else(PoisonError::into_inner).push((window, out.name()));
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
        World::running(tag, "/bin/cat", &[])
    }

    /// A core whose panes run `program args`.
    fn running(tag: &str, program: &str, args: &[&str]) -> World {
        World::built(tag, program, args, None)
    }

    /// A core that is given `host` to ask for what the shell decides.
    fn hosted(tag: &str, host: Arc<dyn Host>) -> World {
        World::built(tag, "/bin/cat", &[], Some(host))
    }

    fn built(tag: &str, program: &str, args: &[&str], host: Option<Arc<dyn Host>>) -> World {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("holzwave-commands-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).expect("scratch dir");
        let sink = Arc::new(Counting::default());
        let shell = ShellSpec::new(program, args);
        let core = match host {
            Some(host) => AppCore::new_with_host(dir.clone(), sink.clone(), shell, None, None, host),
            None => AppCore::new(dir.clone(), sink.clone(), shell),
        };
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

    /// The windows the core was asked to open.
    fn opened(&self) -> Vec<String> {
        self.sink.opened.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// How many events named `name` went to the window `window`.
    fn sent(&self, window: &str, name: &str) -> usize {
        self.sink.log.lock().unwrap_or_else(PoisonError::into_inner).iter().filter(|(w, n)| w == window && *n == name).count()
    }

    /// The pane that has the focus in the window `main`.
    fn focus(&self) -> u64 {
        self.core.view_in("main").expect("the window main exists").focus
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

// ---- the layout commands answer the view the core gives ---------------------------------------------------------------

/// Two cores that were made alike: the registry call runs on one and the plain call on the other, so that the two views
/// can be compared byte for byte (a view carries the number of the next revision, which a second call on the same core
/// would change).
fn assert_same_view(
    tag: &str,
    setup: &dyn Fn(&World),
    name: &str,
    args: &dyn Fn(&World) -> String,
    plain: &dyn Fn(&World) -> Result<holzwave_host::state::View, String>,
) {
    let (through, direct) = (World::new(&format!("{tag}-through")), World::new(&format!("{tag}-direct")));
    setup(&through);
    setup(&direct);
    assert_eq!(args(&through), args(&direct), "the two cores were not made alike");

    let answered = through.call(name, &args(&through));
    let own = plain(&direct).unwrap_or_else(|e| panic!("the plain call of {name} failed: {e}"));

    assert_eq!(answered, Outcome::Ok(serde_json::to_string(&own).expect("a view is JSON")), "{name}");
}

fn split_first(world: &World) {
    world.core.split_in("main", false).expect("a split");
}

fn other_pane(world: &World) -> u64 {
    let view = world.core.view_in("main").expect("the window main exists");
    view.panes.iter().map(|(pane, _)| *pane).find(|pane| *pane != view.focus).expect("a pane that has not the focus")
}

#[test]
fn split_answers_the_view_the_core_gives() {
    assert_same_view("split", &|_| {}, "split", &|_| r#"{"column":true}"#.into(), &|w| w.core.split_in("main", true));
}

#[test]
fn new_tab_answers_the_view_the_core_gives() {
    assert_same_view("new-tab", &|_| {}, "new_tab", &|_| "{}".into(), &|w| w.core.new_tab_in("main"));
}

#[test]
fn new_workspace_answers_the_view_the_core_gives() {
    assert_same_view(
        "new-workspace",
        &|_| {},
        "new_workspace",
        &|_| r#"{"name":"notes"}"#.into(),
        &|w| w.core.new_workspace_in("main", "notes"),
    );
}

#[test]
fn focus_pane_answers_the_view_the_core_gives() {
    assert_same_view(
        "focus-pane",
        &split_first,
        "focus_pane",
        &|w| format!(r#"{{"pane":{}}}"#, other_pane(w)),
        &|w| w.core.focus_pane_in("main", other_pane(w)),
    );
}

#[test]
fn close_pane_answers_the_view_the_core_gives() {
    assert_same_view(
        "close-pane",
        &split_first,
        "close_pane",
        &|w| format!(r#"{{"pane":{}}}"#, other_pane(w)),
        &|w| w.core.close_pane_in("main", other_pane(w)),
    );
}

#[test]
fn rename_tab_answers_the_view_the_core_gives() {
    assert_same_view(
        "rename-tab",
        &|_| {},
        "rename_tab",
        &|_| r#"{"index":0,"name":"build"}"#.into(),
        &|w| w.core.rename_tab_in("main", 0, "build"),
    );
}

#[test]
fn move_tab_answers_the_view_the_core_gives() {
    assert_same_view(
        "move-tab",
        &|w| {
            w.core.new_tab_in("main").expect("a second tab");
        },
        "move_tab",
        &|_| r#"{"from":0,"to":1}"#.into(),
        &|w| w.core.move_tab_in("main", 0, 1),
    );
}

#[test]
fn set_ratio_answers_the_view_the_core_gives() {
    let split = |w: &World| w.core.view_in("main").expect("view").dividers[0].split;
    assert_same_view(
        "set-ratio",
        &split_first,
        "set_ratio",
        &|w| format!(r#"{{"split":{},"ratio":0.25}}"#, split(w)),
        &|w| w.core.set_ratio_in("main", split(w), 0.25),
    );
}

#[test]
fn a_layout_command_for_a_window_that_does_not_exist_answers_the_error_and_changes_nothing() {
    let world = World::new("layout-no-window");

    for (name, args) in [
        ("split", r#"{"column":false}"#),
        ("new_tab", "{}"),
        ("rename_tab", r#"{"index":0,"name":"x"}"#),
        ("close_workspace", r#"{"index":0}"#),
    ] {
        assert_eq!(world.call_in("nope", name, args), Outcome::Err(json_string("no such window")), "{name}");
    }
    assert_eq!(world.core.view_in("main").expect("view").tabs.len(), 1);
}

// ---- the pointer and keyboard rows read their documented shapes ----------------------------------------------------------

const KEY: &str = r#"{"pane":PANE,"code":"KeyA","key":"a","mods":4,"repeat":false,"release":false,"composing":false}"#;
const MOUSE: &str = r#"{"pane":PANE,"action":"press","button":"left","x":12.5,"y":3,"mods":0}"#;

#[cfg(unix)]
#[test]
fn the_input_rows_accept_their_documented_shapes_and_refuse_a_string_for_a_number() {
    let world = World::new("input-shapes");
    let pane = world.focus();
    world.core.start_pane(pane, 80, 24, 0, 0).expect("cat starts");
    let with = |template: &str| template.replace("PANE", &pane.to_string());

    assert_eq!(world.call("key", &with(KEY)), Outcome::Ok("null".into()));
    assert_eq!(world.call("mouse", &with(MOUSE)), Outcome::Ok("null".into()));
    assert_eq!(world.call("wheel", &with(r#"{"pane":PANE,"rows":-3,"x":1.5,"y":2.5,"mods":1}"#)), Outcome::Ok("null".into()));
    assert_eq!(world.call("resize", &with(r#"{"pane":PANE,"cols":100,"rows":30}"#)), Outcome::Ok("null".into()));
    assert_eq!(
        world.call("resize", &with(r#"{"pane":PANE,"cols":100,"rows":30,"cell_w":9,"cell_h":null}"#)),
        Outcome::Ok("null".into()),
        "the cell size may be left out or null"
    );
    for (name, template, field) in [
        ("key", KEY.replace(r#""mods":4"#, r#""mods":"4""#), "u32"),
        ("mouse", MOUSE.replace(r#""x":12.5"#, r#""x":"12.5""#), "f64"),
        ("wheel", r#"{"pane":PANE,"rows":"-3","x":1,"y":2,"mods":0}"#.to_owned(), "i32"),
        ("resize", r#"{"pane":PANE,"cols":"100","rows":30}"#.to_owned(), "u32"),
    ] {
        let text = error_text(&world.call(name, &with(&template)));
        assert!(text.contains(field), "{name}: {text}");
    }
    // A float is no whole number.
    assert!(matches!(world.call("resize", &with(r#"{"pane":PANE,"cols":100.5,"rows":30}"#)), Outcome::Err(_)));
}

#[cfg(unix)]
#[test]
fn search_select_and_copy_mode_read_their_tagged_operations() {
    let world = World::new("tagged-ops");
    let pane = world.focus();
    world.core.start_pane(pane, 80, 24, 0, 0).expect("cat starts");
    let with = |template: &str| template.replace("PANE", &pane.to_string());

    assert_eq!(
        world.call("search", &with(r#"{"pane":PANE,"op":{"kind":"set","needle":"a"}}"#)),
        Outcome::Ok(r#"{"total":0,"current":null}"#.into())
    );
    assert_eq!(world.call("search", &with(r#"{"pane":PANE,"op":{"kind":"clear"}}"#)), Outcome::Ok(r#"{"total":0,"current":null}"#.into()));
    assert_eq!(world.call("select", &with(r#"{"pane":PANE,"op":{"kind":"begin","x":1,"y":2,"mode":"word"}}"#)), Outcome::Ok("null".into()));
    assert_eq!(world.call("select", &with(r#"{"pane":PANE,"op":{"kind":"extend","x":4,"y":2}}"#)), Outcome::Ok("null".into()));
    assert_eq!(world.call("select", &with(r#"{"pane":PANE,"op":{"kind":"clear"}}"#)), Outcome::Ok("null".into()));
    let entered = world.call("copy_mode", &with(r#"{"pane":PANE,"op":{"kind":"enter"}}"#));
    assert_eq!(entered, Outcome::Ok(r#"{"active":true,"selecting":false,"yanked":null}"#.into()));
    assert_eq!(
        world.call("copy_mode", &with(r#"{"pane":PANE,"op":{"kind":"move","motion":"line_start"}}"#)),
        Outcome::Ok(r#"{"active":true,"selecting":false,"yanked":null}"#.into())
    );
    // An operation that is not one of the documented ones is refused and no value of it is repeated.
    let text = error_text(&world.call("search", &with(r#"{"pane":PANE,"op":{"kind":"explode"}}"#)));
    assert!(text.contains("unknown variant"), "{text}");
    let text = error_text(&world.call("select", &with(r#"{"pane":PANE,"op":{"kind":"begin","x":1,"y":2,"mode":"paragraph"}}"#)));
    assert!(text.contains("unknown variant"), "{text}");
    let text = error_text(&world.call("select", &with(r#"{"pane":PANE,"op":{"kind":"begin","x":"left","y":2,"mode":"cell"}}"#)));
    assert!(!text.contains("left"), "{text}");
}

// ---- what the browser build does with the commands it does not have -------------------------------------------------

const UNAVAILABLE: [(&str, &str); 8] = [
    ("tear_off_pane", r#"{"pane":PANE,"at_cursor":false}"#),
    ("tear_off_tab", r#"{"pane":PANE,"at_cursor":false}"#),
    ("release_outside", r#"{"pane":PANE,"kind":"pane"}"#),
    ("drag_over", r#"{"pane":PANE,"kind":"tab"}"#),
    ("drag_end", "{}"),
    ("attach_pane", r#"{"pane":PANE,"target":null,"edge":null,"workspace":0}"#),
    ("attach_tab", r#"{"pane":PANE,"workspace":0}"#),
    ("clipboard_text", "{}"),
];

#[test]
fn the_commands_the_browser_does_not_have_answer_one_sentence_and_run_nothing() {
    let world = World::new("unavailable");
    split_first(&world);
    let pane = other_pane(&world);
    let before = world.core.view_in("main").expect("view");

    for (name, template) in UNAVAILABLE {
        let row = lookup(name).expect("a row");
        assert_eq!(row.web, Web::Unavailable, "{name}");
        let outcome = world.call(name, &template.replace("PANE", &pane.to_string()));
        assert_eq!(outcome, Outcome::Err(json_string("Not available in the browser.")), "{name}");
    }

    assert_eq!(world.opened(), Vec::<String>::new(), "no window was made");
    let after = world.core.view_in("main").expect("view");
    assert_eq!((after.panes.clone(), after.tabs.len(), after.windows), (before.panes.clone(), 1, 1));
    assert_eq!(after.rev, before.rev + 1, "nothing but the two views of this test was built");
}

#[test]
fn the_plain_function_of_a_tear_off_would_have_made_a_window_so_the_test_above_can_fail() {
    let world = World::new("tear-off-plain");
    split_first(&world);
    let pane = other_pane(&world);

    holzwave_host::commands::layout::tear_off_pane(&world.core, "main", pane, false).expect("the plain function tears off");

    assert_eq!(world.opened().len(), 1);
}

#[test]
fn an_unavailable_row_answers_the_sentence_whatever_the_arguments_are() {
    let world = World::new("unavailable-any");

    for (name, _) in UNAVAILABLE {
        for args in ["{}", "", "not json", r#"{"pane":"x"}"#, r#"{"unknown":1}"#] {
            assert_eq!(world.call(name, args), Outcome::Err(json_string(NOT_IN_BROWSER)), "{name} {args:?}");
        }
    }
}

#[test]
fn window_ready_runs_the_replay_for_the_window_it_is_given_and_answers_null() {
    let world = World::new("window-ready");

    assert_eq!(world.call("window_ready", "{}"), Outcome::Ok("null".into()));
    assert_eq!(world.call_in("nope", "window_ready", "{}"), Outcome::Err(json_string("no such window")));
}

/// Wait until `done` holds, for at most five seconds.
#[cfg(unix)]
fn eventually(what: &str, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "gave up waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn window_ready_tells_the_page_the_last_title_of_its_panes() {
    let world = World::running("replay", "/bin/sh", &["-c", r"printf '\033]2;hello\007'; sleep 30"]);
    let pane = world.focus();
    world.core.start_pane(pane, 80, 24, 0, 0).expect("the shell starts");
    eventually("the title of the pane", || world.sent("main", "title") >= 1);
    let before = world.sent("main", "title");

    assert_eq!(world.call("window_ready", "{}"), Outcome::Ok("null".into()));

    assert_eq!(world.sent("main", "title"), before + 1, "the last title was told again");
}

#[cfg(unix)]
#[test]
fn copy_selection_answers_false_whatever_is_selected_and_touches_no_clipboard() {
    let world = World::running("copy-selection", "/bin/sh", &["-c", "printf 'some text'; sleep 30"]);
    let pane = world.focus();
    world.core.start_pane(pane, 80, 24, 0, 0).expect("the shell starts");
    let args = format!(r#"{{"pane":{pane}}}"#);
    assert_eq!(world.call("copy_selection", &args), Outcome::Ok("false".into()), "nothing is selected");
    // The output may not have reached the screen yet: select everything again until the selection holds it.
    eventually("a selection with text", || {
        let all = format!(r#"{{"pane":{pane},"op":{{"kind":"all"}}}}"#);
        assert_eq!(world.call("select", &all), Outcome::Ok("null".into()));
        matches!(world.call("selection_text", &args), Outcome::Ok(text) if text.contains("some text"))
    });

    assert_eq!(world.call("copy_selection", &args), Outcome::Ok("false".into()), "the page copies with the browser");
    assert_eq!(world.call("copy_selection", r#"{"pane":4711}"#), Outcome::Ok("false".into()), "not even a pane is looked for");
    assert!(matches!(world.call("copy_selection", r#"{"pane":"x"}"#), Outcome::Err(_)), "the arguments are still read");
}

// ---- guards over every row of the table -----------------------------------------------------------------------------
//
// These read `COMMANDS`, so a row that a later change adds is held to them the moment it is in the table.

/// The groups of the rows of the first table plan.
const FIRST_GROUPS: [&str; 2] = ["terminal", "layout"];

/// The rows of the first table plan (the terminal and layout groups) that the browser build answers with the one sentence.
fn unavailable_rows() -> Vec<&'static Command> {
    COMMANDS.iter().filter(|row| row.web == Web::Unavailable && FIRST_GROUPS.contains(&row.group)).collect()
}

/// A value of the right shape for an argument type, as JSON text, or `None` for a type nobody wrote a sample for. A new
/// type in the table fails `every_argument_type_has_a_sample` until it has one.
fn sample(ty: &str) -> Option<&'static str> {
    let ty: String = ty.chars().filter(|c| !c.is_whitespace()).collect();
    if ty.starts_with("Option<") {
        return Some("null");
    }
    Some(match ty.as_str() {
        "PaneId" | "SplitId" | "u32" | "u64" | "usize" | "i32" => "1",
        "f32" | "f64" => "0.5",
        "bool" => "false",
        "String" => r#""x""#,
        "Edge" => r#""left""#,
        "DropKind" => r#""pane""#,
        "EditActionMsg" => r#""word_back""#,
        "MouseActionMsg" => r#""press""#,
        "MouseButtonMsg" => r#""left""#,
        "SelectOpMsg" | "SearchOpMsg" => r#"{"kind":"clear"}"#,
        "CopyOpMsg" => r#"{"kind":"enter"}"#,
        "Value" => r#""x""#,
        "Vec<String>" => "[]",
        "BlockKind" => r#""files""#,
        "Profile" => r#"{"id":"a","name":"A","host":"h"}"#,
        "SecretChange" => r#"{"kind":"keep"}"#,
        _ => return None,
    })
}

/// The argument object of `row` with a valid sample for every argument.
fn sample_args(row: &Command) -> String {
    let fields: Vec<String> = row
        .args
        .iter()
        .map(|arg| format!("{}:{}", json_string(arg.name), sample(arg.ty).unwrap_or_else(|| panic!("no sample for {}", arg.ty))))
        .collect();
    format!("{{{}}}", fields.join(","))
}

/// What a core shows of its layout besides the revision number, to see that nothing changed.
fn layout_state(world: &World) -> (Vec<(u64, String)>, usize, usize, Vec<String>) {
    let view = world.core.view_in("main").expect("the window main exists");
    (
        view.panes.iter().map(|(pane, rect)| (*pane, format!("{rect:?}"))).collect(),
        view.tabs.len(),
        view.windows,
        view.workspaces.iter().map(|w| w.name.clone()).collect(),
    )
}

#[test]
fn every_argument_type_has_a_sample() {
    for row in COMMANDS {
        for arg in row.args {
            assert!(sample(arg.ty).is_some(), "{}.{} has the type {}, which has no sample in this test", row.name, arg.name, arg.ty);
        }
    }
}

#[test]
fn table_caps_follow_the_limits() {
    for row in COMMANDS {
        let want = match row.name {
            "key" | "mouse" | "wheel" | "resize" => 4 * 1024,
            "paste" | "file_save" | "widget_note_save" | "markdown_render" => 8 * 1024 * 1024,
            _ => 64 * 1024,
        };
        assert_eq!(row.cap, want, "{}", row.name);
        assert!(row.cap > 0 && row.cap <= 8 * 1024 * 1024, "{}", row.name);
        assert!(row.admits(row.cap) && !row.admits(row.cap + 1), "{}", row.name);
    }
}

#[test]
fn table_rows_refuse_unknown_fields() {
    let world = World::new("unknown-all");
    let mut checked = 0;
    for row in COMMANDS.iter().filter(|row| row.web != Web::Unavailable) {
        let (events, state) = (world.events(), layout_state(&world));
        // The extra field alone, and the extra field after a valid sample of every argument: the second one shows that
        // the field is what is refused and that the sample is well formed.
        let alone = r#"{"zz_extra":1}"#.to_owned();
        let sample = sample_args(row);
        let after = format!("{},\"zz_extra\":1}}", &sample[..sample.len() - 1]);
        let after = if row.args.is_empty() { alone.clone() } else { after };
        for args in [alone, after] {
            let text = error_text(&world.call(row.name, &args));
            assert!(text.contains("zz_extra") && text.contains("unknown field"), "{}: {text}", row.name);
            assert!(text.chars().count() <= ERROR_TEXT_LIMIT, "{}", row.name);
        }
        assert_eq!(world.events(), events, "{}: a refused call changed something", row.name);
        assert_eq!(layout_state(&world), state, "{}: a refused call changed the layout", row.name);
        assert_eq!(world.opened(), Vec::<String>::new(), "{}: a refused call made a window", row.name);
        checked += 1;
    }
    assert!(checked >= 37, "only {checked} rows were checked");
}

#[test]
fn table_rows_refuse_missing_fields() {
    let world = World::new("missing-all");
    let mut checked = 0;
    for row in COMMANDS.iter().filter(|row| row.web != Web::Unavailable) {
        let first_required = row.args.iter().find(|arg| !arg.ty.replace(' ', "").starts_with("Option<"));
        let Some(first) = first_required else { continue };
        let text = error_text(&world.call(row.name, "{}"));
        assert!(
            text.contains("missing field") && text.contains(first.name),
            "{}: `{{}}` should be refused for the missing {}: {text}",
            row.name,
            first.name
        );
        checked += 1;
    }
    assert!(checked >= 30, "only {checked} rows were checked");
}

#[test]
fn table_rows_accept_an_object_with_only_their_optional_fields() {
    let world = World::new("optional-only");
    for row in COMMANDS.iter().filter(|row| row.web != Web::Unavailable) {
        if row.args.iter().all(|arg| arg.ty.replace(' ', "").starts_with("Option<")) && !row.args.is_empty() {
            let outcome = world.call(row.name, "{}");
            let text = match &outcome {
                Outcome::Err(json) => json.clone(),
                _ => String::new(),
            };
            assert!(!text.contains("missing field"), "{}: an optional field is required: {text}", row.name);
        }
    }
}

#[test]
fn error_text_is_bounded() {
    let world = World::new("bounded-all");
    let long = "A".repeat(100 * 1024);
    // A refusal is bounded and never holds the value. (A long value that is a valid value of a text argument, such as a
    // name, is no refusal: the command takes it and the core cuts it.)
    let check = |row: &Command, outcome: Outcome, what: &str| {
        let Outcome::Err(json) = outcome else { return };
        let text: String = serde_json::from_str(&json).unwrap_or_else(|_| panic!("{} {what}: {json:.100}", row.name));
        assert!(text.chars().count() <= ERROR_TEXT_LIMIT, "{} {what}: {} characters", row.name, text.chars().count());
        // The name of a field that is wrong is named, cut; a value is never repeated.
        assert!(
            what == "as a field name" || !text.contains("AAAAAAAAAA"),
            "{} {what}: the long value is in the error: {text}",
            row.name
        );
    };
    let mut refused_values = 0;
    for row in COMMANDS.iter().filter(|row| row.web != Web::Unavailable) {
        check(row, world.call(row.name, &format!("{{\"{long}\":1}}")), "as a field name");
        if let Some(first) = row.args.first() {
            let outcome = world.call(row.name, &format!("{{\"{}\":\"{long}\"}}", first.name));
            refused_values += usize::from(matches!(outcome, Outcome::Err(_)));
            check(row, outcome, "as a value");
        }
    }
    assert!(refused_values >= 30, "only {refused_values} rows refused the long value");
}

#[test]
fn unavailable_rows_do_not_leak() {
    let world = World::new("leak");
    split_first(&world);
    let named: BTreeSet<&str> = UNAVAILABLE.iter().map(|(name, _)| *name).collect();
    let rows: BTreeSet<&str> = unavailable_rows().iter().map(|row| row.name).collect();
    assert_eq!(rows, named, "the table and the list of this test name different unavailable rows");

    let pane = other_pane(&world);
    let (events, state) = (world.events(), layout_state(&world));
    for row in unavailable_rows() {
        let right_shape = sample_args(row);
        let other_values = right_shape.replace(":1", &format!(":{pane}"));
        let expected = Outcome::Err(json_string(NOT_IN_BROWSER));
        for args in [right_shape, other_values, "{}".to_owned()] {
            assert_eq!(world.call(row.name, &args), expected, "{} {args}", row.name);
        }
        assert_eq!(row.error, Some("String"), "{}: it answers the error value as a string", row.name);
    }
    assert_eq!(world.events(), events);
    assert_eq!(layout_state(&world), state);
    assert_eq!(world.opened(), Vec::<String>::new());
}

#[test]
fn a_window_row_passes_the_label_it_is_given_and_a_pane_row_ignores_it() {
    let world = World::new("scope");
    assert_eq!(world.call_in("nope", "get_layout", "{}"), Outcome::Err(json_string("no such window")));
    let own = world.core.selection_text(4711).expect_err("no such pane");
    for label in ["main", "window-7", "nope"] {
        assert_eq!(world.call_in(label, "selection_text", r#"{"pane":4711}"#), Outcome::Err(json_string(&own)), "{label}");
    }
    let windows: Vec<&str> = COMMANDS.iter().filter(|row| row.scope == Scope::Window).map(|row| row.name).collect();
    for name in ["get_layout", "split", "new_tab", "close_pane", "move_pane", "window_ready", "tear_off_pane"] {
        assert!(windows.contains(&name), "{name} works on the layout of the calling window");
    }
    for name in ["key", "paste", "selection_text", "mouse", "start_pane", "copy_selection"] {
        assert!(!windows.contains(&name), "{name} names a pane and needs no window");
    }
}

// ---- the file commands: a grant with a browser address and a raw range ------------------------------------------------

/// The eight-mebibyte piece a range answer is cut to, as the host crate spells it.
const CHUNK: usize = 8 * 1024 * 1024;

/// The byte at place `i` of a picture file made by [`write_picture`] after its 16-byte header.
fn body_byte(i: usize) -> u8 {
    (i % 251) as u8
}

/// A file that a grant accepts (the first bytes of a PNG, then a known pattern) of `total` bytes. Returns its path text.
fn write_picture(world: &World, name: &str, total: usize) -> (String, Vec<u8>) {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
    bytes.extend((0..total - bytes.len()).map(body_byte));
    let path = world.dir.join(name);
    std::fs::write(&path, &bytes).expect("a picture file");
    (path.to_string_lossy().into_owned(), bytes)
}

/// Grant the file at `path` through the registry and return the token of the answer.
fn grant_token(world: &World, path: &str) -> String {
    let args = format!(r#"{{"source":"local","path":{}}}"#, json_string(path));
    let Outcome::Ok(text) = world.call("file_grant", &args) else { panic!("file_grant did not answer") };
    let answer: serde_json::Value = serde_json::from_str(&text).expect("the answer is JSON");
    answer["token"].as_str().expect("a token").to_owned()
}

fn range_args(token: &str, offset: u64, len: u64) -> String {
    format!(r#"{{"token":{},"offset":{offset},"len":{len}}}"#, json_string(token))
}

#[test]
fn tracer_a_grant_answers_the_address_of_the_browser_and_the_members_of_the_desktop_answer() {
    let world = World::new("tracer-grant");
    let (path, bytes) = write_picture(&world, "a.bin", 20 * 1024);

    let outcome = world.call("file_grant", &format!(r#"{{"source":"local","path":{}}}"#, json_string(&path)));

    let Outcome::Ok(text) = outcome else { panic!("file_grant answered {outcome:?}") };
    let answer: serde_json::Value = serde_json::from_str(&text).expect("the answer is JSON");
    let token = answer["token"].as_str().expect("a token");
    assert_eq!(token.len(), 32, "{token}");
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(answer["url"], format!("/files/{token}"));
    // The members of the desktop answer, and nothing else: the url first, then the flattened grant.
    let mut members: Vec<&str> = answer.as_object().expect("an object").keys().map(String::as_str).collect();
    members.sort_unstable();
    assert_eq!(members, ["mime", "size", "token", "url"]);
    assert_eq!((answer["mime"].as_str(), answer["size"].as_u64()), (Some("image/png"), Some(bytes.len() as u64)));
    assert!(text.starts_with(&format!(r#"{{"url":"/files/{token}","token":"{token}""#)), "{text}");
    // The grant is the core's: the core knows the token and what it names.
    assert_eq!(world.core.granted(token), Some(("image/png".to_owned(), bytes.len() as u64)));
}

#[test]
fn tracer_a_range_answers_raw_bytes_cut_to_the_chunk_and_to_the_end_of_the_file() {
    let world = World::new("tracer-range");
    let (path, bytes) = write_picture(&world, "a.bin", 20 * 1024);
    let (big_path, big) = write_picture(&world, "big.bin", CHUNK + 1000);
    let (token, big_token) = (grant_token(&world, &path), grant_token(&world, &big_path));

    assert_eq!(world.call("file_range", &range_args(&token, 5, 100)), Outcome::Binary(bytes[5..105].to_vec()));
    assert_eq!(world.call("file_range", &range_args(&token, 0, 0)), Outcome::Binary(Vec::new()));
    // Fewer bytes at the end of the file, and none past it.
    let tail = bytes.len() as u64 - 10;
    assert_eq!(world.call("file_range", &range_args(&token, tail, 100)), Outcome::Binary(bytes[bytes.len() - 10..].to_vec()));
    assert_eq!(world.call("file_range", &range_args(&token, bytes.len() as u64 + 50, 100)), Outcome::Binary(Vec::new()));
    // Nine mebibytes asked for: the chunk comes back, and it is the first chunk of the file.
    let Outcome::Binary(piece) = world.call("file_range", &range_args(&big_token, 0, 9 * 1024 * 1024)) else {
        panic!("a range is a raw answer")
    };
    assert_eq!(piece.len(), CHUNK);
    assert!(piece == big[..CHUNK], "the chunk is the start of the file");
    // And a piece of exactly the chunk from further in.
    let Outcome::Binary(rest) = world.call("file_range", &range_args(&big_token, 500, u64::MAX)) else { panic!("raw") };
    assert_eq!(rest.len(), CHUNK);
    assert!(rest == big[500..500 + CHUNK]);
}

#[test]
fn tracer_a_range_of_a_grant_nobody_made_or_that_was_revoked_answers_no_such_grant() {
    let world = World::new("tracer-revoked");
    let (path, bytes) = write_picture(&world, "a.bin", 20 * 1024);
    let token = grant_token(&world, &path);
    let unknown = "0".repeat(32);
    let refused = Outcome::Err(json_string("no such grant"));

    assert_eq!(world.call("file_range", &range_args(&unknown, 0, 10)), refused);
    assert_eq!(world.call("file_range", &range_args(&token, 0, 10)), Outcome::Binary(bytes[..10].to_vec()));
    assert_eq!(world.call("file_revoke", &format!(r#"{{"token":"{token}"}}"#)), Outcome::Ok("null".into()));
    assert_eq!(world.call("file_range", &range_args(&token, 0, 10)), refused, "the grant ended");
    assert_eq!(world.call("file_revoke", &format!(r#"{{"token":"{unknown}"}}"#)), Outcome::Ok("null".into()), "nothing to end");
    assert_eq!(world.core.granted(&token), None);
}

#[test]
fn tracer_a_range_reads_its_arguments_strictly_and_a_grant_of_a_text_file_is_refused_as_before() {
    let world = World::new("tracer-strict");
    let (path, _) = write_picture(&world, "a.bin", 4096);
    let token = grant_token(&world, &path);
    let before = world.events();

    let text = error_text(&world.call("file_range", &format!(r#"{{"token":"{token}","offset":"5","len":1}}"#)));
    assert!(text.contains("u64") && !text.contains('5'), "what was expected, not what was sent: {text}");
    let text = error_text(&world.call("file_range", &format!(r#"{{"token":"{token}","offset":5,"len":1,"extra":1}}"#)));
    assert!(text.contains("extra") && text.contains("unknown field"), "{text}");
    let text = error_text(&world.call("file_range", &format!(r#"{{"token":"{token}","offset":5}}"#)));
    assert!(text.contains("len") && text.contains("missing field"), "{text}");
    assert_eq!(world.events(), before);

    let note = world.dir.join("note.txt");
    std::fs::write(&note, "not a picture").expect("a text file");
    let own = world.core.file_grant("local", note.to_str().expect("utf-8")).expect_err("text is not granted");
    let args = format!(r#"{{"source":"local","path":{}}}"#, json_string(note.to_str().expect("utf-8")));
    assert_eq!(world.call("file_grant", &args), Outcome::Err(json_string(&own)));
}

/// A host that is the neutral one in everything but what a server sets: the address of a file, the process list, the text
/// scale and the source of the Local Network permission.
struct Elsewhere {
    base: NeutralHost,
    processes: bool,
    scale: u32,
    source: Arc<dyn holzwave_host::local_network::Source>,
}

impl Host for Elsewhere {
    fn platform(&self) -> Platform {
        Platform::Web
    }

    fn secrets(&self) -> Arc<dyn holzwave_core::secrets::SecretStore> {
        self.base.secrets()
    }

    fn local_network(&self) -> Arc<dyn holzwave_host::local_network::Source> {
        self.source.clone()
    }

    fn text_scale(&self) -> u32 {
        self.scale
    }

    fn limits(&self) -> HostLimits {
        HostLimits::default()
    }

    fn file_url(&self, token: &str) -> String {
        format!("https://files.example.test/d/{token}")
    }

    fn local_processes(&self) -> bool {
        self.processes
    }
}

fn elsewhere(processes: bool) -> Arc<dyn Host> {
    elsewhere_with(processes, 100, Arc::new(holzwave_host::local_network::Unsupported))
}

fn elsewhere_with(processes: bool, scale: u32, source: Arc<dyn holzwave_host::local_network::Source>) -> Arc<dyn Host> {
    Arc::new(Elsewhere { base: NeutralHost::new(), processes, scale, source })
}

#[test]
fn tracer_the_two_seams_have_the_defaults_of_the_browser_build() {
    let host = NeutralHost::new();

    assert_eq!(host.file_url("abc"), "/files/abc");
    assert_eq!(host.file_url(&"f".repeat(32)), format!("/files/{}", "f".repeat(32)));
    assert!(host.local_processes());
}

#[test]
fn tracer_the_address_of_a_grant_is_what_the_host_says_and_the_other_members_do_not_change() {
    let neutral = World::new("tracer-url-neutral");
    let other = World::hosted("tracer-url-other", elsewhere(true));
    let (path, _) = write_picture(&neutral, "a.bin", 4096);
    let (other_path, _) = write_picture(&other, "a.bin", 4096);

    let (Outcome::Ok(a), Outcome::Ok(b)) = (
        neutral.call("file_grant", &format!(r#"{{"source":"local","path":{}}}"#, json_string(&path))),
        other.call("file_grant", &format!(r#"{{"source":"local","path":{}}}"#, json_string(&other_path))),
    ) else {
        panic!("both grants are answered")
    };

    let (a, b): (serde_json::Value, serde_json::Value) = (serde_json::from_str(&a).unwrap(), serde_json::from_str(&b).unwrap());
    assert_eq!(a["url"], format!("/files/{}", a["token"].as_str().unwrap()));
    assert_eq!(b["url"], format!("https://files.example.test/d/{}", b["token"].as_str().unwrap()));
    assert_eq!((&a["mime"], &a["size"]), (&b["mime"], &b["size"]));
}

#[test]
fn tracer_only_file_range_answers_raw_bytes() {
    let raw: Vec<&str> = COMMANDS.iter().filter(|row| row.raw).map(|row| row.name).collect();

    assert_eq!(raw, ["file_range"]);
    let row = lookup("file_range").expect("file_range");
    assert_eq!(row.answer.replace(' ', ""), "tauri::ipc::Response");
    assert_eq!(row.error, Some("String"));
    assert_eq!((row.scope, row.mode, row.cap, row.web), (Scope::None, Mode::Async, CAP_DEFAULT, Web::Same));
    let revoke = lookup("file_revoke").expect("file_revoke");
    assert_eq!((revoke.mode, revoke.error, revoke.answer), (Mode::Sync, None, "()"));
}

// ---- the second group: settings, connections, blocks, files, history, sync and the browser build ------------------------

/// The outcome the registry gives for the result of a plain call of the core: the JSON of an answer, or of the error text.
fn same_as<T: serde::Serialize>(result: Result<T, String>) -> Outcome {
    match result {
        Ok(value) => Outcome::Ok(serde_json::to_string(&value).expect("an answer is JSON")),
        Err(error) => Outcome::Err(json_string(&error)),
    }
}

/// The same for a call that cannot fail.
fn same_ok<T: serde::Serialize>(value: T) -> Outcome {
    same_as(Ok::<T, String>(value))
}

/// The JSON value of an answer that must be an `Ok`.
fn answer(outcome: Outcome) -> serde_json::Value {
    match outcome {
        Outcome::Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| panic!("{text:.200} is not JSON: {e}")),
        other => panic!("expected an answer, got {other:?}"),
    }
}

/// Every file of a directory tree, as (path, bytes).
fn files_below(dir: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(path),
                Ok(kind) if kind.is_file() => {
                    let bytes = std::fs::read(&path).unwrap_or_default();
                    found.push((path, bytes));
                }
                _ => {}
            }
        }
    }
    found
}

/// Fails when a file below `dir` holds `secret`.
fn assert_nothing_below_holds(dir: &std::path::Path, secret: &str) {
    for (path, bytes) in files_below(dir) {
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains(secret), "{} holds the secret", path.display());
    }
}

fn font_size(view: &serde_json::Value) -> u64 {
    view["settings"]["font_size"].as_u64().expect("the view has the font size")
}

#[test]
fn settings_are_set_read_and_reset_through_the_registry() {
    let world = World::new("settings-roundtrip");
    let default = font_size(&answer(world.call("get_settings", "{}")));
    let other = if default == 17 { 18 } else { 17 };

    let set = answer(world.call("set_setting", &format!(r#"{{"key":"font_size","value":{other}}}"#)));
    assert_eq!(font_size(&set), other);
    assert_eq!(font_size(&answer(world.call("get_settings", "{}"))), other, "the value is in force");
    assert!(std::fs::read_to_string(world.dir.join("settings.json")).expect("the file was written").contains(&other.to_string()));

    let reset = answer(world.call("reset_setting", r#"{"key":"font_size"}"#));
    assert_eq!(font_size(&reset), default);
    assert_eq!(world.call("get_settings", "{}"), same_ok(world.core.get_settings()));
}

#[test]
fn a_setting_the_core_refuses_answers_the_error_text_of_the_core() {
    let world = World::new("settings-refused");
    let unknown = world.call("set_setting", r#"{"key":"no_such_setting","value":1}"#);
    let wrong = world.call("set_setting", r#"{"key":"font_size","value":"big"}"#);
    let reset = world.call("reset_setting", r#"{"key":"no_such_setting"}"#);

    assert_eq!(unknown, same_as(world.core.set_setting("no_such_setting", serde_json::json!(1))));
    assert_eq!(wrong, same_as(world.core.set_setting("font_size", serde_json::json!("big"))));
    assert_eq!(reset, same_as(world.core.reset_setting("no_such_setting")));
    assert!(error_text(&unknown).contains("no_such_setting"), "{unknown:?}");
    assert!(error_text(&wrong).contains("font_size"), "{wrong:?}");
    assert!(error_text(&reset).contains("no_such_setting"), "{reset:?}");
    assert!(!world.dir.join("settings.json").exists(), "a refused setting writes nothing");
}

#[test]
fn reload_settings_shows_a_file_that_was_edited() {
    let world = World::new("settings-reload");
    world.call("set_setting", r#"{"key":"font_size","value":9}"#);
    let unchanged = answer(world.call("reload_settings", "{}"));
    assert_eq!(font_size(&unchanged), 9, "nothing changed on disk");
    let path = world.dir.join("settings.json");
    let text = std::fs::read_to_string(&path).expect("settings.json");
    assert!(text.contains(r#""font_size": 9"#), "{text}");
    std::fs::write(&path, text.replace(r#""font_size": 9"#, r#""font_size": 12"#)).expect("edit the file");

    let reloaded = answer(world.call("reload_settings", "{}"));

    assert_eq!(font_size(&reloaded), 12);
    assert_eq!(font_size(&answer(world.call("get_settings", "{}"))), 12);
}

#[test]
fn the_text_scale_is_the_number_the_host_gives() {
    let neutral = World::new("scale-neutral");
    let large = World::hosted("scale-large", elsewhere_with(true, 150, Arc::new(CountingSource::default())));

    assert_eq!(neutral.call("os_text_scale", "{}"), Outcome::Ok("100".into()));
    assert_eq!(large.call("os_text_scale", "{}"), Outcome::Ok("150".into()));
}

#[test]
fn markdown_is_rendered_by_the_core_and_a_document_over_its_limit_answers_null() {
    let world = World::new("markdown");
    let render = |text: &str| world.call("markdown_render", &format!(r#"{{"text":{}}}"#, json_string(text)));

    let html = render("# a");
    assert_eq!(html, same_ok(holzwave_core::preview::render_markdown("# a")));
    assert!(matches!(&html, Outcome::Ok(text) if text.contains("<h1")), "{html:?}");
    let limit = 2 * 1024 * 1024;
    assert_eq!(render(&"a".repeat(limit + 1)), Outcome::Ok("null".into()));
    assert!(matches!(render(&"a".repeat(limit)), Outcome::Ok(text) if text.len() > limit));
    // The page's own script never gets through the renderer.
    let text = answer(render("<script>alert(1)</script>\n\nplain")).as_str().expect("html").to_owned();
    assert!(!text.contains("<script"), "{text}");
}

#[test]
fn app_info_names_the_version_the_platform_and_the_host() {
    let world = World::new("app-info");

    let info = answer(world.call("app_info", "{}"));

    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(info["os"], std::env::consts::OS);
    assert_eq!(world.call("app_info", "{}"), same_ok(world.core.app_info()));
    let web = World::hosted("app-info-web", elsewhere(false));
    assert_eq!(answer(web.call("app_info", "{}"))["os"], "web", "the platform is the host's");
}

// ---- the commands that start programs or read the desktop's permission are not available in the browser ----------------

/// A source of the Local Network permission that counts every question it is asked.
#[derive(Default)]
struct CountingSource {
    asked: AtomicUsize,
}

impl holzwave_host::local_network::Source for CountingSource {
    fn applies(&self) -> bool {
        self.asked.fetch_add(1, Ordering::SeqCst);
        true
    }

    fn probe(&self, _window: std::time::Duration) -> holzwave_core::local_network::Reading {
        self.asked.fetch_add(1, Ordering::SeqCst);
        holzwave_core::local_network::Reading::Unknown
    }
}

const UNAVAILABLE_BROWSER: [(&str, &str); 5] = [
    ("open_url", r#"{"url":"ftp://example.test/x"}"#),
    ("reveal_path", r#"{"path":"/no/such/holzwave/path","base":null}"#),
    ("local_network_status", "{}"),
    ("local_network_answer", r#"{"allow":true}"#),
    ("local_network_open_settings", "{}"),
];

#[test]
fn the_commands_that_need_the_desktop_answer_one_sentence_and_run_nothing() {
    let source = Arc::new(CountingSource::default());
    let world = World::hosted("needs-desktop", elsewhere_with(true, 100, source.clone()));
    // What the plain functions would have answered: the checks of the core run before it starts anything, so a link of
    // another scheme and a path that is not there are refused with their own sentences, and nothing is started.
    let own_open = world.core.open_url("ftp://example.test/x").expect_err("the core refuses another scheme");
    let own_reveal = world.core.reveal_path("/no/such/holzwave/path", None).expect_err("the core refuses a missing path");
    assert_ne!(own_open, NOT_IN_BROWSER);
    assert_ne!(own_reveal, NOT_IN_BROWSER);
    // The counter works: asking the host of the permission by hand is counted.
    let before = source.asked.load(Ordering::SeqCst);
    let _ = world.core.local_network().view();
    assert!(source.asked.load(Ordering::SeqCst) > before, "the source counts what it is asked");
    let (events, asked, windows) = (world.events(), source.asked.load(Ordering::SeqCst), world.opened());

    for (name, args) in UNAVAILABLE_BROWSER {
        let row = lookup(name).expect("a row");
        assert_eq!(row.web, Web::Unavailable, "{name}");
        assert_eq!(world.call(name, args), Outcome::Err(json_string(NOT_IN_BROWSER)), "{name}");
        for other in ["{}", "", "not json", r#"{"unknown":1}"#] {
            assert_eq!(world.call(name, other), Outcome::Err(json_string(NOT_IN_BROWSER)), "{name} {other:?}");
        }
    }

    assert_eq!(source.asked.load(Ordering::SeqCst), asked, "the permission source was not asked");
    assert_eq!((world.events(), world.opened()), (events, windows));
    assert!(!world.dir.join("local-network.json").exists(), "no choice was stored");
}

#[test]
fn the_unavailable_rows_of_this_group_are_exactly_these_five() {
    let rows: BTreeSet<&str> =
        COMMANDS.iter().filter(|row| row.web == Web::Unavailable && !FIRST_GROUPS.contains(&row.group)).map(|row| row.name).collect();
    let named: BTreeSet<&str> = UNAVAILABLE_BROWSER.iter().map(|(name, _)| *name).collect();
    assert_eq!(rows, named);
}

// ---- connections, login prompts and the installation of wsh --------------------------------------------------------------

#[test]
fn a_saved_connection_is_listed_without_any_secret_and_a_deleted_one_is_gone() {
    let world = World::new("connections");
    assert_eq!(answer(world.call("connections_list", "{}")), serde_json::json!([]));
    let save = r#"{"profile":{"id":"web","name":"Web","host":"example.test","user":"u"},"secret":{"kind":"set","value":"hunter2-connection-secret"}}"#;

    let saved = world.call("connection_save", save);

    let saved = answer(saved);
    assert_eq!((saved["id"].as_str(), saved["has_secret"].as_bool()), (Some("web"), Some(true)));
    let listed = world.call("connections_list", "{}");
    let Outcome::Ok(text) = &listed else { panic!("{listed:?}") };
    assert!(!text.contains("hunter2"), "the list holds a secret: {text}");
    assert_eq!(listed, same_ok(world.core.connections_list()));
    assert_eq!(answer(listed)[0]["id"], "web");
    assert_nothing_below_holds(&world.dir, "hunter2-connection-secret");

    assert_eq!(world.call("connection_delete", r#"{"id":"web"}"#), Outcome::Ok("null".into()));
    assert_eq!(answer(world.call("connections_list", "{}")), serde_json::json!([]));
    assert_eq!(world.call("connection_delete", r#"{"id":"web"}"#), same_as(world.core.connection_delete("web")));
}

#[test]
fn a_connection_the_core_refuses_answers_the_text_of_the_core() {
    let world = World::new("connection-refused");
    let own = world.core.connect_in("main", "no-such-connection").expect_err("no such connection");

    assert_eq!(world.call("connect", r#"{"id":"no-such-connection"}"#), Outcome::Err(json_string(&own)));
    assert_eq!(world.call_in("nope", "connect", r#"{"id":"no-such-connection"}"#), same_as(world.core.connect_in("nope", "no-such-connection")));
    let bad = world.call("connection_save", r#"{"profile":{"id":"Bad Id","name":"x","host":"h"},"secret":{"kind":"keep"}}"#);
    assert!(matches!(bad, Outcome::Err(_)), "{bad:?}");
    assert_eq!(answer(world.call("connections_list", "{}")), serde_json::json!([]), "nothing was saved");
    assert_eq!(answer(world.call("pane_connections", "{}")), serde_json::json!([]), "no pane has a connection on a fresh core");
}

#[test]
fn a_login_prompt_nobody_asked_changes_nothing() {
    let world = World::new("prompts");

    assert_eq!(world.call("login_prompt_answer", r#"{"id":"no-such-prompt","answers":["one","two"]}"#), Outcome::Ok("false".into()));
    assert_eq!(world.call("login_prompt_cancel", r#"{"id":"no-such-prompt"}"#), Outcome::Ok("false".into()));
    assert_eq!(world.call("login_prompt_answer", r#"{"id":"","answers":[]}"#), same_ok(world.core.login_prompt_answer("", Vec::new())));
    assert!(matches!(world.call("login_prompt_answer", r#"{"id":"x","answers":"one"}"#), Outcome::Err(_)), "answers is a list");
}

#[test]
fn wsh_install_answers_the_sentences_of_the_core() {
    let world = World::new("wsh-install");
    let pane = world.focus();

    assert_eq!(world.call("wsh_install", "{\"pane\":4711}"), same_as(world.core.wsh_install(4711)));
    assert_eq!(world.call("wsh_install", &format!(r#"{{"pane":{pane}}}"#)), same_as(world.core.wsh_install(pane)));
    assert!(matches!(world.call("wsh_install", "{\"pane\":4711}"), Outcome::Err(_)));
    let dir = world.dir.to_string_lossy().into_owned();
    let opened = answer(world.call("block_open", &format!(r#"{{"kind":"files","source":"local","path":{},"placement":"tab"}}"#, json_string(&dir))));
    let block = opened["focus"].as_u64().expect("the new pane has the focus");
    let own = world.core.wsh_install(block);
    assert!(own.is_err());
    assert_eq!(world.call("wsh_install", &format!(r#"{{"pane":{block}}}"#)), same_as(own));
}

// ---- blocks, widgets and the monitor ------------------------------------------------------------------------------------

/// Open the same block on two cores that were made alike and compare the views the registry and the core answer. The path
/// is the directory of the first core for both: it is only read.
fn assert_same_block_view(tag: &str, kind: &str, source: &str, path: impl Fn(&World) -> String, placement: &str) {
    let (through, direct) = (World::new(&format!("{tag}-through")), World::new(&format!("{tag}-direct")));
    let path = path(&through);
    let args = format!(
        r#"{{"kind":"{kind}","source":"{source}","path":{},"placement":"{placement}"}}"#,
        json_string(&path)
    );
    let kind_value: holzwave_core::blocks::BlockKind = serde_json::from_value(serde_json::json!(kind)).expect("a kind");

    let answered = through.call("block_open", &args);
    let own = direct.core.block_open_in("main", placement, kind_value, source, &path);

    assert_eq!(answered, same_as(own), "{kind} {placement}");
    assert!(matches!(answered, Outcome::Ok(_)), "{kind} {placement}: {answered:?}");
    assert_eq!(through.call("pane_blocks", "{}"), same_ok(through.core.blocks_list()));
    assert_eq!(through.call("pane_blocks", "{}"), same_ok(direct.core.blocks_list()), "the two cores list the same block");
}

#[test]
fn blocks_of_each_kind_open_as_the_core_opens_them() {
    assert_same_block_view("block-files", "files", "local", |w| w.dir.to_string_lossy().into_owned(), "tab");
    assert_same_block_view("block-files-right", "files", "local", |w| w.dir.to_string_lossy().into_owned(), "right");
    assert_same_block_view(
        "block-preview",
        "preview",
        "local",
        |w| {
            let file = w.dir.join("readme.md");
            std::fs::write(&file, "# hello\n").expect("a file");
            file.to_string_lossy().into_owned()
        },
        "down",
    );
    assert_same_block_view("block-monitor", "monitor", "local", |_| String::new(), "tab");
}

#[test]
fn a_block_can_be_moved_to_another_path_and_the_core_refuses_what_it_refuses() {
    let world = World::new("block-update");
    let dir = world.dir.to_string_lossy().into_owned();
    let sub = world.dir.join("sub");
    std::fs::create_dir(&sub).expect("a folder");
    let opened = answer(world.call("block_open", &format!(r#"{{"kind":"files","source":"local","path":{},"placement":"tab"}}"#, json_string(&dir))));
    let pane = opened["focus"].as_u64().expect("the new pane");

    let moved = world.call("block_update", &format!(r#"{{"pane":{pane},"path":{}}}"#, json_string(&sub.to_string_lossy())));

    assert_eq!(moved, Outcome::Ok("null".into()));
    let listed = answer(world.call("pane_blocks", "{}"));
    assert_eq!(listed[0]["path"], sub.to_string_lossy().as_ref());
    assert_eq!(world.call("block_update", r#"{"pane":4711,"path":"/"}"#), same_as(world.core.block_update(4711, "/")));
    assert!(matches!(world.call("block_update", r#"{"pane":4711,"path":"/"}"#), Outcome::Err(_)));
}

#[test]
fn widgets_start_empty_and_a_note_is_saved_with_the_core_limits() {
    let world = World::new("widgets");
    assert_eq!(world.call("widgets_list", "{}"), same_ok(world.core.widgets_list()));
    assert_eq!(answer(world.call("widgets_list", "{}"))["notes"], serde_json::json!({}));

    assert_eq!(world.call("widget_note_save", r#"{"id":"scratch","text":"remember this"}"#), Outcome::Ok("null".into()));

    assert_eq!(answer(world.call("widgets_list", "{}"))["notes"]["scratch"], "remember this");
    let long = "n".repeat(64 * 1024 + 1);
    let own = world.core.widget_note_save("scratch", &long);
    let through = world.call("widget_note_save", &format!(r#"{{"id":"scratch","text":"{long}"}}"#));
    assert_eq!(through, same_as(own));
    assert!(matches!(through, Outcome::Err(_)), "a note of 64 KiB and one byte is refused by the core");
    assert_eq!(answer(world.call("widgets_list", "{}"))["notes"]["scratch"], "remember this", "the old note is kept");
}

/// Open a monitor block on `world` and return the pane.
fn monitor_pane(world: &World) -> u64 {
    let opened = answer(world.call("block_open", r#"{"kind":"monitor","source":"local","path":"","placement":"tab"}"#));
    opened["focus"].as_u64().expect("the new pane has the focus")
}

#[test]
fn a_pane_that_is_not_a_monitor_gets_the_error_of_the_core() {
    let world = World::new("monitor-error");
    let pane = world.focus();

    assert_eq!(world.call("monitor_sample", &format!(r#"{{"pane":{pane}}}"#)), same_as(world.core.monitor_sample(pane)));
    assert!(matches!(world.call("monitor_sample", &format!(r#"{{"pane":{pane}}}"#)), Outcome::Err(_)));
    assert_eq!(world.call("monitor_sample", "{\"pane\":4711}"), same_as(world.core.monitor_sample(4711)));
}

#[test]
fn the_local_monitor_lists_processes_only_where_the_host_allows_it() {
    let desktop = World::new("monitor-desktop");
    let web = World::hosted("monitor-web", elsewhere(false));
    let shared = World::hosted("monitor-shared", elsewhere(true));

    for (world, procs) in [(&desktop, true), (&shared, true), (&web, false)] {
        let pane = monitor_pane(world);
        let reading = answer(world.call("monitor_sample", &format!(r#"{{"pane":{pane}}}"#)));
        assert_eq!(reading["state"], "live");
        let listed = reading["procs"].as_array().expect("a list of processes");
        assert_eq!(!listed.is_empty(), procs, "{reading}");
        // The numbers are there either way.
        assert!(reading["cores"].as_u64().is_some_and(|cores| cores >= 1));
        assert!(reading["mem_total"].as_u64().is_some_and(|total| total > 0));
        assert!(reading["cpu"].is_number() && reading["uptime"].is_number());
        assert!(reading["net_rx"].is_number() && reading["net_tx"].is_number() && reading["mem_used"].is_number());
    }
}

// ---- files --------------------------------------------------------------------------------------------------------------

#[test]
fn the_file_commands_answer_what_the_core_answers() {
    let world = World::new("files");
    std::fs::create_dir(world.dir.join("folder")).expect("a folder");
    std::fs::write(world.dir.join("note.txt"), "some text\n").expect("a text file");
    std::fs::write(world.dir.join("readme.md"), "# Title\n\nbody\n").expect("a markdown file");
    let (dir, text, markdown) = (
        world.dir.to_string_lossy().into_owned(),
        world.dir.join("note.txt").to_string_lossy().into_owned(),
        world.dir.join("readme.md").to_string_lossy().into_owned(),
    );
    let missing = world.dir.join("missing.txt").to_string_lossy().into_owned();
    let args = |path: &str| format!(r#"{{"source":"local","path":{}}}"#, json_string(path));

    assert_eq!(world.call("files_list", &args(&dir)), same_as(world.core.files_list("local", &dir)));
    for path in [&text, &markdown, &missing] {
        assert_eq!(world.call("file_open", &args(path)), same_as(world.core.file_open("local", path)), "open {path}");
        assert_eq!(world.call("file_stat", &args(path)), same_as(world.core.file_stat("local", path)), "stat {path}");
    }
    assert!(matches!(world.call("file_open", &args(&missing)), Outcome::Err(_)));
    assert!(matches!(world.call("files_list", &args(&missing)), Outcome::Err(_)));
    let listing = answer(world.call("files_list", &args(&dir)));
    let names: Vec<&str> = listing["entries"].as_array().expect("entries").iter().map(|e| e["name"].as_str().expect("a name")).collect();
    assert_eq!(names[0], "folder", "folders first: {names:?}");
    let html = answer(world.call("file_open", &args(&markdown)));
    assert!(html["html"].as_str().is_some_and(|html| html.contains("<h1")), "{html}");
    // A path that goes up with `..`, or through a link, is the core's business: the answer is the core's.
    let up = format!("{dir}/folder/../note.txt");
    assert_eq!(world.call("file_open", &args(&up)), same_as(world.core.file_open("local", &up)));
    #[cfg(unix)]
    {
        let link = world.dir.join("link.txt");
        std::os::unix::fs::symlink(&text, &link).expect("a link");
        let link = link.to_string_lossy().into_owned();
        assert_eq!(world.call("file_open", &args(&link)), same_as(world.core.file_open("local", &link)));
        assert_eq!(world.call("file_stat", &args(&link)), same_as(world.core.file_stat("local", &link)));
    }
}

#[test]
fn a_save_is_refused_when_the_file_changed_and_writes_when_the_digest_is_right() {
    let world = World::new("file-save");
    let path = world.dir.join("edit.txt");
    std::fs::write(&path, "first\n").expect("a file");
    let path_text = path.to_string_lossy().into_owned();
    let opened = answer(world.call("file_open", &format!(r#"{{"source":"local","path":{}}}"#, json_string(&path_text))));
    let digest = opened["digest"].as_str().expect("a digest").to_owned();
    let save = |text: &str, digest: &str| {
        format!(
            r#"{{"source":"local","path":{},"text":{},"expected_modified":null,"expected_digest":{}}}"#,
            json_string(&path_text),
            json_string(text),
            if digest.is_empty() { "null".to_owned() } else { json_string(digest) }
        )
    };

    let stale = "0".repeat(64);
    let refused = world.call("file_save", &save("second\n", &stale));
    assert_eq!(refused, same_as(world.core.file_save("local", &path_text, "second\n", None, Some(&stale))));
    assert!(error_text(&refused).contains("changed"), "{refused:?}");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "first\n", "the file is unchanged");

    let saved = answer(world.call("file_save", &save("second\n", &digest)));
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "second\n");
    assert_ne!(saved["digest"].as_str(), Some(digest.as_str()), "the answer carries the digest of what was written");
    assert_eq!(saved["digest"].as_str().map(str::len), Some(64));
    // The new digest is the one the next save is checked against.
    assert!(matches!(world.call("file_save", &save("third\n", &saved["digest"].as_str().expect("digest").to_owned())), Outcome::Ok(_)));
    assert!(matches!(world.call("file_save", &save("fourth\n", &digest)), Outcome::Err(_)), "the old digest is stale now");
    // A missing expected digest and change time overwrite, as before.
    assert!(matches!(world.call("file_save", &save("last\n", "")), Outcome::Ok(_)));
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "last\n");
}

// ---- history, the sync and the secrets of the sync -----------------------------------------------------------------------

fn type_command(world: &World, line: &str) {
    let command = holzwave_ipc::Cmd::Hist { line: line.to_owned(), cwd: "/work".to_owned(), exit: Some(0) };
    world.core.ipc_command(None, command).expect("the line is recorded");
}

#[test]
fn history_finds_a_typed_command_by_a_fuzzy_query_and_forgets_it() {
    let world = World::new("history");
    let search = |query: &str| world.call("history_search", &format!(r#"{{"query":{},"limit":10}}"#, json_string(query)));
    assert_eq!(answer(search("")), serde_json::json!([]), "a fresh history lists nothing");

    type_command(&world, "cargo build --release");
    type_command(&world, "git status");

    let all = answer(search(""));
    assert_eq!(all.as_array().map(Vec::len), Some(2));
    let found = answer(search("cgbr"));
    assert_eq!(found[0]["cmd"], "cargo build --release", "{found}");
    assert_eq!(search("cgbr"), same_ok(world.core.history_search_in("main", "cgbr", 10)));
    assert_eq!(world.call_in("nope", "history_search", r#"{"query":"","limit":10}"#), Outcome::Ok("[]".into()), "an unknown window finds nothing");

    let deleted = answer(world.call("history_delete", r#"{"cmd":"cargo build --release"}"#));
    assert!(deleted.as_u64().is_some_and(|n| n >= 1), "{deleted}");
    assert_eq!(answer(search("")).as_array().map(Vec::len), Some(1));
    assert_eq!(world.call("history_clear", "{}"), Outcome::Ok("null".into()));
    assert_eq!(answer(search("")), serde_json::json!([]));
}

#[test]
fn the_sync_starts_off_and_keeps_no_secret_when_it_cannot_reach_the_server() {
    let world = World::new("sync");
    let status = world.call("sync_status", "{}");
    assert_eq!(status, same_ok(world.core.sync_status()));
    assert_eq!(answer(status)["state"], "off");
    let (password, passphrase) = ("sync-password-1f3a9c", "sync-passphrase-77b2e4");
    let args = format!(r#"{{"endpoint":"https://127.0.0.1:9/dav","user":"alice","password":"{password}","passphrase":"{passphrase}"}}"#);

    let outcome = world.call("sync_save", &args);

    assert!(matches!(outcome, Outcome::Err(_)), "an unreachable server is no setup: {outcome:?}");
    let Outcome::Err(text) = &outcome else { unreachable!() };
    assert!(!text.contains(password) && !text.contains(passphrase), "the error holds a secret: {text}");
    assert_nothing_below_holds(&world.dir, password);
    assert_nothing_below_holds(&world.dir, passphrase);
    assert_eq!(answer(world.call("sync_status", "{}"))["configured"], false);
    assert_eq!(world.call("sync_off", "{}"), same_ok(world.core.sync_off()));
    assert_eq!(answer(world.call("sync_off", "{}"))["state"], "off");
    assert_eq!(world.call("sync_now", "{}"), same_as(world.core.sync_now()));
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
