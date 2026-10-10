//! Tests for `session.json`: the document, damaged files, the rules for stored working directories, window geometry
//! limits, and where a saved window is placed against the monitors that are connected now.

mod common;

use std::fs;

use common::scratch_dir;
use holzwave_core::layout::LoadStatus;
use holzwave_core::session::{
    MAX_CWD_BYTES, MAX_CWDS, MonitorRect, Placement, SESSION_FILE, Session, WindowState,
};

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
    session.cwds.insert(1, "/Users/holz/Projects".into());
    session.cwds.insert(7, "/tmp".into());
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
    let text = r#"{
        "version": 1,
        "window": {"x": 10, "y": 20, "width": 900, "height": 700, "maximized": false},
        "cwds": {"1": "/tmp", "2": 5, "three": "/var", "-4": "/etc", "5": "/usr"},
        "future_key": {"anything": true}
    }"#;
    let (session, status) = Session::from_json(text);
    assert_eq!(session.window, Some(window(10, 20, 900, 700)));
    let kept: Vec<(u64, &str)> = session.cwds.iter().map(|(k, v)| (*k, v.as_str())).collect();
    assert_eq!(kept, vec![(1, "/tmp"), (5, "/usr")]);
    assert!(matches!(status, LoadStatus::Repaired(ref notes) if !notes.is_empty()), "{status:?}");
}

#[test]
fn a_broken_window_is_dropped_and_the_directories_survive() {
    let text = r#"{"version": 1, "window": {"x": 1, "y": 2}, "cwds": {"3": "/tmp"}}"#;
    let (session, status) = Session::from_json(text);
    assert_eq!(session.window, None);
    assert_eq!(session.cwds.get(&3).map(String::as_str), Some("/tmp"));
    assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
}

#[test]
fn another_version_is_read_as_version_1_with_a_note() {
    let (session, status) = Session::from_json(r#"{"version": 9, "cwds": {"1": "/tmp"}}"#);
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
    session.cwds.insert(2, "/tmp".into());
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
    s.cwds.insert(1, "/tmp".into());
    s.cwds.insert(2, "/var".into());
    s.cwds.insert(3, "/usr".into());
    let notes = s.sanitise(&[1, 3, 99]);
    let kept: Vec<u64> = s.cwds.keys().copied().collect();
    assert_eq!(kept, vec![1, 3]);
    assert!(!notes.is_empty(), "the removal is reported");
}

#[test]
fn sanitise_drops_entries_that_cannot_be_a_spawn_directory() {
    let mut s = Session::default();
    let at_limit = format!("/{}", "a".repeat(MAX_CWD_BYTES - 1));
    let too_long = format!("/{}", "a".repeat(MAX_CWD_BYTES));
    assert_eq!(at_limit.len(), MAX_CWD_BYTES);
    s.cwds.insert(1, at_limit.clone());
    s.cwds.insert(2, too_long);
    s.cwds.insert(3, "/tmp/with\nnewline".into());
    s.cwds.insert(4, "/tmp/escape\u{1b}[31m".into());
    s.cwds.insert(5, "/tmp/nul\u{0}byte".into());
    s.cwds.insert(6, "relative/dir".into());
    s.cwds.insert(7, "~/Projects".into());
    s.cwds.insert(8, String::new());
    s.cwds.insert(9, "/tmp".into());
    s.sanitise(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
    let kept: Vec<u64> = s.cwds.keys().copied().collect();
    assert_eq!(kept, vec![1, 9], "only the path at the limit and /tmp survive");
    assert_eq!(s.cwds[&1], at_limit);
}

#[test]
fn load_applies_the_content_rules_without_knowing_the_live_panes() {
    let text = r#"{"version": 1, "cwds": {"1": "relative", "2": "/tmp/a\nb", "3": "/tmp"}}"#;
    let (session, status) = Session::from_json(text);
    let kept: Vec<u64> = session.cwds.keys().copied().collect();
    assert_eq!(kept, vec![3]);
    assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
}

#[test]
fn a_file_with_thousands_of_directories_is_cut_to_the_limit() {
    let entries: Vec<String> = (0..2000).map(|i| format!("\"{i}\": \"/tmp\"")).collect();
    let text = format!("{{\"version\": 1, \"cwds\": {{{}}}}}", entries.join(","));
    let (session, status) = Session::from_json(&text);
    assert_eq!(session.cwds.len(), MAX_CWDS);
    assert!(matches!(status, LoadStatus::Repaired(_)), "{status:?}");
}

#[test]
fn set_cwd_applies_the_same_content_rules() {
    let mut s = Session::default();
    assert!(s.set_cwd(1, "/tmp"));
    assert!(!s.set_cwd(2, "relative"));
    assert!(!s.set_cwd(3, "/tmp/\u{7}"));
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
    s.cwds.insert(2, "/tmp\n".into());
    s.cwds.insert(3, "/tmp/\u{1b}".into());
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
