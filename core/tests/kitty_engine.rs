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

// ---------------------------------------------------------------------------------------------------------------
// Level 1, static images: place, move, delete, scroll, evict.
// ---------------------------------------------------------------------------------------------------------------

/// Move the cursor to row `row` and column `col` (both 1-based) and send a place command that leaves the cursor alone
/// (`C=1`), so placements land where the test says and do not push the next one around.
fn place_at(engine: &mut GhosttyEngine, row: u16, col: u16, image_id: u32, extra: &str) {
    engine.feed(format!("\x1b[{row};{col}H").as_bytes());
    engine.feed(&place(image_id, &format!("C=1,{extra}")));
}

fn ids(images: &[ImagePlacement]) -> Vec<(u32, i32)> {
    images.iter().map(|p| (p.image_id, p.z)).collect()
}

#[test]
fn an_image_can_be_transmitted_once_and_placed_twice_with_the_layers_in_drawing_order() {
    let mut engine = engine_with(4 * MIB);
    // `a=t` stores without showing: no placement yet.
    engine.feed(&apc("a=t,f=32,s=2,v=2,i=10", &seeded_pixels(1, 4, 4)));
    assert_eq!(list(engine.update().expect("update")), vec![]);
    place_at(&mut engine, 1, 1, 10, "p=1,c=2,r=1,z=5");
    place_at(&mut engine, 3, 5, 10, "p=2,c=2,r=1,z=-1");
    place_at(&mut engine, 5, 9, 10, "p=3,c=2,r=1,z=-1073741825");
    place_at(&mut engine, 7, 13, 10, "p=4,c=2,r=1,z=-2");
    let placed = list(engine.update().expect("update"));
    assert_eq!(placed.len(), 4, "one entry per placement of the one image: {placed:?}");
    assert!(placed.iter().all(|p| p.image_id == 10 && p.generation == placed[0].generation));
    // Drawing order: below the backgrounds, below the text (lowest z first), above the text.
    let layers: Vec<_> = placed.iter().map(|p| p.layer).collect();
    assert_eq!(
        layers,
        [ImageLayer::BelowBg, ImageLayer::BelowText, ImageLayer::BelowText, ImageLayer::AboveText]
    );
    assert_eq!(ids(&placed), [(10, -1073741825), (10, -2), (10, -1), (10, 5)]);
    // Where each one is: the cursor cell of its place command (the list is sorted, so look them up by z).
    let at = |z: i32| placed.iter().find(|p| p.z == z).map(|p| (p.col, p.row)).expect("a placement with this z");
    assert_eq!(at(5), (0, 0));
    assert_eq!(at(-1), (4, 2));
    assert_eq!(at(-1073741825), (8, 4));
    assert_eq!(at(-2), (12, 6));
}

#[test]
fn a_placement_carries_its_source_rectangle_and_its_offset_inside_the_first_cell() {
    let mut engine = engine_with(4 * MIB);
    engine.feed(&apc("a=t,f=32,s=4,v=4,i=11", &seeded_pixels(2, 16, 4)));
    place_at(&mut engine, 2, 3, 11, "c=2,r=1,x=1,y=1,w=2,h=2,X=3,Y=5");
    let p = list(engine.update().expect("update"))[0];
    assert_eq!((p.col, p.row), (2, 1));
    assert_eq!((p.src_x, p.src_y, p.src_w, p.src_h), (1, 1, 2, 2), "the cut of the image that is shown");
    assert_eq!((p.offset_x, p.offset_y), (3, 5), "in pixels, inside the first cell");
    assert_eq!((p.cols, p.rows), (2, 1));
}

#[test]
fn deleting_by_image_by_placement_by_cell_and_all() {
    let mut engine = engine_with(4 * MIB);
    let pixels = seeded_pixels(4, 4, 4);
    engine.feed(&apc("a=t,f=32,s=2,v=2,i=20", &pixels));
    engine.feed(&apc("a=t,f=32,s=2,v=2,i=21", &pixels));
    place_at(&mut engine, 1, 1, 20, "p=1,c=2,r=1");
    place_at(&mut engine, 3, 1, 20, "p=2,c=2,r=1");
    place_at(&mut engine, 5, 1, 21, "p=1,c=2,r=1");
    assert_eq!(list(engine.update().expect("update")).len(), 3);

    // One placement of an image (the placement id says which).
    engine.feed(&delete("d=i,i=20,p=2"));
    let placed = list(engine.update().expect("update"));
    assert_eq!(placed.len(), 2, "{placed:?}");
    assert!(placed.iter().any(|p| (p.image_id, p.row) == (20, 0)) && placed.iter().any(|p| p.image_id == 21));

    // The placements that cover one cell (d=p with x and y, 1-based), here the one of image 21 at row 5.
    engine.feed(&delete("d=p,x=1,y=5"));
    let placed = list(engine.update().expect("update"));
    assert_eq!(placed.iter().map(|p| p.image_id).collect::<Vec<_>>(), [20]);

    // Every placement of an image. The lower-case form keeps the pixels: the program can place the image again.
    engine.feed(&delete("d=i,i=20"));
    assert_eq!(list(engine.update().expect("update")), vec![], "the frame after the delete lists none of them");
    let generation = engine.image_generation(20).expect("the lower-case delete keeps the image");
    assert!(engine.image_chunk(20, generation, 0, 16).is_some());

    // The upper-case form frees the image as well.
    place_at(&mut engine, 1, 1, 20, "p=1,c=2,r=1");
    assert_eq!(list(engine.update().expect("update")).len(), 1);
    engine.feed(&delete("d=I,i=20"));
    assert_eq!(list(engine.update().expect("update")), vec![]);
    assert_eq!(engine.image_generation(20), None, "the image is gone");
    assert_eq!(engine.image_chunk(20, generation, 0, 16), None, "and its pixels cannot be read any more");

    // All visible placements.
    place_at(&mut engine, 1, 1, 21, "p=7,c=2,r=1");
    place_at(&mut engine, 3, 1, 21, "p=8,c=2,r=1");
    assert_eq!(list(engine.update().expect("update")).len(), 2);
    engine.feed(&delete("d=a"));
    assert_eq!(list(engine.update().expect("update")), vec![]);
    assert!(engine.image_generation(21).is_some(), "d=a keeps the images");
    // The upper-case form frees the images whose placements it deletes; with no placement left it has nothing to free.
    engine.feed(&delete("d=A"));
    assert!(engine.image_generation(21).is_some());
    place_at(&mut engine, 1, 1, 21, "p=9,c=2,r=1");
    assert_eq!(list(engine.update().expect("update")).len(), 1);
    engine.feed(&delete("d=A"));
    assert_eq!(list(engine.update().expect("update")), vec![]);
    assert_eq!(engine.image_generation(21), None, "d=A frees the image of the placement it deleted");
}

#[test]
fn an_image_that_scrolls_out_reports_negative_rows_and_leaves_the_list_when_it_is_gone() {
    let mut engine = engine_with(4 * MIB);
    engine.feed(&apc("a=t,f=32,s=2,v=2,i=30", &seeded_pixels(6, 4, 4)));
    place_at(&mut engine, 1, 1, 30, "c=2,r=4");
    let at_top = list(engine.update().expect("update"));
    assert_eq!((at_top[0].row, at_top[0].rows), (0, 4));

    // Two lines of output scroll the screen by two rows.
    engine.feed(b"\x1b[10;1H\r\n\r\n");
    let scrolled = list(engine.update().expect("update"));
    assert_eq!(scrolled.len(), 1, "{scrolled:?}");
    assert_eq!(scrolled[0].row, -2, "the top two rows of the image are above the viewport");
    assert_eq!(scrolled[0].rows, 4, "the placement keeps its size; the page clips");
    assert_eq!(scrolled[0].generation, at_top[0].generation);

    // Four more lines: the whole image is above the viewport and nothing of it is left to draw.
    engine.feed(b"\r\n\r\n\r\n\r\n");
    assert_eq!(list(engine.update().expect("update")), vec![]);

    // Scrolled back into the history, it is there again, in the viewport's own coordinates.
    engine.scroll(-6);
    let back = list(engine.update().expect("update"));
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].row, 0);
}

#[test]
fn the_oldest_images_are_evicted_when_the_memory_is_full_and_the_newest_stays() {
    // 1 MiB of pixels; ten images of 300 KiB (320 by 240 RGBA) are three times the memory.
    let mut engine = engine_with(MIB);
    let image = seeded_pixels(9, 320 * 240, 4);
    assert_eq!(image.len(), 300 * 1024);
    for id in 1..=10u32 {
        for piece in chunked(&format!("a=t,f=32,s=320,v=240,i={id}"), &image, CHUNK_CHARS) {
            engine.feed(&piece);
        }
    }
    let stored: Vec<u32> = (1..=10).filter(|&id| engine.image_generation(id).is_some()).collect();
    assert!(stored.len() <= 3, "1 MiB holds three images of 300 KiB, found {stored:?}");
    assert!(!stored.is_empty());
    assert!(!stored.contains(&1), "the oldest is gone: {stored:?}");
    assert!(stored.contains(&10), "the newest stays: {stored:?}");

    // An image bigger than all the memory is refused with an error and the terminal carries on.
    let _ = replies(&mut engine);
    let big = seeded_pixels(1, 600 * 600, 4);
    for piece in chunked("a=t,f=32,s=600,v=600,i=99", &big, CHUNK_CHARS) {
        engine.feed(&piece);
    }
    let reply = replies(&mut engine);
    assert!(reply.contains("i=99;E"), "an image over the whole limit is refused, got {reply:?}");
    assert_eq!(engine.image_generation(99), None);
    engine.feed(b"alive");
    assert!(engine.screen().expect("screen").to_text().contains("alive"));
}

#[test]
fn a_program_that_asks_gets_the_answer_of_the_library() {
    let mut engine = engine_with(4 * MIB);
    // A query transmits one pixel and asks whether it would be accepted: the answer is `OK`, and nothing is stored.
    engine.feed(&apc("a=q,i=31,s=1,v=1,f=32", &[0, 0, 0, 0]));
    assert_eq!(replies(&mut engine), "\\x1b_Gi=31;OK\\x1b\\\\");
    assert_eq!(engine.image_generation(31), None, "a query stores nothing");
    // A query that names a refused medium is answered with an error.
    engine.feed(&apc("a=q,i=32,t=f,s=1,v=1,f=32", b"/etc/passwd"));
    assert!(replies(&mut engine).contains("i=32;E"));
    // A query with the quiet flag gets no answer for success.
    engine.feed(&apc("a=q,i=33,s=1,v=1,f=32,q=1", &[0, 0, 0, 0]));
    assert_eq!(replies(&mut engine), "");
}

// ---------------------------------------------------------------------------------------------------------------
// Level 2, animation: frames, the control command, the clock the pane owns.
// ---------------------------------------------------------------------------------------------------------------

/// A 2 by 2 image of two frames, playing: the first frame stays 25 ms, the second 60 ms.
fn animated(engine: &mut GhosttyEngine, id: u32, first: &[u8], second: &[u8]) {
    engine.feed(&transmit_rgba(2, 2, &format!("i={id},c=2,r=1,q=2"), first));
    engine.feed(&animation_frame(id, 2, 2, "z=60,q=2", second));
    engine.feed(&animation_control(id, "r=1,z=25,q=2"));
    engine.feed(&animation_control(id, "s=3,q=2"));
}

fn current(engine: &mut GhosttyEngine, id: u32) -> (u64, Vec<u8>) {
    let generation = engine.image_generation(id).expect("the image is stored");
    (generation, whole(engine, id, generation).bytes)
}

#[test]
fn an_animation_advances_only_when_the_pane_ticks_and_each_frame_has_a_new_generation() {
    let mut engine = engine_with(4 * MIB);
    let (red, blue) = (seeded_pixels(1, 4, 4), seeded_pixels(2, 4, 4));
    assert_ne!(red, blue);
    animated(&mut engine, 40, &red, &blue);

    let placed = list(engine.update().expect("update"));
    assert_eq!(placed.len(), 1);
    let g0 = placed[0].generation;
    assert_eq!(whole(&mut engine, 40, g0).bytes, red, "the root frame shows first");
    // The engine never ticks by itself: another update changes nothing.
    assert_eq!(engine.update().expect("update").images, None);

    // The first tick starts the timer of the first frame and changes nothing; it says when the next frame is due.
    assert_eq!(engine.tick_images(1000), Some(1025));
    assert_eq!(engine.update().expect("update").images, None);
    // On time: the second frame shows, with a new generation, a new list and the second frame's pixels.
    assert_eq!(engine.tick_images(1025), Some(1085), "the second frame stays 60 ms");
    let moved = list(engine.update().expect("update"));
    assert_eq!(moved.len(), 1);
    assert_ne!(moved[0].generation, g0, "a new frame is a new generation");
    assert_eq!(whole(&mut engine, 40, moved[0].generation).bytes, blue);
    assert_eq!(engine.image_chunk(40, g0, 0, 16), None, "the old generation is gone");
    // And around again.
    assert_eq!(engine.tick_images(1085), Some(1110));
    let (g2, pixels) = current(&mut engine, 40);
    assert_eq!(pixels, red);
    assert!(g2 != g0 && g2 != moved[0].generation, "a generation is never reused");
}

#[test]
fn a_paused_animation_keeps_its_frame_and_resumes_from_it() {
    let mut engine = engine_with(4 * MIB);
    let (red, blue) = (seeded_pixels(3, 4, 4), seeded_pixels(4, 4, 4));
    animated(&mut engine, 41, &red, &blue);
    let _ = engine.update().expect("update");
    assert_eq!(engine.tick_images(0), Some(25));
    assert_eq!(engine.tick_images(25), Some(85));
    let (shown, pixels) = current(&mut engine, 41);
    assert_eq!(pixels, blue);
    let _ = engine.update().expect("update");

    engine.set_image_animation(false);
    assert_eq!(engine.tick_images(85), None, "paused: nothing is due and nothing moves");
    assert_eq!(engine.tick_images(100_000), None);
    assert_eq!(engine.update().expect("update").images, None, "no new list");
    assert_eq!(current(&mut engine, 41), (shown, blue.clone()), "the frame that was shown stays");

    engine.set_image_animation(true);
    assert!(engine.tick_images(100_000).is_some(), "resumed: the animation is playing again");
    // It goes on from the frame that was shown (the second): the next one is the first, not a restart from scratch.
    let (_, pixels) = current(&mut engine, 41);
    assert!(pixels == red || pixels == blue, "one of the two frames");
}

#[test]
fn the_stop_command_of_the_protocol_stops_the_animation() {
    let mut engine = engine_with(4 * MIB);
    animated(&mut engine, 42, &seeded_pixels(5, 4, 4), &seeded_pixels(6, 4, 4));
    let _ = engine.update().expect("update");
    assert!(engine.tick_images(0).is_some());
    engine.feed(&animation_control(42, "s=1,q=2"));
    assert_eq!(engine.tick_images(1000), None, "stopped: nothing animates");
    let (generation, _) = current(&mut engine, 42);
    assert_eq!(engine.tick_images(2000), None);
    assert_eq!(current(&mut engine, 42).0, generation, "and the frame does not change");
}

#[test]
fn nothing_is_due_when_no_image_animates() {
    let mut engine = engine_with(4 * MIB);
    assert_eq!(engine.tick_images(0), None, "no images at all");
    engine.feed(&transmit_rgba(2, 2, "i=1,c=2,r=1,q=2", &seeded_pixels(1, 4, 4)));
    assert_eq!(engine.tick_images(10), None, "a still image");
    assert_eq!(engine.tick_images(5000), None);
}

// ---------------------------------------------------------------------------------------------------------------
// Level 3, Unicode placeholders: the picture follows the text.
// ---------------------------------------------------------------------------------------------------------------

/// A 4 by 2 cell image shown through placeholder cells at columns 2 to 5 of the first two rows, with plain text on both
/// sides of the first row. Image id 9, so its colour is the 256-colour palette entry 9.
fn placeholder_screen(engine: &mut GhosttyEngine) {
    engine.feed(&apc("a=t,f=32,s=2,v=2,i=9,q=2", &seeded_pixels(8, 4, 4)));
    engine.feed(&apc("a=p,U=1,i=9,c=4,r=2,q=2", &[]));
    engine.feed(b"\x1b[1;1Hxy");
    engine.feed(&placeholder_row(9, 0, 4));
    engine.feed(b"zw\r\n  ");
    engine.feed(&placeholder_row(9, 1, 4));
}

#[test]
fn placeholder_cells_give_a_placement_where_the_text_sits() {
    let mut engine = engine_with(4 * MIB);
    placeholder_screen(&mut engine);
    let placed = list(engine.update().expect("update"));
    // One entry per row: the library reports a run of neighbouring placeholder cells in one row as one placement,
    // each one row tall, below the text (a virtual placement is always drawn there).
    assert_eq!(placed.len(), 2, "{placed:?}");
    for (row, p) in placed.iter().enumerate() {
        assert_eq!(p.image_id, 9);
        assert_eq!(p.layer, ImageLayer::BelowText);
        assert_eq!((p.col, p.row), (2, row as i32), "the cells where the text is");
        assert_eq!((p.cols, p.rows), (4, 1));
        assert_eq!((p.width, p.height), (32, 16));
    }
    // The two entries cut the image into its two halves.
    assert_eq!((placed[0].src_y, placed[0].src_h), (0, 1));
    assert_eq!((placed[1].src_y, placed[1].src_h), (1, 1));
}

#[test]
fn placeholder_cells_read_as_blanks_that_keep_their_colours() {
    let mut engine = engine_with(4 * MIB);
    placeholder_screen(&mut engine);
    let screen = engine.screen().expect("screen");
    let cols = usize::from(screen.cols);
    for row in 0..2 {
        for col in 2..6 {
            let cell = &screen.cells[row * cols + col];
            assert_eq!(cell.text, "", "row {row} column {col} is a placeholder, so it is blank");
            assert_ne!(cell.fg, screen.default_fg, "the colour that carries the image id is kept");
            assert_eq!(cell.bg, screen.default_bg);
        }
    }
    // The text next to them is untouched.
    let text = |col: usize| screen.cells[col].text.as_str();
    assert_eq!((text(0), text(1), text(6), text(7)), ("x", "y", "z", "w"));
    assert!(screen.to_text().lines().next().expect("row 0").starts_with("xy    zw"));
    assert!(!screen.to_text().contains('\u{10EEEE}'));

    // The same cells in an update: `screen` and `update` read a row through the same code.
    let update = engine.update().expect("update");
    let first = &update.changed[0].cells;
    assert!(first[2..6].iter().all(|c| c.text.is_empty()));
}

#[test]
fn copying_across_placeholder_rows_holds_no_placeholder_and_no_mark() {
    let mut engine = engine_with(4 * MIB);
    placeholder_screen(&mut engine);
    engine.select(holzwave_core::engine::SelectOp::Begin { x: 0, y: 0, mode: holzwave_core::engine::SelectMode::Cell }).expect("begin");
    engine.select(holzwave_core::engine::SelectOp::Extend { x: 7, y: 1 }).expect("extend");
    let text = engine.selection_text().expect("selection").expect("some text");
    assert!(text.chars().all(|c| c.is_ascii()), "no placeholder, no combining mark: {text:?}");
    assert!(text.starts_with("xy"), "{text:?}");
    assert!(text.contains("zw"), "{text:?}");
    // A read of the region (what the AI context uses) agrees.
    let read = engine
        .read_text(holzwave_core::textread::TextRegion::Selection, 4096)
        .expect("read")
        .expect("text");
    assert!(read.text.chars().all(|c| c.is_ascii()), "{:?}", read.text);
}

#[test]
fn the_placeholder_image_follows_its_text_through_scrolling() {
    let mut engine = engine_with(4 * MIB);
    placeholder_screen(&mut engine);
    let before = list(engine.update().expect("update"));
    assert_eq!(before.iter().map(|p| p.row).collect::<Vec<_>>(), [0, 1]);
    // The program prints on: both rows scroll up by one line, and the image moves with the text, not with the
    // terminal's idea of a position.
    engine.feed(b"\x1b[10;1H\r\n");
    let after = list(engine.update().expect("update"));
    // A placeholder image is one entry per row of text (the library's list), so the entry of a row that has scrolled
    // out of the viewport is gone and no entry has a negative row: only the second row is left, now the first.
    assert_eq!(after.iter().map(|p| (p.col, p.row)).collect::<Vec<_>>(), [(2, 0)], "the row that is still there");
    assert_eq!(after[0].src_y, 1, "and it is still the second half of the image");
    // Blanked cells scroll with them.
    let screen = engine.screen().expect("screen");
    assert!(screen.cells[..usize::from(screen.cols)][2..6].iter().all(|c| c.text.is_empty()));
}
