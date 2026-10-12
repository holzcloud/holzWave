//! The parts of several windows that need neither Tauri nor the core's state, so that they are tested without a
//! window: which window is under the pointer, which windows were focused last, the one drop that is on its way from
//! one window to another, where a new window opens, and what the shell remembers of each window's geometry.
//!
//! Units. Everything here is in logical points (CSS pixels of a page at zoom 1 on macOS). The caller converts what
//! the window system reports: on macOS the pointer position is relative to the primary monitor and in its scale,
//! while the frame of a window is in the scale of the monitor the window is on, so each is divided by its own scale
//! factor before it gets here (research section 7, "units trap").

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use holzwave_core::layout::PaneId;
use serde::{Deserialize, Serialize};

/// The geometry a window had while it was neither maximised nor full screen: x, y, width, height in pixels.
pub type Normal = (i32, i32, u32, u32);

/// What is being dragged out of a window: one pane, or the whole tab the pane is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropKind {
    Pane,
    Tab,
}

/// Where a window that a tear-off creates opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// Under the pointer (a pane or tab released over nothing).
    Cursor,
    /// Beside the window it came from, offset by [`BESIDE_OFFSET`] (the palette action, no pointer involved).
    Beside(String),
}

/// What is under the pointer when a pane or tab is released outside its window.
#[derive(Clone, Debug, PartialEq)]
pub enum CursorSpot {
    /// Another window, with the pointer in that window's client coordinates (CSS pixels).
    Window { label: String, x: f64, y: f64 },
    /// No other window of the app.
    Nothing,
    /// The pointer cannot be read (the platform refused, or there is no pointer); nothing is done.
    Unknown,
}

/// How far a window opened beside another is moved right and down, in points.
pub const BESIDE_OFFSET: f64 = 32.0;

/// Where, relative to the pointer, the top left corner of a window that opens under it is: the pointer is then about
/// on the title bar, near the left, as when a tab is carried.
pub const CURSOR_GRAB: (f64, f64) = (60.0, 16.0);

/// The top left corner of a window that opens under the pointer.
pub fn position_under_cursor(cursor: (f64, f64)) -> (f64, f64) {
    (cursor.0 - CURSOR_GRAB.0, cursor.1 - CURSOR_GRAB.1)
}

/// The top left corner of a window that opens beside the window at `origin`.
pub fn position_beside(origin: (f64, f64)) -> (f64, f64) {
    (origin.0 + BESIDE_OFFSET, origin.1 + BESIDE_OFFSET)
}

// ---- which window is under the pointer --------------------------------------------------------------------------

/// The frame of one window, in logical points.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub label: String,
    /// The outer rectangle: the web view fills the window (overlay title bar), so this is the area that counts.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// The top left corner of the page (the inner area), the origin of client coordinates.
    pub inner_x: f64,
    pub inner_y: f64,
}

impl Frame {
    fn contains(&self, point: (f64, f64)) -> bool {
        point.0 >= self.x && point.0 < self.x + self.width && point.1 >= self.y && point.1 < self.y + self.height
    }

    /// `point` in the client coordinates of the page of this window.
    pub fn client(&self, point: (f64, f64)) -> (f64, f64) {
        (point.0 - self.inner_x, point.1 - self.inner_y)
    }
}

/// The window under `cursor`, never `exclude` (the window the gesture started in: the page decided the pointer is
/// outside of it). There is no z order to ask the window system for, so of several windows that overlap at that
/// point the one that was focused most recently wins (`mru` lists labels, most recent first); a window that was
/// never focused loses against every one that was, and ties go to the earlier frame.
pub fn window_under<'a>(cursor: (f64, f64), frames: &'a [Frame], mru: &[String], exclude: &str) -> Option<&'a Frame> {
    frames
        .iter()
        .filter(|frame| frame.label != exclude && frame.contains(cursor))
        .min_by_key(|frame| mru.iter().position(|label| *label == frame.label).unwrap_or(usize::MAX))
}

/// The windows in the order they were focused, most recent first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mru(Vec<String>);

impl Mru {
    /// `label` was focused.
    pub fn touch(&mut self, label: &str) {
        self.0.retain(|l| l != label);
        self.0.insert(0, label.to_owned());
    }

    /// The window is gone.
    pub fn forget(&mut self, label: &str) {
        self.0.retain(|l| l != label);
    }

    pub fn order(&self) -> &[String] {
        &self.0
    }
}

/// What the shell remembers per window between two window events: the last normal geometry and the focus order.
#[derive(Default)]
pub struct WindowTracker {
    normals: Mutex<HashMap<String, Normal>>,
    mru: Mutex<Mru>,
}

impl WindowTracker {
    pub fn normal(&self, label: &str) -> Option<Normal> {
        self.normals.lock().unwrap_or_else(PoisonError::into_inner).get(label).copied()
    }

    pub fn set_normal(&self, label: &str, normal: Option<Normal>) {
        let mut normals = self.normals.lock().unwrap_or_else(PoisonError::into_inner);
        match normal {
            Some(normal) => {
                normals.insert(label.to_owned(), normal);
            }
            None => {
                normals.remove(label);
            }
        }
    }

    pub fn focused(&self, label: &str) {
        self.mru.lock().unwrap_or_else(PoisonError::into_inner).touch(label);
    }

    pub fn order(&self) -> Vec<String> {
        self.mru.lock().unwrap_or_else(PoisonError::into_inner).order().to_vec()
    }

    /// The window is gone: nothing is kept of it.
    pub fn forget(&self, label: &str) {
        self.normals.lock().unwrap_or_else(PoisonError::into_inner).remove(label);
        self.mru.lock().unwrap_or_else(PoisonError::into_inner).forget(label);
    }
}

// ---- the drop that is on its way -----------------------------------------------------------------------------------

/// How long a drop that was handed to another window waits for that window's answer.
pub const DROP_TTL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
struct InFlight {
    kind: DropKind,
    pane: PaneId,
    from: String,
    at: Instant,
}

/// The one drop that is between two windows: the window where the gesture started released a pane (or tab) over
/// another window, and that window has to answer with where in it the pane goes. Only a drop that was handed over,
/// for that very pane and kind, and not too long ago, can be answered; a stale or repeated answer finds nothing.
#[derive(Debug, Default)]
pub struct Drops(Option<InFlight>);

impl Drops {
    /// A drop of `pane` from the window `from` is handed over now. It replaces any earlier one.
    pub fn begin(&mut self, kind: DropKind, pane: PaneId, from: &str, now: Instant) {
        self.0 = Some(InFlight { kind, pane, from: from.to_owned(), at: now });
    }

    /// The window the drop of `pane` came from, if that drop is in flight and not expired at `now`. An expired drop
    /// is forgotten. Does not consume it: [`Drops::finish`] does, after the move worked.
    pub fn source(&mut self, kind: DropKind, pane: PaneId, now: Instant) -> Option<String> {
        let flight = self.0.as_ref()?;
        if now.saturating_duration_since(flight.at) > DROP_TTL {
            self.0 = None;
            return None;
        }
        (flight.kind == kind && flight.pane == pane).then(|| flight.from.clone())
    }

    /// The drop of `pane` is done (or given up).
    pub fn finish(&mut self, kind: DropKind, pane: PaneId) {
        if self.0.as_ref().is_some_and(|f| f.kind == kind && f.pane == pane) {
            self.0 = None;
        }
    }

    /// Whether anything is in flight, for tests.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(label: &str, x: f64, y: f64, width: f64, height: f64) -> Frame {
        Frame { label: label.into(), x, y, width, height, inner_x: x, inner_y: y + 28.0 }
    }

    fn labels(list: &[&str]) -> Vec<String> {
        list.iter().map(|l| (*l).to_owned()).collect()
    }

    #[test]
    fn the_window_under_the_pointer_is_found_and_the_source_never_is() {
        let frames = [frame("main", 0.0, 0.0, 800.0, 600.0), frame("window-5", 900.0, 100.0, 500.0, 400.0)];

        let hit = window_under((1000.0, 200.0), &frames, &[], "main");
        assert_eq!(hit.map(|f| f.label.as_str()), Some("window-5"));
        assert_eq!(window_under((1000.0, 200.0), &frames, &[], "window-5"), None, "the source is excluded");
        assert_eq!(window_under((850.0, 200.0), &frames, &[], "main"), None, "between the windows");
        assert_eq!(window_under((100.0, 100.0), &frames, &[], "window-5").map(|f| f.label.as_str()), Some("main"));
    }

    #[test]
    fn the_edges_belong_to_the_window_on_the_left_and_top_only() {
        let frames = [frame("a", 100.0, 100.0, 200.0, 200.0)];
        assert!(window_under((100.0, 100.0), &frames, &[], "x").is_some(), "top left corner is inside");
        assert!(window_under((299.9, 299.9), &frames, &[], "x").is_some());
        assert!(window_under((300.0, 150.0), &frames, &[], "x").is_none(), "the right edge is outside");
        assert!(window_under((150.0, 300.0), &frames, &[], "x").is_none(), "the bottom edge is outside");
        assert!(window_under((99.9, 150.0), &frames, &[], "x").is_none());
    }

    #[test]
    fn of_overlapping_windows_the_one_focused_last_wins() {
        let frames = [frame("main", 0.0, 0.0, 800.0, 600.0), frame("window-5", 100.0, 100.0, 800.0, 600.0)];
        let point = (300.0, 300.0);

        let by = |mru: &[&str]| window_under(point, &frames, &labels(mru), "window-9").map(|f| f.label.clone());

        assert_eq!(by(&["window-5", "main"]).as_deref(), Some("window-5"));
        assert_eq!(by(&["main", "window-5"]).as_deref(), Some("main"));
        assert_eq!(by(&["window-5"]).as_deref(), Some("window-5"), "a window that was never focused loses");
        assert_eq!(by(&[]).as_deref(), Some("main"), "with no focus history the first frame wins");
    }

    #[test]
    fn client_coordinates_are_relative_to_the_page_not_the_outer_frame() {
        let f = frame("window-5", 900.0, 100.0, 500.0, 400.0);
        // inner_y is 28 below the outer top (the title bar of the sample); the pointer 50 below the outer top is
        // 22 below the top of the page.
        assert_eq!(f.client((950.0, 150.0)), (50.0, 22.0));
    }

    #[test]
    fn the_focus_order_moves_a_window_to_the_front_and_forgets_it() {
        let mut mru = Mru::default();
        mru.touch("main");
        mru.touch("window-5");
        mru.touch("main");
        assert_eq!(mru.order(), ["main", "window-5"]);
        mru.forget("main");
        assert_eq!(mru.order(), ["window-5"]);
        mru.forget("nobody");
        assert_eq!(mru.order(), ["window-5"]);
    }

    #[test]
    fn the_tracker_keeps_one_geometry_and_one_focus_history_per_window() {
        let tracker = WindowTracker::default();
        tracker.set_normal("main", Some((1, 2, 3, 4)));
        tracker.set_normal("window-5", Some((5, 6, 7, 8)));
        tracker.focused("main");
        tracker.focused("window-5");
        assert_eq!(tracker.normal("main"), Some((1, 2, 3, 4)));
        assert_eq!(tracker.normal("window-5"), Some((5, 6, 7, 8)));
        assert_eq!(tracker.order(), ["window-5", "main"]);

        tracker.forget("window-5");

        assert_eq!(tracker.normal("window-5"), None);
        assert_eq!(tracker.normal("main"), Some((1, 2, 3, 4)), "the other window is untouched");
        assert_eq!(tracker.order(), ["main"]);
    }

    #[test]
    fn a_new_window_opens_under_the_pointer_or_beside_its_source() {
        assert_eq!(position_under_cursor((500.0, 400.0)), (440.0, 384.0));
        assert_eq!(position_beside((100.0, 80.0)), (132.0, 112.0));
    }

    #[test]
    fn a_drop_in_flight_is_answered_once_for_its_pane_and_kind() {
        let t0 = Instant::now();
        let mut drops = Drops::default();
        assert_eq!(drops.source(DropKind::Pane, 7, t0), None, "nothing is in flight");

        drops.begin(DropKind::Pane, 7, "main", t0);

        assert_eq!(drops.source(DropKind::Pane, 7, t0).as_deref(), Some("main"));
        assert_eq!(drops.source(DropKind::Tab, 7, t0), None, "another kind is not this drop");
        assert_eq!(drops.source(DropKind::Pane, 8, t0), None, "another pane is not this drop");
        assert!(!drops.is_empty(), "a refused question does not consume the drop");
        drops.finish(DropKind::Pane, 8);
        assert!(!drops.is_empty(), "finishing another pane keeps it");
        drops.finish(DropKind::Pane, 7);
        assert!(drops.is_empty());
        assert_eq!(drops.source(DropKind::Pane, 7, t0), None, "a repeated answer finds nothing");
    }

    #[test]
    fn a_drop_in_flight_expires_and_a_newer_one_replaces_it() {
        let t0 = Instant::now();
        let mut drops = Drops::default();
        drops.begin(DropKind::Pane, 7, "main", t0);
        assert_eq!(drops.source(DropKind::Pane, 7, t0 + DROP_TTL).as_deref(), Some("main"), "up to the limit");
        assert_eq!(drops.source(DropKind::Pane, 7, t0 + DROP_TTL + Duration::from_millis(1)), None, "past it");
        assert!(drops.is_empty(), "an expired drop is forgotten");

        drops.begin(DropKind::Pane, 7, "main", t0);
        drops.begin(DropKind::Tab, 9, "window-3", t0);
        assert_eq!(drops.source(DropKind::Pane, 7, t0), None, "the older drop was replaced");
        assert_eq!(drops.source(DropKind::Tab, 9, t0).as_deref(), Some("window-3"));
    }
}
