//! Scripted input for the Kitty graphics and OSC 8 tests.
//!
//! Include it from a test file with `mod kitty_support;`. It builds the escape sequences from plain arguments (so no
//! test writes its own base64 or APC framing), drives a raw `libghostty_vt::Terminal` with explicit limits, lists the
//! image placements of its render state, and reads the memory of the test process.
//!
//! Everything here is test code: it takes no input from outside the test, uses no clock and no random bytes (pixels
//! come from a fixed seed), and never panics inside a library callback (the reply sink only pushes into a buffer).
//! Each test crate uses a subset of the helpers, so dead code is allowed.
#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use libghostty_vt::alloc::Allocator;
use libghostty_vt::render::{ImageIterator, ImageLayer, KittyPlacementKind};
use libghostty_vt::terminal::{Point, PointCoordinate};
use libghostty_vt::{RenderState, Terminal};

/// The escape byte.
pub const ESC: u8 = 0x1b;

/// The size of one chunk of a chunked transmission, in base64 characters (the protocol's limit is 4096).
pub const CHUNK_CHARS: usize = 4096;

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding and no line breaks. A test helper only: the base64 of a Kitty payload is test input,
/// not production code.
pub fn base64(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        out.push(B64[usize::from(b[0] >> 2)]);
        out.push(B64[usize::from((b[0] & 0x03) << 4 | b[1] >> 4)]);
        out.push(if chunk.len() > 1 { B64[usize::from((b[1] & 0x0f) << 2 | b[2] >> 6)] } else { b'=' });
        out.push(if chunk.len() > 2 { B64[usize::from(b[2] & 0x3f)] } else { b'=' });
    }
    out
}

/// One Kitty graphics command: `ESC _ G control ; base64(payload) ESC \`. With an empty payload the `;` is left out
/// (place and delete commands carry none).
pub fn apc(control: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() * 4 / 3 + control.len() + 8);
    out.extend_from_slice(&[ESC, b'_', b'G']);
    out.extend_from_slice(control.as_bytes());
    if !payload.is_empty() {
        out.push(b';');
        out.extend_from_slice(&base64(payload));
    }
    out.extend_from_slice(&[ESC, b'\\']);
    out
}

/// The first command of a chunked transmission: the control keys plus `m=1` (or `m=0` when this is the only piece),
/// and one piece of already encoded base64 text.
pub fn chunk_first(control: &str, piece: &[u8], more: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(piece.len() + control.len() + 16);
    out.extend_from_slice(&[ESC, b'_', b'G']);
    out.extend_from_slice(control.as_bytes());
    out.extend_from_slice(if more { b",m=1;" } else { b",m=0;" });
    out.extend_from_slice(piece);
    out.extend_from_slice(&[ESC, b'\\']);
    out
}

/// A later command of a chunked transmission: only `m` and one piece of base64 text.
pub fn chunk_next(piece: &[u8], more: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(piece.len() + 16);
    out.extend_from_slice(&[ESC, b'_', b'G']);
    out.extend_from_slice(if more { b"m=1;" } else { b"m=0;" });
    out.extend_from_slice(piece);
    out.extend_from_slice(&[ESC, b'\\']);
    out
}

/// A transmission cut into commands of `chunk_chars` base64 characters (use [`CHUNK_CHARS`]): the control keys only
/// on the first, `m=1` on every piece but the last (`m=0`). `chunk_chars` must be a multiple of 4.
pub fn chunked(control: &str, payload: &[u8], chunk_chars: usize) -> Vec<Vec<u8>> {
    assert!(chunk_chars > 0 && chunk_chars.is_multiple_of(4), "a chunk is a whole number of base64 groups");
    let text = base64(payload);
    let pieces: Vec<&[u8]> = if text.is_empty() { vec![&text[..]] } else { text.chunks(chunk_chars).collect() };
    let last = pieces.len() - 1;
    pieces
        .iter()
        .enumerate()
        .map(|(i, piece)| if i == 0 { chunk_first(control, piece, i != last) } else { chunk_next(piece, i != last) })
        .collect()
}

/// The Adler-32 checksum that ends a zlib stream.
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}

/// A valid zlib stream made of stored (uncompressed) deflate blocks, so a compressed payload (`o=z`) can be built
/// without a crate. It does not shrink anything; a hostile probe gets its "bomb" by declaring a large size in the
/// keys, not by compression.
pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    }
    let blocks: Vec<&[u8]> = data.chunks(65535).collect();
    let last = blocks.len().wrapping_sub(1);
    for (i, block) in blocks.iter().enumerate() {
        out.push(u8::from(i == last));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Writes the bits of a deflate stream: values least significant bit first, Huffman codes most significant first.
struct Bits {
    out: Vec<u8>,
    acc: u64,
    used: u32,
}

impl Bits {
    fn put(&mut self, value: u32, count: u32) {
        self.acc |= u64::from(value) << self.used;
        self.used += count;
        while self.used >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.used -= 8;
        }
    }

    fn code(&mut self, code: u32, len: u32) {
        self.put(code.reverse_bits() >> (32 - len), len);
    }

    /// A literal or length symbol of the fixed Huffman table.
    fn symbol(&mut self, sym: u32) {
        match sym {
            0..=143 => self.code(0x30 + sym, 8),
            144..=255 => self.code(0x190 + sym - 144, 9),
            256..=279 => self.code(sym - 256, 7),
            _ => self.code(0xC0 + sym - 280, 8),
        }
    }

    /// A match of `len` (3..=258) bytes at distance 1.
    fn run(&mut self, len: u32) {
        const BASE: [u32; 29] = [
            3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195,
            227, 258,
        ];
        const EXTRA: [u32; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
        let idx = BASE.iter().rposition(|&b| b <= len).expect("a length of at least 3");
        self.symbol(257 + idx as u32);
        self.put(len - BASE[idx], EXTRA[idx]);
        self.code(0, 5); // distance code 0: distance 1
    }
}

/// The CRC-32 of a PNG chunk (the IEEE polynomial, bit by bit: small and slow, and only test input goes through it).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { crc >> 1 ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// One PNG chunk: length, type, data and the CRC of type plus data.
fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 12);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut checked = kind.to_vec();
    checked.extend_from_slice(data);
    out.extend_from_slice(&crc32(&checked).to_be_bytes());
    out
}

/// A PNG file of `width` by `height` pixels, 8-bit RGBA, with no filtering and a zlib stream of stored blocks. It is
/// made here so that a test never needs a PNG crate: the decoder under test is the engine's own.
pub fn png_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    assert_eq!(rgba.len(), width as usize * height as usize * 4, "one RGBA quadruple per pixel");
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 bits, colour type 6 (RGBA), deflate, filter method 0, no interlace
    let mut raw = Vec::with_capacity(rgba.len() + height as usize);
    for row in rgba.chunks(width as usize * 4) {
        raw.push(0); // filter type "none"
        raw.extend_from_slice(row);
    }
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    out.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&png_chunk(b"IDAT", &zlib_stored(&raw)));
    out.extend_from_slice(&png_chunk(b"IEND", &[]));
    out
}

/// The first bytes of a PNG that declare `width` by `height` pixels and nothing else: the signature and an IHDR chunk
/// with its CRC (so it looks valid to a parser that checks the chunk), and no pixel data at all.
pub fn png_header_only(width: u32, height: u32) -> Vec<u8> {
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    out.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    out
}

/// A real zlib stream that inflates to `n` zero bytes in about `n / 160` bytes (fixed Huffman, matches of 258 bytes at
/// distance 1). This is the decompression bomb of the probes; nothing here allocates `n` bytes.
pub fn zlib_zeros(n: u64) -> Vec<u8> {
    let mut bits = Bits { out: vec![0x78, 0x01], acc: 0, used: 0 };
    bits.put(1, 1); // final block
    bits.put(1, 2); // fixed Huffman codes
    if n > 0 {
        bits.symbol(0);
        let mut rest = n - 1;
        while rest >= 258 {
            bits.run(258);
            rest -= 258;
        }
        if rest >= 3 {
            bits.run(rest as u32);
        } else {
            for _ in 0..rest {
                bits.symbol(0);
            }
        }
    }
    bits.symbol(256);
    bits.put(0, 7); // pad the last byte
    // Adler-32 of zeros: a stays 1 and b counts the bytes.
    let adler = ((n % 65521) as u32) << 16 | 1;
    bits.out.extend_from_slice(&adler.to_be_bytes());
    bits.out
}

/// `pixels` from a fixed seed: `count` pixels of `bpp` bytes each, the same bytes on every run.
pub fn seeded_pixels(seed: u64, count: usize, bpp: usize) -> Vec<u8> {
    let mut x = if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed };
    (0..count * bpp)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// A transmit-and-display command for raw RGBA (`f=32`) with extra keys, for example `"i=7,c=2,r=1"`.
pub fn transmit_rgba(width: u32, height: u32, extra: &str, rgba: &[u8]) -> Vec<u8> {
    apc(&format!("a=T,f=32,s={width},v={height},{extra}"), rgba)
}

/// A place command for an image already stored: `a=p,i=<id>` plus extra keys.
pub fn place(image_id: u32, extra: &str) -> Vec<u8> {
    apc(&format!("a=p,i={image_id},{extra}"), &[])
}

/// A delete command: `a=d` plus the target keys, for example `delete("d=I,i=7")`.
pub fn delete(spec: &str) -> Vec<u8> {
    apc(&format!("a=d,{spec}"), &[])
}

/// An animation frame (`a=f`) for image `image_id` with raw RGBA pixels and extra keys (for example `"r=2,z=40"`).
pub fn animation_frame(image_id: u32, width: u32, height: u32, extra: &str, rgba: &[u8]) -> Vec<u8> {
    apc(&format!("a=f,i={image_id},f=32,s={width},v={height},{extra}"), rgba)
}

/// An animation control command (`a=a`) for image `image_id`, for example `animation_control(7, "s=3,v=1")`.
pub fn animation_control(image_id: u32, extra: &str) -> Vec<u8> {
    apc(&format!("a=a,i={image_id},{extra}"), &[])
}

/// An OSC 8 hyperlink around `text`: `ESC ] 8 ; ; uri ESC \ text ESC ] 8 ; ; ESC \`.
pub fn osc8(uri: &str, text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[ESC, b']']);
    out.extend_from_slice(b"8;;");
    out.extend_from_slice(uri.as_bytes());
    out.extend_from_slice(&[ESC, b'\\']);
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(&[ESC, b']']);
    out.extend_from_slice(b"8;;");
    out.extend_from_slice(&[ESC, b'\\']);
    out
}

/// The first 16 row and column diacritics of the protocol's table (the engine's table starts with these).
const DIACRITICS: [char; 16] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}', '\u{0346}',
    '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
];

/// One row of a Unicode-placeholder image: the foreground colour carries the image id (256-colour form below 256,
/// 24-bit form above), and every cell is U+10EEEE followed by the row and column diacritics. Row and columns are
/// below 16 (the helper's table).
pub fn placeholder_row(image_id: u32, row: usize, cols: usize) -> Vec<u8> {
    assert!(row < DIACRITICS.len() && cols <= DIACRITICS.len(), "the helper knows 16 diacritics");
    assert!(image_id < 1 << 24, "the high byte of the id would need a third diacritic");
    let mut s = if image_id < 256 {
        format!("\x1b[38;5;{image_id}m")
    } else {
        format!("\x1b[38;2;{};{};{}m", image_id >> 16 & 0xff, image_id >> 8 & 0xff, image_id & 0xff)
    };
    for col in DIACRITICS.iter().take(cols) {
        s.push('\u{10EEEE}');
        s.push(DIACRITICS[row]);
        s.push(*col);
    }
    s.push_str("\x1b[39m");
    s.into_bytes()
}

/// A raw terminal with its render state, an image-placement iterator and the sink for what the terminal writes back.
pub struct Raw {
    pub terminal: Terminal<'static, 'static>,
    state: RenderState<'static>,
    images: ImageIterator<'static>,
    /// Everything the terminal wrote back to the program (Kitty replies, size reports, ...), in order.
    pub replies: Rc<RefCell<Vec<u8>>>,
}

/// One entry of the render state's image list, as plain values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub image_id: u32,
    pub generation: u64,
    pub layer: ImageLayer,
    pub z: i32,
    pub col: i32,
    pub row: i32,
    pub grid: (u32, u32),
    pub cell_offset: (u32, u32),
    pub pixel: (u32, u32),
    pub source: (u32, u32, u32, u32),
    pub placement_id: Option<u32>,
    pub kind: Option<KittyPlacementKind>,
}

/// A raw terminal of `cols` by `rows` cells with the given cell size in pixels and Kitty storage limit in bytes. The
/// default limits of the library stay as they are: the three media are refused and one APC sequence may hold 65 MiB.
pub fn raw_terminal(cols: u16, rows: u16, cell_px: (u32, u32), storage_limit: u64) -> Raw {
    raw_from(Terminal::new(cols, rows).expect("terminal"), cols, rows, cell_px, storage_limit)
}

/// The same terminal, made with the given libghostty allocator (the probes pass [`counting_allocator`]).
pub fn raw_terminal_with_alloc(
    cols: u16,
    rows: u16,
    cell_px: (u32, u32),
    storage_limit: u64,
    alloc: &'static Allocator<'static>,
) -> Raw {
    raw_from(Terminal::new_with_alloc(alloc, cols, rows).expect("terminal"), cols, rows, cell_px, storage_limit)
}

/// The allocator of Rust's global allocator as a libghostty allocator, leaked so that it lives for the process. With a
/// counting `#[global_allocator]` in the test binary, every allocation of a terminal made with it is counted.
pub fn global_allocator() -> &'static Allocator<'static> {
    Box::leak(Box::new(Allocator::GLOBAL))
}

fn raw_from(
    mut terminal: Terminal<'static, 'static>,
    cols: u16,
    rows: u16,
    cell_px: (u32, u32),
    storage_limit: u64,
) -> Raw {
    terminal.set_kitty_image_storage_limit(storage_limit).expect("storage limit");
    terminal.resize(cols, rows, cell_px.0, cell_px.1).expect("resize");
    let replies: Rc<RefCell<Vec<u8>>> = Rc::default();
    let sink = replies.clone();
    // A callback inside the library: a plain push, no unwrap, no panic path.
    terminal
        .on_pty_write(move |_t, data| {
            if let Ok(mut buf) = sink.try_borrow_mut() {
                buf.extend_from_slice(data);
            }
        })
        .expect("pty sink");
    Raw {
        terminal,
        state: RenderState::new().expect("render state"),
        images: ImageIterator::new().expect("image iterator"),
        replies,
    }
}

impl Raw {
    /// Feed bytes to the terminal.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    /// Bound one APC sequence (`None` returns to the library's default of 65 MiB).
    pub fn set_apc_cap(&mut self, bytes: Option<usize>) {
        self.terminal.set_apc_max_bytes_kitty(bytes).expect("apc cap");
    }

    /// How many error replies the terminal has written back since the buffer was last taken (`;E` starts the text of
    /// every Kitty error reply: `EINVAL`, `ENOMEM`, ...). The replies stay in the buffer.
    pub fn error_replies(&self) -> usize {
        let buf = self.replies.borrow();
        buf.windows(2).filter(|w| w == b";E").count()
    }

    /// Resize the grid and the cell size together.
    pub fn resize(&mut self, cols: u16, rows: u16, cell_px: (u32, u32)) {
        self.terminal.resize(cols, rows, cell_px.0, cell_px.1).expect("resize");
    }

    /// The replies written back so far, taken (the buffer is empty afterwards), as text with escapes visible.
    pub fn take_replies(&mut self) -> String {
        let bytes = std::mem::take(&mut *self.replies.borrow_mut());
        bytes.escape_ascii().to_string()
    }

    /// The render state's image list: one entry per placement with something to draw, and the generation of the
    /// list. The order is the library's, not the drawing order.
    pub fn placements(&mut self) -> (u64, Vec<Placed>) {
        let snapshot = self.state.update(&self.terminal).expect("update");
        let generation = snapshot.image_placements_generation().expect("generation");
        let mut out = Vec::new();
        let mut it = self.images.update(&snapshot).expect("images");
        while let Some(p) = it.next() {
            out.push(Placed {
                image_id: p.image_id().expect("id"),
                generation: p.image_generation().expect("image generation"),
                layer: p.layer().expect("layer"),
                z: p.z().expect("z"),
                col: p.viewport_col().expect("col"),
                row: p.viewport_row().expect("row"),
                grid: (p.grid_cols().expect("cols"), p.grid_rows().expect("rows")),
                cell_offset: (p.cell_offset_x().expect("ox"), p.cell_offset_y().expect("oy")),
                pixel: (p.pixel_width().expect("pw"), p.pixel_height().expect("ph")),
                source: (
                    p.source_x().expect("sx"),
                    p.source_y().expect("sy"),
                    p.source_width().expect("sw"),
                    p.source_height().expect("sh"),
                ),
                placement_id: p.kitty_placement_id().expect("placement id"),
                kind: p.kitty_kind().expect("kind"),
            });
        }
        (generation, out)
    }

    /// The stored pixels of an image (a copy), its width and height, or `None` when no such image is stored.
    pub fn image_pixels(&self, id: u32) -> Option<(Vec<u8>, u32, u32)> {
        let graphics = self.terminal.kitty_graphics().ok()?;
        let image = graphics.image(id)?;
        let data = image.data().ok()??.to_vec();
        Some((data, image.width().ok()?, image.height().ok()?))
    }

    /// Whether an image with this id is stored (its pixels may still be pending).
    pub fn has_image(&self, id: u32) -> bool {
        self.terminal.kitty_graphics().ok().and_then(|g| g.image(id)).is_some()
    }

    /// The number of images in the storage of the active screen among ids `1..=max_id` (the storage has no count
    /// accessor, so a probe that knows its ids asks for them).
    pub fn stored_among(&self, max_id: u32) -> usize {
        (1..=max_id).filter(|&id| self.has_image(id)).count()
    }

    /// A point of the active area, for `grid_ref`.
    pub fn active(x: u16, y: u32) -> Point {
        Point::Active(PointCoordinate { x, y })
    }
}

/// The resident memory of this process in KiB, or `None` where it cannot be read. macOS: `ps -o rss= -p <pid>`;
/// Linux: the `VmRSS` line of `/proc/self/status`.
pub fn rss_kib() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
        line.split_whitespace().nth(1)?.parse().ok()
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Samples the resident memory of the process from a thread every 5 ms while a probe feeds a terminal, and keeps the
/// maximum. [`Sampler::finish`] stops the thread and takes one last sample.
pub struct Sampler {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<u64>>,
    baseline_kib: Option<u64>,
}

impl Sampler {
    /// Start sampling. The baseline is the resident memory at this moment.
    pub fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut peak = 0;
            loop {
                if let Some(kib) = rss_kib() {
                    peak = peak.max(kib);
                }
                if flag.load(Ordering::Relaxed) {
                    return peak;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Self { stop, handle: Some(handle), baseline_kib: rss_kib() }
    }

    /// Stop the thread. Returns `(baseline_kib, peak_kib)`, or `None` on a platform where memory cannot be read.
    pub fn finish(mut self) -> Option<(u64, u64)> {
        let last = rss_kib();
        self.stop.store(true, Ordering::Relaxed);
        let peak = self.handle.take()?.join().ok()?;
        let baseline = self.baseline_kib?;
        Some((baseline, peak.max(last?).max(baseline)))
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The 3072 bytes that one 4096-character chunk of a flood carries (a fixed pattern; the engine does not look at it).
const FLOOD_BLOCK: usize = 3072;

/// A slice of about 64 KiB of ready chunk commands for a flood: the first slice opens the transmission with `control`
/// (and `m=1`), every other slice only continues it. The caller feeds slices until it has sent enough; no slice is ever
/// larger than 64 KiB, so a probe never holds the payload it sends.
pub struct FloodSource {
    piece: Vec<u8>,
    first: bool,
    control: String,
    sent_payload: u64,
}

impl FloodSource {
    /// A flood that opens with `control`, for example `"a=T,f=32,s=5000,v=5000,i=1"`.
    pub fn new(control: &str) -> Self {
        let block = seeded_pixels(5, FLOOD_BLOCK / 4, 4);
        Self { piece: base64(&block), first: true, control: control.to_string(), sent_payload: 0 }
    }

    /// The next slice (15 chunk commands, 61 KiB or so), `more` always true: the flood never finishes a transmission.
    pub fn next_slice(&mut self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 * 1024);
        for _ in 0..15 {
            if self.first {
                out.extend_from_slice(&chunk_first(&self.control, &self.piece, true));
                self.first = false;
            } else {
                out.extend_from_slice(&chunk_next(&self.piece, true));
            }
            self.sent_payload += FLOOD_BLOCK as u64;
        }
        out
    }

    /// The payload bytes (decoded) sent so far.
    pub fn sent_payload(&self) -> u64 {
        self.sent_payload
    }
}
