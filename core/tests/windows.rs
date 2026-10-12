//! The window set through the public API and the files: a layout file and a session file from before windows are one
//! window, a set of one window writes the same bytes as before, and a set of several windows survives a restart.

mod common;

use std::fs;

use common::scratch_dir;
use holzwave_core::layout::{
    Axis, AttachTo, Edge, Layout, LoadStatus, MAIN_WINDOW, SetClose, WindowSet,
};
use holzwave_core::persist::write_atomic;
use holzwave_core::session::{Session, WindowState};

#[test]
fn a_layout_file_and_a_session_file_from_before_windows_are_one_window() {
    let dir = scratch_dir("windows-old-files");
    let (mut layout, _) = Layout::new();
    layout.split_focused(Axis::Row);
    layout.new_workspace("work");
    let bytes = serde_json::to_vec_pretty(&layout).unwrap();
    write_atomic(&dir.join("layout.json"), &bytes).unwrap();
    let session_text = r#"{"version": 1, "window": {"x": 10, "y": 20, "width": 900, "height": 600}, "cwds": {"3": "/tmp"}}"#;
    write_atomic(&dir.join("session.json"), session_text.as_bytes()).unwrap();

    let (set, status) = WindowSet::from_json(&fs::read_to_string(dir.join("layout.json")).unwrap());
    assert_eq!(status, LoadStatus::Clean);
    assert_eq!(set.labels(), [MAIN_WINDOW]);
    assert_eq!(set.layout(MAIN_WINDOW), Some(&layout));
    assert_eq!(set.to_json().unwrap().into_bytes(), bytes, "writing it back changes nothing, byte for byte");

    let (session, status) = Session::load(&dir);
    assert_eq!(status, LoadStatus::Clean);
    assert_eq!(session.window_for(MAIN_WINDOW), Some(WindowState { x: 10, y: 20, width: 900, height: 600, maximized: false }));
    assert!(session.windows.is_empty());
    assert_eq!(fs::read_to_string(dir.join("session.json")).unwrap(), session_text, "loading never writes");
}

#[test]
fn windows_and_their_frames_survive_a_restart_and_no_pane_is_lost_or_doubled() {
    let dir = scratch_dir("windows-restart");
    let (mut set, first) = WindowSet::new();
    let second = set.split_focused(MAIN_WINDOW, Axis::Row).unwrap();
    let third = set.new_tab(MAIN_WINDOW).unwrap();
    set.edit(MAIN_WINDOW, |l| l.rename_pane(second, "build"));
    let torn = set.tear_off_pane(second).unwrap();
    let moved = set.move_pane_to(third, &torn.label, AttachTo::Pane { target: second, edge: Edge::Bottom }).unwrap();
    assert!(!moved.source_closed);

    let mut session = Session::default();
    session.set_window_for(MAIN_WINDOW, WindowState { x: 0, y: 0, width: 1000, height: 640, maximized: false });
    session.set_window_for(&torn.label, WindowState { x: 1100, y: 50, width: 800, height: 500, maximized: false });
    write_atomic(&dir.join("layout.json"), set.to_json().unwrap().as_bytes()).unwrap();
    session.save(&dir).unwrap();

    let (back, status) = WindowSet::from_json(&fs::read_to_string(dir.join("layout.json")).unwrap());
    assert_eq!(status, LoadStatus::Clean);
    assert_eq!(back, set);
    let mut panes = back.all_panes();
    panes.sort_unstable();
    assert_eq!(panes, [first, second, third]);
    assert_eq!(back.window_of(third), Some(torn.label.as_str()));
    assert_eq!(back.layout(&torn.label).unwrap().pane_name(second), Some("build"));

    let (frames, status) = Session::load(&dir);
    assert_eq!(status, LoadStatus::Clean);
    for label in back.labels() {
        assert!(frames.window_for(label).is_some(), "a frame for {label}");
    }
}

#[test]
fn the_end_of_the_last_window_is_the_end_of_the_app_and_nothing_else_is() {
    let (mut set, first) = WindowSet::new();
    let second = set.new_tab(MAIN_WINDOW).unwrap();
    let torn = set.tear_off_pane(second).unwrap();
    assert_eq!(set.close_pane(second), SetClose::WindowClosed(torn.label));
    assert_eq!(set.close_pane(first), SetClose::LastPane);
    assert_eq!(set.all_panes(), vec![first]);
}
