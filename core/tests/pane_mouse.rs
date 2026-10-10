//! Mouse and wheel through the pane actor (D-24): a real shell on a real PTY shows what the program received, and a
//! recording engine shows how the actor routes and orders the two messages.
//!
//! The actor tests of plan 12 stay in `pane.rs`. Every wait is bounded and every pane is closed at the end of its
//! test (and by the handle's `Drop` when an assertion fails first).
#![cfg(feature = "ghostty")]

mod common;

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::wait_until;
use holzwave_core::engine::{
    DomKey, EngineTheme, KeyEvent, Mods, MouseAction, MouseButton, MouseEvent, PasteOutcome, Screen, SelectMode,
    SelectOp, TerminalEngine, Update, WheelEvent, WheelOutcome,
};
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
use holzwave_core::queue::OUTPUT_LIMIT;
use holzwave_core::wire::{FrameMsg, RowMsg};

const WAIT: Duration = Duration::from_secs(10);

/// The cell size of every pane here: 8 x 16 device pixels.
const CELL: (u32, u32) = (8, 16);

/// Collects everything a pane sends to its sink.
#[derive(Clone, Default)]
struct Probe {
    log: Arc<Mutex<Vec<PaneEvent>>>,
}

impl Probe {
    fn sink(&self) -> impl FnMut(PaneEvent) + Send + 'static {
        let log = self.log.clone();
        move |event| log.lock().unwrap().push(event)
    }

    fn events(&self) -> Vec<PaneEvent> {
        self.log.lock().unwrap().clone()
    }

    fn frames(&self) -> Vec<FrameMsg> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                PaneEvent::Frame(f) => Some(f),
                _ => None,
            })
            .collect()
    }

    /// Whether any frame carried a changed row with this text.
    fn has_text(&self, needle: &str) -> bool {
        self.frames().iter().any(|f| f.changed.iter().any(|r| row_text(r).contains(needle)))
    }

    /// The top row of the viewport in the newest frame.
    fn offset(&self) -> Option<u64> {
        self.frames().last().map(|f| f.scrollbar.0)
    }
}

/// The text of a wire row: one code point per cell, the spacer marker left out.
fn row_text(row: &RowMsg) -> String {
    row.runs.iter().flat_map(|run| run.0.chars()).filter(|c| *c != '\0').collect()
}

fn at_bottom(frame: &FrameMsg) -> bool {
    let (offset, total, len) = frame.scrollbar;
    offset + len >= total
}

fn spec_sh(script: &str) -> PaneSpec {
    let mut spec = PaneSpec::new("/bin/sh", 80, 24).args(["-c", script]);
    spec.cell_px = CELL;
    spec
}

fn spawn(spec: PaneSpec, probe: &Probe) -> PaneHandle {
    PaneHandle::spawn_ghostty(1, spec, probe.sink()).expect("the pane starts")
}

/// The pixel at the centre of cell (`col`, `row`).
fn centre(col: u32, row: u32) -> (f32, f32) {
    ((col * CELL.0 + CELL.0 / 2) as f32, (row * CELL.1 + CELL.1 / 2) as f32)
}

fn click(action: MouseAction, button: MouseButton, mods: Mods, at: (f32, f32)) -> MouseEvent {
    MouseEvent { action, button, mods, x: at.0, y: at.1 }
}

fn wheel(rows: i32, mods: Mods, at: (f32, f32)) -> WheelEvent {
    WheelEvent { rows, mods, x: at.0, y: at.1 }
}

fn close(pane: &PaneHandle) {
    pane.close();
    assert!(pane.join(Duration::from_secs(5)), "the pane did not end");
}

#[test]
fn a_click_reaches_a_program_that_asked_for_the_mouse() {
    let probe = Probe::default();
    // The tty echoes what the engine writes to it with ESC made visible as ^[, so the screen shows the report.
    let pane = spawn(spec_sh("printf '\\033[?1000h\\033[?1006h'; printf ready; exec cat -v"), &probe);
    assert!(wait_until(WAIT, || probe.has_text("ready")), "the shell never got going: {:?}", probe.events());

    pane.mouse(click(MouseAction::Press, MouseButton::Left, Mods::NONE, centre(4, 2))).expect("mouse");

    assert!(
        wait_until(WAIT, || probe.has_text("^[[<0;5;3M")),
        "the press never reached the program: {:?}",
        probe.frames().last()
    );
    close(&pane);
}

#[test]
fn a_click_is_not_sent_to_a_program_that_did_not_ask_for_the_mouse() {
    let probe = Probe::default();
    let pane = spawn(spec_sh("printf ready; exec cat -v"), &probe);
    assert!(wait_until(WAIT, || probe.has_text("ready")));

    pane.mouse(click(MouseAction::Press, MouseButton::Left, Mods::NONE, centre(4, 2))).expect("mouse");
    pane.mouse(click(MouseAction::Release, MouseButton::Left, Mods::NONE, centre(4, 2))).expect("mouse");
    // A key after it: once its echo is on the screen the two clicks had every chance to arrive before it.
    pane.key(DomKey { code: "KeyZ".into(), key: "z".into(), ..DomKey::default() }).expect("key");

    assert!(wait_until(WAIT, || probe.has_text("z")), "the key never echoed");
    assert!(!probe.has_text("^[["), "a report reached a program that did not ask: {:?}", probe.frames().last());
    close(&pane);
}

#[test]
fn the_wheel_in_the_alternate_screen_reaches_the_program_as_arrow_keys() {
    let probe = Probe::default();
    let pane = spawn(spec_sh("printf '\\033[?1049hready'; exec cat -v"), &probe);
    assert!(wait_until(WAIT, || probe.has_text("ready")), "the shell never got going: {:?}", probe.events());

    pane.wheel(wheel(-2, Mods::NONE, centre(4, 2))).expect("wheel");

    assert!(
        wait_until(WAIT, || probe.has_text("^[[A^[[A")),
        "the arrows never reached the program: {:?}",
        probe.frames().last()
    );
    close(&pane);
}

#[test]
fn the_wheel_with_tracking_on_reaches_the_program_as_wheel_reports() {
    let probe = Probe::default();
    let pane = spawn(spec_sh("printf '\\033[?1000h\\033[?1006h'; printf ready; exec cat -v"), &probe);
    assert!(wait_until(WAIT, || probe.has_text("ready")));

    pane.wheel(wheel(1, Mods::NONE, centre(4, 2))).expect("wheel");

    assert!(wait_until(WAIT, || probe.has_text("^[[<65;5;3M")), "no report: {:?}", probe.frames().last());
    close(&pane);
}

/// 100 lines of output, so that the pane has history to scroll through.
const HISTORY_LOOP: &str = "i=0; while [ $i -lt 100 ]; do echo line $i; i=$((i+1)); done";

#[test]
fn a_wheel_on_the_primary_screen_moves_the_scrollbar_offset_in_the_frame_header() {
    let probe = Probe::default();
    let pane = spawn(spec_sh(&format!("{HISTORY_LOOP}; exec cat")), &probe);
    assert!(wait_until(WAIT, || probe.has_text("line 99")));
    assert!(wait_until(WAIT, || probe.frames().last().is_some_and(at_bottom)));
    let bottom = probe.offset().expect("a frame");

    pane.wheel(wheel(-5, Mods::NONE, centre(4, 2))).expect("wheel");

    assert!(
        wait_until(WAIT, || probe.offset() == Some(bottom - 5)),
        "the viewport did not move: {:?} (from {bottom})",
        probe.offset()
    );
    close(&pane);
}

#[test]
fn shift_scrolls_locally_even_when_the_program_tracks_the_mouse() {
    let probe = Probe::default();
    let script = format!("{HISTORY_LOOP}; printf '\\033[?1000h\\033[?1006h'; printf tracking; exec cat -v");
    let pane = spawn(spec_sh(&script), &probe);
    assert!(wait_until(WAIT, || probe.has_text("tracking")), "{:?}", probe.frames().last());
    assert!(wait_until(WAIT, || probe.frames().last().is_some_and(at_bottom)));
    let bottom = probe.offset().expect("a frame");

    pane.wheel(wheel(-4, Mods::SHIFT, centre(4, 2))).expect("wheel");

    assert!(wait_until(WAIT, || probe.offset() == Some(bottom - 4)), "{:?} from {bottom}", probe.offset());
    // And nothing was sent to the program.
    thread::sleep(Duration::from_millis(300));
    assert!(!probe.has_text("^[[<"), "Shift+wheel reached the program");
    close(&pane);
}

/// What the recording engine saw, in order, and a few counters.
#[derive(Default)]
struct Spy {
    calls: Mutex<Vec<String>>,
    fed: AtomicUsize,
    /// How often the actor asked for an update, which it does when it has a frame to send.
    updates: AtomicUsize,
    /// Bit `n` is set once the capital letter `A + n` was fed.
    upper: AtomicU32,
}

impl Spy {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn saw(&self, letter: u8) -> bool {
        self.upper.load(Ordering::Relaxed) & (1 << (letter - b'A')) != 0
    }
}

/// An engine that records what the actor asks of it and answers mouse and wheel with fixed bytes.
struct Recorder {
    spy: Arc<Spy>,
    size: (u16, u16),
    feed_delay: Duration,
    reset: bool,
}

const CHUNK: usize = 64 * 1024;

impl TerminalEngine for Recorder {
    fn feed(&mut self, bytes: &[u8]) {
        self.spy.fed.fetch_add(bytes.len(), Ordering::Relaxed);
        let mut seen = 0u32;
        for b in bytes.iter().filter(|b| b.is_ascii_uppercase()) {
            seen |= 1 << (b - b'A');
        }
        self.spy.upper.fetch_or(seen, Ordering::Relaxed);
        if !self.feed_delay.is_zero() {
            thread::sleep(self.feed_delay.mul_f64(bytes.len() as f64 / CHUNK as f64));
        }
    }

    fn resize(&mut self, cols: u16, rows: u16, _cell_px: (u32, u32)) -> holzwave_core::Result<()> {
        self.size = (cols, rows);
        self.reset = true;
        Ok(())
    }

    fn size(&self) -> holzwave_core::Result<(u16, u16)> {
        Ok(self.size)
    }

    fn scroll(&mut self, rows: isize) {
        self.spy.calls.lock().unwrap().push(format!("scroll {rows}"));
    }

    fn scroll_to_bottom(&mut self) {
        self.spy.calls.lock().unwrap().push("scroll_to_bottom".into());
    }

    fn screen(&mut self) -> holzwave_core::Result<Screen> {
        Ok(Screen::default())
    }

    fn set_option_as_alt(&mut self, _on: bool) {}

    fn encode_key(&mut self, _event: &KeyEvent) -> holzwave_core::Result<Vec<u8>> {
        Ok(Vec::new())
    }

    fn encode_text(&mut self, text: &str) -> holzwave_core::Result<Vec<u8>> {
        Ok(text.as_bytes().to_vec())
    }

    fn paste(&mut self, _text: &str, _allow_unsafe: bool) -> holzwave_core::Result<PasteOutcome> {
        Ok(PasteOutcome::Empty)
    }

    fn take_replies(&mut self) -> Vec<u8> {
        Vec::new()
    }

    fn update(&mut self) -> holzwave_core::Result<Update> {
        self.spy.updates.fetch_add(1, Ordering::Relaxed);
        let reset = std::mem::take(&mut self.reset);
        Ok(Update { full: reset, reset, cols: self.size.0, rows: self.size.1, ..Update::default() })
    }

    fn select(&mut self, op: SelectOp) -> holzwave_core::Result<()> {
        self.spy.calls.lock().unwrap().push(format!("select {op:?}"));
        Ok(())
    }

    fn mouse(&mut self, event: &MouseEvent) -> holzwave_core::Result<Vec<u8>> {
        self.spy.calls.lock().unwrap().push(format!("mouse {:?} {:?}", event.action, event.button));
        Ok(b"Q\r".to_vec())
    }

    fn wheel(&mut self, event: &WheelEvent) -> holzwave_core::Result<WheelOutcome> {
        self.spy.calls.lock().unwrap().push(format!("wheel {}", event.rows));
        Ok(match event.rows {
            0 => WheelOutcome::Nothing,
            r if r < 0 => WheelOutcome::Bytes(b"W\r".to_vec()),
            _ => WheelOutcome::Scrolled,
        })
    }
}

fn spawn_recorder(spec: PaneSpec, probe: &Probe, feed_delay: Duration) -> (PaneHandle, Arc<Spy>) {
    let spy = Arc::new(Spy::default());
    let engine_spy = spy.clone();
    let pane = PaneHandle::spawn(
        1,
        spec,
        move |_| Ok(Recorder { spy: engine_spy, size: (80, 24), feed_delay, reset: true }),
        probe.sink(),
    )
    .expect("the pane starts");
    (pane, spy)
}

#[test]
fn mouse_bytes_go_to_the_child_and_a_report_does_not_clear_the_selection_or_scroll_to_the_bottom() {
    let probe = Probe::default();
    // Echo off, and the shell reads two lines and prints them with capitals that the flood never contains.
    let (pane, spy) =
        spawn_recorder(spec_sh("stty -echo; read -r a; read -r b; printf 'GOT-%s-%s' \"$a\" \"$b\"; sleep 30"), &probe, Duration::ZERO);

    pane.select(SelectOp::Begin { x: 1, y: 1, mode: SelectMode::Cell }).expect("select");
    assert!(wait_until(WAIT, || spy.calls().iter().any(|c| c.starts_with("select Begin"))));
    pane.mouse(click(MouseAction::Press, MouseButton::Right, Mods::NONE, centre(1, 1))).expect("mouse");
    pane.wheel(wheel(-1, Mods::NONE, centre(1, 1))).expect("wheel");

    assert!(wait_until(WAIT, || spy.saw(b'Q') && spy.saw(b'W')), "the child never got both: {:?}", spy.calls());
    let calls = spy.calls();
    assert_eq!(calls, vec!["select Begin { x: 1, y: 1, mode: Cell }", "mouse Press Right", "wheel -1"]);
    close(&pane);
}

#[test]
fn a_local_scroll_outcome_asks_for_a_frame_and_sends_nothing_to_the_child() {
    let probe = Probe::default();
    let (pane, spy) = spawn_recorder(spec_sh("sleep 30"), &probe, Duration::ZERO);
    assert!(wait_until(WAIT, || !probe.frames().is_empty()), "no first frame");
    // Let the start settle: no output is coming, so the number of updates stops growing.
    thread::sleep(Duration::from_millis(200));
    let before = spy.updates.load(Ordering::Relaxed);

    pane.wheel(wheel(3, Mods::NONE, centre(0, 0))).expect("wheel");

    assert!(
        wait_until(WAIT, || spy.updates.load(Ordering::Relaxed) > before),
        "a scroll did not ask for a new frame"
    );
    assert_eq!(spy.calls(), vec!["wheel 3"], "nothing else was asked of the engine");
    close(&pane);
}

#[test]
fn mouse_and_wheel_are_handled_ahead_of_queued_output() {
    let probe = Probe::default();
    // The consumer needs 100 ms per 64 KiB (about 0.65 MB/s) and the producer is endless, so the queue is full of
    // output when the two messages arrive. Handled in arrival order they would wait behind about a second of it.
    let (pane, spy) = spawn_recorder(
        spec_sh("stty -echo; yes aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa & read -r a; read -r b; printf 'GOT-%s-%s' \"$a\" \"$b\"; sleep 30"),
        &probe,
        Duration::from_millis(100),
    );
    assert!(
        wait_until(WAIT, || pane.queue_high_water() >= OUTPUT_LIMIT / 2),
        "the flood never filled the queue"
    );

    let sent = Instant::now();
    pane.mouse(click(MouseAction::Press, MouseButton::Left, Mods::NONE, centre(1, 1))).expect("mouse");
    pane.wheel(wheel(-1, Mods::NONE, centre(1, 1))).expect("wheel");

    assert!(
        wait_until(Duration::from_millis(700), || spy.calls().len() == 2),
        "the messages starved behind the output: {:?} after {:?}",
        spy.calls(),
        sent.elapsed()
    );
    assert!(pane.queue_high_water() >= OUTPUT_LIMIT / 2);
    close(&pane);
}
