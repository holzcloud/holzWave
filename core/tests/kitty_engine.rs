#![cfg(feature = "ghostty")]
//! The Kitty graphics protocol through the engine: a program's bytes in, the placement list and bounded pixel reads
//! out, and the limits and refusals read back from the terminal.
//!
//! Every test sends the bytes a real program sends (built by `kitty_support`) to a `GhosttyEngine` and looks at what the
//! engine's own API says: `update().images`, `image_chunk`, `take_replies` and `image_limits`. Where the library does
//! something other than the plan expected, the assertion states what it does and a comment says so.

mod kitty_support;

use holzwave_core::engine::{ImageChunk, ImageLayer, ImagePlacement, Update};
use holzwave_core::{GhosttyEngine, TerminalEngine};
use kitty_support::*;

const MIB: u64 = 1024 * 1024;

/// A 40 by 10 engine with an 8 by 16 pixel cell and the given image memory.
fn engine_with(limit: u64) -> GhosttyEngine {
    let mut engine = GhosttyEngine::with_image_limit(40, 10, 100, limit).expect("engine");
    engine.resize(40, 10, (8, 16)).expect("resize");
    engine
}

fn replies(engine: &mut GhosttyEngine) -> String {
    engine.take_replies().escape_ascii().to_string()
}

/// The placements of the next update; panics when the update says "no change", so a test that expects a list gets one.
fn list(update: Update) -> Vec<ImagePlacement> {
    update.images.expect("the placement list changed, so the update carries it")
}

fn whole(engine: &mut GhosttyEngine, id: u32, generation: u64) -> ImageChunk {
    engine.image_chunk(id, generation, 0, MIB as usize).expect("the image has pixels")
}

/// A scratch directory that the test creates (never reuses) and removes again, for the files of the media test.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("holzwave-engine-{tag}-{}", std::process::id()));
        std::fs::create_dir(&dir).expect("a fresh scratch directory (a leftover one is not reused)");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn tracer_an_rgba_image_gives_one_placement_and_the_next_update_says_nothing_changed() {
    let mut engine = engine_with(4 * MIB);
    let rgba = seeded_pixels(7, 4, 4);
    engine.feed(&transmit_rgba(2, 2, "i=7,c=2,r=1", &rgba));
    assert_eq!(replies(&mut engine), "\\x1b_Gi=7;OK\\x1b\\\\", "the program is told that the image arrived");

    let images = list(engine.update().expect("update"));
    assert_eq!(images.len(), 1, "{images:?}");
    let p = images[0];
    assert_eq!(p.image_id, 7);
    assert_ne!(p.generation, 0, "a stored image never has generation 0");
    assert_eq!((p.layer, p.z), (ImageLayer::AboveText, 0));
    assert_eq!((p.col, p.row), (0, 0));
    assert_eq!((p.cols, p.rows), (2, 1), "c=2,r=1");
    assert_eq!((p.offset_x, p.offset_y), (0, 0));
    assert_eq!((p.width, p.height), (16, 16), "two cells of 8 px, one of 16 px");
    assert_eq!((p.src_x, p.src_y, p.src_w, p.src_h), (0, 0, 2, 2), "all of the 2 by 2 image");

    let again = engine.update().expect("update");
    assert_eq!(again.images, None, "nothing changed, so the consumer keeps its list");
}

#[test]
fn the_pixels_come_back_in_bounded_windows_and_only_for_the_generation_asked_for() {
    let mut engine = engine_with(4 * MIB);
    let rgba = seeded_pixels(11, 4, 4);
    engine.feed(&transmit_rgba(2, 2, "i=7,c=2,r=1", &rgba));
    let generation = list(engine.update().expect("update"))[0].generation;

    let all = whole(&mut engine, 7, generation);
    assert_eq!((all.width, all.height, all.total, all.offset), (2, 2, 16, 0));
    assert_eq!(all.generation, generation);
    // Straight alpha: what the program sent comes back byte for byte, with nothing multiplied in.
    assert_eq!(all.bytes, rgba);

    let window = engine.image_chunk(7, generation, 8, 4).expect("a window");
    assert_eq!((window.offset, window.total), (8, 16));
    assert_eq!(window.bytes, rgba[8..12]);

    let tail = engine.image_chunk(7, generation, 12, 100).expect("the rest");
    assert_eq!(tail.bytes, rgba[12..16], "a window never reaches past the image");
    let past = engine.image_chunk(7, generation, 1000, 100).expect("an offset past the end is an empty window");
    assert_eq!((past.offset, past.bytes.len()), (16, 0));
    let nothing = engine.image_chunk(7, generation, 0, 0).expect("max 0");
    assert!(nothing.bytes.is_empty());

    assert_eq!(engine.image_chunk(7, generation + 1, 0, 16), None, "another generation is not this image");
    assert_eq!(engine.image_chunk(8, generation, 0, 16), None, "no image 8");
}

#[test]
fn a_png_is_decoded_to_straight_rgba() {
    let mut engine = engine_with(4 * MIB);
    // A pixel with partial alpha stays as it is: nothing is multiplied.
    let mut pixels = seeded_pixels(3, 16, 4);
    pixels[3] = 128;
    let png = png_rgba(4, 4, &pixels);
    engine.feed(&apc("a=T,f=100,i=5,c=1,r=1", &png));
    assert_eq!(replies(&mut engine), "\\x1b_Gi=5;OK\\x1b\\\\");
    let placed = list(engine.update().expect("update"));
    assert_eq!(placed.len(), 1);
    let image = whole(&mut engine, 5, placed[0].generation);
    assert_eq!((image.width, image.height, image.total), (4, 4, 64));
    assert_eq!(image.bytes, pixels);
    assert_eq!((placed[0].src_w, placed[0].src_h), (4, 4));
}

#[test]
fn an_rgb_image_comes_back_as_rgba_with_opaque_alpha() {
    let mut engine = engine_with(4 * MIB);
    engine.feed(&apc("a=T,f=24,s=2,v=1,i=6,c=1,r=1", &[10, 20, 30, 40, 50, 60]));
    let placed = list(engine.update().expect("update"));
    let image = whole(&mut engine, 6, placed[0].generation);
    assert_eq!((image.width, image.height, image.total), (2, 1, 8));
    assert_eq!(image.bytes, [10, 20, 30, 255, 40, 50, 60, 255]);
    // A window in the middle of the widened form: bytes 3 to 6 are alpha, red, green of the two pixels.
    let window = engine.image_chunk(6, placed[0].generation, 3, 3).expect("window");
    assert_eq!(window.bytes, [255, 40, 50]);
}

#[test]
fn a_zlib_compressed_image_is_inflated() {
    let mut engine = engine_with(4 * MIB);
    let rgba = seeded_pixels(21, 6, 4);
    engine.feed(&apc("a=T,f=32,o=z,s=3,v=2,i=9,c=3,r=1", &zlib_stored(&rgba)));
    let placed = list(engine.update().expect("update"));
    let image = whole(&mut engine, 9, placed[0].generation);
    assert_eq!(image.bytes, rgba);
}

#[test]
fn a_chunked_transmission_arrives_whole() {
    let mut engine = engine_with(4 * MIB);
    // 100 by 100 RGBA is 40 000 bytes: 53 334 base64 characters, 14 chunks of 4096.
    let rgba = seeded_pixels(5, 100 * 100, 4);
    let pieces = chunked("a=T,f=32,s=100,v=100,i=3,c=10,r=5", &rgba, CHUNK_CHARS);
    assert!(pieces.len() > 10);
    for piece in &pieces {
        engine.feed(piece);
    }
    let placed = list(engine.update().expect("update"));
    assert_eq!(placed.len(), 1);
    let image = whole(&mut engine, 3, placed[0].generation);
    assert_eq!(image.total, 40_000);
    assert_eq!(image.bytes, rgba);
}

#[test]
fn a_png_header_that_declares_gigabytes_is_refused_and_the_terminal_stays_usable() {
    let mut engine = engine_with(4 * MIB);
    engine.feed(&apc("a=T,f=100,i=4,c=1,r=1", &png_header_only(30_000, 30_000)));
    let reply = replies(&mut engine);
    assert!(reply.contains("i=4;E"), "an error reply for image 4, got {reply:?}");
    assert_eq!(engine.image_generation(4), None, "nothing is stored");
    let update = engine.update().expect("update");
    assert_eq!(update.images, Some(vec![]), "the first update starts the list, and it is empty");

    // The same refusal for a size over the dimension limit, and for a header that is not a PNG at all.
    engine.feed(&apc("a=t,f=100,i=4", &png_header_only(10_001, 1)));
    engine.feed(&apc("a=t,f=100,i=4", b"this is not a png but it is longer than twenty-four bytes"));
    let reply = replies(&mut engine);
    assert_eq!(reply.matches("i=4;E").count(), 2, "{reply:?}");
    assert_eq!(engine.image_generation(4), None);

    // And a good header with a truncated body: the library's decoder is reached and refuses.
    let good = png_rgba(4, 4, &seeded_pixels(1, 16, 4));
    engine.feed(&apc("a=t,f=100,i=4", &good[..good.len() - 20]));
    assert!(replies(&mut engine).contains("i=4;E"), "a truncated PNG is refused");
    assert_eq!(engine.image_generation(4), None);

    // The terminal still works: text, and a good image.
    engine.feed(b"hello");
    engine.feed(&transmit_rgba(1, 1, "i=1,c=1,r=1", &[1, 2, 3, 4]));
    let update = engine.update().expect("update");
    assert_eq!(list(update).len(), 1);
    assert!(engine.screen().expect("screen").to_text().contains("hello"));
}

#[test]
fn the_three_media_read_back_as_refused_and_each_transmission_is_answered_with_an_error() {
    let mut engine = engine_with(4 * MIB);
    let limits = engine.image_limits().expect("limits");
    assert!(!limits.from_file, "the file medium is refused");
    assert!(!limits.from_temp_file, "the temporary-file medium is refused");
    assert!(!limits.from_shared_memory, "the shared-memory medium is refused");
    assert_eq!(limits.storage_bytes, 4 * MIB);

    // A real file with known bytes: if the engine read it (t=f), or read and deleted it (t=t), the test would see it.
    let scratch = Scratch::new("media");
    let path = scratch.0.join("pixels.rgba");
    let bytes = seeded_pixels(2, 4, 4);
    std::fs::write(&path, &bytes).expect("write the file");
    let name = path.to_str().expect("a utf-8 path");

    for (medium, id) in [("f", 1), ("t", 2), ("s", 3)] {
        engine.feed(&apc(&format!("a=T,t={medium},f=32,s=2,v=2,i={id},c=1,r=1"), name.as_bytes()));
        let reply = replies(&mut engine);
        assert!(reply.contains(&format!("i={id};E")), "t={medium} is answered with an error, got {reply:?}");
        assert_eq!(engine.image_generation(id), None, "t={medium} stored no image");
    }
    let update = engine.update().expect("update");
    assert_eq!(update.images, Some(vec![]), "no placement for any of them");
    assert_eq!(std::fs::read(&path).expect("the file is still there"), bytes, "and the file was not touched");

    // The files a program is most likely to try, and a path that climbs out of a directory.
    for target in ["/etc/passwd", "../../../../../etc/passwd", "/dev/zero"] {
        engine.feed(&apc("a=T,t=f,f=32,s=2,v=2,i=8", target.as_bytes()));
        assert!(replies(&mut engine).contains("i=8;E"), "{target} is refused");
        assert_eq!(engine.image_generation(8), None);
    }

    // Still refused after a reset (RIS), which returns the terminal to its defaults.
    engine.feed(b"\x1bc");
    let limits = engine.image_limits().expect("limits");
    assert!(!limits.from_file && !limits.from_temp_file && !limits.from_shared_memory);
}

#[test]
fn new_keeps_its_signature_and_has_64_mib() {
    let engine = GhosttyEngine::new(40, 10, 100).expect("engine");
    assert_eq!(engine.image_limits().expect("limits").storage_bytes, 64 * MIB);
    assert_eq!(holzwave_core::images::DEFAULT_IMAGE_BYTES, 64 * MIB);
}

#[test]
fn a_limit_of_zero_turns_the_protocol_off() {
    let mut engine = engine_with(0);
    assert_eq!(engine.image_limits().expect("limits").storage_bytes, 0);
    engine.feed(&transmit_rgba(1, 1, "i=1,c=1,r=1", &[1, 2, 3, 4]));
    assert_eq!(engine.image_generation(1), None, "nothing is stored");
    // The library stays silent when the protocol is off, which is how a program learns that there is no graphics
    // support (the plan expected an error reply; a silent terminal is the library's answer and the better one).
    assert_eq!(replies(&mut engine), "");
    let update = engine.update().expect("update");
    assert_eq!(update.images, Some(vec![]));
    engine.feed(b"still works");
    assert!(engine.screen().expect("screen").to_text().contains("still works"));
}

#[test]
fn without_a_known_cell_size_the_list_is_empty_and_the_image_appears_when_it_is_known() {
    // Never told the cell size (no `resize` with pixels): the pixel size of a placement cannot be known.
    let mut engine = GhosttyEngine::with_image_limit(40, 10, 100, 4 * MIB).expect("engine");
    engine.feed(&transmit_rgba(2, 2, "i=7,c=2,r=1", &seeded_pixels(1, 4, 4)));
    assert_eq!(engine.image_generation(7).is_some(), true, "the image itself is stored");
    let update = engine.update().expect("update");
    assert_eq!(update.images, Some(vec![]), "an empty list, not a missing one, so the page clears its pictures");

    engine.resize(40, 10, (8, 16)).expect("resize");
    let placed = list(engine.update().expect("update"));
    assert_eq!(placed.len(), 1, "now the cell size is known");
    assert_eq!((placed[0].width, placed[0].height), (16, 16));
}

#[test]
fn the_list_follows_a_change_of_the_cell_size() {
    let mut engine = engine_with(4 * MIB);
    engine.feed(&transmit_rgba(2, 2, "i=7,c=2,r=1", &seeded_pixels(1, 4, 4)));
    let first = list(engine.update().expect("update"));
    assert_eq!((first[0].width, first[0].height), (16, 16));
    engine.resize(40, 10, (10, 20)).expect("resize");
    let second = list(engine.update().expect("update"));
    assert_eq!((second[0].width, second[0].height), (20, 20), "the same cells at a new cell size");
    assert_eq!(first[0].generation, second[0].generation, "the pixels did not change");
}

#[test]
fn a_reset_update_always_carries_the_list_even_when_nothing_changed() {
    let mut engine = engine_with(4 * MIB);
    engine.feed(&transmit_rgba(2, 2, "i=7,c=2,r=1", &seeded_pixels(1, 4, 4)));
    let first = list(engine.update().expect("update"));
    assert_eq!(engine.update().expect("update").images, None);
    engine.request_full();
    let update = engine.update().expect("update");
    assert!(update.reset);
    assert_eq!(update.images, Some(first), "a consumer that starts over gets the list again");
}

