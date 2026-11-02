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
    let mut terminal = Terminal::new(cols, rows).expect("terminal");
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
