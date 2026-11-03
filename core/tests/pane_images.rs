//! Images through a real pane (POL-01, D-03): a real shell prints a Kitty graphics sequence, a sink sees the placement
//! in a frame, and the pixels can be read back from the handle in bounded pieces. No Tauri, no window.
//!
//! Every wait is bounded and every pane is closed at the end of its test (and by the handle's `Drop` when an
//! assertion fails first), so no child outlives the test binary.
#![cfg(feature = "ghostty")]
// These tests start `/bin/sh`, which a Windows host does not have.
#![cfg(unix)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::wait_until;
use holzwave_core::pane::{PaneEvent, PaneHandle, PaneSpec};
use holzwave_core::wire::{FrameMsg, PlacementMsg, image_data_bytes};

const WAIT: Duration = Duration::from_secs(10);

/// One pixel, red 0, green 0, blue 0, alpha 255, shown in one cell with the id 5.
const PRINT_ONE_PIXEL: &str = r"printf '\033_Ga=T,f=32,s=1,v=1,i=5,c=1,r=1;AAAA/w==\033\\'";

#[derive(Clone, Default)]
struct Probe {
    frames: Arc<Mutex<Vec<FrameMsg>>>,
}

impl Probe {
    fn sink(&self) -> impl FnMut(PaneEvent) + Send + 'static {
        let frames = self.frames.clone();
        move |event| {
            if let PaneEvent::Frame(frame) = event {
                frames.lock().unwrap().push(frame);
            }
        }
    }

    fn frames(&self) -> Vec<FrameMsg> {
        self.frames.lock().unwrap().clone()
    }

    fn has_text(&self, needle: &str) -> bool {
        self.frames().iter().any(|f| {
            f.changed.iter().any(|r| {
                let text: String = r.runs.iter().flat_map(|run| run.0.chars()).collect();
                text.contains(needle)
            })
        })
    }

    /// The first frame that carries a list of images with something in it.
    fn first_with_images(&self) -> Option<FrameMsg> {
        self.frames().into_iter().find(|f| f.images.as_ref().is_some_and(|list| !list.is_empty()))
    }
}

fn spec(script: &str) -> PaneSpec {
    let mut spec = PaneSpec::new("/bin/sh", 40, 10).args(["-c", script]);
    spec.cell_px = (8, 16);
    spec
}

fn spawn(script: &str, probe: &Probe) -> PaneHandle {
    PaneHandle::spawn_ghostty(1, spec(script), probe.sink()).expect("the pane starts")
}

/// A pane that has printed the one-pixel picture, and the frame that told about it.
fn pane_with_picture(then: &str, probe: &Probe) -> (PaneHandle, PlacementMsg) {
    let pane = spawn(&format!("{PRINT_ONE_PIXEL}; {then}"), probe);
    assert!(wait_until(WAIT, || probe.first_with_images().is_some()), "a frame with the placement arrives");
    let frame = probe.first_with_images().expect("frame");
    let list = frame.images.expect("images");
    (pane, list[0].clone())
}

#[test]
fn a_picture_printed_by_a_real_program_is_one_placement_in_a_frame() {
    let probe = Probe::default();
    let (pane, placement) = pane_with_picture("sleep 5", &probe);
    let frame = probe.first_with_images().expect("frame");
    assert_eq!(frame.images.as_ref().expect("images").len(), 1);
    assert_eq!(placement.0, 5, "the image id is the first number");
    assert_ne!(placement.1, 0, "a generation is never 0");
    assert_eq!(placement.2, 2, "z of 0 is above the text");
    // Column 0, row 0, one cell wide and tall, no offset, 8 by 16 pixels on the screen, the whole 1 by 1 source.
    assert_eq!((placement.4, placement.5, placement.6, placement.7), (0, 0, 1, 1));
    assert_eq!((placement.8, placement.9, placement.10, placement.11), (0, 0, 8, 16));
    assert_eq!((placement.12, placement.13, placement.14, placement.15), (0, 0, 1, 1));
    let json = serde_json::to_value(&frame).expect("json");
    let array = json["images"][0].as_array().expect("one array per placement");
    assert_eq!(array.len(), 16, "sixteen numbers");
    assert_eq!(array[0], 5);
    pane.close();
}

#[test]
fn a_frame_with_only_text_changes_carries_no_images() {
    let probe = Probe::default();
    let (pane, _) = pane_with_picture("sleep 0.3; printf 'later text'; sleep 5", &probe);
    assert!(wait_until(WAIT, || probe.has_text("later text")), "the text arrives");
    let frames = probe.frames();
    let with_text = frames
        .iter()
        .find(|f| f.changed.iter().any(|r| r.runs.iter().any(|run| run.0.contains("later text"))))
        .expect("the frame with the text");
    assert!(with_text.images.is_none(), "the list did not change, so the frame does not carry it");
    let json = serde_json::to_value(with_text).expect("json");
    assert!(json.get("images").is_none(), "and the key is not on the wire at all");
    pane.close();
}

#[test]
fn a_request_for_a_full_frame_carries_the_placement_again() {
    let probe = Probe::default();
    let (pane, placement) = pane_with_picture("sleep 5", &probe);
    let before = probe.frames().len();
    pane.request_full().expect("request");
    assert!(wait_until(WAIT, || probe.frames().len() > before), "the full frame arrives");
    let full = probe.frames().into_iter().skip(before).find(|f| f.full).expect("a full frame");
    assert_eq!(full.images.as_deref(), Some(&[placement][..]), "the same placement, in the same order");
    pane.close();
}

#[test]
fn the_pixels_are_read_from_the_handle_by_id_and_generation() {
    let probe = Probe::default();
    let (pane, placement) = pane_with_picture("sleep 5", &probe);
    let chunk = pane.image_chunk(5, placement.1, 0, 1024 * 1024).expect("answer").expect("the image is there");
    assert_eq!((chunk.width, chunk.height, chunk.total, chunk.offset), (1, 1, 4, 0));
    assert_eq!(chunk.bytes, [0, 0, 0, 255]);
    // The raw answer has the 16-byte header and then the bytes.
    let raw = image_data_bytes(&chunk).expect("fits");
    assert_eq!(&raw[..16], [0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 4, 0, 0, 0, 0]);
    assert_eq!(&raw[16..], [0, 0, 0, 255]);
    pane.close();
}

#[test]
fn a_read_is_cut_to_what_was_asked_and_an_offset_past_the_end_has_no_bytes() {
    let probe = Probe::default();
    let (pane, placement) = pane_with_picture("sleep 5", &probe);
    let two = pane.image_chunk(5, placement.1, 1, 2).expect("answer").expect("image");
    assert_eq!((two.offset, two.bytes.len()), (1, 2));
    let past = pane.image_chunk(5, placement.1, usize::MAX, 100).expect("answer").expect("image");
    assert_eq!((past.total, past.offset, past.bytes.len()), (4, 4, 0), "the offset is brought to the end");
    let none = pane.image_chunk(5, placement.1, 0, 0).expect("answer").expect("image");
    assert!(none.bytes.is_empty(), "a bound of 0 reads nothing");
    pane.close();
}

#[test]
fn another_generation_or_another_image_answers_none_never_other_pixels() {
    let probe = Probe::default();
    let (pane, placement) = pane_with_picture("sleep 5", &probe);
    assert!(pane.image_chunk(5, placement.1 + 1, 0, 100).expect("answer").is_none(), "wrong generation");
    assert!(pane.image_chunk(5, 0, 0, 100).expect("answer").is_none(), "generation 0 is never valid");
    assert!(pane.image_chunk(6, placement.1, 0, 100).expect("answer").is_none(), "unknown image");
    assert!(pane.image_chunk(u32::MAX, u64::MAX, usize::MAX, usize::MAX).expect("answer").is_none(), "extremes");
    pane.close();
}

#[test]
fn a_closed_pane_answers_an_error() {
    let probe = Probe::default();
    let (pane, placement) = pane_with_picture("sleep 5", &probe);
    pane.close();
    assert!(pane.join(WAIT), "the pane ended");
    assert!(pane.image_chunk(5, placement.1, 0, 100).is_err(), "a pane that is gone cannot be read");
}

#[test]
fn deleting_the_picture_sends_an_empty_list_and_then_the_read_fails() {
    let probe = Probe::default();
    let script = r"sleep 0.3; printf '\033_Ga=d,d=I,i=5\033\\'; sleep 5";
    let (pane, placement) = pane_with_picture(script, &probe);
    assert!(
        wait_until(WAIT, || probe.frames().iter().any(|f| f.images.as_ref().is_some_and(Vec::is_empty))),
        "a frame says the list is now empty"
    );
    assert!(pane.image_chunk(5, placement.1, 0, 100).expect("answer").is_none(), "the pixels are gone");
    pane.close();
}
