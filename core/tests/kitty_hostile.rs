#![cfg(feature = "ghostty")]
//! Hostile Kitty graphics streams against the engine.
//!
//! Everything a program sends is untrusted. Each case builds an engine with a 16 MiB image limit and a cell size, feeds
//! the hostile bytes in slices of 64 KiB (no case holds more than that of what it sends), and then asserts three things:
//! the engine still answers `update` and echoes a plain line of text, the memory it holds for images stayed inside its
//! bound, and the images that are stored stay inside the limit. The replies a program can provoke stay inside the
//! engine's reply cap.
//!
//! The bound on memory has two forms. The count of the terminal's own allocator (`image_memory().peak`) is exact and is
//! asserted in both profiles: it never passes `image_memory_cap(limit)`. The resident memory of the process is asserted
//! in release builds only (a debug build measures more than the product does), against the number written in each case:
//! the growth that was measured with the case alone in its process (two runs, identical), plus 25 percent, and never less
//! than 4 MiB for the cases that cost nothing. The cases run one after another in one process and reuse the pages the
//! earlier ones freed, so a run of the whole file measures less than a case alone.

mod kitty_support;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use holzwave_core::images::{KITTY_APC_MAX_BYTES, MAX_DECODED_IMAGE_BYTES, image_memory_cap};
use holzwave_core::{GhosttyEngine, TerminalEngine};
use kitty_support::*;

const MIB: u64 = 1024 * 1024;

/// The image limit of every case (the setting's default is 64 MiB; a smaller engine makes the runs short and the bounds
/// tight).
const LIMIT: u64 = 16 * MIB;

/// The engine's cap on the replies it keeps for the PTY (`REPLY_CAP` in the engine), which is part of the contract.
const REPLY_CAP: usize = 64 * 1024;

/// The cases share the process-wide resident-memory sampler, so they run one at a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// One hostile case: an engine, the memory sampler, and the checks that every case ends with.
struct Case {
    name: &'static str,
    engine: GhosttyEngine,
    sampler: Sampler,
    /// How many reply bytes the engine has written back in all.
    replies: usize,
    /// The replies not looked at yet, as text; the case keeps at most 1 MiB of it.
    text: String,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl Case {
    fn new(name: &'static str) -> Self {
        Self::with_limit(name, LIMIT)
    }

    fn with_limit(name: &'static str, limit: u64) -> Self {
        let serial = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut engine = GhosttyEngine::with_image_limit(80, 24, 1000, limit).expect("engine");
        engine.resize(80, 24, (8, 16)).expect("resize");
        Self { name, engine, sampler: Sampler::start(), replies: 0, text: String::new(), _serial: serial }
    }

    /// Feed in slices of 64 KiB, as a pty delivers, and keep what the engine wrote back for the program.
    fn feed(&mut self, bytes: &[u8]) {
        for slice in bytes.chunks(64 * 1024) {
            self.feed_slice(slice);
        }
    }

    fn feed_slice(&mut self, slice: &[u8]) {
        self.engine.feed(slice);
        // A real pane takes the replies after every read; the cap is per read.
        let taken = self.engine.take_replies();
        assert!(taken.len() <= REPLY_CAP, "{}: {} reply bytes after one slice", self.name, taken.len());
        self.replies += taken.len();
        if self.text.len() < MIB as usize {
            self.text.push_str(&taken.escape_ascii().to_string());
        }
    }

    /// The replies the engine wrote back and the case has not looked at yet, as text.
    fn replies_text(&mut self) -> String {
        std::mem::take(&mut self.text)
    }

    /// The end of every case. `ids` are the image ids the case could have stored; `bound_mib` is the most the resident
    /// memory of the process may have grown (release builds).
    fn finish(mut self, ids: impl Iterator<Item = u32>, bound_mib: u64) -> Outcome {
        // 1. The engine still answers, and a plain line of text reaches the screen. An open transmission does not stop
        //    the text that comes after its terminator.
        self.engine.feed(b"\x1b\\\r\n");
        let marker = format!("alive-after-{}", self.name);
        self.engine.feed(format!("{marker}\r\n").as_bytes());
        self.engine.update().expect("the engine still answers update");
        let text = self.engine.screen().expect("screen").to_text();
        assert!(text.contains(&marker), "{}: the terminal no longer echoes text: {text:?}", self.name);

        // 2. Memory: the count of the terminal's allocator is exact; the resident set is measured in release builds.
        let memory = self.engine.image_memory();
        let limit = self.engine.image_limits().expect("limits").storage_bytes;
        assert_eq!(memory.cap, image_memory_cap(limit), "{}: the cap follows the limit", self.name);
        assert!(
            memory.peak <= memory.cap,
            "{}: the terminal held {} bytes for images, the cap is {}",
            self.name,
            memory.peak,
            memory.cap
        );
        let measured = self.sampler.finish();
        let growth_mib = measured.map(|(base, peak)| (peak - base).div_ceil(1024));
        println!(
            "HOSTILE {}: growth_mib={} counted_peak_mib={} refused={} bound_mib={bound_mib}",
            self.name,
            growth_mib.map_or("n/a".to_string(), |g| g.to_string()),
            memory.peak / MIB as usize,
            memory.refused
        );
        #[cfg(not(debug_assertions))]
        if let Some(growth) = growth_mib {
            assert!(growth <= bound_mib, "{}: the process grew by {growth} MiB, the bound is {bound_mib} MiB", self.name);
        }
        #[cfg(debug_assertions)]
        let _ = bound_mib;

        // 3. What is stored stays inside the limit.
        let (mut count, mut bytes) = (0usize, 0u64);
        for id in ids {
            if let Some(generation) = self.engine.image_generation(id) {
                count += 1;
                bytes += self.engine.image_chunk(id, generation, 0, 0).map_or(0, |c| c.total);
            }
        }
        assert!(bytes <= limit, "{}: {count} images of {bytes} bytes are stored, the limit is {limit}", self.name);
        Outcome { engine: self.engine, stored: count, replies: self.replies, memory_refused: memory.refused }
    }
}

struct Outcome {
    engine: GhosttyEngine,
    stored: usize,
    replies: usize,
    memory_refused: usize,
}

fn errors(text: &str) -> usize {
    text.matches(";E").count()
}

/// A start chunk with `control` and `chunks` continuation chunks of 4096 base64 characters, none of them the last, fed
/// one at a time so that the test itself holds no more than one chunk.
fn feed_endless(case: &mut Case, control: &str, chunks: usize) {
    let piece = base64(&seeded_pixels(5, 3072 / 4, 4));
    case.feed_slice(&chunk_first(control, &piece, true));
    let next = chunk_next(&piece, true);
    for _ in 0..chunks {
        case.feed_slice(&next);
    }
}

/// A transmission of `total` payload bytes in chunks of 3072 bytes (4096 characters), made and fed one chunk at a time so
/// that the test never holds the payload. Returns the 3072-byte block it repeats, to compare the pixels read back.
fn feed_chunked(case: &mut Case, control: &str, total: usize) -> Vec<u8> {
    let block = seeded_pixels(8, 768, 4);
    let mut sent = 0;
    while sent < total {
        let n = (total - sent).min(block.len());
        let piece = base64(&block[..n]);
        let more = sent + n < total;
        let chunk = if sent == 0 { chunk_first(control, &piece, more) } else { chunk_next(&piece, more) };
        case.feed_slice(&chunk);
        sent += n;
    }
    block
}

// ---------------------------------------------------------------------------------------------------------------
// Malformed transmissions: no allocation worth the name, an error for the program, nothing stored.
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn hostile_invalid_base64_is_refused() {
    let mut case = Case::new("invalid_base64");
    for payload in [&b"!!!!@@@@####"[..], b"AAAA====AAAA", b"A", b"AAAAA", b"\xff\xfe\xfd\xfc", b"AA\0AAAAA"] {
        case.feed(&chunk_first("a=T,f=32,s=1,v=1,i=1,q=0", payload, false));
    }
    let replies = case.replies_text();
    assert!(errors(&replies) >= 1, "bad base64 is answered with an error: {replies:?}");
    let out = case.finish(1..=4, 4);
    assert_eq!(out.stored, 0, "nothing is stored");
}

#[test]
fn hostile_a_payload_shorter_or_longer_than_the_declared_size_is_refused() {
    let mut case = Case::new("wrong_sizes");
    // 100 by 100 RGBA is 40 000 bytes: send 100 and then 50 000.
    case.feed(&apc("a=T,f=32,s=100,v=100,i=1", &seeded_pixels(1, 25, 4)));
    let short = case.replies_text();
    assert!(short.contains("i=1;E"), "too little data: {short:?}");
    case.feed(&apc("a=T,f=32,s=100,v=100,i=2", &seeded_pixels(2, 12_500, 4)));
    let long = case.replies_text();
    assert!(long.contains("i=2;E"), "too much data: {long:?}");
    // RGB the same way, and a size that makes the product overflow 32 bits.
    case.feed(&apc("a=T,f=24,s=100,v=100,i=3", &seeded_pixels(3, 100, 3)));
    case.feed(&apc("a=T,f=32,s=65536,v=65536,i=4", &seeded_pixels(4, 4, 4)));
    let rest = case.replies_text();
    assert_eq!(errors(&rest), 2, "{rest:?}");
    let out = case.finish(1..=4, 4);
    assert_eq!(out.stored, 0);
}

#[test]
fn hostile_huge_dimensions_cost_nothing() {
    let mut case = Case::new("huge_dimensions");
    for (w, h) in [(100_000u64, 100_000u64), (10_001, 1), (1, 10_001), (4_294_967_295, 4_294_967_295), (0, 5), (5, 0)] {
        case.feed(&apc(&format!("a=T,f=32,s={w},v={h},i=1"), &seeded_pixels(1, 4, 4)));
    }
    case.feed(&apc("a=T,f=32,s=99999999999999999999,v=1,i=1", &seeded_pixels(1, 4, 4)));
    let replies = case.replies_text();
    assert!(errors(&replies) >= 6, "{replies:?}");
    let out = case.finish(1..=2, 4);
    assert_eq!(out.stored, 0);
}

#[test]
fn hostile_negative_and_non_numeric_keys_do_not_break_the_parser() {
    let mut case = Case::new("bad_keys");
    for control in [
        "a=T,f=32,s=-5,v=2,i=1",
        "a=T,f=32,s=abc,v=2,i=1",
        "a=T,f=32,s=1,v=1,i=-1",
        "a=T,f=32,s=1,v=1,i=1,c=x,r=y",
        "a=T,f=-1,s=1,v=1,i=1",
        "a=T,f=999,s=1,v=1,i=1",
        "a=Z,f=32,s=1,v=1,i=1",
        "a=T,t=q,f=32,s=1,v=1,i=1",
        "a=T,f=32,s=1,v=1,i=1,z=99999999999",
        "a=T,f=32,s=1,v=1,i=1,x=-1,y=-1,w=99999,h=99999",
        "=,=,=,,,,",
        "a",
        "a=,f=,s=,v=",
        "i=1,i=2,i=3,i=4,a=T,a=p,a=d",
    ] {
        case.feed(&apc(control, &seeded_pixels(1, 1, 4)));
    }
    // Control bytes where a key goes, and a key that is a megabyte long.
    case.feed(&apc("a=T,f=32,s=1,v=1,\0\x01\x02=\u{7f},i=1", &seeded_pixels(1, 1, 4)));
    case.feed(&apc(&format!("a=T,f=32,s=1,v=1,{}=1,i=1", "k".repeat(1 << 20)), &seeded_pixels(1, 1, 4)));
    let out = case.finish(0..=4, 7);
    assert!(out.stored <= 4);
}

#[test]
fn hostile_the_largest_ids_are_plain_numbers() {
    let mut case = Case::new("largest_ids");
    case.feed(&apc("a=T,f=32,s=1,v=1,i=4294967295,p=4294967295,c=1,r=1", &seeded_pixels(1, 1, 4)));
    // One past the largest id, and an id that does not fit any integer: errors, not wrap-arounds to an existing id.
    case.feed(&apc("a=T,f=32,s=1,v=1,i=4294967296,c=1,r=1", &seeded_pixels(2, 1, 4)));
    case.feed(&apc("a=T,f=32,s=1,v=1,i=18446744073709551616,c=1,r=1", &seeded_pixels(3, 1, 4)));
    case.feed(&place(4_294_967_295, "p=4294967295,c=1,r=1"));
    case.feed(&delete("d=I,i=4294967296"));
    let max = u32::MAX;
    assert!(case.engine.image_generation(max).is_some(), "the largest id is a valid id");
    let out = case.finish([max, 0, 1, 2, 3].into_iter(), 4);
    assert_eq!(out.stored, 1, "only the image with the largest id: no wrap-around stored another");
}

// ---------------------------------------------------------------------------------------------------------------
// Transmissions that never end, change or have no start.
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn hostile_a_start_chunk_followed_by_5000_chunks_that_never_end() {
    let mut case = Case::new("endless_small_declared");
    // The start declares 100 by 100 (40 000 bytes); 5000 chunks are 15 MB: the library must give up.
    feed_endless(&mut case, "a=T,f=32,s=100,v=100,i=1", 5000);
    // The library does not compare the data with the declared size while chunks keep coming: it holds them (15 MB here,
    // inside the cap) and says nothing. The memory is the open transmission's, and it is freed when the program closes
    // it, which is then answered with an error because the data is not 100 by 100 pixels.
    assert_eq!(case.replies_text(), "", "nothing is said while the transmission stays open");
    let held = case.engine.image_memory().live;
    assert!(held >= 14 * MIB as usize, "the open transmission is what holds the memory: {held}");
    assert!(held < 40 * MIB as usize, "inside the cap, and a small multiple of what was sent: {held}");
    case.feed(&chunk_next(b"", false));
    let closing = case.replies_text();
    assert!(closing.contains(";E"), "closing a transmission whose data is the wrong size is an error: {closing:?}");
    assert!(case.engine.image_memory().live < MIB as usize, "and the memory is free again");
    let out = case.finish(1..=4, 59);
    assert_eq!(out.stored, 0, "an open transmission never became an image");
}

#[test]
fn hostile_a_start_chunk_that_declares_100_mib_followed_by_endless_chunks() {
    let mut case = Case::new("endless_declared_100_mib");
    // The flood of the memory spike: declared 5000 by 5000 (100 MB), 5000 chunks of 3 KiB (15 MB) per round, three rounds
    // of continuation. With the library's own bound only, this reached 400 MiB.
    feed_endless(&mut case, "a=T,f=32,s=5000,v=5000,i=1", 5000 * 3);
    let out = case.finish(1..=4, 189);
    assert_eq!(out.stored, 0);
    assert!(out.replies > 0 || out.memory_refused == 0);
}

#[test]
fn hostile_a_continuation_that_changes_a_key_is_not_a_second_image() {
    let mut case = Case::new("changed_key");
    let piece = base64(&seeded_pixels(5, 3072 / 4, 4));
    case.feed(&chunk_first("a=T,f=32,s=100,v=100,i=1", &piece, true));
    for i in 0..2000 {
        // The continuation names another image and another action.
        let control = if i % 2 == 0 { "i=2,a=p,m=1" } else { "i=3,f=24,s=9,v=9,m=1" };
        let mut chunk = vec![ESC, b'_', b'G'];
        chunk.extend_from_slice(control.as_bytes());
        chunk.push(b';');
        chunk.extend_from_slice(&piece);
        chunk.extend_from_slice(&[ESC, b'\\']);
        case.feed(&chunk);
    }
    let out = case.finish(1..=4, 13);
    assert_eq!(out.stored, 0, "the continuations did not become images of their own");
}

#[test]
fn hostile_chunks_without_a_start_are_errors() {
    let mut case = Case::new("no_start");
    let piece = base64(&seeded_pixels(5, 3072 / 4, 4));
    let mut stream = Vec::new();
    for _ in 0..3000 {
        stream.extend_from_slice(&chunk_next(&piece, true));
    }
    stream.extend_from_slice(&chunk_next(&piece, false));
    case.feed(&stream);
    let out = case.finish(0..=4, 54);
    assert_eq!(out.stored, 0);
}

// ---------------------------------------------------------------------------------------------------------------
// Compressed and PNG data that lies about its size.
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn hostile_a_zlib_bomb_is_refused_inside_the_bound() {
    let mut case = Case::new("zlib_bomb");
    // 2.5 MB of deflate that inflates to 400 MB, declared as 10000 by 10000.
    let bomb = zlib_zeros(400_000_000);
    for piece in chunked("a=T,f=32,s=10000,v=10000,o=z,i=1", &bomb, CHUNK_CHARS) {
        case.feed_slice(&piece);
    }
    let replies = case.replies_text();
    assert!(replies.contains("i=1;E"), "the bomb is answered with an error: {replies:?}");
    let out = case.finish(1..=2, 22);
    assert_eq!(out.stored, 0);
}

#[test]
fn hostile_a_png_that_declares_gigabytes_is_refused_from_its_header() {
    let mut case = Case::new("png_gigabytes");
    for (w, h) in [(30_000, 30_000), (10_000, 10_000), (u32::MAX, u32::MAX), (0, 0), (10_001, 1)] {
        case.feed(&apc("a=T,f=100,i=1", &png_header_only(w, h)));
    }
    assert!(MAX_DECODED_IMAGE_BYTES < 400 * MIB, "the header check is tighter than the library's 400 MiB");
    let replies = case.replies_text();
    assert_eq!(replies.matches("i=1;E").count(), 5, "{replies:?}");
    let out = case.finish(1..=2, 4);
    assert_eq!(out.stored, 0);
}

#[test]
fn hostile_a_png_with_a_good_header_and_a_truncated_body_is_refused() {
    let mut case = Case::new("png_truncated");
    let good = png_rgba(64, 64, &seeded_pixels(1, 64 * 64, 4));
    for keep in [24, 33, 60, good.len() / 2, good.len() - 12, good.len() - 1] {
        case.feed(&apc("a=T,f=100,i=1", &good[..keep]));
    }
    let replies = case.replies_text();
    assert_eq!(replies.matches("i=1;E").count(), 6, "{replies:?}");
    // And the whole file is fine, so the refusals were about the bytes.
    case.feed(&apc("a=T,f=100,i=2,c=1,r=1", &good));
    assert!(case.replies_text().contains("i=2;OK"));
    let out = case.finish(1..=2, 4);
    assert_eq!(out.stored, 1);
}

#[test]
fn hostile_a_png_whose_ihdr_length_is_wrong_is_refused() {
    let mut case = Case::new("png_bad_ihdr");
    let good = png_rgba(8, 8, &seeded_pixels(1, 64, 4));
    for length in [0u32, 12, 14, 0x7fff_ffff, u32::MAX] {
        let mut bad = good.clone();
        bad[8..12].copy_from_slice(&length.to_be_bytes());
        case.feed(&apc("a=T,f=100,i=1", &bad));
    }
    // A wrong chunk type, a wrong signature, a PNG that is only its signature, and noise of PNG length.
    let mut bad = good.clone();
    bad[12..16].copy_from_slice(b"IDAT");
    case.feed(&apc("a=T,f=100,i=1", &bad));
    let mut bad = good.clone();
    bad[1] = b'X';
    case.feed(&apc("a=T,f=100,i=1", &bad));
    case.feed(&apc("a=T,f=100,i=1", &good[..8]));
    case.feed(&apc("a=T,f=100,i=1", &seeded_pixels(7, 1000, 4)));
    let replies = case.replies_text();
    assert_eq!(replies.matches("i=1;E").count(), 9, "{replies:?}");
    let out = case.finish(1..=2, 4);
    assert_eq!(out.stored, 0);
}

// ---------------------------------------------------------------------------------------------------------------
// Counts: thousands of images, a million queries.
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn hostile_twenty_thousand_one_pixel_images() {
    let mut case = Case::new("many_images");
    let mut stream = Vec::new();
    for id in 1..=20_000u32 {
        stream.extend_from_slice(&transmit_rgba(1, 1, &format!("i={id},q=2"), &[1, 2, 3, 4]));
        if stream.len() >= 60 * 1024 {
            case.feed_slice(&std::mem::take(&mut stream));
        }
    }
    case.feed_slice(&stream);
    let out = case.finish(1..=20_000, 19);
    // 20 000 images of 4 bytes are 80 KB, far inside 16 MiB: all of them are kept, and the count is the only cost.
    assert_eq!(out.stored, 20_000);

    // With a limit that 20 000 images pass, the oldest go and the newest stay.
    let mut case = Case::with_limit("many_images_evicting", 64 * 1024);
    let mut stream = Vec::new();
    for id in 1..=20_000u32 {
        stream.extend_from_slice(&transmit_rgba(1, 1, &format!("i={id},q=2"), &[1, 2, 3, 4]));
        if stream.len() >= 60 * 1024 {
            case.feed_slice(&std::mem::take(&mut stream));
        }
    }
    case.feed_slice(&stream);
    let engine = &case.engine;
    assert!(engine.image_generation(1).is_none(), "the oldest image is gone");
    assert!(engine.image_generation(20_000).is_some(), "the newest stays");
    let kept = (1..=20_000u32).filter(|&id| engine.image_generation(id).is_some()).count();
    assert!(kept <= 64 * 1024 / 4, "{kept} images of 4 bytes are inside 64 KiB");
    assert!(kept >= 1000, "eviction takes the oldest first, it does not empty the storage: {kept}");
    let out = case.finish(1..=20_000, 19);
    assert!(out.stored >= 1000);
}

#[test]
fn hostile_a_million_queries_in_one_feed_leave_the_reply_buffer_inside_its_cap() {
    let mut case = Case::new("million_queries");
    let unit = apc("a=q,i=1,s=1,v=1,f=32", &[0, 0, 0, 0]);
    let per_slice = (60 * 1024) / unit.len();
    let slice: Vec<u8> = unit.iter().copied().cycle().take(per_slice * unit.len()).collect();
    let rounds = 1_000_000usize.div_ceil(per_slice);
    let started = Instant::now();
    for _ in 0..rounds {
        // One read of the pty: the engine keeps at most the cap of replies for it, however many queries it holds.
        case.engine.feed(&slice);
        let taken = case.engine.take_replies();
        assert!(taken.len() <= REPLY_CAP, "{} reply bytes from one read", taken.len());
    }
    let elapsed = started.elapsed();
    println!("HOSTILE million_queries: {} queries in {elapsed:?}", rounds * per_slice);
    // The bound that the summary states: 20 seconds on any machine this runs on; the Mac needs about a second in release.
    assert!(elapsed < Duration::from_secs(20), "a million queries took {elapsed:?}");
    let out = case.finish(1..=2, 4);
    assert_eq!(out.stored, 0, "a query stores nothing");
}

// ---------------------------------------------------------------------------------------------------------------
// The medium and the size of one sequence.
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn hostile_files_named_by_a_program_are_not_read() {
    let mut case = Case::new("media_paths");
    for (path, id) in [("/etc/passwd", 1), ("/etc/hosts", 2), ("../../../../etc/passwd", 3), ("/dev/zero", 4), ("/dev/random", 5)] {
        for medium in ["f", "t", "s"] {
            case.feed(&apc(&format!("a=T,t={medium},f=32,s=2,v=2,i={id}"), path.as_bytes()));
        }
    }
    let replies = case.replies_text();
    assert_eq!(errors(&replies), 15, "every attempt is answered with an error: {replies:?}");
    let out = case.finish(1..=5, 4);
    assert_eq!(out.stored, 0);
}

#[test]
fn hostile_an_escape_sequence_one_byte_over_the_cap_is_dropped() {
    let mut case = Case::new("apc_over_cap");
    // One command whose base64 text is KITTY_APC_MAX_BYTES + 1 characters long, streamed in pieces of 64 KiB.
    let block = base64(&seeded_pixels(6, 12 * 1024, 4)); // 65 536 characters
    let mut head = vec![ESC, b'_', b'G'];
    head.extend_from_slice(b"a=T,f=32,s=1000,v=1000,i=1;");
    case.feed_slice(&head);
    let mut sent = 0;
    while sent + block.len() <= KITTY_APC_MAX_BYTES {
        case.feed_slice(&block);
        sent += block.len();
    }
    case.feed_slice(&block[..KITTY_APC_MAX_BYTES + 1 - sent]);
    case.feed_slice(&[ESC, b'\\']);
    let out = case.finish(1..=2, 34);
    assert_eq!(out.stored, 0, "the command was dropped and nothing was stored");
}

// ---------------------------------------------------------------------------------------------------------------
// The mitigation: the terminal's allocator.
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn the_allocator_refuses_a_flood_past_its_bound_and_the_terminal_survives() {
    let mut case = Case::new("allocator_flood");
    // Declared 5000 by 5000, 150 MB of chunks: far past the cap of 2 x 16 + 64 = 96 MiB.
    let mut flood = FloodSource::new("a=T,f=32,s=5000,v=5000,i=1");
    while flood.sent_payload() < 150_000_000 {
        case.feed_slice(&flood.next_slice());
    }
    let memory = case.engine.image_memory();
    assert_eq!(memory.cap, image_memory_cap(LIMIT));
    assert!(memory.refused >= 1, "the allocator said no at least once: {memory:?}");
    assert!(memory.peak <= memory.cap, "{memory:?}");
    let replies = case.replies_text();
    assert!(errors(&replies) >= 1 || case.replies > 0, "the program is told: {replies:?}");
    // The terminal survived: after a closing chunk a small valid image is accepted, and what the flood held is free.
    case.feed(&apc("m=0", &[]));
    let _ = case.replies_text();
    case.feed(&transmit_rgba(1, 1, "i=2,c=1,r=1", &[1, 2, 3, 4]));
    assert!(case.engine.image_generation(2).is_some(), "a small image is accepted after the refusal");
    let live = case.engine.image_memory().live;
    assert!(live < 8 * MIB as usize, "the flood's buffer was freed, {live} bytes are live");
    let out = case.finish(1..=2, 203);
    assert_eq!(out.stored, 1);
}

#[test]
fn an_image_just_under_the_limit_still_arrives_in_chunks_while_the_allocator_is_on() {
    let mut case = Case::new("allocator_legit");
    // 1983 by 1983 RGBA is 15.0 MiB, just under the 16 MiB limit; the library grows its buffer by half each time and holds
    // the old and the new one at once, which the cap leaves room for.
    let block = feed_chunked(&mut case, "a=T,f=32,s=1983,v=1983,i=1,c=10,r=5", 1983 * 1983 * 4);
    let replies = case.replies_text();
    assert!(replies.contains("i=1;OK"), "the image arrived: {:?}", &replies[replies.len().saturating_sub(80)..]);
    assert_eq!(case.engine.image_memory().refused, 0, "the allocator refused nothing");
    let generation = case.engine.image_generation(1).expect("stored");
    let chunk = case.engine.image_chunk(1, generation, 0, 16).expect("pixels");
    assert_eq!(chunk.total, 1983 * 1983 * 4);
    assert_eq!(chunk.bytes, block[..16]);
    let out = case.finish(1..=2, 78);
    assert_eq!(out.stored, 1);
}

#[test]
fn a_64_mib_image_arrives_in_4096_character_chunks_with_the_default_limit() {
    // The limit that the setting defaults to: an image of exactly 64 MiB (4096 by 4096 RGBA) arrives whole, one
    // 4096-character chunk at a time, and the allocator refuses nothing.
    let mut case = Case::with_limit("allocator_legit_default", 64 * MIB);
    let block = feed_chunked(&mut case, "a=T,f=32,s=4096,v=4096,i=1,c=10,r=5", 4096 * 4096 * 4);
    let replies = case.replies_text();
    assert!(replies.contains("i=1;OK"), "the image arrived: {:?}", &replies[replies.len().saturating_sub(80)..]);
    let memory = case.engine.image_memory();
    assert_eq!(memory.refused, 0, "{memory:?}");
    assert!(memory.peak <= memory.cap && memory.peak > 64 * MIB as usize, "{memory:?}");
    let generation = case.engine.image_generation(1).expect("stored");
    assert_eq!(case.engine.image_chunk(1, generation, 0, 16).expect("pixels").bytes, block[..16]);
    let out = case.finish(1..=2, 362);
    assert_eq!(out.stored, 1);
}
