//! The limits of the Kitty graphics protocol and the one PNG decoder the engine installs.
//!
//! Everything a program sends through the protocol is untrusted: sizes it declares, base64 it encodes, zlib it
//! compresses and PNG bytes it sends. The parser, the chunking, the zlib inflate and the eviction are the library's
//! (libghostty-vt); this module holds the numbers that bound them and the decoder that is put in front of the library's
//! own PNG decoder, so that a header which claims gigabytes is refused before anything is allocated.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

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

    #[test]
    fn installing_the_decoder_twice_is_one_install() {
        assert!(ensure_png_decoder());
        assert!(ensure_png_decoder());
    }
}
