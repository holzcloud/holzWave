//! The limits of the Kitty graphics protocol and the one PNG decoder the engine installs.
//!
//! Everything a program sends through the protocol is untrusted: sizes it declares, base64 it encodes, zlib it
//! compresses and PNG bytes it sends. The parser, the chunking, the zlib inflate and the eviction are the library's
//! (libghostty-vt); this module holds the numbers that bound them and the decoder that is put in front of the library's
//! own PNG decoder, so that a header which claims gigabytes is refused before anything is allocated.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use allocator_api2::alloc::{AllocError, Allocator as RustAllocator, Global, Layout};
use libghostty_vt::alloc::Allocator;
use libghostty_vt::kitty::graphics::{DecodePng, DecodedImage, RustPngDecoder, set_png_decoder};

pub use crate::engine::MAX_PLACEMENTS;

/// The image memory of one engine when the caller names no limit: 64 MiB of decoded pixels per screen.
pub const DEFAULT_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// The most bytes a decoded PNG may take (`width * height * 4`). A PNG that declares more is refused from its header.
pub const MAX_DECODED_IMAGE_BYTES: u64 = 128 * 1024 * 1024;

/// The most pixels a PNG may have in either direction. The library caps a raw image at the same number.
pub const MAX_IMAGE_DIMENSION: u32 = 10_000;

/// The most bytes of one Kitty escape sequence the terminal buffers. The protocol cuts a transmission into pieces of
/// 4096 characters, so 8 MiB is two thousand times that, and a single hostile sequence costs at most this much (the
/// library's own default is 65 MiB).
pub const KITTY_APC_MAX_BYTES: usize = 8 * 1024 * 1024;

/// The smallest request that [`BoundedAlloc`] may refuse. Everything smaller is always granted, so a refusal only ever
/// hits the big buffers of a hostile stream (the data of an open transmission, an inflate, a PNG's pixels), never the
/// small bookkeeping of the terminal, whose failure paths are not the ones that were written for an image that does
/// not fit.
pub const REFUSE_FROM_BYTES: usize = 64 * 1024;

/// What one terminal may hold in live allocations while images arrive, as a function of its image limit: two screens
/// of stored images (the main and the alternate screen each keep the limit), plus 64 MiB of room for the image that is
/// being received. The room is what the library's growth needs: it grows the buffer of a transmission by half each
/// time and holds the old and the new buffer at once, so an image of the limit's size peaks at about 2.4 times its
/// size (measured: 140 MiB for a 58 MiB image), which the cap lets through.
pub fn image_memory_cap(image_bytes: u64) -> usize {
    let cap = image_bytes.saturating_mul(2).saturating_add(64 * 1024 * 1024);
    usize::try_from(cap).unwrap_or(usize::MAX)
}

/// The eight bytes every PNG file starts with.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// The width and height a PNG declares in its header, `None` when the bytes are not the start of an acceptable PNG:
/// the signature, then an IHDR chunk of length 13, then a size that is not zero, at most [`MAX_IMAGE_DIMENSION`] in
/// either direction, and whose decoded size stays within [`MAX_DECODED_IMAGE_BYTES`]. Reads 24 bytes and allocates
/// nothing, so it can run on any input.
pub fn check_png_header(data: &[u8]) -> Option<(u32, u32)> {
    let head = data.get(..24)?;
    if head[..8] != PNG_SIGNATURE || head[8..12] != 13u32.to_be_bytes() || &head[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(head[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(head[20..24].try_into().ok()?);
    if width == 0 || height == 0 || width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return None;
    }
    let decoded = u64::from(width).checked_mul(u64::from(height))?.checked_mul(4)?;
    (decoded <= MAX_DECODED_IMAGE_BYTES).then_some((width, height))
}

/// The PNG decoder of the engine: the header check in front of the library's own decoder (the `png` crate).
///
/// It never panics and returns `None` for anything it does not like: a panic inside the library's callback would abort
/// the whole process, and a refusal is only an error reply to the program.
#[derive(Clone, Copy, Debug, Default)]
pub struct BoundedPng;

impl DecodePng for BoundedPng {
    fn decode_png<'alloc>(&mut self, alloc: &'alloc Allocator<'_>, data: &[u8]) -> Option<DecodedImage<'alloc>> {
        check_png_header(data)?;
        // The library's decoder allocates its output through `alloc`, which the library bounds, and only after it has
        // read the header again. The unwind guard is a second line: nothing in it should panic, and a panic here must
        // not become an abort of the process.
        catch_unwind(AssertUnwindSafe(|| RustPngDecoder::new().decode_png(alloc, data))).ok().flatten()
    }
}

/// Install [`BoundedPng`] as the library's PNG decoder, once per process. Every engine calls it before it makes its
/// first terminal; a second call, from any thread, waits for the first and then does nothing.
///
/// Without a decoder the library answers every PNG transmission with an error; with the library's plain decoder a
/// header could claim gigabytes. Returns whether the decoder is installed.
pub fn ensure_png_decoder() -> bool {
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    *INSTALLED.get_or_init(|| {
        // SAFETY: `set_png_decoder` writes a process-global hook that libghostty does not synchronise, so no thread may
        // be inside a libghostty call at that moment. This is the only place that writes it, it runs under the
        // `OnceLock` (a thread that reaches it after the first waits until the hook is set), and every engine calls it
        // before it makes its first terminal, so no libghostty object of this product exists before the hook is set and
        // none is created without having passed here. The decoder is `Send + 'static` and never panics.
        unsafe { set_png_decoder(Some(Box::new(BoundedPng))) }.is_ok()
    })
}

/// What a terminal's allocator has seen, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageMemory {
    /// Live allocations of the terminal right now.
    pub live: usize,
    /// The most live allocations at any time since the terminal was made.
    pub peak: usize,
    /// The bound: a request of [`REFUSE_FROM_BYTES`] or more that would take `live` past it is refused.
    pub cap: usize,
    /// How many requests have been refused.
    pub refused: usize,
}

/// The allocator of one terminal: Rust's global allocator behind libghostty's allocator interface, with a count of the
/// live bytes and a bound on them.
///
/// The library holds the data of a Kitty transmission that is still arriving in one growing buffer, and its zlib
/// inflate and PNG decode write to buffers sized from what the program declares. Its own bound for one image is 400 MiB
/// and it is applied only when the image is complete, so without this a program could make one pane hold 400 MiB for as
/// long as it keeps a transmission open. A refused request makes the library drop that transmission and answer the
/// program with an error; the terminal stays usable (tested with floods and a zlib bomb).
///
/// It is shared by nothing: every engine has its own, so one pane cannot use up the room of another. The count is exact
/// for what the library allocates through it (the images and their loading buffers; the scrollback of the terminal is
/// not allocated here, the library maps that memory itself).
#[derive(Debug)]
pub struct BoundedAlloc {
    live: AtomicUsize,
    peak: AtomicUsize,
    cap: usize,
    refused: AtomicUsize,
}

impl BoundedAlloc {
    /// An allocator that refuses a request of [`REFUSE_FROM_BYTES`] or more that would take the live bytes past `cap`.
    pub fn new(cap: usize) -> Self {
        Self { live: AtomicUsize::new(0), peak: AtomicUsize::new(0), cap, refused: AtomicUsize::new(0) }
    }

    /// The counts so far.
    pub fn memory(&self) -> ImageMemory {
        ImageMemory {
            live: self.live.load(Ordering::Relaxed),
            peak: self.peak.load(Ordering::Relaxed),
            cap: self.cap,
            refused: self.refused.load(Ordering::Relaxed),
        }
    }

    /// Take `size` bytes into the count, or say no when a big request would pass the cap.
    fn reserve(&self, size: usize) -> bool {
        let mut live = self.live.load(Ordering::Relaxed);
        loop {
            let after = live.saturating_add(size);
            if size >= REFUSE_FROM_BYTES && after > self.cap {
                self.refused.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            match self.live.compare_exchange_weak(live, after, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => {
                    self.peak.fetch_max(after, Ordering::Relaxed);
                    return true;
                }
                Err(seen) => live = seen,
            }
        }
    }
}

// SAFETY: every method forwards to `Global`, Rust's own allocator, with the layout it was given, so the memory is valid,
// unique and sized as the trait requires. The count only observes: `deallocate` is called by the library with the
// pointer and layout of an earlier `allocate`, and that is what is subtracted. `grow` and `shrink` are the trait's
// provided methods (allocate a new block, copy, free the old one), so they go through the two methods below and the
// count stays exact. Nothing here panics: a refused request is an `Err`, and the library turns that into an error reply.
unsafe impl RustAllocator for BoundedAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if !self.reserve(layout.size()) {
            return Err(AllocError);
        }
        Global.allocate(layout).inspect_err(|_| {
            self.live.fetch_sub(layout.size(), Ordering::Relaxed);
        })
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.live.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller upholds the trait's contract, and the block came from `Global.allocate` above.
        unsafe { Global.deallocate(ptr, layout) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG header with the given declared size: signature, IHDR length, `IHDR`, width, height. The CRC and the rest
    /// are not needed by the check.
    fn header(width: u32, height: u32) -> Vec<u8> {
        let mut out = PNG_SIGNATURE.to_vec();
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        out
    }

    #[test]
    fn a_plain_header_is_accepted_and_its_size_read() {
        assert_eq!(check_png_header(&header(4, 3)), Some((4, 3)));
        assert_eq!(check_png_header(&header(1, 1)), Some((1, 1)));
        assert_eq!(check_png_header(&header(10_000, 3_000)), Some((10_000, 3_000)), "120 MB decoded is inside 128 MiB");
    }

    #[test]
    fn a_header_that_declares_too_much_is_refused() {
        assert_eq!(check_png_header(&header(0, 5)), None);
        assert_eq!(check_png_header(&header(5, 0)), None);
        assert_eq!(check_png_header(&header(10_001, 1)), None, "one pixel over the dimension limit");
        assert_eq!(check_png_header(&header(1, 10_001)), None);
        assert_eq!(check_png_header(&header(30_000, 30_000)), None, "3.6 GB decoded");
        assert_eq!(check_png_header(&header(u32::MAX, u32::MAX)), None);
        assert_eq!(check_png_header(&header(10_000, 10_000)), None, "400 MB decoded is over 128 MiB");
        // The product is checked: no wrap-around lets a huge size look small.
        assert_eq!(check_png_header(&header(1 << 31, 1 << 31)), None);
    }

    #[test]
    fn bytes_that_are_not_the_start_of_a_png_are_refused() {
        let good = header(2, 2);
        assert!(check_png_header(&good).is_some());
        assert_eq!(check_png_header(&[]), None);
        assert_eq!(check_png_header(&good[..23]), None, "one byte short of the size fields");
        let mut bad = good.clone();
        bad[0] = 0x88;
        assert_eq!(check_png_header(&bad), None, "wrong signature");
        let mut bad = good.clone();
        bad[11] = 12;
        assert_eq!(check_png_header(&bad), None, "an IHDR of length 12");
        let mut bad = good.clone();
        bad[12..16].copy_from_slice(b"IDAT");
        assert_eq!(check_png_header(&bad), None, "the first chunk is not IHDR");
        assert_eq!(check_png_header(&vec![0xff; 4096]), None);
    }

    #[test]
    fn the_decoder_refuses_without_touching_the_library_when_the_header_is_bad() {
        let mut decoder = BoundedPng;
        let alloc = Allocator::GLOBAL;
        assert!(decoder.decode_png(&alloc, &header(30_000, 30_000)).is_none());
        assert!(decoder.decode_png(&alloc, b"not a png at all, but long enough to have 24 bytes").is_none());
        assert!(decoder.decode_png(&alloc, &[]).is_none());
        // A good header with no body: the library's decoder is reached and says no, without a panic.
        assert!(decoder.decode_png(&alloc, &header(4, 4)).is_none());
    }

    fn layout(size: usize) -> Layout {
        Layout::from_size_align(size, 8).expect("a layout")
    }

    #[test]
    fn a_big_request_past_the_cap_is_refused_and_a_small_one_never_is() {
        let alloc = BoundedAlloc::new(1 << 20);
        let first = alloc.allocate(layout(600 * 1024)).expect("inside the cap");
        // 600 KiB live; another 600 KiB would pass 1 MiB.
        assert!(alloc.allocate(layout(600 * 1024)).is_err());
        assert_eq!(alloc.memory().refused, 1);
        assert_eq!(alloc.memory().live, 600 * 1024, "a refused request is not counted");
        // A small request is granted even when the live bytes are at the cap.
        let small = alloc.allocate(layout(4096)).expect("small requests are always granted");
        let more: Vec<_> = (0..200).map(|_| alloc.allocate(layout(4096)).expect("granted")).collect();
        assert!(alloc.memory().live > 1 << 20, "small requests may take the count past the cap");
        // Freeing brings the count down and the room back.
        for p in more {
            unsafe { alloc.deallocate(p.cast(), layout(4096)) };
        }
        unsafe { alloc.deallocate(small.cast(), layout(4096)) };
        unsafe { alloc.deallocate(first.cast(), layout(600 * 1024)) };
        let again = alloc.allocate(layout(900 * 1024)).expect("room again after the free");
        unsafe { alloc.deallocate(again.cast(), layout(900 * 1024)) };
        let memory = alloc.memory();
        assert_eq!((memory.live, memory.cap, memory.refused), (0, 1 << 20, 1));
        assert!(memory.peak >= 900 * 1024);
    }

    #[test]
    fn growing_goes_through_the_count_and_a_refused_grow_leaves_the_block_alone() {
        let alloc = BoundedAlloc::new(1 << 20);
        let block = alloc.allocate(layout(400 * 1024)).expect("block");
        unsafe { block.cast::<u8>().as_ptr().write_bytes(0xAB, 400 * 1024) };
        // The old and the new block are live together while a grow copies: 400 + 700 KiB is over the cap.
        let refused = unsafe { alloc.grow(block.cast(), layout(400 * 1024), layout(700 * 1024)) };
        assert!(refused.is_err());
        assert_eq!(alloc.memory().live, 400 * 1024, "the old block is still counted and still there");
        assert_eq!(unsafe { *block.cast::<u8>().as_ptr().add(1000) }, 0xAB);
        // A grow that fits keeps the contents and moves the count to the new size.
        let grown = unsafe { alloc.grow(block.cast(), layout(400 * 1024), layout(500 * 1024)) };
        // 400 + 500 KiB is inside 1 MiB.
        let grown = grown.expect("a grow inside the cap");
        assert_eq!(alloc.memory().live, 500 * 1024);
        assert_eq!(unsafe { *grown.cast::<u8>().as_ptr().add(399 * 1024) }, 0xAB);
        unsafe { alloc.deallocate(grown.cast(), layout(500 * 1024)) };
        assert_eq!(alloc.memory().live, 0);
    }

    #[test]
    fn the_cap_follows_the_image_limit_and_cannot_overflow() {
        const MIB: u64 = 1024 * 1024;
        assert_eq!(image_memory_cap(64 * MIB), (2 * 64 + 64) as usize * MIB as usize);
        assert_eq!(image_memory_cap(0), 64 * MIB as usize, "even with the protocol off the terminal keeps its room");
        assert_eq!(image_memory_cap(u64::MAX), usize::MAX);
    }

    #[test]
    fn installing_the_decoder_twice_is_one_install() {
        assert!(ensure_png_decoder());
        assert!(ensure_png_decoder());
    }
}
