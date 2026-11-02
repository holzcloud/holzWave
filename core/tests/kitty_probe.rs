#![cfg(feature = "ghostty")]
//! Facts about the Kitty graphics and OSC 8 support of the pinned engine (libghostty-rs 575ed6b, Ghostty 521cf2c5,
//! Zig 0.17.0), measured through a raw `libghostty_vt::Terminal` that is fed scripted escape sequences.
//!
//! The always-run tests pin what the engine does, so that a bump of the engine revision that changes one of these
//! facts fails here first. Where the engine does something other than the research assumed, the assertion states the
//! real behaviour and a `FACT` line says so (run with `-- --nocapture` to read them). The hostile-stream probes are
//! at the end of the file: ignored, release builds only, one `PROBE` line each.

mod kitty_support;

use kitty_support::*;
use libghostty_vt::Error;
use libghostty_vt::kitty::graphics::ImageFormat;
use libghostty_vt::render::{ImageLayer, KittyPlacementKind};

/// A scratch directory that the test creates (never reuses) and removes again, for the one file of the media test.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("holzwave-kitty-{tag}-{}", std::process::id()));
        std::fs::create_dir(&dir).expect("a fresh scratch directory (a leftover one is not reused)");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The harness itself: exact bytes.
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn builders_produce_exact_bytes() {
    // base64 (RFC 4648 test vectors).
    for (plain, coded) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("foobar", "Zm9vYmFy")] {
        assert_eq!(base64(plain.as_bytes()), coded.as_bytes());
    }
    // One command: ESC _ G control ; base64 ESC \ , no line break, and no ';' without a payload.
    assert_eq!(apc("a=T,f=32,s=1,v=1,i=7", &[1, 2, 3, 4]), b"\x1b_Ga=T,f=32,s=1,v=1,i=7;AQIDBA==\x1b\\");
    assert_eq!(apc("a=d,d=I,i=7", &[]), b"\x1b_Ga=d,d=I,i=7\x1b\\");
    assert_eq!(place(7, "c=2"), b"\x1b_Ga=p,i=7,c=2\x1b\\");
    assert_eq!(delete("d=I,i=7"), b"\x1b_Ga=d,d=I,i=7\x1b\\");
    // Chunks: the control keys only on the first piece, m=1 on every piece but the last, 4096 characters at most.
    let payload = seeded_pixels(9, 3000, 4); // 12 000 bytes, 16 000 base64 characters
    let pieces = chunked("a=T,f=32,s=3000,v=1,i=7", &payload, CHUNK_CHARS);
    assert_eq!(pieces.len(), 4);
    assert!(pieces[0].starts_with(b"\x1b_Ga=T,f=32,s=3000,v=1,i=7,m=1;"));
    assert!(pieces[1].starts_with(b"\x1b_Gm=1;") && pieces[2].starts_with(b"\x1b_Gm=1;"));
    assert!(pieces[3].starts_with(b"\x1b_Gm=0;"));
    let mut joined = Vec::new();
    for (i, piece) in pieces.iter().enumerate() {
        assert!(piece.ends_with(b"\x1b\\"), "every piece ends with ST");
        let semi = piece.iter().position(|&b| b == b';').expect("a payload separator");
        let text = &piece[semi + 1..piece.len() - 2];
        assert!(text.len() <= CHUNK_CHARS, "piece {i} is {} characters", text.len());
        assert!(!text.iter().any(|&b| b == b'\n' || b == b'\r'), "no line break in the base64 text");
        joined.extend_from_slice(text);
    }
    assert_eq!(joined, base64(&payload), "the pieces joined are the base64 of the payload");
    let single = chunked("a=T", &[1, 2, 3], CHUNK_CHARS);
    assert_eq!(single, [b"\x1b_Ga=T,m=0;AQID\x1b\\".to_vec()]);
    // zlib: stored blocks and the Adler-32 of the data (the known value for "Wikipedia" is 0x11E60398).
    assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    assert_eq!(zlib_stored(b""), [0x78, 0x01, 0x01, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01]);
    let z = zlib_stored(b"abc");
    assert_eq!(&z[..7], [0x78, 0x01, 0x01, 0x03, 0x00, 0xfc, 0xff]);
    assert_eq!(&z[7..10], b"abc");
    assert_eq!(&z[10..], adler32(b"abc").to_be_bytes());
    // OSC 8 and a placeholder row.
    assert_eq!(osc8("https://example.org/a", "link"), b"\x1b]8;;https://example.org/a\x1b\\link\x1b]8;;\x1b\\");
    let row = String::from_utf8(placeholder_row(7, 1, 2)).unwrap();
    assert_eq!(row, "\x1b[38;5;7m\u{10EEEE}\u{030D}\u{0305}\u{10EEEE}\u{030D}\u{030D}\x1b[39m");
}

// ---------------------------------------------------------------------------------------------------------------
// Static images: bytes to placement to pixels.
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn tracer_rgba_image_from_an_apc_sequence_to_its_placement_and_pixels() {
    let mut t = raw_terminal(20, 5, (8, 16), 1 << 20);
    let rgba = seeded_pixels(7, 4, 4);
    assert_eq!(rgba.len(), 16);
    t.feed(&transmit_rgba(2, 2, "i=7,c=2,r=1", &rgba));
    assert_eq!(t.take_replies(), "\\x1b_Gi=7;OK\\x1b\\\\", "the program is told the image arrived");

    let (list_generation, placed) = t.placements();
    assert_ne!(list_generation, 0, "the placement list has a generation once an image is placed");
    assert_eq!(placed.len(), 1, "one placement: {placed:?}");
    let p = &placed[0];
    assert_eq!(p.image_id, 7);
    assert_ne!(p.generation, 0, "a stored image never has generation 0");
    assert_eq!((p.layer, p.z), (ImageLayer::AboveText, 0));
    assert_eq!((p.col, p.row), (0, 0));
    assert_eq!(p.grid, (2, 1), "c=2,r=1");
    assert_eq!(p.pixel, (16, 16), "two cells of 8 px wide, one of 16 px high");
    assert_eq!(p.source, (0, 0, 2, 2), "the whole 2x2 image");
    assert_eq!(p.kind, Some(KittyPlacementKind::Fixed));

    // The generation of the entry is the generation of the stored image: the page's cache key.
    let graphics = t.terminal.kitty_graphics().unwrap();
    let image = graphics.image(7).expect("image 7 is stored");
    assert_eq!(image.generation().unwrap(), p.generation);
    assert_eq!(image.format().unwrap(), ImageFormat::Rgba);
    assert_eq!((image.width().unwrap(), image.height().unwrap()), (2, 2));
    assert_eq!(image.data().unwrap().expect("pixels"), &rgba[..], "the 16 bytes that were sent come back unchanged");
}

#[test]
fn alpha_is_straight_not_premultiplied() {
    let mut t = raw_terminal(20, 5, (8, 16), 1 << 20);
    t.feed(&transmit_rgba(1, 1, "i=3,c=1,r=1", &[200, 100, 50, 128]));
    let (data, w, h) = t.image_pixels(3).expect("image 3");
    assert_eq!((w, h), (1, 1));
    // Straight alpha: the colour channels are what was sent. A premultiplied store would give [100, 50, 25, 128].
    assert_eq!(data, [200, 100, 50, 128]);
    eprintln!("FACT Alpha: Image::data() returns straight RGBA, [200,100,50,128] in and out, format Rgba");
}

#[test]
fn rgb_images_keep_their_own_format() {
    // The page must know what format the pull command returns: does the engine widen RGB to RGBA?
    let mut t = raw_terminal(20, 5, (8, 16), 1 << 20);
    t.feed(&apc("a=T,f=24,s=2,v=1,i=4,c=2,r=1", &[10, 20, 30, 40, 50, 60]));
    let graphics = t.terminal.kitty_graphics().unwrap();
    let image = graphics.image(4).expect("image 4");
    let format = image.format().unwrap();
    let data = image.data().unwrap().expect("pixels").to_vec();
    eprintln!("FACT RGB: f=24 is stored as {format:?} with {} bytes for 2 pixels", data.len());
    match format {
        ImageFormat::Rgb => assert_eq!(data, [10, 20, 30, 40, 50, 60]),
        ImageFormat::Rgba => assert_eq!(data, [10, 20, 30, 255, 40, 50, 60, 255]),
        other => panic!("unexpected stored format {other:?}"),
    }
}

#[test]
fn cell_px_decides_the_pixel_size_and_follows_resize() {
    let mut t = raw_terminal(20, 5, (8, 16), 1 << 20);
    t.feed(&transmit_rgba(1, 1, "i=3,c=3,r=2", &[1, 2, 3, 255]));
    let (_, placed) = t.placements();
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0].grid, (3, 2));
    assert_eq!(placed[0].pixel, (24, 32), "c=3,r=2 at 8x16 px per cell");
    let before = t.placements().0;

    t.resize(20, 5, (10, 20));
    let (after, placed) = t.placements();
    assert_eq!(placed.len(), 1, "the placement survives a resize");
    assert_eq!(placed[0].pixel, (30, 40), "the same placement after resize to 10x20 px per cell");
    assert_ne!(after, before, "a resize that changes the cell size changes the list generation");

    // A terminal that never got a cell size (cell_px (0, 0), what an engine has before the page reports one) stores
    // the image but lists nothing to draw.
    let mut t = raw_terminal(20, 5, (0, 0), 1 << 20);
    t.feed(&transmit_rgba(1, 1, "i=3,c=1,r=1", &[1, 2, 3, 255]));
    assert!(t.has_image(3));
    assert!(t.placements().1.is_empty(), "no cell size, no placement list");
    t.resize(20, 5, (8, 16));
    assert_eq!(t.placements().1.len(), 1, "the list appears when the cell size arrives");
    eprintln!("FACT Cell px: resize(cols, rows, cell_w, cell_h) gives whole-cell pixel sizes (24x32 at 8x16, 30x40 at 10x20); with (0,0) the list is empty until a size arrives");
}

// ---------------------------------------------------------------------------------------------------------------
// Media and limits.
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn media_limits_read_back_as_refused_on_a_fresh_terminal() {
    let t = raw_terminal(20, 5, (8, 16), 1 << 20);
    assert!(!t.terminal.is_kitty_image_from_file_allowed().unwrap());
    assert_eq!(t.terminal.kitty_image_temp_file_dir().unwrap(), None);
    assert!(!t.terminal.is_kitty_image_from_shared_mem_allowed().unwrap());
    assert_eq!(t.terminal.kitty_image_storage_limit().unwrap(), 1 << 20);
}

#[test]
fn file_temp_and_shared_memory_media_are_refused_with_an_error_reply() {
    // A real file that the test made, with known bytes: if the engine read it, the image would be stored.
    let scratch = Scratch::new("media");
    let file = scratch.0.join("pixels.rgba");
    std::fs::write(&file, [1u8, 2, 3, 255]).unwrap();
    let path = file.to_str().expect("a UTF-8 temporary path").as_bytes().to_vec();
    for medium in ["f", "t", "s"] {
        let mut t = raw_terminal(20, 5, (8, 16), 1 << 20);
        t.feed(&apc(&format!("a=T,t={medium},f=32,s=1,v=1,i=9,c=1,r=1"), &path));
        let reply = t.take_replies();
        assert_eq!(reply, "\\x1b_Gi=9;EINVAL: unsupported medium\\x1b\\\\", "t={medium}");
        assert!(!t.has_image(9), "t={medium} must not store an image");
        assert!(t.placements().1.is_empty(), "t={medium} must not place anything");
    }
    assert!(file.exists(), "the file is untouched (a t=t read would delete it)");
    eprintln!("FACT Media: fresh terminal reads back file=false, temp_dir=None, shared_mem=false; t=f, t=t and t=s are each answered EINVAL: unsupported medium and store no image");
}

#[test]
fn the_storage_limit_refuses_an_oversize_image_and_zero_disables() {
    let mut t = raw_terminal(20, 5, (8, 16), 1000);
    t.feed(&transmit_rgba(20, 20, "i=3", &seeded_pixels(1, 400, 4))); // 1600 bytes against a limit of 1000
    assert_eq!(t.take_replies(), "\\x1b_Gi=3;ENOMEM: out of memory\\x1b\\\\");
    assert!(!t.has_image(3));

    let mut t = raw_terminal(20, 5, (8, 16), 0);
    t.feed(&transmit_rgba(1, 1, "i=3", &[1, 2, 3, 4]));
    assert!(!t.has_image(3), "limit 0 disables the protocol");
    assert_eq!(t.take_replies(), "", "and a disabled terminal does not answer");
}

// ---------------------------------------------------------------------------------------------------------------
// The other builders, proved against the engine.
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn chunked_and_zlib_transmissions_store_the_same_pixels() {
    let big = seeded_pixels(3, 64 * 64, 4); // 16 KiB: 6 pieces of 4096 characters
    let mut t = raw_terminal(20, 5, (8, 16), 1 << 22);
    let pieces = chunked("a=T,f=32,s=64,v=64,i=21,c=4,r=2", &big, CHUNK_CHARS);
    assert_eq!(pieces.len(), 6);
    for piece in &pieces {
        t.feed(piece);
    }
    assert_eq!(t.take_replies(), "\\x1b_Gi=21;OK\\x1b\\\\", "one reply, after the last piece");
    assert_eq!(t.image_pixels(21).expect("chunked image").0, big);

    t.feed(&apc("a=T,f=32,s=64,v=64,i=22,o=z,c=4,r=2", &zlib_stored(&big)));
    assert_eq!(t.take_replies(), "\\x1b_Gi=22;OK\\x1b\\\\");
    assert_eq!(t.image_pixels(22).expect("zlib image").0, big, "the engine inflates o=z before it stores");
}

#[test]
fn place_and_delete_builders_work() {
    let mut t = raw_terminal(20, 5, (8, 16), 1 << 20);
    t.feed(&apc("a=t,f=32,s=1,v=1,i=11", &[1, 2, 3, 4]));
    assert!(t.placements().1.is_empty(), "a=t only stores");
    t.feed(&place(11, "c=2,r=2,p=4"));
    let (_, placed) = t.placements();
    assert_eq!(placed.len(), 1);
    assert_eq!((placed[0].placement_id, placed[0].grid, placed[0].pixel), (Some(4), (2, 2), (16, 32)));
    t.feed(&delete("d=i,i=11,p=4"));
    assert!(t.placements().1.is_empty(), "d=i removes the placement");
    assert!(t.has_image(11), "and keeps the image data (lower-case d)");
    t.feed(&delete("d=I,i=11"));
    assert!(!t.has_image(11), "upper-case d frees the data too");
}

#[test]
fn animation_builders_play_frames_on_the_tick() {
    let mut t = raw_terminal(20, 5, (8, 16), 1 << 20);
    let (red, green, blue) = ([255u8, 0, 0, 255], [0u8, 255, 0, 255], [0u8, 0, 255, 255]);
    t.feed(&transmit_rgba(1, 1, "i=5,c=1,r=1", &red));
    t.feed(&animation_frame(5, 1, 1, "z=40", &green));
    t.feed(&animation_frame(5, 1, 1, "z=40", &blue));
    let replies = t.take_replies();
    assert_eq!(replies, "\\x1b_Gi=5;OK\\x1b\\\\\\x1b_Gi=5,r=2;OK\\x1b\\\\\\x1b_Gi=5,r=3;OK\\x1b\\\\");

    // Frames alone do not animate: the control command starts the loop.
    assert_eq!(t.terminal.kitty_graphics_animation_tick(0).unwrap(), None, "nothing plays before a=a,s=3");
    t.feed(&animation_control(5, "s=3,v=1"));
    let mut seen = Vec::new();
    let mut due = Vec::new();
    for now in [0u64, 10, 50, 100, 150] {
        due.push(t.terminal.kitty_graphics_animation_tick(now).unwrap());
        let graphics = t.terminal.kitty_graphics().unwrap();
        seen.push(graphics.image(5).unwrap().data().unwrap().unwrap().to_vec());
    }
    // The root frame has gap 0 and is skipped; the loop alternates the two frames that carry a gap of 40 ms.
    assert_eq!(seen, [green, green, blue, green, blue].map(|c| c.to_vec()));
    assert_eq!(due, [Some(40), Some(40), Some(90), Some(140), Some(190)], "the next due time is a plain ms value");
    eprintln!("FACT Animation: a=f frames do not play until a=a,s=3; the first tick steps to frame 2, one frame per call, next_due = now + gap");
}

// ---------------------------------------------------------------------------------------------------------------
// Unicode placeholders.
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn placeholder_images_are_listed_one_entry_per_run_per_row() {
    let mut t = raw_terminal(20, 5, (8, 16), 1 << 20);
    let rgba = seeded_pixels(2, 4, 4);
    t.feed(&apc("a=T,f=32,s=2,v=2,i=9,U=1,c=4,r=2", &rgba));
    assert_eq!(t.take_replies(), "\\x1b_Gi=9;OK\\x1b\\\\");
    assert!(t.placements().1.is_empty(), "the virtual placement alone draws nothing: it needs placeholder cells");

    t.feed(&placeholder_row(9, 0, 4));
    t.feed(b"\r\n");
    t.feed(&placeholder_row(9, 1, 4));
    let (_, placed) = t.placements();
    assert_eq!(placed.len(), 2, "one entry per row for a 4x2 block of neighbouring cells: {placed:?}");
    let mut rows = placed.clone();
    rows.sort_by_key(|p| p.row);
    for (i, p) in rows.iter().enumerate() {
        assert_eq!(p.image_id, 9);
        assert_eq!(p.kind, Some(KittyPlacementKind::Virtual), "is_virtual");
        assert_eq!((p.layer, p.z), (ImageLayer::BelowText, -1), "placeholders are always below the text");
        assert_eq!((p.col, p.row), (0, i as i32));
        assert_eq!(p.grid, (4, 1), "a run is four cells wide and one row high");
        assert_eq!(p.pixel, (32, 16));
        assert_eq!(p.source, (0, i as u32, 2, 1), "row {i} shows the {i}th half of the 2x2 image");
    }
    assert_eq!(rows[0].generation, rows[1].generation, "both rows are the same image");

    // Two runs on one row (a plain character between them) are two entries: one per run of neighbouring cells.
    let mut t = raw_terminal(20, 5, (8, 16), 1 << 20);
    t.feed(&apc("a=T,f=32,s=2,v=2,i=9,U=1,c=4,r=2", &rgba));
    t.feed(&placeholder_row(9, 0, 2));
    t.feed(b" ");
    t.feed(&placeholder_row(9, 0, 2));
    let (_, placed) = t.placements();
    let mut runs: Vec<_> = placed.iter().map(|p| (p.col, p.row, p.grid)).collect();
    runs.sort_unstable();
    assert_eq!(runs, [(0, 0, (2, 1)), (3, 0, (2, 1))], "{placed:?}");
    eprintln!("FACT Placeholders: one render-state entry per run of neighbouring placeholder cells per row, kind Virtual, layer BelowText, z -1, src rect = the row's slice of the image");
}

// ---------------------------------------------------------------------------------------------------------------
// OSC 8.
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn osc8_marks_exactly_the_link_cells_and_returns_their_uri() {
    let mut t = raw_terminal(40, 4, (8, 16), 1 << 20);
    t.feed(b"ab");
    t.feed(&osc8("https://example.org/a", "link"));
    t.feed(b"after");
    let uri = "https://example.org/a";
    for x in 0..12u16 {
        let gr = t.terminal.grid_ref(Raw::active(x, 0)).unwrap();
        let linked = (2..6).contains(&x);
        assert_eq!(gr.cell().unwrap().has_hyperlink().unwrap(), linked, "cell {x}");
        let mut buf = [0u8; 64];
        let n = gr.hyperlink_uri(&mut buf).unwrap();
        if linked {
            assert_eq!(&buf[..n], uri.as_bytes(), "cell {x}");
        } else {
            assert_eq!(n, 0, "a cell without a link returns Ok(0): cell {x}");
        }
    }
    let row = t.terminal.grid_ref(Raw::active(0, 0)).unwrap().row().unwrap();
    assert!(row.has_hyperlink().unwrap(), "the row with a link reports it");
    let row = t.terminal.grid_ref(Raw::active(0, 1)).unwrap().row().unwrap();
    assert!(!row.has_hyperlink().unwrap(), "an untouched row reports none");
}

#[test]
fn osc8_buffer_too_small_and_the_uri_length_the_engine_keeps() {
    let mut t = raw_terminal(40, 4, (8, 16), 1 << 20);
    t.feed(&osc8("https://example.org/a", "link"));
    let gr = t.terminal.grid_ref(Raw::active(1, 0)).unwrap();
    let mut small = [0u8; 4];
    assert!(
        matches!(gr.hyperlink_uri(&mut small), Err(Error::OutOfSpace { required: 21 })),
        "a 4 byte buffer is OutOfSpace and names the 21 bytes of the URI"
    );

    // The engine drops a link whose URI it does not keep (the OSC string is bounded): the cell is plain text.
    let mut longest_kept = 0;
    for len in 2040..=2048usize {
        t.feed(b"\x1b[2J\x1b[H");
        let uri = format!("https://e.org/{}", "a".repeat(len - 14));
        assert_eq!(uri.len(), len);
        t.feed(&osc8(&uri, "L"));
        let gr = t.terminal.grid_ref(Raw::active(0, 0)).unwrap();
        let linked = gr.cell().unwrap().has_hyperlink().unwrap();
        let mut big = vec![0u8; 8192];
        let n = gr.hyperlink_uri(&mut big).unwrap();
        assert_eq!(n, if linked { len } else { 0 }, "kept in full or not at all, never truncated (len {len})");
        if linked {
            longest_kept = len;
        }
    }
    assert_eq!(longest_kept, 2046, "the longest URI the engine keeps");
    eprintln!("FACT OSC 8 length: the engine keeps a URI of at most {longest_kept} bytes whole and drops a longer one (no link, no truncation)");
}

#[test]
fn osc8_row_flag_follows_the_link_cells_in_the_swept_cases() {
    // `Row::has_hyperlink` is documented as "may have false positives". Each case starts from a row whose first four
    // cells are a link, applies one edit at column 0 and compares the row flag with the truth, counted per cell.
    let cases: [(&str, &[u8]); 8] = [
        ("ECH 4", b"\x1b[4X"),
        ("EL 0", b"\x1b[K"),
        ("EL 1 (cell 0 only)", b"\x1b[1K"),
        ("EL 2", b"\x1b[2K"),
        ("DCH 4", b"\x1b[4P"),
        ("overwrite 2 of 4", b"xx"),
        ("overwrite 4 of 4", b"xxxx"),
        ("overwrite 6", b"xxxxxx"),
    ];
    let mut false_positives = Vec::new();
    for (name, edit) in cases {
        let mut t = raw_terminal(40, 4, (8, 16), 1 << 20);
        t.feed(&osc8("https://example.org/a", "link"));
        t.feed(b"\r");
        t.feed(edit);
        let truth =
            (0..40u16).any(|x| t.terminal.grid_ref(Raw::active(x, 0)).unwrap().cell().unwrap().has_hyperlink().unwrap());
        let flag = t.terminal.grid_ref(Raw::active(0, 0)).unwrap().row().unwrap().has_hyperlink().unwrap();
        if flag && !truth {
            false_positives.push(name);
        }
        assert!(!truth || flag, "{name}: a row with a link cell must report it (no false negative)");
    }
    eprintln!("FACT OSC 8 row flag: false positives in the 8 swept edits: {false_positives:?}");
    assert!(false_positives.is_empty(), "if this fails the engine plan must keep the per-cell check: {false_positives:?}");
}
