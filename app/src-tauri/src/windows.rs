//! The parts of several windows that need neither Tauri nor the core's state, so that they are tested without a
//! window: which window is under the pointer, which windows were focused last, the one drop that is on its way from
//! one window to another, where a new window opens, and what the shell remembers of each window's geometry.
//!
//! Units. There is one space for hit-testing and placing, and which it is depends on the platform (research phase
//! 2.2, S8):
//!
//! - macOS: logical points. The pointer position is relative to the primary monitor and in its scale, while the
//!   frame of a window is in the scale of the monitor the window is on, so the caller divides each by its own scale
//!   factor before it gets here (research section 7, "units trap"). A point is a CSS pixel at zoom 1.
//! - Windows and Linux: physical pixels. The pointer and the frames of all windows already share one desktop space,
//!   whatever scale each monitor has, so nothing is divided and the numbers compare as they are.
//!
//! The page's CSS pixels come only after [`Frame::client`]: it divides the offset from the page's origin by the number
//! of units per CSS pixel of that window ([`Frame::scale`], 1 on macOS, the window's scale factor elsewhere). At 150 %
//! one CSS pixel is 1.5 physical ones, so a pointer 150 physical pixels right of the page origin is at x = 100. The
//! same factor, [`units_per_point`], turns the offsets of a new window (given in points) into the space of the
//! pointer.
//!
//! Under Wayland the window system tells an application neither where the pointer nor where a window is
//! ([`pointer_is_global`]), so none of the numbers above exist there and nothing is placed or attached by position.

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
    /// The pointer cannot be read (the platform refused, there is no pointer, or the window system does not say where
    /// it is, as under Wayland); nothing is done.
    Unknown,
}

/// How far a window opened beside another is moved right and down, in points.
pub const BESIDE_OFFSET: f64 = 32.0;

/// Where, relative to the pointer, the top left corner of a window that opens under it is: the pointer is then about
/// on the title bar, near the left, as when a tab is carried.
pub const CURSOR_GRAB: (f64, f64) = (60.0, 16.0);

/// The top left corner of a window that opens under the pointer, in the units of `cursor`. [`CURSOR_GRAB`] is in
/// points, so it is multiplied by `units_per_point` (see [`units_per_point`]).
pub fn position_under_cursor(cursor: (f64, f64), units_per_point: f64) -> (f64, f64) {
    (cursor.0 - CURSOR_GRAB.0 * units_per_point, cursor.1 - CURSOR_GRAB.1 * units_per_point)
}

/// The top left corner of a window that opens beside the window at `origin`, in the units of `origin`.
/// [`BESIDE_OFFSET`] is in points, so it is multiplied by `units_per_point`.
pub fn position_beside(origin: (f64, f64), units_per_point: f64) -> (f64, f64) {
    (origin.0 + BESIDE_OFFSET * units_per_point, origin.1 + BESIDE_OFFSET * units_per_point)
}

/// A scale factor as the window system reports it, or 1 when it is not a usable number.
fn usable(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 { scale } else { 1.0 }
}

/// How many units of the shared space (see the module doc) one point, that is one CSS pixel, is on a window whose
/// display scale is `scale`: macOS compares in points, so 1; elsewhere in physical pixels, so the scale itself.
pub fn units_per_point(os: &str, scale: f64) -> f64 {
    if os == "macos" { 1.0 } else { usable(scale) }
}

/// What the numbers a window reports in its own pixels are divided by to get them into the shared space: macOS
/// reports in the scale of the monitor of the window and compares in points, so the scale; elsewhere they are
/// physical already, so 1.
pub fn shared_divisor(os: &str, scale: f64) -> f64 {
    if os == "macos" { usable(scale) } else { 1.0 }
}

/// A point of the shared space as whole physical pixels, for a window that is placed in them: rounded, and held to
/// what the window system can take.
pub fn physical_pixels(point: (f64, f64)) -> (i32, i32) {
    let pixel = |v: f64| {
        if v.is_finite() { v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32 } else { 0 }
    };
    (pixel(point.0), pixel(point.1))
}

/// Whether the window system says where the pointer and the windows are, in one desktop space. Always, except
/// under Wayland, where an application has neither (tao: `cursor_position` is 0,0 and window positions have no
/// global coordinate system), so every number would be a wrong one: a release outside a window would be taken for
/// a drop at the corner of another. A Wayland session is `XDG_SESSION_TYPE=wayland` or a `WAYLAND_DISPLAY`; an app
/// forced onto X11 (`GDK_BACKEND` starting with `x11`, so XWayland) sees global coordinates again.
///
/// `os` is as `std::env::consts::OS` spells it; `env` looks up a variable of the process.
pub fn pointer_is_global(os: &str, env: &dyn Fn(&str) -> Option<String>) -> bool {
    if os != "linux" {
        return true;
    }
    let value = |name: &str| env(name).map(|v| v.trim().to_ascii_lowercase()).unwrap_or_default();
    let on_x11 = value("GDK_BACKEND").split(',').next().is_some_and(|first| first.trim() == "x11");
    let wayland = value("XDG_SESSION_TYPE") == "wayland" || !value("WAYLAND_DISPLAY").is_empty();
    on_x11 || !wayland
}

// ---- which window is under the pointer --------------------------------------------------------------------------

/// The frame of one window, in the shared space of the module doc.
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
    /// How many units of this space one CSS pixel of the page is ([`units_per_point`]).
    pub scale: f64,
}

impl Frame {
    fn contains(&self, point: (f64, f64)) -> bool {
        point.0 >= self.x && point.0 < self.x + self.width && point.1 >= self.y && point.1 < self.y + self.height
    }

    /// `point` in the client coordinates of the page of this window, in CSS pixels.
    pub fn client(&self, point: (f64, f64)) -> (f64, f64) {
        ((point.0 - self.inner_x) / self.scale, (point.1 - self.inner_y) / self.scale)
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

// ---- the window a drag is over --------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
struct Hover {
    from: String,
    over: String,
}

/// The window that a drag from another window is over right now, so that it can be told when the drag leaves it or ends.
/// There is at most one drag at a time (one pointer), so there is at most one hover. The shell keeps nothing else of it:
/// the outline lives in the page that is hovered.
#[derive(Debug, Default)]
pub struct Hovers(Option<Hover>);

impl Hovers {
    /// The drag that started in `from` is over the window `over` now. The window it was over before, when that was
    /// another one, is returned: it has to be told the drag left.
    pub fn enter(&mut self, from: &str, over: &str) -> Option<String> {
        let before = self.0.replace(Hover { from: from.to_owned(), over: over.to_owned() });
        before.map(|h| h.over).filter(|w| w != over)
    }

    /// The drag that started in `from` is over no other window, or it ended. The window it was over is returned.
    pub fn leave(&mut self, from: &str) -> Option<String> {
        if self.0.as_ref().is_some_and(|h| h.from == from) { self.0.take().map(|h| h.over) } else { None }
    }

    /// The window `label` is gone. A hover over it is dropped (nobody is left to tell); a hover that started in it
    /// ends, and the window it was over is returned.
    pub fn forget(&mut self, label: &str) -> Option<String> {
        match self.0.as_ref() {
            Some(h) if h.from == label => self.0.take().map(|h| h.over),
            Some(h) if h.over == label => {
                self.0 = None;
                None
            }
            _ => None,
        }
    }

    /// The window the drag is over, for tests.
    #[cfg(test)]
    pub fn over(&self) -> Option<&str> {
        self.0.as_ref().map(|h| h.over.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(label: &str, x: f64, y: f64, width: f64, height: f64) -> Frame {
        Frame { label: label.into(), x, y, width, height, inner_x: x, inner_y: y + 28.0, scale: 1.0 }
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| pairs.iter().find(|(n, _)| *n == name).map(|(_, v)| (*v).to_owned())
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
    fn a_drag_is_over_one_window_and_the_one_it_left_is_named() {
        let mut hovers = Hovers::default();
        assert_eq!(hovers.enter("main", "window-2"), None, "nothing was hovered before");
        assert_eq!(hovers.enter("main", "window-2"), None, "still the same window: nobody to tell");
        assert_eq!(hovers.enter("main", "window-3").as_deref(), Some("window-2"), "moved on: the first is told");
        assert_eq!(hovers.over(), Some("window-3"));
        assert_eq!(hovers.leave("window-2"), None, "another window's drag does not end this one");
        assert_eq!(hovers.leave("main").as_deref(), Some("window-3"));
        assert_eq!(hovers.leave("main"), None, "ending twice tells nobody twice");
        assert_eq!(hovers.over(), None);
    }

    #[test]
    fn a_window_that_goes_ends_the_hover_it_started_and_drops_the_one_over_it() {
        let mut hovers = Hovers::default();
        hovers.enter("main", "window-2");
        assert_eq!(hovers.forget("window-9"), None);
        assert_eq!(hovers.over(), Some("window-2"));
        assert_eq!(hovers.forget("main").as_deref(), Some("window-2"), "the source went: the other is told");
        hovers.enter("main", "window-2");
        assert_eq!(hovers.forget("window-2"), None, "the hovered window went: nobody to tell");
        assert_eq!(hovers.over(), None);
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
        assert_eq!(position_under_cursor((500.0, 400.0), 1.0), (440.0, 384.0));
        assert_eq!(position_beside((100.0, 80.0), 1.0), (132.0, 112.0));
    }

    #[test]
    fn a_window_placed_under_the_pointer_or_beside_another_at_200_percent_moves_by_scaled_offsets() {
        // The grab point (60, 16) and the offset (32) are points; in physical pixels at 200 % they are twice as many.
        assert_eq!(position_under_cursor((1000.0, 800.0), 2.0), (880.0, 768.0));
        assert_eq!(position_beside((100.0, 80.0), 2.0), (164.0, 144.0));
        assert_eq!(position_under_cursor((1000.0, 800.0), 1.5), (910.0, 776.0), "and at 150 % one and a half times");
    }

    #[test]
    fn the_units_of_the_shared_space_are_points_on_macos_and_physical_pixels_elsewhere() {
        assert_eq!(units_per_point("macos", 2.0), 1.0, "a point is a point");
        assert_eq!(units_per_point("windows", 1.5), 1.5);
        assert_eq!(units_per_point("linux", 2.0), 2.0);
        assert_eq!(shared_divisor("macos", 2.0), 2.0, "macOS reports in the scale of the monitor");
        assert_eq!(shared_divisor("windows", 1.5), 1.0, "elsewhere the numbers are physical already");
        assert_eq!(shared_divisor("linux", 2.0), 1.0);
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(units_per_point("windows", bad), 1.0, "{bad}");
            assert_eq!(shared_divisor("macos", bad), 1.0, "{bad}");
        }
    }

    #[test]
    fn a_150_percent_client_offset_is_in_css_pixels() {
        // The page starts at physical (1500, 345) and the pointer is at (1650, 645): 150 and 300 physical pixels
        // from the origin are 100 and 200 CSS pixels at a scale of 1.5.
        let f = Frame { scale: 1.5, inner_x: 1500.0, inner_y: 345.0, ..frame("window-5", 1500.0, 300.0, 900.0, 600.0) };
        assert_eq!(f.client((1650.0, 645.0)), (100.0, 200.0));
    }

    #[test]
    fn a_client_offset_at_125_and_200_percent_is_divided_by_the_scale_of_that_window() {
        let page = frame("w", 1000.0, 72.0, 900.0, 600.0);
        let at = |scale: f64| Frame { scale, inner_x: 1000.0, inner_y: 100.0, ..page.clone() };
        assert_eq!(at(1.25).client((1250.0, 350.0)), (200.0, 200.0));
        assert_eq!(at(2.0).client((1400.0, 500.0)), (200.0, 200.0));
        assert_eq!(at(1.0).client((1200.0, 300.0)), (200.0, 200.0), "at 100 % nothing changes");
    }

    #[test]
    fn a_drop_on_a_window_at_another_scale_lands_where_the_pointer_is() {
        // Two windows in one physical desktop: `left` at 100 %, `right` at 200 %. The hit test compares physical
        // numbers; only the offset into the page is converted, and with the scale of the window that was hit.
        let left = frame("left", 0.0, 0.0, 1920.0, 1080.0);
        let right = Frame { scale: 2.0, inner_x: 1920.0, inner_y: 56.0, ..frame("right", 1920.0, 0.0, 2000.0, 1000.0) };
        let frames = [left, right];

        let hit = window_under((2120.0, 456.0), &frames, &[], "left").expect("the pointer is over the right window");
        assert_eq!(hit.label, "right");
        assert_eq!(hit.client((2120.0, 456.0)), (100.0, 200.0), "200 physical pixels right are 100 CSS pixels");
        let back = window_under((300.0, 128.0), &frames, &[], "right").expect("and over the left one");
        assert_eq!(back.client((300.0, 128.0)), (300.0, 100.0), "the left window is at 100 %");
    }

    #[test]
    fn a_position_in_physical_pixels_is_rounded_and_held_to_what_the_window_system_takes() {
        assert_eq!(physical_pixels((880.4, 767.5)), (880, 768));
        assert_eq!(physical_pixels((-12.6, 0.0)), (-13, 0), "a monitor left of the primary has negative numbers");
        assert_eq!(physical_pixels((f64::NAN, f64::INFINITY)), (0, 0));
        assert_eq!(physical_pixels((1e20, -1e20)), (i32::MAX, i32::MIN));
    }

    #[test]
    fn a_wayland_session_has_no_pointer_the_application_can_read() {
        for session in [
            env(&[("XDG_SESSION_TYPE", "wayland")]),
            env(&[("XDG_SESSION_TYPE", "Wayland")]),
            env(&[("WAYLAND_DISPLAY", "wayland-0")]),
            env(&[("XDG_SESSION_TYPE", "x11"), ("WAYLAND_DISPLAY", "wayland-0")]),
            env(&[("XDG_SESSION_TYPE", "wayland"), ("GDK_BACKEND", "wayland")]),
            env(&[("WAYLAND_DISPLAY", "wayland-0"), ("GDK_BACKEND", "wayland,x11")]),
            env(&[("WAYLAND_DISPLAY", "wayland-0"), ("GDK_BACKEND", "*")]),
        ] {
            assert!(!pointer_is_global("linux", &session));
        }
    }

    #[test]
    fn an_app_on_xwayland_or_a_plain_x11_session_has_a_global_pointer() {
        assert!(
            pointer_is_global("linux", &env(&[("XDG_SESSION_TYPE", "wayland"), ("GDK_BACKEND", "x11")])),
            "GDK_BACKEND=x11 under a Wayland session is XWayland, where positions are global"
        );
        assert!(pointer_is_global("linux", &env(&[("WAYLAND_DISPLAY", "wayland-0"), ("GDK_BACKEND", " X11,wayland")])));
        assert!(pointer_is_global("linux", &env(&[("XDG_SESSION_TYPE", "x11"), ("DISPLAY", ":0")])));
        assert!(pointer_is_global("linux", &env(&[])), "a session that says nothing is not Wayland");
        assert!(pointer_is_global("linux", &env(&[("WAYLAND_DISPLAY", "")])), "an empty display is none");
    }

    #[test]
    fn the_pointer_is_global_on_macos_and_windows_and_on_a_platform_nobody_knows() {
        let wayland = env(&[("XDG_SESSION_TYPE", "wayland"), ("WAYLAND_DISPLAY", "wayland-0")]);
        assert!(pointer_is_global("macos", &wayland), "the variables mean nothing there");
        assert!(pointer_is_global("windows", &wayland));
        assert!(pointer_is_global("plan9", &wayland), "an unknown platform keeps the numbers it gets");
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
