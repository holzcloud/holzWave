//! Tests for `session.json`: the document, damaged files, the rules for stored working directories, window geometry
//! limits, and where a saved window is placed against the monitors that are connected now.

mod common;

use std::fs;

use common::scratch_dir;
use holzwave_core::layout::{LoadStatus, MAX_WINDOWS};
use holzwave_core::session::{
    MAX_CWD_BYTES, MAX_CWDS, MonitorRect, Placement, SESSION_FILE, Session, WindowState,
};

// Three distinct directories that exist and are absolute on the host. A stored working directory has to be
// absolute and `/tmp` is not on Windows; forward slashes keep the text valid inside a JSON string there.
const DIR_A: &str = if cfg!(windows) { "C:/Windows" } else { "/tmp" };
const DIR_B: &str = if cfg!(windows) { "C:/Users" } else { "/var" };
const DIR_C: &str = if cfg!(windows) { "C:/Program Files" } else { "/usr" };

fn window(x: i32, y: i32, width: u32, height: u32) -> WindowState {
    WindowState { x, y, width, height, maximized: false }
}

fn monitor(x: i32, y: i32, width: u32, height: u32) -> MonitorRect {
    MonitorRect { x, y, width, height }
}

fn session_with_window(w: WindowState) -> Session {
    Session { window: Some(w), ..Session::default() }
}

// ---- the document ---------------------------------------------------------------------------------------

#[test]
fn a_default_session_round_trips_and_is_pretty_printed_with_version_1() {
    let session = Session::default();
    assert_eq!(session.version, 1);
    let text = session.to_json();
    assert!(text.contains("\"version\": 1"), "pretty printed with version 1: {text:?}");
    assert!(text.ends_with('\n'));
    let (back, status) = Session::from_json(&text);
    assert_eq!(back, session);
    assert_eq!(status, LoadStatus::Clean);
}

#[test]
fn a_session_with_window_and_directories_round_trips() {
    let mut session = session_with_window(WindowState { x: -40, y: 25, width: 1280, height: 800, maximized: true });
    session.cwds.insert(1, format!("{DIR_B}/holz/Projects"));
    session.cwds.insert(7, DIR_A.into());
    let (back, status) = Session::from_json(&session.to_json());
    assert_eq!(back, session);
    assert_eq!(status, LoadStatus::Clean);
}

#[test]
fn garbage_gives_an_empty_session_and_reset_never_an_error() {
    for text in ["", "{", "[]", "null", "42", "\"session\"", "\u{0}\u{1}\u{2}", "{\"cwds\": [1, 2,"] {
        let (session, status) = Session::from_json(text);
        assert_eq!(session, Session::default(), "text: {text:?}");
        assert!(matches!(status, LoadStatus::Reset(ref why) if !why.is_empty()), "text: {text:?} gave {status:?}");
    }
}

#[test]
fn one_wrong_entry_costs_only_that_entry() {
    let text = format!(
        r#"{{
        "version": 1,
        "window": {{"x": 10, "y": 20, "width": 900, "height": 700, "maximized": false}},
        "cwds": {{"1": "{DIR_A}", "2": 5, "three": "{DIR_B}", "-4": "{DIR_B}", "5": "{DIR_C}"}},
        "future_key": {{"anything": true}}
    }}"#
    );
    let (session, status) = Session::from_json(&text);
    assert_eq!(session.window, Some(window(10, 20, 900, 700)));
    let kept: Vec<(u64, &str)> = session.cwds.iter().map(|(k, v)| (*k, v.as_str())).collect();
    assert_eq!(kept, vec![(1, DIR_A), (5, DIR_C)]);
    assert!(matches!(status, LoadStatus::Repaired(ref notes) if !notes.is_empty()), "{status:?}");
}

#[test]
fn a_broken_window_is_dropped_and_the_directories_survive() {
    let text = format!(r#"{{"version": 1, "window": {{"x": 1, "y": 2}}, "cwds": {{"3": "{DIR_A}"}}}}"#);
    let (session, status) = Session::from_json(&text);
    assert_eq!(session.window, None);
    assert_eq!(session.cwds.get(&3).map(String::as_str), Some(DIR_A));
    assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
}

#[test]
fn another_version_is_read_as_version_1_with_a_note() {
    let (session, status) = Session::from_json(&format!(r#"{{"version": 9, "cwds": {{"1": "{DIR_A}"}}}}"#));
    assert_eq!(session.version, 1);
    assert_eq!(session.cwds.len(), 1);
    assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
}

// ---- load and save --------------------------------------------------------------------------------------

#[test]
fn load_without_a_file_is_the_default_and_clean() {
    let dir = scratch_dir("session-missing");
    let (session, status) = Session::load(&dir);
    assert_eq!(session, Session::default());
    assert_eq!(status, LoadStatus::Clean);
    assert!(!dir.join(SESSION_FILE).exists(), "loading never writes");
}

#[test]
fn save_then_load_returns_the_same_session() {
    let dir = scratch_dir("session-save").join("nested");
    let mut session = session_with_window(window(100, 80, 1100, 700));
    session.cwds.insert(2, DIR_A.into());
    session.save(&dir).expect("save creates the directory and writes");
    assert!(dir.join(SESSION_FILE).is_file());
    let (back, status) = Session::load(&dir);
    assert_eq!(back, session);
    assert_eq!(status, LoadStatus::Clean);
}

#[test]
fn a_damaged_file_is_reset_and_kept_as_session_json_bad() {
    let dir = scratch_dir("session-damaged");
    fs::write(dir.join(SESSION_FILE), b"{ this is not json").expect("write");
    let (session, status) = Session::load(&dir);
    assert_eq!(session, Session::default());
    assert!(matches!(status, LoadStatus::Reset(_)), "{status:?}");
    assert_eq!(fs::read(dir.join("session.json.bad")).expect("the damaged file is kept"), b"{ this is not json");
}

#[test]
fn a_file_that_is_not_utf8_is_reset_and_kept() {
    let dir = scratch_dir("session-binary");
    fs::write(dir.join(SESSION_FILE), [0xff, 0xfe, 0x00, 0x7b]).expect("write");
    let (session, status) = Session::load(&dir);
    assert_eq!(session, Session::default());
    assert!(matches!(status, LoadStatus::Reset(_)), "{status:?}");
    assert!(dir.join("session.json.bad").is_file());
}

#[test]
fn an_oversized_file_is_reset_without_being_read_and_moved_aside() {
    let dir = scratch_dir("session-huge");
    fs::write(dir.join(SESSION_FILE), vec![b' '; 1024 * 1024 + 1]).expect("write");
    let (session, status) = Session::load(&dir);
    assert_eq!(session, Session::default());
    assert!(matches!(status, LoadStatus::Reset(_)), "{status:?}");
    assert!(dir.join("session.json.bad").is_file());
    assert!(!dir.join(SESSION_FILE).exists(), "the oversized file was moved, not copied");
}

#[test]
fn a_directory_in_the_place_of_the_file_does_not_fail_the_start() {
    let dir = scratch_dir("session-dir");
    fs::create_dir(dir.join(SESSION_FILE)).expect("mkdir");
    let (session, status) = Session::load(&dir);
    assert_eq!(session, Session::default());
    assert!(matches!(status, LoadStatus::Reset(_)), "{status:?}");
}

// ---- sanitise -------------------------------------------------------------------------------------------

#[test]
fn sanitise_keeps_only_directories_of_live_panes() {
    let mut s = Session::default();
    s.cwds.insert(1, DIR_A.into());
    s.cwds.insert(2, DIR_B.into());
    s.cwds.insert(3, DIR_C.into());
    let notes = s.sanitise(&[1, 3, 99]);
    let kept: Vec<u64> = s.cwds.keys().copied().collect();
    assert_eq!(kept, vec![1, 3]);
    assert!(!notes.is_empty(), "the removal is reported");
}

#[test]
fn sanitise_drops_entries_that_cannot_be_a_spawn_directory() {
    let mut s = Session::default();
    // A path of exactly the limit: the directory, a separator and as many letters as fit.
    let at_limit = format!("{DIR_A}/{}", "a".repeat(MAX_CWD_BYTES - DIR_A.len() - 1));
    let too_long = format!("{at_limit}a");
    assert_eq!(at_limit.len(), MAX_CWD_BYTES);
    s.cwds.insert(1, at_limit.clone());
    s.cwds.insert(2, too_long);
    s.cwds.insert(3, format!("{DIR_A}/with\nnewline"));
    s.cwds.insert(4, format!("{DIR_A}/escape\u{1b}[31m"));
    s.cwds.insert(5, format!("{DIR_A}/nul\u{0}byte"));
    s.cwds.insert(6, "relative/dir".into());
    s.cwds.insert(7, "~/Projects".into());
    s.cwds.insert(8, String::new());
    s.cwds.insert(9, DIR_A.into());
    s.sanitise(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
    let kept: Vec<u64> = s.cwds.keys().copied().collect();
    assert_eq!(kept, vec![1, 9], "only the path at the limit and the plain directory survive");
    assert_eq!(s.cwds[&1], at_limit);
}

#[test]
fn load_applies_the_content_rules_without_knowing_the_live_panes() {
    let text = format!(r#"{{"version": 1, "cwds": {{"1": "relative", "2": "{DIR_A}/a\nb", "3": "{DIR_A}"}}}}"#);
    let (session, status) = Session::from_json(&text);
    let kept: Vec<u64> = session.cwds.keys().copied().collect();
    assert_eq!(kept, vec![3]);
    assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
}

#[test]
fn a_file_with_thousands_of_directories_is_cut_to_the_limit() {
    let entries: Vec<String> = (0..2000).map(|i| format!("\"{i}\": \"{DIR_A}\"")).collect();
    let text = format!("{{\"version\": 1, \"cwds\": {{{}}}}}", entries.join(","));
    let (session, status) = Session::from_json(&text);
    assert_eq!(session.cwds.len(), MAX_CWDS);
    assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
}

#[test]
fn set_cwd_applies_the_same_content_rules() {
    let mut s = Session::default();
    assert!(s.set_cwd(1, DIR_A));
    assert!(!s.set_cwd(2, "relative"));
    assert!(!s.set_cwd(3, &format!("{DIR_A}/\u{7}")));
    assert!(!s.set_cwd(4, ""));
    assert_eq!(s.cwds.keys().copied().collect::<Vec<_>>(), vec![1]);
}

// ---- cwd_for --------------------------------------------------------------------------------------------

#[test]
fn cwd_for_returns_an_existing_absolute_directory() {
    let dir = scratch_dir("session-cwd");
    let mut s = Session::default();
    s.cwds.insert(1, dir.to_str().expect("utf-8 temp dir").to_string());
    assert_eq!(s.cwd_for(1), Some(dir));
}

#[test]
fn cwd_for_is_none_for_a_missing_directory_a_file_and_an_unknown_pane() {
    let dir = scratch_dir("session-cwd-none");
    let file = dir.join("a-file");
    fs::write(&file, b"x").expect("write");
    let gone = dir.join("gone");
    let mut s = Session::default();
    s.cwds.insert(1, gone.to_str().expect("utf-8").to_string());
    s.cwds.insert(2, file.to_str().expect("utf-8").to_string());
    assert_eq!(s.cwd_for(1), None, "a vanished directory");
    assert_eq!(s.cwd_for(2), None, "a file is not a directory");
    assert_eq!(s.cwd_for(3), None, "no entry");
}

#[test]
fn cwd_for_checks_the_content_rules_even_when_the_field_was_set_by_hand() {
    let mut s = Session::default();
    // The map is public; a value that never went through sanitise must still not be handed to a spawn.
    s.cwds.insert(1, "relative".into());
    s.cwds.insert(2, format!("{DIR_A}\n"));
    s.cwds.insert(3, format!("{DIR_A}/\u{1b}"));
    assert_eq!(s.cwd_for(1), None);
    assert_eq!(s.cwd_for(2), None);
    assert_eq!(s.cwd_for(3), None);
}

// ---- geometry limits ------------------------------------------------------------------------------------

#[test]
fn size_and_position_are_clamped_when_loaded() {
    let text = r#"{"version": 1, "window": {"x": 1000000000, "y": -1000000000, "width": 5, "height": 99999999999, "maximized": false}}"#;
    let (session, status) = Session::from_json(text);
    assert_eq!(session.window, Some(window(32768, -32768, 200, 16384)));
    assert!(matches!(status, LoadStatus::Repaired(ref notes) if !notes.is_empty()), "{status:?}");
}

#[test]
fn values_on_the_boundary_are_kept_exactly() {
    let w = window(-32768, 32768, 200, 16384);
    let (session, status) = Session::from_json(&session_with_window(w).to_json());
    assert_eq!(session.window, Some(w));
    assert_eq!(status, LoadStatus::Clean);
}

#[test]
fn set_window_clamps_what_the_shell_reports() {
    let mut s = Session::default();
    s.set_window(window(70000, -70000, 10, 70000));
    assert_eq!(s.window, Some(window(32768, -32768, 200, 16384)));
}

// ---- placement ------------------------------------------------------------------------------------------

#[test]
fn a_window_inside_a_monitor_keeps_its_position_and_size() {
    let s = session_with_window(window(100, 80, 1000, 700));
    let p = s.placement(&[monitor(0, 0, 1920, 1080)]);
    assert_eq!(p, Placement { size: (1000, 700), position: Some((100, 80)), maximized: false });
}

#[test]
fn a_window_on_a_monitor_that_is_gone_loses_its_position_and_keeps_its_size() {
    // It was on a second monitor at x 1920..3840; only the first is connected now.
    let s = session_with_window(window(2200, 100, 900, 600));
    let p = s.placement(&[monitor(0, 0, 1920, 1080)]);
    assert_eq!(p, Placement { size: (900, 600), position: None, maximized: false });
}

#[test]
fn the_visible_part_must_be_at_least_100_by_50_on_one_monitor() {
    let m = [monitor(0, 0, 1920, 1080)];
    // Exactly 100 x 50 visible at the bottom right corner: kept.
    let at_edge = session_with_window(window(1820, 1030, 800, 600)).placement(&m);
    assert_eq!(at_edge.position, Some((1820, 1030)));
    // One pixel less in either direction: dropped.
    let narrow = session_with_window(window(1821, 1030, 800, 600)).placement(&m);
    assert_eq!(narrow.position, None);
    let short = session_with_window(window(1820, 1031, 800, 600)).placement(&m);
    assert_eq!(short.position, None);
    // Hanging off the top left: kept while 100 x 50 is still visible.
    let top_left = session_with_window(window(-700, -550, 800, 600)).placement(&m);
    assert_eq!(top_left.position, Some((-700, -550)));
}

#[test]
fn the_visible_parts_of_two_monitors_do_not_add_up() {
    // 60 px on the left monitor and 40 px on the right one, with a gap of 100 px between them: together that
    // is 100 px, but neither monitor reaches it.
    let m = [monitor(0, 0, 1000, 1000), monitor(1100, 0, 1000, 1000)];
    let p = session_with_window(window(940, 100, 200, 600)).placement(&m);
    assert_eq!(p.position, None);
}

#[test]
fn a_window_on_a_second_monitor_with_negative_coordinates_is_kept() {
    let m = [monitor(0, 0, 1920, 1080), monitor(-1920, -200, 1920, 1080)];
    let p = session_with_window(window(-1500, 0, 1000, 700)).placement(&m);
    assert_eq!(p.position, Some((-1500, 0)));
}

#[test]
fn without_any_monitor_the_position_is_dropped() {
    let p = session_with_window(window(100, 80, 1000, 700)).placement(&[]);
    assert_eq!(p, Placement { size: (1000, 700), position: None, maximized: false });
}

#[test]
fn maximized_is_carried_over() {
    let mut s = session_with_window(window(0, 0, 1400, 900));
    s.window.as_mut().expect("window").maximized = true;
    let p = s.placement(&[monitor(0, 0, 1920, 1080)]);
    assert!(p.maximized);
    assert_eq!(p.position, Some((0, 0)));
}

#[test]
fn nothing_saved_gives_the_default_size_and_no_position() {
    let p = Session::default().placement(&[monitor(0, 0, 1920, 1080)]);
    assert_eq!(p, Placement { size: (1000, 640), position: None, maximized: false });
}

#[test]
fn placement_clamps_a_size_that_was_set_by_hand() {
    // The fields are public; placement must not trust them.
    let s = session_with_window(window(0, 0, 4, 4_000_000_000));
    let p = s.placement(&[monitor(0, 0, 1920, 1080)]);
    assert_eq!(p.size, (200, 16384));
}

// ---- several windows ------------------------------------------------------------------------------------

#[test]
fn a_session_without_other_windows_is_written_exactly_as_before_windows_existed() {
    let mut session = session_with_window(window(10, 20, 1000, 640));
    session.cwds.insert(4, DIR_A.into());
    let text = session.to_json();
    assert!(text.contains("\"version\": 1"), "{text}");
    assert!(!text.contains("windows"), "no table for no other windows: {text}");
    let (back, status) = Session::from_json(&text);
    assert_eq!((back, status), (session, LoadStatus::Clean));
}

#[test]
fn a_version_1_file_loads_as_one_window_without_a_note() {
    let text = format!(
        r#"{{"version": 1, "window": {{"x": 10, "y": 20, "width": 1100, "height": 700, "maximized": true}}, "cwds": {{"3": "{DIR_A}"}}}}"#
    );
    let (session, status) = Session::from_json(&text);
    assert_eq!(status, LoadStatus::Clean);
    assert_eq!(session.version, 1);
    assert_eq!(session.window_for("main"), Some(WindowState { x: 10, y: 20, width: 1100, height: 700, maximized: true }));
    assert!(session.windows.is_empty());
    assert_eq!(session.window_for("window-5"), None);
}

#[test]
fn the_frames_of_other_windows_make_a_version_2_file_that_round_trips() {
    let mut session = session_with_window(window(0, 0, 1000, 640));
    assert!(session.set_window_for("window-7", window(1100, 40, 800, 600)));
    assert!(session.set_window_for("window-12", WindowState { maximized: true, ..window(5, 6, 700, 500) }));
    assert_eq!(session.version, 2);
    let text = session.to_json();
    assert!(text.contains("\"version\": 2") && text.contains("\"window-7\""), "{text}");
    let (back, status) = Session::from_json(&text);
    assert_eq!(status, LoadStatus::Clean);
    assert_eq!(back, session);
    assert_eq!(back.window_for("window-7"), Some(window(1100, 40, 800, 600)));
    assert_eq!(back.window_for("main"), Some(window(0, 0, 1000, 640)));
}

#[test]
fn a_frame_is_set_per_label_and_a_bad_label_or_a_full_table_changes_nothing() {
    let mut session = Session::default();
    assert!(session.set_window_for("main", window(1, 2, 800, 600)));
    assert_eq!(session.window, Some(window(1, 2, 800, 600)), "main is the window entry");
    assert!(session.windows.is_empty());
    assert_eq!(session.version, 1);
    for bad in ["", "window-", "window-*", "Window-1", "other", "window-1/../x"] {
        assert!(!session.set_window_for(bad, window(0, 0, 800, 600)), "{bad:?}");
    }
    assert!(session.windows.is_empty());
    for n in 1..MAX_WINDOWS {
        assert!(session.set_window_for(&format!("window-{n}"), window(0, 0, 800, 600)));
    }
    let before = session.clone();
    assert!(!session.set_window_for("window-99", window(0, 0, 800, 600)), "one frame too many");
    assert_eq!(session, before);
    assert!(session.set_window_for("window-1", window(9, 9, 900, 900)), "a known label can always be updated");
}

#[test]
fn a_frame_is_clamped_like_the_main_one() {
    let mut session = Session::default();
    session.set_window_for("window-3", window(1_000_000, -1_000_000, 5, 99_999_999));
    let frame = session.window_for("window-3").unwrap();
    assert_eq!((frame.x, frame.y, frame.width, frame.height), (32768, -32768, 200, 16384));
}

#[test]
fn placement_for_uses_the_frame_of_that_window_and_the_default_for_an_unknown_one() {
    let screen = [monitor(0, 0, 1920, 1080)];
    let mut session = session_with_window(window(10, 10, 800, 600));
    session.set_window_for("window-4", window(900, 100, 700, 500));
    assert_eq!(session.placement_for("main", &screen), session.placement(&screen));
    assert_eq!(
        session.placement_for("window-4", &screen),
        Placement { size: (700, 500), position: Some((900, 100)), maximized: false }
    );
    assert_eq!(
        session.placement_for("window-5", &screen),
        Placement { size: holzwave_core::session::DEFAULT_SIZE, position: None, maximized: false }
    );
    // An off-screen window keeps its size and loses its position, as the main one does.
    session.set_window_for("window-6", window(9000, 100, 700, 500));
    assert_eq!(session.placement_for("window-6", &screen).position, None);
    assert_eq!(session.placement_for("window-6", &screen).size, (700, 500));
}

#[test]
fn the_frame_of_a_window_that_is_gone_is_forgotten_and_the_version_goes_back_to_1() {
    let mut session = session_with_window(window(0, 0, 800, 600));
    session.set_window_for("window-1", window(0, 0, 800, 600));
    session.set_window_for("window-2", window(0, 0, 800, 600));
    let notes = session.sanitise_windows(&["main", "window-2"]);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(session.windows.contains_key("window-2") && !session.windows.contains_key("window-1"));
    assert_eq!(session.version, 2);
    session.forget_window("window-2");
    assert_eq!(session.version, 1);
    assert!(session.sanitise_windows(&["main"]).is_empty());
    assert_eq!(session.window, Some(window(0, 0, 800, 600)), "the main frame stays");
}

#[test]
fn bad_entries_of_the_windows_table_are_dropped_one_by_one_with_a_note() {
    let text = r#"{"version": 2, "window": {"x": 1, "y": 2, "width": 800, "height": 600},
        "windows": {
          "window-1": {"x": 5, "y": 6, "width": 700, "height": 500},
          "window-2": {"x": "left", "y": 6, "width": 700, "height": 500},
          "main": {"x": 5, "y": 6, "width": 123, "height": 456},
          "window-*": {"x": 5, "y": 6, "width": 700, "height": 500},
          "nope": {"x": 5, "y": 6, "width": 700, "height": 500},
          "window-3": 7
        }}"#;
    let (session, status) = Session::from_json(text);
    assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
    assert_eq!(session.windows.keys().map(String::as_str).collect::<Vec<_>>(), ["window-1"]);
    assert_eq!(session.window, Some(window(1, 2, 800, 600)), "a `main` in the table does not replace the window entry");
    assert_eq!(session.version, 2);
}

#[test]
fn a_windows_table_that_is_not_an_object_is_dropped_and_the_rest_is_kept() {
    for table in ["[]", "7", "\"x\"", "true"] {
        let text = format!(r#"{{"version": 2, "window": {{"x": 1, "y": 2, "width": 800, "height": 600}}, "windows": {table}}}"#);
        let (session, status) = Session::from_json(&text);
        assert!(matches!(status, LoadStatus::Repaired(_)), "{table}: {status:?}");
        assert_eq!(session.window, Some(window(1, 2, 800, 600)));
        assert!(session.windows.is_empty());
        assert_eq!(session.version, 1, "nothing left of the table, so a session of one window");
    }
}

#[test]
fn a_windows_table_beyond_the_limit_is_cut() {
    let entries: Vec<String> =
        (1..=40).map(|n| format!(r#""window-{n}": {{"x": 0, "y": 0, "width": 800, "height": 600}}"#)).collect();
    let (session, status) = Session::from_json(&format!(r#"{{"version": 2, "windows": {{{}}}}}"#, entries.join(",")));
    assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
    assert_eq!(session.windows.len(), MAX_WINDOWS - 1);
}

#[test]
fn a_frame_in_the_table_is_clamped_with_a_note() {
    let text = r#"{"version": 2, "windows": {"window-1": {"x": 99999999, "y": 0, "width": 1, "height": 800}}}"#;
    let (session, status) = Session::from_json(text);
    assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
    let frame = session.window_for("window-1").unwrap();
    assert_eq!((frame.x, frame.width), (32768, 200));
}

#[test]
fn an_unknown_version_with_a_table_is_read_as_version_2_with_a_note() {
    let text = r#"{"version": 9, "windows": {"window-1": {"x": 0, "y": 0, "width": 800, "height": 600}}}"#;
    let (session, status) = Session::from_json(text);
    assert_eq!(session.version, 2);
    assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
    assert_eq!(session.windows.len(), 1);
}

#[test]
fn a_session_with_windows_survives_save_and_load_and_damaged_copies_never_panic() {
    let dir = scratch_dir("session-windows");
    let mut session = session_with_window(window(0, 0, 1000, 640));
    session.set_window_for("window-8", window(50, 60, 900, 700));
    session.cwds.insert(2, DIR_A.into());
    session.save(&dir).unwrap();
    let (back, status) = Session::load(&dir);
    assert_eq!((back, status), (session.clone(), LoadStatus::Clean));

    let text = session.to_json();
    for end in (0..text.len()).filter(|e| text.is_char_boundary(*e)) {
        let (back, _) = Session::from_json(&text[..end]);
        assert!(back.windows.len() < MAX_WINDOWS);
    }
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        for bad in ['}', '-', '"', '9', '[', 'x'] {
            let mut copy = chars.clone();
            copy[i] = bad;
            let (back, _) = Session::from_json(&copy.iter().collect::<String>());
            assert!(back.windows.keys().all(|l| holzwave_core::layout::valid_window_label(l)));
        }
    }
}
